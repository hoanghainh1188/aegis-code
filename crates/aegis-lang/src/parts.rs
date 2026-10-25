//! Profiles by directory, for a repo of more than one language (ADR 0004
//! §1, #71). The repo's own profile covers the whole tree; each part gives
//! a directory and everything under it to another profile. A path belongs
//! to the deepest part that holds it, so parts may nest.

use std::path::Path;

use crate::{LangError, ResolvedProfile};

/// One directory of the repo and the profile its files go by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePart {
    /// Relative to the repo root with `/` separators; `""` is the root.
    pub dir: String,
    pub profile: ResolvedProfile,
}

impl ProfilePart {
    /// `path` relative to this part's directory, when the part holds it.
    pub fn relative<'p>(&self, path: &'p str) -> Option<&'p str> {
        in_dir(&self.dir, path)
    }

    /// How the gate and the agent name the part: `` `web/` (TypeScript) ``,
    /// or `the repo root (Rust)`.
    pub fn label(&self) -> String {
        part_label(&self.dir, &self.profile)
    }
}

/// How the gate and the agent name the part of `dir` going by `profile`.
pub fn part_label(dir: &str, profile: &ResolvedProfile) -> String {
    let name = &profile.profile.display_name;
    if dir.is_empty() {
        format!("the repo root ({name})")
    } else {
        format!("`{dir}/` ({name})")
    }
}

/// Index of the deepest of `dirs` holding `path`; `""` holds every path.
pub fn deepest<'d>(dirs: impl IntoIterator<Item = &'d str>, path: &str) -> Option<usize> {
    dirs.into_iter()
        .enumerate()
        .filter(|(_, d)| in_dir(d, path).is_some())
        .max_by_key(|(_, d)| d.len())
        .map(|(i, _)| i)
}

/// `path` inside `dir`, relative to the root.
pub fn join_dir(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.to_owned()
    } else {
        format!("{dir}/{path}")
    }
}

/// The repo's profile and its parts. The first part is always the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoProfiles {
    parts: Vec<ProfilePart>,
}

/// `path` relative to `dir`, when `dir` holds it; `""` holds every path.
pub fn in_dir<'p>(dir: &str, path: &'p str) -> Option<&'p str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// A part's directory: a plain relative path, no `.`, `..`, empty
/// component, leading or trailing `/`.
pub fn check_dir(dir: &str) -> Result<(), String> {
    let plain = !dir.is_empty()
        && dir
            .split('/')
            .all(|c| !matches!(c, "" | "." | "..") && !c.contains('\\'));
    if plain {
        Ok(())
    } else {
        Err(format!(
            "`{dir}` must be a plain relative directory such as `web` or `services/api`"
        ))
    }
}

impl From<ResolvedProfile> for RepoProfiles {
    fn from(profile: ResolvedProfile) -> Self {
        Self::single(profile)
    }
}

impl RepoProfiles {
    /// One profile for the whole repo.
    pub fn single(profile: ResolvedProfile) -> Self {
        Self {
            parts: vec![ProfilePart {
                dir: String::new(),
                profile,
            }],
        }
    }

    /// `root` for the whole repo, and each `(dir, profile)` for its
    /// directory.
    pub fn new(
        root: ResolvedProfile,
        dirs: Vec<(String, ResolvedProfile)>,
    ) -> Result<Self, LangError> {
        let mut parts = Self::single(root).parts;
        for (dir, profile) in dirs {
            let invalid = |message: String| LangError::InvalidOverrides {
                language: profile.language(),
                message,
            };
            check_dir(&dir).map_err(invalid)?;
            if parts.iter().any(|p| p.dir == dir) {
                return Err(invalid(format!("`{dir}` is given a profile twice")));
            }
            parts.push(ProfilePart { dir, profile });
        }
        Ok(Self { parts })
    }

    pub fn root(&self) -> &ResolvedProfile {
        &self.parts[0].profile
    }

    pub fn parts(&self) -> &[ProfilePart] {
        &self.parts
    }

    /// Index in [`Self::parts`] of the deepest part holding `path`
    /// (relative to the root, `/` separators).
    pub fn index_of(&self, path: &str) -> usize {
        deepest(self.parts.iter().map(|p| p.dir.as_str()), path).unwrap_or(0)
    }

    /// The deepest part holding `path`.
    pub fn part_of(&self, path: &str) -> &ProfilePart {
        &self.parts[self.index_of(path)]
    }

