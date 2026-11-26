//! The gate on a job's tree: its sandbox, fetch and caches per part (#5,
//! #66, #71), the hidden tests, and a kept patch.

use super::*;

/// Where a part's caches live on the host: the repo's cache dir, a
/// directory of its own for a part (#71).
pub(super) fn part_cache(cfg: &RunConfig, part: &ProfilePart) -> PathBuf {
    let cache = cfg.run.state_dir.join("cache").join(&cfg.repo.key);
    match part.dir.is_empty() {
        true => cache,
        false => cache.join("parts").join(part.dir.replace('/', "_")),
    }
}

/// The gate's container: the profile's image, and each cache directory
/// outside the tree kept on the host under `state_dir/cache/<repo>/`, so it
/// survives between jobs.
/// The repo's build cache (`target/` for Rust), when the gate runs in a
/// sandbox: every container sees the tree at the same `/work`, so a build
/// started from another tree's directory is reused (#104). Directories the
/// fetch step fills (a venv) are mounted, not cached here.
pub(crate) fn build_cache(cfg: &RunConfig, part: &ProfilePart) -> Option<BuildCache> {
    let profile = &part.profile;
    let deps = &profile.profile.dependencies.dirs;
    let dirs: Vec<String> = profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| !d.starts_with('/') && !deps.contains(d))
        .cloned()
        .collect();
    (cfg.run.sandbox && !dirs.is_empty()).then(|| BuildCache {
        dir: part_cache(cfg, part).join("build"),
        dirs,
    })
}

/// The Sonar step of the configuration, its scanner cache under the
/// state dir, shared by the repo's jobs.
pub(crate) fn sonar_step(cfg: &RunConfig) -> Result<Option<SonarStep>, LifecycleError> {
    let Some(sonar) = &cfg.sonar else {
        return Ok(None);
    };
    let cache = cfg.run.state_dir.join("cache").join("sonar");
    std::fs::create_dir_all(&cache).map_err(|e| LifecycleError::io(&cache, &e))?;
    sonar.step(Some(cache)).map(Some)
}

/// The profile's fetch command and its egress, when the gate runs in a
/// sandbox (#66). On the machine there is no cache to fetch into.
pub(crate) fn fetch_step(cfg: &RunConfig, profile: &ResolvedProfile) -> Option<FetchStep> {
    let command = profile.fetch.command()?.render(&[]).ok()?;
    cfg.run.sandbox.then(|| FetchStep {
        egress: Egress {
            docker: cfg.run.docker.clone(),
            proxy_image: cfg.run.egress_image.clone(),
            allow: profile.fetch_hosts.clone(),
            ready_timeout: Duration::from_secs(cfg.run.egress_ready_timeout_s),
        },
        command,
        audit_hosts: profile.profile.dependencies.audit_hosts.clone(),
    })
}

/// The base's dependencies, fetched through the egress proxy before the
/// agent starts (#66); `Some(reason)` when that fails.
pub(super) async fn fetch_base(
    cfg: &RunConfig,
    part: &ProfilePart,
    tree: &Path,
    job_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Option<String>, LifecycleError> {
    let (Some(fetch), Some(sandbox)) = (fetch_step(cfg, &part.profile), sandbox(cfg, part)?) else {
        return Ok(None);
    };
    let log = match part.dir.is_empty() {
        true => "fetch.log".to_owned(),
        false => format!("fetch-{}.log", part.dir.replace('/', "_")),
    };
    let gate_cancel = CancelToken::new();
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let timeout = Duration::from_secs(cfg.run.step_timeout_s);
    let (tree, log) = (tree.to_path_buf(), job_dir.join(log));
    let outcome = tokio::task::spawn_blocking(move || -> Result<_, aegis_gate::GateError> {
        let session = fetch.egress.start()?;
        let ran = session.fetch(&sandbox, &fetch.command, &tree, &log, timeout, gate_cancel)?;
        Ok((ran, session.log()))
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("the fetch did not finish: {e}")))??;
    let (ran, proxy) = outcome;
    if ran.exit.success() {
        // The base's dependencies, upstream's only: the remote cache may
        // send them up (#665). Without the marker it waits for a build.
        let _ = aegis_gate::mark_refreshed(&part_cache(cfg, part));
        return Ok(None);
    }
    let log = std::fs::read_to_string(&ran.log).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(20).collect();
    Ok(Some(format!(
        "fetching the dependencies of {} failed:\n{}\n\nproxy:\n{proxy}",
        part.label(),
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    )))
}

pub(crate) fn sandbox(
    cfg: &RunConfig,
    part: &ProfilePart,
) -> Result<Option<Sandbox>, LifecycleError> {
    if !cfg.run.sandbox {
        return Ok(None);
    }
    let profile = &part.profile;
    let cache = part_cache(cfg, part);
    let mut mounts = Vec::new();
    for dir in profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| d.starts_with('/'))
    {
        let host = cache.join(dir.trim_start_matches('/').replace('/', "_"));
        std::fs::create_dir_all(&host).map_err(|e| LifecycleError::io(&host, &e))?;
        mounts.push((host, dir.clone()));
    }
    // Dependencies installed inside the tree (a venv) live outside it, so
    // the base copy and every container of the job see the same ones.
    for dir in &profile.profile.dependencies.dirs {
        let host = cache.join("deps").join(dir.replace('/', "_"));
        std::fs::create_dir_all(&host).map_err(|e| LifecycleError::io(&host, &e))?;
        mounts.push((host, format!("{WORKDIR}/{dir}")));
    }
    // For the worker's sweep (#593): these caches are used even by a repo
    // without a build cache. A marker that cannot be written leaves the
    // sweep the directory's own time; the job goes on.
    if !mounts.is_empty() {
        let _ = aegis_gate::mark_used(&cache);
    }
    Ok(Some(Sandbox {
        docker: cfg.run.docker.clone(),
        image: profile.image.clone(),
        mounts,
        env: cfg.run.sandbox_env.clone().into_iter().collect(),
        network: None,
        read_only_mounts: true,
        hardening: Hardening::new(cfg.run.sandbox_limits, &profile.profile.sandbox)
            .with_runtime(cfg.run.sandbox_runtime.clone()),
    }))
}

