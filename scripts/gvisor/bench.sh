#!/usr/bin/env bash
# Replays eval jobs under runc and under runsc on the bench daemon (#322):
# the same job, the same caches, only `[run] sandbox_runtime` differs.
# `watch.py` records each container the job starts (fetch, formatter, gate
# steps, hidden tests) with its time, peak RAM and CPU.
#
#   scripts/gvisor/up.sh
#   scripts/gvisor/bench.sh config/gin.toml gin-4851 [reps]
#
# The first run of each job is a warm-up under runc (images, caches) and is
# labelled `warm`; then the runtimes alternate `reps` times (default 2).
# Results: .workspace/gvisor/containers.jsonl and runs.jsonl; per-run logs
# next to them.
#
# On Docker Desktop runsc cannot write the project's files (file sharing
# shows them as owned by the reader, the gofer reads as root) and the fetch
# step cannot resolve the egress proxy (#548). The #322 runs applied
# `docker-desktop.patch` (not for commit: it widens what ACI inherits) and set
#   AEGIS_BENCH_RUNSC_USER=0:0 AEGIS_BENCH_FETCH_RUNTIME=runc
# and rebuilt both `aegis` and `aegis-aci`. AEGIS_BENCH_RUNTIMES picks the
# runtimes (default `runc runsc`).
set -euo pipefail

config=${1:?run config, e.g. config/gin.toml}
issue=${2:?eval issue id, e.g. gin-4851}
reps=${3:-2}
runtimes=${AEGIS_BENCH_RUNTIMES:-runc runsc}

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
out="$root/.workspace/gvisor"
mkdir -p "$out"
export DOCKER_HOST=${DOCKER_HOST:-tcp://127.0.0.1:23750}
export PATH="$root/target/release:$root/.workspace/tools/cbm:$PATH"

# The watcher runs inside the bench container, against the host's daemon.
if ! DOCKER_HOST= docker exec aegis-gvisor-bench pgrep -f watch.py >/dev/null; then
  DOCKER_HOST= docker exec -d aegis-gvisor-bench \
    python3 "$root/scripts/gvisor/watch.py" "$out/containers.jsonl" "$out/label"
  sleep 1
fi

# `config` with `[run] sandbox_runtime = <runtime>`; paths in it stay
# relative to the project root, where aegis runs.
overlay() {
  local runtime=$1 file="$out/config-$(basename "$config" .toml)-$1.toml"
  awk -v rt="$runtime" '
    { print }
    /^\[run\]$/ { print "sandbox_runtime = \"" rt "\"" }
  ' "$config" | grep -v '^# *sandbox_runtime' > "$file"
  echo "$file"
}

run() {
  local runtime=$1 tag=$2 label="$issue $1 $2"
  local cfg log ledger before after start rc
  cfg=$(overlay "$runtime")
  log="$out/eval-$issue-$runtime-$tag.log"
  ledger=$(awk -F'"' '/^state_dir/ { print $2 }' "$config")/ledger-replay
  before=$(ls "$ledger" 2>/dev/null | grep -ci "^$issue-dry" || true)
  echo "$label" > "$out/label"
  echo "== $label"
  start=$(date +%s)
  set +e
  aegis eval --config "$cfg" --set eval/issues --only "$issue" --replay --no-pr > "$log" 2>&1
  rc=$?
  set -e
  local took=$(( $(date +%s) - start ))
  echo "" > "$out/label"
  # The run's ledger record: the newest of the issue's dry runs.
  local record
  record=$(ls -t "$ledger"/*.json 2>/dev/null | grep -i "/$issue-dry[0-9]*\.json$" | head -1 || true)
  after=$(ls "$ledger" 2>/dev/null | grep -ci "^$issue-dry" || true)
  python3 - "$record" "$label" "$rc" "$took" "$before" "$after" >> "$out/runs.jsonl" <<'EOF'
import json, sys
record, label, rc, took, before, after = sys.argv[1:]
row = {"label": label, "exit": int(rc), "wall_s": int(took)}
if record and int(after) > int(before):
    d = json.load(open(record))
    row.update(
        outcome=d.get("outcome"),
        hidden_tests_passed=d.get("hidden_tests_passed"),
        gate_s=[round(g["duration_s"], 1) for g in d.get("gate_runs", [])],
        gate_passed=[g["passed"] for g in d.get("gate_runs", [])],
        failed_step=[g.get("failed_step") for g in d.get("gate_runs", [])],
        job_wall_s=round(d.get("wall_time_s") or 0, 1),
        record=record.rsplit("/", 1)[-1],
    )
print(json.dumps(row))
EOF
  tail -1 "$out/runs.jsonl"
}

# AEGIS_BENCH_WARM=0 skips the warm-up when the caches are already filled.
if [ "${AEGIS_BENCH_WARM:-1}" != 0 ]; then
  run runc warm
fi
# Not `seq`: BSD `seq 0` counts down to 0.
for (( rep = 1; rep <= reps; rep++ )); do
  for runtime in $runtimes; do
    run "$runtime" "$rep"
  done
done
