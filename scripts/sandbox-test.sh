#!/usr/bin/env bash
# Runs the profile's full test step on a target repo inside the sandbox
# image, with no network (#5, "Xong khi"). Dependencies are fetched first in
# a container that does have network, into the cache the gate mounts; #66
# replaces that fetch with one through a registry proxy.
#
#   scripts/sandbox-test.sh rust   .workspace/repos/typst
#   scripts/sandbox-test.sh python .workspace/repos/scrapy
#
# Caches go to .workspace/state/cache/<repo name>/ like the lifecycle's.
set -euo pipefail

profile=${1:?profile: rust or python}
repo=$(cd "${2:?repo path}" && pwd)
name=$(basename "$repo")
root=$(cd "$(dirname "$0")/.." && pwd)
cache="$root/.workspace/state/cache/$name"
image="aegis-sandbox-$profile"
user="$(id -u):$(id -g)"

docker image inspect "$image" >/dev/null 2>&1 || docker build -t "$image" "$root/docker/$profile"

common=(--rm --user "$user" --volume "$repo:/work" --workdir /work)

case "$profile" in
  rust)
    mkdir -p "$cache/usr_local_cargo_registry" "$cache/usr_local_cargo_git"
    mounts=(--volume "$cache/usr_local_cargo_registry:/usr/local/cargo/registry"
            --volume "$cache/usr_local_cargo_git:/usr/local/cargo/git")
    fetch=(env CARGO_NET_OFFLINE=false cargo fetch --locked)
    # The clone may hold a host build in target/; keep the container's apart.
    # Docker Desktop gives the VM 8 GB here; 12 parallel links of typst run out.
    test=(env CARGO_TARGET_DIR=/work/target/sandbox CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only cargo test --workspace --no-fail-fast)
    ;;
  python)
    mkdir -p "$cache/home_aegis_.cache_uv"
    mounts=(--volume "$cache/home_aegis_.cache_uv:/home/aegis/.cache/uv")
    # scrapy's test requirements (#2); other repos list theirs in RepoConfig.
    deps="attrs coverage pexpect psutil pyftpdlib pygments pytest pytest-cov pytest-rerunfailures pytest-timeout pytest-xdist sybil pytest-twisted"
    fetch=(sh -c "uv venv -q --allow-existing .venv && UV_OFFLINE=0 uv pip install -q --python .venv/bin/python -e . $deps")
    test=(python -m pytest -q -p no:cacheprovider -n auto)
    ;;
  *) echo "unknown profile $profile" >&2; exit 2 ;;
esac

echo "== fetch (network on)"
start=$(date +%s)
docker run "${common[@]}" "${mounts[@]}" "$image" "${fetch[@]}"
echo "fetch: $(( $(date +%s) - start ))s"

log="$root/.workspace/sandbox-$name-test.log"
echo "== test (--network none), full output in $log"
start=$(date +%s)
set +e
docker run "${common[@]}" "${mounts[@]}" --network none --cap-drop ALL \
  --security-opt no-new-privileges "$image" "${test[@]}" > "$log" 2>&1
rc=$?
tail -15 "$log"
echo "test: $(( $(date +%s) - start ))s, exit $rc"
exit "$rc"
