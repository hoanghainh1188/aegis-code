//! What the model is told about the language it writes in (#588). The line
//! goes at the end of the first user message (BP3), never in the system
//! prompt (BP2), so repos in different locales share the cached prefix.

use aegis_pr::Locale;

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
