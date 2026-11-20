//! Detects a real recording and immediately takes over, checking cancellation without a shared filesystem transport.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use std::{fs, io::BufReader, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    time::{sleep, timeout},
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{client_async, tungstenite::Message};
use wave_companion::network;
use wave_protocol::{ClientMessage, Credential, Invitation, ServerMessage, VERSION};

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let path = std::env::args().nth(1).context("Pass pairing.json path")?;
    let invitation: Invitation = serde_json::from_slice(&fs::read(path)?)?;
    let credential: Credential =
        serde_json::from_str(&network::keyring_entry(&invitation.bridge_id)?.get_password()?)?;
    let worker = network::start();
    let bridge = timeout(Duration::from_secs(20), async {
        loop {
            if let Some(bridge) = worker
                .state
                .lock()
                .unwrap()
                .bridges
                .iter()
                .find(|b| b.id == invitation.bridge_id)
                .cloned()
            {
                break bridge;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(credential.certificate_pem.as_bytes())) {
        roots.add(cert?)?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    println!("READY: press and hold Assistant until the controller microphone stops.");
    let recording = timeout(Duration::from_secs(120), async {
        loop {
            let value: serde_json::Value = http
                .get("http://127.0.0.1:8008/_mic")
                .send()
                .await?
                .json()
                .await?;
            if value["on"] == true && value["recording"]["packets"].as_u64().unwrap_or(0) > 0 {
                break anyhow::Ok(
                    value["recording"]["file"]
                        .as_str()
                        .context("Missing file")?
                        .to_owned(),
                );
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    println!("Detected live recording; taking over now.");
    let tcp = TcpStream::connect((bridge.address.as_str(), bridge.port)).await?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("stadia-wave.local")?, tcp)
        .await?;
    let mut socket = client_async("wss://stadia-wave.local/ws", tls).await?.0;
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Connect {
                version: VERSION,
                companion_id: credential.companion_id,
                token: credential.token,
                takeover: true,
            })?
            .into(),
        ))
        .await?;
    loop {
        let message = timeout(Duration::from_secs(5), socket.next())
            .await?
            .context("Socket closed")??;
        let event: ServerMessage = serde_json::from_str(message.to_text()?)?;
        if matches!(event, ServerMessage::Connected { .. }) {
            break;
        }
        if let ServerMessage::Error { message } = event {
            bail!(message);
        }
    }
    let value: serde_json::Value = http
        .get("http://127.0.0.1:8008/_mic")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(value["on"], false);
    assert!(
        !std::path::Path::new(&recording).exists(),
        "Cancelled source file must be removed"
    );
    while let Ok(Some(message)) = timeout(Duration::from_secs(2), socket.next()).await {
        if let Message::Text(text) = message? {
            let event: ServerMessage = serde_json::from_str(&text)?;
            assert!(!matches!(
                event,
                ServerMessage::Audio { .. } | ServerMessage::RecordingStarted { .. }
            ));
        }
    }
    socket.close(None).await?;
    println!(
        "PASS: microphone stopped during recording; cancelled audio removed and never sent to new session."
    );
    Ok(())
}
