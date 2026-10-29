# ADR 0004 – Multi-Language Target Repositories via Language Profiles

- Date: 2026-10-20
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Amendments: 2026-10-20, added C#/.NET profile (#61); 2026-10-25, directory-scoped profiles for monorepos (#71); 2026-10-26, fail-closed build artifact policy across directory boundaries (#200)
- Related: [ADR 0001](0001-platform-in-rust.md), [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), [ADR 0003](0003-platform-service-architecture.md), Proposal §3, §6, §9, Design §3.1, §6.1, §7

## Context

The initial Proposal assumed target repositories would be exclusively "internal, Rust-based, > 100k LoC", resulting in hardcoded Rust assumptions across several subsystems:

| Component | Hardcoded Rust Assumptions |
| --- | --- |
| ACI (Design §3.1, #7) | Tree-sitter Rust grammar, `rust-analyzer`; `edit` runs `rustfmt` |
| Gate (Design §6.1, #11) | `cargo fmt` $\rightarrow$ `clippy` $\rightarrow$ repro test $\rightarrow$ `cargo test` $\rightarrow$ `cargo llvm-cov` |
| Dependency Sandboxing (Design §7, #12) | `Cargo.lock`, `cargo deny` |
| Sonar (#17, #27) | Ingests Clippy and LCOV reports |
| Sandbox & Caching (#5, #2) | Rust toolchain container, caches `target/` and Cargo registry |
| Prompts (#13) | Rust idiomatic conventions |

The business objective now encompasses arbitrary enterprise repositories in any mainstream programming language. Postponing multi-language support to Phase 3 (#37) would require rewriting ACI, gates, and sandboxes after the core engine stabilizes.

The language-agnostic core engine (provider, agent loop, token ledger, router) is decoupled from target languages. The language-dependent components (ACI, verification gate, sandboxes, dependency policy) must be generalized via declarative abstractions.

## Decision

### 1. Declarative Language Profiles
All language-dependent behaviors are encapsulated within a **Language Profile** contract:

| Dimension | Specification | Representation |
| --- | --- | --- |
| Detection | Marker manifests (`Cargo.toml`, `pyproject.toml`, `package.json`, `pom.xml` / `build.gradle*`, `go.mod`, `*.sln` / `*.csproj`) | Metadata |
| ACI | Tree-sitter grammars and symbol extraction queries for `search_symbol`; optional LSP commands | Compiled grammars; `.scm` query files |
| Edit Validation | Syntax validation via tree-sitter (runs offline without toolchains); repository formatters | Metadata |
| Gate Pipeline | Named verification stages: format, lint, repro test (command template with `{test}`), full test suite, coverage report format | Metadata |
| Dependencies | Pre/post-patch lockfile parsing, detecting newly introduced dependencies; security vulnerability and license auditing | Ecosystem code |
| Sandbox | Toolchain base container image, persistent cache directories, fetch commands | Metadata |
| Sonar | Imported linter reports and coverage formats | Metadata |
| Prompts | Idiomatic language conventions (BP2, Design §4.1) | Text markdown |

- **Default Commands as Baselines:** Repositories often maintain custom CI commands (`npm run lint`, `./gradlew check`, `make test`). `RepoConfig` (ADR 0003 §7) supports granular step overrides. Omitted steps are skipped and recorded cleanly in gate outputs without fabricating commands.
- **Repository Profile Declarations:** Repositories declare their primary profile in `RepoConfig`. Manifest detection serves as an onboarding suggestion.
- **Monorepos (#71):** `[[repo.dirs]]` (`path`, `language`, `overrides`) maps specific directories to distinct language profiles; `language` defines the root fallback. Subdirectories support nesting (files resolve to the deepest enclosing directory). ACI parses files using the enclosing profile grammar, executing formatters and `run_tests` within that directory (in isolated sandbox containers, #198). Gate verification executes only for modified directories in sequential order. Sonar executes globally once after coverage generation across modified sub-projects (#199). Dependency gates enforce a fail-closed policy: modifying manifest or lockfile paths recognized by a parent profile from within an uninspected sub-project halts the gate (#200).
- **Static Dispatch (ADR 0003 §7):** `enum Language { Rust, Python, TypeScript, Java, Go, CSharp }`, avoiding dynamic trait objects (`Box<dyn ...>`). Adding a language requires adding an enum variant alongside profile data.
- Shared logic resides in crate `aegis-lang`: profile schemas, configuration loaders, tree-sitter grammars, and lockfile parsers. `aegis-aci` and `aegis-gate` consume profiles without referencing language-specific tool names.

### 2. Six Canonical Language Profiles

| Profile | ACI (Tree-sitter / LSP) | Format / Lint | Test / Coverage | Lockfile |
| --- | --- | --- | --- | --- |
| **Rust** | `tree-sitter-rust` / `rust-analyzer` | `rustfmt`, `clippy` | `cargo test` / `cargo llvm-cov` (LCOV) | `Cargo.lock` |
| **Python** | `tree-sitter-python` / `basedpyright` | `ruff format`, `ruff check` | `pytest` / `coverage.py` (Cobertura XML) | `uv.lock`, `poetry.lock`, pinned `requirements*.txt` |
| **TypeScript / JavaScript** | `tree-sitter-typescript`, `tree-sitter-javascript` / `typescript-language-server` | `prettier`, `eslint`, `tsc --noEmit` | Repo test runner (Jest, Vitest...) / LCOV | `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` |
| **Java** | `tree-sitter-java` / `jdtls` | Spotless, Checkstyle | Maven / Gradle / JaCoCo XML | Gradle lockfile; Maven resolved dependency tree |
| **Go** | `tree-sitter-go` / `gopls` | `gofmt`, `go vet`, `golangci-lint` | `go test` / `-coverprofile` | `go.sum` |
| **C# / .NET** | `tree-sitter-c-sharp` / Roslyn language server | `dotnet format --verify-no-changes`, Roslyn analyzers (`dotnet build -warnaserror`) | `dotnet test` / coverlet (OpenCover) | `packages.lock.json` |

- All six profiles feature production-grade tree-sitter grammars, established language servers, standardized lockfile formats, and official SonarQube Server / Cloud scanner plugins.
- **C# / .NET Integration:** SonarQube for .NET wraps the compilation step (`begin` $\rightarrow$ `dotnet build` $\rightarrow$ `end`). The gate pipeline for .NET embeds Sonar analysis directly within the build step.
- Additional ecosystems (Kotlin, PHP) are scheduled for Phase 3 (#37).

### 3. Universal Dependency Auditing and Sandboxing
- Retains the security boundary established in #12 across all ecosystems: existing dependencies in the default branch lockfile are retained; new dependencies must appear in the repository allow-list and originate from official registries. Redirecting packages to unauthorized git URLs or rogue registries is blocked.
- Dedicated lockfile parsers in `aegis-lang`.
- **Security Advisories and License Compliance:** Rust utilizes `cargo deny` with `policies/deny.toml`. Other ecosystems leverage **`osv-scanner`** (#65) against lockfiles, enforcing license allow-lists (`policies/licenses.toml`). License lookups query deps.dev APIs during the dependency fetch phase (#66) outside the offline gate.

### 4. Dependency Ingestion with Isolated Network Egress
Verification gates execute tests with `--network none` (Design §7). For ecosystems with complex installation routines (Node, Python, Java, .NET) or lifecycle install hooks (`postinstall`, `setup.py`):
- Dependency resolution runs as a dedicated pre-gate step in an isolated container. The container accesses only authorized package registries via an allow-listed egress proxy, carrying zero secrets and mounting no external workspaces.
- Pre-gate fetch executes only after lockfile validation (Section 3) passes. Unauthorized dependencies are never downloaded.
- Package lifecycle scripts are disabled whenever supported (`npm ci --ignore-scripts`).
- Downloaded artifacts (caches, virtualenvs, `node_modules`) are mounted read-only into the sandbox container; the gate executes in strict `--network none` isolation.

### 5. Implementation Roadmap

| Phase | Deliverables |
| --- | --- |
| **Phase 1 – MVP / PoC** | Crate `aegis-lang` and profile schema; **Rust** and **Python** profiles. ACI (#7), gate (#11), sandbox (#5), dependency auditing (#12), and prompts (#13) engineered around profiles from Day 1. Benchmark KPIs over one Rust and one Python repository |
| **Phase 2 – Platform** | **TypeScript/JavaScript**, **Java**, **Go**, **C#/.NET** profiles; monorepo support (directory-scoped profiles) alongside multi-repo management (#24) |
| **Phase 3 – Advanced Scaling** | F#, VB.NET extensions; secondary language profiles as required (#37) |

### 6. Retaining Rust for Platform Implementation
ADR 0001 selected Rust partly due to language parity with initial target repos. While target repos now span diverse languages, Rust remains the platform language:
- Core engine is fully implemented and tested in Rust: provider, agent loop, token ledger, model router.
- Core rationale of ADR 0001 holds: no Agent SDK dependency, full control over prompt caching layouts and deterministic breakpoints.
- Single static binary, minimal memory footprint, strong type safety, ideal for resilient system daemons (ADR 0003).
- Rust remains a supported profile, allowing Aegis to autonomously maintain its own codebase.

## Consequences

- Introduces `aegis-lang` crate bundling tree-sitter grammars for six languages, audited via `cargo deny` (#42).
- One container sandbox image per profile.
- Minimal test fixtures created in `tests/fixtures/` for unit testing ACI, gates, and lockfile parsers without full repository clones.
- KPIs reported per language profile. The evaluation benchmark (#4) incorporates issues across all active profiles.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Retain Rust-only, defer others to Phase 3 (#37) | Forces hardcoded Rust assumptions into core crates, requiring complete rewrites later |
| Shell-only commands without language-aware ACI | Deprives agent of `search_symbol` and AST syntax guards during edits; SWE-agent research proves structured AST tools outperform freeform shell access |
| LSP-only without tree-sitter | LSPs have heavy startup latencies and struggle on syntax-broken files; tree-sitter runs instantly on partial or invalid code |
| Language plugins via WASM or IPC | Unnecessary architectural complexity for six core enterprise languages; static enum dispatch satisfies performance and safety requirements |
| Rewrite platform in Python or TypeScript | Core engine is already built and verified in Rust; rationale for avoiding Agent SDK in ADR 0001 remains valid |
