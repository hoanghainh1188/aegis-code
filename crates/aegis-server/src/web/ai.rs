//! The AI each mode uses, for the admin (#367): its provider, the model
//! and effort of each tier, its default budget, the name of its key's
//! secret and whether the store has it (never its value), and the
//! residency guard its provider checks (#592). "Check" sends the smallest request there is
//! through the provider the workers use, audited as `ai.check`, at most
//! once a minute per mode since each costs a little. Only an admin: for
//! anyone else it is not found.

use std::time::{Duration, Instant};

use aegis_lifecycle::ModeConfig;
use aegis_provider::{
    CachePlan, Message, ModelPolicy, ModelProvider, Provider, ProviderConfig, ProviderError,
    Request,
};
use aegis_store::{ModeName, Store};
use askama::Template;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;

use super::admin::admin_only;
use super::repo_admin::{RepoAdmin, RepoAdminExt, editing};
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

/// Shown even when the server has no file for them: every deployment
/// knows these two. Any other mode shows when `[modes]` names it.
const BUILTIN_MODES: [ModeName; 2] = [ModeName::POC, ModeName::CUSTOMER_JP];
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
    name: String,
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
    guard: Option<String>,
    tiers: Vec<TierRow>,
    max_cost_usd: String,
    /// The mode's call cap, or the language profile's when it sets none
    /// (#471).
    model_calls: String,
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

fn mode_config<'a>(admin: &'a RepoAdmin, name: &ModeName) -> Option<&'a ModeConfig> {
    admin.modes().config(name)
}

/// The configured modes, then the built-in ones the server has no file
/// for.
fn mode_names(admin: &RepoAdmin) -> Vec<ModeName> {
    let mut names: Vec<ModeName> = admin.modes().iter().map(|(n, _)| n.clone()).collect();
    for builtin in BUILTIN_MODES {
        if admin.modes().config(&builtin).is_none() {
            names.push(builtin);
        }
    }
    names
}

/// What `policy` lets through, said plainly.
fn guard_text(policy: &ModelPolicy) -> Option<String> {
    const CHECKED: &str = "checked when the server starts, which refuses to start otherwise, and again before each request";
    match policy {
        ModelPolicy::Any => None,
        ModelPolicy::AllowedPrefixes(prefixes) => {
            let list: Vec<String> = prefixes.iter().map(|p| format!("`{p}`")).collect();
            Some(format!(
                "only models whose IDs start with {}; {CHECKED}",
                list.join(", ")
            ))
        }
        ModelPolicy::LocalOnly => Some(format!("only the local provider; {CHECKED}")),
    }
}

async fn is_set(admin: &RepoAdmin, name: &str) -> bool {
    matches!(admin.secrets.get_or_load(name).await, Ok(Some(_)))
}

async fn config_view(admin: &RepoAdmin, name: &ModeName, mode: &ModeConfig) -> ConfigView {
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
            require_jp_profile.then(|| {
                "only `jp.` inference profiles; checked when the server starts, which refuses to start otherwise".to_owned()
            }),
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
    // The mode was checked when the server loaded it: `ok` is what it
    // loaded with.
    let residency = match mode.check(name) {
        Ok(policy) => guard_text(&policy),
        Err(e) => Some(format!(
            "the configuration breaks it, so nothing is sent: {e}"
        )),
    };
    let guard = residency.or(guard);
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
        model_calls: mode.budget.max_model_calls.map_or_else(
            || "the language profile's cap of model calls (40 if it sets none)".into(),
            |n| format!("{n} model calls"),
        ),
    }
}

async fn page(
    admin: &RepoAdmin,
    nonce: &Nonce,
    me: &Me,
    checked: Option<(&ModeName, CheckView)>,
) -> WebResult {
    let mut checked = checked;
    let mut modes = Vec::new();
    for name in mode_names(admin) {
        let config = match mode_config(admin, &name) {
            Some(mode) => Some(config_view(admin, &name, mode).await),
            None => None,
        };
        let check = match &checked {
            Some((m, _)) if m.key() == name.key() => checked.take().map(|(_, c)| c),
            _ => None,
        };
        modes.push(ModeView {
            name: name.to_string(),
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

/// Sends the smallest request there is to `mode`'s light model, through
/// a provider with the mode's residency policy, as the workers use it.
async fn run_check(admin: &RepoAdmin, name: &ModeName, mode: &ModeConfig) -> CheckView {
    let secret = key_value(admin, mode).await;
    if let (Some(name), None) = (key_name(mode), &secret) {
        return CheckView {
            ok: false,
            detail: format!("the secret store has no `{name}`; nothing was sent"),
        };
    }
    let model = mode.models.light.id.clone();
    let policy = match mode.check(name) {
        Ok(policy) => policy,
        Err(e) => {
            return CheckView {
                ok: false,
                detail: e.to_string(),
            };
        }
    };
    let provider = match Provider::from_config_with_secrets(&mode.provider, &admin.secrets).await {
        Ok(p) => p.with_policy(policy),
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
    let not_found = || WebError::not_found(format!("mode `{name}`"));
    let name = ModeName::parse(&name).map_err(|_| not_found())?;
    let Some(mode) = mode_config(&admin, &name) else {
        if BUILTIN_MODES.iter().any(|b| b.key() == name.key()) {
            return Err(WebError::new(
                StatusCode::CONFLICT,
                format!("this server has no configuration for mode `{name}`"),
            ));
        }
        return Err(not_found());
    };
    {
        let mut last = admin
            .ai_checks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = last
            .get(&name.key())
            .filter(|at| at.elapsed() < CHECK_EVERY)
        {
            let wait = (CHECK_EVERY - at.elapsed()).as_secs() + 1;
            return Err(WebError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("mode `{name}` was checked a moment ago; check again in {wait} s"),
            ));
        }
        last.insert(name.key(), Instant::now());
    }
    let result = run_check(&admin, &name, mode).await;
    let detail = json!({
        "mode": name.as_str(),
        "model": mode.models.light.id,
        "ok": result.ok,
        "result": result.detail,
    });
    store.audit_event(&access.actor, "ai.check", detail).await?;
    page(&admin, &nonce, &me, Some((&name, result))).await
}
