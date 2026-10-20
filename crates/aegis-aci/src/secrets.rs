//! Secret values hidden from what the model sees (design §7). Text from
//! the repo (file windows, search hits, edit snippets) and from outside
//! (issues, gate logs) is scanned for common secret shapes; each value is
//! replaced by `[REDACTED:<kind>]`. Only the value goes: the rest of the
//! line, comments included, stays, since comments are the documentation of
//! legacy code (Proposal §7). Files on disk are never changed.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

/// What a hidden value is replaced with; also refused in edits, so the
/// agent cannot write the placeholder over the real value.
pub const MARKER: &str = "[REDACTED:";

/// (kind, pattern). The value to hide is the capture group `v`, or the
/// whole match when the pattern has none.
const PATTERNS: &[(&str, &str)] = &[
    ("aws-access-key", r"\b(?P<v>(?:AKIA|ASIA)[A-Z0-9]{16})\b"),
    (
        "aws-secret-key",
        r#"(?i)aws_?secret_?(?:access_?)?key\s*[:=]\s*["']?(?P<v>[A-Za-z0-9/+=]{40})\b"#,
    ),
    ("github-token", r"\b(?P<v>gh[pousr]_[A-Za-z0-9]{36,})\b"),
    ("github-token", r"\b(?P<v>github_pat_[A-Za-z0-9_]{22,})\b"),
    ("anthropic-key", r"\b(?P<v>sk-ant-[A-Za-z0-9_-]{20,})"),
    ("openai-key", r"\b(?P<v>sk-(?:proj-)?[A-Za-z0-9_-]{20,})"),
    ("slack-token", r"\b(?P<v>xox[abposr]-[A-Za-z0-9-]{10,})"),
    ("google-api-key", r"\b(?P<v>AIza[0-9A-Za-z_-]{35})"),
    ("stripe-key", r"\b(?P<v>(?:sk|rk)_live_[0-9A-Za-z]{24,})"),
    (
        "jwt",
        r"\b(?P<v>eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})",
    ),
    (
        "url-password",
        r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:(?P<v>[^\s@/]{3,})@",
    ),
    (
        "assigned-secret",
        r#"(?i)\b(?:password|passwd|pwd|secret|api[_-]?key|access[_-]?key|auth[_-]?token|private[_-]?key|client[_-]?secret)\b["']?\s*[:=]\s*["'](?P<v>[^"'\s]{8,})["']"#,
    ),
];

const PEM_BEGIN: &str = "-----BEGIN ";
const PEM_END: &str = "-----END ";

static COMPILED: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .map(|(kind, p)| (*kind, Regex::new(p).expect("secret pattern compiles")))
        .collect()
});

/// Words a real secret value practically never contains, but placeholders
/// in docs and tests do (`username:password@`, `aws_secret`, `fake-key`).
const PLACEHOLDER_WORDS: &[&str] = &[
    "changeme",
    "example",
    "placeholder",
    "your",
    "xxxx",
    "dummy",
    "redacted",
    "fake",
    "sample",
    "test",
    "password",
    "secret",
    "pass",
    "foo",
    "bar",
];

/// Text in a password position that is a placeholder or a reference to
/// one, not a value. Only the `password = "…"` and `user:…@` shapes are
/// filtered: the token shapes (`ghp_…`, `AKIA…`) are specific enough.
fn placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.contains('{')
        || value.starts_with('<')
        || value.starts_with('$')
        || value.starts_with("%(")
        || value.contains(MARKER)
        || PLACEHOLDER_WORDS.iter().any(|w| lower.contains(w))
        || value.chars().all(|c| c.is_ascii_digit())
        || value
            .chars()
            .all(|c| c == value.chars().next().unwrap_or(c))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// Values hidden, by kind.
    pub hidden: BTreeMap<&'static str, usize>,
}

impl Redacted {
    pub fn count(&self) -> usize {
        self.hidden.values().sum()
    }

    /// The text, with a line telling the agent what was hidden.
    pub fn with_notice(&self) -> String {
        if self.hidden.is_empty() {
            return self.text.clone();
        }
        let kinds: Vec<&str> = self.hidden.keys().copied().collect();
        format!(
            "{}\n[aegis: {} secret value(s) hidden ({}). The file is unchanged; leave those \
             values as they are.]",
            self.text,
            self.count(),
            kinds.join(", ")
        )
    }
}

fn redact_line(line: &str, hidden: &mut BTreeMap<&'static str, usize>) -> String {
    let mut out = line.to_owned();
    for (kind, re) in COMPILED.iter() {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for caps in re.captures_iter(&out) {
            let m = caps.name("v").or_else(|| caps.get(0)).expect("match");
            if matches!(*kind, "assigned-secret" | "url-password") && placeholder(m.as_str()) {
                continue;
            }
            spans.push((m.start(), m.end()));
        }
        // Right to left, so earlier offsets stay valid.
        for (start, end) in spans.into_iter().rev() {
            out.replace_range(start..end, &format!("{MARKER}{kind}]"));
            *hidden.entry(kind).or_default() += 1;
        }
    }
    out
}

