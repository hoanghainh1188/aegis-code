#!/usr/bin/env bash
# The Helm chart without a cluster (#281): lint it, render it, and hold the
# properties ADR 0008 and ADR 0010 ask for. CI runs this; the runbook's kind check is
# the one that runs a job.
set -euo pipefail

cd "$(dirname "$0")"
chart=aegis
values=$chart/ci/test-values.yaml
failures=0

render() { helm template aegis "$chart" -n aegis -f "$values" "$@"; }
# One document of the rendered output: its kind and name.
doc() {
  awk -v kind="kind: $1" -v name="  name: $2" '
    function flush() { if (k && n) printf "%s", buf; buf=""; k=0; n=0 }
    /^---/ { flush(); next }
    { buf = buf $0 "\n" }
    $0 == kind { k=1 }
    $0 == name { n=1 }
    END { flush() }'
}
check() {
  local what=$1 expected=$2 actual=$3
  if [ "$expected" = "$actual" ]; then
    echo "ok   $what"
  else
    echo "FAIL $what: expected $expected, got $actual"
    failures=$((failures + 1))
  fi
}
count() { { grep -cE -- "$1" || true; }; }
# One container of a rendered Pod, by name: its lines up to the next one.
container() {
  awk -v name="        - name: $1" '
    $0 == name { c=1; print; next }
    c && (/^        - name: / || /^      [a-zA-Z]/) { c=0 }
    c'
}
fails_with() {
  local what=$1 message=$2
  shift 2
  if output=$(render "$@" 2>&1); then
    echo "FAIL $what: rendered"
    failures=$((failures + 1))
  elif grep -q -- "$message" <<<"$output"; then
    echo "ok   $what"
  else
    echo "FAIL $what: $output"
    failures=$((failures + 1))
  fi
}

helm lint --strict "$chart" -f "$values"

all=$(render)
worker=$(doc StatefulSet aegis-worker <<<"$all")
api=$(doc Deployment aegis-api <<<"$all")
config=$(doc ConfigMap aegis-config <<<"$all")

# Only the sidecar is privileged, and the API has nothing of Docker.
check "one privileged container, in the whole chart" 1 "$(count 'privileged: true' <<<"$all")"
check "the API has no Docker" 0 "$(count 'DOCKER_HOST|dind|docker' <<<"$api")"
# ADR 0008 §2: the daemon listens on its socket, never on TCP.
check "the daemon's only listener is the socket" 1 "$(count '--host=unix:///run/dind/docker.sock' <<<"$worker")"
check "no TCP listener for the daemon" 0 "$(grep -v '^ *#' <<<"$all" | count 'tcp://|2375')"
check "dind skips the image's entrypoint" 1 "$(count '^ +- dind$' <<<"$worker")"
# Before the worker, after it on the way down.
check "dind and the proxy are the native sidecars" 2 "$(count 'restartPolicy: Always' <<<"$worker")"
check "workspace and /tmp are 2770" 1 "$(count 'chmod 2770 /srv/aegis/.workspace /tmp' <<<"$worker")"
check "docker storage is a directory of its volume" 1 "$(count 'mountPath: /var/lib/docker, subPath: docker' <<<"$worker")"
check "no Pod gets a ServiceAccount token" 0 "$(count 'automountServiceAccountToken: true' <<<"$all")"
check "worker Pod has automount off" 1 "$(count 'automountServiceAccountToken: false' <<<"$worker")"
check "API Pod has automount off" 1 "$(count 'automountServiceAccountToken: false' <<<"$api")"
check "workers are in their own namespace" 1 "$(count '^  namespace: aegis-worker$' <<<"$worker")"
check "both namespaces have the configuration" 2 "$(count 'kind: ConfigMap' <<<"$all")"
check "the worker outlives its drain" 1 "$(count 'terminationGracePeriodSeconds: 660$' <<<"$worker")"
# Twice: the API's namespace and the workers'.
check "secrets are files" 2 "$(count '^    source = "file"$' <<<"$config")"
check "the metadata service is out of reach" 2 "$(count '169.254.0.0/16' <<<"$(doc NetworkPolicy aegis-api <<<"$all")$(doc NetworkPolicy aegis-worker <<<"$all")")"

