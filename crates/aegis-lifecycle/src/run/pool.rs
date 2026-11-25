//! The execution pool (#665): each job gets an execution pod of its own
//! from the deployment's `provision` command and gives it back with its
//! `release` command when the job ends. A pod is a remote execution host
//! (#664) for one job: the job's containers run on its Docker, through its
//! `aegis-docker-proxy --listen-tls` with the job's own certificate.
//!
//! The platform knows nothing of what is behind the commands (Kubernetes,
//! Firecracker, a fixed set of hosts, a warm pool): they run on the worker
//! and say, in JSON, how to reach the pod. A job that goes on from its
//! checkpoint (#662) after its worker failed asks for the pod it had
//! (`AEGIS_POD`), which the command hands back while it still runs.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;

use crate::config::{ExecutionPool, RemoteExecution};

/// What a hook may print: more is not a pod's description.
const MAX_OUTPUT: usize = 64 * 1024;

/// The job a pod is for, as the pool's commands are told.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PodJob<'a> {
    pub id: &'a str,
    pub dir: &'a Path,
    pub state_dir: &'a Path,
}

/// A pod the pool handed a job: its name and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pod {
    pub name: String,
    pub remote: RemoteExecution,
}

/// `provision`'s answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Granted {
    pod: String,
    docker_host: String,
    tls_cert: PathBuf,
    tls_key: PathBuf,
    tls_ca: PathBuf,
}

/// A pod's name, as `release` gets it back: printable, one line.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(format!(
            "the pod's name `{}` is not one line of text",
            name.escape_debug()
        ));
    }
    Ok(())
}

/// The last lines of a command's error output, for the job's reason.
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let start = text.len().saturating_sub(600);
    let start = (start..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

/// Runs one of the pool's commands for `job`, `pod` as `AEGIS_POD`; its
/// stdout when it exits 0 within the pool's timeout.
async fn hook(
    pool: &ExecutionPool,
    name: &str,
    argv: &[String],
    job: PodJob<'_>,
    pod: Option<&str>,
) -> Result<Vec<u8>, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| format!("run.execution_pool.{name} names no command"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("AEGIS_JOB_ID", job.id)
        .env("AEGIS_JOB_DIR", job.dir)
        .env("AEGIS_STATE_DIR", job.state_dir)
        .env_remove("AEGIS_POD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(pod) = pod {
        command.env("AEGIS_POD", pod);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("the pool's {name} command `{program}` did not start: {e}"))?;
    let timeout = Duration::from_secs(pool.timeout_s);
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "the pool's {name} command took more than {}s",
                pool.timeout_s
            )
        })?
        .map_err(|e| format!("the pool's {name} command: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the pool's {name} command failed ({}): {}",
            out.status,
            tail(&out.stderr)
        ));
    }
    if out.stdout.len() > MAX_OUTPUT {
        return Err(format!(
            "the pool's {name} command printed more than {MAX_OUTPUT} bytes"
        ));
    }
    Ok(out.stdout)
}

/// A pod for `job` from the pool; `previous`, the one it had before its
/// worker failed, if any.
pub(crate) async fn provision(
    pool: &ExecutionPool,
    job: PodJob<'_>,
    previous: Option<&str>,
) -> Result<Pod, String> {
    let out = hook(pool, "provision", &pool.provision, job, previous).await?;
    let granted: Granted = serde_json::from_slice(&out)
        .map_err(|e| format!("the pool's provision command did not print a pod: {e}"))?;
    let pod = Pod {
        name: granted.pod,
        remote: RemoteExecution {
            docker_host: granted.docker_host,
            tls_cert: granted.tls_cert,
            tls_key: granted.tls_key,
            tls_ca: granted.tls_ca,
        },
    };
    let checked = check_name(&pod.name).and_then(|()| pod.remote.check());
    if let Err(why) = checked {
        // It may be running already: the pool gets it back.
        if check_name(&pod.name).is_ok() {
            release(pool, job, &pod.name).await;
        }
        return Err(format!("the pool's provision command: {why}"));
    }
    Ok(pod)
}

