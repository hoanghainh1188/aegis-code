# ADR 0001 – Platform Implementation in Rust with Custom Agent Loop

- Date: 2026-10-17
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai

## Context

The initial Proposal and Technical Design selected the Claude Agent SDK as the agent harness (leveraging its built-in tool loop, file manipulation tools, and command execution primitives) while implementing custom logic only for missing components.

The project scope has since expanded to building a comprehensive production platform, and the engineering team decided to utilize Rust — the same language as the primary maintained repository (`typst`). The Claude Agent SDK only provides Python and TypeScript distributions; Anthropic does not maintain an official Rust SDK.

Upon re-evaluating the Agent SDK's value proposition, its actual contribution proved smaller than expected because the technical design already specified:

- Custom ACI tool suite (Design §3.1), replacing default file read/edit tools;
- Dedicated history compaction and observation collapsing rules (§3.3);
- Deterministic prompt caching layout with 4 strict breakpoints (§4.1);
- Model router, token ledger, and gate runner (§5, §6, §8).

The remainder of the SDK amounted to a standard tool-use execution loop of several hundred lines of code.

## Decision

1. The platform is written entirely in **Rust**, structured as a Cargo workspace with one crate per module (`crates/`).
2. Implement a custom agent loop. Model providers are invoked via the `ModelProvider` trait with two implementations:
   - Claude API — direct HTTP client without third-party community crate dependencies.
   - AWS Bedrock — official AWS SDK for Rust.
   Data sovereignty assertion "Model ID must prefix with `jp.`" (Design §7) is enforced inside the provider layer.
3. The ACI tool suite is implemented as an MCP server using the official Rust MCP SDK. The orchestrator interacts with ACI and Codebase-Memory using the standard MCP protocol.
4. *(Superseded by [ADR 0002](0002-full-platform-mvp-as-checkpoint.md): PoC is an evaluation milestone, not a stopping gate.)* Retain the PoC evaluation milestone from Proposal §9: design toward a full platform, but implement sequentially (provider $\rightarrow$ agent loop $\rightarrow$ ACI $\rightarrow$ gate $\rightarrow$ ledger), evaluating against a 30–50 issue suite before building pilot interfaces (ticket connectors, web dashboard).

## Fallback Strategy

If developing the custom agent loop delays the PoC timeline: invoke Claude Code headless (`claude -p`) as a subprocess from Rust, connect ACI via MCP, and parse token usage from the output JSON. This fallback applies strictly to the PoC configuration (Claude API); enterprise customer workloads remain routed through the Bedrock provider.

## Consequences

- Direct ownership of the agent loop, retry strategies, streaming, `cache_control` placement, and history collapsing.
- Complete control over prompt layout and cache breakpoints $\rightarrow$ ensures meeting the KPI threshold of cache read ratio $\ge 70\%$.
- Immune to breaking upstream changes in Python/TypeScript SDKs, while requiring self-managed alignment with Anthropic Messages API updates.
- Language parity with target repositories facilitates contributions and opens the path toward self-hosting and self-maintenance. *(Re-evaluated in [ADR 0004](0004-multi-language-profiles.md): target repositories span multiple languages; the platform remains in Rust).*

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Python/TS + Agent SDK (Original Design) | Team prefers Rust; SDK provides minimal leverage given custom ACI, caching, and collapsing requirements |
| Hybrid: Python/TS Orchestrator + Rust Tools | Dual-language runtime creates excessive operational and CI/CD overhead for a compact platform |
| Community Rust crates for Claude API | Unofficial and prone to delays in adopting prompt caching and reasoning effort parameters |