# Helm reads numbers as floats; integers must stay integers in TOML.
big=$(render --set config.auth.apiRequestsPerMinute=1000000 --set config.worker.maxWorkers=12)
check "a large integer" 2 "$(count '^    api_requests_per_minute = 1000000$' <<<"$big")"
check "a small integer" 2 "$(count '^    max_workers = 12$' <<<"$big")"
check "a float" 2 "$(count '^    max_cost_usd = 4.00$' <<<"$big")"

check "a change of configuration rolls the Pods" 1 \
  "$(sort -u <<<"$(grep 'checksum/config' <<<"$all"; render --set config.worker.maxWorkers=9 | grep 'checksum/config')" | wc -l | awk '{print ($1 >= 2)}')"

# ADR 0010: whoever holds the daemon's socket can reach root on the node.
aegis=$(container aegis <<<"$worker")
proxy=$(container docker-proxy <<<"$worker")
check "the worker has no daemon socket" 0 "$(count 'dind|hostPath|/var/run/docker' <<<"$aegis")"
check "the worker has the proxy's socket" 1 "$(count 'name: DOCKER_HOST, value: "unix:///run/aegis-docker/docker.sock"' <<<"$aegis")"
check "the worker mounts the proxy's socket" 1 "$(count 'name: docker-api, mountPath: /run/aegis-docker' <<<"$aegis")"
check "only dind, the image step and the proxy mount the daemon's socket" 3 "$(count 'name: dind-socket, mountPath' <<<"$worker")"
check "the proxy runs as aegis" 1 "$(count 'runAsUser: 10001' <<<"$proxy")"
check "the proxy has a read-only root" 1 "$(count 'readOnlyRootFilesystem: true' <<<"$proxy")"
check "the proxy has no capabilities" 1 "$(count 'drop: \[ALL\]' <<<"$proxy")"
check "the proxy is not privileged" 0 "$(count 'privileged' <<<"$proxy")"
check "the proxy reads the workspace only" 1 "$(count 'name: workspace, mountPath: /srv/aegis/.workspace, readOnly: true' <<<"$proxy")"
check "the proxy talks to the daemon" 1 "$(count '^ +- /run/dind/docker.sock$' <<<"$proxy")"
check "the proxy's mounts stay in the workspace" 1 "$(count '^ +- /srv/aegis/.workspace$' <<<"$proxy")"
check "the proxy opens no port" 0 "$(count 'ports:|tcp://|--network' <<<"$proxy")"
check "the proxy waits for its socket" 1 "$(count 'command: \["test", "-S", "/run/aegis-docker/docker.sock"\]' <<<"$proxy")"
order=$(sed '/^      volumes:/q' <<<"$worker" | grep -E '^        - name: ' | awk '{print $3}' | paste -sd, -)
check "the proxy starts after dind and the images" "volume-modes,dind,sandbox-images,docker-proxy,aegis" "$order"
noimages=$(render --set sandboxImages.source=none | doc StatefulSet aegis-worker)
check "without an image step the proxy still starts" 1 "$(count 'name: docker-proxy' <<<"$noimages")"
check "without an image step only two mount the daemon's socket" 2 "$(count 'name: dind-socket, mountPath' <<<"$noimages")"
sonar=$(render --set-json 'worker.dockerProxy.extraArgs=["--network","sonar"]' | doc StatefulSet aegis-worker | container docker-proxy)
check "the proxy takes a mode's Sonar arguments" 1 "$(count '^ +- sonar$' <<<"$sonar")"

