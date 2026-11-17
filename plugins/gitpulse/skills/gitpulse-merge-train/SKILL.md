---
name: gitpulse-merge-train
description: Verify several branches together with ONE full gate run instead of one per branch, in GitPulse, DevCouncil, Manvi or ScholarLM. Merges the batch onto a persistent integration worktree, runs the repository's own full gate once, compares individual failures against main (main is often already red), re-checks suspected failures alone, and bisects with only the failing tests to name the branch that broke them. Use when two or more finished branches or lanes are waiting to merge, when asked to "verify these branches together", "run a merge train", or "check what this batch breaks". Produces a verified train branch and a report; it never lands, pushes or deletes anything.
license: MIT
compatibility: macOS with git 2.38+ (merge-tree --write-tree), Go 1.27+ to build the failset tool, and each repository's own gate toolchain (bun, cargo, go).
metadata:
  agent-plugins: "1.0.0"
---

# Merge train

One full gate run per **batch**, not per branch. A batch of N branches that verifies clean costs one base run plus one batch run. A batch with a culprit adds about log N runs of **only the failing tests**, then one full run of what is left.

**What this skill does not do:**
- It does not land, push, delete branches or remove worktrees. It ends with a `train/<stamp>` branch and a report, and the owner decides what lands.
- It never runs in a repository's main checkout. Other sessions build and edit there, so a gate run there measures their work too.

## Before you start

1. **Orient.** Call `gitpulse_insights` for the repository. Check each facet's `ok`: a facet that could not scan has not come back clean.
2. **Choose the batch with the owner.** List candidate branches with their ahead/behind counts against main.
   - Leave out branches whose tip moved in the last hour, or whose worktree has a live agent session. Those are live work, not finished work.
   - A branch 0 ahead / 0 behind with its own fresh worktree is an unstarted lane, not a leftover.
   - Show the list and get a yes before merging anything.
3. **Take the lock.** Run `mkdir "$(git rev-parse --git-common-dir)/merge-train.lock"`.
   - If that fails, another train is running in this repository. Stop and say so; never delete someone else's lock.
   - Remove your lock at the end, on every path, including failure.
4. **Check the disk.** `df -h .` must show at least the free space in the table below. Below it, stop and report.

## The four repositories

Only the DevCouncil row has been run (2026-11-16 trial). The other three come from reading each gate script and are **not yet run**. The DevCouncil row was wrong in three ways until it was tried.

That trial also ran from a GitPulse session in bypass permission mode, using `git -C` against the integration worktree. Working from *inside* the worktree, as step 1 says, has not been exercised yet under the command gates.

| Repository | Full gate (run in the integration worktree) | Integration worktree | Free disk | Notes |
|---|---|---|---|---|
| GitPulse | `bun run ci:local` | `<repo>/.claude/worktrees/merge-train` | 60 GB | Runs every gate past failures. The coverage target reaches 43–50 GB: delete `src-tauri/target/llvm-cov-target` after each run. Median run ~73 min under load. |
| DevCouncil | `npm --prefix <worktree> run ci:local -- --continue` | `/Users/bharath/Code/devtools/DevCouncil-train` (a **sibling**: the gate and `backend/go_orchestrator/go.mod` need `../gusset`, at the commit `.github/actions/setup-gusset/action.yml` pins) | 40 GB (its warm `rust/target` was 34 GB on 2026-11-16) | A different runner from GitPulse's despite the name. It prints `== step ==`. With `--continue` it runs every step whose prerequisites passed, runs `rust/verify.sh --quick --continue` (cargo `--no-fail-fast`), and ends with a summary block of PASS / FAIL / SKIP / N/A rows. **`--continue` exists only from branch `claude/gate-continue-mode-6331e3` (4bb53241), not yet on main.** A tree without it ignores the flag and stops at its first failing step, which compare reports as `partial`. Needs `devmap` on PATH for the selector end-to-end step (SKIP without it). No npm dependencies to install. |
| Manvi | `./verify.sh` | `/Users/bharath/Code/devtools/Manvi-train` (a **sibling**: `manvi/go.mod` replaces `../../DevCouncil` and `../../gusset`) | 30 GB | **Stops at its first failing step**, so steps after it are never compared (reported as `partial`). First run in a fresh worktree: prebuild `cargo build --manifest-path crates/Cargo.toml -p dc-verify --bin dcverify -p dc-store --bin dcstore`, or `go test` times out building them. |
| ScholarLM | `bun run ci:local -- --continue` | `<repo>/tmp/wt/merge-train` | 30 GB | Without `--continue` it stops at the first failing step. |

