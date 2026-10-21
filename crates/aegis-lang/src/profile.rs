//! Profile data (`profiles/<language>.toml`), its checks, the built-in set
//! and detection from marker files (ADR 0004 §1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::{CommandTemplate, LangError, Language, Placeholder};

/// Profiles shipped with the platform, one per [`Language`].
const BUILTIN: [(&str, &str); 6] = [
    (
        "profiles/rust.toml",
        include_str!("../../../profiles/rust.toml"),
    ),
    (
        "profiles/python.toml",
        include_str!("../../../profiles/python.toml"),
    ),
    (
        "profiles/go.toml",
        include_str!("../../../profiles/go.toml"),
    ),
    (
        "profiles/typescript.toml",
        include_str!("../../../profiles/typescript.toml"),
    ),
    (
        "profiles/java.toml",
        include_str!("../../../profiles/java.toml"),
    ),
    (
        "profiles/csharp.toml",
        include_str!("../../../profiles/csharp.toml"),
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
    /// Type check separate from lint (e.g. `tsc --noEmit`); compiled
    /// languages check types in `test`.
    #[serde(default)]
    pub typecheck: Option<CommandTemplate>,
    /// Runs one test, with `{test}`.
    pub repro_test: Option<CommandTemplate>,
    pub test: CommandTemplate,
    pub coverage: Option<CommandTemplate>,
    /// How `test` reports failing tests, so the gate can compare them with
    /// the base branch. Without it only the exit code is compared.
    #[serde(default)]
    pub test_output: Option<TestOutput>,
}

/// Test runner output the gate can read failing test names from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutput {
    /// Rust's built-in harness: `test name ... FAILED`.
    Libtest,
    /// pytest's short summary: `FAILED path::name`.
    Pytest,
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
    /// Hosts the fetch container may reach through the egress proxy
    /// (#66): the ecosystem's registry and nothing else.
    #[serde(default)]
    pub fetch_hosts: Vec<String>,
    /// Where the fetch step installs dependencies inside the tree (e.g.
    /// `.venv`). Kept outside the job's tree and mounted read-only into
    /// the gate's containers.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Build configuration the readers cannot check (e.g. `.npmrc`,
    /// `.cargo/config.toml`, `go.work`) that can change where dependencies
    /// come from or run code at build time. A patch that touches one is
    /// blocked (#83). Patterns as [`Profile::protected_changes`] matches them.
    #[serde(default)]
    pub protected: Vec<String>,
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
    /// Conventions for BP2 (#13): a file name in `prompts/conventions/`,
    /// built into the binary like the profiles.
    pub conventions: Option<String>,
}

/// `prompts/conventions/<name>`, the files a profile's `[prompt]
/// conventions` may name.
const CONVENTIONS: [(&str, &str); 2] = [
    (
        "rust.md",
        include_str!("../../../prompts/conventions/rust.md"),
    ),
    (
        "python.md",
        include_str!("../../../prompts/conventions/python.md"),
    ),
];

fn conventions_text(name: &str) -> Option<&'static str> {
    CONVENTIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
}

