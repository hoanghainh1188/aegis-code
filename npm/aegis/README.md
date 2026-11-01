# `@hoanghainh/aegis` — Aegis Developer CLI

[![npm: @hoanghainh/aegis](https://img.shields.io/npm/v/@hoanghainh/aegis?style=flat-square&color=crimson&label=npm&logo=npm)](https://www.npmjs.com/package/@hoanghainh/aegis)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg?style=flat-square)](https://github.com/hoanghainh1188/aegis-code/blob/main/LICENSE)
[![Node: >=18](https://img.shields.io/badge/node-%3E%3D18-339933.svg?style=flat-square&logo=nodedotjs)](https://nodejs.org)
[![Platforms: macOS & Linux (arm64, x64)](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20(arm64%2C%20x64)-informational?style=flat-square)]()

Command-line interface for [**Aegis Platform (`aegis-code`)**](https://github.com/hoanghainh1188/aegis-code) — dispatch software maintenance issues, stream real-time verification progress, and manage AI-generated Pull Requests directly from your terminal.

The CLI communicates exclusively with the `aegis-server` REST API using your personal access token; agent execution, isolated Docker sandboxes, and model inference run entirely on the remote Aegis platform.

---

## Installation

```bash
npm install -g @hoanghainh/aegis
```

Pre-compiled native binaries for **macOS** (`arm64`, `x86_64`) and **Linux** (`arm64`, `x86_64`) are bundled directly inside the npm package — **zero runtime downloads** or `postinstall` network calls.

---

## Quick Start

### 1. Authenticate
Generate a personal token on your Aegis dashboard (`https://<server>/me/tokens`), then log in:

```bash
aegis login https://<server>
```

Paste your token when prompted (input is hidden). Credentials are saved to `~/.config/aegis/cli.toml` with `0600` permissions.

Verify your session and accessible repositories:

```bash
aegis whoami
aegis repos
```

> **CI/CD & Headless Environments:** Set `AEGIS_SERVER` and `AEGIS_TOKEN` environment variables instead of running `aegis login`.

### 2. Dispatch & Track an Issue

```bash
aegis submit <repo> <issue>        # e.g. aegis submit typst typst/typst#123
aegis watch <job>                  # Streams state changes; exits 0 when a PR is opened
aegis open <job>                   # Opens the generated PR or dashboard job page
```

---

## Command Reference

| Command | Description |
| :--- | :--- |
| `aegis login <server>` | Authenticate with a personal token from `<server>/me/tokens` |
| `aegis logout` | Remove saved credentials from `~/.config/aegis/cli.toml` |
| `aegis whoami` | Display current authenticated user and server |
| `aegis repos` | List repositories where your account holds a role |
| `aegis submit <repo> <issue>` | Dispatch a maintenance job (`owner/repo#12` for GitHub, `KEY-12` for Jira, or pass `--source gitlab` / `--source backlog`) |
| `aegis list [--repo <r>] [--status <s>] [--limit <n>]` | List jobs filtered by repository or status |
| `aegis show <job>` | Inspect job metadata, run history, and termination cause |
| `aegis watch <job> [--every <s>]` | Stream job progress until completion; exits `0` when a PR is opened |
| `aegis cancel <job>` | Cancel a queued or running job |
| `aegis retry <job>` | Re-run a failed or canceled job |
| `aegis review <job>` | Trigger an automated review round on the opened Pull Request |
| `aegis open <job>` | Open the Pull Request URL (or dashboard job page) in your default browser |

Append `--json` to any command for machine-readable JSON output suitable for scripts and CI pipelines.

---

## Exit Codes

| Code | Meaning |
| :---: | :--- |
| `0` | Success (for `watch`: job completed and opened a Pull Request) |
| `1` | `watch`: job finished without opening a PR (stopped by budget/steps, failed, or canceled) |
| `2` | Error: invalid CLI usage, unauthenticated, or server error |

---

## Documentation & Links

* **Full CLI Documentation:** [docs/client.md](https://github.com/hoanghainh1188/aegis-code/blob/main/docs/client.md)
* **Local Trial Walkthrough:** [docs/local-trial.md](https://github.com/hoanghainh1188/aegis-code/blob/main/docs/local-trial.md)
* **Aegis Platform Repository:** [github.com/hoanghainh1188/aegis-code](https://github.com/hoanghainh1188/aegis-code)
* **License:** [Apache-2.0](https://github.com/hoanghainh1188/aegis-code/blob/main/LICENSE)
