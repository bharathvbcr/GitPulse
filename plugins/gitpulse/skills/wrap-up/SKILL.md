---
name: wrap-up
description: Wind a repository's work down cleanly. Lets every running agent finish its current task and stop, fixes the critical bugs found, reviews and verifies every branch's changes with Opus (or the host's strongest model), merges what passes into the default branch while keeping locked-in branches, prunes stale and regressed branches after backing them up, and brings the GitPulse board up to date, verifying each finished card's integration and filing the remaining non-critical gaps as tasks. Use when asked to "wrap up", "wind everything down", "land everything and clean up", or "close out the session's work". It never pushes, never kills a session, and never deletes a locked-in branch.
license: MIT
compatibility: git 2.43+, gitpulse-mcp on PATH, a repository trusted in GitPulse, the repository's own gate toolchain, and gh for the optional pull request step.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# Wrap up

Wrap-up runs the other skills in order. Each step names the skill that owns it; follow that skill and don't restate it here.

| Step | Owner |
|---|---|
| Locked-in branches, repository settings, commit, verify, land, tidy | `gitpulse:commit-and-merge` |
| Batch verification | `gitpulse:merge-train` |
| Pull requests | `gitpulse:merge-prs` |
| Cards | `gitpulse:tasks` |
| Host load | `gitpulse:load` |

Pass the absolute `repo_path` on every `gitpulse_*` and `devmap_*` call.

**What wrap-up authorizes,** once the owner has said yes to its plan (step 3):
- merging the verified branches into the **local** default branch;
- deleting local branches that have landed, or that verification confirmed as regressed, each after a backup ref.

**What still needs the owner:**
- any push or remote-branch deletion;
- merging pull requests (`gitpulse:merge-prs` asks its own yes);
- each item under "Stop and ask" below.

## Roles

| Role | Claude Code | Other hosts |
|---|---|---|
| Fix and commit | `Agent` with `model: "sonnet"` and `isolation: "worktree"` | the host's fast coding model |
| Review and verify | `Agent` with `model: "opus"`, or you if you are Opus | the host's strongest model |
| Judgment calls | the `advisor` tool if present, else the review model | the strongest model |

- Run at most 4 workers at once.
- Reviewers read and decide. They never write code or run git writes.
- Every worker brief names DevMap and GitPulse Insights as its first tools, and states:
  - the files in scope;
  - the acceptance criteria;
  - the rule that it commits by path in its own worktree and never pushes.

## Stop and ask the owner before

- destructive data or schema migrations;
- breaking a public API, CLI or MCP contract;
- major dependency upgrades;
- force-push or history rewrite;
- any push or remote delete;
- deleting a branch that has neither landed nor been confirmed as regressed, or one whose worktree has uncommitted files;
- deleting or merging a card a person wrote;
- anything a card lists under owner decisions.

## 0. Settle one integrator

Take the integration lock and look for other integrators, exactly as `gitpulse:commit-and-merge` "Before you start" says. Hold the lock for the whole wrap-up and release it on every exit path. The skills wrap-up calls see that you hold it and skip their own lock step.

## 1. Wind down the running agents

1. **Find every session.**
   - `gitpulse_sessions` and `gitpulse_insights` (`agents.live`).
   - `gitpulse_agent_peers`.
   - Inside a GitPulse terminal, `gitpulse_terminal_list` for your own children.
   - In Claude Code, `ListAgents`.

   A facet with `ok: false` means sessions you cannot see. Say so.
2. **Your own children:**
   1. Tell each one to finish its current task, commit it by path, and stop (`gitpulse_terminal_send`).
   2. Wait with `gitpulse_terminal_wait`.
   3. Read its last output with `gitpulse_terminal_read`, then call `gitpulse_terminal_close`.
3. **Sessions you did not start:**
   1. Message each one (`gitpulse_agent_send`; `SendMessage` in Claude Code). The first line should read: "Wrap-up: finish your current task, commit it by path, reply with your branch and head sha, and take no new work."
   2. Wait for replies with `gitpulse_agent_wait`.
   3. Re-check `gitpulse_sessions` for state `done` or `idle`.
   4. Bound the wait: 30 minutes by default, or what the owner says.
4. **Never kill a session.** No `kill`, no `pkill`, no closing a terminal you do not own.
   - A session that does not answer, that cannot be messaged (desktop app sessions, sessions under another config directory), or that is still working when the wait ends keeps its branch locked-in for this run.
   - Name each such session in the report.

## 2. Inventory and commit

Follow `gitpulse:commit-and-merge` steps 1 and 2: the inventory (with its unstarted and integration classes), the locked-in list, and commits in the worktrees whose sessions have finished. Also gather the critical-bug sources for step 4:
- a red gate on the default branch's base run;
- board cards with critical severity;
- `in_progress` cards whose sessions stopped mid-task.

If the owner asked for pull requests too, run `gitpulse:merge-prs` now, before anything lands locally. Then `git fetch origin`.
- **The local default branch has no local-only commits** (`git rev-list --count origin/<default>..<default>` is 0): fast-forward it to `origin/<default>`.
- **It has unpushed commits:** don't move it. Make the train's base a merge of `origin/<default>` into the local default, built in the integration worktree, and put that merge first in the plan. Then everything this run verifies already contains the merged pull requests.

## 3. Plan, then get one yes

Show the plan as `gitpulse:commit-and-merge` step 3 does: every candidate with its pinned sha and order, with the locked-in candidates in their own group. Add two more groups:
- **Prune:** the landed branches that will be removed, each after a backup ref.
- **The rule for regressed branches:** a branch that verification confirms as regressed is left out of the merge and deleted after a backup ref. The exceptions are a locked-in branch, a dirty worktree, and a branch whose card is `in_progress`; those are only reported.

Wait for the owner's yes, and take the plan as they edit it. Everything below follows the approved plan. A branch that changes after this point gets re-pinned and shown again.

## 4. Fix critical bugs

**Critical** means data loss, a security hole, a crash or hang on a normal path, a broken build or gate on the default branch, or a regression one of the branches introduced. Everything else becomes a card in step 8.

For each critical bug:
1. **Brief it** (review model): the root cause with `file:line` evidence, the files in scope, and a test that fails before the fix.
2. **Fix it** (worker) on a new branch, test first.
   - When the bug lives on a candidate branch, start the new branch from that candidate's pinned sha (`fix/<candidate>-<slug>`). An isolated worker cannot check out a branch that is checked out elsewhere, and the candidate's own worktree belongs to its session.
   - Then add the new branch to the plan, right after the branch it fixes.
3. **Review the fix** (review model). Approve it, or send it back with reasons.

Show the owner the plan again with the fix branches added.

## 5. Review and verify every branch

**Review.** For every candidate branch, the review model reads the diff (`<merge-base>..<pinned sha>`) against the branch's card and brief, with `devmap_impact` on the changed symbols. Ask it for:
- correctness bugs;
- regressions;
- tests that would not fail without the change;
- edits to files another lane holds.

Each finding is critical (step 4, then re-review) or not (a card in step 8).

**Verify** the approved batch with `gitpulse:commit-and-merge` step 4. That is `gitpulse:merge-train` for two or more branches, and you already hold its lock. It finds the branch behind each new failure and confirms it on a re-run.

**Regressed** means one of two things:
- merge-train confirmed the branch introduced a failure: it fails on the batch, passes on the default branch, and does so twice;
- the branch carries a critical finding that could not be fixed in this run.

A load flake is not a regression.

## 6. Merge

1. Land the verified train as `gitpulse:commit-and-merge` step 5 says.
2. Afterwards, run the full gate once on the new default tip, unless its tree equals the verified tree.

## 7. Prune

Recompute the locked-in list right before this step. A locked-in branch is never pruned, whether it is stale, landed or regressed. Unstarted and integration branches are never pruned either.

**For each other branch in the approved Prune group, and each confirmed regressed branch:**
1. **Back it up** with a create-only ref: `git update-ref refs/backup/<yyyymmdd-hhmm>/<branch> <sha> ""`.
2. **Remove it:**
   - **Landed:** use `gitpulse:commit-and-merge` step 6.
   - **Regressed:**
     1. First file or fold the card from step 8, naming the failing tests and the backup ref.
     2. If its worktree has uncommitted files, or its card is `in_progress`, stop here and only report it.
     3. Otherwise remove its worktree with `git worktree remove <path>`, without `--force`. It refuses a dirty or locked worktree.
     4. Then delete the branch with `git update-ref -d refs/heads/<branch> <backed-up sha>`.
     5. Never call `gitpulse_worktree_prune` with `force`: it discards uncommitted files.

**Stale branches** (unmerged, no recent commit, no card, no session) and **regressed branches you only reported** are listed for the owner with their ahead count and last commit. They are not deleted.

**Worktree admin entries:**
1. Read `git worktree list --porcelain` for entries marked `prunable`, whose directories are gone.
2. For each one on a detached HEAD whose commit is on no branch (`git branch --contains <sha>` is empty), take a backup ref first, or the commit becomes unreachable.
3. Then run `git worktree prune --dry-run`, and then `git worktree prune`. It never touches locked entries.

**Remote branches** are only listed, never deleted.

**Your own leftovers.** Remove only the scratch worktrees, logs and build output this run created. Keep the integration worktree for the next train.

## 8. Update the board

Follow `gitpulse:tasks` for every write: list first, fold into an existing card before filing a new one, and group related findings.

**Finished cards.** For each card whose branch merged:
1. Check the integration on the default branch:
   - `gitpulse_landed` for its commits, against `origin/<default>` for pull request merges;
   - each acceptance criterion re-read in the source on the default branch, not on the branch;
   - its tests run there.
2. If all of that holds, call `gitpulse_complete_task` with a summary of what landed and how it was verified. Done is evidence-gated, so the card may land in review. Say which.
3. If a criterion is not met on the default branch, the card stays open with a note saying which one.

**Other cards:**
- **Regressed branches:** fold into the branch's card, or file one, with the failing tests, the backup ref, and the reviewer's findings.
- **Non-critical gaps and bugs** from review, verification and the fixes: one card per root cause, with `file:line` evidence, a failure scenario, severity and priority, and testable acceptance criteria.
- **Integration gaps:** a merged card whose feature is not reachable (no caller, not wired into the registry, a missing permission grant) gets a card of its own.

## 9. Report

Write `wrap-up-<stamp>.md` to `"$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/reports/"`. Never write it into a tracked path. It holds:
- **Sessions:** the ones wound down, and the ones that could not be, with why.
- **Fixes:** the critical fixes, with their commits and tests.
- **Reviews:** each branch's review verdict and findings.
- **Merges:** what merged, and the default branch's old and new tips.
- **Kept:** the locked-in branches and their reasons; what was deferred or left out, and why.
- **Deleted:** each deleted branch with its backup ref.
- **Listed for the owner:** the stale branches, and the regressed branches that were only reported.
- **Cards:** the ones moved, with their status, and the ones filed or folded, with ids.
- **Not run:** each check that was not run, with why.

Release the lock. Then tell the owner the same in a few lines, and say plainly that nothing was pushed.
