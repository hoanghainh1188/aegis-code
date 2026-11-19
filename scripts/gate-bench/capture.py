#!/usr/bin/env python3
"""Records the containers of one gate run and keeps a copy of its tree (#587).

Run it next to a replay; it stops when the replay's command ends:

    scripts/gate-bench/capture.py OUT -- aegis eval --config config/gin.toml \
        --set eval/issues --only gin-4851 --replay --no-pr

OUT/containers.jsonl gets `docker inspect` of every `aegis-gate-*` container
as it starts. OUT/tree is a copy-on-write clone of the job's tree, taken when
the first step without a network starts: the patch is applied and the fetch
step has filled the dependencies. `parallel.py` replays the steps from both.
"""

import json
import os
import subprocess
import sys
import threading


def clone(src, dst):
    flags = ["-c", "-R"] if sys.platform == "darwin" else ["-R", "--reflink=auto"]
    subprocess.run(["cp", *flags, src, dst], check=True)


def watch(out, stop):
    events = subprocess.Popen(
        ["docker", "events", "--filter", "event=start", "--format", "{{json .}}"],
        stdout=subprocess.PIPE,
        text=True,
    )
    tree = os.path.join(out, "tree")
    with open(os.path.join(out, "containers.jsonl"), "w") as log:
        for line in events.stdout:
            if stop.is_set():
                break
            event = json.loads(line)
            actor = event.get("Actor", {})
            name = actor.get("Attributes", {}).get("name", "")
            if not name.startswith("aegis-gate-"):
                continue
            shown = subprocess.run(
                ["docker", "inspect", actor["ID"]], capture_output=True, text=True
            )
            if shown.returncode != 0:
                # Gone already: a step shorter than this call.
                continue
            info = json.loads(shown.stdout)[0]
            log.write(json.dumps(info) + "\n")
            log.flush()
            host = info["HostConfig"]
            if host.get("NetworkMode") == "none" and not os.path.exists(tree):
                work = next(
                    (b.split(":")[0] for b in host.get("Binds") or [] if ":/work" in b),
                    None,
                )
                if work:
                    clone(work, tree)
    events.terminate()


def main():
    if "--" not in sys.argv or len(sys.argv) < 4:
        sys.exit(__doc__)
    split = sys.argv.index("--")
    out, command = sys.argv[1], sys.argv[split + 1 :]
    os.makedirs(out, exist_ok=True)
    stop = threading.Event()
    watcher = threading.Thread(target=watch, args=(out, stop), daemon=True)
    watcher.start()
    code = subprocess.run(command).returncode
    stop.set()
    # One more event unblocks the reader.
    subprocess.run(["docker", "run", "--rm", "--name", "aegis-bench-stop",
                    "--entrypoint", "true", "aegis-sandbox-python"],
                   capture_output=True)
    watcher.join(timeout=10)
    sys.exit(code)


if __name__ == "__main__":
    main()
