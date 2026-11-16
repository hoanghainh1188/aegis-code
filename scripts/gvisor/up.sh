#!/usr/bin/env bash
# Starts the bench daemon of #322: Docker-in-Docker with runsc (Dockerfile
# here), the project mounted at its own path so the paths `aegis` gives
# `docker run --volume` exist inside, and the daemon on 127.0.0.1 only.
#
#   scripts/gvisor/up.sh            # then: export DOCKER_HOST=tcp://127.0.0.1:23750
#   docker rm -f aegis-gvisor-bench # when done; images stay in the volume
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
port=${AEGIS_BENCH_PORT:-23750}
docker image inspect aegis-gvisor-bench >/dev/null 2>&1 \
  || docker build -t aegis-gvisor-bench "$root/scripts/gvisor"
docker rm -f aegis-gvisor-bench >/dev/null 2>&1 || true
docker run -d --name aegis-gvisor-bench --privileged \
  -e DOCKER_TLS_CERTDIR= \
  -p "127.0.0.1:$port:2375" \
  -v "$root:$root" \
  -v aegis-gvisor-bench-data:/var/lib/docker \
  aegis-gvisor-bench --host tcp://0.0.0.0:2375 --host unix:///var/run/docker.sock >/dev/null
for _ in $(seq 60); do
  DOCKER_HOST="tcp://127.0.0.1:$port" docker version >/dev/null 2>&1 && break
  sleep 1
done
DOCKER_HOST="tcp://127.0.0.1:$port" docker info --format 'runtimes: {{range $k, $v := .Runtimes}}{{$k}} {{end}}| cgroup {{.CgroupDriver}} v{{.CgroupVersion}}'
