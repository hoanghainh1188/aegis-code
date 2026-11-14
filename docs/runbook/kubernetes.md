# Operations Runbook: Kubernetes

The Helm chart in [`deploy/helm/aegis`](../../deploy/helm/aegis) runs aegis on a Kubernetes cluster, as [ADR 0008](../adr/0008-gate-sandbox-on-kubernetes.md) decided. Every value is described in [`values.yaml`](../../deploy/helm/aegis/values.yaml).

| | |
| --- | --- |
| API | Deployment, `--role api`, no Docker. Service on 8080, optional Ingress |
| Workers | StatefulSet, `--role worker`, in a namespace of their own. Each Pod has a Docker daemon in a `dind` sidecar, `aegis-docker-proxy` in front of it, and two volumes: the workspace and `/var/lib/docker` |
| PostgreSQL | Your own by default. `postgresql.enabled` runs one in the release, for a trial |
| Secrets | Kubernetes Secrets you create, mounted as files in `/run/secrets` ([#261](README.md#on-premise-file-based-secrets-261)). None is in the chart or its values |

## Security Model

**The `dind` sidecar is privileged, so whoever holds its socket can become root on its node.** The worker does not hold it: only `dind`, the `sandbox-images` step and the `docker-proxy` sidecar mount that socket. The `aegis` container reaches the daemon through [`aegis-docker-proxy`](../adr/0010-docker-socket-proxy.md), which passes only the gate's calls and hardened sandboxes with the workspace mounted, so a compromised worker process cannot start a privileged container.

The proxy runs as uid 10001 with a read-only root and no capabilities. A Pod has no `network_mode: none`, so unlike on Compose it shares the Pod's network; it listens only on a Unix socket. Refused calls are logged as `refused` with method, path and reason: `kubectl -n aegis-worker logs aegis-worker-0 -c docker-proxy`. A mode with Sonar needs `worker.dockerProxy.extraArgs: [--network, <[sonar] network>, --writable-image, <[sonar] image>]`, or its scanner is refused. Repos with sidecars (#490) add `--sidecar-image, <image>` for each image; the sidecars run in the Pod's `dind` daemon like the sandboxes.

**What is still open.** A sandbox that escapes its container lands inside the privileged sidecar and can become root on the node, and the proxy's two known gaps apply here too: a process that can write to the workspace can race the mount check, and any image may run, though only hardened. The chart limits what that reaches:

- Run workers on a dedicated, tainted node pool (`worker.nodeSelector`, `worker.tolerations`), so that the node holds nothing else.
- The worker namespace is the only one that needs Pod Security level `privileged`. The API's and PostgreSQL's Pods meet `restricted`.
- A worker's Secret has only what jobs need. The session key, the webhook secrets and the OIDC client secret stay in the API's Secret, so a worker cannot forge sign-ins or webhooks.
- No Pod has a ServiceAccount token.
- NetworkPolicies let a worker out only on DNS, 443 and PostgreSQL's port, and let nothing in but the Prometheus you name. The cloud metadata range is excluded. **By default those ports stay open towards the rest of the cluster**, including the Kubernetes API's Service on 443: add your cluster's Pod and Service CIDRs to `networkPolicy.blockedCidrs` to close them. The chart's PostgreSQL keeps its own rule, but a database of yours inside those CIDRs would be cut off. The cluster's network plugin must enforce NetworkPolicy, or these do nothing.
- A compromised sandbox has less to work with: it runs inside the Pod's daemon with `--network none`, no capabilities and no socket.

Clusters that cannot allow `privileged` need Sysbox or Kata through `worker.runtimeClassName`.

## 1. Installation

**Needs a release after 0.6.0:** the 0.6.0 image refuses the files of a Secret volume (`is open to others than its owner`).

Needs: Kubernetes 1.29 or later (native sidecars; checked on 1.37), Helm (checked with 4.3), a default StorageClass or the `storageClass` values, and PostgreSQL 17 unless you use the chart's.

Each worker needs room for the sandbox images (about 7 GB for all six profiles, more with build caches): see `worker.persistence`. These sizes cannot be changed after install.

1. **Namespaces.** The worker namespace must allow the sidecar:

   ```bash
   kubectl create namespace aegis
   kubectl create namespace aegis-worker
   kubectl label namespace aegis-worker pod-security.kubernetes.io/enforce=privileged
   ```

2. **Secrets.** One file per secret, named as the secret is, then one Kubernetes Secret per role:

   ```bash
   kubectl -n aegis create secret generic aegis-api --from-file=./secrets/api/
   kubectl -n aegis-worker create secret generic aegis-worker --from-file=./secrets/worker/
   ```

   | Secret | Files |
   | --- | --- |
   | `aegis-api` | `AEGIS_DATABASE_URL`, `AEGIS_SESSION_KEY` (`openssl rand -hex 32`), and as you use them `AEGIS_GITHUB_WEBHOOK_SECRET`, `AEGIS_GITLAB_WEBHOOK_TOKEN`, `AEGIS_OIDC_CLIENT_SECRET` |
   | `aegis-worker` | `AEGIS_DATABASE_URL`, the mode's API key (`ANTHROPIC_API_KEY` for `poc`), `AEGIS_GITHUB_COMMENT_TOKEN`, each repo's fork token, Sonar and tracker tokens |

   With the chart's PostgreSQL, also create the password and point `AEGIS_DATABASE_URL` at its Service:

   ```bash
   kubectl -n aegis create secret generic aegis-postgres --from-file=password=./secrets/postgres_password
   # postgres://aegis:<password>@aegis-postgresql.aegis.svc.cluster.local:5432/aegis
   ```

3. **Values.** A `values.yaml` of your own, at least:

   ```yaml
   config:
     auth:
       publicUrl: https://aegis.example.internal
   api:
     secretName: aegis-api
   worker:
     secretName: aegis-worker
   ingress:
     enabled: true
     className: nginx
     host: aegis.example.internal
     tls:
       - hosts: [aegis.example.internal]
         secretName: aegis-tls
   networkPolicy:
     # Your Ingress controller.
     apiFrom:
       - namespaceSelector:
           matchLabels:
             kubernetes.io/metadata.name: ingress-nginx
   ```

4. **Install.** The worker's first start fetches the sandbox images, so allow for it:

   ```bash
   helm install aegis deploy/helm/aegis -n aegis -f values.yaml --wait --timeout 30m
   ```

   **Check:** `kubectl -n aegis-worker get pods` shows `aegis-worker-0` Running and ready, and `kubectl -n aegis logs deploy/aegis-api` shows `API listening`.

### Metrics

Every Pod serves `/metrics` on 9464. Set `metrics.serviceMonitor.enabled` with the Prometheus Operator, and name Prometheus in `networkPolicy.metricsFrom`, or the policies refuse the scrape. The alert rules and dashboard are the same as elsewhere: [`deploy/alerts`](../../deploy/alerts), [`deploy/grafana`](../../deploy/grafana).

### Sandbox Images

`sandboxImages.source` says where a worker gets them, before it starts:

- `pull`: from `sandboxImages.registry` (by default `image.registry`), once, without credentials: `imagePullSecrets` do not reach the Pod's own daemon, so a registry that needs a login calls for `offline`. They stay on the Pod's volume.
- `offline`: from a PersistentVolumeClaim in the worker namespace (`sandboxImages.offline.claimName`) holding the release's `aegis-sandbox-<profile>-<version>-<arch>.tar.gz` ([docs/release.md](../release.md)), `ReadOnlyMany` if workers are on more than one node. The server, `docker:29-dind` and PostgreSQL images must then be in a registry your cluster can reach: set `image.registry`, `worker.dind.image.repository` and `postgresql.image.repository`.
- `none`: you load them yourself.

## 2. Registering Repositories

The steps are those of [Adding Repositories](repos.md), with this prefix for the CLI:

```bash
kubectl -n aegis-worker exec aegis-worker-0 -c aegis -- aegis-server --config /etc/aegis/server.toml <command>
```

- A repo file must be on the workspace volume. Make its directory once with `kubectl -n aegis-worker exec aegis-worker-0 -c aegis -- mkdir -p .workspace/repo-files`, then `kubectl -n aegis-worker cp myrepo.toml aegis-worker-0:/srv/aegis/.workspace/repo-files/myrepo.toml -c aegis`.
- A new token is a new key in the `aegis-worker` Secret. The server reads the updated volume within `[secrets] refresh_s` plus the kubelet's sync (about a minute), with no restart:

  ```bash
  kubectl -n aegis-worker patch secret aegis-worker --type merge -p "{\"data\":{\"AEGIS_TOKEN_MYREPO\":\"$(printf %s "$TOKEN" | base64 | tr -d '\n')\"}}"
  ```

- Each worker Pod keeps its own clone, made the first time it runs a job of that repo.

## 3. Upgrades and Rollback

1. Read the CHANGELOG, and review schema changes against the [Expand/Contract Guidelines](README.md#database-migrations-expandcontract-pattern).
2. Back up PostgreSQL. With the chart's:

   ```bash
   kubectl -n aegis exec aegis-postgresql-0 -- pg_dump -U aegis -Fc aegis > aegis-$(date +%F).dump
   ```

3. Upgrade from the chart of the target version:

   ```bash
   git checkout vX.Y.Z
   helm upgrade aegis deploy/helm/aegis -n aegis -f values.yaml --wait --timeout 30m
   ```

   Workers restart one at a time. Each finishes its running jobs for up to `config.worker.drainSeconds` (10 minutes), with its daemon still up, so one worker can take that long. The workspace and the daemon's storage stay; the new version's sandbox images are fetched before the new worker starts, and older ones removed.

4. **Rollback:**

   ```bash
   helm history aegis -n aegis
   helm rollback aegis <revision> -n aegis --wait --timeout 30m
   ```

   The schema is not rolled back; older versions run on schemas migrated by newer ones. If a bug corrupted data, restore the backup of step 2.

`worker.persistence` cannot change in an upgrade: Kubernetes refuses changes to a StatefulSet's `volumeClaimTemplates`. `helm uninstall` leaves the volumes of the StatefulSets, and the worker namespace; delete them with `kubectl` when you mean to.

## 4. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| Pod refused with `violates PodSecurity` | The worker namespace lacks the label of step 1 |
| `aegis-worker-0` is `Init:CrashLoopBackOff` | `kubectl -n aegis-worker logs aegis-worker-0 -c sandbox-images`: a pull that failed, or a bundle file missing for this version and architecture |
| `Init` stays on `dind` | `kubectl -n aegis-worker logs aegis-worker-0 -c dind`. A daemon that cannot use `overlayfs` on the volume usually means a StorageClass with an unsuitable filesystem (NFS): use a block volume |
| A job fails with `aegis-docker-proxy refused the call` | `kubectl -n aegis-worker logs aegis-worker-0 -c docker-proxy` names the call and the reason. A mode with Sonar needs `worker.dockerProxy.extraArgs` |
| `secret ... is open to others than its owner` | The Secret is mounted some other way than this chart's. Files must be `0400` or `0600`, or `0440` with root as owner and the process's group as group |
| `AEGIS_DATABASE_URL: not loaded` | The key is missing from that role's Secret |
| A job's fetch step cannot reach a host | The worker's egress: only 443 is open. Add a port in `networkPolicy.workerExtraEgressPorts` |
| Prometheus shows the targets down | `networkPolicy.metricsFrom` is empty or does not match Prometheus |

## 5. Checking the Chart on kind

`deploy/helm/check.sh` lints and renders the chart without a cluster; CI runs it. A real run through the gate needs kind and an image with the `aegis` CLI, which the server image does not have: the steps are in [the measurement](../measurements/2026-11-09-helm-chart-kind.md).
