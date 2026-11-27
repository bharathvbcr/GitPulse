---
name: merge-train
description: Verify several branches together with ONE full gate run instead of one per branch, in any repository. Merges the batch onto a persistent integration worktree, runs the repository's own full gate once, compares individual failures against the default branch (which is often already red), re-checks suspected failures alone, and bisects with only the failing tests to name the branch that broke them. Use when two or more finished branches or lanes are waiting to merge, when asked to "verify these branches together", "run a merge train", or "check what this batch breaks". Produces a verified train branch and a report; it never lands, pushes or deletes anything.
license: MIT
compatibility: git 2.38+ (merge-tree --write-tree), Go 1.27.4+ (the version failset/go.mod names) to build the failset tool, and the repository's own gate toolchain.
metadata:
  agent-plugins: "1.0.0"
---

# Merge train

One full gate run per **batch**, not per branch.
- A batch of N branches that verifies clean costs one base run plus one batch run.
- A batch with a culprit adds about log N runs of **only the failing tests**, then one full run of what is left.

Below, **main** means the repository's default branch, worked out as `waylode:commit-and-merge` describes.

**What this skill does not do:**
- It does not land, push, delete branches or remove worktrees. It ends with a `train/<stamp>` branch and a report, and the owner decides what lands. The `waylode:commit-and-merge` and `waylode:wrap-up` skills land a verified train.
- It never runs in a repository's main checkout. Other sessions build and edit there, so a gate run there measures their work too.

## Before you start

1. **Orient.** Call `waylode_insights` for the repository. Check each facet's `ok`: a facet that could not scan has not come back clean.
2. **Choose the batch with the owner.** List candidate branches with their ahead/behind counts against main.
   - Leave out branches whose tip moved in the last hour, or whose worktree has a live agent session. Those are live work, not finished work.
   - A branch 0 ahead / 0 behind with its own fresh worktree is an unstarted lane, not a leftover.
   - Show the list and get a yes before merging anything. When `waylode:commit-and-merge` or `waylode:wrap-up` called this skill, the batch is the plan the owner already approved.
3. **Coordination & locks.**
   - Waylode's Rust landing engine (`LandQueue`, `waylode_worktree_merge`, `waylode-worktree merge`) already serializes cross-process ref/checkout mutations with a kernel-released `flock` at `"$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/mutation.lock"` and coordinates concurrent conflict resolution via `<common>/gitpulse/land_tickets.sqlite`.
   - For multi-step external gate runs orchestrated by this skill, keep a lightweight train coordinator lock only if needed by the caller (`waylode:commit-and-merge`, `waylode:merge-prs`, `waylode:wrap-up`), and prefer Waylode's pooled CoW scratch worktree (`<common>/gitpulse/verify-scratch/`, which preserves `target/` and `node_modules/` via APFS CoW cloning and `git clean -fdx -e target -e node_modules`) or `gitpulse.trainWorktree` over the main checkout.
   - If a legacy directory lock `merge-train.lock` exists from an older script and `kill -0 <pid>` fails on its `owner` file, tell the owner it is stale; only they remove it.
4. **Check the disk.** `df -h .` must show at least `gitpulse.gateDiskGb` free (30 GB when unset). Below it, stop and report.

## The repository's gate

Read the gate, the integration worktree and the disk floor from the repository settings in `waylode:commit-and-merge`:
- `gitpulse.gate`, else the gate the repository's own instructions or CI scripts name;
- `gitpulse.trainWorktree`, default `<repo>/.worktrees/merge-train`. Ignore that path through the clone's `info/exclude` first, as that skill says.
- `gitpulse.gateDiskGb`.

Before the first train in a repository, read the gate script itself and answer these. Write the answers into the report.

- **Does it stop at its first failing step?** If it has a continue-past-failure flag (`--continue`, `--no-fail-fast`, `-k`), use it on both the base and the batch run.
  - Without one, every step after the first red step runs on neither tree. `compare` reports that as `partial`; see step 5.
- **Does it need sibling checkouts?** For example, a `go.mod` `replace` or a script that reads `../<other repo>`.
  - If so, the integration worktree must sit beside them: set `gitpulse.trainWorktree` to a sibling path.
  - Pin each sibling at the commit the gate expects, usually the one its CI setup step names.
- **Does it write outside its own tree?** For example, it builds a binary into a sibling checkout.
  - If so, base and batch runs must run one after the other, never at the same time.
