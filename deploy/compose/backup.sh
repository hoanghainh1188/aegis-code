#!/usr/bin/env bash
# Backs up aegis on Compose (#262, docs/runbook/on-premise.md): the
# database with pg_dump (custom format, for pg_restore), and with
# --workspace the state directory too (clones, module maps, code graph
# indexes; all rebuilt by the server when missing, so optional). Secrets
# and server.toml are not included: keep ./secrets and ./server.toml in
# your own vault.
#
# Usage: sudo ./backup.sh <out dir> [--workspace]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: backup.sh <out dir> [--workspace]}
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
cd "$HERE"

docker compose exec -T postgres pg_dump -U aegis -d aegis -Fc > "$OUT/aegis-db-$STAMP.dump"
echo "database: $OUT/aegis-db-$STAMP.dump"
if [ "${2:-}" = "--workspace" ]; then
  tar -C /srv/aegis -czf "$OUT/aegis-workspace-$STAMP.tar.gz" .workspace/state
  echo "workspace: $OUT/aegis-workspace-$STAMP.tar.gz"
fi
