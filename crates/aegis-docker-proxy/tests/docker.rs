//! "Xong khi" of #321 against a real daemon: through the proxy, mounting
//! the machine and privileged containers are refused, a hardened sandbox
//! with the workspace mounted runs, and calls aegis does not make fail.
//! Needs Docker and the `python:3.12-alpine` image; skipped otherwise,
//! unless `AEGIS_REQUIRE_DOCKER=1`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aegis_docker_proxy::{Policy, Proxy, bind};

const IMAGE: &str = "python:3.12-alpine";

/// The daemon's socket, from the current context.
fn daemon() -> Option<PathBuf> {
    let out = Command::new("docker")
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .output()
        .ok()?;
    let host = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let socket = PathBuf::from(host.strip_prefix("unix://")?);
    let image = Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .output()
        .is_ok_and(|o| o.status.success());
    (out.status.success() && image).then_some(socket)
}

fn docker_ready() -> Option<PathBuf> {
    let socket = daemon();
    if socket.is_none() {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or {IMAGE} is missing"
        );
        eprintln!("skipping: docker or {IMAGE} is missing");
    }
    socket
}

/// The flags `Hardening::args` gives every sandbox (#320).
fn hardened() -> Vec<String> {
    let user = host_user();
    [
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "64",
        "--memory",
        "64m",
        "--memory-swap",
        "64m",
        "--cpus",
        "1",
        "--read-only",
        "--network",
        "none",
        "--user",
        &user,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// This process's uid:gid, as the gate's `host_user`: the owner of what
/// the test writes.
fn host_user() -> String {
    use std::os::unix::fs::MetadataExt;
    let probe = std::env::temp_dir().join(format!("adp-user-{}", std::process::id()));
    std::fs::write(&probe, "").unwrap();
    let meta = std::fs::metadata(&probe).unwrap();
    let _ = std::fs::remove_file(&probe);
    format!("{}:{}", meta.uid(), meta.gid())
}

fn run(socket: &Path, args: &[String]) -> Output {
    Command::new("docker")
        .env("DOCKER_HOST", format!("unix://{}", socket.display()))
        .args(args)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn only_hardened_sandboxes_of_the_workspace_get_through() {
    let Some(daemon) = docker_ready() else {
        return;
    };
    // Short: a Unix socket's path has a length limit (104 bytes on macOS).
    let root = std::env::temp_dir().join(format!("adp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let socket = root.join("d.sock");
    let listener = {
        let _guard = runtime().enter();
        bind(&socket).unwrap()
    };
    let proxy = Proxy {
        upstream: daemon,
        policy: Policy {
            mount_roots: vec![workspace.clone()],
            ..Policy::default()
        },
    };
    runtime().spawn(proxy.serve(listener));

    let name = |n: u32| format!("aegis-gate-{}-{n}", std::process::id());
    let sandbox = |n: u32, extra: &[&str], argv: &[&str]| {
        let mut args: Vec<String> = ["run", "--rm", "--name", &name(n)]
            .map(str::to_owned)
            .to_vec();
        args.extend(hardened());
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        args.push(IMAGE.into());
        args.extend(argv.iter().map(|s| (*s).to_owned()));
        args
    };

    let work = format!("{}:/work", workspace.display());
    let ok = run(&socket, &sandbox(0, &["--volume", &work], &["ls", "/work"]));
    assert!(ok.status.success(), "{}", text(&ok));

    for (n, extra, why) in [
        (1, vec!["--volume", "/:/host"], "outside the workspace"),
        (2, vec!["--privileged"], "Privileged"),
        (3, vec!["--cap-add", "SYS_ADMIN"], "CapAdd"),
        (4, vec!["--pid", "host"], "PidMode"),
    ] {
        let out = run(&socket, &sandbox(n, &extra, &["true"]));
        assert!(!out.status.success(), "{extra:?}: {}", text(&out));
        assert!(text(&out).contains(why), "{extra:?}: {}", text(&out));
    }

    // Unhardened, or not named by aegis.
    let bare = run(
        &socket,
        &["run", "--rm", "--name", &name(5), IMAGE, "true"].map(str::to_owned),
    );
    assert!(text(&bare).contains("refused"), "{}", text(&bare));
    for args in [
        vec!["ps"],
        vec!["exec", "anything", "true"],
        vec!["volume", "create"],
    ] {
        let out = run(
            &socket,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        );
        assert!(text(&out).contains("refused"), "{args:?}: {}", text(&out));
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}