# Options.
offline=$(render --set sandboxImages.source=offline --set sandboxImages.offline.claimName=bundle)
check "offline loads from the volume" 1 "$(count 'gunzip -c' <<<"$offline")"
check "offline does not pull" 0 "$(count 'docker pull' <<<"$offline")"
check "none has no image step" 0 "$(count 'name: sandbox-images' <<<"$(render --set sandboxImages.source=none)")"
check "runtimeClassName" 1 "$(count 'runtimeClassName: kata' <<<"$(render --set worker.runtimeClassName=kata)")"
check "ServiceMonitor is off by default" 0 "$(count 'kind: ServiceMonitor' <<<"$all")"
check "ServiceMonitor when asked" 1 "$(count 'kind: ServiceMonitor' <<<"$(render --set metrics.serviceMonitor.enabled=true)")"
check "Ingress when asked" 1 "$(count 'kind: Ingress' <<<"$(render --set ingress.enabled=true --set ingress.host=a.example)")"
check "PostgreSQL is yours by default" 0 "$(count 'kind: StatefulSet' <<<"$(doc StatefulSet aegis-postgresql <<<"$all")")"
check "PostgreSQL when asked" 1 "$(count '^kind: StatefulSet$' <<<"$(render --set postgresql.enabled=true --set postgresql.secretName=pg | doc StatefulSet aegis-postgresql)")"
namespace=$(render --set worker.namespace.create=true | doc Namespace aegis-worker)
check "a created namespace allows the sidecar" 1 "$(count 'pod-security.kubernetes.io/enforce: privileged' <<<"$namespace")"
check "a created namespace is kept on uninstall" 1 "$(count 'helm.sh/resource-policy: keep' <<<"$namespace")"
check "one namespace is possible" 1 "$(count 'kind: ConfigMap' <<<"$(render --set worker.namespace.name=aegis)")"

check "no proxy is trusted without an Ingress" 2 "$(count '^    trusted_proxies = 0$' <<<"$all")"
check "one proxy is trusted with an Ingress" 2 "$(count '^    trusted_proxies = 1$' <<<"$(render --set ingress.enabled=true --set ingress.host=a.example)")"
check "an explicit 0 stays 0" 2 "$(count '^    trusted_proxies = 0$' <<<"$(render --set ingress.enabled=true --set ingress.host=a.example --set config.auth.trustedProxies=0)")"
long=$(helm template aegis-0123456789012345678901234567890123456789012345 "$chart" -n aegis -f "$values" --set postgresql.enabled=true --set postgresql.secretName=pg)
longest=$(awk '/^kind: StatefulSet$/ {s=1} s && /^  name: / {print length($2); s=0}' <<<"$long" | sort -n | tail -1)
check "a long release name leaves room for the revision hash" 1 "$([ "$longest" -le 52 ] && echo 1 || echo "$longest")"
bundled=$(render --set postgresql.enabled=true --set postgresql.secretName=pg --set-json 'networkPolicy.blockedCidrs=["10.0.0.0/8"]')
check "the chart's PostgreSQL has its own egress rule" 2 "$(count 'app.kubernetes.io/component: postgresql' <<<"$(doc NetworkPolicy aegis-api <<<"$bundled"; doc NetworkPolicy aegis-worker <<<"$bundled")")"

# Values that cannot work are refused when rendering, not at runtime.
values=/dev/null
fails_with "publicUrl is required" "config.auth.publicUrl is required" --set api.secretName=a --set worker.secretName=b
fails_with "the API's Secret is required" "api.secretName is required" --set config.auth.publicUrl=https://a --set worker.secretName=b
fails_with "the worker's Secret is required" "worker.secretName is required" --set config.auth.publicUrl=https://a --set api.secretName=b
values=$chart/ci/test-values.yaml
fails_with "an unknown image source" "sandboxImages.source must be" --set sandboxImages.source=ftp
fails_with "offline needs a volume" "sandboxImages.offline.claimName is required" --set sandboxImages.source=offline
fails_with "the API's namespace is not made privileged" "needs a namespace other than" --set worker.namespace.name=aegis --set worker.namespace.create=true
fails_with "postgresql needs a Secret" "postgresql.secretName is required" --set postgresql.enabled=true

if [ "$failures" -ne 0 ]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "all checks passed"
