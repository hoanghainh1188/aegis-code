"""A stand-in for the GitHub REST API, enough for aegis jobs and review rounds.

GET  /repos/<o>/<r>/issues/<n>            the issue the job works on
POST /repos/<o>/<r>/pulls                 the PR the job opens
GET  /repos/<o>/<r>/pulls/<n>             the PR: state, head, base
POST /repos/<o>/<r>/pulls/<n>/reviews     a review (send-review.sh), as a reviewer
GET  /repos/<o>/<r>/pulls/<n>/reviews     its reviews
GET  /repos/<o>/<r>/pulls/<n>/comments    their line comments
POST /repos/<o>/<r>/issues/<n>/comments   a comment of the agent (issue or PR)
GET  /repos/<o>/<r>/issues/<n>/comments   comments on an issue or PR
GET  /<o>/<r>/pull/<n>                    the PR as a page: body, diff, review, replies
GET  /<o>/<r>/issues/<n>                  the issue and its comments

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

    def do_GET(self):
        parts = self.path.split("?")[0].strip("/").split("/")
        log("GET", self.path)
        if parts[0] == "repos" and len(parts) >= 5:
            return self.api_get(parts[1], parts[2], parts[3:])
        if len(parts) == 4 and parts[2] == "pull":
            return self.pull_page(int(parts[3]))
        if len(parts) == 4 and parts[2] == "issues":
            return self.issue_page(int(parts[3]))
        self.not_found()

    def api_get(self, o, r, rest):
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

    def do_POST(self):
        parts = self.path.strip("/").split("/")
        body = self.read()
        log("POST", self.path, json.dumps(body)[:300])
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
