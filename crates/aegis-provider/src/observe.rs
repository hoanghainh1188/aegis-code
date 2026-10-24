//! What a provider's calls did, for metrics (#32): told to an observer
//! as each call ends, from the response the call already got. No extra
//! request, and nothing of the request or the answer but the model, the
//! token counts and the kind of error.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{ModelProvider, ProviderError, Request, Response, Usage};

/// One call that ended.
#[derive(Debug)]
pub struct CallReport<'a> {
    pub model: &'a str,
    pub elapsed: Duration,
    /// The call before it, through the same [`Observed`], failed with an
    /// error worth retrying: this call is most likely its retry.
    pub retry: bool,
    pub result: Result<&'a Usage, &'a ProviderError>,
}

pub trait CallObserver: Send + Sync {
    fn call_ended(&self, call: &CallReport<'_>);
}

/// `inner`, telling `observer` of each call. One per job: retries are
/// counted from the calls it saw, and a job sends one call at a time.
#[derive(Debug)]
pub struct Observed<P, O> {
    inner: P,
    observer: O,
    last_retryable: AtomicBool,
}

impl<P, O> Observed<P, O> {
    pub fn new(inner: P, observer: O) -> Self {
        Self {
            inner,
            observer,
            last_retryable: AtomicBool::new(false),
        }
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P, O> ModelProvider for Observed<P, O>
where
    P: ModelProvider + Sync,
    O: CallObserver,
{
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        let started = Instant::now();
        let response = self.inner.send(request).await;
        let retryable = matches!(&response, Err(e) if e.is_retryable());
        let retry = self.last_retryable.swap(retryable, Ordering::Relaxed);
        self.observer.call_ended(&CallReport {
            model: &request.model,
            elapsed: started.elapsed(),
            retry,
            result: response.as_ref().map(|r| &r.usage),
        });
        response
    }
}

impl ProviderError {
    /// A short name of the error's kind, for a metric's label.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::PolicyViolation { .. } => "policy",
            Self::RateLimited { .. } => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Api { status, .. } if *status >= 500 => "api_5xx",
            Self::Api { .. } => "api_4xx",
            Self::Transport(_) => "transport",
            Self::Decode(_) => "decode",
            Self::Config(_) => "config",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::{CachePlan, StopReason};

    struct Script(Mutex<VecDeque<Result<Response, ProviderError>>>);

    impl ModelProvider for Script {
        async fn send(&self, _: &Request) -> Result<Response, ProviderError> {
            self.0.lock().unwrap().pop_front().unwrap()
        }
    }

    /// Model, retry, cache reads or the error's kind.
    type Call = (String, bool, Result<u64, &'static str>);

    #[derive(Default)]
    struct Seen(Mutex<Vec<Call>>);

    impl CallObserver for Seen {
        fn call_ended(&self, call: &CallReport<'_>) {
            let result = call
                .result
                .map(|u| u.cache_read_input_tokens)
                .map_err(ProviderError::kind);
            self.0
                .lock()
                .unwrap()
                .push((call.model.to_owned(), call.retry, result));
        }
    }

    fn ok(cache_read: u64) -> Result<Response, ProviderError> {
        Ok(Response {
            id: "m".into(),
            model: "core".into(),
            content: vec![],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage {
                cache_read_input_tokens: cache_read,
                ..Usage::default()
            },
        })
    }

    #[tokio::test]
    async fn each_call_is_told_with_its_usage_or_error_and_retries_are_marked() {
        let script = Script(Mutex::new(VecDeque::from([
            ok(7),
            Err(ProviderError::Overloaded),
            ok(9),
            Err(ProviderError::Api {
                status: 400,
                kind: "invalid_request_error".into(),
                message: String::new(),
            }),
            ok(0),
        ])));
        let observed = Observed::new(script, Seen::default());
        let request = Request {
            model: "core".into(),
            max_tokens: 16,
            effort: None,
            system: vec![],
            tools: vec![],
            messages: vec![],
            cache: CachePlan::default(),
        };
        for _ in 0..5 {
            let _ = observed.send(&request).await;
        }
        let seen = observed.observer.0.lock().unwrap().clone();
        let core = |retry, result| ("core".to_owned(), retry, result);
        assert_eq!(
            seen,
            [
                core(false, Ok(7)),
                core(false, Err("overloaded")),
                core(true, Ok(9)),
                core(false, Err("api_4xx")),
                // A 400 is not retried: the next call is a new one.
                core(false, Ok(0)),
            ]
        );
    }

    #[test]
    fn kinds_split_server_and_client_errors() {
        let api = |status| ProviderError::Api {
            status,
            kind: String::new(),
            message: String::new(),
        };
        assert_eq!(api(503).kind(), "api_5xx");
        assert_eq!(api(401).kind(), "api_4xx");
        assert_eq!(ProviderError::Transport("x".into()).kind(), "transport");
    }
}
