# Tenants (Contracts)

For an instance that runs AMS for several customers at once (#525, [ADR 0011](../adr/0011-ams-direction.md)). A tenant is one contract. Every repo belongs to exactly one tenant. Repos registered before tenants existed belong to `default`, which keeps today's paths and secret names, so a single-customer instance needs none of this page.

## What a Tenant Keeps Apart

| | How |
| --- | --- |
| People | Roles are per repo, as before. A tenant's **admins** have every right in its repos (their admin pages, roles, pauses, budgets, jobs) and none in another tenant's. The platform's pages (users, services, AI, audit, pause all, registering repos) stay the platform admins'. |
| Budget | `monthly budget`: a cap on the spend of all its repos together this month, on top of each repo's own cap. When less than one job's cap is left, no job of any of its repos is created or claimed; queued ones wait, saying so. |
| Workers | `max jobs`: at most this many of its jobs run at once over its repos. At the same SLA level (#524), the claim takes the job of the tenant running fewest, so one contract cannot hold every worker. |
| Secrets | A repo of a tenant other than `default` may only name secrets starting with the tenant's prefix: `ACME_` for `acme` (`ACME_FORK_TOKEN`, `ACME_JIRA_TOKEN`…). Registering or updating it is refused otherwise. |
| Disk | Its clones, job worktrees, maps and caches live under `<state_dir>/tenants/<id>/` of their mode. |
| Audit, dashboard | The audit log filters by tenant (what was done in its repos and to it). The dashboard groups KPIs by tenant once there is more than one. |

Shared by every tenant: the mode's model provider key (`api_key_env`), the webhook secrets and the comment tokens of the instance.

## Commands

Run as the other server commands are ([repos.md](repos.md), first table), audited as `cli`:

```bash
aegis-server --config /etc/aegis/server.toml tenant add acme "Acme Corp"
aegis-server --config /etc/aegis/server.toml tenant limits acme --budget 500 --max-jobs 2
aegis-server --config /etc/aegis/server.toml tenant admin ops@acme.example acme
aegis-server --config /etc/aegis/server.toml tenant list
```

- **New repo in a tenant:** add `tenant = "acme"` to its repo file, then `repo add`. Repos added from the admin pages go into `default`.
- **Existing repo:** `tenant move <repo-slug> acme`, once none of its jobs runs. Its secret names must already carry the prefix. Its next job clones it under the tenant's directory; the old clone can then be removed.
- `tenant revoke-admin <who> <slug>` takes the role back. `--budget none` and `--max-jobs none` remove a cap.

## AMS Report

The **AMS** page (`/ams`) shows, for the jobs created this month (the budget's month and time zone), by contract and SLA level (#526):

- **MTTR:** from the ticket coming in (the job's creation) to the end of its last run, over the jobs that ended with a result: a PR, a diagnosis or a stop.
- **On time:** of those with an SLA deadline (ITSM tickets, #453), the share that ended by it. **Open, overdue:** queued or running jobs already past their deadline.
- **Triage / PR:** the share resolved by a diagnosis on the ticket (#451), and by a PR.
- **Cost per contract:** what each tenant's repos spent this month, against its cap.

A person sees the contracts of the repos they have a role in; platform admins see all of them. `aegis-local report` prints the same tables when the ledger is in PostgreSQL, with the month in UTC. Prometheus has the same figures: `aegis_ams_mttr_seconds`, `aegis_ams_sla_on_time_ratio`, `aegis_ams_overdue_open_jobs` and `aegis_ams_resolved_jobs{by}`, by `tenant` and `priority`, and `aegis_tenant_budget_spent_usd` / `_cap_usd` by `tenant`.
