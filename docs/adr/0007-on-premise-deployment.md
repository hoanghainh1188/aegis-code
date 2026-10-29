# ADR 0007 – On-Premise Self-Hosted Deployment

- Date: 2026-10-29
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #262 (alongside #261, #263; Kubernetes in #264)

## Context

[ADR 0003](0003-platform-service-architecture.md) §6 established an initial deployment model: EC2, RDS, AWS Secrets Manager, and ECR on AWS, provisioned via OpenTofu. Certain enterprise customers require deploying the entire platform within their internal infrastructure without cloud accounts or outbound Internet access.

The platform architecture is compact:
- A single unified binary `aegis-server` (roles: `api`, `worker`, or `all`);
- PostgreSQL managing both persistent state and the transactional job queue (`FOR UPDATE SKIP LOCKED`), requiring no Redis or message brokers;
- Language sandbox images executing directly on the host Docker daemon.

## Decision

1. **Preserve Core Architecture:**
   - Identical `aegis-server` binary and sandbox images as the AWS cloud deployment.
   - Identical sandbox execution model: the `aegis` container invokes the host Docker daemon via socket. Workspaces share identical paths `/srv/aegis/.workspace` inside and outside containers, owned by uid 10001.
   - Identical database migrations: the server executes migrations on boot, adhering to the expand/contract pattern.

2. **Differences from Cloud Deployment:**

   | Dimension | AWS Cloud (ADR 0003 §6) | On-Premise Self-Hosted |
   | --- | --- | --- |
   | Secrets Management | AWS Secrets Manager | Local directory files, `[secrets] source = "file"` (#261) |
   | Container Images | ECR, tagged by commit SHA | GHCR, tagged by release version `X.Y.Z`; or offline tarball bundles (#263) |
   | Database | AWS RDS PostgreSQL | Bundled Docker Compose container, or customer dedicated PostgreSQL |
   | Model Inference | Claude API, AWS Bedrock `jp.` | Local model options (`provider.kind = "local"`, #38, #233), custom mode files |
   | TLS Termination | AWS Application Load Balancer | Customer reverse proxy (Nginx, Caddy, Traefik) |
   | Logging | AWS CloudWatch Logs | Host Docker log driver (JSON/journald) |

3. **Two Deployment Methods for Single Linux Hosts:**
   - **Docker Compose** (`deploy/compose/`), default and recommended:
     - Bundles PostgreSQL and `aegis-server --role all`;
     - PostgreSQL binds to internal Compose bridge network, unexposed to host;
     - `install.sh` provisions host dependencies, `aegis-deploy.sh <version>` handles deployment and rollbacks, `backup.sh` handles automated backups.
   - **systemd** (`deploy/systemd/`), for environments with existing PostgreSQL instances:
     - **Shared systemd unit `aegis.service`:** Shared identically with AWS OpenTofu user data;
     - Host-specific parameters (network, ports, secrets, log driver) configured in `/etc/aegis/aegis.env` via `AEGIS_RUN_ARGS`.

4. **Synchronized Release Versioning:**
   - Server and sandboxes share synchronized version tags (`vX.Y.Z`).
   - Rollback re-runs `aegis-deploy` targeting the previous version. Database schema is not rolled back; the server skips unfamiliar migrations (#176).

5. **Kubernetes Path:** Gate execution requires a Docker daemon, which modern Kubernetes nodes running containerd lack. Kubernetes architecture is established separately in #264: [ADR 0008](0008-gate-sandbox-on-kubernetes.md) (Docker-in-Docker sidecar in worker Pods; Helm chart in #281).

## Consequences

- Customers only require a single Linux machine with Docker and Compose; zero cloud accounts or external network connectivity needed when paired with offline bundles and local models.
- **Privilege Boundary:** Access to the Docker socket conveys host root privileges. The host should be dedicated exclusively to Aegis.
- **Hardcoded Workspace Paths:** Host and container workspaces must reside at `/srv/aegis/.workspace`, owned by uid 10001.
- **Shared systemd Unit:** Modifications to `deploy/systemd/aegis.service` affect both on-premise and AWS cloud host templates.
- **Operational Ownership:** Backup schedules (`pg_dump`), host monitoring, and TLS termination remain the customer's operational responsibility, guided by the On-Premise Runbook.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Helm chart only | Many small self-hosted teams lack Kubernetes clusters; sandbox isolation in K8s required dedicated investigation (#264) |
| Docker-in-Docker for local sandboxes | Requires privileged container and duplicates image pulls/caches; adds overhead without increasing security compared to socket mounting |
| Standalone `.deb` / `.rpm` binary packages | Server requires Docker, Codebase-Memory, and Git toolchains; container packaging cleanly bundles all dependencies |
