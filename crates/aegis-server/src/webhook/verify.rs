//! Who sent a delivery (ADR 0003 §4): GitHub signs the body with the
//! webhook's secret (`X-Hub-Signature-256`, HMAC-SHA256), GitLab sends the
//! secret token itself (`X-Gitlab-Token`). Both compared in constant time.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// `header` is `sha256=<hex of HMAC-SHA256(secret, body)>`.
pub fn github(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(signature) = header
        .and_then(|h| h.strip_prefix("sha256="))
        .and_then(decode_hex)
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// `header` is the token itself. Compared as digests, so neither its
/// length nor its content shows in the time taken.
pub fn gitlab(token: &[u8], header: Option<&str>) -> bool {
    let Some(given) = header else {
        return false;
    };
    let given = Sha256::digest(given.as_bytes());
    let expected = Sha256::digest(token);
    given.as_slice().ct_eq(expected.as_slice()).into()
}

/// An Azure DevOps service hook (#636) signs nothing: it sends the
/// secret as its Basic auth password (`authorization`, any user name) or
/// in a header of the subscription's (`custom`). Both are checked, so the
/// time taken does not say which was sent.
pub fn azure_devops(secret: &[u8], authorization: Option<&str>, custom: Option<&str>) -> bool {
    use base64::Engine;
    let password = authorization
        .and_then(|a| a.strip_prefix("Basic "))
        .and_then(|b| {
            base64::engine::general_purpose::STANDARD
                .decode(b.trim())
                .ok()
        })
        .and_then(|d| String::from_utf8(d).ok())
        .and_then(|d| d.split_once(':').map(|(_, p)| p.to_owned()));
    let basic = gitlab(secret, password.as_deref());
    let header = gitlab(secret, custom);
    basic | header
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's own example ("Validating webhook deliveries").
    const SECRET: &[u8] = b"It's a Secret to Everybody";
    const BODY: &[u8] = b"Hello, World!";
    const SIGNATURE: &str =
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn github_signatures_are_checked() {
        assert!(github(SECRET, BODY, Some(SIGNATURE)));
        assert!(!github(SECRET, b"Hello, World?", Some(SIGNATURE)));
        assert!(!github(b"another secret", BODY, Some(SIGNATURE)));
        assert!(!github(
            SECRET,
            BODY,
            Some(&SIGNATURE.replace("sha256=", "sha1="))
        ));
        assert!(!github(
            SECRET,
            BODY,
            Some(&SIGNATURE[..SIGNATURE.len() - 2])
        ));
        assert!(!github(SECRET, BODY, Some("sha256=zz")));
        assert!(!github(SECRET, BODY, None));
    }

    #[test]
    fn azure_secrets_come_as_a_basic_password_or_a_header() {
        // `aegis:s3cret`.
        assert!(azure_devops(
            b"s3cret",
            Some("Basic YWVnaXM6czNjcmV0"),
            None
        ));
        assert!(azure_devops(b"s3cret", None, Some("s3cret")));
        // `aegis:other`.
        assert!(!azure_devops(
            b"s3cret",
            Some("Basic YWVnaXM6b3RoZXI="),
            None
        ));
        assert!(!azure_devops(b"s3cret", Some("Bearer s3cret"), None));
        assert!(!azure_devops(b"s3cret", Some("Basic !!"), Some("no")));
        assert!(!azure_devops(b"s3cret", None, None));
    }

    #[test]
    fn gitlab_tokens_are_checked() {
        assert!(gitlab(b"token", Some("token")));
        assert!(!gitlab(b"token", Some("token2")));
        assert!(!gitlab(b"token", Some("")));
        assert!(!gitlab(b"token", None));
    }

    #[test]
    fn hex_is_decoded() {
        assert_eq!(decode_hex("00ff7A"), Some(vec![0, 255, 122]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("é0"), None);
    }
}
