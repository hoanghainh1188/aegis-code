# Evaluation Benchmark Suite (#4)

Curated dataset of closed issues from real target repositories, utilized to evaluate Core KPIs in Proposal §8 (#18). Each issue is specified in an `issues/<repo>-<number>.toml` definition file containing:
- Baseline reproduction commit (`base_commit`);
- Human reference fix (`fix_commit`, `fix_pr`);
- Unit tests introduced by the human patch (`hidden_tests`);
- Pristine issue description captured at ingestion time (`body`).

Benchmark execution harness: #98.

---

## Dataset Selection Methodology

The benchmark dataset was constructed via automated tooling:
- `scripts/eval/collect.py <owner/repo> <language> <since_date>`: candidate discovery;
- `scripts/eval/build.py`: synthesizes `.toml` issue definitions;
- `scripts/eval/verify.py`: validates issue eligibility.

1. Issue closed with state "completed" within a 6-month window (from 2026-04-21), resolved by a merged pull request in the upstream repository.
2. The resolving PR modifies production source code (`.rs`, `.py`, `.ts`, `.go`, `.java`, `.cs`, excluding `docs/` and `tests/`) while simultaneously adding or modifying tests.
3. PR excludes alterations to external dependencies, toolchains, or CI configurations: lockfiles, package manifests, `.github/`, `tox.ini`, `rust-toolchain`. Verification gates intentionally block unapproved dependency changes (#12, #83); including them would invalidate agent benchmark accuracy.
4. The patch touches at most 12 files and 300 lines of source code.
5. Issues must carry labels `bug` or `regression` and be closed after 2026-06-01 to reflect authentic software maintenance (Design §5.2) and minimize pre-training data leakage.
6. One PR corresponds strictly to one issue.
7. Verification with `verify.py`: at `base_commit`, applying only the PR's test files results in **failing** tests; at `fix_commit`, tests **pass**. Issues passing this gate include the `[verified]` metadata table.

Steps 1–6 yielded 21 `scrapy` and 20 `typst` candidates. Step 7 filtered the dataset down to **32 issues: 18 typst (Rust) and 14 scrapy (Python)**. Excluded candidates:

| Issue | Reason for Exclusion |
| --- | --- |
| typst-6183 | Tests located in `crates/typst-cli/tests`, unexecutable via core typst suite |
| typst-8679 | Comparison between base and fix revealed zero test files |
| scrapy-210, scrapy-7796 | Hidden tests already passed on `base_commit`. Issue 7796 required Python 3.14 (PEP 649) and skipped under 3.12 |
| scrapy-3552 | Tests hung on base commit (blocked on `--pdb` prompt, exceeding 30-min timeout) |
| scrapy-5794 | Test module failed import on both base and fix commits |
| scrapy-7260, scrapy-7626 | Failed on both commits: spawned `scrapy` CLI subprocesses asserting on environment-dependent log strings |
| scrapy-7601 | No tests executed: httpx handler required uninstalled optional dependencies |

Scrapy validation environment: Python 3.12, `test-requirements` and `extra-deps` from `tox.ini` (excluding `google-cloud-storage` and interactive shell utilities).

---

### Phase 2 Expansion: TypeScript, Go, Java, C# (#155, #220, #224)

Following the same criteria for issues closed after 2026-06-01. Exception for Criterion 5: `zod` does not utilize issue labels, and certain `gin` or `gson` issues lacked explicit labels; unlabelled issues were evaluated by issue content (retaining verified bugs, omitting feature requests).

After Step 7 verification, **20 issues** were incorporated:
- **TypeScript** (`colinhacks/zod`): zod-6027, zod-6641, zod-6643, zod-6657, zod-6659
- **Go** (`gin-gonic/gin`): gin-4622, gin-4638, gin-4760, gin-4818, gin-4851
- **Java** (`google/gson`): gson-992, gson-1247, gson-1587, gson-1768, gson-3113
- **C#**: `App-vNext/Polly` (Polly-3086, Polly-3286), `nsubstitute/NSubstitute` (NSubstitute-828, NSubstitute-974), `StackExchange/StackExchange.Redis` (StackExchange.Redis-2689)

Total benchmark comprises **52 issues**: 18 typst, 14 scrapy, 5 zod, 5 gin, 5 gson, 5 C#.

### Execution via Platform Harness (#259)

Each target repository maintains an isolated configuration mirroring `config/poc.toml`:
- `config/gin.toml`, `config/zod.toml`, `config/gson.toml`, `config/polly.toml`, `config/nsubstitute.toml`, `config/stackexchange-redis.toml`

Run benchmark evaluations:

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH aegis eval --config config/gin.toml --set eval/issues --replay --no-pr
PATH=$PWD/.workspace/tools/cbm:$PATH aegis eval --config config/polly.toml --set eval/issues --only Polly-3086 --only Polly-3286
```

---

## Schema Reference

| Field | Definition |
| --- | --- |
| `id` | `<repo>-<issue-number>`, also used as identifier in ledger and git branch `agent/<id>` |
| `language` | Language profile (`rust`, `python`, `typescript`, `go`, `java`, `csharp`) for per-ecosystem KPI tracking |
| `repo`, `issue`, `url`, `title`, `closed_at`, `labels` | Upstream issue metadata |
| `base_commit` | First parent of the merge commit: repository state immediately preceding the fix |
| `fix_commit`, `fix_pr` | Human reference patch commit and PR identifier |
| `source_files`, `test_files` | Files modified by the human patch |
| `hidden_tests` | Typst: test names under `tests/suite`; Scrapy, Zod: test file paths; Gin: `Test...` functions; Gson: test classes; C#: `Namespace.Class` test classes. Concealed from agent |
| `changed_tests` | Reference visual/hash output updates (typst only) |
| `hidden_known_failures` | Known sandbox environment failures (requiring external daemons or network access), excluded from scoring |
| `body` | Ingested issue text ensuring deterministic inputs across benchmark runs |
| `[verified]` | Verification outputs emitted by `verify.py` |

Hidden test commands:
- Typst (Rust): `cargo test --workspace --test tests -- "^(name1|name2)$"`
- Scrapy (Python): `python -m pytest -q -p no:cacheprovider <files>...`
- Zod (TypeScript): `vitest run <files>...`
- Gin (Go): `go test -v -run "^(Test1|Test2)$" ./...`
- Gson (Java): `mvn test -Dtest=TestClass1,TestClass2 -pl <module>`
- Polly, NSubstitute, StackExchange.Redis (C#): `dotnet test [--project] <project> --filter "FullyQualifiedName~Ns.Class.|..."`

---

## Codebase-Memory Call Graph Integration

Configurations `config/poc.toml` and `config/typst.toml` enable call graph traversal (Design §3.2, #185): prior to gate execution, the platform inspects call graphs to identify external callers of modified functions and supplies them to the agent (#203).

The platform requires `codebase-memory-mcp` available on `$PATH`. The prebuilt binary is packaged inside `aegis-server` container images. For local developer workstations, download v0.12.1 from [GitHub Releases](https://github.com/DeusData/codebase-memory-mcp/releases/tag/v0.12.1), verify checksums, and extract into `.workspace/tools/cbm/`:

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH aegis eval --config config/typst.toml --set eval/issues
```
