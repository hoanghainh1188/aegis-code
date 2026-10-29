# ADR 0008 – Gate Verification Sandboxes on Kubernetes

- Date: 2026-10-29
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #264 (Helm chart in #281)
- Empirical Data: [Experimental Verification on kind](../measurements/2026-10-29-k8s-gate-sandbox.md)

## Context

The gate executes every verification step inside an isolated sandbox container: `docker run --network none --cap-drop ALL …` via the host Docker daemon ([ADR 0003](0003-platform-service-architecture.md) §6, [ADR 0007](0007-on-premise-deployment.md)). All isolation constraints are enforced on that CLI invocation.

Beyond the gate, ACI (`aegis-aci`) also executes formatting linters during `edit`, runs tests via `run_tests`, and hosts long-lived Language Server Protocol (LSP) processes communicating over stdio inside sandbox containers.

Modern Kubernetes worker nodes execute containerd and lack a host Docker daemon. Enterprise customers require running Aegis within existing Kubernetes clusters.

## Decision

1. **Dedicated Docker Daemon per Worker Pod via DinD Sidecar:**
   - The `aegis` container runs non-root without privileges (uid 10001, `drop: [ALL]`, `allowPrivilegeEscalation: false`).
   - The `dind` sidecar container (`docker:<version>-dind`) executes `privileged`.
   - The two containers communicate exclusively over a local unix socket shared via an `emptyDir` volume.
   - Gate and ACI require zero code changes: they issue standard `docker run` commands with identical security flags as on EC2 and Docker Compose. (Requires resolving #278: configuring `aegis-aci` to honor `DOCKER_HOST`).

2. **Sidecar Configuration Details** (empirically validated on `kind`):
   - `dockerd` listens strictly on the local unix domain socket: `command: [dind, dockerd, --host=unix://…, --group=10001]`. Bypasses default entrypoints: absent TLS directories, default entrypoints expose unauthenticated `tcp://0.0.0.0:2375` (granting cluster-wide root access across the Pod network).
   - Workspace (`/srv/aegis/.workspace`) is a shared volume mounted at identical paths in both containers.
   - `fsGroup: 10001`, with an initContainer setting volume permissions to `2770`. (`codebase-memory-mcp` aborts if parent directories of HOME are world-writable; standard Kubernetes `emptyDir` with `fsGroup` defaults to `2777`).
   - DinD `/var/lib/docker` is backed by dedicated persistent storage per Pod (StatefulSet with PVC templates), avoiding repeated multi-gigabyte sandbox image pulls (Go 1.4 GB, .NET 1.6 GB) across Pod restarts.
   - Sandbox images are pulled from GHCR or loaded from offline bundles via `docker load`.

3. **Restricting Privileged Surface Area:**
   - Only worker Pods host privileged DinD sidecars. API instances run as standard stateless Deployments without Docker.
   - Workers reside in dedicated namespaces governed by Pod Security Admission `privileged`.
   - Workers execute on dedicated tainted node pools (`tolerations`), isolated from general workloads.
   - Enforce `automountServiceAccountToken: false` and strict NetworkPolicies.

4. **Security Equivalence with Docker Compose:** On Compose hosts, Docker socket access conveys host root privileges. On Kubernetes, compromising the worker process grants node root via the privileged sidecar. The agent's sandbox cannot access the daemon: it runs inside the Pod's isolated daemon under `--network none`, zero capabilities, and no socket mounts (`CapEff` is 0; outbound routing is blocked).

## Consequences

- No changes to gate runner or ACI logic; resolving #278 enables immediate execution.
- Requires `privileged` capability for one container. Clusters enforcing strict zero-privilege policies require alternative runtimes (Sysbox, Kata Containers).
- StatefulSet persistence model reserves independent image storage per worker Pod.
- Helm chart (#281) codifies this architecture.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Mount node `/var/run/docker.sock` | Modern nodes run containerd, not Docker. Even if present, socket access escapes the Pod lifecycle, breaking resource quotas and automated cleanup |
| Execute each gate step as a dynamic Kubernetes Job/Pod | Requires building a completely separate runner for `aegis-gate` and `aegis-aci` (formatters, `run_tests`, persistent LSP over stdio); requires shared ReadWriteMany storage; adds ~1.6 s latency per step (1.9 s Pod boot vs 0.26 s container run) |
| Rootless DinD (`docker:dind-rootless`) | Official Docker documentation confirms rootless DinD still requires `privileged` when nested inside containers; adds complexity without removing privilege flags |
| Sysbox Runtime | Requires installing custom kernel runtimes on host nodes; viable as a chart option (`runtimeClassName`) for customer-managed clusters |
| Kata Containers / gVisor VM Isolation | Replaces host-root risk with VM-sandbox isolation; chart will expose `runtimeClassName` hooks for Kata |
