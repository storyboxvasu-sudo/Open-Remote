use app_common::{
    AccessLevel, AuthChallenge, AuthResponse, AuthResult, ConnectionHandshakeRequest,
    ConnectionHandshakeResponse, IncomingRequestInfo, InputEvent, SignalingMessage,
    UnattendedAccessConfig,
};
use core_capture::ScreenCapturer;
use core_codec::FrameEncoder;
use core_input::InputInjector;
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[derive(Error, Debug)]
pub enum NetError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("WebSocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codec error: {0}")]
    Codec(String),
    #[error("Peer not found on local network: {0}")]
    PeerNotFound(String),
    #[error("Connection declined: {0}")]
    ConnectionDeclined(String),
    #[error("Authentication required")]
    AuthRequired(AuthChallenge),
    #[error("Incorrect password: {0}")]
    IncorrectPassword(String),
}

pub const DISCOVERY_PORT: u16 = 44320;
pub const MULTICAST_ADDR: &str = "239.255.42.99";

/// UDP LAN Peer Discovery message protocol (port 44320)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "data")]
pub enum DiscoveryMessage {
    Query {
        target_peer_id: String,
    },
    Response {
        peer_id: String,
        lan_ip: String,
        port: u16,
        #[serde(default)]
        control_port: u16,
        #[serde(default)]
        video_port: u16,
    },
    Announce {
        peer_id: String,
        lan_ip: String,
        port: u16,
        #[serde(default)]
        control_port: u16,
        #[serde(default)]
        video_port: u16,
    },
}

/// Normalizes peer IDs by stripping all non-digits (e.g. "925-447-871" -> "925447871")
pub fn normalize_peer_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_digit()).collect()
}

// Global LAN discovered peers cache: normalized_peer_id -> SocketAddr
static DISCOVERED_CACHE: parking_lot::RwLock<Option<HashMap<String, SocketAddr>>> = parking_lot::RwLock::new(None);

pub fn cache_peer(peer_id: &str, addr: SocketAddr) {
    let norm = normalize_peer_id(peer_id);
    if norm.is_empty() {
        return;
    }
    let mut lock = DISCOVERED_CACHE.write();
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(map) = lock.as_mut() {
        map.insert(norm, addr);
    }
}

pub fn get_cached_peer(peer_id: &str) -> Option<SocketAddr> {
    let norm = normalize_peer_id(peer_id);
    let lock = DISCOVERED_CACHE.read();
    lock.as_ref()?.get(&norm).copied()
}

/// Helper to create and bind a UDP socket with SO_REUSEADDR
pub fn create_udp_socket(addr: SocketAddr) -> Result<UdpSocket, NetError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let std_socket: std::net::UdpSocket = socket.into();
    let tokio_socket = UdpSocket::from_std(std_socket)?;
    Ok(tokio_socket)
}

