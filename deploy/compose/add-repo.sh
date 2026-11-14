#!/usr/bin/env bash
# Registers one GitHub repo with a running aegis (#476,
# docs/runbook/on-premise.md §2), as root, in deploy/compose:
#
# - asks for the repo, its fork and the fork's token (read without echo);
# - writes the token to ./secrets/AEGIS_TOKEN_<SLUG> unless it is there;
# - writes the repo file to the workspace and runs `repo add`, which
#   clones the repo with the token;
# - grants the maintainers and reviewers their roles;
# - prints the webhook settings and the repo's budget page.
#
# Each value can come from the environment instead: AEGIS_REPO_URL,
# AEGIS_REPO_SLUG, AEGIS_REPO_BRANCH, AEGIS_REPO_LANGUAGE, AEGIS_REPO_MODE,
# AEGIS_FORK_OWNER, AEGIS_FORK_NAME, AEGIS_FORK_TOKEN, AEGIS_MAINTAINERS,
# AEGIS_REVIEWERS (emails, separated by spaces or commas) and
# AEGIS_MONTHLY_USD. AEGIS_NONINTERACTIVE=1, or no terminal: nothing is
# asked, and a value missing from the environment stops the script.
#
# Usage: sudo ./add-repo.sh   (sudo -E keeps the environment's values)
set -euo pipefail

AEGIS_UID=10001
REPO_FILES=/srv/aegis/.workspace/repo-files
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"
# shellcheck source=SCRIPTDIR/lib/prompt.sh
. "$HERE/lib/prompt.sh"

