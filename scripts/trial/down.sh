#!/usr/bin/env bash
# Stops the local trial and removes it: the server, the fake GitHub, the
# two containers, and the trial's directory with its database.
set -uo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}

for pid in "$WORK/server.pid" "$WORK/fake_github.pid"; do
  if [ -f "$pid" ]; then
    kill "$(cat "$pid")" 2>/dev/null
  fi
done
docker rm -f aegis-trial-pg aegis-trial-sso >/dev/null 2>&1
rm -rf "$WORK"
echo "trial removed ($WORK)"
