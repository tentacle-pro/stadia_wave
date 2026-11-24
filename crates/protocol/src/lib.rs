use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 3;
pub const SERVICE_TYPE: &str = "_stadia-wave._tcp.local.";
pub const DEFAULT_PORT: u16 = 8787;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invitation {
    pub bridge_id: String,
    pub certificate_pem: String,
    pub pairing_token: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub bridge_id: String,
    pub certificate_pem: String,
    pub companion_id: String,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    IssuePairingCode,
    PairingHello {
        message: Vec<u8>,
    },
    LocalConnect {
        version: u32,
        takeover: bool,
    },
    Pair {
        version: u32,
        companion_id: String,
        name: String,
        token: String,
        takeover: bool,
    },
    Connect {
        version: u32,
        companion_id: String,
        token: String,
        takeover: bool,
    },
    Rumble {
        session_id: String,
        request_id: String,
        controller: String,
    },
    AudioReceived {
        session_id: String,
        recording_id: String,
    },
    Ping {
        session_id: String,
        request_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    PairingCode {
        code: String,
    },
    Paired {
        token: String,
    },
    Connected {
        bridge_id: String,
        session_id: String,
    },
    Status {
        bridge_name: String,
    },
    Hardware {
        session_id: String,
        snapshot: HardwareState,
    },
    Input {
        session_id: String,
        controller: String,
        pressed: Vec<String>,
        released: Vec<String>,
        held: Vec<String>,
        #[serde(default)]
        triggers: Option<[u8; 2]>,
        #[serde(default)]
        axes: Option<[i32; 4]>,
    },
    RecordingStarted {
        session_id: String,
        recording_id: String,
        controller: String,
    },
    RecordingCancelled {
        session_id: String,
        recording_id: String,
        reason: String,
    },
    Audio {
        session_id: String,
        recording_id: String,
        controller: String,
        bytes: usize,
        seconds: f64,
        sha256: String,
    },
    CommandResult {
        session_id: String,
        request_id: String,
        ok: bool,
        message: String,
    },
    Replaced,
    Busy,
    Pong {
        session_id: String,
        request_id: String,
    },
    Error {
        message: String,
    },
}

pub const MAX_AUDIO_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RECORDING_SECONDS: f64 = 60.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HardwareState {
    pub running: bool,
    #[serde(default)]
    pub version: Option<String>,
    pub message: String,
    pub controllers: Vec<ControllerState>,
    /// A controller is online but the receiver cannot reach it. On macOS this means
    /// the local-network permission, a firewall, or client isolation blocks its UDP path.
    #[serde(default)]
    pub network_blocked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ControllerState {
    pub id: String,
    pub ready: bool,
    pub battery_percent: Option<u8>,
    pub rumble_ready: bool,
    pub held: Vec<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
}

pub mod pairing;