/// Hides secret values in `text`, line by line; a PEM private key block is
/// hidden whole, from its `BEGIN` line to its `END` line.
pub fn redact(text: &str) -> Redacted {
    let mut hidden = BTreeMap::new();
    let mut lines = Vec::new();
    let mut in_key = false;
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let is_key_line = |marker: &str| body.contains(marker) && body.contains("PRIVATE KEY-----");
        if is_key_line(PEM_BEGIN) {
            in_key = true;
            *hidden.entry("private-key").or_default() += 1;
            lines.push(format!("{body}{newline}"));
            continue;
        }
        if in_key {
            if is_key_line(PEM_END) {
                in_key = false;
                lines.push(format!("{body}{newline}"));
            } else {
                // Keep a window's `n:` prefix so the line numbers still add up.
                let prefix = body
                    .split_once(':')
                    .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .map(|(n, _)| format!("{n}:"))
                    .unwrap_or_default();
                lines.push(format!("{prefix}{MARKER}private-key]{newline}"));
            }
            continue;
        }
        lines.push(format!("{}{newline}", redact_line(body, &mut hidden)));
    }
    Redacted {
        text: lines.concat(),
        hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fake values built at run time, so no secret-shaped string sits in
    // the source for scanners to flag.
    fn fake(prefix: &str, body: char, n: usize) -> String {
        format!("{prefix}{}", body.to_string().repeat(n))
    }

    #[test]
    fn hides_common_token_shapes_and_keeps_the_rest() {
        let aws = format!("AKIA{}", "ABCDEFGHIJ234567");
        let gh = fake("ghp_", 'a', 36);
        let ant = fake("sk-ant-api03-", 'b', 30);
        let slack = fake("xoxb-", '1', 20);
        let google = fake("AIza", 'c', 35);
        let text = format!(
            "12:    key = \"{aws}\"  # deploy key, keep for now\n13:token={gh}\n14:x({ant}, {slack}, {google})\n"
        );
        let r = redact(&text);
        assert_eq!(
            r.text,
            "12:    key = \"[REDACTED:aws-access-key]\"  # deploy key, keep for now\n\
             13:token=[REDACTED:github-token]\n\
             14:x([REDACTED:anthropic-key], [REDACTED:slack-token], [REDACTED:google-api-key])\n"
        );
        assert_eq!(r.count(), 5);
    }

    #[test]
    fn hides_assigned_secrets_but_not_placeholders() {
        let r = redact(
            "password = \"hunter2hunter2\"\napi_key: 'tok-12345678'\nsecret = \"${API_SECRET}\"\n\
             password = \"changeme\"\npassword = \"short\"\nPASSWORD_FIELD = 'password'\n",
        );
        assert_eq!(
            r.text,
            "password = \"[REDACTED:assigned-secret]\"\napi_key: '[REDACTED:assigned-secret]'\n\
             secret = \"${API_SECRET}\"\npassword = \"changeme\"\npassword = \"short\"\n\
             PASSWORD_FIELD = 'password'\n"
        );
    }

    #[test]
    fn hides_url_passwords_jwts_and_whole_private_keys() {
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "a".repeat(12),
            "b".repeat(12),
            "c".repeat(12)
        );
        let begin = format!("{PEM_BEGIN}RSA PRIVATE KEY-----");
        let end = format!("{PEM_END}RSA PRIVATE KEY-----");
        let text = format!(
            "1:url = \"postgres://app:s3cr3tpw@db:5432/x\"\n2:auth = \"{jwt}\"\n3:{begin}\n4:MIIEowIBAAKCAQEA\n5:qwertyuiop\n6:{end}\n7:after\n"
        );
        let r = redact(&text);
        assert_eq!(
            r.text,
            format!(
                "1:url = \"postgres://app:[REDACTED:url-password]@db:5432/x\"\n\
                 2:auth = \"[REDACTED:jwt]\"\n3:{begin}\n4:[REDACTED:private-key]\n\
                 5:[REDACTED:private-key]\n6:{end}\n7:after\n"
            )
        );
        assert!(r.with_notice().ends_with(
            "[aegis: 3 secret value(s) hidden (jwt, private-key, url-password). The file is \
             unchanged; leave those values as they are.]"
        ));
    }

    #[test]
    fn placeholders_in_urls_and_assignments_stay() {
        let docs = "ftp://username:password@host/\nhttps://user1:password1@h\ns3://aws_key:aws_secret@b\n\
                    https://user:443@h\nhttps://x-access-token:$GH_TOKEN@github.com\n\
                    ftp://foo:{pw_quoted}@h\nAWS_SECRET_ACCESS_KEY = \"settings_secret\"\n";
        let r = redact(docs);
        assert_eq!(r.text, docs, "{:?}", r.hidden);
        assert_eq!(
            redact("ftp://user:2%23um25%21M%23JZ@h\n").text,
            "ftp://user:[REDACTED:url-password]@h\n"
        );
    }

    #[test]
    fn ordinary_code_is_left_alone() {
        let code = "fn token(&self) -> &str { &self.token }\nlet password = read_password()?;\n\
                    // see https://example.com/docs#auth\nconst SKIP: &str = \"sk-\";\n";
        let r = redact(code);
        assert_eq!(r.text, code);
        assert!(r.hidden.is_empty());
        assert_eq!(r.with_notice(), code);
    }
}
