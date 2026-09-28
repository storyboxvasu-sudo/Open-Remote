use serde::{Deserialize, Serialize};

/// Unique 9-digit peer identification
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

impl PeerId {
    pub fn generate() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let num: u32 = rng.gen_range(100_000_000..999_999_999);
        Self(format!("{}-{}-{}", &num.to_string()[0..3], &num.to_string()[3..6], &num.to_string()[6..9]))
    }
}

/// Remote control input messages sent over WebRTC DataChannel / Direct UDP
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum InputEvent {
    MouseMove {
        /// Normalized X coordinate: 0.0 (left) to 1.0 (right)
        x: f64,
        /// Normalized Y coordinate: 0.0 (top) to 1.0 (bottom)
        y: f64,
    },
    MouseDown {
        button: MouseButton,
        x: f64,
        y: f64,
    },
    MouseUp {
        button: MouseButton,
        x: f64,
        y: f64,
    },
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
    KeyDown {
        scancode: u32,
        key: String,
    },
    KeyUp {
        scancode: u32,
        key: String,
    },
    ClipboardSync {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Video frame descriptor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMeta {
    pub width: u32,
    pub height: u32,
    pub timestamp_ms: u64,
    pub is_keyframe: bool,
}

/// Signaling protocol envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", content = "payload")]
pub enum SignalingMessage {
    Register { peer_id: String },
    Registered { peer_id: String, success: bool },
    Offer { target: String, sdp: String },
    Answer { target: String, sdp: String },
    Candidate { target: String, candidate: String },
    Ping,
    Pong,
}
