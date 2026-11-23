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
    pub short_pairing: bool,
}

#[derive(Clone, PartialEq)]
pub struct ViewState {
    pub bridges: Vec<Bridge>,
    pub status: String,
    pub connected: bool,
    pub active_bridge: Option<String>,
    pub connecting_bridge: Option<String>,
    pub pairing_required: Option<String>,
    pub heartbeats: u64,
    pub latency: Vec<Option<f64>>,
    pub axes: [i32; 4],
    pub triggers: [u8; 2],
    pub held: Vec<String>,
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
            connecting_bridge: None,
            pairing_required: None,
            heartbeats: 0,
            latency: vec![],
            axes: [0; 4],
            triggers: [0; 2],
            held: vec![],
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

pub(crate) const LOCAL_ID: &str = "@local";

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
                    if id == LOCAL_ID {
                        continue;
                    }
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
                        short_pairing: service.get_property_val_str("pairing")
                            == Some("short-code"),
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

    let local_state = state.clone();
    let local_discovery = tokio::spawn(async move {
        loop {
            let available = crate::local_bridge::available();
            {
                let mut view = local_state.lock().unwrap();
                if available && !view.bridges.iter().any(|bridge| bridge.id == LOCAL_ID) {
                    view.bridges.insert(
                        0,
                        Bridge {
                            id: LOCAL_ID.into(),
                            name: "Wave Bridge · 本机".into(),
                            address: "Unix Socket".into(),
                            port: 0,
                            short_pairing: false,
                        },
                    );
                } else if !available {
                    view.bridges.retain(|bridge| bridge.id != LOCAL_ID);
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
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
                            view.connecting_bridge = None;
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
        let bridge_id = bridge.id.clone();
        {
            let mut view = state.lock().unwrap();
            view.connecting_bridge = Some(bridge_id.clone());
            view.status = format!("正在连接 {}…", bridge.name);
        }
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
                view.connecting_bridge = None;
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
        if view.connected
            && let Some((_, invitation)) = target.as_mut()
        {
            invitation.clear();
        }
        view.connected = false;
        view.active_bridge = None;
        view.session_id = None;
        clear_recording(&mut view, dictation.as_ref());
        view.hardware = HardwareState::default();
        view.retrying = false;
        view.connecting_bridge = None;
        log::info!("Connection ended; evaluating recovery");
        match result {
            Ok(Ok(Exit::Retry(message))) => {
                view.status = format!("连接中断，正在重连：{message}");
                view.retrying = true;
                view.connecting_bridge = Some(bridge_id);
                takeover = false;
                waiting = true;
            }
            Ok(Ok(Exit::PairingRequired(message))) => {
                view.pairing_required = Some(bridge_id);
                view.status = message.into();
                target = None;
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
    local_discovery.abort();
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
    PairingRequired(&'static str),
    Replaced,
    Busy,
}

async fn bootstrap_pairing(bridge: &Bridge, code: &str) -> Result<(String, String)> {
    use wave_protocol::pairing::{self, Challenge, Ed25519Group, Password, Spake2};
    tokio::time::timeout(Duration::from_secs(8), async {
        let (a, b) = pairing::identities();
        let (exchange, message) =
            Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &a, &b);
        let tcp = TcpStream::connect((bridge.address.as_str(), bridge.port)).await?;
        let (mut socket, _) =
            client_async(format!("ws://{}:{}/ws", bridge.address, bridge.port), tcp).await?;
        let request = ClientMessage::PairingHello { message };
        socket
            .send(Message::Text(serde_json::to_string(&request)?.into()))
            .await?;
        let message = socket
            .next()
            .await
            .context("配对窗口不可用，请在 Bridge 上重新生成配对码")??;
        let challenge: Challenge = serde_json::from_str(message.to_text()?)?;
        if challenge.id != bridge.id {
            bail!("配对返回的 Bridge 身份与扫描结果不一致");
        }
        let secret = exchange
            .finish(&challenge.message)
            .map_err(|_| anyhow::anyhow!("配对握手失败"))?;
        let key = pairing::key(&secret);
        ring::hmac::verify(
            &key,
            &pairing::transcript("server-proof", &challenge.id, &challenge.certificate_pem),
            &challenge.proof,
        )
        .map_err(|_| anyhow::anyhow!("配对码不正确，或无法验证 Bridge 身份"))?;
        let token = pairing::grant(&key, &challenge.id, &challenge.certificate_pem);
        Ok((challenge.certificate_pem, token))
    })
    .await
    .context("短码配对超时")?
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
    trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
    impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}
    let local = bridge.id == LOCAL_ID;
    let (auth, remote) = if local {
        (
            ClientMessage::LocalConnect {
                version: VERSION,
                takeover,
            },
            None,
        )
    } else {
        let entry = profile_keyring_entry(&bridge.id, profile)?;
        let code = invitation_path.trim();
        let short_pairing = bridge.short_pairing && !code.is_empty();
        if short_pairing && !(code.len() == 8 && code.bytes().all(|byte| byte.is_ascii_digit())) {
            bail!("请输入 8 位配对码");
        }
        let saved = if short_pairing {
            None
        } else {
            match entry.get_password() {
                Ok(value) => match serde_json::from_str::<Credential>(&value) {
                    Ok(credential) if credential.bridge_id == bridge.id => Some(credential),
                    _ => return Ok(Exit::PairingRequired("保存的凭据无效，请重新配对。")),
                },
                Err(keyring::Error::NoEntry) => None,
                Err(_) => {
                    return Ok(Exit::PairingRequired(
                        "无法读取钥匙串凭据。可重新配对，或在钥匙串中恢复访问。",
                    ));
                }
            }
        };
        let (cert, auth, companion_id) = if let Some(saved) = saved {
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
        } else if short_pairing {
            let (cert, token) = bootstrap_pairing(bridge, code).await?;
            let companion_id = Uuid::new_v4().to_string();
            (
                cert,
                ClientMessage::Pair {
                    version: VERSION,
                    companion_id: companion_id.clone(),
                    name: "Wave Companion".into(),
                    token,
                    takeover,
                },
                companion_id,
            )
        } else {
            if code.is_empty() || bridge.short_pairing {
                return Ok(Exit::PairingRequired("需要配对才能连接这台 Bridge。"));
            }
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
        (auth, Some((entry, cert, companion_id)))
    };
    let socket = tokio::time::timeout(Duration::from_secs(10), async {
        let (stream, url): (Box<dyn Transport>, String) = if let Some((_, cert, _)) = &remote {
            let mut roots = rustls::RootCertStore::empty();
            for cert in rustls_pemfile::certs(&mut BufReader::new(cert.as_bytes())) {
                roots.add(cert?)?;
            }
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let tcp = TcpStream::connect((bridge.address.as_str(), bridge.port)).await?;
            let tls = TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("stadia-wave.local")?, tcp)
                .await?;
            (
                Box::new(tls),
                format!("wss://{}:{}/ws", bridge.address, bridge.port),
            )
        } else {
            let stream =
                tokio::net::UnixStream::connect(crate::local_bridge::socket_path()).await?;
            if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
                bail!("本机 Bridge 的系统用户不匹配");
            }
            (Box::new(stream), "ws://localhost/ws".into())
        };
        anyhow::Ok(client_async(url, stream).await?.0)
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
        let mut ping_at = std::collections::HashMap::new();
        let mut last_received = tokio::time::Instant::now();
        let mut current_session: Option<String> = None;
        let mut keychain_warning = None;
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
                        if !ping_at.is_empty() {
                            let mut view = state.lock().unwrap();
                            view.latency.push(None);
                            if view.latency.len() > 60 { view.latency.remove(0); }
                        }
                        ping_at.clear();
                        let request_id = Uuid::new_v4().to_string();
                        ping_at.insert(request_id.clone(), tokio::time::Instant::now());
                        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(
                            serde_json::to_string(&ClientMessage::Ping { session_id: session_id.clone(), request_id })?.into()
                        ))).await??;
                    }
                }
                message = socket.next() => {
                    let message = message.context("Bridge 关闭了连接")??;
                    last_received = tokio::time::Instant::now();
                    match message {
                        Message::Text(text) => match serde_json::from_str::<ServerMessage>(&text)? {
                            ServerMessage::PairingCode { .. } => bail!("Unexpected local control message"),
                            ServerMessage::Paired { token } => {
                                let Some((entry, cert, companion_id)) = &remote else { bail!("Unexpected pairing response on local connection"); };
                                let credential = Credential {
                                    bridge_id: bridge.id.clone(), certificate_pem: cert.clone(),
                                    companion_id: companion_id.clone(), token,
                                };
                                // Keep this session usable even if Keychain cannot persist the new token.
                                if let Err(error) = entry.set_password(&serde_json::to_string(&credential)?) {
                                    keychain_warning = Some(format!("钥匙串保存失败，下次连接需重新配对（{error}）"));
                                }
                            }
                            ServerMessage::Connected { bridge_id, session_id } => {
                                if !local && bridge_id != bridge.id { bail!("Bridge identity mismatch"); }
                                current_session = Some(session_id.clone());
                                let mut state = state.lock().unwrap();
                                state.connected = true;
                                state.active_bridge = Some(bridge.id.clone());
                                state.connecting_bridge = None;
                                if state.pairing_required.as_ref() == Some(&bridge.id) {
                                    state.pairing_required = None;
                                }
                                state.retrying = false;
                                state.session_id = Some(session_id);
                                state.heartbeats = 0;
                                state.status = format!("已连接 {} · {}", bridge.name, if local { "本机直连" } else { "WSS 已认证" });
                                if let Some(warning) = &keychain_warning {
                                    state.status.push_str(&format!(" · {warning}"));
                                }
                                log::info!("CONNECTED {} via {}:{}", bridge.id, bridge.address, bridge.port);
                            }
                            ServerMessage::Status { .. } => {}
                            ServerMessage::Hardware { session_id, snapshot } if current_session.as_ref() == Some(&session_id) => {
                                state.lock().unwrap().hardware = snapshot;
                            }
                            ServerMessage::Input { session_id, controller, pressed, released, triggers, axes, held } if current_session.as_ref() == Some(&session_id) => {
                                crate::mapping::input(&session_id, &controller, &held, axes, triggers,
                                    dictation.as_ref().is_some_and(|d| d.mapping_blocked())
                                        || state.lock().unwrap().recording.is_some());
                                if let Some(dictation) = dictation {
                                    dictation.controller_input(&bridge.id, &controller, &pressed);
                                }
                                {
                                    let mut view = state.lock().unwrap();
                                    if let Some(axes) = axes { view.axes = axes; }
                                    if let Some(triggers) = triggers { view.triggers = triggers; }
                                    view.held = held;
                                }
                                let analog = triggers.map(|[left, right]| format!("  L2 {left}/255  R2 {right}/255")).unwrap_or_default();
                                let sticks = axes.map(|[lx, ly, rx, ry]| format!("  左摇杆 X {lx} Y {ly}  右摇杆 X {rx} Y {ry}")).unwrap_or_default();
                                state.lock().unwrap().input = format!("{controller}  按下 {}  松开 {}{analog}{sticks}", pressed.join(", "), released.join(", "));
                            }
                            ServerMessage::RecordingStarted { session_id, recording_id, .. } if current_session.as_ref() == Some(&session_id) => {
                                crate::mapping::release();
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
                            ServerMessage::Pong { session_id, request_id } if current_session.as_ref() == Some(&session_id) => {
                                let mut view = state.lock().unwrap();
                                view.heartbeats += 1;
                                if let Some(sent) = ping_at.remove(&request_id) {
                                    view.latency.push(Some(sent.elapsed().as_secs_f64() * 1000.0));
                                    if view.latency.len() > 60 { view.latency.remove(0); }
                                }
                                log::debug!("PONG");
                            }
                            ServerMessage::Pong { .. } => {}
                            ServerMessage::Replaced => return Ok(Exit::Replaced),
                            ServerMessage::Busy => return Ok(Exit::Busy),
                            ServerMessage::Error { message } if !local && message == "Authentication failed" => {
                                return Ok(Exit::PairingRequired("Bridge 已拒绝保存的凭据，请重新配对。"));
                            }
                            ServerMessage::Error { message } => return Err(anyhow::anyhow!(message)),
                        },
                        Message::Binary(data) => {
                            let audio = incoming_audio.as_mut().context("缺少录音元数据")?;
                            if audio.data.len() + data.len() > audio.bytes { bail!("录音数据超出声明长度"); }
                            audio.data.extend_from_slice(&data);
                            if audio.data.len() == audio.bytes {
                                let audio = incoming_audio.take().unwrap();
                                let directory = ProjectDirs::from("dev", "stadiawave", "companion").context("Missing user data directory")?.data_dir().join("recordings");
                                let is_new = !directory.join(format!("{}.ogg", audio.id)).exists();
                                let recording = audio.save(&directory)?;
                                if is_new { crate::statistics::update(|stats| { stats.received += 1; stats.seconds += recording.seconds; }); }
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
