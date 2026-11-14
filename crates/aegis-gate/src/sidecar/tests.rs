use super::*;

fn postgres() -> SidecarSpec {
    SidecarSpec {
        name: "postgres".into(),
        image: "postgres:16-alpine".into(),
        user: "70:70".into(),
        env: BTreeMap::from([("POSTGRES_PASSWORD".into(), "test".into())]),
        tmpfs: vec![
            "/var/lib/postgresql/data".into(),
            "/var/run/postgresql".into(),
        ],
        ready: vec!["pg_isready".into(), "-h".into(), "localhost".into()],
        ready_timeout_s: 20,
        localhost: false,
        test_env: BTreeMap::from([(
            "DATABASE_URL".into(),
            "postgres://postgres:test@postgres:5432/postgres".into(),
        )]),
        memory_mb: None,
        cpus: None,
    }
}

fn redis() -> SidecarSpec {
    SidecarSpec {
        name: "redis".into(),
        image: "redis:7-alpine".into(),
        user: "999:1000".into(),
        env: BTreeMap::new(),
        tmpfs: vec!["/data".into()],
        ready: vec!["redis-cli".into(), "ping".into()],
        ready_timeout_s: 20,
        localhost: false,
        test_env: BTreeMap::new(),
        memory_mb: Some(256),
        cpus: Some(1),
    }
}

#[test]
fn a_declared_service_is_checked() {
    check_sidecars(&[postgres(), redis()]).unwrap();
    let bad = |f: fn(&mut SidecarSpec), says: &str| {
        let mut spec = postgres();
        f(&mut spec);
        let err = check_sidecars(&[spec]).unwrap_err();
        assert!(err.contains(says), "{err}");
    };
    bad(|s| s.name = "Postgres".into(), "host name");
    bad(|s| s.name = "pg_1".into(), "host name");
    bad(|s| s.name = "-pg".into(), "host name");
    bad(|s| s.user = "0:0".into(), "non-root");
    bad(|s| s.user = "postgres".into(), "non-root");
    bad(|s| s.ready = vec![], "`ready`");
    bad(|s| s.ready_timeout_s = 0, "ready_timeout_s");
    bad(|s| s.tmpfs = vec!["data".into()], "plain absolute");
    bad(|s| s.tmpfs = vec!["/data,uid=0".into()], "plain absolute");
    bad(|s| s.tmpfs = vec!["/a/../etc".into()], "plain absolute");
    bad(|s| s.image = "postgres 16".into(), "image");
    bad(
        |s| {
            s.test_env.insert("BAD-KEY".into(), "x".into());
        },
        "environment variable",
    );
    bad(|s| s.memory_mb = Some(0), "limits");

    let err = check_sidecars(&[postgres(), postgres()]).unwrap_err();
    assert!(err.contains("twice"), "{err}");
    let mut a = postgres();
    a.localhost = true;
    let mut b = redis();
    b.localhost = true;
    let err = check_sidecars(&[a, b]).unwrap_err();
    assert!(err.contains("only one"), "{err}");
}

#[test]
fn a_sidecar_runs_hardened_with_its_data_on_tmpfs() {
    let args = Sidecars::run_args(
        &postgres(),
        "aegis-sidecar-1-0-postgres",
        "aegis-sidecar-1-0",
    );
    let text = args.join(" ");
    for want in [
        "run -d --name aegis-sidecar-1-0-postgres",
        "--network aegis-sidecar-1-0",
        "--network-alias postgres",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "--user 70:70",
        "--read-only",
        "--memory 1024m --memory-swap 1024m",
        "--tmpfs /var/lib/postgresql/data:rw,nosuid,nodev,size=512m,uid=70,gid=70,mode=0700",
        "--env POSTGRES_PASSWORD=test",
    ] {
        assert!(text.contains(want), "{want} not in {text}");
    }
    assert_eq!(args.last().unwrap(), "postgres:16-alpine");
    // Nothing of the host.
    assert!(!text.contains("--volume"), "{text}");
}

/// A `docker` that logs its arguments to `calls` in its directory; the
/// probe exits with the code in the file `probe`, `inspect` says the
/// container runs unless the file `stopped` is there.
#[cfg(unix)]
struct FakeDocker {
    dir: std::path::PathBuf,
}

#[cfg(unix)]
impl FakeDocker {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aegis-sidecar-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = "#!/bin/sh\n\
            d=$(dirname \"$0\")\n\
            echo \"$*\" >> \"$d/calls\"\n\
            case \"$1\" in\n\
            inspect) if [ -e \"$d/stopped\" ]; then echo false; else echo true; fi; exit 0;;\n\
            logs) echo 'database system is starting up'; exit 0;;\n\
            run) case \"$*\" in *--rm*) exit $(cat \"$d/probe\" 2>/dev/null || echo 0);; esac; echo id; exit 0;;\n\
            esac\n\
            exit 0\n";
        let path = dir.join("docker");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's fork may hold the write end until it execs
        // (ETXTBSY on Linux).
        for _ in 0..100 {
            match Command::new(&path).arg("noop").output() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
        let _ = std::fs::remove_file(dir.join("calls"));
        Self { dir }
    }

