#!/usr/bin/env bash
# Prepares this machine for aegis with Docker Compose (#262,
# docs/runbook/on-premise.md), once, as root, in deploy/compose:
#
# - /srv/aegis/.workspace, owned by the image's user (uid 10001);
# - ./secrets, with the database password and URL, the session key and
#   the GitHub webhook secret generated; other secrets are added there;
# - .env with the Docker socket's group; server.toml from the example.
#
# Nothing that exists is overwritten: running it again only fills gaps.
# Usage: sudo ./install.sh
set -euo pipefail

AEGIS_UID=10001
WORKSPACE=/srv/aegis/.workspace
SOCKET=/var/run/docker.sock
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"

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

if [ ! -e .env ]; then
  {
    echo "# Read by docker compose (docker-compose.yml)."
    echo "DOCKER_GID=$(stat -c %g "$SOCKET" 2>/dev/null || stat -f %g "$SOCKET")"
    echo "AEGIS_REGISTRY=ghcr.io/hoanghainh1188"
    echo "# Sandbox images to pull with the server: the profiles of your repos;"
    echo "# python is also the egress proxy of the gate's fetch step."
    echo "AEGIS_SANDBOXES=\"python go rust node java dotnet\""
    echo "AEGIS_VERSION="
  } > .env
  echo "wrote .env"
fi
[ -e server.toml ] || { cp server.toml.example server.toml; echo "wrote server.toml"; }

cat <<'NEXT'

Next (docs/runbook/on-premise.md):
  1. server.toml: public_url, [auth.oidc], modes.
  2. Secrets of your setup, one file each in ./secrets (0400, owned by
     uid 10001): ANTHROPIC_API_KEY or your model's key,
     AEGIS_OIDC_CLIENT_SECRET, AEGIS_GITHUB_COMMENT_TOKEN, each repo's
     fork token. The GitHub webhook secret is generated:
       cat secrets/AEGIS_GITHUB_WEBHOOK_SECRET
  3. sudo ./aegis-deploy.sh <version>
NEXT