/// The policy file's packages and the repo's own.
pub(super) fn allowed_packages(cfg: &RunConfig) -> Result<BTreeSet<String>, LifecycleError> {
    let own = cfg.repo.allowed_package_names.iter().cloned();
    let Some(path) = &cfg.repo.allowed_packages else {
        return Ok(own.collect());
    };
    let text = std::fs::read_to_string(path).map_err(|e| LifecycleError::io(path, &e))?;
    let mut names = AllowList::from_toml(&text)?.names().clone();
    names.extend(own);
    Ok(names)
}

/// Where a job's gate runs: its patched `tree`, the `base` commit the
/// patch is on, and the job's own directory under the state dir.
#[derive(Clone, Copy)]
pub(crate) struct JobTree<'a> {
    pub tree: &'a Path,
    pub base: &'a str,
    pub job_dir: &'a Path,
}

/// What a job's gate lets its patch do: change the dependencies as
/// `dependencies` says (#500), and prove itself with tests as `tests` does
/// (#676): a bug-fix's reproduction, or a feature's acceptance tests.
#[derive(Debug, Clone)]
pub(crate) struct GatePolicy {
    pub dependencies: DependencyPolicy,
    pub tests: aegis_gate::TestMode,
}

impl GatePolicy {
    /// A bug-fix's: the lockfiles as they are, a test that reproduces it.
    pub(crate) fn locked() -> Self {
        Self {
            dependencies: DependencyPolicy::Locked,
            tests: aegis_gate::TestMode::Reproduce,
        }
    }
}

/// The gate on the job's tree, off the async runtime; `cancel` stops it.
pub(crate) async fn gate(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    job: JobTree<'_>,
    answer: &FinalAnswer,
    policy: &GatePolicy,
    cancel: &CancellationToken,
) -> Result<GateReport, LifecycleError> {
    let JobTree {
        tree,
        base,
        job_dir,
    } = job;
    let allowed = allowed_packages(cfg)?;
    let dependency_policy = policy.dependencies.clone();
    let test_mode = policy.tests;
    let root = &profiles.parts()[0];
    // Each directory's own container, fetch and cache (#71).
    let parts = profiles.parts()[1..]
        .iter()
        .map(|p| {
            Ok((
                sandbox(cfg, p)?,
                fetch_step(cfg, &p.profile),
                build_cache(cfg, p),
            ))
        })
        .collect::<Result<Vec<_>, LifecycleError>>()?;
    let sandbox = sandbox(cfg, root)?;
    let sonar = sonar_step(cfg)?;
    let gate_cancel = CancelToken::new();
    // Stops the gate when the job is cancelled, and when this future is
    // dropped (a server aborting the job).
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let owned = (
        tree.to_path_buf(),
        base.to_owned(),
        profiles.clone(),
        parts,
        allowed,
        answer.repro.clone(),
        job_dir.join("gate"),
        Duration::from_secs(cfg.run.step_timeout_s),
        sandbox,
        fetch_step(cfg, &root.profile),
        build_cache(cfg, root),
        answer
            .allowed_test_changes
            .iter()
            .map(|c| c.test.clone())
            .collect::<Vec<_>>(),
        answer
            .allowed_reference_changes
            .iter()
            .map(|g| ReferenceGroup {
                dir: g.references.clone(),
                tests: g.tests.clone(),
            })
            .collect::<Vec<_>>(),
        sonar,
        cfg.repo.audit_policy(),
        cfg.repo.scope_policy(),
    );
    let sidecars = cfg.repo.gate_sidecars(&cfg.run);
    let secrets = cfg.repo.secrets.clone();
    let report = tokio::task::spawn_blocking(move || {
        let (
            repo,
            base,
            profiles,
            parts,
            allowed,
            repro,
            work_dir,
            step_timeout,
            sandbox,
            fetch,
            build_cache,
            allowed_tests,
            reference_groups,
            sonar,
            audit,
            scope,
        ) = owned;
        let parts = profiles.parts()[1..]
            .iter()
            .zip(parts)
            .map(|(p, (sandbox, fetch, build_cache))| GatePart {
                dir: p.dir.clone(),
                profile: &p.profile,
                sandbox,
                fetch,
                build_cache,
            })
            .collect();
        run_gate(&GateRequest {
            repo: &repo,
            base: &base,
            profile: profiles.root(),
            allowed: &allowed,
            repro: repro.as_ref(),
            work_dir: &work_dir,
            env: vec![],
            step_timeout,
            cancel: gate_cancel,
            sandbox,
            fetch,
            build_cache,
            allowed_tests,
            reference_groups,
            sonar,
            audit,
            scope,
            secrets,
            parts,
            sidecars,
            dependency_policy,
            test_mode,
        })
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("the gate did not finish: {e}")))?;
    Ok(report?)
}

