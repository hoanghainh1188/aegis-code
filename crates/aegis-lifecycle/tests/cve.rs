//! A CVE job (#501, ADR 0015) from start to PR: the platform raises the
//! package, the gate checks it under the upgrade policy (#500), and the
//! agent works only when the checks fail. The upgrade command is a stand-in
//! (`cp` of a prepared lockfile) and the gate's commands are too: what is
//! tested is the lifecycle, not cargo.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aegis_lang::Profiles;
use aegis_ledger::{IssueId, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{CveTarget, IssueInput, Platform, RunConfig, run_issue};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use common::{PRICES, Scratch, config, fork_refs, git, github, repo_with_bug};

/// Core follows its script; Light answers anything (commit messages).
struct Scripted {
    core: Mutex<VecDeque<Vec<ContentBlock>>>,
    requests: Mutex<Vec<Request>>,
}

impl ModelProvider for Scripted {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let content = match request.model.as_str() {
            "core" => self
                .core
                .lock()
                .unwrap()
                .pop_front()
                .expect("Core called more than scripted"),
            _ => vec![ContentBlock::text("chore: raise left-pad to 1.0.1")],
        };
        let stop = if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(Response {
            id: "r".into(),
            model: request.model.clone(),
            content,
            stop_reason: Some(stop),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 10,
                ..Usage::default()
            },
        })
    }
}

fn entry(version: &str) -> String {
    format!(
        "\n[[package]]\nname = \"left-pad\"\nversion = \"{version}\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"{}\"\n",
        "0".repeat(64)
    )
}

/// The fixture with `left-pad` 1.0.0 locked on `main`, the upgraded
/// lockfile prepared beside the clone, and a configuration whose upgrade
/// copies it in and whose test step is `test`.
fn setup(test: &str) -> (Scratch, RunConfig, String) {
    let scratch = repo_with_bug();
    let clone = scratch.0.join("clone");
    let lock = std::fs::read_to_string(clone.join("Cargo.lock")).unwrap();
    std::fs::write(
        clone.join("Cargo.lock"),
        format!("{lock}{}", entry("1.0.0")),
    )
    .unwrap();
    git(&clone, &["add", "-A"]);
    git(
        &clone,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "lock left-pad",
        ],
    );
    let upgraded = format!("{lock}{}", entry("1.0.1"));
    std::fs::write(scratch.0.join("left-pad-1.0.1.lock"), &upgraded).unwrap();

    let mut cfg = config(&scratch, "http://127.0.0.1:1", 20);
    let overrides: toml::Table = toml::from_str(&format!(
        r#"
upgrade = ["cp", "{root}/{{package}}-{{version}}.lock", "Cargo.lock"]
audit = ["true", "{{lockfile}}"]
[gate]
format = false
lint = false
test = ["sh", "-c", "{test}"]
coverage = ["sh", "-c", "echo TN: > lcov.info"]
[coverage]
format = "lcov"
path = "lcov.info"
"#,
        root = scratch.0.display()
    ))
    .unwrap();
    cfg.repo.overrides = overrides;
    (scratch, cfg, upgraded)
}

fn platform(cfg: RunConfig, core: Vec<Vec<ContentBlock>>) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config: cfg,
        provider: Scripted {
            core: Mutex::new(core.into()),
            requests: Mutex::new(vec![]),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github_token: "t".into(),
    })
}

fn cve_issue(package: &str, version: &str) -> IssueInput {
    let target = CveTarget {
        package: package.into(),
        version: version.into(),
        advisory: "RUSTSEC-2099-0001".into(),
    };
    IssueInput {
        id: "cve-left-pad".into(),
        title: target.title(),
        body: String::new(),
        url: target.url(),
        base: None,
        hidden: None,
        kind: None,
        cve: Some(target),
    }
}

fn fork_file(scratch: &Scratch, file: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(scratch.0.join("fork.git"))
        .args(["show", &format!("refs/heads/agent/cve-left-pad:{file}")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_that_passes_the_gate_opens_its_pr_without_the_agent() {
    let (scratch, mut cfg, upgraded) = setup("true");
    let server = github().await;
    cfg.repo.fork.api = server.uri();
    let p = platform(cfg, vec![]);
    let report = run_issue(
        Arc::clone(&p),
        cve_issue("left-pad", "1.0.1"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.model_calls, 0, "no Core or Expert call");
    assert_eq!(fork_refs(&scratch), "refs/heads/agent/cve-left-pad");
    assert_eq!(fork_file(&scratch, "Cargo.lock"), upgraded);
    // The PR names the advisory.
    let pulls = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&pulls[0].body).unwrap();
    let text = body["body"].as_str().unwrap();
    assert!(
        text.contains("https://osv.dev/vulnerability/RUSTSEC-2099-0001")
            && text.contains("'left-pad' is raised to 1.0.1"),
        "{text}"
    );
    let record = p
        .store
        .record(&IssueId::parse("cve-left-pad").unwrap())
        .unwrap();
    assert!(record.gate_runs[0].passed);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_breaking_upgrade_gets_the_agent_to_adapt_the_code() {
    // The new version "breaks" the code until it is adapted.
    let (scratch, mut cfg, upgraded) = setup("grep -q adapted src/lib.rs");
    let server = github().await;
    cfg.repo.fork.api = server.uri();
    let adapt = vec![ContentBlock::ToolUse {
        id: "t1".into(),
        name: "edit".into(),
        input: json!({"file": "src/lib.rs", "from": 1, "to": 0, "content": "// adapted to left-pad 1.0.1"}),
    }];
    let answer = vec![ContentBlock::text(
        json!({
            "status": "fixed",
            "cause": "left-pad 1.0.1 changed its API.",
            "changes": [{"location": "src/lib.rs:1", "note": "adapted to left-pad 1.0.1"}],
            "repro_test": null,
            "allowed_test_changes": [],
            "stop_reason": null
        })
        .to_string(),
    )];
    let p = platform(cfg, vec![adapt, answer]);
    let report = run_issue(
        Arc::clone(&p),
        cve_issue("left-pad", "1.0.1"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(report.model_calls, 2);
    // The agent was told what was done and what not to touch.
    let first = p
        .provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.model == "core")
        .map(|r| format!("{:?}", r.messages[0]))
        .unwrap();
    assert!(
        first.contains("RUSTSEC-2099-0001") && first.contains("Do not edit Cargo.lock"),
        "{first}"
    );
    assert_eq!(fork_file(&scratch, "Cargo.lock"), upgraded);
    assert!(fork_file(&scratch, "src/lib.rs").contains("adapted"));
    let record = p
        .store
        .record(&IssueId::parse("cve-left-pad").unwrap())
        .unwrap();
    assert_eq!(record.gate_runs.len(), 2);
    assert!(!record.gate_runs[0].passed && record.gate_runs[1].passed);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_that_fails_stops_the_job_with_its_reason() {
    let (scratch, cfg, _) = setup("true");
    let p = platform(cfg, vec![]);
    // No prepared lockfile for this version: `cp` fails.
    let report = run_issue(
        Arc::clone(&p),
        cve_issue("left-pad", "9.9.9"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    let reason = report.reason.unwrap_or_default();
    assert!(
        reason.contains("raising `left-pad` to 9.9.9 failed"),
        "{reason}"
    );
    assert_eq!(fork_refs(&scratch), "");
    // A target that is not plain is refused before anything runs.
    let p = platform(setup("true").1, vec![]);
    let err = run_issue(p, cve_issue("--config=x", "1"), CancellationToken::new())
        .await
        .unwrap();
    assert!(
        err.reason
            .unwrap_or_default()
            .contains("not a plain package name")
    );
}
