//! The dependency step: the patch's dependency changes checked, resolved,
//! fetched and audited (#66, #80, #159).

use super::*;

impl<'a> Run<'a> {
    pub(super) fn dependencies(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::Dependencies;
        let tree = self.tree();
        let i = self.part_index();
        let others: Vec<(&str, &Profile)> = self
            .profiles
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .map(|(j, p)| (self.dirs[j].as_str(), *p))
            .collect();
        let checked = deps::check(&deps::DependencyCheck {
            profile: &self.part.profile.profile,
            repo: self.req.repo,
            dir: &self.part.dir,
            base: &self.base,
            changes: &self.part.changes,
            ignored: &self.part.ignored,
            allowed: self.req.allowed,
            tree: tree.map_or(&[], |t| t.on_change.as_slice()),
            others: &others,
            policy: &self.req.dependency_policy,
        })?;
        Ok(match checked {
            None => StepReport::skipped(name, Skip::NoDependencyChange),
            Some(Ok(())) => self.fetch_checked(tree)?,
            Some(Err(why)) => report(name, StepStatus::Failed, &[], why),
        })
    }

    /// The patch's dependencies passed the check: compare the resolved
    /// trees when there is one (#80), fetch them through the egress proxy
    /// into the caches the later steps read (#66), then audit the changed
    /// lockfiles (#159). Without a sandbox there is nothing to fetch into,
    /// and the audit runs on the machine.
    pub(super) fn fetch_checked(
        &mut self,
        tree: Option<&DependencyTree>,
    ) -> Result<StepReport, GateError> {
        let name = StepName::Dependencies;
        let mut ran = Vec::new();
        if let Some(failed) = self.fetch(tree, &mut ran)? {
            return Ok(failed);
        }
        if let Some(failed) = self.audit(&mut ran)? {
            return Ok(failed);
        }
        let all: Vec<&Ran> = ran.iter().collect();
        Ok(report(name, StepStatus::Passed, &all, String::new()))
    }

    /// `Some` is the step's report when fetching does not pass.
    pub(super) fn fetch(
        &mut self,
        tree: Option<&DependencyTree>,
        ran: &mut Vec<Ran>,
    ) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let req: &GateRequest<'_> = self.req;
        let (Some(fetch), Some(sandbox)) = (self.part.fetch.clone(), self.part.sandbox.clone())
        else {
            return Ok(None);
        };
        let session = fetch.egress.start()?;
        if let Some(tree) = tree
            && let Some(failed) = self.compare_trees(tree, &session, &sandbox, ran)?
        {
            return Ok(Some(failed));
        }
        let fetched = session.fetch(
            &sandbox,
            &fetch.command,
            &self.part.repo,
            &self.log("fetch"),
            req.step_timeout,
            req.cancel.clone(),
        )?;
        ran.push(fetched);
        let all: Vec<&Ran> = ran.iter().collect();
        let fetched = ran.last().expect("just pushed");
        Ok(match fetched.exit {
            Exit::Cancelled => Some(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => None,
            e => Some(report(
                name,
                StepStatus::Failed,
                &all,
                format!(
                    "fetching the new dependencies failed ({}):\n{}\n\nproxy:\n{}",
                    exit_text(e),
                    tail(&fetched.log),
                    session.log()
                ),
            )),
        })
    }

    /// Lockfiles of the part's profile the patch changed and kept.
    pub(super) fn changed_lockfiles(&self) -> Vec<String> {
        let lockfiles = &self.part.profile.profile.dependencies.lockfiles;
        self.part
            .changes
            .iter()
            .filter(|c| deps::matches_any(lockfiles, &c.path))
            .filter(|c| self.part.repo.join(&c.path).is_file())
            .map(|c| c.path.clone())
            .collect()
    }

