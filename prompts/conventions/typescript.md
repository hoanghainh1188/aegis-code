# TypeScript / JavaScript conventions

These apply unless the repository's own conventions below say otherwise.

- Code must pass the repository's `prettier --check`, `eslint` and
  `tsc --noEmit` as they are run here; the formatter runs after each edit.
- Match the surrounding code: module style (ESM or CommonJS), types where
  the module has them, the same naming and error handling. Do not add `any`
  or `@ts-ignore` to make the type check pass.
- Keep exported APIs stable: do not rename or change the signature of an
  export unless the issue asks for it.
- Tests use the runner the repository already uses (its `npm test`) and
  live where its tests live, named as they are (`*.test.ts`, `*.spec.ts`,
  `__tests__/`). Use its helpers and fixtures instead of writing new ones.
- Put the reproduction test in its own new test file, for example
  `<tests dir>/issue-regression.test.ts`, never in a test file that already
  exists.
- The reproduction test is run as `npm test -- <id>`. Report its id as that
  file's path from the repository root, for example
  `test/issue-regression.test.ts`, so the runner runs that file only.
- Tests must not use the network or depend on the current date or time;
  the checks run without network access.
- Prefer packages `package.json` already depends on. New packages are
  blocked by the checks.
