---
name: commit-and-merge
description: Commit the finished work in a repository's worktrees and merge the verified branches into its default branch, while keeping every locked-in branch in place. Locked-in means a locked worktree, a branch a live agent holds, a protected or archive name, or the repository's own list (git config gitpulse.lockedBranch, such as labs/**). A locked-in branch may be merged once it verifies, but is never deleted, rewritten or committed into. Use when asked to "commit and merge the branches", "merge everything into main", "land the lanes", or "merge but keep my locked branches". It never pushes and never deletes a remote branch.
license: MIT
compatibility: git 2.43+ (merge-tree --write-tree, merge-file --object-id), waylode-mcp on PATH, a repository trusted in Waylode, and the repository's own gate toolchain.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# Commit and merge

Commit what is finished, verify it, and merge it into the default branch. Locked-in branches are kept exactly as they are.

**What this skill never does:**
- push;
- delete, rename, reset, rebase or amend a remote branch or a locked-in branch;
- commit into a worktree that a live session holds;
- remove a worktree lock it did not create;
- kill a process.

The `waylode:wrap-up` skill runs this procedure as one of its steps. The `waylode:merge-prs` skill covers pull requests on the remote.

Pass the absolute `repo_path` on every `waylode_*` and `devmap_*` call.

## Repository settings

Settings live in the repository's local git config (under the `gitpulse.*` compat namespace). Every worktree of the clone shares them, and none of them is committed.

| Key | Meaning | Default |
|---|---|---|
| `gitpulse.lockedBranch` | Multi-valued. Each value is a branch name or a pattern under `refs/heads/`, such as `labs`, `labs/**` or `release/2026-*` (matching below). | none |
| `gitpulse.gate` | The full gate command, run from the root of the worktree under test. | discovered (below) |
| `gitpulse.trainWorktree` | Path of the persistent integration worktree. A sibling path when the gate needs sibling checkouts beside it. | `<repo>/.worktrees/merge-train` |
| `gitpulse.gateDiskGb` | Free disk, in GiB, a full gate run needs. | 30 |

- Read them with `git config --get-all gitpulse.lockedBranch` and `git config --get gitpulse.gate`.
- This skill never writes them. When a setting is missing and you had to ask for it, show the owner the `git config --local …` line that would save it.
- **Before using the default integration worktree,** run `git check-ignore -q .worktrees/`. If `.worktrees/` is not ignored, add `/.worktrees/` to the clone's own exclude file, `$(git rev-parse --git-path info/exclude)`, which is never committed, and say that you did. Never put it under `.gitpulse/worktrees/`: that directory belongs to Waylode's lanes.

**The default branch.** Take the first of these that exists:
1. `git symbolic-ref --quiet --short refs/remotes/origin/HEAD`, without the `origin/` prefix;
2. `git config --get init.defaultBranch`;
3. `main`, then `master`.

Otherwise ask.

**The full gate.** Take the first of these that names one, and say which source you used:
1. `gitpulse.gate`.
2. The repository's own instructions (`AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`) naming the command that must pass before merging.
3. A local CI script: package.json `ci:local`, `ci` or `verify`; a Makefile or justfile `ci`, `verify` or `check` target; `./verify.sh`.
4. The steps the CI workflow runs for the default branch.

If none, or more than one, fits, ask the owner. Never invent a gate. Running the tests you guess are relevant is not running the gate.

## Locked-in branches

A branch is **locked-in** when any of these is true:

1. **Locked worktree.** `git worktree list --porcelain` shows a `locked` line in its worktree's block. The reason follows the word; `initializing` means Waylode is still creating it.
2. **Held by a live agent.** Any one of these makes it held:
   - `waylode_sessions` or `waylode_insights` (`agents.live`) lists a session in its worktree;
   - `waylode_worktree_radar` shows a lane claim on it;
   - an `in_progress` card on `waylode_list_tasks` names it;
   - `lsof -d cwd` shows a process whose working directory is inside its worktree. Agents' build and test children count.
3. **Protected name.**
   - The default branch, plus `main`, `master`, `develop`, `staging`, `production`, `trunk` and `release/**`.
   - Waylode's work-in-progress patterns: `wip/**`, `draft/**`, `**/wip` and `**/draft`.
   - `archive/**`.
4. **The repository's list:** every `gitpulse.lockedBranch` value.

**Expanding patterns.** Expand the protected names and the repository's values the same way, with git's own matching. Never hand-roll a glob.
1. Normalise each value first: drop a leading `refs/heads/` or `heads/`.
2. Expand it with:
   ```sh
   git for-each-ref --format='%(refname:short)' "refs/heads/<value>"
   ```
   Run it once per pattern. The matching rules:
   - `*` stays inside one path segment: `labs/*` matches `labs/geek` but **not** `labs/geek/v2`.
   - `**` crosses `/`: `labs/**` matches both, and `**/wip` matches `wip` at any depth.
   - A value with no wildcard matches that branch **and everything under `<value>/`**: `labs` matches `labs/geek` and `labs/geek/v2`.

   So write a whole family as `labs` or `labs/**`, never `labs/*`.
3. For remote branches (pull request heads), expand the same values under `refs/remotes/origin/<value>` after `git fetch origin`.

**A repository value that matches nothing** may be a typo, and then a branch the owner meant to keep has no protection. Report it, and delete nothing in this run until the owner answers. Merging may go on.

**Fail closed.** A facet that could not scan (`ok: false`, `truncated`, `deferred`) leaves the branches it covers as unknown, and an unknown branch is locked-in for this run.

**Recompute the list mechanically** at the start, again right before each merge, and again before each deletion. Intersect it with what you are about to touch, and refuse on any hit. A prose rule in a brief is not a check.

**What locked-in means:**
- **Never removed.** Never deleted, renamed, reset, rebased, amended or force-moved. Its worktree is never removed or pruned, and its lock is never removed.
- **Never committed into** by this skill. Its uncommitted work belongs to its holder.
- **Merged only when it verifies**, like any other branch. Merge it at the commit pinned when the plan was shown, never at a ref that may have moved since, and list it in its own group of the plan so the owner sees each one. Merging reads the branch and never changes it.
- **Held by a live agent:** merged only after its session has finished or gone idle, with its tip unchanged since the plan. Otherwise it is deferred to the next run, with the reason.

**Edge cases:**
- **Already merged.** Keep it, and don't call it stale.
- **Behind the default branch but not ahead.** There is nothing to merge; keep it.
- **A worktree on a detached HEAD with commits that are on no branch** (`git branch --contains <sha>` is empty). Treat it as locked-in and name its sha in the report. Never remove it.
- **A branch checked out in more than one worktree, or in the main checkout.** It is in use, so it is held.
- **A stale lock whose holder is gone.** It is still locked. Tell the owner; only they unlock it.
- **Branch names that start with `-` or contain unusual characters.** Pass full refs (`refs/heads/<name>`) and `--` before paths.

## Before you start

1. **Orient.** Call `waylode_insights` and `waylode_sessions`. Check each facet's `ok`: a facet that could not scan has not come back clean.
2. **Cross-process serialization.** Waylode's Rust engine (`waylode_worktree_merge`, `waylode-worktree merge`, and `LandQueue`) automatically serializes ref and checkout mutations across processes via a kernel-released `flock` at `"$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/mutation.lock"`, and coordinates hot-file conflict resolution via sorted-path SQLite WAL FIFO tickets (`<common>/gitpulse/land_tickets.sqlite`).
   - **When a calling skill** (`waylode:wrap-up`, `waylode:merge-prs`) coordinates a multi-step external gate run, keep its outer coordinator lock if used; otherwise do **not** create a manual `mkdir .../merge-train.lock` directory just to invoke `waylode_worktree_merge` or `LandQueue` (directory locks leak on `SIGKILL`, whereas `mutation.lock` auto-releases on crash).
   - **Look for an integrator operating outside Waylode's engine:**
     - entries in the default branch's reflog from the last few hours that this session did not make;
     - an integration branch (`train/*`, `integrat*`) whose tip moved in the last hour;
     - peers that `waylode_agent_peers` (or, in Claude Code, `ListAgents`) name as integrating.

     Message each one (`waylode_agent_send`) and agree on one integrator before doing anything.
3. **Check host load and disk.** Follow the `waylode:load` skill. Free disk must be at least `gitpulse.gateDiskGb`.
4. **Resolve.** Settle the default branch, the gate and the locked-in list, as above.

## 1. Inventory

For every local branch and every worktree, record:
- ahead and behind against the default branch (`git rev-list --left-right --count <default>...<branch>`);
- the age of its tip;
- its uncommitted file count;
- its locked-in reasons;
- the status of any card that names it;
- whether its tip has landed: `waylode_landed` with `commit: <tip>` and `base: <default>`. That check also recognises squash and rebase merges.

Put each branch in the **first** class that fits:

| Class | Meaning |
|---|---|
| integration | `train/*` and other branches a merge train or integrator built. They are never candidates. Keep the newest per integrator; the rest are listed for the owner. |
| unstarted | 0 ahead, and no commit was ever made on it: `git reflog show refs/heads/<branch>` has exactly one entry, its creation. Count the entries rather than reading the message: a branch made with `git update-ref` has an empty one. If there is no reflog at all, it is unknown: keep it. A lane that has not begun is never landed and never pruned. |
| landed | `waylode_landed` says `landed: true`, and the branch did carry commits of its own. Nothing to merge. |
| candidate | Ahead and not deferred. |
| deferred | A live holder is still working, it conflicts, or it touches files a sibling lane holds (`waylode_collision_risk`). Record the reason. |

## 2. Commit

**Where you commit.** Only in a worktree that meets all of these:
- its branch is not locked-in, for any of the four reasons;
- no live session holds it;
- none of its changed files was modified in the last 15 minutes. A session the tools cannot see may still be writing.

**Everywhere else:**
- For a worktree a session holds, ask that session to commit (`waylode_agent_send` or `waylode_worktree_send_message`) and wait for its reply. Never commit for it.
- For a worktree with recent changes and no visible holder, ask the owner.
- The default branch's own checkout: never commit there. The owner may name files to commit there, and then you commit only those, by path.

**In each worktree you commit in:**
1. Read `git status --porcelain=v1 -z` and the diff. Group the changes by concern: one coherent concern per commit.
2. Stage by path only: `git add -- <paths>`.
   - Never `git add -A`, `git add .` or `git add -u`. A worktree can carry another session's files.
   - Never stage `.env*`, keys, credentials, or build output. Leave untracked files that look generated or secret where they are, and list them in the report.
3. Run the repository's quick checks for what changed (format, type check, the touched tests) before committing.
4. Write the message in the repository's own style (read `git log`): what changed and why.
5. Never switch branches in a worktree that still has uncommitted changes.

## 3. Plan, then get one yes

Show a table with one row per branch:
- branch;
- ahead and behind;
- class;
- locked-in reasons;
- the pinned commit (the tip sha now);
- the proposed order.

Order the candidates:
- a branch that contains another goes after it;
- otherwise the smallest and cleanest go first.

Locked-in candidates get their own group. Wait for the owner's yes, and take the plan as they edit it. Nothing merges without it.

## 4. Verify

- **Pre-check every merge** against what the integration branch holds now: `git merge-tree --write-tree --name-only --messages HEAD <sha>`. On a conflict, leave that branch out and record why. Never resolve conflicts in a branch you don't own.
- **Gate the candidates:**
  - **Two or more:** run them through the `waylode:merge-train` skill with this batch. You already hold its lock, so skip its lock step.
  - **One:** in the integration worktree, merge the pinned commit onto the default branch's tip with `git merge --no-ff --no-edit <sha>`. Then run the full gate there and compare it with a base run of the default branch, using merge-train's compare and confirm steps.
- **Review each candidate's diff** (`<merge-base>..<sha>`) with the strongest model the host has. In Claude Code that is an `Agent` with `model: "opus"`. Give the reviewer the brief, the branch's card if it has one, and `devmap_impact` on the symbols the diff changes. Ask for correctness bugs, regressions, and tests that would not fail without the change.
- **Record the verified result:** the commit, and its tree from `git rev-parse <commit>^{tree}`. Waylode's verification gate and `LandQueue` compare `^{tree}` equality (and verified lands best-effort advance `refs/gitpulse/green/<default>` monotonically via CAS).
- **A branch that regresses** or fails review is not merged. Report its failing tests or findings. This skill never deletes it; `waylode:wrap-up` decides that.

## 5. Land

1. **Primary path — Rust `LandQueue` & `merge_tree::land` (`waylode_worktree_merge` / `waylode-worktree merge`):**
   - Prefer Waylode's Rust landing engine over manual plumbing. It handles:
     - **Batched single-quarantine landing (`LandQueue`):** coalesces concurrent lands (`10ms` window, AIMD sizing) into one `ObjectQuarantine` + one `git pack-objects` migration + one atomic CAS `update-ref`, isolating conflicting lanes without losing clean lanes (`CompleteEarly` safety proof, `split_dependents`, and sequential solo fallback on hook rejection).
     - **Idempotent land identity (`land_identities`):** persists `(target_ref, source_oid) -> landed_oid` in `.devcouncil/ledger.sqlite` with `revertOf` (including revert-of-revert) awareness so retried lands return `AlreadyLanded` cleanly.
     - **Cross-process `mutation.lock` & disjoint-dirty advancement:** under `<common>/gitpulse/mutation.lock` (`flock`), `merge_tree::land` moves `refs/heads/<default>` by CAS and advances every checkout holding `<default>` via `git read-tree -m -u` — allowing strictly disjoint uncommitted/staged/untracked edits to remain untouched while failing closed on all 5 data-loss guards (APFS case-folding, `symlink_metadata == NotFound` for added/type-changed paths including gitignored files, `160000` gitlink refusal, untruncated dirty scan, per-holder `repo_op`/`index.lock` checks, and `pre_cached_digest` CAS rollback).
     - **Verified green pointer:** when verified in pooled CoW scratch (`<common>/gitpulse/verify-scratch/`), best-effort advances `refs/gitpulse/green/<default>` without failing an already-landed merge.
2. **If `waylode_worktree_merge` is unavailable or `refs/heads/<default>` moved since the verified run:**
   - Re-verify if `git rev-parse <new>^{tree}` differs from the verified tree.
   - If landing manually without `LandQueue`:
     - **Checked out nowhere:** `git update-ref refs/heads/<default> <new> <old>`.
     - **Checked out in a clean or strictly disjoint-dirty worktree:** `git merge --ff-only <new>` in that worktree.
3. **If git or `merge_tree::land` refuses** because dirty files overlap the landing (`HeldReason::CheckoutDirty`), stop. Show the owner the overlapping paths and whose session holds them. Land only after the holder commits, or after the owner explicitly approves an emergency manual three-way landing.

   **Emergency / Manual Fallback Only — Three-way landing (`git merge-file`):** Use this manual procedure *only* as an emergency fallback when `waylode_worktree_merge` / `LandQueue` refuses an overlapping dirty file and the owner explicitly approves merging their uncommitted edit in place. Run it in the checkout that has the default branch, with `<old>` as its current tip. Give git every path with `--literal-pathspecs`, so a name like `docs/[x].md` cannot also match `docs/x.md`.
   1. **List both sides.** The dirty paths: `git status --porcelain=v1 -z -uall --no-renames`, which includes untracked files. The landed paths: `git diff --no-renames --raw -z <old> <new>`, which also gives each path's old and new mode.
   2. **Refuse** unless every overlap is a regular file that both sides modified and nothing staged. Refuse if:
      - an overlapping path has staged changes (`git diff --cached --no-renames --name-only -z`);
      - the landing adds, deletes or changes the type or mode of an overlapping path;
      - either side of an overlap is a symlink or a submodule (mode `120000` or `160000`);
      - a path the landing adds already exists on disk as a file, a directory or a symlink, even an ignored or dangling one (`test -e <path> || test -L <path>`);
      - a dirty path and a landed path differ but one is a directory prefix of the other (`foo` and `foo/bar`).
   3. **Save every dirty file** as a blob with `git hash-object -w -- <path>`, and keep the list. It proves later that nothing else changed, and it is what you restore from.
   4. **Merge each overlapping file** into a temporary file. Never merge in place, because a conflict writes markers into the holder's file:
      ```sh
      git merge-file -p --object-id <saved blob> <old>:<path> <new>:<path> > <tmp>
      ```
      A non-zero exit is a conflict: delete the temp files and stop.
   5. **Check, then write.** Each overlapping file must still hash to its saved blob. Then copy each temp file into it with `cat <tmp> > <path>`, which keeps the file's mode, and delete the temp file.
   6. **Write the other landed paths** with `git --literal-pathspecs restore --source=<new> --worktree -- <paths>`. It creates added files, removes deleted ones, and sets modes and symlinks, none of which `git show` does.
   7. **Move the ref last,** with the compare-and-swap. If it fails, someone else moved the branch. Put every file back and stop:
      - the merged files with `git cat-file blob <saved blob> > <path>`;
      - the paths the landing added, which `<old>` does not have, with `git --literal-pathspecs clean -f -- <paths>` (`restore` refuses a path missing from its source, and then restores nothing);
      - the rest with `git --literal-pathspecs restore --source=<old> --worktree -- <paths>`.
   8. **Line up the index** with `git --literal-pathspecs reset -q -- <every landed path>`. Otherwise the holder's next commit would revert the landing.
   9. **Prove it.** Every dirty file outside the landing still hashes to its saved blob, and `git status` lists only the holder's own changes.

   This is the one place this skill moves a ref that another worktree has checked out. It needs the owner's yes every time.
4. **Never push.** Say that the default branch moved locally, and by how many commits.

## 6. Tidy what this run merged

Recompute the locked-in list first. Then, for each branch merged in this run that is **not** locked-in:
1. Confirm with `waylode_landed` that its tip is on the default branch.
2. Take a create-only backup ref: `git update-ref refs/backup/<yyyymmdd-hhmm>/<branch> <sha> ""`. The empty old value makes it fail rather than overwrite an existing ref.
3. **If it has a worktree,** call `waylode_worktree_prune` with `branch`: first with `dry_run: true`, then without it. Read each worktree's own outcome in `outcomes`.
   - The tool skips a worktree with a live process, a lane claim or a live session, and those stay.
   - Never pass `force`. It also discards uncommitted files.
   - If the tool skips a clean worktree only because of its own integration check (a squash or rebase landing that `waylode_landed` recognised), `git worktree remove <path>` without `--force` is the fallback. It refuses a dirty or locked worktree.
4. **If no worktree is left,** delete the branch with `git update-ref -d refs/heads/<branch> <backed-up sha>`. Check first that `git worktree list --porcelain` has no `branch refs/heads/<branch>` line: `update-ref -d` does not look, and deleting a checked-out branch leaves that worktree on an unborn branch. That is a compare-and-swap, so it fails if the branch moved. Do not use `git branch -d`: it checks the branch's upstream rather than the default branch, and refuses squash-landed work.

Locked-in branches stay, merged or not.

## 7. Report

Write `commit-and-merge-<stamp>.md` to `"$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/reports/"`. Never write it into a tracked path of the main checkout. It holds:
- the commits made, with their worktrees;
- the plan as approved;
- what merged, with the default branch's old and new tip;
- what was deferred or left out, and why;
- the locked-in branches, each with its reason;
- the backups and deletions;
- each check that was not run, and why.

Then remove the lock, and tell the owner the same in a few lines.
