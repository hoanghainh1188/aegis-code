//! The AI each mode uses, for the admin (#367): its provider, the model
//! and effort of each tier, its default budget, the name of its key's
//! secret and whether the store has it (never its value), and for
//! `customer-jp` its guard. "Check" sends the smallest request there is
//! through the provider the workers use, audited as `ai.check`, at most
//! once a minute per mode since each costs a little. Only an admin: for
//! anyone else it is not found.

use std::time::{Duration, Instant};

use aegis_lifecycle::ModeConfig;
use aegis_provider::{
    CachePlan, Message, ModelProvider, Provider, ProviderConfig, ProviderError, Request,
};
use aegis_store::Store;
use askama::Template;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;

use super::admin::admin_only;
use super::repo_admin::{RepoAdmin, RepoAdminExt, editing};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

const MODES: [&str; 2] = ["poc", "customer-jp"];
/// The least time between two checks of a mode.
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// Tokens the check's answer may take: it only needs to come back.
const CHECK_MAX_TOKENS: u32 = 16;

struct TierRow {
    tier: &'static str,
    model: String,
    effort: String,
}

struct ModeView {
    name: &'static str,
    /// `None`: the server has no configuration for this mode.
    config: Option<ConfigView>,
    check: Option<CheckView>,
}

struct ConfigView {
    provider: &'static str,
    /// Region, endpoint: where its requests go.
    target: String,
    /// The key's secret and whether the store has it; `None` when the
    /// provider takes none (Bedrock uses the server's role).
    key: Option<(String, bool)>,
    guard: Option<&'static str>,
    tiers: Vec<TierRow>,
    max_cost_usd: String,
    max_model_calls: u32,
}

pub struct CheckView {
    pub ok: bool,
    pub detail: String,
}

#[derive(Template)]
#[template(path = "admin_ai.html")]
struct AiPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    modes: Vec<ModeView>,
}

fn mode_config<'a>(admin: &'a RepoAdmin, name: &str) -> Option<&'a ModeConfig> {
    let modes = admin.modes();
    match name {
        "poc" => modes.poc.as_ref(),
        "customer-jp" => modes.customer_jp.as_ref(),
        _ => None,
    }
}

async fn is_set(admin: &RepoAdmin, name: &str) -> bool {
    matches!(admin.secrets.get_or_load(name).await, Ok(Some(_)))
}

async fn config_view(admin: &RepoAdmin, name: &str, mode: &ModeConfig) -> ConfigView {
    let (provider, target, key, guard) = match &mode.provider {
        ProviderConfig::ClaudeApi {
            api_key_env,
            base_url,
        } => (
            "Claude API",
            base_url
                .clone()
                .unwrap_or_else(|| aegis_provider::DEFAULT_BASE_URL.into()),
            Some((api_key_env.clone(), is_set(admin, api_key_env).await)),
            None,
        ),
        ProviderConfig::Bedrock {
            region,
            require_jp_profile,
        } => (
            "Amazon Bedrock",
            format!("region {region}"),
            None,
            require_jp_profile.then_some(
                "only `jp.` inference profiles; checked when the server starts, which refuses to start otherwise",
            ),
        ),
        ProviderConfig::Local {
            base_url,
            api_key_env,
            ..
        } => {
            let key = match api_key_env {
                Some(k) => Some((k.clone(), is_set(admin, k).await)),
                None => None,
            };
            ("Local (OpenAI-compatible)", base_url.clone(), key, None)
        }
    };
    let guard = guard.or((name == "customer-jp").then_some("checked when the server starts"));
    let tier = |tier, id: &str, effort: Option<aegis_provider::Effort>| TierRow {
        tier,
        model: id.to_owned(),
        effort: effort.map_or_else(|| "default".into(), |e| format!("{e:?}").to_lowercase()),
    };
    ConfigView {
        provider,
        target,
        key,
        guard,
        tiers: vec![
            tier("light", &mode.models.light.id, mode.models.light.effort),
            tier("core", &mode.models.core.id, mode.models.core.effort),
            tier("expert", &mode.models.expert.id, mode.models.expert.effort),
        ],
        max_cost_usd: format!("${:.2}", mode.budget.max_cost_usd),
        max_model_calls: mode.budget.max_model_calls,
    }
}

async fn page(
    admin: &RepoAdmin,
    nonce: &Nonce,
    me: &Me,
    checked: Option<(&str, CheckView)>,
) -> WebResult {
    let mut checked = checked;
    let mut modes = Vec::new();
    for name in MODES {
        let config = match mode_config(admin, name) {
            Some(mode) => Some(config_view(admin, name, mode).await),
            None => None,
        };
        let check = match &checked {
            Some((m, _)) if *m == name => checked.take().map(|(_, c)| c),
            _ => None,
        };
        modes.push(ModeView {
            name,
            config,
            check,
        });
    }
    render(&AiPage {
        nonce: nonce.as_str(),
        me,
        modes,
    })
}

