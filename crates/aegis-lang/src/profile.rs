//! Profile data (`profiles/<language>.toml`), its checks, the built-in set
//! and detection from marker files (ADR 0004 §1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::{CommandTemplate, LangError, Language, Placeholder};

/// Profiles shipped with the platform. The other languages get theirs in
/// #67–#70.
const BUILTIN: [(&str, &str); 2] = [
    (
        "profiles/rust.toml",
        include_str!("../../../profiles/rust.toml"),
    ),
    (
        "profiles/python.toml",
        include_str!("../../../profiles/python.toml"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub language: Language,
    pub display_name: String,
    pub detect: Detect,
    #[serde(default)]
    pub aci: Aci,
    #[serde(default)]
    pub edit: Edit,
    pub gate: GateSteps,
    pub coverage: Option<Coverage>,
    pub dependencies: Dependencies,
    pub sandbox: Sandbox,
    #[serde(default)]
    pub sonar: Sonar,
    #[serde(default)]
    pub prompt: Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// File names at the repo root; `*` matches any run of characters.
    pub markers: Vec<String>,
    /// Source file extensions, without the dot.
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aci {
    /// Language server, optional (ADR 0004 §1).
    pub lsp: Option<CommandTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// Formatter run on the edited file, with `{file}`.
    pub format: Option<CommandTemplate>,
}

/// Gate steps in order (Thiết kế §6.1). Only `test` is mandatory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateSteps {
    pub format: Option<CommandTemplate>,
    pub lint: Option<CommandTemplate>,
    /// Runs one test, with `{test}`.
    pub repro_test: Option<CommandTemplate>,
    pub test: CommandTemplate,
    pub coverage: Option<CommandTemplate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageFormat {
    Lcov,
    Cobertura,
    Jacoco,
    GoCoverprofile,
    OpenCover,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub format: CoverageFormat,
    /// Report path, relative to the repo root.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    pub manifests: Vec<String>,
    pub lockfiles: Vec<String>,
    /// Source string of the official registry as the lockfile reader
    /// reports it; anything else counts as an unofficial source.
    pub official_source: String,
    /// Fetch command, run in its own container before the gate (#66).
    pub fetch: Option<CommandTemplate>,
    /// Advisory and license audit of one lockfile, with `{lockfile}` and
    /// optionally `{licenses}`. Needs network (license data from deps.dev),
    /// so it runs with the fetch step (#66), not in the offline gate.
    pub audit: Option<CommandTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    pub image: String,
    pub cache_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sonar {
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    /// Conventions file for BP2 (#13).
    pub conventions: Option<String>,
}

impl Profile {
    /// Parses and checks one profile. `source_name` is only used in errors.
    pub fn from_toml(source_name: &str, text: &str) -> Result<Self, LangError> {
        let invalid = |message: String| LangError::InvalidProfile {
            source_name: source_name.to_owned(),
            message,
        };
        let profile: Self = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        profile.check().map_err(invalid)?;
        Ok(profile)
    }

    fn check(&self) -> Result<(), String> {
        let field = |name: &str, c: &CommandTemplate, allowed: &[Placeholder], required| {
            c.check_placeholders(allowed, required)
                .map_err(|e| format!("{name}: {e}"))
        };
        if self.detect.markers.is_empty() || self.detect.extensions.is_empty() {
            return Err("detect.markers and detect.extensions must not be empty".into());
        }
        if let Some(bad) = self
            .detect
            .extensions
            .iter()
            .find(|e| e.is_empty() || e.contains('.'))
        {
            return Err(format!(
                "extension `{bad}` must be non-empty and without a dot"
            ));
        }
        if let Some(c) = &self.aci.lsp {
            field("aci.lsp", c, &[], &[])?;
        }
        if let Some(c) = &self.edit.format {
            field("edit.format", c, &[Placeholder::File], &[Placeholder::File])?;
        }
        check_gate(&self.gate)?;
        if self.gate.coverage.is_some() != self.coverage.is_some() {
            return Err("gate.coverage and [coverage] must be set together".into());
        }
        if let Some(c) = &self.dependencies.fetch {
            field("dependencies.fetch", c, &[], &[])?;
        }
        if let Some(c) = &self.dependencies.audit {
            check_audit(c).map_err(|e| format!("dependencies.audit: {e}"))?;
        }
        if self.dependencies.official_source.trim().is_empty() {
            return Err("dependencies.official_source must not be empty".into());
        }
        check_image(&self.sandbox.image).map_err(|e| format!("sandbox.image: {e}"))?;
        let escapes = |p: &String| Path::new(p).components().any(|c| c.as_os_str() == "..");
        if let Some(bad) = self.sandbox.cache_dirs.iter().find(|p| escapes(p)) {
            return Err(format!("cache dir `{bad}` must not contain `..`"));
        }
        if let Some(c) = &self.coverage {
            check_repo_path(&c.path).map_err(|e| format!("coverage.path: {e}"))?;
        }
        Ok(())
    }

    /// Whether `path` is a source file of this language, by extension
    /// (case-insensitive).
    pub fn owns_file(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            self.detect
                .extensions
                .iter()
                .any(|x| x.eq_ignore_ascii_case(e))
        })
    }

    fn matches_marker(&self, file_name: &str) -> bool {
        self.detect.markers.iter().any(|m| glob_match(m, file_name))
    }
}

/// Image names become a `docker` argument: no option-like or split values.
pub(crate) fn check_image(image: &str) -> Result<(), String> {
    if image.is_empty() {
        Err("must not be empty".into())
    } else if image.starts_with('-') {
        Err("must not start with `-`".into())
    } else if image.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err("must not contain whitespace or control characters".into())
    } else {
        Ok(())
    }
}

/// Paths the gate reads inside the repo: relative, no `..`.
pub(crate) fn check_repo_path(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if path.is_empty() {
        Err("must not be empty".into())
    } else if p.is_absolute() || path.starts_with('/') || path.starts_with('\\') {
        Err(format!("`{path}` must be relative to the repo root"))
    } else if p.components().any(|c| c.as_os_str() == "..") {
        Err(format!("`{path}` must not contain `..`"))
    } else {
        Ok(())
    }
}

pub(crate) fn check_audit(c: &CommandTemplate) -> Result<(), String> {
    c.check_placeholders(
        &[Placeholder::Lockfile, Placeholder::Licenses],
        &[Placeholder::Lockfile],
    )
}

pub(crate) fn check_gate(gate: &GateSteps) -> Result<(), String> {
    let plain = [
        ("gate.format", &gate.format),
        ("gate.lint", &gate.lint),
        ("gate.coverage", &gate.coverage),
    ];
    for (name, c) in plain {
        if let Some(c) = c {
            c.check_placeholders(&[], &[])
                .map_err(|e| format!("{name}: {e}"))?;
        }
    }
    gate.test
        .check_placeholders(&[], &[])
        .map_err(|e| format!("gate.test: {e}"))?;
    if let Some(c) = &gate.repro_test {
        c.check_placeholders(&[Placeholder::Test], &[Placeholder::Test])
            .map_err(|e| format!("gate.repro_test: {e}"))?;
    }
    Ok(())
}

/// `*` matches any run of characters; everything else is literal.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.len() >= head.len()
                && name.starts_with(head)
                && (0..=name.len() - head.len()).any(|i| {
                    name.is_char_boundary(head.len() + i)
                        && glob_match(tail, &name[head.len() + i..])
                })
        }
    }
}

