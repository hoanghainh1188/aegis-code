#!/usr/bin/env python3
"""LSP under runc and runsc (#322): starts a profile's language server the
way ACI does (`docker run -i`, no network, caps dropped, the tree mounted
at /work), opens one file and asks for its symbols, and reports how long
`initialize` and `documentSymbol` took and what the server wrote to stderr
(inotify and file-watch errors show up there).

    DOCKER_HOST=tcp://127.0.0.1:23750 python3 scripts/gvisor/lsp_smoke.py \
        <runtime> <user> <image> <repo dir> <file in repo> <language id> -- <server argv…>

`user` is the `--user` to run as; on Docker Desktop runsc needs `0:0`
(see the report). Prints one JSON line.
"""

import json
import subprocess
import sys
import threading
import time
from pathlib import Path

TIMEOUT_S = 180


def frame(msg: dict) -> bytes:
    body = json.dumps(msg).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body


def read_frame(out) -> dict | None:
    length = None
    while True:
        line = out.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        key, _, value = line.decode().partition(":")
        if key.lower() == "content-length":
            length = int(value)
    return json.loads(out.read(length)) if length is not None else None


def main() -> None:
    sep = sys.argv.index("--")
    runtime, user, image, repo, file, lang = sys.argv[1:sep]
    server = sys.argv[sep + 1 :]
    repo = str(Path(repo).resolve())
    argv = [
        "docker", "run", "-i", "--rm", "--runtime", runtime, "--network", "none",
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--memory", "4g", "--user", user, "--volume", f"{repo}:/work",
        "--workdir", "/work", image, *server,
    ]
    started = time.monotonic()
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stderr: list[str] = []
    threading.Thread(
        target=lambda: stderr.extend(l.decode(errors="replace") for l in proc.stderr), daemon=True
    ).start()
    result = {"runtime": runtime, "image": image, "server": server[0], "file": file}

    def request(id_: int, method: str, params: dict) -> dict | None:
        proc.stdin.write(frame({"jsonrpc": "2.0", "id": id_, "method": method, "params": params}))
        proc.stdin.flush()
        deadline = time.monotonic() + TIMEOUT_S
        while time.monotonic() < deadline:
            msg = read_frame(proc.stdout)
            if msg is None:
                return None
            if msg.get("id") == id_ and "method" not in msg:
                return msg
            if "method" in msg and "id" in msg:  # server request: answer null
                proc.stdin.write(frame({"jsonrpc": "2.0", "id": msg["id"], "result": None}))
                proc.stdin.flush()
        return None

    def notify(method: str, params: dict) -> None:
        proc.stdin.write(frame({"jsonrpc": "2.0", "method": method, "params": params}))
        proc.stdin.flush()

    try:
        init = request(1, "initialize", {
            "processId": None, "rootUri": "file:///work",
            "workspaceFolders": [{"uri": "file:///work", "name": "work"}],
            "capabilities": {"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": True}}},
        })
        result["initialize_s"] = round(time.monotonic() - started, 2)
        result["initialize_ok"] = bool(init and "result" in init)
        notify("initialized", {})
        uri = f"file:///work/{file}"
        text = (Path(repo) / file).read_text()
        notify("textDocument/didOpen", {"textDocument": {"uri": uri, "languageId": lang, "version": 1, "text": text}})
        t = time.monotonic()
        syms = request(2, "textDocument/documentSymbol", {"textDocument": {"uri": uri}})
        result["symbols_s"] = round(time.monotonic() - t, 2)
        result["symbols"] = len((syms or {}).get("result") or [])
        result["symbols_error"] = (syms or {}).get("error")
        request(3, "shutdown", {})
        notify("exit", {})
    finally:
        try:
            proc.stdin.close()
            proc.wait(timeout=20)
        except (OSError, subprocess.TimeoutExpired):
            proc.kill()
    result["exit"] = proc.returncode
    noisy = [l.strip() for l in stderr if any(k in l.lower() for k in ("inotify", "watch", "permission", "error"))]
    result["stderr_flags"] = noisy[:5]
    print(json.dumps(result))


if __name__ == "__main__":
    main()
