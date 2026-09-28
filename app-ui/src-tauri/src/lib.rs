use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use tauri::State;

use app_common::{InputEvent, PeerId};
use core_capture::ScreenCapturer;
use core_codec::FrameEncoder;
use core_net::{read_remote_frame, DirectLanClient, DirectLanHost};

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
}

#[allow(dead_code)]
struct ActiveHost {
    _capturer: Arc<ScreenCapturer>,
    _stop_flag: Arc<AtomicBool>,
    control_port: u16,
    video_port: u16,
}

#[allow(dead_code)]
struct ActiveClient {
    client: Arc<DirectLanClient>,
    stop_flag: Arc<AtomicBool>,
    local_ws_port: u16,
    target_addr: SocketAddr,
}

pub struct AppEngineState {
    peer_id: String,
    host: Arc<Mutex<Option<ActiveHost>>>,
    client: Arc<Mutex<Option<ActiveClient>>>,
}

impl AppEngineState {
    pub fn new() -> Self {
        Self {
            peer_id: PeerId::generate().0,
            host: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
        }
    }
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
    let mut host_guard = state.host.lock().await;
    if host_guard.is_some() {
        return Ok(HostStatus {
            success: true,
            control_port: 44321,
            video_port: 44322,
            message: "Host is already running".to_string(),
        });
    }

    let base_port = port.unwrap_or(44321);
    let capturer = ScreenCapturer::new()
        .map_err(|e| format!("Failed to initialize GPU screen capturer: {:?}", e))?;
    let capturer_arc = Arc::new(capturer);

    let host = Arc::new(DirectLanHost::new(base_port));

    host.start_input_listener()
        .await
        .map_err(|e| format!("Failed to bind UDP input listener: {:?}", e))?;

    host.start_video_stream(Arc::clone(&capturer_arc))
        .await
        .map_err(|e| format!("Failed to start TCP video stream: {:?}", e))?;

    let stop_flag = Arc::new(AtomicBool::new(false));

    *host_guard = Some(ActiveHost {
        _capturer: capturer_arc,
        _stop_flag: stop_flag,
        control_port: base_port,
        video_port: base_port + 1,
    });

    Ok(HostStatus {
        success: true,
        control_port: base_port,
        video_port: base_port + 1,
        message: format!("Host active on UDP:{} and TCP:{}", base_port, base_port + 1),
    })
}

#[tauri::command]
async fn stop_hosting(state: State<'_, AppEngineState>) -> Result<bool, String> {
    let mut host_guard = state.host.lock().await;
    if let Some(host) = host_guard.take() {
        host._stop_flag.store(true, Ordering::Relaxed);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
async fn connect_to_remote(
    target_ip: String,
    port: Option<u16>,
    state: State<'_, AppEngineState>,
) -> Result<ClientConnectResult, String> {
    let mut client_guard = state.client.lock().await;
    if client_guard.is_some() {
        return Err("A remote session is already active".to_string());
    }

    let base_port = port.unwrap_or(44321);
    let target_addr: SocketAddr = format!("{}:{}", target_ip, base_port)
        .parse()
        .map_err(|e| format!("Invalid target address: {}", e))?;

    let client = DirectLanClient::connect(target_addr)
        .await
        .map_err(|e| format!("Failed to initialize LAN client: {:?}", e))?;
    let client_arc = Arc::new(client);

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
    let client_for_stream = Arc::clone(&client_arc);

    // Spawn background task to pipe remote TCP video stream -> local WebSocket client
    tokio::spawn(async move {
        // Connect to remote video stream
        let mut video_stream = match client_for_stream.connect_video_stream().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[open-remote] Failed to connect to remote video stream: {:?}", e);
                return;
            }
        };

        // Accept connection from frontend Svelte canvas
        if let Ok((raw_stream, _)) = ws_listener.accept().await {
            if let Ok(mut ws_stream) = accept_async(raw_stream).await {
                let encoder = FrameEncoder::new();

                while !stop_flag_clone.load(Ordering::Relaxed) {
                    match read_remote_frame(&mut video_stream, &encoder).await {
                        Ok((meta, raw_pixels)) => {
                            // Packet structure:
                            // [width: u32 (4B)][height: u32 (4B)][timestamp: u64 (8B)][raw RGBA pixels]
                            let mut buffer = Vec::with_capacity(16 + raw_pixels.len());
                            buffer.extend_from_slice(&meta.width.to_be_bytes());
                            buffer.extend_from_slice(&meta.height.to_be_bytes());
                            buffer.extend_from_slice(&meta.timestamp_ms.to_be_bytes());
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
    });

    Ok(ClientConnectResult {
        success: true,
        local_ws_port,
        target_ip,
        message: "Connected successfully".to_string(),
    })
}

#[tauri::command]
async fn disconnect_remote(state: State<'_, AppEngineState>) -> Result<bool, String> {
    let mut client_guard = state.client.lock().await;
    if let Some(client) = client_guard.take() {
        client.stop_flag.store(true, Ordering::Relaxed);
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppEngineState::new())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_system_info,
            start_hosting,
            stop_hosting,
            connect_to_remote,
            disconnect_remote,
            send_input
        ])
        .run(tauri::generate_context!())
        .expect("error while running OpenRemote desktop application");
}
