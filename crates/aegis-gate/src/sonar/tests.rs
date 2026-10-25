//! The step against a fake scanner (a shell script that writes what the
//! real one writes) and a fake Sonar server (wiremock).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aegis_lang::{Language, Profile, Profiles};
use serde_json::json;
use tokio::runtime::Runtime;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::command::{CancelToken, Runner};

const TREE: &str = "0123456789abcdef0123456789abcdef01234567";
const PR_KEY: &str = "aegis-0123456789ab";
const TOKEN: &str = "squ_test_token";

const FAKE_SCANNER: &str = r#"
printf '%s\n' "$@" > "$FAKE_RECORD/args"
printf '%s' "$SONAR_TOKEN" > "$FAKE_RECORD/token"
cp .aegis-gate-sonar/project.properties "$FAKE_RECORD/settings"
if [ -n "$FAKE_TASK" ]; then
  mkdir -p .aegis-gate-sonar/work
  printf 'projectKey=p\nceTaskId=%s\n' "$FAKE_TASK" > .aegis-gate-sonar/work/report-task.txt
fi
echo "INFO: analysis done"
exit "${FAKE_EXIT:-0}"
"#;

struct Server {
    rt: Runtime,
    mock: MockServer,
}

impl Server {
    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let mock = rt.block_on(MockServer::start());
        Self { rt, mock }
    }

    fn mount(&self, mock: Mock) {
        self.rt.block_on(mock.mount(&self.mock));
    }

    fn task(&self, status: &str, component: &str, pull_request: Option<&str>) {
        let mut task = json!({"id": "T1", "status": status, "componentKey": component});
        if status == "SUCCESS" {
            task["analysisId"] = json!("A1");
        }
        if let Some(pr) = pull_request {
            task["pullRequest"] = json!(pr);
        }
        self.mount(
            Mock::given(method("GET"))
                .and(path("/api/ce/task"))
                .and(query_param("id", "T1"))
                .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"task": task}))),
        );
    }

    fn gate(&self, body: serde_json::Value) {
        self.mount(
            Mock::given(method("GET"))
                .and(path("/api/qualitygates/project_status"))
                .and(query_param("analysisId", "A1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body)),
        );
    }

    fn issues(&self, body: serde_json::Value) {
        self.mount(
            Mock::given(method("GET"))
                .and(path("/api/issues/search"))
                .and(query_param("componentKeys", "proj"))
                .and(query_param("pullRequest", PR_KEY))
                .respond_with(ResponseTemplate::new(200).set_body_json(body)),
        );
    }
}

/// A repo dir with the scanner script, and where the script records.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    logs: PathBuf,
    record: PathBuf,
    profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-sonar-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, logs, record) = (root.join("repo"), root.join("logs"), root.join("record"));
        for d in [&repo, &logs, &record] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root.join("scanner.sh"), FAKE_SCANNER).unwrap();
        let profile = Profiles::builtin()
            .unwrap()
            .get(Language::Rust)
            .unwrap()
            .clone();
        Self {
            root,
            repo,
            logs,
            record,
            profile,
        }
    }

    fn step(&self, url: &str) -> SonarStep {
        SonarStep {
            url: url.to_owned(),
            token: TOKEN.to_owned(),
            organization: None,
            project_key: "proj".into(),
            pull_request: true,
            scanner: vec![
                "sh".into(),
                self.root.join("scanner.sh").display().to_string(),
            ],
            image: None,
            network: "bridge".into(),
            cache: None,
            properties: BTreeMap::new(),
            wait: Duration::from_secs(5),
            poll: Duration::from_millis(20),
        }
    }

    fn run_with(
        &self,
        step: &SonarStep,
        env: &[(&str, &str)],
        cancel: CancelToken,
    ) -> Result<StepReport, GateError> {
        let mut vars = vec![("FAKE_RECORD".to_owned(), self.record.display().to_string())];
        vars.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let runner = Runner {
            env: vars,
            timeout: Duration::from_secs(30),
            cancel,
            sandbox: None,
        };
        run(
            step,
            &Context {
                repo: &self.repo,
                base: "main",
                tree: TREE,
                profile: &self.profile,
                properties: self.profile.sonar.properties.clone(),
                runner: &runner,
                logs: &self.logs,
            },
        )
    }

    fn run(&self, step: &SonarStep) -> Result<StepReport, GateError> {
        self.run_with(step, &[("FAKE_TASK", "T1")], CancelToken::new())
    }

    fn recorded(&self, name: &str) -> String {
        std::fs::read_to_string(self.record.join(name)).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn passing_gate() -> serde_json::Value {
    json!({"projectStatus": {"status": "OK", "conditions": []}})
}

fn no_issues() -> serde_json::Value {
    json!({"paging": {"pageIndex": 1, "pageSize": 100, "total": 0}, "issues": []})
}

#[test]
fn passes_when_the_quality_gate_is_ok_and_keeps_the_repo_settings_out() {
    let server = Server::start();
    server.task("SUCCESS", "proj", Some(PR_KEY));
    server.gate(passing_gate());
    server.issues(no_issues());
    let fx = Fixture::new();
    // A patch's own settings must not reach the scanner.
    std::fs::write(
        fx.repo.join("sonar-project.properties"),
        "sonar.exclusions=**/*\n",
    )
    .unwrap();
    let step = fx.step(&server.mock.uri());

    let report = fx.run(&step).unwrap();

    assert_eq!(report.status, StepStatus::Passed, "{}", report.detail);
    assert!(
        report.detail.contains("quality gate OK"),
        "{}",
        report.detail
    );
    assert_eq!(fx.recorded("token"), TOKEN);
    let args = fx.recorded("args");
    for expected in [
        "-Dproject.settings=.aegis-gate-sonar/project.properties",
        &format!("-Dsonar.host.url={}", server.mock.uri()),
        "-Dsonar.projectKey=proj",
        &format!("-Dsonar.pullrequest.key={PR_KEY}"),
        "-Dsonar.pullrequest.base=main",
    ] {
        assert!(args.lines().any(|l| l == expected), "{expected} in\n{args}");
    }
    assert!(!args.contains(TOKEN), "the token is not an argument");
    let settings = fx.recorded("settings");
    assert!(
        settings.contains("sonar.rust.lcov.reportPaths=lcov.info"),
        "{settings}"
    );
    assert!(
        settings.contains("sonar.exclusions=.aegis-gate-sonar/**,.sonarqube/**"),
        "{settings}"
    );
    assert!(!settings.contains("sonar.exclusions=**/*"), "{settings}");
    assert!(!fx.repo.join(scanner::DIR).exists(), "cleaned up");
    let log = std::fs::read_to_string(&report.logs[0]).unwrap();
    assert!(!log.contains(TOKEN), "{log}");
}

#[test]
fn fails_with_the_conditions_and_new_issues_for_the_agent() {
    let server = Server::start();
    server.task("SUCCESS", "proj", Some(PR_KEY));
    server.gate(json!({"projectStatus": {"status": "ERROR", "conditions": [
        {"status": "ERROR", "metricKey": "new_coverage", "comparator": "LT",
         "errorThreshold": "80", "actualValue": "42.5"},
        {"status": "OK", "metricKey": "new_duplicated_lines_density", "comparator": "GT",
         "errorThreshold": "3", "actualValue": "0.0"}
    ]}}));
    server.issues(
        json!({"paging": {"pageIndex": 1, "pageSize": 100, "total": 23}, "issues": [
            {"key": "I1", "rule": "rust:S3776", "severity": "CRITICAL",
             "component": "proj:src/parse.rs", "line": 41, "message": "Reduce complexity"},
            {"key": "I2", "rule": "rust:S1135", "component": "proj:src/lib.rs",
             "message": "Complete the task"}
        ]}),
    );
    let fx = Fixture::new();

    let report = fx.run(&fx.step(&server.mock.uri())).unwrap();

    assert_eq!(report.status, StepStatus::Failed);
    let d = &report.detail;
    assert!(d.contains("new_coverage = 42.5 (fails when LT 80)"), "{d}");
    assert!(!d.contains("new_duplicated_lines_density"), "{d}");
    assert!(d.contains("23 new issues:"), "{d}");
    assert!(
        d.contains("src/parse.rs:41 rust:S3776 CRITICAL: Reduce complexity"),
        "{d}"
    );
    assert!(
        d.contains("src/lib.rs rust:S1135: Complete the task"),
        "{d}"
    );
    assert!(d.contains("… and 21 more"), "{d}");
    assert!(
        d.contains(&format!("dashboard?id=proj&pullRequest={PR_KEY}")),
        "{d}"
    );
}

#[test]
fn waits_while_the_server_is_still_analysing() {
    let server = Server::start();
    server.mount(
        Mock::given(method("GET"))
            .and(path("/api/ce/task"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"task": {"id": "T1", "status": "IN_PROGRESS", "componentKey": "proj"}}),
            ))
            .up_to_n_times(2)
            .with_priority(1),
    );
    server.task("SUCCESS", "proj", Some(PR_KEY));
    server.gate(passing_gate());
    server.issues(no_issues());
    let fx = Fixture::new();

    let report = fx.run(&fx.step(&server.mock.uri())).unwrap();

    assert_eq!(report.status, StepStatus::Passed, "{}", report.detail);
}

