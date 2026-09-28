use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{self, JoinHandle};
use thiserror::Error;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("No primary monitor detected: {0}")]
    MonitorNotFound(String),
    #[error("Capture pipeline error: {0}")]
    PipelineError(String),
    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

struct FrameReceiverHandler {
    sender: Sender<RawFrame>,
}

impl GraphicsCaptureApiHandler for FrameReceiverHandler {
    type Flags = Sender<RawFrame>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            sender: ctx.flags,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let w = frame.width();
        let h = frame.height();

        if let Ok(mut raw_buffer) = frame.buffer() {
            let raw_slice = raw_buffer.as_raw_buffer();
            let _ = self.sender.send(RawFrame {
                width: w,
                height: h,
                data: raw_slice.to_vec(),
            });
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct ScreenCapturer {
    receiver: std::sync::Mutex<Receiver<RawFrame>>,
    _thread: JoinHandle<()>,
    pub width: u32,
    pub height: u32,
}

impl ScreenCapturer {
    pub fn new() -> Result<Self, CaptureError> {
        #[cfg(windows)]
        {
            let monitor = Monitor::primary()
                .map_err(|e| CaptureError::MonitorNotFound(format!("{:?}", e)))?;
            let (tx, rx) = channel::<RawFrame>();

            let settings = Settings::new(
                monitor,
                CursorCaptureSettings::Default,
                DrawBorderSettings::Default,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                tx,
            );

            let _thread = thread::spawn(move || {
                let _ = FrameReceiverHandler::start(settings);
            });

            // Wait for the first frame to determine dimensions
            let first_frame = rx
                .recv()
                .map_err(|e| CaptureError::PipelineError(e.to_string()))?;
            let width = first_frame.width;
            let height = first_frame.height;

            // Re-channel the first frame back so it's not lost
            let (new_tx, new_rx) = channel::<RawFrame>();
            let _ = new_tx.send(first_frame);

            // Forward remaining frames
            let forward_thread = thread::spawn(move || {
                while let Ok(frame) = rx.recv() {
                    if new_tx.send(frame).is_err() {
                        break;
                    }
                }
            });

            Ok(Self {
                receiver: std::sync::Mutex::new(new_rx),
                _thread: forward_thread,
                width,
                height,
            })
        }
        #[cfg(not(windows))]
        {
            Err(CaptureError::UnsupportedPlatform)
        }
    }

    pub fn next_frame(&self) -> Option<RawFrame> {
        self.receiver.lock().ok()?.try_recv().ok()
    }

    pub fn next_frame_blocking(&self) -> Result<RawFrame, CaptureError> {
        self.receiver
            .lock()
            .map_err(|e| CaptureError::PipelineError(e.to_string()))?
            .recv()
            .map_err(|e| CaptureError::PipelineError(e.to_string()))
    }
}