    fn docker(&self) -> String {
        self.dir.join("docker").display().to_string()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn set(&self, file: &str, text: &str) {
        std::fs::write(self.dir.join(file), text).unwrap();
    }
}

#[cfg(unix)]
impl Drop for FakeDocker {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
fn verbs(calls: &[String]) -> Vec<String> {
    calls
        .iter()
        .map(|c| {
            let words: Vec<&str> = c.split_whitespace().collect();
            match words.as_slice() {
                ["run", "--rm", ..] => "probe".to_owned(),
                ["run", "-d", "--name", name, ..] => format!("run {}", short(name)),
                ["network", verb, .., net] => format!("network {verb} {}", short(net)),
                ["inspect", .., name] => format!("inspect {}", short(name)),
                [verb, .., name] => format!("{verb} {}", short(name)),
                _ => c.clone(),
            }
        })
        .collect()
}

/// `aegis-sidecar-<pid>-<n>-postgres` → `postgres`, the network → `net`.
#[cfg(unix)]
fn short(name: &str) -> String {
    let rest = name.strip_prefix(SIDECAR_PREFIX).unwrap_or(name);
    let mut parts = rest.splitn(3, '-');
    let (_, _, tail) = (parts.next(), parts.next(), parts.next());
    tail.unwrap_or("net").to_owned()
}

#[cfg(unix)]
#[test]
fn sidecars_start_reset_and_go_away() {
    let fake = FakeDocker::new("life");
    let sidecars = Sidecars {
        docker: fake.docker(),
        specs: vec![postgres(), redis()],
    };
    let session = sidecars.start().unwrap();
    assert_eq!(
        verbs(&fake.calls()),
        [
            "network create net",
            "run postgres",
            "run redis",
            "inspect postgres",
            "probe",
            "inspect redis",
            "probe",
        ]
    );
    let probe = fake.calls()[4].clone();
    assert!(
        probe.contains("--network container:aegis-sidecar-"),
        "{probe}"
    );
    assert!(
        probe.contains("--entrypoint pg_isready postgres:16-alpine -h localhost"),
        "{probe}"
    );
    assert!(probe.contains("--user 70:70"), "{probe}");

    fake.set("calls", "");
    session.reset().unwrap();
    assert_eq!(
        verbs(&fake.calls()),
        [
            "stop postgres",
            "start postgres",
            "stop redis",
            "start redis",
            "inspect postgres",
            "probe",
            "inspect redis",
            "probe",
        ]
    );

    fake.set("calls", "");
    drop(session);
    assert_eq!(
        verbs(&fake.calls()),
        ["rm postgres", "rm redis", "network rm net"]
    );
}

#[cfg(unix)]
#[test]
fn the_test_steps_join_the_sidecars() {
    let fake = FakeDocker::new("join");
    let sandbox = Sandbox {
        docker: fake.docker(),
        image: "aegis-sandbox-python".into(),
        mounts: vec![],
        env: vec![("A".into(), "1".into())],
        network: None,
        read_only_mounts: true,
        hardening: crate::Hardening::default(),
    };
    let by_name = Sidecars {
        docker: fake.docker(),
        specs: vec![postgres(), redis()],
    }
    .start()
    .unwrap();
    let test = by_name.sandbox_for(&sandbox);
    let network = test.network.clone().unwrap();
    assert!(
        network.starts_with(SIDECAR_PREFIX) && !network.contains("postgres"),
        "{network}"
    );
    assert!(test.env.contains(&("A".into(), "1".into())));
    assert!(
        test.env
            .iter()
            .any(|(k, v)| k == "DATABASE_URL" && v.contains("@postgres:5432"))
    );

    let mut local = postgres();
    local.localhost = true;
    let on_localhost = Sidecars {
        docker: fake.docker(),
        specs: vec![local, redis()],
    }
    .start()
    .unwrap();
    let network = on_localhost.sandbox_for(&sandbox).network.unwrap();
    assert!(
        network.starts_with(&format!("container:{SIDECAR_PREFIX}"))
            && network.ends_with("-postgres"),
        "{network}"
    );
}

#[cfg(unix)]
#[test]
fn a_sidecar_that_is_not_ready_in_time_is_an_error_with_its_log() {
    let fake = FakeDocker::new("slow");
    fake.set("probe", "2");
    let mut spec = postgres();
    spec.ready_timeout_s = 1;
    let err = Sidecars {
        docker: fake.docker(),
        specs: vec![spec],
    }
    .start()
    .unwrap_err()
    .to_string();
    assert!(err.contains("sidecar `postgres`"), "{err}");
    assert!(
        err.contains("not ready after 1s (`pg_isready -h localhost`)"),
        "{err}"
    );
    assert!(err.contains("database system is starting up"), "{err}");
    // What was started is gone.
    let calls = verbs(&fake.calls());
    assert_eq!(
        calls[calls.len() - 2..],
        ["rm postgres", "network rm net"],
        "{calls:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_sidecar_that_stops_or_a_probe_that_cannot_start_is_an_error() {
    let fake = FakeDocker::new("stopped");
    fake.set("stopped", "");
    let err = Sidecars {
        docker: fake.docker(),
        specs: vec![postgres()],
    }
    .start()
    .unwrap_err()
    .to_string();
    assert!(err.contains("stopped before it was ready"), "{err}");

    let fake = FakeDocker::new("refused");
    fake.set("probe", "125");
    let err = Sidecars {
        docker: fake.docker(),
        specs: vec![postgres()],
    }
    .start()
    .unwrap_err()
    .to_string();
    assert!(err.contains("readiness probe did not start"), "{err}");
}
