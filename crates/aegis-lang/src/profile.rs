//! Profile data (`profiles/<language>.toml`), its checks, the built-in set
//! and detection from marker files (ADR 0004 §1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// Language server, optional (ADR 0004 §1); speaks LSP on stdio.
    pub lsp: Option<CommandTemplate>,
    /// The server's own settings, sent as `initializationOptions` (#89):
    /// rust-analyzer's `checkOnSave = false`, so it does not build the repo.
    #[serde(default)]
    pub lsp_options: Option<LspOptions>,
}

/// A TOML table passed to the language server as JSON.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(transparent)]
pub struct LspOptions(pub toml::Table);

// The table holds settings, never NaN floats to compare.
impl Eq for LspOptions {}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// Formatter run on the edited file, with `{file}`.
    pub format: Option<CommandTemplate>,
    /// The formatter reads the file on stdin and prints it formatted on
    /// stdout; `{file}` is then optional (a name hint). rustfmt given a
    /// path also rewrites the `mod` files it declares; given stdin it
    /// formats that file only (#88).
    #[serde(default)]
    pub stdin: bool,
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
    /// Repos the profile's `coverage` does not run on (C# under
    /// Microsoft.Testing.Platform, #228): the step is skipped there unless
    /// the repo gives its own command.
    #[serde(default)]
    pub coverage_skip: Vec<FileMatch>,
    /// How `test` reports failing tests, so the gate can compare them with
    /// the base branch. Without it only the exit code is compared.
    #[serde(default)]
    pub test_output: Option<TestOutput>,
}

/// A file named `file` in the part's directory or the nearest one above it,
/// up to the repo root, whose text matches the regex `pattern`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMatch {
    pub file: String,
    pub pattern: String,
}

impl FileMatch {
    /// Whether the nearest `file` from `dir` up to `root` matches.
    pub fn matches(&self, dir: &Path, root: &Path) -> bool {
        let Ok(pattern) = regex::Regex::new(&self.pattern) else {
            return false;
        };
        let nearest = dir
            .ancestors()
            .take_while(|d| d.starts_with(root))
            .map(|d| d.join(&self.file))
            .find(|f| f.is_file());
        nearest
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_some_and(|text| pattern.is_match(&text))
    }

    fn check(&self) -> Result<(), String> {
        let name = self.file.as_str();
        if matches!(name, "" | "." | "..") || name.contains(['/', '\\']) {
            return Err(format!("`{}` is not a file name", self.file));
        }
        regex::Regex::new(&self.pattern)
            .map(drop)
            .map_err(|e| e.to_string())
    }
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
    /// Advisory and license audit of one lockfile the patch changed, with
    /// `{lockfile}` or `{lockfile_dir}`, and optionally `{licenses}`,
    /// `{deny_config}`, `{osv_config}` (the repo's policy, #159). Needs
    /// network (advisory databases, license data from deps.dev), so it runs
    /// with the fetch step (#66), through `audit_hosts`.
    pub audit: Option<CommandTemplate>,
    /// Hosts the audit may reach, HTTPS only (advisory databases, license
    /// data); not the fetch step's.
    #[serde(default)]
    pub audit_hosts: Vec<String>,
    /// The resolved dependency tree, for builds that lock nothing (Maven,
    /// #80): compared before and after the patch like a lockfile.
    #[serde(default)]
    pub tree: Option<DependencyTree>,
}

/// A command that writes the resolved dependency tree to `{output}`. The
/// gate runs it in the fetch container (#66) on the base and on the patch,
/// before fetching, and reads both with the lockfile reader for `file`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyTree {
    /// Names the format, as [`crate::read_lockfile`] knows it
    /// (`maven-dependency-tree.txt`).
    pub file: String,
    pub command: CommandTemplate,
    /// File name patterns whose change needs the tree, such as `pom.xml`.
    /// Manifests listed here may change without a lockfile.
    pub on_change: Vec<String>,
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
    pub scanner: SonarScanner,
    /// Build run between `begin` and `end` of [`SonarScanner::Dotnet`],
    /// in the gate's sandbox; required for it, refused otherwise.
    #[serde(default)]
    pub build: Option<CommandTemplate>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

