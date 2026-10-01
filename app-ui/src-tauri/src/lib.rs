use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

use app_common::{
    AccessLevel, HostSessionInfo, IncomingRequestInfo, InputEvent,
    MonitorDescriptor, PeerConfig, PeerId, RecentSession,
};
use core_capture::ScreenCapturer;
use core_codec::FrameEncoder;
use core_net::{read_remote_frame, resolve_target_address, DirectLanClient, DirectLanHost};

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemInfo {
    pub peer_id: String,
    pub lan_ip: String,
    pub default_control_port: u16,
    pub default_video_port: u16,
    pub is_hosting: bool,
    pub is_connected: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HostStatus {
    pub success: bool,
    pub peer_id: String,
    pub control_port: u16,
    pub video_port: u16,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientConnectResult {
    pub success: bool,
    pub local_ws_port: u16,
    pub target_ip: String,
    pub message: String,
    pub initial_access_level: AccessLevel,
}

pub struct ActiveHost {
    pub peer_id: String,
    pub capturer: Arc<ScreenCapturer>,
    pub host_net: Arc<DirectLanHost>,
    pub stop_flag: Arc<AtomicBool>,
    pub control_port: u16,
    pub video_port: u16,
}

impl ActiveHost {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.host_net.stop();
    }
}

impl Drop for ActiveHost {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct ActiveClient {
    pub client: Arc<DirectLanClient>,
    pub stop_flag: Arc<AtomicBool>,
    pub local_ws_port: u16,
    pub target_addr: SocketAddr,
    pub bridge_task: Option<tokio::task::JoinHandle<()>>,
}

impl ActiveClient {
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.bridge_task.take() {
            handle.abort();
        }
    }
}

impl Drop for ActiveClient {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct AppEngineState {
    pub peer_id: String,
    pub capturer: Arc<ScreenCapturer>,
    pub host: Arc<Mutex<Option<ActiveHost>>>,
    pub client: Arc<Mutex<Option<ActiveClient>>>,
    pub app_handle: Arc<parking_lot::Mutex<Option<tauri::AppHandle>>>,
    pub pending_requests: Arc<parking_lot::Mutex<HashMap<String, oneshot::Sender<(bool, AccessLevel)>>>>,
    pub default_access_level: Arc<parking_lot::RwLock<AccessLevel>>,
}

impl AppEngineState {
    pub fn new() -> Self {
        let peer_id = PeerId::load_or_create().0;
        let capturer = Arc::new(
            ScreenCapturer::new()
                .or_else(|_| ScreenCapturer::new_with_monitor_index(1))
                .unwrap_or_else(|e| {
                    eprintln!("[open-remote] Warning: ScreenCapturer init error ({:?}), creating primary fallback...", e);
                    ScreenCapturer::enumerate_monitors();
                    ScreenCapturer::new().unwrap_or_else(|_| {
                        panic!("Failed to initialize screen capture subsystem: {:?}", e);
                    })
                })
        );
        Self {
            peer_id,
            capturer,
            host: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            app_handle: Arc::new(parking_lot::Mutex::new(None)),
            pending_requests: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            default_access_level: Arc::new(parking_lot::RwLock::new(AccessLevel::Standard)),
        }
    }

    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        *self.app_handle.lock() = Some(handle);
    }
}

async fn internal_start_hosting(
    state: &AppEngineState,
    port: u16,
) -> Result<HostStatus, String> {
    let mut host_guard = state.host.lock().await;
    if host_guard.is_some() {
        return Ok(HostStatus {
            success: true,
            peer_id: state.peer_id.clone(),
            control_port: port,
            video_port: port + 1,
            message: "Host is already running".to_string(),
        });
    }

    let capturer = Arc::clone(&state.capturer);
    let host_net = Arc::new(DirectLanHost::with_peer_id(port, state.peer_id.clone(), Arc::clone(&capturer)));
    host_net.set_default_access_level(*state.default_access_level.read());

    // Hook prompt callback for incoming connection requests
    let app_handle_opt = Arc::clone(&state.app_handle);
    let pending_requests_map = Arc::clone(&state.pending_requests);
    let def_level_lock = Arc::clone(&state.default_access_level);

    host_net.set_prompt_callback(move |info: IncomingRequestInfo| {
        let app_handle_opt = Arc::clone(&app_handle_opt);
        let pending_requests_map = Arc::clone(&pending_requests_map);
        let def_level = *def_level_lock.read();

        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            pending_requests_map.lock().insert(info.request_id.clone(), tx);

            if let Some(app) = app_handle_opt.lock().as_ref() {
                // Restore & focus host window so user notices the connection request dialog
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
                let _ = app.emit("incoming-connection-request", &info);
            }

            // Wait for user to Accept or Decline (60s timeout)
            match tokio::time::timeout(tokio::time::Duration::from_secs(60), rx).await {
                Ok(Ok((accepted, access_level))) => {
                    pending_requests_map.lock().remove(&info.request_id);
                    (accepted, access_level)
                }
                _ => {
                    pending_requests_map.lock().remove(&info.request_id);
                    (false, def_level)
                }
            }
        })
    });

    host_net
        .start_input_listener()
        .await
        .map_err(|e| format!("Failed to bind UDP input listener: {:?}", e))?;

    host_net
        .start_video_stream()
        .await
        .map_err(|e| format!("Failed to start TCP video stream: {:?}", e))?;

    let _ = host_net.start_discovery_responder().await;
    let _ = core_net::start_background_client_discovery().await;

    let stop_flag = Arc::new(AtomicBool::new(false));

    *host_guard = Some(ActiveHost {
        peer_id: state.peer_id.clone(),
        capturer,
        host_net,
        stop_flag,
        control_port: port,
        video_port: port + 1,
    });

    Ok(HostStatus {
        success: true,
        peer_id: state.peer_id.clone(),
        control_port: port,
        video_port: port + 1,
        message: format!("Host active on UDP:{} and TCP:{}", port, port + 1),
    })
}

