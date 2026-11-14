# Agentic Code Maintenance Platform – Proposal

Oct 17, 2026 · @Nguyen Han Hoang Hai

Objective: Build a comprehensive autonomous software maintenance platform ([ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md)). Began with a 6–8 week MVP/PoC on production repositories to evaluate baseline KPIs (Section 8); benchmark outcomes serve to steer technical direction rather than acting as a project termination gate. The platform is implemented in Rust with a custom agent loop ([ADR 0001](../adr/0001-platform-in-rust.md)).

Detailed implementation specifications reside in [Technical Architecture](01-architecture-detail.md).

---

## 0. Project Justification & Feasibility

**Decision: Proceed. Target a complete platform, executing core engine development and empirical KPI validation on an MVP/PoC prior to constructing service layers.**

**Why this initiative matters:**
- Code maintenance is repetitive, constrained by deterministic tests, and represents the optimal sweet spot for autonomous agents.
- Low exploration cost: API spend is under $200 for 50 benchmark issues (Section 9); primary investment is 1–2 engineers over several weeks.
- Tangible ROI: Proven PoC results immediately support deploying AI assistance into enterprise maintenance engagements (e.g. AMS).

**Why validate the core engine first:**
- Complex context compression research (SWEzze, SaraCoder) is experimental; early adoption is unjustified without baseline empirical telemetry.
- Building in Rust with a custom agent loop (ADR 0001) places all downstream service capabilities on core engine correctness.
- The principal risk is non-technical: whether the agent can resolve authentic enterprise bugs, and whether customers permit transmitting code to LLM inference APIs.

**Buy vs. Build Matrix:**

| Off-the-Shelf | Custom Built (Rust) |
| --- | --- |
| Claude API / AWS Bedrock (AWS SDK for Rust) | Agent loop, prompt cache breakpoints, observation collapsing |
| Codebase-Memory MCP, Tree-sitter, Language Servers | Bounded ACI tool suite (MCP server), Language Profiles |
| Docker, Toolchain Images, SonarQube | Model routing escalation rules + $4 / 40-turn circuit breaker |
| | 6-step verification gate: Docker test sandbox + Sonar + human review |
| | Multi-channel forge connectors (GitHub, GitLab, Jira, Backlog) |
| | Financial token ledger and KPI tracking |

**PoC Contingency Policy:** If the MVP falls short of target thresholds in Section 8, the initiative does not terminate; rather, expansion of service layers pauses until core bottlenecks (fault localization, token burn, cache misses, gate latency) are remediated (ADR 0002).

**Key Decisions (as of Oct 17):**

| Area | Decision | Impact |
| --- | --- | --- |
| Target Repositories | Internal repositories > 100k LoC across diverse ecosystems via Language Profiles ([ADR 0004](../adr/0004-multi-language-profiles.md)). PoC evaluated on Rust and Python | Codebase-Memory MCP indexing (Section 3) |
| Test Suites | Pre-existing CI test suites required | Shortens Phase 0 prerequisites |
| Data Residency | Customer code restricted strictly to Japan | Internal PoC is unconstrained; customer deployments route via Bedrock `jp.` profile (Section 7) |
| SonarQube | Support both SonarQube Server and Cloud | Supported across all six initial language profiles |
| Platform Language | 100% Rust with custom agent loop | Bypasses Python Claude Agent SDK; see [ADR 0001](../adr/0001-platform-in-rust.md) |

---

## 1. Objectives & Scope

The system ingests maintenance issues and delivers verified, human-reviewable Pull Requests. The agent **never merges autonomously**.

**3 Measurable Core KPIs:**
1. **Accuracy:** Accepted PR rate $\ge 40\%$ across evaluation benchmark suites (Section 8).
2. **Cost Efficiency:** Average API expenditure $\le \$4$ per issue (including failed runs).
3. **Safety / Zero Regression:** Zero PRs breaking existing tests ever reach human reviewers.

**Input Ingestion:** Bug tickets (Jira/Backlog), broken CI runs on default branches, SonarQube static analysis alerts.

**Output Deliverables:** Dedicated git branch + structured Pull Request, complete with root-cause analysis, reproduction proofs, and full test outputs.

**Explicit Non-Goals for Initial Phases:**
- Complex feature development or cross-service architectural refactoring;
- Autonomous merging into protected branches;
- Repositories lacking automated test execution pipelines.

---

## 2. High-Level 4-Layer Control Architecture

The platform enforces a 4-layer control architecture:
1. **Layer 1 – Context Engine:** AST retrieval via Tree-sitter and bounded ACI tools.
2. **Layer 2 – Deterministic Prompt Caching:** Strict 4-breakpoint hierarchy applied across all model calls.
3. **Layer 3 – Model Routing & Circuit Breakers:** Light $\rightarrow$ Core $\rightarrow$ Expert escalation capped at $4 / 40 turns.
4. **Layer 4 – 6-Step Verification Gate:** Network-isolated Docker sandbox (`--network none`) executing formatting, linting, repro tests, full test suites, coverage checks, and Sonar gates.

