//! Commands as argv arrays with named placeholders. Nothing goes through a
//! shell, so a value can never add a command; it can only fill its own
//! argument.

use std::fmt;

use serde::Deserialize;

use crate::LangError;

/// Values the lifecycle may fill into a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// Path of the file being edited (`edit.format`).
    File,
    /// Name or path of the reproduction test (`gate.repro_test`).
    Test,
    /// Lockfile being audited (`dependencies.audit`).
    Lockfile,
    /// Allowed licenses, comma-separated (`dependencies.audit`).
    Licenses,
    /// File the resolved dependency tree is written to (`dependencies.tree`).
    Output,
}

impl Placeholder {
    const ALL: [Self; 5] = [
        Self::File,
        Self::Test,
        Self::Lockfile,
        Self::Licenses,
        Self::Output,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::File => "{file}",
            Self::Test => "{test}",
            Self::Lockfile => "{lockfile}",
            Self::Licenses => "{licenses}",
            Self::Output => "{output}",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Test => "test",
            Self::Lockfile => "lockfile",
            Self::Licenses => "licenses",
            Self::Output => "output",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<String>")]
pub struct CommandTemplate(Vec<String>);

impl TryFrom<Vec<String>> for CommandTemplate {
    type Error = LangError;

    fn try_from(argv: Vec<String>) -> Result<Self, Self::Error> {
        let invalid = |m: &str| Err(LangError::InvalidCommand(m.to_owned()));
        let Some(program) = argv.first() else {
            return invalid("empty command");
        };
        if program.trim().is_empty() {
            return invalid("empty program name");
        }
        if pieces(program).iter().any(|p| p.token().is_some()) {
            return invalid("the program itself cannot be a placeholder");
        }
        if argv.iter().any(|a| a.contains('\0')) {
            return invalid("NUL byte in argument");
        }
        Ok(Self(argv))
    }
}

impl CommandTemplate {
    pub fn argv(&self) -> &[String] {
        &self.0
    }

    pub fn uses(&self, placeholder: Placeholder) -> bool {
        self.0
            .iter()
            .flat_map(|a| pieces(a))
            .any(|p| p == Piece::Token(placeholder.name()))
    }

    /// Checks that only `allowed` placeholders appear and every `required`
    /// one does. An unknown `{name}` fails when the profile loads, so a typo
    /// such as `{tset}` is caught early. Other braces (`{}`, `${VAR}`) are
    /// plain text, as in `xargs -I{}`.
    pub(crate) fn check_placeholders(
        &self,
        allowed: &[Placeholder],
        required: &[Placeholder],
    ) -> Result<(), String> {
        for name in self
            .0
            .iter()
            .flat_map(|a| pieces(a))
            .filter_map(Piece::token)
        {
            match Placeholder::from_name(name) {
                None => return Err(format!("unknown placeholder `{{{name}}}`")),
                Some(p) if !allowed.contains(&p) => {
                    return Err(format!("`{}` is not allowed here", p.token()));
                }
                Some(_) => {}
            }
        }
        match required.iter().find(|p| !self.uses(**p)) {
            Some(p) => Err(format!("must contain `{}`", p.token())),
            None => Ok(()),
        }
    }

    /// Argv with placeholders filled in one pass, so a value is never
    /// scanned for placeholders itself. A value that starts with `-` could
    /// be read as an option, so it is refused.
    pub fn render(&self, values: &[(Placeholder, &str)]) -> Result<Vec<String>, LangError> {
        for (p, value) in values {
            let reason = if value.is_empty() {
                Some("empty")
            } else if value.starts_with('-') {
                Some("starts with `-`")
            } else if value.contains('\0') || value.contains('\n') {
                Some("contains a NUL byte or newline")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(LangError::InvalidValue {
                    placeholder: p.name(),
                    reason,
                });
            }
        }
        self.0
            .iter()
            .map(|arg| {
                pieces(arg)
                    .into_iter()
                    .map(|piece| match piece {
                        Piece::Text(t) => Ok(t),
                        Piece::Token(name) => Placeholder::from_name(name)
                            .and_then(|p| values.iter().find(|(q, _)| *q == p))
                            .map(|(_, v)| *v)
                            .ok_or_else(|| {
                                LangError::InvalidCommand(format!("no value for `{{{name}}}`"))
                            }),
                    })
                    .collect::<Result<String, _>>()
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    /// `{name}` with `name` in `[a-z_]+`.
    Token(&'a str),
}

impl<'a> Piece<'a> {
    fn token(self) -> Option<&'a str> {
        match self {
            Self::Token(name) => Some(name),
            Self::Text(_) => None,
        }
    }
}

fn pieces(arg: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    while let Some(open) = arg[i..].find('{').map(|o| i + o) {
        let name_end = arg[open + 1..]
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .map_or(arg.len(), |e| open + 1 + e);
        if name_end > open + 1 && arg[name_end..].starts_with('}') {
            if open > text_start {
                out.push(Piece::Text(&arg[text_start..open]));
            }
            out.push(Piece::Token(&arg[open + 1..name_end]));
            text_start = name_end + 1;
            i = text_start;
        } else {
            i = open + 1;
        }
    }
    if text_start < arg.len() {
        out.push(Piece::Text(&arg[text_start..]));
    }
    out
}

impl fmt::Display for CommandTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(argv: &[&str]) -> CommandTemplate {
        CommandTemplate::try_from(argv.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn rejects_bad_commands() {
        for bad in [vec![], vec![" "], vec!["{test}"], vec!["ls", "a\0b"]] {
            let argv: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(CommandTemplate::try_from(argv).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn renders_values_as_single_arguments() {
        let c = cmd(&["cargo", "test", "--", "--exact", "{test}"]);
        let argv = c
            .render(&[(Placeholder::Test, "parser::tests::empty; rm -rf /")])
            .unwrap();
        assert_eq!(argv.len(), 5);
        assert_eq!(argv[4], "parser::tests::empty; rm -rf /");
    }

    #[test]
    fn renders_embedded_placeholders() {
        let c = cmd(&["tool", "--path={file}"]);
        assert_eq!(
            c.render(&[(Placeholder::File, "src/a.py")]).unwrap(),
            vec!["tool", "--path=src/a.py"]
        );
    }

    #[test]
    fn refuses_values_that_look_like_options_or_are_missing() {
        let c = cmd(&["pytest", "{test}"]);
        assert!(matches!(
            c.render(&[(Placeholder::Test, "--collect-only")]),
            Err(LangError::InvalidValue {
                placeholder: "test",
                ..
            })
        ));
        assert!(c.render(&[(Placeholder::Test, "")]).is_err());
        assert!(c.render(&[(Placeholder::Test, "a\nb")]).is_err());
        assert!(matches!(c.render(&[]), Err(LangError::InvalidCommand(_))));
    }

    #[test]
    fn values_are_not_scanned_for_placeholders() {
        let c = cmd(&["tool", "{file}", "{test}"]);
        let argv = c
            .render(&[(Placeholder::File, "a{test}b.py"), (Placeholder::Test, "T")])
            .unwrap();
        assert_eq!(argv, vec!["tool", "a{test}b.py", "T"]);
    }

    #[test]
    fn other_braces_are_plain_text() {
        let c = cmd(&["xargs", "-I{}", "echo", "${HOME}", "{x1}", "{}"]);
        assert!(c.check_placeholders(&[], &[]).is_ok());
        assert_eq!(c.render(&[]).unwrap(), c.argv());
        assert!(cmd(&["sh", "{_a}"]).check_placeholders(&[], &[]).is_err());
    }

    #[test]
    fn checks_placeholders() {
        let c = cmd(&["pytest", "{test}"]);
        assert!(
            c.check_placeholders(&[Placeholder::Test], &[Placeholder::Test])
                .is_ok()
        );
        assert!(c.check_placeholders(&[], &[]).is_err());
        assert!(
            cmd(&["pytest"])
                .check_placeholders(&[Placeholder::Test], &[Placeholder::Test])
                .is_err()
        );
        assert!(
            cmd(&["pytest", "{tset}"])
                .check_placeholders(&[Placeholder::Test], &[])
                .is_err()
        );
    }
}
