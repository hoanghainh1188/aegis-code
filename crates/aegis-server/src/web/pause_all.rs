//! Pausing every agent from the dashboard (#510), as `aegis-server
//! pause-all` does ([`crate::pause_all`]): each step behind a second
//! click. Only an admin: for anyone else the page and its forms are not
//! found.

use aegis_store::{PauseLevel, Store};
use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::admin::admin_only;
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

#[derive(Template)]
#[template(path = "admin_pause.html")]
struct PausePage<'a> {
    nonce: &'a str,
    me: &'a Me,
    notice: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct Done {
    done: Option<String>,
}

fn notice_of(done: Option<&str>) -> Option<&'static str> {
    match done? {
        "paused" => Some("All agents paused: no new job runs until they are resumed."),
        "stopped" => Some("Emergency stop: running jobs stop at their next lease renewal."),
        "resumed" => Some("All agents resumed: queued jobs run again."),
        _ => None,
    }
}

pub async fn show(
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Query(q): Query<Done>,
) -> WebResult {
    admin_only(&access)?;
    render(&PausePage {
        nonce: nonce.as_str(),
        me: &me,
        notice: notice_of(q.done.as_deref()),
    })
}

#[derive(Debug, Deserialize)]
pub struct PauseForm {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    level: Option<String>,
}

pub async fn pause(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Form(form): Form<PauseForm>,
) -> WebResult {
    admin_only(&access)?;
    let level = match form.level.as_deref() {
        None | Some("intake") => PauseLevel::Intake,
        Some("emergency") => PauseLevel::Emergency,
        Some(other) => {
            return Err(WebError::new(
                StatusCode::BAD_REQUEST,
                format!("level: `{other}` is neither `intake` nor `emergency`"),
            ));
        }
    };
    let paused = store
        .pause_instance_by(level, Some(form.reason.as_str()), &access.actor)
        .await?;
    tracing::warn!(
        level = level.as_str(),
        by = %access.actor.name,
        cancelled = paused.cancelled,
        "every agent paused"
    );
    let done = match level {
        PauseLevel::Intake => "paused",
        PauseLevel::Emergency => "stopped",
    };
    Ok(Redirect::to(&format!("/admin/pause?done={done}")).into_response())
}

pub async fn resume(State(store): State<Store>, Extension(access): Extension<Access>) -> WebResult {
    admin_only(&access)?;
    store.resume_instance_by(&access.actor).await?;
    tracing::warn!(by = %access.actor.name, "every agent resumed");
    Ok(Redirect::to("/admin/pause?done=resumed").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_outcomes_have_a_notice() {
        assert!(notice_of(Some("paused")).is_some());
        assert!(notice_of(Some("stopped")).is_some());
        assert!(notice_of(Some("resumed")).is_some());
        assert_eq!(notice_of(Some("<script>")), None);
        assert_eq!(notice_of(None), None);
    }
}