#[tauri::command]
async fn get_system_info(state: State<'_, AppEngineState>) -> Result<SystemInfo, String> {
    let lan_ip = match local_ip_address::local_ip() {
        Ok(ip) => ip.to_string(),
        Err(_) => "127.0.0.1".to_string(),
    };

    let is_hosting = state.host.lock().await.is_some();
    let is_connected = state.client.lock().await.is_some();

    Ok(SystemInfo {
        peer_id: state.peer_id.clone(),
        lan_ip,
        default_control_port: 44321,
        default_video_port: 44322,
        is_hosting,
        is_connected,
    })
}

#[tauri::command]
async fn start_hosting(
    port: Option<u16>,
    state: State<'_, AppEngineState>,
) -> Result<HostStatus, String> {
    let base_port = port.unwrap_or(44321);
    internal_start_hosting(&state, base_port).await
}

#[tauri::command]
async fn stop_hosting(state: State<'_, AppEngineState>) -> Result<bool, String> {
    let mut host_guard = state.host.lock().await;
    if let Some(host) = host_guard.take() {
        host.stop();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
async fn connect_to_remote(
    target_address: Option<String>,
    target_ip: Option<String>,
    port: Option<u16>,
    state: State<'_, AppEngineState>,
) -> Result<ClientConnectResult, String> {
    let mut client_guard = state.client.lock().await;
    if client_guard.is_some() {
        return Err("A remote session is already active".to_string());
    }

    let input = target_address
        .or(target_ip)
        .ok_or_else(|| "Target address or Peer ID is required".to_string())?;

    let default_port = port.unwrap_or(44321);
    let my_id = state.peer_id.clone();

    // Resolves 9-digit Peer ID via LAN UDP discovery, direct IP, or IP:Port
    let target_addr = resolve_target_address(&input, default_port, Some(&my_id))
        .await
        .map_err(|e| e)?;

    let client = DirectLanClient::connect_with_peer_id(target_addr, my_id)
        .await
        .map_err(|e| format!("Failed to initialize connection to {}: {:?}", target_addr, e))?;
    let client_arc = Arc::new(client);

    // Perform handshake and connect video stream
    let (mut video_stream, initial_access_level) = client_arc
        .connect_video_stream()
        .await
        .map_err(|e| format!("{}", e))?;

    // Save successfully connected Peer ID or IP in recent history
    let mut config = PeerConfig::load();
    config.add_recent(&input, None);

    // Setup local loopback WebSocket server on ephemeral port for ultra-fast binary frame transfer
    let ws_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind local loopback WebSocket: {}", e))?;
    let local_ws_port = ws_listener
        .local_addr()
        .map_err(|e| format!("Failed to get local port: {}", e))?
        .port();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_flag);

    let bridge_task = tokio::spawn(async move {
        if let Ok((raw_stream, _)) = ws_listener.accept().await {
            let _ = raw_stream.set_nodelay(true);
            if let Ok(mut ws_stream) = accept_async(raw_stream).await {
                let encoder = FrameEncoder::new();

                // Send initial permission notification to client frontend
                let init_perm_json = serde_json::to_string(&serde_json::json!({
                    "type": "permission_update",
                    "access_level": initial_access_level,
                })).unwrap_or_default();
                let _ = ws_stream.send(Message::Text(init_perm_json.into())).await;

                while !stop_flag_clone.load(Ordering::Relaxed) {
                    match read_remote_frame(&mut video_stream, &encoder).await {
                        Ok((meta, raw_pixels)) => {
                            // Forward display manifest from remote host to frontend
                            if let Some(ref monitors) = meta.monitors {
                                let displays: Vec<serde_json::Value> = monitors
                                    .iter()
                                    .map(|m| {
                                        serde_json::json!({
                                            "id": m.index,
                                            "name": m.name,
                                            "resolution": format!("{}x{}", m.width, m.height),
                                            "width": m.width,
                                            "height": m.height,
                                            "is_primary": m.is_primary,
                                        })
                                    })
                                    .collect();

                                let manifest_json = serde_json::to_string(&serde_json::json!({
                                    "type": "display_manifest",
                                    "displays": displays,
                                    "active_display_id": meta.active_monitor.unwrap_or(1),
                                })).unwrap_or_default();
                                let _ = ws_stream.send(Message::Text(manifest_json.into())).await;
                            }

                            // Forward live permission updates from host
                            if let Some(lvl) = meta.access_level {
                                let perm_json = serde_json::to_string(&serde_json::json!({
                                    "type": "permission_update",
                                    "access_level": lvl,
                                })).unwrap_or_default();
                                let _ = ws_stream.send(Message::Text(perm_json.into())).await;
                            }

                            // Optimized Packet structure with dirty rect header (36 bytes header + pixels):
                            let mut buffer = Vec::with_capacity(36 + raw_pixels.len());
                            buffer.extend_from_slice(&meta.width.to_be_bytes());
                            buffer.extend_from_slice(&meta.height.to_be_bytes());
                            buffer.extend_from_slice(&meta.timestamp_ms.to_be_bytes());
                            buffer.extend_from_slice(&meta.dirty_x.to_be_bytes());
                            buffer.extend_from_slice(&meta.dirty_y.to_be_bytes());
                            buffer.extend_from_slice(&meta.dirty_w.to_be_bytes());
                            buffer.extend_from_slice(&meta.dirty_h.to_be_bytes());
                            buffer.push(if meta.is_keyframe { 1 } else { 0 });
                            buffer.extend_from_slice(&[0u8; 3]);
                            buffer.extend_from_slice(&raw_pixels);

                            if ws_stream.send(Message::Binary(buffer.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("[open-remote] Stream read ended or closed: {:?}", e);
                            break;
                        }
                    }
                }
            }
        }
    });

    *client_guard = Some(ActiveClient {
        client: Arc::clone(&client_arc),
        stop_flag,
        local_ws_port,
        target_addr,
        bridge_task: Some(bridge_task),
    });

    Ok(ClientConnectResult {
        success: true,
        local_ws_port,
        target_ip: target_addr.to_string(),
        message: format!("Connected to {}", target_addr),
        initial_access_level,
    })
}

#[tauri::command]
async fn respond_connection_request(
    request_id: String,
    accept: bool,
    access_level: AccessLevel,
    state: State<'_, AppEngineState>,
) -> Result<bool, String> {
    let sender = state.pending_requests.lock().remove(&request_id);
    if let Some(tx) = sender {
        let _ = tx.send((accept, access_level));
        if accept {
            if let Some(app) = state.app_handle.lock().as_ref() {
                let _ = app.emit("session-status-changed", ());
            }
        }
        Ok(true)
    } else {
        Err("Connection request has expired or was already handled".to_string())
    }
}

#[tauri::command]
async fn set_session_access_level(
    access_level: AccessLevel,
    state: State<'_, AppEngineState>,
) -> Result<bool, String> {
    let host_guard = state.host.lock().await;
    if let Some(ref host) = *host_guard {
        host.host_net.set_access_level(access_level);
        if let Some(app) = state.app_handle.lock().as_ref() {
            let _ = app.emit("session-status-changed", ());
        }
        Ok(true)
    } else {
        Err("Host is not active".to_string())
    }
}

#[tauri::command]
async fn set_default_access_level(
    access_level: AccessLevel,
    state: State<'_, AppEngineState>,
) -> Result<bool, String> {
    *state.default_access_level.write() = access_level;
    let host_guard = state.host.lock().await;
    if let Some(ref host) = *host_guard {
        host.host_net.set_default_access_level(access_level);
    }
    if let Some(app) = state.app_handle.lock().as_ref() {
        let _ = app.emit("session-status-changed", ());
    }
    Ok(true)
}

#[tauri::command]
async fn get_host_session_state(
    state: State<'_, AppEngineState>,
) -> Result<HostSessionInfo, String> {
    let host_guard = state.host.lock().await;
    let default_level = *state.default_access_level.read();
    if let Some(ref host) = *host_guard {
        let client_opt = host.host_net.active_client();
        let access_level = host.host_net.access_level();
        let (client_peer_id, client_ip) = match client_opt {
            Some((id, ip)) => (Some(id), Some(ip)),
            None => (None, None),
        };
        Ok(HostSessionInfo {
            is_active: client_peer_id.is_some(),
            client_peer_id,
            client_ip,
            access_level,
            default_access_level: default_level,
        })
    } else {
        Ok(HostSessionInfo {
            is_active: false,
            client_peer_id: None,
            client_ip: None,
            access_level: default_level,
            default_access_level: default_level,
        })
    }
}

#[tauri::command]
async fn disconnect_remote(state: State<'_, AppEngineState>) -> Result<bool, String> {
    let mut client_guard = state.client.lock().await;
    if let Some(mut client) = client_guard.take() {
        client.stop();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
async fn send_input(event: InputEvent, state: State<'_, AppEngineState>) -> Result<(), String> {
    let client_guard = state.client.lock().await;
    if let Some(active) = client_guard.as_ref() {
        active
            .client
            .send_input(&event)
            .await
            .map_err(|e| format!("Failed to send input: {:?}", e))?;
    }
    Ok(())
}

#[tauri::command]
fn get_available_monitors() -> Vec<MonitorDescriptor> {
    ScreenCapturer::enumerate_monitors()
}

#[tauri::command]
async fn switch_monitor(
    monitor_index: usize,
    state: State<'_, AppEngineState>,
) -> Result<bool, String> {
    state
        .capturer
        .switch_monitor(monitor_index)
        .map_err(|e| format!("Failed to switch monitor: {:?}", e))?;
    Ok(true)
}

#[tauri::command]
async fn switch_remote_monitor(
    monitor_index: usize,
    state: State<'_, AppEngineState>,
) -> Result<bool, String> {
    let client_guard = state.client.lock().await;
    if let Some(active) = client_guard.as_ref() {
        active
            .client
            .send_input(&InputEvent::SwitchMonitor { monitor_index })
            .await
            .map_err(|e| format!("Failed to switch remote monitor: {:?}", e))?;
        Ok(true)
    } else {
        Err("No active remote session".to_string())
    }
}

#[tauri::command]
fn get_recent_sessions() -> Vec<RecentSession> {
    PeerConfig::load().recent_sessions
}

#[tauri::command]
fn save_recent_session(peer_id: String, alias: Option<String>) -> Result<(), String> {
    let mut config = PeerConfig::load();
    config.add_recent(&peer_id, alias);
    Ok(())
}

#[tauri::command]
fn remove_recent_session(peer_id: String) -> Result<(), String> {
    let mut config = PeerConfig::load();
    config.remove_recent(&peer_id);
    Ok(())
}

#[tauri::command]
fn app_minimize(#[allow(unused_variables)] window: tauri::Window) {
    #[cfg(desktop)]
    let _ = window.minimize();
}

#[tauri::command]
fn app_toggle_maximize(#[allow(unused_variables)] window: tauri::Window) {
    #[cfg(desktop)]
    if let Ok(is_max) = window.is_maximized() {
        if is_max {
            let _ = window.unmaximize();
        } else {
            let _ = window.maximize();
        }
    }
}

fn shutdown_services(state: &AppEngineState) {
    if let Ok(mut host_guard) = state.host.try_lock() {
        if let Some(host) = host_guard.take() {
            host.stop();
        }
    }
    if let Ok(mut client_guard) = state.client.try_lock() {
        if let Some(mut client) = client_guard.take() {
            client.stop();
        }
    }
    core_net::stop_background_client_discovery();
}

#[tauri::command]
fn app_close(#[allow(unused_variables)] window: tauri::Window) {
    #[cfg(desktop)]
    let _ = window.hide();
}

#[tauri::command]
async fn app_exit_completely(app: tauri::AppHandle, state: State<'_, AppEngineState>) -> Result<(), String> {
    shutdown_services(&state);
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn app_start_dragging(#[allow(unused_variables)] window: tauri::Window) {
    #[cfg(desktop)]
    let _ = window.start_dragging();
}

#[cfg(target_os = "windows")]
fn cleanup_orphaned_instances() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let current_pid = std::process::id();
    let script = format!(
        "Get-Process -Name 'OpenRemote', 'open-remote-gui' -ErrorAction SilentlyContinue | Where-Object {{ $_.Id -ne {} }} | Stop-Process -Force -ErrorAction SilentlyContinue",
        current_pid
    );
    let _ = std::process::Command::new("powershell")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .output();
}

#[cfg(target_os = "windows")]
fn ensure_firewall_rules() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    // Add Windows Firewall rules silently for ports 44320 & 44321
    std::thread::spawn(|| {
        let rules = [
            ("OpenRemote LAN Discovery", "UDP", "44320"),
            ("OpenRemote Control & Video", "TCP", "44321,44322"),
            ("OpenRemote Control UDP", "UDP", "44321"),
        ];
        for (name, protocol, port) in rules {
            let _ = std::process::Command::new("netsh")
                .creation_flags(CREATE_NO_WINDOW)
                .args([
                    "advfirewall",
                    "firewall",
                    "add",
                    "rule",
                    &format!("name={}", name),
                    "dir=in",
                    "action=allow",
                    &format!("protocol={}", protocol),
                    &format!("localport={}", port),
                    "profile=any",
                ])
                .output();
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = AppEngineState::new();

    tauri::Builder::default()
        .manage(app_state)
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "windows")]
            {
                cleanup_orphaned_instances();
                ensure_firewall_rules();
            }

            // Automatically and persistently start host listening server & discovery on boot via internal_start_hosting
            let app_handle = app.handle().clone();
            {
                let state = app_handle.state::<AppEngineState>();
                state.set_app_handle(app_handle.clone());
            }
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppEngineState>();
                if let Err(e) = internal_start_hosting(&state, 44321).await {
                    eprintln!("[open-remote] Host auto-start notice: {}", e);
                } else {
                    eprintln!("[open-remote] Host server and discovery responder auto-started successfully on port 44321");
                }
            });

            #[cfg(desktop)]
            {
                // Tray icon and menu setup
                let open_item = tauri::menu::MenuItem::with_id(app, "open", "Open OpenRemote", true, None::<&str>)?;
                let status_item = tauri::menu::MenuItem::with_id(app, "status", "Status: Ready (Listening on port 44321)", false, None::<&str>)?;
                let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
                let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
                let quit_item = tauri::menu::MenuItem::with_id(app, "quit", "Quit / Exit Completely", true, None::<&str>)?;

                let tray_menu = tauri::menu::Menu::with_items(app, &[
                    &open_item,
                    &sep1,
                    &status_item,
                    &sep2,
                    &quit_item,
                ])?;

                let mut tray_builder = tauri::tray::TrayIconBuilder::new()
                    .tooltip("OpenRemote - High Performance Remote Desktop")
                    .menu(&tray_menu)
                    .show_menu_on_left_click(false);

                if let Some(icon) = app.default_window_icon() {
                    tray_builder = tray_builder.icon(icon.clone());
                }

                let _tray = tray_builder
                    .on_menu_event(|app, event| {
                        match event.id.as_ref() {
                            "open" => {
                                if let Some(window) = app.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.unminimize();
                                    let _ = window.set_focus();
                                }
                            }
                            "quit" => {
                                let state = app.state::<AppEngineState>();
                                shutdown_services(&state);
                                app.exit(0);
                            }
                            _ => {}
                        }
                    })
                    .on_tray_icon_event(|tray, event| {
                        match event {
                            tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, button_state: tauri::tray::MouseButtonState::Up, .. } => {
                                let app = tray.app_handle();
                                if let Some(window) = app.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.unminimize();
                                    let _ = window.set_focus();
                                }
                            }
                            tauri::tray::TrayIconEvent::DoubleClick { button: tauri::tray::MouseButton::Left, .. } => {
                                let app = tray.app_handle();
                                if let Some(window) = app.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.unminimize();
                                    let _ = window.set_focus();
                                }
                            }
                            _ => {}
                        }
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                #[cfg(desktop)]
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                tauri::WindowEvent::Destroyed => {
                    let app = window.app_handle();
                    let state = app.state::<AppEngineState>();
                    shutdown_services(&state);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_system_info,
            start_hosting,
            stop_hosting,
            connect_to_remote,
            disconnect_remote,
            send_input,
            get_available_monitors,
            switch_monitor,
            switch_remote_monitor,
            get_recent_sessions,
            save_recent_session,
            remove_recent_session,
            respond_connection_request,
            set_session_access_level,
            set_default_access_level,
            get_host_session_state,
            app_minimize,
            app_toggle_maximize,
            app_close,
            app_exit_completely,
            app_start_dragging
        ])
        .run(tauri::generate_context!())
        .expect("error while running OpenRemote desktop application");
}
