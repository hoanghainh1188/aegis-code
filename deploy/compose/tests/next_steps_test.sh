#!/usr/bin/env bash
# Tests of lib/next_steps.sh (#556): what add-repo.sh prints once a repo
# is registered, per connection. No root, no Docker, no terminal.
# Usage: deploy/compose/tests/next_steps_test.sh
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=SCRIPTDIR/../lib/next_steps.sh
. "$HERE/../lib/next_steps.sh"

failures=0
check() { # check "name" command...
  local name=$1
  shift
  if "$@"; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    failures=$((failures + 1))
  fi
}
has() { grep -qF -- "$2" <<<"$1" || { echo "  [$2] missing" >&2; return 1; }; }
lacks() { ! grep -qF -- "$2" <<<"$1" || { echo "  [$2] present" >&2; return 1; }; }

url=https://github.com/acme/shop
public=https://aegis.example.internal

app=$(repo_next_steps app "$url" "$public" /opt/aegis/compose 7 100)
check "app: no repo webhook" lacks "$app" "Add webhook"
check "app: no repo webhook secret" lacks "$app" "AEGIS_GITHUB_WEBHOOK_SECRET"
check "app: says the app's webhook covers it" has "$app" "the app's own webhook covers it"
check "app: the installation on the owner" has "$app" "The app installed on acme"
check "app: binding the installation" has "$app" "tenant bind-installation <id> default"
check "app: the budget page" has "$app" "\$100 on $public/repos/7/budget"
check "app: the first job" has "$app" "3. First job"

token=$(repo_next_steps token "$url" "$public" /opt/aegis/compose 7 100 bot shop-fork)
check "token: the repo webhook" has "$token" "Webhook on $url"
check "token: its payload URL" has "$token" "Payload URL  $public/webhooks/github"
check "token: its secret" has "$token" "sudo cat /opt/aegis/compose/secrets/AEGIS_GITHUB_WEBHOOK_SECRET"
check "token: the fork's events" has "$token" "On the fork bot/shop-fork"
check "token: no installation" lacks "$token" "bind-installation"
check "token: the budget page" has "$token" "\$100 on $public/repos/7/budget"

no_id=$(repo_next_steps app "$url" "$public" /opt/aegis/compose "" 100)
check "no repo id: the dashboard's budget page" has "$no_id" "on the repo's budget page of the dashboard"

[ "$failures" = 0 ] || { echo "$failures failed"; exit 1; }
echo "all passed"
