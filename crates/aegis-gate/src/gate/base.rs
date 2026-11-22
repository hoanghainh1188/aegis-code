//! The base revision a step compares the patch with, and running a step's
//! command on either tree.

use super::*;

impl<'a> Run<'a> {
    pub(super) fn log(&self, name: &str) -> PathBuf {
        self.part.logs.join(format!("{name}.log"))
    }

    pub(super) fn base_tree(&mut self) -> Result<&BaseTree, GateError> {
        if self.base_tree.is_none() {
            let path = self.work.join("base");
            self.base_tree = Some(BaseTree::create(self.req.repo, &self.base, &path)?);
        }
        let tree = self.base_tree.as_ref().expect("just created");
        if !self.part.base_seeded {
            let dir = tree.path().join(&self.part.dir);
            if let Some(cache) = &self.part.build_cache {
                cache.seed(&dir)?;
                self.seeded.push((cache.clone(), self.part.dir.clone()));
            }
            self.fetch_base(&dir)?;
            self.part.base_seeded = true;
        }
        Ok(self.base_tree.as_ref().expect("just created"))
    }

    /// The profile's fetch in the base copy's `dir`, through the egress
    /// proxy, when the fetch leaves state in the tree (#303).
    pub(super) fn fetch_base(&self, dir: &Path) -> Result<(), GateError> {
        let (Some(fetch), Some(sandbox)) = (&self.part.fetch, &self.part.sandbox) else {
            return Ok(());
        };
        if !self.part.profile.profile.dependencies.fetch_base {
            return Ok(());
        }
        let session = fetch.egress.start()?;
        let ran = session.fetch(
            sandbox,
            &fetch.command,
            dir,
            &self.log("fetch-base"),
            self.req.step_timeout,
            self.req.cancel.clone(),
        )?;
        match ran.exit {
            e if e.success() => Ok(()),
            Exit::Cancelled => Ok(()),
            e => Err(GateError::BaseFetch(format!(
                "{}:\n{}\n\nproxy:\n{}",
                exit_text(e),
                tail(&ran.log),
                session.log()
            ))),
        }
    }

    /// The base copy's directory of the part, once the copy exists.
    pub(super) fn base_dir(&mut self) -> Result<PathBuf, GateError> {
        let dir = self.part.dir.clone();
        Ok(self.base_tree()?.path().join(dir))
    }

    /// Runs in the part's directory of the patched repo, or of the base
    /// copy when `on_base`; each side has its own `TMPDIR`.
    pub(super) fn run_in(
        &self,
        argv: &[String],
        on_base: bool,
        log: &str,
    ) -> Result<Ran, GateError> {
        let (dir, tmp) = match (on_base, &self.base_tree) {
            (true, Some(tree)) => (tree.path().join(&self.part.dir), self.work.join("tmp-base")),
            _ => (self.part.repo.clone(), self.work.join("tmp")),
        };
        std::fs::create_dir_all(&tmp).map_err(|e| GateError::io(&tmp, &e))?;
        let tmp = tmp.display().to_string();
        let extra = [("TMPDIR", tmp.as_str())];
        match self.sidecar_runner()? {
            Some(runner) => runner.run(argv, &dir, &self.log(log), &extra),
            None => self.part.runner.run(argv, &dir, &self.log(log), &extra),
        }
    }

    /// The part's runner on the sidecars' network, when the step reaches
    /// them: they start the first time, and are reset every time after so
    /// each command finds fresh services.
    pub(super) fn sidecar_runner(&self) -> Result<Option<Runner>, GateError> {
        let (Some(sidecars), Some(sandbox)) = (&self.req.sidecars, &self.part.runner.sandbox)
        else {
            return Ok(None);
        };
        if !self.with_sidecars.get() || sidecars.specs.is_empty() {
            return Ok(None);
        }
        let mut session = self.sidecars.borrow_mut();
        match session.as_ref() {
            Some(running) => running.reset()?,
            None => *session = Some(sidecars.start()?),
        }
        let sandbox = session.as_ref().map(|s| s.sandbox_for(sandbox));
        Ok(Some(Runner {
            sandbox,
            ..self.part.runner.clone()
        }))
    }
}
