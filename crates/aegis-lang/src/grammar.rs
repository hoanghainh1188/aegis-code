//! Tree-sitter grammars and symbol queries. A language can have several
//! grammars (TypeScript, TSX and JavaScript for the `typescript` profile),
//! picked by file extension.

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::Query;

use crate::{LangError, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grammar {
    Rust,
    Python,
    Go,
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    CSharp,
}

impl Grammar {
    const COUNT: usize = 8;

    /// Grammar for a file of `language`; without a path, the language's
    /// main grammar. `None` while the language has no grammar yet.
    pub(crate) fn for_file(language: Language, path: Option<&Path>) -> Option<Self> {
        let ext = path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match language {
            Language::Rust => Some(Self::Rust),
            Language::Python => Some(Self::Python),
            Language::Go => Some(Self::Go),
            Language::TypeScript => Some(match ext.as_deref() {
                Some("tsx") => Self::Tsx,
                Some("js" | "jsx" | "mjs" | "cjs") => Self::JavaScript,
                _ => Self::TypeScript,
            }),
            Language::Java => Some(Self::Java),
            Language::CSharp => Some(Self::CSharp),
        }
    }

    pub(crate) fn language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        }
    }

    fn query_source(self) -> &'static str {
        match self {
            Self::Rust => include_str!("../../../profiles/queries/rust.scm"),
            Self::Python => include_str!("../../../profiles/queries/python.scm"),
            Self::Go => include_str!("../../../profiles/queries/go.scm"),
            Self::TypeScript | Self::Tsx => {
                include_str!("../../../profiles/queries/typescript.scm")
            }
            Self::JavaScript => include_str!("../../../profiles/queries/javascript.scm"),
            Self::Java => include_str!("../../../profiles/queries/java.scm"),
            Self::CSharp => include_str!("../../../profiles/queries/csharp.scm"),
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The symbol query, compiled once and shared across threads.
    pub(crate) fn query(self, language: Language) -> Result<&'static Query, LangError> {
        static QUERIES: [OnceLock<Result<Query, String>>; Grammar::COUNT] =
            [const { OnceLock::new() }; Grammar::COUNT];
        QUERIES[self.index()]
            .get_or_init(|| {
                Query::new(&self.language(), self.query_source()).map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|message| LangError::InvalidQuery {
                language,
                message: message.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Grammar; Grammar::COUNT] = [
        Grammar::Rust,
        Grammar::Python,
        Grammar::Go,
        Grammar::TypeScript,
        Grammar::Tsx,
        Grammar::JavaScript,
        Grammar::Java,
        Grammar::CSharp,
    ];

    #[test]
    fn every_query_compiles_against_its_grammar() {
        for g in ALL {
            assert!(g.query(Language::Rust).is_ok(), "{g:?}");
        }
        assert_eq!(
            ALL.iter().map(|g| g.index()).collect::<Vec<_>>(),
            (0..Grammar::COUNT).collect::<Vec<_>>()
        );
    }

    #[test]
    fn picks_typescript_grammars_by_extension() {
        let ts = |p: &str| Grammar::for_file(Language::TypeScript, Some(Path::new(p)));
        assert_eq!(ts("a.ts"), Some(Grammar::TypeScript));
        assert_eq!(ts("a.mts"), Some(Grammar::TypeScript));
        assert_eq!(ts("App.TSX"), Some(Grammar::Tsx));
        assert_eq!(ts("a.jsx"), Some(Grammar::JavaScript));
        assert_eq!(ts("a.cjs"), Some(Grammar::JavaScript));
        assert_eq!(
            Grammar::for_file(Language::TypeScript, None),
            Some(Grammar::TypeScript)
        );
        assert!(
            Language::ALL
                .iter()
                .all(|l| Grammar::for_file(*l, None).is_some())
        );
    }
}
