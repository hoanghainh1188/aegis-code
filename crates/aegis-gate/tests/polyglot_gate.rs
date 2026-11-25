//! #598: a polyglot monorepo, the Go fixture at the root and the
//! TypeScript fixture under `web/` (`[[repo.dirs]]`, #71). The gate picks
//! each changed file's profile by its path and runs that part's steps in
//! its own image (`aegis-sandbox-go`, `aegis-sandbox-node`) with no
//! network; one `GateReport` holds them all. A Go step would not start in
//! the Node image nor a TypeScript one in the Go image, so a pass says the
//! image was the part's. Needs Docker, the network and the images built
//! from `docker/<image>`, the egress proxy's too (`aegis-sandbox-python`);
//! skipped otherwise, unless `AEGIS_REQUIRE_DOCKER=1`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aegis_gate::{
    CancelToken, Egress, FetchStep, GatePart, GateReport, GateRequest, Hardening, Limits, Sandbox,
    StepName, StepStatus, run_gate,
};
use aegis_lang::{Language, ProfileOverrides, Profiles, ResolvedProfile};

#[path = "../../../tests/fixture_copy.rs"]
mod fixture_copy;
use fixture_copy::copy_fixture as copy_dir;

fn profile(language: Language, overrides: &str) -> ResolvedProfile {
    let overrides: ProfileOverrides = toml::from_str(overrides).unwrap();
    Profiles::builtin()
        .unwrap()
        .get(language)
        .unwrap()
        .resolve(&overrides)
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The egress proxy runs in the Python image (#66).
const PROXY_IMAGE: &str = "aegis-sandbox-python";

fn docker_ready(images: &[&str]) -> bool {
    let ok = images.iter().chain([&PROXY_IMAGE]).all(|image| {
        Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or one of {images:?} is missing"
        );
        eprintln!("skipping: docker or one of {images:?} is missing");
    }
    ok
}

fn host_user() -> String {
    let id = |flag: &str| {
        let out = Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    format!("{}:{}", id("-u"), id("-g"))
}

struct Monorepo(PathBuf);

impl Drop for Monorepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Monorepo {
    /// Go at the root, TypeScript in `web/`, committed as `main`.
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-gate-polyglot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let repo = Self(root);
        copy_dir(&fixtures.join("go"), &repo.dir());
        copy_dir(&fixtures.join("typescript"), &repo.dir().join("web"));
        // What the fetch and the steps write into the tree.
        repo.write(
            ".gitignore",
            "coverage.out\nweb/node_modules/\nweb/coverage/\n",
        );
        git(&repo.dir(), &["init", "-q", "-b", "main"]);
        repo.commit("base");
        repo
    }

    fn dir(&self) -> PathBuf {
        self.0.join("repo")
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.dir().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn append(&self, file: &str, text: &str) {
        let path = self.dir().join(file);
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, before + text).unwrap();
    }

    fn commit(&self, message: &str) {
        git(&self.dir(), &["add", "-A"]);
        git(
            &self.dir(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                message,
            ],
        );
    }

    /// The profile's cache directories outside the tree, on the host under
    /// the work dir, one set per part.
    fn caches(&self, dir: &str, profile: &ResolvedProfile) -> Vec<(PathBuf, String)> {
        profile
            .profile
            .sandbox
            .cache_dirs
            .iter()
            .filter(|d| d.starts_with('/'))
            .map(|cache| {
                let host = self
                    .0
                    .join("cache")
                    .join(if dir.is_empty() { "root" } else { dir })
                    .join(cache.trim_start_matches('/').replace('/', "_"));
                std::fs::create_dir_all(&host).unwrap();
                (host, cache.clone())
            })
            .collect()
    }

    /// The part's fetch in a container of its image with the network, its
    /// directory at `/work`, as the lifecycle runs it before the gate.
    fn fetch(&self, dir: &str, profile: &ResolvedProfile) -> Vec<(PathBuf, String)> {
        let mounts = self.caches(dir, profile);
        let argv = profile.fetch.command().unwrap().render(&[]).unwrap();
        let mut cmd = Command::new("docker");
        cmd.args(["run", "--rm", "--user", &host_user(), "--workdir", "/work"])
            .arg("--volume")
            .arg(format!("{}:/work", self.dir().join(dir).display()));
        for (host, inside) in &mounts {
            cmd.arg("--volume")
                .arg(format!("{}:{inside}", host.display()));
        }
        let out = cmd.arg(&profile.image).args(&argv).output().unwrap();
        assert!(
            out.status.success(),
            "{argv:?} in {}:\n{}{}",
            profile.image,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        mounts
    }

    /// The gate with each part in its own image, as the lifecycle gives it.
    fn gate(&self) -> GateReport {
        let (go, ts) = (profile(Language::Go, ""), profile(Language::TypeScript, ""));
        let (go_mounts, ts_mounts) = (self.fetch("", &go), self.fetch("web", &ts));
        let allowed = BTreeSet::new();
        let (dir, work) = (self.dir(), self.0.join("work"));
        run_gate(&GateRequest {
            repo: &dir,
            base: "main",
            profile: &go,
            allowed: &allowed,
            repro: None,
            work_dir: &work,
            env: vec![],
            step_timeout: Duration::from_secs(900),
            cancel: CancelToken::new(),
            sandbox: Some(sandbox(&go, &go_mounts)),
            fetch: Some(fetch_step(&go)),
            build_cache: None,
            allowed_tests: vec![],
            reference_groups: vec![],
            sonar: None,
            audit: Default::default(),
            scope: aegis_gate::ScopePolicy::default(),
            secrets: aegis_gate::SecretPolicy::default(),
            parts: vec![GatePart {
                dir: "web".into(),
                profile: &ts,
                sandbox: Some(sandbox(&ts, &ts_mounts)),
                fetch: Some(fetch_step(&ts)),
                build_cache: None,
            }],
            sidecars: None,
            dependency_policy: Default::default(),
            test_mode: Default::default(),
        })
        .unwrap()
    }
}

fn sandbox(profile: &ResolvedProfile, mounts: &[(PathBuf, String)]) -> Sandbox {
    Sandbox {
        docker: "docker".into(),
        image: profile.image.clone(),
        mounts: mounts.to_vec(),
        env: vec![],
        network: None,
        read_only_mounts: true,
        hardening: Hardening::new(Limits::default(), &profile.profile.sandbox),
    }
}

fn fetch_step(profile: &ResolvedProfile) -> FetchStep {
    FetchStep {
        egress: Egress {
            docker: "docker".into(),
            proxy_image: PROXY_IMAGE.into(),
            allow: profile.fetch_hosts.clone(),
            ready_timeout: Egress::DEFAULT_READY_TIMEOUT,
        },
        command: profile.fetch.command().unwrap().render(&[]).unwrap(),
        audit_hosts: vec![],
    }
}

/// The parts the test step ran in, by their log directories: `root` for
/// the repo's root, the part's directory otherwise.
fn tested_parts(report: &GateReport) -> Vec<String> {
    let mut parts: Vec<String> = report
        .step(StepName::Test)
        .unwrap()
        .logs
        .iter()
        .map(|log| {
            let shown = log.display().to_string();
            if shown.contains("/logs/web/") {
                "web".to_owned()
            } else {
                "root".to_owned()
            }
        })
        .collect();
    parts.dedup();
    parts
}

fn ready() -> bool {
    docker_ready(&[
        &profile(Language::Go, "").image,
        &profile(Language::TypeScript, "").image,
    ])
}

#[test]
fn a_typescript_patch_is_gated_in_the_node_image_only() {
    if !ready() {
        return;
    }
    let repo = Monorepo::new();
    repo.append("web/src/cart.ts", "\nexport const CART_VERSION = 2;\n");
    let report = repo.gate();
    assert!(report.passed(), "{report}");
    assert_eq!(tested_parts(&report), ["web"], "{report}");
    assert_eq!(
        report.step(StepName::Typecheck).unwrap().status,
        StepStatus::Passed,
        "{report}"
    );
}

#[test]
fn a_go_patch_is_gated_in_the_go_image_only() {
    if !ready() {
        return;
    }
    let repo = Monorepo::new();
    repo.append(
        "cart.go",
        "\n// Version of the cart package.\nconst Version = 2\n",
    );
    let report = repo.gate();
    assert!(report.passed(), "{report}");
    assert_eq!(tested_parts(&report), ["root"], "{report}");
}

#[test]
fn a_patch_across_both_is_gated_by_each_part_in_one_report() {
    if !ready() {
        return;
    }
    let repo = Monorepo::new();
    repo.append(
        "cart.go",
        "\n// Version of the cart package.\nconst Version = 2\n",
    );
    repo.append("web/src/cart.ts", "\nexport const CART_VERSION = 2;\n");
    let report = repo.gate();
    assert!(report.passed(), "{report}");
    assert_eq!(tested_parts(&report), ["root", "web"], "{report}");

    // A broken TypeScript file fails in its part, named as such.
    let repo = Monorepo::new();
    repo.append(
        "cart.go",
        "\n// Version of the cart package.\nconst Version = 2\n",
    );
    repo.append(
        "web/src/cart.ts",
        "\nexport const broken: number = \"two\";\n",
    );
    let report = repo.gate();
    assert!(!report.passed(), "{report}");
    let failed = report
        .steps
        .iter()
        .find(|s| s.status == StepStatus::Failed)
        .unwrap();
    assert!(failed.detail.contains("web"), "{}", failed.detail);
}
