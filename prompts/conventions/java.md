# Java conventions

These apply unless the repository's own conventions below say otherwise.

- Code and tests must compile (`mvn test-compile`) with the repository's
  own build; formatting follows the surrounding code.
- Follow the exception handling already used in the class: the same
  checked or unchecked exceptions, the same messages. Do not swallow
  exceptions.
- Keep public APIs stable: do not rename or change the signature of a
  `public` or `protected` member unless the issue asks for it.
- Tests use the framework the module already uses (JUnit 4 or 5, TestNG)
  and live under `src/test/java` in the package of the class they test.
- Put the reproduction test in its own new test class, for example
  `IssueRegressionTest` next to the existing tests of that package, never in
  a test class that already exists.
- The reproduction test is run as `mvn test -Dtest=<id>`. Report its id in
  Surefire's form, `IssueRegressionTest#methodName`, with the simple class
  name, not a file path.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer the JDK and dependencies the build already declares. New
  dependencies are blocked by the checks.
