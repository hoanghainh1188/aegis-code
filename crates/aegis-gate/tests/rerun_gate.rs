//! #306: a test that fails on the patch and not on the base is run again
//! on its own, through the profile's `repro_test` with the runner's own
//! selector, up to `reruns` times; it counts as broken only when it fails
//! every time. Fake commands stand in for the runner: in the patched repo
//! (which has `.git`, the base copy has not) the whole suite fails
//! `load_sensitive`, as a test does that times out under the suite's load.

mod common;

use aegis_gate::{StepName, StepStatus};
use aegis_lang::{Language, ResolvedProfile};
use common::{Repo, profile};

/// The whole suite: `load_sensitive` fails in the patched repo, passes on
/// the base copy.
const SUITE: &str = "echo 'Running unittests src/lib.rs (target/debug/deps/fx-1)'; \
     if [ -e .git ]; then echo 'test load_sensitive ... FAILED'; \
     echo 'test result: FAILED. 0 passed; 1 failed'; exit 1; fi; \
     echo 'test load_sensitive ... ok'; echo 'test result: ok. 1 passed; 0 failed'";

/// `repro_test` with `{test}` as `$1`: `run` decides how the selected test
/// does, `$TMPDIR/runs` counts the runs.
fn gate_profile(run: &str, extra: &str) -> ResolvedProfile {
    let repro = format!(
        "echo x >> \"$TMPDIR/runs\"; echo 'Running unittests src/lib.rs (target/debug/deps/fx-1)'; {run}"
    );
    profile(
        Language::Rust,
        &format!(
            r#"
[gate]
format = ["true"]
lint = ["true"]
coverage = false
test_output = "libtest"
test = ["sh", "-c", '''{SUITE}''']
repro_test = ["sh", "-c", '''{repro}''', "repro", "{{test}}"]
{extra}
"#
        ),
    )
}

const PASSES: &str = "echo \"test $1 ... ok\"; echo 'test result: ok. 1 passed; 0 failed'";
const FAILS: &str =
    "echo \"test $1 ... FAILED\"; echo 'test result: FAILED. 0 passed; 1 failed'; exit 1";

fn test_step(profile: &ResolvedProfile) -> aegis_gate::StepReport {
    let repo = Repo::new("rust", &[]);
    let report = repo.gate(profile, None);
    report.step(StepName::Test).unwrap().clone()
}

fn logs(step: &aegis_gate::StepReport) -> Vec<String> {
    step.logs
        .iter()
        .map(|l| l.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_test_failing_only_in_the_whole_suite_is_flaky() {
    let step = test_step(&gate_profile(PASSES, ""));
    assert_eq!(step.status, StepStatus::Passed, "{}", step.detail);
    assert!(
        step.detail.contains("flaky") && step.detail.contains("fx::load_sensitive"),
        "{}",
        step.detail
    );
    // It passed on its own at once: no further run.
    assert_eq!(
        logs(&step),
        ["test.log", "test-base.log", "test-rerun-1.log"]
    );
}

#[test]
fn a_test_failing_every_time_on_its_own_is_broken() {
    let step = test_step(&gate_profile(FAILS, ""));
    assert_eq!(step.status, StepStatus::Failed, "{}", step.detail);
    assert!(
        step.detail
            .starts_with("1 tests fail that pass on the base revision:\n  fx::load_sensitive"),
        "{}",
        step.detail
    );
    assert_eq!(
        logs(&step),
        [
            "test.log",
            "test-base.log",
            "test-rerun-1.log",
            "test-rerun-2.log",
            "test-rerun-3.log"
        ]
    );
}

#[test]
fn the_repo_sets_how_many_times() {
    let step = test_step(&gate_profile(FAILS, "reruns = 1"));
    assert_eq!(step.status, StepStatus::Failed, "{}", step.detail);
    assert_eq!(
        logs(&step),
        ["test.log", "test-base.log", "test-rerun-1.log"]
    );
}

#[test]
fn a_test_that_passes_on_a_later_run_is_flaky() {
    // Fails the first time it runs on its own, passes the second.
    let once = format!("if [ \"$(wc -l < \"$TMPDIR/runs\")\" -lt 2 ]; then {FAILS}; fi; {PASSES}");
    let step = test_step(&gate_profile(&once, ""));
    assert_eq!(step.status, StepStatus::Passed, "{}", step.detail);
    assert!(step.detail.contains("flaky"), "{}", step.detail);
    assert_eq!(
        logs(&step),
        [
            "test.log",
            "test-base.log",
            "test-rerun-1.log",
            "test-rerun-2.log"
        ]
    );
}

#[test]
fn a_selection_that_runs_nothing_falls_back_to_the_whole_suite() {
    let nothing = "echo 'running 0 tests'; echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out'";
    let step = test_step(&gate_profile(nothing, ""));
    // The whole suite fails it again: broken, as before #306.
    assert_eq!(step.status, StepStatus::Failed, "{}", step.detail);
    assert_eq!(
        logs(&step),
        [
            "test.log",
            "test-base.log",
            "test-rerun-1.log",
            "test-rerun.log"
        ]
    );
}
