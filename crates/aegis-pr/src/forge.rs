//! Where the agent's change goes: a pull request on GitHub or a merge
//! request on GitLab (#252), from a branch of the same repository.

use crate::{AgentBranch, GitHub, GitLab, PrError, Remote};

#[derive(Debug, Clone)]
pub enum Forge {
    GitHub(GitHub),
    GitLab(GitLab),
}

impl Forge {
    /// Where the agent's branch is pushed.
    pub fn remote(&self) -> Result<Remote, PrError> {
        match self {
            Self::GitHub(github) => github.remote(),
            Self::GitLab(gitlab) => gitlab.remote(),
        }
    }

    /// Comments on PR or MR `number` (#254). The caller makes `body` inert.
    pub async fn comment_on_request(&self, number: u64, body: &str) -> Result<(), PrError> {
        match self {
            Self::GitHub(github) => github.comment_on_issue(number, body).await.map(drop),
            Self::GitLab(gitlab) => gitlab.comment_on_merge_request(number, body).await,
        }
    }

    /// Opens a PR or MR from `head` into `base`; its web URL.
    pub async fn open_request(
        &self,
        head: &AgentBranch,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<String, PrError> {
        match self {
            Self::GitHub(github) => github
                .open_pull_request(head, base, title, body, draft)
                .await
                .map(|pr| pr.url),
            Self::GitLab(gitlab) => gitlab
                .open_merge_request(head, base, title, body, draft)
                .await
                .map(|mr| mr.web_url),
        }
    }
}
