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
import re
import subprocess
import sys
import time
import tomllib
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / ".workspace/eval"
CLONES = {
    "scrapy/scrapy": ROOT / ".workspace/repos/scrapy",
    "typst/typst": ROOT / ".workspace/repos/typst",
    "colinhacks/zod": ROOT / ".workspace/repos/zod",
    "gin-gonic/gin": ROOT / ".workspace/repos/gin",
    "apache/commons-lang": ROOT / ".workspace/repos/commons-lang",
    "google/gson": ROOT / ".workspace/repos/gson",
    "FluentValidation/FluentValidation": ROOT / ".workspace/repos/FluentValidation",
    "restsharp/RestSharp": ROOT / ".workspace/repos/RestSharp",
}
SCRAPY_PYTHON = ROOT / ".workspace/measure/scrapy-venv/bin/python"
TIMEOUT = 1800
# Output that shows at least one test ran, for runners that exit 0 when
# their filter matches nothing.
RAN = {
    "go": re.compile(r"^--- (PASS|FAIL): ", re.MULTILINE),
    "csharp": re.compile(r"\b[Tt]otal(?: tests)?:\s*[1-9]"),
    "java": re.compile(r"Tests run: [1-9]"),
}


def clone_path(repo):
    name = repo.split("/")[1]
    return CLONES.get(repo, ROOT / f".workspace/repos/{name}")


def git(repo, *args, check=True):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=check)


def worktree(item):
    name = item["repo"].split("/")[1]
    tree = WORK / f"wt-{name}"
    if not tree.exists():
        git(clone_path(item["repo"]), "worktree", "add", "--detach", "--force", str(tree), item["base_commit"])
    return tree


def checkout(tree, commit):
    git(tree, "checkout", "-q", "--force", commit)
    git(tree, "clean", "-fdq")


def command(item, tree):
    lang = item["language"]
    if lang == "python":
        env = {**os.environ, "PYTHONPATH": str(tree)}
        return [str(SCRAPY_PYTHON), "-m", "pytest", "-q", "-p", "no:cacheprovider", *item["hidden_tests"]], env
    if lang == "rust":
        env = {**os.environ, "CARGO_TARGET_DIR": str(WORK / "target-typst")}
        pattern = "^(" + "|".join(item["hidden_tests"]) + ")$"
        return ["cargo", "test", "--workspace", "--test", "tests", "--", pattern], env
    if lang in ("typescript", "javascript"):
        env = {**os.environ}
        # Straight to vitest: `npm test` may go through a package manager
        # this machine lacks (zod's goes through nub).
        if any(tree.glob("vitest.config.*")):
            return ["npx", "--no-install", "vitest", "run", *item["hidden_tests"]], env
        return ["npm", "test", "--", *item["hidden_tests"]], env
    if lang == "go":
        env = {**os.environ}
        pattern = "^(" + "|".join(item["hidden_tests"]) + ")$" if item["hidden_tests"] else "."
        return ["go", "test", "-v", "-run", pattern, "./..."], env
    if lang == "java":
        env = {**os.environ}
        test_arg = ",".join(item["hidden_tests"])
        # Only the modules that hold the test files: others may not build
        # in the `test` phase (gson's test-jpms needs the installed jar).
        modules = sorted({f.split("/src/test/")[0] for f in item["test_files"] if "/src/test/" in f})
        return ["mvn", "-B", "test", f"-Dtest={test_arg}", "-Dsurefire.failIfNoSpecifiedTests=false",
                *(["-pl", ",".join(modules)] if modules else [])], env
    if lang == "csharp":
        # English output, for RAN.
        env = {**os.environ, "DOTNET_CLI_UI_LANGUAGE": "en"}
        # The trailing dot keeps `Foo.BarTests` from matching `Foo.BarTestsExtra`.
        filter_expr = "|".join(f"FullyQualifiedName~{t}." for t in item["hidden_tests"])
        # Test projects often target frameworks this machine cannot run
        # (net48 needs mono); pick the one to run, e.g. net8.0.
        framework = os.environ.get("EVAL_DOTNET_FRAMEWORK")
        # Repos moving to .slnx keep the .sln for a while; `dotnet test`
        # refuses a folder with both.
        solution = [p.name for p in sorted(tree.glob("*.sln")) or sorted(tree.glob("*.slnx"))][:1]
        return ["dotnet", "test", *solution, "--filter", filter_expr,
                *(["--framework", framework] if framework else [])], env
    raise ValueError(f"unknown language: {lang}")


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
    return rc, round(time.time() - started), text


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
    base_rc, base_s, _ = run(item, tree, "base")
    checkout(tree, item["fix_commit"])
    fix_rc, fix_s, fix_out = run(item, tree, "fix")
    ran = RAN.get(item["language"])
    if fix_rc == 0 and ran and not ran.search(fix_out):
        # `go test -run` and `dotnet test --filter` exit 0 when nothing
        # matches, so a wrong name would pass at both commits (#219).
        return {"ok": False, "base_rc": base_rc, "fix_rc": fix_rc, "base_s": base_s, "fix_s": fix_s,
                "why": "hidden tests ran no test at the fix"}
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
