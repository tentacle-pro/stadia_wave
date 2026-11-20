use crate::dictation::Dictation;
use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use rustls::pki_types::ServerName;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::BufReader,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{client_async, tungstenite::Message};
use uuid::Uuid;
use wave_protocol::{
    ClientMessage, Credential, HardwareState, Invitation, MAX_AUDIO_BYTES, MAX_RECORDING_SECONDS,
    SERVICE_TYPE, ServerMessage, VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Bridge {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
}

#[derive(Clone, PartialEq)]
pub struct ViewState {
    pub bridges: Vec<Bridge>,
    pub status: String,
    pub connected: bool,
    pub active_bridge: Option<String>,
    pub heartbeats: u64,
    pub session_id: Option<String>,
    pub retrying: bool,
    pub hardware: HardwareState,
    pub input: String,
    pub recording: Option<String>,
    pub audio: Option<ReceivedRecording>,
    pub command_result: String,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            bridges: vec![],
            status: "正在通过 Bonjour 发现 Bridge…".into(),
            connected: false,
            active_bridge: None,
            heartbeats: 0,
            session_id: None,
            retrying: false,
            hardware: HardwareState::default(),
            input: String::new(),
            recording: None,
            audio: None,
            command_result: String::new(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct ReceivedRecording {
    pub id: String,
    pub path: std::path::PathBuf,
    pub seconds: f64,
}

pub enum Command {
    Restore {
        bridge: Bridge,
    },
    Connect {
        bridge: Bridge,
        invitation_path: String,
    },
    Rumble {
        controller: String,
    },
    Disconnect,
}

pub struct Worker {
    pub state: Arc<Mutex<ViewState>>,
    pub commands: mpsc::UnboundedSender<Command>,
}

pub fn start() -> Worker {
    start_for_desktop(None)
}

pub(crate) fn start_for_desktop(dictation: Option<Dictation>) -> Worker {
    start_worker(
        std::env::var("WAVE_COMPANION_PROFILE").unwrap_or_else(|_| "default".into()),
        dictation,
    )
}

pub fn start_with_profile(profile: String) -> Worker {
    start_worker(profile, None)
}

fn start_worker(profile: String, dictation: Option<Dictation>) -> Worker {
    let state = Arc::new(Mutex::new(ViewState::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = state.clone();
    std::thread::spawn(move || {
        let result = tokio::runtime::Runtime::new()
            .context("Unable to start network runtime")
            .and_then(|runtime| runtime.block_on(run(shared.clone(), rx, profile, dictation)));
        if let Err(error) = result {
            shared.lock().unwrap().status = format!("{error:#}");
        }
    });
    Worker {
        state,
        commands: tx,
    }
}

async fn run(
    state: Arc<Mutex<ViewState>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    profile: String,
    dictation: Option<Dictation>,
) -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE_TYPE)?;
    let discovery_state = state.clone();
    let discovery = tokio::spawn(async move {
        while let Ok(event) = events.recv_async().await {
            let mut state = discovery_state.lock().unwrap();
            match event {
                ServiceEvent::ServiceResolved(service) => {
                    let Some(id) = service.get_property_val_str("bridge_id") else {
                        continue;
                    };
                    if service.get_property_val_str("version") != Some(&VERSION.to_string()) {
                        continue;
                    }
                    let mut addresses: Vec<_> = service.get_addresses_v4().into_iter().collect();
                    addresses.sort();
                    let Some(address) = addresses.first() else {
                        continue;
                    };
                    let bridge = Bridge {
                        id: id.to_owned(),
                        name: service
                            .fullname
                            .trim_end_matches(SERVICE_TYPE)
                            .trim_end_matches('.')
                            .into(),
                        address: address.to_string(),
                        port: service.port,
                    };
                    state.bridges.retain(|entry| entry.id != id);
                    state.bridges.push(bridge);
                    state.bridges.sort_by(|a, b| a.id.cmp(&b.id));
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let name = fullname
                        .trim_end_matches(SERVICE_TYPE)
                        .trim_end_matches('.');
                    state.bridges.retain(|entry| entry.name != name);
                }
                _ => {}
            }
        }
    });

    let mut target: Option<(Bridge, String)> = None;
    let mut takeover = true;
    let mut retry_delay = 1;
    let mut waiting = false;
    'coordinator: loop {
        if target.is_none() {
            match commands.recv().await {
                Some(Command::Connect {
                    bridge,
                    invitation_path,
                }) => {
                    target = Some((bridge, invitation_path));
                    takeover = true;
                    waiting = false;
                    retry_delay = 1;
                }
                Some(Command::Restore { bridge }) => {
                    target = Some((bridge, String::new()));
                    takeover = false;
                    waiting = false;
                }
                Some(Command::Rumble { .. }) => continue,
                Some(Command::Disconnect) => {
                    state.lock().unwrap().status = "已断开".into();
                    continue;
                }
                None => break,
            }
        }
        if waiting {
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        Some(Command::Connect { bridge, invitation_path }) => {
                            target = Some((bridge, invitation_path));
                            takeover = true;
                            retry_delay = 1;
                        }
                        Some(Command::Restore { .. }) => continue,
                        Some(Command::Rumble { .. }) => continue,
                        Some(Command::Disconnect) => {
                            target = None;
                            let mut view = state.lock().unwrap();
                            view.retrying = false;
                            view.status = "已断开".into();
                        }
                        None => break,
                    }
                    waiting = false;
                    continue;
                }
                _ = tokio::time::sleep(Duration::from_secs(retry_delay)) => {}
            }
            retry_delay = (retry_delay * 2).min(8);
        }
        let (mut bridge, invitation_path) = target.clone().unwrap();
        if let Some(updated) = state
            .lock()
            .unwrap()
            .bridges
            .iter()
            .find(|entry| entry.id == bridge.id)
            .cloned()
        {
            bridge = updated;
        }
        state.lock().unwrap().status = format!("正在连接 {}…", bridge.name);
        let (device_commands, device_rx) = mpsc::unbounded_channel();
        let session_state = state.clone();
        let session_profile = profile.clone();
        let session_dictation = dictation.clone();
        let mut task = tokio::spawn(async move {
            session(
                &bridge,
                &invitation_path,
                session_state,
                &session_profile,
                takeover,
                device_rx,
                session_dictation.as_ref(),
            )
            .await
        });
        let result = loop {
            let outcome = tokio::select! {
            command = commands.recv() => {
                if let Some(Command::Rumble { controller }) = command {
                    let _ = device_commands.send(controller);
                    continue;
                }
                task.abort();
                let _ = (&mut task).await;
                let mut view = state.lock().unwrap();
                view.connected = false;
                view.active_bridge = None;
                view.session_id = None;
                clear_recording(&mut view, dictation.as_ref());
                view.hardware = HardwareState::default();
                view.retrying = false;
                match command {
                    Some(Command::Connect { bridge, invitation_path }) => {
                        target = Some((bridge, invitation_path));
                        takeover = true;
                    }
                    Some(Command::Disconnect) => {
                        target = None;
                        view.status = "已断开".into();
                    }
                    None => break 'coordinator,
                    Some(Command::Restore { bridge }) => {
                        target = Some((bridge, String::new()));
                        takeover = false;
                    }
                    Some(Command::Rumble { .. }) => unreachable!(),
                }
                waiting = false;
                retry_delay = 1;
                continue 'coordinator;
            }
            result = &mut task => result,
            };
            break outcome;
        };
        let mut view = state.lock().unwrap();
        view.connected = false;
        view.active_bridge = None;
        view.session_id = None;
        clear_recording(&mut view, dictation.as_ref());
        view.hardware = HardwareState::default();
        view.retrying = false;
        log::info!("Connection ended; evaluating recovery");
        match result {
            Ok(Ok(Exit::Retry(message))) => {
                view.status = format!("连接中断，正在重连：{message}");
                view.retrying = true;
                takeover = false;
                waiting = true;
            }
            Ok(Ok(Exit::Replaced)) => {
                view.status = "连接已被接管。点击连接可重新接管。".into();
                target = None;
            }
            Ok(Ok(Exit::Busy)) => {
                view.status = "Bridge 正由另一 Companion 使用。点击连接可接管。".into();
                target = None;
            }
            Ok(Err(error)) => {
                view.status = format!("连接失败：{error:#}");
                target = None;
            }
            Err(error) => {
                view.status = format!("连接任务失败：{error}");
                target = None;
            }
        }
    }
    discovery.abort();
    daemon.shutdown()?;
    Ok(())
}

