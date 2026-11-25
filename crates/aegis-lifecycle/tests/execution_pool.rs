//! #665: a run with `[run.execution_pool]` gets each job a pod of its own
//! from the deployment's `provision` command and gives it back with
//! `release` when the job ends. A worker that dies mid-job does not
//! release its pod: the job's next worker goes on from the checkpoint
//! (#662) and asks the pool for the same pod, which it gets back.
//!
//! The pool is two shell scripts; its pod is `aegis-docker-proxy
//! --listen-tls` serving this machine's daemon, as in `remote_execution`.
//! The failover test needs Docker and the `aegis-sandbox-python` image
//! (`docker/python`) and is skipped otherwise, unless
//! `AEGIS_REQUIRE_DOCKER=1`; the others need neither.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aegis_docker_proxy::{Policy, Proxy};
use aegis_lang::Profiles;
use aegis_ledger::{IssueId, LedgerStore, MemoryLedger, Outcome, PriceTable};
use aegis_lifecycle::{IssueInput, Platform, RunConfig, run_issue};
use aegis_mtls::MtlsFiles;
use aegis_provider::{
    ContentBlock, ModelProvider, ProviderError, Request, Response, StopReason, Usage,
};
use common::{PRICES, aci_bin, git};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const IMAGE: &str = "aegis-sandbox-python";

/// The daemon's socket, when Docker and the image are here.
fn docker() -> Option<PathBuf> {
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
    if out.status.success() && image {
        return Some(socket);
    }
    assert!(
        std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
        "docker or {IMAGE} is missing"
    );
    eprintln!("skipping: docker or {IMAGE} is missing");
    None
}

struct Ca {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

fn ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        pem: cert.pem(),
        issuer: Issuer::new(params, key),
    }
}

