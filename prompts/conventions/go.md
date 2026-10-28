# Go conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass `gofmt -d .` (no diff) and `go vet ./...`; the formatter
  runs after each edit.
- Follow the error handling already used in the package: return `error`
  values and wrap them as the package does. Do not add `panic` to library
  code where the package returns errors.
- Keep exported APIs stable: do not rename or change the signature of an
  exported identifier unless the issue asks for it.
- Tests live next to the code they test, in `*_test.go` files of the same
  package. Use the helpers the package's tests already have.
- Put the reproduction test in its own new file in the package it tests,
  for example `<package dir>/issue_regression_test.go`, with a test name no
  other test in the module has, such as `TestIssueRegression`. Never put it
  in a test file that already exists.
- The reproduction test is run as `go test ./... -run <id>`. Report its id
  as the bare test function name, `TestIssueRegression`: never a file path,
  never `file_test.go::TestName`. A run that prints only
  `ok … [no tests to run]` ran nothing: the id is wrong.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer the standard library and modules `go.mod` already requires. New
  modules are blocked by the checks.
