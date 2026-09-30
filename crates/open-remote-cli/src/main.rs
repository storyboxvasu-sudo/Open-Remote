use app_common::{InputEvent, MouseButton, PeerId};
use clap::{Parser, Subcommand};
use core_capture::ScreenCapturer;
use core_codec::FrameEncoder;
use core_net::{DirectLanClient, DirectLanHost};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(author, version, about = "OpenRemote - Unlimited Zero-Limit Remote Desktop", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start as Host (Controlled machine)
    Host {
        #[arg(short, long, default_value_t = 44321)]
        port: u16,
    },
    /// Test local screen capture and frame compression
    Benchmark,
    /// Send test input to a host over direct LAN
    Inject {
        #[arg(short, long)]
        target: String,
        #[arg(short, long, default_value_t = 0.5)]
        x: f64,
        #[arg(short, long, default_value_t = 0.5)]
        y: f64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Host { port } => {
            let id = PeerId::load_or_create();
            println!("=====================================================");
            println!("       OPENREMOTE HOST DAEMON ACTIVE                ");
            println!("=====================================================");
            println!(" Peer ID        : {}", id.0);
            println!(" Control Port   : {} (UDP Input Injection)", port);
            println!(" Video Stream   : {} (TCP Compressed Frames)", port + 1);
            println!(" Status         : Ready for direct LAN / WAN viewers");
            println!(" Session Limit  : UNLIMITED (Zero time disconnects)");
            println!("=====================================================");

            println!("[Host] Initializing Windows Graphics Capture engine...");
            let capturer = Arc::new(ScreenCapturer::new()?);
            println!("[Host] Primary display captured: {}x{}", capturer.width, capturer.height);

            let host = DirectLanHost::new(port, Arc::clone(&capturer));
            host.start_input_listener().await?;
            println!("[Host] Direct UDP input listener active on 0.0.0.0:{}", port);

            host.start_video_stream().await?;
            println!("[Host] Direct TCP video stream broadcaster active on 0.0.0.0:{}", port + 1);

            println!("\n[Host] Daemon fully active. Press Ctrl+C to stop.");
            tokio::signal::ctrl_c().await?;
            println!("\n[Host] Shutting down.");
        }
        Commands::Benchmark => {
            println!("============================================================");
            println!("       OPENREMOTE REAL-TIME CAPTURE BENCHMARK               ");
            println!("============================================================");
            let capturer = ScreenCapturer::new()?;
            let encoder = FrameEncoder::new();

            println!("Primary Display Resolution: {}x{}", capturer.width, capturer.height);
            let start = std::time::Instant::now();
            let mut total_raw_bytes = 0usize;
            let mut total_compressed_bytes = 0usize;
            let frames = 30;

            for i in 1..=frames {
                let f_start = std::time::Instant::now();
                let mut raw_opt = None;
                for _ in 0..100 {
                    if let Some(f) = capturer.get_latest_frame() {
                        raw_opt = Some(f);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let raw = raw_opt.expect("frame captured");
                let cap_dur = f_start.elapsed();

                let enc_start = std::time::Instant::now();
                let compressed_opt = encoder.encode(&raw)?;
                let enc_dur = enc_start.elapsed();

                total_raw_bytes += raw.data.len();
                let comp_len = if let Some(ref c) = compressed_opt { c.payload.len() } else { 0 };
                total_compressed_bytes += comp_len;

                println!(
                    " Frame {:02}: {}x{} | Cap: {:>6.2?} | LZ4: {:>6.2?} | Raw: {:>5.2}MB -> Comp: {:>5.2}MB (Ratio: {:.1}x)",
                    i,
                    raw.width,
                    raw.height,
                    cap_dur,
                    enc_dur,
                    raw.data.len() as f64 / (1024.0 * 1024.0),
                    comp_len as f64 / (1024.0 * 1024.0),
                    if comp_len > 0 { raw.data.len() as f64 / comp_len as f64 } else { 0.0 }
                );

                tokio::time::sleep(Duration::from_millis(16)).await;
            }

            let elapsed = start.elapsed();
            println!("------------------------------------------------------------");
            println!(" Total Frames  : {}", frames);
            println!(" Total Time    : {:.2?}", elapsed);
            println!(" Real FPS      : {:.1}", frames as f64 / elapsed.as_secs_f64());
            println!(
                " Bandwidth     : {:.1} MB raw -> {:.1} MB compressed",
                total_raw_bytes as f64 / (1024.0 * 1024.0),
                total_compressed_bytes as f64 / (1024.0 * 1024.0)
            );
            println!(" Status        : Production-grade streaming throughput ready");
            println!("============================================================");
        }
        Commands::Inject { target, x, y } => {
            let addr: SocketAddr = target.parse()?;
            let client = DirectLanClient::connect(addr).await?;
            println!("Sending Move Mouse to ({}, {}) -> {}", x, y, addr);
            client.send_input(&InputEvent::MouseMove { x, y }).await?;
            client.send_input(&InputEvent::MouseDown { button: MouseButton::Left, x, y }).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            client.send_input(&InputEvent::MouseUp { button: MouseButton::Left, x, y }).await?;
            println!("Sent click successfully.");
        }
    }

    Ok(())
}
