use std::fmt;

use serde::{Deserialize, Serialize};

/// Languages with a profile (ADR 0004 §2). Static dispatch (ADR 0003 §7):
/// adding a language is a new variant plus its profile data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Java,
    Go,
    CSharp,
}

impl Language {
    pub const ALL: [Self; 6] = [
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::Java,
        Self::Go,
        Self::CSharp,
    ];

    /// Identifier used in config files and `profiles/<id>.toml`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Java => "java",
            Self::Go => "go",
            Self::CSharp => "csharp",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_serde_names() {
        for language in Language::ALL {
            assert_eq!(serialized(language), language.id());
        }
    }

    fn serialized(language: Language) -> String {
        #[derive(Serialize)]
        struct Wrap {
            language: Language,
        }
        let text = toml::to_string(&Wrap { language }).unwrap();
        text.trim()
            .trim_start_matches("language = ")
            .trim_matches('"')
            .to_owned()
    }

    #[test]
    fn display_is_the_id() {
        assert_eq!(Language::CSharp.to_string(), "csharp");
    }
}