#[test]
fn an_analysis_that_never_finishes_is_an_error_not_a_failed_patch() {
    let server = Server::start();
    server.task("PENDING", "proj", Some(PR_KEY));
    let fx = Fixture::new();
    let step = SonarStep {
        wait: Duration::from_millis(200),
        ..fx.step(&server.mock.uri())
    };

    let err = fx.run(&step).unwrap_err();

    assert!(
        matches!(&err, GateError::Sonar(m) if m.contains("did not finish")),
        "{err}"
    );
}

#[test]
fn a_cancel_while_waiting_skips_the_step() {
    let server = Server::start();
    server.task("PENDING", "proj", Some(PR_KEY));
    let fx = Fixture::new();
    let cancel = CancelToken::new();
    let later = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        later.cancel();
    });

    let report = fx
        .run_with(&fx.step(&server.mock.uri()), &[("FAKE_TASK", "T1")], cancel)
        .unwrap();
    canceller.join().unwrap();

    assert_eq!(report.status, StepStatus::Skipped(Skip::Cancelled));
}

#[test]
fn a_refused_token_is_an_error() {
    let server = Server::start();
    server.mount(
        Mock::given(method("GET"))
            .and(path("/api/ce/task"))
            .respond_with(ResponseTemplate::new(401)),
    );
    let fx = Fixture::new();

    let err = fx.run(&fx.step(&server.mock.uri())).unwrap_err();

    assert!(
        matches!(&err, GateError::Sonar(m) if m.contains("refused the token")),
        "{err}"
    );
}

