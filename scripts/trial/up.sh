#!/usr/bin/env bash
# The local trial of the whole platform (docs/local-trial.md): PostgreSQL
# and a mock SSO in Docker, a fake GitHub, aegis-server with the sample Go
# repo registered. Usage: scripts/trial/up.sh [ollama|claude]
# Undo with scripts/trial/down.sh.
set -euo pipefail

MODEL=${1:-ollama}
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${AEGIS_TRIAL_DIR:-$REPO/.workspace/trial}
PG=aegis-trial-pg
SSO=aegis-trial-sso

say() { printf '\n==> %s\n' "$*"; }
die() { printf 'up.sh: %s\n' "$*" >&2; exit 1; }

case $MODEL in
  ollama | claude) ;;
  *) die "usage: up.sh [ollama|claude]" ;;
esac
for tool in docker cargo git python3 openssl curl; do
  command -v "$tool" >/dev/null || die "$tool is needed"
done
docker info >/dev/null 2>&1 || die "Docker is not running"
if [ "$MODEL" = ollama ]; then
  command -v ollama >/dev/null || die "ollama is needed (https://ollama.com), or run: up.sh claude"
  curl -sf http://localhost:11434/api/version >/dev/null || die "Ollama is not running: ollama serve"
elif [ -z "${ANTHROPIC_API_KEY:-}" ]; then
  die "ANTHROPIC_API_KEY is not set"
fi
[ -e "$WORK" ] && die "$WORK exists: scripts/trial/down.sh first"
for name in $PG $SSO; do
  docker container inspect "$name" >/dev/null 2>&1 && die "container $name exists: scripts/trial/down.sh first"
done
for port in 5433 8070 8080 8090 9464; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then die "port $port is taken"; fi
done

say "Building aegis-server and aegis-aci (release)"
cargo build --release --manifest-path "$REPO/Cargo.toml" -p aegis-server -p aegis-aci

say "Images: the Go sandbox, the egress proxy, PostgreSQL, the mock SSO"
docker image inspect aegis-sandbox-go >/dev/null 2>&1 || docker build -t aegis-sandbox-go "$REPO/docker/go"
docker pull -q python:3.12-alpine >/dev/null
docker pull -q postgres:17-alpine >/dev/null
docker pull -q ghcr.io/navikt/mock-oauth2-server:2.1.10 >/dev/null
if [ "$MODEL" = ollama ]; then
  say "Model: gpt-oss:20b with a 32k context, as aegis-gpt-oss"
  ollama pull gpt-oss:20b
  ollama create aegis-gpt-oss -f "$HERE/Modelfile"
fi

mkdir -p "$WORK/state" "$WORK/upstream"
fill() { sed -e "s#@WORK@#$WORK#g" -e "s#@REPO@#$REPO#g" "$1" > "$2"; }

say "Secrets (test values, in $WORK/env.sh)"
PG_PASSWORD=$(openssl rand -hex 16)
(
  umask 077
  cat > "$WORK/env.sh" <<ENV
export AEGIS_DATABASE_URL=postgres://postgres:$PG_PASSWORD@localhost:5433/postgres
export AEGIS_SESSION_KEY=$(openssl rand -hex 32)
export AEGIS_OIDC_CLIENT_SECRET=$(openssl rand -hex 16)
export AEGIS_GITHUB_WEBHOOK_SECRET=$(openssl rand -hex 32)
export AEGIS_GITHUB_COMMENT_TOKEN=trial-comment-token
export AEGIS_GITHUB_API=http://127.0.0.1:8070
export AEGIS_TRIAL_FORK_TOKEN=trial-fork-token
export RUST_LOG=info
ENV
)

say "PostgreSQL on :5433, mock SSO on :8090"
docker run -d --name $PG -e POSTGRES_PASSWORD="$PG_PASSWORD" -p 5433:5432 postgres:17-alpine >/dev/null
# Signs anyone in as dev@example.com, with the claims the server needs.
SSO_CONFIG='{"interactiveLogin":false,"tokenCallbacks":[{"issuerId":"default","requestMappings":[{"requestParam":"grant_type","match":"authorization_code","claims":{"sub":"dev@example.com","email":"dev@example.com","email_verified":true,"aud":["aegis"]}}]}]}'
docker run -d --name $SSO -e JSON_CONFIG="$SSO_CONFIG" -p 8090:8080 ghcr.io/navikt/mock-oauth2-server:2.1.10 >/dev/null
for _ in $(seq 60); do
  docker exec $PG pg_isready -q -U postgres 2>/dev/null && break
  sleep 1
done
docker exec $PG pg_isready -q -U postgres || die "PostgreSQL did not start"

say "The sample repo, its fork and the fake GitHub on :8070"
cp -R "$HERE/demo" "$WORK/upstream/demo"
git -C "$WORK/upstream/demo" init -q -b main
git -C "$WORK/upstream/demo" add -A
git -C "$WORK/upstream/demo" -c user.name=demo -c user.email=demo@example.com commit -q -m "textutil: Truncate"
git clone -q --bare "$WORK/upstream/demo" "$WORK/fork.git"
nohup python3 "$HERE/fake_github.py" "$WORK/fork.git" > "$WORK/fake_github.log" 2>&1 &
echo $! > "$WORK/fake_github.pid"

say "Configuration ($MODEL)"
cp "$HERE/server.toml" "$WORK/server.toml"
fill "$HERE/mode-$MODEL.toml" "$WORK/mode.toml"
fill "$HERE/demo.toml" "$WORK/demo.toml"
{
  cat "$REPO/config/pricing.toml"
  printf '\n# The trial model on Ollama: free.\n[models."aegis-gpt-oss"]\ninput = 0.0\noutput = 0.0\ncache_read_multiplier = 0.0\n'
} > "$WORK/pricing.toml"

say "aegis-server on :8080 (log: $WORK/server.log)"
BIN="$REPO/target/release/aegis-server"
(
  cd "$WORK"
  # shellcheck disable=SC1091
  . ./env.sh
  nohup "$BIN" --config server.toml --role all > server.log 2>&1 &
  echo $! > server.pid
)
for _ in $(seq 60); do
  curl -sf http://127.0.0.1:8080/api/v1/health >/dev/null && break
  sleep 1
done
curl -sf http://127.0.0.1:8080/api/v1/health >/dev/null || die "the server did not start: $WORK/server.log"

say "Registering the repo and the first admin"
(
  cd "$WORK"
  # shellcheck disable=SC1091
  . ./env.sh
  "$BIN" --config server.toml repo add demo.toml 2>/dev/null
  "$BIN" --config server.toml grant dev@example.com admin 2>/dev/null
  # A service token for the trial's scripts (send-review.sh).
  TOKEN=$("$BIN" --config server.toml token trial 2>/dev/null | head -1)
  "$BIN" --config server.toml grant trial admin 2>/dev/null
  echo "export AEGIS_TRIAL_API_TOKEN=$TOKEN" >> env.sh
)

cat <<DONE

Ready. Next:
  scripts/trial/send-webhook.sh                    # label issue #1: a job starts
  tail -f $WORK/server.log
  http://localhost:8080                            # dashboard, as dev@example.com
  http://127.0.0.1:8070/demo-org/demo/issues/1     # the issue and the job's comment
  http://127.0.0.1:8070/demo-org/demo/pull/100     # the PR, once the job opens it
  scripts/trial/send-review.sh 100 "<text>" [<file> <line> "<comment>"]   # a review round
Undo: scripts/trial/down.sh
DONE
