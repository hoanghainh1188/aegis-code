#!/usr/bin/env bash
# Reviews the agent's PR on the fake GitHub ("Request changes", with an
# optional comment on a line), then asks for a review round of it through
# the API (#243), as a maintainer would (docs/local-trial.md).
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

REVIEW=$(python3 - "$@" <<'PY'
import json, sys
args = sys.argv[1:]
review = {"event": "REQUEST_CHANGES", "body": args[1], "comments": []}
if len(args) == 5:
    review["comments"].append({"path": args[2], "line": int(args[3]), "body": args[4]})
print(json.dumps(review))
PY
)
curl -sSf -X POST "http://127.0.0.1:8070/repos/demo-org/demo/pulls/$PR/reviews" \
  -H 'content-type: application/json' --data-binary "$REVIEW" >/dev/null

# The job that opened the PR.
JOB=$(curl -sSf "http://127.0.0.1:8080/api/v1/jobs?limit=200" \
  -H "Authorization: Bearer $AEGIS_TRIAL_API_TOKEN" |
  python3 -c "
import json, sys
jobs = json.load(sys.stdin)['data']
found = [j['id'] for j in jobs if (j['pr_url'] or '').endswith('/pull/$PR') and j['review_of'] is None]
print(found[0] if found else '')
")
if [ -z "$JOB" ]; then
  echo "send-review.sh: no job opened PR $PR" >&2
  exit 1
fi
curl -sS -X POST "http://127.0.0.1:8080/api/v1/jobs/$JOB/review" \
  -H "Authorization: Bearer $AEGIS_TRIAL_API_TOKEN"
echo
