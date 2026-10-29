# aegis-code

Autonomous code maintenance platform written 100% in Rust (Cargo workspace, `crates/`) that ingests maintenance issues and synthesizes verified, regression-free pull requests. Canonical references: [docs/design/00-proposal.md](docs/design/00-proposal.md) (takes precedence in case of discrepancy), [docs/design/01-architecture-detail.md](docs/design/01-architecture-detail.md), and [docs/adr/](docs/adr/).

---

## Issue-Driven Development

Every unit of work must trace back to a GitHub issue.

- **Branch Naming:** `<issue-number>-<short-description>`, lowercase, hyphen-separated. Example: `10-router-escalation`.
- **Pull Requests:** PR description must include `Closes #<issue-number>` so the issue closes automatically upon merge. A single PR must address exactly one issue.
- **Labels:** Every issue must have at least one `area/*` label indicating the affected crate or domain:
  - Crates: `area/provider`, `area/orchestrator`, `area/router`, `area/aci`, `area/gate`, `area/ledger`, `area/pr`, `area/sonar`, `area/lang` (language profiles, [ADR 0004](docs/adr/0004-multi-language-profiles.md))
  - Domains: `area/infra` (Docker, CI, policies, prompts), `area/eval` (benchmarks, KPIs)
  - Platform (Phase 2): `area/server`, `area/connectors`, `area/web`, `area/security`, `area/deploy`
  - Specialized: `research` for exploratory spikes/measurements; `tech-debt` for deferred review items.
- **Milestones:** `Phase 0 – Setup`, `Phase 1 – MVP / PoC`, `Phase 2 – Platform`, or `Phase 3 – Advanced Scaling` following Proposal §9 and [ADR 0002](docs/adr/0002-full-platform-mvp-as-checkpoint.md).
- **Dependencies:** Document explicit prerequisites at the bottom of the issue body as `**Dependencies:** #7, #9`. Never begin an issue until all dependencies are closed.
- Out-of-scope discoveries arising during development must be filed as separate issues rather than bundled into active PRs.

---

## Verification Before Opening a PR

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p aegis-provider --all-targets --features bedrock -- -D warnings
cargo test --workspace
cargo test -p aegis-provider --features bedrock
cargo deny check
```

Integration tests in `aegis-store` and end-to-end issue runner tests require a live PostgreSQL instance: set `AEGIS_TEST_DATABASE_URL` (e.g. `postgres://postgres:aegis@localhost:5432/postgres`), otherwise database tests will skip. When updating database queries or migrations in `aegis-store`, re-run `cargo sqlx prepare --workspace` against a migrated test database and commit the updated `.sqlx/` artifacts; CI enforces freshness via `--check`.

Commits must strictly adhere to the Conventional Commits specification (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, `ci:`).
