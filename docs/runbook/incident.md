# Incident Runbook: Pausing Every Agent

For an incident that is not one repo's (#510): a model acting strangely, a prompt injection spreading across repos, a provider billing wrong, a gate image that lets something through. One switch stops every agent of the instance while the dashboard, the API and webhook intake keep running. For a single repo that runs wrong or spends too fast, pause that repo instead ([repos.md](repos.md), `repo pause`, #374).

## The Two Levels

| Level | What stops | What keeps going | When to use it |
| --- | --- | --- | --- |
| **Intake** (`intake`, the default) | No queued job is claimed by any worker. | Running jobs finish as they would. Webhooks, trackers and Sonar still record jobs; they wait in the queue, each saying `waiting: all agents are paused by …`. | You need time to look: odd PRs, a spike in spend, a provider warning. Nothing running is known to be harmful. |
| **Emergency** (`emergency`) | All of intake, and every running job is asked to cancel, as `POST /api/v1/jobs/{id}/cancel` asks one. Each worker stops its job at its next lease renewal (`renew_s`, about 20 s) and removes the job's containers. | Queued jobs are kept, not cancelled. Intake still records new jobs. | What is running is doing harm or costing money now: a model writing to PRs it should not, an injection being acted on, runaway token use. |

An intake pause can be raised to an emergency stop; an emergency stop is not lowered, only resumed. Running jobs cancelled by an emergency stop end `cancelled` and are not retried on their own: retry the ones still wanted after resuming (`POST /api/v1/jobs/{id}/retry`, or *Retry* on the job's page).

## Pausing

Admins only. Any one of these does it, and each is audited as `instance.pause` with who, the level, the reason and how many running jobs were asked to cancel:

- **Dashboard:** *Admin → Pause all* (`/admin/pause`), confirm with the second button.
- **API** (an admin service or personal token):

  ```bash
  curl -sS -X POST "$AEGIS/api/v1/admin/pause" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' -d '{"level":"emergency","reason":"injection in PR bodies"}'
  ```

  `GET /api/v1/admin/pause` shows the current pause. A non-admin token gets `403 forbidden`; pausing twice at the same level gets `409 conflict`.
- **Command line**, for when the dashboard and the API cannot be reached; it goes straight to the database, audited as `cli`:

  ```bash
  aegis-server --config /etc/aegis/server.toml pause-all [--emergency] [--reason "<text>"]
  ```

  Run it as the other server commands are run ([repos.md](repos.md), first table): on AWS `sudo docker exec aegis aegis-server --config /etc/aegis/server.toml pause-all …` through SSM, with Compose `docker compose exec aegis …`, on Kubernetes `kubectl -n aegis-worker exec aegis-worker-0 -c aegis -- aegis-server …`.

While paused, every dashboard page shows a red band with who paused and why, `aegis_paused` is 1, and the alert `AegisPaused` (warning) fires after a minute so the whole team knows. `AegisQueueBacklog` stays quiet meanwhile: the queue is meant to wait.

Do not stop the worker processes instead: that stops the dashboard and the API too on an `all` machine, and a restarted worker would take jobs again.

## While Paused

1. Find the cause: the audit log (`/admin/audit`), the jobs of the last hour (`/jobs`), provider errors and tokens on the Grafana dashboard, the jobs' logs.
2. For an emergency stop, check the running jobs ended: `GET /api/v1/jobs?status=running` should empty within a minute. A job still `running` after its lease (`lease_s`) belongs to a worker that died; the next reclaim ends it as cancelled.
3. Close or revert PRs the agents opened during the incident, on the forge.
4. Fix the cause: roll back the deployment ([README §2](README.md#2-rollback)), rotate a key ([README §3](README.md#3-secret-rotation)), pause or remove the repos involved, or cancel the queued jobs that should not run (`POST /api/v1/jobs/{id}/cancel`).

## Resuming

Once the cause is fixed, any one of these resumes every agent, audited as `instance.resume`:

- *Admin → Pause all → Resume all agents*,
- `POST /api/v1/admin/resume`,
- `aegis-server --config /etc/aegis/server.toml resume-all`.

Workers claim the queued jobs again at once, oldest first, within `max_workers` and each repo's `max_concurrent_jobs`, budget and own pause. No queued job is lost. Repos paused one by one stay paused. Watch the first jobs on `/jobs` and the provider panels before leaving it.
