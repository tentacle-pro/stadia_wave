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
    net::{TcpListener, UnixListener},
    sync::{Mutex, oneshot},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use uuid::Uuid;
use wave_protocol::{ClientMessage, Invitation, SERVICE_TYPE, ServerMessage, VERSION};

#[derive(Clone, Serialize, Deserialize)]
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
    short_pairing: bool,
    pairing: Option<PairingWindow>,
    active: Option<Active>,
    name: String,
    device: stadiacast::Handle,
}

struct PairingWindow {
    code: String,
    deadline: tokio::time::Instant,
    attempts: u8,
    grants: Vec<String>,
}

impl PairingWindow {
    fn new() -> Result<Self> {
        use ring::rand::{SecureRandom, SystemRandom};
        let random = SystemRandom::new();
        let code = loop {
            let mut bytes = [0; 4];
            random
                .fill(&mut bytes)
                .map_err(|_| anyhow::anyhow!("Secure random unavailable"))?;
            let value = u32::from_ne_bytes(bytes);
            if value < 4_200_000_000 {
                break format!("{:08}", value % 100_000_000);
            }
        };
        Ok(Self {
            code,
            deadline: tokio::time::Instant::now() + Duration::from_secs(300),
            attempts: 5,
            grants: Vec::new(),
        })
    }

    fn valid(&self) -> bool {
        tokio::time::Instant::now() < self.deadline
    }
}

async fn print_pairing_code(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = fs::symlink_metadata(path.parent().context("Missing socket directory")?)?;
    if !directory.is_dir() || directory.permissions().mode() & 0o077 != 0 {
        bail!("Bridge socket directory must be private");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .context("Connect to the running Bridge (--remote)")?;
        if stream.peer_cred()?.uid() != directory.uid() {
            bail!("Bridge socket owner mismatch");
        }
        let (mut socket, _) = tokio_tungstenite::client_async("ws://localhost/ws", stream).await?;
        socket
            .send(Message::Text(
                serde_json::to_string(&ClientMessage::IssuePairingCode)?.into(),
            ))
            .await?;
        let response = socket
            .next()
            .await
            .context("Bridge closed the control socket")??;
        match serde_json::from_str(response.to_text()?)? {
            ServerMessage::PairingCode { code } => println!("{code}"),
            ServerMessage::Error { message } => bail!("{message}"),
            _ => bail!("Unexpected pairing response"),
        }
        Ok(())
    })
    .await?
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

