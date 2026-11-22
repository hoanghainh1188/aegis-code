#!/usr/bin/env bash
# One job of the #587 trial: capture a replay's gate containers and tree,
# then run its steps one after another and all at once, on every CPU and
# pinned to four.
#
#   scripts/gate-bench/trial.sh config/gson.toml gson-992 [reps]
#
# Needs the sandbox images and a release build of `aegis` and `aegis-aci`.
# Results: .workspace/gate-parallel/<issue>/{containers,parallel}.jsonl.
set -euo pipefail
config=${1:?run config, e.g. config/gson.toml}
issue=${2:?eval issue id, e.g. gson-992}
reps=${3:-3}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
export PATH="$root/target/release:$root/.workspace/tools/cbm:$PATH"
out="$root/.workspace/gate-parallel/$issue"
bench="$root/scripts/gate-bench"
mkdir -p "$out"
if [ ! -d "$out/tree" ]; then
  "$bench/capture.py" "$out" -- aegis-local eval --config "$config" --set eval/issues \
    --only "$issue" --replay --no-pr > "$out/replay.log" 2>&1
  tail -1 "$out/replay.log"
fi
echo "== $issue, every CPU"
"$bench/parallel.py" "$out" --reps "$reps"
echo "== $issue, CPUs 0-3"
"$bench/parallel.py" "$out" --reps "$reps" --cpuset 0-3
