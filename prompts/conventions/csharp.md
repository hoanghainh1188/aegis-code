# C# / .NET conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass `dotnet format --verify-no-changes` and build with
  `-warnaserror`: analyzer warnings are errors here. The formatter runs
  after each edit.
- Match the surrounding code: nullable annotations where the project has
  them enabled, `async`/`await` as the class uses it, the same exceptions
  and argument checks. Do not add `#pragma warning disable` or `!` to
  silence a warning.
- Keep public APIs stable: do not rename or change the signature of a
  `public` or `protected` member unless the issue asks for it.
- Tests use the framework the test project already uses (xUnit, NUnit,
  MSTest) and live in the test project that covers the code you change.
- Put the reproduction test in its own new test class and file in that
  test project, for example `IssueRegressionTests.cs`, never in a test
  class that already exists.
- The reproduction test is run as `dotnet test --filter <id>`. Report its id
  as a filter expression that selects only it, for example
  `FullyQualifiedName~IssueRegressionTests.TrimsMultibyteText`, never a file
  path. A run whose summary counts no test ran nothing: the filter is wrong.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer the packages the projects already reference. New packages are
  blocked by the checks.
