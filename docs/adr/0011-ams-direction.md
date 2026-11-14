# ADR 0011 – Application Maintenance Services (AMS): Phase 3 Readiness and Phase 4 Enterprise Scale

- Date: 2026-11-12
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #461 (frames #451–#457)
- Amends: [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), Decision 1 (roadmap spanned Phase 0 through Phase 3)
- Related: [ADR 0005](0005-kpi-thresholds-after-phase-1.md), [ADR 0009](0009-prompt-injection-threat-model.md), Proposal §0, §1, §9
- Amendments: 2026-11-14, the CVE lane (#455) starts before the pilot (§4)

## Context

The Proposal names AMS (Application Maintenance Services) once, in §0, as an example of where PoC results would be put to use. [ADR 0002](0002-full-platform-mvp-as-checkpoint.md) set the roadmap at Phase 0 through Phase 3 and named Phase 3 "Advanced Scaling". Since then, issues #451–#457 have been filed for AMS work, and a GitHub milestone `Phase 4 – AMS Quy mô Doanh nghiệp` now holds two of them. Neither the milestone nor the AMS issues had a basis in the design documents.

An AMS contract covers more than code fixes. Set against that work, aegis today covers one part: an **L2/L3 code fix that ends in a pull request** ([Proposal §1](../design/00-proposal.md#1-objectives--scope)). It takes a Jira/Backlog ticket, a broken CI run or a Sonar alert, reproduces the defect in a sandbox, and opens a PR that passes the gate. A person reviews it. aegis never merges.

| AMS activity | aegis today |
| --- | --- |
| L1: intake, user questions, known-error workarounds | Not covered. Every ticket goes straight into the code-fix loop. |
| L2: diagnosis of tickets that are not code defects (data, configuration, infrastructure) | Not covered. Such a ticket either fails the loop or produces a needless PR. |
| L2/L3: code defect fixed by a patch | **Covered**: reproduction test, patch, six-step gate, PR. |
| Data fixes and configuration changes | Not covered. aegis changes only source in a repository. |
| Production incidents: on-call, restoring service, rollback | Not covered. aegis does not touch running systems. |
| Change management: CAB approval, release windows, deployment | Not covered. The PR is the only artifact aegis hands over. |
| Periodic maintenance: CVE and dependency upgrades | Not covered. Manifests and lockfiles are blocked in every job ([ADR 0009](0009-prompt-injection-threat-model.md)). |
| SLA commitments and reporting across several contracts | Not covered. Jobs belong to a repository, not to a tenant or contract; the queue is first in, first out. |

Some of this cannot be fixed by tuning the engine:

- a ticket reaches aegis with at most three attached log files (`Ticket::body_with_logs`), and AMS tickets often lack a full stack trace;
- the gate runs tests in one container under `--network none`, so legacy suites that need a database cannot run;
- tickets usually live in an ITSM tool (ServiceNow, Jira Service Management) with a priority and an SLA deadline, which aegis does not read.

## Decision

1. **aegis stays a code-maintenance engine inside an AMS process.** It is not an ITSM tool, an on-call tool or a deployment tool. These rules hold in every phase below:
   - aegis never merges, never deploys, and never changes production data or configuration;
   - for a ticket that is not a code defect, aegis writes a read-only diagnosis back to the ticket. It does not apply a fix;
   - after a production incident, aegis can take on the code fix that follows. Restoring service stays with the operations team;
   - the PR goes into the customer's change process as its input. aegis does not replace that process.

2. **Phase 3 – `Mở rộng` (AMS readiness).** These issues bring the engine to AMS tickets without widening what it may change:

   | Issue | Area | What it adds | Done when |
   | --- | --- | --- | --- |
   | #451 | orchestrator | Triage at the `Light` tier before a worktree is set up: `code_bug`, `ops_or_config`, `data_issue`, `needs_info`. For tickets that are not `code_bug`, a read-only diagnostic report or runbook hint is written back to the ticket, and no PR is opened. | Ops/config tickets open no needless PR and get a useful diagnosis on the tracker. |
   | #452 | connectors | Incident telemetry pulled from APM (Sentry, Datadog, CloudWatch) by error code, correlation ID or time window: stack trace, release commit, frequency, breadcrumbs. It is sanitized as untrusted text and size-capped before it reaches the prompt. | A job from a ticket without logs still gets a real stack trace to write its reproduction test from. |
   | #453 | connectors | Intake from Jira Service Management and/or ServiceNow. Severity (`P1`–`P3`) and the SLA deadline are stored on the job, and status, PR link or diagnosis are synced back. | A job is created from a sample JSM/ServiceNow ticket with the right priority, and the result reaches the ITSM ticket. |
   | #454 | gate | Temporary service sidecars (PostgreSQL, Redis, …) declared in `RepoConfig`, on an internal network with no internet egress, for the life of the gate run. A benchmark on repositories with few tests puts the Golden Master procedure of [Design §6.2](../design/01-architecture-detail.md#62-golden-master-characterization-testing) to an empirical test. | One repository with database-backed integration tests and one with few tests go through the gate, with a measurement report. |
   | #455 | orchestrator | A separate `cve-remediation` job mode, started by an `osv-scanner` alert or a controlled label. It may raise only the configured package to the configured version, and adapt code to small breaking changes. | One sample CVE is fixed (lockfile raised, six-step gate passed) while bug-fix and review-round jobs still cannot touch manifests or lockfiles. |

   The Phase 3 items already planned remain: additional languages (#37) and review evidence (#36).

3. **Phase 4 – `AMS Quy mô Doanh nghiệp` (enterprise AMS).** These issues turn the engine into a service that runs several AMS contracts at once:

   | Issue | Area | What it adds | Done when |
   | --- | --- | --- | --- |
   | #456 | store | Incident and domain memory per repository in PostgreSQL: summaries of merged PRs, lessons from review rounds (`/aegis fix`), runbooks and business rules. It is extracted at the `Light` tier after merge and recalled into `BP3` of the prompt, so the cached prefix stays stable. | The agent recalls at least three relevant past lessons into a fix, and a report compares acceptance rate and model calls with and without memory. |
   | #457 | server | A `tenant / contract` entity: OIDC permissions, monthly budget, secrets and worktrees isolated per tenant. The `FOR UPDATE SKIP LOCKED` queue orders by SLA (`P1 > P2 > P3`) without starving any tenant. AMS report: MTTR, SLA on-time rate, share of tickets resolved at triage versus by PR, cost per contract. | Two independent tenants run on one instance, a `P1` job overtakes a `P3`, and the dashboard shows the SLA/MTTR report. |

4. **Entry conditions.** Phase 3 AMS work starts only after the pilot with a real team (#158), since #451–#455 all depend on it. Phase 4 starts only when both of these exist:
   - **the pilot report (#158)**, compared against the Phase 2 milestone of Proposal §9: the team uses aegis daily, monthly cost is within budget, and KPIs hold at MVP level;
   - **KPIs measured on an internal repository (#157)**, compared with the thresholds of [ADR 0005](0005-kpi-thresholds-after-phase-1.md). Phase 1 numbers come from public repositories (typst, scrapy) whose fixes a model may have seen.

   **Amendment 2026-11-14 (#455):** the CVE lane (#455) starts before the pilot. It does not touch the bug-fix path, which it tightens instead (#500, [ADR 0015](0015-cve-remediation-lane.md)): the pilot measures the same engine. #451–#454 keep the condition above, apart from what serves the pilot directly (#490, #497).

   In addition, the Phase 3 issues Phase 4 depends on must be closed: #451 for #456, and #451 and #453 for #457. If the pilot or the internal KPIs fall short, ADR 0002 applies: find the cause in the ledger and harden the core engine before adding AMS layers.

5. **Measurement milestones.** Each phase closes on a measurement in `docs/measurements/`, not on a feature list:
   - **Phase 3:**
     - triage labels are checked against human labels on pilot tickets. The report counts code defects routed away from the fix loop, because such a ticket gets no PR;
     - the ADR 0005 KPIs (acceptance, zero regression, cost per issue, review time, cache ratio) hold on tickets that came through APM and ITSM intake;
     - the #454 sidecar and Golden Master report: regressions caught, and gate time and cost.
   - **Phase 4:**
     - MTTR and SLA on-time rate per tenant;
     - the share of tickets resolved at triage versus by PR;
     - cost per contract against its budget;
     - acceptance rate and model calls with and without incident memory.

   The first measurement sets the baseline. Thresholds are then fixed by an ADR, as ADR 0005 did after Phase 1, not guessed in advance.

6. **AMS work does not loosen existing controls.**
   - APM telemetry, ITSM fields and recalled memory are untrusted data under [ADR 0009](0009-prompt-injection-threat-model.md);
   - text written back to a ticket goes through the same `inert()` handling as PR text;
   - sidecars get no egress;
   - the CVE mode is a separate job mode with an allow-list. It is not an exception inside the bug-fix mode;
   - tenants never share secrets, worktrees or budgets.

## Consequences

- The roadmap now has four phases with these milestone names: `Phase 0 – Chuẩn bị`, `Phase 1 – MVP / PoC`, `Phase 2 – Platform`, `Phase 3 – Mở rộng`, `Phase 4 – AMS Quy mô Doanh nghiệp`. Proposal §9, CLAUDE.md and README use these names. ADR 0002's "Phase 3 – Advanced Scaling" is now `Phase 3 – Mở rộng`.
- Even after Phase 4, aegis covers L2/L3 code fixes plus diagnosis. L1 conversations with end users, data fixes, configuration changes, production incident response and change management stay outside it. Taking on any of these would cross the rules in Decision 1 and needs a new ADR.
- Triage brings a new failure mode: a code defect labelled `ops_or_config` gets a diagnosis and no PR. The Phase 3 measurement counts these. Until then, a person can relabel the ticket to send it to the fix loop.
- AMS layers depend on pilot evidence that does not exist yet, so Phase 3 and Phase 4 issues stay unstarted until #158, and for Phase 4 also #157, report.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Make aegis a full AMS platform (ITSM, on-call, deployment, data fixes) | Breaks the never-merge, never-deploy rule, and duplicates ITSM tools that customers already run |
| Keep AMS out of the roadmap | AMS engagements are where the platform is meant to be used (Proposal §0). Without triage, ITSM intake and sidecars, a large share of AMS tickets cannot enter the loop |
| Put multi-tenancy and SLA queueing (#457) into Phase 3 | It needs the SLA data from #453 and the triage lane from #451, and is worth building only once the pilot shows the engine holds up on real tickets |
| Start Phase 4 on the Phase 1 KPIs alone | Those numbers come from public repositories a model may have seen; ADR 0005 already asks for re-validation on internal repositories (#157) |
