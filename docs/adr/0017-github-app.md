# ADR 0017 – GitHub App: One Installation Instead of a Bot, a Fork and Tokens per Repo

- Date: 2026-11-15
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #536 (Milestone: `Phase 2 – Platform`), split into #538 (token source), #539 (config, jobs), #540 (webhook, comments), #541 (tenants), #542 (deploy, docs)
- Related: [ADR 0003](0003-platform-service-architecture.md) §4 (webhooks), [ADR 0007](0007-on-premise-deployment.md), [ADR 0009](0009-prompt-injection-threat-model.md), #30 (secret store), #158 (pilot), #244 (review rounds), #525 (tenants)

## Context

Connecting a GitHub repo today takes four things from people outside the platform team ([on-premise runbook](../runbook/on-premise.md#preparation-checklist), items 5–8; [repos.md](../runbook/repos.md#prerequisites)):

1. a bot account and, per repo, the fork it opens PRs in (`[settings.fork]`, `ForkConfig` in `crates/aegis-lifecycle/src/config.rs`), kept in sync with upstream by hand;
2. a fine-grained PAT of the bot per repo (`fork.token_env`), used for clone and fetch, push of `agent/*`, opening the PR (`crates/aegis-lifecycle/src/publish.rs`, `crates/aegis-pr/src/github.rs`), reading CI runs and logs (`crates/aegis-pr/src/actions.rs`) and review edits;
3. a separate token that may comment on upstream issues and check who may ask for a review round, `AEGIS_GITHUB_COMMENT_TOKEN` (`crates/aegis-server/src/webhook/notify.rs`);
4. an admin of each upstream repo to add the webhook signed with `AEGIS_GITHUB_WEBHOOK_SECRET` (the connection page can do it with a short-lived admin token, but someone with admin rights still has to hand one over).

The machine side is scripted (`install.sh`, `aegis-deploy.sh`, `add-repo.sh`); this per-repo GitHub work and the long-lived PATs it leaves behind are now the largest part of a deployment. PATs belong to a person-like account, expire on their own schedule, and grant everything for as long as they live.

A GitHub App replaces all four: the customer installs it once on their org (all repos or chosen ones); the platform mints short-lived tokens for the repos it was given; it comments as `<app>[bot]`; and GitHub delivers the webhooks of every installed repo to the app's one webhook URL.

The pilot (#158) starts on GitHub with v0.14.0 on the token path. That path must keep working unchanged.

## Decision

### 1. Two ways to connect a GitHub repo; the existing one stays

A repo's settings name exactly one of:

- `[settings.fork]` – the token path, exactly as today (fork or upstream as `fork`, `token_env`, `AEGIS_GITHUB_COMMENT_TOKEN`, the repo's own webhook). Nothing in it changes, no migration.
- `[settings.app]` – the app path. It carries no secret name: the repo's `url` names the upstream repo, the app config of the instance (§3) does the rest. Options: `api` for GitHub Enterprise Server (default the instance's).

Both present, or neither, is a configuration error. GitLab keeps `[settings.fork]` with `platform = "gitlab"`; this ADR does not touch it. A repo moves from one path to the other by editing its settings (dashboard form or `add-repo.sh` gain the choice); jobs already running finish on the path they started with.

### 2. Without a fork: `agent/*` branches in the upstream repo

On the app path the agent's branch is pushed to the upstream repo and the PR opened there, from `agent/<issue>` into the default branch: the model repos.md already calls *Direct Upstream Repository* (`fork` equal to upstream), with the app as the writer instead of a bot.

- The platform pushes only `agent/*` branches (the `AgentBranch` type already enforces it), never force-pushes a branch it did not create, and never pushes the default branch.
- The runbook asks the customer for a ruleset on the default branch (and release branches) that requires a PR and gives the app no bypass, so a fault in the platform cannot write to them either.
- The app is registered **without** the `workflows` permission: GitHub then refuses any push from it that touches `.github/workflows/`, whatever the patch contains.
- The fork model stays available on the token path for customers who refuse write access to upstream. An app installed on a fork org (two installations, one writing to the fork, one reading and commenting upstream) is possible later but is not part of this decision.

### 3. The app's identity and installation tokens

The instance has one app, configured in `server.toml`:

```toml
[github_app]
app_id = 123456                     # or client_id, which GitHub now prefers as the JWT issuer
# api = "https://ghe.example.com/api/v3"   # GHES; github.com by default
# private_key_env = "AEGIS_GITHUB_APP_PRIVATE_KEY"        # default
# webhook_secret_env = "AEGIS_GITHUB_APP_WEBHOOK_SECRET"  # default
```

The private key (PEM) and the webhook secret live in the secret store (#30) like every other secret, read when used, so rotation needs no restart. Without `[github_app]` the instance behaves exactly as v0.14.0, and a repo with `[settings.app]` is refused at registration.

Tokens:

- **App JWT**: RS256, signed with the private key, `iat` 60 s in the past, `exp` at most 10 minutes; used only to find an installation and to mint tokens, never sent anywhere else.
- **Installation of a repo**: `GET /repos/{owner}/{repo}/installation` with the JWT, cached per repo; the cache is dropped on a 404/401 and on the `installation*` events (§5). Payloads of events never decide which installation a token is minted for.
- **Installation token**: `POST /app/installations/{id}/access_tokens` with `repositories: [<repo>]` and the permissions of the use (below), so a token reaches one repo with only what the call needs. Tokens live one hour; a job can last longer, so tokens are minted **per use** and cached until 10 minutes before `expires_at`, never once per job.

| Use | Code today | Permissions asked |
| --- | --- | --- |
| clone, fetch the base, read the issue | `clone.rs`, `lifecycle.rs` | `contents: read`, `issues: read`, `metadata: read` |
| CI runs and logs of a failed run | `actions.rs` | `actions: read` |
| push `agent/*`, open the PR, review round push and reply | `publish.rs`, `github.rs`, `git.rs` | `contents: write`, `pull_requests: write` |
| review edits of the PR (`review_edits`) | `github.rs` | `pull_requests: read` |
| comment on the issue or PR, check the sender's permission | `notify.rs` | `issues: write`, `pull_requests: write`, `metadata: read` |

The app is registered with the union: Metadata read, Contents read/write, Pull requests read/write, Issues read/write, Actions read; no Workflows, no Administration, no organization permissions. Whether `GET collaborators/{login}/permission` is allowed with only `metadata: read` is checked during implementation; if it needs more, that permission is added to the table and the app registration, not to every token.

  *Checked in #538:* GitHub's docs for that endpoint do not name the permission a fine-grained or installation token needs; the Metadata permission is described as covering listing collaborators, so `metadata: read` (already in the comment token's set) is the likely minimum but is **not confirmed**. `can_write` asks with a `Comment` token (`issues: write`, `pull_requests: write`, `metadata: read`) since #540, tested against a fake GitHub only; the runbook ([install guide, B10](../install-on-premise.md#b10-the-github-app-instead-of-a-bot-and-tokens)) has the first real installation confirm it: a review round asked for by a collaborator must start, and one by someone without write access must be refused. Until then the table stays as is.

In code, the string token a job holds today (`Platform::github_token`, `Source::token`, `GitHub::new(.., token)`) becomes a token source: `Static(Secret)` for the token path, `App { installation, permissions }` for the app path, asked for a token at each call. The token path keeps reading `token_env` from the store as now. Installation tokens go where PATs go today and nowhere else: git's environment for the platform's own git calls and the `Authorization` header of API calls; never into a sandbox, a prompt, a log or the ledger. The output scan (#509) already masks `ghs_…` tokens and JWTs.

### 4. Comments and review-round checks from the app

For a repo on the app path, `GitHubNotifier` (job ended, budget spent, review-round replies) and `can_write` (#244) use an installation token of that repo instead of `AEGIS_GITHUB_COMMENT_TOKEN`. Comments appear as `<app-slug>[bot]`. Repos on the token path keep the comment token; an instance with only app repos needs no comment token at all, and `install.sh` stops asking for it once `[github_app]` is set.

The review-round trigger already ignores senders of kind `Bot`, so the app's own comments never start a round.

### 5. The app's webhook and `installation` events

The app's webhook points at the same `POST /webhooks/github` with its own secret, `AEGIS_GITHUB_APP_WEBHOOK_SECRET`. *Amended in #542:* an app created from a manifest gets its webhook secret from GitHub, which the manifest cannot set; `github-app.sh` saves the secret GitHub returns (with the app's id and key) instead of `install.sh` generating one.

- **Provenance from the signature, not the payload.** A delivery is verified against the repo-webhook secret and the app secret (each in constant time). Which one verified says where it came from: a delivery signed with the app secret counts only for repos on the app path, one signed with the repo-webhook secret only for repos on the token path. A repo moving between paths while both webhooks exist therefore never gets a job twice, and the `installation` field of a payload is never trusted to say which path a delivery belongs to.
- The app subscribes to the events the repo webhooks already deliver (`issues`, `workflow_run`, `pull_request_review`, `pull_request_review_comment`, `issue_comment`), handled as today, plus:
  - `installation` (`created`, `deleted`, `suspend`, `unsuspend`, `new_permissions_accepted`) and `installation_repositories` (`added`, `removed`): recorded in a new table `github_installations` (installation id, account login, suspended, repositories, tenant; additive migration) and used to drop the token cache.
- An event never registers a repo by itself: registering still needs a language, mode and limits. The dashboard's Add repo lists the installed repos not yet registered to pick from. A repo whose installation is removed or suspended stops taking jobs with that reason on its connection page; the job in progress fails at its next token with the same reason.

### 6. Tenants (#525)

One app serves every tenant: each customer installs the operator's app on their own org, and every installation is one tenant's.

- An installation belongs to at most one tenant. A new installation arrives unbound; a platform admin binds it to a tenant (dashboard or `aegis tenant`). Until then no token is minted for it.
- A repo of tenant A may use only an installation bound to A, checked at registration and at each token. This is what keeps tenant B from registering a repo of A's org: the platform's key could mint a token for any installation, so the binding, not the key, is the boundary.
- The private key and the app webhook secret are platform secrets, outside the per-tenant prefix rule, like `AEGIS_GITHUB_WEBHOOK_SECRET` today; `[settings.app]` names no secret, so the prefix check of `registry.rs` has nothing to refuse. A tenant that requires its own app identity (own name, own key under its prefix) is a later extension: `[github_app]` becomes a list keyed by name and `[settings.app]` names one.

## Rollout

Each part is its own issue and PR; none changes the token path:

1. #538 `aegis-pr`: app JWT, installation lookup, scoped installation tokens with cache, and the token source used by `GitHub`, `Remote` and clone.
2. #539 `aegis-lifecycle` / `aegis-server`: `[github_app]` in `server.toml`, `[settings.app]` in repo settings, jobs take tokens from the source per use.
3. #540 `aegis-server` webhook: two-secret verification and provenance, `installation*` events, `github_installations`, comments and `can_write` with the app.
4. #541 Tenants: binding installations to tenants (with or after #525).
5. #542 Deploy and docs: `github-app.sh` saves the app's id, key and the webhook secret GitHub generates, and `install.sh` stops asking for the comment token once `[github_app]` is set; an [app manifest](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest) so creating the app is one click with the right permissions and events; runbook checklist with the app path (items 5–8 become "install the app; add the default-branch ruleset").

## Consequences

- **Positive:** connecting a GitHub repo becomes one installation by an org admin plus registering the repo; no bot account, no fork to sync, no PAT per repo, no comment token, no webhook per repo.
- **Positive:** no long-lived GitHub credential per repo: tokens last an hour, reach one repo, and carry only the permissions of their use; revoking is uninstalling.
- **Positive:** the app cannot change workflows, and the platform's commits and comments are attributed to an app, not to a user account.
- **Negative:** on the app path the agent's branches live in the upstream repo, so its PRs run upstream CI as same-repo PRs, **with the repo's Actions secrets**, which PRs from a fork do not get. Agent-written code and tests then run where those secrets are readable. The runbook must say so, recommend environments with required reviewers for secrets that matter, and keep the fork model (token path) for repos where that is not acceptable. This is already true of the token path's direct-upstream model today.
- **Negative:** the private key is one secret that reaches every installation of the instance; its loss is worse than one PAT's. It stays in the secret store, is rotated by adding a new key in the app settings before removing the old, and the tenant binding (§6) limits what a misconfigured repo can reach, not what a stolen key can.
- **Negative:** two GitHub paths to keep working and tested until the token path can be retired, which this ADR does not decide.