fn files(
    dir: &Path,
    who: &str,
    issuer: &Ca,
    names: &[&str],
    server: bool,
    trusts: &Ca,
) -> MtlsFiles {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    params.extended_key_usages = vec![if server {
        ExtendedKeyUsagePurpose::ServerAuth
    } else {
        ExtendedKeyUsagePurpose::ClientAuth
    }];
    let cert = params.signed_by(&key, &issuer.issuer).unwrap();
    let write = |name: String, text: String| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    MtlsFiles {
        cert: write(format!("{who}.crt"), cert.pem()),
        key: write(format!("{who}.key"), key.serialize_pem()),
        ca: write(format!("{who}-ca.crt"), trusts.pem.clone()),
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The Python fixture with its off-by-one (`>`) committed on `main`, a
/// bare fork next to it.
fn python_repo() -> Scratch {
    let root = std::env::temp_dir().join(format!(
        "aegis-pool-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let clone = root.join("clone");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/python");
    for file in ["pyproject.toml", "cart/__init__.py", "tests/test_cart.py"] {
        let to = clone.join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixture.join(file), &to).unwrap();
    }
    let lib = std::fs::read_to_string(clone.join("cart/__init__.py")).unwrap();
    std::fs::write(
        clone.join("cart/__init__.py"),
        lib.replacen(">= MAX_ITEMS", "> MAX_ITEMS", 1),
    )
    .unwrap();
    std::fs::write(
        clone.join(".gitignore"),
        "__pycache__/\n.venv/\ncoverage.xml\n",
    )
    .unwrap();
    git(&clone, &["init", "-q", "-b", "main"]);
    git(&clone, &["add", "-A"]);
    git(
        &clone,
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
    git(&root, &["init", "-q", "--bare", "fork.git"]);
    Scratch(root)
}

/// A `docker` that logs each call, then runs the real one.
fn logging_docker(root: &Path) -> (PathBuf, PathBuf) {
    let log = root.join("docker-calls.log");
    let program = root.join("docker-logged");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec docker \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    (program, log)
}

/// The pool: `provision` hands out `pod-<n>`, n from 1, (the one asked for, when
/// `AEGIS_POD` names one) at `docker_host` with the worker's certificate;
/// both commands write down what they were told in `pool.log`.
fn pool_scripts(
    root: &Path,
    docker_host: &str,
    worker: &MtlsFiles,
    fail: bool,
) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = root.join("pool.log");
    let provision = root.join("pod-up");
    let release = root.join("pod-down");
    let grant = if fail {
        "echo 'no capacity left in the pool' >&2\nexit 2".to_owned()
    } else {
        format!(
            r#"if [ -n "$AEGIS_POD" ]; then pod="$AEGIS_POD"; else
  n=$(grep -c '^provision' '{log}'); pod="pod-$n"; fi
echo "{{\"pod\": \"$pod\", \"docker_host\": \"{docker_host}\", \"tls_cert\": \"{cert}\", \"tls_key\": \"{key}\", \"tls_ca\": \"{ca}\"}}""#,
            log = log.display(),
            cert = worker.cert.display(),
            key = worker.key.display(),
            ca = worker.ca.display(),
        )
    };
    std::fs::write(
        &provision,
        format!(
            "#!/bin/sh\necho \"provision $AEGIS_JOB_ID ${{AEGIS_POD:--}} $1\" >> '{}'\n{grant}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::write(
        &release,
        format!(
            "#!/bin/sh\necho \"release $AEGIS_JOB_ID $AEGIS_POD\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    for script in [&provision, &release] {
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (provision, release)
}

fn pool_log(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("pool.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn config(root: &Path, docker_host: &str, worker: &MtlsFiles, fail: bool) -> RunConfig {
    let (docker, _) = logging_docker(root);
    let (provision, release) = pool_scripts(root, docker_host, worker, fail);
    RunConfig::from_toml(&format!(
        r#"
[provider]
kind = "claude_api"
[models.light]
id = "light"
[models.core]
id = "core"
[models.expert]
id = "expert"
[run]
state_dir = "{state}"
pricing = "unused"
aci_bin = "{aci}"
step_timeout_s = 600
sandbox = true
docker = "{docker}"
open_prs = false
[run.execution_pool]
provision = ["{provision}", "--zone=a"]
release = ["{release}"]
timeout_s = 60
[budget]
max_cost_usd = 4.0
max_model_calls = 20
[repo]
path = "{clone}"
language = "python"
base = "main"
key = "fixture-py"
[repo.overrides]
fetch = false
audit = false
[repo.fork]
owner = "me"
name = "fixture-py"
api = "http://127.0.0.1:1"
token_env = "UNUSED"
push_to_local = "{fork}"
[identity]
name = "aegis-bot"
email = "aegis-bot@example.invalid"
"#,
        state = root.join("state").display(),
        aci = aci_bin().display(),
        clone = root.join("clone").display(),
        fork = root.join("fork.git").display(),
        docker = docker.display(),
        provision = provision.display(),
        release = release.display(),
    ))
    .unwrap()
}

/// Light answers by its job; Core follows its script, then, while
/// `hang` is set, never answers (a worker that dies mid-call).
struct Scripted {
    core: Mutex<Vec<Vec<ContentBlock>>>,
    core_calls: AtomicUsize,
    hang: AtomicBool,
}

impl ModelProvider for Scripted {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let system = request.system.concat();
        let content = match request.model.as_str() {
            "light" if system.contains("You triage") => vec![ContentBlock::text(
                r#"{"kind": "bug", "has_repro_info": true, "suspected_crates": [], "summary": "Cart accepts a fourth item."}"#,
            )],
            "light" => vec![ContentBlock::text("fix: refuse a fourth item in the cart")],
            "core" => {
                self.core_calls.fetch_add(1, Ordering::SeqCst);
                let next = {
                    let mut script = self.core.lock().unwrap();
                    (!script.is_empty()).then(|| script.remove(0))
                };
                match next {
                    Some(content) => content,
                    None if self.hang.load(Ordering::SeqCst) => std::future::pending().await,
                    None => panic!("core script exhausted"),
                }
            }
            other => panic!("unexpected model {other}"),
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
            id: "msg".into(),
            model: "scripted".into(),
            content,
            stop_reason: Some(stop),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 100,
                ..Usage::default()
            },
        })
    }
}

fn call(id: &str, name: &str, input: serde_json::Value) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
    }]
}

const REPRO: &str = "import pytest\n\nfrom cart import CartFullError, cart_of\n\n\ndef test_fourth_item_is_refused():\n    cart = cart_of(1, 2, 3)\n    with pytest.raises(CartFullError):\n        cart.add(4)\n";

