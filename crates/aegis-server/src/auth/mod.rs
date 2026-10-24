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

const SESSION_KEY_ENV: &str = "AEGIS_SESSION_KEY";
const OIDC_SECRET_ENV: &str = "AEGIS_OIDC_CLIENT_SECRET";
const MIN_SESSION_KEY_LEN: usize = 32;

/// The secrets sign-in needs. `Debug` shows none of them.
pub struct Secrets {
    /// Signs the review form's `opened_at` and the sign-in state.
    pub session_key: Vec<u8>,
    /// The dashboard's OIDC client secret; `None` for a public client.
    pub oidc_client_secret: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets").finish_non_exhaustive()
    }
}

/// The one place the auth secrets are read: from the environment for now,
/// from the secret manager once #30 lands.
pub fn load_secrets() -> Result<Secrets, String> {
    let session_key = std::env::var(SESSION_KEY_ENV)
        .map_err(|_| format!("set {SESSION_KEY_ENV}: at least {MIN_SESSION_KEY_LEN} random bytes"))?
        .into_bytes();
    if session_key.len() < MIN_SESSION_KEY_LEN {
        return Err(format!(
            "{SESSION_KEY_ENV}: at least {MIN_SESSION_KEY_LEN} bytes"
        ));
    }
    let oidc_client_secret = std::env::var(OIDC_SECRET_ENV)
        .ok()
        .filter(|s| !s.is_empty());
    Ok(Secrets {
        session_key,
        oidc_client_secret,
    })
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
    pub fn new(store: Store, config: AuthSection, secrets: Secrets) -> Result<Self, String> {
        let oidc = config
            .oidc
            .as_ref()
            .map(|o| oidc::Oidc::new(o, &config, secrets.oidc_client_secret))
            .transpose()?;
        let limiter = RateLimiter::per_minute(config.api_requests_per_minute);
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                signer: Signer::new(&secrets.session_key),
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
