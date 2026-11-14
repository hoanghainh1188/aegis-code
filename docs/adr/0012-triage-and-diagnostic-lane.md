# ADR 0012 – L1/L2 Triage and Non-Code Diagnostic Lane

- Date: 2026-11-13
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #451 (Milestone: `Phase 3 – Mở rộng`, builds on [ADR 0011](0011-ams-direction.md), prerequisite for #456, #457)
- Amendments: 2026-11-14, triage runs in shadow first (#497): see "Amendment (#497)" below

## Context

In enterprise Application Management Services (AMS), tickets received from Git forges (GitHub, GitLab) and ITSM systems (Jira Service Management, ServiceNow) are heterogeneous. Empirical industry data shows that 50%–70% of reported issues do not stem from code bugs in the maintained repository:
- **Operations & Configuration:** Missing environment variables, expired secrets, network gateway timeouts, or misconfigured ingress.
- **Data Inconsistencies:** Corrupted database rows, violated foreign key constraints, or dirty customer state.
- **Ambiguous Inquiries (`needs_info`):** Vague descriptions lacking reproduction steps, correlation IDs, or stack traces.

Previously, Aegis treated every incoming ticket as an actionable code defect: it immediately allocated an isolated Git worktree, indexed the repository into the `codebase-memory-mcp` SQLite call graph, and launched the Core agent loop (`claude-sonnet-5-5`). When confronted with an infrastructure or data issue, the model either:
1. Burned through its token budget searching for nonexistent code faults until tripped by the $4 / 40-call circuit breaker (`stopped_budget` / `stopped_steps`); or
2. Hallucinated speculative application workarounds, resulting in rejected Pull Requests that burdened human reviewers.

## Decision

1. **Light Tier Triage Before Worktree Allocation:**
   - In `aegis-lifecycle`, a new triage step executes at the **Light tier** (`claude-haiku-5-5` or local light model) *prior to* allocating a Git worktree, indexing call graphs, or dispatching the Core model.
   - The ticket body, logs, and high-level repository metadata are classified into exactly one of four taxonomy buckets:
     * `code_bug`: Reproducible defect in application logic or tests. Proceeds to standard code synthesis.
     * `ops_or_config`: Operational, network, environment, or configuration failure.
     * `data_issue`: Data corruption, dirty state, or database constraint issue.
     * `needs_info`: Incomplete bug report lacking essential diagnostic context.

2. **Non-Code Diagnostic Lane:**
   - For all non-`code_bug` outcomes, Aegis diverts the job away from the editing/gate pipeline into the **Diagnostic Lane**.
   - No Git branches (`agent/*`) are pushed and no Pull Requests are opened.
   - The agent performs a bounded read-only inspection of repository configuration schemas, environment definitions, and documentation.
   - Synthesizes a structured **Diagnostic Report** containing:
     * **Classification & Confidence Score**
     * **Root Cause Analysis (RCA)** from an operational perspective
     * **Remediation Runbook:** Specific shell commands, configuration adjustments, or SQL data patch suggestions for human operators
     * **Clarifying Questions:** Specific details required from the reporter (if `needs_info`).

3. **ITSM / Tracker Feedback Loop:**
   - `aegis-tracker` publishes the Diagnostic Report as a formatted markdown comment directly to the source ticket (Jira, ServiceNow, Backlog).
   - The job terminates cleanly with outcome `Outcome::Diagnosed { category }`.
   - Average token spend drops from ~$0.55–$4.00 to under **$0.03** per non-code incident.

4. **Telemetry & Dashboard Metrics:**
   - Emits Prometheus counter `aegis_triage_total{category="code_bug|ops_or_config|data_issue|needs_info"}`.
   - Surfaces ticket deflection rates on the Askama + htmx dashboard, measuring the percentage of AMS incidents resolved without developer intervention.

## Amendment (#497): shadow triage first

A misread ticket in the diagnostic lane costs more than a wasted job: a real code bug read as `ops_or_config` gets no PR. How often that happens is not known yet, so the triage runs in **shadow** before the lane (§2, §3) is turned on (#451, after the pilot #158):

1. **No extra call.** The Light classification every job already makes (`aegis-lifecycle/src/light.rs`) also returns `triage` (the four categories of §1), `triage_confidence` (0–1) and `triage_reason`. A reply without them, or with them out of range, still classifies the issue; the job has no triage then. Tickets whose source sets their kind (a CI failure, Sonar) skip Light and get none.
2. **Recorded only.** The ledger keeps a `triaged` event (`IssueRecord.triage`), and the server counts `aegis_triage_total{category}`. Every job still works on the code, opens its PR and is reviewed as before; nothing is posted to the tracker.
3. **Not shown per job.** The job page, which reviewers read, leaves the triage out, as it does hidden test results (ADR 0005): a reviewer who saw "ops_or_config, 0.9" might judge the PR by it, and the measurement would measure that.
4. **Measured against the jobs.** `aegis report` and the dashboard show, per category, the jobs, their PRs, accepted and rejected PRs, and jobs with no PR. A ticket read as anything but `code_bug` whose PR was accepted is a miss the lane would have made. #451 turns the lane on, per category and above a confidence, only where the pilot's table shows such misses are rare.

## Consequences

- **Positive:** Eliminates wasteful model calls and noisy PRs for non-code incidents; reduces human triage time for L1/L2 support staff.
- **Positive:** Drastically improves AMS profit margins by automating initial root-cause diagnosis.
- **Negative / Trade-off:** False negatives (a genuine code bug misclassified as `ops_or_config`) would fail to open a PR. To mitigate this, reviewers or operators can override the triage classification by commenting `/aegis fix --force-code` on the ticket.
