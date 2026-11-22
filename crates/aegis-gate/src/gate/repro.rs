//! The reproduction test step: it passes with the patch and fails on the
//! base revision.

use super::*;

impl<'a> Run<'a> {
    /// The reproduction test passes with the patch and fails on the base
    /// revision with only its own files copied in.
    pub(super) fn repro(&mut self) -> Result<StepReport, GateError> {
        let name = StepName::ReproTest;
        let command = match &self.part.profile.gate.repro_test {
            Step::Skipped(reason) => return Ok(StepReport::skipped(name, skip_of(*reason))),
            Step::Run(c) => c.clone(),
        };
        let Some(repro) = self.part.repro.clone() else {
            return Ok(StepReport::skipped(name, Skip::NoReproTest));
        };
        let argv = command.render(&[(Placeholder::Test, &repro.test)])?;
        let patched = self.run_in(&argv, false, "repro_test")?;
        match patched.exit {
            Exit::Cancelled => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            e if !e.success() => {
                let detail = format!(
                    "`{}` does not pass with the patch ({}):\n{}",
                    repro.test,
                    exit_text(e),
                    tail(&patched.log)
                );
                return Ok(report(name, StepStatus::Failed, &[&patched], detail));
            }
            _ => {}
        }
        // A runner that exits 0 when the id selects nothing would pass on
        // the base too and be refused there for the wrong reason (#235).
        if self
            .part
            .profile
            .gate
            .repro_ran_nothing(&read_log(&patched.log))
        {
            let detail = format!(
                "`{}` ran no test with the patch: no test has that id. Give the id in the form \
                 this language's test command takes, as its conventions say.",
                repro.test
            );
            return Ok(report(name, StepStatus::Failed, &[&patched], detail));
        }
        let repo = self
            .part
            .repo
            .canonicalize()
            .map_err(|e| GateError::io(&self.part.repo, &e))?;
        let sources = repro
            .files
            .iter()
            .map(|f| regular_file_in(&repo, f))
            .collect::<Result<Vec<_>, _>>()?;
        let base_dir = self.base_dir()?;
        // Files the base has and the patch changes: the copy brings every
        // change of the patch in them, a fix there too (#140).
        let carried: Vec<&str> = repro
            .files
            .iter()
            .filter(|f| base_dir.join(f).exists())
            .filter(|f| self.part.changes.iter().any(|c| &c.path == *f))
            .map(String::as_str)
            .collect();
        for (file, from) in repro.files.iter().zip(&sources) {
            let to = base_dir.join(file);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| GateError::io(parent, &e))?;
            }
            std::fs::copy(from, &to).map_err(|e| GateError::io(from, &e))?;
        }
        let base = self.run_in(&argv, true, "repro_test-base");
        if let Some(tree) = &self.base_tree {
            let files: Vec<String> = repro.files.iter().map(|f| self.part.shown(f)).collect();
            tree.restore(self.req.repo, &self.base, &files)?;
        }
        let base = base?;
        let ran = [&patched, &base];
        let harness = self.part.profile.gate.harness.as_ref();
        let failed_on_base = match (base.exit, self.part.profile.gate.test_output) {
            (Exit::Cancelled, _) => return Ok(StepReport::skipped(name, Skip::Cancelled)),
            (Exit::TimedOut, _) => {
                let why = format!("`{}` timed out on the base revision", repro.test);
                return Ok(report(name, StepStatus::Failed, &ran, why));
            }
            (e, _) if e.success() => false,
            // The test itself must fail there, not the build around it.
            (_, Some(format)) => failing_tests_with(format, harness, &read_log(&base.log))
                .iter()
                .any(|id| names_test(format, id, &repro.test)),
            (e, None) => matches!(e, Exit::Code(_)),
        };
        if failed_on_base {
            return Ok(report(name, StepStatus::Passed, &ran, String::new()));
        }
        let reported = |r: &Ran| {
            let format = self.part.profile.gate.test_output?;
            let ids = ran_tests(format, &read_log(&r.log))?;
            Some(ids.iter().any(|id| is_in(id, &repro.test)))
        };
        let why = if base.exit.success()
            && reported(&patched) == Some(true)
            && reported(&base) == Some(false)
        {
            format!(
                "`{}` did not run on the base revision: no test by that name is there. Only the \
                 files in `repro_test.files` ({}) are copied onto the base; a test written inside \
                 a source file the fix changes is not there. Put the reproduction test in a file \
                 that holds only tests and list that file.",
                repro.test,
                quoted(&repro.files)
            )
        } else if base.exit.success() {
            let mut why = format!(
                "`{}` also passes on the base revision, without the fix: it does not reproduce \
                 the issue",
                repro.test
            );
            if !carried.is_empty() {
                why.push_str(&format!(
                    ". The patch also changes {}, copied onto the base with the test: if the fix \
                     is in there, it was copied too. Put the reproduction test in a file that \
                     holds only tests.",
                    quoted(&carried)
                ));
            }
            why
        } else {
            format!(
                "`{}` did not run as a failing test on the base revision ({}):\n{}",
                repro.test,
                exit_text(base.exit),
                tail(&base.log)
            )
        };
        Ok(report(name, StepStatus::Failed, &ran, why))
    }
}