die() { echo "add-repo.sh: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "run as root (sudo): it writes secrets and files owned by uid $AEGIS_UID"
[ -e server.toml ] && [ -d secrets ] || die "no server.toml or ./secrets: run ./install.sh first"
aegis() { docker compose exec -T aegis aegis-server --config /etc/aegis/server.toml "$@"; }
aegis repo list >/dev/null || die "aegis is not running: run ./aegis-deploy.sh <version> first"

# need VAR "question" CHECK "what it must be" [default]: a value is required.
need() {
  local var=$1 question=$2 check=$3 must=$4 default=${5:-}
  ask_valid "$var" "$question" "$check" "$default" || die "$var: $must"
  [ -n "${!var}" ] || die "$var is required: $must"
}
github_repo_url() { [[ $1 =~ ^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; }
github_name() { [[ $1 =~ ^[A-Za-z0-9._-]+$ ]]; }
branch_name() { [[ $1 =~ ^[A-Za-z0-9._/-]+$ ]] && [[ $1 != */ ]]; }
language() { [[ $1 =~ ^(python|rust|go|typescript|java|csharp)$ ]]; }
mode() { [[ $1 =~ ^(poc|customer-jp)$ ]]; }
amount() { [[ $1 =~ ^[0-9]+(\.[0-9]{1,2})?$ ]]; }
emails() {
  local e
  for e in ${1//,/ }; do valid_email "$e" || return 1; done
}

need AEGIS_REPO_URL "Upstream repo (https://github.com/<owner>/<name>)" github_repo_url \
  "https://github.com/<owner>/<name>"
upstream_owner=$(basename "$(dirname "$AEGIS_REPO_URL")")
upstream_name=$(basename "$AEGIS_REPO_URL")
need AEGIS_REPO_SLUG "Slug, in paths and branch names" valid_slug \
  "letters, digits, '.', '_', '-'" "$upstream_name"
need AEGIS_REPO_BRANCH "Default branch" branch_name "a branch name" main
need AEGIS_REPO_LANGUAGE "Language (python, rust, go, typescript, java, csharp)" language \
  "python, rust, go, typescript, java or csharp"
need AEGIS_REPO_MODE "Mode (poc, customer-jp)" mode "poc or customer-jp" poc
need AEGIS_FORK_OWNER "Owner of the fork the bot opens PRs in" github_name \
  "a GitHub user or organisation" "$upstream_owner"
need AEGIS_FORK_NAME "Name of the fork" github_name "a GitHub repo name" "$upstream_name"

token_env=$(token_name "$AEGIS_REPO_SLUG")
if [ ! -e "secrets/$token_env" ]; then
  ask_secret AEGIS_FORK_TOKEN "Token of the bot for $AEGIS_FORK_OWNER/$AEGIS_FORK_NAME"
  [ -n "$AEGIS_FORK_TOKEN" ] || die "AEGIS_FORK_TOKEN is required: the fork's token (docs/runbook/repos.md, step 2)"
  (umask 077; printf '%s\n' "$AEGIS_FORK_TOKEN" > "secrets/.$token_env.new")
  chown "$AEGIS_UID:$AEGIS_UID" "secrets/.$token_env.new"
  mv "secrets/.$token_env.new" "secrets/$token_env"
  echo "secret $token_env"
fi

# Granting before someone's first sign-in needs an email the SSO marks
# verified; without that (Entra ID), grant after they have signed in.
interactive && echo "People granted before their first sign-in need an email the SSO marks verified (not Entra ID); else leave these empty and grant later." >&2
ask_valid AEGIS_MAINTAINERS "Emails of maintainers, who start jobs and set budgets" emails \
  || die "AEGIS_MAINTAINERS: emails"
ask_valid AEGIS_REVIEWERS "Emails of reviewers" emails || die "AEGIS_REVIEWERS: emails"
ask_valid AEGIS_MONTHLY_USD "Monthly budget of the repo in USD" amount \
  || die "AEGIS_MONTHLY_USD: an amount such as 100 or 49.50"

mkdir -p "$REPO_FILES"
chown "$AEGIS_UID:$AEGIS_UID" "$REPO_FILES"
file="$REPO_FILES/$AEGIS_REPO_SLUG.toml"
[ -e "$file" ] && die "$file exists: the repo is registered already (repo update to change it)"
(umask 077; cat > "$file.new" <<TOML
# Written by add-repo.sh (#476); docs/runbook/repos.md lists every key.
slug = "$AEGIS_REPO_SLUG"
url = "$AEGIS_REPO_URL"
default_branch = "$AEGIS_REPO_BRANCH"
language = "$AEGIS_REPO_LANGUAGE"
mode = "$AEGIS_REPO_MODE"
max_concurrent_jobs = 1

[settings.fork]
owner = "$AEGIS_FORK_OWNER"
name = "$AEGIS_FORK_NAME"
token_env = "$token_env"
TOML
)
chown "$AEGIS_UID:$AEGIS_UID" "$file.new"
mv "$file.new" "$file"

added=$(aegis repo add "$file") || { rm -f "$file"; die "repo add failed (see above)"; }
echo "$added"
id=$(printf '%s\n' "$added" | sed -n 's/.*(id \([0-9][0-9]*\)).*/\1/p' | head -n 1)

for email in ${AEGIS_MAINTAINERS//,/ }; do
  if aegis grant "$email" "$AEGIS_REPO_SLUG" maintainer >/dev/null; then
    echo "maintainer: $email"
  else
    echo "add-repo.sh: granting $email maintainer failed; run: grant $email $AEGIS_REPO_SLUG maintainer" >&2
  fi
done
for email in ${AEGIS_REVIEWERS//,/ }; do
  if aegis grant "$email" "$AEGIS_REPO_SLUG" reviewer >/dev/null; then
    echo "reviewer: $email"
  else
    echo "add-repo.sh: granting $email reviewer failed; run: grant $email $AEGIS_REPO_SLUG reviewer" >&2
  fi
done

public_url=$(sed -n 's/^public_url = "\(.*\)"$/\1/p' server.toml)
cat <<NEXT

Next (docs/runbook/on-premise.md §2):
  1. Webhook on $AEGIS_REPO_URL (Settings > Webhooks > Add webhook):
       Payload URL  $public_url/webhooks/github
       Content type application/json
       Secret       sudo cat $HERE/secrets/AEGIS_GITHUB_WEBHOOK_SECRET
       Events       Issues, Workflow runs
     On the fork $AEGIS_FORK_OWNER/$AEGIS_FORK_NAME, the same with Pull request
     reviews, Pull request review comments and Issue comments.
NEXT
if [ -n "$AEGIS_MONTHLY_USD" ] && [ -n "$id" ]; then
  echo "  2. Monthly budget: \$$AEGIS_MONTHLY_USD on $public_url/repos/$id/budget (a maintainer)."
else
  echo "  2. Monthly budget: on the repo's budget page of the dashboard (a maintainer)."
fi
echo "  3. First job: the label aegis on a small, well-described bug."
