# ADR 0006 – Developer CLI: `aegis`

- Date: 2026-10-28
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #231

## Context

Developers interact with Aegis through issue trackers (Jira/Backlog), webhooks (labels like `aegis`, broken CI runs, PR/MR review comments), and the web dashboard. No dedicated lightweight client existed for developers' local terminals. The existing `aegis` binary (crate `aegis-cli`) is an internal operator and evaluation tool: it executes the full pipeline locally, requiring local Docker engines, model API keys, forge tokens, and configuration files, compiled directly from source.

While `aegis-server` `/api/v1` exposed comprehensive job operations (create, inspect, cancel, retry, review rounds), it initially authenticated only service tokens (`aegis-server token <service>`) lacking individual human user identities.

## Decision

1. **Authentication via Personal Access Tokens:**
   - Authenticated web dashboard users generate personal access tokens at `/me/tokens`: custom label, configurable expiration up to 90 days, capped at 10 active tokens per user.
   - Tokens are displayed exactly once upon generation and never leaked in URLs; the server stores cryptographic SHA-256 hashes.
   - Requests carrying personal tokens execute under that user's identity: honoring repo-specific RBAC roles and logging user identities in audit trails. Users can revoke individual tokens at any time.
   - OIDC device authorization flow is deferred: corporate IdP device grant support varies, and personal web tokens fulfill CLI needs cleanly.

2. **Dedicated Binary `aegis` in Crate `aegis-client`:**
   - Lightweight dependency tree: depends solely on HTTP (`reqwest`, `rustls`) and JSON parsing; excludes the agent loop, gate runner, Docker, or model providers. Compiles quickly and installs cleanly without runtime prerequisites.
   - Consumes `/api/v1` endpoints, introducing `GET /api/v1/me` and `GET /api/v1/repos` (reporting accessible repositories and assigned RBAC roles) to resolve slugs to IDs.
   - Commands: `login`, `logout`, `whoami`, `repos`, `submit`, `list`, `show`, `watch`, `cancel`, `retry`, `review`, `open`; `--json` flag for scripting.
   - Enforces HTTPS for remote transmissions (HTTP permitted strictly for local test loops). Sessions persist in `~/.config/aegis/cli.toml` with `0600` permissions; `AEGIS_SERVER` and `AEGIS_TOKEN` override for CI/CD scripting.
   - **Update (#336):** Renamed from `aegis-job` to `aegis` before the first client release, since the commands cover more than jobs (login, repos, review); environment variables `AEGIS_JOB_*` became `AEGIS_*` and the session file `job.toml` became `cli.toml`.

3. **Distribution Strategy:**
   - Compiled for macOS (arm64, x86_64) and Linux (arm64, x86_64) bundled into `tar.gz` with SHA-256 checksums attached to GitHub Releases.
   - Rust developers can install via `cargo install --locked --git https://github.com/hoanghainh1188/aegis-code aegis-client`.
   - **Update (#263):** Releases synchronize with server versions (`vX.Y.Z`, [docs/release.md](../release.md)). Pre-bundled npm package `@hoanghainh/aegis` embeds native binaries for four platforms with a lightweight JS launcher, requiring zero runtime compilation or downloads.

## Consequences

- Developers install a single lightweight binary, authenticate via dashboard tokens, and dispatch and monitor maintenance jobs from their terminal without operator privileges. Audit logs distinguish human users from automated services.
- Personal tokens are stored locally on developer workstations; risk is mitigated by expiration ceilings and granular user revocation.
- Dashboard requires `public_url` configuration to display accurate login instructions for `aegis login`.

## Considered Alternatives

| Alternative | Rationale for Rejection |
| --- | --- |
| Distribute `aegis-cli` to developers | Bloated with heavy dependencies (Docker daemon, Tree-sitter, SQLite); requires local secrets and direct repo access |
| OIDC Device Flow (`device_code`) | Complex IdP setup variations across enterprise customers; web token generation is universal and simple |
| Interactive Web Browser OAuth Callback | Requires opening local listening ports on developer workstations, frequently blocked by enterprise host firewalls |
