//! Pausing every agent of the instance (#510), for an incident that is not
//! one repo's: from the API, the dashboard (`/admin/pause`) or the command
//! line, for when neither can be reached. Admin only; audited.
//!
//! ```text
//! POST /api/v1/admin/pause   {"level": "intake"|"emergency", "reason": "…"}
//! POST /api/v1/admin/resume
//! GET  /api/v1/admin/pause   # the pause, or `{"paused": false}`
//!
//! aegis-server --config <file> pause-all [--emergency] [--reason <text>]
//! aegis-server --config <file> resume-all
//! ```
//!
//! `intake` (the default): no queued job is claimed; jobs are still
//! recorded and wait. `emergency`: also asks every running job to cancel;
//! each stops at its worker's next lease renewal. See
//! `docs/runbook/incident.md`.

use aegis_store::{Actor, InstancePause, InstancePaused, PauseLevel, Store};
use axum::Extension;
use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult, ok};
use crate::auth::Access;

pub const USAGE: &str = "\
aegis-server --config <file> pause-all [--emergency] [--reason <text>]
aegis-server --config <file> resume-all";

/// Whether `first`, the first argument, is this module's command.
pub fn is_command(first: &str) -> bool {
    matches!(first, "pause-all" | "resume-all")
}

/// The pause as the API shows it.
#[derive(Debug, Serialize)]
struct PauseView {
    paused: bool,
    level: Option<&'static str>,
    paused_at: Option<String>,
    paused_by: Option<String>,
    reason: Option<String>,
    /// Running jobs just asked to cancel; only in the answer to a pause.
    #[serde(skip_serializing_if = "Option::is_none")]
    cancelled: Option<u64>,
}

impl PauseView {
    fn of(pause: Option<InstancePause>) -> Self {
        match pause {
            Some(p) => Self {
                paused: true,
                level: Some(p.level.as_str()),
                paused_at: Some(crate::when::rfc3339(p.at)),
                paused_by: Some(p.by),
                reason: p.reason,
                cancelled: None,
            },
            None => Self {
                paused: false,
                level: None,
                paused_at: None,
                paused_by: None,
                reason: None,
                cancelled: None,
            },
        }
    }
}

impl From<InstancePaused> for PauseView {
    fn from(p: InstancePaused) -> Self {
        Self {
            cancelled: Some(p.cancelled),
            ..Self::of(Some(p.pause))
        }
    }
}

fn admin_only(access: &Access) -> Result<(), ApiError> {
    if access.is_admin() {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only an admin pauses or resumes every agent",
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PauseBody {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn level_of(text: Option<&str>) -> Result<PauseLevel, ApiError> {
    match text {
        None => Ok(PauseLevel::Intake),
        Some(t) => PauseLevel::parse(t).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid",
                format!("level: `{t}` is neither `intake` nor `emergency`"),
            )
        }),
    }
}

/// `GET /api/v1/admin/pause`.
pub(crate) async fn show(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
) -> ApiResult {
    admin_only(&access)?;
    let pause = store.instance_pause().await?;
    Ok(ok(StatusCode::OK, PauseView::of(pause)))
}

/// `POST /api/v1/admin/pause`.
pub(crate) async fn pause(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    body: Result<Json<PauseBody>, JsonRejection>,
) -> ApiResult {
    admin_only(&access)?;
    let Json(body) = body?;
    let level = level_of(body.level.as_deref())?;
    let paused = store
        .pause_instance_by(level, body.reason.as_deref(), &access.actor)
        .await?;
    tracing::warn!(
        level = level.as_str(),
        by = %access.actor.name,
        cancelled = paused.cancelled,
        "every agent paused"
    );
    Ok(ok(StatusCode::OK, PauseView::from(paused)))
}

/// `POST /api/v1/admin/resume`.
pub(crate) async fn resume(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
) -> ApiResult {
    admin_only(&access)?;
    store.resume_instance_by(&access.actor).await?;
    tracing::warn!(by = %access.actor.name, "every agent resumed");
    Ok(ok(StatusCode::OK, PauseView::of(None)))
}

/// `pause-all` / `resume-all`, audited as `cli`.
pub async fn run(store: &Store, args: &[String]) -> Result<String, String> {
    let actor = Actor::cli();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let e = |e: aegis_store::StoreError| e.to_string();
    match args.as_slice() {
        ["pause-all", rest @ ..] => {
            let (level, reason) = pause_flags(rest)?;
            let paused = store
                .pause_instance_by(level, reason, &actor)
                .await
                .map_err(e)?;
            Ok(match level {
                PauseLevel::Intake => {
                    "all agents paused (intake): no new job runs; queued jobs wait".to_owned()
                }
                PauseLevel::Emergency => format!(
                    "all agents paused (emergency): {} running job(s) asked to cancel; \
                     queued jobs wait",
                    paused.cancelled
                ),
            })
        }
        ["resume-all"] => {
            let was = store.resume_instance_by(&actor).await.map_err(e)?;
            Ok(format!(
                "all agents resumed (were paused {} by {}): queued jobs run again",
                was.level.as_str(),
                was.by
            ))
        }
        _ => Err(USAGE.into()),
    }
}

fn pause_flags<'a>(mut rest: &[&'a str]) -> Result<(PauseLevel, Option<&'a str>), String> {
    let mut level = PauseLevel::Intake;
    let mut reason = None;
    while let Some((flag, tail)) = rest.split_first() {
        rest = tail;
        match *flag {
            "--emergency" => level = PauseLevel::Emergency,
            "--reason" => {
                let (why, tail) = rest.split_first().ok_or("--reason needs a value")?;
                reason = Some(*why);
                rest = tail;
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok((level, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_pick_the_level_and_the_reason() {
        assert_eq!(pause_flags(&[]).unwrap(), (PauseLevel::Intake, None));
        assert_eq!(
            pause_flags(&["--reason", "spend", "--emergency"]).unwrap(),
            (PauseLevel::Emergency, Some("spend"))
        );
        assert!(pause_flags(&["--reason"]).is_err());
        assert!(pause_flags(&["--now"]).is_err());
    }

    #[test]
    fn the_level_defaults_to_intake() {
        assert_eq!(level_of(None).unwrap(), PauseLevel::Intake);
        assert_eq!(level_of(Some("emergency")).unwrap(), PauseLevel::Emergency);
        assert!(level_of(Some("halt")).is_err());
    }

    #[test]
    fn only_its_two_commands_are_its_own() {
        assert!(is_command("pause-all") && is_command("resume-all"));
        assert!(!is_command("pause"));
    }
}