- **What does it need on PATH or prebuilt?** For example, a helper binary its tests call, or a tool a step skips without.
  - Prebuild or install those before the base run, so a fresh worktree does not time out building them inside a test.
- **How big is its build output?** Coverage and release targets can reach tens of GB. Delete them between runs when disk is short, and say so.

**Logs** go to `"$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/merge-train/"`. That path is inside git's own directory, so it is never tracked in any repository. Never write logs into the main checkout.

**Run every gate in the background.** A full gate can take most of an hour, so wait for its completion notice rather than polling it.

## Procedure

### 1. The integration worktree (once per repository, then reused)

- From the repository root, run `git worktree add --detach <path> main`. If it already exists, keep it: its warm build output saves a cold build every train.
- Do the rest of the train **from inside that worktree**: enter it with `EnterWorktree` by path, or brief a subagent whose working directory is that worktree.
  - Command gates can refuse `git -C <other worktree>` and `cd` into one.
  - Never move another checkout's refs with plumbing (`update-ref`, `commit-tree`).

### 2. Build the comparison tool

```bash
LOGS="$(git rev-parse --path-format=absolute --git-common-dir)/gitpulse/merge-train"
mkdir -p "$LOGS"
go -C <this skill's directory>/failset build -o "$LOGS/failset" .
```

`failset compare <base-log> <batch-log>` exits:
- **0** clean;
- **1** new failures;
- **2** cannot compare: a log has no verdict, because it was killed, ran out of disk, or is still running;
- **3** partial: no new failures where it could compare, but something could not be compared.

**Supported gate output.** `failset` can only compare gates whose output it recognises. For any other gate it exits 2, and the compare is done by hand: collect each log's failing test ids and diff the two sets. Say in the report that you did.

**The summaries it reads:**
- a `ci-local.mjs`-style summary block, in its default, `--continue` and `--only` forms;
- a `verify.sh` that prints one line per step;
- a `local-ci.js` runner.

**The runners it reads inside those:** nextest, libtest, vitest, `go test`, pytest, Playwright and the Waylode browser harness.
- Where a gate prints a summary block, its rows are the verdict for every step, including a nested block such as a `verify.sh` that the outer runner calls last.
- A step is one of:
  - passed;
  - failed;
  - skipped (it could not run);
  - not-applicable (it had nothing to do here, such as a release-tag step with no local tag).
- A gate whose output matches none of these is `cannot compare` (exit 2). Compare its failing test names by hand, and say so in the report.

### 3. Base run: main as it is now

1. `git switch --detach main`.
2. Write a versions header into the log:
   - `git rev-parse HEAD`;
   - the version of every toolchain the gate uses (`bun --version`, `cargo --version`, `go version`, and the helper binaries it calls);
   - the commit of every sibling checkout it reads.
3. Append the gate's output to the same log: `$LOGS/base-<main-sha12>.log`.

**Reusing a base log:** an existing base log can stand in only when:
- its sha **and** versions header match;
- `failset extract` exits 0, meaning it is complete;
- no gate in it is listed as `incomplete`.

Otherwise run it again. A base log from a different main, from different tools or from different siblings is not this base.

### 4. Batch run

1. `git switch -C train/<yyyymmdd-hhmm> main`.
2. For each branch in the agreed order:
   - Check it first against what the train branch holds **now**: `git merge-tree --write-tree --name-only --messages HEAD <branch>`.
   - Never pre-check two branches against each other with `--merge-base=main`. A branch that is behind main then appears to revert main's newer commits, which shows a conflict that a real sequential merge does not have.
   - On a conflict, **leave that branch out** and record why; don't resolve conflicts on a branch you don't own.
   - Otherwise `git merge --no-ff --no-edit <branch>`. When the batch was pinned to commits, merge the pinned sha, not the moving branch name.
3. A clean merge can still hide a semantic conflict. Run the cheapest compile or type check the repository has first (`cargo check --all-targets`, `go build ./...`, `tsc --noEmit` or its wrapper), and stop there if it fails.
4. Run the full gate into `$LOGS/batch-<stamp>.log`.

### 5. Compare

Run `"$LOGS/failset" compare <base-log> <batch-log>`, and read every section, not just the verdict:

- **NEW FAILURES:** failures the batch has and main does not.
- **MASKED:** the gate already failed on main with no per-test detail (fmt, clippy, lint, a coverage floor). A new failure inside it cannot be told apart. Compare that gate's own error lines by hand and report what you found.
- **NOT COMPARED:** a gate that ran in only one log, one that was skipped in both, or one whose test runner stopped part way. This is not a pass. Name each one in the report.
  - A gate skipped on this platform in both logs keeps the verdict at `partial`: the batch's effect on it is unknown.
  - An item marked "failed on the batch; the base never ran it" is a batch failure with nothing to compare it to, because the base stopped first. Treat it like a new failure: confirm it on main (step 6) before landing anything.
