# Reviewer Guide: Judging an Aegis Pull Request

For engineers who review the pull requests Aegis opens. Aegis never merges: every PR waits for a human, and your verdict is also what the platform's KPIs are computed from ([ADR 0005](adr/0005-kpi-thresholds-after-phase-1.md)). A typical review takes a few minutes, because the PR carries its own evidence.

## 1. Where PRs Come From

A PR is opened only after the job's **gate** has passed in a network-isolated sandbox: format, lint, the reproducing test (FAIL before the patch, PASS after), the full test suite, coverage, Sonar (when configured), and an integrity check that none of those steps rewrote the patch's files (so what passed is exactly what you review). If the gate cannot be passed within the job's ceiling ($4 or 40 model calls), no PR is opened; the job ends as `stopped_budget` or `stopped_steps` and explains why in a comment on the issue.

PRs are opened in the configured **fork** (a bot-owned fork, or the upstream repo itself), on a branch under `agent/` (e.g. `agent/demo-org-demo-1-…`), as drafts by default.

## 2. Reading the PR

The PR body has fixed sections (currently with Vietnamese headings; [example](../assets/screenshots/trial-pull-request.png)):

| Section | Heading in the PR | What to check |
| --- | --- | --- |
| Issue | `## Issue` | The link is the issue you expect. |
| Root cause | `## Nguyên nhân` | The explanation names the actual cause, not the symptom. If the cause is wrong, the fix usually is too, whatever the tests say. |
| Changes | `## Thay đổi (theo từng đoạn)` | One line per changed hunk (`file:lines — note`). Every hunk should follow from the root cause; unrelated edits are a reason to ask for changes. |
| Tests | `## Test` | *Test tái hiện*: the reproducing test that failed before the patch and passes after. Read it: does it test the behaviour the issue describes? *Test được phép đổi*: existing tests the patch was allowed to change, each with a reason. Any entry here deserves attention: it is a deliberate behaviour change. |
| Gate | `## Kết quả gate` | One mark per step: `✔` passed, `✘` failed, `–` skipped. A PR only exists if no step failed. |
| Cost | `## Chi phí` | Models used, number of calls, USD spent. |

If a gate reference (a stored expected output) was updated, the Tests section adds *Tham chiếu gate đã cập nhật, người duyệt cần xác nhận*: the files and tests whose reference changed. Confirm each one is an intended change.

Then read the diff as you would any colleague's. The dashboard's job page (`/jobs/<id>`) shows how the agent got there: every model call, tool call, escalation and gate run.

## 3. Asking for Changes: Review Rounds

When the PR is close but not right, let the agent revise it instead of editing it yourself:

- Submit a **Request changes** review on the PR, with inline comments where needed, **or**
- Comment `/aegis fix` on its own line (GitLab: on the merge request; you need Developer role or higher), **or**
- Run `aegis-job review <job>` ([client.md](client.md)).

The agent returns to the same branch, applies your comments, runs the gate again, pushes an additional commit (never a force-push), and replies to your comments on the PR. Only people with write access to the repo can trigger a round. A PR gets at most 3 rounds by default (`[reviews] max_rounds`), each with its own budget.

Write comments the way you would for a new colleague: say what is wrong and what you expect ("Keep the cut on a rune boundary"), and ask questions directly ("Why is `max <= 1` special?"); the agent answers them.

## 4. Recording Your Verdict

On the dashboard, open the job and click **Duyệt PR** (Review PR), or go to `/jobs/<id>/review`. The time between opening this page and submitting is recorded as the review time, so open it when you start reading the PR.

| Field | Meaning |
| --- | --- |
| *Kết luận* (Verdict) | **Chấp nhận** (Accepted): merged as is. **Chấp nhận sau khi sửa** (Accepted with edits): merged after you edited it. **Từ chối** (Rejected): not merged. |
| *Số dòng đã sửa* (Lines edited) | How many lines you changed yourself before merging. |
| *PR làm vỡ test cũ* (Broke existing tests) | Tick if the PR broke an existing test after all, e.g. found in CI after merge. This counts against the zero-regression KPI. |

A PR counts as **accepted** in the KPIs when it is merged as is, or accepted with at most 10 lines edited. Changes made by the agent in review rounds do not count as your edits. If you change your mind later, submit again with *Thay review cũ* (Replace previous review) ticked.

## 5. When to Reject

- The root cause is wrong, or the fix treats a symptom.
- The patch changes behaviour that the issue did not ask for, and a review round did not fix it.
- The reproducing test does not actually test the issue.

Rejecting is useful data, not a failure: rejected PRs and their jobs are what the team looks at to improve the agent.
