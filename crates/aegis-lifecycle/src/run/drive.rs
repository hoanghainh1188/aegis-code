//! The Core/Expert loop: model turns, the final answer checked, gate runs
//! between attempts, escalation or stop.

use super::*;

/// The ledger's view of the tools of `turn` (#129).
pub(super) fn tool_calls(turn: &TurnRecord) -> Vec<ToolCall> {
    turn.tool_calls
        .iter()
        .map(|c| ToolCall {
            name: c.name.clone(),
            is_error: c.is_error,
            target: c.target.clone(),
        })
        .collect()
}

pub(super) fn model_spec(cfg: &RunConfig, tier: Tier) -> ModelSpec {
    let model = cfg.models.for_tier(tier);
    ModelSpec {
        id: model.id.clone(),
        effort: model.effort,
        max_tokens: cfg.run.max_tokens,
        context_window: cfg.run.context_window,
    }
}

/// Text of the model's last reply, only when the last message is the
/// model's: never the issue or a gate summary.
pub(super) fn last_reply(session: &Session) -> String {
    session
        .messages()
        .last()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Values the ACI server hid in the tool results of `messages`.
pub(super) fn hidden_in_tool_results(messages: &[Message]) -> u32 {
    let n = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => {
                Some(content.matches(REDACTED_MARKER).count())
            }
            _ => None,
        })
        .sum();
    count(n)
}

/// Model calls the agent needs at least to act on the code graph's note
/// before the gate: read a file, edit it, answer again, with one to spare.
pub(super) const IMPACT_CALLS: u32 = 4;

/// The job [`drive`] works on: the repo's profiles, the issue, the tree
/// the gate runs on, what to do with a patch that passes, and what the
/// patch may change in the dependencies (#500).
pub(super) struct DriveContext<'a, E> {
    pub(super) profiles: &'a RepoProfiles,
    pub(super) issue: &'a IssueInput,
    /// The job's workspace, reached through it only (#663).
    pub(super) env: &'a E,
    /// The commit the patch is checked against.
    pub(super) base: &'a str,
    pub(super) finish: &'a Finish,
    pub(super) policy: &'a GatePolicy,
    /// Where the job's checkpoints go (#662); `None`: none are kept.
    pub(super) saving: Option<checkpoint::Saving<'a>>,
}

/// Each model call of a loop, as it ends: recorded in the ledger, then
/// the job's checkpoint written (#662), so a crash loses at most the call
/// in flight.
struct Progress<'p, 'l, 's, S, E> {
    ledger: &'l mut IssueLedger<'s, S>,
    store: &'p S,
    env: &'p E,
    saving: Option<checkpoint::Saving<'p>>,
    tier: Tier,
    /// The router as the loop started, and the calls before it.
    router: Router,
    prior_calls: u32,
    /// The loop's state but for the session and the router, as it started.
    base: checkpoint::DriveState,
    dearest_call_usd: f64,
    /// The first ledger write that failed: the job stops on it after the
    /// loop, as it did when the calls were written after it.
    error: Option<LifecycleError>,
}

impl<S, E> aegis_orchestrator::TurnObserver for Progress<'_, '_, '_, S, E>
where
    S: LedgerStore + Send + Sync,
    E: ExecutionEnv,
{
    async fn turn_done(&mut self, session: &Session, turns: &[TurnRecord]) {
        let Some(turn) = turns.last() else { return };
        if self.error.is_some() {
            return;
        }
        let recorded = async {
            let cost = self
                .ledger
                .model_call(self.tier, &turn.model, &turn.usage)
                .await?;
            self.dearest_call_usd = self.dearest_call_usd.max(cost);
            self.ledger.tool_calls(self.tier, tool_calls(turn)).await?;
            Ok::<_, LifecycleError>(())
        }
        .await;
        if let Err(e) = recorded {
            self.error = Some(e);
            return;
        }
        let Some(saving) = self.saving else { return };
        let (router, decision) = fold_tools(self.router, self.prior_calls, turns);
        let pending_escalation = match decision {
            Decision::Escalate(reason) if router.tier() != self.router.tier() => {
                Some(reason.to_string())
            }
            _ => None,
        };
        let state = checkpoint::DriveState {
            session: session.clone(),
            router: router.state(),
            dearest_call_usd: self.dearest_call_usd,
            first_expert_call: self.base.first_expert_call || pending_escalation.is_some(),
            pending_escalation,
            ..self.base.clone()
        };
        let turn_no = self.ledger.record().model_calls;
        checkpoint::save(self.store, self.env, saving, turn_no, &state).await;
    }
}