#[test]
fn a_task_of_another_project_or_pull_request_is_refused() {
    let server = Server::start();
    server.task("SUCCESS", "other", Some(PR_KEY));
    let fx = Fixture::new();

    let err = fx.run(&fx.step(&server.mock.uri())).unwrap_err();

    assert!(
        matches!(&err, GateError::Sonar(m) if m.contains("not the analysis")),
        "{err}"
    );
}

#[test]
fn a_failing_scanner_fails_the_step_with_its_log() {
    let server = Server::start();
    let fx = Fixture::new();

    let report = fx
        .run_with(
            &fx.step(&server.mock.uri()),
            &[("FAKE_EXIT", "3")],
            CancelToken::new(),
        )
        .unwrap();

    assert_eq!(report.status, StepStatus::Failed);
    assert!(report.detail.contains("exit code 3"), "{}", report.detail);
    assert!(report.detail.contains("analysis done"), "{}", report.detail);
}

#[test]
fn a_report_task_left_by_the_patch_is_not_trusted() {
    let server = Server::start();
    let fx = Fixture::new();
    let forged = fx.repo.join(scanner::DIR).join("work");
    std::fs::create_dir_all(&forged).unwrap();
    std::fs::write(forged.join("report-task.txt"), "ceTaskId=FORGED\n").unwrap();

    // This scanner writes no task of its own.
    let err = fx
        .run_with(&fx.step(&server.mock.uri()), &[], CancelToken::new())
        .unwrap_err();

    assert!(
        matches!(&err, GateError::Sonar(m) if m.contains("no task")),
        "{err}"
    );
}

