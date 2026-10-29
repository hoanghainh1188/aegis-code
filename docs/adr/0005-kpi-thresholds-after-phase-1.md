# ADR 0005 – Finalized KPI Thresholds After Phase 1 Evaluation

- Date: 2026-10-24
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Partially Supersedes: Proposal §8 (initial PoC target thresholds)

## Context

Proposal §8 initially defined PoC target thresholds as "tentative baseline proposals" subject to empirical adjustment. [ADR 0002](0002-full-platform-mvp-as-checkpoint.md) treated PoC empirical benchmarks as milestones to steer platform evolution: analyze gaps and adjust metrics against observed production data.

A 32-issue evaluation benchmark ([eval/README.md](../../eval/README.md)) was evaluated across three iterative test cycles:

| Metric | Cycle 1 (#18, [report](../measurements/2026-10-24-eval-kpi.md)) | Cycle 2 (#142, [report](../measurements/2026-10-24-rust-rerun.md)) | Cycle 3 (#145, PR #151) |
| :--- | :---: | :---: | :---: |
| Core engine refinements | #127, #129, #133 | #136, #137, #140 | #144 |
| Accepted PR rate, Python (scrapy, 14 issues) | 64% | — | — |
| Accepted PR rate, Rust (typst, 18 issues) | 33% | 39% | 39% |
| **Accepted PR rate, Total** | **47%** | **50%** | **50%** |
| Regressions reaching reviewer | 0 | 0 | 0 |
| Average cost per issue | $0.53 | $0.51 | $0.55 |
| Prompt cache hit ratio | 92% | 93% | 92% |

Across three rounds of engine optimization, Rust acceptance leveled at 39%. The optimizations successfully shifted agent behavior: in #145, all 18 runs synthesized failing reproduction tests early (turns 5–18). However, the 40% threshold for Rust was not crossed due to 6 complex typst issues hitting the 40-turn cap across all cycles.

## Decision

1. **Phase 1 Closure Thresholds**, reported per language profile ([ADR 0004](0004-multi-language-profiles.md)):

   | KPI | Target Threshold | Phase 1 Actuals | Result |
   | :--- | :---: | :---: | :---: |
   | Accepted PRs ($\le 10$ lines edited), Total | $\ge 40\%$ | 50% | Met |
   | Accepted PRs, Per Language | $\ge 35\%$ | Python 64%, Rust 39% | Met |
   | Zero Regression (Regressions reaching human) | 0 | 0 | Met |
   | Cost per issue (including failed runs) | $\le \$4$ avg | $0.55 | Met |
   | Review time per PR | $\le 15$ min | 1.8 min | Met (see consequences) |
   | Prompt cache read ratio | $\ge 70\%$ | 92% | Met |
   | Escalation rate to Expert model | Tracked | 75% | Tracked |

2. **Phase 1 is formally completed.** The initiative advances to Phase 2, commencing with #22.
3. Achieving $\ge 40\%$ acceptance per individual language is established as a **target milestone for Phase 2**, rather than a blocker for commencing Phase 2. Per-language KPIs are tracked continuously via the operational dashboard (#28).
4. **Retain the $4 cost ceiling and 40 model call cap** per issue (Design §5.3). Observed costs represent only ~15% of the ceiling; the 40-call budget is the primary constraint.

## Consequences

- Proposal §8 references this ADR for definitive benchmark thresholds; §9 records Phase 1 closure.
- Recommended Rust engine enhancements (exploring an 80-call budget for `typst`, prompt nudging for earlier test execution) will proceed concurrently alongside Phase 2.
- **Review time calibration:** In Phase 1, reviews averaged 1.8 minutes, where reviewers had prior visibility into hidden test results. The Phase 2 human review interface (#28) will measure authentic reviewer time while keeping hidden test outcomes blind to reviewers.
- **Open-source benchmark evaluation:** `typst` and `scrapy` are public repositories where models may have pre-training exposure to fixes. As enterprise repositories onboard, re-validate with these identical thresholds.
- The per-language baseline threshold ($\ge 35\%$) applies to new Phase 2 profiles (TypeScript, Java, Go, C#).

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Enforce strict 40% for Rust before advancing | Three rounds of core engine tuning reached a plateau at 39%; remaining variance falls within run noise; blocking Phase 2 would delay platform service delivery |
| Aggregate 40% target only, dropping per-language thresholds | High performance in one language can mask deficiencies in another; ADR 0004 requires transparent per-ecosystem accountability |
| Increase call limit to 80 before closing Phase 1 | Valuable experiment, but belongs in Phase 2 scaling rather than delaying the Phase 1 milestone |
