#!/usr/bin/env python3
"""Writes eval/issues/<id>.toml for the picked candidates (#4).

    scripts/eval/build.py .workspace/eval/picked.json

Needs the clones of the target repos with the upstream history fetched
(`.workspace/repos/<name>`, remote `upstream`). The base of each issue is
the first parent of the fix's merge commit: the code as it was right before
the fix. Hidden tests are what the fix added: test names in typst's
`tests/suite/**/*.typ`, test files for pytest.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
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
TYPST_HEADER = re.compile(r"^\+--- ([A-Za-z0-9_-]+)")


def clone_path(repo):
    name = repo.split("/")[1]
    return CLONES.get(repo, ROOT / f".workspace/repos/{name}")


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True).stdout


def toml_str(value):
    return json.dumps(value, ensure_ascii=False)


def hidden_tests(item, clone, base):
    lang = item["language"]
    if lang == "python":
        # Test modules only; helpers (mock servers, base classes) are copied
        # with the test files but not run on their own.
        return [f for f in item["test_files"]
                if f.startswith("tests/") and f.rsplit("/", 1)[-1].startswith("test_") and f.endswith(".py")]
    if lang == "rust":
        names = []
        diff = git(clone, "diff", base, item["fix_commit"], "--", "tests/suite")
        for line in diff.splitlines():
            m = TYPST_HEADER.match(line)
            if m and m.group(1) not in names:
                names.append(m.group(1))
        return names
    if lang == "go":
        # Test function names added in test files
        diff = git(clone, "diff", base, item["fix_commit"], "--", *item["test_files"])
        names = []
        for line in diff.splitlines():
            if line.startswith("+func Test") or line.startswith("+func ("):
                m = re.search(r"func\s+(?:\([^)]+\)\s+)?(Test[A-Za-z0-9_]+)", line)
                if m and m.group(1) not in names:
                    names.append(m.group(1))
        return names if names else item["test_files"]
    if lang == "java":
        # Test class names (e.g. StringUtilsTrimTest for mvn test -Dtest=...)
        names = []
        for f in item["test_files"]:
            name = f.rsplit("/", 1)[-1]
            if name.endswith(".java"):
                names.append(name[:-5])
        return names if names else item["test_files"]
    if lang in ("typescript", "javascript"):
        return item["test_files"]
    if lang == "csharp":
        return item["test_files"]
    return item["test_files"]


def main():
    picked = json.load(open(sys.argv[1]))
    out_dir = ROOT / "eval/issues"
    out_dir.mkdir(parents=True, exist_ok=True)
    for items in picked.values():
        for item in items:
            clone = clone_path(item["repo"])
            base = git(clone, "rev-parse", f"{item['fix_commit']}^1").strip()
            issue = json.loads(subprocess.run(
                ["gh", "api", f"repos/{item['repo']}/issues/{item['issue']}"],
                capture_output=True, text=True, check=True).stdout)
            name = item["repo"].split("/")[1]
            ident = f"{name}-{item['issue']}"
            lines = [
                f"# {item['title']}",
                f"id = {toml_str(ident)}",
                f"language = {toml_str(item['language'])}",
                f"repo = {toml_str(item['repo'])}",
                f"issue = {item['issue']}",
                f"url = {toml_str(item['url'])}",
                f"title = {toml_str(item['title'])}",
                f"closed_at = {toml_str(item['closed_at'])}",
                f"labels = {toml_str(item['labels'])}",
                f"base_commit = {toml_str(base)}",
                f"fix_commit = {toml_str(item['fix_commit'])}",
                f"fix_pr = {item['fix_pr']}",
                f"source_files = {toml_str(item['source_files'])}",
                f"test_files = {toml_str(item['test_files'])}",
                f"hidden_tests = {toml_str(hidden_tests(item, clone, base))}",
                f"body = {toml_str(issue.get('body') or '')}",
                "",
            ]
            (out_dir / f"{ident}.toml").write_text("\n".join(lines))
            print(ident, base[:10], file=sys.stderr)


if __name__ == "__main__":
    main()