/// A set of profiles, at most one per language.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profiles(BTreeMap<Language, Profile>);

impl Profiles {
    pub fn builtin() -> Result<Self, LangError> {
        BUILTIN
            .iter()
            .map(|(name, text)| Profile::from_toml(name, text))
            .collect::<Result<Vec<_>, _>>()
            .and_then(Self::from_profiles)
    }

    /// Each language at most once, and no extension claimed by two
    /// languages, so [`Self::language_of`] is unambiguous.
    pub fn from_profiles(profiles: Vec<Profile>) -> Result<Self, LangError> {
        profiles
            .into_iter()
            .try_fold(Self::default(), |Self(mut map), p| {
                let language = p.language;
                let invalid = |message: String| LangError::InvalidProfile {
                    source_name: language.id().to_owned(),
                    message,
                };
                let clash = map.values().find_map(|other| {
                    p.detect
                        .extensions
                        .iter()
                        .find(|e| {
                            other
                                .detect
                                .extensions
                                .iter()
                                .any(|o| o.eq_ignore_ascii_case(e))
                        })
                        .map(|e| (other.language, e.clone()))
                });
                if let Some((other, ext)) = clash {
                    return Err(invalid(format!(
                        "extension `{ext}` is already used by {other}"
                    )));
                }
                match map.insert(language, p) {
                    Some(_) => Err(invalid("duplicate profile".into())),
                    None => Ok(Self(map)),
                }
            })
    }

