# ADR 0014 – Per-Language Default Call Cap, 80 for Rust

- Date: 2026-11-14
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #471 (Milestone: `Phase 2 – Platform`)
- Amends: [ADR 0005](0005-kpi-thresholds-after-phase-1.md), Decision 4 (one 40-call cap per issue)
- Related: [ADR 0004](0004-multi-language-profiles.md), Design §5.3, [#156 report](../measurements/2026-11-14-typst-call-cap.md)

## Context

ADR 0005 kept a $4 cost ceiling and a 40-call cap (Core + Expert calls) for every issue, in every language. The cap was then the binding limit for Rust. Six typst issues ran out of calls in each of the three Phase 1 cycles, while the costs stayed near 15% of the ceiling.

#156 set an 80-call cap in `config/typst.toml` and ran its 8 typst issues twice ([report](../measurements/2026-11-14-typst-call-cap.md)):

- `typst-7541` passes only above 40 calls, in both runs (53 and 61 calls). `typst-3582` passed with 46 calls in one run.
- The five issues that fail stop before 80 calls, so the higher cap does not spend more on them in vain.
- The most expensive run cost $1.99, half the ceiling. The mean was $0.84 per issue.

The other languages seldom reach 40 calls. C# averaged 14.7 calls in [#448](../measurements/2026-11-13-csharp-prompt-rules.md).

A cap of 80 that lives in one eval config does not reach a Rust repo registered on the server, or one run from another config.

## Decision

1. **Each language profile may set its own default call cap**, in a `[budget]` table with one key, `max_model_calls` ([ADR 0004](0004-multi-language-profiles.md)). The cost ceiling stays a platform setting.
2. **The cap of a job resolves in this order:**
   - the `max_model_calls` in the run's `[budget]`. That is a config file, a server mode, or a repo's own budget in the registry. A repo budget that sets only the cost keeps its mode's call cap;
   - the cap set by the profile of the repo's `language`;
   - 40.

   In a monorepo, the profile is the one of the repo's `language`, not the profile of a `[[repo.dirs]]` entry. A job has one budget.
3. **The Rust profile sets 80.** No other profile sets a cap, so they keep 40.
4. **The $4 cost ceiling of ADR 0005 Decision 4 is unchanged.** The 40-call cap remains the platform default.
5. Configs that only repeated 40 no longer set `max_model_calls`, so the profile default reaches them. `config/typst.toml` drops its own 80.

## Consequences

- A Rust repo gets 80 calls unless its `[budget]` sets a cap. For that repo, the most a job can cost is still $4.
- A repo whose budget in the registry sets `max_model_calls` keeps that value. The web interface never writes the key on its own.
- The admin AI page shows "the language profile's cap" for a mode with no call cap of its own.
- A future profile can set its own cap with a measurement report, without touching ADR 0005 again.
- Per-language KPIs (ADR 0005 Decision 3) compare Rust at 80 calls from now on. Earlier Rust reports at 40 calls (#18, #142, #145) are not directly comparable.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Raise the platform default to 80 for every language | No measurement outside Rust shows a need for it. It would double the room a stuck job has in languages that seldom reach 40. |
| Keep 80 in `config/typst.toml` only | Registered Rust repos and other Rust configs would stay at 40, which #156 showed is too low for some typst issues. |
| Let a language set the cost ceiling too | The highest Rust run cost $1.99, under the $4 ceiling, so the ceiling is not what limits Rust. |
