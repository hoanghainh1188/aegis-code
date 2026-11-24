# ADR 0019 – Spec-Driven Hierarchical Feature Lane (Small & Large Features)

- Date: 2026-11-24
- Status: Proposed
- Decider: @Nguyen Han Hoang Hai
- Issues: #679 (frames #675, #676, #677, #678; Milestone: `Phase 4 – AMS Quy mô Doanh nghiệp`)
- Related: [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), [ADR 0009](0009-prompt-injection-threat-model.md), [ADR 0011](0011-ams-direction.md), [ADR 0012](0012-triage-and-diagnostic-lane.md), #662 (job checkpointing), #669 (Phase 4 roadmap)

## Context

`aegis-code` was originally scoped strictly as a **code maintenance** platform: `IssueKind` (`crates/aegis-ledger/src/record.rs`) supports only `Bug`, `CiFailure`, and `Sonar` (plus the controlled CVE lane in [ADR 0015](0015-cve-remediation-lane.md)). Its verification gate (`aegis-gate`) proves correctness through a **Reproducing Test** (`FAIL` on `base` -> `PASS` on patch), and its economic circuit breaker bounds each job to `$4` / 40–80 model calls.

Expanding Aegis to handle **new feature requests**—ranging from small enhancements to large multi-module epics—breaks three assumptions of the bug-fix loop:

1. **No pre-existing bug to reproduce:** Features introduce new behavior rather than fixing broken behavior, so `ReproTest` (`FAIL` on `base`) does not apply.
2. **Design alignment must happen before coding:** For bugs, reviewing the final PR is sufficient. For features, an AI choosing the wrong schema, API contract, or module boundary wastes tokens and human review time if not aligned upfront.
3. **Large features cannot fit in one `drive()` loop or one PR:** Trying to synthesize a 15–30 file feature in a single `$4` / 40-call loop exhausts context and budget, and produces oversized PRs that violate the platform's human-reviewability goal.

## Decision

Introduce a **Spec-Driven Hierarchical Feature Lane** that handles both small features (1 sub-job) and large features (a DAG of sub-jobs) without loosening existing security or quality invariants:

### 1. Ledger, Store, and DAG Job Model (#675)
- Add `IssueKind::Feature` and `TriageCategory::FeatureRequest`.
- Add `Outcome::AwaitingApproval` so a planning job pauses without holding a worker lease or counting as failed while waiting for human sign-off.
- Extend `jobs` in `aegis-store` with `parent_job_id` and `depends_on` so PostgreSQL's `FOR UPDATE SKIP LOCKED` queue claims child jobs only after all prerequisites succeed.
- Enforce two-tier budgets: each sub-job retains the standard `$4` / 40–80 call circuit breaker, while the parent Epic has its own configurable total budget ceiling.

### 2. Gate Semantics for Features (#676)
- Add an **`AcceptanceTest`** mode to `aegis-gate` for `IssueKind::Feature`: verifies that new acceptance tests covering the RFC's criteria execute and **PASS** on the patched tree (and are non-trivial), without requiring them to fail on `base`. All other 10 gate steps (`Scope`, `Secrets`, `Dependencies`, `Format`, `Lint`, `Typecheck`, `Test`, `Coverage`, `Sonar`, `Integrity`) remain mandatory.
- Add **`DependencyPolicy::AllowProposed`**: permits adding only the exact packages/versions explicitly listed in the RFC and approved by a human at the Design Gate, still subject to `policies/` license and advisory audits.

### 3. Read-Only Architect Phase & Human Design Gate (#677)
- Before any code edit, an **Architect Phase** explores the repository (using read-only `aegis-aci`, `codebase-memory-mcp`, and `repo_memories`) and generates a structured **RFC + DAG Plan**:
  - Acceptance Criteria (Given/When/Then)
  - API / DB schema / interface contracts
  - Proposed dependencies (if any)
  - Breakdown into a **DAG of small Sub-Jobs** (1 sub-job for small features; multiple scoped sub-jobs for large features, each sized for a single `drive()` loop and a reviewable PR)
  - Recommended delivery mode (`FeatureDeliveryMode`)
