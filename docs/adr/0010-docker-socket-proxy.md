# ADR 0010 – A Filtering Proxy Between aegis and the Docker Daemon

- Date: 2026-11-04
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #321 (follows the sandbox hardening of #320)

## Context

On EC2 and on premises (Docker Compose or systemd), the `aegis` container started its gate and ACI sandboxes through the host daemon's socket, `/var/run/docker.sock`. Whoever holds that socket is root of the machine: a single injected argument into `docker run` (`-v /:/host`, `--privileged`), or a compromised `aegis` process, would hand over the host and its IAM instance profile ([runbook](../runbook/README.md#worker-and-docker-isolation)).

aegis itself needs only a small part of the Docker API. The gate's runner, the egress proxy of the fetch step and the ACI's formatter and language server make these calls, and only these:

- `docker run`: create, attach, wait, start, kill and remove a container, plus `inspect` for `OOMKilled`;
- `docker logs` and `logs -f`;
- `docker network create --internal`, `connect`, `inspect`, `rm` for the fetch step's network;
- `docker version`, and pulling or inspecting an image.

Every container comes from `Hardening::args` (#320): no capabilities, `no-new-privileges`, memory, CPU and process limits, a read-only root, a non-root user, and mounts that are only the job's tree and the workspace caches.

## Decision

1. **A proxy of our own in Rust, `aegis-docker-proxy`** (`crates/aegis-docker-proxy`), rather than an off-the-shelf proxy such as `docker-socket-proxy`. That one is HAProxy configured by API path and method only. It can allow or forbid `POST /containers/create` as a whole, but it cannot read the request body, and the body is where `Binds`, `Privileged`, `CapAdd` and `NetworkMode` are. Checking the body is the point of #321. The platform is Rust throughout (ADR 0001), and `hyper` is already in the dependency tree.

2. **What passes.** Only the routes above. Every call on an existing container needs a container named by aegis (`aegis-gate-`, `aegis-egress-`, `aegis-format-`, `aegis-lsp-`). An ID is resolved through the daemon first. So `exec`, `build`, `archive`, `update`, volumes, plugins, swarm, listing containers, and any call on `postgres`, `aegis` or the proxy itself are refused.

3. **Bodies are rebuilt, never forwarded.** Docker decodes JSON with Go's `encoding/json`: it matches keys without regard to case, and a later duplicate wins. A body checked as received (`"Privileged": false, "privileged": true`) could mean something else to the daemon. So the proxy forwards a new object made of the keys it checked, spelled as Docker spells them. Any other key must hold its zero value, which is also what the daemon assumes when the key is absent, and is dropped. Calls without a body to check must have an empty body, and their framing headers are dropped. Query strings are decoded as Go decodes them: a key given twice, or a key the call does not take (`fromSrc` on a pull), is refused.

   What a created container may be:
   - **Required:** exactly the #320 hardening (`CapDrop ["ALL"]`, `SecurityOpt ["no-new-privileges"]` alone, memory with no swap, CPUs, processes), and a `uid:gid` in plain digits, neither 0.
   - **Root file system:** read-only, unless the image is listed.
   - **Left at the daemon's defaults** (refused when set to anything else):
     - privileges, capabilities (`CapAdd` and `Capabilities`), devices and groups;
     - namespaces and cgroups;
     - masked `/proc` paths, seccomp and AppArmor;
     - log driver, ports and restart policy.
   - **Mounts:**
     - **Bind mounts** must resolve, after following symbolic links, to a path under a `--mount-root` (the workspace). Docker follows links in mount sources, so a link in a job's tree pointing at `/` would otherwise mount the machine. Missing sources are refused because the daemon would create them.
     - **Other mounts** may only be anonymous volumes and tmpfs, with no driver or bind options: a `local` volume's options can bind any host path.
   - **Networks:** `none`, the fetch step's `aegis-fetch-*` networks (created only as internal bridges), or a network the deployment lists, with no endpoint settings. The default bridge is allowed without bind mounts: that is the egress proxy, which holds nothing of a job.
   - **API versions** before 1.44 (Docker 25) are refused. Before 1.24, `start` took a `HostConfig` of its own. Before 1.41, an unset `CgroupnsMode` meant the host's namespace on cgroup v1.
   - **tmpfs** must be `nosuid,nodev`, as `Hardening::args` sets it.

4. **Deployment.** The proxy is a binary in the `aegis-server` image, run as its own container:
   - it runs as uid 10001 with the socket's group, with no network, a read-only root and no capabilities;
   - only it mounts the daemon's socket. The workspace is mounted read-only at the same path, so links can be resolved;
   - its socket (`/run/aegis-docker/docker.sock`, mode 0600) is shared with `aegis`, which gets `DOCKER_HOST` pointing at it and no longer has the daemon's group;
   - on Compose it is the `docker-proxy` service; under systemd it is `aegis-docker-proxy.service`, which `aegis.service` requires. On AWS this is the same unit.

5. **Sonar.** The scanner runs on `[sonar] network` (default `bridge`) with its cache mounted and a writable root. A mode that enables Sonar adds `--network <that network>` and `--writable-image <the scanner image>` to the proxy's arguments.

6. **Kubernetes is out of scope.** Each worker Pod has its own daemon in a DinD sidecar ([ADR 0008](0008-gate-sandbox-on-kubernetes.md)), so the socket gives root of that Pod's daemon, not of a shared node daemon. The proxy can be added there later as a third container.

## Consequences

- A flaw in how the gate builds `docker run` arguments, or a compromised `aegis` process, no longer reaches the host. It can start only sandboxes as hardened as the gate's, with only the workspace mounted, and act only on aegis's own containers.
- The proxy holds the daemon's socket, so it is now the component that is root-equivalent. It is small, has no network, parses only HTTP/1.1 and JSON, and logs each refused call (method, path, reason; never the body, which holds the container's environment).
- Remaining gaps:
  - **The mount check races.** A process able to write to the workspace could swap a directory for a link between the check and the daemon's mount. Rootless Docker, or worker machines apart from API nodes (#323), is the next step.
  - **No limit on images.** A container may run any image, hardened. Pinning images to the profiles' list is a later change if needed. With that gap comes another: the daemon looks up `User` in the image's `/etc/passwd` before reading it as a number, so an image that names a user `1000` with uid 0 runs as root, though still with no capabilities.
- A new call in the gate or the ACI must be added to the proxy, or it is refused: the proxy's log names it. Running the gate's Docker tests with `DOCKER_HOST` on a local proxy catches this.
