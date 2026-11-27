//! A job's start: its fresh tree, the module map and the conventions the
//! agent is told, the tools set up, then the work loop.

use super::*;

pub(super) async fn prepare_and_work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    if cancel.is_cancelled() {
        return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
    }
    // A job that stopped mid-way (a crash, a worker released) goes on
    // from its checkpoint (#662): its issue was read, classified and
    // counted already.
    if issue.cve.is_none() {
        let record = ledger.record();
        let (id, calls) = (record.issue_id.clone(), record.model_calls);
        if let Some(resume) = checkpoint::load(&platform.store, &id, &cfg.repo.path, calls).await {
            let issue = &IssueInput {
                title: redact(&issue.title).text,
                body: redact(&issue.body).text,
                ..issue.clone()
            };
            let profiles = cfg.repo.profiles(&platform.profiles)?;
            return work_from(
                platform,
                ledger,
                &profiles,
                issue,
                String::new(),
                &Finish::Publish,
                Some(resume),
                cancel,
            )
            .await;
        }
    }
    // Issue text goes to the model too (design §7).
    let (title, body) = (redact(&issue.title), redact(&issue.body));
    ledger
        .secrets_redacted(count(title.count() + body.count()))
        .await?;
    // What the job works on, for its page (#316).
    ledger.issue_read(&title.text, &issue.url).await?;
    ledger.ticket_meta(ticket_meta(cfg, issue)).await?;
    let issue = &IssueInput {
        title: title.text,
        body: body.text,
        ..issue.clone()
    };
    // A CVE job (#501): nothing for Light to classify; the platform's
    // upgrade comes first, in `work`.
    if let Some(target) = &issue.cve {
        target.check().map_err(LifecycleError::Config)?;
        let profiles = cfg.repo.profiles(&platform.profiles)?;
        return work(
            platform,
            ledger,
            &profiles,
            issue,
            String::new(),
            &Finish::Publish,
            cancel,
        )
        .await;
    }
    // An error spike (#706): diagnosed whatever the repo's triage lane.
    if issue.kind == Some(IssueKind::Anomaly) {
        return crate::anomaly::diagnose_spike(platform, ledger, issue).await;
    }
    // The repo's deprecation warnings (#705): listed at the base, fixed as
    // a batch.
    if issue.kind == Some(IssueKind::Deprecation) {
        let profiles = cfg.repo.profiles(&platform.profiles)?;
        return super::deprecations::deprecation_batch(platform, ledger, &profiles, issue, cancel)
            .await;
    }
    let (class, response) = match issue.kind {
        // Its source knows: nothing for Light to decide.
        Some(kind) => (
            Classification {
                kind,
                // A failed CI run's log is its reproduction (#250).
                has_repro_info: kind == IssueKind::CiFailure,
                ..light::default_classification(issue)
            },
            None,
        ),
        None => light::classify(&platform.provider, &cfg.models.light, issue).await,
    };
    if let Some(r) = &response {
        ledger
            .model_call(Tier::Light, &cfg.models.light.id, &r.usage)
            .await?;
    }
    // A feature (#677): its source says so, or Light triaged it as one
    // for a repo that turned the feature lane on. Light's own `kind` never
    // names a feature (#675).
    let feature = issue.kind == Some(IssueKind::Feature)
        || class
            .triage
            .as_ref()
            .is_some_and(|t| cfg.repo.feature_lane.takes(t));
    let kind = if feature {
        IssueKind::Feature
    } else {
        class.kind
    };
    ledger.classified(kind).await?;
    // Recorded (#497); the job works on the code whatever it says, unless
    // the repo turned the diagnosis lane on for it (#451).
    if let Some(triage) = class.triage.clone() {
        ledger.triaged(triage.clone()).await?;
        if !feature && cfg.repo.triage_lane.takes(&triage) {
            return crate::diagnose::diagnose(platform, ledger, issue, &triage).await;
        }
    }
    // Before any code is written: the plan, for a person to approve.
    if feature {
        return crate::feature::architect(platform, ledger, issue).await;
    }
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    if class.needs_sonar_batching() {
        return sonar_batch(platform, ledger, &profiles, issue, cancel).await;
    }
    let first = issue.first_message(&class.summary, class.has_repro_info);
    work(
        platform,
        ledger,
        &profiles,
        issue,
        first,
        &Finish::Publish,
        cancel,
    )
    .await
}

