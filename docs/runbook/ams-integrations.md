# Operations Runbook: AMS Integrations & Specialized Lanes

This runbook explains how to enable, configure, and verify the Application Maintenance Services (AMS) features introduced in `v0.13.0`–`v0.17.1`:

1. **ITSM Intake & SLA Sync** (Jira Service Management & ServiceNow)
2. **Read-Only APM Telemetry Enrichment** (Sentry & Datadog)
3. **Incident & Domain Memory** (Automatic extraction & curated runbooks/rules)
4. **Controlled CVE Remediation Lane**
5. **L1/L2 Triage & Non-Code Diagnosis Lane**

All configuration keys referenced below are formally defined in [Configuration Reference](../configuration.md). For tenant/contract isolation and the `/ams` MTTR/SLA dashboard, see [Tenants Runbook](tenants.md).

---

## 1. ITSM Intake & SLA Sync (JSM & ServiceNow)

Aegis polls JSM or ServiceNow tickets, maps severity (`P1`, `P2`, `P3`) and `sla_deadline` onto each queued job, prioritizes the worker queue by SLA with aging (`[worker] sla_aging_s`, default `3600`), and writes status, PR links, or diagnosis reports back to the ticket.

### Prerequisites
- `[trackers] poll_s > 0` in `server.toml` (default `300`).
- Secret stored in `[secrets]` (prefix with `<TENANT>_` for non-default tenants, e.g., `ACME_JSM_TOKEN` or `ACME_SERVICENOW_TOKEN`).

### Configuration

**Jira Service Management (Cloud or Data Center):**
```toml
[settings.tracker]
kind = "jsm"
url = "https://acme.atlassian.net"
deployment = "cloud"                  # or "data-center" (omit `user` for PAT bearer auth)
user = "aegis-bot@acme.example"
token_env = "ACME_JSM_TOKEN"
jql = 'project = SD AND labels = aegis AND status = "Waiting for support"'
service_desk_id = "12"                # optional
sla_field = "customfield_10050"       # optional; falls back to `duedate`
pr_transition = "In Review"           # optional transition when PR opens
diagnosis_transition = "Waiting for customer" # optional when diagnosed without PR
internal_comments = true              # default true ("public": false)
```

**ServiceNow:**
```toml
[settings.tracker]
kind = "servicenow"
url = "https://acme.service-now.com"
user = "aegis_svc"                    # optional; omit for OAuth Bearer token
token_env = "ACME_SERVICENOW_TOKEN"
table = "incident"                    # default "incident"
query = "active=true^assignment_group.name=AppMaint^state=1"
pr_state = "2"                        # optional state when PR opens
diagnosis_state = "3"                 # optional state when diagnosed without PR
work_notes = true                     # default true (internal work_notes vs customer comments)
```

### Verification
1. Register/update the repo and wait one `[trackers] poll_s` cycle.
2. Confirm the job appears on `/jobs` with `priority` (`P1`/`P2`/`P3`) and `sla_deadline` populated.
3. Confirm the ITSM ticket receives an internal note (`internal_comments = true` / `work_notes = true`) with the PR link or diagnosis report once the job finishes.

---

## 2. Read-Only APM Telemetry Enrichment (Sentry & Datadog)

When an AMS ticket lacks a full stack trace, Aegis can query Sentry or Datadog (read-only) by error code, correlation ID, or time window and inject a size-capped, untrusted telemetry block (≤ 6,000 chars, ≤ 10 breadcrumbs) into `BP3` of the prompt.

### Configuration

**Sentry:**
```toml
[settings.apm]
kind = "sentry"
url = "https://sentry.io"
organization = "acme-org"
project = "orders-api"                # optional
token_env = "ACME_SENTRY_TOKEN"
```

**Datadog:**
```toml
[settings.apm]
kind = "datadog"
url = "https://api.datadoghq.com"
token_env = "ACME_DD_API_KEY"
app_key_env = "ACME_DD_APP_KEY"
service = "orders-api"                # optional
env = "production"                    # optional
```

### Safety & Verification
- Telemetry text is treated as untrusted data ([ADR 0009](../adr/0009-prompt-injection-threat-model.md)): secrets are redacted and platform tags are defanged before reaching the model.
- Check the job detail page (`/jobs/<id>`) or server logs (`tracing` span `job`) to confirm telemetry enrichment succeeded without errors.

---

