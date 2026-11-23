//! Bootstrap TLS trust with a one-time password. The grant is sent only over pinned TLS.
use ring::hmac;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Challenge {
    pub id: String,
    pub certificate_pem: String,
    pub message: Vec<u8>,
    pub proof: Vec<u8>,
}

pub fn key(secret: &[u8]) -> hmac::Key {
    hmac::Key::new(hmac::HMAC_SHA256, secret)
}

pub fn transcript(label: &str, id: &str, cert: &str) -> Vec<u8> {
    let mut input = b"stadia-wave/remote-pair/v1".to_vec();
    for field in [label, id, cert] {
        input.extend_from_slice(&(field.len() as u64).to_be_bytes());
        input.extend_from_slice(field.as_bytes());
    }
    input
}

pub fn grant(key: &hmac::Key, id: &str, cert: &str) -> String {
    hex::encode(hmac::sign(key, &transcript("tls-grant", id, cert)).as_ref())
}

pub use spake2::{Ed25519Group, Identity, Password, Spake2};

pub fn identities() -> (Identity, Identity) {
    (
        Identity::new(b"stadia-wave/companion/v1"),
        Identity::new(b"stadia-wave/bridge/v1"),
    )
}