/// Helper to bind UDP socket with SO_REUSEADDR and auto-retry on AddrInUse (OS 10048)
pub async fn bind_udp_reuse(addr: SocketAddr) -> Result<UdpSocket, NetError> {
    let mut backoff = tokio::time::Duration::from_millis(150);
    const MAX_RETRIES: usize = 3;

    for attempt in 0..=MAX_RETRIES {
        match create_udp_socket(addr) {
            Ok(sock) => return Ok(sock),
            Err(e) => {
                let is_addr_in_use = match &e {
                    NetError::Io(io_err) => {
                        io_err.kind() == std::io::ErrorKind::AddrInUse
                            || io_err.raw_os_error() == Some(10048)
                    }
                    _ => false,
                };

                if is_addr_in_use && attempt < MAX_RETRIES {
                    eprintln!(
                        "[core-net] UDP bind to {} failed (AddrInUse 10048). Retrying in {:?} (attempt {}/{})",
                        addr, backoff, attempt + 1, MAX_RETRIES
                    );
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

/// Helper to create and bind a TCP listener with SO_REUSEADDR
pub fn create_tcp_listener(addr: SocketAddr) -> Result<TcpListener, NetError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    let std_listener: std::net::TcpListener = socket.into();
    let tokio_listener = TcpListener::from_std(std_listener)?;
    Ok(tokio_listener)
}

/// Helper to bind TCP listener with SO_REUSEADDR and auto-retry on AddrInUse (OS 10048)
pub async fn bind_tcp_reuse(addr: SocketAddr) -> Result<TcpListener, NetError> {
    let mut backoff = tokio::time::Duration::from_millis(150);
    const MAX_RETRIES: usize = 3;

    for attempt in 0..=MAX_RETRIES {
        match create_tcp_listener(addr) {
            Ok(listener) => return Ok(listener),
            Err(e) => {
                let is_addr_in_use = match &e {
                    NetError::Io(io_err) => {
                        io_err.kind() == std::io::ErrorKind::AddrInUse
                            || io_err.raw_os_error() == Some(10048)
                    }
                    _ => false,
                };

                if is_addr_in_use && attempt < MAX_RETRIES {
                    eprintln!(
                        "[core-net] TCP bind to {} failed (AddrInUse 10048). Retrying in {:?} (attempt {}/{})",
                        addr, backoff, attempt + 1, MAX_RETRIES
                    );
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

pub type RequestPromptCallback = Arc<
    dyn Fn(IncomingRequestInfo) -> BoxFuture<'static, (bool, AccessLevel)> + Send + Sync,
>;

/// Host Direct LAN Server: binds local ports and listens for direct peer connections
pub struct DirectLanHost {
    pub peer_id: String,
    pub control_port: u16,
    pub video_port: u16,
    injector: Arc<InputInjector>,
    capturer: Arc<ScreenCapturer>,
    tasks: Arc<parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    access_level: Arc<parking_lot::RwLock<AccessLevel>>,
    default_access_level: Arc<parking_lot::RwLock<AccessLevel>>,
    unattended_access: Arc<parking_lot::RwLock<UnattendedAccessConfig>>,
    active_client: Arc<parking_lot::RwLock<Option<(String, String)>>>,
    prompt_callback: Arc<parking_lot::Mutex<Option<RequestPromptCallback>>>,
}

impl DirectLanHost {
    pub fn new(base_port: u16, capturer: Arc<ScreenCapturer>) -> Self {
        Self::with_peer_id(base_port, app_common::PeerId::load_or_create().0, capturer)
    }

    pub fn with_peer_id(base_port: u16, peer_id: String, capturer: Arc<ScreenCapturer>) -> Self {
        Self {
            peer_id,
            control_port: base_port,
            video_port: base_port + 1,
            injector: Arc::new(InputInjector::new()),
            capturer,
            tasks: Arc::new(parking_lot::Mutex::new(Vec::new())),
            access_level: Arc::new(parking_lot::RwLock::new(AccessLevel::Standard)),
            default_access_level: Arc::new(parking_lot::RwLock::new(AccessLevel::Standard)),
            unattended_access: Arc::new(parking_lot::RwLock::new(UnattendedAccessConfig::default())),
            active_client: Arc::new(parking_lot::RwLock::new(None)),
            prompt_callback: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn access_level(&self) -> AccessLevel {
        *self.access_level.read()
    }

    pub fn set_access_level(&self, level: AccessLevel) {
        *self.access_level.write() = level;
    }

    pub fn default_access_level(&self) -> AccessLevel {
        *self.default_access_level.read()
    }

    pub fn set_default_access_level(&self, level: AccessLevel) {
        *self.default_access_level.write() = level;
    }

    pub fn unattended_access(&self) -> UnattendedAccessConfig {
        self.unattended_access.read().clone()
    }

    pub fn set_unattended_access(&self, config: UnattendedAccessConfig) {
        *self.unattended_access.write() = config;
    }

    pub fn active_client(&self) -> Option<(String, String)> {
        self.active_client.read().clone()
    }

    pub fn set_prompt_callback<F>(&self, callback: F)
    where
        F: Fn(IncomingRequestInfo) -> BoxFuture<'static, (bool, AccessLevel)> + Send + Sync + 'static,
    {
        *self.prompt_callback.lock() = Some(Arc::new(callback));
    }

    /// Starts the background listener for incoming control events over direct LAN UDP with socket reuse
    pub async fn start_input_listener(&self) -> Result<(), NetError> {
        let addr: SocketAddr = format!("0.0.0.0:{}", self.control_port)
            .parse()
            .map_err(|e: std::net::AddrParseError| NetError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
        let socket = bind_udp_reuse(addr).await?;
        let injector = Arc::clone(&self.injector);
        let capturer = Arc::clone(&self.capturer);
        let access_level_lock = Arc::clone(&self.access_level);

        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, _peer_addr)) => {
                        if let Ok(event) = serde_json::from_slice::<InputEvent>(&buf[..len]) {
                            let current_level = *access_level_lock.read();
                            match current_level {
                                AccessLevel::ViewOnly => {
                                    // In ViewOnly mode, discard all clicks, movements, keystrokes and scrolls.
                                    // Allow monitor switching so remote viewer can inspect attached displays.
                                    if let InputEvent::SwitchMonitor { monitor_index } = &event {
                                        let _ = capturer.switch_monitor(*monitor_index);
                                        let monitors = ScreenCapturer::enumerate_monitors();
                                        if let Some(m) = monitors.iter().find(|m| m.index == *monitor_index) {
                                            injector.set_active_monitor_bounds(m.x, m.y, m.width, m.height);
                                        }
                                    }
                                }
                                AccessLevel::Standard | AccessLevel::FullAccess => {
                                    match &event {
                                        InputEvent::SwitchMonitor { monitor_index } => {
                                            let _ = capturer.switch_monitor(*monitor_index);
                                            let monitors = ScreenCapturer::enumerate_monitors();
                                            if let Some(m) = monitors.iter().find(|m| m.index == *monitor_index) {
                                                injector.set_active_monitor_bounds(m.x, m.y, m.width, m.height);
                                            }
                                        }
                                        _ => {
                                            let _ = injector.inject(&event);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.tasks.lock().push(handle);
        Ok(())
    }

    /// Starts the video streaming TCP listener, broadcasting compressed desktop frames to connected viewers
    pub async fn start_video_stream(&self) -> Result<(), NetError> {
        let addr: SocketAddr = format!("0.0.0.0:{}", self.video_port)
            .parse()
            .map_err(|e: std::net::AddrParseError| NetError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
        let listener = bind_tcp_reuse(addr).await?;
        let capturer = Arc::clone(&self.capturer);
        let prompt_callback = Arc::clone(&self.prompt_callback);
        let access_level = Arc::clone(&self.access_level);
        let default_access_level = Arc::clone(&self.default_access_level);
        let unattended_access = Arc::clone(&self.unattended_access);
        let active_client = Arc::clone(&self.active_client);

        let handle = tokio::spawn(async move {
            loop {
                if let Ok((mut stream, peer_addr)) = listener.accept().await {
                    let _ = stream.set_nodelay(true);
                    let cap = Arc::clone(&capturer);
                    let cb_opt = prompt_callback.lock().clone();
                    let access_lvl_clone = Arc::clone(&access_level);
                    let def_access_lvl = *default_access_level.read();
                    let unattended_cfg_lock = Arc::clone(&unattended_access);
                    let active_client_clone = Arc::clone(&active_client);
                    let enc = FrameEncoder::new();

                    tokio::spawn(async move {
                        // 1. Read Client Handshake Request
                        let mut req_len_buf = [0u8; 4];
                        if tokio::time::timeout(tokio::time::Duration::from_secs(10), stream.read_exact(&mut req_len_buf)).await.is_err() {
                            return;
                        }
                        let req_len = u32::from_be_bytes(req_len_buf) as usize;
                        if req_len > 65535 {
                            return;
                        }
                        let mut req_buf = vec![0u8; req_len];
                        if stream.read_exact(&mut req_buf).await.is_err() {
                            return;
                        }
                        let handshake_req: ConnectionHandshakeRequest = match serde_json::from_slice(&req_buf) {
                            Ok(r) => r,
                            Err(_) => return,
                        };

                        let client_peer_id = handshake_req.client_peer_id.clone();
                        let client_ip = peer_addr.ip().to_string();
                        let request_id = format!(
                            "{}-{}",
                            client_peer_id,
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis()
                        );

                        let u_cfg = unattended_cfg_lock.read().clone();
                        let (accepted, chosen_level) = if u_cfg.enabled && u_cfg.password_hash.is_some() {
                            // Unattended Access is enabled on Host
                            if let Some(auth_resp) = &handshake_req.auth_response {
                                // Client provided credentials upfront (e.g. remembered password)
                                let expected_hash = u_cfg.password_hash.clone().unwrap_or_default();
                                if auth_resp.hash.eq_ignore_ascii_case(&expected_hash) {
                                    let chosen = u_cfg.profile;
                                    let success_resp = ConnectionHandshakeResponse {
                                        accepted: true,
                                        access_level: chosen,
                                        reason: None,
                                        auth_challenge: None,
                                        auth_result: Some(AuthResult {
                                            success: true,
                                            profile: Some(chosen),
                                            reason: None,
                                        }),
                                    };
                                    let resp_bytes = serde_json::to_vec(&success_resp).unwrap_or_default();
                                    let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                    if stream.write_all(&resp_len).await.is_err() || stream.write_all(&resp_bytes).await.is_err() {
                                        return;
                                    }
                                    (true, chosen)
                                } else {
                                    let fail_resp = ConnectionHandshakeResponse {
                                        accepted: false,
                                        access_level: AccessLevel::Standard,
                                        reason: Some("Incorrect password".to_string()),
                                        auth_challenge: None,
                                        auth_result: Some(AuthResult {
                                            success: false,
                                            profile: None,
                                            reason: Some("Incorrect password".to_string()),
                                        }),
                                    };
                                    let resp_bytes = serde_json::to_vec(&fail_resp).unwrap_or_default();
                                    let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                    let _ = stream.write_all(&resp_len).await;
                                    let _ = stream.write_all(&resp_bytes).await;
                                    let _ = stream.shutdown().await;
                                    return;
                                }
                            } else {
                                // 1. Send AuthChallenge to Client
                                let challenge_resp = ConnectionHandshakeResponse {
                                    accepted: false,
                                    access_level: u_cfg.profile,
                                    reason: Some("REQUEST_PASSWORD".to_string()),
                                    auth_challenge: Some(AuthChallenge {
                                        challenge: "REQUEST_PASSWORD".to_string(),
                                        salt: u_cfg.salt.clone().unwrap_or_default(),
                                    }),
                                    auth_result: None,
                                };
                                let resp_bytes = serde_json::to_vec(&challenge_resp).unwrap_or_default();
                                let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                if stream.write_all(&resp_len).await.is_err() || stream.write_all(&resp_bytes).await.is_err() {
                                    return;
                                }

                                // 2. Hold connection in pending state (30s timeout) and concurrently display Host Accept/Reject prompt
                                let prompt_cb = cb_opt.clone();
                                let host_info = IncomingRequestInfo {
                                    request_id: request_id.clone(),
                                    client_peer_id: client_peer_id.clone(),
                                    client_ip: client_ip.clone(),
                                };
                                let host_prompt_fut = async {
                                    if let Some(cb) = prompt_cb {
                                        cb(host_info).await
                                    } else {
                                        futures_util::future::pending::<(bool, AccessLevel)>().await
                                    }
                                };
                                tokio::pin!(host_prompt_fut);

                                let auth_timeout = tokio::time::sleep(tokio::time::Duration::from_secs(30));
                                tokio::pin!(auth_timeout);

                                tokio::select! {
                                    _ = &mut auth_timeout => {
                                        let timeout_resp = ConnectionHandshakeResponse {
                                            accepted: false,
                                            access_level: AccessLevel::Standard,
                                            reason: Some("Authentication timed out".to_string()),
                                            auth_challenge: None,
                                            auth_result: Some(AuthResult {
                                                success: false,
                                                profile: None,
                                                reason: Some("Authentication timed out".to_string()),
                                            }),
                                        };
                                        let resp_bytes = serde_json::to_vec(&timeout_resp).unwrap_or_default();
                                        let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                        let _ = stream.write_all(&resp_len).await;
                                        let _ = stream.write_all(&resp_bytes).await;
                                        let _ = stream.shutdown().await;
                                        return;
                                    }

                                    (host_accepted, host_chosen_level) = &mut host_prompt_fut => {
                                        if host_accepted {
                                            let success_resp = ConnectionHandshakeResponse {
                                                accepted: true,
                                                access_level: host_chosen_level,
                                                reason: None,
                                                auth_challenge: None,
                                                auth_result: Some(AuthResult {
                                                    success: true,
                                                    profile: Some(host_chosen_level),
                                                    reason: None,
                                                }),
                                            };
                                            let resp_bytes = serde_json::to_vec(&success_resp).unwrap_or_default();
                                            let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                            if stream.write_all(&resp_len).await.is_err() || stream.write_all(&resp_bytes).await.is_err() {
                                                return;
                                            }
                                            (true, host_chosen_level)
                                        } else {
                                            let decline_resp = ConnectionHandshakeResponse {
                                                accepted: false,
                                                access_level: AccessLevel::Standard,
                                                reason: Some("Connection declined by remote host".to_string()),
                                                auth_challenge: None,
                                                auth_result: Some(AuthResult {
                                                    success: false,
                                                    profile: None,
                                                    reason: Some("Connection declined by remote host".to_string()),
                                                }),
                                            };
                                            let resp_bytes = serde_json::to_vec(&decline_resp).unwrap_or_default();
                                            let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                            let _ = stream.write_all(&resp_len).await;
                                            let _ = stream.write_all(&resp_bytes).await;
                                            let _ = stream.shutdown().await;
                                            return;
                                        }
                                    }

                                    client_res = async {
                                        let mut next_len_buf = [0u8; 4];
                                        stream.read_exact(&mut next_len_buf).await?;
                                        let next_len = u32::from_be_bytes(next_len_buf) as usize;
                                        if next_len > 65535 {
                                            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Payload too large"));
                                        }
                                        let mut next_buf = vec![0u8; next_len];
                                        stream.read_exact(&mut next_buf).await?;
                                        serde_json::from_slice::<ConnectionHandshakeRequest>(&next_buf)
                                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                                    } => {
                                        match client_res {
                                            Ok(next_req) => {
                                                if let Some(auth_resp) = next_req.auth_response {
                                                    let expected_hash = u_cfg.password_hash.clone().unwrap_or_default();
                                                    if auth_resp.hash.eq_ignore_ascii_case(&expected_hash) {
                                                        let chosen = u_cfg.profile;
                                                        let success_resp = ConnectionHandshakeResponse {
                                                            accepted: true,
                                                            access_level: chosen,
                                                            reason: None,
                                                            auth_challenge: None,
                                                            auth_result: Some(AuthResult {
                                                                success: true,
                                                                profile: Some(chosen),
                                                                reason: None,
                                                            }),
                                                        };
                                                        let resp_bytes = serde_json::to_vec(&success_resp).unwrap_or_default();
                                                        let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                                        if stream.write_all(&resp_len).await.is_err() || stream.write_all(&resp_bytes).await.is_err() {
                                                            return;
                                                        }
                                                        (true, chosen)
                                                    } else {
                                                        let fail_resp = ConnectionHandshakeResponse {
                                                            accepted: false,
                                                            access_level: AccessLevel::Standard,
                                                            reason: Some("Incorrect password".to_string()),
                                                            auth_challenge: None,
                                                            auth_result: Some(AuthResult {
                                                                success: false,
                                                                profile: None,
                                                                reason: Some("Incorrect password".to_string()),
                                                            }),
                                                        };
                                                        let resp_bytes = serde_json::to_vec(&fail_resp).unwrap_or_default();
                                                        let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                                                        let _ = stream.write_all(&resp_len).await;
                                                        let _ = stream.write_all(&resp_bytes).await;
                                                        let _ = stream.shutdown().await;
                                                        return;
                                                    }
                                                } else {
                                                    let _ = stream.shutdown().await;
                                                    return;
                                                }
                                            }
                                            Err(_) => {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // Unattended Access is disabled: Prompt host user
                            let (user_accepted, chosen_level) = if let Some(cb) = cb_opt {
                                let info = IncomingRequestInfo {
                                    request_id,
                                    client_peer_id: client_peer_id.clone(),
                                    client_ip: client_ip.clone(),
                                };
                                cb(info).await
                            } else {
                                (true, def_access_lvl)
                            };

                            let resp = ConnectionHandshakeResponse {
                                accepted: user_accepted,
                                access_level: chosen_level,
                                reason: if user_accepted {
                                    None
                                } else {
                                    Some("Connection request declined by host".to_string())
                                },
                                auth_challenge: None,
                                auth_result: None,
                            };
                            let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
                            let resp_len = (resp_bytes.len() as u32).to_be_bytes();
                            if stream.write_all(&resp_len).await.is_err() || stream.write_all(&resp_bytes).await.is_err() {
                                return;
                            }

                            if !user_accepted {
                                let _ = stream.shutdown().await;
                                return;
                            }
                            (user_accepted, chosen_level)
                        };

                        if !accepted {
                            let _ = stream.shutdown().await;
                            return;
                        }

                        // Session is accepted: record active state
                        *access_lvl_clone.write() = chosen_level;
                        *active_client_clone.write() = Some((client_peer_id.clone(), client_ip.clone()));

                        // 4. Video Streaming loop
                        let mut last_seq = 0u64;
                        let mut last_monitor = cap.active_monitor_index();
                        let mut send_monitors = true;
                        let mut last_send_time = tokio::time::Instant::now();

                        loop {
                            let current_seq = cap.frame_counter();
                            let current_mon = cap.active_monitor_index();

                            if current_mon != last_monitor {
                                last_monitor = current_mon;
                                send_monitors = true;
                                enc.reset();
                            }

                            let should_send = (current_seq != last_seq)
                                || (last_seq == 0)
                                || (last_send_time.elapsed().as_millis() >= 500);

                            if should_send {
                                if let Some(frame) = cap.get_latest_frame() {
                                    match enc.encode(&frame) {
                                        Ok(Some(mut compressed)) => {
                                            if send_monitors || last_seq == 0 {
                                                compressed.meta.monitors = Some(ScreenCapturer::enumerate_monitors());
                                                compressed.meta.active_monitor = Some(current_mon);
                                                send_monitors = false;
                                            }
                                            compressed.meta.access_level = Some(*access_lvl_clone.read());

                                            let meta_bytes = serde_json::to_vec(&compressed.meta).unwrap_or_default();
                                            let meta_len = meta_bytes.len() as u32;
                                            let payload_len = compressed.payload.len() as u32;

                                            // Consolidated single TCP write: 8-byte header + meta + payload
                                            let mut packet = Vec::with_capacity(8 + meta_bytes.len() + compressed.payload.len());
                                            packet.extend_from_slice(&meta_len.to_be_bytes());
                                            packet.extend_from_slice(&payload_len.to_be_bytes());
                                            packet.extend_from_slice(&meta_bytes);
                                            packet.extend_from_slice(&compressed.payload);

                                            if stream.write_all(&packet).await.is_err() {
                                                break;
                                            }

                                            last_seq = current_seq;
                                            last_send_time = tokio::time::Instant::now();
                                        }
                                        Ok(None) => {
                                            // Idle screen: skip packet sending completely
                                            last_seq = current_seq;
                                        }
                                        Err(e) => {
                                            eprintln!("[core-net] Frame encode error: {:?}", e);
                                        }
                                    }
                                }
                            }

                            tokio::time::sleep(tokio::time::Duration::from_millis(4)).await;
                        }

                        *active_client_clone.write() = None;
                    });
                }
            }
        });

        self.tasks.lock().push(handle);
        Ok(())
    }

    /// Starts the background UDP discovery responder and continuous beacon
    pub async fn start_discovery_responder(&self) -> Result<(), NetError> {
        let socket = bind_discovery_socket().await?;

        let my_peer_id = self.peer_id.clone();
        let my_norm_id = normalize_peer_id(&my_peer_id);
        let control_port = self.control_port;
        let video_port = self.video_port;

        let socket_for_announce = Arc::clone(&socket);
        let my_peer_id_announce = my_peer_id.clone();

        // Continuous UDP discovery heartbeat every 1.5s
        let h1 = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(1500));
            let mcast_addr: SocketAddr = format!("{}:{}", MULTICAST_ADDR, DISCOVERY_PORT).parse().unwrap();
            let bcast_addr: SocketAddr = format!("255.255.255.255:{}", DISCOVERY_PORT).parse().unwrap();
            let loopback_addr: SocketAddr = format!("127.0.0.1:{}", DISCOVERY_PORT).parse().unwrap();

            loop {
                interval.tick().await;
                let resolved_ip = match local_ip_address::local_ip() {
                    Ok(ip) => ip.to_string(),
                    Err(_) => "127.0.0.1".to_string(),
                };

                let announce = DiscoveryMessage::Announce {
                    peer_id: my_peer_id_announce.clone(),
                    lan_ip: resolved_ip.clone(),
                    port: control_port,
                    control_port,
                    video_port,
                };

                if let Ok(bytes) = serde_json::to_vec(&announce) {
                    let _ = socket_for_announce.send_to(&bytes, bcast_addr).await;
                    let _ = socket_for_announce.send_to(&bytes, mcast_addr).await;
                    let _ = socket_for_announce.send_to(&bytes, loopback_addr).await;

                    if let Ok(ip) = local_ip_address::local_ip() {
                        if let std::net::IpAddr::V4(ipv4) = ip {
                            let oct = ipv4.octets();
                            let subnet_bcast = format!("{}.{}.{}.255:{}", oct[0], oct[1], oct[2], DISCOVERY_PORT);
                            let _ = socket_for_announce.send_to(&bytes, &subnet_bcast).await;
                        }
                    }
                }
            }
        });

        // Listen for discovery queries and announcements from peers on LAN
        let h2 = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, peer_addr)) => {
                        if let Ok(msg) = serde_json::from_slice::<DiscoveryMessage>(&buf[..len]) {
                            match msg {
                                DiscoveryMessage::Query { target_peer_id } => {
                                    if normalize_peer_id(&target_peer_id) == my_norm_id {
                                        let resolved_ip = match local_ip_address::local_ip() {
                                            Ok(ip) => ip.to_string(),
                                            Err(_) => "127.0.0.1".to_string(),
                                        };
                                        let response = DiscoveryMessage::Response {
                                            peer_id: my_peer_id.clone(),
                                            lan_ip: resolved_ip,
                                            port: control_port,
                                            control_port,
                                            video_port,
                                        };
                                        if let Ok(resp_bytes) = serde_json::to_vec(&response) {
                                            let _ = socket.send_to(&resp_bytes, peer_addr).await;
                                        }
                                    }
                                }
                                DiscoveryMessage::Announce { peer_id: remote_id, lan_ip, port, .. } => {
                                    if normalize_peer_id(&remote_id) != my_norm_id {
                                        let target_ip = lan_ip.parse::<std::net::IpAddr>().unwrap_or(peer_addr.ip());
                                        let port_to_use = if port > 0 { port } else { 44321 };
                                        cache_peer(&remote_id, SocketAddr::new(target_ip, port_to_use));
                                    }
                                }
                                DiscoveryMessage::Response { peer_id: remote_id, lan_ip, port, .. } => {
                                    let target_ip = lan_ip.parse::<std::net::IpAddr>().unwrap_or(peer_addr.ip());
                                    let port_to_use = if port > 0 { port } else { 44321 };
                                    cache_peer(&remote_id, SocketAddr::new(target_ip, port_to_use));
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.tasks.lock().push(h1);
        self.tasks.lock().push(h2);
        Ok(())
    }

    /// Abort all spawned network tasks and immediately close all listening sockets
    pub fn stop(&self) {
        let mut tasks = self.tasks.lock();
        for handle in tasks.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for DirectLanHost {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Helper function to create, bind, and join multicast group on port 44320 with SO_REUSEADDR and retry
pub async fn bind_discovery_socket() -> Result<Arc<UdpSocket>, NetError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let mut backoff = tokio::time::Duration::from_millis(150);
    const MAX_RETRIES: usize = 3;

    for attempt in 0..=MAX_RETRIES {
        let res = (|| -> Result<Arc<UdpSocket>, NetError> {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.set_reuse_address(true)?;
            let _ = socket.set_broadcast(true);
            let _ = socket.set_nonblocking(true);

            let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT);
            socket.bind(&bind_addr.into())?;

            // Join local multicast group
            if let Ok(mcast_ip) = MULTICAST_ADDR.parse::<Ipv4Addr>() {
                let _ = socket.join_multicast_v4(&mcast_ip, &Ipv4Addr::UNSPECIFIED);
            }

            let std_socket: std::net::UdpSocket = socket.into();
            let tokio_socket = UdpSocket::from_std(std_socket)?;
            Ok(Arc::new(tokio_socket))
        })();

        match res {
            Ok(sock) => return Ok(sock),
            Err(e) => {
                let is_addr_in_use = match &e {
                    NetError::Io(io_err) => {
                        io_err.kind() == std::io::ErrorKind::AddrInUse
                            || io_err.raw_os_error() == Some(10048)
                    }
                    _ => false,
                };
                if is_addr_in_use && attempt < MAX_RETRIES {
                    eprintln!(
                        "[core-net] Discovery socket bind failed (AddrInUse 10048). Retrying in {:?} (attempt {}/{})",
                        backoff, attempt + 1, MAX_RETRIES
                    );
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

static CLIENT_DISCOVERY_HANDLE: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>> = parking_lot::Mutex::new(None);

/// Starts an active background client listener to capture UDP discovery announcements
pub async fn start_background_client_discovery() -> Result<(), NetError> {
    {
        let lock = CLIENT_DISCOVERY_HANDLE.lock();
        if lock.is_some() {
            return Ok(());
        }
    }
    let socket = bind_discovery_socket().await?;
    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, peer_addr)) => {
                    if let Ok(msg) = serde_json::from_slice::<DiscoveryMessage>(&buf[..len]) {
                        match msg {
                            DiscoveryMessage::Announce { peer_id, lan_ip, port, .. }
                            | DiscoveryMessage::Response { peer_id, lan_ip, port, .. } => {
                                let target_ip = lan_ip.parse::<std::net::IpAddr>().unwrap_or(peer_addr.ip());
                                let port_to_use = if port > 0 { port } else { 44321 };
                                cache_peer(&peer_id, SocketAddr::new(target_ip, port_to_use));
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    {
        let mut lock = CLIENT_DISCOVERY_HANDLE.lock();
        *lock = Some(handle);
    }
    Ok(())
}

pub fn stop_background_client_discovery() {
    let mut lock = CLIENT_DISCOVERY_HANDLE.lock();
    if let Some(handle) = lock.take() {
        handle.abort();
    }
}

/// Controller Direct LAN Client: connects directly to a target IP:Port over the local network
pub struct DirectLanClient {
    pub target_addr: SocketAddr,
    pub my_peer_id: String,
    control_socket: UdpSocket,
}

impl DirectLanClient {
    pub async fn connect(target_addr: SocketAddr) -> Result<Self, NetError> {
        Self::connect_with_peer_id(target_addr, app_common::PeerId::load_or_create().0).await
    }

    pub async fn connect_with_peer_id(target_addr: SocketAddr, my_peer_id: String) -> Result<Self, NetError> {
        let control_socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self {
            target_addr,
            my_peer_id,
            control_socket,
        })
    }

    pub async fn send_input(&self, event: &InputEvent) -> Result<(), NetError> {
        let payload = serde_json::to_vec(event)?;
        self.control_socket.send_to(&payload, self.target_addr).await?;
        Ok(())
    }

    pub async fn connect_video_stream(&self) -> Result<(TcpStream, AccessLevel), NetError> {
        self.connect_video_stream_with_password(None).await
    }

    pub async fn connect_video_stream_with_password(
        &self,
        password: Option<String>,
    ) -> Result<(TcpStream, AccessLevel), NetError> {
        let video_addr = SocketAddr::new(self.target_addr.ip(), self.target_addr.port() + 1);
        let mut stream = TcpStream::connect(video_addr).await?;
        let _ = stream.set_nodelay(true);

        // Send initial Handshake Request
        let req = ConnectionHandshakeRequest {
            client_peer_id: self.my_peer_id.clone(),
            client_name: None,
            auth_response: None,
        };
        let req_bytes = serde_json::to_vec(&req)?;
        let req_len = (req_bytes.len() as u32).to_be_bytes();
        stream.write_all(&req_len).await?;
        stream.write_all(&req_bytes).await?;

        // Read initial Handshake Response (wait up to 65s for Host user to accept or challenge)
        let mut resp_len_buf = [0u8; 4];
        tokio::time::timeout(
            tokio::time::Duration::from_secs(65),
            stream.read_exact(&mut resp_len_buf),
        )
        .await
        .map_err(|_| {
            NetError::ConnectionDeclined("Connection request timed out waiting for host response".to_string())
        })??;

        let resp_len = u32::from_be_bytes(resp_len_buf) as usize;
        if resp_len > 65535 {
            return Err(NetError::ConnectionDeclined("Invalid response size".to_string()));
        }
        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await?;
        let resp: ConnectionHandshakeResponse = serde_json::from_slice(&resp_buf)?;

        // If host sent an AuthChallenge:
        if let Some(challenge) = resp.auth_challenge {
            if let Some(pwd) = password {
                // Compute salted hash and send follow-up AuthResponse
                let hash = app_common::hash_password(&pwd, &challenge.salt);
                let auth_req = ConnectionHandshakeRequest {
                    client_peer_id: self.my_peer_id.clone(),
                    client_name: None,
                    auth_response: Some(AuthResponse { hash }),
                };
                let auth_bytes = serde_json::to_vec(&auth_req)?;
                let auth_len = (auth_bytes.len() as u32).to_be_bytes();
                stream.write_all(&auth_len).await?;
                stream.write_all(&auth_bytes).await?;

                // Read verification response (AuthResult)
                let mut auth_resp_len_buf = [0u8; 4];
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(15),
                    stream.read_exact(&mut auth_resp_len_buf),
                )
                .await
                .map_err(|_| {
                    NetError::ConnectionDeclined("Timed out waiting for authentication verification".to_string())
                })??;

                let auth_resp_len = u32::from_be_bytes(auth_resp_len_buf) as usize;
                let mut auth_resp_buf = vec![0u8; auth_resp_len];
                stream.read_exact(&mut auth_resp_buf).await?;
                let auth_result_resp: ConnectionHandshakeResponse = serde_json::from_slice(&auth_resp_buf)?;

                if auth_result_resp.accepted {
                    return Ok((stream, auth_result_resp.access_level));
                } else {
                    let reason = auth_result_resp
                        .reason
                        .unwrap_or_else(|| "Incorrect password".to_string());
                    return Err(NetError::IncorrectPassword(reason));
                }
            } else {
                // No password provided: prompt client UI
                return Err(NetError::AuthRequired(challenge));
            }
        }

        if !resp.accepted {
            return Err(NetError::ConnectionDeclined(
                resp.reason
                    .unwrap_or_else(|| "Connection was declined by the remote host".to_string()),
            ));
        }

        Ok((stream, resp.access_level))
    }

    pub async fn connect_video_stream_interactive<F, Fut>(
        &self,
        password: Option<String>,
        on_auth_challenge: F,
    ) -> Result<(TcpStream, AccessLevel), NetError>
    where
        F: FnOnce(AuthChallenge) -> Fut,
        Fut: std::future::Future<Output = Option<String>>,
    {
        let video_addr = SocketAddr::new(self.target_addr.ip(), self.target_addr.port() + 1);
        let mut stream = TcpStream::connect(video_addr).await?;
        let _ = stream.set_nodelay(true);

        // Send initial Handshake Request
        let req = ConnectionHandshakeRequest {
            client_peer_id: self.my_peer_id.clone(),
            client_name: None,
            auth_response: None,
        };
        let req_bytes = serde_json::to_vec(&req)?;
        let req_len = (req_bytes.len() as u32).to_be_bytes();
        stream.write_all(&req_len).await?;
        stream.write_all(&req_bytes).await?;

        // Read initial Handshake Response (wait up to 35s for Host user to accept or challenge)
        let mut resp_len_buf = [0u8; 4];
        tokio::time::timeout(
            tokio::time::Duration::from_secs(35),
            stream.read_exact(&mut resp_len_buf),
        )
        .await
        .map_err(|_| {
            NetError::ConnectionDeclined("Connection request timed out waiting for host response".to_string())
        })??;

        let resp_len = u32::from_be_bytes(resp_len_buf) as usize;
        if resp_len > 65535 {
            return Err(NetError::ConnectionDeclined("Invalid response size".to_string()));
        }
        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await?;
        let resp: ConnectionHandshakeResponse = serde_json::from_slice(&resp_buf)?;

        // If host sent an AuthChallenge:
        if let Some(challenge) = resp.auth_challenge {
            if let Some(pwd) = password {
                // Compute salted hash and send follow-up AuthResponse immediately
                let hash = app_common::hash_password(&pwd, &challenge.salt);
                let auth_req = ConnectionHandshakeRequest {
                    client_peer_id: self.my_peer_id.clone(),
                    client_name: None,
                    auth_response: Some(AuthResponse { hash }),
                };
                let auth_bytes = serde_json::to_vec(&auth_req)?;
                let auth_len = (auth_bytes.len() as u32).to_be_bytes();
                stream.write_all(&auth_len).await?;
                stream.write_all(&auth_bytes).await?;

                // Read verification response (AuthResult)
                let mut auth_resp_len_buf = [0u8; 4];
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(15),
                    stream.read_exact(&mut auth_resp_len_buf),
                )
                .await
                .map_err(|_| {
                    NetError::ConnectionDeclined("Timed out waiting for authentication verification".to_string())
                })??;

                let auth_resp_len = u32::from_be_bytes(auth_resp_len_buf) as usize;
                let mut auth_resp_buf = vec![0u8; auth_resp_len];
                stream.read_exact(&mut auth_resp_buf).await?;
                let auth_result_resp: ConnectionHandshakeResponse = serde_json::from_slice(&auth_resp_buf)?;

                if auth_result_resp.accepted {
                    return Ok((stream, auth_result_resp.access_level));
                } else {
                    let reason = auth_result_resp
                        .reason
                        .unwrap_or_else(|| "Incorrect password".to_string());
                    return Err(NetError::IncorrectPassword(reason));
                }
            } else {
                // Interactive flow: hold connection and prompt user modal concurrently with host unsolicited response
                let challenge_salt = challenge.salt.clone();
                let client_peer_id = self.my_peer_id.clone();

                tokio::select! {
                    user_pwd_opt = on_auth_challenge(challenge) => {
                        if let Some(pwd) = user_pwd_opt {
                            let hash = app_common::hash_password(&pwd, &challenge_salt);
                            let auth_req = ConnectionHandshakeRequest {
                                client_peer_id,
                                client_name: None,
                                auth_response: Some(AuthResponse { hash }),
                            };
                            let auth_bytes = serde_json::to_vec(&auth_req)?;
                            let auth_len = (auth_bytes.len() as u32).to_be_bytes();
                            stream.write_all(&auth_len).await?;
                            stream.write_all(&auth_bytes).await?;

                            let mut auth_resp_len_buf = [0u8; 4];
                            tokio::time::timeout(
                                tokio::time::Duration::from_secs(15),
                                stream.read_exact(&mut auth_resp_len_buf),
                            )
                            .await
                            .map_err(|_| {
                                NetError::ConnectionDeclined("Timed out waiting for authentication verification".to_string())
                            })??;

                            let auth_resp_len = u32::from_be_bytes(auth_resp_len_buf) as usize;
                            let mut auth_resp_buf = vec![0u8; auth_resp_len];
                            stream.read_exact(&mut auth_resp_buf).await?;
                            let auth_result_resp: ConnectionHandshakeResponse = serde_json::from_slice(&auth_resp_buf)?;

                            if auth_result_resp.accepted {
                                return Ok((stream, auth_result_resp.access_level));
                            } else {
                                let reason = auth_result_resp
                                    .reason
                                    .unwrap_or_else(|| "Incorrect password".to_string());
                                return Err(NetError::IncorrectPassword(reason));
                            }
                        } else {
                            let _ = stream.shutdown().await;
                            return Err(NetError::ConnectionDeclined("Authorization cancelled by user".to_string()));
                        }
                    }

                    host_unsolicited = async {
                        let mut next_len_buf = [0u8; 4];
                        stream.read_exact(&mut next_len_buf).await?;
                        let next_len = u32::from_be_bytes(next_len_buf) as usize;
                        if next_len > 65535 {
                            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Payload too large"));
                        }
                        let mut next_buf = vec![0u8; next_len];
                        stream.read_exact(&mut next_buf).await?;
                        serde_json::from_slice::<ConnectionHandshakeResponse>(&next_buf)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                    } => {
                        match host_unsolicited {
                            Ok(host_resp) => {
                                if host_resp.accepted {
                                    return Ok((stream, host_resp.access_level));
                                } else {
                                    return Err(NetError::ConnectionDeclined(
                                        host_resp.reason.unwrap_or_else(|| "Connection declined by host".to_string())
                                    ));
                                }
                            }
                            Err(_) => {
                                return Err(NetError::ConnectionDeclined("Connection closed by host".to_string()));
                            }
                        }
                    }
                }
            }
        }

        if !resp.accepted {
            return Err(NetError::ConnectionDeclined(
                resp.reason
                    .unwrap_or_else(|| "Connection was declined by the remote host".to_string()),
            ));
        }

        Ok((stream, resp.access_level))
    }
}

/// WAN Signaling Client: connects to the central rendezvous/signaling server
pub struct SignalingClient {
    server_url: String,
}

impl SignalingClient {
    pub fn new(server_url: String) -> Self {
        Self { server_url }
    }

    pub async fn register(&self, peer_id: &str) -> Result<(), NetError> {
        let (ws_stream, _) = connect_async(&self.server_url).await?;
        let (mut write, mut _read) = ws_stream.split();

        let msg = SignalingMessage::Register {
            peer_id: peer_id.to_string(),
        };
        let text = serde_json::to_string(&msg)?;
        write.send(Message::Text(text.into())).await?;

        Ok(())
    }
}

/// Reads and decompresses next frame from a remote TCP video stream
pub async fn read_remote_frame<R: tokio::io::AsyncReadExt + Unpin>(
    stream: &mut R,
    encoder: &FrameEncoder,
) -> Result<(app_common::FrameMeta, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let mut header_buf = [0u8; 8];
    stream.read_exact(&mut header_buf).await?;
    let meta_len = u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]) as usize;
    let payload_len = u32::from_be_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]) as usize;

    let mut meta_buf = vec![0u8; meta_len];
    stream.read_exact(&mut meta_buf).await?;
    let meta: app_common::FrameMeta = serde_json::from_slice(&meta_buf)?;

    let mut payload_buf = vec![0u8; payload_len];
    stream.read_exact(&mut payload_buf).await?;

    let mut raw_pixels = encoder
        .decode(&payload_buf)
        .map_err(|e| format!("Frame decode failed: {}", e))?;

    // Convert Windows BGRA format to standard RGBA format for HTML5 Canvas
    for pixel in raw_pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    Ok((meta, raw_pixels))
}

/// Broadcasts a UDP query on the local network (port 44320) to resolve a 9-digit Peer ID to its LAN IP:Port
pub async fn resolve_peer_id_on_lan(peer_id: &str, timeout_ms: u64) -> Result<SocketAddr, NetError> {
    let target_norm = normalize_peer_id(peer_id);

    // 1. Check in-memory cache first
    if let Some(cached) = get_cached_peer(&target_norm) {
        return Ok(cached);
    }

    // 2. Broadcast active query
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;

    let query = DiscoveryMessage::Query {
        target_peer_id: target_norm.clone(),
    };
    let query_bytes = serde_json::to_vec(&query)?;

    // Send query to broadcast, multicast, loopback, and local subnet
    let bcast_addr: SocketAddr = format!("255.255.255.255:{}", DISCOVERY_PORT).parse().unwrap();
    let mcast_addr: SocketAddr = format!("{}:{}", MULTICAST_ADDR, DISCOVERY_PORT).parse().unwrap();
    let loopback_addr: SocketAddr = format!("127.0.0.1:{}", DISCOVERY_PORT).parse().unwrap();

    let _ = socket.send_to(&query_bytes, bcast_addr).await;
    let _ = socket.send_to(&query_bytes, mcast_addr).await;
    let _ = socket.send_to(&query_bytes, loopback_addr).await;

    if let Ok(ip) = local_ip_address::local_ip() {
        if let std::net::IpAddr::V4(ipv4) = ip {
            let oct = ipv4.octets();
            let subnet_bcast = format!("{}.{}.{}.255:{}", oct[0], oct[1], oct[2], DISCOVERY_PORT);
            let _ = socket.send_to(&query_bytes, &subnet_bcast).await;
        }
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut buf = vec![0u8; 4096];

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, peer_addr))) => {
                if let Ok(msg) = serde_json::from_slice::<DiscoveryMessage>(&buf[..len]) {
                    match msg {
                        DiscoveryMessage::Response { peer_id: resp_id, lan_ip, port, .. }
                        | DiscoveryMessage::Announce { peer_id: resp_id, lan_ip, port, .. } => {
                            let target_ip = lan_ip.parse::<std::net::IpAddr>().unwrap_or(peer_addr.ip());
                            let port_to_use = if port > 0 { port } else { 44321 };
                            let addr = SocketAddr::new(target_ip, port_to_use);
                            cache_peer(&resp_id, addr);
                            if normalize_peer_id(&resp_id) == target_norm {
                                return Ok(addr);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => break,
        }
    }

    Err(NetError::PeerNotFound(format!(
        "Peer ID {} was not found on your local network. Please ensure the remote PC is running OpenRemote on the same Wi-Fi/LAN, or connect directly using its LAN IP address.",
        peer_id
    )))
}

/// Parses user input (Peer ID, IP, or IP:Port) and resolves it to a concrete SocketAddr
pub async fn resolve_target_address(
    input: &str,
    default_port: u16,
    my_peer_id: Option<&str>,
) -> Result<SocketAddr, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Target address cannot be empty".to_string());
    }

    // 1. Input Normalization: Strip all hyphens, spaces, and formatting characters
    let digits_only: String = normalize_peer_id(raw);

    // If input is 9 digits (Peer ID) and not an IP with periods
    if digits_only.len() == 9 && !raw.contains('.') {
        // If it's my own Peer ID, connect to loopback
        if let Some(my_id) = my_peer_id {
            let my_digits: String = normalize_peer_id(my_id);
            if digits_only == my_digits {
                return Ok(SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    default_port,
                ));
            }
        }

        // Check local cache first before LAN query
        if let Some(cached) = get_cached_peer(&digits_only) {
            return Ok(cached);
        }

        // Perform LAN discovery across broadcast and multicast
        return resolve_peer_id_on_lan(&digits_only, 3000)
            .await
            .map_err(|e| e.to_string());
    }

    // 2. Direct SocketAddr check (e.g. 192.168.1.15:44321)
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // 3. Direct IP without port (e.g. 192.168.1.15)
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }

    // 4. Hostname:Port or Hostname (e.g. localhost, localhost:44321, desktop.local)
    let host_with_port = if raw.contains(':') {
        raw.to_string()
    } else {
        format!("{}:{}", raw, default_port)
    };

    let lookup_result = tokio::net::lookup_host(host_with_port).await;
    match lookup_result {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                Ok(addr)
            } else {
                Err(format!("Could not resolve host: {}", raw))
            }
        }
        Err(e) => Err(format!("Invalid target address '{}': {}", raw, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_normalize_peer_id_variations() {
        assert_eq!(normalize_peer_id("925-447-871"), "925447871");
        assert_eq!(normalize_peer_id("925 447 871"), "925447871");
        assert_eq!(normalize_peer_id("925447871"), "925447871");
    }

    #[tokio::test]
    async fn test_cache_and_resolve_normalized_peer_id() {
        let test_addr: SocketAddr = "192.168.1.55:44321".parse().unwrap();
        cache_peer("925-447-871", test_addr);
        assert_eq!(get_cached_peer("925447871"), Some(test_addr));
        assert_eq!(get_cached_peer("925-447-871"), Some(test_addr));

        let res = resolve_target_address("925-447-871", 44321, None).await.unwrap();
        assert_eq!(res, test_addr);
    }

    #[tokio::test]
    async fn test_resolve_ip_without_port() {
        let res = resolve_target_address("192.168.1.100", 44321, None).await.unwrap();
        assert_eq!(res.to_string(), "192.168.1.100:44321");
    }

    #[tokio::test]
    async fn test_resolve_ip_with_custom_port() {
        let res = resolve_target_address("192.168.1.100:55000", 44321, None).await.unwrap();
        assert_eq!(res.to_string(), "192.168.1.100:55000");
    }

    #[tokio::test]
    async fn test_resolve_own_peer_id_to_loopback() {
        let my_id = "901-435-944";
        let res = resolve_target_address("901435944", 44321, Some(my_id)).await.unwrap();
        assert_eq!(res.to_string(), "127.0.0.1:44321");
    }

    #[tokio::test]
    async fn test_resolve_loopback_string() {
        let res = resolve_target_address("127.0.0.1", 44321, None).await.unwrap();
        assert_eq!(res.to_string(), "127.0.0.1:44321");
    }
}
