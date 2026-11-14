# Platform Topology: System Context and Crates

How Aegis sits among the systems around it, and how its 16 crates are wired together. The scope and the four control layers are in the [Proposal](00-proposal.md); the core engine, layer by layer, is in the [Architecture Specification](01-architecture-detail.md). The service architecture decisions are in [ADR 0003](../adr/0003-platform-service-architecture.md).

## 1. System Context & External Integrations

Aegis sits between your engineering workflow tools (Git forges, issue trackers, static analyzers) and LLM providers, operating within a strictly controlled host boundary:

<p align="center">
  <img src="../../assets/architecture-context.svg" alt="Aegis System Context & External Integrations" width="100%" />
</p>

<details>
<summary><b>🔍 View Interactive Mermaid Diagram (System Context)</b></summary>

```mermaid
flowchart LR
    classDef actor fill:#0c2d48,stroke:#38BDF8,stroke-width:1.5px,color:#F8FAFC;
    classDef external fill:#111927,stroke:#60A5FA,stroke-width:1.5px,color:#E2E8F0;
    classDef aegis fill:#042f2e,stroke:#00F5D4,stroke-width:2px,color:#F8FAFC;
    classDef sandbox fill:#451a03,stroke:#F59E0B,stroke-width:1.5px,color:#FEF3C7;
    classDef llm fill:#2e1065,stroke:#A855F7,stroke-width:1.5px,color:#F3E8FF;
    classDef infra fill:#064e3b,stroke:#10B981,stroke-width:1.5px,color:#ECFDF5;

    subgraph Actors["👥 Human Actors"]
        Dev["Developer / Reviewer"]:::actor
        Op["Platform Operator"]:::actor
    end

    subgraph ExternalDevOps["🔗 External DevOps & Issue Systems"]
        Forge["Git Forges<br/>(GitHub / GitLab)"]:::external
        Tracker["Issue Trackers<br/>(Jira / Backlog)"]:::external
        Sonar["Code Quality<br/>(SonarQube Server / Cloud)"]:::external
    end

    subgraph AegisBoundary["🛡️ Aegis Platform Boundary"]
        CLI["aegis CLI<br/>(Developer Client)"]:::aegis
        Server["aegis-server<br/>(API · Webhooks · Pollers · Dashboard · Workers)"]:::aegis
        Sandbox["Isolated Docker Sandboxes<br/>(--network none · non-root · cap-drop ALL)"]:::sandbox
        Proxy["Allow-listed Egress Proxy<br/>(Pre-gate dependency fetch)"]:::sandbox
    end

    subgraph AIProviders["🧠 AI / LLM Providers"]
        Claude["Anthropic Claude API<br/>(PoC / Direct Mode)"]:::llm
        Bedrock["AWS Bedrock ap-northeast-1<br/>(Japan Residency: jp.*)"]:::llm
        Ollama["Local Ollama<br/>(Offline / Local Trial)"]:::llm
    end

    subgraph Infra["🗄️ State, Security & Observability"]
        PG[("PostgreSQL<br/>(State & SKIP LOCKED Queue)")]:::infra
        OIDC["OIDC Provider<br/>(SSO Authentication)"]:::infra
        Secrets["Secrets Store<br/>(AWS Secrets Manager / Files)"]:::infra
        Prom["Prometheus & Grafana<br/>(:9464/metrics & Alerts)"]:::infra
    end

    Dev -->|"Browser (SSR Dashboard)"| Server
    Dev -->|"Terminal (REST API)"| CLI
    CLI -->|"Submit / Watch / Cancel"| Server
    Dev -->|"Label 'aegis' / Review PR / '/aegis fix'"| Forge

    Forge -->|"HMAC Webhooks (Issues, CI fails, Reviews)"| Server
    Server -->|"Clone worktree, push branch 'agent/*', open PR"| Forge
    Server <-->|"Poll JQL/Filters & write back outcomes"| Tracker
    Server <-->|"Fetch alerts & run Sonar Quality Gate"| Sonar

    Server -->|"HTTPS / AWS SDK (Prompt Caching BP1–BP4)"| Claude
    Server -->|"HTTPS / AWS SDK (ModelPolicy::JapanOnly)"| Bedrock
    Server -->|"Local HTTP"| Ollama

    Server <-->|"ACID transactions, leases, token ledger"| PG
    Server <-->|"OAuth2 / OIDC login flow"| OIDC
    Server <-- "Boot load & live rotation" --> Secrets
    Prom -->|"Scrape :9464/metrics"| Server
    Op -->|"Monitor & manage"| Prom

    Server -->|"spawn_blocking (docker run)"| Sandbox
    Sandbox -.->|"Pre-gate package resolution only"| Proxy
```

</details>