## 3. Incident & Domain Memory

Aegis maintains per-repository institutional memory in PostgreSQL (`repo_memories`) across four kinds:
- `merged_pr` — automatically extracted at the `Light` tier when an agent PR is merged via webhook.
- `review_lesson` — automatically extracted from `/aegis fix` review round feedback.
- `runbook` — operator-curated operational runbooks for recurring incident classes.
- `business_rule` — operator-curated domain invariants the agent must respect.

Recalled memories are sanitized (`redact` + `defang`, size-capped) and injected into `BP3` after the issue text so `BP2` prompt caching stays intact.

### Managing Curated Memories via API
Use a `maintainer` token against `/api/v1/repos/{id}/memories` (see [API Reference](../api.md#incident--domain-memory)):

```bash
# Add or update a business rule (idempotent on repo_id + kind + source_ref)
curl -sS -X POST "$AEGIS/api/v1/repos/1/memories" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "kind": "business_rule",
    "source_ref": "billing/rounding-rule",
    "title": "JPY amounts must never use floating-point rounding",
    "summary": "All JPY invoice totals use integer yen with floor rounding per accounting spec v2.",
    "paths": ["src/billing/invoice.rs"],
    "keywords": "jpy yen rounding invoice total"
  }'

# List memories (optional ?kind=business_rule)
curl -sS "$AEGIS/api/v1/repos/1/memories?kind=business_rule" \
  -H "Authorization: Bearer $TOKEN"

# Remove an outdated or incorrect memory
curl -sS -X DELETE "$AEGIS/api/v1/repos/1/memories/42" \
  -H "Authorization: Bearer $TOKEN"
```

All memory writes and deletes are recorded in `audit_log` (`memory.upsert`, `memory.delete`).

---

## 4. Controlled CVE Remediation Lane

Bug-fix jobs always run with `DependencyPolicy::Locked` and cannot alter lockfile versions ([ADR 0015](../adr/0015-cve-remediation-lane.md)). To upgrade a vulnerable package:

1. Verify the repo's language profile supports `dependencies.upgrade` (or define `[settings.overrides.dependencies] upgrade = [...]`).
2. Trigger a CVE job as a `maintainer` via the Dashboard (**Jobs → CVE upgrade**) or API:
   ```bash
   curl -sS -X POST "$AEGIS/api/v1/repos/1/cve-jobs" \
     -H "Authorization: Bearer $TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"package": "axios", "version": "1.7.9", "advisory": "GHSA-xxxx-yyyy-zzzz"}'
   ```
3. **What happens:**
   - The **platform** runs the upgrade command inside the pre-gate fetch container behind the allow-listed egress proxy.
   - The gate verifies that every changed manifest/lockfile matches the upgrade output exactly, that the target package is locked at the target version, and that no remaining advisories/disallowed licenses exist.
   - If the full test suite passes immediately, Aegis opens the PR with **zero LLM calls**.
   - If the upgrade causes a small breaking change, the agent is invoked to adapt application code only (still forbidden from touching lockfiles).

---

## 5. L1/L2 Triage & Non-Code Diagnosis Lane

By default, every ticket runs **shadow triage** (the `Light` model classifies tickets into `code_bug`, `ops_or_config`, `data_issue`, or `needs_info` for measurement, while still sending every ticket to the fix loop).

### Safe Rollout Procedure ([ADR 0012](../adr/0012-triage-and-diagnostic-lane.md))
1. **Measure first (Shadow Mode):** Leave `[settings.triage_lane]` unset during initial weeks and inspect triage classifications against actual human outcomes.
2. **Enable selectively per category:** Turn on a non-code category only once shadow triage shows that real `code_bug` tickets are rarely misclassified into that category:
   ```toml
   [settings.triage_lane]
   categories = ["ops_or_config", "needs_info"]   # never "code_bug"
   min_confidence = 0.85                          # default 0.8 (0.0..=1.0)
   ```
3. **Guarantees when enabled:**
   - Tickets routed to the diagnosis lane receive a read-only diagnostic report (findings, operator steps, reporter questions) written back to the forge/ITSM ticket—**no worktree, no patch, and no PR**.
   - Every finding and step must cite a verified line in the repository or ticket (checked-source grounding); reports with zero verifiable citations are held and handed to a human.
   - If a real code defect is ever misrouted, a maintainer can submit it directly to the fix loop via the dashboard or `aegis submit`.
