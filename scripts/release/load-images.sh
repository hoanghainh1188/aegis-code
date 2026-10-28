#!/usr/bin/env bash
# Loads the images of an offline bundle (#263, docs/release.md) into the
# local Docker daemon, each checked against its SHA-256 first. Run in the
# directory holding aegis-*.tar.gz and their .sha256 files, on a machine
# with no access to the registry. Sandbox images are also tagged with the
# bare name the profiles use (`aegis-sandbox-rust`).
#
# Usage: ./load-images.sh [dir]
set -euo pipefail

cd "${1:-.}"
shopt -s nullglob
bundles=(aegis-*.tar.gz)
if [ ${#bundles[@]} -eq 0 ]; then
  echo "load-images.sh: no aegis-*.tar.gz in $(pwd)" >&2
  exit 1
fi
for file in "${bundles[@]}"; do
  if command -v sha256sum >/dev/null; then
    sha256sum --check --quiet "$file.sha256"
  else
    shasum -a 256 --check --quiet "$file.sha256"
  fi
  loaded=$(gunzip -c "$file" | docker load | sed -n 's/^Loaded image: //p')
  echo "loaded $loaded"
  # The profiles name their sandbox by its bare name (profiles/*.toml),
  # as the AWS deployment tags it after a pull.
  case "$loaded" in
    */aegis-sandbox-*:*)
      bare=${loaded##*/}
      docker tag "$loaded" "${bare%%:*}"
      echo "tagged ${bare%%:*}"
      ;;
  esac
done
[ -f versions.txt ] && cat versions.txt
