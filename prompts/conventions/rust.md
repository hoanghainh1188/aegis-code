# Rust conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass `cargo fmt --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` as they are run on
  this repository; the formatter runs after each edit.
- Follow the error handling already used in the crate (`Result` with the
  crate's error type, `?` to propagate). Do not add `unwrap()`, `expect()` or
  `panic!` to library code where the crate returns errors; they are fine in
  tests.
- Do not add `unsafe`. If the fix seems to need it, stop and explain.
- Keep public APIs stable: do not rename or change the signature of a `pub`
  item unless the issue asks for it.
- Unit tests go in the `#[cfg(test)] mod tests` of the file they test;
  tests of the public API go in the crate's `tests/` directory. The
  reproduction test is the exception: never put it in a `#[cfg(test)]` module
  of a file your fix changes. Put it in its own file under the crate's
  `tests/` with a unique name, for example `tests/issue_regression.rs`.
- The reproduction test is run as
  `cargo test --workspace -- --exact <name>`; give it a name that is unique
  in the workspace and report the name as libtest prints it
  (`module::test_name` for unit tests, `test_name` in a `tests/` file).
- Prefer the standard library and crates the workspace already depends on.
  New crates are blocked by the checks.