#[test]
fn cloud_sends_the_organization_and_main_branch_reads_the_new_code_period() {
    let server = Server::start();
    server.mount(
        Mock::given(method("GET"))
            .and(path("/api/ce/task"))
            .and(query_param("organization", "acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"task": {
                "id": "T1", "status": "SUCCESS", "componentKey": "proj", "analysisId": "A1"
            }}))),
    );
    server.mount(
        Mock::given(method("GET"))
            .and(path("/api/qualitygates/project_status"))
            .and(query_param("organization", "acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(passing_gate())),
    );
    server.mount(
        Mock::given(method("GET"))
            .and(path("/api/issues/search"))
            .and(query_param("organization", "acme"))
            .and(query_param("inNewCodePeriod", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(no_issues())),
    );
    let fx = Fixture::new();
    let step = SonarStep {
        organization: Some("acme".into()),
        pull_request: false,
        ..fx.step(&server.mock.uri())
    };

    let report = fx.run(&step).unwrap();

    assert_eq!(report.status, StepStatus::Passed, "{}", report.detail);
    let args = fx.recorded("args");
    assert!(args.contains("-Dsonar.organization=acme"), "{args}");
    assert!(!args.contains("pullrequest"), "{args}");
}

#[test]
fn a_project_without_quality_gate_is_an_error() {
    let server = Server::start();
    server.task("SUCCESS", "proj", Some(PR_KEY));
    server.gate(json!({"projectStatus": {"status": "NONE"}}));
    let fx = Fixture::new();

    let err = fx.run(&fx.step(&server.mock.uri())).unwrap_err();

    assert!(
        matches!(&err, GateError::Sonar(m) if m.contains("no quality gate")),
        "{err}"
    );
}

#[test]
fn the_token_stays_out_of_debug_output() {
    let fx = Fixture::new();
    let text = format!("{:?}", fx.step("https://sonarcloud.io"));
    assert!(!text.contains(TOKEN), "{text}");
}

#[test]
fn reads_the_open_issues_page_by_page_with_their_flows() {
    let server = Server::start();
    let issue = |key: String| {
        json!({"key": key, "rule": "rust:S1", "component": "proj:src/lib.rs", "line": 3,
               "message": "m", "textRange": {"startLine": 3, "endLine": 4},
               "flows": [{"locations": [{"component": "proj:src/a.rs",
                   "textRange": {"startLine": 9, "endLine": 9}, "msg": "source"}]}]})
    };
    for page in 1..=2u32 {
        let count = if page == 1 { api::MAX_ISSUES } else { 30 };
        let issues: Vec<_> = (0..count).map(|i| issue(format!("K{page}-{i}"))).collect();
        server.mount(
            Mock::given(method("GET"))
                .and(path("/api/issues/search"))
                .and(query_param("componentKeys", "proj"))
                .and(query_param("resolved", "false"))
                .and(query_param("p", page.to_string().as_str()))
                .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"paging": {"total": api::MAX_ISSUES + 30}, "issues": issues}),
                )),
        );
    }
    let fx = Fixture::new();
    let step = fx.step(&server.mock.uri());

    let (all, total) = open_issues(&step, 1000).unwrap();
    assert_eq!(
        (all.len(), total),
        (api::MAX_ISSUES + 30, (api::MAX_ISSUES + 30) as u64)
    );
    assert_eq!(all[api::MAX_ISSUES].key, "K2-0");
    assert_eq!(
        all[0].text_range,
        Some(TextRange {
            start_line: 3,
            end_line: 4
        })
    );
    assert_eq!(all[0].flows[0].locations[0].msg.as_deref(), Some("source"));

    let (some, total) = open_issues(&step, 10).unwrap();
    assert_eq!((some.len(), total), (10, (api::MAX_ISSUES + 30) as u64));
}
