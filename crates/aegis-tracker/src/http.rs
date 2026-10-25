//! What both clients share: one HTTP client, answers checked and read
//! within a size, errors that never carry the URL (Backlog's has the key).

use std::time::Duration;

use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;

use crate::TrackerError;

/// A request still running after this long fails.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// The most of an answer read, but for logs.
const MAX_ANSWER_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn client(tracker: &'static str) -> Result<reqwest::Client, TrackerError> {
    reqwest::Client::builder()
        .user_agent("aegis-code")
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| unreached(tracker, e))
}

pub(crate) fn unreached(tracker: &'static str, e: reqwest::Error) -> TrackerError {
    TrackerError::Api {
        tracker,
        status: 0,
        message: e.without_url().to_string(),
    }
}

/// Sends it; an answer that is not 2xx is an error with the tracker's own
/// message, found by `message`.
pub(crate) async fn send(
    tracker: &'static str,
    request: RequestBuilder,
    message: fn(&serde_json::Value) -> Option<String>,
) -> Result<Response, TrackerError> {
    let response = request.send().await.map_err(|e| unreached(tracker, e))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let (bytes, _) = read(tracker, response, 64 * 1024).await?;
    let text = String::from_utf8_lossy(&bytes);
    let message = serde_json::from_str(&text)
        .ok()
        .and_then(|v| message(&v))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(TrackerError::Api {
        tracker,
        status,
        message,
    })
}

/// [`send`], then the answer as `T`.
pub(crate) async fn json<T: DeserializeOwned>(
    tracker: &'static str,
    request: RequestBuilder,
    message: fn(&serde_json::Value) -> Option<String>,
) -> Result<T, TrackerError> {
    let response = send(tracker, request, message).await?;
    let status = response.status().as_u16();
    let (bytes, cut) = read(tracker, response, MAX_ANSWER_BYTES).await?;
    if cut {
        return Err(TrackerError::Api {
            tracker,
            status,
            message: format!("an answer over {MAX_ANSWER_BYTES} bytes"),
        });
    }
    serde_json::from_slice(&bytes).map_err(|e| TrackerError::Api {
        tracker,
        status,
        message: format!("unexpected answer: {e}"),
    })
}

/// At most `limit` bytes of the body, and whether there was more.
pub(crate) async fn read(
    tracker: &'static str,
    mut response: Response,
    limit: u64,
) -> Result<(Vec<u8>, bool), TrackerError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| unreached(tracker, e))? {
        let room = usize::try_from(limit).unwrap_or(usize::MAX) - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, false))
}
