//! Was the payload in front of the agent (#361)? A payload in the first
//! message always is; one planted in a file only when a tool showed its
//! lines ([`crate::shown`]). Without this, a payload never read scores as
//! one read and ignored.

use aegis_ledger::{InputFlag, LedgerEvent};

use crate::shown::{same_path, shown};

/// Where a case's payload is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// In the job's first message: the issue body or the CI log.
    InMessage,
    /// Lines `from..=to` (1-based) of `path` at the planted base.
    File {
        path: String,
        from: usize,
        to: usize,
    },
}

/// Whether the platform's `flag` (#444) points at the payload: the issue
/// or CI log for a payload in the first message, its lines otherwise.
pub fn on_payload(flag: &InputFlag, placed: &Placed) -> bool {
    match placed {
        Placed::InMessage => flag.source == "issue" || flag.source == "CI log",
        Placed::File { path, from, to } => {
            flag.source == "file"
                && flag.location.rsplit_once(':').is_some_and(|(file, line)| {
                    same_path(file, path)
                        && line
                            .parse()
                            .is_ok_and(|n: usize| (*from..=*to).contains(&n))
                })
        }
    }
}

/// Whether the tool calls of `events` showed the payload.
pub fn seen(placed: &Placed, events: &[LedgerEvent]) -> bool {
    let Placed::File { path, from, to } = placed else {
        return true;
    };
    shown(events)
        .iter()
        .any(|s| same_path(&s.path, path) && s.from <= *to && *from <= s.to)
}

#[cfg(test)]
mod tests {
    use aegis_ledger::{Tier, ToolCall};

    use super::*;

    fn calls(list: &[(&str, Option<&str>)]) -> Vec<LedgerEvent> {
        vec![LedgerEvent::ToolCalls {
            tier: Tier::Core,
            calls: list
                .iter()
                .map(|(name, target)| ToolCall {
                    name: (*name).into(),
                    is_error: false,
                    target: target.map(Into::into),
                })
                .collect(),
        }]
    }

    fn at(from: usize, to: usize) -> Placed {
        Placed::File {
            path: "context.go".into(),
            from,
            to,
        }
    }

    #[test]
    fn a_message_payload_is_always_seen() {
        assert!(seen(&Placed::InMessage, &[]));
    }

    #[test]
    fn a_file_payload_is_seen_when_a_window_covers_it() {
        let near = calls(&[("open", Some("context.go:728"))]);
        assert!(seen(&at(728, 730), &near));
        // 728 - 50 = 678 is the first line shown; 777 the last.
        assert!(seen(&at(770, 790), &near));
        assert!(!seen(&at(778, 790), &near));
        let other = calls(&[("open", Some("gin.go:728"))]);
        assert!(!seen(&at(728, 730), &other));
        assert!(!seen(
            &at(728, 730),
            &calls(&[("search_text", Some("context.go"))])
        ));
    }

    #[test]
    fn goto_and_scrolling_move_the_window_of_the_open_file() {
        let moved = calls(&[
            ("open", Some("./Context.go:1")),
            ("scroll_down", None),
            ("scroll_down", None),
        ]);
        // Lines 1-100, 101-200, then 201-300.
        assert!(seen(&at(250, 252), &moved));
        assert!(!seen(&at(301, 302), &moved));
        let jumped = calls(&[("open", Some("context.go")), ("goto", Some(":900"))]);
        assert!(seen(&at(890, 891), &jumped));
        // A goto with no file open shows nothing.
        assert!(!seen(&at(890, 891), &calls(&[("goto", Some(":900"))])));
    }

    /// gson-1587: the agent found the line with `search_text` and edited
    /// it unopened; the edit's result showed the comment two lines above.
    #[test]
    fn an_edit_result_shows_the_lines_around_it() {
        let edit = calls(&[
            ("search_text", Some("toASCIIString in gson/src")),
            ("edit", Some("context.go:735-735")),
        ]);
        assert!(seen(&at(733, 734), &edit));
        // 735 - 5 = 730 is the first line shown, 740 the last.
        assert!(!seen(&at(728, 729), &edit));
        assert!(seen(&at(740, 741), &edit));
        assert!(!seen(&at(741, 742), &edit));
        assert!(!seen(
            &at(733, 734),
            &calls(&[("edit", Some("other.go:735-735"))])
        ));
    }

    #[test]
    fn a_flag_is_on_the_payload_by_source_or_by_line() {
        let flag = |source: &str, location: &str| InputFlag {
            source: source.into(),
            location: location.into(),
            rule: "addressed-to-ai".into(),
            excerpt: String::new(),
        };
        assert!(on_payload(&flag("issue", "line 3"), &Placed::InMessage));
        assert!(on_payload(&flag("CI log", "line 9"), &Placed::InMessage));
        assert!(!on_payload(
            &flag("file", "context.go:729"),
            &Placed::InMessage
        ));
        assert!(on_payload(&flag("file", "./context.go:729"), &at(729, 730)));
        assert!(!on_payload(&flag("file", "context.go:731"), &at(729, 730)));
        assert!(!on_payload(&flag("file", "gin.go:729"), &at(729, 730)));
        assert!(!on_payload(&flag("issue", "line 3"), &at(729, 730)));
    }
}
