//! Symbols for the ACI's `search_symbol` (Thiết kế §3.1, #7). Each language
//! has a tree-sitter query in `profiles/queries/<language>.scm` using the
//! `@name` / `@definition.<kind>` convention.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

use crate::syntax::{MAX_SOURCE_BYTES, grammar};
use crate::{LangError, Language};

/// Nodes that wrap a definition with its decorators; the symbol's range
/// starts at the wrapper so `open` shows the decorators too.
const WRAPPERS: &[&str] = &["decorated_definition"];

/// Longest signature returned; longer ones are cut with `…`.
const MAX_SIGNATURE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Union,
    TypeAlias,
    Trait,
    Impl,
    Module,
    Macro,
    Constant,
    Static,
    Class,
    Interface,
    Variable,
}

impl SymbolKind {
    fn from_capture(name: &str) -> Option<Self> {
        Some(match name.strip_prefix("definition.")? {
            "function" => Self::Function,
            "method" => Self::Method,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "union" => Self::Union,
            "type" => Self::TypeAlias,
            "trait" => Self::Trait,
            "impl" => Self::Impl,
            "module" => Self::Module,
            "macro" => Self::Macro,
            "constant" => Self::Constant,
            "static" => Self::Static,
            "class" => Self::Class,
            "interface" => Self::Interface,
            "variable" => Self::Variable,
            _ => return None,
        })
    }

    /// Kinds that hold other symbols, reported as their `container`.
    fn is_container(self) -> bool {
        matches!(
            self,
            Self::Impl | Self::Trait | Self::Module | Self::Class | Self::Interface
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    /// Name of the innermost impl, trait, module or class around it.
    pub container: Option<String>,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// First line of the definition, trimmed.
    pub signature: String,
}

fn query_source(language: Language) -> Option<&'static str> {
    match language {
        Language::Rust => Some(include_str!("../../../profiles/queries/rust.scm")),
        Language::Python => Some(include_str!("../../../profiles/queries/python.scm")),
        Language::Go => Some(include_str!("../../../profiles/queries/go.scm")),
        // Queries come with their profiles (#67, #68, #70).
        Language::TypeScript | Language::Java | Language::CSharp => None,
    }
}

pub fn has_symbol_query(language: Language) -> bool {
    query_source(language).is_some() && grammar(language).is_some()
}

struct Found {
    pattern: usize,
    range: (usize, usize),
    symbol: Symbol,
    /// Container named by an `@container` capture, not by nesting.
    explicit_container: Option<String>,
}

/// Definitions in `source`, in source order.
pub fn symbols(language: Language, source: &str) -> Result<Vec<Symbol>, LangError> {
    let unsupported = || LangError::UnsupportedGrammar(language);
    let grammar = grammar(language).ok_or_else(unsupported)?;
    let query_text = query_source(language).ok_or_else(unsupported)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err(LangError::SourceTooLarge {
            language,
            size: source.len(),
            limit: MAX_SOURCE_BYTES,
        });
    }
    let query = compiled_query(language, &grammar, query_text)?;
    let mut parser = Parser::new();
    parser.set_language(&grammar).map_err(|_| unsupported())?;
    let tree = parser
        .parse(source, None)
        .ok_or(LangError::ParseFailed(language))?;

