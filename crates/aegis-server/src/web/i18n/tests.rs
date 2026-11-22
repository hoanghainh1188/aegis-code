use super::*;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

#[test]
fn the_cookie_then_the_browser_then_the_server_choose() {
    let en = Locale::En;
    assert_eq!(choose(&headers(&[]), Locale::Ja), Locale::Ja);
    assert_eq!(
        choose(
            &headers(&[("accept-language", "fr-FR, ja;q=0.8, vi;q=0.9")]),
            en
        ),
        Locale::Vi
    );
    assert_eq!(
        choose(&headers(&[("accept-language", "ja-JP")]), en),
        Locale::Ja
    );
    assert_eq!(
        choose(&headers(&[("accept-language", "fr, de")]), Locale::Vi),
        Locale::Vi
    );
    assert_eq!(choose(&headers(&[("accept-language", "ja;q=0")]), en), en);
    assert_eq!(
        choose(
            &headers(&[("cookie", "a=b; aegis_lang=vi"), ("accept-language", "ja")]),
            en
        ),
        Locale::Vi
    );
    // A cookie that is not one of ours is ignored.
    assert_eq!(
        choose(
            &headers(&[("cookie", "aegis_lang=xx"), ("accept-language", "ja")]),
            en
        ),
        Locale::Ja
    );
}

#[test]
fn the_switch_goes_back_only_to_this_sites_pages() {
    let back =
        |referer: &str| back_to(&headers(&[("host", "aegis.example"), ("referer", referer)]));
    assert_eq!(
        back("https://aegis.example/jobs?status=done").as_deref(),
        Some("/jobs?status=done")
    );
    assert_eq!(back("https://evil.example/jobs"), None);
    assert_eq!(back("https://aegis.example//evil.example"), None);
    assert_eq!(back("https://aegis.example/\\evil.example"), None);
    assert_eq!(back("https://aegis.example/lang/ja"), None);
    assert_eq!(back("javascript:alert(1)"), None);
}

#[tokio::test]
async fn a_request_reads_its_own_language_and_english_is_the_fallback() {
    assert_eq!(t("Jobs"), "Jobs");
    LOCALE
        .scope(Locale::Ja, async {
            assert_eq!(lang(), "ja");
            assert_eq!(t("no such text"), "no such text");
        })
        .await;
    assert_eq!(lang(), "en");
}

/// Untranslated on purpose: names, units and examples, the same in every
/// language.
const SAME_EVERYWHERE: &[&str] = &[
    "aegis",
    "ledger",
    "UTC",
    "USD",
    "AI",
    "MTTR",
    "PR",
    "PRs",
    "KPIs",
    "AMS",
    "URL",
    "name@example.com",
    "ci-bot",
    "requests",
    "GHSA-9wx4-h78v-vm56",
    "https://github.com/owner/name",
    "AEGIS_GITHUB_TOKEN",
    "application/json",
    "laptop",
    "English",
    "日本語",
    "Tiếng",
    "Việt",
];

/// Elements whose text is code or markup, not words to translate.
const NOT_WORDS: &[&str] = &["code", "kbd", "pre", "samp", "script", "style"];

/// The strings `src` passes to `crate::t`, unescaped.
fn wrapped(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find("crate::t(\"") {
        rest = &rest[at + "crate::t(\"".len()..];
        let mut text = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            match chars.next() {
                Some((_, '\\')) => text.extend(chars.next().map(|(_, c)| c)),
                Some((i, '"')) => break i,
                Some((_, c)) => text.push(c),
                None => panic!("an unclosed crate::t(\"…"),
            }
        };
        rest = &rest[end..];
        found.push(text);
    }
    found
}

/// `src` without its template tags, expressions and comments.
fn without_template(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(at) = rest.find('{') {
        let close = match rest[at..].chars().nth(1) {
            Some('{') => "}}",
            Some('%') => "%}",
            Some('#') => "#}",
            _ => {
                out.push_str(&rest[..=at]);
                rest = &rest[at + 1..];
                continue;
            }
        };
        out.push_str(&rest[..at]);
        let end = rest[at..].find(close).expect("an unclosed template tag") + at + 2;
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The English left as it was: text and labelling attributes outside
/// `crate::t`, but for [`SAME_EVERYWHERE`] and [`NOT_WORDS`].
fn bare_english(src: &str) -> Vec<String> {
    let html = without_template(src);
    let mut found = Vec::new();
    let mut skipping: Vec<String> = Vec::new();
    let words = |text: &str, found: &mut Vec<String>| {
        for word in text.split_whitespace() {
            let letters = word.chars().filter(char::is_ascii_alphabetic).count();
            let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
            if letters >= 2
                && !SAME_EVERYWHERE.contains(&trimmed)
                && !SAME_EVERYWHERE.contains(&word)
            {
                found.push(word.to_owned());
            }
        }
    };
    let mut rest = html.as_str();
    while !rest.is_empty() {
        let next = rest.find('<').unwrap_or(rest.len());
        if skipping.is_empty() {
            words(&rest[..next], &mut found);
        }
        rest = &rest[next..];
        if rest.is_empty() {
            break;
        }
        let end = rest.find('>').map_or(rest.len(), |e| e + 1);
        let tag = &rest[..end];
        rest = &rest[end..];
        let name: String = tag
            .trim_start_matches('<')
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        if tag.starts_with("</") {
            if skipping.last() == Some(&name) {
                skipping.pop();
            }
        } else if NOT_WORDS.contains(&name.as_str()) && !tag.ends_with("/>") {
            skipping.push(name);
        }
        for attr in [
            "placeholder",
            "title",
            "aria-label",
            "aria-description",
            "alt",
            "label",
            "hx-confirm",
        ] {
            let key = format!(" {attr}=\"");
            if let Some(at) = tag.find(&key) {
                let value = &tag[at + key.len()..];
                let value = &value[..value.find('"').unwrap_or(value.len())];
                words(value, &mut found);
            }
        }
    }
    found
}

fn templates() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut all: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    all.sort();
    all
}

#[test]
fn every_page_text_has_a_japanese_and_a_vietnamese_entry() {
    let mut missing = Vec::new();
    for (name, src) in templates() {
        for text in wrapped(&src) {
            for locale in [Locale::Ja, Locale::Vi] {
                if translate(locale, &text).is_none() {
                    missing.push(format!("{name} {locale}: {text:?}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "no translation:\n{}",
        missing.join("\n")
    );
}

#[test]
fn no_page_has_english_outside_the_catalog() {
    let mut bare = Vec::new();
    for (name, src) in templates() {
        for word in bare_english(&src) {
            bare.push(format!("{name}: {word}"));
        }
    }
    assert!(bare.is_empty(), "not in crate::t:\n{}", bare.join("\n"));
}

#[test]
fn the_scanners_see_what_they_should() {
    let src = r#"<p title="Hi there">{{ crate::t("a \"b\"") }} Bare <code>left alone</code> {% if x > 1 %}aegis{% endif %}</p>"#;
    assert_eq!(wrapped(src), ["a \"b\""]);
    assert_eq!(bare_english(src), ["Hi", "there", "Bare"]);
}
