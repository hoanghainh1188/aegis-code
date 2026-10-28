#!/usr/bin/env bash
# Runs aegis at <version> with Docker Compose (#262,
# docs/runbook/on-premise.md): pulls the server and sandbox images of that
# version (unless --offline: loaded from the offline bundle already), tags
# the sandboxes with the bare names the profiles use, sets AEGIS_VERSION
# in .env and waits for the server to be healthy. The database is migrated
# by the server as it starts; an older server runs on a newer database
# (expand/contract), so rolling back is this script with the version
# before.
#
# Usage: sudo ./aegis-deploy.sh [--offline] <version>
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"
die() { echo "aegis-deploy.sh: $*" >&2; exit 1; }

offline=0
if [ "${1:-}" = "--offline" ]; then
  offline=1
  shift
fi
version=${1:-}
[ -n "$version" ] || die "usage: aegis-deploy.sh [--offline] <version>"
[ -f .env ] || die "no .env: run ./install.sh first"
# shellcheck disable=SC1091
. ./.env
registry=${AEGIS_REGISTRY:-ghcr.io/hoanghainh1188}
previous=${AEGIS_VERSION:-}

images=("$registry/aegis-server:$version")
for p in ${AEGIS_SANDBOXES:-python}; do
  images+=("$registry/aegis-sandbox-$p:$version")
done
for image in "${images[@]}"; do
  if [ "$offline" = 1 ]; then
    docker image inspect "$image" >/dev/null 2>&1 \
      || die "$image is not loaded: run load-images.sh of the $version bundle"
  else
    docker pull --quiet "$image" >/dev/null
  fi
done
for p in ${AEGIS_SANDBOXES:-python}; do
  docker tag "$registry/aegis-sandbox-$p:$version" "aegis-sandbox-$p"
done

sed -i.bak "s/^AEGIS_VERSION=.*/AEGIS_VERSION=$version/" .env && rm -f .env.bak
docker compose up --detach --wait --wait-timeout 300 \
  || { docker compose logs --tail 50 aegis >&2; die "aegis $version is not healthy"; }
if [ -n "$previous" ] && [ "$previous" != "$version" ]; then
  echo "aegis $version healthy; roll back with: ./aegis-deploy.sh $previous"
else
  echo "aegis $version healthy"
fi
