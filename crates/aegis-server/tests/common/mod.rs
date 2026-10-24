//! Sign-in for the tests: sessions and tokens made in the store as the
//! SSO callback and `aegis-server token` make them.

#![allow(dead_code, reason = "each test file uses part of it")]

use aegis_server::auth::{Auth, Secrets, random_token, token_hash};
use aegis_server::config::AuthSection;
use aegis_store::{Actor, RepoId, RepoRole, Store, UserId, UserKind};

pub const SESSION_KEY: &[u8] = b"a test session key, 32 bytes or more";

pub fn auth_with(store: &Store, config: AuthSection, oidc_secret: Option<&str>) -> Auth {
    let secrets = Secrets {
        session_key: SESSION_KEY.to_vec(),
        oidc_client_secret: oidc_secret.map(str::to_owned),
    };
    Auth::new(store.clone(), config, secrets).unwrap()
}

/// No SSO: sessions come from [`sign_in`].
pub fn auth(store: &Store) -> Auth {
    auth_with(store, AuthSection::default(), None)
}

/// A signed-in browser: its cookie and its session's CSRF token.
pub struct Browser {
    pub user: UserId,
    pub cookie: String,
    pub csrf: String,
}

pub async fn sign_in(store: &Store, email: &str) -> Browser {
    let user = store
        .sign_in(&format!("sub-{email}"), email, true)
        .await
        .unwrap();
    let (token, csrf) = (random_token().unwrap(), random_token().unwrap());
    let expires = chrono::Utc::now() + chrono::Duration::hours(1);
    store
        .start_session(&user, &token_hash(&token), &csrf, expires)
        .await
        .unwrap();
    Browser {
        user: user.id,
        cookie: format!("__Host-aegis_session={token}"),
        csrf,
    }
}

pub async fn admin(store: &Store, name: &str) {
    let kind = if name.contains('@') {
        UserKind::Person
    } else {
        UserKind::Service
    };
    store
        .set_admin(kind, name, true, &Actor::cli())
        .await
        .unwrap();
}

pub async fn grant(store: &Store, name: &str, repo: RepoId, role: RepoRole) {
    let kind = if name.contains('@') {
        UserKind::Person
    } else {
        UserKind::Service
    };
    store
        .set_repo_role(kind, name, repo, Some(role), &Actor::cli())
        .await
        .unwrap();
}

/// A new API token of the service `name`.
pub async fn token(store: &Store, name: &str) -> String {
    let token = random_token().unwrap();
    store
        .create_token(name, &token_hash(&token), &Actor::cli())
        .await
        .unwrap();
    token
}
