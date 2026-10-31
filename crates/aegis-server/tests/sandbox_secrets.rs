//! No secret of the server reaches a sandbox container (#30, design §6–§7).
//! The `docker` client is a script that records what it was given: its
//! command line, which is what the container gets, and its environment.
//! One test in this file: it sets variables of the process.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use aegis_gate::{CancelToken, Hardening, Runner, Sandbox};
use aegis_secrets::{EnvSource, MemorySource, Secrets};
use aegis_server::secrets::DATABASE_URL;
use aegis_server::webhook::{
    COMMENT_TOKEN_ENV, GITHUB_SECRET_ENV, GITLAB_COMMENT_TOKEN_ENV, GITLAB_TOKEN_ENV,
};

// Only `docker run` is the container; `inspect` and `rm` follow it.
const FAKE_DOCKER: &str = r#"#!/bin/sh
[ "$1" = run ] || exit 0
printf '%s\n' "$@" > "$(dirname "$0")/argv"
env > "$(dirname "$0")/env"
"#;

/// Distinct values, each found in a dump if it leaked.
const STORED: &[(&str, &str)] = &[
    (DATABASE_URL, "postgres://u:leak-db-1@h/db"),
    ("ANTHROPIC_API_KEY", "leak-model-2"),
    ("GITHUB_TOKEN", "leak-fork-3"),
    ("SONAR_TOKEN", "leak-sonar-4"),
    (COMMENT_TOKEN_ENV, "leak-comment-5"),
    (GITLAB_COMMENT_TOKEN_ENV, "leak-comment-8"),
    ("AEGIS_JIRA_TOKEN", "leak-tracker-9"),
    ("AEGIS_BACKLOG_API_KEY", "leak-tracker-10"),
];
const IN_ENV: &[(&str, &str)] = &[
    (GITHUB_SECRET_ENV, "leak-hook-6"),
    (GITLAB_TOKEN_ENV, "leak-hook-7"),
];

#[tokio::test]
async fn a_sandbox_container_gets_no_secret_from_either_source() {
    // Secrets Manager's stand-in holds some; the dev source, the process
    // environment, the others: the worst case of a dev machine.
    for (name, value) in IN_ENV {
        // SAFETY: the only test of this binary; nothing else reads the
        // environment while it is set.
        unsafe { std::env::set_var(name, value) };
    }
    let stored = Secrets::new(MemorySource::new(STORED.iter().copied()));
    let names: Vec<&str> = STORED.iter().map(|(n, _)| *n).collect();
    stored.require(&names).await.unwrap();
    let from_env = Secrets::new(EnvSource::for_dev_and_tests());
    let names: Vec<&str> = IN_ENV.iter().map(|(n, _)| *n).collect();
    from_env.require(&names).await.unwrap();

    let dir = tempdir();
    let docker = dir.join("docker");
    std::fs::write(&docker, FAKE_DOCKER).unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let runner = Runner {
        env: vec![("GATE_STEP".into(), "test".into())],
        timeout: Duration::from_secs(30),
        cancel: CancelToken::new(),
        sandbox: Some(Sandbox {
            docker: docker.display().to_string(),
            image: "aegis-sandbox-rust".into(),
            mounts: vec![],
            env: vec![("CARGO_TERM_COLOR".into(), "never".into())],
            network: None,
            read_only_mounts: true,
            hardening: Hardening::default(),
        }),
    };
    let ran = runner
        .run(
            &["cargo".into(), "test".into()],
            &work,
            &dir.join("test.log"),
            &[("TMPDIR", "/tmp")],
        )
        .unwrap();
    assert!(ran.exit.success(), "{ran:?}");

    let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
    let env = std::fs::read_to_string(dir.join("env")).unwrap();
    for (name, value) in STORED.iter().chain(IN_ENV) {
        assert!(!argv.contains(value), "{name} on the command line:\n{argv}");
        assert!(!env.contains(value), "{name} in the client's env:\n{env}");
        assert!(!argv.contains(name), "{name} passed by name:\n{argv}");
    }
    // The container's environment is the profile's and the step's only.
    let lines: Vec<&str> = argv.lines().collect();
    let passed: Vec<&str> = lines
        .windows(2)
        .filter(|w| w[0] == "--env")
        .map(|w| w[1])
        .collect();
    assert_eq!(
        passed,
        ["CARGO_TERM_COLOR=never", "GATE_STEP=test"],
        "{argv}"
    );
    assert!(argv.contains("--network\nnone"), "{argv}");
    // The client itself gets only what it needs to find the daemon.
    for line in env.lines() {
        let key = line.split('=').next().unwrap_or_default();
        assert!(
            aegis_gate::CLIENT_ENV.contains(&key) || ["PWD", "SHLVL", "_", "OLDPWD"].contains(&key),
            "the client got {key}"
        );
    }
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aegis-sandbox-secrets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
