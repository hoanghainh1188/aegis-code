//! Metrics in Prometheus' format at `/metrics` (ADR 0003 §6, #32).
//!
//! `/metrics` is served on its own listener, `[metrics] listen`, by every
//! role, and not by the API: a worker-only process has no API, and a
//! service token (#29) would cost a database query and a rate-limit slot
//! per scrape. The port is meant for the private network only (a
//! security group that lets in only Prometheus); it shows counts, never a
//! job, an issue or a secret.
//!
//! Labels stay few: `repo` (slug), `language`, `tier`, a kind or a
//! status. Never a job id or an issue: each value is a new series.
//!
//! - Jobs: those this process runs now, and each attempt's end and time.
//! - Provider calls ([`JobObserver`]), from the response each call got:
//!   calls, errors by kind, retries, tokens by kind (input, output, cache
//!   read and write) and time. The cache read ratio is
//!   `cache_read / (input + cache_read + cache_write)`.
//! - The queue, read from the database at each scrape: ready, waiting
//!   (backoff) and running jobs, and how long the oldest ready one waited.
//!   Every process reports the same queue; alerts take the max.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use aegis_provider::{CallObserver, CallReport};
use aegis_store::Store;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use tokio_util::sync::CancellationToken;

const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct AttemptLabels {
    /// The job's state after the attempt (`queued` for a retry), or
    /// `lease_lost`, or `unrecorded` when its end could not be written.
    pub status: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CallLabels {
    repo: String,
    language: String,
    tier: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ResultLabels {
    repo: String,
    language: String,
    tier: &'static str,
    /// `ok`, or the error's kind ([`aegis_provider::ProviderError::kind`]).
    result: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct TokenLabels {
    repo: String,
    language: String,
    tier: &'static str,
    /// `input`, `output`, `cache_read`, `cache_write`.
    kind: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct TierLabels {
    tier: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct QueueLabels {
    state: &'static str,
}

fn seconds(start: f64, factor: f64, length: u16) -> Histogram {
    Histogram::new(exponential_buckets(start, factor, length))
}

/// Every metric of the process.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    pub jobs_running: Gauge,
    attempts: Family<AttemptLabels, Counter>,
    attempt_seconds: Family<AttemptLabels, Histogram>,
    provider_calls: Family<ResultLabels, Counter>,
    provider_retries: Family<CallLabels, Counter>,
    provider_tokens: Family<TokenLabels, Counter>,
    provider_call_seconds: Family<TierLabels, Histogram>,
    queue_jobs: Family<QueueLabels, Gauge>,
    queue_oldest_ready_seconds: Gauge<f64, AtomicU64>,
}

impl Metrics {
    fn new() -> Self {
        let mut registry = Registry::with_prefix("aegis");
        let jobs_running = Gauge::default();
        registry.register(
            "jobs_running",
            "Attempts this process runs now",
            jobs_running.clone(),
        );
        let attempts = Family::default();
        registry.register(
            "job_attempts",
            "Attempts ended, by the job's state after them",
            attempts.clone(),
        );
        // 10 s to about 3 h.
        let attempt_seconds =
            Family::new_with_constructor((|| seconds(10.0, 2.0, 11)) as fn() -> Histogram);
        registry.register(
            "job_attempt_duration_seconds",
            "How long attempts ran",
            attempt_seconds.clone(),
        );
        let provider_calls = Family::default();
        registry.register(
            "provider_calls",
            "Model calls ended, by result: ok or the error's kind",
            provider_calls.clone(),
        );
        let provider_retries = Family::default();
        registry.register(
            "provider_retries",
            "Model calls sent again after an error worth retrying",
            provider_retries.clone(),
        );
        let provider_tokens = Family::default();
        registry.register(
            "provider_tokens",
            "Tokens of answered calls: input, output, cache_read, cache_write",
            provider_tokens.clone(),
        );
        // 0.25 s to about 17 min.
        let provider_call_seconds =
            Family::new_with_constructor((|| seconds(0.25, 2.0, 13)) as fn() -> Histogram);
        registry.register(
            "provider_call_duration_seconds",
            "How long model calls took, errors included",
            provider_call_seconds.clone(),
        );
        let queue_jobs = Family::default();
        registry.register(
            "queue_jobs",
            "Jobs in the queue: ready, waiting (backoff) and running",
            queue_jobs.clone(),
        );
        let queue_oldest_ready_seconds = Gauge::default();
        registry.register(
            "queue_oldest_ready_seconds",
            "How long the oldest ready job has waited since it was due",
            queue_oldest_ready_seconds.clone(),
        );
        Self {
            registry,
            jobs_running,
            attempts,
            attempt_seconds,
            provider_calls,
            provider_retries,
            provider_tokens,
            provider_call_seconds,
            queue_jobs,
            queue_oldest_ready_seconds,
        }
    }

    pub fn attempt_ended(&self, status: &str, seconds: f64) {
        let labels = AttemptLabels {
            status: status.to_owned(),
        };
        self.attempts.get_or_create(&labels).inc();
        self.attempt_seconds.get_or_create(&labels).observe(seconds);
    }

    fn queue(&self, stats: &aegis_store::QueueStats) {
        for (state, count) in [
            ("ready", stats.ready),
            ("waiting", stats.waiting),
            ("running", stats.running),
        ] {
            self.queue_jobs
                .get_or_create(&QueueLabels { state })
                .set(count);
        }
        self.queue_oldest_ready_seconds.set(stats.oldest_ready_s);
    }

    /// The text Prometheus scrapes.
    pub fn render(&self) -> Result<String, std::fmt::Error> {
        let mut text = String::new();
        prometheus_client::encoding::text::encode(&mut text, &self.registry)?;
        Ok(text)
    }
}

/// The metrics of this process.
pub fn metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}

/// Tells [`metrics`] of a job's model calls, under its repo's labels.
#[derive(Debug, Clone)]
pub struct JobObserver {
    repo: String,
    language: String,
    /// Each model id of the job's configuration and its tier.
    tiers: Vec<(String, &'static str)>,
}

impl JobObserver {
    pub fn new(repo: &str, language: &str, tiers: Vec<(String, &'static str)>) -> Self {
        Self {
            repo: repo.to_owned(),
            language: language.to_owned(),
            tiers,
        }
    }

    fn tier(&self, model: &str) -> &'static str {
        self.tiers
            .iter()
            .find(|(id, _)| id == model)
            .map_or("other", |(_, tier)| tier)
    }
}

impl CallObserver for JobObserver {
    fn call_ended(&self, call: &CallReport<'_>) {
        let m = metrics();
        let tier = self.tier(call.model);
        let labels = CallLabels {
            repo: self.repo.clone(),
            language: self.language.clone(),
            tier,
        };
        let result = match call.result {
            Ok(_) => "ok",
            Err(e) => e.kind(),
        };
        m.provider_calls
            .get_or_create(&ResultLabels {
                repo: labels.repo.clone(),
                language: labels.language.clone(),
                tier,
                result,
            })
            .inc();
        if call.retry {
            m.provider_retries.get_or_create(&labels).inc();
        }
        m.provider_call_seconds
            .get_or_create(&TierLabels { tier })
            .observe(call.elapsed.as_secs_f64());
        let Ok(usage) = call.result else { return };
        for (kind, tokens) in [
            ("input", usage.input_tokens),
            ("output", usage.output_tokens),
            ("cache_read", usage.cache_read_input_tokens),
            ("cache_write", usage.cache_creation_input_tokens),
        ] {
            m.provider_tokens
                .get_or_create(&TokenLabels {
                    repo: labels.repo.clone(),
                    language: labels.language.clone(),
                    tier,
                    kind,
                })
                .inc_by(tokens);
        }
    }
}

/// `GET /metrics`, the queue read from `store` at each scrape.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(store)
}

async fn scrape(State(store): State<Store>) -> Response {
    let m = metrics();
    match store.queue_stats().await {
        Ok(stats) => m.queue(&stats),
        // The other metrics still count; the queue's stay as last read.
        Err(e) => tracing::error!(error = %e, "reading the queue for /metrics"),
    }
    match m.render() {
        Ok(text) => ([(header::CONTENT_TYPE, CONTENT_TYPE)], text).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "rendering /metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Serves [`router`] on `listen` until `shutdown`.
pub async fn serve(
    listen: SocketAddr,
    store: Store,
    shutdown: CancellationToken,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("metrics: listen on {listen}: {e}"))?;
    tracing::info!(listen = %listen, "metrics listening");
    axum::serve(listener, router(store))
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
        .map_err(|e| format!("metrics: {e}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aegis_provider::{ProviderError, Usage};

    use super::*;

    fn observer() -> JobObserver {
        JobObserver::new(
            "unit-observer",
            "rust",
            vec![("m-core".into(), "core"), ("m-light".into(), "light")],
        )
    }

    fn line_of<'a>(text: &'a str, start: &str) -> Option<&'a str> {
        text.lines().find(|l| l.starts_with(start))
    }

    #[test]
    fn calls_count_by_result_tier_and_token_kind() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 90,
            cache_creation_input_tokens: 20,
            ..Usage::default()
        };
        let o = observer();
        let ok = CallReport {
            model: "m-core",
            elapsed: Duration::from_millis(300),
            retry: false,
            result: Ok(&usage),
        };
        o.call_ended(&ok);
        let overloaded = ProviderError::Overloaded;
        o.call_ended(&CallReport {
            model: "m-light",
            elapsed: Duration::from_secs(1),
            retry: true,
            result: Err(&overloaded),
        });
        let text = metrics().render().unwrap();
        let has = |start: &str, value: &str| {
            let line = line_of(&text, start).unwrap_or_else(|| panic!("{start}\n{text}"));
            assert!(line.ends_with(value), "{line}");
        };
        has(
            r#"aegis_provider_tokens_total{repo="unit-observer",language="rust",tier="core",kind="cache_read"}"#,
            " 90",
        );
        has(
            r#"aegis_provider_tokens_total{repo="unit-observer",language="rust",tier="core",kind="cache_write"}"#,
            " 20",
        );
        has(
            r#"aegis_provider_calls_total{repo="unit-observer",language="rust",tier="core",result="ok"}"#,
            " 1",
        );
        has(
            r#"aegis_provider_calls_total{repo="unit-observer",language="rust",tier="light",result="overloaded"}"#,
            " 1",
        );
        has(
            r#"aegis_provider_retries_total{repo="unit-observer",language="rust",tier="light"}"#,
            " 1",
        );
        assert!(text.ends_with("# EOF\n"));
    }

    #[test]
    fn a_model_outside_the_configuration_is_tier_other() {
        assert_eq!(observer().tier("m-core"), "core");
        assert_eq!(observer().tier("surprise"), "other");
    }
}
