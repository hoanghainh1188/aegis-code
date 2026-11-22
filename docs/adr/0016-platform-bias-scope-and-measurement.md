# ADR 0016 – Platform Bias Scope, Instrumentation, and Measurement (WORTH – R)

- Date: 2026-11-15
- Status: Accepted (eval baseline recorded; customer pilot validation opens with `#158`)
- Decider: @Nguyen Han Hoang Hai
- Issue: `#511` (`research: phạm vi và cách đo thiên lệch (bias) của nền tảng (WORTH – R)`, Phase 3 – Mở rộng; dependency: `#158`)
- Related: [ADR 0004](0004-multi-language-profiles.md), [ADR 0005](0005-kpi-thresholds-after-phase-1.md), [ADR 0012](0012-triage-and-diagnostic-lane.md), [ADR 0014](0014-per-language-call-cap.md), [eval baseline note](../measurements/2026-11-15-bias-scope-and-eval-baseline.md)

## Context

Under WORTH – R ("bias"), the platform needs an explicit definition of which automated decisions can disadvantage a customer team, codebase, or workflow, along which operational axes those disparities occur, and how KPI reporting surfaces them continuously.

Aegis does not process human demographic attributes (applicant identity, age, gender, ethnicity). Treating generic ML fairness checklists as applicable would obscure the real failure modes of an autonomous maintenance agent—such as English-centric prompt/routing overfitting, tracker adapters dropping context from ITSM tickets, or call/budget caps cutting off noisier ecosystems prematurely.

## Decision

### 1. In-scope automated decisions

Five platform decisions can systematically under-serve one slice of incoming work relative to another:

1. **Shadow triage classification (`crates/aegis-lifecycle/src/light.rs`, ADR 0012):** Light classifies every incoming ticket into `code_bug`, `needs_info`, `ops_or_config`, `user_error`, or `duplicate`. When a diagnostic lane is enabled for a slice, misclassifying an actual bug as non-code (`triage miss`) silently withholds a fix.
2. **Tier routing & escalation (`aegis-router`):** Deciding when Light hands off to Core and when Core escalates to Expert (e.g., after repeated rejected edits or read-only steps). Premature or missed escalation directly drives acceptance and per-issue cost differences across stacks and ticket styles.
3. **Budget, step-cap, and answer-source stopping:** Terminating runs before a PR is opened via `StoppedBudget`, `StoppedSteps`, or `StoppedExpertFail`, or holding an ungrounded diagnostic answer (`held_for_sources`, `#508`). Stop rates measure where the agent stalls or burns budget without deliverable output.
4. **Fix generation & verification (`aegis-gate`, `aegis-lifecycle`):** Producing patches under language-specific build/test runners, prompt rules, and call caps (ADR 0004, ADR 0014), measured by human reviewer acceptance (`Accepted` + `AcceptedWithEdits` with `<= 10` edited lines and no broken tests) and review time.
5. **Review-round adaptation (`review_and_work`):** Handling reviewer feedback on PRs/MRs across GitHub and GitLab without regressing.

### 2. Evaluation dimensions & non-applicable axes

Every completed run in the ledger (`aegis-ledger`) and PostgreSQL job store (`aegis-store`) is sliced along **four operational dimensions**:

1. **Programming language (`programming_language`):** Stack profile (`rust`, `python`, `go`, `java`, `typescript`, `csharp`, etc.), driven by tooling quality, compiler feedback, and per-language call caps.
2. **Ticket natural language (`ticket_language`):** `en`, `ja`, `vi`, `other` (recorded via deterministic script detection in `detect_ticket_language` before sending prose elsewhere, stripping fenced code blocks and URLs so embedded identifiers or isolated loanword diacritics do not misclassify English tickets as `vi`).
3. **Ticket source (`ticket_source`):** `github`, `gitlab`, `jira`, `jsm`, `servicenow`, `backlog`, `sonar`, `ci`, `cli`, `review`, `cve`, capturing formatting differences between developer bug reports, ITSM escalations, scanner alerts, and CI failure logs.
4. **Repository (`repo`):** Per-repository disparity caused by monolithic complexity, flaky/slow test suites, custom conventions, or insufficient repository memory.

**Explicitly out of scope (non-applicable):**

- **Protected human attributes (gender, age, race, nationality of ticket author):** Aegis never collects, infers, or stores personal demographic attributes. Author identifiers are treated strictly as opaque operational handles for RBAC, webhook verification, and comment attribution; measuring disparities by inferred demographics would violate privacy minimization without actionable engineering signal.
- **Individual developer productivity scoring:** PR acceptance rate, edit lines, and review minutes measure *platform* quality, not reviewer or assignee performance.

### 3. Metrics & investigation thresholds

