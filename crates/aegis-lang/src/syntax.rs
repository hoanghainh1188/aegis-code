//! Syntax check shared by every profile (ADR 0004 §1): tree-sitter parses
//! code that does not compile, needs no toolchain, and runs before `edit`
//! writes a file.

use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::grammar::Grammar;
use crate::{LangError, Language};

/// At most this many errors are reported; one is enough to refuse an edit.
const MAX_ERRORS: usize = 20;

/// 1-based position of an `ERROR` or missing node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    /// The parser expected a token that is not there.
    pub missing: bool,
}

pub fn has_grammar(language: Language) -> bool {
    Grammar::for_file(language, None).is_some()
}

/// Larger sources are refused rather than parsed.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

/// Syntax errors in `source`, parsed with the language's main grammar.
pub fn syntax_errors(language: Language, source: &str) -> Result<Vec<SyntaxError>, LangError> {
    syntax_errors_in(language, None, source)
}

/// Syntax errors in `source`, with the grammar for `path`'s extension
/// (e.g. TSX for `.tsx`). Empty when it parses cleanly.
pub fn syntax_errors_in(
    language: Language,
    path: Option<&Path>,
    source: &str,
) -> Result<Vec<SyntaxError>, LangError> {
    let grammar = Grammar::for_file(language, path)
        .ok_or(LangError::UnsupportedGrammar(language))?
        .language();
    if source.len() > MAX_SOURCE_BYTES {
        return Err(LangError::SourceTooLarge {
            language,
            size: source.len(),
            limit: MAX_SOURCE_BYTES,
        });
    }
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|_| LangError::UnsupportedGrammar(language))?;
    let tree = parser
        .parse(source, None)
        .ok_or(LangError::ParseFailed(language))?;
    Ok(collect(tree.root_node()))
}

/// Pre-order walk with a cursor, not recursion, so deeply nested input
/// cannot overflow the stack. Does not descend into an error, whose
/// children add noise.
fn collect(root: Node<'_>) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let descend = if node.is_error() || node.is_missing() {
            let start = node.start_position();
            errors.push(SyntaxError {
                line: start.row + 1,
                column: start.column + 1,
                missing: node.is_missing(),
            });
            false
        } else {
            node.has_error()
        };
        if errors.len() >= MAX_ERRORS {
            return errors;
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return errors;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_code_has_no_errors() {
        let rust = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        assert!(syntax_errors(Language::Rust, rust).unwrap().is_empty());
        let py = "def add(a, b):\n    return a + b\n";
        assert!(syntax_errors(Language::Python, py).unwrap().is_empty());
    }

    #[test]
    fn reports_position_of_rust_error() {
        let errors = syntax_errors(Language::Rust, "fn main() {\n    let x = ;\n}\n").unwrap();
        assert!(!errors.is_empty());
        assert_eq!(errors[0].line, 2);
    }

    #[test]
    fn reports_python_errors() {
        let errors = syntax_errors(Language::Python, "def f(:\n    return 1\n").unwrap();
        assert_eq!(errors[0].line, 1);
        let unclosed = syntax_errors(Language::Python, "x = [1, 2\n").unwrap();
        assert!(!unclosed.is_empty());
    }

    #[test]
    fn caps_the_number_of_errors() {
        let broken = "fn ( {\n".repeat(200);
        assert!(syntax_errors(Language::Rust, &broken).unwrap().len() <= MAX_ERRORS);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let deep = "x = ".to_owned() + &"(".repeat(200_000);
        assert!(!syntax_errors(Language::Python, &deep).unwrap().is_empty());
    }

    #[test]
    fn refuses_huge_sources() {
        let huge = "a".repeat(MAX_SOURCE_BYTES + 1);
        assert!(matches!(
            syntax_errors(Language::Rust, &huge),
            Err(LangError::SourceTooLarge { .. })
        ));
    }

    #[test]
    fn languages_without_grammar_say_so() {
        assert!(
            [
                Language::Rust,
                Language::Python,
                Language::Go,
                Language::TypeScript
            ]
            .into_iter()
            .all(has_grammar)
        );
        assert_eq!(
            syntax_errors(Language::CSharp, "class A {}"),
            Err(LangError::UnsupportedGrammar(Language::CSharp))
        );
    }
}
