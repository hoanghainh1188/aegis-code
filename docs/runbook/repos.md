# Operations Runbook: Adding Repositories

Onboarding a GitHub or GitLab repository, from registration to the first PR. The steps are the same on every deployment; only the prefix of the server's commands and the place of secrets and repo files differ:

| Deployment | `...` in the commands below | Fork token | Repo file |
| --- | --- | --- | --- |
| AWS ([runbook](README.md)) | `sudo docker exec aegis aegis-server --config /etc/aegis/server.toml` | Secrets Manager, [§3](README.md#3-secret-rotation) | `/srv/aegis/.workspace/repo-files/` on the host |
| On-premise Compose ([runbook](on-premise.md#2-registering-repositories)) | `docker compose exec aegis aegis-server --config /etc/aegis/server.toml`, from `deploy/compose/` | File `./secrets/<token_env>` (`0600`, uid 10001) | `/srv/aegis/.workspace/repo-files/` on the host |
| Kubernetes ([runbook](kubernetes.md#2-registering-repositories)) | `kubectl -n aegis-worker exec aegis-worker-0 -c aegis -- aegis-server --config /etc/aegis/server.toml` | Key of the `aegis-worker` Secret | Copied onto the workspace volume |

On-premise Compose, GitHub repos: `sudo ./add-repo.sh` asks for the values and does steps 1–7 ([on-premise runbook §2](on-premise.md#2-registering-repositories)). A first install, with screenshots, is in the [install guide](../install-on-premise.md#part-c-the-first-repo-and-the-first-job).

### Prerequisites

1. **Target PR Repository (`fork`).** The agent pushes branches named `agent/*` and submits PRs **into `fork.owner/fork.name`**, targeting its default branch. Two deployment models:
   - **Dedicated Bot Fork:** Maintainers merge PRs from the bot fork into the upstream repository. The bot requires zero write access to the upstream repo.
   - **Direct Upstream Repository (`fork` equals upstream repo):** PRs open directly in the upstream repo. The bot requires write access, scoped to `agent/*` branches via branch protection rules.

   The fork default branch must match the upstream default branch and be synchronized regularly. PRs open in draft status when `[run] draft_prs = true` (default).

2. **Fork Authentication Token** (`fork.token_env`, secret key name). Fine-grained Personal Access Token (PAT) for the bot account:
   - Target fork repository: *Contents* Read/Write, *Pull requests* Read/Write.
   - Upstream repository: *Issues* Read (to ingest issue details), *Actions* Read (to analyze CI run failures and logs, #250). For private repositories, grant *Contents* Read for cloning and fetching.

   Store token values as described in [AWS Runbook §3](README.md#3-secret-rotation) (on-premise: a file in `./secrets`; Kubernetes: a key of the worker Secret); add new secret names to `repo_secret_names` first.

   **GitLab Forks (#252):** In `[settings.fork]`, specify `platform = "gitlab"` and configure `api` as the instance API root (`https://gitlab.com/api/v4` or self-hosted); `owner` is the namespace (including subgroups), `name` is the project slug. The agent pushes branches over HTTPS (token authenticated via user `oauth2`) and opens **merge requests** (draft when `draft_prs = true`). Token requires scopes `api` and `write_repository` (plus `read_repository` for private repos). GitLab issues (labeled `aegis`) and broken CI pipelines on the default branch (#253) trigger automated repair jobs. **Review rounds on MRs (#254):** Comments containing `/aegis fix` by project members with Developer role or higher trigger repair cycles on the agent's branch.

3. **Issue Comments:** Configured globally via `AEGIS_GITHUB_COMMENT_TOKEN` with *Issues* Write permission on upstream repositories. For GitLab, set `AEGIS_GITLAB_COMMENT_TOKEN`. For GitHub Enterprise or self-hosted GitLab, configure container environment variables `AEGIS_GITHUB_API` / `AEGIS_GITLAB_API`.

### Registration and Cloning

4. Create repository definition file, e.g. `typst.toml` (all keys in `[settings]` validate against `RepoSettings` in [registry.rs](../../crates/aegis-lifecycle/src/registry.rs); unrecognized keys are rejected):

   ```toml
   slug = "typst"                         # Identifier in paths, branches, dashboard
   url = "https://github.com/typst/typst" # Upstream repo URL matched against webhooks
   default_branch = "main"
   language = "rust"                      # python, rust, go, typescript, java, csharp
   mode = "poc"                           # or customer-jp
   max_concurrent_jobs = 1

   [settings]
   # Repository-specific conventions appended to system prompt after language profile
   conventions = """
   Tests for a crate go in its tests/ directory.
   Run `cargo test -p <crate>` for one crate.
   """

   [settings.fork]
   owner = "acme-aegis-bot"
   name = "typst"
   token_env = "AEGIS_TOKEN_TYPST"
   ```

   Copy file to host workspace (e.g. `/srv/aegis/.workspace/repo-files/typst.toml`), then execute:
   ```bash
   sudo docker exec aegis aegis-server --config /etc/aegis/server.toml repo add /srv/aegis/.workspace/repo-files/typst.toml
   ```
   This command:
   - Validates settings against deployment mode (`customer-jp` requires valid regional mode);
   - Clones repository into `<state_dir>/repos/<slug>` (with the fork token, which is sent for public repos too, so it must be valid);
   - Records an audit log `repo.register` and outputs the repository ID.

   Workers fetch latest commits from the default branch before starting every job, requiring no manual git sync.

   **On the dashboard instead (#330):** an admin can do step 4 at `/admin/repos` → **Add a repo**. It runs the same checks, takes the fork token by the name of its secret (**Token: name of the secret**), clones in the background, and shows the result on the repo's page, with the end of git's output if the clone failed. An invalid token shows as `could not read Username for 'https://github.com'`.

5. Management commands: `... repo show <slug> > repo.json`, edit, then `... repo update repo.json`. List repositories via `... repo list` (a paused one ends with `paused`). Pause a repository that runs wrong or spends too fast with `... repo pause <slug> [--reason <text>] [--cancel-queued]`: its queued jobs wait (or are cancelled) and new ones are still recorded until `... repo resume <slug>`; the repository's admin page does the same (#374).

6. **Writing Effective Conventions:** Detail commands required to execute a **single** test and specify where reproduction tests should be authored. General language test conventions already exist in `prompts/conventions/`; repository conventions should focus on project-specific guidelines.

7. **Services the tests need (#490, [ADR 0013](../adr/0013-ephemeral-database-sidecars.md)):** tests that connect to PostgreSQL, Redis or MySQL fail in the gate's `--network none` sandbox, and `aegis check-repo` reports them under "Network & DB Isolation". Declare each service in the repo file, under `[settings]`:

   ```toml
   [[settings.sidecars]]
   name = "postgres"                      # host name on the sidecars' network
   image = "postgres:16-alpine"           # must be in the proxy's --sidecar-image list
   user = "70:70"                         # the image's service user, never root
   env = { POSTGRES_PASSWORD = "test" }
   tmpfs = ["/var/lib/postgresql/data", "/var/run/postgresql"]  # what it writes, on tmpfs
   ready = ["pg_isready", "-h", "localhost"]                    # exits 0 once it accepts connections
   ready_timeout_s = 60
   test_env = { DATABASE_URL = "postgres://postgres:test@postgres:5432/postgres" }
   # localhost = true                     # tests that connect to localhost:5432
   ```

   - Only `repro_test`, `test` and `coverage` reach the sidecars, on an internal network of their own: no step reaches the host or the Internet. Format, lint and the fetch step stay as before.
   - Tests reach a sidecar by its `name` (`postgres:5432`), through what `test_env` gives them. For tests that hard-code `localhost`, `localhost = true` runs the test steps in that sidecar's network namespace; other sidecars are still reached by name. One sidecar per repo may have it.
   - Each test command finds fresh services: the sidecars are stopped and started between commands, which empties their tmpfs. A test suite that needs a schema creates it, as in its own CI.
   - The service runs as `user` with no capabilities and a read-only root. An image that must start as root (to `chown` its data directory) does not run; use its non-root user and list what it writes in `tmpfs`. Common users: `postgres:*-alpine` 70:70, `postgres:*` (Debian) 999:999, `redis:*` 999:1000, `mysql:*` 999:999.
   - The deployment allows the image: the proxy's `--sidecar-image` ([On-Premise](on-premise.md#2-registering-repositories)), and the image is pulled or loaded on the machine. A run configuration declares the same as `[[repo.sidecars]]`.
   - The agent's `run_tests` reaches them too (#491), on sidecars of its own session: the agent sees the result the gate will.

8. **CVE jobs (#455, [ADR 0015](../adr/0015-cve-remediation-lane.md)):** a job that raises one package to the version that fixes a security advisory. Bug-fix jobs never change a locked version (#500); this is how dependencies are upgraded.

   ```bash
   aegis cve --config <file> --package requests --version 2.32.4 --advisory GHSA-9wx4-h78v-vm56
   ```

   - The platform runs the profile's upgrade command (`cargo update --package … --precise …`, `uv lock --upgrade-package …==…`, `npm install …@… --package-lock-only`, `go get …@…`, Maven's `versions:use-dep-version`, `dotnet add package … --version …`) through the egress proxy, then the gate checks the result: the manifests and lockfiles must stay exactly as the upgrade wrote them, the package must be at the version, and the audit must no longer report the advisory.
   - If the gate passes, the PR opens with no agent turn (one Light call writes the commit message). If it fails, typically on a breaking change, the agent adapts the code and must not touch any manifest or lockfile.
   - A repo whose build needs another command overrides it in `[repo.overrides]` (`upgrade = ["…", "{package}", "{version}"]`), or turns it off with `upgrade = false`.
   - Only the root of a monorepo is upgraded for now. The job's id defaults to `cve-<package>-<version>`; the PR links the advisory on osv.dev.

### RBAC and Budgets

7. Grant dashboard permissions: `... grant <email> <repo-slug> maintainer|reviewer|viewer` (maintainer: start/cancel jobs, set budgets; reviewer: review PRs; viewer: read-only).
8. Monthly Budget Enforcement (recommended): Use a service token with maintainer role on the repository:

   ```bash
   curl -X PUT https://<domain>/api/v1/repos/<id>/budget -H "Authorization: Bearer $TOKEN" \
     -H 'content-type: application/json' -d '{"monthly_usd": 50}'
   ```

   Jobs exceeding monthly budgets are canceled with an automated explanatory comment posted to the issue. Per-job caps are governed by server `[worker] max_cost_usd` or repo `[settings.budget]`.

### Webhook Configuration

9. **GitHub:** Upstream repository $\rightarrow$ Settings $\rightarrow$ Webhooks $\rightarrow$ Add webhook:
   - *Payload URL*: `https://<domain>/webhooks/github`, *Content type*: `application/json`;
   - *Secret*: Value of secret `AEGIS_GITHUB_WEBHOOK_SECRET`;
   - *Events*: Select **Issues** and **Workflow runs**.

   Adding label `aegis` (customizable via `AEGIS_WEBHOOK_LABEL`) triggers automated repair; CI build failures on the default branch trigger automated CI repair jobs (#250).

   **Review Rounds (#243, #244):** On the **repository hosting agent PRs** (`fork`), configure webhooks for **Pull request reviews**, **Pull request review comments**, and **Issue comments**. A "Request changes" review or a comment containing `/aegis fix` by an authorized collaborator triggers a review round: the agent resumes work on the existing branch, re-executes verification gates, pushes an additional commit (no force-push), and responds to reviewer feedback.
   - Capped at `[reviews] max_rounds` rounds per PR (default 3), each round governed by independent budget limits.
   - Maintainers can also request review rounds via API: `POST /api/v1/jobs/<id>/review`.

   **Checking and making the webhooks from the dashboard:** the repo's page → **Connection to the forge** (#331) shows the payload URL and events, whether each secret is in the store, and the repo's last webhooks (GitHub's `ping` on creating the webhook counts as connected; an ignored one says why). **Check now** asks the forge whether the fork token may push and the comment token may comment, and whether the repo's URL is still its name. With `[web] make_webhooks = true` in `server.toml` (off by default), the same page makes both webhooks instead (#376): pick the repo or the fork, paste a token that may manage its webhooks (GitHub `admin:repo_hook`, GitLab Maintainer), and the server creates the webhook, or updates the one already pointing at it, signed with its own secret. The token is used for that request only: not kept, logged or recorded; the audit log says only that the webhook was made, with its id.

10. **GitLab:** Project $\rightarrow$ Settings $\rightarrow$ Webhooks: URL `https://<domain>/webhooks/gitlab`, *Secret token*: `AEGIS_GITLAB_WEBHOOK_TOKEN`, events: **Issues events**, **Pipeline events**, and **Comments** (#254).

### Verification

11. Open a test issue with clear reproduction steps and label it `aegis`. Within seconds:
    - Webhook delivery returns HTTP `202`;
    - Web dashboard displays new job transitioning to `running`; logs show `claimed` with `job_id`.
12. Upon completion: results are posted as comments on the issue; generated PRs appear on the `fork` repo on branch `agent/...`.
13. Debugging: inspect webhook deliveries and logs (invalid HMAC $\rightarrow$ `401`; URL mismatch $\rightarrow$ `200` with `"status": "ignored"`).
14. Hand over: point reviewers to the [Reviewer Guide](../reviewing.md) and developers to the [`aegis` client](../client.md).
