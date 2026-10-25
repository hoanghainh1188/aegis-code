//! HMAC-SHA256 with the session key, for what the server hands out and
//! must get back unchanged, such as the review form's `opened_at` (#167).
//! Each use has its own purpose, so a signature made for one is worth
//! nothing for another.

use aegis_secrets::{Secret, Secrets};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use super::{MIN_SESSION_KEY_LEN, SESSION_KEY, session_key};

#[derive(Clone)]
pub struct Signer {
    key: Key,
}

#[derive(Clone)]
enum Key {
    Fixed(Vec<u8>),
    /// The session key as the store last read it, so a rotated key signs
    /// from then on; the one read at start if the store's became unusable.
    Store {
        secrets: Secrets,
        at_start: Secret,
    },
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer").finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

impl Signer {
    /// With a key of its own: for tests and tools.
    pub fn new(key: &[u8]) -> Self {
        Self {
            key: Key::Fixed(key.to_vec()),
        }
    }

    /// With the session key of the secret store, which must hold it.
    pub fn from_secrets(secrets: Secrets) -> Result<Self, String> {
        let at_start = session_key(&secrets)?;
        Ok(Self {
            key: Key::Store { secrets, at_start },
        })
    }

    fn key(&self) -> Vec<u8> {
        match &self.key {
            Key::Fixed(key) => key.clone(),
            Key::Store { secrets, at_start } => secrets
                .get(SESSION_KEY)
                .filter(|k| k.expose().len() >= MIN_SESSION_KEY_LEN)
                .unwrap_or_else(|| at_start.clone())
                .expose()
                .as_bytes()
                .to_vec(),
        }
    }

    fn mac(&self, purpose: &str, message: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key()).expect("HMAC takes a key of any length");
        mac.update(purpose.as_bytes());
        mac.update(&[0]);
        mac.update(message);
        mac
    }

    /// The signature of `message` for `purpose`, as hex.
    pub fn sign(&self, purpose: &str, message: &str) -> String {
        hex(&self
            .mac(purpose, message.as_bytes())
            .finalize()
            .into_bytes())
    }

    /// Whether `signature` is [`Signer::sign`]'s; in constant time.
    pub fn verify(&self, purpose: &str, message: &str, signature: &str) -> bool {
        unhex(signature).is_some_and(|sig| {
            self.mac(purpose, message.as_bytes())
                .verify_slice(&sig)
                .is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_hold_only_for_their_message_purpose_and_key() {
        let s = Signer::new(b"0123456789abcdef0123456789abcdef");
        let sig = s.sign("review", "7:1:1000");
        assert!(s.verify("review", "7:1:1000", &sig));
        assert!(!s.verify("review", "7:1:1001", &sig));
        assert!(!s.verify("login", "7:1:1000", &sig));
        assert!(!s.verify("review", "7:1:1000", "zz"));
        assert!(!Signer::new(b"another key").verify("review", "7:1:1000", &sig));
    }

    #[test]
    fn a_rotated_session_key_signs_from_then_on() {
        let first = "k".repeat(MIN_SESSION_KEY_LEN);
        let source = aegis_secrets::MemorySource::new([(SESSION_KEY, first.as_str())]);
        let secrets = Secrets::new(source.clone());
        assert!(
            Signer::from_secrets(secrets.clone()).is_err(),
            "not loaded yet"
        );
        block_on(secrets.load(SESSION_KEY)).unwrap();
        let s = Signer::from_secrets(secrets.clone()).unwrap();
        let before = s.sign("review", "m");
        assert_eq!(before, Signer::new(first.as_bytes()).sign("review", "m"));
        source.set(SESSION_KEY, &"n".repeat(MIN_SESSION_KEY_LEN));
        block_on(secrets.refresh());
        assert_ne!(s.sign("review", "m"), before);
        assert!(!s.verify("review", "m", &before));
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }
}
