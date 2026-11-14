#!/usr/bin/env bash
# Prepares this machine for aegis with Docker Compose (#262,
# docs/runbook/on-premise.md), once, as root, in deploy/compose:
#
# - /srv/aegis/.workspace, owned by the image's user (uid 10001);
# - ./secrets, with the database password and URL, the session key and
#   the GitHub webhook secret generated; other secrets are added there;
# - .env with the Docker socket's group; server.toml from the example.
#
# On a terminal it then asks for the site's values that are still unset
# (#476): public_url, [auth.oidc], the first admin's email, the sandboxes
# to pull, and the secrets ANTHROPIC_API_KEY, AEGIS_OIDC_CLIENT_SECRET and
# AEGIS_GITHUB_COMMENT_TOKEN (read without echo). Each can come from the
# environment instead: AEGIS_PUBLIC_URL, AEGIS_OIDC_ISSUER,
# AEGIS_OIDC_CLIENT_ID, AEGIS_ADMIN_EMAIL, AEGIS_SANDBOXES and the secrets
# by name. AEGIS_NONINTERACTIVE=1, or no terminal: nothing is asked.
#
# Nothing that exists is overwritten: running it again only fills gaps.
# Usage: sudo ./install.sh   (sudo -E keeps the environment's values)
set -euo pipefail

AEGIS_UID=10001
WORKSPACE=/srv/aegis/.workspace
SOCKET=/var/run/docker.sock
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"
# shellcheck source=SCRIPTDIR/lib/prompt.sh
. "$HERE/lib/prompt.sh"

die() { echo "install.sh: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "run as root (sudo): it creates $WORKSPACE and files owned by uid $AEGIS_UID"
command -v docker >/dev/null || die "no docker"
docker compose version >/dev/null 2>&1 || die "no docker compose plugin"
[ -S "$SOCKET" ] || die "no Docker socket at $SOCKET"
command -v openssl >/dev/null || die "no openssl (secrets are generated with it)"

mkdir -p "$WORKSPACE"
chown "$AEGIS_UID:$AEGIS_UID" "$WORKSPACE"
chmod 0750 "$WORKSPACE"

# A secret file only its owner reads (#261); written aside, then renamed.
put_secret() {
  local name=$1 value=$2
  [ -e "secrets/$name" ] && return 0
  (umask 077; printf '%s\n' "$value" > "secrets/.$name.new")
  chown "$AEGIS_UID:$AEGIS_UID" "secrets/.$name.new"
  mv "secrets/.$name.new" "secrets/$name"
  echo "secret $name"
}

mkdir -p secrets modes
chown "$AEGIS_UID:$AEGIS_UID" secrets
chmod 0700 secrets
if [ ! -e secrets/postgres_password ]; then
  password=$(openssl rand -hex 24)
  put_secret postgres_password "$password"
  put_secret AEGIS_DATABASE_URL "postgres://aegis:$password@postgres:5432/aegis"
fi
put_secret AEGIS_SESSION_KEY "$(openssl rand -hex 32)"
put_secret AEGIS_GITHUB_WEBHOOK_SECRET "$(openssl rand -hex 32)"

ALL_SANDBOXES="python go rust node java dotnet"
if [ ! -e .env ]; then
  ask AEGIS_SANDBOXES "Sandboxes to pull, the languages of your repos ($ALL_SANDBOXES)" "$ALL_SANDBOXES"
  case " $AEGIS_SANDBOXES " in
    *" python "*) ;;
    *) AEGIS_SANDBOXES="python $AEGIS_SANDBOXES" ;; # also the fetch step's egress proxy
  esac
  {
    echo "# Read by docker compose (docker-compose.yml)."
    echo "DOCKER_GID=$(stat -c %g "$SOCKET" 2>/dev/null || stat -f %g "$SOCKET")"
    echo "AEGIS_REGISTRY=ghcr.io/hoanghainh1188"
    echo "# Sandbox images to pull with the server: the profiles of your repos;"
    echo "# python is also the egress proxy of the gate's fetch step."
    echo "AEGIS_SANDBOXES=\"$AEGIS_SANDBOXES\""
    echo "AEGIS_VERSION="
  } > .env
  echo "wrote .env"
fi
[ -e server.toml ] || { cp server.toml.example server.toml; echo "wrote server.toml"; }

if grep -q '^public_url = "https://aegis.example.internal"' server.toml; then
  ask_valid AEGIS_PUBLIC_URL "Public URL of aegis, behind your TLS proxy (https://...)" valid_url \
    || die "AEGIS_PUBLIC_URL: an https:// address without a trailing slash"
  if [ -n "$AEGIS_PUBLIC_URL" ]; then
    set_public_url server.toml "$AEGIS_PUBLIC_URL"
    echo "server.toml: public_url"
  fi
fi
if grep -q '^# \[auth.oidc\]$' server.toml; then
  ask_valid AEGIS_OIDC_ISSUER "OIDC issuer of your SSO (https://...)" valid_url \
    || die "AEGIS_OIDC_ISSUER: an https:// address without a trailing slash"
  if [ -n "$AEGIS_OIDC_ISSUER" ]; then
    ask_valid AEGIS_OIDC_CLIENT_ID "OIDC client ID" valid_client_id "aegis" \
      || die "AEGIS_OIDC_CLIENT_ID: letters, digits, '.', '_', ':', '-'"
    set_oidc server.toml "$AEGIS_OIDC_ISSUER" "$AEGIS_OIDC_CLIENT_ID"
    echo "server.toml: [auth.oidc]"
  fi
fi
if ! grep -q '^AEGIS_ADMIN_EMAIL=.' .env; then
  # A role granted before the first sign-in needs an email the SSO marks
  # verified; without that (Entra ID), grant after signing in instead.
  interactive && echo "Leave the next one empty if your SSO does not mark emails verified (Entra ID): grant admin after your first sign-in." >&2
  ask_valid AEGIS_ADMIN_EMAIL "Email of the first admin, as your SSO reports it" valid_email \
    || die "AEGIS_ADMIN_EMAIL: not an email"
  if [ -n "$AEGIS_ADMIN_EMAIL" ]; then
    set_env .env AEGIS_ADMIN_EMAIL "$AEGIS_ADMIN_EMAIL"
    echo ".env: AEGIS_ADMIN_EMAIL (aegis-deploy.sh grants it admin)"
  fi
fi

# Secrets of the site, each asked only while its file is missing.
ask_secret_file() { # ask_secret_file NAME "question"
  local name=$1
  [ -e "secrets/$name" ] && return 0
  ask_secret "$name" "$2"
  [ -z "${!name}" ] || put_secret "$name" "${!name}"
}
ask_secret_file ANTHROPIC_API_KEY "Claude API key (not needed with a local model)"
if grep -q '^\[auth.oidc\]$' server.toml; then
  ask_secret_file AEGIS_OIDC_CLIENT_SECRET "OIDC client secret"
fi
ask_secret_file AEGIS_GITHUB_COMMENT_TOKEN "GitHub token that comments on upstream issues"

cat <<'NEXT'

Next (docs/runbook/on-premise.md):
  1. server.toml: check public_url, [auth.oidc] and modes; anything
     skipped above is still the example's.
  2. Secrets skipped above, one file each in ./secrets (0600, owned by
     uid 10001). Running ./install.sh again asks for them.
  3. sudo ./aegis-deploy.sh <version>
  4. sudo ./add-repo.sh, for each repo.
NEXT
