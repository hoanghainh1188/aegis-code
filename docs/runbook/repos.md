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