- **FIXED:** failures main has and the batch does not.

**When main stops a stop-at-first-failure gate early.** If that first red step is red on main, everything after it runs on neither tree, and `compare` says so (`partial`, "every step after the first failure"). The steps after it may be exactly what the batch changed. In that case:
1. Run the steps after the red one yourself, on the batch tree, past failures.
   - Read the step list from the gate script itself, with the environment it sets.
   - Give each test runner its own keep-going flag: cargo `--no-fail-fast`, `go test` across all packages, vitest without `--bail`. Without it, cargo stops at the first failing test binary, and the crates after it never run.
   - Check which crates, packages and steps actually ran, from the runner's own lines, before calling anything covered.
2. A pass on the batch cannot be a new failure, so a run of only the batch is enough when everything passes. A batch failure there still needs the same steps run on main.
3. Say in the report that these steps were run by hand, not by the gate.

The lasting fix is a continue-past-failure mode in the gate itself. Name its absence in the report.

### 6. Confirm before blaming

Machines running several agents are often loaded, and load-sensitive tests fail on their own. Re-run each new failure **alone**, on the batch tree and on a detached main checkout of the same worktree:

| Runner | Re-run one test |
|---|---|
| nextest | `cargo nextest run [--manifest-path <crate>/Cargo.toml] -E 'binary_id(<binary>) & test(=<name>)'` (the binary is the id's first word) |
| cargo test | `cargo test [-p <crate>] <name> -- --exact` |
| vitest | the repository's test script with `-- <file> -t '<name>'`. Check that the `RUN` line names the integration worktree, not another checkout. |
| go | `go test -run '^<Name>$' <package>` |
| pytest | `pytest '<file>::<name>'` |
| a gate step | the gate's own single-step mode, if it has one (`--only <step>`, `--leg <name>`) |

- **Fails on the batch and passes on main, twice:** the batch introduced it.
- **Fails on both:** it belongs to main, and the base run missed it as a flake. It is not the batch's.
- **Passes on both:** a load flake. Report it and don't blame a branch for it.
  - Then run the failing test's whole binary or package a few times on both trees. A test can pass alone and fail only beside the others in its process.

**Scripting these re-runs:** read the runner's own result lines (`test result: ok. 1 passed`, `--- PASS`), never a pipeline's exit status. `(cargo test … | grep …)` returns grep's status, so a script built that way reports "pass" whatever the test did.

### 7. Bisect with only the confirmed failures

1. Split the batch in half, merge the first half onto main in the same worktree, and re-run **only** the confirmed failing tests. Do not re-run the full gate.
2. Recurse into the half that fails. With one branch left, that branch is the culprit.
3. If both halves pass and the whole batch fails, the failure needs branches from both halves together. Report the pair; don't name either alone.
4. Leave the culprits out, rebuild `train/<stamp>` from the rest, and run the **full** gate once more.
   - The reduced batch is a different tree, so the earlier batch run does not vouch for it.
   - Compare against the same base log.

### 8. Report and stop

Write `$LOGS/report-<stamp>.md` with:
- the batch as agreed, the branches left out for conflicts, and the culprits with their failing tests;
- the final verdict and every MASKED and NOT COMPARED item;
- the gate facts from "The repository's gate";
- the wall time of each run.

Then tell the owner:
- the train branch, and that nothing landed;
- what verified clean;
- what could not be compared, and why.

Remove the lock, unless a calling skill holds it. Leave the integration worktree for the next train, apart from build output you deleted for disk.

## What is built in the Rust engine vs external gates

- **Landing & verification pointers (`LandQueue` + `refs/gitpulse/green/`):** Waylode's Rust `LandQueue` performs single-quarantine batched landing, cross-process `mutation.lock` serialization, disjoint-dirty target advancement (`read-tree -m -u` with 5 safety guards), `land_identities` idempotency (`revertOf`), tree-OID equivalence checks (`^{tree}`), and monotonic CAS updates to `refs/gitpulse/green/<branch>` after verified lands.
- **Skipping gates a diff cannot affect.** That belongs inside each repository's own gate, not in a second gate list here.
- **Telling lanes to stop running the full suite.** Until a train has actually run in a repository, every lane still runs its full suite.
