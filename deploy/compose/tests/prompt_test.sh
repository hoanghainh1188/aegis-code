#!/usr/bin/env bash
# Tests of lib/prompt.sh (#476): no root, no Docker, no terminal.
# Usage: deploy/compose/tests/prompt_test.sh
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=SCRIPTDIR/../lib/prompt.sh
. "$HERE/../lib/prompt.sh"

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
equal() { [ "$1" = "$2" ] || { echo "  expected [$2], got [$1]" >&2; return 1; }; }
not() { ! "$@"; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Without a terminal nothing is asked: the environment, else the default.
exec </dev/null
export AEGIS_NONINTERACTIVE=1
export FROM_ENV=set
ask FROM_ENV "unused" "default"
check "ask keeps a value from the environment" equal "$FROM_ENV" set
unset NO_ENV
ask NO_ENV "unused" "fallback"
check "ask takes the default" equal "$NO_ENV" fallback
unset EMPTY
ask EMPTY "unused"
check "ask leaves it empty without a default" equal "$EMPTY" ""
unset HIDDEN
ask_secret HIDDEN "unused"
check "ask_secret leaves it empty" equal "$HIDDEN" ""
export KEY=sk-test
ask_secret KEY "unused"
check "ask_secret keeps the environment's" equal "$KEY" sk-test
check "not interactive under AEGIS_NONINTERACTIVE" not interactive

export GOOD_URL=https://aegis.example.com
check "ask_valid takes a valid value" ask_valid GOOD_URL "unused" valid_url
export BAD_URL=http://aegis.example.com
check "ask_valid refuses an invalid one without a terminal" \
  not ask_valid BAD_URL "unused" valid_url
unset NO_URL
check "ask_valid lets an empty value skip" ask_valid NO_URL "unused" valid_url

check "url" valid_url "https://aegis.example.com"
check "url with a path" valid_url "https://sso.example.com/realms/dev"
check "url not http" not valid_url "http://aegis.example.com"
check "url no trailing slash" not valid_url "https://aegis.example.com/"
check "url no &" not valid_url "https://a.example.com/?x=1&y=2"
check "url no quote" not valid_url 'https://a.example.com/"x'
check "url no pipe" not valid_url "https://a.example.com/|x"
check "client id" valid_client_id "aegis"
check "client id no space" not valid_client_id "ae gis"
check "email" valid_email "dev@example.com"
check "email needs a domain" not valid_email "dev@localhost"
check "slug" valid_slug "my-repo.v2"
check "slug may be uppercase" valid_slug "Typst"
check "slug starts with a letter or digit" not valid_slug "-repo"
check "slug no slash" not valid_slug "a/b"
check "token name" equal "$(token_name my-repo.v2)" AEGIS_TOKEN_MY_REPO_V2

cp "$HERE/../server.toml.example" "$TMP/server.toml"
set_public_url "$TMP/server.toml" "https://aegis.acme.jp"
check "public_url replaced" grep -qx 'public_url = "https://aegis.acme.jp"' "$TMP/server.toml"
set_oidc "$TMP/server.toml" "https://sso.acme.jp/realms/dev" "aegis-pilot"
check "oidc table" grep -qx '\[auth.oidc\]' "$TMP/server.toml"
check "oidc issuer" grep -qx 'issuer = "https://sso.acme.jp/realms/dev"' "$TMP/server.toml"
check "oidc client" grep -qx 'client_id = "aegis-pilot"' "$TMP/server.toml"
if command -v python3 >/dev/null; then
  parses() {
    python3 -I -c 'import sys, tomllib
t = tomllib.load(open(sys.argv[1], "rb"))
assert t["auth"]["public_url"] == "https://aegis.acme.jp"
assert t["auth"]["oidc"]["client_id"] == "aegis-pilot"' "$1"
  }
  check "server.toml still parses" parses "$TMP/server.toml"
fi

printf 'A=1\nAEGIS_SANDBOXES="python go"\n' > "$TMP/.env"
set_env "$TMP/.env" AEGIS_SANDBOXES '"python rust"'
set_env "$TMP/.env" AEGIS_ADMIN_EMAIL dev@example.com
check "env replaced" grep -qx 'AEGIS_SANDBOXES="python rust"' "$TMP/.env"
check "env added" grep -qx 'AEGIS_ADMIN_EMAIL=dev@example.com' "$TMP/.env"
check "env kept" grep -qx 'A=1' "$TMP/.env"

[ "$failures" = 0 ] || { echo "$failures failed"; exit 1; }
echo "all passed"
