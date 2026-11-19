#!/usr/bin/env python3
"""The gate's steps one after another against all at once (#587).

Replays what `capture.py` recorded: every step that ran without a network,
but the last one (the eval's hidden tests), each in a fresh container with
the flags, mounts and limits the gate gave it:

    scripts/gate-bench/parallel.py OUT [--reps 3] [--cpuset 0-3] [--shared-tree]

Each step gets its own copy-on-write clone of OUT/tree, so steps that write
into the tree (`coverage.out`, `target/`) cannot disturb each other;
`--shared-tree` gives them one tree instead. `--cpuset` pins every container
to those CPUs, to stand in for a smaller worker.

Appends one JSON line per round to OUT/parallel.jsonl: the wall time of each
mode, each step's own time and exit code, and the peak of the summed memory
and CPU of the running containers (`docker stats`, sampled every second).
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time

PREFIX = "aegis-bench-step"


def clone(src, dst):
    flags = ["-c", "-R"] if sys.platform == "darwin" else ["-R", "--reflink=auto"]
    subprocess.run(["cp", *flags, src, dst], check=True)


def steps_of(out):
    with open(os.path.join(out, "containers.jsonl")) as log:
        infos = [json.loads(line) for line in log]
    offline = [i for i in infos if i["HostConfig"].get("NetworkMode") == "none"]
    return offline[:-1]


def argv_of(info, name, tree, cpuset):
    host, config = info["HostConfig"], info["Config"]
    argv = ["docker", "run", "--name", name, "--network", "none"]
    for cap in host.get("CapDrop") or []:
        argv += ["--cap-drop", cap]
    for opt in host.get("SecurityOpt") or []:
        argv += ["--security-opt", opt]
    argv += [
        "--pids-limit", str(host["PidsLimit"]),
        "--memory", str(host["Memory"]),
        "--memory-swap", str(host["MemorySwap"]),
        "--cpus", str(host["NanoCpus"] / 1e9),
        "--user", config["User"],
    ]
    if cpuset:
        argv += ["--cpuset-cpus", cpuset]
    if host.get("ReadonlyRootfs"):
        argv.append("--read-only")
    for path, options in (host.get("Tmpfs") or {}).items():
        argv += ["--tmpfs", f"{path}:{options}"]
    for mount in host.get("Mounts") or []:
        if mount["Type"] == "volume":
            argv += ["--mount", f"type=volume,destination={mount['Target']}"]
    for bind in host.get("Binds") or []:
        source, rest = bind.split(":", 1)
        if rest.startswith("/work"):
            source = tree
        argv += ["--volume", f"{source}:{rest}"]
    argv += ["--workdir", config["WorkingDir"]]
    for pair in config.get("Env") or []:
        argv += ["--env", pair]
    return argv + [config["Image"], *config["Cmd"]]


def to_mb(text):
    units = {"B": 1 / 2**20, "KiB": 1 / 1024, "MiB": 1, "GiB": 1024}
    for unit in ("KiB", "MiB", "GiB", "B"):
        if text.endswith(unit):
            return float(text[: -len(unit)]) * units[unit]
    return 0.0


class Sampler(threading.Thread):
    """The peak of the containers' summed memory (MB) and CPU (cores)."""

    def __init__(self):
        super().__init__(daemon=True)
        self.done = threading.Event()
        self.peak_mb = 0.0
        self.peak_cpu = 0.0

    def run(self):
        while not self.done.is_set():
            shown = subprocess.run(
                ["docker", "stats", "--no-stream", "--format",
                 "{{.Name}} {{.CPUPerc}} {{.MemUsage}}"],
                capture_output=True, text=True,
            )
            mb = cpu = 0.0
            for line in shown.stdout.splitlines():
                parts = line.split()
                if len(parts) >= 3 and parts[0].startswith(PREFIX):
                    cpu += float(parts[1].rstrip("%")) / 100
                    mb += to_mb(parts[2])
            self.peak_mb, self.peak_cpu = max(self.peak_mb, mb), max(self.peak_cpu, cpu)


def run_step(info, name, tree, cpuset, results, index):
    start = time.monotonic()
    done = subprocess.run(argv_of(info, name, tree, cpuset), capture_output=True)
    results[index] = {
        "cmd": " ".join(info["Config"]["Cmd"])[:60],
        "seconds": round(time.monotonic() - start, 2),
        "exit": done.returncode,
    }
    subprocess.run(["docker", "rm", "--force", "--volumes", name], capture_output=True)


def round_of(mode, steps, out, cpuset, shared, tag):
    scratch = os.path.join(out, f"trees-{tag}")
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(scratch)
    trees = []
    for index in range(len(steps)):
        tree = os.path.join(scratch, "0" if shared else str(index))
        if not os.path.exists(tree):
            clone(os.path.join(out, "tree"), tree)
        trees.append(tree)
    results = [None] * len(steps)
    sampler = Sampler()
    sampler.start()
    start = time.monotonic()
    threads = []
    for index, info in enumerate(steps):
        name = f"{PREFIX}-{os.getpid()}-{tag}-{index}"
        thread = threading.Thread(
            target=run_step, args=(info, name, trees[index], cpuset, results, index)
        )
        thread.start()
        threads.append(thread)
        if mode == "sequential":
            thread.join()
    for thread in threads:
        thread.join()
    wall = time.monotonic() - start
    sampler.done.set()
    sampler.join()
    shutil.rmtree(scratch, ignore_errors=True)
    return {
        "mode": mode,
        "wall_s": round(wall, 2),
        "peak_mb": round(sampler.peak_mb),
        "peak_cpu": round(sampler.peak_cpu, 1),
        "steps": results,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out")
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--cpuset")
    parser.add_argument("--shared-tree", action="store_true")
    args = parser.parse_args()
    steps = steps_of(args.out)
    with open(os.path.join(args.out, "parallel.jsonl"), "a") as log:
        for rep in range(args.reps):
            for mode in ("sequential", "parallel"):
                row = round_of(mode, steps, args.out, args.cpuset, args.shared_tree,
                               f"{mode[:3]}{rep}")
                row.update(rep=rep, cpuset=args.cpuset, shared_tree=args.shared_tree)
                log.write(json.dumps(row) + "\n")
                log.flush()
                exits = [s["exit"] for s in row["steps"]]
                print(f"{mode:10} {row['wall_s']:8.1f}s  peak {row['peak_mb']} MB, "
                      f"{row['peak_cpu']} CPU  exits {exits}", flush=True)


if __name__ == "__main__":
    main()
