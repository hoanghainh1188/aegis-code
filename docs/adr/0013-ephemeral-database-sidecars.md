# ADR 0013 – Ephemeral Database Sidecars in Gate Sandbox

- Date: 2026-11-13
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #454 (Milestone: `Phase 3 – Mở rộng`, builds on [ADR 0011](0011-ams-direction.md))
- Amendments: 2026-11-14, sidecars implemented in the gate (#490) and the agent's `run_tests` (#491): see "Amendment (#490)" below

## Context

A substantial proportion of legacy applications managed under enterprise Application Management Services (AMS) contracts do not possess mocked unit test suites. Their integration and regression test suites require live backing services:
- Relational databases (PostgreSQL, MySQL, SQL Server)
- In-memory data structures and caches (Redis, Memcached)
- Document stores and message brokers (RabbitMQ, MongoDB)

Under Aegis's existing sandbox design ([ADR 0001](0001-platform-in-rust.md), [ADR 0010](0010-docker-socket-proxy.md)), `aegis-gate` runs verification commands inside an air-gapped container configured with `--network none`. In this isolated mode:
1. Tests attempting socket connections to `127.0.0.1:5432` or `localhost:6379` fail with immediate `ECONNREFUSED` or connection timeouts.
2. Executing database servers on the host machine is strictly prohibited: host services violate multi-tenant isolation, risk state pollution between consecutive jobs, and allow potentially hostile test code to access host resources.
3. Enabling external network egress is prohibited: internet-routable containers introduce prompt injection exfiltration risks and breach air-gapped sandbox compliance.

## Decision

1. **Configurable Service Sidecars (`RepoConfig.sidecars`):**
   - Repositories declare required ephemeral services in `RepoConfig` (`config/*.toml` or database registry):
     ```toml
     [[repo.sidecars]]
     name = "postgres"
     image = "postgres:16-alpine"
     env = { POSTGRES_PASSWORD = "test", POSTGRES_DB = "testdb" }
     init_sql = "schema.sql"               # optional migration / seed DDL
     ready_command = "pg_isready -h localhost -p 5432"
     ready_timeout_s = 20
     ```

2. **Isolated Internal Docker Bridge (`--internal`):**
   - For each gate run requiring sidecars, `aegis-gate` allocates an isolated Docker bridge network:
     `docker network create --internal aegis-gate-<job-id>`
   - The `--internal` flag ensures Docker disables default gateway routing to the host and external internet:
     * **Allowed:** Container-to-container communication within `aegis-gate-<job-id>` via container names (e.g., `postgres:5432`, `redis:6379`).
     * **Blocked:** All ingress from outside and all egress to the host machine or public internet.

3. **Ephemeral Lifecycle & Resource Guardrails:**
   - **Startup:** The sidecar containers spin up before Step 1 of the gate and execute healthchecks via `ready_command`.
   - **In-Memory Storage:** Database storage directories (`/var/lib/postgresql/data`, etc.) mount strictly as anonymous `tmpfs` volumes: no host disk binds are permitted.
   - **Teardown:** An RAII guard in `aegis-gate` terminates and purges all sidecars and the internal bridge network upon gate completion or cancellation (`SIGTERM`/`SIGINT`), ensuring zero state leakage across runs.

4. **Integration with `aegis-docker-proxy` (ADR 0010):**
   - The filtering proxy is updated to permit network creation matching `aegis-gate-*` provided `--internal` is set.
   - Image creation is restricted strictly to an allow-list of approved database sidecar images (`postgres:*`, `redis:*`, `mysql:*`).

5. **Golden Master Characterization Testing:**
   - In legacy repositories with low test coverage, `aegis-gate` utilizes the sidecar database to execute baseline characterization snapshots (recording database state and API response payloads before the patch) and verifies zero regression post-patch.

## Amendment (#490): what the gate implements

The sidecars were split out of #454 to unblock the team pilot (#158); Golden Master characterization (§5) stays in #454. Where this differs from the decision above, this section holds.

1. **Configuration.** `[[repo.sidecars]]` in a run configuration, `[[settings.sidecars]]` in a registry repo file (`aegis_gate::SidecarSpec`): `name` (host name), `image`, `user` (non-root `uid:gid`), `env`, `tmpfs` (directories it writes), `ready` (an argv, not a shell string), `ready_timeout_s`, `localhost`, `test_env`, optional `memory_mb` and `cpus`. There is no `init_sql`: test suites create their schema, as in their own CI. `RunConfig::check` refuses sidecars without `run.sandbox`.
2. **Addressing: both.** Tests reach a sidecar by its `name` on the run's internal network, through `test_env`. For legacy tests that hard-code `localhost`, `localhost = true` runs the test steps in that sidecar's network namespace (`--network container:<sidecar>`); the others stay reachable by name. One per repo.
3. **Which steps.** Only `repro_test`, `test` and `coverage`. Format, lint, typecheck and the fetch step keep `--network none` or their own network; no step reaches the host or the Internet.
4. **Lifecycle.** The sidecars start when the first test step of a gate run needs them, and are reused for the run, monorepo parts included. Before every test command after the first (the base, reruns, coverage) they are stopped and started again, which empties their tmpfs: each command finds fresh services, so the patched run cannot leave state the base run reads. A dropped session removes the containers and the network, on success, error and cancellation; a killed worker process leaves them, as it leaves the egress proxy.
5. **Readiness.** The proxy has no `exec`, so `ready` runs in a short probe container of the sidecar's image, in its network namespace, every 500 ms until it exits 0. A sidecar that stops, or is not ready in `ready_timeout_s`, is a `GateError` with the end of its log.
6. **Hardening.** Sidecars get the gate's flags (#320): no capabilities, `no-new-privileges`, limits (1 GB, 2 CPUs, 512 pids by default), a read-only root, `/tmp` and the declared directories on tmpfs owned by `user`, and no bind mount of the host. An image that must start as root to `chown` its data directory does not run; its non-root user does (`postgres:*-alpine` 70:70).
7. **Proxy (ADR 0010).** Networks `aegis-sidecar-*` only as internal bridges; containers named `aegis-sidecar-*` only with an image in `--sidecar-image` (exact names, set by the deployment, not patterns), on their own network or a sidecar's namespace, with no bind mount; network aliases only for them; `NetworkMode: container:<x>` only when `x` is a sidecar.
8. **The agent's `run_tests` (#491).** The lifecycle hands the ACI the repo's sidecars (`--sidecars <file>`, `[[sidecar]]`). They start at the first `run_tests`, are reset before each later one, and live for the agent's session, apart from the gate's own: the ACI removes them when the session ends (`AciServer::stop_tests`, as its process exits without dropping). An ACI process that is killed leaves them, like a killed worker.
9. **Not yet.** The offline bundle does not hold sidecar images; the operator loads them.

## Consequences

- **Positive:** Enables automated maintenance and regression verification on legacy enterprise repositories previously considered "untestable" in air-gapped environments.
- **Positive:** Preserves the core security invariant: zero internet connectivity and zero host access during code execution.
- **Negative / Trade-off:** Sidecar container startup adds 5–15 seconds of initialization latency per gate run. This is mitigated by reusing the sidecar across the 6 gate steps of a single run, and only spinning up sidecars when declared in `RepoConfig`.
