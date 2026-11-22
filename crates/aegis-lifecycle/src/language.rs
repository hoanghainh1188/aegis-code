//! What the model is told about the language it writes in (#588). The line
//! goes at the end of the first user message (BP3), never in the system
//! prompt (BP2), so repos in different locales share the cached prefix.

use aegis_pr::Locale;

/// The language a repo writes in (#589): its own `locale`, else its
/// tenant's `default_locale`, else the server's `[web] default_locale`
/// (itself `en` when unset). A tenant's code is checked by the database;
/// one this binary does not know is an error, not a silent default.
pub fn resolve_locale(
    repo: Option<Locale>,
    tenant: Option<&str>,
    server: Locale,
) -> Result<Locale, String> {
    if let Some(locale) = repo {
        return Ok(locale);
    }
    match tenant {
        Some(code) => code
            .parse()
            .map_err(|e| format!("the tenant's default_locale: {e}")),
        None => Ok(server),
    }
}

/// One line asking for the people-facing parts of the answer in `locale`.
/// Code, paths and quoted excerpts stay as they are: the platform checks
/// each excerpt against the line it cites.
pub(crate) fn directive(locale: Locale) -> String {
    format!(
        "\nWrite every explanation meant for people (the root cause, change notes, \
         review replies, findings, steps and questions) in {}. Keep code, identifiers, \
         file paths and quoted excerpts exactly as they are in the source.\n",
        locale.language()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repo_wins_then_the_tenant_then_the_server() {
        let (en, ja, vi) = (Locale::En, Locale::Ja, Locale::Vi);
        assert_eq!(resolve_locale(Some(ja), Some("vi"), en), Ok(ja));
        assert_eq!(resolve_locale(None, Some("vi"), en), Ok(vi));
        assert_eq!(resolve_locale(None, None, ja), Ok(ja));
        assert_eq!(resolve_locale(None, None, Locale::default()), Ok(en));
        assert!(resolve_locale(None, Some("fr"), en).is_err());
    }

    #[test]
    fn names_the_language_and_keeps_quotes_verbatim() {
        for (locale, name) in [
            (Locale::En, "in English."),
            (Locale::Ja, "in Japanese."),
            (Locale::Vi, "in Vietnamese."),
        ] {
            let line = directive(locale);
            assert!(line.contains(name), "{line}");
            assert!(line.contains("quoted excerpts exactly"), "{line}");
        }
    }
}
