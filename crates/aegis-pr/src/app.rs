//! The GitHub App path of [ADR 0017](../../../docs/adr/0017-github-app.md)
//! (#538): the app's JWT, the installation of a repo, and installation
//! tokens scoped to one repo and the permissions of one use, minted when
//! asked and cached until ten minutes before they expire.
//!
//! [`TokenSource`] is what a client holds instead of a token string: a
//! fixed token on the token path (`Static`, exactly as before), or the app
//! asked for a token at each use (`App`). Tokens and the key never appear
//! in `Debug`, logs or errors.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeyPair;
use aws_lc_rs::signature::RSA_PKCS1_SHA256;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::PrError;
use crate::github::{checked_api, plain_name};

const API_VERSION: &str = "2022-11-28";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// The JWT's `iat` this far in the past, for clocks a little ahead of
/// GitHub's.
const JWT_BACKDATE_S: i64 = 60;
/// Its life: GitHub takes at most ten minutes.
const JWT_LIFE_S: i64 = 9 * 60;
/// A cached installation token is used until this long before it expires.
const TOKEN_MARGIN_S: i64 = 10 * 60;

/// What a token is minted for (ADR 0017 §3): each use asks only for what
/// its calls need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenUse {
    /// Clone, fetch the base, read the issue.
    Read,
    /// A failed run's jobs and logs.
    Ci,
    /// Push `agent/*`, open the PR, a review round's push and reply.
    Publish,
    /// The edits reviewers made to the agent's PR.
    ReviewEdits,
    /// Comment on the issue or PR; check who may ask for a review round.
    Comment,
}

impl TokenUse {
    /// The permissions asked for, as `POST …/access_tokens` takes them.
    pub fn permissions(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Read => &[
                ("contents", "read"),
                ("issues", "read"),
                ("metadata", "read"),
            ],
            Self::Ci => &[("actions", "read"), ("metadata", "read")],
            Self::Publish => &[
                ("contents", "write"),
                ("pull_requests", "write"),
                ("metadata", "read"),
            ],
            Self::ReviewEdits => &[("pull_requests", "read"), ("metadata", "read")],
            Self::Comment => &[
                ("issues", "write"),
                ("pull_requests", "write"),
                ("metadata", "read"),
            ],
        }
    }
}

/// The app's RSA private key, from the PEM GitHub hands out (PKCS #1,
/// `BEGIN RSA PRIVATE KEY`) or PKCS #8 (`BEGIN PRIVATE KEY`). Signing is
/// constant time (aws-lc).
pub struct AppKey(KeyPair);

impl fmt::Debug for AppKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppKey(…)")
    }
}

impl AppKey {
    pub fn from_pem(pem: &str) -> Result<Self, PrError> {
        let bad = |why: &str| PrError::App(format!("the app's private key: {why}"));
        let (kind, body) = pem_body(pem).ok_or_else(|| bad("not a PEM private key"))?;
        let der = STANDARD
            .decode(body)
            .map_err(|_| bad("its base64 does not decode"))?;
        let key = match kind {
            PemKind::Pkcs1 => KeyPair::from_der(&der),
            PemKind::Pkcs8 => KeyPair::from_pkcs8(&der),
        }
        .map_err(|_| bad("not an RSA key"))?;
        Ok(Self(key))
    }

    /// The app's JWT, issued by `issuer` (the app id or its client id)
    /// at `now`.
    pub fn jwt(&self, issuer: &str, now: DateTime<Utc>) -> Result<String, PrError> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let iat = now.timestamp() - JWT_BACKDATE_S;
        let claims = json!({ "iat": iat, "exp": iat + JWT_LIFE_S, "iss": issuer });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{header}.{claims}");
        let mut signature = vec![0; self.0.public_modulus_len()];
        self.0
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut signature,
            )
            .map_err(|_| PrError::App("signing the app's JWT failed".into()))?;
        Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

enum PemKind {
    Pkcs1,
    Pkcs8,
}

