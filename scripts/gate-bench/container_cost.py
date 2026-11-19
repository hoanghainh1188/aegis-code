#!/usr/bin/env python3
"""What one gate container costs besides its command (#587).

Runs `true` in a sandbox image with the flags `Sandbox::wrap` gives a gate
step (crates/aegis-gate/src/sandbox.rs), then reads its OOM flag and removes
it, as the gate does after every step, and times the three calls:

    scripts/gate-bench/container_cost.py aegis-sandbox-go aegis-sandbox-java ...

Prints one JSON line per image: median and p90 seconds of `docker run`,
`docker inspect` and `docker rm`, over REPS runs (default 20) after 2
warm-ups. `--writable DIR` adds the profile's writable volumes.
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time


def timed(argv):
    start = time.monotonic()
    done = subprocess.run(argv, capture_output=True, text=True, check=False)
    took = time.monotonic() - start
    if done.returncode != 0:
        sys.exit(f"{' '.join(argv)}: {done.stderr.strip()}")
    return took


def run_argv(name, image, work, writable):
    argv = [
        "docker", "run", "--name", name,
        "--network", "none",
        "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges",
        "--pids-limit", "4096",
        "--memory", "8192m", "--memory-swap", "8192m",
        "--cpus", "4",
        "--user", f"{os.getuid()}:{os.getgid()}",
        "--read-only",
        "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=2048m",
    ]
    for directory in writable:
        argv += ["--mount", f"type=volume,destination={directory}"]
    argv += ["--volume", f"{work}:/work", "--workdir", "/work", image, "true"]
    return argv


def percentile(values, share):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * share))]


def measure(image, reps, writable, work):
    phases = {"run": [], "inspect": [], "rm": []}
    for i in range(reps + 2):
        name = f"aegis-bench-{os.getpid()}-{i}"
        took = {
            "run": timed(run_argv(name, image, work, writable)),
            "inspect": timed(
                ["docker", "inspect", "--format", "{{.State.OOMKilled}}", name]
            ),
            "rm": timed(["docker", "rm", "--volumes", name]),
        }
        if i >= 2:
            for phase, seconds in took.items():
                phases[phase].append(seconds)
    row = {"image": image, "reps": reps}
    for phase, values in phases.items():
        row[f"{phase}_median_s"] = round(statistics.median(values), 3)
        row[f"{phase}_p90_s"] = round(percentile(values, 0.9), 3)
    totals = [sum(v) for v in zip(*phases.values())]
    row["total_median_s"] = round(statistics.median(totals), 3)
    row["total_p90_s"] = round(percentile(totals, 0.9), 3)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("images", nargs="+")
    parser.add_argument("--reps", type=int, default=20)
    parser.add_argument("--writable", action="append", default=["/home/aegis"])
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(dir=os.getcwd()) as work:
        for image in args.images:
            print(json.dumps(measure(image, args.reps, args.writable, work)), flush=True)


if __name__ == "__main__":
    main()