/// How the gate's Sonar step analyses the repo (design §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SonarScanner {
    /// SonarScanner CLI (`sonar-scanner`) on the tree as it is.
    #[default]
    Cli,
    /// SonarScanner for .NET: `begin`, the profile's build, `end`.
    Dotnet,
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
            let required: &[Placeholder] = if self.edit.stdin {
                &[]
            } else {
                &[Placeholder::File]
            };
            field("edit.format", c, &[Placeholder::File], required)?;
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
        if let Some(t) = &self.dependencies.tree {
            check_tree(self.language, t).map_err(|e| format!("dependencies.tree: {e}"))?;
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
        match (self.sonar.scanner, &self.sonar.build) {
            (SonarScanner::Dotnet, None) => {
                return Err("sonar.build is required with scanner = \"dotnet\"".into());
            }
            (SonarScanner::Cli, Some(_)) => {
                return Err("sonar.build only goes with scanner = \"dotnet\"".into());
            }
            (_, Some(c)) => field("sonar.build", c, &[], &[])?,
            (_, None) => {}
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
        &[
            Placeholder::Lockfile,
            Placeholder::LockfileDir,
            Placeholder::Licenses,
            Placeholder::DenyConfig,
            Placeholder::OsvConfig,
        ],
        &[],
    )?;
    if c.uses(Placeholder::Lockfile) || c.uses(Placeholder::LockfileDir) {
        Ok(())
    } else {
        Err("must contain `{lockfile}` or `{lockfile_dir}`".into())
    }
}

pub(crate) fn check_tree(language: Language, tree: &DependencyTree) -> Result<(), String> {
    tree.command
        .check_placeholders(&[Placeholder::Output], &[Placeholder::Output])?;
    if let Err(LangError::UnsupportedLockfile { .. }) =
        crate::read_lockfile(language, &tree.file, "")
    {
        return Err(format!("no reader for `{}`", tree.file));
    }
    if tree.on_change.is_empty() || tree.on_change.iter().any(|p| p.is_empty()) {
        return Err("on_change must list file name patterns".into());
    }
    Ok(())
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
    for m in &gate.coverage_skip {
        m.check().map_err(|e| format!("gate.coverage_skip: {e}"))?;
    }
    if !gate.coverage_skip.is_empty() && gate.coverage.is_none() {
        return Err("gate.coverage_skip needs gate.coverage".into());
    }
    Ok(())
}

