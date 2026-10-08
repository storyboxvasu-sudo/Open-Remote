use serde::{Deserialize, Serialize};

use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentSession {
    pub peer_id: String,
    pub alias: Option<String>,
    pub last_connected_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorDescriptor {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub is_primary: bool,
}

impl From<MonitorDescriptor> for DisplayInfo {
    fn from(m: MonitorDescriptor) -> Self {
        Self {
            id: m.index,
            name: m.name,
            width: m.width,
            height: m.height,
            x_offset: m.x,
            y_offset: m.y,
            is_primary: m.is_primary,
        }
    }
}

impl From<DisplayInfo> for MonitorDescriptor {
    fn from(d: DisplayInfo) -> Self {
        Self {
            index: d.id,
            name: d.name,
            width: d.width,
            height: d.height,
            is_primary: d.is_primary,
            x: d.x_offset,
            y: d.y_offset,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHandshake {
    pub host_peer_id: String,
    pub monitors: Vec<MonitorDescriptor>,
    pub active_monitor_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnattendedAccessConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub salt: Option<String>,
    #[serde(default)]
    pub profile: AccessLevel,
}

impl Default for UnattendedAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            password_hash: None,
            salt: None,
            profile: AccessLevel::Standard,
        }
    }
}

pub fn generate_salt() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let random_bytes: [u8; 16] = rng.gen();
    hex::encode(random_bytes)
}

pub fn hash_password(password: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn verify_password(password: &str, salt: &str, expected_hash: &str) -> bool {
    let computed = hash_password(password, salt);
    computed.eq_ignore_ascii_case(expected_hash)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub peer_id: String,
    #[serde(default)]
    pub machine_guid: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub recent_sessions: Vec<RecentSession>,
    #[serde(default)]
    pub unattended_access: UnattendedAccessConfig,
    #[serde(default)]
    pub signaling_server: Option<String>,
}

impl PeerConfig {
    pub fn load() -> Self {
        let path = PeerId::config_path();
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = serde_json::from_str::<PeerConfig>(&data) {
                    return cfg;
                }
            }
        }
        let peer_id = PeerId::load_or_create();
        PeerConfig {
            peer_id: peer_id.0,
            machine_guid: PeerId::get_machine_guid(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            recent_sessions: Vec::new(),
            unattended_access: UnattendedAccessConfig::default(),
            signaling_server: None,
        }
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = PeerId::config_path();
        let dir = PeerId::config_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, json)
    }

    pub fn add_recent(&mut self, peer_id: &str, alias: Option<String>) {
        let clean_id = peer_id.trim();
        if clean_id.is_empty() {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Remove if exists
        self.recent_sessions.retain(|s| s.peer_id != clean_id);

        // Prepend as most recent
        self.recent_sessions.insert(
            0,
            RecentSession {
                peer_id: clean_id.to_string(),
                alias,
                last_connected_at: now,
            },
        );

        // Limit to max 20 recent sessions
        if self.recent_sessions.len() > 20 {
            self.recent_sessions.truncate(20);
        }

        let _ = self.save();
    }

    pub fn remove_recent(&mut self, peer_id: &str) {
        let clean_id = peer_id.trim();
        self.recent_sessions.retain(|s| s.peer_id != clean_id);
        let _ = self.save();
    }
}

/// Unique 9-digit peer identification
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

