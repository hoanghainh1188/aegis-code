//! #664: a job whose run names `[run.remote_execution]` keeps its tree on
//! the worker and runs every container of the repo (the formatter of
//! `edit`, the gate's steps) on the execution host's Docker, through its
//! `aegis-docker-proxy --listen-tls` with mutual TLS. The host is played
//! by the proxy serving this machine's daemon, the run's `state_dir`
//! shared as it must be. A worker whose certificate the host does not
//! take stops before the agent starts, saying why.
//!
//! Needs Docker and the `aegis-sandbox-python` image (`docker/python`);
//! skipped otherwise, unless `AEGIS_REQUIRE_DOCKER=1`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use aegis_docker_proxy::{Policy, Proxy};
use aegis_lang::Profiles;
use aegis_ledger::{MemoryLedger, Outcome, PriceTable};
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
        "aegis-remote-{}-{}",
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

fn config(root: &Path, docker_host: &str, worker: &MtlsFiles) -> RunConfig {
    let (docker, _) = logging_docker(root);
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
[run.remote_execution]
docker_host = "{docker_host}"
tls_cert = "{cert}"
tls_key = "{key}"
tls_ca = "{ca}"
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
        cert = worker.cert.display(),
        key = worker.key.display(),
        ca = worker.ca.display(),
        docker = docker.display(),
    ))
    .unwrap()
}

/// Light answers by its job; Core follows its script.
struct Scripted {
    core: Mutex<Vec<Vec<ContentBlock>>>,
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
                let mut script = self.core.lock().unwrap();
                assert!(!script.is_empty(), "core script exhausted");
                script.remove(0)
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

fn platform(config: RunConfig) -> Arc<Platform<Scripted, MemoryLedger>> {
    Arc::new(Platform {
        config,
        provider: Scripted {
            core: Mutex::new(script()),
        },
        store: MemoryLedger::new(),
        prices: PriceTable::from_toml(PRICES).unwrap(),
        profiles: Profiles::builtin().unwrap(),
        github: aegis_pr::TokenSource::Static("t".into()),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_runs_its_containers_on_the_execution_host() {
    let Some(socket) = docker() else { return };
    let scratch = python_repo();
    let root = &scratch.0;
    let certs = root.join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    let (workers, hosts, other) = (ca("workers"), ca("hosts"), ca("other"));
    let host = files(&certs, "host", &hosts, &["localhost"], true, &workers);
    let worker = files(&certs, "worker", &workers, &["worker"], false, &hosts);
    let stranger = files(&certs, "stranger", &other, &["worker"], false, &hosts);

    // The execution host: the proxy on this machine's daemon, the run's
    // state shared at the same path (here, the same disk).
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

    let p = platform(config(root, &docker_host, &worker));
    let report = run_issue(Arc::clone(&p), issue(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::PrOpened, "{report:?}");
    let record = p
        .store
        .record(&aegis_ledger::IssueId::parse("fixture-py-7").unwrap())
        .unwrap();
    assert!(
        record.gate_runs.iter().any(|g| g.passed),
        "{:?}",
        record.gate_runs
    );
    let job_dir = root.join("state/jobs/fixture-py-7");
    let wrapper = std::fs::read_to_string(job_dir.join("docker-remote")).unwrap();
    assert!(
        wrapper.contains(&docker_host) && wrapper.contains("--tlsverify"),
        "{wrapper}"
    );
    // Every container of the job ran on the host: each `docker run` the
    // gate and the ACI made went to its proxy with the worker's
    // certificate, the formatter's and the gate's steps among them.
    let calls = std::fs::read_to_string(root.join("docker-calls.log")).unwrap();
    let runs: Vec<&str> = calls.lines().filter(|l| l.contains(" run ")).collect();
    assert!(runs.len() >= 4, "{calls}");
    assert!(
        runs.iter()
            .all(|l| l.starts_with(&format!("--host {docker_host} --tlsverify"))),
        "{calls}"
    );
    assert!(
        runs.iter().any(|l| l.contains("ruff format")),
        "the ACI's formatter: {calls}"
    );
    assert!(
        runs.iter().any(|l| l.contains("pytest")),
        "the gate's tests: {calls}"
    );

    // A worker the host does not take stops before the agent starts.
    let q = platform(config(root, &docker_host, &stranger));
    let mut refused = issue();
    refused.id = "fixture-py-8".into();
    let report = run_issue(Arc::clone(&q), refused, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::StoppedSteps);
    let reason = report.reason.unwrap_or_default();
    assert!(reason.contains("remote execution host"), "{reason}");
    assert_eq!(
        q.provider.core.lock().unwrap().len(),
        3,
        "Core never called"
    );
}

/// Without the sandbox the gate runs on the worker: a remote host would
/// run nothing, so the run is refused.
#[test]
fn remote_execution_needs_the_sandbox() {
    let root = std::env::temp_dir().join(format!("aegis-remote-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let worker = MtlsFiles {
        cert: "/w.crt".into(),
        key: "/w.key".into(),
        ca: "/ca.crt".into(),
    };
    let err = {
        let toml = format!(
            "[provider]\nkind = \"claude_api\"\n[models.light]\nid = \"l\"\n[models.core]\nid = \"c\"\n[models.expert]\nid = \"e\"\n[run]\nstate_dir = \"{}\"\npricing = \"p\"\n[run.remote_execution]\ndocker_host = \"tcp://h:2376\"\ntls_cert = \"{}\"\ntls_key = \"{}\"\ntls_ca = \"{}\"\n[budget]\nmax_cost_usd = 4.0\n[repo]\npath = \"{}\"\nlanguage = \"python\"\nbase = \"main\"\nkey = \"k\"\n[repo.fork]\nowner = \"o\"\nname = \"n\"\ntoken_env = \"T\"\n[identity]\nname = \"a\"\nemail = \"a@e.invalid\"\n",
            root.display(),
            worker.cert.display(),
            worker.key.display(),
            worker.ca.display(),
            root.display()
        );
        RunConfig::from_toml(&toml).unwrap_err().to_string()
    };
    assert!(
        err.contains("run.remote_execution needs run.sandbox"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
