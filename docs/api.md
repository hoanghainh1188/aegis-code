# REST API & Webhook Reference (`aegis-code`)

This document specifies the HTTP REST API (`/api/v1/*`) and webhook ingress (`/webhooks/*`) exposed by `aegis-server` (`--role api` or `--role all`).

---

## 1. Conventions

### Envelope
Every `/api/v1/*` response uses the same JSON envelope:

```json
{
  "data": { ... },
  "error": null,
  "meta": null
}
```

Paginated list endpoints populate `meta`:

```json
{
  "data": [ ... ],
  "error": null,
  "meta": {
    "limit": 50,
    "next_cursor": 1042
  }
}
```

Pass `?cursor=<next_cursor>&limit=<n>` to fetch the next page (`next_cursor` is `null` on the last page; default `limit` is `50`).

### Authentication & Authorization
- Every `/api/v1/*` route except `GET /api/v1/health` requires `Authorization: Bearer <token>`.
- Both **service tokens** (created via `aegis-server token` or admin UI) and **personal tokens** (created at `/me/tokens`) are accepted.
- **Repository scoping:** A caller sees only repositories it holds a role in (`viewer`, `reviewer`, `maintainer`, or `admin`). Requests targeting a repository or job outside the caller's visibility return `404 not_found` rather than `403` to prevent enumeration.
- **Limits:** API request bodies are capped at **64 KiB**; request timeout is **30 s**. Rate limits are governed by `[auth] api_requests_per_minute` (default `120`) and `api_failures_per_minute` (default `20`) in [Configuration Reference](configuration.md#auth--authoidc).
- **Strict JSON:** All request bodies use `deny_unknown_fields`; unrecognized fields return `422` / `400`.

### Error Codes

| HTTP Status | `error.code` | Meaning |
| :---: | :--- | :--- |
| `400` | `bad_request` | Malformed JSON, path, or query parameters. |
| `401` | `unauthorized` | Missing, invalid, or revoked Bearer token. |
| `403` | `forbidden` | Caller can view the resource but lacks the required role (`maintainer` or `admin`). |
| `404` | `not_found` | Route, repo, job, or memory does not exist (or is outside caller's scope). |
| `409` | `conflict` | State conflict (e.g., duplicate live job for the same issue, pausing twice at the same level). |
| `409` | `budget_spent` | Repository or tenant monthly budget has less than one job cap remaining. |
| `422` | `invalid` | Validation failure on a field (`source`, `issue_ref`, `kind`, `monthly_usd`, etc.). |
| `429` | `rate_limited` | Rate limit exceeded. |
| `500` | `internal` | Unexpected server or database error (details stay in server logs). |

---

## 2. REST Endpoints (`/api/v1`)

### Health & Identity

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `GET /api/v1/health` | Public | Liveness probe. Returns `200` with `{"status": "ok"}`. |
| `GET /api/v1/me` | Any token | Returns caller identity: `{"name": "...", "admin": true|false}`. |
| `GET /api/v1/repos` | Any token | Lists repositories visible to the caller, including `id`, `slug`, `url`, `default_branch`, `language`, and caller's `role`. |

### Jobs & Runs

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `POST /api/v1/jobs` | `maintainer` | Enqueue a job (`201 Created`). |
| `GET /api/v1/jobs` | `viewer` | List jobs. Query: `repo_id`, `status` (`queued`\|`running`\|`done`\|`failed`\|`cancelled`), `limit`, `cursor`. |
| `GET /api/v1/jobs/{id}` | `viewer` | Fetch a single job (`JobView`). |
| `GET /api/v1/jobs/{id}/runs` | `viewer` | Paginated execution attempts (`RunView`) for a job. Query: `limit`, `cursor`. |
| `POST /api/v1/jobs/{id}/cancel` | `maintainer` | Cancel a queued job (`200 OK`) or request cancellation of a running job (`202 Accepted`, stops at next lease renewal). |
| `POST /api/v1/jobs/{id}/retry` | `maintainer` | Create a new retry job from a finished/failed/cancelled job (`201 Created`). |
| `POST /api/v1/jobs/{id}/review` | `maintainer` | Queue the next review round on the PR opened by job `{id}` (`201 Created`, capped at `[reviews] max_rounds`). |

**`POST /api/v1/jobs` Request Body:**
```json
{
  "repo_id": 1,
  "issue_ref": "42",
  "source": "repo",
  "ledger_key": "optional-custom-key"
}
```
- `source`: `"repo"` (resolves issue number against the repo's forge), `"cli"`, `"github"`, `"gitlab"`, `"jira"`, `"backlog"`, `"jsm"`, `"servicenow"`, `"sonar"`, or `"ci"`.
- `"review"` and `"cve"` are rejected here; use `POST /api/v1/jobs/{id}/review` and `POST /api/v1/repos/{id}/cve-jobs` respectively.

**`JobView` Fields:**
`id`, `repo_id`, `issue_ref`, `ledger_key`, `source`, `priority` (`"P1"`\|`"P2"`\|`"P3"`\|`null`), `sla_deadline`, `status`, `outcome`, `reason`, `pr_url`, `attempts`, `retries`, `cancel_requested`, `retry_of`, `review_of`, `review_round`, `locked_by`, `locked_until`, `run_after`, `created_at`, `updated_at`.

### CVE Remediation Jobs

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `POST /api/v1/repos/{id}/cve-jobs` | `maintainer` | Queue a controlled CVE dependency upgrade job ([ADR 0015](adr/0015-cve-remediation-lane.md)). Returns `201 Created` with `JobView`. |

```bash
curl -sS -X POST "$AEGIS/api/v1/repos/1/cve-jobs" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"package": "serde", "version": "1.0.215", "advisory": "GHSA-xxxx-yyyy-zzzz"}'
```

### Repository Budgets

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `GET /api/v1/repos/{id}/budget` | `viewer` | Returns current month spend, cap (`monthly_usd`), `left_usd`, `state` (`unlimited`\|`ok`\|`warn`\|`spent`), `time_zone`, `warn_ratio`, `job_reserve_usd`. |
| `PUT /api/v1/repos/{id}/budget` | `maintainer` | Sets or clears monthly cap: `{"monthly_usd": 100.0}` or `{"monthly_usd": null}` (unlimited). |

### Incident & Domain Memory

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `GET /api/v1/repos/{id}/memories` | `viewer` | Lists stored memories newest-first (each `MemoryView` includes `scope`: `"repo"` \| `"tenant"`). Query: `kind` (`merged_pr`\|`review_lesson`\|`runbook`\|`business_rule`), `limit`, `cursor`. |
| `POST /api/v1/repos/{id}/memories` | `maintainer` / `admin` | Idempotently upserts a memory on `(repo_id, kind, source_ref)`. Returns `201 Created` with `MemoryView`. |
| `DELETE /api/v1/repos/{id}/memories/{memory_id}` | `maintainer` / `admin` | Deletes a memory entry (`200 OK` with `{"deleted": true, "id": ...}`). |

**`POST /api/v1/repos/{id}/memories` Request Body:**
```json
{
  "kind": "business_rule",
  "source_ref": "orders/idempotency-invariant",
  "title": "Order cancellation must preserve refund ledger rows",
  "summary": "Never delete refund_entries when cancelling an order; mark status = voided instead.",
  "paths": ["src/orders/cancel.rs", "src/billing/refund.rs"],
  "keywords": "cancel order refund voided ledger",
  "scope": "repo",
  "job_id": null
}
```
- `scope`: `"repo"` (default for new entries; visible only to this repo) or `"tenant"` (shared across all repos of the repo's tenant). Omitting `scope` on an existing memory preserves its current scope. Invalid values return `422 invalid`.
- **Authorization:** Only a **tenant admin** or **platform admin** can share a memory (`"scope": "tenant"`), update an already shared memory, or delete a shared memory; other callers (including repo `maintainer`) receive `403 forbidden`.

### Global Instance Pause (Admin Only)

| Method & Path | Role | Description |
| :--- | :--- | :--- |
| `GET /api/v1/admin/pause` | `admin` | Shows current pause state (`active`, `level`, `reason`, `paused_by`, `paused_at`). |
| `POST /api/v1/admin/pause` | `admin` | Pauses all agents: `{"level": "intake"|"emergency", "reason": "..."}`. See [Incident Runbook](runbook/incident.md). |
| `POST /api/v1/admin/resume` | `admin` | Resumes all agents; queued jobs are claimed again immediately. |

---

## 3. Webhook Ingress (`/webhooks`)

Webhooks have their own delivery size limit (larger than the 64 KiB API limit) and deduplicate deliveries in PostgreSQL (`webhook_deliveries`).

### `POST /webhooks/github`
- **Verification:** `X-Hub-Signature-256: sha256=<hex(HMAC-SHA256(secret, raw_body))>` checked in constant time against `AEGIS_GITHUB_WEBHOOK_SECRET` (token path) or `AEGIS_GITHUB_APP_WEBHOOK_SECRET` (GitHub App path).
- **Headers:** `X-GitHub-Event`, `X-GitHub-Delivery`.
- **Handled Events:**
  - `issues` (`labeled` with `aegis`): enqueues a bug-fix job (subject to `issue_trust` policy).
  - `workflow_run` (`completed`, `conclusion = failure` on default branch): enqueues a CI-fix job.
  - `pull_request_review` (`submitted`, `state = changes_requested`) and `pull_request_review_comment` / `issue_comment` (`/aegis fix`): enqueues a review round on an agent PR (requires repo write permission).
  - `pull_request` (`closed`, `merged = true` on an agent PR): triggers Light-tier `merged_pr` memory extraction.
  - `installation` / `installation_repositories` (GitHub App): synchronizes installation and repository bindings.

### `POST /webhooks/gitlab`
- **Verification:** `X-Gitlab-Token` compared in constant time via SHA-256 digest against `AEGIS_GITLAB_WEBHOOK_TOKEN`.
- **Headers:** `X-Gitlab-Event`, `X-Gitlab-Event-UUID`.
- **Handled Events:**
  - `Issue Hook` (labeled `aegis`): enqueues a bug-fix job.
  - `Pipeline Hook` (`failed` on default branch): enqueues a CI-fix job.
  - `Note Hook` (`/aegis fix` on an agent MR): enqueues a review round.
  - `Merge Request Hook` (`merged` on an agent MR): triggers `merged_pr` memory extraction.

---

## 4. Further Reading
- [Developer CLI (`aegis`)](client.md)
- [Configuration Reference](configuration.md)
- [AMS Integrations Runbook](runbook/ams-integrations.md)
- [Incident Runbook: Pausing Every Agent](runbook/incident.md)
