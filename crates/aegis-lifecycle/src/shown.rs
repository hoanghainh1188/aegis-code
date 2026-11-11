//! The file lines the agent's tools showed it, replayed from the ledger's
//! tool calls: what the agent read of the repo. Used to tell whether a
//! planted payload was seen (#361) and to scan what was read for embedded
//! instructions (#444).

use aegis_ledger::LedgerEvent;

/// Lines the ACI viewer shows at a time (`aegis_aci::WINDOW`).
const WINDOW: usize = 100;

/// Lines an `edit` result shows around the edited range
/// (`aegis_aci::CONTEXT`).
const EDIT_CONTEXT: usize = 5;

/// Lines `from..=to` (1-based) of `path`, as the tool call named it
/// (a leading `./` dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub path: String,
    pub from: usize,
    pub to: usize,
}

/// Every range the tool calls of `events` showed, in order. Two kinds of
/// call show a file's lines: the viewer, replayed here (`open file[:line]`,
/// `goto :line`, `scroll_down`, `scroll_up`), and `edit`, whose result
/// shows the edited range with [`EDIT_CONTEXT`] lines around it (an agent
/// can edit a line it found with `search_text` without opening the file).
/// The end of a file can shift a window up; that is not modelled, which
/// can only miss lines near the end, never invent any.
pub fn shown(events: &[LedgerEvent]) -> Vec<Shown> {
    let mut ranges = Vec::new();
    let mut open: Option<(String, usize)> = None;
    for (name, target) in calls(events) {
        let moved = step(&open, name, target);
        if let Some((path, first)) = &moved {
            ranges.push(Shown {
                path: path.clone(),
                from: *first,
                to: first + WINDOW - 1,
            });
            open = moved;
        }
        if let ("edit", Some(t)) = (name, target)
            && let Some((path, first, last)) = edited(t)
        {
            ranges.push(Shown {
                path,
                from: first.saturating_sub(EDIT_CONTEXT).max(1),
                to: last + EDIT_CONTEXT,
            });
        }
    }
    ranges
}

/// The same path, as a tool or a planting wrote it.
pub fn same_path(a: &str, b: &str) -> bool {
    a.trim()
        .trim_start_matches("./")
        .eq_ignore_ascii_case(b.trim().trim_start_matches("./"))
}

/// The viewer after a call that moves it: the open file and its first
/// shown line; `None` when the call does not move the viewer.
fn step(
    open: &Option<(String, usize)>,
    name: &str,
    target: Option<&str>,
) -> Option<(String, usize)> {
    let centred = |line: usize| line.saturating_sub(WINDOW / 2).max(1);
    let (file, first) = open.as_ref().map(|(f, l)| (f.clone(), *l)).unzip();
    match (name, target) {
        ("open", Some(t)) => {
            let (path, line) = split(t);
            Some((clean(path), centred(line.unwrap_or(1))))
        }
        ("goto", Some(t)) => Some((file?, centred(split(t).1?))),
        ("scroll_down", _) => Some((file?, first? + WINDOW)),
        ("scroll_up", _) => Some((file?, first?.saturating_sub(WINDOW).max(1))),
        _ => None,
    }
}

/// An `edit` target `file:from-to` into its file and range.
fn edited(target: &str) -> Option<(String, usize, usize)> {
    let (file, range) = target.split_once(':')?;
    let range: String = range
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    let (from, to) = range.split_once('-').unwrap_or((&range, &range));
    Some((clean(file), from.parse().ok()?, to.parse().ok()?))
}

/// `file:line` (or `:line`) into its file and line.
fn split(target: &str) -> (&str, Option<usize>) {
    match target.split_once(':') {
        Some((file, rest)) => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (file, digits.parse().ok())
        }
        None => (target, None),
    }
}

fn clean(path: &str) -> String {
    path.trim().trim_start_matches("./").to_owned()
}

fn calls(events: &[LedgerEvent]) -> impl Iterator<Item = (&str, Option<&str>)> {
    events.iter().flat_map(|e| match e {
        LedgerEvent::ToolCalls { calls, .. } => calls
            .iter()
            .map(|c| (c.name.as_str(), c.target.as_deref()))
            .collect(),
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use aegis_ledger::{Tier, ToolCall};

    use super::*;

    #[test]
    fn ranges_keep_the_path_as_written_and_follow_the_viewer() {
        let events = vec![LedgerEvent::ToolCalls {
            tier: Tier::Core,
            calls: [
                ("open", Some("./src/Lib.rs:120")),
                ("scroll_down", None),
                ("search_text", Some("fn x in src")),
                ("edit", Some("src/a.rs:3-4")),
            ]
            .iter()
            .map(|(name, target)| ToolCall {
                name: (*name).into(),
                is_error: false,
                target: target.map(Into::into),
            })
            .collect(),
        }];
        let r = |path: &str, from, to| Shown {
            path: path.into(),
            from,
            to,
        };
        assert_eq!(
            shown(&events),
            [
                r("src/Lib.rs", 70, 169),
                r("src/Lib.rs", 170, 269),
                r("src/a.rs", 1, 9)
            ]
        );
        assert!(same_path("./src/Lib.rs", "src/lib.rs"));
        assert!(!same_path("src/lib.rs", "lib.rs"));
    }
}
