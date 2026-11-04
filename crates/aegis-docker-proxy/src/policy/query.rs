//! A call's query string, read as the daemon reads it (Go's
//! `url.ParseQuery`): `+` is a space, `%XX` is a byte, `;` is an error. A
//! key twice is refused, since the proxy and the daemon could each take a
//! different one, and so is a key the call does not take.

use std::collections::BTreeMap;

/// The decoded parameters of `query`, each of them in `allowed`.
pub fn parse(query: Option<&str>, allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let query = query.unwrap_or("");
    if query.contains(';') {
        return Err("`;` in the query".into());
    }
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (key, value) = (unescape(key)?, unescape(value)?);
        if !allowed.contains(&key.as_str()) {
            return Err(format!("query parameter {key:?} is not allowed here"));
        }
        if out.insert(key.clone(), value).is_some() {
            return Err(format!("query parameter {key:?} given twice"));
        }
    }
    Ok(out)
}

fn unescape(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or_else(|| format!("bad escape in {s:?}"))?;
                out.push(hex);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| format!("{s:?} is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_as_the_daemon_does() {
        let q = parse(
            Some("fromImage=docker.io%2Flibrary%2Fimg&tag=latest"),
            &["fromImage", "tag"],
        )
        .unwrap();
        assert_eq!(q["fromImage"], "docker.io/library/img");
        assert_eq!(q["tag"], "latest");
        assert!(parse(None, &[]).unwrap().is_empty());
    }

    #[test]
    fn an_escaped_or_repeated_key_is_refused() {
        // `n%61me` is `name` to the daemon.
        let twice = parse(Some("n%61me=evil&name=aegis-gate-1-0"), &["name"]).unwrap_err();
        assert!(twice.contains("twice"), "{twice}");
        let unknown = parse(Some("fromSrc=-"), &["fromImage"]).unwrap_err();
        assert!(unknown.contains("fromSrc"), "{unknown}");
        assert!(parse(Some("name=a;b"), &["name"]).is_err());
        assert!(parse(Some("name=%zz"), &["name"]).is_err());
    }
}
