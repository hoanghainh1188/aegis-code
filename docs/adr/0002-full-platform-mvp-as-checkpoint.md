# ADR 0002 – Full Platform Vision with MVP/PoC as KPI Evaluation Milestone

- Date: 2026-10-18
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Partially Supersedes: [ADR 0001](0001-platform-in-rust.md), Section 4 (PoC stopping gate)

## Context

The initial Proposal positioned the PoC as a binary "go / no-go" gate: failing to meet KPI thresholds (Proposal §8) would terminate the entire initiative. ADR 0001 initially retained this stopping gate.

The strategic objective has evolved to building a comprehensive production platform: running as a persistent multi-tenant service, integrating with enterprise ticket workflows, managing multi-repository workloads, and offering web dashboards with role-based access control — far beyond an isolated PoC script.

## Decision

1. The project objective is a comprehensive, production-grade platform (spanning Phase 0 through Phase 3 as outlined in Proposal §9).
2. The MVP/PoC evaluation over 30–50 benchmark issues is retained as an **empirical milestone to steer technical direction**, rather than a binary stopping gate:
   - Meeting KPI thresholds $\rightarrow$ Proceed directly along the roadmap.
   - Falling short of targets $\rightarrow$ Analyze root causes from token ledgers (fault localization, token burn, cache hit rates, gate bottlenecks) and prioritize hardening the core agent loop before expanding service layers. Hard caps and thresholds are adjusted against empirical data.
3. Implementation sequence remains strictly layered: complete and measure the core engine first (provider $\rightarrow$ agent loop $\rightarrow$ ACI $\rightarrow$ gate $\rightarrow$ ledger $\rightarrow$ issue lifecycle) before constructing the service layer (Phase 2) and scaling capabilities (Phase 3). Every platform feature relies entirely on core engine reliability.
4. Each phase corresponds to dedicated GitHub milestones and issues; this ADR serves as the canonical roadmap authority.

## Consequences

- Project roadmap establishes explicit milestones for `Phase 2 – Platform` and `Phase 3 – Advanced Scaling`.
- Core engine architecture must support concurrent issue execution, multi-repository settings, and persistent state storage — even when the initial MVP runs sequentially from a developer CLI. Specifically: avoid global static state, isolate configuration per repository, and abstract ledger persistence behind traits swap-able with PostgreSQL.
- Service layer architecture and core engine design constraints are formalized in [ADR 0003](0003-platform-service-architecture.md).
- The risk of "building service scaffolding over an unproven engine" is mitigated by mandatory Phase 1 KPI evaluation and continuous dashboard observability in Phase 2.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Retain PoC as a binary stopping gate | Conflicts with the long-term objective of delivering an enterprise-ready platform |
| Bypass KPI benchmarking and build platform directly | High risk of over-investing in service infrastructure before verifying patch quality, cost predictability, and regression guarantees |