/// The Core/Expert loop with gate runs between attempts. `prepared` is an
/// answer to check before any agent turn: a CVE job's upgrade (#501).
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive<P, S, T, E>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    tools: &CodeGraph<'_, T>,
    session: Session,
    context: DriveContext<'_, E>,
    prepared: Option<FinalAnswer>,
    resume: Option<checkpoint::DriveState>,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
    T: ServerCalls + Sync,
    E: ExecutionEnv,
{
    let DriveContext {
        profiles,
        issue,
        env,
        base,
        finish,
        policy,
        saving,
    } = context;
    let (tree, job_dir) = (env.root(), env.job_dir());
    let mut prepared = prepared;
    let cfg = &platform.config;
    let regional = matches!(cfg.provider, ProviderConfig::Bedrock { .. });
    let limits = cfg.budget_limits(&platform.profiles)?;
    let mut router = Router::new(cfg.budget.rules());
    let mut session = session;
    let mut answer_retries = 0;
    let mut dearest_call_usd: f64 = 0.0;
    let mut first_expert_call = false;
    let mut impact_checks: usize = 0;
    let mut known_functions: BTreeSet<String> = BTreeSet::new();
    // A resumed job (#662): where its checkpoint left it. When the model
    // had given its answer, it is read again instead of asked for.
    let mut reread = false;
    if let Some(state) = resume {
        if let Some(reason) = &state.pending_escalation {
            let calls = ledger.record().model_calls;
            ledger.escalated(reason.clone(), calls).await?;
        }
        router = Router::resumed(cfg.budget.rules(), state.router);
        reread = state
            .session
            .messages()
            .last()
            .is_some_and(|m| m.role == Role::Assistant);
        session = state.session;
        answer_retries = state.answer_retries;
        dearest_call_usd = state.dearest_call_usd;
        first_expert_call = state.first_expert_call;
        impact_checks = state.impact_checks;
        known_functions = state.known_functions;
    }
    'turns: loop {
        let answer = 'answer: {
            if let Some(answer) = prepared.take() {
                let (summary, hidden) = summary_of(&answer);
                ledger.secrets_redacted(hidden).await?;
                ledger.answered(summary).await?;
                break 'answer answer;
            }
            let reply = if std::mem::take(&mut reread) {
                last_reply(&session)
            } else {
                let tier = router.tier();
                let mut budget = BudgetCheck::new(
                    Arc::new(platform.prices.clone()),
                    regional,
                    limits,
                    tier,
                    Spent::from_record(ledger.record()),
                );
                if std::mem::take(&mut first_expert_call) && dearest_call_usd > 0.0 {
                    // New model, cold cache: the first Expert call costs more.
                    budget = budget.with_first_call_estimate(dearest_call_usd * 3.0);
                }
                let check =
                    JobCheck::new(budget, router, cancel.clone()).with_impact_checks(impact_checks);
                let seen = session.messages().len();
                let model = model_spec(cfg, tier);
                let prior_calls = check.budget.model_calls(&[]);
                let mut progress = Progress {
                    ledger: &mut *ledger,
                    store: &platform.store,
                    env,
                    saving,
                    tier,
                    router,
                    prior_calls,
                    base: checkpoint::DriveState {
                        session: Session::default(),
                        router: router.state(),
                        answer_retries,
                        dearest_call_usd,
                        first_expert_call,
                        impact_checks,
                        known_functions: known_functions.clone(),
                        pending_escalation: None,
                    },
                    dearest_call_usd,
                    error: None,
                };
                let outcome = run_loop_observed(
                    &platform.provider,
                    tools,
                    &check,
                    &model,
                    session,
                    &LoopConfig::default(),
                    &mut progress,
                )
                .await;
                dearest_call_usd = progress.dearest_call_usd;
                if let Some(e) = progress.error {
                    return Err(e);
                }
                let (after_edits, decision) = fold_tools(router, prior_calls, &outcome.turns);
                if after_edits.tier() != router.tier() {
                    // Escalated on the loop's last turn: Expert takes over from here.
                    first_expert_call = true;
                    if let Decision::Escalate(reason) = decision {
                        let calls = ledger.record().model_calls;
                        ledger.escalated(reason.to_string(), calls).await?;
                    }
                }
                router = after_edits;
                session = outcome.session;
                ledger
                    .secrets_redacted(hidden_in_tool_results(&session.messages()[seen..]))
                    .await?;

                match outcome.end {
                    LoopEnd::ProviderFailed(e) => {
                        return Ok(Ending::stop(
                            Outcome::StoppedSteps,
                            format!("model provider failed: {e}"),
                        ));
                    }
                    LoopEnd::Stopped { reason } => match check.halt() {
                        Some(Halt::Cancelled) => {
                            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
                        }
                        Some(Halt::Budget(stop)) => {
                            return Ok(Ending::stop(stop.outcome(), stop.to_string()));
                        }
                        Some(Halt::RemindTest { calls, last_test }) => {
                            // The router counts the reminder, so the next one comes
                            // only after more calls without a test run (#144, #156).
                            let (next, decision) = router.observe(Signal::ModelCalls { calls });
                            debug_assert_eq!(decision, Decision::RemindTest { calls, last_test });
                            router = next;
                            ledger.test_reminded(tier, calls).await?;
                            session = session.with_user_text(test_reminder(calls, last_test));
                            continue 'turns;
                        }
                        Some(Halt::Escalate(reason)) => {
                            if router.tier() == Tier::Core {
                                // Not an edit rule, which `fold_tools` already applied:
                                // Core used its calls without a patch.
                                let calls = ledger.record().model_calls;
                                let (next, decision) = router.observe(Signal::ModelCalls { calls });
                                debug_assert_eq!(decision, Decision::Escalate(reason));
                                router = next;
                                ledger.escalated(reason.to_string(), calls).await?;
                            }
                            first_expert_call = true;
                            continue 'turns;
                        }
                        Some(Halt::CheckImpact) => {
                            let record = ledger.record();
                            let calls_left =
                                limits.max_model_calls.saturating_sub(record.model_calls);
                            let usd_left = cfg.budget.max_cost_usd - record.cost_usd;
                            let room = calls_left >= IMPACT_CALLS
                                && usd_left >= dearest_call_usd * f64::from(IMPACT_CALLS);
                            if !room {
                                // Not enough budget room to act on impact notes; stop checking.
                                impact_checks = 2;
                                continue 'turns;
                            }
                            let current_functions = changed_functions(tree, base);
                            if impact_checks == 0 || !current_functions.is_subset(&known_functions)
                            {
                                known_functions = current_functions;
                                impact_checks += 1;
                                if let Some(impact) = tools.impact(base).await {
                                    match impact {
                                        Ok(impact) => {
                                            ledger.impact_checked(impact.files(), None).await?;
                                            if let Some(note) = impact.note() {
                                                session = session.with_user_text(note);
                                            }
                                        }
                                        Err(why) => {
                                            ledger.impact_checked(Vec::new(), Some(why)).await?
                                        }
                                    }
                                }
                            }
                            continue 'turns;
                        }
                        None => return Ok(Ending::stop(Outcome::StoppedSteps, reason)),
                    },
                    LoopEnd::Finished { .. } => last_reply(&session),
                }
            };

            match parse_answer(&reply) {
                Ok(answer) => {
                    // What the agent said it did, for the job's page (#316).
                    let (summary, hidden) = summary_of(&answer);
                    ledger.secrets_redacted(hidden).await?;
                    ledger.answered(summary).await?;
                    if answer.status == Status::Fixed {
                        // Its text is checked as its code will be (#508).
                        let check = check_sources(tree, base, &answer);
                        let retry = !check.holds() && answer_retries < MAX_ANSWER_RETRIES;
                        let held = !check.holds() && !retry;
                        ledger
                            .sources_checked(count(check.cited), count(check.wrong.len()), held)
                            .await?;
                        let wrong = check
                            .wrong
                            .iter()
                            .take(MAX_SOURCE_COMPLAINTS)
                            .map(|w| format!("- {w}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if retry {
                            answer_retries += 1;
                            session = session.with_message(Message::user_text(format!(
                                "Your final answer cites places that could not be checked in \
                                 the repository:\n{wrong}\nEvery `changes[].location` and \
                                 `evidence[].location` must be a file of the repository (as \
                                 patched, or as it was), with lines inside it, and an \
                                 `excerpt` must be text found at those lines. Open the files \
                                 again if needed, then reply with only the corrected JSON \
                                 object."
                            )));
                            continue 'turns;
                        }
                        if held {
                            return Ok(Ending::stop(
                                Outcome::StoppedSteps,
                                format!("the answer has no verifiable sources:\n{wrong}"),
                            ));
                        }
                    }
                    answer
                }
                Err(why) if answer_retries < MAX_ANSWER_RETRIES => {
                    answer_retries += 1;
                    session = session.with_message(Message::user_text(format!(
                        "Your final answer could not be read ({why}). Reply with only the JSON \
                     object described in the instructions."
                    )));
                    continue 'turns;
                }
                Err(why) => {
                    return Ok(Ending::stop(
                        Outcome::StoppedSteps,
                        format!("no usable final answer: {why}"),
                    ));
                }
            }
        };
        if answer.status == Status::Stopped {
            let why = answer
                .stop_reason
                .unwrap_or_else(|| "the agent stopped".into());
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                format!("agent stopped: {why}"),
            ));
        }

        let gate_started = Instant::now();
        let report = env
            .run_gate(cfg, profiles, base, &answer, policy, cancel)
            .await?;
        ledger
            .gate_run(gate_run(&report, gate_started.elapsed()))
            .await?;
        // The patch does not reach a PR; the agent is told where (#509).
        ledger
            .secrets_blocked("patch", count(report.secrets_found))
            .await?;
        if report.cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        if report.passed() {
            let (_, decision) = router.observe(Signal::GatePassed);
            debug_assert_eq!(decision, Decision::OpenPr);
            if let Finish::Keep(author) = finish {
                let commit = commit_tree(
                    &cfg.repo.path,
                    &report.tree,
                    &report.base,
                    &format!("Sonar group {}", issue.id),
                    author,
                )?;
                return Ok(Ending {
                    outcome: Outcome::PrOpened,
                    reason: None,
                    pr_url: None,
                    kept: Some(Kept { commit, answer }),
                    patch: None,
                    diagnosis: None,
                    plan: None,
                });
            }
            // What the job read that speaks to an agent, for the reviewer
            // (#444): in the ledger, and in the PR it opens.
            let round = match finish {
                Finish::Update(round) => Some(round.as_ref()),
                _ => None,
            };
            let flags = flag_inputs(platform, ledger, issue, round, &report.base).await?;
            // The PR as the gate passed it, for the job's page (#312).
            let patch = Some(job::patch(&cfg.repo.path, &report.base, &report.tree)?);
            // Without `open_prs` (an eval's local round, #439) a round ends
            // like any run that opens no PR: its patch kept, hidden tests run.
            if let Finish::Update(round) = finish
                && cfg.run.open_prs
            {
                let published =
                    publish_round(platform, ledger, tree, issue, round, &answer, &report).await?;
                return Ok(Ending {
                    outcome: Outcome::PrOpened,
                    reason: published.note,
                    pr_url: Some(published.url),
                    kept: None,
                    patch,
                    diagnosis: None,
                    plan: None,
                });
            }
            // A part of an integration PR (#678): merged into the
            // feature's branch, no PR of its own.
            if let Finish::Feature(task) = finish
                && task.integration_branch.is_some()
            {
                let part = crate::feature_integrate::Part {
                    profiles,
                    issue,
                    task,
                    answer: &answer,
                    report: &report,
                    policy,
                    job_dir,
                };
                return crate::feature_integrate::integrate(platform, ledger, part, cancel).await;
            }
            let pr_url = if cfg.run.open_prs {
                let fix = PublishContext {
                    profiles,
                    tree,
                    issue,
                    answer: &answer,
                    gate: &report,
                    flags: &flags,
                    feature: match finish {
                        Finish::Feature(task) => Some(task.as_ref()),
                        _ => None,
                    },
                    whole: None,
                };
                let published = publish(platform, ledger, fix).await?;
                Some(published.url)
            } else {
                keep_patch(cfg, job_dir, &report.base, &report.tree)?;
                None
            };
            if let Some(hidden) = &issue.hidden {
                let part = profiles
                    .parts()
                    .iter()
                    .find(|p| p.dir == hidden.dir)
                    .ok_or_else(|| {
                        LifecycleError::Config(format!(
                            "the hidden tests are in `{}/`, which has no profile",
                            hidden.dir
                        ))
                    })?;
                let (passed, took) = run_hidden(cfg, part, tree, job_dir, hidden, cancel).await?;
                ledger.hidden_tests(passed, took.as_secs_f64()).await?;
            }
            // With `open_prs` off the run ends where it would open the PR.
            let Some(pr_url) = pr_url else {
                // A shadow job keeps what its PR is opened from later,
                // without the agent (#699): the answer and the patch's base.
                // A bug fix's only: a CVE upgrade or a feature's part is
                // gated under a policy the kept answer does not hold.
                if cfg.run.shadow && matches!(finish, Finish::Publish) && issue.cve.is_none() {
                    let kept = aegis_ledger::ShadowKept {
                        answer: redact(&answer.to_reply()).text,
                        base: report.base.clone(),
                    };
                    ledger.shadow_kept(kept).await?;
                }
                return Ok(Ending::no_pr(cfg, patch));
            };
            return Ok(Ending {
                outcome: Outcome::PrOpened,
                reason: None,
                pr_url: Some(pr_url),
                kept: None,
                patch,
                diagnosis: None,
                plan: None,
            });
        }

        // Only a repro step that ran tells whether the patch fixed the issue;
        // one skipped after an earlier failure says nothing.
        let repro_ran = report
            .step(StepName::ReproTest)
            .map(|s| &s.status)
            .filter(|s| matches!(s, StepStatus::Passed | StepStatus::Failed));
        let (next, decision) = match repro_ran {
            Some(status) => router.observe(Signal::PatchTested {
                repro_passes: *status == StepStatus::Passed,
            }),
            None => (router, Decision::Continue),
        };
        router = next;
        let decision = match decision {
            Decision::Continue => {
                let (next, decision) = router.observe(Signal::GateFailed);
                router = next;
                decision
            }
            other => other,
        };
        if let Decision::Stop(reason) = decision {
            return Ok(Ending::stop(reason.outcome(), reason.to_string()));
        }
        if let Decision::Escalate(reason) = decision {
            first_expert_call = true;
            let calls = ledger.record().model_calls;
            ledger.escalated(reason.to_string(), calls).await?;
        }
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        let gate_text = redact(&report.to_string());
        ledger.secrets_redacted(count(gate_text.count())).await?;
        let (summary, response) =
            light::summarize_gate(&platform.provider, &cfg.models.light, &gate_text.text).await;
        if let Some(r) = &response {
            ledger
                .model_call(Tier::Light, &cfg.models.light.id, &r.usage)
                .await?;
        }
        session = session.with_message(Message::user_text(format!(
            "The checks failed on your patch:\n\n{summary}\n\nFix the problem with the tools, \
             then reply with the final JSON answer again."
        )));
        // The gate's verdict is kept: a crash before the next call does
        // not run it again (#662).
        if let Some(saving) = saving {
            let state = checkpoint::DriveState {
                session: session.clone(),
                router: router.state(),
                answer_retries,
                dearest_call_usd,
                first_expert_call,
                impact_checks,
                known_functions: known_functions.clone(),
                pending_escalation: None,
            };
            let turn = ledger.record().model_calls;
            checkpoint::save(&platform.store, env, saving, turn, &state).await;
        }
    }
}