fn clear_recording(view: &mut ViewState, dictation: Option<&Dictation>) {
    if let Some(id) = view.recording.take()
        && let Some(dictation) = dictation
    {
        dictation.recording_cancelled(&id);
    }
}

pub fn keyring_entry(bridge_id: &str) -> Result<keyring::Entry> {
    profile_keyring_entry(bridge_id, "default")
}

fn profile_keyring_entry(bridge_id: &str, profile: &str) -> Result<keyring::Entry> {
    let account = if profile == "default" {
        bridge_id.to_owned()
    } else {
        format!("{bridge_id}/{profile}")
    };
    Ok(keyring::Entry::new("dev.stadiawave.companion", &account)?)
}

enum Exit {
    Retry(String),
    Replaced,
    Busy,
}

async fn session(
    bridge: &Bridge,
    invitation_path: &str,
    state: Arc<Mutex<ViewState>>,
    profile: &str,
    takeover: bool,
    mut device_commands: mpsc::UnboundedReceiver<String>,
    dictation: Option<&Dictation>,
) -> Result<Exit> {
    let entry = profile_keyring_entry(&bridge.id, profile)?;
    let saved = match entry.get_password() {
        Ok(value) => Some(serde_json::from_str::<Credential>(&value)?),
        Err(keyring::Error::NoEntry) => None,
        Err(error) => return Err(error.into()),
    };
    let (cert, auth, companion_id) = if let Some(saved) = saved {
        if saved.bridge_id != bridge.id {
            bail!("Stored Bridge identity mismatch");
        }
        (
            saved.certificate_pem,
            ClientMessage::Connect {
                version: VERSION,
                companion_id: saved.companion_id.clone(),
                token: saved.token,
                takeover,
            },
            saved.companion_id,
        )
    } else {
        let invitation: Invitation = serde_json::from_slice(
            &fs::read(Path::new(invitation_path.trim()))
                .context("请选择 Bridge 生成的 pairing.json 文件")?,
        )?;
        if invitation.bridge_id != bridge.id {
            bail!("配对文件与所选 Bridge 不匹配");
        }
        let companion_id = Uuid::new_v4().to_string();
        (
            invitation.certificate_pem,
            ClientMessage::Pair {
                version: VERSION,
                companion_id: companion_id.clone(),
                name: "Wave Companion".into(),
                token: invitation.pairing_token,
                takeover,
            },
            companion_id,
        )
    };
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(cert.as_bytes())) {
        roots.add(cert?)?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let socket = tokio::time::timeout(Duration::from_secs(10), async {
        let tcp = TcpStream::connect((bridge.address.as_str(), bridge.port)).await?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("stadia-wave.local")?, tcp)
            .await?;
        anyhow::Ok(
            client_async(format!("wss://{}:{}/ws", bridge.address, bridge.port), tls)
                .await?
                .0,
        )
    })
    .await;
    let mut socket = match socket {
        Ok(Ok(socket)) => socket,
        Ok(Err(error)) => return Ok(Exit::Retry(format!("{error:#}"))),
        Err(_) => return Ok(Exit::Retry("连接超时".into())),
    };

    let outcome: Result<Exit> = async {
        socket.send(Message::Text(serde_json::to_string(&auth)?.into())).await?;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_received = tokio::time::Instant::now();
        let mut current_session: Option<String> = None;
        let mut incoming_audio: Option<IncomingAudio> = None;
        loop {
            tokio::select! {
                Some(controller) = device_commands.recv() => {
                    if let Some(session_id) = &current_session {
                        let request_id = Uuid::new_v4().to_string();
                        socket.send(Message::Text(serde_json::to_string(&ClientMessage::Rumble {
                            session_id: session_id.clone(), request_id, controller,
                        })?.into())).await?;
                    }
                }
                _ = heartbeat.tick() => {
                    if last_received.elapsed() > Duration::from_secs(20) { bail!("Bridge 响应超时"); }
                    if let Some(session_id) = &current_session {
                        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(
                            serde_json::to_string(&ClientMessage::Ping { session_id: session_id.clone(), request_id: Uuid::new_v4().to_string() })?.into()
                        ))).await??;
                    }
                }
                message = socket.next() => {
                    let message = message.context("Bridge 关闭了连接")??;
                    last_received = tokio::time::Instant::now();
                    match message {
                        Message::Text(text) => match serde_json::from_str::<ServerMessage>(&text)? {
                            ServerMessage::Paired { token } => {
                                let credential = Credential {
                                    bridge_id: bridge.id.clone(), certificate_pem: cert.clone(),
                                    companion_id: companion_id.clone(), token,
                                };
                                // Keychain errors are terminal; retrying would consume another invitation.
                                if let Err(error) = entry.set_password(&serde_json::to_string(&credential)?) {
                                    return Err(anyhow::anyhow!("无法保存 Keychain 配对凭据：{error}"));
                                }
                            }
                            ServerMessage::Connected { bridge_id, session_id } => {
                                if bridge_id != bridge.id { bail!("Bridge identity mismatch"); }
                                current_session = Some(session_id.clone());
                                let mut state = state.lock().unwrap();
                                state.connected = true;
                                state.active_bridge = Some(bridge_id);
                                state.retrying = false;
                                state.session_id = Some(session_id);
                                state.heartbeats = 0;
                                state.status = format!("已连接 {} · WSS 已认证", bridge.name);
                                log::info!("CONNECTED {} via {}:{}", bridge.id, bridge.address, bridge.port);
                            }
                            ServerMessage::Status { .. } => {}
                            ServerMessage::Hardware { session_id, snapshot } if current_session.as_ref() == Some(&session_id) => {
                                state.lock().unwrap().hardware = snapshot;
                            }
                            ServerMessage::Input { session_id, controller, pressed, released, .. } if current_session.as_ref() == Some(&session_id) => {
                                if let Some(dictation) = dictation {
                                    dictation.controller_input(&bridge.id, &controller, &pressed);
                                }
                                state.lock().unwrap().input = format!("{controller}  按下 {}  松开 {}", pressed.join(", "), released.join(", "));
                            }
                            ServerMessage::RecordingStarted { session_id, recording_id, .. } if current_session.as_ref() == Some(&session_id) => {
                                if let Some(dictation) = dictation {
                                    dictation.recording_started(recording_id.clone());
                                }
                                state.lock().unwrap().recording = Some(recording_id);
                            }
                            ServerMessage::RecordingCancelled { session_id, reason, .. } if current_session.as_ref() == Some(&session_id) => {
                                let mut view = state.lock().unwrap();
                                clear_recording(&mut view, dictation);
                                view.command_result = reason;
                                incoming_audio = None;
                            }
                            ServerMessage::Audio { session_id, recording_id, controller, bytes, seconds, sha256 } if current_session.as_ref() == Some(&session_id) => {
                                if incoming_audio.is_some() || bytes == 0 || bytes > MAX_AUDIO_BYTES
                                    || !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_RECORDING_SECONDS
                                    || Uuid::parse_str(&recording_id).is_err() {
                                    bail!("无效录音元数据");
                                }
                                incoming_audio = Some(IncomingAudio { session_id, id: recording_id, controller, bytes, seconds, hash: sha256, data: Vec::with_capacity(bytes) });
                            }
                            ServerMessage::CommandResult { session_id, request_id, message, .. } if current_session.as_ref() == Some(&session_id) => {
                                let mut view = state.lock().unwrap();
                                view.command_result = message;
                                if request_id == "recording" { clear_recording(&mut view, dictation); }
                            }
                            ServerMessage::Hardware { .. } | ServerMessage::Input { .. } | ServerMessage::RecordingStarted { .. }
                                | ServerMessage::RecordingCancelled { .. } | ServerMessage::Audio { .. } | ServerMessage::CommandResult { .. } => {}
                            ServerMessage::Pong { session_id, .. } if current_session.as_ref() == Some(&session_id) => {
                                state.lock().unwrap().heartbeats += 1;
                                log::debug!("PONG");
                            }
                            ServerMessage::Pong { .. } => {}
                            ServerMessage::Replaced => return Ok(Exit::Replaced),
                            ServerMessage::Busy => return Ok(Exit::Busy),
                            ServerMessage::Error { message } => return Err(anyhow::anyhow!(message)),
                        },
                        Message::Binary(data) => {
                            let audio = incoming_audio.as_mut().context("缺少录音元数据")?;
                            if audio.data.len() + data.len() > audio.bytes { bail!("录音数据超出声明长度"); }
                            audio.data.extend_from_slice(&data);
                            if audio.data.len() == audio.bytes {
                                let audio = incoming_audio.take().unwrap();
                                let directory = ProjectDirs::from("dev", "stadiawave", "companion").context("Missing user data directory")?.data_dir().join("recordings");
                                let recording = audio.save(&directory)?;
                                socket.send(Message::Text(serde_json::to_string(&ClientMessage::AudioReceived {
                                    session_id: audio.session_id.clone(), recording_id: recording.id.clone(),
                                })?.into())).await?;
                                let mut view = state.lock().unwrap();
                                if let Some(dictation) = dictation {
                                    dictation.received(recording.clone(), audio.controller, bridge.id.clone());
                                }
                                view.recording = None;
                                view.command_result = "录音已接收".into();
                                view.audio = Some(recording);
                            }
                        }
                        Message::Close(frame) => {
                            if frame.as_ref().is_some_and(|frame| frame.reason == "replaced") {
                                return Ok(Exit::Replaced);
                            }
                            bail!("Bridge 关闭了连接");
                        }
                        Message::Ping(data) => {
                            tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Pong(data))).await??;
                        }
                        _ => {}
                    }
                }
            }
        }
    }.await;
    match outcome {
        Ok(exit) => Ok(exit),
        Err(error) if current_connection_error(&error) => Ok(Exit::Retry(format!("{error:#}"))),
        Err(error) => Err(error),
    }
}

