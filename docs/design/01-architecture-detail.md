# Technical Architecture Specification – Core Engine

Oct 17, 2026 · @Nguyen Han Hoang Hai

This specification details the technical architecture of the core engine (Phase 1 – MVP / PoC), engineered to support the platform vision defined in [ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md) and adhering to architectural constraints in [ADR 0003](../adr/0003-platform-service-architecture.md) §7. Developers implement against this document. In case of roadmap discrepancy, ADR 0002 and ADR 0003 serve as canonical references.

---

## 1. Scope & Baseline Assumptions

| Parameter | Specification |
| --- | --- |
| Target Repositories | Internal repositories > 100k LoC in any language supported by Language Profiles ([ADR 0004](../adr/0004-multi-language-profiles.md)); PoC evaluated on Rust and Python |
| Test Execution | Pre-existing CI test suites |
| Platform Implementation | 100% Rust, Cargo workspace (`crates/`) — see [ADR 0001](../adr/0001-platform-in-rust.md) |
| Agent Harness | Custom Rust agent loop; models invoked via `ModelProvider` trait (Claude API via HTTP, Bedrock via AWS SDK for Rust) |
| Codebase Indexing | Codebase-Memory MCP (call graphs, #185), Tree-sitter (AST queries, module maps), and Language Server Protocol (LSP) per profile (Sections 3.1, 3.2) |
| Execution Sandbox | Docker container per profile, network-isolated (`--network none`); dependencies resolved pre-gate via allow-listed egress proxy |
| SonarQube | Supports SonarQube Server and Cloud. The Sonar gate executes when configured with `[sonar]` (#91) |
| Output | Dedicated branch `agent/<issue-id>` + structured PR; human review and merge |

**Dual Operating Configurations:**
- **PoC Configuration (Internal Repositories):** Direct Claude API invocation. No regional restrictions.
- **Enterprise Customer Configuration (Strict Japan Residency):** AWS Bedrock Tokyo (`ap-northeast-1`) using regional inference profile prefix `jp.`.
All variations between environments are encapsulated strictly in configuration files, never hardcoded in code.

---

## 2. Component Taxonomy & Execution Loop

**Core Crates & Responsibilities:**

| Component | Responsibility | Implementation |
| --- | --- | --- |
| **Orchestrator** | Issue workflow, session turns, state persistence, token costing | Custom (`crates/aegis-orchestrator`) |
| **Model Provider** | Claude API & AWS Bedrock clients, `cache_control` placement, regional enforcement | Custom (`crates/aegis-provider`) |
| **Router** | Model tier selection, escalation rules, budget circuit breakers | Custom (`crates/aegis-router`) |
| **ACI Tools** | Bounded code inspection, AST search, syntax-safe edits | Custom (`crates/aegis-aci`, Rust MCP server) |
| **Language Profile** | Ecosystem-specific grammars, linters, test commands, lockfile parsers | Custom (`crates/aegis-lang`, [ADR 0004](../adr/0004-multi-language-profiles.md)) |
| **Codebase-Memory MCP** | Relational call graph indexes in SQLite | Integrated (`codebase-memory-mcp` v0.12.1) |
| **Gate Runner** | Format, lint, test, coverage, and Sonar validation in Docker | Custom (`crates/aegis-gate`) |
| **PR Synthesizer** | Opens branches and PRs, formats root-cause analysis | Custom (`crates/aegis-pr`) |
| **Token Ledger** | Per-call token accounting and USD cost tracking | Custom (`crates/aegis-ledger`) |
| **Sonar Batcher** | Clusters static analysis issues, manages integration branches | Custom (`crates/aegis-sonar-batch`, #17) |

**Issue Execution Loop:**
1. Light model reads issue, performs classification, and prunes stack traces.
2. Worker initializes branch `agent/<issue-id>` from the default branch.
3. Core model localizes root cause via ACI tools, module maps, and Codebase-Memory call graphs.
4. Core model synthesizes a minimal reproduction test, confirming that it **FAILS**.
5. Core model synthesizes code patch.
6. Gate runner executes 6-step verification suite inside isolated Docker sandbox (Section 6.1).
7. Passes $\rightarrow$ PR writer submits structured pull request. Job completes.
8. Fails $\rightarrow$ Failure output pruned by Light model, fed back to Core model (resumes Step 5).
9. Escalation trigger hit $\rightarrow$ Escalates to Expert model with full conversation history.
10. Circuit breaker hit ($4 limit / 40 turns) $\rightarrow$ Terminates execution, logs cause in ledger, hands off to human engineer.

---

## 3. Layer 1 – Context Engine

### 3.1 Bounded ACI Tool Suite

All tools return structured outputs with line numbers and file paths, avoiding raw shell emissions:

| Tool | Parameters | Output | Operational Bounds |
| --- | --- | --- | --- |
| `search_symbol` | Symbol name (function, struct, class) | Signatures, file paths, line ranges | Capped at 50 results |
| `search_text` | String literal, directory scope | Matching file and line occurrences | Capped at 50 results |
| `find_file` | Filename or glob pattern | Matching relative file paths | Capped at 50 results |
| `find_definition` / `find_references` | File + line, or symbol name | Exact definition or reference sites | Proxies to profile LSP (`aci.lsp`, #115); capped at 50 results |
| `open` | File path, starting line | 100-line window with line numbers | Exactly 100 lines per window |
| `scroll_up` / `scroll_down` / `goto` | Direction or target line | Shifted 100-line viewing window | — |
| `edit` | File, line range, replacement text | Surrounding diff context ($\pm 5$ lines) | Pre-validated via Tree-sitter + profile formatters; invalid edits rejected immediately |
| `run_tests` | Test name identifier | Result (PASSED/FAILED), failed test names, trailing output | Sandboxed test execution capped at 240 s timeout (#137) |

The 100-line viewing window and 50-item search limits adhere to SWE-agent empirical findings: bounded windows significantly outperform full-file dumps and micro-slices (30 lines).

### 3.2 Codebase-Memory MCP Call Graph Indexing

- Per-job SQLite call graph database created under `<state_dir>/index`.
- Relational edges: `CALLS`, `IMPORTS`, `USAGES`, `IMPLEMENTS`, `INHERITS`, `TESTS`.
- Pre-Gate Change Detection (#203): Prior to gate execution, the platform queries the call graph to detect callers/callees of modified functions outside the patch files (`detect_changes`), presenting impact analysis to the agent.
- Language Server Protocol (LSP) integration (`find_definition`, `find_references`) provides symbol resolution where macro expansion or dynamic imports challenge static ASTs.

### 3.3 History Compaction & Observation Collapsing

- Core agent reasoning and actions are strictly preserved across all turns.
- Tool observation outputs (file contents, compiler logs) older than the 5 most recent turns collapse into `[Observation collapsed]` tokens when context utilization exceeds 60% of the model window.
- Collapsing executes in batches to maintain prefix stability and protect prompt caching.

### 3.4 SonarQube Batching

When ingesting multiple static analysis warnings:
- Single-location alerts cluster by enclosing AST node (function or class).
- Dataflow alerts (source $\rightarrow$ propagation $\rightarrow$ sink) cluster by tainted dataflow trace.
- Clusters execute concurrently in isolated worktrees, merging into integration branches prior to final submission (Section 6.3).

---

## 4. Layer 2 – Deterministic Prompt Caching

### 4.1 4-Breakpoint Hierarchy

| Order | Section | Stability | Cache Breakpoint |
| :---: | --- | --- | :---: |
| 1 | Tool definitions (ACI + MCP schemas) | Immutable across session | **BP1** |
| 2 | System prompt + profile conventions + module maps | Fixed per repository | **BP2** |
| 3 | Issue description + reproduction test metadata | Fixed per issue | **BP3** |
| 4 | Message history and tool observations | Incremental per turn | **BP4 (Dynamic)** |

### 4.2 Cache Retention and Economics

- 5-minute cache breaks even on the 1st read; 1-hour cache breaks even on the 2nd read.
- Input cost calculation formula:
  $$C_{in} = R \cdot (N_{read} \cdot M_{read} + N_{write} \cdot M_{write} + N_{uncached})$$
  Where $M_{read} = 0.05$ (5% of base price for Sonnet/Opus), $M_{write} = 1.25$ (for 5-min TTL).

---

## 5. Layer 3 – Model Routing & Escalation

### 5.1 Model Allocations

| Tier | PoC (Claude API) | Customer Production (AWS Bedrock Tokyo) |
| --- | --- | --- |
| **Light** | `claude-haiku-5-5` | `jp.anthropic.claude-haiku-5-5` |
| **Core** | `claude-sonnet-5-5` | `jp.anthropic.claude-opus-5-5` (low effort) |
| **Expert** | `claude-opus-5-5` (high effort) | `jp.anthropic.claude-opus-5-5` (high effort) |

### 5.2 State Transition Matrix

| Current State | Condition | Next State |
| :--- | --- | :--- |
| **Core** | Verification gate passes | PR Synthesis |
| **Core** | Syntax linter rejects edits twice consecutively | Expert |
| **Core** | Repro test remains failing after 2 patch attempts | Expert |
| **Core** | 15 turns elapsed without a patch candidate (#127) | Expert |
| **Core / Expert** | 8 turns elapsed without invoking `run_tests` (#144) | Prompt nudge injection |
| **Expert** | Verification gate passes | PR Synthesis |
| **Expert** | Fails gate twice | Terminate (Handoff) |
| **Any** | Spend $> \$4$ or $> 40$ model turns | Terminate (Handoff) |

---

## 6. Layer 4 – Verification Gate

### 6.1 6-Step Verification Pipeline

Executes sequentially in isolated Docker sandboxes (`--network none`):

| Step | Scope (Rust Profile Example) | Pass Criteria |
| :---: | --- | --- |
| **0. Dependencies** | Validates lockfile changes against allow-list and CVE databases | Zero rogue packages |
| **1. Formatting** | `cargo fmt --check` | Zero diff |
| **2. Linting** | `cargo clippy --all-targets -- -D warnings` | Zero compiler warnings |
| **3. Repro Test** | `cargo test <repro-test-name>` | Transitions from FAIL to PASS |
| **4. Regression** | `cargo test --workspace` | All existing tests pass |
| **5. Coverage** | `cargo llvm-cov --workspace --lcov --output-path lcov.info` | Valid coverage report |
| **6. SonarQube** | `sonar-scanner` (ingests linter and coverage reports) | Passes Sonar Quality Gate (#91) |

### 6.2 Golden Master Characterization Testing

For untested legacy code:
1. Agent synthesizes characterization tests asserting on existing behavior on the base commit (must PASS).
2. After patching, tests whose outputs are intentionally altered must be explicitly declared in `allowed_test_changes` or `allowed_reference_changes` (#147) with technical justifications.
3. Human reviewers review and sign off on declared behavior changes.

### 6.3 Structured Pull Request Format

```markdown
## Issue
<issue-link>

## Root Cause
<concise-explanation>

## Changes
- <file:line> — <description-and-rationale>

## Verification & Tests
- Reproduction Test: <name> (FAIL → PASS)
- Permitted Test Changes: <list-or-none>
- Updated Golden Master References: <list-or-none>

## Gate Results
fmt ✔ · lint ✔ · repro-test ✔ · full-test ✔ · coverage <x%> · Sonar ✔

## Token Ledger & Cost
<model-breakdown> · <turns> · <total-usd>
```

---

## 7. Operational Security Safeguards

- **Network Air-Gap:** Sandboxes execute under `--network none`, `--cap-drop ALL`, non-root user (uid 10001).
- **Bounded Containers (#320):** Every sandbox container (gate steps, `run_tests`, formatter, LSP, fetch, egress proxy) has bounded memory with no swap, CPUs and processes (`[run.sandbox_limits]`; a profile's `sandbox.memory_mb` / `sandbox.cpus` override), and a read-only image except a size-capped `/tmp` and the profile's `sandbox.writable` directories (per-container volumes). The flags are built in one place (`aegis_gate::Hardening`). A gate step whose process the kernel kills for memory is reported as out of memory (`State.OOMKilled`), not as an ordinary failure. The Sonar scanner, a third-party image running no patch code, keeps a writable image.
- **Protected Files (#352):** A patch that changes CI (`.github/`, `.gitlab-ci.yml`, ...), CODEOWNERS, or the configuration of the checks (Sonar, linters, coverage, test runners) fails the dependency step, whatever the profile (`aegis_lang::ALWAYS_PROTECTED`); a profile adds its own `dependencies.protected`.
- **Zero Secret Exposure:** Container environments never mount host `.env` files or platform credentials.
- **Dependency Ingestion Sandbox:** Packages are fetched strictly via an egress proxy restricted to official package registries.
- **Indirect Prompt Injection Defense:** All comments, README files, and issue bodies are treated as untrusted data; system prompts explicitly instruct agents to reject embedded user commands.

---

## 8. Empirical Metric Telemetry

The token ledger records structured JSON metrics per issue: `issue_id`, `outcome`, `tiers_used`, `model_calls`, `tokens` (input, output, cache reads/writes), `cost_usd`, `gate_runs`, `wall_time_s`, and human review outcomes.
