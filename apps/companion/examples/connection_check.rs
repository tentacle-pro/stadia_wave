//! End-to-end regression checks for takeover, automatic reconnect and stale commands.
//! Usage: cargo run -p wave-companion --example connection_check -- target/debug/wave-bridge
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use std::{
    fs,
    io::BufReader,
    path::PathBuf,
    process::{Child, Command as Process},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{client_async, tungstenite::Message};
use uuid::Uuid;
use wave_companion::network::{self, Bridge, Command, Worker};
use wave_protocol::{ClientMessage, Credential, Invitation, ServerMessage, VERSION};

struct Rig {
    process: Option<Child>,
    directory: PathBuf,
    profiles: Vec<String>,
    bridge_id: Option<String>,
}

impl Rig {
    fn launch(&mut self, binary: &str, port: u16) -> Result<()> {
        self.process = Some(
            Process::new(binary)
                .args(["--port", &port.to_string(), "--data-dir"])
                .arg(&self.directory)
                .spawn()?,
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.stop();
        if let Some(id) = &self.bridge_id {
            for profile in &self.profiles {
                if let Ok(entry) =
                    keyring::Entry::new("dev.stadiawave.companion", &format!("{id}/{profile}"))
                {
                    let _ = entry.delete_credential();
                }
            }
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> Result<T> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if let Some(value) = check() {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("Timed out waiting for state")
}

async fn connected(worker: &Worker) -> Result<String> {
    wait_for(|| {
        let view = worker.state.lock().unwrap();
        if view.connected && view.heartbeats > 0 {
            view.session_id.clone()
        } else {
            None
        }
    })
    .await
    .with_context(|| worker.state.lock().unwrap().status.clone())
}

async fn raw(
    bridge: &Bridge,
    credential: &Credential,
    takeover: bool,
    invalid: bool,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(credential.certificate_pem.as_bytes())) {
        roots.add(cert?)?;
    }
    let tls = TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));
    let tcp = TcpStream::connect((bridge.address.as_str(), bridge.port)).await?;
    let stream = tls
        .connect(ServerName::try_from("stadia-wave.local")?, tcp)
        .await?;
    let mut socket = client_async("wss://stadia-wave.local/ws", stream).await?.0;
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Connect {
                version: VERSION,
                companion_id: credential.companion_id.clone(),
                token: if invalid {
                    "invalid".into()
                } else {
                    credential.token.clone()
                },
                takeover,
            })?
            .into(),
        ))
        .await?;
    Ok(socket)
}

async fn receive<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Result<ServerMessage>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = timeout(Duration::from_secs(5), socket.next())
            .await?
            .context("Socket closed")??;
        let event: ServerMessage = serde_json::from_str(message.to_text()?)?;
        if matches!(event, ServerMessage::Hardware { .. }) {
            continue;
        }
        return Ok(event);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let binary = std::env::args()
        .nth(1)
        .context("Pass the wave-bridge binary path")?;
    let port = {
        let listener = std::net::TcpListener::bind("0.0.0.0:0")?;
        listener.local_addr()?.port()
    };
    let profiles = vec![
        format!("check-a-{}", Uuid::new_v4()),
        format!("check-b-{}", Uuid::new_v4()),
    ];
    let mut rig = Rig {
        process: None,
        directory: std::env::temp_dir().join(format!("wave-check-{}", Uuid::new_v4())),
        profiles: profiles.clone(),
        bridge_id: None,
    };
    rig.launch(&binary, port)?;
    let invitation_path = rig.directory.join("pairing.json");
    let invitation: Invitation = wait_for(|| {
        fs::read(&invitation_path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
    })
    .await?;
    rig.bridge_id = Some(invitation.bridge_id.clone());
    let a = network::start_with_profile(profiles[0].clone());
    let b = network::start_with_profile(profiles[1].clone());
    let bridge = wait_for(|| {
        a.state
            .lock()
            .unwrap()
            .bridges
            .iter()
            .find(|entry| entry.id == invitation.bridge_id)
            .cloned()
    })
    .await?;
    let connect = |worker: &Worker| {
        worker.commands.send(Command::Connect {
            bridge: bridge.clone(),
            invitation_path: invitation_path.to_string_lossy().into(),
        })
    };
    connect(&a)?;
    let old_id = connected(&a).await?;
    connect(&b)?;
    let b_id = connected(&b).await?;
    assert_ne!(old_id, b_id);
    wait_for(|| {
        a.state
            .lock()
            .unwrap()
            .status
            .contains("已被接管")
            .then_some(())
    })
    .await?;
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(!a.state.lock().unwrap().retrying);
    assert!(!a.state.lock().unwrap().connected);
    assert_eq!(b.state.lock().unwrap().session_id.as_ref(), Some(&b_id));
    println!("PASS: two paired identities, takeover and no reconnect fight");

    let entry = keyring::Entry::new(
        "dev.stadiawave.companion",
        &format!("{}/{}", bridge.id, profiles[0]),
    )?;
    let credential: Credential = serde_json::from_str(&entry.get_password()?)?;
    let mut rejected = raw(&bridge, &credential, true, true).await?;
    assert!(matches!(
        receive(&mut rejected).await?,
        ServerMessage::Error { .. }
    ));
    let mut busy = raw(&bridge, &credential, false, false).await?;
    assert!(matches!(receive(&mut busy).await?, ServerMessage::Busy));
    assert_eq!(b.state.lock().unwrap().session_id.as_ref(), Some(&b_id));
    println!("PASS: failed authentication and automatic reconnect cannot evict active session");

    // A reacquires the Bridge, then must reconnect automatically after its restart.
    connect(&a)?;
    connected(&a).await?;
    rig.stop()?;
    wait_for(|| a.state.lock().unwrap().retrying.then_some(())).await?;
    rig.launch(&binary, port)?;
    connected(&a).await?;
    println!("PASS: server restart restores authenticated connection");

    let mut current = raw(&bridge, &credential, true, false).await?;
    let active_id = match receive(&mut current).await? {
        ServerMessage::Connected { session_id, .. } => session_id,
        other => bail!("Expected Connected, received {other:?}"),
    };
    assert!(matches!(
        receive(&mut current).await?,
        ServerMessage::Status { .. }
    ));
    current
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Ping {
                session_id: old_id,
                request_id: "stale".into(),
            })?
            .into(),
        ))
        .await?;
    assert!(matches!(
        receive(&mut current).await?,
        ServerMessage::Error { .. }
    ));
    current
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Ping {
                session_id: active_id.clone(),
                request_id: "current".into(),
            })?
            .into(),
        ))
        .await?;
    assert!(
        matches!(receive(&mut current).await?, ServerMessage::Pong { session_id, .. } if session_id == active_id)
    );
    println!("PASS: old cleanup and stale commands cannot clear or mutate new session");
    current.close(None).await?;
    a.commands.send(Command::Disconnect)?;
    b.commands.send(Command::Disconnect)?;
    Ok(())
}
