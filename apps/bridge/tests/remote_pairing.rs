use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};
use wave_protocol::{
    ClientMessage, ServerMessage, VERSION,
    pairing::{self, Challenge, Ed25519Group, Password, Spake2},
};

struct Bridge {
    child: Child,
    dir: PathBuf,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Bridge {
    fn code(&self) -> Result<String> {
        let output = Command::new(env!("CARGO_BIN_EXE_wave-bridge"))
            .args(["pair", "--data-dir"])
            .arg(&self.dir)
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}

async fn bootstrap(port: u16, code: &str) -> Result<(Challenge, String)> {
    let (a, b) = pairing::identities();
    let (exchange, message) =
        Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &a, &b);
    let stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let (mut ws, _) = client_async("ws://localhost/ws", stream).await?;
    let hello = serde_json::to_string(&ClientMessage::PairingHello { message })?;
    ws.send(Message::Text(hello.into())).await?;
    let payload = ws.next().await.context("No pairing response")??;
    let challenge: Challenge = serde_json::from_str(payload.to_text()?)?;
    let secret = exchange
        .finish(&challenge.message)
        .map_err(|_| anyhow::anyhow!("Bad SPAKE2 message"))?;
    let key = pairing::key(&secret);
    ring::hmac::verify(
        &key,
        &pairing::transcript("server-proof", &challenge.id, &challenge.certificate_pem),
        &challenge.proof,
    )
    .map_err(|_| anyhow::anyhow!("Bad server proof"))?;
    let grant = pairing::grant(&key, &challenge.id, &challenge.certificate_pem);
    Ok((challenge, grant))
}

type Socket = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

async fn authenticate(port: u16, challenge: &Challenge, auth: ClientMessage) -> Result<Socket> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut challenge.certificate_pem.as_bytes()) {
        roots.add(certificate?)?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect("stadia-wave.local".try_into()?, stream)
        .await?;
    let (mut ws, _) = client_async("wss://localhost/ws", stream).await?;
    ws.send(Message::Text(serde_json::to_string(&auth)?.into()))
        .await?;
    Ok(ws)
}

async fn receive(socket: &mut Socket) -> Result<ServerMessage> {
    let message = socket
        .next()
        .await
        .context("No authentication response")??;
    Ok(serde_json::from_str(message.to_text()?)?)
}

fn pair(id: &str, token: String) -> ClientMessage {
    ClientMessage::Pair {
        version: VERSION,
        companion_id: id.into(),
        name: id.into(),
        token,
        takeover: true,
    }
}

async fn check_remote_pairing() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let dir = PathBuf::from("/tmp").join(format!("wave-pair-test-{}", uuid::Uuid::new_v4()));
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let mut bridge = Bridge {
        child: Command::new(env!("CARGO_BIN_EXE_wave-bridge"))
            .args(["--remote", "--port", &port.to_string(), "--data-dir"])
            .arg(&dir)
            .stdout(Stdio::piped())
            .spawn()?,
        dir,
    };
    let output = bridge
        .child
        .stdout
        .take()
        .context("Missing Bridge stdout")?;
    let mut output = BufReader::new(output);
    let mut ready = String::new();
    output.read_line(&mut ready)?;
    ensure!(ready.starts_with("Bridge ready"), "{ready}");
    assert!(!bridge.dir.join("pairing.json").exists());
    assert!(bootstrap(port, "00000000").await.is_err());
    let first_code = bridge.code()?;
    // Exhaust the attempt budget with incorrect codes, then confirm the correct code also fails.
    let wrong = if first_code == "00000000" {
        "00000001"
    } else {
        "00000000"
    };
    for _ in 0..5 {
        assert!(bootstrap(port, wrong).await.is_err());
    }
    assert!(bootstrap(port, &first_code).await.is_err());

    let code = bridge.code()?;
    let (challenge, grant) = bootstrap(port, &code).await?;
    let mut first = authenticate(port, &challenge, pair("first", grant.clone())).await?;
    let ServerMessage::Paired { token } = receive(&mut first).await? else {
        bail!("Expected first credential")
    };
    assert!(matches!(
        receive(&mut first).await?,
        ServerMessage::Connected { .. }
    ));
    assert!(matches!(
        receive(&mut first).await?,
        ServerMessage::Status { .. }
    ));
    let mut replay = authenticate(port, &challenge, pair("replay", grant)).await?;
    assert!(matches!(
        receive(&mut replay).await?,
        ServerMessage::Error { .. }
    ));

    let code = bridge.code()?;
    let (replacement, grant) = bootstrap(port, &code).await?;
    assert_eq!(replacement.certificate_pem, challenge.certificate_pem);
    let mut second = authenticate(port, &replacement, pair("second", grant)).await?;
    assert!(matches!(
        receive(&mut second).await?,
        ServerMessage::Paired { .. }
    ));
    loop {
        match receive(&mut first).await? {
            ServerMessage::Replaced => break,
            ServerMessage::Hardware { .. } => {}
            other => bail!("Expected replacement, received {other:?}"),
        }
    }
    let mut revoked = authenticate(
        port,
        &challenge,
        ClientMessage::Connect {
            version: VERSION,
            companion_id: "first".into(),
            token,
            takeover: true,
        },
    )
    .await?;
    assert!(matches!(
        receive(&mut revoked).await?,
        ServerMessage::Error { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn short_code_is_explicit_limited_single_use_and_replaces_authorization() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), check_remote_pairing()).await?
}
