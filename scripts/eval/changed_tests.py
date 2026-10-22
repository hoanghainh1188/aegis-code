#!/usr/bin/env python3
"""Finds, for each typst eval issue (#4), the existing tests whose reference
output the fix changes: an image in `tests/ref/<stage>/` or a line of a
`tests/ref/<stage>/hashes.txt`. They go in the issue file as
`changed_tests`; the dry run (#104) declares them as allowed test changes,
as an agent must, and the gate updates their references (#105).

    scripts/eval/changed_tests.py [id ...]

Tests the fix adds are not listed: they are its hidden tests.
"""

import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ISSUES = ROOT / "eval/issues"
CLONE = ROOT / ".workspace/repos/typst"


def show(rev, path):
    out = subprocess.run(["git", "-C", str(CLONE), "show", f"{rev}:{path}"], capture_output=True)
    return out.stdout if out.returncode == 0 else None


def hashed(text):
    """`<hash> <test name>` lines, by name."""
    lines = {}
    for line in text.decode().splitlines():
        parts = line.split()
        if len(parts) == 2:
            lines[parts[1]] = parts[0]
    return lines


def changed(issue):
    base, fix, hidden = issue["base_commit"], issue["fix_commit"], set(issue["hidden_tests"])
    names = set()
    for path in issue.get("test_files", []):
        if not path.startswith("tests/ref/"):
            continue
        old, new = show(base, path), show(fix, path)
        if old is None or new is None or old == new:
            continue
        if path.endswith("/hashes.txt"):
            before, after = hashed(old), hashed(new)
            names.update(n for n in before if n in after and before[n] != after[n])
        else:
            names.add(re.sub(r"\.[^.]+$", "", path.rsplit("/", 1)[-1]))
    return sorted(names - hidden)


def write(path, names):
    """Replaces the `changed_tests` of the file, before `[verified]`."""
    text = path.read_text()
    text = re.sub(r"(?ms)^# Existing tests whose reference.*?^changed_tests = \[.*?^\]\n", "", text)
    if not names:
        path.write_text(text)
        return
    block = (
        "# Existing tests whose reference output the fix changes (#105).\n"
        "changed_tests = [\n" + "".join(f"    {json.dumps(n)},\n" for n in names) + "]\n"
    )
    head, sep, tail = text.partition("\n[verified]")
    path.write_text(head.rstrip("\n") + "\n" + block + sep + tail)


def main(ids):
    for path in sorted(ISSUES.glob("typst-*.toml")):
        issue = tomllib.loads(path.read_text())
        if ids and issue["id"] not in ids:
            continue
        names = changed(issue)
        write(path, names)
        print(f"{issue['id']}\t{len(names)}")


if __name__ == "__main__":
    main(set(sys.argv[1:]))
