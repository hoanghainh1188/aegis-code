//! A repo's runbooks and business rules synced from its main branch
//! (#660), [`aegis_lifecycle::memory_sources`]: on a push to that branch,
//! after an agent PR is merged into it, or when its admin asks. The
//! clone's main branch is fetched first, then read; nothing else of the
//! repo is.

use std::sync::Arc;

use aegis_lifecycle::{Modes, RepoSettings, SyncedMemory};
use aegis_secrets::Secrets;
use aegis_store::{Actor, GitSync, NewRepoMemory, Repo, Store};

use crate::github_app::AppHandle;

/// What a sync needs of the server.
#[derive(Clone)]
pub struct MemorySync {
    pub modes: Arc<Modes>,
    pub secrets: Secrets,
    pub app: Option<Arc<AppHandle>>,
}

/// The repo's `[memory_sources]`; `None` when it names none, or its
/// settings do not read.
pub fn sources_of(repo: &Repo) -> Option<aegis_lifecycle::MemorySources> {
    serde_json::from_value::<RepoSettings>(repo.config.clone())
        .ok()
        .map(|s| s.memory_sources)
        .filter(|s| !s.is_empty())
}

fn entry(repo: &Repo, m: SyncedMemory) -> NewRepoMemory {
    NewRepoMemory {
        repo_id: repo.id,
        kind: m.kind,
        source_ref: m.source_ref,
        title: m.title,
        summary: m.summary,
        paths: Vec::new(),
        keywords: m.keywords,
        job_id: None,
        scope: None,
    }
}

impl MemorySync {
    /// Syncs `repo`'s memory with its main branch as `actor`: with no
    /// `[memory_sources]`, its `git:` memories are removed. The error is
    /// what went wrong, the token never in it.
    pub async fn sync(&self, store: &Store, repo: &Repo, actor: &Actor) -> Result<GitSync, String> {
        let settings: RepoSettings =
            serde_json::from_value(repo.config.clone()).map_err(|e| e.to_string())?;
        let config = self.modes.run_config(repo).map_err(|e| e.to_string())?;
        let token = crate::github_app::read_token(
            &self.secrets,
            self.app.as_deref(),
            &settings,
            &repo.url,
            repo.tenant_id,
        )
        .await?;
        let hide = |e: String| match &token {
            Some(t) if !t.is_empty() => e.replace(t.as_str(), "***"),
            _ => e,
        };
        self.modes
            .clone_repo(repo, token.as_deref())
            .await
            .map_err(|e| hide(e.to_string()))?;
        let found = aegis_lifecycle::memory_sources::read_memory_sources_async(
            &config.repo.path,
            &repo.default_branch,
            &settings.memory_sources,
        )
        .await
        .map_err(|e| e.to_string())?;
        let entries: Vec<NewRepoMemory> = found.into_iter().map(|m| entry(repo, m)).collect();
        store
            .sync_git_memories(repo.id, &entries, actor)
            .await
            .map_err(|e| e.to_string())
    }

    /// [`Self::sync`] off the caller, when `repo` names sources; the
    /// outcome goes to the log.
    pub fn sync_in_background(&self, store: &Store, repo: &Repo) {
        if sources_of(repo).is_none() {
            return;
        }
        let (this, store, repo) = (self.clone(), store.clone(), repo.clone());
        tokio::spawn(async move {
            match this.sync(&store, &repo, &Actor::memory_sync()).await {
                Ok(done) => tracing::info!(
                    repo = %repo.slug,
                    added = done.added,
                    updated = done.updated,
                    unchanged = done.unchanged,
                    removed = done.removed,
                    "memory synced from the main branch"
                ),
                Err(e) => tracing::warn!(repo = %repo.slug, error = %e, "memory sync failed"),
            }
        });
    }
}
