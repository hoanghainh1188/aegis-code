"""A stand-in for the GitHub REST API, enough for aegis jobs and review rounds,
and for GitLab's (under /api/v4) enough for a GitLab issue's job (#252).

GET  /repos/<o>/<r>/issues/<n>            the issue the job works on
POST /repos/<o>/<r>/pulls                 the PR the job opens
GET  /repos/<o>/<r>/pulls/<n>             the PR: state, head, base
POST /repos/<o>/<r>/pulls/<n>/reviews     a review (send-review.sh), as a reviewer
GET  /repos/<o>/<r>/pulls/<n>/reviews     its reviews
GET  /repos/<o>/<r>/pulls/<n>/comments    their line comments
POST /repos/<o>/<r>/issues/<n>/comments   a comment of the agent (issue or PR)
GET  /repos/<o>/<r>/issues/<n>/comments   comments on an issue or PR
GET  /repos/<o>/<r>/collaborators/<u>/permission   admin for the reviewer only
GET  /repos/<o>/<r>/actions/runs/77[/jobs], /actions/jobs/1/logs   a failed CI run
GET  /<o>/<r>/pull/<n>                    the PR as a page: body, diff, review, replies
GET  /<o>/<r>/issues/<n>                  the issue and its comments

GitLab: GET  /api/v4/projects/<path>/issues/<iid>, POST .../merge_requests,
POST .../issues/<iid>/notes; the MR as a page at /<path>/-/merge_requests/<iid>.

PRs are numbered from 100, so they never share a number with an issue.
"""

import html
import json
import subprocess
import sys
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT = 8070
ROOT = f"http://127.0.0.1:{PORT}"
REMOTE = Path(sys.argv[1])  # the bare repo jobs push to
FIRST_PR = 100
REVIEWER = {"login": "reviewer", "type": "User"}
AGENT = {"login": "aegis-bot", "type": "Bot"}
ISSUES = {
    1: {
        "title": "Truncate cuts multi-byte characters in half",
        "body": (
            "`textutil.Truncate(\"Xin chào thế giới\", 9)` returns invalid UTF-8: "
            "it slices bytes, not characters, so a Vietnamese or emoji "
            "character at the cut is split and shows as `�`.\n\n"
            "Expected: at most `max` characters (runes), the last one `…` "
            "when the text was cut, and always valid UTF-8."
        ),
    }
}
PULLS, COMMENTS, REVIEWS, LINE_COMMENTS = [], [], [], []
MERGE_REQUESTS, NOTES = [], []
CI_RUN = 77
CI_LOG = (
    "2026-10-28T09:00:01Z go test ./...\n"
    "2026-10-28T09:00:03Z --- FAIL: TestTruncateKeepsWholeRunes (0.00s)\n"
    "2026-10-28T09:00:03Z     textutil_test.go:21: Truncate(\"Xin chào thế giới\", 9) = "
    "\"Xin ch\\xc3…\", want valid UTF-8 with at most 9 runes\n"
    "2026-10-28T09:00:03Z FAIL\n"
    "2026-10-28T09:00:03Z FAIL\texample.com/demo/textutil\t0.004s\n"
    "2026-10-28T09:00:03Z ##[error]Process completed with exit code 1.\n"
)


def log(*parts):
    print(*parts, flush=True)


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def git(*args):
    return subprocess.run(["git", "-C", str(REMOTE), *args],
                          capture_output=True, text=True).stdout.strip()


def pull(number):
    index = number - FIRST_PR
    return PULLS[index] if 0 <= index < len(PULLS) else None