fn pem_body(pem: &str) -> Option<(PemKind, String)> {
    let kind = if pem.contains("-----BEGIN RSA PRIVATE KEY-----") {
        PemKind::Pkcs1
    } else if pem.contains("-----BEGIN PRIVATE KEY-----") {
        PemKind::Pkcs8
    } else {
        return None;
    };
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("-----"))
        .collect();
    (!body.is_empty()).then_some((kind, body))
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
struct Minted {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
struct Cached {
    token: String,
    expires_at: DateTime<Utc>,
}

/// The instance's GitHub App: its id, key and API root, with the caches of
/// installations (per repo) and tokens (per installation, repo and use).
pub struct GitHubApp {
    client: reqwest::Client,
    api: String,
    issuer: String,
    key: AppKey,
    installations: Mutex<HashMap<(String, String), u64>>,
    tokens: Mutex<HashMap<(u64, String, TokenUse), Cached>>,
}

impl fmt::Debug for GitHubApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubApp")
            .field("api", &self.api)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl GitHubApp {
    /// `issuer` is the app id or its client id; `api` the API root
    /// (`https://api.github.com`, or a GitHub Enterprise one).
    pub fn new(api: &str, issuer: &str, key: AppKey) -> Result<Self, PrError> {
        if issuer.trim().is_empty() {
            return Err(PrError::App("the app's id is empty".into()));
        }
        let client = reqwest::Client::builder()
            .user_agent("aegis-code")
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| PrError::Http(e.to_string()))?;
        Ok(Self {
            client,
            api: checked_api(api)?,
            issuer: issuer.trim().to_owned(),
            key,
            installations: Mutex::default(),
            tokens: Mutex::default(),
        })
    }

    fn jwt(&self) -> Result<String, PrError> {
        self.key.jwt(&self.issuer, Utc::now())
    }

    /// Drops what is known of `owner/repo`'s installation: an
    /// `installation*` event changed it, or GitHub refused it.
    pub fn forget(&self, owner: &str, repo: &str) {
        let removed = lock(&self.installations).remove(&(owner.to_owned(), repo.to_owned()));
        if let Some(id) = removed {
            lock(&self.tokens).retain(|(i, r, _), _| !(*i == id && r == repo));
        }
    }

    /// The installation of the app on `owner/repo`, from the cache or
    /// `GET /repos/{owner}/{repo}/installation` with the app's JWT.
    pub async fn installation(&self, owner: &str, repo: &str) -> Result<u64, PrError> {
        if !plain_name(owner) || !plain_name(repo) {
            return Err(PrError::App(format!(
                "`{owner}/{repo}` is not a repository name"
            )));
        }
        let key = (owner.to_owned(), repo.to_owned());
        if let Some(id) = lock(&self.installations).get(&key) {
            return Ok(*id);
        }
        let url = format!("{}/repos/{owner}/{repo}/installation", self.api);
        let response = self
            .client
            .get(&url)
            .bearer_auth(self.jwt()?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        let found: Installation = read(response, || {
            format!("the app is not installed on `{owner}/{repo}`")
        })
        .await?;
        lock(&self.installations).insert(key, found.id);
        Ok(found.id)
    }

    /// A token for `owner/repo` with the permissions of `use_`, from the
    /// cache while it has more than ten minutes left, else minted:
    /// `POST /app/installations/{id}/access_tokens`, for that repo only.
    pub async fn token(&self, owner: &str, repo: &str, use_: TokenUse) -> Result<String, PrError> {
        let installation = self.installation(owner, repo).await?;
        let key = (installation, repo.to_owned(), use_);
        let fresh =
            |c: &Cached| c.expires_at - Utc::now() > chrono::TimeDelta::seconds(TOKEN_MARGIN_S);
        if let Some(c) = lock(&self.tokens).get(&key).filter(|c| fresh(c)) {
            return Ok(c.token.clone());
        }
        let permissions: serde_json::Map<String, serde_json::Value> = use_
            .permissions()
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect();
        let url = format!(
            "{}/app/installations/{installation}/access_tokens",
            self.api
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(self.jwt()?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .json(&json!({ "repositories": [repo], "permissions": permissions }))
            .send()
            .await
            .map_err(|e| PrError::Http(e.to_string()))?;
        let refused = response.status().as_u16();
        let minted: Minted = match read(response, || {
            format!("the app's installation {installation} gave no token for `{owner}/{repo}`")
        })
        .await
        {
            Ok(m) => m,
            Err(e) => {
                // Uninstalled, suspended, or the repo taken off it: ask
                // again next time instead of trusting the cache.
                if matches!(refused, 401 | 403 | 404 | 422) {
                    self.forget(owner, repo);
                }
                return Err(e);
            }
        };
        lock(&self.tokens).insert(
            key,
            Cached {
                token: minted.token.clone(),
                expires_at: minted.expires_at,
            },
        );
        Ok(minted.token)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The reply as `T`, or an error that says `what` with GitHub's status and
/// message, never the request's credentials.
async fn read<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    what: impl FnOnce() -> String,
) -> Result<T, PrError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| PrError::Http(e.to_string()))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(PrError::GitHub {
            status: status.as_u16(),
            message: format!("{}: {message}", what()),
        });
    }
    serde_json::from_str(&text).map_err(|e| PrError::GitHub {
        status: status.as_u16(),
        message: format!("{}: unexpected reply: {e}", what()),
    })
}

/// Where a client's token comes from (ADR 0017 §3): the token path's fixed
/// token, or the app asked at each use.
#[derive(Clone)]
pub enum TokenSource {
    Static(String),
    App {
        app: Arc<GitHubApp>,
        owner: String,
        repo: String,
        use_: TokenUse,
    },
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static(_) => f.write_str("Static(…)"),
            Self::App {
                owner, repo, use_, ..
            } => write!(f, "App({owner}/{repo}, {use_:?})"),
        }
    }
}

