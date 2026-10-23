//! "Xong khi" of #17: a Sonar issue with warnings in two functions of the
//! Rust fixture becomes two sessions side by side, each passing its own
//! gate, merged into `agent/integration-<issue>` with the gate after each
//! merge, and one PR. The model is scripted per group; the ACI server,
//! the gate, git and the ledger are real.

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

use common::{PRICES, config, fork_refs, github, repo_with_bug, tools_ready};

const WARNINGS: &str = r#"{"total": 2, "issues": [
  {"key": "W-add", "rule": "rust:S125", "severity": "MINOR", "component": "fixture:src/lib.rs",
   "line": 27, "textRange": {"startLine": 27, "endLine": 27}, "flows": [],
   "message": "Explain why the push cannot overflow."},
  {"key": "W-total", "rule": "rust:S7471", "severity": "MINOR", "component": "fixture:src/lib.rs",
   "line": 34, "textRange": {"startLine": 34, "endLine": 34}, "flows": [],
   "message": "Sum copied values instead of references."}
]}"#;

/// Each group's Core script, by the place its first message names.
const SCRIPTS: [(&str, u32, &str, &str); 2] = [
    (
        "src/lib.rs:23-29",
        27,
        "        self.items.push(price); // the length was checked above",
        "explain the push",
    ),
    (
        "src/lib.rs:33-35",
        34,
        "        self.items.iter().copied().sum()",
        "sum copied values",
    ),
];

struct PerGroup {
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
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "sonar", "has_repro_info": false, "suspected_crates": [], "summary": "Two Sonar warnings."}"#,
            )],
            "light" if system.contains("failed check") => vec![ContentBlock::text("failed")],
            "light" => vec![ContentBlock::text(
                "chore: fix the Sonar warnings of the cart",
            )],
            "core" => {
                let first = text_of(&request.messages[0]);
                let (_, line, content, note) = SCRIPTS
                    .iter()
                    .find(|(place, ..)| first.contains(place))
                    .unwrap_or_else(|| panic!("no script for {first}"));
                let step = request
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::Assistant)
                    .count();
                match step {
                    0 => vec![ContentBlock::ToolUse {
                        id: format!("t{line}"),
                        name: "edit".into(),
                        input: json!({"file": "src/lib.rs", "from": line, "to": line, "content": content}),
                    }],
                    _ => vec![ContentBlock::text(
                        json!({
                            "status": "fixed",
                            "cause": "Sonar warning",
                            "changes": [{"location": format!("src/lib.rs:{line}"), "note": note}],
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

#[tokio::test(flavor = "multi_thread")]
async fn a_sonar_issue_goes_from_warnings_to_one_pr() {
    if !tools_ready() {
        return;
    }
    let scratch = repo_with_bug();
    let server = github().await;
    let p = Arc::new(Platform {
        config: config(&scratch, &server.uri(), 20),
        provider: PerGroup {
            requests: Mutex::new(vec![]),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    });
    let issue = IssueInput {
        id: "fixture-8".into(),
        title: "Sonar warnings in the cart".into(),
        body: format!("Two new warnings.\n\n```json\n{WARNINGS}\n```\n"),
        url: "https://github.com/me/fixture/issues/8".into(),
        base: None,
        hidden: None,
    };

    let report = run_issue(Arc::clone(&p), issue, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(
        report.pr_url.as_deref(),
        Some("https://github.com/me/fixture/pull/3")
    );
    // Both groups' Core calls are the issue's.
    assert_eq!(report.model_calls, 4);
    assert_eq!(
        fork_refs(&scratch),
        "refs/heads/agent/integration-fixture-8"
    );

    let lib = show(
        &scratch.0.join("fork.git"),
        "agent/integration-fixture-8:src/lib.rs",
    );
    assert!(lib.contains("// the length was checked above"), "{lib}");
    assert!(lib.contains("self.items.iter().copied().sum()"), "{lib}");

    let record = p
        .store
        .record(&IssueId::parse("fixture-8").unwrap())
        .unwrap();
    assert_eq!(record.kind, Some(IssueKind::Sonar));
    assert_eq!(record.outcome, Some(Outcome::PrOpened));
    // Each group's own gate, then the gate after each of the two merges.
    assert_eq!(record.gate_runs.len(), 4);
    assert!(record.gate_runs.iter().all(|g| g.passed));
    // The groups' records hold their sessions but never finish: KPIs count
    // the batch once.
    for group in ["fixture-8-g1", "fixture-8-g2"] {
        assert_eq!(p.store.record(&IssueId::parse(group).unwrap()), None);
    }

    let pr: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(pr["head"], "agent/integration-fixture-8");
    let body = pr["body"].as_str().unwrap();
    assert!(
        body.contains("2 of 2 warnings fixed in 2 of 2 groups"),
        "{body}"
    );
    assert!(
        body.contains("explain the push") && body.contains("sum copied values"),
        "{body}"
    );
}
