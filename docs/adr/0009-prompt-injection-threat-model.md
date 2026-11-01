# ADR 0009 – Prompt Injection Threat Model

- Date: 2026-11-01
- Status: Accepted
- Decider: @Nguyen Han Hoang Hai
- Issue: #356 (follows #351, #352, #357)

## Context

aegis-code reads text it does not control: issue titles and bodies, tickets (Jira/Backlog), review notes, CI logs, Sonar messages, and the repository itself (README, comments, test output). That text reaches an LLM agent that edits code and whose patch becomes a pull request. Anyone who can write into one of those places can try to give the agent orders ([Design §7](../design/01-architecture-detail.md)).

No prompt wording makes a model immune. The platform therefore does not depend on the model obeying: the model is treated as a confused deputy, and what it can do and what it can change are bounded outside it.

## Assets and attackers

Assets: repository secrets and platform credentials; the CI that runs on the fork; the checks (gate, Sonar) that decide a patch is good; the dependency supply chain; the reviewer's trust in the PR.

Attacker: an author of any untrusted text above (an outside issue reporter, a commenter, a dependency's README, a poisoned CI log). They cannot act on the host or push code; they can only influence what the agent reads.

## Threats and controls

| Threat | Controls (layer) | Residual risk |
|---|---|---|
| Work outside the issue's scope | System prompt "# Trust" (`prompts/system.md`); tools limited to ACI, no shell; patch reviewed by a person; PR is opened in the fork on `agent/*` only | No automatic scope check (#359) |
| Text ends its own data block (`</issue>`) | `defang` on every platform tag in prompts (#351); data labelled "not instructions" | Other tags the model itself treats as structure |
| Secret leaves through the model | `redact` on all text sent to the model (`aegis-aci/src/secrets.rs`); containers get no host env or `.env`; `--network none` | Secrets without a known pattern (#360) |
| Data leaves through the PR | `inert()` on every model-written field (body, commit, reply): no markup, mentions, links, images, HTML or entities (#357) | Plain text itself can carry data; a reviewer reads it |
| CI or workflow edited, checks loosened | `ALWAYS_PROTECTED` blocks CI, CODEOWNERS, Sonar, linter, coverage and test-runner config for every profile (#352); profile `protected` for build config | `conftest.py`, `build.rs`, Makefile and scripts are not protected on purpose |
| Malicious dependency | Gate dependency step: lockfile allow-list and official source; fetch only through the egress proxy to registry hosts; advisory/license audit | Manifest-only changes without a lockfile in some ecosystems |
| Sandbox escape or exfiltration from tests | `--network none`, `--cap-drop ALL`, no-new-privileges, read-only rootfs, resource limits (design §7, #320) | Kernel/container-runtime bugs |
| Deceiving the reviewer | PR body built by the platform from fixed sections; model text inert and length-capped; gate results come from the platform, not the model | Persuasive but wrong prose in `cause`/`note` |
| Injection through who may trigger a job | Webhook signature, rate limit; review notes only from people with write rights | Issue text by outsiders on labelled issues (#358) |

## Decision

1. **The model is untrusted for authority, not only for correctness.** Every control that matters is enforced by the platform (gate, file protection, sandbox, `inert`, `redact`), none by asking the model.
2. **Untrusted text is data in three steps:** redact secrets, defang platform tags, label as data. All three apply to every source (issue, ticket, review, CI log, Sonar, diff, gate output).
3. **Anything the agent may not change is listed in code**, not in the prompt: `ALWAYS_PROTECTED` and each profile's `dependencies.protected`. The prompt only tells the agent so it stops early.
4. **A person reviews and merges** every PR; no autonomous merge into protected branches.
5. New controls are added with a test that carries an attack payload.

## Consequences

- A successful injection can at most produce a patch that passes the gate and looks right to a reviewer; the remaining risk is in the reviewer's attention, which #359 (scope check) reduces.
- Legitimate fixes that must change a protected file (for example a CI failure caused by a workflow) stop and need a person.
- Open work: #358 (who may trigger), #359 (patch scope), #360 (entropy redaction), #361 (injection eval, needs approved API spend).
