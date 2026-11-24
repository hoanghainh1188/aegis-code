# Aegis Documentation Hub

Documentation is organized by audience so **customer/operator** guides stay cleanly separated from **internal engineering and evaluation** records.

---

## 1. Customer & Operator Documentation (English)

For platform operators, security reviewers, developers, and PR reviewers deploying and running Aegis.

### Getting Started & Day-One Installation
| Guide | Purpose |
| :--- | :--- |
| [Local 30-Minute Trial](local-trial.md) | End-to-end local sandbox (PostgreSQL, mock GitHub, mock OIDC, Ollama or Claude). |
| [On-Premise Install Guide](install-on-premise.md) | Step-by-step first install on a blank Linux machine (Docker Compose, SSO, GitHub App/bot, first PR). |

### Configuration, Security & Governance
| Guide | Purpose |
| :--- | :--- |
| [Configuration Reference](configuration.md) | Single reference for `server.toml` (`[web] default_locale`), mode TOML (`[provider]`, `[residency]`, `[memory.embedding]`), language profiles (including `kotlin`), and `repos.config` (`[settings]`). |
| [REST API & Webhook Reference](api.md) | `/api/v1/*` endpoints (jobs, memories with `repo`/`tenant` scope) and `/webhooks/*` ingress (GitHub, GitLab, Bitbucket Cloud, Bitbucket Data Center, Azure DevOps). |
| [Security & Compliance Overview](security.md) | Data flow, data residency (chat & embedding), sandbox & `aegis-docker-proxy` isolation, prompt injection defense, RBAC, offline bundle SBOMs/checksums, and auditability. |
| [Vulnerability Reporting Policy](../SECURITY.md) | Supported versions and private security reporting process. |

### Day-Two Operations Runbooks
| Runbook | Purpose |
| :--- | :--- |
| [On-Premise Operations](runbook/on-premise.md) | Upgrades, rollback, backups, unattended installs, [Self-Hosted presets](../deploy/presets/README.md), `pgvector` setup, web pre-flight onboarding, and systemd deployment. |
| [AWS Production Runbook](runbook/README.md) | EC2/RDS/Secrets Manager deployment, secret rotation, and incident triage ([OpenTofu IaC](../deploy/tofu/README.md)). |
| [Kubernetes Runbook](runbook/kubernetes.md) | Helm chart deployment with DinD + `aegis-docker-proxy` sidecars and `pgvector` configuration. |
| [Adding Repositories](runbook/repos.md) | Registering repos (dashboard pre-flight wizard or CLI), polyglot monorepos (`[[repo.dirs]]`), roles, budgets, webhooks (GitHub, GitLab, Bitbucket, Azure DevOps), GitHub App, sidecars, and secret allow-lists. |
| [Tenants (Contracts)](runbook/tenants.md) | Multi-contract isolation (roles, budgets, secret prefixes, disk paths) and the `/ams` MTTR/SLA report. |
| [Incident Runbook: Pausing Every Agent](runbook/incident.md) | Global kill-switch (`intake` / `emergency`) via UI, API, or CLI. |
| [Release Operations](release.md) | Versioning, GHCR images, npm CLI releases, and air-gapped offline bundles (SPDX SBOMs, `SHA256SUMS-<arch>.txt`, `pgvector`, Ollama). |

### Developers & Reviewers
| Guide | Purpose |
| :--- | :--- |
| [Developer CLI (`aegis`)](client.md) | Submitting, watching, and cancelling jobs from the terminal ([npm package](../npm/aegis/README.md)). |
| [Reviewer Guide](reviewing.md) | Reviewing Aegis PRs, requesting review rounds (`/aegis fix`), and recording verdicts. |

---

## 2. Internal Engineering, Architecture & Evaluation Documentation

For contributors, architects, and benchmark maintainers.

### Design Specifications (English)
- [00 – Proposal](design/00-proposal.md) — Problem statement, KPIs, buy-vs-build, and implementation roadmap.
- [01 – Architecture Specification](design/01-architecture-detail.md) — Detailed 4-layer core agent control pipeline.
- [02 – Platform Topology](design/02-platform-topology.md) — System context, external integrations, and 17-crate workspace topology.
- [Contributing & Conventions](../CLAUDE.md) — Issue-driven workflow, labels, commit conventions, and pre-PR verification.

### Architectural Decision Records (ADRs, English)
- [ADR 0001: Platform Written in Rust](adr/0001-platform-in-rust.md)
- [ADR 0002: Full Platform MVP as Checkpoint](adr/0002-full-platform-mvp-as-checkpoint.md)
- [ADR 0003: Platform Service Architecture](adr/0003-platform-service-architecture.md)
- [ADR 0004: Multi-Language Profiles](adr/0004-multi-language-profiles.md)
- [ADR 0005: KPI Thresholds After Phase 1](adr/0005-kpi-thresholds-after-phase-1.md)
- [ADR 0006: Developer Client (`aegis`)](adr/0006-developer-client.md)
- [ADR 0007: On-Premise Deployment](adr/0007-on-premise-deployment.md)
- [ADR 0008: Gate Sandbox on Kubernetes](adr/0008-gate-sandbox-on-kubernetes.md)
- [ADR 0009: Prompt Injection Threat Model](adr/0009-prompt-injection-threat-model.md)
- [ADR 0010: Docker Socket Proxy](adr/0010-docker-socket-proxy.md)
- [ADR 0011: AMS Direction (Phase 3 and Phase 4)](adr/0011-ams-direction.md)
- [ADR 0012: L1/L2 Triage and Non-Code Diagnostic Lane](adr/0012-triage-and-diagnostic-lane.md)
- [ADR 0013: Ephemeral Database Sidecars in Gate Sandbox](adr/0013-ephemeral-database-sidecars.md)
- [ADR 0014: Per-Language Default Call Cap, 80 for Rust](adr/0014-per-language-call-cap.md)
- [ADR 0015: CVE Remediation Lane](adr/0015-cve-remediation-lane.md)
- [ADR 0016: Platform Bias Scope and Measurement](adr/0016-platform-bias-scope-and-measurement.md)
- [ADR 0017: GitHub App Integration](adr/0017-github-app.md)
- [ADR 0018: Multi-Provider and Residency Failover](adr/0018-multi-provider-and-residency-failover.md)

### Benchmarks, Measurements & Pilot (English / Vietnamese)
- [Measurement Reports Index](measurements/README.md) — Current KPIs per language and chronological experiment reports (Vietnamese).
- [Team Pilot Plan (#158)](pilot/2026-11-team-pilot-plan.md) — Four-week on-premise team pilot plan (Vietnamese).
- [Evaluation Benchmark Suite](../eval/README.md) · [Golden Master Eval](../eval/golden-master/README.md) · [Prompt Injection Eval](../eval/injection/README.md).
