use app_common::{InputEvent, SignalingMessage};
use core_capture::ScreenCapturer;
use core_codec::FrameEncoder;
use core_input::InputInjector;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
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
}

/// Host Direct LAN Server: binds a local port and listens for direct peer connections
pub struct DirectLanHost {
    control_port: u16,
    video_port: u16,
    injector: Arc<InputInjector>,
}

impl DirectLanHost {
    pub fn new(base_port: u16) -> Self {
        Self {
            control_port: base_port,
            video_port: base_port + 1,
            injector: Arc::new(InputInjector::new()),
        }
    }

    /// Starts the background listener for incoming control events over direct LAN UDP
    pub async fn start_input_listener(&self) -> Result<(), NetError> {
        let addr = format!("0.0.0.0:{}", self.control_port);
        let socket = UdpSocket::bind(&addr).await?;
        let injector = Arc::clone(&self.injector);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, _peer_addr)) => {
                        if let Ok(event) = serde_json::from_slice::<InputEvent>(&buf[..len]) {
                            let _ = injector.inject(&event);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(())
    }

    /// Starts the video streaming TCP listener, broadcasting compressed desktop frames to connected viewers
    pub async fn start_video_stream(&self, capturer: Arc<ScreenCapturer>) -> Result<(), NetError> {
        let addr = format!("0.0.0.0:{}", self.video_port);
        let listener = TcpListener::bind(&addr).await?;
        let encoder = Arc::new(FrameEncoder::new());

        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let cap = Arc::clone(&capturer);
                    let enc = Arc::clone(&encoder);

                    tokio::spawn(async move {
                        loop {
                            if let Some(raw_frame) = cap.next_frame() {
                                if let Ok(compressed) = enc.encode(&raw_frame) {
                                    let meta_bytes = serde_json::to_vec(&compressed.meta).unwrap_or_default();
                                    let meta_len = meta_bytes.len() as u32;
                                    let payload_len = compressed.payload.len() as u32;

                                    if stream.write_all(&meta_len.to_be_bytes()).await.is_err() {
                                        break;
                                    }
                                    if stream.write_all(&meta_bytes).await.is_err() {
                                        break;
                                    }
                                    if stream.write_all(&payload_len.to_be_bytes()).await.is_err() {
                                        break;
                                    }
                                    if stream.write_all(&compressed.payload).await.is_err() {
                                        break;
                                    }
                                }
                            } else {
                                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                            }
                        }
                    });
                }
            }
        });

        Ok(())
    }
}

/// Controller Direct LAN Client: connects directly to a target IP:Port over the local network
pub struct DirectLanClient {
    target_addr: SocketAddr,
    control_socket: UdpSocket,
}

impl DirectLanClient {
    pub async fn connect(target_addr: SocketAddr) -> Result<Self, NetError> {
        let control_socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self {
            target_addr,
            control_socket,
        })
    }

    pub async fn send_input(&self, event: &InputEvent) -> Result<(), NetError> {
        let payload = serde_json::to_vec(event)?;
        self.control_socket.send_to(&payload, self.target_addr).await?;
        Ok(())
    }

    pub async fn connect_video_stream(&self) -> Result<TcpStream, NetError> {
        let video_addr = SocketAddr::new(self.target_addr.ip(), self.target_addr.port() + 1);
        let stream = TcpStream::connect(video_addr).await?;
        Ok(stream)
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
    let mut meta_len_buf = [0u8; 4];
    stream.read_exact(&mut meta_len_buf).await?;
    let meta_len = u32::from_be_bytes(meta_len_buf) as usize;

    let mut meta_buf = vec![0u8; meta_len];
    stream.read_exact(&mut meta_buf).await?;
    let meta: app_common::FrameMeta = serde_json::from_slice(&meta_buf)?;

    let mut payload_len_buf = [0u8; 4];
    stream.read_exact(&mut payload_len_buf).await?;
    let payload_len = u32::from_be_bytes(payload_len_buf) as usize;

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
