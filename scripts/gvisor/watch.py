#!/usr/bin/env python3
"""Records every container the bench daemon runs (#322): its name, image,
command, runtime and network, how long it ran, its exit code, its peak RAM
and its CPU time, one JSON line each. Runs inside the bench container
(`scripts/gvisor/up.sh`), next to the daemon, so it reads the containers'
cgroups directly:

    docker exec -d aegis-gvisor-bench \
        python3 $PWD/scripts/gvisor/watch.py $PWD/.workspace/gvisor/containers.jsonl \
        $PWD/.workspace/gvisor/label

The label file holds what the bench is running now (`gin-4851 runsc 2`);
each record takes the label read when its container started.

Peak RAM is the cgroup's `memory.peak`, read every 0.2 s while the
container runs (the cgroup goes away with it): under runsc it counts the
sentry and the gofer too, which is the cost being measured.
"""

import json
import os
import subprocess
import sys
import threading
import time

CGROUP = "/sys/fs/cgroup/docker"
PERIOD = 0.2


def read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def cpu_usec(cgroup):
    stat = read(os.path.join(cgroup, "cpu.stat")) or ""
    for line in stat.splitlines():
        key, _, value = line.partition(" ")
        if key == "usage_usec":
            return int(value)
    return None


def inspect(cid):
    out = subprocess.run(
        ["docker", "inspect", cid], capture_output=True, text=True, check=False
    )
    if out.returncode != 0:
        return {}
    info = json.loads(out.stdout)[0]
    return {
        "name": info["Name"].lstrip("/"),
        "image": info["Config"]["Image"],
        "cmd": info["Config"].get("Cmd") or [],
        "runtime": info["HostConfig"].get("Runtime") or "runc",
        "network": info["HostConfig"].get("NetworkMode"),
        "memory_limit": info["HostConfig"].get("Memory"),
    }


class Watch:
    def __init__(self, out, label_file):
        self.out = out
        self.label_file = label_file
        self.running = {}
        self.lock = threading.Lock()

    def label(self):
        return (read(self.label_file) or "").strip()

    def started(self, cid, at):
        record = {"id": cid[:12], "label": self.label(), "started": at}
        record.update(inspect(cid))
        with self.lock:
            self.running[cid] = {"record": record, "peak": 0, "cpu": None}

    def sample(self):
        while True:
            with self.lock:
                items = list(self.running.items())
            for cid, state in items:
                cgroup = os.path.join(CGROUP, cid)
                peak = read(os.path.join(cgroup, "memory.peak"))
                if peak:
                    state["peak"] = max(state["peak"], int(peak))
                cpu = cpu_usec(cgroup)
                if cpu is not None:
                    state["cpu"] = cpu
            time.sleep(PERIOD)

    def died(self, cid, at, exit_code):
        with self.lock:
            state = self.running.pop(cid, None)
        if state is None:
            return
        record = state["record"]
        record["seconds"] = round(at - record["started"], 3)
        record["exit"] = exit_code
        record["peak_mb"] = round(state["peak"] / 2**20, 1)
        if state["cpu"] is not None:
            record["cpu_s"] = round(state["cpu"] / 1e6, 2)
        with open(self.out, "a") as f:
            f.write(json.dumps(record) + "\n")

    def run(self):
        threading.Thread(target=self.sample, daemon=True).start()
        events = subprocess.Popen(
            [
                "docker",
                "events",
                "--filter",
                "type=container",
                "--filter",
                "event=start",
                "--filter",
                "event=die",
                "--format",
                "{{json .}}",
            ],
            stdout=subprocess.PIPE,
            text=True,
        )
        for line in events.stdout:
            event = json.loads(line)
            actor = event.get("Actor", {})
            # API 1.44+ dropped the top-level `id` and `status`.
            cid = event.get("id") or actor.get("ID")
            action = event.get("status") or event.get("Action")
            at = event["timeNano"] / 1e9
            if not cid:
                continue
            if action == "start":
                self.started(cid, at)
            elif action == "die":
                code = actor.get("Attributes", {}).get("exitCode")
                self.died(cid, at, int(code) if code is not None else None)


if __name__ == "__main__":
    Watch(sys.argv[1], sys.argv[2]).run()
