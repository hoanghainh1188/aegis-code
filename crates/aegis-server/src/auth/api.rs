//! `/api/v1` for connectors and services: `Authorization: Bearer <token>`
//! from `aegis-server token`, rate limited per service (#161).
//!
//! Requests without a live token are counted per client address, and an
//! address past its rate is refused before its token is looked up, so a
//! flood of made-up tokens costs the database (shared with the workers)
//! no more than that rate (#173).

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aegis_store::{ActorKind, UserId};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{Access, Auth, token_hash};
use crate::api::ApiError;

/// Keys a limiter keeps before it forgets those back at a full bucket.
const MAX_KEYS: usize = 10_000;

/// A token bucket per key (a service, a client address): `per_minute`
/// requests, refilled evenly. Kept in memory, so each API process counts
/// its own (ADR 0003 §4).
#[derive(Debug)]
pub struct RateLimiter<K = UserId> {
    per_minute: u32,
    buckets: Mutex<HashMap<K, (f64, Instant)>>,
}

impl<K: Eq + Hash + Copy> RateLimiter<K> {
    pub fn per_minute(per_minute: u32) -> Self {
        Self {
            per_minute,
            buckets: Mutex::default(),
        }
    }

    fn capacity(&self) -> f64 {
        f64::from(self.per_minute.max(1))
    }

    /// `Err` with how long to wait when `key` has used up its requests.
    pub fn take(&self, key: K) -> Result<(), Duration> {
        self.take_at(key, Instant::now())
    }

    fn take_at(&self, key: K, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= MAX_KEYS && !buckets.contains_key(&key) {
            // A full bucket is the same as none: forgetting it changes nothing.
            buckets.retain(|_, b| self.refilled(*b, now) < self.capacity());
        }
        let bucket = buckets.entry(key).or_insert((self.capacity(), now));
        let tokens = self.refilled(*bucket, now);
        if tokens >= 1.0 {
            *bucket = (tokens - 1.0, now);
            Ok(())
        } else {
            *bucket = (tokens, now);
            Err(Duration::from_secs_f64(
                (1.0 - tokens) / (self.capacity() / 60.0),
            ))
        }
    }

    /// `Err` with how long to wait when `key` has nothing left, without
    /// taking anything.
    pub fn check(&self, key: K) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: K, now: Instant) -> Result<(), Duration> {
        let buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        match buckets.get(&key).map(|b| self.refilled(*b, now)) {
            Some(tokens) if tokens < 1.0 => Err(Duration::from_secs_f64(
                (1.0 - tokens) / (self.capacity() / 60.0),
            )),
            _ => Ok(()),
        }
    }

    fn refilled(&self, (tokens, at): (f64, Instant), now: Instant) -> f64 {
        let per_second = self.capacity() / 60.0;
        let refilled = now.saturating_duration_since(at).as_secs_f64() * per_second;
        (tokens + refilled).min(self.capacity())
    }

    #[cfg(test)]
    fn keys(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }
}

