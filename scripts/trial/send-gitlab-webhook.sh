#!/usr/bin/env bash
# Sends the webhook GitLab sends when the label `aegis` is put on issue 1
# of the sample repo's GitLab project (`Issue Hook`), with the trial's
# token: the server queues a `gitlab` job, which opens a merge request on
# the fake GitLab (#252). docs/local-trial.md.
# Usage: scripts/trial/send-gitlab-webhook.sh [issue-iid]
set -euo pipefail

ISSUE=${1:-1}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-gitlab-webhook.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"

BODY=$(printf '{"object_kind":"issue","project":{"path_with_namespace":"demo-org/demo","web_url":"file://%s/upstream/demo-gl"},"object_attributes":{"iid":%d},"changes":{"labels":{"previous":[],"current":[{"title":"aegis"}]}}}' "$WORK" "$ISSUE")
curl -sS http://127.0.0.1:8080/webhooks/gitlab \
  -H 'content-type: application/json' \
  -H 'X-Gitlab-Event: Issue Hook' \
  -H "X-Gitlab-Event-UUID: trial-gl-$(date +%s)-$ISSUE" \
  -H "X-Gitlab-Token: $AEGIS_GITLAB_WEBHOOK_TOKEN" \
  --data-binary "$BODY"
echo
