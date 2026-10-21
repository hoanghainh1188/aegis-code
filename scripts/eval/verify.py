#!/usr/bin/env python3
"""Checks that each eval issue (#4) can be replayed: on its base commit,
with only the fix's test files copied in, the hidden tests fail; on the fix
commit they pass. Results go to `.workspace/eval/verify.json`; passing
issues get a `[verified]` table in their file.

    scripts/eval/verify.py [id ...]

Runs the target repos' own tests on this machine (upstream code, not agent
patches), in one worktree per repo under `.workspace/eval/`.
"""

import json
import os
import subprocess
import sys
import time
import tomllib
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / ".workspace/eval"
CLONES = {"scrapy/scrapy": ROOT / ".workspace/repos/scrapy", "typst/typst": ROOT / ".workspace/repos/typst"}
SCRAPY_PYTHON = ROOT / ".workspace/measure/scrapy-venv/bin/python"
TIMEOUT = 1800


def git(repo, *args, check=True):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=check)


def worktree(item):
    name = item["repo"].split("/")[1]
    tree = WORK / f"wt-{name}"
    if not tree.exists():
        git(CLONES[item["repo"]], "worktree", "add", "--detach", "--force", str(tree), item["base_commit"])
    return tree


def checkout(tree, commit):
    git(tree, "checkout", "-q", "--force", commit)
    git(tree, "clean", "-fdq")


def command(item, tree):
    if item["language"] == "python":
        env = {**os.environ, "PYTHONPATH": str(tree)}
        return [str(SCRAPY_PYTHON), "-m", "pytest", "-q", "-p", "no:cacheprovider", *item["hidden_tests"]], env
    env = {**os.environ, "CARGO_TARGET_DIR": str(WORK / "target-typst")}
    pattern = "^(" + "|".join(item["hidden_tests"]) + ")$"
    return ["cargo", "test", "--workspace", "--test", "tests", "--", pattern], env


def run(item, tree, label):
    argv, env = command(item, tree)
    started = time.time()
    try:
        out = subprocess.run(argv, cwd=tree, env=env, capture_output=True, text=True, timeout=TIMEOUT)
        rc, text = out.returncode, out.stdout + out.stderr
    except subprocess.TimeoutExpired:
        rc, text = None, "timed out"
    (WORK / "logs").mkdir(exist_ok=True)
    (WORK / "logs" / f"{item['id']}-{label}.log").write_text(text)
    return rc, round(time.time() - started)


def verify(item):
    tree = worktree(item)
    changed = git(tree, "diff", "--name-only", item["base_commit"], item["fix_commit"]).stdout.split()
    missing = [t for t in item["test_files"] if t not in changed]
    if missing:
        return {"ok": False, "why": f"test files not in base..fix: {missing}"}
    checkout(tree, item["base_commit"])
    for path in item["test_files"]:
        # As bytes: typst's reference images are binary. A file the fix
        # deletes does not exist there and is skipped.
        shown = subprocess.run(["git", "-C", str(tree), "show", f"{item['fix_commit']}:{path}"],
                               capture_output=True)
        if shown.returncode == 0:
            target = tree / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(shown.stdout)
    base_rc, base_s = run(item, tree, "base")
    checkout(tree, item["fix_commit"])
    fix_rc, fix_s = run(item, tree, "fix")
    # pytest: 1 = tests failed, 2 = a test module does not import (it uses
    # what the fix adds); 3-5 are usage problems or no tests at all.
    base_failed = base_rc not in (0, None) and (item["language"] != "python" or base_rc in (1, 2))
    ok = base_failed and fix_rc == 0
    return {"ok": ok, "base_rc": base_rc, "fix_rc": fix_rc, "base_s": base_s, "fix_s": fix_s,
            "why": None if ok else "hidden tests do not go from fail to pass"}


def main():
    wanted = set(sys.argv[1:])
    results_path = WORK / "verify.json"
    results = json.loads(results_path.read_text()) if results_path.exists() else {}
    for path in sorted((ROOT / "eval/issues").glob("*.toml")):
        item = tomllib.loads(path.read_text())
        if wanted and item["id"] not in wanted:
            continue
        result = verify(item)
        results[item["id"]] = result
        results_path.write_text(json.dumps(results, indent=1))
        print(item["id"], result, file=sys.stderr, flush=True)
        text = path.read_text().split("\n[verified]")[0].rstrip("\n") + "\n"
        if result["ok"]:
            text += (f"\n[verified]\nbase = \"fail\"\nfix = \"pass\"\non = \"{date.today()}\"\n"
                     f"seconds = {result['base_s'] + result['fix_s']}\n")
        path.write_text(text)


if __name__ == "__main__":
    main()
