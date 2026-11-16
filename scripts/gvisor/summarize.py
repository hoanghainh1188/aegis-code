#!/usr/bin/env python3
"""Tables of the #322 bench (`bench.sh`, `watch.py`) in Markdown.

    python3 scripts/gvisor/summarize.py .workspace/gvisor

1. Per job: wall time of the whole replay, and of its gate runs, under
   each runtime (mean of the measured runs; the warm-up is left out), and
   whether the job still passed.
2. Per job and kind of container (fetch, gate steps, formatter, hidden
   tests…): total seconds, CPU seconds and the highest peak RAM in one run,
   runc against runsc.
"""

import json
import sys
from collections import defaultdict
from pathlib import Path
from statistics import mean

RUNTIMES = ("runc", "runsc")


def rows(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def kind(c: dict) -> str:
    """What a container was for, from its image and command."""
    cmd = c.get("cmd") or []
    text = " ".join(cmd)
    if c.get("network") not in (None, "none", "default"):
        if "egress" in text or "mitm" in text or "proxy" in text:
            return "egress proxy"
        return "fetch (network)"
    first = " ".join(cmd[:3])
    return first if len(first) <= 60 else first[:57] + "…"


def ratio(a: float, b: float) -> str:
    return f"×{b / a:.2f}" if a > 0 else "–"


def jobs_table(runs: list[dict]) -> list[str]:
    by = defaultdict(lambda: defaultdict(list))
    for r in runs:
        issue, runtime, tag = r["label"].split()
        if tag == "warm":
            continue
        by[issue][runtime].append(r)
    out = [
        "| Job | runc: job / gate (s) | runsc: job / gate (s) | Job ×runsc | Gate ×runsc | Passed under runsc |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for issue, per in by.items():
        cells, means = [], {}
        for rt in RUNTIMES:
            rs = per.get(rt, [])
            if not rs:
                cells.append("–")
                continue
            job = mean(r.get("job_wall_s") or r["wall_s"] for r in rs)
            gate = mean(sum(r.get("gate_s") or [0]) for r in rs)
            means[rt] = (job, gate)
            cells.append(f"{job:.0f} / {gate:.0f} (n={len(rs)})")
        passed = all(
            r.get("hidden_tests_passed") and all(r.get("gate_passed") or [False])
            for r in per.get("runsc", [])
        )
        if len(means) == 2:
            jr = ratio(means["runc"][0], means["runsc"][0])
            gr = ratio(means["runc"][1], means["runsc"][1])
        else:
            jr = gr = "–"
        out.append(
            f"| {issue} | {cells[0]} | {cells[1]} | {jr} | {gr} | {'yes' if passed else 'NO'} |"
        )
    return out


def containers_table(containers: list[dict]) -> list[str]:
    # (issue, kind) -> runtime -> run tag -> [seconds, cpu, peak]
    by = defaultdict(lambda: defaultdict(lambda: defaultdict(lambda: [0.0, 0.0, 0.0, 0])))
    failed = defaultdict(set)
    for c in containers:
        label = (c.get("label") or "").split()
        if len(label) != 3 or label[2] == "warm":
            continue
        issue, runtime, tag = label
        k = kind(c)
        cell = by[(issue, k)][runtime][tag]
        cell[0] += c.get("seconds") or 0
        cell[1] += c.get("cpu_s") or 0
        cell[2] = max(cell[2], c.get("peak_mb") or 0)
        cell[3] += 1
        if c.get("exit") not in (0, None):
            failed[(issue, k)].add(f"{runtime} exit {c['exit']}")
    out = [
        "| Job | Container | runc s / CPU s / peak MB | runsc s / CPU s / peak MB | Time ×runsc | Peak RAM Δ | Non-zero exits |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for (issue, k), per in sorted(by.items()):
        cells, means = [], {}
        for rt in RUNTIMES:
            runs = list(per.get(rt, {}).values())
            if not runs:
                cells.append("–")
                continue
            s = mean(r[0] for r in runs)
            cpu = mean(r[1] for r in runs)
            peak = max(r[2] for r in runs)
            n = runs[0][3]
            means[rt] = (s, peak)
            cells.append(f"{s:.1f} / {cpu:.1f} / {peak:.0f}" + (f" ({n}×)" if n > 1 else ""))
        if len(means) == 2:
            tr = ratio(means["runc"][0], means["runsc"][0])
            delta = f"{means['runsc'][1] - means['runc'][1]:+.0f} MB"
        else:
            tr = delta = "–"
        exits = ", ".join(sorted(failed.get((issue, k), []))) or ""
        out.append(f"| {issue} | `{k}` | {cells[0]} | {cells[1]} | {tr} | {delta} | {exits} |")
    return out


def main() -> None:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".workspace/gvisor")
    print("## Jobs\n")
    print("\n".join(jobs_table(rows(root / "runs.jsonl"))))
    print("\n## Containers\n")
    print("\n".join(containers_table(rows(root / "containers.jsonl"))))


if __name__ == "__main__":
    main()