/// Path pattern for [`Profile::protected_changes`] and coverage report paths.
pub fn path_glob(pattern: &str, path: &str) -> bool {
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

/// Finds all regular files under `root` that match `pattern`.
///
/// Supports literal paths, wildcards (`*`) within path segments (such as
/// `TestResults/*/coverage.opencover.xml`), and `**` across directory
/// levels. Results are sorted.
pub fn find_reports(root: &Path, pattern: &str) -> Vec<PathBuf> {
    fn match_segments(current: &Path, segments: &[&str], results: &mut Vec<PathBuf>) {
        let Some((seg, rest)) = segments.split_first() else {
            if current.is_file() {
                results.push(current.to_path_buf());
            }
            return;
        };
        if *seg == "**" {
            match_segments(current, rest, results);
            if let Ok(entries) = std::fs::read_dir(current) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        match_segments(&path, segments, results);
                    }
                }
            }
        } else if seg.contains('*') {
            if let Ok(entries) = std::fs::read_dir(current) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if glob_match(seg, &name_str) {
                        let path = entry.path();
                        if rest.is_empty() {
                            if path.is_file() {
                                results.push(path);
                            }
                        } else if path.is_dir() {
                            match_segments(&path, rest, results);
                        }
                    }
                }
            }
        } else {
            let next = current.join(seg);
            if rest.is_empty() {
                if next.is_file() {
                    results.push(next);
                }
            } else if next.is_dir() {
                match_segments(&next, rest, results);
            }
        }
    }

    let pattern = pattern.trim_start_matches("./");
    if pattern.is_empty() {
        return Vec::new();
    }
    if !pattern.contains('*') {
        let direct = root.join(pattern);
        return if direct.is_file() {
            vec![direct]
        } else {
            Vec::new()
        };
    }
    let segments: Vec<&str> = pattern
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut results = Vec::new();
    match_segments(root, &segments, &mut results);
    results.sort();
    results.dedup();
    results
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
        assert_eq!(py.sonar.scanner, SonarScanner::Cli);
    }

    #[test]
    fn dotnet_runs_sonar_around_its_build() {
        let cs = builtin().get(Language::CSharp).unwrap().clone();
        assert_eq!(cs.sonar.scanner, SonarScanner::Dotnet);
        assert!(cs.sonar.build.is_some());
        let rust = builtin().get(Language::Rust).unwrap().clone();
        assert_eq!(rust.sonar.scanner, SonarScanner::Cli);
        assert!(rust.sonar.build.is_none());
    }

    #[test]
    fn sonar_build_goes_with_the_dotnet_scanner_only() {
        let rust = include_str!("../../../profiles/rust.toml");
        let cli_with_build = format!("{rust}\n[sonar]\nbuild = [\"cargo\", \"build\"]\n");
        let err = Profile::from_toml("t", &cli_with_build).unwrap_err();
        assert!(err.to_string().contains("sonar.build only"), "{err}");
        let dotnet_without = format!("{rust}\n[sonar]\nscanner = \"dotnet\"\n");
        let err = Profile::from_toml("t", &dotnet_without).unwrap_err();
        assert!(err.to_string().contains("sonar.build is required"), "{err}");
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
            &t.replace("\"stdout\"]", "\"stdout\", \"{fiel}\"]"),
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

    #[test]
    fn find_reports_matches_literal_and_glob_paths() {
        let dir_name = format!("aegis_test_find_reports_{}", std::process::id());
        let root = std::env::temp_dir().join(dir_name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // 1. Literal path
        let literal = root.join("lcov.info");
        std::fs::write(&literal, "TN:").unwrap();
        assert_eq!(find_reports(&root, "lcov.info"), vec![literal.clone()]);
        assert_eq!(find_reports(&root, "missing.xml"), Vec::<PathBuf>::new());

        // 2. Glob pattern with single wildcard segment (e.g. TestResults/*/coverage.opencover.xml)
        let guid1 = root.join("TestResults").join("1111-guid");
        let guid2 = root.join("TestResults").join("2222-guid");
        std::fs::create_dir_all(&guid1).unwrap();
        std::fs::create_dir_all(&guid2).unwrap();
        let report1 = guid1.join("coverage.opencover.xml");
        let report2 = guid2.join("coverage.opencover.xml");
        std::fs::write(&report1, "<CoverageSession1/>").unwrap();
        std::fs::write(&report2, "<CoverageSession2/>").unwrap();

        let pattern = "TestResults/*/coverage.opencover.xml";
        assert_eq!(
            find_reports(&root, pattern),
            vec![report1.clone(), report2.clone()]
        );

        // 3. Trailing wildcard file name
        assert_eq!(
            find_reports(&root, "TestResults/*/*.xml"),
            vec![report1.clone(), report2.clone()]
        );

        // 4. Recursive glob **
        let nested = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        let deep_report = nested.join("deep.xml");
        std::fs::write(&deep_report, "<deep/>").unwrap();
        assert_eq!(find_reports(&root, "a/**/deep.xml"), vec![deep_report]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn csharp_skips_its_coverage_under_the_testing_platform() {
        let root = std::env::temp_dir().join(format!("aegis_test_mtp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let part = root.join("services/api");
        std::fs::create_dir_all(&part).unwrap();
        let cs = builtin().get(Language::CSharp).unwrap().clone();
        let skipped = |dir: &Path| cs.gate.coverage_skip.iter().any(|m| m.matches(dir, &root));

        assert!(!skipped(&part), "no global.json: VSTest");
        let mtp = "{\n  \"test\": {\n    \"runner\": \"Microsoft.Testing.Platform\"\n  }\n}\n";
        std::fs::write(root.join("global.json"), mtp).unwrap();
        assert!(skipped(&root));
        assert!(
            skipped(&part),
            "the repo root's global.json holds for its parts"
        );
        // The nearest global.json is the one `dotnet` reads.
        std::fs::write(
            part.join("global.json"),
            r#"{"sdk": {"version": "10.0.100"}}"#,
        )
        .unwrap();
        assert!(!skipped(&part));
        assert!(skipped(&root));
        // Nothing above the repo root counts.
        let inner = root.join("services");
        assert!(!cs.gate.coverage_skip[0].matches(&inner, &inner));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_bad_coverage_skips() {
        let cs = BUILTIN
            .iter()
            .find(|(name, _)| name.ends_with("csharp.toml"))
            .unwrap()
            .1;
        let skip = "file = \"global.json\"";
        rejects(
            &cs.replace(skip, "file = \"../global.json\""),
            "is not a file name",
        );
        rejects(
            &cs.replace(skip, "x = 1, file = \"global.json\""),
            "unknown field",
        );
        let pattern = r#"'"runner"\s*:\s*"Microsoft\.Testing\.Platform"'"#;
        assert!(cs.contains(pattern));
        rejects(&cs.replace(pattern, "'('"), "gate.coverage_skip");
    }
}