impl Profile {
    /// The language's code conventions for the system prompt (BP2), if the
    /// profile has them.
    pub fn conventions(&self) -> Option<&'static str> {
        self.prompt
            .conventions
            .as_deref()
            .and_then(conventions_text)
    }

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
        if let Some(name) = &self.prompt.conventions
            && conventions_text(name).is_none()
        {
            return Err(format!(
                "prompt.conventions: no file `{name}` in prompts/conventions"
            ));
        }
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
        if let Some(bad) = self
            .dependencies
            .protected
            .iter()
            .find(|p| p.is_empty() || p.contains(".."))
        {
            return Err(format!(
                "protected pattern `{bad}` must be non-empty and without `..`"
            ));
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

    /// Changed paths that match a `dependencies.protected` pattern, ignoring
    /// case (over-matching only blocks more). A
    /// pattern without `/` matches a file name in any directory; one with
    /// `/` matches the whole path from the repo root, `**` standing for any
    /// number of directories.
    pub fn protected_changes<P: AsRef<Path>>(&self, changed: &[P]) -> Vec<String> {
        changed
            .iter()
            .map(|p| p.as_ref().to_string_lossy().replace('\\', "/"))
            .filter(|path| {
                self.dependencies.protected.iter().any(|pattern| {
                    path_glob(&pattern.to_ascii_lowercase(), &path.to_ascii_lowercase())
                })
            })
            .collect()
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
        ("gate.typecheck", &gate.typecheck),
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

/// Path pattern for [`Profile::protected_changes`].
pub(crate) fn path_glob(pattern: &str, path: &str) -> bool {
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|i| segments(rest, &path[i..])),
            Some((seg, rest)) => path
                .split_first()
                .is_some_and(|(head, tail)| glob_match(seg, head) && segments(rest, tail)),
        }
    }
    let path = path.trim_start_matches("./");
    if !pattern.contains('/') {
        return path
            .rsplit('/')
            .next()
            .is_some_and(|name| glob_match(pattern, name));
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// `*` matches any run of characters; everything else is literal.
pub fn glob_match(pattern: &str, name: &str) -> bool {
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
    fn builtin_has_one_profile_per_language() {
        let p = builtin();
        assert_eq!(p.languages().collect::<Vec<_>>(), Language::ALL.to_vec());
        assert_eq!(p.get(Language::Rust).unwrap().display_name, "Rust");
        assert_eq!(
            Profiles::default().get(Language::Java),
            Err(LangError::MissingProfile(Language::Java))
        );
    }

    #[test]
    fn rust_profile_matches_the_design() {
        let rust = builtin().get(Language::Rust).unwrap().clone();
        assert_eq!(
            rust.gate.test.argv(),
            ["cargo", "test", "--workspace", "--no-fail-fast"]
        );
        assert_eq!(rust.gate.test_output, Some(TestOutput::Libtest));
        assert!(
            rust.conventions()
                .unwrap()
                .starts_with("# Rust conventions")
        );
        let python = builtin().get(Language::Python).unwrap().clone();
        assert!(
            python
                .conventions()
                .unwrap()
                .starts_with("# Python conventions")
        );
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
        assert_eq!(p.detect(["package.json"]), vec![Language::TypeScript]);
        assert_eq!(p.detect(["pom.xml"]), vec![Language::Java]);
        assert_eq!(p.detect(["App.sln"]), vec![Language::CSharp]);
        assert!(p.detect(["Makefile", "README.md"]).is_empty());
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
    fn path_glob_matches_names_anywhere_and_rooted_paths() {
        assert!(path_glob(".npmrc", ".npmrc"));
        assert!(path_glob(".npmrc", "packages/app/.npmrc"));
        assert!(path_glob(".cargo/config.toml", ".cargo/config.toml"));
        assert!(!path_glob(
            ".cargo/config.toml",
            "crates/a/.cargo/config.toml"
        ));
        assert!(path_glob(
            "**/.cargo/config.toml",
            "crates/a/.cargo/config.toml"
        ));
        assert!(path_glob("vendor/**", "vendor/github.com/a/b/x.go"));
        assert!(path_glob("vendor/**", "./vendor/modules.txt"));
        assert!(!path_glob("vendor/**", "src/vendor.go"));
        assert!(path_glob(
            "gradle/wrapper/*",
            "gradle/wrapper/gradle-wrapper.jar"
        ));
    }

    #[test]
    fn every_profile_protects_build_configuration_it_cannot_read() {
        let p = builtin();
        let cases: &[(Language, &[&str], &[&str])] = &[
            (
                Language::Rust,
                &[
                    ".cargo/config.toml",
                    "crates/a/.cargo/config",
                    "rust-toolchain.toml",
                ],
                &["src/lib.rs", "Cargo.toml"],
            ),
            (
                Language::Python,
                &["pip.conf", "uv.toml"],
                &["pyproject.toml", "app/main.py"],
            ),
            (
                Language::Go,
                &[
                    "go.work",
                    "go.work.sum",
                    "vendor/modules.txt",
                    "vendor/github.com/a/b/x.go",
                ],
                &["go.mod", "cmd/vendor.go"],
            ),
            (
                Language::TypeScript,
                &[
                    ".npmrc",
                    "packages/app/.npmrc",
                    ".yarnrc.yml",
                    ".pnpmfile.cjs",
                ],
                &["package.json", "src/npmrc.ts"],
            ),
            (
                Language::Java,
                &[
                    ".mvn/maven.config",
                    ".mvn/jvm.config",
                    "settings.xml",
                    "build.gradle.kts",
                    "gradle/wrapper/gradle-wrapper.properties",
                    "gradlew",
                ],
                &["pom.xml", "src/main/java/A.java"],
            ),
            (
                Language::CSharp,
                &["Directory.Build.rsp", "build.rsp"],
                &["App.csproj", "nuget.config"],
            ),
        ];
        for (language, blocked, allowed) in cases {
            let profile = p.get(*language).unwrap();
            assert_eq!(
                profile.protected_changes(blocked).len(),
                blocked.len(),
                "{language}: {blocked:?}"
            );
            assert!(
                profile.protected_changes(allowed).is_empty(),
                "{language}: {allowed:?}"
            );
        }
        // Case does not help a patch through.
        assert_eq!(
            p.get(Language::TypeScript)
                .unwrap()
                .protected_changes(&[".NPMRC"])
                .len(),
            1
        );
        assert!(
            p.get(Language::CSharp)
                .unwrap()
                .touches_dependencies(&["NuGet.Config", "src/App.FSPROJ"])
        );
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
                "[\"cargo\", \"test\", \"--workspace\", \"--no-fail-fast\"]",
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
            &t.replace(
                "conventions = \"rust.md\"",
                "conventions = \"../secrets.md\"",
            ),
            "no file `../secrets.md` in prompts/conventions",
        );
        rejects(
            &t.replace(
                "test = [\"cargo\", \"test\", \"--workspace\", \"--no-fail-fast\"]\n",
                "",
            ),
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
