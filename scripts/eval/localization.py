#!/usr/bin/env python3
"""Shows, for the latest run of each eval issue, whether the agent found
and changed the files the upstream fix changed (`source_files`, #185): the
model call at which it first opened one of them, how many files it opened,
the source files it wrote (`edit`/`create_file` outside tests), and which
of them were not the fix's, or which of the fix's it left out.

    scripts/eval/localization.py [prefix ...]

A prefix (`typst`, `scrapy-76`) keeps the issues whose id starts with it.
Reads the ledger under `.workspace/state/ledger`; runs made before #129
have no tool events and are left out.
"""

import json
import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LEDGER = ROOT / ".workspace/state/ledger"
ISSUES = ROOT / "eval/issues"
RUN = re.compile(r"^(.+)-R(\d+)\.events\.jsonl$")
READS = {"open", "goto", "scroll_up", "scroll_down"}
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
    return dict(sorted(latest.items()))


def file_of(target):
    """The file a tool call's target names: `path:12-14`, `text in path`."""
    target = target.split(" in ")[-1] if " in " in target else target
    return re.sub(r":\d+(-\d+)?$", "", target).strip()


def is_test(path):
    return path.startswith("tests/") or "/tests/" in path or Path(path).name.startswith("test_")


def summary(path, fix):
    """First call that opened a file of `fix`, files opened, source files
    written, and the outcome; calls are Core/Expert model calls."""
    calls, first, opened, written = 0, None, set(), set()
    outcome, any_tools = "running", False
    for line in path.read_text().splitlines():
        event = json.loads(line)
        kind = event["event"]
        if kind == "model_call" and event["tier"] != "light":
            calls += 1
        elif kind == "tool_calls":
            any_tools = True
            for call in event["calls"]:
                file = file_of(call.get("target") or "")
                if call["name"] in READS:
                    opened.add(file)
                if call["name"] in WRITES and not call["is_error"] and not is_test(file):
                    written.add(file)
                touched = call["name"] in READS or call["name"] in WRITES
                if touched and file in fix and first is None:
                    first = calls
        elif kind == "finished":
            outcome = event["outcome"]
    return (first, opened, written, outcome) if any_tools else None


def cell(value):
    return "—" if value is None else str(value)


def main():
    print("| Run | Kết quả | File của bản sửa | Mở file đúng ở lượt | Số file đã mở | File nguồn đã sửa | Sửa thừa | Thiếu |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- |")
    totals = Counter()
    for issue, (n, path) in latest_runs(sys.argv[1:]).items():
        fix = set(tomllib.loads((ISSUES / f"{issue}.toml").read_text())["source_files"])
        found = summary(path, fix)
        if found is None:
            continue
        first, opened, written, outcome = found
        extra, missing = sorted(written - fix), sorted(fix - written)
        print(
            f"| {issue}-r{n} | {outcome} | {len(fix)} | {cell(first)} | {len(opened)} "
            f"| {len(written)} | {', '.join(extra) or '—'} | {len(missing) if written else '—'} |"
        )
        totals["runs"] += 1
        totals["found by call 4"] += first is not None and first <= 4
        totals["wrote only other files"] += bool(written) and not (written & fix)
        totals["wrote extra files"] += bool(extra)
        totals["left fix files out"] += bool(written & fix) and bool(missing)
        totals["wrote no source"] += not written
    print()
    print(", ".join(f"{k}: {v}" for k, v in totals.items()))


if __name__ == "__main__":
    main()
