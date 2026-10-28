#!/usr/bin/env bash
# Sends the webhook GitLab sends when a pipeline fails on `main` of the
# sample repo's GitLab project (`Pipeline Hook`, pipeline 77 of the fake
# GitLab, whose log shows a failing test), with the trial's token: the
# server queues a CI job that opens a merge request (#253).
# docs/local-trial.md.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
if [ ! -f "$WORK/env.sh" ]; then
  echo "send-gitlab-ci-failure.sh: no trial in $WORK: scripts/trial/up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1091
. "$WORK/env.sh"

BODY=$(printf '{"object_kind":"pipeline","project":{"path_with_namespace":"demo-org/demo","web_url":"file://%s/upstream/demo-gl"},"object_attributes":{"id":77,"ref":"main","tag":false,"status":"failed","source":"push"}}' "$WORK")
curl -sS http://127.0.0.1:8080/webhooks/gitlab \
  -H 'content-type: application/json' \
  -H 'X-Gitlab-Event: Pipeline Hook' \
  -H "X-Gitlab-Event-UUID: trial-gl-ci-$(date +%s)" \
  -H "X-Gitlab-Token: $AEGIS_GITLAB_WEBHOOK_TOKEN" \
  --data-binary "$BODY"
echo