/// The address the request came from: the TCP peer, or with `trusted`
/// proxies in front, the address the outermost of them saw, which is
/// `trusted`-th from the right in `X-Forwarded-For` (each proxy appends
/// the address it saw). Without either, all such requests share one key.
pub(crate) fn client_address(request: &Request, trusted: u8) -> IpAddr {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    if trusted == 0 {
        return peer.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
    let forwarded: Vec<&str> = request
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    forwarded
        .len()
        .checked_sub(usize::from(trusted))
        .and_then(|i| forwarded[i].parse().ok())
        .or(peer)
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

fn too_many(wait: Duration) -> Response {
    let mut response = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "too many requests; wait and retry",
    )
    .into_response();
    let secs = wait.as_secs().max(1).to_string();
    if let Ok(value) = HeaderValue::from_str(&secs) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// The bearer token, if it has the shape of one ([`super::random_token`]):
/// anything else is refused without a database query.
fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn unauthorized() -> Response {
    let mut response = ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "an API token is needed: Authorization: Bearer <token>",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

/// Lets through requests with a live token, within their rate, with the
/// token's [`Access`] in the request's extensions.
/// An address past its rate of failures is refused before its token is
/// looked up, even a live one.
pub async fn guard(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    let client = client_address(&request, auth.trusted_proxies());
    let failures = auth.failure_limiter();
    if let Err(wait) = failures.check(client) {
        return too_many(wait);
    }
    let failed = || {
        // Whether this one was the last allowed matters only to the next.
        let _ = failures.take(client);
        unauthorized()
    };
    let Some(token) = bearer(&request) else {
        return failed();
    };
    let store = auth.store();
    let user = match store.token_user(&token_hash(token)).await {
        Ok(Some(user)) => user,
        Ok(None) => return failed(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    if let Err(wait) = auth.limiter().take(user.id) {
        return too_many(wait);
    }
    let access = match Access::load(store, &user, ActorKind::Token).await {
        Ok(access) => access,
        Err(e) => return ApiError::from(e).into_response(),
    };
    request.extensions_mut().insert(access);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_service_has_its_own_bucket_refilled_over_the_minute() {
        let limiter = RateLimiter::per_minute(2);
        let t0 = Instant::now();
        let (a, b) = (UserId(1), UserId(2));
        assert!(limiter.take_at(a, t0).is_ok());
        assert!(limiter.take_at(a, t0).is_ok());
        let wait = limiter.take_at(a, t0).unwrap_err();
        assert!(
            wait > Duration::from_secs(29) && wait <= Duration::from_secs(30),
            "{wait:?}"
        );
        assert!(limiter.take_at(b, t0).is_ok());
        assert!(limiter.take_at(a, t0 + Duration::from_secs(30)).is_ok());
    }

    #[test]
    fn check_tells_an_empty_bucket_without_taking() {
        let limiter = RateLimiter::per_minute(1);
        let t0 = Instant::now();
        assert!(limiter.check_at(7, t0).is_ok(), "unseen");
        assert!(limiter.check_at(7, t0).is_ok(), "check takes nothing");
        assert!(limiter.take_at(7, t0).is_ok());
        assert!(limiter.check_at(7, t0).is_err());
        assert!(limiter.check_at(7, t0 + Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn keys_back_at_a_full_bucket_are_forgotten_past_the_cap() {
        let limiter = RateLimiter::per_minute(60);
        let t0 = Instant::now();
        for key in 0..MAX_KEYS {
            limiter.take_at(key, t0).unwrap();
        }
        assert_eq!(limiter.keys(), MAX_KEYS);
        // A second later, every bucket is full again.
        limiter
            .take_at(MAX_KEYS, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(limiter.keys(), 1);
    }

    fn request(peer: Option<&str>, forwarded: &[&str]) -> Request {
        let mut request = Request::new(axum::body::Body::empty());
        if let Some(peer) = peer {
            let addr: SocketAddr = format!("{peer}:443").parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(addr));
        }
        for f in forwarded {
            request
                .headers_mut()
                .append("x-forwarded-for", HeaderValue::from_str(f).unwrap());
        }
        request
    }

    #[test]
    fn the_client_is_the_peer_or_what_the_trusted_proxies_saw() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // No proxy trusted: the header is anyone's to write.
        let r = request(Some("10.0.0.5"), &["1.2.3.4"]);
        assert_eq!(client_address(&r, 0), ip("10.0.0.5"));
        // Behind one proxy: what it appended, not what the client wrote.
        let r = request(Some("10.0.0.5"), &["6.6.6.6, 1.2.3.4"]);
        assert_eq!(client_address(&r, 1), ip("1.2.3.4"));
        let r = request(Some("10.0.0.5"), &["6.6.6.6", "1.2.3.4, 10.0.0.9"]);
        assert_eq!(client_address(&r, 2), ip("1.2.3.4"));
        // Fewer entries than proxies, or none parsable: the peer.
        let r = request(Some("10.0.0.5"), &[]);
        assert_eq!(client_address(&r, 1), ip("10.0.0.5"));
        let r = request(Some("10.0.0.5"), &["junk"]);
        assert_eq!(client_address(&r, 1), ip("10.0.0.5"));
        let r = request(None, &[]);
        assert_eq!(client_address(&r, 0), ip("0.0.0.0"));
    }
}
