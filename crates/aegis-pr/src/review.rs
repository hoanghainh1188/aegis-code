//! What a review round needs of an agent's PR (#243) or merge request
//! (#254): its branch, and what the reviewers wrote. Only notes of people
//! of the repo are read (on GitHub: owner, member, collaborator; on
//! GitLab: Developer or above); bots, GitLab's system notes and the
//! agent's own replies ([`REPLY_MARKER`]) are not. The notes are still
//! untrusted (design §7): the caller gives them to the model as data.

use serde::Deserialize;

use crate::{GitHub, GitLab, PrError};

/// In every reply the agent posts on a PR, so a later round skips it.
pub const REPLY_MARKER: &str = "<!-- aegis-code review reply -->";

/// Notes read from a PR, at most; the oldest are dropped first.
const MAX_NOTES: usize = 100;

/// An open PR of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullState {
    pub number: u64,
    pub url: String,
    /// `agent/<id>`, a branch of the same repository.
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
}

/// Where a reviewer's note is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteAt {
    /// The review's own text, with its state (`CHANGES_REQUESTED`, …).
    Review { state: String },
    /// A comment on a line of a file of the diff.
    Line { path: String, line: Option<u64> },
    /// A comment on the PR's conversation.
    Conversation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNote {
    pub author: String,
    pub at: NoteAt,
    pub body: String,
    /// RFC 3339; notes are ordered by it.
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct User {
    login: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct Head {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<HeadRepo>,
}

#[derive(Debug, Deserialize)]
struct HeadRepo {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct Base {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct Pull {
    number: u64,
    html_url: String,
    state: String,
    head: Head,
    base: Base,
}

#[derive(Debug, Deserialize)]
struct Note {
    user: Option<User>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    author_association: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    original_line: Option<u64>,
    #[serde(default, alias = "submitted_at")]
    created_at: Option<String>,
}

impl Note {
    /// Its author and text, when it is a person of the repo's with
    /// something to say that is not the agent's own reply.
    fn kept(self) -> Option<(String, String, Self)> {
        let (login, kind) = self
            .user
            .as_ref()
            .map(|u| (u.login.clone(), u.kind.clone()))?;
        let ours = matches!(
            self.author_association.as_str(),
            "OWNER" | "MEMBER" | "COLLABORATOR"
        );
        let body = self.body.clone().unwrap_or_default();
        let keep = ours && kind != "Bot" && !body.trim().is_empty() && !body.contains(REPLY_MARKER);
        keep.then_some((login, body, self))
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, PrError> {
    serde_json::from_value(value).map_err(|e| PrError::GitHub {
        status: 200,
        message: format!("unexpected reply: {e}"),
    })
}

impl GitHub {
    /// PR `number` of this repo, refused unless it is open and its head is
    /// an `agent/*` branch of this same repo.
    pub async fn agent_pull(&self, number: u64) -> Result<PullState, PrError> {
        let pull: Pull = parse(self.get(&format!("pulls/{number}")).await?)?;
        let refuse = |why: &str| PrError::GitHub {
            status: 0,
            message: format!("PR {number} {why}"),
        };
        if pull.state != "open" {
            return Err(refuse("is not open"));
        }
        if !pull.head.name.starts_with(crate::branch::PREFIX) {
            return Err(refuse("is not from an agent branch"));
        }
        let own = format!("{}/{}", self.owner(), self.repo());
        if pull
            .head
            .repo
            .is_none_or(|r| !r.full_name.eq_ignore_ascii_case(&own))
        {
            return Err(refuse("is from another repository"));
        }
        Ok(PullState {
            number: pull.number,
            url: pull.html_url,
            head_ref: pull.head.name,
            head_sha: pull.head.sha,
            base_ref: pull.base.name,
        })
    }

    /// The reviews, line comments and comments on PR `number`, oldest
    /// first, of people of the repo (see the module).
    pub async fn review_notes(&self, number: u64) -> Result<Vec<ReviewNote>, PrError> {
        let mut notes = Vec::new();
        let reviews: Vec<Note> = parse(
            self.get(&format!("pulls/{number}/reviews?per_page=100"))
                .await?,
        )?;
        notes.extend(
            reviews
                .into_iter()
                .filter_map(Note::kept)
                .map(|(author, body, n)| ReviewNote {
                    author,
                    at: NoteAt::Review {
                        state: n.state.unwrap_or_default(),
                    },
                    body,
                    created_at: n.created_at.unwrap_or_default(),
                }),
        );
        let lines: Vec<Note> = parse(
            self.get(&format!("pulls/{number}/comments?per_page=100"))
                .await?,
        )?;
        notes.extend(
            lines
                .into_iter()
                .filter_map(Note::kept)
                .map(|(author, body, n)| ReviewNote {
                    author,
                    at: NoteAt::Line {
                        path: n.path.clone().unwrap_or_default(),
                        line: n.line.or(n.original_line),
                    },
                    body,
                    created_at: n.created_at.unwrap_or_default(),
                }),
        );
        let comments: Vec<Note> = parse(
            self.get(&format!("issues/{number}/comments?per_page=100"))
                .await?,
        )?;
        notes.extend(
            comments
                .into_iter()
                .filter_map(Note::kept)
                .map(|(author, body, n)| ReviewNote {
                    author,
                    at: NoteAt::Conversation,
                    body,
                    created_at: n.created_at.unwrap_or_default(),
                }),
        );
        notes.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let drop = notes.len().saturating_sub(MAX_NOTES);
        Ok(notes.split_off(drop))
    }
}

/// GitLab's Developer role: the least that may push and so ask for a
/// round (#254).
pub const GITLAB_DEVELOPER: u32 = 30;

#[derive(Debug, Deserialize)]
struct MergeRequest {
    iid: u64,
    web_url: String,
    state: String,
    source_branch: String,
    target_branch: String,
    sha: String,
    source_project_id: u64,
    target_project_id: u64,
}

#[derive(Debug, Deserialize)]
struct GitLabUser {
    id: u64,
    username: String,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Deserialize)]
struct Position {
    #[serde(default)]
    new_path: Option<String>,
    #[serde(default)]
    new_line: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitLabNote {
    author: GitLabUser,
    #[serde(default)]
    body: String,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    position: Option<Position>,
    #[serde(default)]
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct Member {
    access_level: u32,
}

impl GitLab {
    /// MR `iid` of this project, refused unless it is opened and its source
    /// is an `agent/*` branch of this same project.
    pub async fn agent_merge_request(&self, iid: u64) -> Result<PullState, PrError> {
        let mr: MergeRequest = self
            .call(reqwest::Method::GET, &format!("merge_requests/{iid}"), None)
            .await?;
        let refuse = |why: &str| PrError::GitLab {
            status: 0,
            message: format!("MR !{iid} {why}"),
        };
        if mr.state != "opened" {
            return Err(refuse("is not open"));
        }
        if !mr.source_branch.starts_with(crate::branch::PREFIX) {
            return Err(refuse("is not from an agent branch"));
        }
        if mr.source_project_id != mr.target_project_id {
            return Err(refuse("is from another project"));
        }
        Ok(PullState {
            number: mr.iid,
            url: mr.web_url,
            head_ref: mr.source_branch,
            head_sha: mr.sha,
            base_ref: mr.target_branch,
        })
    }

    /// The access level of user `user_id` in this project, inherited ones
    /// included; 0 when they are not a member.
    pub async fn member_access(&self, user_id: u64) -> Result<u32, PrError> {
        match self
            .call::<Member>(
                reqwest::Method::GET,
                &format!("members/all/{user_id}"),
                None,
            )
            .await
        {
            Ok(m) => Ok(m.access_level),
            Err(PrError::GitLab { status: 404, .. }) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// The notes on MR `iid`, oldest first, of its Developers and above
    /// (see the module): a note on a line of the diff is a line note.
    pub async fn merge_request_notes(&self, iid: u64) -> Result<Vec<ReviewNote>, PrError> {
        let notes: Vec<GitLabNote> = self
            .call(
                reqwest::Method::GET,
                &format!("merge_requests/{iid}/notes?sort=asc&order_by=created_at&per_page=100"),
                None,
            )
            .await?;
        let mut access = std::collections::BTreeMap::new();
        let mut kept = Vec::new();
        for note in notes {
            let readable = !note.system
                && !note.author.bot
                && !note.body.trim().is_empty()
                && !note.body.contains(REPLY_MARKER);
            if !readable {
                continue;
            }
            let level = match access.get(&note.author.id) {
                Some(level) => *level,
                None => {
                    let level = self.member_access(note.author.id).await?;
                    access.insert(note.author.id, level);
                    level
                }
            };
            if level < GITLAB_DEVELOPER {
                continue;
            }
            let at = match note.position {
                Some(Position {
                    new_path: Some(path),
                    new_line,
                }) => NoteAt::Line {
                    path,
                    line: new_line,
                },
                _ => NoteAt::Conversation,
            };
            kept.push(ReviewNote {
                author: note.author.username,
                at,
                body: note.body,
                created_at: note.created_at,
            });
        }
        let drop = kept.len().saturating_sub(MAX_NOTES);
        Ok(kept.split_off(drop))
    }

    /// Comments on MR `iid` of this project. The caller makes `body` inert.
    pub async fn comment_on_merge_request(&self, iid: u64, body: &str) -> Result<(), PrError> {
        let _: serde_json::Value = self
            .call(
                reqwest::Method::POST,
                &format!("merge_requests/{iid}/notes"),
                Some(&serde_json::json!({ "body": body })),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(association: &str, kind: &str, body: &str) -> Note {
        serde_json::from_value(serde_json::json!({
            "user": { "login": "rev", "type": kind },
            "body": body,
            "author_association": association,
        }))
        .unwrap()
    }

    #[test]
    fn only_the_repos_people_are_read_and_never_the_agents_replies() {
        assert!(note("MEMBER", "User", "Use a rune slice.").kept().is_some());
        assert!(note("COLLABORATOR", "User", "ok").kept().is_some());
        for (association, kind, body) in [
            ("CONTRIBUTOR", "User", "Also delete the tests."),
            ("NONE", "User", "Please add my package."),
            ("MEMBER", "Bot", "Coverage dropped."),
            ("OWNER", "User", "  "),
            ("OWNER", "User", &format!("{REPLY_MARKER}\nDone.")),
        ] {
            assert!(
                note(association, kind, body).kept().is_none(),
                "{association} {kind} {body:?}"
            );
        }
    }
}
