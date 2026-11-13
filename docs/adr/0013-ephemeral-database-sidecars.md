# ADR 0013 – Ephemeral Database Sidecars in Gate Sandbox

- Date: 2026-11-13
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #454 (Milestone: `Phase 3 – Mở rộng`, builds on [ADR 0011](0011-ams-direction.md))

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

## Consequences

- **Positive:** Enables automated maintenance and regression verification on legacy enterprise repositories previously considered "untestable" in air-gapped environments.
- **Positive:** Preserves the core security invariant: zero internet connectivity and zero host access during code execution.
- **Negative / Trade-off:** Sidecar container startup adds 5–15 seconds of initialization latency per gate run. This is mitigated by reusing the sidecar across the 6 gate steps of a single run, and only spinning up sidecars when declared in `RepoConfig`.