    let names = query.capture_names();
    let mut by_node: BTreeMap<usize, Found> = BTreeMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut definition = None;
        let mut container = None;
        for c in m.captures() {
            let capture = names[c.index as usize];
            if capture == "name" {
                name = Some(c.node);
            } else if capture == "container" {
                container = Some(c.node);
            } else if let Some(kind) = SymbolKind::from_capture(capture) {
                definition = Some((kind, c.node));
            }
        }
        let (Some(name), Some((kind, node))) = (name, definition) else {
            continue;
        };
        // The first pattern to capture a node wins (methods before functions).
        if by_node
            .get(&node.id())
            .is_some_and(|f| f.pattern <= m.pattern_index)
        {
            continue;
        }
        let text = |n: tree_sitter::Node<'_>| source[n.byte_range()].to_owned();
        let outer = node
            .parent()
            .filter(|p| WRAPPERS.contains(&p.kind()))
            .unwrap_or(node);
        by_node.insert(
            node.id(),
            Found {
                pattern: m.pattern_index,
                range: (outer.start_byte(), outer.end_byte()),
                symbol: Symbol {
                    kind,
                    name: match kind {
                        SymbolKind::Impl => impl_name(&text(name)),
                        _ => text(name),
                    },
                    container: None,
                    start_line: outer.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    signature: signature(&source[node.byte_range()]),
                },
                explicit_container: container.map(|c| receiver_type(&text(c))),
            },
        );
    }

    let mut found: Vec<Found> = by_node.into_values().collect();
    found.sort_by_key(|f| (f.range.0, std::cmp::Reverse(f.range.1)));
    let containers: Vec<((usize, usize), String)> = found
        .iter()
        .filter(|f| f.symbol.kind.is_container())
        .map(|f| (f.range, f.symbol.name.clone()))
        .collect();
    Ok(found
        .into_iter()
        .map(|f| {
            let nested = containers
                .iter()
                .filter(|(r, _)| r.0 <= f.range.0 && f.range.1 <= r.1 && *r != f.range)
                .min_by_key(|(r, _)| r.1 - r.0)
                .map(|(_, name)| name.clone());
            Symbol {
                container: f.explicit_container.or(nested),
                ..f.symbol
            }
        })
        .collect())
}

/// Queries compile once per language and are shared across threads.
fn compiled_query(
    language: Language,
    grammar: &tree_sitter::Language,
    text: &str,
) -> Result<&'static Query, LangError> {
    static RUST: OnceLock<Result<Query, String>> = OnceLock::new();
    static PYTHON: OnceLock<Result<Query, String>> = OnceLock::new();
    static GO: OnceLock<Result<Query, String>> = OnceLock::new();
    let cell = match language {
        Language::Rust => &RUST,
        Language::Python => &PYTHON,
        Language::Go => &GO,
        other => return Err(LangError::UnsupportedGrammar(other)),
    };
    cell.get_or_init(|| Query::new(grammar, text).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|message| LangError::InvalidQuery {
            language,
            message: message.clone(),
        })
}

/// Type named by a receiver or parameter list, e.g. Go's `(c *Cart)` or
/// `(s Stack[T])`: the last word, without parentheses, then as [`impl_name`].
fn receiver_type(text: &str) -> String {
    let inner = text.trim().trim_start_matches('(').trim_end_matches(')');
    impl_name(inner.split_whitespace().last().unwrap_or(inner))
}

/// `impl<T> fmt::Display for &mut crate::a::Foo<T>` is the impl of `Foo`:
/// the name an agent searches for, without references, generics or path.
fn impl_name(type_text: &str) -> String {
    let t = type_text.trim();
    let t = t.trim_start_matches(['&', '*']).trim_start();
    let t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
    let t = t.strip_prefix("dyn ").unwrap_or(t).trim_start();
    let t = t.split(['<', '[']).next().unwrap_or(t);
    t.rsplit("::").next().unwrap_or(t).trim().to_owned()
}

