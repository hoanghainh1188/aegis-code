//! HMAC-SHA256 with the session key, for what the server hands out and
//! must get back unchanged: the review form's `opened_at` (#167) and the
//! sign-in state cookie. Each use has its own purpose, so a signature made
//! for one is worth nothing for another.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

#[derive(Clone)]
pub struct Signer {
    key: Vec<u8>,
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
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    fn mac(&self, purpose: &str, message: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes a key of any length");
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

    /// `message` and its signature in one cookie-safe string.
    pub fn seal(&self, purpose: &str, message: &str) -> String {
        let body = hex(message.as_bytes());
        let signature = self.sign(purpose, &body);
        format!("{body}.{signature}")
    }

    /// The message of a [`Signer::seal`], if it is one and unchanged.
    pub fn open(&self, purpose: &str, sealed: &str) -> Option<String> {
        let (body, signature) = sealed.split_once('.')?;
        if !self.verify(purpose, body, signature) {
            return None;
        }
        String::from_utf8(unhex(body)?).ok()
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
    fn sealed_messages_open_unchanged_only() {
        let s = Signer::new(b"k");
        let sealed = s.seal("login", "{\"next\":\"/jobs?a=1\"}");
        assert_eq!(
            s.open("login", &sealed).as_deref(),
            Some("{\"next\":\"/jobs?a=1\"}")
        );
        let tampered = format!("00{sealed}");
        assert_eq!(s.open("login", &tampered), None);
        assert_eq!(s.open("review", &sealed), None);
        assert_eq!(s.open("login", "nonsense"), None);
    }
}