fn script() -> Vec<Vec<ContentBlock>> {
    vec![
        call(
            "t1",
            "create_file",
            json!({"file": "tests/test_fourth.py", "content": REPRO}),
        ),
        call(
            "t2",
            "edit",
            json!({"file": "cart/__init__.py", "from": 17, "to": 17, "content": "        if len(self.items) >= MAX_ITEMS:"}),
        ),
        vec![ContentBlock::text(
            json!({
                "status": "fixed",
                "cause": "`add` compared with `>` so a fourth item was accepted.",
                "evidence": [{"location": "cart/__init__.py:17", "excerpt": "MAX_ITEMS"}],
                "changes": [{"location": "cart/__init__.py:17", "note": "compare with >="}],
                "repro_test": {"name": "tests/test_fourth.py", "files": ["tests/test_fourth.py"]},
                "allowed_test_changes": [],
                "stop_reason": null
            })
            .to_string(),
        )],
    ]
}

fn issue() -> IssueInput {
    IssueInput {
        cve: None,
        id: "fixture-py-7".into(),
        title: "Cart accepts a fourth item".into(),
        body: "`cart_of(1, 2, 3).add(4)` does not raise CartFullError.".into(),
        url: "https://github.com/me/fixture-py/issues/7".into(),
        base: None,
        hidden: None,
        kind: None,
        source: Some("github".into()),
        memory: String::new(),
    }
}

fn platform(
    config: RunConfig,
    core: Vec<Vec<ContentBlock>>,
) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: Scripted {
            core: Mutex::new(core),
            core_calls: AtomicUsize::new(0),
            hang: AtomicBool::new(false),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github: aegis_pr::TokenSource::Static("t".into()),
    })
}

