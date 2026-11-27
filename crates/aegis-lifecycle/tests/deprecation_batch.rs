//! "Xong khi" of #705: on the Rust fixture with a deprecated function
//! called from two functions, a deprecation job lists the two calls at the
//! base with the profile's `[deprecations]` command, fixes each in a
//! session of its own, merges both into `agent/integration-<issue>` with
//! the gate after each merge, and opens one PR naming the old API and its
//! replacement. The model is scripted per group; cargo, the gate, git and
//! the ledger are real.

mod common;

use std::process::Command;
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{IssueId, IssueKind, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{IssueInput, Platform, run_issue};
use aegis_provider::{
    ContentBlock, Message, ModelProvider, ProviderError, Request, Response, Role, StopReason, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{PRICES, Scratch, config, fork_refs, git, github, repo_with_bug, tools_ready};

const DEPRECATED: &str = "
#[deprecated(note = \"use `sum_of` instead\")]
pub fn old_total(a: u32) -> u32 {
    a
}

pub fn sum_of(a: u32) -> u32 {
    a
}

pub fn first() -> u32 {
    old_total(1)
}

pub fn second() -> u32 {
    old_total(2)
}
";

/// Each group's script: the function's place in its first message, the
/// line of its call, and what replaces it.
struct Script {
    place: String,
    line: usize,
    content: &'static str,
}

struct PerGroup {
    scripts: Vec<Script>,
    requests: Mutex<Vec<Request>>,
}

fn text_of(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn reply(content: Vec<ContentBlock>) -> Response {
    let tool = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    Response {
        id: "msg".into(),
        model: "scripted".into(),
        content,
        stop_reason: Some(if tool {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        }),
        usage: Usage {
            input_tokens: 1000,
            output_tokens: 100,
            ..Usage::default()
        },
    }
}

impl ModelProvider for PerGroup {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let system = request.system.concat();
        let content = match request.model.as_str() {
            "light" if system.contains("You triage") => panic!("a deprecation job is not triaged"),
            "light" if system.contains("failed check") => vec![ContentBlock::text("failed")],
            "light" => vec![ContentBlock::text(
                "chore: replace the deprecated old_total",
            )],
            "core" => {
                let first = text_of(&request.messages[0]);
                let script = self
                    .scripts
                    .iter()
                    .find(|s| first.contains(&s.place))
                    .unwrap_or_else(|| panic!("no script for {first}"));
                let step = request
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::Assistant)
                    .count();
                match step {
                    0 => vec![ContentBlock::ToolUse {
                        id: format!("t{}", script.line),
                        name: "edit".into(),
                        input: json!({"file": "src/lib.rs", "from": script.line, "to": script.line, "content": script.content}),
                    }],
                    _ => vec![ContentBlock::text(
                        json!({
                            "status": "fixed",
                            "cause": "a call of the deprecated old_total",
                            "evidence": [{"location": format!("src/lib.rs:{}", script.line)}],
                            "changes": [{"location": format!("src/lib.rs:{}", script.line), "note": "call sum_of"}],
                            "allowed_test_changes": [],
                            "stop_reason": null
                        })
                        .to_string(),
                    )],
                }
            }
            other => panic!("unexpected model {other}"),
        };
        Ok(reply(content))
    }
}

fn show(fork: &std::path::Path, spec: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(fork)
        .args(["show", spec])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The fixture with the deprecated function and its two callers, committed;
/// each caller's script.
fn repo_with_deprecations() -> (Scratch, Vec<Script>) {
    let scratch = repo_with_bug();
    let repo = scratch.0.join("clone");
    let lib = repo.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap() + DEPRECATED;
    std::fs::write(&lib, &text).unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "old_total",
        ],
    );
    let line_of = |needle: &str| text.lines().position(|l| l == needle).unwrap() + 1;
    let script = |name: &str, content| {
        let start = line_of(&format!("pub fn {name}() -> u32 {{"));
        Script {
            place: format!("src/lib.rs:{start}-{}", start + 2),
            line: start + 1,
            content,
        }
    };
    let scripts = vec![
        script("first", "    sum_of(1)"),
        script("second", "    sum_of(2)"),
    ];
    (scratch, scripts)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_deprecated_calls_in_two_functions_become_one_pr() {
    if !tools_ready() {
        return;
    }
    let (scratch, scripts) = repo_with_deprecations();
    let server = github().await;
    let p = Arc::new(Platform {
        config: config(&scratch, &server.uri(), 20),
        provider: PerGroup {
            scripts,
            requests: Mutex::new(vec![]),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github: aegis_pr::TokenSource::Static("t".into()),
    });
    let issue = IssueInput {
        cve: None,
        id: "fixture-9".into(),
        title: "Deprecation warnings in fixture".into(),
        body: "The scheduled deprecation cleanup.".into(),
        url: "https://github.com/me/fixture".into(),
        base: None,
        hidden: None,
        kind: Some(IssueKind::Deprecation),
        source: Some("deprecation".into()),
        memory: String::new(),
    };
    let report = run_issue(Arc::clone(&p), issue, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.model_calls, 4, "two sessions of two Core calls");
    assert_eq!(
        fork_refs(&scratch),
        "refs/heads/agent/integration-fixture-9"
    );

    let lib = show(
        &scratch.0.join("fork.git"),
        "agent/integration-fixture-9:src/lib.rs",
    );
    assert!(
        lib.contains("    sum_of(1)") && lib.contains("    sum_of(2)"),
        "{lib}"
    );
    assert_eq!(
        lib.matches("old_total(").count(),
        1,
        "only its definition is left: {lib}"
    );

    let record = p
        .store
        .record(&IssueId::parse("fixture-9").unwrap())
        .unwrap();
    assert_eq!(record.kind, Some(IssueKind::Deprecation));
    assert_eq!(record.outcome, Some(Outcome::PrOpened));
    assert_eq!(record.gate_runs.len(), 4);
    assert!(record.gate_runs.iter().all(|g| g.passed));

    let first = p
        .provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.model == "core")
        .map(|r| text_of(&r.messages[0]))
        .unwrap();
    assert!(first.contains("deprecation warnings"), "{first}");
    assert!(!first.contains("Sonar"), "{first}");

    let pr: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(pr["head"], "agent/integration-fixture-9");
    let body = pr["body"].as_str().unwrap();
    assert!(
        body.contains("2 of 2 warnings fixed in 2 of 2 groups"),
        "{body}"
    );
    // The PR's text is inert: the compiler's backticks are quotes there.
    assert!(
        body.contains(
            "'src/lib.rs:73' use of deprecated function 'old_total': use 'sum_of' instead"
        ) || body.contains("use of deprecated function 'old_total': use 'sum_of' instead"),
        "the old API and its replacement: {body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repo_without_deprecations_stops_with_nothing_to_fix() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    let p = Arc::new(Platform {
        config: config(&scratch, &server.uri(), 20),
        provider: PerGroup {
            scripts: vec![],
            requests: Mutex::new(vec![]),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github: aegis_pr::TokenSource::Static("t".into()),
    });
    let issue = IssueInput {
        cve: None,
        id: "fixture-10".into(),
        title: "Deprecation warnings in fixture".into(),
        body: String::new(),
        url: String::new(),
        base: None,
        hidden: None,
        kind: Some(IssueKind::Deprecation),
        source: Some("deprecation".into()),
        memory: String::new(),
    };
    let report = run_issue(Arc::clone(&p), issue, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::StoppedSteps, "{report:?}");
    assert!(
        report
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("no deprecation warnings"),
        "{report:?}"
    );
    assert!(p.provider.requests.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}
