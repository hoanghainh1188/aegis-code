"""A stand-in for the GitHub REST API, enough for one aegis job.

GET  /repos/<o>/<r>/issues/<n>           the issue the job works on
POST /repos/<o>/<r>/pulls                the PR the job opens
POST /repos/<o>/<r>/issues/<n>/comments  the job's status comment
GET  /<o>/<r>/pull/<n>                   the PR as a page: body and diff
GET  /<o>/<r>/issues/<n>                 the issue and its comments
"""

import html
import json
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT = 8070
ROOT = f"http://127.0.0.1:{PORT}"
REMOTE = Path(sys.argv[1])  # the bare repo jobs push to
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
PULLS, COMMENTS = [], []


def log(*parts):
    print(*parts, flush=True)


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

    def read(self):
        length = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(length) or b"{}")

    def do_GET(self):
        parts = self.path.strip("/").split("/")
        log("GET", self.path)
        if len(parts) == 5 and parts[0] == "repos" and parts[3] == "issues":
            issue = ISSUES.get(int(parts[4]))
            if issue is None:
                return self.json(404, {"message": "Not Found"})
            o, r, n = parts[1], parts[2], parts[4]
            return self.json(200, {**issue, "number": int(n),
                                   "html_url": f"{ROOT}/{o}/{r}/issues/{n}"})
        if len(parts) == 4 and parts[2] == "pull":
            return self.pull_page(int(parts[3]))
        if len(parts) == 4 and parts[2] == "issues":
            return self.issue_page(int(parts[3]))
        self.json(404, {"message": "Not Found"})

    def do_POST(self):
        parts = self.path.strip("/").split("/")
        body = self.read()
        log("POST", self.path, json.dumps(body)[:300])
        o, r = parts[1], parts[2]
        if parts[3:] == ["pulls"]:
            number = len(PULLS) + 1
            PULLS.append(body)
            return self.json(201, {"number": number,
                                   "html_url": f"{ROOT}/{o}/{r}/pull/{number}"})
        if len(parts) == 6 and parts[3] == "issues" and parts[5] == "comments":
            COMMENTS.append((int(parts[4]), body.get("body", "")))
            n = len(COMMENTS)
            return self.json(201, {"id": n,
                                   "html_url": f"{ROOT}/{o}/{r}/issues/{parts[4]}#c{n}"})
        self.json(404, {"message": "Not Found"})

    def page(self, title, inner):
        self.send(200, f"<!doctype html><meta charset=utf-8><title>{html.escape(title)}</title>"
                       "<style>body{font:15px system-ui;max-width:960px;margin:2em auto;padding:0 16px}"
                       "pre{background:#f6f8fa;padding:12px;overflow:auto}"
                       ".add{color:#116329}.del{color:#82071e}</style>" + inner,
                  "text/html; charset=utf-8")

    def pull_page(self, number):
        if not 0 < number <= len(PULLS):
            return self.json(404, {"message": "Not Found"})
        pr = PULLS[number - 1]
        diff = subprocess.run(
            ["git", "-C", str(REMOTE), "diff", f"{pr['base']}...{pr['head']}"],
            capture_output=True, text=True).stdout
        lines = "\n".join(
            f"<span class={'add' if l.startswith('+') else 'del' if l.startswith('-') else ''}>"
            f"{html.escape(l)}</span>" for l in diff.splitlines())
        self.page(pr["title"], f"<h1>{html.escape(pr['title'])} #{number}</h1>"
                  f"<p><code>{html.escape(pr['head'])}</code> → <code>{html.escape(pr['base'])}</code>"
                  f"{' (draft)' if pr.get('draft') else ''}</p>"
                  f"<h2>Description</h2><pre>{html.escape(pr['body'])}</pre>"
                  f"<h2>Diff</h2><pre>{lines}</pre>")

    def issue_page(self, number):
        issue = ISSUES.get(number)
        if issue is None:
            return self.json(404, {"message": "Not Found"})
        comments = "".join(f"<pre>{html.escape(c)}</pre>" for n, c in COMMENTS if n == number)
        self.page(issue["title"], f"<h1>{html.escape(issue['title'])} #{number}</h1>"
                  f"<pre>{html.escape(issue['body'])}</pre><h2>Comments</h2>{comments or '<p>none</p>'}")

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    log(f"fake GitHub on {ROOT}, pushes go to {REMOTE}")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
