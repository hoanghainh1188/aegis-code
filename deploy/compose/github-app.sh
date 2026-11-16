#!/usr/bin/env bash
# Creates the instance's GitHub App from a manifest (ADR 0017, #542), as
# root, in deploy/compose, after ./install.sh:
#
# - writes github-app-manifest.html: opened in a browser signed in to
#   GitHub, one click creates the app with the right permissions (no
#   Workflows, no Administration), events and webhook;
# - takes back the address GitHub then sends the browser to, and trades
#   its code for the app's id, private key and webhook secret;
# - writes the key and the secret to ./secrets and [github_app] to
#   server.toml.
#
# Values from the environment: AEGIS_GITHUB_ORG (empty: your personal
# account), AEGIS_GITHUB_APP_NAME, AEGIS_GITHUB_URL (GHES' web root;
# https://github.com by default) and AEGIS_GITHUB_APP_CODE (the address or
# its code). Without a terminal, they must all be there.
#
# Usage: sudo ./github-app.sh   (sudo -E keeps the environment's values)
set -euo pipefail

AEGIS_UID=10001
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"
# shellcheck source=SCRIPTDIR/lib/prompt.sh
. "$HERE/lib/prompt.sh"

die() { echo "github-app.sh: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "run as root (sudo): it writes secrets owned by uid $AEGIS_UID"
[ -e server.toml ] && [ -d secrets ] || die "no server.toml or ./secrets: run ./install.sh first"
command -v curl >/dev/null || die "no curl"
command -v jq >/dev/null || die "no jq (it reads GitHub's answer)"
command -v openssl >/dev/null || die "no openssl"
grep -q '^\[github_app\]' server.toml && die "server.toml has [github_app] already"
grep -q '^# \[github_app\]$' server.toml \
  || die "server.toml has no commented [github_app]: copy it from server.toml.example"
for name in AEGIS_GITHUB_APP_PRIVATE_KEY AEGIS_GITHUB_APP_WEBHOOK_SECRET; do
  [ -e "secrets/$name" ] && die "secrets/$name exists: remove it to make a new app"
done
public_url=$(sed -n 's/^public_url = "\(.*\)"$/\1/p' server.toml)
valid_url "$public_url" && [ "$public_url" != "https://aegis.example.internal" ] \
  || die "set public_url in server.toml first: GitHub sends the app's webhooks there"

put_secret() {
  local name=$1 value=$2
  (umask 077; printf '%s\n' "$value" > "secrets/.$name.new")
  chown "$AEGIS_UID:$AEGIS_UID" "secrets/.$name.new"
  mv "secrets/.$name.new" "secrets/$name"
  echo "secret $name"
}
github_org() { [ -z "$1" ] || [[ $1 =~ ^[A-Za-z0-9][A-Za-z0-9-]{0,38}$ ]]; }

ask_valid AEGIS_GITHUB_URL "GitHub's web root (GitHub Enterprise Server: https://<host>)" valid_url \
  "https://github.com" || die "AEGIS_GITHUB_URL: an https:// address without a trailing slash"
ask_valid AEGIS_GITHUB_ORG "Organisation that owns the app (empty: your personal account)" github_org \
  || die "AEGIS_GITHUB_ORG: a GitHub organisation's login"
host=${public_url#https://}
ask_valid AEGIS_GITHUB_APP_NAME "Name of the app (unique on GitHub)" valid_app_name \
  "aegis-code ${host%%[.:/]*}" || die "AEGIS_GITHUB_APP_NAME: letters, digits, '.', '_', '-', spaces; at most 34"
[ -n "$AEGIS_GITHUB_APP_NAME" ] || die "AEGIS_GITHUB_APP_NAME is required"

# AEGIS_GITHUB_URL is set by ask_valid above.
# shellcheck disable=SC2153
if [ "$AEGIS_GITHUB_URL" = "https://github.com" ]; then
  api="https://api.github.com"
else
  api="$AEGIS_GITHUB_URL/api/v3"
fi
if [ -n "$AEGIS_GITHUB_ORG" ]; then
  new="$AEGIS_GITHUB_URL/organizations/$AEGIS_GITHUB_ORG/settings/apps/new"
else
  new="$AEGIS_GITHUB_URL/settings/apps/new"
fi
state=$(openssl rand -hex 16)
manifest=$(github_app_manifest "$public_url" "$AEGIS_GITHUB_APP_NAME")
page="$HERE/github-app-manifest.html"
cat > "$page" <<HTML
<!doctype html>
<meta charset="utf-8">
<title>Create the aegis-code GitHub App</title>
<form method="post" action="$(html_attr "$new?state=$state")">
  <input type="hidden" name="manifest" value="$(html_attr "$manifest")">
  <p>Signed in to GitHub as $( [ -n "$AEGIS_GITHUB_ORG" ] && echo "an owner of $AEGIS_GITHUB_ORG" || echo "yourself" )?</p>
  <button type="submit">Create the GitHub App</button>
</form>
HTML
cat <<STEPS

1. Open this file in a browser signed in to GitHub (copy it to your
   machine first if this host has no browser):
     $page
2. Click "Create the GitHub App", then GitHub's "Create GitHub App".
3. GitHub then opens $public_url/api/v1/health?code=…
   Paste that whole address here (within the hour).
STEPS
ask AEGIS_GITHUB_APP_CODE "Address GitHub opened (or its code)"
code=$(code_of "${AEGIS_GITHUB_APP_CODE:-}")
[ -n "$code" ] || die "no code in the address: run ./github-app.sh again"
given=$(state_of "$AEGIS_GITHUB_APP_CODE")
[ -z "$given" ] || [ "$given" = "$state" ] \
  || die "the address carries another state: it is not the answer to this page"

answer=$(mktemp "$HERE/secrets/.github-app.XXXXXX")
trap 'rm -f "$answer" "$page"' EXIT
curl -fsS -X POST -H "Accept: application/vnd.github+json" \
  "$api/app-manifests/$code/conversions" -o "$answer" \
  || die "GitHub refused the code (used already, or older than an hour): run ./github-app.sh again"
id=$(jq -er '.id' "$answer") || die "GitHub's answer has no app id"
slug=$(jq -er '.slug' "$answer") || die "GitHub's answer has no app slug"
pem=$(jq -er '.pem' "$answer") || die "GitHub's answer has no private key"
secret=$(jq -er '.webhook_secret // empty' "$answer") || die "GitHub's answer has no webhook secret"
put_secret AEGIS_GITHUB_APP_PRIVATE_KEY "$pem"
put_secret AEGIS_GITHUB_APP_WEBHOOK_SECRET "$secret"
if [ "$api" = "https://api.github.com" ]; then
  set_github_app server.toml "$id"
else
  set_github_app server.toml "$id" "$api"
fi
echo "server.toml: [github_app] app_id = $id"

cat <<NEXT

The app $slug exists. Next (docs/runbook/repos.md, GitHub App):
  1. Restart aegis so it reads [github_app]: sudo ./aegis-deploy.sh <version>
  2. Install the app on your org, with the repos aegis may work on:
       $AEGIS_GITHUB_URL/apps/$slug/installations/new
  3. Bind the installation to its tenant (default on a single tenant):
       sudo docker compose exec aegis aegis-server --config /etc/aegis/server.toml tenant installations
       sudo docker compose exec aegis aegis-server --config /etc/aegis/server.toml tenant bind-installation <id> default
  4. On each repo's default branch, a ruleset that requires a pull request,
     with no bypass for the app.
  5. sudo ./add-repo.sh, connection "app".
NEXT