class Handler(BaseHTTPRequestHandler):
    def send(self, status, body, kind="application/json"):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def json(self, status, value):
        self.send(status, json.dumps(value))

    def not_found(self):
        self.json(404, {"message": "Not Found"})

    def read(self):
        length = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(length) or b"{}")

    def gitlab(self, method, rest, body=None):
        """/api/v4/projects/<url-encoded path>/..."""
        from urllib.parse import unquote
        if len(rest) < 3 or rest[0] != "projects":
            return self.not_found()
        project, tail = unquote(rest[1]), rest[2:]
        if method == "GET" and len(tail) == 2 and tail[0] == "issues":
            issue = ISSUES.get(int(tail[1]))
            if issue is None:
                return self.not_found()
            return self.json(200, {"iid": int(tail[1]), "title": issue["title"],
                                   "description": issue["body"],
                                   "web_url": f"{ROOT}/{project}/-/issues/{tail[1]}"})
        if method == "POST" and tail == ["merge_requests"]:
            MERGE_REQUESTS.append(body)
            iid = len(MERGE_REQUESTS)
            return self.json(201, {"iid": iid,
                                   "web_url": f"{ROOT}/{project}/-/merge_requests/{iid}"})
        if method == "POST" and len(tail) == 3 and tail[0] == "issues" and tail[2] == "notes":
            NOTES.append((int(tail[1]), body.get("body", "")))
            return self.json(201, {"id": len(NOTES)})
        return self.not_found()

    def do_GET(self):
        parts = self.path.split("?")[0].strip("/").split("/")
        log("GET", self.path)
        if parts[:2] == ["api", "v4"]:
            return self.gitlab("GET", parts[2:])
        if len(parts) >= 4 and parts[-2] == "merge_requests":
            return self.merge_request_page(int(parts[-1]))
        if parts[0] == "repos" and len(parts) >= 5:
            return self.api_get(parts[1], parts[2], parts[3:])
        if len(parts) == 4 and parts[2] == "pull":
            return self.pull_page(int(parts[3]))
        if len(parts) == 4 and parts[2] == "issues":
            return self.issue_page(int(parts[3]))
        self.not_found()

    def api_get(self, o, r, rest):
        if rest[0] == "collaborators" and rest[2:] == ["permission"]:
            # The reviewer may write; anyone else is not a collaborator.
            if rest[1] != REVIEWER["login"]:
                return self.not_found()
            return self.json(200, {"permission": "admin"})
        if rest[0] == "actions":
            return self.actions(o, r, rest[1:])
        kind, number = rest[0], int(rest[1])
        if kind == "issues" and rest[2:] == []:
            issue = ISSUES.get(number)
            if issue is None:
                return self.not_found()
            return self.json(200, {**issue, "number": number,
                                   "html_url": f"{ROOT}/{o}/{r}/issues/{number}"})
        if kind == "issues" and rest[2:] == ["comments"]:
            return self.json(200, [c for n, c in COMMENTS if n == number])
        pr = pull(number)
        if kind != "pulls" or pr is None:
            return self.not_found()
        if rest[2:] == []:
            return self.json(200, {
                "number": number, "html_url": f"{ROOT}/{o}/{r}/pull/{number}",
                "state": "open",
                "head": {"ref": pr["head"], "sha": git("rev-parse", f"refs/heads/{pr['head']}"),
                         "repo": {"full_name": f"{o}/{r}"}},
                "base": {"ref": pr["base"]},
            })
        if rest[2:] == ["reviews"]:
            return self.json(200, [v for n, v in REVIEWS if n == number])
        if rest[2:] == ["comments"]:
            return self.json(200, [c for n, c in LINE_COMMENTS if n == number])
        self.not_found()

    def actions(self, o, r, rest):
        """Run 77 of the CI workflow `test`, failed on `main` (send-ci-failure.sh)."""
        run_url = f"{ROOT}/{o}/{r}/actions/runs/{CI_RUN}"
        if rest == ["runs", str(CI_RUN)]:
            return self.json(200, {
                "id": CI_RUN, "name": "test", "html_url": run_url, "head_branch": "main",
                "head_sha": git("rev-parse", "refs/heads/main"), "conclusion": "failure"})
        if rest == ["runs", str(CI_RUN), "jobs"]:
            return self.json(200, {"jobs": [{
                "id": 1, "name": "go test", "html_url": f"{run_url}/job/1", "conclusion": "failure",
                "steps": [{"name": "checkout", "conclusion": "success"},
                          {"name": "go test ./...", "conclusion": "failure"}]}]})
        if rest == ["jobs", "1", "logs"]:
            return self.send(200, CI_LOG, "text/plain; charset=utf-8")
        self.not_found()

    def do_POST(self):
        parts = self.path.strip("/").split("/")
        body = self.read()
        log("POST", self.path, json.dumps(body)[:300])
        if parts[:2] == ["api", "v4"]:
            return self.gitlab("POST", parts[2:], body)
        o, r = parts[1], parts[2]
        if parts[3:] == ["pulls"]:
            PULLS.append(body)
            number = FIRST_PR + len(PULLS) - 1
            return self.json(201, {"number": number,
                                   "html_url": f"{ROOT}/{o}/{r}/pull/{number}"})
        if len(parts) == 6 and parts[3] == "pulls" and parts[5] == "reviews":
            number = int(parts[4])
            if pull(number) is None:
                return self.not_found()
            REVIEWS.append((number, {
                "user": REVIEWER, "author_association": "OWNER", "body": body.get("body", ""),
                "state": {"REQUEST_CHANGES": "CHANGES_REQUESTED"}.get(body.get("event"), "COMMENTED"),
                "submitted_at": now()}))
            for c in body.get("comments", []):
                LINE_COMMENTS.append((number, {
                    "user": REVIEWER, "author_association": "OWNER", "body": c.get("body", ""),
                    "path": c.get("path"), "line": c.get("line"), "created_at": now()}))
            return self.json(200, {"id": len(REVIEWS)})
        if len(parts) == 6 and parts[3] == "issues" and parts[5] == "comments":
            COMMENTS.append((int(parts[4]), {
                "user": AGENT, "author_association": "NONE", "body": body.get("body", ""),
                "created_at": now()}))
            n = len(COMMENTS)
            return self.json(201, {"id": n,
                                   "html_url": f"{ROOT}/{o}/{r}/issues/{parts[4]}#c{n}"})
        self.not_found()

    def page(self, title, inner):
        self.send(200, f"<!doctype html><meta charset=utf-8><title>{html.escape(title)}</title>"
                       "<style>body{font:15px system-ui;max-width:960px;margin:2em auto;padding:0 16px}"
                       "pre{background:#f6f8fa;padding:12px;overflow:auto;white-space:pre-wrap}"
                       ".add{color:#116329}.del{color:#82071e}</style>" + inner,
                  "text/html; charset=utf-8")

    def pull_page(self, number):
        pr = pull(number)
        if pr is None:
            return self.not_found()
        diff = git("diff", f"{pr['base']}...{pr['head']}")
        commits = git("log", "--format=%h %an: %s", f"{pr['base']}..{pr['head']}")
        lines = "\n".join(
            f"<span class={'add' if l.startswith('+') else 'del' if l.startswith('-') else ''}>"
            f"{html.escape(l)}</span>" for l in diff.splitlines())
        notes = "".join(
            f"<pre>{html.escape(v['state'])}: {html.escape(v['body'])}</pre>"
            for n, v in REVIEWS if n == number)
        notes += "".join(
            f"<pre>{html.escape(str(c['path']))}:{c['line']}: {html.escape(c['body'])}</pre>"
            for n, c in LINE_COMMENTS if n == number)
        replies = "".join(f"<pre>{html.escape(c['body'])}</pre>"
                          for n, c in COMMENTS if n == number)
        self.page(pr["title"], f"<h1>{html.escape(pr['title'])} #{number}</h1>"
                  f"<p><code>{html.escape(pr['head'])}</code> → <code>{html.escape(pr['base'])}</code>"
                  f"{' (draft)' if pr.get('draft') else ''}</p>"
                  f"<h2>Description</h2><pre>{html.escape(pr['body'])}</pre>"
                  f"<h2>Commits</h2><pre>{html.escape(commits)}</pre>"
                  f"<h2>Review</h2>{notes or '<p>none</p>'}"
                  f"<h2>Replies</h2>{replies or '<p>none</p>'}"
                  f"<h2>Diff</h2><pre>{lines}</pre>")

    def merge_request_page(self, iid):
        if not 0 < iid <= len(MERGE_REQUESTS):
            return self.not_found()
        mr = MERGE_REQUESTS[iid - 1]
        diff = git("diff", f"{mr['target_branch']}...{mr['source_branch']}")
        notes = "".join(f"<pre>{html.escape(b)}</pre>" for _, b in NOTES)
        self.page(mr["title"], f"<h1>{html.escape(mr['title'])} !{iid}</h1>"
                  f"<p><code>{html.escape(mr['source_branch'])}</code> → "
                  f"<code>{html.escape(mr['target_branch'])}</code></p>"
                  f"<h2>Description</h2><pre>{html.escape(mr['description'])}</pre>"
                  f"<h2>Diff</h2><pre>{html.escape(diff)}</pre>"
                  f"<h2>Notes on the issue</h2>{notes or '<p>none</p>'}")

    def issue_page(self, number):
        issue = ISSUES.get(number)
        if issue is None:
            return self.not_found()
        comments = "".join(f"<pre>{html.escape(c['body'])}</pre>"
                           for n, c in COMMENTS if n == number)
        self.page(issue["title"], f"<h1>{html.escape(issue['title'])} #{number}</h1>"
                  f"<pre>{html.escape(issue['body'])}</pre><h2>Comments</h2>{comments or '<p>none</p>'}")

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    log(f"fake GitHub on {ROOT}, pushes go to {REMOTE}")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