    /// The profile that reads `path` as source: its part's, when that
    /// profile owns the file's extension, else the first other part's that
    /// does (a Python script in a Rust repo). `None` when no profile reads
    /// it.
    pub fn for_file(&self, path: &str) -> Option<&ResolvedProfile> {
        let owns = |p: &ProfilePart| p.profile.profile.owns_file(Path::new(path));
        let own = self.part_of(path);
        if owns(own) {
            return Some(&own.profile);
        }
        self.parts.iter().find(|p| owns(p)).map(|p| &p.profile)
    }

    /// Whether any profile reads `path` as source.
    pub fn owns_file(&self, path: &str) -> bool {
        self.for_file(path).is_some()
    }

    /// Indexes of the parts holding at least one of `paths`, in order.
    pub fn touched<'p>(&self, paths: impl IntoIterator<Item = &'p str>) -> Vec<usize> {
        let mut touched: Vec<usize> = paths.into_iter().map(|p| self.index_of(p)).collect();
        touched.sort_unstable();
        touched.dedup();
        touched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Language, ProfileOverrides, Profiles};

    fn resolved(language: Language) -> ResolvedProfile {
        Profiles::builtin()
            .unwrap()
            .get(language)
            .unwrap()
            .resolve(&ProfileOverrides::default())
            .unwrap()
    }

    fn rust_with_python_and_ts() -> RepoProfiles {
        RepoProfiles::new(
            resolved(Language::Rust),
            vec![
                ("tools".into(), resolved(Language::Python)),
                ("tools/web".into(), resolved(Language::TypeScript)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn a_path_belongs_to_the_deepest_part_that_holds_it() {
        let repo = rust_with_python_and_ts();
        assert_eq!(repo.part_of("src/lib.rs").dir, "");
        assert_eq!(repo.part_of("tools/gen.py").dir, "tools");
        assert_eq!(repo.part_of("tools/web/app.ts").dir, "tools/web");
        // A sibling whose name starts like a part is not in it.
        assert_eq!(repo.part_of("toolsx/gen.py").dir, "");
        assert_eq!(repo.part_of("tools").dir, "");
        assert_eq!(
            repo.part_of("tools/web/app.ts")
                .relative("tools/web/app.ts"),
            Some("app.ts")
        );
    }

    #[test]
    fn a_file_is_read_with_its_part_s_grammar_or_another_that_owns_it() {
        let repo = rust_with_python_and_ts();
        let language = |p: &str| repo.for_file(p).map(ResolvedProfile::language);
        assert_eq!(language("src/lib.rs"), Some(Language::Rust));
        assert_eq!(language("tools/gen.py"), Some(Language::Python));
        assert_eq!(language("tools/web/app.ts"), Some(Language::TypeScript));
        // A script outside the Python part still has Python's grammar.
        assert_eq!(language("scripts/release.py"), Some(Language::Python));
        assert_eq!(language("README.md"), None);
    }

    #[test]
    fn the_parts_a_patch_touches_are_listed_once_in_order() {
        let repo = rust_with_python_and_ts();
        assert_eq!(
            repo.touched(["tools/web/a.ts", "src/lib.rs", "tools/web/b.ts"]),
            [0, 2]
        );
        assert!(repo.touched([]).is_empty());
    }

    #[test]
    fn a_part_s_directory_must_be_plain_and_given_once() {
        for bad in ["", "/web", "web/", "./web", "a/../web", "a//b", "a\\b"] {
            let err = RepoProfiles::new(
                resolved(Language::Rust),
                vec![(bad.into(), resolved(Language::Python))],
            )
            .unwrap_err();
            assert!(err.to_string().contains("plain relative"), "{bad}: {err}");
        }
        let twice = RepoProfiles::new(
            resolved(Language::Rust),
            vec![
                ("web".into(), resolved(Language::TypeScript)),
                ("web".into(), resolved(Language::Python)),
            ],
        )
        .unwrap_err();
        assert!(twice.to_string().contains("twice"), "{twice}");
    }

    #[test]
    fn parts_are_named_by_directory_and_language() {
        let repo = rust_with_python_and_ts();
        assert_eq!(repo.parts()[0].label(), "the repo root (Rust)");
        assert_eq!(repo.parts()[1].label(), "`tools/` (Python)");
    }
}
