//! aegis-pr end to end, without the network: a real gate run on a git
//! repo, pushed to a local bare "fork"; GitHub and the Light model faked.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aegis_gate::{CancelToken, GateReport, GateRequest, run_gate};
use aegis_lang::{Language, ProfileOverrides, Profiles};
use aegis_pr::{
    AllowedTestChange, ChangeNote, CommitInput, Cost, GitHub, GitLab, Identity, PrContent, PrError,
    Remote, abandon, push_gated, render, write_commit_message,
};
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-pr-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repo with one base commit, a patch in the working tree, and a bare
/// "fork" to push to. Returns (repo, fork, report).
fn gated(scratch: &Scratch, test_command: &str) -> (PathBuf, PathBuf, GateReport) {
    let repo = scratch.0.join("repo");
    let fork = scratch.0.join("fork.git");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 {\n    2\n}\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
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
            "base",
        ],
    );
    git(&scratch.0, &["init", "-q", "--bare", "fork.git"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();

    let overrides: ProfileOverrides = toml::from_str(&format!(
        "[gate]\nformat = false\nlint = false\nrepro_test = false\ncoverage = false\ntest = {test_command}\n"
    ))
    .unwrap();
    let profile = Profiles::builtin()
        .unwrap()
        .get(Language::Rust)
        .unwrap()
        .resolve(&overrides)
        .unwrap();
    let allowed = BTreeSet::new();
    let work = scratch.0.join("work");
    let report = run_gate(&GateRequest {
        repo: &repo,
        base: "main",
        profile: &profile,
        allowed: &allowed,
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(60),
        cancel: CancelToken::new(),
        sandbox: None,
        fetch: None,
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
    })
    .unwrap();
    (repo, fork, report)
}

fn bot() -> Identity {
    Identity {
        name: "aegis-bot".into(),
        email: "aegis-bot@example.com".into(),
    }
}

#[test]
fn the_pushed_commit_is_exactly_the_gated_tree_on_the_base() {
    let scratch = Scratch::new();
    let (repo, fork, report) = gated(&scratch, "[\"true\"]");
    assert!(report.passed(), "{report}");
    // A later change in the working tree is not part of the PR.
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn one() -> u32 {\n    99\n}\n",
    )
    .unwrap();
    let remote = Remote::local(&fork).unwrap();

    let pushed = push_gated(
        &repo,
        "42",
        &report,
        "fix: return one\n\nIssue: x\n",
        &bot(),
        &remote,
    )
    .unwrap();
    assert_eq!(pushed.branch.name(), "agent/42");
    assert_eq!(git(&fork, &["rev-parse", "agent/42"]), pushed.commit);
    assert_eq!(git(&fork, &["rev-parse", "agent/42^{tree}"]), report.tree);
    assert_eq!(git(&fork, &["rev-parse", "agent/42^"]), report.base);
    assert_eq!(
        git(&fork, &["show", "agent/42:src/lib.rs"]),
        "pub fn one() -> u32 {\n    1\n}"
    );
    assert_eq!(
        git(&fork, &["log", "-1", "--format=%an <%ae>%n%s", "agent/42"]),
        "aegis-bot <aegis-bot@example.com>\nfix: return one"
    );
    // Only the agent branch exists in the fork.
    assert_eq!(
        git(&fork, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/agent/42"
    );

    abandon(&repo, &pushed, &remote).unwrap();
    assert_eq!(git(&fork, &["for-each-ref", "--format=%(refname)"]), "");
}

#[test]
fn nothing_is_pushed_when_the_gate_failed_or_the_id_is_bad() {
    let scratch = Scratch::new();
    let (repo, fork, report) = gated(&scratch, "[\"false\"]");
    assert!(!report.passed());
    let remote = Remote::local(&fork).unwrap();
    assert!(matches!(
        push_gated(&repo, "42", &report, "fix: x", &bot(), &remote),
        Err(PrError::GateNotPassed)
    ));
    let scratch = Scratch::new();
    let (repo, fork, report) = gated(&scratch, "[\"true\"]");
    let remote = Remote::local(&fork).unwrap();
    assert!(matches!(
        push_gated(&repo, "../main", &report, "fix: x", &bot(), &remote),
        Err(PrError::InvalidIssueId(_))
    ));
    assert_eq!(git(&fork, &["for-each-ref", "--format=%(refname)"]), "");
}

#[test]
fn the_body_follows_the_template_and_model_text_is_inert() {
    let scratch = Scratch::new();
    let (_, _, report) = gated(&scratch, "[\"true\"]");
    let body = render(&PrContent {
        issue_url: "https://github.com/me/typst/issues/7".into(),
        cause: "The check used `>` instead of `>=` (reported by @alice, closes #3).".into(),
        changes: vec![ChangeNote {
            location: "src/lib.rs:2".into(),
            note: "return 1".into(),
        }],
        repro_test: Some("tests::one_is_one".into()),
        allowed_test_changes: vec![AllowedTestChange {
            test: "golden::render".into(),
            reason: "output now ends with a newline".into(),
        }],
        allowed_reference_changes: vec![],
        gate: &report,
        coverage_percent: Some(82.04),
        cost: Cost {
            models: vec!["claude-sonnet-5-5".into(), "claude-haiku-5-5".into()],
            model_calls: 7,
            usd: 0.4321,
        },
    });
    assert_eq!(
        body,
        "## Issue\nhttps://github.com/me/typst/issues/7\n\n\
         ## Nguyên nhân\nThe check used '>' instead of '>=' (reported by @\u{200b}alice, closes #\u{200b}3).\n\n\
         ## Thay đổi (theo từng đoạn)\n- `src/lib.rs:2` — return 1\n\n\
         ## Test\n- Test tái hiện: `tests::one_is_one` (FAIL → PASS)\n\
         - Test được phép đổi:\n  - `golden::render` — output now ends with a newline\n\n\
         ## Kết quả gate\nformat – · lint – · repro_test – · test ✔ · coverage – · sonar – · integrity ✔\n\n\
         ## Chi phí\nclaude-sonnet-5-5, claude-haiku-5-5 · 7 lượt · $0.43\n"
    );
}

#[tokio::test]
async fn opens_the_pr_in_the_fork_and_reports_api_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/me/typst/pulls"))
        .and(header("authorization", "Bearer secret"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(body_partial_json(json!({
            "head": "agent/7", "base": "main", "draft": true, "maintainer_can_modify": false
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 12, "html_url": "https://github.com/me/typst/pull/12"
        })))
        .mount(&server)
        .await;
    let github = GitHub::new(&server.uri(), "me", "typst", "secret").unwrap();
    let branch = aegis_pr::AgentBranch::for_issue("7").unwrap();
    let pr = github
        .open_pull_request(&branch, "main", "fix: x", "body", true)
        .await
        .unwrap();
    assert_eq!(
        (pr.number, pr.url.as_str()),
        (12, "https://github.com/me/typst/pull/12")
    );

    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({"message": "Validation Failed"})),
        )
        .mount(&failing)
        .await;
    let github = GitHub::new(&failing.uri(), "me", "typst", "secret").unwrap();
    let err = github
        .open_pull_request(&branch, "main", "fix: x", "body", false)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "GitHub API 422: Validation Failed");
    assert!(GitHub::new(&server.uri(), "me/../other", "typst", "t").is_err());
}

