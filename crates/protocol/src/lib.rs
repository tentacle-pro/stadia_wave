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
    pub message: String,
    pub controllers: Vec<ControllerState>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ControllerState {
    pub id: String,
    pub ready: bool,
    pub battery_percent: Option<u8>,
    pub rumble_ready: bool,
    pub held: Vec<String>,
}
