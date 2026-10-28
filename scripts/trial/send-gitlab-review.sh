#!/usr/bin/env bash
# Comments on the agent's merge request on the fake GitLab as `reviewer`
# (a Maintainer of the project), optionally on a line, ending with a line
# `/aegis fix`, then sends the `Note Hook` GitLab sends for it: the server
# queues a review round (#254). docs/local-trial.md.
# Usage: scripts/trial/send-gitlab-review.sh <mr-iid> "<note>" [<file> <line>]
set -euo pipefail

if [ $# -ne 2 ] && [ $# -ne 4 ]; then
  echo 'usage: send-gitlab-review.sh <mr-iid> "<note>" [<file> <line>]' >&2
  exit 2
fi
MR=$1
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-gitlab-review.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"
FAKE=http://127.0.0.1:8070
API=$FAKE/api/v4/projects/demo-org%2Fdemo

NOTE=$(python3 - "$@" <<'PY'
import json, sys
args = sys.argv[1:]
note = {"body": args[1] + "\n\n/aegis fix"}
if len(args) == 4:
    note.update(path=args[2], line=int(args[3]))
print(json.dumps(note))
PY
)
curl -sSf -X POST "$API/merge_requests/$MR/notes" -H 'X-Trial-User: reviewer' \
  -H 'content-type: application/json' --data-binary "$NOTE" >/dev/null

BRANCH=$(curl -sSf "$API/merge_requests/$MR" |
  python3 -c "import json, sys; print(json.load(sys.stdin)['source_branch'])")
BODY=$(printf '{"object_kind":"note","project":{"path_with_namespace":"demo-org/demo","web_url":"file://%s/upstream/demo-gl"},"user":{"id":42,"username":"reviewer","bot":false},"object_attributes":{"note":"/aegis fix","noteable_type":"MergeRequest"},"merge_request":{"iid":%d,"state":"opened","source_branch":"%s","url":"%s/demo-org/demo/-/merge_requests/%d"}}' \
  "$WORK" "$MR" "$BRANCH" "$FAKE" "$MR")
curl -sS http://127.0.0.1:8080/webhooks/gitlab \
  -H 'content-type: application/json' \
  -H 'X-Gitlab-Event: Note Hook' \
  -H "X-Gitlab-Event-UUID: trial-gl-note-$(date +%s)-$MR" \
  -H "X-Gitlab-Token: $AEGIS_GITLAB_WEBHOOK_TOKEN" \
  --data-binary "$BODY"
echo
