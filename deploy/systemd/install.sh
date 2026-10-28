#!/usr/bin/env bash
# Prepares this Linux machine for aegis under systemd (#262,
# docs/runbook/on-premise.md), once, as root: the workspace, /etc/aegis
# (server.toml, aegis.env, secrets, modes), the unit and aegis-deploy.
# PostgreSQL is yours (on this machine or another): put its URL in
# /etc/aegis/secrets/AEGIS_DATABASE_URL. Nothing that exists is
# overwritten. Then: aegis-deploy <version>.
#
# Usage: sudo deploy/systemd/install.sh
set -euo pipefail

AEGIS_UID=10001
HERE=$(cd "$(dirname "$0")" && pwd)
die() { echo "install.sh: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "run as root (sudo)"
command -v docker >/dev/null || die "no docker"
command -v systemctl >/dev/null || die "no systemd"
command -v openssl >/dev/null || die "no openssl"

install -d -o "$AEGIS_UID" -g "$AEGIS_UID" -m 0750 /srv/aegis/.workspace
install -d -m 0755 /etc/aegis /etc/aegis/modes
install -d -o "$AEGIS_UID" -g "$AEGIS_UID" -m 0700 /etc/aegis/secrets
if [ ! -e /etc/aegis/secrets/AEGIS_SESSION_KEY ]; then
  (umask 077; openssl rand -hex 32 > /etc/aegis/secrets/AEGIS_SESSION_KEY)
  chown "$AEGIS_UID:$AEGIS_UID" /etc/aegis/secrets/AEGIS_SESSION_KEY
fi
[ -e /etc/aegis/server.toml ] \
  || install -m 0644 "$HERE/../compose/server.toml.example" /etc/aegis/server.toml
if [ ! -e /etc/aegis/aegis.env ]; then
  sed "s/^DOCKER_GID=.*/DOCKER_GID=$(stat -c %g /var/run/docker.sock)/" \
    "$HERE/aegis.env.example" > /etc/aegis/aegis.env
fi
[ -e /etc/aegis/image.env ] || : > /etc/aegis/image.env
install -m 0644 "$HERE/aegis.service" /etc/systemd/system/aegis.service
install -m 0755 "$HERE/aegis-deploy" /usr/local/bin/aegis-deploy
systemctl daemon-reload
systemctl enable aegis

cat <<'NEXT'
Next (docs/runbook/on-premise.md):
  1. /etc/aegis/server.toml: public_url, [auth.oidc], modes. With the
     host network, PostgreSQL on this machine is 127.0.0.1.
  2. /etc/aegis/secrets: AEGIS_DATABASE_URL and the secrets of your setup,
     one file each (0400, owned by uid 10001).
  3. aegis-deploy <version>
NEXT
