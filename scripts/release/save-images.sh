#!/usr/bin/env bash
# The offline bundle of one release (#263, docs/release.md): each image of
# the version saved to its own aegis-<name>-<version>.tar.gz (a GitHub
# Release asset is at most 2 GB), its SHA-256, and versions.txt (digests,
# last database migration). Run by release.yml; works on any machine that
# can pull the images.
#
# Usage: scripts/release/save-images.sh <version> <registry/owner> <out dir> [name…]
# Names default to the server and every sandbox profile.
set -euo pipefail

VERSION=$1
PREFIX=$2
OUT=$3
shift 3
NAMES=("$@")
if [ ${#NAMES[@]} -eq 0 ]; then
  NAMES=(server sandbox-python sandbox-rust sandbox-node sandbox-java sandbox-go sandbox-dotnet)
fi
REPO=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT"

{
  echo "aegis-code $VERSION"
  last=$(find "$REPO/crates/aegis-store/migrations" -name '*.sql' -exec basename {} .sql \; | sort | tail -1)
  echo "migration $last"
} > "$OUT/versions.txt"

for name in "${NAMES[@]}"; do
  image="$PREFIX/aegis-$name:$VERSION"
  file="aegis-$name-$VERSION.tar.gz"
  docker pull --quiet "$image" >/dev/null
  digest=$(docker image inspect --format '{{index .RepoDigests 0}}' "$image")
  echo "image $image ${digest#*@}" >> "$OUT/versions.txt"
  docker save "$image" | gzip -6 > "$OUT/$file"
  if command -v sha256sum >/dev/null; then
    (cd "$OUT" && sha256sum "$file" > "$file.sha256")
  else
    (cd "$OUT" && shasum -a 256 "$file" > "$file.sha256")
  fi
  # The runner's disk holds one image at a time.
  docker image rm "$image" >/dev/null
  echo "saved $file"
done
cp "$REPO/scripts/release/load-images.sh" "$OUT/"