    pub fn get(&self, language: Language) -> Result<&Profile, LangError> {
        self.0
            .get(&language)
            .ok_or(LangError::MissingProfile(language))
    }

    pub fn languages(&self) -> impl Iterator<Item = Language> + '_ {
        self.0.keys().copied()
    }

    /// Languages whose marker files appear among the repo root's entries.
    /// Only a suggestion: the repo declares its profile in `RepoConfig`.
    pub fn detect<'a>(&self, root_entries: impl IntoIterator<Item = &'a str>) -> Vec<Language> {
        let entries: Vec<&str> = root_entries.into_iter().collect();
        self.0
            .values()
            .filter(|p| entries.iter().any(|e| p.matches_marker(e)))
            .map(|p| p.language)
            .collect()
    }

    /// Language whose extensions include `path`'s, for picking a grammar.
    pub fn language_of(&self, path: &Path) -> Option<Language> {
        self.0
            .values()
            .find(|p| p.owns_file(path))
            .map(|p| p.language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Profiles {
        Profiles::builtin().unwrap()
    }

    #[test]
    fn builtin_has_rust_and_python() {
        let p = builtin();
        assert_eq!(
            p.languages().collect::<Vec<_>>(),
            vec![Language::Rust, Language::Python]
        );
        assert_eq!(p.get(Language::Rust).unwrap().display_name, "Rust");
        assert_eq!(
            p.get(Language::Java),
            Err(LangError::MissingProfile(Language::Java))
        );
    }

    #[test]
    fn rust_profile_matches_the_design() {
        let rust = builtin().get(Language::Rust).unwrap().clone();
        assert_eq!(rust.gate.test.argv(), ["cargo", "test", "--workspace"]);
        assert!(
            rust.gate
                .repro_test
                .as_ref()
                .unwrap()
                .uses(Placeholder::Test)
        );
        assert_eq!(rust.coverage.unwrap().format, CoverageFormat::Lcov);
        assert_eq!(rust.dependencies.lockfiles, vec!["Cargo.lock"]);
    }

    #[test]
    fn python_profile_matches_the_design() {
        let py = builtin().get(Language::Python).unwrap().clone();
        assert_eq!(py.coverage.unwrap().format, CoverageFormat::Cobertura);
        assert_eq!(py.dependencies.official_source, "https://pypi.org/simple");
        assert_eq!(
            py.sonar.properties["sonar.python.coverage.reportPaths"],
            "coverage.xml"
        );
    }

    #[test]
    fn detects_languages_from_root_entries() {
        let p = builtin();
        assert_eq!(p.detect(["Cargo.toml", "README.md"]), vec![Language::Rust]);
        assert_eq!(p.detect(["requirements-dev.txt"]), vec![Language::Python]);
        assert_eq!(
            p.detect(["pyproject.toml", "Cargo.toml"]),
            vec![Language::Rust, Language::Python]
        );
        assert!(p.detect(["package.json"]).is_empty());
    }

    #[test]
    fn maps_files_to_languages() {
        let p = builtin();
        assert_eq!(p.language_of(Path::new("src/lib.rs")), Some(Language::Rust));
        assert_eq!(
            p.language_of(Path::new("pkg/stubs.pyi")),
            Some(Language::Python)
        );
        assert_eq!(p.language_of(Path::new("Makefile")), None);
    }

    #[test]
    fn glob_matches_like_a_file_pattern() {
        assert!(glob_match("requirements*.txt", "requirements.txt"));
        assert!(glob_match("requirements*.txt", "requirements-dev.txt"));
        assert!(glob_match("*.csproj", "App.csproj"));
        assert!(!glob_match("*.csproj", "App.csproj.bak"));
        assert!(!glob_match("Cargo.toml", "cargo.toml"));
    }

    fn rust_text() -> &'static str {
        BUILTIN[0].1
    }

    fn rejects(text: &str, needle: &str) {
        let err = Profile::from_toml("test.toml", text)
            .unwrap_err()
            .to_string();
        assert!(err.contains(needle), "expected `{needle}` in: {err}");
    }

    #[test]
    fn rejects_bad_profiles() {
        let t = rust_text();
        rejects(
            &t.replace("\"--exact\", \"{test}\"", "\"--exact\""),
            "gate.repro_test: must contain `{test}`",
        );
        rejects(
            &t.replace(
                "[\"cargo\", \"test\", \"--workspace\"]",
                "[\"cargo\", \"test\", \"{file}\"]",
            ),
            "gate.test",
        );
        rejects(
            &t.replace("\"2021\", \"{file}\"", "\"2021\", \"{fiel}\""),
            "unknown placeholder",
        );
        rejects(
            &t.replace("extensions = [\"rs\"]", "extensions = [\".rs\"]"),
            "without a dot",
        );
        rejects(
            &t.replace("\"target\",", "\"../escape\","),
            "must not contain `..`",
        );
        rejects(
            &t.replace("format = \"lcov\"\npath = \"lcov.info\"", "")
                .replace("[coverage]", ""),
            "set together",
        );
        rejects(
            &t.replace("language = \"rust\"", "language = \"cobol\""),
            "unknown variant",
        );
        rejects(&format!("{t}\n[extra]\nx = 1\n"), "unknown field");
        rejects(
            &t.replace("test = [\"cargo\", \"test\", \"--workspace\"]\n", ""),
            "missing field `test`",
        );
    }

    #[test]
    fn rejects_duplicate_profiles_and_shared_extensions() {
        let rust = Profile::from_toml("a", rust_text()).unwrap();
        let err = Profiles::from_profiles(vec![rust.clone(), rust.clone()]).unwrap_err();
        assert!(err.to_string().contains("already used by rust"));
        let mut python = builtin().get(Language::Python).unwrap().clone();
        python.detect.extensions.push("RS".into());
        let err = Profiles::from_profiles(vec![rust, python]).unwrap_err();
        assert!(err.to_string().contains("`RS` is already used by rust"));
    }

    #[test]
    fn rejects_unsafe_image_and_coverage_path() {
        let t = rust_text();
        let image = "image = \"aegis-sandbox-rust\"";
        rejects(
            &t.replace(image, "image = \"--privileged\""),
            "must not start with `-`",
        );
        rejects(
            &t.replace(image, "image = \"img --network=host\""),
            "whitespace",
        );
        let path = "path = \"lcov.info\"";
        rejects(
            &t.replace(path, "path = \"../../etc/x\""),
            "must not contain `..`",
        );
        rejects(&t.replace(path, "path = \"/etc/passwd\""), "relative");
    }

    #[test]
    fn extension_match_ignores_case() {
        let p = builtin();
        assert_eq!(
            p.language_of(Path::new("src/MAIN.RS")),
            Some(Language::Rust)
        );
    }
}
