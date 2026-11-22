//! The steps that check the patch as it is: format, lint and typecheck,
//! scope, secrets, coverage, integrity and Sonar.

use super::*;

impl<'a> Run<'a> {
    /// Format, lint, typecheck: the command exits 0, or it fails on the
    /// base revision too and the patch adds no finding in the files it
    /// changed ("no new warnings", design §6.1; #104).
    pub(super) fn simple(&mut self, name: StepName, step: &Step) -> Result<StepReport, GateError> {
        let command = match step {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let argv = command.render(&[])?;
        let ran = self.run_in(&argv, false, name.id())?;
        let failed = |ran: &[&Ran], why: String| report(name, StepStatus::Failed, ran, why);
        match ran.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => {
                return Ok(report(name, StepStatus::Passed, &[&ran], String::new()));
            }
            Exit::Code(_) => {}
            e => {
                return Ok(failed(
                    &[&ran],
                    format!("{}:\n{}", exit_text(e), tail(&ran.log)),
                ));
            }
        }
        self.base_tree()?;
        let base = self.run_in(&argv, true, &format!("{}-base", name.id()))?;
        let both = [&ran, &base];
        match base.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            Exit::Code(c) if c != 0 => {}
            _ => {
                let why = format!("{}:\n{}", exit_text(ran.exit), tail(&ran.log));
                return Ok(failed(&both, why));
            }
        }
        let changed: Vec<String> = self.part.changes.iter().map(|c| c.path.clone()).collect();
        let new = new_findings(&read_log(&ran.log), &read_log(&base.log), &changed);
        if new.is_empty() {
            let passed = report(name, StepStatus::Passed, &both, String::new());
            return Ok(as_on_base(passed, true));
        }
        let why = format!(
            "new findings in the changed files (the base revision fails too, without these):\n{}\n\n{}",
            new.join("\n"),
            tail(&ran.log)
        );
        Ok(failed(&both, why))
    }

    /// The dependency tree to resolve for this patch: the profile has one,
    /// the patch changes a file of its `on_change`, and the gate has the
    /// fetch container to resolve it in.
    pub(super) fn tree(&self) -> Option<&'a DependencyTree> {
        let profile: &'a ResolvedProfile = self.part.profile;
        let tree = profile.tree.as_ref()?;
        (self.part.fetch.is_some() && self.part.sandbox.is_some())
            .then_some(tree)
            .filter(|t| {
                self.part
                    .changes
                    .iter()
                    .any(|c| deps::matches_any(&t.on_change, &c.path))
            })
    }

    pub(super) fn scope(&self) -> Result<StepReport, GateError> {
        let name = StepName::Scope;
        let violations = analyze_scope(
            self.req.repo,
            self.req.base,
            &self.all_changes,
            &self.req.scope,
        )?;

        let detail = violations.summary();
        if self.req.scope.enforce && !violations.is_empty() {
            Ok(StepReport {
                name,
                status: StepStatus::Failed,
                duration: Duration::ZERO,
                detail: format!("patch violates scope limits: {detail}"),
                logs: vec![],
                fails_on_base: false,
            })
        } else {
            Ok(StepReport {
                name,
                status: StepStatus::Passed,
                duration: Duration::ZERO,
                detail,
                logs: vec![],
                fails_on_base: false,
            })
        }
    }

    /// The lines the whole patch adds, scanned for secrets (#509): any
    /// found fails the step, named by file, line and kind, never by value.
    pub(super) fn secrets(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::Secrets;
        let scan = scan_patch(
            self.req.repo,
            self.req.base,
            &self.all_changes,
            &self.req.secrets,
        )?;
        self.secrets_found = scan.found.len();
        let detail = scan.summary();
        let status = if scan.found.is_empty() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        };
        let detail = match status {
            StepStatus::Failed => format!(
                "the patch adds {} secret(s); remove them (read the value from the \
                 environment or a secret store instead), or, for a fake value in a test \
                 fixture, ask for a `secrets.allow` exception:\n{detail}",
                scan.found.len()
            ),
            _ => detail,
        };
        Ok(report(name, status, &[], detail))
    }

    pub(super) fn coverage(&self) -> Result<StepReport, GateError> {
        let name = StepName::Coverage;
        let command = match &self.part.profile.gate.coverage {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c,
        };
        let root = self.part.repo.ancestors().nth(depth(&self.part.dir));
        let not_for_repo = root.is_some_and(|root| {
            let skip = &self.part.profile.gate.coverage_skip;
            skip.iter().any(|m| m.matches(&self.part.repo, root))
        });
        if not_for_repo {
            return Ok(StepReport::skipped(name, Skip::NotForRepo));
        }
        let pattern = self.part.profile.coverage.as_ref().map(|c| c.path.as_str());
        if let Some(pat) = pattern {
            for old in aegis_lang::find_reports(&self.part.repo, pat) {
                std::fs::remove_file(&old).map_err(|e| GateError::io(&old, &e))?;
            }
        }
        let ran = self.run_in(&command.render(&[])?, false, name.id())?;
        let failed = |why: String| report(name, StepStatus::Failed, &[&ran], why);
        let reports = pattern.map(|pat| (pat, aegis_lang::find_reports(&self.part.repo, pat)));
        Ok(match (ran.exit, &reports) {
            (Exit::Cancelled, _) => StepReport::skipped(name, Skip::Cancelled),
            (e @ (Exit::TimedOut | Exit::Signal | Exit::OutOfMemory { .. }), _) => {
                failed(format!("{}:\n{}", exit_text(e), tail(&ran.log)))
            }
            (e, Some((pat, found))) if found.is_empty() => {
                let p = self.part.repo.join(pat);
                failed(format!(
                    "no coverage report at `{}` ({}):\n{}",
                    p.display(),
                    exit_text(e),
                    tail(&ran.log)
                ))
            }
            (_, Some((pat, found))) if found.len() > 1 => {
                let shown: Vec<String> = found
                    .iter()
                    .map(|p| {
                        p.strip_prefix(&self.part.repo)
                            .map(|rel| rel.display().to_string())
                            .unwrap_or_else(|_| p.display().to_string())
                    })
                    .collect();
                failed(format!(
                    "multiple coverage reports match `{}`: {}",
                    pat,
                    shown.join(", ")
                ))
            }
            (e, None) if !e.success() => failed(format!("{}:\n{}", exit_text(e), tail(&ran.log))),
            // The coverage run also runs the tests; their failures were
            // judged against the base by the test step (#104).
            _ => report(name, StepStatus::Passed, &[&ran], String::new()),
        })
    }

    /// None of the steps rewrote a file of the patch.
    pub(super) fn integrity(&self) -> StepReport {
        let name = StepName::Integrity;
        let changed = self.snapshot.changed_since(self.req.repo);
        if changed.is_empty() {
            return report(name, StepStatus::Passed, &[], String::new());
        }
        let changed: BTreeSet<String> = changed.into_iter().collect();
        let why = format!(
            "the steps changed files of the patch, so what was checked is not what would be \
             sent:\n{}",
            list(&changed)
        );
        report(name, StepStatus::Failed, &[], why)
    }

    pub(super) fn sonar(&self) -> Result<StepReport, GateError> {
        let Some(step) = &self.req.sonar else {
            return Ok(StepReport::skipped(StepName::Sonar, Skip::NotConfigured));
        };
        sonar::run(
            step,
            &sonar::Context {
                repo: self.req.repo,
                base: self.req.base,
                tree: &self.tree,
                profile: &self.req.profile.profile,
                properties: sonar::repo_properties(
                    all_parts(self.req)
                        .iter()
                        .map(|p| (p.dir.as_str(), &p.profile.profile)),
                ),
                runner: &self.root_runner,
                logs: &self.work.join("logs"),
            },
        )
    }
}
