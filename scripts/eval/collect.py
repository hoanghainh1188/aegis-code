#!/usr/bin/env python3
"""Candidates for the PoC eval set (#4): issues of a target repo closed as
completed by a merged PR in the last months, where the PR changes source
and tests, and nothing about dependencies or CI.

    scripts/eval/collect.py scrapy/scrapy python 2026-04-21 > candidates-scrapy.json

Reads GitHub through `gh api` (read only). Prints JSON, one object per
candidate, for a person to pick from.
"""

import json
import subprocess
import sys

QUERY = """
query($owner: String!, $name: String!, $since: DateTime!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(states: CLOSED, first: 50, after: $after, filterBy: {since: $since},
           orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title closedAt stateReason url
        labels(first: 10) { nodes { name } }
        timelineItems(itemTypes: [CLOSED_EVENT], last: 1) {
          nodes { ... on ClosedEvent { closer { __typename
            ... on PullRequest { number merged url baseRefName
              repository { nameWithOwner }
              mergeCommit { oid } } } } }
        }
      }
    }
  }
}
"""

# Files a patch must not need: dependencies, toolchain, CI.
BLOCKED = (
    "Cargo.lock", "Cargo.toml", "pyproject.toml", "setup.py", "setup.cfg",
    "requirements", "tox.ini", ".github/", "rust-toolchain", "uv.lock",
)
MAX_SOURCE_LINES = 300
MAX_FILES = 12


def gh(args, stdin=None):
    out = subprocess.run(["gh", "api", *args], input=stdin, capture_output=True,
                         text=True, check=True)
    return json.loads(out.stdout)


def is_test(path, language):
    if language == "python":
        name = path.rsplit("/", 1)[-1]
        return path.startswith("tests/") or name.startswith("test_") or "/tests/" in path
    return path.startswith("tests/") or "/tests/" in path


def is_source(path, language):
    ext = ".py" if language == "python" else ".rs"
    return path.endswith(ext) and not path.startswith(("docs/", "tests/")) and "/tests/" not in path


def pr_files(repo, number):
    files, page = [], 1
    while True:
        batch = gh([f"repos/{repo}/pulls/{number}/files?per_page=100&page={page}"])
        files += batch
        if len(batch) < 100:
            return files
        page += 1


def main():
    repo, language, since = sys.argv[1], sys.argv[2], sys.argv[3]
    owner, name = repo.split("/")
    after, out = None, []
    while True:
        args = ["graphql", "-f", f"query={QUERY}", "-f", f"owner={owner}",
                "-f", f"name={name}", "-f", f"since={since}T00:00:00Z"]
        if after:
            args += ["-f", f"after={after}"]
        issues = gh(args)["data"]["repository"]["issues"]
        for issue in issues["nodes"]:
            if issue["stateReason"] != "COMPLETED" or issue["closedAt"] < since:
                continue
            events = issue["timelineItems"]["nodes"]
            pr = events[0]["closer"] if events and events[0] else None
            if not pr or pr["__typename"] != "PullRequest" or not pr["merged"]:
                continue
            if pr["repository"]["nameWithOwner"] != repo or not pr["mergeCommit"]:
                continue
            files = pr_files(repo, pr["number"])
            paths = [f["filename"] for f in files]
            if any(b in p for p in paths for b in BLOCKED):
                continue
            tests = [p for p in paths if is_test(p, language)]
            sources = [p for p in paths if is_source(p, language)]
            source_lines = sum(f["additions"] + f["deletions"] for f in files
                               if f["filename"] in sources)
            if not tests or not sources or len(paths) > MAX_FILES or source_lines > MAX_SOURCE_LINES:
                continue
            out.append({
                "repo": repo, "language": language, "issue": issue["number"],
                "title": issue["title"], "url": issue["url"], "closed_at": issue["closedAt"],
                "labels": [l["name"] for l in issue["labels"]["nodes"]],
                "fix_pr": pr["number"], "fix_pr_url": pr["url"], "base_branch": pr["baseRefName"],
                "fix_commit": pr["mergeCommit"]["oid"],
                "source_files": sources, "test_files": tests, "source_lines": source_lines,
            })
        if not issues["pageInfo"]["hasNextPage"]:
            break
        after = issues["pageInfo"]["endCursor"]
    json.dump(out, sys.stdout, indent=1)
    print(f"{len(out)} candidates", file=sys.stderr)


if __name__ == "__main__":
    main()