struct Light(Result<&'static str, ()>);

impl ModelProvider for Light {
    async fn send(&self, _: &Request) -> Result<Response, ProviderError> {
        match self.0 {
            Ok(text) => Ok(Response {
                id: "m".into(),
                model: "claude-haiku-5-5".into(),
                content: vec![ContentBlock::text(text)],
                stop_reason: Some(StopReason::EndTurn),
                usage: Usage::default(),
            }),
            Err(()) => Err(ProviderError::Overloaded),
        }
    }
}

#[tokio::test]
async fn commit_messages_from_the_light_model_are_checked() {
    let changes = [ChangeNote {
        location: "src/lib.rs:2".into(),
        note: "return 1".into(),
    }];
    let input = CommitInput {
        issue_title: "one() returns 2.",
        issue_url: "https://github.com/me/typst/issues/7",
        cause: "typo",
        changes: &changes,
    };
    let good = write_commit_message(
        &Light(Ok("fix: return 1 from one()\n\nIt returned 2.")),
        "claude-haiku-5-5",
        None,
        &input,
    )
    .await;
    assert!(!good.fallback && good.response.is_some());
    assert_eq!(
        good.text,
        "fix: return 1 from one()\n\nIt returned 2.\n\nIssue: https://github.com/me/typst/issues/7\n"
    );

    let rambling = write_commit_message(
        &Light(Ok("Sure! Here is a message:\nfix: x")),
        "m",
        None,
        &input,
    )
    .await;
    assert!(rambling.fallback && rambling.response.is_some());
    assert_eq!(
        rambling.text,
        "fix: one() returns 2\n\nIssue: https://github.com/me/typst/issues/7\n"
    );

    let down = write_commit_message(&Light(Err(())), "m", None, &input).await;
    assert!(down.fallback && down.response.is_none());
}

#[test]
fn the_repos_own_git_config_cannot_redirect_the_push() {
    let scratch = Scratch::new();
    let (repo, fork, report) = gated(&scratch, "[\"true\"]");
    let evil = scratch.0.join("evil.git");
    git(&scratch.0, &["init", "-q", "--bare", "evil.git"]);
    let fork_url = format!("file://{}", fork.canonicalize().unwrap().display());
    let evil_url = format!("file://{}", evil.display());
    git(
        &repo,
        &["config", &format!("url.{evil_url}.insteadOf"), &fork_url],
    );
    git(&repo, &["config", "core.hooksPath", "/nonexistent"]);

    let remote = Remote::local(&fork).unwrap();
    push_gated(&repo, "9", &report, "fix: x", &bot(), &remote).unwrap();
    assert_eq!(
        git(&fork, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/agent/9"
    );
    assert_eq!(git(&evil, &["for-each-ref", "--format=%(refname)"]), "");
}

#[test]
fn the_push_goes_to_the_repo_the_pr_is_opened_in() {
    let github = GitHub::new("https://api.github.com", "me", "typst", "t").unwrap();
    assert_eq!(
        github.remote().unwrap().url(),
        "https://github.com/me/typst.git"
    );
    let enterprise = GitHub::new("https://git.corp.example/api/v3", "me", "typst", "t").unwrap();
    assert_eq!(
        enterprise.remote().unwrap().url(),
        "https://git.corp.example/me/typst.git"
    );
    assert!(GitHub::new("http://api.github.com", "me", "typst", "t").is_err());
}

#[tokio::test]
async fn review_edits_count_the_lines_after_the_agents_commit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/me/typst/pulls/12/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"sha": "aaa"}, {"sha": "bbb"}, {"sha": "ccc"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/me/typst/compare/aaa...ccc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [{"additions": 3, "deletions": 1}, {"additions": 0, "deletions": 2}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/me/typst/pulls/13/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"sha": "aaa"}])))
        .mount(&server)
        .await;
    let github = GitHub::new(&server.uri(), "me", "typst", "t").unwrap();
    assert_eq!(github.review_edits(12).await.unwrap(), 6);
    assert_eq!(
        github.review_edits(13).await.unwrap(),
        0,
        "only the agent's commit"
    );
}

