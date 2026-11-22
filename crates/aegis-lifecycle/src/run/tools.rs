//! The ACI and code-graph tools a job's agent gets as MCP servers, and the
//! files they read.

use super::*;

/// In the Codebase-Memory server's `args` and `env`: the index directory
/// under the state dir.
pub(super) const INDEX_DIR: &str = "{index_dir}";

/// `repo.dirs` as the ACI's `--parts` file (`[[part]]`, #71), with the
/// mounts of each part's gate steps (its `.venv`, caches, #198).
pub(super) fn parts_file(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
) -> Result<String, LifecycleError> {
    let mut parts = Vec::new();
    // `RepoProfiles` keeps the order of `repo.dirs`, after the root.
    for (d, p) in cfg.repo.dirs.iter().zip(&profiles.parts()[1..]) {
        let mounts: Vec<toml::Value> = sandbox(cfg, p)?
            .map(|s| s.mounts)
            .unwrap_or_default()
            .into_iter()
            .map(|(host, inside)| {
                let mut mount = toml::Table::new();
                mount.insert("host".into(), host.display().to_string().into());
                mount.insert("path".into(), inside.into());
                mount.into()
            })
            .collect();
        let mut part = toml::Table::new();
        part.insert("dir".into(), d.path.clone().into());
        part.insert("language".into(), d.language.id().into());
        part.insert("overrides".into(), d.overrides.clone().into());
        part.insert("mounts".into(), mounts.into());
        parts.push(toml::Value::from(part));
    }
    let mut file = toml::Table::new();
    file.insert("part".into(), parts.into());
    toml::to_string(&file).map_err(|e| LifecycleError::Config(format!("repo.dirs: {e}")))
}

/// `repo.sidecars` as the ACI's `--sidecars` file (`[[sidecar]]`, #491).
pub(super) fn sidecars_file(cfg: &RunConfig) -> Result<String, LifecycleError> {
    #[derive(serde::Serialize)]
    struct File<'a> {
        sidecar: &'a [aegis_gate::SidecarSpec],
    }
    toml::to_string(&File {
        sidecar: &cfg.repo.sidecars,
    })
    .map_err(|e| LifecycleError::Config(format!("repo.sidecars: {e}")))
}

/// The variables the `docker` client needs to find the daemon, as `lookup`
/// gives them, other than those every MCP server gets anyway (#278).
pub(super) fn docker_client_env(lookup: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    CLIENT_ENV
        .iter()
        .filter(|k| !INHERITED_ENV.contains(k))
        .filter_map(|k| Some(((*k).to_owned(), lookup(k)?)))
        .collect()
}

pub(super) fn tool_specs(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    job_dir: &Path,
) -> Result<Vec<McpServerSpec>, LifecycleError> {
    let overrides = job_dir.join("overrides.toml");
    let text = toml::to_string(&cfg.repo.overrides)
        .map_err(|e| LifecycleError::Config(format!("repo.overrides: {e}")))?;
    std::fs::write(&overrides, text).map_err(|e| LifecycleError::io(&overrides, &e))?;
    let root = &profiles.parts()[0];
    let profile = &root.profile;
    let mut args = vec![
        "--repo".into(),
        tree.display().to_string(),
        "--language".into(),
        cfg.repo.language.id().into(),
        "--overrides".into(),
        overrides.display().to_string(),
        // `run_tests` stops at the step timeout, or sooner (#137).
        "--test-timeout".into(),
        cfg.run.step_timeout_s.to_string(),
        "--test-dir".into(),
        job_dir.join("run_tests").display().to_string(),
    ];
    if !cfg.repo.dirs.is_empty() {
        let parts = job_dir.join("parts.toml");
        std::fs::write(&parts, parts_file(cfg, profiles)?)
            .map_err(|e| LifecycleError::io(&parts, &e))?;
        args.extend(["--parts".into(), parts.display().to_string()]);
    }
    // With the gate in containers, `edit`'s formatter and `run_tests` run
    // there too: the machine needs no language tools (#104), and the test
    // sees what the gate's would (#137), on the daemon the gate uses (#278).
    let sandboxed = sandbox(cfg, root)?;
    let aci_env = match sandboxed {
        Some(_) => docker_client_env(|k| std::env::var(k).ok()),
        None => vec![],
    };
    if let Some(sandbox) = sandboxed {
        args.extend([
            "--format-image".into(),
            profile.image.clone(),
            "--docker".into(),
            cfg.run.docker.clone(),
        ]);
        for (host, inside) in &sandbox.mounts {
            args.extend([
                "--format-mount".into(),
                format!("{}:{inside}", host.display()),
            ]);
        }
        for (key, value) in &sandbox.env {
            args.extend(["--test-env".into(), format!("{key}={value}")]);
        }
        // The run's limits: the ACI puts each part's profile's own in place.
        let limits = cfg.run.sandbox_limits;
        for (flag, n) in [
            ("--sandbox-memory-mb", limits.memory_mb),
            ("--sandbox-cpus", limits.cpus.into()),
            ("--sandbox-pids", limits.pids.into()),
            ("--sandbox-tmp-mb", limits.tmp_mb),
        ] {
            args.extend([flag.into(), n.to_string()]);
        }
        if let Some(runtime) = &cfg.run.sandbox_runtime {
            args.extend(["--sandbox-runtime".into(), runtime.clone()]);
        }
        // `run_tests` reaches the services the gate's test steps do (#491).
        if !cfg.repo.sidecars.is_empty() {
            let file = job_dir.join("sidecars.toml");
            std::fs::write(&file, sidecars_file(cfg)?)
                .map_err(|e| LifecycleError::io(&file, &e))?;
            args.extend(["--sidecars".into(), file.display().to_string()]);
        }
    }
    let mut specs = vec![McpServerSpec {
        name: "aci".into(),
        program: cfg.aci_bin().display().to_string(),
        args,
        cwd: None,
        env: aci_env,
        tools: None,
    }];
    if let Some(cbm) = &cfg.codebase_memory {
        // One index directory for every repo: the server's daemon is one
        // per user and serves a single cache directory, so a job with
        // another one could not start while a job is running (#185). Each
        // job indexes its tree as its own project and drops it at the end
        // (`CodeGraph`). The server runs in the index directory, not in the
        // tree: the daemon the first job's server starts keeps that
        // directory as its own, and once the job's tree is gone
        // `detect_changes` can run no git command (#203).
        let index = cfg.run.state_dir.join("index");
        std::fs::create_dir_all(&index).map_err(|e| LifecycleError::io(&index, &e))?;
        code_graph::check_index_dirs(&index)?;
        let cwd = index.clone();
        let index = index.display().to_string();
        let fill = |v: &String| v.replace(INDEX_DIR, &index);
        specs.push(McpServerSpec {
            name: code_graph::SERVER.into(),
            program: cbm.program.clone(),
            args: cbm.args.iter().map(fill).collect(),
            cwd: Some(cwd),
            env: cbm.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect(),
            tools: Some(cbm.tools.clone()),
        });
    }
    Ok(specs)
}