| External System | Direction | Protocol / Mechanism | Purpose & Security Boundary | Handled By |
| :--- | :---: | :--- | :--- | :--- |
| **Developers & Reviewers** | $\leftrightarrow$ | HTTPS (Browser / `htmx`), REST API (`/api/v1/*`) | View job ledgers on the dashboard, submit/watch jobs via `aegis`, or review PRs on GitHub/GitLab. | `aegis-server`, `aegis-client` |
| **GitHub / GitLab** | $\leftrightarrow$ | Webhooks (`X-Hub-Signature-256` / `X-Gitlab-Token`), Git / HTTPS API | Ingests labeled issues, `main` CI breakages, and PR review comments; pushes `agent/<id>` branches and opens structured PRs. | `aegis-server`, `aegis-pr` |
| **Jira / Backlog** | $\leftrightarrow$ | REST API Polling (atomic `polls` schedule) | Polls issues matching configured JQL/filters and writes resolution status/PR links back to the ticket. | `aegis-tracker`, `aegis-server` |
| **SonarQube** | $\leftrightarrow$ | Web API + `sonar-scanner` CLI | Ingests static analysis warnings (clustered by AST/dataflow) and verifies patches against the Sonar Quality Gate. | `aegis-sonar-batch`, `aegis-gate` |
| **Claude / Bedrock / Ollama** | $\rightarrow$ | HTTPS / AWS SDK (`aws-sdk-bedrockruntime`) | Executes model inference across Light/Core/Expert tiers. Enforces `jp.` inference profiles in `ap-northeast-1` for `customer-jp`. | `aegis-provider`, `aegis-router` |
| **PostgreSQL** | $\leftrightarrow$ | TCP / TLS (`sqlx` compile-time checked) | Stores repositories, transactional job queue (`FOR UPDATE SKIP LOCKED`), worker leases, user sessions, and token cost ledgers. | `aegis-store`, `aegis-ledger` |
| **Docker Daemon & Host** | $\rightarrow$ | Host CLI (`docker run` / `docker rm`) | Runs per-job Git worktrees on host and spawns air-gapped verification containers (`--network none`, `--cap-drop ALL`, uid 10001, bounded memory/CPU/processes, read-only image). | `aegis-gate`, `aegis-lifecycle` |
| **OIDC / Secrets / Metrics** | $\leftrightarrow$ | OIDC, AWS Secrets Manager API, Prometheus | Authenticates dashboard users via SSO, rotates credentials without restart, and exports operational/financial metrics on `:9464/metrics`. | `aegis-server`, `aegis-secrets` |

---

## 2. Internal Architecture & Component Topology

Internally, Aegis is structured into **five modular subsystems** across 16 Rust crates. The unified binary `aegis-server` can run all subsystems in a single process (`--role all`) or split horizontally into stateless API nodes (`--role api`) and compute workers (`--role worker`):

<p align="center">
  <img src="../../assets/architecture-internal.svg" alt="Aegis Internal Component & Crate Topology" width="100%" />
</p>

<details>
<summary><b>🔍 View Interactive Mermaid Diagram (Internal Crate Topology)</b></summary>

```mermaid
flowchart TB
    classDef entry fill:#0c2d48,stroke:#38BDF8,stroke-width:1.5px,color:#F8FAFC;
    classDef service fill:#042f2e,stroke:#00F5D4,stroke-width:1.8px,color:#F8FAFC;
    classDef store fill:#064e3b,stroke:#10B981,stroke-width:1.5px,color:#ECFDF5;
    classDef engine fill:#2e1065,stroke:#A855F7,stroke-width:1.5px,color:#F3E8FF;
    classDef tools fill:#451a03,stroke:#F59E0B,stroke-width:1.5px,color:#FEF3C7;

    subgraph Entrypoints["1. Entrypoints & Clients"]
        Client["aegis-client<br/>(aegis CLI)"]:::entry
        LocalCLI["aegis-cli<br/>(aegis local & eval runner)"]:::entry
    end

    subgraph ServiceLayer["2. Platform Service Layer (aegis-server)"]
        API["HTTP API & Webhook Ingress<br/>(Axum · HMAC · Rate Limiter)"]:::service
        UI["SSR Web Dashboard<br/>(Askama + htmx · OIDC · CSRF)"]:::service
        Poller["Background Pollers<br/>(Jira · Backlog · Sonar schedules)"]:::service
        WorkerPool["Worker Pool & Lease Manager<br/>(Concurrency limits · CancellationToken)"]:::service
    end

    subgraph Persistence["3. State, Secrets & Accounting"]
        Store["aegis-store<br/>(PostgreSQL Queue · Leases · Repos · Audit)"]:::store
        Secrets["aegis-secrets<br/>(AWS Secrets Manager · File secrets · Rotation)"]:::store
        Ledger["aegis-ledger<br/>(Per-call Token Accounting & USD Cost)"]:::store
    end

    subgraph CoreEngine["4. Issue Lifecycle & Agent Engine"]
        Lifecycle["aegis-lifecycle<br/>(End-to-End Job Coordinator)"]:::engine
        Orch["aegis-orchestrator<br/>(Agent Turn Loop · History Collapsing)"]:::engine
        Router["aegis-router<br/>(Light / Core / Expert · $4 & Call-Cap Breaker)"]:::engine
        Provider["aegis-provider<br/>(Claude / Bedrock / Ollama · BP1–BP4 Cache)"]:::engine
    end

    subgraph CodeAndDelivery["5. Code Intelligence, Verification & Delivery"]
        ACI["aegis-aci<br/>(MCP Server: open, search, syntax-safe edit)"]:::tools
        MemMCP["codebase-memory-mcp<br/>(SQLite Call Graph: CALLS, IMPORTS, TESTS)"]:::tools
        Lang["aegis-lang<br/>(Tree-sitter AST · LSP · 6 Language Profiles)"]:::tools
        Gate["aegis-gate<br/>(6-Step Docker Sandbox Runner)"]:::tools
        PR["aegis-pr<br/>(Git Branching · PR Template · Golden Master)"]:::tools
        SonarBatch["aegis-sonar-batch<br/>(AST / Dataflow Alert Clustering)"]:::tools
        Tracker["aegis-tracker<br/>(Jira & Backlog REST Client)"]:::tools
    end

    Client -->|"REST /api/v1"| API
    API --> Store
    UI --> Store
    Poller --> Tracker
    Poller --> SonarBatch
    Poller --> Store
    WorkerPool <-->|"SELECT ... FOR UPDATE SKIP LOCKED"| Store
    WorkerPool --> Lifecycle
    LocalCLI --> Lifecycle

    Lifecycle --> Secrets
    Lifecycle --> Orch
    Lifecycle --> Router
    Lifecycle --> Gate
    Lifecycle --> PR
    Lifecycle --> Ledger
    Lifecycle --> Tracker
    Lifecycle --> SonarBatch
    Store --> Ledger

    Orch --> Router
    Router --> Provider
    Provider --> Secrets
    Orch <-->|"MCP over stdio (rmcp)"| ACI
    Orch <-->|"MCP over stdio (rmcp)"| MemMCP

    ACI --> Lang
    ACI --> Gate
    Gate --> Lang
    SonarBatch --> Lang
    SonarBatch --> Gate
    PR --> Gate
    PR --> Provider
```

