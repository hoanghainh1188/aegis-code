#!/usr/bin/env bash
# Sends the webhook GitHub sends when the label `aegis` is put on an issue
# of the sample repo, signed with the trial's secret (docs/local-trial.md).
# Usage: scripts/trial/send-webhook.sh [issue-number]
set -euo pipefail

ISSUE=${1:-1}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-webhook.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"

BODY=$(printf '{"action":"labeled","label":{"name":"aegis"},"issue":{"number":%d},"repository":{"full_name":"demo-org/demo","html_url":"file://%s/upstream/demo"}}' "$ISSUE" "$WORK")
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$AEGIS_GITHUB_WEBHOOK_SECRET" | sed 's/^.* //')
curl -sS http://127.0.0.1:8080/webhooks/github \
  -H 'content-type: application/json' \
  -H 'X-GitHub-Event: issues' \
  -H "X-GitHub-Delivery: trial-$(date +%s)-$ISSUE" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  --data-binary "$BODY"
echo