- The RFC is sanitized (`redact` + `inert`), posted to the source issue/dashboard, and the job pauses in `Outcome::AwaitingApproval`.
- Only a write-authorized human commenting `/aegis approve [mode=integration_pr|stacked_prs]` unblocks and enqueues the DAG sub-jobs.
- A write-authorized human may also request design changes via `/aegis revise <feedback>`, resuming the Architect Phase (from its checkpoint, up to a configured revision cap) to publish an updated RFC and DAG before returning to `Outcome::AwaitingApproval`.

### 4. Dual Delivery Modes (`FeatureDeliveryMode`, #678)
Rather than forcing one PR strategy for all feature sizes, the platform supports configurable delivery at the repo or approval level:
- **`integration_pr`:** Sub-jobs that pass the gate are sequentially merged into an integration branch (`feature/<epic-id>`) with an integration gate after each merge (reusing the pattern in `aegis-sonar-batch/src/integrate.rs`). Once the DAG completes, Aegis opens **one final PR** into `main`. Best for cohesive small/medium features (2–3 sub-jobs).
- **`stacked_prs`:** Each sub-job opens an independent small PR targeting its prerequisite branch (or `feature/<epic-id>`), enabling incremental human review. Best for large, cross-module epics.
- **`auto` (default):** Selects `integration_pr` for small DAGs (e.g., `<= 3` sub-jobs) and recommends `stacked_prs` for larger DAGs; reviewers can override on `/aegis approve`.

### 5. Feature Feedback Loops (Design, Code Review & Architectural Learning)
The Feature Lane closes three distinct feedback loops:
1. **Design Feedback Loop (`/aegis revise`):** Iterates on the RFC and DAG at `Outcome::AwaitingApproval` before any code is written, eliminating wasted implementation turns.
2. **Code Review Loop (Review Rounds):** Supports standard Aegis review rounds (*Request changes* / `/aegis fix`, no force-push) on each individual sub-PR (`stacked_prs`, rebasing downstream sub-jobs if needed) or on the `feature/<epic-id>` branch (`integration_pr`).
3. **Architectural Learning Loop (`repo_memories`):** When a Feature Epic is merged, Aegis extracts not only `MergedPr` and `ReviewLesson` entries, but also architectural invariants and human design corrections from the approved/revised RFC into `BusinessRule` memories (and `RejectedPattern` on rejected sub-PRs, #657). Subsequent Architect and bug-fix sessions recall these rules into `BP3`.

## Security & Operational Invariants

- **Human-in-the-Loop at both Design and Merge:** Aegis never starts coding a feature without explicit human approval of the RFC/DAG, and never merges autonomously.
- **Untrusted Input Boundary ([ADR 0009](0009-prompt-injection-threat-model.md)):** Feature descriptions, comments, and recalled memories remain untrusted (`redact` + `defang`, `BP3` only). `/aegis approve` is accepted only from users with write permissions on the repository.
- **Resilience:** Combines naturally with machine-independent job checkpointing (#662) and hybrid/remote execution (#664, #665) so parallel sub-jobs of an Epic can execute across workers/pods without resource contention.

## Consequences

- **Positive:** Unifies small enhancements and large multi-module features under one architecture without breaking `$4` sub-job bounds or flooding reviewers with 3,000-line PRs.
- **Positive:** Reuses existing platform building blocks (`SKIP LOCKED`, `aegis-sonar-batch` integration branching, `aegis-gate`, `repo_memories`, and checkpointing).
- **Negative / Trade-off:** Adds human latency at the Design Gate (`AwaitingApproval`) and introduces DAG scheduling/rebasing complexity for `stacked_prs`.