</details>

### How Internal Components Work Together
1. **Service & Scheduling (`aegis-server`, `aegis-store`, `aegis-secrets`):**
   * **Ingress & UI:** `aegis-server` exposes the REST API, HMAC-verified webhook endpoints, and an **Askama + htmx** server-rendered dashboard secured by OIDC SSO and in-memory token-bucket rate limiting.
   * **Transactional PostgreSQL Queue:** Instead of external brokers (Redis/SQS), `aegis-store` manages the job queue directly in PostgreSQL using `SELECT ... FOR UPDATE SKIP LOCKED` with distributed worker leases (`locked_until`), per-repo concurrency caps, and graceful `SIGTERM` draining.
2. **Lifecycle Coordination (`aegis-lifecycle`):**
   * Serves as the central facade invoked by both `aegis-server` workers (in production) and `aegis-cli` (for local single-issue runs and evaluation benchmarks).
   * Prepares an isolated Git worktree per job, builds the `codebase-memory-mcp` SQLite call graph index, and orchestrates the transition from triage $\rightarrow$ agent loop $\rightarrow$ verification gate $\rightarrow$ PR creation.
3. **Agent Execution Loop (`aegis-orchestrator`, `aegis-router`, `aegis-provider`, `aegis-ledger`):**
   * `aegis-orchestrator` manages multi-turn agent sessions, dispatches tool calls over MCP (`rmcp` child processes), and compacts older tool observations once context usage exceeds $60\%$.
   * `aegis-router` selects the active tier (`Light` $\rightarrow$ `Core` $\rightarrow$ `Expert`) based on failure signals and enforces the economic circuit breaker ($\$4$ or the call cap) backed by real-time accounting in `aegis-ledger`.
   * `aegis-provider` serializes requests with deterministic 4-breakpoint prompt caching (`BP1`–`BP4`) and enforces regional routing policies (`ModelPolicy::JapanOnly` for `customer-jp.toml`).
4. **Code Intelligence & Sandboxed Verification (`aegis-aci`, `aegis-lang`, `aegis-gate`):**
   * `aegis-aci` runs as an MCP tool server providing bounded code windows (`open`, `search_symbol`, `find_definition` via LSP) and syntax-guarded edits (`edit`).
   * `aegis-lang` supplies ecosystem-specific rules (`profiles/*.toml`), Tree-sitter AST grammars, lockfile parsers, and test runners for **Rust, Python, TypeScript/JavaScript, Java, Go, and C#**.
   * `aegis-gate` executes the 6-step verification suite inside a network-isolated Docker container (`--network none`), including automatic re-runs to filter out flaky tests.
5. **External Delivery & Clustering (`aegis-pr`, `aegis-tracker`, `aegis-sonar-batch`):**
   * `aegis-pr` pushes `agent/<issue-id>` branches, formats the bilingual/structured PR description (RCA, diff rationale, gate proof, Golden Master declarations, token ledger), and handles follow-up review rounds on the same branch.
   * `aegis-sonar-batch` groups related SonarQube warnings by enclosing AST node or tainted dataflow trace and merges verified fixes into an integration branch.
   * `aegis-tracker` synchronizes ticket context and final PR status with Jira and Backlog.