impl PeerId {
    /// Generates a random 9-digit Peer ID in format XXX-XXX-XXX
    pub fn generate() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let num: u32 = rng.gen_range(100_000_000..999_999_999);
        Self(format!("{}-{}-{}", &num.to_string()[0..3], &num.to_string()[3..6], &num.to_string()[6..9]))
    }

    /// Derives a deterministic 9-digit ID from a unique machine identifier (e.g., MachineGuid)
    pub fn derive_from_hardware(seed: &str) -> Self {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in seed.as_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let num = 100_000_000 + (hash % 900_000_000);
        let s = format!("{:09}", num);
        Self(format!("{}-{}-{}", &s[0..3], &s[3..6], &s[6..9]))
    }

    /// Returns the persistent configuration directory for OpenRemote
    pub fn config_dir() -> PathBuf {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local_app_data).join("open-remote")
        } else if let Ok(app_data) = std::env::var("APPDATA") {
            PathBuf::from(app_data).join("open-remote")
        } else if let Ok(user_profile) = std::env::var("USERPROFILE") {
            PathBuf::from(user_profile).join("AppData").join("Local").join("open-remote")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".config").join("open-remote")
        } else {
            PathBuf::from(".").join(".open-remote")
        }
    }

    /// Returns the full path to config.json
    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    /// Reads MachineGuid from the Windows registry if available
    #[cfg(target_os = "windows")]
    pub fn get_machine_guid() -> Option<String> {
        use winreg::enums::*;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let crypto = hklm.open_subkey("SOFTWARE\\Microsoft\\Cryptography").ok()?;
        let guid: String = crypto.get_value("MachineGuid").ok()?;
        if guid.trim().is_empty() {
            None
        } else {
            Some(guid)
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn get_machine_guid() -> Option<String> {
        None
    }

    /// Loads the permanent Peer ID from local storage, or generates and persists a new one
    pub fn load_or_create() -> Self {
        let path = Self::config_path();

        // 1. Attempt to load existing config
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(config) = serde_json::from_str::<PeerConfig>(&data) {
                    if !config.peer_id.trim().is_empty() {
                        return Self(config.peer_id);
                    }
                }
            }
        }

        // 2. Generate permanent ID (prefer deterministic hardware GUID, fallback to random)
        let guid = Self::get_machine_guid();
        let peer_id = match &guid {
            Some(hardware_id) => Self::derive_from_hardware(hardware_id),
            None => Self::generate(),
        };

        // 3. Persist to disk
        let config = PeerConfig {
            peer_id: peer_id.0.clone(),
            machine_guid: guid,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            recent_sessions: Vec::new(),
            unattended_access: UnattendedAccessConfig::default(),
            signaling_server: None,
        };

        let dir = Self::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(&config) {
            let _ = std::fs::write(&path, json);
        }

        peer_id
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
    SwitchMonitor {
        monitor_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessLevel {
    ViewOnly,
    Standard,
    FullAccess,
}

impl Default for AccessLevel {
    fn default() -> Self {
        Self::Standard
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub challenge: String,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    pub success: bool,
    pub profile: Option<AccessLevel>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionHandshakeRequest {
    pub client_peer_id: String,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub auth_response: Option<AuthResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionHandshakeResponse {
    pub accepted: bool,
    pub access_level: AccessLevel,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub auth_challenge: Option<AuthChallenge>,
    #[serde(default)]
    pub auth_result: Option<AuthResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingRequestInfo {
    pub request_id: String,
    pub client_peer_id: String,
    pub client_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSessionInfo {
    pub is_active: bool,
    pub client_peer_id: Option<String>,
    pub client_ip: Option<String>,
    pub access_level: AccessLevel,
    pub default_access_level: AccessLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMeta {
    pub width: u32,
    pub height: u32,
    pub timestamp_ms: u64,
    pub is_keyframe: bool,
    #[serde(default)]
    pub dirty_x: u32,
    #[serde(default)]
    pub dirty_y: u32,
    #[serde(default)]
    pub dirty_w: u32,
    #[serde(default)]
    pub dirty_h: u32,
    #[serde(default)]
    pub monitors: Option<Vec<MonitorDescriptor>>,
    #[serde(default)]
    pub active_monitor: Option<usize>,
    #[serde(default)]
    pub access_level: Option<AccessLevel>,
}

/// Signaling protocol envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", content = "payload")]
pub enum SignalingMessage {
    Register {
        peer_id: String,
    },
    Registered {
        peer_id: String,
        success: bool,
        #[serde(default)]
        active_peers: Option<usize>,
    },
    Lookup {
        target: String,
    },
    LookupResult {
        target: String,
        online: bool,
    },
    Offer {
        target: String,
        sdp: String,
        #[serde(default)]
        from: Option<String>,
    },
    Answer {
        target: String,
        sdp: String,
        #[serde(default)]
        from: Option<String>,
    },
    Candidate {
        target: String,
        candidate: serde_json::Value,
        #[serde(default)]
        from: Option<String>,
    },
    PeerNotFound {
        target: String,
        reason: String,
    },
    Ping,
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_id_deterministic_from_hardware() {
        let guid = "23c35349-e58f-4cc5-8e6b-4694f3eadce7";
        let id1 = PeerId::derive_from_hardware(guid);
        let id2 = PeerId::derive_from_hardware(guid);
        assert_eq!(id1, id2);
        assert_eq!(id1.0.len(), 11); // 9 digits + 2 hyphens = 11 chars
        assert_eq!(&id1.0[3..4], "-");
        assert_eq!(&id1.0[7..8], "-");
    }

    #[test]
    fn test_peer_id_load_or_create() {
        let peer_id = PeerId::load_or_create();
        assert_eq!(peer_id.0.len(), 11);
        let path = PeerId::config_path();
        assert!(path.exists(), "Config file must exist after load_or_create");

        // Subsequent call must return identical peer_id
        let peer_id2 = PeerId::load_or_create();
        assert_eq!(peer_id, peer_id2);
    }
}
