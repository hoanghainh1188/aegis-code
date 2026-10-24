//! Sign-in, roles and who may do what (ADR 0003 §5, #29).
//!
//! - People sign in to the dashboard through the company SSO (OIDC, in
//!   [`oidc`]); the session lives in Postgres, the cookie holds only its
//!   token. Every form carries the session's CSRF token.
//! - Services (connectors, CI) call `/api/v1` with a bearer token
//!   ([`api`]), rate limited per service.
//! - An admin may do everything; otherwise a user has one role per repo:
//!   viewer, reviewer (also fills reviews), maintainer (also creates,
//!   cancels and retries jobs). Someone without a role in a repo does not
//!   see it at all. Roles and tokens are given with `aegis-server grant`
//!   and `aegis-server token` ([`cli`]).
//! - Webhooks keep their signatures (#25) and need neither.

pub mod api;
pub mod cli;
pub mod oidc;
mod sign;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aegis_secrets::{Secret, Secrets};
use aegis_store::{Actor, ActorKind, RepoId, RepoRole, Store, StoreError, TokenHash, User};
use sha2::{Digest, Sha256};

use crate::config::AuthSection;
pub use api::RateLimiter;
pub use sign::Signer;

/// The session cookie. `__Host-`: only over HTTPS (or loopback), only for
/// this host, path `/`.
pub const SESSION_COOKIE: &str = "__Host-aegis_session";
/// The form field, and the htmx header, that carry the CSRF token.
pub const CSRF_FIELD: &str = "_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The secrets of sign-in, in the secret store (#30) like every other.
/// Signs the review form's `opened_at` and the sign-in state.
pub const SESSION_KEY: &str = "AEGIS_SESSION_KEY";
/// The dashboard's OIDC client secret; needed with `[auth.oidc]`.
pub const OIDC_CLIENT_SECRET: &str = "AEGIS_OIDC_CLIENT_SECRET";
pub const MIN_SESSION_KEY_LEN: usize = 32;

/// Reads the secrets of sign-in now, as the API starts: the session key,
/// and the OIDC client secret with `[auth.oidc]`. Fails naming each one
/// missing, never a value. Both are then read again with the others when
/// they rotate.
pub async fn load_at_start(secrets: &Secrets, config: &AuthSection) -> Result<(), String> {
    let mut names = vec![SESSION_KEY];
    if config.oidc.is_some() {
        names.push(OIDC_CLIENT_SECRET);
    }
    secrets.require(&names).await.map_err(|e| e.to_string())?;
    session_key(secrets).map(drop)
}

/// The session key as last read, if it is long enough.
fn session_key(secrets: &Secrets) -> Result<Secret, String> {
    let key = secrets
        .get(SESSION_KEY)
        .ok_or_else(|| format!("{SESSION_KEY}: not loaded"))?;
    if key.expose().len() < MIN_SESSION_KEY_LEN {
        return Err(format!(
            "{SESSION_KEY}: at least {MIN_SESSION_KEY_LEN} bytes"
        ));
    }
    Ok(key)
}

/// A fresh random token as hex: session tokens, CSRF tokens, API tokens.
pub fn random_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// What the store keeps of a token.
pub fn token_hash(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

/// Sign-in and the checks every request goes through; cheap to clone.
#[derive(Clone)]
pub struct Auth {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    config: AuthSection,
    signer: Signer,
    oidc: Option<oidc::Oidc>,
    limiter: RateLimiter,
}

impl Auth {
    /// `secrets` must hold the session key ([`load_at_start`]).
    pub fn new(store: Store, config: AuthSection, secrets: Secrets) -> Result<Self, String> {
        let signer = Signer::from_secrets(secrets.clone())?;
        let oidc = config
            .oidc
            .as_ref()
            .map(|o| oidc::Oidc::new(o, &config, secrets))
            .transpose()?;
        let limiter = RateLimiter::per_minute(config.api_requests_per_minute);
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                signer,
                oidc,
                limiter,
                config,
            }),
        })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn signer(&self) -> &Signer {
        &self.inner.signer
    }

    pub(crate) fn oidc(&self) -> Option<&oidc::Oidc> {
        self.inner.oidc.as_ref()
    }

    pub(crate) fn limiter(&self) -> &RateLimiter {
        &self.inner.limiter
    }

    pub(crate) fn session_ttl(&self) -> Duration {
        Duration::from_secs(u64::from(self.inner.config.session_hours) * 3600)
    }

    /// Whether pages answer to `host` (a `Host` header without its port):
    /// loopback, or the host of `public_url`. This also stops DNS
    /// rebinding.
    pub(crate) fn serves_host(&self, host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "::1")
            || self
                .inner
                .config
                .public_url
                .as_ref()
                .and_then(|u| u.host_str())
                .is_some_and(|h| h.eq_ignore_ascii_case(host))
    }
}

/// Who is asking and what they may do.
#[derive(Debug, Clone)]
pub struct Access {
    pub actor: Actor,
    admin: bool,
    roles: BTreeMap<RepoId, RepoRole>,
}

impl Access {
    /// `user`'s roles, read now: a role taken away counts at once.
    pub async fn load(store: &Store, user: &User, kind: ActorKind) -> Result<Self, StoreError> {
        let roles = if user.is_admin {
            BTreeMap::new()
        } else {
            store.repo_roles(user.id).await?.into_iter().collect()
        };
        Ok(Self {
            actor: Actor::user(user, kind),
            admin: user.is_admin,
            roles,
        })
    }

    pub fn is_admin(&self) -> bool {
        self.admin
    }

    /// Whether this may do in `repo` what `needed` allows.
    pub fn can(&self, repo: RepoId, needed: RepoRole) -> bool {
        self.admin || self.roles.get(&repo).is_some_and(|r| r.includes(needed))
    }

    /// The repos this may see; `None`: every repo.
    pub fn visible(&self) -> Option<Vec<RepoId>> {
        (!self.admin).then(|| self.roles.keys().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_store::{UserId, UserKind};

    fn access(admin: bool, roles: &[(i64, RepoRole)]) -> Access {
        Access {
            actor: Actor::user(
                &User {
                    id: UserId(1),
                    kind: UserKind::Person,
                    name: "a@x.test".into(),
                    is_admin: admin,
                },
                ActorKind::User,
            ),
            admin,
            roles: roles.iter().map(|(r, role)| (RepoId(*r), *role)).collect(),
        }
    }

    #[test]
    fn roles_include_those_below_and_only_in_their_repo() {
        let a = access(false, &[(1, RepoRole::Reviewer)]);
        assert!(a.can(RepoId(1), RepoRole::Viewer));
        assert!(a.can(RepoId(1), RepoRole::Reviewer));
        assert!(!a.can(RepoId(1), RepoRole::Maintainer));
        assert!(!a.can(RepoId(2), RepoRole::Viewer));
        assert_eq!(a.visible(), Some(vec![RepoId(1)]));
        let admin = access(true, &[]);
        assert!(admin.can(RepoId(9), RepoRole::Maintainer));
        assert_eq!(admin.visible(), None);
    }

    #[test]
    fn tokens_are_random_and_stored_hashed() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(token_hash(&a), token_hash(&b));
        assert_eq!(token_hash(&a), token_hash(&a));
    }
}