/// A feature's sub-job (#678): no triage, the ticket's; it starts from the
/// branch of the sub-job it is stacked on, fetched from the fork, or from
/// the main branch, and is told its part of the plan.
pub(super) async fn sub_job_and_work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    task: crate::FeatureTask,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    if cancel.is_cancelled() {
        return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
    }
    let finish = Finish::Feature(Box::new(task));
    let Finish::Feature(task) = &finish else {
        unreachable!("just made")
    };
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    // Gone on from its checkpoint (#662), from the commit it started at.
    let record = ledger.record();
    let (id, calls) = (record.issue_id.clone(), record.model_calls);
    if let Some(resume) = checkpoint::load(&platform.store, &id, &cfg.repo.path, calls).await {
        let issue = &IssueInput {
            title: redact(&issue.title).text,
            body: redact(&issue.body).text,
            ..issue.clone()
        };
        return work_from(
            platform,
            ledger,
            &profiles,
            issue,
            String::new(),
            &finish,
            Some(resume),
            cancel,
        )
        .await;
    }
    let (title, body) = (redact(&issue.title), redact(&issue.body));
    ledger
        .secrets_redacted(count(title.count() + body.count()))
        .await?;
    ledger.issue_read(&title.text, &issue.url).await?;
    ledger.ticket_meta(ticket_meta(cfg, issue)).await?;
    ledger.classified(IssueKind::Feature).await?;
    let first = match task.first_message(&IssueInput {
        title: title.text.clone(),
        body: body.text.clone(),
        ..issue.clone()
    }) {
        Ok(first) => first,
        Err(why) => return Ok(Ending::stop(Outcome::StoppedSteps, why)),
    };
    let base = match (&task.stacked_on, &task.integration_branch) {
        (Some(on), _) => Some(fetch_branch(platform, &on.branch, &issue.id).await?),
        // The integration branch as it is, with every part this one
        // depends on; none yet: the main branch.
        (None, Some(branch)) => fetch_branch(platform, branch, &issue.id).await.ok(),
        (None, None) => issue.base.clone(),
    };
    let issue = &IssueInput {
        title: title.text,
        body: body.text,
        base,
        ..issue.clone()
    };
    work(platform, ledger, &profiles, issue, first, &finish, cancel).await
}

/// `branch` of the fork as it is now, fetched into the clone under
/// `refs/aegis/feature/<id>`: its head commit.
pub(crate) async fn fetch_branch<P, S>(
    platform: &Platform<P, S>,
    branch: &str,
    id: &str,
) -> Result<String, LifecycleError> {
    let cfg = &platform.config;
    let remote = fork_remote(platform).await?;
    let clone = cfg.repo.path.clone();
    let branch = branch.to_owned();
    let url = remote.url().to_owned();
    let auth = remote
        .credentials()
        .filter(|(_, t)| !t.is_empty())
        .map(|(u, t)| (u, t.to_owned()));
    let into = format!("feature/{id}");
    tokio::task::spawn_blocking(move || {
        let source = crate::clone::Source {
            url: &url,
            token: auth.as_ref().map(|(_, t)| t.as_str()),
            user: auth.as_ref().map_or(aegis_pr::GITHUB_USER, |(u, _)| u),
        };
        crate::clone::fetch_ref(&clone, source, &branch, &into)
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("fetching the branch it goes on: {e}")))?
}

/// Steps 2–10 on the job's own worktree.
pub(crate) async fn work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profiles: &RepoProfiles,
    issue: &IssueInput,
    first_message: String,
    finish: &Finish,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    work_from(
        platform,
        ledger,
        profiles,
        issue,
        first_message,
        finish,
        None,
        cancel,
    )
    .await
}

