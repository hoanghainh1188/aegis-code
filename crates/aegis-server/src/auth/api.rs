//! `/api/v1` for connectors and services: `Authorization: Bearer <token>`
//! from `aegis-server token`, rate limited per service (#161).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aegis_store::{ActorKind, UserId};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{Access, Auth, token_hash};
use crate::api::ApiError;

/// A token bucket per service: `per_minute` requests, refilled evenly.
/// Kept in memory, so each API process counts its own.
#[derive(Debug)]
pub struct RateLimiter {
    per_minute: u32,
    buckets: Mutex<HashMap<UserId, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn per_minute(per_minute: u32) -> Self {
        Self {
            per_minute,
            buckets: Mutex::default(),
        }
    }

    /// `Err` with how long to wait when `user` has used up its requests.
    pub fn take(&self, user: UserId) -> Result<(), Duration> {
        self.take_at(user, Instant::now())
    }

    fn take_at(&self, user: UserId, now: Instant) -> Result<(), Duration> {
        let capacity = f64::from(self.per_minute.max(1));
        let per_second = capacity / 60.0;
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let (tokens, at) = buckets.entry(user).or_insert((capacity, now));
        let refilled = now.saturating_duration_since(*at).as_secs_f64() * per_second;
        *tokens = (*tokens + refilled).min(capacity);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / per_second))
        }
    }
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
pub async fn guard(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    let Some(token) = bearer(&request) else {
        return unauthorized();
    };
    let store = auth.store();
    let user = match store.token_user(&token_hash(token)).await {
        Ok(Some(user)) => user,
        Ok(None) => return unauthorized(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    if let Err(wait) = auth.limiter().take(user.id) {
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
        return response;
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
}
