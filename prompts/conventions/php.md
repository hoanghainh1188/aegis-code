# PHP conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass `php-cs-fixer` (`--dry-run --diff`) and static analysis
  (`phpstan analyse`) without errors. The formatter runs after each edit.
- Match the surrounding style: `declare(strict_types=1);` where the file uses
  it, explicit parameter/return type declarations, and PHPDoc blocks where the
  project expects them. Do not suppress warnings with `@phpstan-ignore-*` or
  `@`.
- Keep public APIs stable: do not rename or change the signature of a `public`
  or `protected` method, function, or class unless the issue asks for it.
- Tests use PHPUnit (`vendor/bin/phpunit`) and live in the repository's test
  directory (`tests/`).
- Put the reproduction test in its own new test class and file under `tests/`,
  for example `tests/IssueRegressionTest.php`, never in an existing test file.
- The reproduction test is run as `vendor/bin/phpunit --filter <id>`. Report
  its id as the test class or `ClassName::testMethod` filter, never a file path.
  A run that prints `No tests executed!` ran nothing: the filter is wrong.
- Tests must not use the network or depend on the current date or time; the
  checks run without network access.
- Prefer packages already in `composer.lock`. Adding new packages is blocked by
  the dependency checks unless allow-listed.