**Logs** go to `tmp/merge-train/` inside the integration worktree, which is gitignored in all four repositories. Write every log there, never in the main checkout.

**Run every gate in the background.** A full gate takes most of an hour, so wait for its completion notice rather than polling it.

## Procedure

### 1. The integration worktree (once per repository, then reused)

- From the repository root, run `git worktree add --detach <path> main`. If it already exists, keep it: its build output is the point, since a warm target saves a cold build every train.
- Do the rest of the train **from inside that worktree**: enter it with `EnterWorktree` by path, or brief a subagent whose working directory is that worktree.
  - The command gates refuse `git -C <other worktree>` and `cd` into one.
  - Never move another checkout's refs with plumbing (`update-ref`, `commit-tree`).

### 2. Build the comparison tool

```bash
go -C <this skill's directory>/failset build -o "$PWD/tmp/merge-train/failset" .
```

`failset compare <base-log> <batch-log>` exits:
- **0** clean;
- **1** new failures;
- **2** cannot compare: a log has no verdict, because it was killed, ran out of disk, or is still running;
- **3** partial: no new failures where it could compare, but something could not be compared.

It reads the gate summaries of all four repositories. Inside them it reads nextest, libtest, vitest, `go test`, pytest, Playwright and the GitPulse browser harness.
- Where a gate prints a summary block, its rows are the verdict for every step, including a nested block such as DevCouncil's `rust/verify.sh` rows inside `ci:local`.
- A step is one of: passed, failed, skipped (it could not run), or not-applicable (it had nothing to do here, such as DevCouncil's release tag step with no local tag).

### 3. Base run: main as it is now

1. `git switch --detach main`.
2. Write a versions header into the log: `git rev-parse HEAD` and the toolchain versions (`bun --version`, `cargo --version`, `go version`, and `devmap --version` for GitPulse).
3. Append the gate's output to the same log: `tmp/merge-train/base-<main-sha12>.log`.

**Reusing a base log:** an existing base log can stand in only when:
- its sha **and** versions header match;
- `failset extract` exits 0, meaning it is complete;
- no gate in it is listed as `incomplete`.

Otherwise run it again. A base log from a different main, or from different tools, is not this base.

### 4. Batch run

1. `git switch -C train/<yyyymmdd-hhmm> main`.
2. For each branch in the agreed order:
   - Check it first against what the train branch holds **now**: `git merge-tree --write-tree --name-only --messages HEAD <branch>`.
   - Never pre-check two branches against each other with `--merge-base=main`. A branch that is behind main then appears to revert main's newer commits, which shows a conflict that a real sequential merge does not have.
   - On a conflict, **leave that branch out** and record why; don't resolve conflicts on a branch you don't own.
   - Otherwise `git merge --no-ff --no-edit <branch>`.
3. A clean merge can still hide a semantic conflict. Run the cheapest compile check first (`cargo check --all-targets`, `go build ./...`, `bun run check`) and stop there if it fails.
4. Run the full gate into `tmp/merge-train/batch-<stamp>.log`.

### 5. Compare

Run `tmp/merge-train/failset compare <base-log> <batch-log>`, and read every section, not just the verdict:

- **NEW FAILURES:** failures the batch has and main does not.
- **MASKED:** the gate already failed on main with no per-test detail (fmt, clippy, lint, a coverage floor). A new failure inside it cannot be told apart. Compare that gate's own error lines by hand and report what you found.
- **NOT COMPARED:** a gate that ran in only one log, one that was skipped in both, or one whose test runner stopped part way. This is not a pass. Name each one in the report.
  - A gate skipped on this platform in both logs (GitPulse's `onlyOn` gates) keeps the verdict at `partial`: the batch's effect on it is unknown.
  - An item marked "failed on the batch; the base never ran it" is a batch failure with nothing to compare it to, because the base stopped first. Treat it like a new failure: confirm it on main (step 6) before landing anything.
- **FIXED:** failures main has and the batch does not.

**When main stops a stop-at-first-failure gate early.** Manvi's `verify.sh` stops at its first red step, and so does DevCouncil's `ci:local` on a tree without `--continue`. If that step is red on main, everything after it runs on neither tree, and `compare` says so (`partial`, "every step after the first failure").
- On 2026-11-16 that left the whole Rust suite unrun, which was all the two branches in the batch changed.
- In that case, run the steps after the red one yourself, on the batch tree, past failures:
  - Read the step list from the gate script itself, with the environment it sets (DevCouncil's `CGO_CFLAGS` gusset hash, for example).
  - Run cargo with `--no-fail-fast`. Without it, cargo stops at the first failing test binary, and the crates after it never run.
  - Check which crates and steps actually ran, from the `Running …` lines, before calling anything covered.
- A pass on the batch cannot be a new failure, so a run of only the batch is enough when everything passes. A batch failure there still needs the same steps run on main.
- Say in the report that these steps were run by hand, not by the gate.
- The lasting fix is a continue-past-failure mode in each gate. GitPulse's runner and ScholarLM's `--continue` have one, and DevCouncil's is on `claude/gate-continue-mode-6331e3`. Manvi's is planned after its `ci/pipeline-speed` branch lands. Until then, Manvi's base and batch runs must run one after the other, never at the same time: its rust leg writes a binary into the sibling DevCouncil checkout.

### 6. Confirm before blaming

Machines here are often loaded, and load-sensitive tests fail on their own. Re-run each new failure **alone**, on the batch tree and on a detached main checkout of the same worktree:

| Runner | Re-run one test |
|---|---|
| nextest | `cargo nextest run --manifest-path src-tauri/Cargo.toml -E 'binary_id(<binary>) & test(=<name>)'` (the binary is the id's first word) |
| vitest | `bun run test -- <file> -t '<name>'`. For GitPulse use `npm --prefix <worktree> run test -- <file>`, and check the `RUN` line names the worktree. |
| go | `go test -run '^<Name>$' <package>` |
| harness | `node scripts/browser-regressions.mjs --harness <name>` (add `--webkit` for WKWebView) |
| DevCouncil step | `npm run ci:local -- --only '<step>'`: that step and the steps it needs, nothing else (from `claude/gate-continue-mode-6331e3`) |

- **Fails on the batch and passes on main, twice:** the batch introduced it.
- **Fails on both:** it belongs to main, and the base run missed it as a flake. It is not the batch's.
- **Passes on both:** a load flake. Report it and don't blame a branch for it.
  - Then run the failing test's whole binary a few times on both trees. A test can pass alone and fail only beside the others in its process.
  - On 2026-11-16, `devmap-resolve`'s `lsp::session_bounds::a_server_that_dies_at_start_up_reports_its_redacted_stderr` failed once inside the full gate. It then passed alone 6/6 and as part of its 92-test suite 3/3 on the train and 3/3 on main.

**Scripting these re-runs:** read the runner's own result lines (`test result: ok. 1 passed`, `--- PASS`), never a pipeline's exit status. `(cargo test … | grep …)` returns grep's status: a trial script once reported "pass" for every tree that way. The result happened to be right, but the script could not have seen a failure.

### 7. Bisect with only the confirmed failures

1. Split the batch in half, merge the first half onto main in the same worktree, and re-run **only** the confirmed failing tests. Do not re-run the full gate.
2. Recurse into the half that fails. With one branch left, that branch is the culprit.
3. If both halves pass and the whole batch fails, the failure needs branches from both halves together. Report the pair; don't name either alone.
4. Leave the culprits out, rebuild `train/<stamp>` from the rest, and run the **full** gate once more.
   - The reduced batch is a different tree, so the earlier batch run does not vouch for it.
   - Compare against the same base log.

### 8. Report and stop

Write `tmp/merge-train/report-<stamp>.md` with:
- the batch as agreed, the branches left out for conflicts, and the culprits with their failing tests;
- the final verdict and every MASKED and NOT COMPARED item;
- the wall time of each run.

Then tell the owner:
- the train branch, and that nothing landed;
- what verified clean;
- what could not be compared, and why.

Remove the lock. Leave the integration worktree for the next train, apart from the GitPulse coverage target noted in the table.

## What this does not do yet

- **Reusing results for a tree verified before.** This skill reuses only base logs, by main's sha and the toolchain versions. A result keyed by tree hash, which also covers the sibling checkouts and tools the gates read, belongs to the Manvi subcommand that will replace this procedure.
- **Skipping gates a diff cannot affect.** That belongs inside each repository's own gate (GitPulse: the `ci:local` Rust-only merge mode card), not in a second gate list here.
- **Telling lanes to stop running the full suite.** Until a train has actually run in a repository, every lane still runs its full suite.
