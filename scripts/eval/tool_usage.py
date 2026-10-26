#!/usr/bin/env python3
"""Shows, for the latest run of each eval issue, how the agent spent its
model calls (#142): the tools it called (`tool_calls` events, #129), the
call of its first `edit`/`create_file`, of its first `run_tests` (#137) and
of its first gate run, the router's reminders to run a test (#144), its
escalation to Expert (`test_reminded` and `escalated` events, #148), the
files of the code-graph note before the first gate run (`impact_checked`,
#203), and how the run ended.

    scripts/eval/tool_usage.py [prefix ...]

A prefix (`typst`, `scrapy-76`) keeps the issues whose id starts with it.
Reads the ledger under `.workspace/state/ledger`; runs made before #129 have
no tool events and show `—`; runs made before #148 show no reminder or
escalation even when there was one.
"""

import json
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LEDGER = ROOT / ".workspace/state/ledger"
ISSUES = ROOT / "eval/issues"
RUN = re.compile(r"^(.+)-R(\d+)\.events\.jsonl$")
WRITES = {"edit", "create_file"}


def latest_runs(prefixes):
    """The events file of the highest `-r<n>` of each eval issue."""
    ids = {p.stem for p in ISSUES.glob("*.toml")}
    latest = {}
    for path in LEDGER.glob("*.events.jsonl"):
        m = RUN.match(path.name)
        if not m:
            continue
        issue, n = m[1].lower(), int(m[2])
        if issue not in ids or (prefixes and not issue.startswith(tuple(prefixes))):
            continue
        if issue not in latest or n > latest[issue][0]:
            latest[issue] = (n, path)
    return {issue: (n, path) for issue, (n, path) in sorted(latest.items())}


def summary(path):
    """Counts and firsts of one run; calls are Core/Expert model calls."""
    calls, tools = 0, Counter()
    first = {"write": None, "run_tests": None, "gate": None}
    outcome, any_tools = "running", False
    reminders, escalation, impact = [], None, None
    for line in path.read_text().splitlines():
        event = json.loads(line)
        kind = event["event"]
        if kind == "model_call" and event["tier"] != "light":
            calls += 1
        elif kind == "tool_calls":
            any_tools = True
            for call in event["calls"]:
                tools[call["name"]] += 1
                if call["name"] in WRITES and first["write"] is None:
                    first["write"] = calls
                if call["name"] == "run_tests" and first["run_tests"] is None:
                    first["run_tests"] = calls
        elif kind == "test_reminded":
            reminders.append(event["calls"])
        elif kind == "impact_checked":
            impact = "error" if event.get("error") else f"{calls}: {len(event['files'])} file"
        elif kind == "escalated":
            escalation = f"{event['calls']}: {event['reason']}"
        elif kind == "gate_run" and first["gate"] is None:
            first["gate"] = calls
        elif kind == "finished":
            outcome = event["outcome"]
    router = {"reminders": reminders, "escalation": escalation, "impact": impact}
    return calls, tools if any_tools else None, first, router, outcome


def cell(value):
    return "—" if value is None else str(value)


def main():
    rows = latest_runs(sys.argv[1:])
    print(
        "| Run | Kết quả | Lượt | `run_tests` | Sửa/tạo đầu | `run_tests` đầu | Gate đầu "
        "| Nhắc ở lượt | Leo thang | Đồ thị trước gate | Công cụ |"
    )
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    totals = Counter()
    for issue, (n, path) in rows.items():
        calls, tools, first, router, outcome = summary(path)
        top = "—" if tools is None else ", ".join(f"{k} {v}" for k, v in tools.most_common(4))
        runs = "—" if tools is None else str(tools["run_tests"])
        reminded = ", ".join(map(str, router["reminders"])) or "—"
        print(
            f"| {issue}-r{n} | {outcome} | {calls} | {runs} | {cell(first['write'])} "
            f"| {cell(first['run_tests'])} | {cell(first['gate'])} | {reminded} "
            f"| {cell(router['escalation'])} | {cell(router['impact'])} | {top} |"
        )
        totals[outcome] += 1
        totals["used run_tests"] += bool(tools and tools["run_tests"])
        totals["reminded"] += bool(router["reminders"])
        totals["escalated"] += router["escalation"] is not None
        totals["impact checked"] += router["impact"] is not None
    print()
    print(", ".join(f"{k}: {v}" for k, v in sorted(totals.items())))


if __name__ == "__main__":
    main()