pub(crate) struct CancelOnDrop(pub(crate) CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) struct AbortOnDrop(pub(crate) tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The patch the gate passed, whole and ready for `git apply`, in the job
/// dir of a run that opens no PR (#432): nothing else outlives the job's
/// worktree for a reviewer to read. Written before the hidden tests, so a
/// run stopped there still has it.
pub(super) fn keep_patch(
    cfg: &RunConfig,
    job_dir: &Path,
    base: &str,
    tree: &str,
) -> Result<(), LifecycleError> {
    let text = job::full_patch(&cfg.repo.path, base, tree)?;
    let kept = job_dir.join("patch.diff");
    std::fs::write(&kept, format!("{text}\n")).map_err(|e| LifecycleError::io(&kept, &e))
}

/// The fix's own tests on the agent's patch, after the PR is open (the
/// PR holds the gated tree, so copying the tests in changes nothing sent).
/// Same runner and sandbox as the gate's for `part`, the part holding the
/// tests, in its directory (#199); the log stays in the job dir and never
/// reaches the model.
pub(super) async fn run_hidden(
    cfg: &RunConfig,
    part: &ProfilePart,
    tree: &Path,
    job_dir: &Path,
    hidden: &HiddenTests,
    cancel: &CancellationToken,
) -> Result<(bool, Duration), LifecycleError> {
    for file in &hidden.test_files {
        let content = job::show(&cfg.repo.path, &hidden.fix_commit, file)?;
        let Some(content) = content else { continue };
        let target = tree.join(file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LifecycleError::io(parent, &e))?;
        }
        std::fs::write(&target, content).map_err(|e| LifecycleError::io(&target, &e))?;
    }
    let gate_cancel = CancelToken::new();
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let profile: &ResolvedProfile = &part.profile;
    let runner = Runner {
        env: vec![],
        timeout: Duration::from_secs(cfg.run.step_timeout_s),
        cancel: gate_cancel,
        sandbox: sandbox(cfg, part)?,
    };
    let (argv, tree, log) = (
        hidden.command.clone(),
        tree.join(&part.dir),
        job_dir.join("hidden-tests.log"),
    );
    let ran = tokio::task::spawn_blocking(move || runner.run(&argv, &tree, &log, &[]))
        .await
        .map_err(|e| LifecycleError::Config(format!("the hidden tests did not finish: {e}")))??;
    if ran.exit.success() {
        return Ok((true, ran.duration));
    }
    // What failed, kept for the dry run to fill `hidden_known_failures`.
    let failing = profile
        .gate
        .test_output
        .map(|format| {
            failing_tests_with(
                format,
                profile.gate.harness.as_ref(),
                &std::fs::read_to_string(&ran.log).unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let listed = job_dir.join("hidden-failures.txt");
    let text: String = failing.iter().map(|t| format!("{t}\n")).collect();
    std::fs::write(&listed, text).map_err(|e| LifecycleError::io(&listed, &e))?;
    let known = &hidden.known_failures;
    let passed = matches!(ran.exit, aegis_gate::Exit::Code(_))
        && !failing.is_empty()
        && failing.iter().all(|t| known.contains(t));
    Ok((passed, ran.duration))
}

pub(crate) fn gate_run(report: &GateReport, took: Duration) -> GateRun {
    GateRun {
        passed: report.passed(),
        failed_step: report
            .steps
            .iter()
            .find(|s| s.status == StepStatus::Failed)
            .map(|s| s.name.id().to_owned()),
        duration_s: took.as_secs_f64(),
        sidecar_s: report.sidecars.map(|s| s.total_duration().as_secs_f64()),
    }
}
