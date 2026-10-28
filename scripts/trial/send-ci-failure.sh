#!/usr/bin/env bash
# Sends the webhook GitHub sends when the CI workflow fails on `main`
# (`workflow_run`, run 77 of the fake GitHub, whose log shows a failing
# test), signed with the trial's secret: the server queues a CI job (#250).
# docs/local-trial.md.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-ci-failure.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"

BODY=$(printf '{"action":"completed","workflow_run":{"id":77,"conclusion":"failure","head_branch":"main","event":"push","head_repository":{"full_name":"demo-org/demo"}},"repository":{"full_name":"demo-org/demo","html_url":"file://%s/upstream/demo"}}' "$WORK")
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$AEGIS_GITHUB_WEBHOOK_SECRET" | sed 's/^.* //')
curl -sS http://127.0.0.1:8080/webhooks/github \
  -H 'content-type: application/json' \
  -H 'X-GitHub-Event: workflow_run' \
  -H "X-GitHub-Delivery: trial-ci-$(date +%s)" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  --data-binary "$BODY"
echo