Every slice computes the exact [`Kpi`] and [`TriageTable`] formulas (`crates/aegis-ledger/src/kpi.rs`, `crates/aegis-ledger/src/triage.rs`, `crates/aegis-ledger/src/bias.rs`), extended with stop breakdown (`stopped = stopped_budget + stopped_steps + stopped_expert_fail` and `stop_rate = stopped / fix_attempts`).

Once a slice reaches `MIN_BIAS_SAMPLE = 5` finished fix attempts (or 5 reviewed non-code triaged tickets for triage miss), `BiasReport` raises an investigation flag when any rule trips:

| Flag | Condition (`N >= 5`) | Purpose |
| --- | --- | --- |
| `low-accept` | `accept_rate < 35%` (`LANGUAGE_ACCEPT_RATE_MIN`), or `>= 25 pp` below highest eligible sibling in the same dimension (`ACCEPT_GAP_ALERT = 0.25`) | Catch language, source, or repo regressions below target or far behind peers |
| `high-stop` | `stop_rate > 30%` (`STOP_RATE_ALERT = 0.30`), or `>= 20 pp` above lowest eligible sibling (`STOP_GAP_ALERT = 0.20`) | Catch timeout, step-cap, or budget exhaustion skew |
| `high-cost` | `mean_cost_usd > $0.50` (`COST_FLOOR_ALERT`) AND `mean_cost_usd >= 2.0x` median of eligible sibling groups (`COST_RATIO_ALERT = 2.0`) | Catch excessive retry/Expert token burn in one slice |
| `triage-miss` | Among `non-code_bug` triaged tickets with PR review (`>= 5`), accepted-PR rate `> 10%` (`TRIAGE_MISS_RATE_ALERT = 0.10`) | Catch non-English or non-GitHub tickets wrongly diverted away from bug fixing |

Slices with `< 5` samples are still displayed in tables for visibility, without firing alert flags.

### 4. Measurement cadence & pilot gate (`#158`)

- **Every eval run & `aegis-local report` invocation:** Automatically emits `LedgerEvent::TicketMeta` on `prepare_and_work` and `review_and_work`; `aegis-local report` and `/` dashboard (`Dashboard`) display breakdown tables and flag alerts whenever metadata or job records exist.
- **Weekly during `#158` pilot and upon profile/prompt changes:** Operators review `By ticket language`, `By ticket source`, `By programming language`, and `By repo` on the `/` dashboard or via `aegis-local report` export. Any slice holding an active flag across two consecutive review cycles opens a targeted profile, prompt, or cap issue (analogous to `#425`, `#426`, `#156`).
- **Closure boundary for `#511`:** This ADR, the ledger/CLI/dashboard instrumentation, and the 52-issue `eval/issues` baseline establish the measurement definition and baseline. Empirical validation across non-English (`ja`, `vi`) and non-GitHub (`jira`, `jsm`, `servicenow`, `backlog`, `sonar`, `ci`) tickets cannot complete until pilot `#158` produces production/staging job records, and `#511` stays open until that first pilot bias report is appended to `docs/measurements/`.

## Implementation summary

- `crates/aegis-ledger/src/record.rs`: `TicketLanguage`, `detect_ticket_language`, `TicketMeta`, `LedgerEvent::TicketMeta`, backwards-compatible folding on `IssueRecord`.
- `crates/aegis-ledger/src/kpi.rs`: `stopped_budget`, `stopped_steps`, `stopped_expert_fail`, `stopped()`, `stop_rate()`, `"Stopped without PR"` row in `Kpi` text report.
- `crates/aegis-ledger/src/bias.rs`: `BiasReport`, `BiasSection`, `BiasRow`, alert thresholds & Markdown formatter.
- `crates/aegis-lifecycle/src/{issue,run}.rs`: `IssueInput.source` and automatic `ticket_meta` emission in `prepare_and_work` and `review_and_work`.
- `crates/aegis-cli/src/kpi.rs`, `crates/aegis-store/src/kpi.rs`, `crates/aegis-server/src/web/{dashboard,view}.rs`: surfaced `Stopped`, `By ticket source`, `By ticket language`, and `Bias investigation flags (#511)` in CLI report and web dashboard.

## Consequences

- **Positive:** Bias checks reuse exact ledger KPIs instead of a bespoke offline script, need zero extra LLM calls, stay opt-in/non-intrusive on legacy ledgers without `TicketMeta`, and preserve reviewer blindness by never leaking triage/metadata onto job review views.
- **Positive:** Disparity thresholds separate genuine systematic skew (`N >= 5`) from single-issue variance on small pilot subsets.
- **Negative / Limitations:** Deterministic script detection (`detect_ticket_language`) treats ASCII transliterated non-English prose as `en`, and the public `eval/issues` set has `0` non-English and `0` non-GitHub issues, leaving two of the four dimensions unexercised until pilot `#158` runs.