/// The default gateway and this machine's address on the interface that routes to
/// it. A packaged app uses these when it has no saved Stadiacast configuration yet,
/// so the bundled Stadiacast starts without any setup.
fn lan_defaults() -> Option<(String, String)> {
    let route = std::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&route.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .map(|value| value.trim().to_string())
    };
    let gateway = field("gateway:")?;
    let interface = field("interface:")?;
    let address = std::process::Command::new("/usr/sbin/ipconfig")
        .args(["getifaddr", &interface])
        .output()
        .ok()?;
    let address = String::from_utf8_lossy(&address.stdout).trim().to_string();
    let ipv4 = |value: &str| value.parse::<Ipv4Addr>().is_ok();
    (ipv4(&gateway) && ipv4(&address)).then_some((gateway, address))
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
            "wave-bridge [--remote] [--name NAME] [--port PORT] [--data-dir PATH] [--stadiacast PATH --controller-ip IP --advertise-ip IP --dns-upstream IP]"
        );
        println!("wave-bridge pair [--data-dir PATH]");
        return Ok(());
    }
    let data = option("--data-dir").map(PathBuf::from).unwrap_or_else(|| {
        ProjectDirs::from("dev", "stadiawave", "bridge")
            .unwrap()
            .data_dir()
            .to_path_buf()
    });
    let socket_path = data.join("bridge.sock");
    if args.get(1).is_some_and(|arg| arg == "pair") {
        return print_pairing_code(&socket_path).await;
    }
    let short_pairing = args.iter().any(|arg| arg == "--remote");
    fs::create_dir_all(&data)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
    let port: Option<u16> = option("--port").map(|s| s.parse()).transpose()?;
    let port = port.or(short_pairing.then_some(wave_protocol::DEFAULT_PORT));
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("bridge.lock"))?;
    lock.try_lock()
        .context("Bridge already running or data directory cannot be locked")?;
    let local = if port.is_none() || short_pairing {
        match fs::symlink_metadata(&socket_path) {
            Ok(metadata) => {
                use std::os::unix::fs::FileTypeExt;
                if !metadata.file_type().is_socket() {
                    bail!("Bridge socket path is occupied by a non-socket");
                }
                if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                    bail!("Bridge socket is already accepting connections");
                }
                fs::remove_file(&socket_path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let local = UnixListener::bind(&socket_path).context("Bridge socket unavailable")?;
        struct RemoveSocket(PathBuf);
        impl Drop for RemoveSocket {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let remove_socket = RemoveSocket(socket_path.clone());
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        use std::os::unix::fs::MetadataExt;
        let local_uid = fs::metadata(&socket_path)?.uid();
        Some((local, remove_socket, local_uid))
    } else {
        None
    };
    let listener = match port {
        Some(port) => Some(TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?),
        None => None,
    };
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
    let name = option("--name").unwrap_or_else(|| "Wave Bridge".into());
    let advertisement = if let Some(port) = port {
        if short_pairing {
            match fs::remove_file(data.join("pairing.json")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            private_write(
                &data.join("pairing.json"),
                &serde_json::to_vec_pretty(&invitation)?,
            )?;
        }
        let daemon = ServiceDaemon::new()?;
        let properties = [
            ("version", VERSION.to_string()),
            ("bridge_id", identity.id.clone()),
            (
                "pairing",
                if short_pairing { "short-code" } else { "file" }.to_owned(),
            ),
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
        Some((daemon, fullname))
    } else {
        None
    };
    let config_path = data.join("stadiacast.json");
    let options: Option<stadiacast::Options> = if let Some(executable) = option("--stadiacast") {
        let options = stadiacast::Options {
            executable: PathBuf::from(executable)
                .canonicalize()
                .context("Stadiacast executable not found")?,
            controller_ip: Some(option("--controller-ip").context("--controller-ip required")?),
            advertise_ip: option("--advertise-ip").context("--advertise-ip required")?,
            dns_upstream: option("--dns-upstream").context("--dns-upstream required")?,
        };
        for address in options
            .controller_ip
            .iter()
            .chain([&options.advertise_ip, &options.dns_upstream])
        {
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
    } else if let Some(runtime) = bundled_stadiacast() {
        lan_defaults().map(|(gateway, address)| {
            eprintln!("Stadiacast 未配置，使用局域网默认值：DNS {gateway}，本机地址 {address}");
            stadiacast::Options {
                executable: runtime,
                controller_ip: None,
                advertise_ip: address,
                dns_upstream: gateway,
            }
        })
    } else {
        None
    };
    let device = stadiacast::Handle::start(options, &data)?;
    let state = Arc::new(Mutex::new(State {
        identity,
        path,
        invitation,
        short_pairing,
        pairing: None,
        active: None,
        name: name.clone(),
        device: device.clone(),
    }));
    let admission = Arc::new(Mutex::new(()));
    let mut tasks = tokio::task::JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    if let Some(port) = port.filter(|_| short_pairing) {
        println!(
            "Bridge ready: {name}\nPort: {port}\nRun wave-bridge pair to generate a pairing code."
        );
    } else if let Some(port) = port {
        println!(
            "Bridge ready: {name}\nPort: {port}\nCertificate SHA-256: {fingerprint}\nPairing invitation (valid 10 minutes, one use): {}",
            data.join("pairing.json").display()
        );
    } else {
        println!("Bridge ready: {name} (local socket)");
    }
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            incoming = async { match &listener { Some(listener) => listener.accept().await, None => std::future::pending().await } } => {
                let (stream, peer) = incoming?;
                let acceptor = acceptor.clone();
                let state = state.clone();
                let admission = admission.clone();
                tasks.spawn(async move {
                    if let Err(error) = handle(stream, acceptor, state, admission, short_pairing).await {
                        eprintln!("Connection {peer}: {error:#}");
                    }
                });
            }
            incoming = async {
                match &local {
                    Some((listener, _, uid)) => listener.accept().await.map(|(stream, _)| (stream, *uid)),
                    None => std::future::pending().await,
                }
            } => {
                let (stream, local_uid) = incoming?;
                if !stream.peer_cred().is_ok_and(|cred| cred.uid() == local_uid) {
                    continue;
                }
                let state = state.clone();
                let admission = admission.clone();
                tasks.spawn(async move {
                    let result = async {
                        let socket = match tokio::time::timeout(Duration::from_secs(10), accept_async(stream)).await? {
                            Ok(socket) => socket,
                            Err(tokio_tungstenite::tungstenite::Error::Protocol(
                                tokio_tungstenite::tungstenite::error::ProtocolError::HandshakeIncomplete
                            )) => return Ok(()),
                            Err(error) => return Err(error.into()),
                        };
                        authenticate(socket, true, state, admission).await
                    }.await;
                    if let Err(error) = result {
                        eprintln!("Local connection: {error:#}");
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
    if let Some((daemon, fullname)) = advertisement {
        daemon.unregister(&fullname)?;
        daemon.shutdown()?;
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> Transport for T {}
type Socket<T> = tokio_tungstenite::WebSocketStream<T>;

async fn send<T: Transport>(socket: &mut Socket<T>, message: ServerMessage) -> Result<()> {
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
    short_pairing: bool,
) -> Result<()> {
    let mut first = [0u8; 1];
    if short_pairing
        && tokio::time::timeout(Duration::from_secs(5), stream.peek(&mut first)).await?? == 1
        && first[0] == b'G'
    {
        return tokio::time::timeout(Duration::from_secs(5), pairing_bootstrap(stream, state))
            .await?;
    }
    let socket = tokio::time::timeout(Duration::from_secs(10), async {
        let stream = acceptor.accept(stream).await?;
        anyhow::Ok(accept_async(stream).await?)
    })
    .await??;
    authenticate(socket, false, state, admission).await
}

async fn pairing_bootstrap(stream: tokio::net::TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    use wave_protocol::pairing::{self, Ed25519Group, Password, Spake2};
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(4096))
        .max_frame_size(Some(4096));
    let mut socket = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await?;
    let request = socket.next().await.context("Missing pairing request")??;
    let ClientMessage::PairingHello { message } = serde_json::from_str(request.to_text()?)? else {
        bail!("Pairing bootstrap required");
    };
    let response = {
        let mut guard = state.lock().await;
        let id = guard.identity.id.clone();
        let certificate_pem = guard.identity.certificate_pem.clone();
        let window = guard
            .pairing
            .as_mut()
            .filter(|window| window.valid() && window.attempts > 0)
            .context("Pairing window unavailable")?;
        window.attempts -= 1;
        let (a, b) = pairing::identities();
        let (exchange, outbound) =
            Spake2::<Ed25519Group>::start_b(&Password::new(window.code.as_bytes()), &a, &b);
        let secret = exchange
            .finish(&message)
            .map_err(|_| anyhow::anyhow!("Invalid pairing message"))?;
        let key = pairing::key(&secret);
        window
            .grants
            .push(pairing::grant(&key, &id, &certificate_pem));
        let proof = ring::hmac::sign(
            &key,
            &pairing::transcript("server-proof", &id, &certificate_pem),
        );
        pairing::Challenge {
            id,
            certificate_pem,
            message: outbound,
            proof: proof.as_ref().to_vec(),
        }
    };
    socket
        .send(Message::Text(serde_json::to_string(&response)?.into()))
        .await?;
    Ok(())
}

async fn authenticate<T: Transport>(
    mut socket: Socket<T>,
    local: bool,
    state: Arc<Mutex<State>>,
    admission: Arc<Mutex<()>>,
) -> Result<()> {
    let first = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("Missing authentication")??;
    let auth: ClientMessage = serde_json::from_str(first.to_text()?)?;
    if local && matches!(auth, ClientMessage::IssuePairingCode) {
        let response = {
            let mut guard = state.lock().await;
            if guard.short_pairing {
                let window = PairingWindow::new()?;
                let code = window.code.clone();
                guard.pairing = Some(window);
                ServerMessage::PairingCode { code }
            } else {
                ServerMessage::Error {
                    message: "Start Bridge with --remote to issue a code".into(),
                }
            }
        };
        return send(&mut socket, response).await;
    }
    let _admission = admission.lock().await;
    let mut guard = state.lock().await;
    let result: Result<(bool, Option<String>)> = (|| {
        match auth {
            ClientMessage::LocalConnect { version, takeover } if local && version == VERSION => {
                Ok((takeover, None))
            }
            _ if local => bail!("Local connection required"),
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
                if guard.short_pairing {
                    if !guard.pairing.as_ref().is_some_and(|window| {
                        window.valid() && window.grants.iter().any(|grant| grant == &token)
                    }) {
                        bail!("Pairing code invalid, expired or already used");
                    }
                } else if unix_seconds() >= guard.invitation.expires_at
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
                let mut updated = guard.identity.clone();
                if guard.short_pairing {
                    updated.companions.clear();
                }
                updated
                    .companions
                    .insert(companion_id, hex::encode(Sha256::digest(secret.as_bytes())));
                private_write(&guard.path, &serde_json::to_vec_pretty(&updated)?)?;
                guard.identity = updated;
                if guard.short_pairing {
                    guard.pairing = None;
                    return Ok((takeover, Some(secret)));
                }
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

async fn serve<T: Transport>(
    socket: &mut Socket<T>,
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