#[tokio::test]
async fn comments_on_an_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/typst/typst/issues/7/comments"))
        .and(header("authorization", "Bearer t"))
        .and(body_partial_json(json!({ "body": "done" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 99, "html_url": "https://github.com/typst/typst/issues/7#issuecomment-99"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let github = GitHub::new(&server.uri(), "typst", "typst", "t").unwrap();
    let comment = github.comment_on_issue(7, "done").await.unwrap();
    assert_eq!(comment.id, 99);

    let err = github.comment_on_issue(8, "done").await.unwrap_err();
    assert!(matches!(err, PrError::GitHub { status: 404, .. }), "{err}");
}

#[tokio::test]
async fn comments_on_a_gitlab_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/group%2Fsub%2Fapp/issues/4/notes"))
        .and(header("private-token", "t"))
        .and(body_partial_json(json!({ "body": "done" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 31, "body": "done" })))
        .expect(1)
        .mount(&server)
        .await;
    let api = format!("{}/api/v4/", server.uri());
    let gitlab = GitLab::new(&api, "group/sub/app", "t").unwrap();
    assert_eq!(gitlab.comment_on_issue(4, "done").await.unwrap().id, 31);

    let err = gitlab.comment_on_issue(5, "done").await.unwrap_err();
    assert!(matches!(err, PrError::GitLab { status: 404, .. }), "{err}");

    for bad in ["app", "group/../app", "group//app", "group/a b", "/app"] {
        assert!(GitLab::new(&api, bad, "t").is_err(), "{bad}");
    }
    assert!(GitLab::new("http://gitlab.example/api/v4", "group/app", "t").is_err());
}
