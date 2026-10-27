#!/usr/bin/env python3
"""Writes eval/issues/<id>.toml for the picked candidates (#4).

    scripts/eval/build.py .workspace/eval/picked.json

Needs the clones of the target repos with the upstream history fetched
(`.workspace/repos/<name>`, remote `upstream`). The base of each issue is
the first parent of the fix's merge commit: the code as it was right before
the fix. Hidden tests are what the fix added: test names in typst's
`tests/suite/**/*.typ`, test files for pytest, the `Test…` functions the
fix adds or changes for Go, test classes for Java (`Class`) and C#
(`Namespace.Class`).
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
HUNK = re.compile(r"^@@ -\S+ \+(\d+)(?:,(\d+))? @@")
GO_FUNC = re.compile(r"^func\s+(?:\([^)]*\)\s+)?([A-Za-z_]\w*)\s*[\[(]")
CS_NAMESPACE = re.compile(r"^\s*namespace\s+([\w.]+)", re.MULTILINE)


def clone_path(repo):
    name = repo.split("/")[1]
    return CLONES.get(repo, ROOT / f".workspace/repos/{name}")


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True).stdout


def gh(args):
    out = subprocess.run(["gh", "api", *args], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def get_base_commit(clone, repo, fix_commit):
    if clone.exists():
        try:
            return git(clone, "rev-parse", f"{fix_commit}^1").strip()
        except Exception:
            pass
    commit_data = gh([f"repos/{repo}/commits/{fix_commit}"])
    return commit_data["parents"][0]["sha"]


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
        if clone.exists():
            names = []
            diff = git(clone, "diff", base, item["fix_commit"], "--", "tests/suite")
            for line in diff.splitlines():
                m = TYPST_HEADER.match(line)
                if m and m.group(1) not in names:
                    names.append(m.group(1))
            return names
        return item.get("hidden_tests", [])
    if lang == "go":
        return go_tests(item, clone, base)
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
        return csharp_tests(item, clone)
    return item["test_files"]


def changed_lines(clone, base, fix, path):
    """Lines of `path` at `fix` that the fix adds or changes, or that sit
    right after a removed block."""
    lines = []
    for line in git(clone, "diff", "-U0", base, fix, "--", path).splitlines():
        m = HUNK.match(line)
        if m:
            start, count = int(m.group(1)), int(m.group(2) or 1)
            lines.extend(range(start, start + count) if count else [max(start, 1)])
    return lines


def go_tests(item, clone, base):
    """`Test…` functions the fix adds or changes, for `go test -run`. A test
    file name would match no test, so the hidden tests would always pass
    (#219)."""
    names = []
    for path in item["test_files"]:
        source = git(clone, "show", f"{item['fix_commit']}:{path}").splitlines()
        funcs = [(n, m.group(1)) for n, text in enumerate(source, 1)
                 if (m := GO_FUNC.match(text))]
        for line in changed_lines(clone, base, item["fix_commit"], path):
            enclosing = [name for start, name in funcs if start <= line]
            if enclosing and enclosing[-1].startswith("Test") and enclosing[-1] not in names:
                names.append(enclosing[-1])
    if not names:
        sys.exit(f"{item['repo']}#{item['issue']}: the fix changes no Go Test function")
    return names


def csharp_tests(item, clone):
    """`Namespace.Class` of each test file, one class per file named like it,
    for `dotnet test --filter FullyQualifiedName~Namespace.Class.` (#219)."""
    names = []
    for path in item["test_files"]:
        source = git(clone, "show", f"{item['fix_commit']}:{path}")
        cls = path.rsplit("/", 1)[-1].removesuffix(".cs")
        namespace = CS_NAMESPACE.search(source)
        if not namespace or not re.search(rf"\bclass\s+{re.escape(cls)}\b", source):
            sys.exit(f"{item['repo']}#{item['issue']}: no namespace or class {cls} in {path}")
        names.append(f"{namespace.group(1)}.{cls}")
    return names


def main():
    raw = json.load(open(sys.argv[1]))
    if isinstance(raw, list):
        items_list = raw
    elif isinstance(raw, dict):
        items_list = [item for items in raw.values() for item in items]
    else:
        items_list = []

    out_dir = ROOT / "eval/issues"
    out_dir.mkdir(parents=True, exist_ok=True)
    for item in items_list:
        clone = clone_path(item["repo"])
        base = get_base_commit(clone, item["repo"], item["fix_commit"])
        issue = gh([f"repos/{item['repo']}/issues/{item['issue']}"])
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