fn signature(definition: &str) -> String {
    let first = definition.lines().next().unwrap_or_default().trim();
    let first = first.strip_suffix('{').unwrap_or(first).trim_end();
    match first.char_indices().nth(MAX_SIGNATURE_CHARS) {
        Some((cut, _)) => format!("{}…", &first[..cut]),
        None => first.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
pub const LIMIT: usize = 10;
static NAME: &str = "x";
pub type Pair = (u8, u8);

pub struct Point { x: i32, y: i32 }
pub enum Shape { Dot(Point), Line(Point, Point) }

pub trait Area {
    fn area(&self) -> f64;
    fn describe(&self) -> String { String::new() }
}

impl Area for Shape {
    fn area(&self) -> f64 { 0.0 }
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self { Self { x, y } }
}

macro_rules! square { ($x:expr) => { $x * $x }; }

pub mod geometry {
    pub fn origin() -> super::Point { super::Point::new(0, 0) }
}

fn helper() {}
"#;

    fn find<'a>(all: &'a [Symbol], name: &str) -> &'a Symbol {
        all.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no `{name}` in {all:#?}"))
    }

    #[test]
    fn finds_rust_definitions_with_kinds() {
        let all = symbols(Language::Rust, SAMPLE).unwrap();
        let kinds = [
            ("LIMIT", SymbolKind::Constant),
            ("NAME", SymbolKind::Static),
            ("Pair", SymbolKind::TypeAlias),
            ("Point", SymbolKind::Struct),
            ("Shape", SymbolKind::Enum),
            ("Area", SymbolKind::Trait),
            ("square", SymbolKind::Macro),
            ("geometry", SymbolKind::Module),
            ("helper", SymbolKind::Function),
            ("origin", SymbolKind::Function),
        ];
        for (name, kind) in kinds {
            assert_eq!(find(&all, name).kind, kind, "{name}");
        }
    }

    #[test]
    fn methods_belong_to_their_impl_or_trait() {
        let all = symbols(Language::Rust, SAMPLE).unwrap();
        let new = find(&all, "new");
        assert_eq!(
            (new.kind, new.container.as_deref()),
            (SymbolKind::Method, Some("Point"))
        );
        let describe = find(&all, "describe");
        assert_eq!(describe.container.as_deref(), Some("Area"));
        let area: Vec<_> = all.iter().filter(|s| s.name == "area").collect();
        assert_eq!(area.len(), 2, "trait declaration and impl");
        assert!(area.iter().all(|s| s.kind == SymbolKind::Method));
        // A function in a module is not a method.
        let origin = find(&all, "origin");
        assert_eq!(origin.container.as_deref(), Some("geometry"));
    }

    #[test]
    fn impl_blocks_are_named_after_their_type() {
        let all = symbols(Language::Rust, SAMPLE).unwrap();
        let impls: Vec<_> = all.iter().filter(|s| s.kind == SymbolKind::Impl).collect();
        assert_eq!(impls.len(), 2);
        assert_eq!(impls[0].name, "Shape");
        assert_eq!(impls[0].signature, "impl Area for Shape");
    }

    #[test]
    fn generic_and_reference_impls_use_the_base_type_name() {
        let src = "impl<T> Wrapper<T> { fn get(&self) {} }\n\
                   impl fmt::Display for &mut crate::a::Foo { fn fmt(&self) {} }\n\
                   impl<T: Clone> From<T> for Vec<T> {}\n";
        let all = symbols(Language::Rust, src).unwrap();
        let impls: Vec<_> = all
            .iter()
            .filter(|s| s.kind == SymbolKind::Impl)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(impls, vec!["Wrapper", "Foo", "Vec"]);
        assert_eq!(find(&all, "get").container.as_deref(), Some("Wrapper"));
        assert_eq!(find(&all, "fmt").container.as_deref(), Some("Foo"));
    }

    #[test]
    fn reports_lines_and_signatures_in_source_order() {
        let all = symbols(Language::Rust, SAMPLE).unwrap();
        let new = find(&all, "new");
        assert_eq!(
            new.signature,
            "pub fn new(x: i32, y: i32) -> Self { Self { x, y } }"
        );
        let point = find(&all, "Point");
        assert_eq!((point.start_line, point.end_line), (6, 6));
        let starts: Vec<_> = all.iter().map(|s| s.start_line).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn long_signatures_are_cut() {
        let long = format!("fn f({}) {{}}", "a: u8, ".repeat(100));
        let all = symbols(Language::Rust, &long).unwrap();
        assert!(all[0].signature.ends_with('…'));
        assert_eq!(all[0].signature.chars().count(), MAX_SIGNATURE_CHARS + 1);
    }

    #[test]
    fn broken_code_still_yields_the_parts_that_parse() {
        let all = symbols(Language::Rust, "fn ok() {}\nfn broken( {\n").unwrap();
        assert!(all.iter().any(|s| s.name == "ok"));
    }

    const PY: &str = r#"
MAX_ITEMS = 3

def helper(x):
    def inner():
        return x
    return inner

@dataclass
class Cart:
    items: list

    def add(self, price):
        self.items.append(price)

    @property
    def total(self):
        return sum(self.items)

    class Line:
        def cost(self):
            return 0
"#;

    #[test]
    fn finds_python_definitions() {
        let all = symbols(Language::Python, PY).unwrap();
        assert_eq!(find(&all, "MAX_ITEMS").kind, SymbolKind::Constant);
        assert_eq!(find(&all, "helper").kind, SymbolKind::Function);
        let inner = find(&all, "inner");
        assert_eq!(
            (inner.kind, inner.container.as_deref()),
            (SymbolKind::Function, None)
        );
        let cart = find(&all, "Cart");
        assert_eq!(cart.kind, SymbolKind::Class);
        assert_eq!(cart.start_line, 9, "starts at the decorator");
        assert_eq!(cart.signature, "class Cart:");
    }

    #[test]
    fn python_methods_and_decorated_methods_belong_to_their_class() {
        let all = symbols(Language::Python, PY).unwrap();
        let add = find(&all, "add");
        assert_eq!(
            (add.kind, add.container.as_deref()),
            (SymbolKind::Method, Some("Cart"))
        );
        let total = find(&all, "total");
        assert_eq!(
            (total.kind, total.container.as_deref()),
            (SymbolKind::Method, Some("Cart"))
        );
        assert_eq!(total.start_line, 16, "starts at @property");
        assert_eq!(total.signature, "def total(self):");
        let cost = find(&all, "cost");
        assert_eq!(
            (cost.kind, cost.container.as_deref()),
            (SymbolKind::Method, Some("Line"))
        );
        assert_eq!(all.iter().filter(|s| s.name == "total").count(), 1);
    }

    const GO: &str = r#"package cart

const MaxItems = 3

var defaultCart = Cart{}

type Total = int

type Cart struct {
	items []int
}

type Priced interface {
	Total() Total
}

func (c *Cart) Add(price int) error {
	return nil
}

func (s Stack[T]) Push(v T) {}

func New() *Cart { return &Cart{} }
"#;

    #[test]
    fn finds_go_definitions() {
        let all = symbols(Language::Go, GO).unwrap();
        assert_eq!(find(&all, "MaxItems").kind, SymbolKind::Constant);
        assert_eq!(find(&all, "defaultCart").kind, SymbolKind::Variable);
        assert_eq!(find(&all, "Total").kind, SymbolKind::TypeAlias);
        assert_eq!(find(&all, "Cart").kind, SymbolKind::Struct);
        assert_eq!(find(&all, "Priced").kind, SymbolKind::Interface);
        assert_eq!(find(&all, "New").kind, SymbolKind::Function);
        assert_eq!(
            all.iter().filter(|s| s.name == "Cart").count(),
            1,
            "struct, not also type"
        );
    }

    #[test]
    fn go_methods_belong_to_their_receiver_or_interface() {
        let all = symbols(Language::Go, GO).unwrap();
        let add = find(&all, "Add");
        assert_eq!(
            (add.kind, add.container.as_deref()),
            (SymbolKind::Method, Some("Cart"))
        );
        let push = find(&all, "Push");
        assert_eq!(push.container.as_deref(), Some("Stack"));
        let total = all
            .iter()
            .find(|s| s.kind == SymbolKind::Method && s.name == "Total")
            .unwrap();
        assert_eq!(total.container.as_deref(), Some("Priced"));
    }

    #[test]
    fn languages_without_a_query_say_so() {
        for language in [Language::Rust, Language::Python, Language::Go] {
            assert!(has_symbol_query(language), "{language}");
        }
        assert!(!has_symbol_query(Language::Java));
        assert_eq!(
            symbols(Language::Java, "class A {}"),
            Err(LangError::UnsupportedGrammar(Language::Java))
        );
    }
}