    /// The profile's `dependencies.audit` on each changed lockfile, with
    /// the repo's policy (#159). In a sandbox it runs in a container whose
    /// only way out is a proxy to `audit_hosts`. `Some` is the step's
    /// report when an audit does not pass.
    pub(super) fn audit(&mut self, ran: &mut Vec<Ran>) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let req: &GateRequest<'_> = self.req;
        let Step::Run(command) = &self.part.profile.audit else {
            return Ok(None);
        };
        let lockfiles = self.changed_lockfiles();
        if lockfiles.is_empty() {
            return Ok(None);
        }
        req.audit.check_ids().map_err(GateError::InvalidRequest)?;
        let dir = self.work.join("audit");
        std::fs::create_dir_all(&dir).map_err(|e| GateError::io(&dir, &e))?;
        let policy = &req.audit;
        for (file, text) in [
            (
                "deny.toml",
                deny_config(policy.licenses.as_deref(), &policy.ignored_advisories)?,
            ),
            ("osv-scanner.toml", osv_config(&policy.ignored_advisories)?),
        ] {
            let path = dir.join(file);
            std::fs::write(&path, text).map_err(|e| GateError::io(&path, &e))?;
        }
        let licenses = policy
            .licenses
            .clone()
            .unwrap_or_else(default_licenses)
            .join(",");
        let contained = match (&self.part.fetch, &self.part.sandbox) {
            (Some(fetch), Some(sandbox)) => {
                let egress = Egress {
                    allow: fetch.audit_hosts.clone(),
                    ..fetch.egress.clone()
                };
                let mut auditor = sandbox.clone();
                auditor.mounts.push((dir.clone(), AUDIT_DIR.to_owned()));
                Some((egress.start()?, auditor))
            }
            _ => None,
        };
        let policy_dir = match contained {
            Some(_) => AUDIT_DIR.to_owned(),
            None => dir.display().to_string(),
        };
        let (deny, osv) = (
            format!("{policy_dir}/deny.toml"),
            format!("{policy_dir}/osv-scanner.toml"),
        );
        for (i, lockfile) in lockfiles.iter().enumerate() {
            let lockfile_dir = match lockfile.rsplit_once('/') {
                Some((parent, _)) => parent,
                None => ".",
            };
            let argv = command.render(&[
                (Placeholder::Lockfile, lockfile),
                (Placeholder::LockfileDir, lockfile_dir),
                (Placeholder::Licenses, &licenses),
                (Placeholder::DenyConfig, &deny),
                (Placeholder::OsvConfig, &osv),
            ])?;
            let log = format!("audit-{i}");
            let audited = match &contained {
                Some((session, auditor)) => session.fetch(
                    auditor,
                    &argv,
                    &self.part.repo,
                    &self.log(&log),
                    req.step_timeout,
                    req.cancel.clone(),
                )?,
                None => self.run_in(&argv, false, &log)?,
            };
            let exit = audited.exit;
            ran.push(audited);
            match exit {
                Exit::Cancelled => return Ok(Some(StepReport::skipped(name, Skip::Cancelled))),
                e if e.success() => {}
                e => {
                    let last = ran.last().expect("just pushed");
                    let mut why = format!(
                        "auditing {} failed ({}): an advisory or a license outside \
                         the repo's policy, or the audit could not run:\n{}",
                        self.part.shown(lockfile),
                        exit_text(e),
                        tail(&last.log)
                    );
                    if let Some((session, _)) = &contained {
                        why.push_str(&format!("\n\nproxy:\n{}", session.log()));
                    }
                    let all: Vec<&Ran> = ran.iter().collect();
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
            }
        }
        Ok(None)
    }

    /// Resolves the dependency tree on the base and on the patch, in the
    /// fetch container, and rejects what the patch adds like a lockfile
    /// change. Resolving downloads only what the tree needs to be known
    /// (poms for Maven), not the new dependencies themselves. `Some` is
    /// the step's report when it does not pass.
    pub(super) fn compare_trees(
        &mut self,
        tree: &DependencyTree,
        session: &EgressSession,
        sandbox: &Sandbox,
        ran: &mut Vec<Ran>,
    ) -> Result<Option<StepReport>, GateError> {
        let name = StepName::Dependencies;
        let base_dir = self.base_dir()?;
        let patch_dir = self.part.repo.clone();
        let mut trees = Vec::new();
        for (side, dir) in [("base", base_dir.as_path()), ("patch", patch_dir.as_path())] {
            let out = self.work.join(format!("tree-{side}"));
            // A fresh directory: the command may append to the file.
            if out.exists() {
                std::fs::remove_dir_all(&out).map_err(|e| GateError::io(&out, &e))?;
            }
            std::fs::create_dir_all(&out).map_err(|e| GateError::io(&out, &e))?;
            let mut resolver = sandbox.clone();
            resolver.mounts.push((out.clone(), TREE_DIR.to_owned()));
            let inside = format!("{TREE_DIR}/{}", tree.file);
            let argv = tree.command.render(&[(Placeholder::Output, &inside)])?;
            let r = session.fetch(
                &resolver,
                &argv,
                dir,
                &self.log(&format!("tree-{side}")),
                self.req.step_timeout,
                self.req.cancel.clone(),
            )?;
            let exit = r.exit;
            ran.push(r);
            let all: Vec<&Ran> = ran.iter().collect();
            let which = if side == "base" {
                "on the base revision"
            } else {
                "with the patch"
            };
            match exit {
                Exit::Cancelled => return Ok(Some(StepReport::skipped(name, Skip::Cancelled))),
                e if !e.success() => {
                    let last = ran.last().expect("just pushed");
                    let why = format!(
                        "resolving the dependency tree {which} failed ({}):\n{}\n\nproxy:\n{}",
                        exit_text(e),
                        tail(&last.log),
                        session.log()
                    );
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
                _ => {}
            }
            let file = out.join(&tree.file);
            let read = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    read_lockfile(self.part.profile.profile.language, &tree.file, &text)
                        .map_err(|e| e.to_string())
                });
            match read {
                Ok(deps) => trees.push(deps),
                Err(e) => {
                    let why = format!("the dependency tree {which} cannot be read: {e}");
                    return Ok(Some(report(name, StepStatus::Failed, &all, why)));
                }
            }
        }
        let violations = deps::lock_problems(
            &self.req.dependency_policy,
            &trees[0],
            &trees[1],
            self.req.allowed,
            &self.part.profile.profile.dependencies.official_source,
        );
        if violations.is_empty() {
            return Ok(None);
        }
        let why = violations
            .iter()
            .map(|v| format!("dependency tree: {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        let all: Vec<&Ran> = ran.iter().collect();
        Ok(Some(report(name, StepStatus::Failed, &all, why)))
    }
}