/// Gives `pod` back to the pool. A pod that cannot be given back is said
/// in the log: the pool reaps it.
pub(crate) async fn release(pool: &ExecutionPool, job: PodJob<'_>, pod: &str) {
    match hook(pool, "release", &pool.release, job, Some(pod)).await {
        Ok(_) => tracing::info!(job = job.id, pod, "the execution pod is released"),
        Err(e) => tracing::warn!(job = job.id, pod, error = %e, "releasing the execution pod"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aegis-pool-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// A pool whose `provision` prints `body`'s pod and whose `release`
    /// writes down what it was told.
    fn pool(dir: &Path, provision: &str) -> ExecutionPool {
        let release = script(
            dir,
            "release",
            "echo \"$AEGIS_JOB_ID $AEGIS_POD\" >> \"$AEGIS_STATE_DIR/released\"",
        );
        ExecutionPool {
            provision: vec![
                script(dir, "provision", provision),
                "--zone".into(),
                "a".into(),
            ],
            release: vec![release],
            timeout_s: 5,
        }
    }

    fn released(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("released")).unwrap_or_default()
    }

    const GRANT: &str = r#"echo "$@ $AEGIS_POD" > "$AEGIS_JOB_DIR/asked"
cat <<EOF
{"pod": "pod-$AEGIS_JOB_ID", "docker_host": "tcp://10.0.0.7:2376",
 "tls_cert": "$AEGIS_JOB_DIR/job.crt", "tls_key": "$AEGIS_JOB_DIR/job.key",
 "tls_ca": "/etc/aegis/pods-ca.crt"}
EOF"#;

    #[tokio::test]
    async fn a_job_gets_a_pod_of_its_own_and_gives_it_back() {
        let dir = scratch("grant");
        let job_dir = dir.join("jobs/j-1");
        std::fs::create_dir_all(&job_dir).unwrap();
        let pool = pool(&dir, GRANT);
        let job = PodJob {
            id: "j-1",
            dir: &job_dir,
            state_dir: &dir,
        };
        let pod = provision(&pool, job, None).await.unwrap();
        assert_eq!(pod.name, "pod-j-1");
        assert_eq!(pod.remote.docker_host, "tcp://10.0.0.7:2376");
        assert_eq!(pod.remote.tls_cert, job_dir.join("job.crt"));
        assert_eq!(
            std::fs::read_to_string(job_dir.join("asked"))
                .unwrap()
                .trim(),
            "--zone a",
            "its arguments, and no AEGIS_POD on a fresh job"
        );
        release(&pool, job, &pod.name).await;
        assert_eq!(released(&dir), "j-1 pod-j-1\n");

        // After a failed worker: the pod it had is asked for again.
        provision(&pool, job, Some("pod-j-1")).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(job_dir.join("asked"))
                .unwrap()
                .trim(),
            "--zone a pod-j-1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_pool_that_fails_says_why_and_gets_back_a_pod_it_cannot_use() {
        let dir = scratch("fail");
        let job = PodJob {
            id: "j-2",
            dir: &dir,
            state_dir: &dir,
        };
        let cases = [
            (
                "echo 'no capacity in zone a' >&2; exit 3",
                "no capacity in zone a",
            ),
            ("echo not json", "did not print a pod"),
            ("sleep 30", "took more than 1s"),
            (
                r#"echo '{"pod": "p-9", "docker_host": "unix:///var/run/docker.sock", "tls_cert": "/c", "tls_key": "/k", "tls_ca": "/ca"}'"#,
                "is not tcp://<host>:<port>",
            ),
            (
                r#"echo '{"pod": "p-9", "docker_host": "tcp://h:1", "tls_cert": "/c", "tls_key": "/k", "tls_ca": "/ca", "token": "x"}'"#,
                "unknown field",
            ),
        ];
        for (body, why) in cases {
            let pool = ExecutionPool {
                timeout_s: 1,
                ..pool(&dir, body)
            };
            let err = provision(&pool, job, None).await.unwrap_err();
            assert!(err.contains(why), "{body}: {err}");
        }
        // Only the pod it could name but not reach went back.
        assert_eq!(released(&dir), "j-2 p-9\n");

        let missing = ExecutionPool {
            provision: vec![dir.join("absent").display().to_string()],
            ..pool(&dir, GRANT)
        };
        let err = provision(&missing, job, None).await.unwrap_err();
        assert!(err.contains("did not start"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pool_names_both_commands_and_a_timeout() {
        let ok = ExecutionPool {
            provision: vec!["/opt/aegis/pod-up".into()],
            release: vec!["/opt/aegis/pod-down".into()],
            timeout_s: 600,
        };
        assert!(ok.check().is_ok());
        for bad in [
            ExecutionPool {
                provision: vec![],
                ..ok.clone()
            },
            ExecutionPool {
                release: vec![" ".into()],
                ..ok.clone()
            },
            ExecutionPool {
                timeout_s: 0,
                ..ok.clone()
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
        assert!(check_name("pod-1").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("a\nb").is_err());
    }
}