---

## 3. Layer 1 – Context Engine

Avoids ingesting full repository contexts; bounds retrieval to relevant code slices via two primitives:

**a. Bounded Agent-Computer Interface (ACI Tools):**
- AST-based symbol search (`search_symbol`) replacing noisy regex searches.
- Search results capped at 50 entries to force agent refinement.
- File inspection bounded to 100-line windows with line numbers (empirically validated by SWE-agent research).
- The `edit` tool enforces pre-write syntax validation via Tree-sitter linters, immediately rejecting malformed syntax.

**b. Repository Indexing (Codebase-Memory MCP):**
- Stores relational call graphs in SQLite, queried via MCP.
- Status (Oct 25): Codebase-Memory is integrated into the platform (#185). Before gate execution, the platform inspects call graphs to identify external callers of modified functions and provides them to the agent (#203). Language server integration (`find_definition`, `find_references`) supplements AST search (#115).

---

## 4. Layer 2 – Deterministic Prompt Caching

Prefix caching reduces input token expenses by 90–95%, serving as the cornerstone of cost efficiency.

**Prompt Layout Ordering (Static $\rightarrow$ Dynamic):**
1. Tool definitions (BP1);
2. System prompt + language conventions (BP2);
3. Repository module map and reproduction test context (BP3);
4. Issue body and dynamic message history (BP4).

**Reference Pricing Matrix ($ / 1M tokens):**

| Model | Input | Output | 5-min Cache Write | 1-hour Cache Write | Cache Read |
| --- | :---: | :---: | :---: | :---: | :---: | :---: |
| Fable 5.1 | $10.00 | $50.00 | $12.50 | $20.00 | $0.25 (2.5%) |
| Opus 5.5 | $4.00 | $20.00 | $5.00 | $8.00 | $0.20 (5%) |
| Sonnet 5.5 | $2.00 | $10.00 | $2.50 | $4.00 | $0.10 (5%) |
| Haiku 5.5 ($\le 100\text{k}$) | $0.10 | $0.50 | $0.125 | $0.20 | $0.01 (10%) |
| Haiku 5.5 ($> 100\text{k}$) | $0.50 | $2.50 | $0.625 | $1.00 | $0.05 (10%) |

**Cache Economics & Mechanics:**
- 5-minute cache breaks even on the 1st read hit; 1-hour cache breaks even on the 2nd read hit. Every hit refreshes the TTL.
- Minimum cacheable threshold: 512 tokens.
- **Batch Observation Collapsing:** Once context reaches 60% of the model window, older tool observations collapse into `[Observation collapsed]` tokens in batches, preserving reasoning while maintaining high cache hit ratios.
- **Defending Cache Stability:** Avoid embedding timestamps or dynamic IDs in system prompts; enforce strict key ordering during JSON serialization; freeze tool lists across turns.

---

## 5. Layer 3 – Model Routing & Circuit Breakers

Tasks begin at Core tier and escalate to Expert only under explicit triggers:

| Tier | Model | Scope of Work |
| --- | --- | --- |
| **Light** | Haiku 5.5 | Issue triage, log summarization, stack trace pruning, commit message generation |
| **Core** | Sonnet 5.5 | Root-cause localization, reproducing test creation, patch synthesis |
| **Expert** | Opus 5.5 (or Fable 5.1) | Complex multi-file refactoring ($\ge 3$ files), cross-module architectural impacts |

**Escalation Triggers (Core $\rightarrow$ Expert):**
1. Syntax linter rejects edits twice consecutively;
2. Reproducing test remains failing after two patch attempts;
3. Root cause spans $\ge 3$ files;
4. 15 turns elapsed without a viable patch attempt.

**Circuit Breakers (Hand off to Human):**
- Expert tier fails twice;
- Total turns exceed 40, or the cap of the repo's language profile (80 for Rust, [ADR 0014](../adr/0014-per-language-call-cap.md));
- Cumulative token spend exceeds $4 for the issue.

---

## 6. Layer 4 – Regression Verification Gate

Patches are submitted to human reviewers only after passing all verification stages in isolated Docker sandboxes (`--network none`):
1. **Syntax Linter:** Pre-write AST validation.
2. **Dynamic Tests in Docker:**
   - Reproducing test must transition from FAIL to PASS;
   - Full regression test suite must PASS without failures.
3. **SonarQube Quality Gate:** Enforces "Sonar way" (coverage $\ge 80\%$, zero new security hotspots/bugs).
4. **Human Review:** Agent opens a structured PR; engineer performs final review and merges.

**Golden Master Technique for Legacy Code:** When test coverage is absent, the agent synthesizes characterization tests capturing existing behavior prior to patching. Any intended behavior modifications must be explicitly declared under *"Permitted Behavior Changes"* for reviewer sign-off.

---

## 7. Security & Data Sovereignty

- **Japan Data Sovereignty:** For `customer-jp` repositories, all inference routes exclusively via AWS Bedrock Tokyo (`ap-northeast-1`) utilizing regional inference profiles `jp.`.
- **Sandbox Isolation:** Test execution runs in Docker containers under `--network none`, `--cap-drop ALL`, and non-root users.
- **Prompt Injection Defense:** External inputs (issue bodies, ticket comments, README files) are treated as untrusted data.
- **Dependency Sandboxing:** Dependencies are locked to default branch lockfiles; unauthorized packages are blocked.

---

## 8. Target KPIs & Empirical Validation

Evaluated against authentic internal repositories rather than synthetic benchmarks. Benchmark: 30–50 issues closed within the preceding 6 months.

| KPI | Measurement | Baseline Target | Finalized Threshold ([ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md)) | Phase 1 Actuals |
| --- | --- | :---: | :---: | :---: |
| **Acceptance Rate** | PRs accepted ($\le 10$ lines edited) | $\ge 40\%$ | $\ge 40\%$ total, $\ge 35\%$ per language | **50% total** (Python 64%, Rust 39%) |
| **Zero Regression** | Broken tests reaching reviewer | 0 | 0 | **0** |
| **Cost per Issue** | Total model spend including failures | $\le \$4$ avg | $\le \$4$ avg | **$0.55 avg** |
| **Review Time** | Human review duration | $\le 15$ min | $\le 15$ min | **1.8 min** |
| **Prompt Cache Ratio** | Cache read tokens / total input | $\ge 70\%$ | $\ge 70\%$ | **92%** |

---

## 9. Implementation Roadmap

Five phases. Each phase closes on a measurement milestone, not a feature list; a missed milestone sends work back to the core engine before the next phase, without stopping the project ([ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md)). Phase 3 and Phase 4 carry the AMS direction of [ADR 0011](../adr/0011-ams-direction.md), [ADR 0012](../adr/0012-triage-and-diagnostic-lane.md), and [ADR 0013](../adr/0013-ephemeral-database-sidecars.md).

| Phase | Duration | Scope | Measurement Milestone | Status |
| --- | --- | --- | --- | --- |
| **0. Setup** | 1–2 weeks | Repo selection, Docker sandbox hardening, 30–50 issue benchmark curation | Written customer consent; tests run automatically in the sandbox | Completed (Oct 24, 2026); target repos are public forks of typst and scrapy, consent for an internal repository pending (#157) |
| **1. MVP / PoC** | 4–6 weeks | Rust engine: provider, loop, ACI, gate, ledger; Rust and Python profiles; initial KPI evaluation | Thresholds of §8, fixed in [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md) | Completed (Oct 24, 2026) |
| **2. Platform** | 2–3 months | TypeScript, Java, Go, C# profiles; platform service (`aegis-server`), PostgreSQL transactional queue, webhooks, dashboard, RBAC, OpenTofu IaC, on-premise & Helm bundles, developer CLI, prompt injection defense | A team uses it daily (#158); monthly cost within budget; KPIs hold at MVP level, also on an internal repository (#157) | ~95% Completed (Remaining: C# KPI uplift #448, `check-repo` #450, staging #33, internal KPI #157, pilot #158) |
| **3. Expansion & AMS Readiness** | After Phase 2 | L1/L2 triage & diagnostic lane (#451), APM log ingestion (#452), ITSM connectors (#453), ephemeral DB sidecars & Golden Master eval (#454), controlled CVE remediation (#455), additional languages (#37), review evidence (#36). *Note: SBFL (#34) and SWEzze (#35) were evaluated and deferred as `NO-GO` by default.* | Triage labels checked against human labels on pilot tickets, counting code defects routed away from the fix loop; ADR 0005 KPIs hold on tickets from APM and ITSM intake; sidecar & Golden Master report (regressions caught, gate time and cost); one sample CVE fixed without loosening the bug-fix lane | Planned (`Phase 3 – Mở rộng`); AMS items start after the pilot (#158) |
| **4. Enterprise AMS Scale** | After Phase 3 | Incident & Domain Memory (#456), multi-tenant isolation, SLA priority queue (`P1/P2/P3`) & MTTR reporting (#457) | MTTR and SLA on-time rate per tenant; share of tickets resolved at triage versus by PR; cost per contract against budget; acceptance rate and model calls with and without incident memory. Thresholds fixed by an ADR after the first measurement | Planned (`Phase 4 – AMS Quy mô Doanh nghiệp`); entry requires the pilot report (#158), internal-repository KPIs (#157), and #451, #453 closed |
| **5. Autonomous Self-Healing & Prevention** | Future | Proactive maintenance: Anomaly-to-patch pipeline from APM traces, scheduled technical debt auto-pruning, continuous compliance audit | Proactive PR acceptance rate and preventive debt reduction telemetry | Conceptual Vision |

Even after Phase 4, aegis covers L2/L3 code fixes and read-only diagnosis. L1 user support, data and configuration fixes, production incident response and change management stay outside it ([ADR 0011](../adr/0011-ams-direction.md)).

Detailed progress is tracked on the [GitHub milestones](https://github.com/hoanghainh1188/aegis-code/milestones).

### Detailed Phase Breakdown for Next-Gen AMS

#### Phase 3: Expansion & AMS Readiness (`Phase 3 – Mở rộng`)
Transition Aegis from a developer-facing CI tool to an operational platform capable of managing legacy enterprise codebases under active AMS contracts:
- **L1/L2 Triage & Non-Code Diagnostic Lane (#451, [ADR 0012](../adr/0012-triage-and-diagnostic-lane.md)):** Adds a lightweight classification step prior to worktree creation. Distinguishes `code_bug` from `ops_or_config`, `data_issue`, or `needs_info`. For non-code incidents, synthesizes an actionable Diagnostic Report directly to the ticket without opening unwanted PRs.
- **APM & Production Telemetry Ingestion (#452):** Ingests real-world stack traces, error frequencies, release commit SHAs, and breadcrumbs from APM platforms (Sentry, AWS CloudWatch, Datadog) using correlation IDs, overcoming sparse user-reported ticket descriptions.
- **ITSM Connectors & SLA Integration (#453):** Bi-directional synchronization with ServiceNow and Jira Service Management (JSM), mapping enterprise severity levels (`P1`, `P2`, `P3`) and SLA response/resolution deadlines into job metadata.
- **Ephemeral Database Sidecars & Golden Master Evaluation (#454, [ADR 0013](../adr/0013-ephemeral-database-sidecars.md)):** Enables isolated Docker service sidecars (PostgreSQL, Redis, MySQL) within a closed internal network during `aegis-gate` runs. Evaluates characterization test generation (Golden Master) on low-coverage legacy repositories to guarantee zero regression on legacy systems without unit tests.
- **Controlled CVE & Dependency Remediation (#455):** Dedicated `cve-remediation` mode triggered by vulnerability alerts (e.g., OSV/Dependabot). Allows bounded manifest and lockfile upgrades strictly for target packages while preserving hard locks against supply-chain modification in normal bug-fix jobs.
- **Secondary Language Ecosystems (#37) & Spec/RFC Evidence (#36):** Extends multi-language profile support to secondary ecosystems (Kotlin, PHP, etc.) and produces RFC/spec-style architectural change evidence for larger patches.

#### Phase 4: Enterprise AMS Scale & Governance (`Phase 4 – AMS Quy mô Doanh nghiệp`)
Scale Aegis across multiple client contracts with enterprise-grade isolation, institutional memory, and operational KPI reporting:
- **Incident & Domain Memory (#456):** PostgreSQL-backed institutional memory storing postmortems, lessons learned from human review rounds (`/aegis fix`), runbooks, and client-specific domain rules. Uses lightweight extraction to index merged solutions and injects relevant historical insights into `BP3` prompt context.
- **Multi-Tenant Isolation & Contract Quotas (#457):** Complete boundary isolation across tenants: per-client OIDC roles, dedicated monthly token/cost budgets, isolated secret scopes, and independent worktree paths.
- **SLA-Prioritized Queue Scheduling (#457):** Upgrades PostgreSQL `FOR UPDATE SKIP LOCKED` transactional leasing to prioritize high-severity incidents (`P1 > P2 > P3`) with worker preemption and starvation prevention across tenants.
- **AMS Operational Metrics & MTTR Dashboard (#457):** Dedicated executive and operator views measuring Mean Time to Resolve (MTTR), SLA adherence rates, ticket deflection percentages (incidents resolved at triage vs. PR merge), and per-contract margin/cost analytics.

#### Phase 5: Autonomous Self-Healing & Preventive Maintenance
Long-term evolution toward proactive software health:
- **Anomaly-to-Patch Pipeline:** Continuously analyzes APM latency regressions and error spikes to automatically diagnose degradation and open proactive performance patches.
- **Technical Debt & Deprecation Pruning:** Scheduled off-peak refactoring jobs addressing compiler deprecations, dead code elimination, and SonarQube technical debt clusters.
- **Continuous Compliance & Audit Governance:** Automated policy enforcement ensuring all maintained services adhere to security frameworks (SOC2, ISO 27001) and dependency lifecycle policies.
