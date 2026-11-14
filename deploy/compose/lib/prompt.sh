# shellcheck shell=bash
# Asking for a site's values as a script runs (#476), sourced by
# install.sh and add-repo.sh.
#
# A value comes from its environment variable when that is set; else, on
# a terminal, from a question; else it stays empty. AEGIS_NONINTERACTIVE=1
# or no terminal on stdin: nothing is asked, as for unattended installs.
# Secrets are read without echo and never printed.

# Whether questions may be asked.
interactive() {
  [ "${AEGIS_NONINTERACTIVE:-0}" != 1 ] && [ -t 0 ]
}

# ask VAR "question" [default]: sets VAR from the environment, the answer
# or the default, in that order.
ask() {
  local var=$1 question=$2 default=${3:-} answer=
  if [ -n "${!var:-}" ]; then
    return 0
  fi
  if interactive; then
    if [ -n "$default" ]; then
      read -r -p "$question [$default]: " answer
    else
      read -r -p "$question (empty to skip): " answer
    fi
  fi
  printf -v "$var" '%s' "${answer:-$default}"
}

# ask_secret VAR "question": as ask, without echo and without a default.
ask_secret() {
  local var=$1 question=$2 answer=
  if [ -n "${!var:-}" ]; then
    return 0
  fi
  if interactive; then
    read -r -s -p "$question (hidden, empty to skip): " answer
    echo >&2
  fi
  printf -v "$var" '%s' "$answer"
}

# ask_valid VAR "question" CHECK [default]: as ask, until CHECK passes on
# the value or it is empty (skipped). An invalid value from the
# environment, or any without a terminal, fails: returns 1.
ask_valid() {
  local var=$1 question=$2 check=$3 default=${4:-}
  while :; do
    ask "$var" "$question" "$default"
    if [ -z "${!var}" ] || "$check" "${!var}"; then
      return 0
    fi
    echo "not valid: ${!var}" >&2
    interactive || return 1
    printf -v "$var" '%s' ""
  done
}

# An https:// address, no trailing slash, nothing a TOML string or sed
# would read as markup (quotes, backslash, `&`, `|`).
valid_url() {
  local re='^https://[^/[:space:]"\\&|]+(/[^[:space:]"\\&|]*)?$'
  [[ $1 =~ $re ]] && [[ $1 != */ ]]
}

# An OIDC client ID.
valid_client_id() {
  [[ $1 =~ ^[A-Za-z0-9._:-]+$ ]]
}

valid_email() {
  [[ $1 =~ ^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$ ]]
}

# A slug as the server takes it (aegis-store `check_slug`): ASCII letters,
# digits, `.`, `_`, `-`, starting with a letter or digit, at most 128 bytes.
valid_slug() {
  [[ $1 =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] && [ ${#1} -le 128 ]
}

# A secret's name in ./secrets for a slug: AEGIS_TOKEN_<SLUG>.
token_name() {
  printf 'AEGIS_TOKEN_%s' "$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]' | tr -c 'A-Z0-9' '_')"
}

# set_public_url FILE URL: the example's public_url replaced, once.
set_public_url() {
  local file=$1 url=$2
  sed -i.bak "s|^public_url = \"https://aegis.example.internal\"|public_url = \"$url\"|" "$file"
  rm -f "$file.bak"
}

# set_oidc FILE ISSUER CLIENT_ID: the example's commented [auth.oidc]
# made a real one.
set_oidc() {
  local file=$1 issuer=$2 client=$3
  sed -i.bak \
    -e "s|^# \[auth.oidc\]$|[auth.oidc]|" \
    -e "s|^# issuer = .*|issuer = \"$issuer\"|" \
    -e "s|^# client_id = .*|client_id = \"$client\"|" \
    "$file"
  rm -f "$file.bak"
}

# set_env FILE KEY VALUE: KEY=VALUE in a .env file, replaced or added.
set_env() {
  local file=$1 key=$2 value=$3
  if grep -q "^$key=" "$file"; then
    sed -i.bak "s|^$key=.*|$key=$value|" "$file"
    rm -f "$file.bak"
  else
    printf '%s=%s\n' "$key" "$value" >> "$file"
  fi
}
