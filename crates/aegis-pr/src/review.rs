//! What a review round needs of an agent's PR (#243): its branch, and what
//! the reviewers wrote. Only notes of people GitHub counts as the repo's
//! (owner, member, collaborator) are read; bots and the agent's own
//! replies ([`REPLY_MARKER`]) are not. The notes are still untrusted
//! (design §7): the caller gives them to the model as data.

use serde::Deserialize;

use crate::{GitHub, PrError};

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