impl TokenSource {
    /// The token to send now.
    pub async fn token(&self) -> Result<String, PrError> {
        match self {
            Self::Static(token) => Ok(token.clone()),
            Self::App {
                app,
                owner,
                repo,
                use_,
            } => app.token(owner, repo, *use_).await,
        }
    }

    /// The same source for another use: an app source asks for that
    /// use's permissions; a fixed token stays the same token.
    pub fn for_use(&self, use_: TokenUse) -> Self {
        match self {
            Self::Static(t) => Self::Static(t.clone()),
            Self::App {
                app, owner, repo, ..
            } => Self::App {
                app: Arc::clone(app),
                owner: owner.clone(),
                repo: repo.clone(),
                use_,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn test_key() -> (AppKey, Vec<u8>) {
        let pair = KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        let der =
            aws_lc_rs::encoding::AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&pair).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(der.as_ref())
        );
        let public = aws_lc_rs::signature::KeyPair::public_key(&pair)
            .as_ref()
            .to_vec();
        (AppKey::from_pem(&pem).unwrap(), public)
    }

    #[test]
    fn the_jwt_is_rs256_backdated_short_lived_and_verifies() {
        let (key, public) = test_key();
        let now = DateTime::parse_from_rfc3339("2026-11-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let jwt = key.jwt("123456", now).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, json!({"alg": "RS256", "typ": "JWT"}));
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        let iat = claims["iat"].as_i64().unwrap();
        assert_eq!(iat, now.timestamp() - 60);
        assert!(claims["exp"].as_i64().unwrap() - now.timestamp() <= 600);
        assert_eq!(claims["iss"], "123456");
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        aws_lc_rs::signature::UnparsedPublicKey::new(
            &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
            &public,
        )
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the signature verifies with the app's public key");
    }

    #[test]
    fn a_key_that_is_not_an_rsa_pem_is_refused_without_its_text() {
        for bad in [
            "",
            "hello",
            "-----BEGIN PRIVATE KEY-----\n!!!\n-----END PRIVATE KEY-----",
        ] {
            let err = AppKey::from_pem(bad).unwrap_err().to_string();
            assert!(err.contains("private key"), "{err}");
            assert!(!err.contains("!!!"));
        }
    }

    #[test]
    fn each_use_asks_only_for_its_permissions() {
        let names = |u: TokenUse| {
            u.permissions()
                .iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(TokenUse::Read),
            ["contents:read", "issues:read", "metadata:read"]
        );
        assert_eq!(names(TokenUse::Ci), ["actions:read", "metadata:read"]);
        assert!(names(TokenUse::Publish).contains(&"contents:write".to_owned()));
        assert!(
            !names(TokenUse::Publish)
                .iter()
                .any(|p| p.starts_with("workflows"))
        );
        assert!(
            !names(TokenUse::Comment)
                .iter()
                .any(|p| p.starts_with("contents"))
        );
    }

    #[test]
    fn neither_debug_shows_a_token_or_the_key() {
        let s = TokenSource::Static("ghp_secret_value".into());
        assert_eq!(format!("{s:?}"), "Static(…)");
        let (key, _) = test_key();
        assert_eq!(format!("{key:?}"), "AppKey(…)");
    }
}
