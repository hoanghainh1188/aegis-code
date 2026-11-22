//! The test step: new failures told from the base's own, reruns of
//! suspected flakes (#306) and allowed reference updates (#147).

use super::*;

impl<'a> Run<'a> {
    /// All tests; fails only on tests the base revision does not fail.
    pub(super) fn test(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::Test;
        let argv = self.part.profile.gate.test.render(&[])?;
        let patched = self.run_in(&argv, false, "test")?;
        let fail = |ran: &[&Ran], why: String| Ok(report(name, StepStatus::Failed, ran, why));
        match patched.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => {
                return Ok(report(name, StepStatus::Passed, &[&patched], String::new()));
            }
            Exit::TimedOut => return fail(&[&patched], "the tests timed out".into()),
            Exit::Signal => {
                let why = format!("the tests were killed by a signal:\n{}", tail(&patched.log));
                return fail(&[&patched], why);
            }
            e @ Exit::OutOfMemory { .. } => {
                let why = format!("the tests: {}:\n{}", exit_text(e), tail(&patched.log));
                return fail(&[&patched], why);
            }
            _ => {}
        }
        let Some(format) = self.part.profile.gate.test_output else {
            return fail(
                &[&patched],
                format!(
                    "the tests fail ({}):\n{}",
                    exit_text(patched.exit),
                    tail(&patched.log)
                ),
            );
        };
        let harness = self.part.profile.gate.harness.as_ref();
        let read = |ran: &Ran| failing_tests_with(format, harness, &read_log(&ran.log));
        let failing = read(&patched);
        if failing.is_empty() {
            return fail(
                &[&patched],
                format!(
                    "the test command failed ({}) without a failing test, e.g. a build error:\n{}",
                    exit_text(patched.exit),
                    tail(&patched.log)
                ),
            );
        }
        self.base_tree()?;
        let base = self.run_in(&argv, true, "test-base")?;
        let on_base = match base.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if e.success() => BTreeSet::new(),
            _ => {
                let on_base = read(&base);
                if on_base.is_empty() {
                    return fail(
                        &[&patched, &base],
                        format!(
                            "the tests also fail on the base revision ({}), and no failing test \
                             could be read to compare",
                            exit_text(base.exit)
                        ),
                    );
                }
                on_base
            }
        };
        let new: BTreeSet<String> = failing.difference(&on_base).cloned().collect();
        if new.is_empty() {
            let detail = format!(
                "{} tests fail, as on the base revision:\n{}",
                failing.len(),
                list(&failing)
            );
            let passed = report(name, StepStatus::Passed, &[&patched, &base], detail);
            return Ok(as_on_base(passed, true));
        }
        // A test that fails once and passes on a later run is flaky, not
        // broken by the patch (#104): the new failures run again, on their
        // own when the runner can select them (#306), and only those that
        // fail every time count.
        let Some((again, failing_again)) = self.rerun(&new, format, &argv)? else {
            return Ok(StepReport::skipped(name, Skip::Cancelled));
        };
        let (broken, flaky): (BTreeSet<String>, BTreeSet<String>) =
            new.into_iter().partition(|t| failing_again.contains(t));
        let last = again.last().expect("at least one rerun");
        let mut ran: Vec<&Ran> = vec![&patched, &base];
        ran.extend(again.iter());
        if broken.is_empty() {
            let detail = format!(
                "{} tests failed once and passed when run again (flaky):\n{}",
                flaky.len(),
                list(&flaky)
            );
            let passed = report(name, StepStatus::Passed, &ran, detail);
            return Ok(as_on_base(passed, !on_base.is_empty()));
        }
        let Some(update) = self.part.profile.gate.update_references.clone() else {
            return fail(
                &ran,
                format!(
                    "{} tests fail that pass on the base revision:\n{}\n\n{}",
                    broken.len(),
                    list(&broken),
                    tail(&last.log)
                ),
            );
        };
        // Every new failure is a test the agent says had to change, by name
        // or in a group: its reference output is rewritten, then all the
        // tests run again.
        let allowed_tests = self.part.allowed_tests.clone();
        let declared = |id: &String| allowed_tests.iter().any(|t| is_test(id, t));
        let reference_groups = self.part.reference_groups.clone();
        let (groups, ignored): (Vec<&ReferenceGroup>, Vec<&ReferenceGroup>) = reference_groups
            .iter()
            .partition(|g| g.dir_in(&update.dirs).is_some());
        let grouped = |id: &String| groups.iter().any(|g| g.covers(id));
        if broken.iter().all(|id| declared(id) || grouped(id)) {
            let mut names: Vec<String> = allowed_tests
                .iter()
                .filter(|t| broken.iter().any(|id| is_test(id, t)))
                .cloned()
                .collect();
            let in_groups: Vec<&String> = broken.iter().filter(|id| !declared(id)).collect();
            // A group's update may write only in its directory; with tests
            // declared by name too, in the profile's (#105).
            let dirs: Vec<String> = if names.is_empty() {
                groups
                    .iter()
                    .filter(|g| in_groups.iter().any(|id| g.covers(id)))
                    .filter_map(|g| g.dir_in(&update.dirs))
                    .map(str::to_owned)
                    .collect()
            } else {
                update.dirs.clone()
            };
            for id in in_groups {
                let name = harness_name(id).to_owned();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            return self.update_references(&update, &names, &dirs, &argv, &on_base, &ran);
        }
        let mut hint = String::from(
            "If the fix had to change their reference output, declare each in \
             `allowed_test_changes`, or groups of them in `allowed_reference_changes`.",
        );
        if !ignored.is_empty() {
            let dirs: Vec<&str> = ignored.iter().map(|g| g.dir.as_str()).collect();
            hint.push_str(&format!(
                " Groups ignored, their directory is not inside {}: {}.",
                quoted(&update.dirs),
                quoted(&dirs)
            ));
        }
        fail(
            &ran,
            format!(
                "{} tests fail that pass on the base revision:\n{}\n\n{hint}\n\n{}",
                broken.len(),
                list(&broken),
                tail(&last.log)
            ),
        )
    }

    /// Runs `suspects` again: alone through the profile's `repro_test` and
    /// the runner's own selector, up to `reruns` times while any still
    /// fails; otherwise, or when such a run shows it did not run them all
    /// or read no failing test, the whole suite once. The runs, and the
    /// suspects that failed in every one; `None` when cancelled.
    pub(super) fn rerun(
        &self,
        suspects: &BTreeSet<String>,
        format: TestOutput,
        test_argv: &[String],
    ) -> Result<Option<Reruns>, GateError> {
        let harness = self.part.profile.gate.harness.as_ref();
        let read = |ran: &Ran| failing_tests_with(format, harness, &read_log(&ran.log));
        let gate = &self.part.profile.gate;
        let selected = match (selector(format, suspects), gate.repro_test.command()) {
            (Some(test), Some(repro)) => Some(repro.render(&[(Placeholder::Test, &test)])?),
            _ => None,
        };
        let mut runs = Vec::new();
        let mut still = suspects.clone();
        if let Some(argv) = selected {
            for i in 1..=gate.reruns {
                let ran = self.run_in(&argv, false, &format!("test-rerun-{i}"))?;
                if ran.exit == Exit::Cancelled {
                    return Ok(None);
                }
                let (failing, unclear) = if ran.exit.success() {
                    let output = read_log(&ran.log);
                    (BTreeSet::new(), missed(format, &output, suspects))
                } else {
                    let failing = read(&ran);
                    let unclear = failing.is_empty();
                    (failing, unclear)
                };
                runs.push(ran);
                if unclear {
                    still = suspects.clone();
                    break;
                }
                still.retain(|t| failing.contains(t));
                if still.is_empty() || i == gate.reruns {
                    return Ok(Some((runs, still)));
                }
            }
        }
        let again = self.run_in(test_argv, false, "test-rerun")?;
        if again.exit == Exit::Cancelled {
            return Ok(None);
        }
        let failing = if again.exit.success() {
            BTreeSet::new()
        } else {
            read(&again)
        };
        still.retain(|t| failing.contains(t));
        runs.push(again);
        Ok(Some((runs, still)))
    }

    /// Rewrites the reference output of `names` with the profile's command,
    /// checks it wrote nothing outside `dirs`, and runs the tests once more:
    /// they must now fail only as on the base (#105).
    pub(super) fn update_references(
        &mut self,
        update: &ReferenceUpdate,
        names: &[String],
        dirs: &[String],
        test_argv: &[String],
        on_base: &BTreeSet<String>,
        earlier: &[&Ran],
    ) -> Result<StepReport, GateError> {
        let name = StepName::Test;
        let mut ran: Vec<&Ran> = earlier.to_vec();
        let pattern = exact_regex(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let argv = update.command.render(&[(Placeholder::Test, &pattern)])?;
        let updated = self.run_in(&argv, false, "update-references")?;
        match updated.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            Exit::TimedOut | Exit::Signal | Exit::OutOfMemory { .. } => {
                let why = format!(
                    "updating the reference outputs {}:\n{}",
                    exit_text(updated.exit),
                    tail(&updated.log)
                );
                ran.push(&updated);
                return Ok(report(name, StepStatus::Failed, &ran, why));
            }
            // A harness may exit non-zero after it updated what failed.
            Exit::Code(_) => {}
        }
        // Paths from the repo root; `dirs` are inside the part.
        let part_dir = self.part.dir.clone();
        let inside = |path: &str| {
            in_dir(&part_dir, path).is_some_and(|p| dirs.iter().any(|d| is_inside(p, d)))
        };
        let before: BTreeSet<String> = self.all_changes.iter().map(|c| c.path.clone()).collect();
        let changes = git::changes(self.req.repo, &self.base, &self.index)?;
        let mut written: BTreeSet<String> = self
            .snapshot
            .changed_since(self.req.repo)
            .into_iter()
            .collect();
        written.extend(
            changes
                .iter()
                .map(|c| c.path.clone())
                .filter(|p| !before.contains(p)),
        );
        let outside: BTreeSet<String> = written.iter().filter(|p| !inside(p)).cloned().collect();
        if !outside.is_empty() {
            let shown: Vec<String> = dirs.iter().map(|d| self.part.shown(d)).collect();
            let why = format!(
                "updating the reference outputs wrote outside {}:\n{}",
                shown.join(", "),
                list(&outside)
            );
            ran.push(&updated);
            return Ok(report(name, StepStatus::Failed, &ran, why));
        }
        // What is checked, and sent, now includes the new references.
        self.part.changes = parts::scoped_changes(&self.dirs, self.part_index(), &changes);
        self.all_changes = changes;
        self.snapshot = Snapshot::take(
            self.req.repo,
            &git::files(self.req.repo, &self.index)?,
            &self.coverage_files,
        );
        self.tree = git::write_tree(self.req.repo, &self.index)?;

        let after = self.run_in(test_argv, false, "test-after-update")?;
        if after.exit == Exit::Cancelled {
            return Ok(StepReport::skipped(name, Skip::Cancelled));
        }
        let format = self.part.profile.gate.test_output.expect("compared above");
        let harness = self.part.profile.gate.harness.as_ref();
        let failing = if after.exit.success() {
            BTreeSet::new()
        } else {
            failing_tests_with(format, harness, &read_log(&after.log))
        };
        let still: BTreeSet<String> = failing.difference(on_base).cloned().collect();
        let detail = format!(
            "updated the reference outputs of {} declared tests:\n{}\nfiles:\n{}",
            names.len(),
            list(&names.iter().cloned().collect()),
            list(&written)
        );
        ran.push(&updated);
        ran.push(&after);
        if still.is_empty() {
            self.references.tests.extend(names.iter().cloned());
            self.references.files.extend(written);
            let passed = report(name, StepStatus::Passed, &ran, detail);
            return Ok(as_on_base(passed, !failing.is_empty()));
        }
        Ok(report(
            name,
            StepStatus::Failed,
            &ran,
            format!(
                "{detail}\n\n{} tests still fail that pass on the base revision:\n{}\n\n{}",
                still.len(),
                list(&still),
                tail(&after.log)
            ),
        ))
    }
}
