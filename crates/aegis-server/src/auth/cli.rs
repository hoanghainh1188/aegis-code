//! Roles and tokens from the server's command line, audited as `cli`:
//!
//! ```text
//! aegis-server --config <file> <command>
//!   grant  <who> <repo-slug> maintainer|reviewer|viewer
//!   grant  <who> admin
//!   revoke <who> <repo-slug>|admin
//!   token  <service>             # prints a new API token, once
//!   revoke-tokens <service>
//!   revoke-sessions <who>        # signs a person out everywhere
//! ```
//!
//! The database URL comes from the configuration's secret store, as for
//! the server. `<who>` is a person's e-mail (as the SSO gives it; granted before the
//! first sign-in is fine) or a service's name.

use aegis_store::{Actor, RepoRole, Store, UserKind};

use super::{random_token, token_hash};

pub const USAGE: &str = "\
aegis-server --config <file> <command>, the command one of:
  grant <who> <repo-slug> maintainer|reviewer|viewer
  grant <who> admin
  revoke <who> <repo-slug>|admin
  token <service>
  revoke-tokens <service>
  revoke-sessions <who>";

/// Whether `first`, the first argument, names one of these commands.
pub fn is_command(first: &str) -> bool {
    matches!(
        first,
        "grant" | "revoke" | "token" | "revoke-tokens" | "revoke-sessions"
    )
}

fn kind_of(who: &str) -> UserKind {
    if who.contains('@') {
        UserKind::Person
    } else {
        UserKind::Service
    }
}

/// Runs one command; what to print.
pub async fn run(store: &Store, args: &[String]) -> Result<String, String> {
    let actor = Actor::cli();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let e = |e: aegis_store::StoreError| e.to_string();
    match args.as_slice() {
        ["grant" | "revoke", who, "admin"] => {
            let admin = args[0] == "grant";
            let user = store
                .set_admin(kind_of(who), who, admin, &actor)
                .await
                .map_err(e)?;
            Ok(format!("{}: admin = {admin}", user.name))
        }
        ["grant", who, slug, role] => {
            let role = RepoRole::parse(role)
                .ok_or_else(|| format!("unknown role `{role}`: maintainer, reviewer, viewer"))?;
            let repo = repo_id(store, slug).await?;
            let user = store
                .set_repo_role(kind_of(who), who, repo, Some(role), &actor)
                .await
                .map_err(e)?;
            Ok(format!("{}: {} in {slug}", user.name, role.as_str()))
        }
        ["revoke", who, slug] => {
            let repo = repo_id(store, slug).await?;
            let user = store
                .set_repo_role(kind_of(who), who, repo, None, &actor)
                .await
                .map_err(e)?;
            Ok(format!("{}: no role in {slug}", user.name))
        }
        ["token", service] if kind_of(service) == UserKind::Service => {
            let token = random_token().map_err(|e| format!("no randomness: {e}"))?;
            let user = store
                .create_token(service, &token_hash(&token), &actor)
                .await
                .map_err(e)?;
            // The only time the token is shown; only its hash is kept.
            Ok(format!(
                "{token}\n(token of `{}`; give it roles with `aegis-server grant {} <repo> <role>`)",
                user.name, user.name
            ))
        }
        ["revoke-tokens", service] => {
            let n = store.revoke_tokens(service, &actor).await.map_err(e)?;
            Ok(format!("{n} token(s) of `{service}` revoked"))
        }
        ["revoke-sessions", who] => {
            let n = store
                .end_sessions_of(kind_of(who), who, &actor)
                .await
                .map_err(e)?;
            Ok(format!("{n} session(s) of `{who}` ended"))
        }
        _ => Err(USAGE.into()),
    }
}

async fn repo_id(store: &Store, slug: &str) -> Result<aegis_store::RepoId, String> {
    store
        .repo_by_slug(slug)
        .await
        .map_err(|e| e.to_string())?
        .map(|r| r.id)
        .ok_or_else(|| format!("no repo `{slug}`"))
}
