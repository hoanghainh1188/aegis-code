# Kotlin conventions

These apply unless the repository's own conventions below say otherwise.

- Code and tests must compile (`gradle testClasses`) with the repository's
  own build; formatting follows the surrounding code.
- Prefer the idioms the file already uses: `val` over `var`, null safety
  through the type system (`?`, `?.`, `?:`) rather than `!!`, data classes,
  extension functions, `when` over `if` chains. Do not add `!!` to silence
  the compiler.
- Follow the error handling already used: the same exception types, the
  same `require` / `check` / `error` messages, or the same `Result` style.
  Do not swallow exceptions.
- Keep public APIs stable: do not rename or change the signature of a
  `public` or `protected` declaration unless the issue asks for it. A new
  parameter takes a default value.
- Coroutines: keep structured concurrency. No `GlobalScope`, no
  `runBlocking` in production code that is not already blocking.
- Tests use the framework the module already uses (kotlin.test, JUnit 5,
  JUnit 4, Kotest) and live under `src/test/kotlin` in the package of the
  class they test.
- Put the reproduction test in its own new test class, for example
  `IssueRegressionTest` next to the existing tests of that package, never in
  a test class that already exists.
- The reproduction test is run as `gradle test --tests <id>`. Report its id
  in Gradle's form, `IssueRegressionTest.methodName`, with the simple class
  name and the function name, not a file path. Name the test function
  without spaces or backticks.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer the standard library and dependencies the build already declares.
  New dependencies are blocked by the checks.
