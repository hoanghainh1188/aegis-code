#!/usr/bin/env bash
# The offline bundle of one release (#263, docs/release.md) for the
# architecture of the Docker daemon (amd64 or arm64, #405): each image of
# the version saved to its own aegis-<name>-<version>-<arch>.tar.gz (a
# GitHub Release asset is at most 2 GB), its SHA-256, and
# versions-<arch>.txt (digests, last database migration). Run by
# release.yml on a machine of each architecture; works on any machine that
# can pull the images.
#
# Usage: scripts/release/save-images.sh <version> <registry/owner> <out dir> [name…]
# Names default to the server and every sandbox profile. The images of
# others that deploy/compose runs (PostgreSQL, and PostgreSQL with pgvector
# for semantic memory recall, #655) are saved too, as aegis-dep-*: an
# offline machine has no other way to get them; set AEGIS_EXTRA_IMAGES to
# change the list (`""` leaves them out; add `ollama/ollama:<tag>` for a
# local embedding model).
#
# For a customer's security review (#599), each image also gets an SPDX
# SBOM (<file>.spdx.json, by syft), and SHA256SUMS-<arch>.txt lists the
# SHA-256 of every file of the bundle. AEGIS_SBOM=1 (release.yml) fails
# without syft; unset, a missing syft only skips the SBOMs; 0 turns them
# off.
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
ARCH=$(docker version --format '{{.Server.Arch}}')
mkdir -p "$OUT"

{
  echo "aegis-code $VERSION"
  echo "arch $ARCH"
  last=$(find "$REPO/crates/aegis-store/migrations" -name '*.sql' -exec basename {} .sql \; | sort | tail -1)
  echo "migration $last"
} > "$OUT/versions-$ARCH.txt"

checksum() {
  if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

sbom() {
  local image=$1 out=$2
  [ "${AEGIS_SBOM:-}" = 0 ] && return 0
  if ! command -v syft >/dev/null; then
    [ "${AEGIS_SBOM:-}" = 1 ] && { echo "save-images.sh: AEGIS_SBOM=1 needs syft" >&2; exit 1; }
    echo "save-images.sh: no syft, no SBOM for $image" >&2
    return 0
  fi
  syft scan "docker:$image" --quiet -o "spdx-json=$out"
}

save() {
  local image=$1 file=$2
  docker pull --quiet --platform "linux/$ARCH" "$image" >/dev/null
  digest=$(docker image inspect --format '{{index .RepoDigests 0}}' "$image")
  echo "image $image ${digest#*@}" >> "$OUT/versions-$ARCH.txt"
  docker save "$image" | gzip -6 > "$OUT/$file"
  sbom "$image" "$OUT/$file.spdx.json"
  (cd "$OUT" && checksum "$file" > "$file.sha256")
  # The runner's disk holds one image at a time; one in use stays.
  docker image rm "$image" >/dev/null 2>&1 || true
  echo "saved $file"
}

for name in "${NAMES[@]}"; do
  save "$PREFIX/aegis-$name:$VERSION" "aegis-$name-$VERSION-$ARCH.tar.gz"
done
for image in ${AEGIS_EXTRA_IMAGES-postgres:17-alpine pgvector/pgvector:pg17}; do
  save "$image" "aegis-dep-$(echo "$image" | tr '/:' '--')-$ARCH.tar.gz"
done
cp "$REPO/scripts/release/load-images.sh" "$OUT/"
# Every file of this architecture's bundle, and the script that loads it.
(
  cd "$OUT"
  shopt -s nullglob
  files=(*-"$ARCH".* load-images.sh)
  checksum "${files[@]}" | grep -v " SHA256SUMS-$ARCH.txt\$" > "SHA256SUMS-$ARCH.txt.new"
  mv "SHA256SUMS-$ARCH.txt.new" "SHA256SUMS-$ARCH.txt"
)
echo "listed SHA256SUMS-$ARCH.txt"