/// A worker gets its job a pod from the pool and dies after Core's first
/// two calls (the repro test written, the fix made), its pod still up.
/// The job's next worker goes on from the checkpoint, gets the same pod
/// back from the pool, runs the gate there, opens the PR and releases
/// the pod: one pod for the whole job, released once.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_workers_job_goes_on_on_the_same_pod() {
    let Some(socket) = docker() else { return };
    let scratch = python_repo();
    let root = &scratch.0;
    let certs = root.join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    let (workers, hosts) = (ca("workers"), ca("hosts"));
    let host = files(&certs, "pod", &hosts, &["localhost"], true, &workers);
    let worker = files(&certs, "worker", &workers, &["worker"], false, &hosts);

    // The pod: the proxy on this machine's daemon, the run's state at the
    // same path.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = Proxy {
        upstream: socket,
        policy: Policy {
            mount_roots: vec![root.canonicalize().unwrap(), root.clone()],
            ..Policy::default()
        },
    };
    tokio::spawn(proxy.serve_tls(listener, aegis_mtls::acceptor(&host).unwrap()));
    let docker_host = format!("tcp://localhost:{port}");

    let mut first_calls = script();
    let answer = first_calls.pop().unwrap();
    let p = platform(config(root, &docker_host, &worker, false), first_calls);
    p.provider.hang.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let p = Arc::clone(&p);
        async move { run_issue(p, issue(), CancellationToken::new()).await }
    });
    for _ in 0..1200 {
        if p.provider.core_calls.load(Ordering::SeqCst) == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        p.provider.core_calls.load(Ordering::SeqCst),
        3,
        "Core's third call in flight"
    );
    first.abort();
    let _ = first.await;
    assert_eq!(
        pool_log(root),
        ["provision fixture-py-7 - --zone=a"],
        "a fresh job asks for no pod, and a dead worker releases none"
    );
    let id = IssueId::parse("fixture-py-7").unwrap();
    let checkpoint = p
        .store
        .checkpoint(&id)
        .await
        .unwrap()
        .expect("a checkpoint");
    assert_eq!(
        checkpoint.state["pod"], "pod-1",
        "the checkpoint keeps the pod"
    );

    p.provider.core.lock().unwrap().push(answer);
    p.provider.hang.store(false, Ordering::SeqCst);
    let report = run_issue(Arc::clone(&p), issue(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    assert_eq!(
        pool_log(root),
        [
            "provision fixture-py-7 - --zone=a",
            "provision fixture-py-7 pod-1 --zone=a",
            "release fixture-py-7 pod-1",
        ],
        "the next worker asks for the same pod and releases it at the end"
    );
    let record = p.store.record(&id).unwrap();
    assert!(
        record.gate_runs.iter().any(|g| g.passed),
        "{:?}",
        record.gate_runs
    );
    // The gate ran on the pod.
    let calls = std::fs::read_to_string(root.join("docker-calls.log")).unwrap();
    let runs: Vec<&str> = calls.lines().filter(|l| l.contains(" run ")).collect();
    assert!(
        !runs.is_empty()
            && runs
                .iter()
                .all(|l| l.starts_with(&format!("--host {docker_host} --tlsverify"))),
        "{calls}"
    );
    assert!(runs.iter().any(|l| l.contains("pytest")), "{calls}");
    assert!(p.store.checkpoint(&id).await.unwrap().is_none());
}

/// A pool with no pod to give stops the job before the agent's first
/// call, with the pool's own words; there is nothing to release.
#[tokio::test(flavor = "multi_thread")]
async fn a_pool_without_a_pod_stops_the_job_saying_why() {
    let scratch = python_repo();
    let root = &scratch.0;
    let worker = MtlsFiles {
        cert: "/w.crt".into(),
        key: "/w.key".into(),
        ca: "/ca.crt".into(),
    };
    let p = platform(config(root, "tcp://localhost:1", &worker, true), script());
    let report = run_issue(Arc::clone(&p), issue(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    let reason = report.reason.unwrap_or_default();
    assert!(reason.contains("no capacity left in the pool"), "{reason}");
    assert_eq!(
        p.provider.core_calls.load(Ordering::SeqCst),
        0,
        "Core never called"
    );
    assert_eq!(pool_log(root), ["provision fixture-py-7 - --zone=a"]);
    assert!(
        !root.join("state/jobs/fixture-py-7/repo").exists(),
        "no tree made"
    );
}

fn bare_config(extra: &str, sandbox: bool) -> String {
    let root = std::env::temp_dir();
    format!(
        "[provider]\nkind = \"claude_api\"\n[models.light]\nid = \"l\"\n[models.core]\nid = \"c\"\n[models.expert]\nid = \"e\"\n[run]\nstate_dir = \"{}\"\npricing = \"p\"\nsandbox = {sandbox}\n{extra}[budget]\nmax_cost_usd = 4.0\n[repo]\npath = \"{}\"\nlanguage = \"python\"\nbase = \"main\"\nkey = \"k\"\n[repo.fork]\nowner = \"o\"\nname = \"n\"\ntoken_env = \"T\"\n[identity]\nname = \"a\"\nemail = \"a@e.invalid\"\n",
        root.display(),
        root.display()
    )
}

/// A pool needs the sandbox, both its commands, and is the run's only
/// execution host.
#[test]
fn an_execution_pool_is_checked_with_the_run() {
    const POOL: &str =
        "[run.execution_pool]\nprovision = [\"/opt/pod-up\"]\nrelease = [\"/opt/pod-down\"]\n";
    assert!(RunConfig::from_toml(&bare_config(POOL, true)).is_ok());
    let err = |extra: &str, sandbox: bool| {
        RunConfig::from_toml(&bare_config(extra, sandbox))
            .unwrap_err()
            .to_string()
    };
    assert!(
        err(POOL, false).contains("run.execution_pool needs run.sandbox"),
        "{}",
        err(POOL, false)
    );
    let both = format!(
        "{POOL}[run.remote_execution]\ndocker_host = \"tcp://h:2376\"\ntls_cert = \"/c\"\ntls_key = \"/k\"\ntls_ca = \"/ca\"\n"
    );
    assert!(
        err(&both, true).contains("keep one"),
        "{}",
        err(&both, true)
    );
    let no_release = "[run.execution_pool]\nprovision = [\"/opt/pod-up\"]\nrelease = []\n";
    assert!(
        err(no_release, true).contains("run.execution_pool.release names no command"),
        "{}",
        err(no_release, true)
    );
}
