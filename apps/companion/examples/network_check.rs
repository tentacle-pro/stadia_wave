//! Exercises real mDNS discovery, pairing, Keychain persistence and WSS heartbeats.
use anyhow::{Context, Result, bail};
use std::{
    fs,
    time::{Duration, Instant},
};
use wave_companion::network::{self, Command};
use wave_protocol::Invitation;

fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if let Some(value) = check() {
            return Ok(value);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("Timed out waiting for network state")
}

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("Usage: network_check /path/to/pairing.json")?;
    let invitation: Invitation = serde_json::from_slice(&fs::read(&path)?)?;
    let worker = network::start();
    let bridge = wait_for(|| {
        worker
            .state
            .lock()
            .unwrap()
            .bridges
            .iter()
            .find(|bridge| bridge.id == invitation.bridge_id)
            .cloned()
    })?;
    println!("DISCOVERED {}:{} using mDNS", bridge.address, bridge.port);
    for attempt in 0..2 {
        worker.commands.send(Command::Connect {
            bridge: bridge.clone(),
            invitation_path: if attempt == 0 {
                path.clone()
            } else {
                String::new()
            },
        })?;
        wait_for(|| {
            let state = worker.state.lock().unwrap();
            if state.connected && state.heartbeats >= 2 {
                Some(())
            } else {
                None
            }
        })
        .with_context(|| worker.state.lock().unwrap().status.clone())?;
        worker.commands.send(Command::Disconnect)?;
        wait_for(|| {
            if !worker.state.lock().unwrap().connected {
                Some(())
            } else {
                None
            }
        })?;
        println!(
            "PASS connection {}: WSS authenticated, two heartbeats, clean disconnect",
            attempt + 1
        );
    }
    Ok(())
}
