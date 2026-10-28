#!/usr/bin/env python3
"""Would a run that makes no progress stop earlier, from JSON ledgers?

For each finished run (a `<ledger>.events.jsonl`), the Core/Expert calls in
order, with the tools each one asked for. A rule looks at the calls so far
and says "stop here"; the script replays every rule on every run and
prints, per rule:

- runs without a PR it stops, and the money spent after the stop;
- runs that opened a PR it would have stopped (lost PRs), and of those
  the ones whose PR passed the hidden tests (lost good PRs).

Progress is an applied `edit` / `create_file`. The rules:

- `no-edit@N`: no applied edit by call N;
- `stale@M`: M calls since the last applied edit (or since the start);
- `repeat@K`: the same tool on the same target asked K times.

    python3 scripts/eval/early_stop.py .workspace/state/ledger
"""

import argparse
import collections
import glob
import json
import os

WRITES = ("edit", "create_file")


def load(path):
    calls, outcome, good = [], None, False
    for line in open(path, encoding="utf-8"):
        e = json.loads(line)
        kind = e.get("event")
        if kind == "model_call":
            if e["tier"] in ("core", "expert"):
                calls.append({"cost": e["cost_usd"], "tools": []})
            elif calls:
                calls[-1]["cost"] += e["cost_usd"]
        elif kind == "tool_calls" and calls:
            calls[-1]["tools"] += e["calls"]
        elif kind == "hidden_tests":
            good = e["passed"]
        elif kind == "finished":
            outcome = e["outcome"]
    return calls, outcome, good


def no_edit(n):
    def rule(calls):
        for i, c in enumerate(calls):
            if any(t["name"] in WRITES and not t["is_error"] for t in c["tools"]):
                return None
            if i + 1 >= n:
                return i + 1
        return None
    return rule


def stale(m):
    def rule(calls):
        last = 0
        for i, c in enumerate(calls):
            if any(t["name"] in WRITES and not t["is_error"] for t in c["tools"]):
                last = i + 1
            elif i + 1 - last >= m:
                return i + 1
        return None
    return rule


def repeat(k):
    def rule(calls):
        seen = collections.Counter()
        for i, c in enumerate(calls):
            for t in c["tools"]:
                if t["name"] in WRITES:
                    continue
                seen[(t["name"], t.get("target"))] += 1
                if seen[(t["name"], t.get("target"))] >= k:
                    return i + 1
        return None
    return rule


RULES = {
    **{f"no-edit@{n}": no_edit(n) for n in (10, 15, 20, 25)},
    **{f"stale@{m}": stale(m) for m in (10, 15, 20)},
    **{f"repeat@{k}": repeat(k) for k in (3, 4, 5)},
}


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("ledgers", help="directory of <ledger>.events.jsonl files")
    args = parser.parse_args()
    runs = []
    for path in sorted(glob.glob(os.path.join(args.ledgers, "*.events.jsonl"))):
        calls, outcome, good = load(path)
        # Ledgers older than the `tool_calls` event cannot tell progress.
        if calls and outcome and any(c["tools"] for c in calls):
            runs.append((calls, outcome == "pr_opened", outcome == "pr_opened" and good))
    failed = [c for c, pr, _ in runs if not pr]
    goods = sum(1 for *_, g in runs if g)
    spent = sum(x["cost"] for c in failed for x in c)
    total = sum(x["cost"] for c, *_ in runs for x in c)
    print(f"runs {len(runs)}: {len(runs) - len(failed)} with a PR ({goods} passed the hidden "
          f"tests), {len(failed)} without")
    print(f"spent ${total:.2f}; ${spent:.2f} ({spent / total:.0%}) on runs without a PR")
    print(f"{'rule':12} {'stopped':>8} {'saved':>8} {'share':>6} {'lost PRs':>9} {'good':>6}")
    for name, rule in RULES.items():
        stopped, saved, lost, lost_good = 0, 0.0, 0, 0
        for calls, pr, good in runs:
            at = rule(calls)
            if at is None or at >= len(calls):
                continue
            if pr:
                lost += 1
                lost_good += good
            else:
                stopped += 1
                saved += sum(x["cost"] for x in calls[at:])
        print(f"{name:12} {stopped:>4}/{len(failed):<3} ${saved:>6.2f} {saved / spent:>5.0%} "
              f"{lost:>5}/{len(runs) - len(failed)} {lost_good:>3}/{goods}")


if __name__ == "__main__":
    main()