pub async fn show(
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    page(&admin, &nonce, &me, None).await
}

/// What a provider's refusal means, said plainly; `secret` never appears.
fn said(e: &ProviderError, secret: Option<&str>) -> String {
    let text = match e {
        ProviderError::Api {
            status: 401 | 403, ..
        } => "the provider refused the key: wrong, expired, or without access to the model"
            .to_owned(),
        ProviderError::Api {
            status: 404,
            message,
            ..
        } => {
            format!("the model is not there for this key: {message}")
        }
        ProviderError::Api {
            status, message, ..
        } => {
            format!("the provider answered {status}: {message}")
        }
        ProviderError::RateLimited { .. } => "rate limited by the provider; try later".into(),
        ProviderError::Overloaded => "the provider is overloaded; try later".into(),
        ProviderError::Transport(why) => format!("the provider could not be reached: {why}"),
        ProviderError::PolicyViolation { model, reason } => {
            format!("model `{model}` refused by the guard: {reason}")
        }
        other => other.to_string(),
    };
    match secret {
        Some(s) if !s.is_empty() => text.replace(s, "***"),
        _ => text,
    }
}

/// The key's value, to keep it out of what the page says.
async fn key_value(admin: &RepoAdmin, mode: &ModeConfig) -> Option<String> {
    let name = key_name(mode)?;
    admin
        .secrets
        .get_or_load(name)
        .await
        .ok()
        .flatten()
        .map(|s| s.expose().to_owned())
}

/// The name of `mode`'s key, when its provider takes one.
fn key_name(mode: &ModeConfig) -> Option<&str> {
    match &mode.provider {
        ProviderConfig::ClaudeApi { api_key_env, .. } => Some(api_key_env),
        ProviderConfig::Local { api_key_env, .. } => api_key_env.as_deref(),
        ProviderConfig::Bedrock { .. } => None,
    }
}

/// Sends the smallest request there is to `mode`'s light model.
async fn run_check(admin: &RepoAdmin, mode: &ModeConfig) -> CheckView {
    let secret = key_value(admin, mode).await;
    if let (Some(name), None) = (key_name(mode), &secret) {
        return CheckView {
            ok: false,
            detail: format!("the secret store has no `{name}`; nothing was sent"),
        };
    }
    let model = mode.models.light.id.clone();
    let provider = match Provider::from_config_with_secrets(&mode.provider, &admin.secrets).await {
        Ok(p) => p,
        Err(e) => {
            return CheckView {
                ok: false,
                detail: said(&e, secret.as_deref()),
            };
        }
    };
    let request = Request {
        model: model.clone(),
        max_tokens: CHECK_MAX_TOKENS,
        effort: None,
        system: Vec::new(),
        tools: Vec::new(),
        messages: vec![Message::user_text("Reply with the word OK.")],
        cache: CachePlan::default(),
    };
    let started = Instant::now();
    match provider.send(&request).await {
        Ok(answer) => CheckView {
            ok: true,
            detail: format!(
                "`{}` answered in {} ms",
                answer.model,
                started.elapsed().as_millis()
            ),
        },
        Err(e) => CheckView {
            ok: false,
            detail: format!("`{model}`: {}", said(&e, secret.as_deref())),
        },
    }
}

pub async fn check(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Extension(admin): RepoAdminExt,
    Path(name): Path<String>,
) -> WebResult {
    admin_only(&access)?;
    let admin = editing(admin)?;
    let Some(name) = MODES.into_iter().find(|m| *m == name) else {
        return Err(WebError::not_found(format!("mode `{name}`")));
    };
    let Some(mode) = mode_config(&admin, name) else {
        return Err(WebError::new(
            StatusCode::CONFLICT,
            format!("this server has no configuration for mode `{name}`"),
        ));
    };
    {
        let mut last = admin
            .ai_checks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = last.get(name).filter(|at| at.elapsed() < CHECK_EVERY) {
            let wait = (CHECK_EVERY - at.elapsed()).as_secs() + 1;
            return Err(WebError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("mode `{name}` was checked a moment ago; check again in {wait} s"),
            ));
        }
        last.insert(name, Instant::now());
    }
    let result = run_check(&admin, mode).await;
    let detail = json!({
        "mode": name,
        "model": mode.models.light.id,
        "ok": result.ok,
        "result": result.detail,
    });
    store.audit_event(&access.actor, "ai.check", detail).await?;
    page(&admin, &nonce, &me, Some((name, result))).await
}
