# Developer CLI: `aegis`

The command-line interface for software engineers to dispatch and track maintenance jobs directly from their terminal ([ADR 0006](adr/0006-developer-client.md)). The client communicates exclusively with the REST API of `aegis-server`; the agent, execution sandbox, and model inference run on the remote platform. All interactions require only a personal access token.

## Installation

> [!IMPORTANT]
> No release has been published yet, so the npm package and the prebuilt binaries below do not exist until the first `vX.Y.Z` release ([release.md](release.md)). Until then, install [from source](#from-source).

### Via npm (Recommended after the first release)
Supported on macOS (arm64/x86_64) and Linux (arm64/x86_64), requires Node 18+:

```bash
npm install -g @hoanghainh/aegis
```

The package bundles native pre-compiled binaries for all four architectures with zero runtime downloads. Versioning mirrors `aegis-server` (`vX.Y.Z`, [docs/release.md](release.md)).

### Prebuilt Binary
Download `aegis-<version>-<target>.tar.gz` from the repository Releases page, verify its SHA-256 checksum, and place it in your `PATH`:

```bash
shasum -a 256 -c aegis-<version>-<target>.tar.gz.sha256
tar -xzf aegis-<version>-<target>.tar.gz
install aegis-<version>-<target>/aegis ~/.local/bin/
```

### From Source
Requires Rust stable (1.85+):

```bash
cargo install --locked --git https://github.com/hoanghainh1188/aegis-code aegis-client
```

---

## Authentication

1. Open the Aegis dashboard (`https://<server>/`), log in via OIDC SSO, and navigate to **Tokens** (`/me/tokens`). Specify a friendly name (e.g. `laptop`), set an expiration period (up to 90 days), and click **Create Token**. Copy the token string immediately; it is presented only once.
2. Log in on your local machine:

   ```bash
   aegis login https://<server>
   ```

   Paste your token when prompted (input is hidden). Session credentials are saved securely in `~/.config/aegis/cli.toml` with restricted user permissions (`0600`).
3. Verify authentication:
   ```bash
   aegis whoami
   aegis repos
   ```

Tokens enforce RBAC rules assigned to your user account: submitting, canceling, or retrying jobs requires `maintainer` role on the target repository. In case of token compromise, revoke the token immediately via `/me/tokens`. For CI/CD environments or scripts, supply `AEGIS_SERVER` and `AEGIS_TOKEN` environment variables instead of interactive login.

---

## Command Reference

```bash
aegis submit <repo> <issue>        # e.g. aegis submit typst typst/typst#123
aegis watch <job>                  # Streams state changes; exits 0 when PR is opened
aegis show <job>                   # Displays job metadata, run history, and termination cause
aegis open <job>                   # Opens generated PR URL or job dashboard page in browser
aegis list --repo <repo> --status running
```

- `<issue>` format: `owner/repo#12` for GitHub issues, `KEY-12` for Jira tickets. Other sources require the explicit `--source` flag: `--source gitlab` (`group/project#12`), `--source backlog`.
- Action commands: `aegis cancel <job>`, `aegis retry <job>` (for failed or canceled jobs), `aegis review <job>` (triggers automated review round on the opened PR, #243).
- Structured Output: Append `--json` to any command for raw machine-readable JSON output suitable for automation scripts.

---

## Exit Codes

| Code | Semantics |
| :---: | --- |
| 0 | Success; for `watch`: the job completed and successfully opened a Pull Request |
| 1 | `watch`: the job completed without opening a PR (stopped by budget/steps, failed, or canceled) |
| 2 | Error: invalid syntax, unauthenticated, or server rejected request (with error details) |

---

## Local Walkthrough

When running the local trial ([docs/local-trial.md](local-trial.md)): once `up.sh` finishes, log in to the dashboard at <http://localhost:8080>, generate a personal token under **Tokens**, then authenticate and submit:

```bash
aegis login http://localhost:8080
aegis submit demo demo-org/demo#1
aegis watch 1
```