fn current_connection_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
        .is_some()
        || error
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
        || error.to_string() == "Bridge 响应超时"
        || error.to_string() == "Bridge 关闭了连接"
}

struct IncomingAudio {
    session_id: String,
    id: String,
    controller: String,
    bytes: usize,
    seconds: f64,
    hash: String,
    data: Vec<u8>,
}

impl IncomingAudio {
    fn save(&self, directory: &Path) -> Result<ReceivedRecording> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if self.data.len() != self.bytes
            || !self.data.starts_with(b"OggS")
            || hex::encode(Sha256::digest(&self.data)) != self.hash
        {
            bail!("录音完整性检查失败");
        }
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let path = directory.join(format!("{}.ogg", self.id));
        if path.exists() {
            if hex::encode(Sha256::digest(fs::read(&path)?)) != self.hash {
                bail!("录音 ID 冲突");
            }
        } else {
            let temporary = directory.join(format!("{}.tmp", Uuid::new_v4()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&self.data)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
        }
        Ok(ReceivedRecording {
            id: self.id.clone(),
            path,
            seconds: self.seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_integrity_and_id_collision_are_checked_before_delivery() {
        let directory = std::env::temp_dir().join(format!("wave-receive-{}", Uuid::new_v4()));
        let data = b"OggS-test-fixture".to_vec();
        let mut audio = IncomingAudio {
            session_id: "test".into(),
            id: Uuid::new_v4().to_string(),
            controller: "test".into(),
            bytes: data.len(),
            seconds: 1.0,
            hash: hex::encode(Sha256::digest(&data)),
            data,
        };
        let recording = audio.save(&directory).unwrap();
        assert_eq!(fs::read(&recording.path).unwrap(), audio.data);
        assert_eq!(audio.save(&directory).unwrap().path, recording.path);
        audio.data.push(1);
        assert!(audio.save(&directory).is_err());
        audio.bytes = audio.data.len();
        audio.hash = hex::encode(Sha256::digest(&audio.data));
        assert!(audio.save(&directory).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
