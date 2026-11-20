mod stadiacast;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use rustls::pki_types::PrivateKeyDer;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::BufReader,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use uuid::Uuid;
use wave_protocol::{
    ClientMessage, DEFAULT_PORT, Invitation, SERVICE_TYPE, ServerMessage, VERSION,
};

#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    certificate_pem: String,
    key_pem: String,
    companions: HashMap<String, String>,
}

struct Active {
    id: String,
    cancel: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

struct State {
    identity: Identity,
    path: PathBuf,
    invitation: Invitation,
    active: Option<Active>,
    name: String,
    device: stadiacast::Handle,
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn bundled_stadiacast() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let helpers = executable.parent()?;
    let contents = helpers.parent()?;
    let app = contents.parent()?;
    if helpers.file_name()? != "Helpers"
        || contents.file_name()? != "Contents"
        || app.extension()? != "app"
    {
        return None;
    }
    let runtime = helpers.join("stadiacast");
    runtime.is_file().then_some(runtime)
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let args: Vec<_> = std::env::args().collect();
    let option = |key: &str| args.windows(2).find(|v| v[0] == key).map(|v| v[1].clone());
    if args.iter().any(|arg| arg == "--help") {
        println!(
            "wave-bridge [--name NAME] [--port PORT] [--data-dir PATH] [--stadiacast PATH --controller-ip IP --advertise-ip IP --dns-upstream IP]"
        );
        return Ok(());
    }
    let data = option("--data-dir").map(PathBuf::from).unwrap_or_else(|| {
        ProjectDirs::from("dev", "stadiawave", "bridge")
            .unwrap()
            .data_dir()
            .to_path_buf()
    });
    fs::create_dir_all(&data)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
    let port: u16 = option("--port")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    // Bind before loading or modifying state, so a second instance cannot rotate pairing information.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
        .await
        .context("Bridge port unavailable")?;
    let path = data.join("identity.json");
    let identity: Identity = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        let cert = rcgen::generate_simple_self_signed(vec!["stadia-wave.local".to_owned()])?;
        let identity = Identity {
            id: Uuid::new_v4().to_string(),
            certificate_pem: cert.cert.pem(),
            key_pem: cert.signing_key.serialize_pem(),
            companions: HashMap::new(),
        };
        private_write(&path, &serde_json::to_vec_pretty(&identity)?)?;
        identity
    };
    let certificates =
        rustls_pemfile::certs(&mut BufReader::new(identity.certificate_pem.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
    let fingerprint = hex::encode(Sha256::digest(certificates[0].as_ref()));
    let key: PrivateKeyDer =
        rustls_pemfile::private_key(&mut BufReader::new(identity.key_pem.as_bytes()))?
            .context("Missing private key")?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let invitation = Invitation {
        bridge_id: identity.id.clone(),
        certificate_pem: identity.certificate_pem.clone(),
        pairing_token: Uuid::new_v4().to_string(),
        expires_at: unix_seconds() + 600,
    };
    private_write(
        &data.join("pairing.json"),
        &serde_json::to_vec_pretty(&invitation)?,
    )?;
    let name = option("--name").unwrap_or_else(|| "Wave Bridge".into());
    let daemon = ServiceDaemon::new()?;
    let properties = [
        ("version", VERSION.to_string()),
        ("bridge_id", identity.id.clone()),
    ];
    let hostname = format!("wave-{}.local.", identity.id);
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &format!("{} {}", name, &identity.id[..8]),
        &hostname,
        "",
        port,
        &properties[..],
    )?
    .enable_addr_auto();
    let fullname = service.get_fullname().to_owned();
    daemon.register(service)?;
    println!(
        "Bridge ready: {name}\nPort: {port}\nCertificate SHA-256: {fingerprint}\nPairing invitation (valid 10 minutes, one use): {}",
        data.join("pairing.json").display()
    );
    let config_path = data.join("stadiacast.json");
    let options: Option<stadiacast::Options> = if let Some(executable) = option("--stadiacast") {
        let options = stadiacast::Options {
            executable: PathBuf::from(executable)
                .canonicalize()
                .context("Stadiacast executable not found")?,
            controller_ip: option("--controller-ip").context("--controller-ip required")?,
            advertise_ip: option("--advertise-ip").context("--advertise-ip required")?,
            dns_upstream: option("--dns-upstream").context("--dns-upstream required")?,
        };
        for address in [
            &options.controller_ip,
            &options.advertise_ip,
            &options.dns_upstream,
        ] {
            address
                .parse::<Ipv4Addr>()
                .context("Expected an IPv4 address")?;
        }
        private_write(&config_path, &serde_json::to_vec_pretty(&options)?)?;
        Some(options)
    } else if config_path.exists() {
        let mut options: stadiacast::Options = serde_json::from_slice(&fs::read(&config_path)?)?;
        if let Some(runtime) = bundled_stadiacast() {
            options.executable = runtime;
        }
        Some(options)
    } else {
        None
    };
    let device = stadiacast::Handle::start(options, &data)?;
    let state = Arc::new(Mutex::new(State {
        identity,
        path,
        invitation,
        active: None,
        name,
        device: device.clone(),
    }));
    let admission = Arc::new(Mutex::new(()));
    let mut tasks = tokio::task::JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            incoming = listener.accept() => {
                let (stream, peer) = incoming?;
                let acceptor = acceptor.clone();
                let state = state.clone();
                let admission = admission.clone();
                tasks.spawn(async move {
                    if let Err(error) = handle(stream, acceptor, state, admission).await {
                        eprintln!("Connection {peer}: {error:#}");
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    if let Some(active) = state.lock().await.active.take() {
        active.task.abort();
    }
    device.shutdown().await;
    daemon.unregister(&fullname)?;
    daemon.shutdown()?;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>;

async fn send(socket: &mut Socket, message: ServerMessage) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(serde_json::to_string(&message)?.into())),
    )
    .await??;
    Ok(())
}

async fn handle(
    stream: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    state: Arc<Mutex<State>>,
    admission: Arc<Mutex<()>>,
) -> Result<()> {
    let mut socket = tokio::time::timeout(Duration::from_secs(10), async {
        let stream = acceptor.accept(stream).await?;
        anyhow::Ok(accept_async(stream).await?)
    })
    .await??;
    let first = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("Missing authentication")??;
    let auth: ClientMessage = serde_json::from_str(first.to_text()?)?;
    let _admission = admission.lock().await;
    let mut guard = state.lock().await;
    let result: Result<(bool, Option<String>)> = (|| {
        match auth {
            ClientMessage::Pair {
                version,
                companion_id,
                name,
                token,
                takeover,
            } => {
                if version != VERSION {
                    bail!("Incompatible protocol version");
                }
                if unix_seconds() >= guard.invitation.expires_at
                    || token != guard.invitation.pairing_token
                {
                    bail!("Pairing invitation expired or already used");
                }
                if companion_id.is_empty() || companion_id.len() > 128 || name.len() > 128 {
                    bail!("Invalid Companion identity");
                }
                // An automatic attempt must not consume the invitation while occupied.
                if guard.active.is_some() && !takeover {
                    return Ok((false, None));
                }
                let secret = Uuid::new_v4().to_string();
                guard
                    .identity
                    .companions
                    .insert(companion_id, hex::encode(Sha256::digest(secret.as_bytes())));
                private_write(&guard.path, &serde_json::to_vec_pretty(&guard.identity)?)?;
                let next = Invitation {
                    bridge_id: guard.identity.id.clone(),
                    certificate_pem: guard.identity.certificate_pem.clone(),
                    pairing_token: Uuid::new_v4().to_string(),
                    expires_at: unix_seconds() + 600,
                };
                private_write(
                    &guard.path.parent().unwrap().join("pairing.json"),
                    &serde_json::to_vec_pretty(&next)?,
                )?;
                guard.invitation = next;
                Ok((takeover, Some(secret)))
            }
            ClientMessage::Connect {
                version,
                companion_id,
                token,
                takeover,
            } => {
                if version != VERSION {
                    bail!("Incompatible protocol version");
                }
                let digest = hex::encode(Sha256::digest(token.as_bytes()));
                if guard.identity.companions.get(&companion_id) != Some(&digest) {
                    bail!("Authentication failed");
                }
                Ok((takeover, None))
            }
            _ => bail!("Authentication required"),
        }
    })();
    let (takeover, credential) = match result {
        Ok(value) => value,
        Err(error) => {
            drop(guard);
            send(
                &mut socket,
                ServerMessage::Error {
                    message: error.to_string(),
                },
            )
            .await?;
            return Ok(());
        }
    };
    if guard.active.is_some() && !takeover {
        drop(guard);
        send(&mut socket, ServerMessage::Busy).await?;
        return Ok(());
    }
    let previous = guard.active.take();
    let bridge_id = guard.identity.id.clone();
    let name = guard.name.clone();
    let device = guard.device.clone();
    drop(guard);
    // Revoke ownership before awaiting any old socket writes. Admission stays serialized.
    if let Some(mut previous) = previous {
        device.detach(&previous.id).await;
        let _ = previous.cancel.send(());
        if tokio::time::timeout(Duration::from_secs(3), &mut previous.task)
            .await
            .is_err()
        {
            previous.task.abort();
            let _ = previous.task.await;
        }
    }
    let session_id = Uuid::new_v4().to_string();
    let (cancel, cancelled) = oneshot::channel();
    let worker_state = state.clone();
    let id = session_id.clone();
    // Keep the lock until the slot is installed, including when the worker exits immediately.
    let mut guard = state.lock().await;
    let task = tokio::spawn(async move {
        let result = serve(
            &mut socket,
            cancelled,
            &worker_state,
            &id,
            bridge_id,
            name,
            credential,
        )
        .await;
        device.detach(&id).await;
        let mut guard = worker_state.lock().await;
        if guard.active.as_ref().is_some_and(|active| active.id == id) {
            guard.active = None;
        }
        if let Err(error) = result {
            eprintln!("Session {id}: {error:#}");
        }
        println!("Companion disconnected: {id}");
    });
    guard.active = Some(Active {
        id: session_id,
        cancel,
        task,
    });
    Ok(())
}

async fn serve(
    socket: &mut Socket,
    mut cancelled: oneshot::Receiver<()>,
    state: &Arc<Mutex<State>>,
    id: &str,
    bridge_id: String,
    name: String,
    credential: Option<String>,
) -> Result<()> {
    let device = state.lock().await.device.clone();
    let mut hardware = device.bind(id).await?;
    if let Some(token) = credential {
        send(socket, ServerMessage::Paired { token }).await?;
    }
    send(
        socket,
        ServerMessage::Connected {
            bridge_id,
            session_id: id.into(),
        },
    )
    .await?;
    send(socket, ServerMessage::Status { bridge_name: name }).await?;
    println!("Companion connected: {id}");
    let mut last_received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = &mut cancelled => {
                let _ = send(socket, ServerMessage::Replaced).await;
                let _ = tokio::time::timeout(Duration::from_millis(500), socket.close(None)).await;
                return Ok(());
            }
            event = hardware.recv() => {
                let Some(event) = event else { return Ok(()) };
                if !state.lock().await.active.as_ref().is_some_and(|active| active.id == id) { continue; }
                match event {
                    stadiacast::Event::Message(message) => send(socket, message).await?,
                    stadiacast::Event::Audio { metadata, data } => {
                        send(socket, metadata).await?;
                        for chunk in data.chunks(64 * 1024) {
                            tokio::time::timeout(Duration::from_secs(2), socket.send(Message::Binary(chunk.to_vec().into()))).await??;
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(last_received + Duration::from_secs(35)) => {
                bail!("Companion heartbeat timed out");
            }
            message = socket.next() => {
                let Some(message) = message else { return Ok(()) };
                last_received = tokio::time::Instant::now();
                // The old connection may have data queued when a takeover revokes it.
                if !state.lock().await.active.as_ref().is_some_and(|active| active.id == id) {
                    continue;
                }
                match message? {
                    Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text)? {
                        ClientMessage::Rumble { session_id, request_id, controller } if session_id == id => {
                            device.rumble(id, controller, request_id).await?;
                        }
                        ClientMessage::AudioReceived { session_id, recording_id } if session_id == id => {
                            device.received(id, recording_id).await?;
                        }
                        ClientMessage::Ping { session_id, request_id } if session_id == id => {
                            send(socket, ServerMessage::Pong { session_id, request_id }).await?;
                        }
                        _ => {
                            send(socket, ServerMessage::Error { message: "Invalid or stale session command".into() }).await?;
                        }
                    },
                    Message::Close(_) => return Ok(()),
                    Message::Ping(data) => {
                        tokio::time::timeout(Duration::from_secs(2), socket.send(Message::Pong(data))).await??;
                    }
                    _ => {}
                }
            }
        }
    }
}