/// [`work`], from `resume` when given (#662): a fresh tree at the
/// checkpoint's commit with its changes applied, its session and state.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn work_from<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profiles: &RepoProfiles,
    issue: &IssueInput,
    first_message: String,
    finish: &Finish,
    resume: Option<checkpoint::Resume>,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    // A review round starts from the PR's head and is still checked
    // against the PR's base (#243).
    let (base, start) = match (&resume, finish) {
        (Some(r), _) => (r.base.clone(), r.start.clone()),
        (None, Finish::Update(round)) => (
            round.base(&cfg.repo.path, &cfg.repo.base)?,
            round.pull.head_sha.clone(),
        ),
        (None, _) => {
            let base = job::resolve(
                &cfg.repo.path,
                issue.base.as_deref().unwrap_or(&cfg.repo.base),
            )?;
            (base.clone(), base)
        }
    };
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    // The job's workspace (#663): everything below reaches it through
    // `env`. A job that goes on after its worker failed asks the pool for
    // the pod it had (#665).
    let previous = resume.as_ref().and_then(|r| r.pod.clone());
    let env = JobEnv::create(cfg, &start, &job_dir, &issue.id, previous.as_deref()).await?;
    let job = JobStart {
        profiles,
        issue,
        first_message,
        finish,
        resume,
        base: &base,
        start: &start,
    };
    let result = work_in(platform, ledger, &env, job, cancel).await;
    // However the job ended: its pod goes back to the pool.
    env.release(cfg, &issue.id).await;
    result
}

/// What [`work_in`] starts the job from.
struct JobStart<'a> {
    profiles: &'a RepoProfiles,
    issue: &'a IssueInput,
    first_message: String,
    finish: &'a Finish,
    resume: Option<checkpoint::Resume>,
    base: &'a str,
    start: &'a str,
}

/// [`work_from`] in the job's environment `env`.
async fn work_in<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    env: &JobEnv,
    job: JobStart<'_>,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    let JobStart {
        profiles,
        issue,
        first_message,
        finish,
        resume,
        base,
        start,
    } = job;
    let (resumed, system) = match resume {
        Some(r) => {
            env.apply_diff(&r.diff).await?;
            tracing::info!(issue = %issue.id, turn = r.turn, "resuming the job from its checkpoint");
            // The session keeps its own system blocks: the same bytes.
            (Some(r.state), Vec::new())
        }
        None => {
            let map = module_map(cfg, profiles, env.root(), base)?;
            let conventions = language_conventions(profiles);
            let system = system_blocks(&RepoPrompt {
                language_conventions: conventions.as_deref(),
                repo_conventions: cfg.repo.conventions.as_deref(),
                module_summary: Some(&map),
            });
            (None, system)
        }
    };

    // A fresh worker or execution host starts from the store's caches
    // (#665), before the fetch and the build cache seed the tree.
    let remote = super::remote_cache::for_run(cfg).await;
    if let Some(store) = &remote {
        store.pull_parts(cfg, profiles, env.job_dir()).await;
    }
    if let Some(why) = env.prepare_parts(cfg, profiles, cancel).await? {
        return Ok(Ending::stop(Outcome::StoppedSteps, why));
    }
    // A CVE job (#501, ADR 0015): the platform raises the package; the
    // gate checks that first, and the agent works only if it fails. A
    // resumed job's upgrade, if any, is in its tree already.
    let (dependencies, prepared, first_message) =
        match issue.cve.as_ref().filter(|_| resumed.is_none()) {
            // A feature's sub-job (#678): the packages its plan proposed, and
            // its acceptance tests.
            None => match finish {
                Finish::Feature(task) => (
                    DependencyPolicy::AllowProposed(task.proposed()),
                    None,
                    first_message,
                ),
                _ => (DependencyPolicy::Locked, None, first_message),
            },
            Some(target) => {
                let root = &profiles.parts()[0];
                let upgrade =
                    match cve::upgrade(cfg, root, env.root(), env.job_dir(), target, cancel).await?
                    {
                        Ok(upgrade) => upgrade,
                        Err(why) => return Ok(Ending::stop(Outcome::StoppedSteps, why)),
                    };
                let message = cve::first_message(target, &upgrade);
                let answer = cve::answer(target, &upgrade);
                (DependencyPolicy::Upgrade(upgrade), Some(answer), message)
            }
        };
    let policy = GatePolicy {
        dependencies,
        tests: match finish {
            Finish::Feature(_) => aegis_gate::TestMode::Acceptance,
            _ => aegis_gate::TestMode::Reproduce,
        },
    };
    let first_message = first_message + &crate::language::directive(cfg.repo.locale);
    let tools = McpTools::connect(&env.tool_servers(cfg, profiles)?).await?;
    let queries = cfg.codebase_memory.as_ref().map(|c| c.tools.as_slice());
    let graph = match CodeGraph::open(&tools, queries, env.root()).await {
        Ok(graph) => graph,
        Err(e) => {
            tools.shutdown().await;
            return Err(e);
        }
    };
    let session = Session::new(
        system,
        Message::user_text(first_message),
        CacheTtl::FiveMinutes,
    );
    let issue_id = ledger.record().issue_id.clone();
    // Checkpoints only where a job can go on from one (see `checkpoint`).
    let saving = (matches!(finish, Finish::Publish | Finish::Feature(_)) && issue.cve.is_none())
        .then_some(checkpoint::Saving {
            issue: &issue_id,
            base,
            start,
        });
    let context = DriveContext {
        profiles,
        issue,
        env,
        base,
        finish,
        policy: &policy,
        saving,
    };
    let result = drive(
        platform, ledger, &graph, session, context, prepared, resumed, cancel,
    )
    .await;
    graph.close().await;
    tools.shutdown().await;
    // What the gate's base copy refreshed goes up for the next host.
    if let Some(store) = &remote {
        store.push_parts(cfg, profiles, env.job_dir()).await;
    }
    result
}

