#!/usr/bin/env python3
"""How long the agent's context gets, from JSON ledgers (#35).

For each run (a `<ledger>.events.jsonl`), the prompt of every Core/Expert
call is input + cache read + cache writes. The script prints the peak
prompt per run, how many runs crossed a share of the context window (the
orchestrator collapses old observations at 60%), what each tool's result
adds to the next prompt, and where the money went by outcome.

    python3 scripts/eval/context_lengths.py .workspace/state/ledger [--window 200000]

Run it again after #156 (80 calls) or on a local model's ledgers
(`--window 32768`).
"""

import argparse
import collections
import glob
import json
import os
import statistics


def pct(values, p):
    values = sorted(values)
    return values[min(len(values) - 1, round(p / 100 * (len(values) - 1)))]


def prompt(tokens):
    return (tokens["input"] + tokens["cache_read"]
            + tokens["cache_write_5m"] + tokens["cache_write_1h"])


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("ledgers", help="directory of <ledger>.events.jsonl files")
    parser.add_argument("--window", type=int, default=200_000, help="context window in tokens")
    args = parser.parse_args()

    peaks, per_tool, spend = [], collections.defaultdict(list), collections.defaultdict(lambda: [0, 0.0, 0])
    for path in sorted(glob.glob(os.path.join(args.ledgers, "*.events.jsonl"))):
        events = [json.loads(line) for line in open(path, encoding="utf-8")]
        turns, cost, outcome = [], 0.0, None
        for e in events:
            if e.get("event") == "model_call":
                cost += e["cost_usd"]
                if e["tier"] in ("core", "expert"):
                    turns.append([prompt(e["tokens"]), e["tokens"]["output"], []])
            elif e.get("event") == "tool_calls" and turns:
                turns[-1][2] += [c["name"] for c in e["calls"]]
            elif e.get("event") == "finished":
                outcome = e["outcome"]
        if not turns or outcome is None:
            continue
        peaks.append(max(t[0] for t in turns))
        spend[outcome][0] += 1
        spend[outcome][1] += cost
        spend[outcome][2] += len(turns)
        # What one tool's result added: the next prompt minus this one and
        # its output. A collapse, a reminder or a new tier is not one turn.
        for a, b in zip(turns, turns[1:]):
            added = b[0] - a[0] - a[1]
            if 0 <= added <= 40_000 and len(a[2]) == 1:
                per_tool[a[2][0]].append(added)

    if not peaks:
        raise SystemExit("no finished run with Core/Expert calls")
    print(f"runs: {len(peaks)}  window: {args.window}")
    print(f"peak prompt per run: median {int(statistics.median(peaks))}, "
          f"p90 {pct(peaks, 90)}, p99 {pct(peaks, 99)}, max {max(peaks)} "
          f"({max(peaks) / args.window:.0%} of the window)")
    for share in (0.4, 0.5, 0.6):
        over = sum(1 for p in peaks if p >= share * args.window)
        print(f"  runs over {share:.0%}: {over}")
    total = sum(sum(v) for v in per_tool.values())
    print("tokens a tool result adds to the next prompt:")
    for name, added in sorted(per_tool.items(), key=lambda kv: -sum(kv[1]))[:8]:
        print(f"  {name:14} turns {len(added):5}  median {int(statistics.median(added)):6}  "
              f"share {sum(added) / total:4.0%}")
    all_cost = sum(s[1] for s in spend.values())
    print("cost by outcome:")
    for outcome, (runs, cost, calls) in sorted(spend.items(), key=lambda kv: -kv[1][1]):
        print(f"  {outcome:22} runs {runs:4}  ${cost:7.2f} ({cost / all_cost:4.0%})  "
              f"${cost / runs:.2f}/run  {calls / runs:.0f} calls/run")


if __name__ == "__main__":
    main()
