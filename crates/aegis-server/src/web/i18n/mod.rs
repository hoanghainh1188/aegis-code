//! The pages' language (#590): `en`, `ja` or `vi`, chosen per request by
//! the `aegis_lang` cookie (the masthead's switch), else the browser's
//! `Accept-Language`, else the server's `[web] default_locale`.
//!
//! The words are a static catalog: a template says
//! `{{ crate::t("Jobs") }}`, and [`t`] gives the request's translation of
//! that English text, or the English itself when none exists. The request's
//! language is held in a task-local for the handler's future, so every
//! template, fragment and error page reads it without a field of its own.

mod ja;
mod vi;

use aegis_pr::Locale;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use crate::auth::Auth;

/// The cookie the switch sets; a year long, readable by no script.
pub const LANG_COOKIE: &str = "aegis_lang";
const LANG_MAX_AGE_S: i64 = 365 * 24 * 3600;

/// The masthead's switch: each language by its own name.
pub const SWITCH: [(&str, &str); 3] = [("en", "English"), ("ja", "日本語"), ("vi", "Tiếng Việt")];

tokio::task_local! {
    static LOCALE: Locale;
}

/// The request's language; `en` outside a request (a test rendering a
/// template on its own).
pub fn locale() -> Locale {
    LOCALE.try_with(|l| *l).unwrap_or_default()
}

/// The request's language code, for `<html lang>`.
pub fn lang() -> &'static str {
    locale().as_str()
}

/// The page's text for `en` in the request's language, `en` when the
/// catalog has none.
pub fn t(en: &'static str) -> &'static str {
    translate(locale(), en).unwrap_or(en)
}

/// The catalog's entry for `en` in `locale`; `None` for a missing one
/// (`en` has none: it is the key).
pub fn translate(locale: Locale, en: &str) -> Option<&'static str> {
    match locale {
        Locale::En => None,
        Locale::Ja => ja::get(en),
        Locale::Vi => vi::get(en),
    }
}

/// The first of `Accept-Language`'s languages, by its `q`, that is one of
/// ours; `en-US` counts as `en`.
fn from_accept_language(headers: &HeaderMap) -> Option<Locale> {
    let value = headers.get(header::ACCEPT_LANGUAGE)?.to_str().ok()?;
    let mut ranked: Vec<(f32, Locale)> = value
        .split(',')
        .filter_map(|part| {
            let mut bits = part.trim().split(';');
            let tag = bits.next()?.trim();
            let primary = tag.split('-').next()?;
            let locale: Locale = primary.parse().ok()?;
            let q = bits
                .find_map(|b| b.trim().strip_prefix("q="))
                .and_then(|q| q.parse::<f32>().ok())
                .unwrap_or(1.0);
            (q > 0.0).then_some((q, locale))
        })
        .collect();
    // Stable: of equal weight, the first named wins.
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    ranked.first().map(|(_, l)| *l)
}

/// The language of a request: its cookie, its browser's, the server's.
pub fn choose(headers: &HeaderMap, default: Locale) -> Locale {
    crate::auth::oidc::cookie_value(headers, LANG_COOKIE)
        .and_then(|v| v.parse().ok())
        .or_else(|| from_accept_language(headers))
        .unwrap_or(default)
}

/// Runs the rest of the request in its language; the answer may vary by
/// the cookie and the browser's languages, so caches are told.
pub async fn layer(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    let locale = choose(request.headers(), auth.default_locale());
    // The scripts and the stylesheet are the same in every language.
    let assets = request.uri().path().starts_with("/static/");
    let mut response = LOCALE.scope(locale, next.run(request)).await;
    if !assets {
        response.headers_mut().append(
            header::VARY,
            HeaderValue::from_static("Cookie, Accept-Language"),
        );
    }
    response
}

/// `/lang/{code}`: remembers the choice and goes back to the page it was
/// made on, a path of this site only.
pub async fn switch(Path(code): Path<String>, headers: HeaderMap) -> Response {
    let Ok(locale) = code.parse::<Locale>() else {
        return (StatusCode::NOT_FOUND, "no such language").into_response();
    };
    let back = back_to(&headers).unwrap_or_else(|| "/".to_owned());
    let mut response = Redirect::to(&back).into_response();
    if let Ok(c) = HeaderValue::from_str(&format!(
        "{LANG_COOKIE}={}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={LANG_MAX_AGE_S}",
        locale.as_str()
    )) {
        response.headers_mut().insert(header::SET_COOKIE, c);
    }
    response
}

/// The path and query of the `Referer` when it is this host's page; never
/// another site, nor a `//` that a browser reads as one.
fn back_to(headers: &HeaderMap) -> Option<String> {
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let rest = referer
        .strip_prefix("https://")
        .or_else(|| referer.strip_prefix("http://"))?;
    let (authority, path) = rest.split_at(rest.find('/')?);
    let ok = authority.eq_ignore_ascii_case(host)
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.starts_with("/\\")
        && !path.starts_with("/lang/")
        && path.chars().all(|c| c.is_ascii_graphic());
    ok.then(|| path.to_owned())
}

#[cfg(test)]
mod tests;
