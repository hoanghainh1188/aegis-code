# Python conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass `ruff format --check` and `ruff check` as they are run on
  this repository; the formatter runs after each edit.
- Match the surrounding code: type hints where the module uses them, the
  same string quoting and naming, the same logging (never `print` for
  diagnostics).
- Keep public behaviour stable: do not rename or change the signature of a
  public function, class or method unless the issue asks for it.
- Tests use pytest and live where the repository keeps them (usually
  `tests/`, in files named `test_*.py`). Use the fixtures and helpers the
  test suite already has instead of writing new ones.
- A reproduction test is easiest to run in its own file, for example
  `tests/test_issue_regression.py`. It is run as `python -m pytest <id>`;
  report its pytest id, `tests/test_issue_regression.py::test_name`.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer the standard library and packages the project already depends on.
  New dependencies are blocked by the checks.
