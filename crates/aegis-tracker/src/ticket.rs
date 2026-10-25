//! A ticket and its logs, as the agent reads them.

use crate::TrackerError;

/// Longest key taken: `PROJECT_KEY-123456`.
const MAX_KEY_LEN: usize = 64;
/// Logs read per ticket, the first attached.
pub(crate) const MAX_LOGS: usize = 3;
/// What is downloaded of one log at most; a bigger one is skipped.
pub(crate) const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// What is kept of one log: its end, where the failure usually is.
const MAX_LOG_CHARS: usize = 6_000;
/// Files read as logs, by extension.
const LOG_EXTENSIONS: [&str; 7] = ["log", "txt", "out", "err", "trace", "stacktrace", "csv"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub key: String,
    pub title: String,
    pub body: String,
    /// Where people open it.
    pub url: String,
    pub logs: Vec<Log>,
}

/// A text attachment, cut to its last [`MAX_LOG_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    pub name: String,
    pub text: String,
    /// Its start was cut.
    pub truncated: bool,
}

impl Ticket {
    /// The description, then each log in a fence it cannot close.
    pub fn body_with_logs(&self) -> String {
        let mut body = self.body.trim_end().to_owned();
        if self.logs.is_empty() {
            return body;
        }
        body.push_str("\n\n## Attached logs\n");
        for log in &self.logs {
            let fence = "`".repeat(longest_run(&log.text, '`').max(2) + 1);
            let cut = if log.truncated { " (its end only)" } else { "" };
            body.push_str(&format!(
                "\n{}{cut}:\n\n{fence}text\n{}\n{fence}\n",
                log.name,
                log.text.trim_end()
            ));
        }
        body
    }
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for x in text.chars() {
        run = if x == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// `APP-12`: a project key (upper-case letters, digits, `_`, starting
/// with a letter), `-`, a number. The same in Jira and Backlog; it goes
/// into URLs as it is.
pub fn checked_key(key: &str) -> Result<&str, TrackerError> {
    let ok = key.len() <= MAX_KEY_LEN
        && key.split_once('-').is_some_and(|(project, number)| {
            project.starts_with(|c: char| c.is_ascii_uppercase())
                && project
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                && !number.is_empty()
                && !number.starts_with('0')
                && number.bytes().all(|b| b.is_ascii_digit())
        });
    if ok {
        Ok(key)
    } else {
        Err(TrackerError::Config(format!("`{key}` is not a ticket key")))
    }
}

/// Whether an attachment is read as a log: a text file by its type or
/// name, not too big to download.
pub(crate) fn is_log(name: &str, mime: Option<&str>, size: u64) -> bool {
    if size > MAX_LOG_BYTES {
        return false;
    }
    let by_type = mime.is_some_and(|m| m.starts_with("text/"));
    let by_name = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| LOG_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
    by_type || by_name
}

/// `bytes` as text, its end only past [`MAX_LOG_CHARS`].
pub(crate) fn log(name: &str, bytes: &[u8], cut_at_download: bool) -> Log {
    let text = String::from_utf8_lossy(bytes);
    let count = text.chars().count();
    let (text, truncated) = if count > MAX_LOG_CHARS {
        (text.chars().skip(count - MAX_LOG_CHARS).collect(), true)
    } else {
        (text.into_owned(), cut_at_download)
    };
    Log {
        name: name.to_owned(),
        text,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_project_dash_number() {
        for good in ["APP-1", "A1_B-20", "X-999999"] {
            assert!(checked_key(good).is_ok(), "{good}");
        }
        for bad in [
            "app-1", "1APP-1", "APP-01", "APP-", "-1", "APP-1-2", "APP-1#", "APP",
        ] {
            assert!(checked_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn logs_are_text_files_of_a_reasonable_size() {
        assert!(is_log("app.LOG", None, 10));
        assert!(is_log("dump", Some("text/plain"), 10));
        assert!(!is_log("shot.png", Some("image/png"), 10));
        assert!(!is_log("big.log", None, MAX_LOG_BYTES + 1));
    }

    #[test]
    fn a_log_cannot_close_its_fence() {
        let ticket = Ticket {
            key: "APP-1".into(),
            title: "t".into(),
            body: "b".into(),
            url: "u".into(),
            logs: vec![log("x.log", b"```\nescaped?\n````", false)],
        };
        let body = ticket.body_with_logs();
        assert!(body.contains("\n`````text\n"), "{body}");
        assert!(body.trim_end().ends_with("`````"), "{body}");
    }
}
