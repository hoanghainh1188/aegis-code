//! The GitHub App's `installation` and `installation_repositories` events
//! (ADR 0017 §5, #540): recorded in the store, and the app's cached
//! installations and tokens dropped. Taken from the app's webhook only;
//! an event never registers a repo.

use aegis_store::{InstallationSeen, RepoSelection, WebhookOutcome};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use super::{Handled, Hooks, Via, answer, ignored, internal, malformed};

pub(super) fn is_event(event: &str) -> bool {
    matches!(event, "installation" | "installation_repositories")
}

#[derive(Debug, Deserialize)]
struct Account {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Installation {
    id: i64,
    account: Account,
    #[serde(default)]
    repository_selection: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Named {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct Event {
    action: String,
    installation: Installation,
    #[serde(default)]
    repositories: Vec<Named>,
    #[serde(default)]
    repository_selection: Option<String>,
    #[serde(default)]
    repositories_added: Vec<Named>,
    #[serde(default)]
    repositories_removed: Vec<Named>,
}

/// What an event changes.
#[derive(Debug, PartialEq)]
enum Change {
    Seen(InstallationSeen),
    Suspended {
        id: i64,
        on: bool,
    },
    Removed {
        id: i64,
        account: String,
    },
    Repos {
        id: i64,
        account: String,
        selection: RepoSelection,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// Nothing to record; cached tokens still go (their permissions
    /// changed).
    Permissions {
        id: i64,
    },
}

fn names(list: Vec<Named>) -> Vec<String> {
    list.into_iter().map(|n| n.full_name).collect()
}

fn parse(event: &str, body: &[u8]) -> Result<Option<Change>, String> {
    let e: Event = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let (id, account) = (e.installation.id, e.installation.account.login);
    let selection = |given: Option<String>| {
        given
            .as_deref()
            .and_then(RepoSelection::parse)
            .ok_or_else(|| "repository_selection: `all` or `selected`".to_owned())
    };
    Ok(Some(match (event, e.action.as_str()) {
        ("installation", "created") => Change::Seen(InstallationSeen {
            id,
            account,
            selection: selection(e.installation.repository_selection)?,
            repositories: names(e.repositories),
        }),
        ("installation", "deleted") => Change::Removed { id, account },
        ("installation", "suspend") => Change::Suspended { id, on: true },
        ("installation", "unsuspend") => Change::Suspended { id, on: false },
        ("installation", "new_permissions_accepted") => Change::Permissions { id },
        ("installation_repositories", "added" | "removed") => Change::Repos {
            id,
            account,
            selection: selection(
                e.repository_selection
                    .or(e.installation.repository_selection),
            )?,
            added: names(e.repositories_added),
            removed: names(e.repositories_removed),
        },
        _ => return Ok(None),
    }))
}

fn id_of(change: &Change) -> i64 {
    match change {
        Change::Seen(s) => s.id,
        Change::Suspended { id, .. }
        | Change::Removed { id, .. }
        | Change::Repos { id, .. }
        | Change::Permissions { id } => *id,
    }
}

pub(super) async fn handle(
    hooks: &Hooks,
    via: Via,
    delivery: &str,
    event: &str,
    body: &[u8],
) -> Handled {
    if via != Via::App {
        return ignored(
            delivery,
            "installation events count from the GitHub App's webhook only",
        );
    }
    let change = match parse(event, body) {
        Ok(Some(change)) => change,
        Ok(None) => return ignored(delivery, "an installation action the server does not use"),
        Err(why) => return malformed(why),
    };
    let id = id_of(&change);
    let s = &hooks.store;
    let (recorded, what) = match &change {
        Change::Seen(seen) => (
            s.installation_seen(seen).await,
            format!(
                "installed on {} ({} repos named)",
                seen.account,
                seen.repositories.len()
            ),
        ),
        Change::Suspended { id, on } => (
            s.installation_suspended(*id, *on).await.map(drop),
            if *on { "suspended" } else { "unsuspended" }.to_owned(),
        ),
        Change::Removed { id, account } => (
            s.installation_removed(*id, account).await,
            format!("removed from {account}"),
        ),
        Change::Repos {
            id,
            account,
            selection,
            added,
            removed,
        } => (
            s.installation_repos_changed(*id, account, *selection, added, removed)
                .await,
            format!("{} repos added, {} removed", added.len(), removed.len()),
        ),
        Change::Permissions { .. } => (Ok(()), "new permissions accepted".to_owned()),
    };
    if let Err(e) = recorded {
        return match e {
            aegis_store::StoreError::Invalid { field, message } => {
                malformed(format!("{field}: {message}"))
            }
            e => internal(delivery, &e),
        };
    }
    // This process's cache; another's drops its own when GitHub refuses
    // a token of the installation.
    if let Some(handle) = &hooks.config.app {
        match (handle.app().await, u64::try_from(id)) {
            (Ok(app), Ok(id)) => app.forget_installation(id),
            (Err(e), _) => tracing::warn!(error = %e, "the GitHub App, to drop its cache"),
            (_, Err(_)) => {}
        }
    }
    let reason = format!("installation {id}: {what}");
    tracing::info!(
        delivery,
        installation = id,
        what,
        "GitHub App installation recorded"
    );
    Handled::new(
        answer(StatusCode::OK, json!({ "status": "recorded" })),
        WebhookOutcome::Installation,
        Some(reason),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(v: serde_json::Value) -> Vec<u8> {
        v.to_string().into_bytes()
    }

    #[test]
    fn each_action_is_read_as_its_change() {
        let inst =
            json!({"id": 7, "account": {"login": "acme"}, "repository_selection": "selected"});
        let created = body(json!({
            "action": "created", "installation": inst,
            "repositories": [{"full_name": "acme/shop"}]
        }));
        assert_eq!(
            parse("installation", &created).unwrap(),
            Some(Change::Seen(InstallationSeen {
                id: 7,
                account: "acme".into(),
                selection: RepoSelection::Selected,
                repositories: vec!["acme/shop".into()],
            }))
        );
        for (action, change) in [
            (
                "deleted",
                Change::Removed {
                    id: 7,
                    account: "acme".into(),
                },
            ),
            ("suspend", Change::Suspended { id: 7, on: true }),
            ("unsuspend", Change::Suspended { id: 7, on: false }),
            ("new_permissions_accepted", Change::Permissions { id: 7 }),
        ] {
            let b = body(json!({"action": action, "installation": inst}));
            assert_eq!(parse("installation", &b).unwrap(), Some(change), "{action}");
        }
        let repos = body(json!({
            "action": "removed", "installation": inst, "repository_selection": "selected",
            "repositories_added": [], "repositories_removed": [{"full_name": "acme/shop"}]
        }));
        assert_eq!(
            parse("installation_repositories", &repos).unwrap(),
            Some(Change::Repos {
                id: 7,
                account: "acme".into(),
                selection: RepoSelection::Selected,
                added: vec![],
                removed: vec!["acme/shop".into()],
            })
        );
        let other = body(json!({"action": "renamed", "installation": inst}));
        assert_eq!(parse("installation", &other).unwrap(), None);
        assert!(parse("installation", b"{}").is_err());
        let bad = body(json!({
            "action": "created",
            "installation": {"id": 7, "account": {"login": "acme"}, "repository_selection": "some"}
        }));
        assert!(parse("installation", &bad).is_err());
    }
}