/// The module map of the repo at `base`, made from the job's fresh
/// checkout `tree` and kept under the state dir by the digest of the files
/// the profile reads. BP2 is the same for every issue at the same files
/// (design §4.4); a new base that changes one of them makes a new map
/// (#24).
pub(crate) fn module_map(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    base: &str,
) -> Result<String, LifecycleError> {
    let digest = job::files_digest(&cfg.repo.path, base, |p| {
        profiles.owns_file(&p.to_string_lossy())
    })?;
    let checkout = Workspace::new(tree, profiles.clone())?;
    let map_path = cfg
        .run
        .state_dir
        .join("maps")
        .join(&cfg.repo.key)
        .join(format!("{digest}.v{MAP_FORMAT}.md"));
    Ok(load_or_create_summary(&checkout, &map_path)?)
}

/// The profile's conventions for the system prompt; with directories of
/// other languages (#71), each language's, and where each goes.
pub(super) fn language_conventions(profiles: &RepoProfiles) -> Option<String> {
    let parts = profiles.parts();
    if parts.len() == 1 {
        return profiles.root().profile.conventions().map(str::to_owned);
    }
    let mut text = String::from(
        "# Languages of this repo\n\nThis repo has more than one language; each directory below \
         goes by its own conventions and checks, the rest of the repo by the first:\n",
    );
    for part in parts {
        text.push_str(&format!("- {}\n", part.label()));
    }
    text.push_str(
        "\nName files, and tests given to `run_tests` or as `repro_test`, by their path from \
         the repo root (`web/tests/cart.test.ts`): a test is then run with its directory's \
         command, in that directory.\n",
    );
    let mut seen = Vec::new();
    for part in parts {
        let language = part.profile.language();
        if seen.contains(&language) {
            continue;
        }
        seen.push(language);
        if let Some(c) = part.profile.profile.conventions() {
            text.push('\n');
            text.push_str(c.trim_end());
            text.push('\n');
        }
    }
    Some(text)
}
