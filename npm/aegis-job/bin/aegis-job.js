#!/usr/bin/env node
// Runs the aegis-job binary of this machine's platform, shipped in the
// package (vendor/<platform>-<arch>/aegis-job): no download at install,
// no package per platform. Arguments, stdio and exit code pass through.

"use strict";

const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const path = require("node:path");

const target = `${process.platform}-${process.arch}`;
const binary = path.join(__dirname, "..", "vendor", target, "aegis-job");

if (!existsSync(binary)) {
  console.error(
    `aegis-job: no binary for ${target} in this package; built for darwin-arm64, ` +
      "darwin-x64, linux-arm64, linux-x64. Other platforms: cargo install --locked " +
      "--git https://github.com/hoanghainh1188/aegis-code aegis-client"
  );
  process.exit(1);
}

const run = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (run.error) {
  console.error(`aegis-job: ${run.error.message}`);
  process.exit(1);
}
if (run.signal) {
  process.kill(process.pid, run.signal);
}
process.exit(run.status ?? 1);
