# shellcheck shell=bash
# What add-repo.sh prints once a repo is registered (#476, #556), apart so
# a test can check it without root or Docker. Sourced, not run.
#
# repo_next_steps CONNECTION REPO_URL PUBLIC_URL COMPOSE_DIR REPO_ID USD \
#   [FORK_OWNER FORK_NAME]
#
# CONNECTION is `app` or `token`. On the app path the app's one webhook
# covers every repo (docs/runbook/repos.md, ADR 0017): no repo webhook,
# whose deliveries the server would ignore for the repo anyway.

repo_next_steps() {
  local connection=$1 repo_url=$2 public_url=$3 here=$4 id=$5 usd=$6
  local fork_owner=${7:-} fork_name=${8:-}
  local owner budget
  owner=$(basename "$(dirname "$repo_url")")
  if [ -n "$usd" ] && [ -n "$id" ]; then
    budget="\$$usd on $public_url/repos/$id/budget (a maintainer)."
  else
    budget="on the repo's budget page of the dashboard (a maintainer)."
  fi
  if [ "$connection" = app ]; then
    cat <<NEXT
Connected through the GitHub App: its agent/* branches and PRs go to $repo_url itself.

Next (docs/runbook/repos.md, GitHub App):
  1. The app installed on $owner with this repo (no webhook on the repo:
     the app's own webhook covers it), its installation bound to a tenant
     once (bound already for another repo of $owner: nothing to do):
       sudo docker compose exec aegis aegis-server --config /etc/aegis/server.toml tenant installations
       sudo docker compose exec aegis aegis-server --config /etc/aegis/server.toml tenant bind-installation <id> default
NEXT
  else
    cat <<NEXT
Connected through the fork $fork_owner/$fork_name.

Next (docs/runbook/on-premise.md §2):
  1. Webhook on $repo_url (Settings > Webhooks > Add webhook):
       Payload URL  $public_url/webhooks/github
       Content type application/json
       Secret       sudo cat $here/secrets/AEGIS_GITHUB_WEBHOOK_SECRET
       Events       Issues, Workflow runs
     On the fork $fork_owner/$fork_name, the same with Pull request
     reviews, Pull request review comments and Issue comments.
NEXT
  fi
  echo "  2. Monthly budget: $budget"
  echo "  3. First job: the label aegis on a small, well-described bug."
}
