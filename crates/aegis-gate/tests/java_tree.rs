//! "Xong khi" of #80: a patch that bumps a dependency the base already has
//! and so pulls in an unknown transitive artifact is blocked, from the
//! trees Maven resolves on the base and on the patch, in the fetch
//! container behind the egress proxy. Needs Docker, the network, the
//! `maven` image and the Python sandbox image (the proxy); skipped
//! otherwise, unless `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use aegis_gate::{
    CancelToken, Egress, FetchStep, GateReport, GateRequest, Hardening, Sandbox, StepName,
    StepStatus, run_gate,
};
use aegis_lang::Language;
use common::{Repo, profile};

const MAVEN_IMAGE: &str = "maven:3.9.9-eclipse-temurin-17";
const PROXY_IMAGE: &str = "aegis-sandbox-python";

fn docker_ready() -> bool {
    let ok = [MAVEN_IMAGE, PROXY_IMAGE].iter().all(|image| {
        Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or an image is missing"
        );
        eprintln!("skipping: docker, {MAVEN_IMAGE} or {PROXY_IMAGE} is missing");
    }
    ok
}

/// The profile's tree command, with the proxy handed to Maven: Maven reads
/// proxies from `settings.xml` only, not from `HTTPS_PROXY`.
const TREE: &str = r#"
# The image has no osv-scanner; the audit is tested in rust_gate (#159).
audit = ["true", "{lockfile}"]

[tree]
file = "maven-dependency-tree.txt"
on_change = ["pom.xml"]
command = ["sh", "-c", """
p=${HTTPS_PROXY#http://}
printf '<settings><proxies><proxy><protocol>https</protocol><host>%s</host><port>%s</port></proxy></proxies></settings>' \
  "${p%:*}" "${p##*:}" > /tmp/settings.xml
exec mvn -B -q -s /tmp/settings.xml -Dmaven.repo.local=/m2 \
  org.apache.maven.plugins:maven-dependency-plugin:3.8.1:tree \
  -DoutputFile={output} -DappendOutput=true
"""]
"#;

const OKHTTP_3: &str = "<dependency><groupId>com.squareup.okhttp3</groupId><artifactId>okhttp</artifactId><version>3.14.9</version></dependency>\n  </dependencies>";

fn has_jar(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().any(|e| {
        let path = e.unwrap().path();
        if path.is_dir() {
            has_jar(&path)
        } else {
            path.extension().is_some_and(|x| x == "jar")
        }
    })
}

fn gate(repo: &Repo, allowed: &[&str]) -> GateReport {
    let m2 = repo.work().join("m2");
    std::fs::create_dir_all(&m2).unwrap();
    let allowed: BTreeSet<String> = allowed.iter().map(|a| (*a).to_owned()).collect();
    let (dir, work) = (repo.dir(), repo.work().join("gate"));
    run_gate(&GateRequest {
        repo: &dir,
        base: "main",
        profile: &profile(Language::Java, TREE),
        allowed: &allowed,
        repro: None,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(600),
        cancel: CancelToken::new(),
        sandbox: Some(Sandbox {
            docker: "docker".into(),
            image: MAVEN_IMAGE.into(),
            mounts: vec![(m2, "/m2".into())],
            env: vec![("HOME".into(), "/tmp".into())],
            network: None,
            read_only_mounts: true,
            // Not our image: HOME is in /tmp, the rest read-only.
            hardening: Hardening::default(),
        }),
        fetch: Some(FetchStep {
            egress: Egress {
                docker: "docker".into(),
                proxy_image: PROXY_IMAGE.into(),
                allow: vec!["repo.maven.apache.org".into()],
            },
            command: vec!["true".into()],
            audit_hosts: vec![],
        }),
        build_cache: None,
        allowed_tests: vec![],
        reference_groups: vec![],
        sonar: None,
        audit: Default::default(),
        parts: vec![],
    })
    .unwrap()
}

#[test]
fn a_bump_pulling_in_an_unknown_transitive_artifact_is_blocked() {
    if !docker_ready() {
        return;
    }
    let repo = Repo::new("java", &[("pom.xml", "  </dependencies>", OKHTTP_3)]);
    // okhttp 4 is written in Kotlin and brings the Kotlin standard library.
    repo.replace(
        "pom.xml",
        "<version>3.14.9</version>",
        "<version>4.12.0</version>",
    );

    let report = gate(&repo, &[]);
    let step = report
        .steps
        .iter()
        .find(|s| s.name == StepName::Dependencies)
        .unwrap();
    assert_eq!(step.status, StepStatus::Failed, "{}", step.detail);
    assert!(
        step.detail
            .contains("dependency tree: new dependency `org.jetbrains.kotlin:kotlin-stdlib@"),
        "{}",
        step.detail
    );
    assert!(!step.detail.contains("okhttp"), "the bump itself is fine");
    // Resolving the tree fetched poms only: the blocked code never arrived.
    let kotlin_dir = repo.work().join("m2/org/jetbrains/kotlin/kotlin-stdlib");
    assert!(kotlin_dir.is_dir(), "the tree was resolved");
    assert!(!has_jar(&kotlin_dir), "no jar of the blocked dependency");

    // Listed, the same artifacts pass and the fetch runs.
    let kotlin = [
        "org.jetbrains.kotlin:kotlin-stdlib",
        "org.jetbrains.kotlin:kotlin-stdlib-common",
        "org.jetbrains.kotlin:kotlin-stdlib-jdk7",
        "org.jetbrains.kotlin:kotlin-stdlib-jdk8",
        "org.jetbrains:annotations",
        "com.squareup.okio:okio-jvm",
    ];
    let report = gate(&repo, &kotlin);
    let step = report
        .steps
        .iter()
        .find(|s| s.name == StepName::Dependencies)
        .unwrap();
    assert_eq!(step.status, StepStatus::Passed, "{}", step.detail);
    assert_eq!(
        step.logs.len(),
        4,
        "tree on the base, on the patch, fetch, audit"
    );
}
