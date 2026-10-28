#!/usr/bin/env bash
# Reviews the agent's PR on the fake GitHub as `reviewer` ("Request
# changes", with an optional comment on a line), then sends the signed
# `pull_request_review` webhook GitHub sends for it: the server queues a
# review round (#244). docs/local-trial.md.
# Usage: scripts/trial/send-review.sh <pr-number> "<review text>" [<file> <line> "<comment>"]
set -euo pipefail

if [ $# -ne 2 ] && [ $# -ne 5 ]; then
  echo 'usage: send-review.sh <pr-number> "<review text>" [<file> <line> "<comment>"]' >&2
  exit 2
fi
PR=$1
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-review.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"
FAKE=http://127.0.0.1:8070

REVIEW=$(python3 - "$@" <<'PY'
import json, sys
args = sys.argv[1:]
review = {"event": "REQUEST_CHANGES", "body": args[1], "comments": []}
if len(args) == 5:
    review["comments"].append({"path": args[2], "line": int(args[3]), "body": args[4]})
print(json.dumps(review))
PY
)
curl -sSf -X POST "$FAKE/repos/demo-org/demo/pulls/$PR/reviews" \
  -H 'content-type: application/json' --data-binary "$REVIEW" >/dev/null

HEAD=$(curl -sSf "$FAKE/repos/demo-org/demo/pulls/$PR" |
  python3 -c "import json, sys; print(json.load(sys.stdin)['head']['ref'])")
BODY=$(printf '{"action":"submitted","review":{"state":"changes_requested"},"pull_request":{"number":%d,"html_url":"%s/demo-org/demo/pull/%d","state":"open","head":{"ref":"%s"}},"repository":{"full_name":"demo-org/demo","html_url":"%s/demo-org/demo"},"sender":{"login":"reviewer","type":"User"}}' \
  "$PR" "$FAKE" "$PR" "$HEAD" "$FAKE")
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$AEGIS_GITHUB_WEBHOOK_SECRET" | sed 's/^.* //')
curl -sS http://127.0.0.1:8080/webhooks/github \
  -H 'content-type: application/json' \
  -H 'X-GitHub-Event: pull_request_review' \
  -H "X-GitHub-Delivery: trial-review-$(date +%s)-$PR" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  --data-binary "$BODY"
echo
