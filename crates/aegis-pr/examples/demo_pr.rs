//! Opens a real draft PR from the patch in a Python repo's working tree
//! (the "Xong khi" check of #15), with a short gate (one test file).
//!
//! `GH_TOKEN=… cargo run -p aegis-pr --example demo_pr -- <repo> <owner> <name> <base-branch> <python> <test-file>`

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use aegis_gate::{CancelToken, GateRequest, run_gate};
use aegis_lang::{Language, ProfileOverrides, Profiles};
use aegis_pr::{ChangeNote, Cost, GitHub, Identity, PrContent, checked, push_gated, render};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, owner, name, base, python, test_file] = args.as_slice() else {
        return Err(
            "usage: demo_pr <repo> <owner> <name> <base-branch> <python> <test-file>".into(),
        );
    };
    let token = std::env::var("GH_TOKEN")?;
    let repo = Path::new(repo);

    let overrides: ProfileOverrides = toml::from_str(&format!(
        "[gate]\nformat = false\nlint = false\nrepro_test = false\ncoverage = false\n\
         test = [{python:?}, \"-m\", \"pytest\", \"-q\", \"-p\", \"no:cacheprovider\", {test_file:?}]\n\
         test_output = \"pytest\"\n"
    ))?;
    let profile = Profiles::builtin()?
        .get(Language::Python)?
        .resolve(&overrides)?;
    let work = std::env::temp_dir().join("aegis-demo-pr-work");
    let report = run_gate(&GateRequest {
        repo,
        base: base.as_str(),
        profile: &profile,
        allowed: &BTreeSet::new(),
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(300),
        cancel: CancelToken::new(),
        sandbox: None,
        fetch: None,
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: Default::default(),
    })?;
    println!("{report}");
    if !report.passed() {
        return Err("gate failed".into());
    }

    let message = checked("docs: explain why Request copies its headers").expect("valid message")
        + "\n\nIssue: demo run of aegis-code #15\n";
    let identity = Identity {
        name: "aegis-bot".into(),
        email: "aegis-bot@example.invalid".into(),
    };
    let github = GitHub::new("https://api.github.com", owner, name, &token)?;
    let remote = github.remote()?;
    let pushed = push_gated(repo, "demo-15", &report, &message, &identity, &remote)?;
    println!("pushed {} at {}", pushed.branch, pushed.commit);

    let body = render(&PrContent {
        issue_url: "https://github.com/hoanghainh1188/aegis-code/issues/15 (demo, no real issue)"
            .into(),
        cause: "Demo of the aegis-pr template; the patch only adds a comment.".into(),
        changes: vec![ChangeNote {
            location: "scrapy/http/request/__init__.py".into(),
            note: "comment on why headers are copied".into(),
        }],
        repro_test: None,
        allowed_test_changes: vec![],
        allowed_reference_changes: vec![],
        gate: &report,
        coverage_percent: None,
        cost: Cost {
            models: vec!["(none: demo)".into()],
            model_calls: 0,
            usd: 0.0,
        },
    });
    let pr = github
        .open_pull_request(
            &pushed.branch,
            base,
            "[demo] aegis-pr template check (#15)",
            &body,
            true,
        )
        .await?;
    println!("opened {}", pr.url);
    Ok(())
}
