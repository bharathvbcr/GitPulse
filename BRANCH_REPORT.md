# Branch consolidation report — 2026-10-17 (landing run, 07:56)

Scope: local branches only; nothing was pushed. **Main moved 8026dd7e → dc331e8c (fast-forward to
`consolidate/1017`, 311 commits, 377 files), then 329b8be1 committed the untracked reports. `main`
is now the only branch.** The owner said "merge all" this run, with no agent sessions in the repo.

## Inventory (07:56)

`gitpulse_insights` and `gitpulse_collision_risk`: every facet `ok`, 7 of 7 worktrees scanned. One
live Claude session, pid 51822, which is this consolidator; 19544 from the previous run is gone.
0 Codex. Only collision: BRANCH_REPORT.md, untracked in both main and bl-integrate (different files).
No branch tip had moved since the 23:00 section.

| Branch | Ahead/behind main | Last commit | Worktree (dirty) | Live session | Task | Class |
|---|---|---|---|---|---|---|
| main | 0/0 | 10-13 | main checkout (2 modified, = 4f6a0235; 5 untracked) | 51822 (this) | — | keep |
| consolidate/1017 | 311/0 | 10-16 23:37 | none (webcontent-snap detached at its tip) | — | — | mergeable: landing head |
| consolidate/1016 | 309/0 | 10-16 21:34 | consolidate-1016 (ignored tmp/ only) | — | — | contained in 1017 |
| bl/integrate-1013 | 286/0 | 10-16 17:23 | bl-integrate (2 untracked reports) | — | — | contained |
| pf/434d7bef-sidecar-rtt, -adv | 287/0 | 10-16 18:35 | none | — | ft-434d7bef review | contained |
| pf/434d7bef-sidecar-rtt-real | 288/0 | 10-16 19:31 | none | — | ft-434d7bef review | contained |
| wip/webcontent-snapshot | 1/0 | 10-16 23:32 | none | — | ft-bc59566c review | contained |
| gitpulse/webcontent-…-0d178c24 | 0/0 | 10-13 | .gitpulse worktree (10) | — | ft-bc59566c review | merged; dirty files = b1ac76b7 |

Fable reviewed the classification and agreed with every row. It verified:

- `merge-base --is-ancestor` for every tip, plus the detached worktree heads;
- that the webcontent worktree's 10 dirty files match `b1ac76b7:` (sha256, all 10), and that 3 of
  them are older than main, so discard them and never re-apply them;
- that, of the 7 `in_progress` tasks, only ft-626b25fc has commits in the range. Those are the test
  slices that the integrator merged on 10-16. They are partial work on a long-running card, not a
  branch bound to it. The other 6 have no commits in the range.

## Backups

- Refs: `refs/backup/2026-10-17/*`, the 9 branches at inventory, plus `stash-verifdriver` (216b22e3).
- Bundle: `/Users/bharath/branch-backups/GitPulse-2026-10-17.bundle`, 9 branches, verified okay.

## Merged

- **main → dc331e8c**, `git merge --ff-only consolidate/1017`. A fast-forward has no conflicts and
  no merge commit; the conflicts inside it were resolved and Opus-reviewed in the 23:00 and earlier
  sections.
  - Main's two modified verification_driver files blocked the fast-forward. They are byte-identical
    to `4f6a0235:`, which is inside 1017, and the tip has later edits to them, so committing them on
    main would have forked main and conflicted.
  - They were set aside with a path-limited stash, stash@{0} 216b22e3 (also at
    `refs/backup/2026-10-17/stash-verifdriver`). `git diff 4f6a0235 216b22e3` on both paths is
    empty. The owner cancelled the `stash drop`, so the entry is still there; drop it at will.
- **329b8be1** `docs(reports)`. It commits main's 5 untracked reports, and folds in bl-integrate's
  two reports, which existed nowhere else:
  - its 2026-10-14 consolidation section, now in date order below;
  - `BUILD_REPORT-backlog-2026-10-13.md`.
  Both were checked against HEAD byte for byte (`tmp/check-folded.py`).

## Other work searched for

- **Old backup refs.** 48 refs under `refs/backup/2026-10-1{3,4}*` are not reachable from main.
  `git cherry` left 19 distinct patch-unique commits, and every one is on main or superseded:
  - 10 have a commit with the same subject on main (re-landed after rebase).
  - e5ad40ae's cherry-pick onto main changes nothing.
  - The rest were checked by content markers on main:
    - the shared_budget mutex and thread tests;
    - the language-stats SingleFlight and racy-clean cache;
    - the AgentsView visible-timer contract;
    - all 35 entity_collision adversarial tests and every be97d16f function;
    - `ScanDeadline`, `changed_paths_within` and `scan_in_waves`.
  - f2166f9f is a scan bench against the deleted `read_range()`, superseded by its own account.
  - Scripts: `tmp/cherry-unreached.sh`, `tmp/subject-match.sh`, `tmp/already-applied.sh`.
- **Session scratchpads** (`/private/tmp/claude-501/-Users-bharath-Code-devtools-GitPulse*`):
  - The `clone` and `tagless` scratch trees: 3,658 files, every blob already in the object store
    (`tmp/blob-known.sh`).
  - `shallow-s4`: a clean clone of 8026dd7e.
  - `save/storm.rs`: an older draft of `watcher/storm.rs` (main has 555be54e and dbcf8142).
  - The 10-14 branch bundle: all of its heads are covered above.
  - 27 edit fragments: 24 are 80–100% on main. `hosted.rs` landed as `pass.rs` `hosted_owner`.
    `inj.rs` tests an injected existence check that 25d8b3e0 (on main) removed. `w.rs` is a
    socket-backlog probe.
  - Nothing to merge.
- **stash@{1}** "wip task compose (not 0.1.0 CI)", from 2026-09-09. It is unrelated, and was left
  alone as in earlier runs.

## Deleted

| Branch | Was | Backup ref |
|---|---|---|
| consolidate/1016 | 6b7c42fa | refs/backup/2026-10-17/consolidate/1016 |
| consolidate/1017 | dc331e8c | refs/backup/2026-10-17/consolidate/1017 |
| bl/integrate-1013 | b91c79fc | refs/backup/2026-10-17/bl/integrate-1013 |
| pf/434d7bef-sidecar-rtt | 0e357b70 | refs/backup/2026-10-17/pf/434d7bef-sidecar-rtt |
| pf/434d7bef-sidecar-rtt-adv | 0e357b70 | refs/backup/2026-10-17/pf/434d7bef-sidecar-rtt-adv |
| pf/434d7bef-sidecar-rtt-real | d1f7c9b3 | refs/backup/2026-10-17/pf/434d7bef-sidecar-rtt-real |
| wip/webcontent-snapshot | b1ac76b7 | refs/backup/2026-10-17/wip/webcontent-snapshot |
| gitpulse/webcontent-…-0d178c24 | 8026dd7e | refs/backup/2026-10-17/gitpulse/webcontent-memory-attribute-the-290-mb-w-0d178c24 |

All were deleted with `git branch -d`; none needed `-D`.

Worktrees removed:

- consolidate-1016: plain `git worktree remove`.
- bl-integrate: `--force`, after its two untracked reports were committed and checked.
- webcontent-…-0d178c24: `--force`, because its 10 files are in b1ac76b7.
- agent-aa5d0005: clean and reachable.

The ignored `tmp/` contents of consolidate-1016 and bl-integrate (CI logs, merge scripts) are kept
in the main checkout's `tmp/worktree-archive/`.

## Kept

- `main`.
- `seed`, a detached worktree at ed54ac18: clean and reachable, with a seeded `src-tauri/target`
  that lanes clone from.
- `webcontent-snap`, a detached worktree at dc331e8c that is running this run's ci:local.
  Remove it once its log is read.
- No release branches exist.

## Blocked

None.

## Gate (ci:local on dc331e8c, the tree now on main)

GATE_RESULT_PLACEHOLDER

## Restore

```
git branch <name> refs/backup/2026-10-17/<name>
git fetch /Users/bharath/branch-backups/GitPulse-2026-10-17.bundle 'refs/heads/*:refs/restored/*'
git reset --keep 8026dd7e   # only to undo the landing itself; refs/backup/2026-10-17/main
```

---

# Branch consolidation report — 2026-10-16 (late run, 23:00)

Scope: local branches only; nothing was pushed. **Everything is merged into `consolidate/1017`
(dc331e8c), 311 ahead of main and 0 behind. Main is still at 8026dd7e.** The fast-forward was
stopped twice: the blast-radius hook cancelled restoring main's two modified files, and those
files are already committed in 4f6a0235. Landing it is one command, below. No branch was deleted
this run.

## What changed since the 18:00 section

A run between 18:07 and 21:34 (no report section) built `consolidate/1016` from
`bl/integrate-1013` and merged into it, with Opus review of each conflict:

- hopeful-nash (clean);
- terminal-sessions (10 add/add files, PROTOCOL 1→2);
- xterm-gpu (merged before its A/B, on the owner's "merge all");
- tasks-refresh-storm (a conflict in `workbench.rs` `shutdown()`);
- sidecar-rtt-real (clean);
- `wip/main-checkout-verification-base` (4f6a0235): a snapshot of main's two modified
  verification_driver files, with the conflict in `decide()` resolved.

It then deleted those branches and their worktrees. All 35 tips are in
`refs/backup/2026-10-16c/*`, and Fable verified that every one is reachable from `consolidate/1016`.

## Inventory (22:55)

`gitpulse_insights` and `gitpulse_collision_risk`: every facet `ok`, 6 of 6 worktrees scanned. Two
live Claude sessions were found: 19544 and 88472 (this consolidator), plus 0 Codex. Fable found that
19544 is wedged: its child shells and a 7-hour `cargo test` run in worktree directories that have
been deleted. Its cwd is the main checkout.

| Branch | Ahead/behind main | Last commit | Worktree (dirty) | Live session | Task | Class |
|---|---|---|---|---|---|---|
| main | 0/0 | 10-13 | main checkout (2 modified + 5 untracked) | 19544 (wedged), 88472 | — | keep |
| consolidate/1017 | 311/0 | 10-16 23:2x | none (webcontent-snap is detached) | — | — | **mergeable: the landing head** |
| consolidate/1016 | 309/0 | 10-16 21:34 | consolidate-1016 (ci logs only) | — | — | contained in 1017 |
| bl/integrate-1013 | 286/0 | 10-16 17:23 | bl-integrate (2 untracked) | none | — | contained |
| pf/434d7bef-sidecar-rtt, -adv, -real | 287–288/0 | 10-16 | none | — | ft-434d7bef review | contained |
| wip/webcontent-snapshot | 1/0 | 10-16 (new) | webcontent-snap | — | ft-bc59566cd39b | contained (new this run) |
| gitpulse/webcontent-…-0d178c24 | 0/0 (= main) | 10-13 | .gitpulse worktree (**10**) | none | not on board | keep and flag, see below |

Not branches: detached `seed` (ed54ac18) and `agent-aa5d0005…` (26bdc0ea, stale lock), both
reachable. `stash@{0}` "wip task compose" dates from 2026-09-09; it is unrelated and was left alone.

**Task flag.** ft-626b25fc (full-suite test flakes) is **in_progress**. Its `pf/626b25fc-*` slices
were merged into `bl/integrate-1013` by the integrator before this run, so landing main lands them.
The owner said "merge all". Every other lane's task is in `review`.

## Backups

- Refs: `refs/backup/2026-10-16d/*`, 9 refs: the 7 branches at inventory, plus consolidate/1017
  and wip/webcontent-snapshot.
- Bundle: `/Users/bharath/branch-backups/GitPulse-2026-10-16d.bundle`, verified okay. It holds the
  7 branches at inventory; the two new branches are ref-backed only.
- Earlier: `refs/backup/2026-10-16c/*` and `/Users/bharath/branch-backups/GitPulse-2026-10-16c.bundle`.
- Fable found 47 refs under older backup namespaces (2026-10-13 and 2026-10-14) that are not
  reachable from consolidate. Some have patch-unique commits: hx/perf-hardening 13,
  lane/fsevents-abort 10, wip/catchup-scan 14, wip/a25f8a16-spawn-budget 2. **Do not prune those
  namespaces without a separate review.**

## Phase 3: pending work committed

- **webcontent worktree** (no live process, task not on the board). Its 10 uncommitted files
  (ft-bc59566cd39b) were copied byte for byte (sha256 checked) into a new worktree
  `.claude/worktrees/webcontent-snap`. They were committed there as **b1ac76b7** on
  `wip/webcontent-snapshot`, based on 8026dd7e. The original worktree was not touched and still
  has its 10 dirty files.
- **Main checkout:** its two modified files were already captured in 4f6a0235 by the earlier run.

## Merged this run

**dc331e8c**: `wip/webcontent-snapshot` into `consolidate/1017` (= consolidate/1016 + this merge).

Conflicts, both add/add and resolved by keeping both sides, integration side first:

- `TerminalSession.svelte` imports: gpuRenderer and frameProbe on one side, endedBuffer on the other.
- `docs/PERFORMANCE.md`: both sides appended dated sections; the WebGL section comes first, then
  the WebContent soak.

Opus review: approve with changes. Both changes were applied before the commit:

1. An ended session resumed while its tab is off screen runs `reset()` before `exited` goes false.
   The replay arm then wrote the old session's tail into the new session. The replay now runs only
   while the session is still exited, and a restart forgets the tail. Lines are written with
   `\r\n`, because `convertEol` is false. `TerminalSession.test.ts` asserts both; those assertions
   fail against the pre-fix source.
2. The blank line between the two PERFORMANCE.md sections was restored.

Open for the webcontent owner: `endedReleased` belongs to each component instance, so a recreated
terminal never reads the tail. The "recreated terminal" reason in `endedBuffer.ts` does not hold.

Opus confirmed that the WebGL renderer, the ptyd host, `ended_by_gitpulse` and TerminalPanel's
removal paths have no other interaction.

## Gate (ci:local)

- **6b7c42fa (consolidate/1016), run 2, this session:** the frontend unit tests failed: 2 of 10,595,
  both in `scripts/vite-watch.test.ts`, "does not reload when a nested agent worktree gains a
  tsconfig.json". The control reload was never observed (`log: []`). This failed in all four local
  runs on this lineage. **Alone it passes, 5/5** (`bunx vitest run scripts/vite-watch.test.ts`), so
  it is a full-suite load interaction and not proven a regression. Whether main's tree fails the
  same way is unverified. Final result: **15 of 18 gates passed, 3 failed.**
  - Frontend unit tests: the vite-watch failures above.
  - Rust tests (4664 passed, 1 failed in the lib; 15 passed, 1 failed in adversarial_repos):
    `watcher::tests::a_full_refresh_announces_no_change_under_any_index_feature` (src/watcher/mod.rs:3666)
    and `watcher_storm_keeps_session_responsive` (tests/adversarial_repos.rs:453). Both are watcher
    tests, the same family as before; flaky versus real is unverified. The `env.rs` panics in the log
    come from a test of a failing test, and that test passes.
  - Coverage floor: failed only because the two failing gates wrote no lcov reports.
  - The log lived in that worktree's gitignored `tmp/` and was not kept; the counts above are its
    summary.
- Earlier run on b4a0ce6e: 14 of 18 gates passed. Native renderer drag-preview failed, and passed
  522/522 on an isolated rerun. 5 Rust watcher/worktree tests failed, matching the known load-flake
  shape; flaky versus real is unverified.
- **dc331e8c has not had ci:local.** Only `endedBuffer.test.ts` and `footprint-summary.test.ts`
  were run on it (6 passed). TerminalSession and TerminalPanel tests need `bun install` in the
  worktree, because of the `@xterm/addon-webgl` pin.
- The owner said "merge all" and then "merge the remaining", so landing does not wait for green.
  Run ci:local on main after the fast-forward.

## To land (owner, from the main checkout)

```
git checkout -- src-tauri/src/workbench/verification_driver.rs src-tauri/src/workbench/verification_driver_tests.rs
git merge --ff-only consolidate/1017
```

The two files are byte-identical to `4f6a0235:` (cmp checked at 23:25), and 4f6a0235 is in
consolidate/1017. None of the 5 untracked reports is added by the merge.

## Then delete (all are `-d`-safe once main is at dc331e8c)

```
git worktree remove .claude/worktrees/webcontent-snap
git worktree remove .claude/worktrees/consolidate-1016   # it holds only untracked tmp/ logs; it will refuse if not clean, so review those first
git branch -d consolidate/1016 consolidate/1017 bl/integrate-1013 pf/434d7bef-sidecar-rtt pf/434d7bef-sidecar-rtt-adv pf/434d7bef-sidecar-rtt-real wip/webcontent-snapshot
```

Keep:

- `bl/integrate-1013` until its worktree's 2 untracked reports are moved.
- `gitpulse/webcontent-…-0d178c24` and its worktree. Its content is now on main, but the worktree's
  files are still uncommitted. Discard them or close the task from GitPulse.
- `seed`.

## Restore

```
git branch <name> refs/backup/2026-10-16d/<name>      # or 2026-10-16c for the branches deleted earlier
git fetch /Users/bharath/branch-backups/GitPulse-2026-10-16d.bundle 'refs/heads/*:refs/restored/*'
```

---

# Branch consolidation report — 2026-10-16 (evening run, 18:00)

Scope: local branches only; nothing was pushed. **No branch was merged into main. The 16 merged,
empty `worktree-agent-*` branches that had no worktree were deleted.** `main` is unchanged at
`8026dd7e`. The morning run's section follows below.

## Why nothing landed

- **The integrator changed, but integration is still live.** The old integrator, pid 2558, has
  exited. Pid 19544, which is busy and whose working directory is the main checkout, is now merging
  lanes into `bl/integrate-1013`: the reflog shows merges at 15:06–17:23 today. At 17:59 it cut a new
  lane, `pf/434d7bef-sidecar-rtt`, from the integration tip.
- **`bl/integrate-1013` is at b91c79fc, 286 ahead of main and 0 behind.** Every `pf/*` lane, card7,
  hook-collision-guard and `pf/integrate-1015` are now ancestors of it, so landing any of them means
  landing the integration branch.
- **Landing it is a 303-file fast-forward of the main checkout under a busy session.** Fable checked
  that it touches none of main's 7 dirty files, so it would not clobber them. Moving main while
  19544 is still extending the branch is still the owner's call: `git merge --ff-only
  bl/integrate-1013` in the main checkout, once 19544 is idle.
- **ci:local was not run.** No merge commit exists to test. The load average was ~14.

## Inventory (18:01)

Sources:
- `gitpulse_insights` and `gitpulse_collision_risk`: every facet `ok`, 13 of 13 worktrees scanned,
  2 live Claude sessions (19544 busy, and 22924, this consolidator), 0 Codex.
- `%(ahead-behind:main)`, `merge-base --is-ancestor` against `bl/integrate-1013`, `git cherry`, and
  `git merge-tree` trial merges. The script is `tmp/consolidate-1016b/contain.sh`.
- The task board's `in_progress` column. None of the lanes' tasks below is in it.

| Branch | Ahead/behind main | Last commit | In integrate? | Worktree (dirty) | Live session | Class |
|---|---|---|---|---|---|---|
| main | 0/0 | 10-13 | — | main checkout (7) | 19544 busy, 22924 | keep |
| bl/integrate-1013 | 286/0 | 10-16 17:23 | — | bl-integrate (2) | extended by 19544 | **in use** |
| pf/434d7bef-sidecar-rtt | 286/0 | 10-16 17:59 (created) | = tip | agent-a2d90a98 (2), locked 19544 | 19544 | **in use** |
| 21 other pf/* lanes, incl. -adv/-fp/-idle-flake | 148–265/0 | 10-15 to 10-16 | yes | none | — | keep: on main only via integrate |
| pf/integrate-1015 | 147/0 | 10-15 | yes | pf-integrate (0) | — | keep: `-d` refuses until integrate lands |
| audit/card7-hosted-records | 4/0 | 10-15 | yes (merged 14:13) | card7-hosted-records (0) | — | keep: lands with integrate |
| gitpulse/…hook-collision-guard…e12136e7 | 1/0 | 10-14 | yes | (0) | — | keep: lands with integrate |
| gitpulse/…terminal-sessions…17a9fb9d | 4/0 | 10-15 | **no** (4 unique) | (0) | — | mergeable; **blocked** |
| gitpulse/…xterm-gpu…84b48d1e | 3/0 | 10-14 | **no** (3 unique) | (0) | — | mergeable; **blocked** |
| claude/hopeful-nash-2ccfb1 | 1/0 | 10-13 | **no** (1 unique) | hopeful-nash (0) | none now | mergeable; **blocked**, flag |
| fix/tasks-refresh-storm | 0/0 (= main) | 10-13 | — | tasks-refresh-storm (**13**) | none | keep and flag |
| gitpulse/webcontent-…0d178c24 | 0/0 (= main) | 10-13 | — | (**10**) | none | keep and flag |
| worktree-agent-a2d90a98639314434 | 0/522 | 10-08 | — | none (its dir holds sidecar-rtt) | 19544 owns the dir | keep, for 19544's cleanup |
| 16 other worktree-agent-* | 0/522 | 10-08 | — | none | — | merged: **deleted** |

Not branches:
- `seed`, detached at ed54ac18.
- `agent-aa5d0005…`, detached at 26bdc0ea. It is locked by pid 70637, which is now dead, so the
  lock is stale and can be cleaned up later.
- `stash@{0}` "wip task compose" on main, unclassified. It was left alone.

### Fable's review

Fable agreed with every class and that no merge should happen this run. It corrected one point:
tasks-refresh-storm and webcontent sit at main, so "contained in integrate" says nothing about
them. They are dirty worktrees, not lanes. It also added the stash and the stale lock above, and
noted that `BRANCH_REPORT.md` is untracked in both the main checkout and the bl-integrate worktree.
This section was written to the main checkout's copy only.

## Merged

None.

## Conflicts (trial `git merge-tree` onto bl/integrate-1013; no ref was moved)

- **terminal-sessions** conflicts in 10 files: CONTRIBUTING.md, README.md, docs/ARCHITECTURE.md,
  scripts/check-coverage-types.mjs, scripts/check-ipc-contract.test.ts,
  src-tauri/src/commands/terminals.rs, src-tauri/src/lib.rs, src/App.svelte,
  src/lib/components/PromptModal.test.ts and src/lib/stores/modalStore.ts. This needs a real
  resolution pass on top of integrate, with an Opus review.
- **xterm-gpu** conflicts in docs/PERFORMANCE.md only.
- **hopeful-nash** merges cleanly. It is a contracts-only re-vendor at DevCouncil e79c2610 (rule
  `command.git_read_argument`), and integrate does not have it. Under the cross-app schema rule,
  confirm that DevCouncil e79c2610 and the Manvi pin agree before landing it, and run
  `check:vendor-schema` and `check:schema-parity`.

## Deleted (all with `git branch -d`; none needed `-D`)

All 16 were at 98c776d4. Each one's backup ref is `refs/backup/2026-10-16b/<name>`:

worktree-agent-a424330dcbf163195, -a5aa8be791168a329, -a5c8495ed4ab04461, -a5cf70386f373f583,
-a9845f6e70e678aa3, -a9f2e1c0832bf2976, -aa2f731bb568236f4, -aa5c0123fa804774f,
-ab22ba0e9a7156297, -aba04ac8ef15ee005, -ac4b2ef4fb2d3841b, -ac806791bf83766a4,
-ad42591580b32045a, -af3e9d2b268e1ed99, -afc91a74764acc2a9, -afd5dd25036b350e1.

No worktree was removed.

## Backups

- There are 46 refs under `refs/backup/2026-10-16b/`, one for every local branch at 18:02.
- The bundle `/Users/bharath/branch-backups/GitPulse-2026-10-16b.bundle` is outside the repo,
  24.7 MB, and `git bundle verify` reported it okay.

## Blocked, and what unblocks it

1. **bl/integrate-1013 → main.** It needs 19544 idle (or its say-so), then the owner's
   `git merge --ff-only bl/integrate-1013` in the main checkout, and then ci:local on main.
2. **terminal-sessions and xterm-gpu.** They need (1) first. Then merge them `--no-ff` onto main and
   resolve the conflicts listed above.
3. **hopeful-nash.** It needs (1) and the schema-parity check above.
4. **tasks-refresh-storm and webcontent's dirty work.** These need the owner to say whose work it
   is. No live session holds them. The tasks-refresh-storm diff overlaps the main checkout's dirty
   `verification_driver.rs` and its tests.

## Verify

- `main` is at 8026dd7e, unchanged.
- `git fsck --connectivity-only` reported no errors, only dangling trees and blobs from the trial
  merges.
- The re-run of `gitpulse_insights` showed 13 of 13 worktrees, each on the same branch as before,
  and both live sessions still present.

## Restore a deleted branch

```sh
git branch <name> refs/backup/2026-10-16b/<name>
# or, from the bundle:
git fetch /Users/bharath/branch-backups/GitPulse-2026-10-16b.bundle refs/heads/<name>:refs/heads/<name>
```

---

# Branch consolidation report — 2026-10-16

Scope: local branches only; nothing was pushed. **No branch was merged into main; 9 merged, empty
`worktree-agent-*` branches were deleted.** `main` is unchanged at `8026dd7e`. Earlier runs'
reports follow below this section, and sibling reports sit in `.claude/worktrees/bl-integrate/` and
`.claude/worktrees/consolidate-1015/`.

## Why nothing landed

- **All the large work depends on `bl/integrate-1013`.** That branch is at 9103bda4, 195 ahead of
  main, and the busy integrator (pid 2558) last committed to it at 08:03 today. Every `pf/*` lane is
  based on 0f0e0b4b, which is inside the integration branch, so merging any of them would bring in
  the integration branch too.
- **The four side branches cut from main are blocked:** card7, hook-guard, terminal-sessions and
  xterm-gpu.
  - On 10-15 the owner decided they land on top of the integration branch, through
    `consolidate/1015`. Its ci:local run was red and the cause was never found.
  - The host load average is ~178 now, so any ci:local result would be meaningless.
  - Even a clean fast-forward of main would change files under busy pid 19544, whose working
    directory is the main checkout. It would also stop the integration branch from landing as the
    planned fast-forward.
- **Fable reviewed this and agreed:** no merges this run.

## Inventory (2026-10-16 08:24)

Sources:
- `gitpulse_insights` and `gitpulse_collision_risk`. Every facet reported `ok` and all 19 worktrees
  were scanned. There were 11 live Claude sessions and 0 Codex. Collision risk was `truncated`, with 3
  worktrees unscanned.
- `git for-each-ref %(ahead-behind:main)`, `git branch --merged main`, `git merge-base --is-ancestor`
  and `git cherry` against the integration branch. The scripts are in `tmp/consolidate-1016/`.
- The task board, saved as `tmp/consolidate-1016/tasks.json` (199 tasks).

All commits are by BHARATH CHANDRA VADDARAM.

| Branch | Ahead/behind main | Last commit | Merged? | Worktree (dirty) | Live session | Task (status) | Class |
|---|---|---|---|---|---|---|---|
| main | 0/0 | 10-13 | — | main checkout (7) | 9 (19544 and 48259 busy) | — | keep |
| bl/integrate-1013 | 195/0 | 10-16 08:03 | no | bl-integrate (2) | **2558 busy** | integrator | in use |
| bl/ccf03e33-adv | 195/0 | 10-16 | no (3 unique) | agent-a6949… (1), locked 2558 | via lock | ft-ccf03e33 in_progress | in use |
| bl/ccf03e33-rename | 194/0 | 10-16 | no (contained in -adv) | agent-a959… (0) | — | ft-ccf03e33 in_progress | in use |
| pf/626b25fc-output-emits | 195/0 | 10-16 | = integrate tip, 0 unique | agent-a984… (1), locked 19544 | via lock | ft-626b25fc inbox | in use |
| pf/434d7bef-adv | 199/0 | 10-16 | no (3 unique) | agent-afd5… (2), locked 19544 | via lock | ft-434d7bef review | in use |
| pf/79394019-bridge-adv | 151/0 | 10-15 | no (4 unique) | agent-a424… (1), locked 19544 | via lock | ft-79394019 review | in use |
| pf/1de1ff48-redact-adv | 151/0 | 10-15 | no (4 unique) | agent-a5c8… (0), locked 19544 | via lock | ft-1de1ff48 review | in use |
| pf/1de1ff48-redact | 149/0 | 10-15 | no (contained in redact-adv) | none | — | ft-1de1ff48 review | in use, via its -adv lane |
| pf/79394019-bridge | 149/0 | 10-15 | no (contained in bridge-adv) | none | — | ft-79394019 review | in use, via its -adv lane |
| pf/434d7bef-rename-batch | 148/0 | 10-15 | no (contained in 434d7bef-adv) | none | — | ft-434d7bef review | in use, via its -adv lane |
| pf/w0-idle-rows | 148/0 | 10-15 | no (contained in bridge and external) | none | — | ft-79394019 review | in use, via its -adv lane |
| pf/79394019-external | 151/0 | 10-15 | no (contained in external-adv) | none | — | ft-79394019 review | mergeable after integrate; blocked |
| pf/79394019-external-adv | 152/0 | 10-15 | no (5 unique) | none | — | ft-79394019 review | mergeable after integrate; blocked |
| pf/integrate-1015 | 147/0 | 10-15 | 0 unique; ancestor of integrate | pf-integrate (0) | — | — | superseded by integrate; its worktree holds it |
| consolidate/1015 | 148/0 | 10-15 | no (7 ahead of, 54 behind integrate) | consolidate-1015 (1: its report) | — | — | keep: staging branch, blocked |
| audit/card7-hosted-records | 4/0 | 10-15 | no (in consolidate/1015) | card7-hosted-records (0) | — | ft-9cfd4f74 review | mergeable; blocked |
| gitpulse/…hook-collision-guard…e12136e7 | 1/0 | 10-14 | no (in consolidate/1015) | (0) | — | ft-7ba9b4b2 review | mergeable; blocked |
| gitpulse/…terminal-sessions…17a9fb9d | 4/0 | 10-15 | no | (0) | — | ft-1c36bec1 review | mergeable; blocked |
| gitpulse/…xterm-gpu…84b48d1e | 3/0 | 10-14 | no | (0) | — | ft-406d1f4a review | mergeable; blocked |
| claude/hopeful-nash-2ccfb1 | 1/0 | 10-13 | no | hopeful-nash (0) | **22069** idle | none | in use |
| fix/tasks-refresh-storm | 0/0 | 10-13 | yes | tasks-refresh-storm (**13**) | none (pid 17700 gone) | no owning task | keep and flag |
| gitpulse/webcontent-…0d178c24 | 0/0 | 10-13 | yes | (**10**) | none | ft-bc59566c review | keep and flag |
| worktree-agent-{a424…, a5c8…, a6949…, a959…, a984…, afd5…} | 0/522 | 10-08 | yes | none (their agent worktrees are on lane branches) | via locks | — | merged; kept for the orchestrators' cleanup |
| 9 worktree-agent-* listed under "Deleted" | 0/522 | 10-08 | yes | none | — | — | merged: **deleted** |

The `-adv` holders are pid 19544 for the `pf/*` lanes and pid 2558 for the `bl/*` lanes.

Not branches, and outside scope:
- `seed`, detached at ed54ac18.
- `agent-aa5d0005…`, detached at 26bdc0ea and locked by live idle pid 70637.

Both are reachable from main. Remotes were not in scope: `origin/main` is still 522 commits behind
local main.

### Fable's classification review

Fable re-verified the live state and the containment, and confirmed the classes, with three
corrections, all applied above:
- **The four base `pf/*` branches** (redact, bridge, rename-batch, w0-idle-rows) are contained in
  `-adv` lanes that pid 19544 is still extending, so they are in use. Only
  `pf/79394019-external{,-adv}` is free, and it still needs the integration branch on main first.
- **`pf/integrate-1015` cannot be deleted yet.** `-d` refuses it until main contains the
  integration branch, and its own worktree `pf-integrate` holds it either way.
- **The dirty-worktree flags understate the risk** (see Owner actions).

Fable also checked whether any branch could land safely. Hook-guard and card7 each pass the
per-branch rules, and both merge cleanly into main (`merge-tree`). It advised against landing them
anyway, for the sequencing and live-checkout reasons above.

## Backups (Phase 2)

- **Refs:** `refs/backup/2026-10-16/<branch>`, one for each of the 38 local branches at the start
  (list: `tmp/consolidate-1016/backup.list`).
- **Bundle:** `tmp/branch-backup/GitPulse-branches-2026-10-16.bundle`.
  - 24,551,326 bytes, 38 heads, and `git bundle verify` reports it okay.
  - sha256 `794785383b941d2999c0f277d4033840d97c820e8e2dfe00ff2fc6847c3b9080`.
  - It is inside the repo, in the gitignored `tmp/`, because the command gate refuses writes outside
    the repo. **Copy it somewhere durable.**

## Pending work (Phase 3)

Nothing was committed:
- **The main checkout's dirty `verification_driver*.rs`** sits in a checkout that 9 live sessions
  share.
- **`tasks-refresh-storm` and `webcontent-…`:** the gate refuses this session any git write, and any
  non-git read, in a sibling worktree. Both need the owner (see Owner actions).

## Merges (Phase 4)

None. With no merge commits, there were no conflicts to resolve and no Opus reviews to run.

## Deleted (Phase 5)

Each was deleted with `git branch -d` after its backup ref was confirmed. No `-D` was used and no
worktree was removed. All 9 pointed at 98c776d4, an ancestor of main. Each had one reflog entry,
"Created from origin/main", 0 commits, and no worktree.

| Branch | Backup ref |
|---|---|
| worktree-agent-a03177968e4de800d | refs/backup/2026-10-16/worktree-agent-a03177968e4de800d |
| worktree-agent-a4a6ee5124de9efbc | refs/backup/2026-10-16/worktree-agent-a4a6ee5124de9efbc |
| worktree-agent-a4bd520a363df419f | refs/backup/2026-10-16/worktree-agent-a4bd520a363df419f |
| worktree-agent-a66473efbc96bce5b | refs/backup/2026-10-16/worktree-agent-a66473efbc96bce5b |
| worktree-agent-a834ba230a9487333 | refs/backup/2026-10-16/worktree-agent-a834ba230a9487333 |
| worktree-agent-a9434bbcdaf8f4293 | refs/backup/2026-10-16/worktree-agent-a9434bbcdaf8f4293 |
| worktree-agent-aac0995e2add3a0d1 | refs/backup/2026-10-16/worktree-agent-aac0995e2add3a0d1 |
| worktree-agent-abf36c963c0e31af1 | refs/backup/2026-10-16/worktree-agent-abf36c963c0e31af1 |
| worktree-agent-ac22d5659c000aec6 | refs/backup/2026-10-16/worktree-agent-ac22d5659c000aec6 |

Restore:

```sh
git branch <name> refs/backup/2026-10-16/<name>
git fetch tmp/branch-backup/GitPulse-branches-2026-10-16.bundle refs/heads/<name>:refs/heads/<name>
```

Inferred, not verified: these branches were created during the lifetime of pid 19544 (Claude Code
2.1.301). Its cleanup is assumed to treat a missing branch as a logged no-op, as 2.1.296 did. The
worst case is a log line: the branches held no commits, and they are backed up.

## Kept

Every branch in the inventory that is not in the Deleted table was kept, for the class shown in its
row. Here is what moves each one:

- **The integration branch:** the owner's `git merge --ff-only bl/integrate-1013`.
- **The `pf/*` and `bl/ccf03e33-*` lanes:** their orchestrators.
- **The side branches:** consolidate/1015's ci:local passing at normal load.

## Blocked, and what unblocks it

1. **Everything waits on `bl/integrate-1013` reaching main.** When pid 2558 finishes, run the owner
   fast-forward from the main checkout, at a moment when no busy session's working directory is
   that checkout.
2. **`consolidate/1015`** is 54 behind the integration branch. Re-merge the integration branch into
   it, then re-run ci:local with the load average below ~20.
   - If the coverage harness still hangs, bisect card7 on top of the integration branch.
   - Then merge terminal-sessions: AC4 security sign-off is still open.
   - Then merge xterm-gpu: its commit says not to merge until `tui-redraw-bench --summarize` exits 0,
     and it adds the dependency `@xterm/addon-webgl` 0.19.0.
   - Fable notes that hook-guard merges cleanly straight into the integration branch, if the
     integrator would rather take it there.
3. **`pf/79394019-external-adv`** lands after the integration branch, through pid 19544's line.

## Owner actions (flags)

- **The ft-bc59566c work (WebContent, in review) exists only as 10 uncommitted files** in
  `.gitpulse/worktrees/webcontent-…0d178c24`. No commit holds it. Commit it from a session rooted in
  that worktree, or discard it; removing the worktree would lose it. It also overlaps xterm-gpu in
  `TerminalSession.svelte` and in `docs/PERFORMANCE.md`.
- **`tasks-refresh-storm` has 13 unowned dirty files, and the session that held them (pid 17700) is
  gone.**
  - Its `verification_driver*.rs` differ from the main checkout's dirty copies of the same files.
    The `git hash-object` values differ, and the main checkout's copies are larger.
  - So there are two unowned versions of one change. Decide which to keep before anything touches
    either.
- **`ft-f069aa42` is still `in_progress` on the board**, but its lane merged into the integration
  branch at 9103bda4.
- **Older backup tips that are unique against the integration branch**, and whose equivalence was
  not re-proved:
  - `refs/backup/2026-10-14/bl/card14-adversarial`
  - `…/deleted/hx/perf-hardening`
  - `lane/fsevents-abort`
  - `wip/a25f8a16-spawn-budget`
  - `wip/catchup-scan`

  Don't expire those backup namespaces yet. There are now six backup namespaces, 170+ refs.
- **main is 522 commits ahead of `origin/main`,** so nothing since 10-08 has been pushed.

## Verification (Phase 6)

- **`git fsck --connectivity-only --no-dangling`:** exit 0, no output.
- **`gitpulse_insights` after the deletions:** every facet `ok`, the same 11 live sessions, and all
  19 worktrees still on their branches. Nothing referenced a deleted branch.
- **ci:local was not run.** main did not change, and at a load average of ~178 the result would not
  mean anything.
- **Gaps:**
  - The DevMap MCP server failed to connect (ENOENT on its binary), and the code graph is
    analyzer-stale. No DevMap impact checks were run, because no merges happened.
  - Collision risk was truncated, with 3 worktrees unscanned.

## Follow-up 08:45: Fable's advice, as executed

The owner asked me to ask Fable and follow its advice. Fable's answer was that **nothing that
changes git state is safe to do now.** This session made no merges, ran no ci:local and deleted
nothing more.

**State at 08:45 (Fable, re-checked):**
- **Load:** 174.7 / 223.6 / 221.6 on 18 logical CPUs. That is worse than at 08:24.
- **Sessions:** pid 2558 (integrator) and pid 19544 (perf orchestrator, in the main checkout) are
  both still busy.
- **Integration branch:** `bl/integrate-1013` has not moved. It is at 9103bda4, 195 ahead, and main
  is still its ancestor.
- **What blocks a future fast-forward:** nothing in the main checkout. Its dirty
  `verification_driver*.rs` and the five untracked reports are files the integration branch does not
  touch.

**Merge matrix (`git merge-tree`):**

| Into ← from | Result |
|---|---|
| integrate ← hook-guard | CLEAN |
| integrate ← card7 | CONFLICT: content in `CONTRIBUTING.md` and `README.md`; also `docs/ARCHITECTURE.md`, `scripts/check-coverage-types.mjs`, `scripts/check-ipc-contract.test.ts` (count lines) |
| consolidate/1015 ← integrate | CONFLICT: no longer a mechanical re-merge |
| main ← terminal-sessions / xterm-gpu | CLEAN |
| integrate ← terminal-sessions / xterm-gpu | CONFLICT |

**Threshold for ci:local:** a 5-minute load average below 18 (one per logical CPU), with no other
session running `cargo test`. Below about 12 if the terminal suites must be readable:
`ft-626b25fc` records `terminal_output_emits` failing above a load of about 45. There is no
subset flag; ci:local always runs the full gate set.

**Owner checklist:**

1. **Fast-forward main (owner only, wait).** Do this once pid 2558 reports done and pid 19544 has
   exited, or once you accept the working-tree change under it. The fast-forward rewrites 235 files
   (+52,801/−5,016). Run it from the main checkout:
   ```
   git merge-base --is-ancestor main bl/integrate-1013 && \
   git update-ref refs/backup/<date>/main main && \
   git merge --ff-only bl/integrate-1013
   ```
   The integrator's ci:local on its own head stands as this landing's evidence.
2. **Hook-guard (integrator, now).** It merges cleanly into the integration branch, so it lands
   there rather than through consolidate. In `bl-integrate`, run
   `git merge --no-ff gitpulse/gitpulse-hook-collision-guard-pays-a-col-e12136e7`.
   - card7 also goes to the integrator, which resolves the docs and count conflicts from checker
     output, as it did on 10-14 and 10-15.
3. **Retire `consolidate/1015` (owner, after 2).** Its only payload is card7 and hook-guard. Its
   10-15 red was never attributed, and re-merging the integration branch into it now conflicts.
   - Backup: `refs/backup/2026-10-16/consolidate/1015` (6d4211fa).
   - Then run `git worktree remove .claude/worktrees/consolidate-1015` and
     `git branch -D consolidate/1015`.
4. **terminal-sessions and xterm-gpu (owner, wait).**
   - terminal-sessions needs your sign-off on its AC4 security-impact statement.
   - xterm-gpu needs `tui-redraw-bench --summarize` to exit 0, and your yes to adding
     `@xterm/addon-webgl` 0.19.0.
   - Both must be rebased onto the integration branch after step 1.
5. **Uncommitted work (owner, from a session rooted in each worktree).**
   - In `webcontent-…0d178c24`, run
     `git add -A && git commit -m "wip(ft-bc59566c): ended-terminal scrollback drop, footprint summary (review, not done)"`.
   - In `tasks-refresh-storm`, commit its copy as wip first, so it cannot be lost. Then compare it
     with the main checkout's larger dirty copies of `verification_driver*.rs` and pick the
     canonical one. Don't touch the main-checkout copies; a live session may own them.
6. **ft-f069aa42:** leave it to the integrator. The task tools forbid completing a task another
   agent is running.
7. **`pf/integrate-1015` (owner, after pid 19544 exits).** It is kept for now because the call is
   ambiguous: `ft-626b25fc` cited it at 08:09 today. Then run
   `git worktree remove .claude/worktrees/pf-integrate && git branch -D pf/integrate-1015`.
   `-d` would refuse it, because 0f0e0b4b is not in main. It has no unique commits: every commit
   is in the integration branch.
8. **Other deletions:** none are safe now. The 6 remaining `worktree-agent-*` are tied to locked
   lane worktrees, and there is nothing for `git worktree prune` to remove.

## Follow-up 13:38: hook-guard merged into integrate

**The integrator (pid 2558) merged hook-guard into `bl/integrate-1013`.** The merge commit is
`8966ce1c`, with parents `43094031` and `8365f620`. This session checked it read-only:
- hook-guard is an ancestor of the integration head;
- the integration head is `8966ce1c`;
- main is still an ancestor, so the owner fast-forward still applies.

Gates the integrator reports on the merged head:
- fmt and `clippy -D warnings` clean;
- 76 `sysload::` tests pass;
- hermetic_contract 10/10 and supervised_threads 8/8.

It did not run an adversarial pass or the host_load bench.

- **The branch is kept.** `git branch -d` would refuse it until main contains the integration branch.
  Delete it with `-d` after the owner fast-forward. Its backup ref stands.
- **ft-7ba9b4b2 stays in `review`.** The work is not on main, and the adversarial pass and bench were
  not run.
- **Correction:** ft-f069aa42 is already in `review` (revision 3). The `in_progress` flag above came
  from a stale board read.
- **card7:** the integrator takes it after wave 8.

## Follow-up 14:13: card7 merged into integrate

**The integrator merged `audit/card7-hosted-records` (c558599e) into `bl/integrate-1013` at
`5fa26843`**, with parents 8966ce1c and c558599e. Checked read-only here: card7 is an ancestor of the
integration head, and main is still an ancestor.

**Conflicts: count lines only.** They were in `CONTRIBUTING.md`, `README.md`, `docs/ARCHITECTURE.md`
(4 lines), `scripts/check-coverage-types.mjs`, and the history comment in the IPC test.
- **How they were resolved:** inside the conflict regions only, so card7's other edits to those files
  are kept.
- **Where the counts came from:** the checkers, run on the merged tree, not either side.
  - `check:ipc`: 283 handlers.
  - `check:types`: 100 contracts, 248 structs, 1,740 fields, 0 drift.
  - Card7 adds 1 handler and 1 struct with 4 fields to 282/99/247/1,736.
- **The IPC test comment** names `cmd_create_file` (282) and `cmd_terminal_conversations` (283).

**Gates the integrator reports on 5fa26843:**
- The contract vitests: 36 passed, 1 skipped.
- `bun run check` and `clippy -D warnings`: clean.
- Rust `terminal::` and `workbench::conversation`: 192 passed.
- Terminal vitest: 816 passed.
- hermetic_contract 10/10, supervised_threads 8/8, terminal_incident 7/7.
- **The browser coverage harness did not hang.** Chrome and WebKit each passed 163/163 in about 1
  minute, at a load of 22–45.
  - The 10-15 hang (5 of 5 runs) was at a load of 100–700. That fits a load cause, but the hang was
    not reproduced, so this does not prove it.
- **Not run:** a full ci:local, which is due at the end of wave 8.
- **Security note,** recorded in the merge commit: `cmd_terminal_conversations` answers only
  found/absent/unknown, never content. It requires canonical UUIDs, caps each call at 512 and reads
  only Claude Code's project directories.

**What this changes:**
- **`consolidate/1015` is fully superseded.** `git cherry bl/integrate-1013 consolidate/1015` shows 0
  unique patches; both of its payloads, hook-guard and card7, are now in integrate.
  - Owner step 3 (retire it) is now unblocked.
  - Its worktree still holds the 10-15 report as an untracked file, so I did not remove it. Remove it
    only after you keep or discard that report:
    ```
    git worktree remove .claude/worktrees/consolidate-1015
    git branch -D consolidate/1015
    ```
    The backup is `refs/backup/2026-10-16/consolidate/1015`.
- **ft-9cfd4f74 stays in `review`:** card7 is not on main, and ci:local has not been run.
- **The card7 and hook-guard branches** go with `-d` after the owner fast-forward.

## Follow-up: consolidate/1015 retired (owner's instruction)

- **Worktree removed:** `.claude/worktrees/consolidate-1015`, with `git worktree remove --force`.
  - `--force` was needed because the worktree held an untracked file: its 10-15 `BRANCH_REPORT.md`
    (195 lines). The owner chose to discard it.
  - Checked first: `gitpulse_active_changes` listed that single file, the worktree was not locked,
    and no live session was in it.
- **Branch deleted:** `git branch -D consolidate/1015`, which was at 6d4211fa.
  - `-D` was required because 6d4211fa is not in main.
  - It is superseded: `git cherry` against the integration branch shows 0 unique patches.
- **Backup:** `refs/backup/2026-10-16/consolidate/1015`. Restore it with
  `git branch consolidate/1015 refs/backup/2026-10-16/consolidate/1015`.
  - The discarded report is not recoverable from git. Its substance is summarised in this file's
    10-16 sections.

---

# Branch consolidation: 2026-10-14

**Status:** Phases 1–3 are done. Phase 4 waits on one owner command, and phase 5 is partly done. Everything is local; nothing was pushed or deleted on a remote.

This report sits in the `bl-integrate` worktree. The main checkout already has an untracked `BRANCH_REPORT.md` from another session, and it was left alone.

## Backups (phase 2): taken before any change

- **Refs.** `refs/backup/2026-10-14/<branch>` exists for all 29 local branches that were present at inventory time (`git for-each-ref refs/backup/2026-10-14`).
- **Bundle.** `/private/tmp/claude-501/-Users-bharath-Code-devtools-GitPulse/943e48a1-60ff-4d08-a942-72809dca5016/scratchpad/gitpulse-branches-2026-10-14.bundle` (24 MB). `git bundle verify` reports it okay, with 29 heads.
  - It is in the session scratchpad because the gate refuses writes outside the worktree. `/private/tmp` can be cleared on reboot, so **copy it somewhere durable.**

## Inventory (phase 1)

Live sessions: 10. Eight are in the main checkout, one is this session in `bl-integrate`, and one (pid 22069) is in `hopeful-nash-2ccfb1`.

Dirty worktrees:
- **Main checkout:** another session's `verification_driver*.rs` plus 4 untracked reports. In use, so not committed (phase 3).
- **`bl-integrate`:** this session's untracked reports.

"Superseded" was established with `git cherry` against integration. A "−" or no unique patch means the patch is already there.

| Branch | Tip | Last commit | Ahead / behind main | Worktree | Class |
|---|---|---|---|---|---|
| main | 8026dd7e | 2026-10-13 | 0 / 0 | main checkout (8 live sessions, dirty) | **keep** |
| bl/integrate-1013 | d8b51eed | 2026-10-14 | 107 / 0 | bl-integrate (this session) | **mergeable**: the integration head |
| bl/46bece0d-typed-mcp-args | e9962c86 | 2026-10-14 | 4 / 0 | — | merged into integration |
| bl/card10-mcp-budget | fb284c52 | 2026-10-14 | 85 / 0 | — | merged into integration |
| bl/card10-adv | 9ea0486c | 2026-10-14 | 75 / 0 | — | merged into integration (ff into card10) |
| bl/card14-entity-verdict | da3e73de | 2026-10-14 | 25 / 0 | — | merged into integration |
| bl/card16-pulse-walks | bab7d194 | 2026-10-14 | 12 / 0 | — | merged into integration |
| bl/card16-tabswitch-fixture | 481247ff | 2026-10-14 | 70 / 0 | — | merged into integration |
| bl/card22-merge-teardown | f5dd9d65 | 2026-10-13 | 5 / 0 | — | merged into integration |
| bl/card22-s511 | b35b55d4 | 2026-10-14 | 10 / 0 | — | merged into integration |
| bl/card24-event-time | c079596e | 2026-10-14 | 48 / 0 | — | merged into integration |
| bl/card26-nested-worktrees | 26b1178f | 2026-10-14 | 7 / 0 | — | merged into integration |
| bl/e023906b-doc-rename | 13b87213 | 2026-10-14 | 35 / 0 | — | merged into integration |
| bl/f16fad35-write-primitives | 57fbedd2 | 2026-10-14 | 86 / 0 | removed (see phase 5) | merged into integration |
| bl/f16fad35-adv | 0fe61d7f | 2026-10-14 | 77 / 0 | — | merged into integration (ff into f16fad35) |
| bl/46bece0d-adversarial | 511bf1c5 | 2026-10-13 | 2 / 0 | — | **superseded**: 0 unique patches |
| bl/card16-adversarial | 14d23624 | 2026-10-13 | 8 / 0 | — | **superseded**: 0 unique patches |
| bl/card22-adversarial | 068ab14a | 2026-10-13 | 2 / 0 | — | **superseded**: 0 unique patches |
| bl/card22-s511-adversarial | ff608769 | 2026-10-14 | 8 / 0 | — | **superseded**: 0 unique patches |
| bl/card24-adversarial | 25e129ec | 2026-10-14 | 45 / 0 | — | **superseded**: 0 unique patches |
| bl/card26-adversarial | 95311a45 | 2026-10-13 | 3 / 0 | — | **superseded**: 0 unique patches |
| bl/e023906b-adversarial | d3b81e7f | 2026-10-14 | 33 / 0 | — | **superseded**: 0 unique patches |
| bl/card14-adversarial | 0601944d | 2026-10-14 | 27 / 0 | — | **superseded, by name, not by patch**: see the note below |
| claude/hopeful-nash-2ccfb1 | e3e117e5 | 2026-10-13 | 1 / 0 | hopeful-nash-2ccfb1 (live session pid 22069, clean) | **in use**: hands off |
| worktree-agent-a559f981ffe165f6c | 98c776d4 | 2026-10-08 | 0 / 522 | — | merged into main: **deleted** |
| worktree-agent-a85c3e896d9418c43 | 98c776d4 | 2026-10-08 | 0 / 522 | — | merged into main: **deleted** |
| worktree-agent-aba08faa88ce59e99 | 98c776d4 | 2026-10-08 | 0 / 522 | — | merged into main: **deleted** |
| worktree-agent-ac7115b4a8dadff3c | 98c776d4 | 2026-10-08 | 0 / 522 | — | merged into main: **deleted** |
| worktree-agent-af6a1c3f61f0c48f1 | 98c776d4 | 2026-10-08 | 0 / 522 | — | merged into main: **deleted** |

All commits are authored by BHARATH CHANDRA VADDARAM.

Task bindings:
- Each `bl/*` lane carries its card in BUILD_REPORT.md. All of those cards are in `review`.
- No task binding is known for `claude/hopeful-nash-2ccfb1`.

Remote branches (`origin/main`, `origin/main-pql0l2`) were not in scope and were not touched.

**bl/card14-adversarial note.**
- It has 3 commits whose patch id is not in integration.
- be97d16f is an earlier version of the CARD 14 fix. The lane replaced it with squashed commit da3e73de, on the same parent eb8e5d0e, and da3e73de is in integration.
- 7120c199 and 0601944d are the adversarial tests. All 35 test functions they add exist by name in integration's `src-tauri/src/insights/entity_collision.rs`.
- The test bodies were rewritten in the squash, so their equivalence was checked by name, not byte for byte. The backup ref covers the remainder.

Fable reviewed the classification and approved it with three corrections, all applied:
- remove a branch's worktree before deleting the branch;
- set ci:local's pass criterion up front;
- label card14-adversarial as name-equivalent.

## Merge (phase 4)

**Plan, ruled by Fable:** one fast-forward of `main` to the verified integration head, rather than per-lane `--no-ff` re-merges.
- Each lane is already merged with `--no-ff` inside `bl/integrate-1013`, so history is kept.
- Its conflicts were resolved and reviewed there.
- A re-merge would produce a different tree that has not been verified.
- `main` (8026dd7e) is an ancestor of the integration head, so the fast-forward cannot conflict.

**Conflicts met while integrating (all inside `bl/integrate-1013`):**
- The documented-count lines conflicted on almost every lane merge: `CONTRIBUTING.md:212`, `docs/ARCHITECTURE.md:431`, and the `scripts/check-coverage-types.mjs` header.
- Both sides were counts of IPC contracts, structs and fields, each true for its own side.
- Each conflict was resolved from checker output (`bun run check:types`, then `bunx vitest run scripts/documented-counts-contract.test.ts`), never by picking a side. The final values are 99 contracts, 247 structs, 1,733 fields and 282 handlers. An Opus review of the f16fad35 lane re-checked them on the merged tree.
- No other conflicts arose: `git merge-tree` was clean for CARD 10 and f16fad35 against integration.

**ci:local on the integration head (d8b51eed):** 15 of 17 gates pass.
- Rust tests with coverage, both browser suites, clippy, fmt, build, the vendor gates, release, IPC, types and gusset all pass.
- **The two failing gates are one pre-existing cause.** `scripts/serve-ops-contract.test.ts` (Manvi sibling drift) also fails on unmodified `main`. Because of it, vitest wrote no frontend coverage report, so the coverage-floor gate had nothing to read.
- **The coverage floors were checked on a re-run that excluded that file.** It had 0 failures, and the floors hold: frontend lines 96.63%, branches 90.12%; Rust lines 89.65%.
- Full details are in BUILD_REPORT.md.

**Owner action (the gate refuses git in the main checkout from this session):**

```
cd /Users/bharath/Code/devtools/GitPulse && git merge --ff-only bl/integrate-1013
```

## Deletions (phase 5)

**Done:**

| Branch | How | Backup ref |
|---|---|---|
| worktree-agent-a559f981ffe165f6c | `git branch -d` (merged into main) | refs/backup/2026-10-14/worktree-agent-a559f981ffe165f6c |
| worktree-agent-a85c3e896d9418c43 | `git branch -d` | refs/backup/2026-10-14/worktree-agent-a85c3e896d9418c43 |
| worktree-agent-aba08faa88ce59e99 | `git branch -d` | refs/backup/2026-10-14/worktree-agent-aba08faa88ce59e99 |
| worktree-agent-ac7115b4a8dadff3c | `git branch -d` | refs/backup/2026-10-14/worktree-agent-ac7115b4a8dadff3c |
| worktree-agent-af6a1c3f61f0c48f1 | `git branch -d` | refs/backup/2026-10-14/worktree-agent-af6a1c3f61f0c48f1 |

Worktrees removed:
- **agent-a85c3e896d9418c43**, which held bl/f16fad35-write-primitives. It was clean, with no tracked or untracked changes (checked with gitpulse_active_changes). `--force` was needed only because of its ignored build output. Its agent had finished. The branch was kept.
- **Earlier:** the finished lane worktrees agent-a559f981…, agent-af6a1c3f…, agent-ac7115b4… and agent-aba08faa….

**Also done, on the owner's instruction "delete old and stale branches" (2026-10-14), before `main` fast-forwards:**
- **Merged lanes.** All 13 were deleted with `git branch -d`, which accepted every one because each is merged into `bl/integrate-1013`:
  - bl/46bece0d-typed-mcp-args e9962c86
  - bl/card10-mcp-budget fb284c52, bl/card10-adv 9ea0486c
  - bl/card14-entity-verdict da3e73de
  - bl/card16-pulse-walks bab7d194, bl/card16-tabswitch-fixture 481247ff
  - bl/card22-merge-teardown f5dd9d65, bl/card22-s511 b35b55d4
  - bl/card24-event-time c079596e
  - bl/card26-nested-worktrees 26b1178f
  - bl/e023906b-doc-rename 13b87213
  - bl/f16fad35-write-primitives 57fbedd2, bl/f16fad35-adv 0fe61d7f
- **Superseded adversarial branches.** All 8 were deleted with `git branch -D`:
  - bl/46bece0d-adversarial 511bf1c5
  - bl/card14-adversarial 0601944d
  - bl/card16-adversarial 14d23624
  - bl/card22-adversarial 068ab14a
  - bl/card22-s511-adversarial ff608769
  - bl/card24-adversarial 25e129ec
  - bl/card26-adversarial 95311a45
  - bl/e023906b-adversarial d3b81e7f
- **Backups.** Each deleted branch has `refs/backup/2026-10-14/<name>` and an entry in the bundle.
- **Why this is safe.** `bl/integrate-1013` is kept, and every one of these commits (or its patch equivalent) is reachable from it. The commits only become reachable from `main` once the owner fast-forwards.
- **Checks afterwards.** `git worktree prune` was run, and `git fsck --no-dangling` is clean.

**Wave 4 and the follow-up fix (2026-10-14 / 15), deleted after each merge.**
- These branches were created after the backups, so they have no `refs/backup` entry and are not in the bundle.
- Each tip was confirmed contained in `bl/integrate-1013` (`git branch --contains <sha> --list bl/integrate-1013`) before this report was written. Each was deleted with `git branch -d`.
- A tip is restorable with `git branch <name> <sha>` while `bl/integrate-1013` or `main` holds it.

| Branch | Tip | Merged into integration at |
|---|---|---|
| bl/1629347b-repostore-contract | ed0922c0 | 28e6bcc8 |
| bl/1629347b-adv | f9565044 | (in the lane, before 28e6bcc8) |
| bl/card20-default-base | 3f5f290e | 94443dfb |
| bl/card20-adv | 55c65ce9 | (in the lane, before 94443dfb) |
| bl/9fa261d9-grace-drain | 27a16826 | a7390b3b |
| bl/9fa261d9-adv | 10387775 | (in the lane, before a7390b3b) |
| bl/card14-deterministic-property | 4707c2c6 | 5dcb4255 |

The harness's default branches for those agents' worktrees (`worktree-agent-a75541d8…`, `-a159fbea…`, `-ae0679f4…`, `-af3cc2d8…`) all pointed at 98c776d4, which is in `main`, and were deleted with `-d`. Their worktrees were removed after each agent finished.

**Wave 5 (2026-10-15), deleted after each merge.** Same method: each tip was confirmed contained in `bl/integrate-1013` (`git branch -d` refuses otherwise), with no backup ref or bundle entry.

| Branch | Tip | Merged into integration at |
|---|---|---|
| bl/acff3877-denied-stays-denied | 8715cb53 | 5fc8af55 |
| bl/acff3877-adv | 2a8fce4d | 5fc8af55 |
| bl/dadcd71d-merge-file-diff | 9ec8ef1b | 0f0e0b4b |
| bl/dadcd71d-adv | 5397806e | 0f0e0b4b |
| bl/04f5dd26-scan-deadline | 1b91f9a6 | d14568c1 |
| bl/04f5dd26-adv | 8b7c364d | d14568c1 |
| bl/43cfe399-repo-reanchor | fbe3e99b | cd52ff3c |
| bl/43cfe399-adv | c395700c | cd52ff3c |

Their agents' `worktree-agent-*` default branches pointed at 98c776d4 (in `main`) and were deleted after `merge-base --is-ancestor` confirmed it.

**Wave 6 and 7 (2026-10-15), deleted after each merge, same method:**

| Branch | Tip | Merged into integration at |
|---|---|---|
| bl/84a33d12-nonewline | cfedc808 | 2c1fe098 |
| bl/84a33d12-adv | 6e28f02b | 2c1fe098 |
| bl/24f6212d-joiner-deadline | 1feae707 | e346ca99 |
| bl/24f6212d-adv | 0b7464bf | e346ca99 |
| bl/513113a3-pipe-permit | 21effc2b | fc088f9d |
| bl/513113a3-adv | efd68985 | fc088f9d |
| bl/2a2008e9-conflict-mode | 1e195c5f | 86a41ffb |
| bl/2a2008e9-adv | 45f7aff1 | 86a41ffb |
| bl/d41b8491-budget-residuals | 07eaa3b6 | 71d41a54 |
| bl/d41b8491-adv | 4a067184 | 71d41a54 |
| bl/9cfa4f74-graph-spawns | 947a2827 | 3806c4ae |
| bl/9cfa4f74-adv | 99c32651 | 3806c4ae |
| bl/d61584de-mutation-routing | 4318b370 | 1e71c710 |
| bl/d61584de-adv | 3d7a06e3 | 1e71c710 |
| bl/ae9db92a-worktree-routing | 93995cd2 | 67d64b4e |
| bl/ae9db92a-adv | 2314e5b0 | 67d64b4e |

**Wave 7, continued (2026-10-16), same method:**

| Branch | Tip | Merged into integration at |
|---|---|---|
| bl/9cfa4974-churn-key | c522a121 | 10ba9b63 |
| bl/9cfa4974-adv | 215d0a81 | 10ba9b63 |
| bl/f069aa42-baseline | e8328137 | 9103bda4 |
| bl/ccf03e33-rename | 88a5fa20 | 15df37f5 |
| bl/ccf03e33-adv | 39b09f7f | 15df37f5 |
| bl/626b25fc-bounded | 7aab0c58 | bf89c241 |

Each was confirmed an ancestor of `bl/integrate-1013` before `git branch -d`, and each agent's `worktree-agent-*` branch (all at 98c776d4, in `main`) was deleted the same way. The agent worktrees were removed only after no process was found running in them. No branch of this build is live now; `bl/integrate-1013` is the only `bl/*` branch.

**Another session's line:** `pf/integrate-1015` (cut from 0f0e0b4b), `pf/w0-idle-rows`, `pf/434d7bef-rename-batch` and `consolidate/1015` belong to another session and were not touched.

**Branches left after cleanup (2026-10-15):**
- `main`
- `bl/integrate-1013`, head 7c440982 (2026-10-16)
- `claude/hopeful-nash-2ccfb1` (in use)
- Branches other sessions created after this cleanup, which this session neither created nor touched: `audit/card7-hosted-records`, `gitpulse/gitpulse-hook-collision-guard-pays-a-col-e12136e7`, `gitpulse/terminal-sessions-survive-a-gitpulse-app-17a9fb9d`, `gitpulse/webcontent-memory-attribute-the-290-mb-w-0d178c24`, `gitpulse/xterm-gpu-renderer-xterm-addon-webgl-on-84b48d1e`, `worktree-agent-a17ab2f7fd225abe4`, `worktree-agent-a3ad2567c28e6d2e1`, `worktree-agent-af574bbac117c3922`.

**Kept:**

| Branch or worktree | Reason |
|---|---|
| main | The default branch |
| bl/integrate-1013 | This session's worktree. After the fast-forward it is fully merged; delete it when you are done with this worktree |
| claude/hopeful-nash-2ccfb1 | In use: live session pid 22069. One commit re-vendors DevCouncil contracts at e79c2610 (`command.git_read_argument`) and is not on main. CLAUDE.md requires a vendor or schema change to be carried to every consumer, so landing it is its owner's call |
| worktree agent-aa5d0005bf402e80c (detached at 26bdc0ea, locked) | Not a branch; its lock holder is unknown, so it was left alone |
| worktree seed (detached at ed54ac18) | Not a branch; left alone |

## Restore commands

Restore one branch from its backup ref:

```
git branch <name> refs/backup/2026-10-14/<name>
```

Or restore from the bundle:

```
git fetch <bundle-path> refs/heads/<name>:refs/heads/<name>
```

Remove the backup refs once you are satisfied:

```
git for-each-ref --format='delete %(refname)' refs/backup/2026-10-14 | git update-ref --stdin
```

## Verification (phase 6): after the fast-forward

Already run: `git fsck --no-dangling` after the deletions reports no problems.

To do:
- Run `ci:local` on `main`. After the fast-forward, `main` is the same commit as `bl/integrate-1013` (5dcb4255), so the result should match the one in BUILD_REPORT.md.
- Run `git fsck` again.
- Re-run gitpulse_insights to confirm no live session lost its branch.

---

# Branch consolidation report — 2026-10-13

Scope: local branches only (no remote branches were in scope; nothing was pushed).
Result: **no branch was mergeable into main without disrupting a live agent; 8 fully merged
branches were deleted; 10 branches kept.** `main` is unchanged at `8026dd7e`.

## Inventory (before cleanup)

Sources: `gitpulse_insights` / `gitpulse_collision_risk` (all facets `ok`, 12/12 worktrees scanned,
9 live Claude sessions, 0 Codex), `git for-each-ref --format='%(ahead-behind:main)'`,
`git branch --merged main`, `git cherry`, GitPulse task board.

| Branch | Ahead/behind main | Last commit | Merged? | Worktree (dirty files) | Live session | Task (status) | Class |
|---|---|---|---|---|---|---|---|
| main | 0/0 | 2026-10-13 BHARATH CHANDRA VADDARAM | — | main checkout (5) | 8 sessions | — | keep |
| bl/integrate-1013 | 6/0 | 2026-10-13 | no | bl-integrate (0) | **pid 2558, busy** | integrator | in use |
| bl/card22-merge-teardown | 5/0 | 2026-10-13 | no (in integrate-1013) | agent-ab51c92… (0), locked by pid 2558 | via lock | ft-9d044474 (review) | in use |
| bl/card16-pulse-walks | 8/0 | 2026-10-13 | no | agent-a371561… (8), locked by pid 2558 | via lock | ft-9cfa4e74 (in_progress) | in use |
| bl/card26-nested-worktrees | 3/0 | 2026-10-13 | no | agent-a53db45… (7) | — | ft-9d044074 (in_progress) | in use |
| bl/46bece0d-typed-mcp-args | 2/0 | 2026-10-13 | no | agent-a973e3d… (5) | — | ft-46bece0d (in_progress) | in use |
| bl/card16-adversarial | 8/0 | 2026-10-13 | no | agent-a696321… (0) | — | ft-9cfa4e74 (in_progress) | keep (patch-equivalent to its lane, lane not on main) |
| bl/card22-adversarial | 2/0 | 2026-10-13 | no | agent-acc3392… (0) | — | ft-9d044474 (review) | keep (same) |
| bl/card26-adversarial | 3/0 | 2026-10-13 | no | agent-a098a60… (0) | — | ft-9d044074 (in_progress) | keep (same) |
| bl/46bece0d-adversarial | 2/0 | 2026-10-13 | no | agent-a396233… (0) | — | ft-46bece0d (in_progress) | keep (same) |
| worktree-agent-a098a601de8ff9d0e | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-a371561cf98862553 | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-a396233afb117bdb1 | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-a53db45bf259f1929 | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-a6963210af2433d0f | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-a973e3d29f6a431a8 | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-ab51c92a2d5ad8d10 | 0/522 | 2026-10-08 | yes | none | — | — | merged |
| worktree-agent-acc339297585b6b1b | 0/522 | 2026-10-08 | yes | none | — | — | merged |

Detached worktrees (not branches, left alone): `seed` at `ed54ac18` (tag v1.4.0, reachable from
main) and `agent-aa5d0005…` at `26bdc0ea` (reachable from main, locked by desktop session pid 70637).

### Classification review (Fable)

Fable confirmed every row, from read-only git evidence:
- The 8 `worktree-agent-*` branches all point at `98c776d4` (= `origin/main`). Their reflogs show
  they were created from the stale `origin/main` and abandoned 5 s later, when each subagent
  branched `bl/*` from local main. No commits were ever made on them.
- Claude Code 2.1.296 (the orchestrator, pid 2558) deletes a worktree branch only on its
  worktree-remove path, and treats a failed delete as a logged no-op. Both still-locked worktrees
  are hundreds of commits past `98c776d4`, so they take the keep path. Deleting was judged safe.
- Fable advised merging nothing: the only finished lane (card22) is already inside
  `bl/integrate-1013`, which a live integrator owns.

## Backups (Phase 2)

- Refs: `refs/backup/2026-10-13-consolidate/<branch>` for all 18 branches. I used a separate
  namespace because `refs/backup/2026-10-13/` already holds an earlier cleanup's backups.
- Also `refs/backup/2026-10-13-consolidate/dangling/0f003cd6-stash-wip-on-main`: a dropped stash
  (2026-10-08) that no ref held.
- Bundle: `tmp/branch-backup/GitPulse-branches-2026-10-13.bundle` (23,750,995 bytes, `git bundle
  verify` OK, sha256 `7832428519b7386d8765a783d2591a85ec06d6c7ee07fc4d6866cc1a1ec8d969`).
  **It sits inside the repo** (gitignored `tmp/`), because the command gate refuses writes outside
  the project root. Move it out by hand if you want an off-repo copy.

## Pending work committed (Phase 3)

None. Every dirty worktree is either the shared main checkout (8 sessions) or a lane whose task is
in progress, so its edits belong to another agent.

## Branches merged (Phase 4)

None. No merge commits, and so no conflicts to resolve.

## Branches deleted (Phase 5)

All deleted with `git branch -d`, which checked each against main. None needed `-D`, and no
worktrees were removed: none of these branches was checked out anywhere.

| Branch | Was | Backup ref |
|---|---|---|
| worktree-agent-a098a601de8ff9d0e | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a098a601de8ff9d0e |
| worktree-agent-a371561cf98862553 | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a371561cf98862553 |
| worktree-agent-a396233afb117bdb1 | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a396233afb117bdb1 |
| worktree-agent-a53db45bf259f1929 | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a53db45bf259f1929 |
| worktree-agent-a6963210af2433d0f | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a6963210af2433d0f |
| worktree-agent-a973e3d29f6a431a8 | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-a973e3d29f6a431a8 |
| worktree-agent-ab51c92a2d5ad8d10 | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-ab51c92a2d5ad8d10 |
| worktree-agent-acc339297585b6b1b | 98c776d4 | refs/backup/2026-10-13-consolidate/worktree-agent-acc339297585b6b1b |

### Restore

```sh
# one branch
git branch worktree-agent-a098a601de8ff9d0e refs/backup/2026-10-13-consolidate/worktree-agent-a098a601de8ff9d0e
# all eight
git for-each-ref --format='%(refname)' 'refs/backup/2026-10-13-consolidate/worktree-agent-*' |
  while read -r r; do git branch "${r#refs/backup/2026-10-13-consolidate/}" "$r"; done
# from the bundle (e.g. on another clone)
git fetch tmp/branch-backup/GitPulse-branches-2026-10-13.bundle 'refs/heads/*:refs/restored/*'
```

## Branches kept

| Branch | Reason |
|---|---|
| main | Default branch. 522 commits ahead of `origin/main`, still unpushed. |
| bl/integrate-1013 | Live integrator session pid 2558 (busy) is working in it. |
| bl/card22-merge-teardown | Already merged into integrate-1013. Its worktree is locked by pid 2558, and task ft-9d044474 is in review. |
| bl/card16-pulse-walks | Its worktree is dirty (8 files) and locked by pid 2558. Task in progress. |
| bl/card26-nested-worktrees | Its worktree is dirty (7 files). Task in progress. |
| bl/46bece0d-typed-mcp-args | Its worktree is dirty (5 files). Task in progress. |
| bl/{card16,card22,card26,46bece0d}-adversarial | Every commit is patch-equivalent to one on its lane (`git cherry`). The lanes aren't on main, though, so `-d` would refuse. Delete each with `-D` once its lane lands. |

## Blocked

Nothing is blocked on a conflict. Everything that isn't on main is waiting on pid 2558's integration.
Once it finishes, the order Fable suggested is:

1. Fast-forward main to `bl/integrate-1013`. main is its ancestor today, so no merge commit is needed.
2. Land card16, card26 and 46bece0d through the integrator as each reaches review. Pairwise
   conflicts between them were not tested.
3. Then delete the lanes with `-d` and the adversarial siblings with `-D`.

## Update — 2026-10-14

The integrator (pid 2558) is still running. `main` is unchanged at `8026dd7e`.
`bl/integrate-1013` is now 69 commits ahead of main. It contains `card22-merge-teardown`,
`46bece0d-typed-mcp-args`, `card16-pulse-walks`, `card26-nested-worktrees`, `card14-entity-verdict`,
`card22-s511`, `card24-event-time` and `e023906b-doc-rename` (each checked with
`merge-base --is-ancestor`).
Still outside it, with their worktrees locked by live agents: `card10-mcp-budget` and
`f16fad35-write-primitives`.

Overnight, Claude Code created 11 more `worktree-agent-*` branches, all at `98c776d4` and merged
into main.

- Deleted with `git branch -d`: the 9 whose agent worktrees are already gone.
  - worktree-agent-a3762c71205a72f67
  - worktree-agent-a46bc324d6a6cbc5f
  - worktree-agent-a4ce41c912fa940bd
  - worktree-agent-a50b6a24c5698bcdd
  - worktree-agent-a8101db40d2c0f57f
  - worktree-agent-a91792fbce48d0b7b
  - worktree-agent-a992ad00d79dd4ca5
  - worktree-agent-ae30b24a709802815
  - worktree-agent-af14ed0a393397ff9

  Each one's backup is `refs/backup/2026-10-14-consolidate/<branch>`. Restore with
  `git branch <branch> refs/backup/2026-10-14-consolidate/<branch>`.
- Kept: `worktree-agent-a559f981ffe165f6c` and `worktree-agent-a85c3e896d9418c43`. Their agent
  worktrees still exist and are locked by live agents, so Claude Code's own cleanup owns them.

New backups:
- Refs for all 32 branches under `refs/backup/2026-10-14-consolidate/`.
- Bundle `tmp/branch-backup/GitPulse-branches-2026-10-14.bundle` (`git bundle verify` OK).

`git fsck --connectivity-only` exited 0, with only `dangling` lines in its output.

Still not merged to main, and still owned by the integrator: every `bl/*` branch and
`bl/integrate-1013`. `claude/hopeful-nash-2ccfb1` (1 ahead) has a live session in its worktree.
The lane branches whose worktrees the integrator has removed stay until `bl/integrate-1013`
lands on main. Then `git branch -d` will accept them, and the `*-adversarial` siblings go with `-D`.

## Verification (Phase 6)

- `git fsck --connectivity-only`: exit 0. Its only output is `dangling` lines (unreferenced
  objects, which are normal); no missing or broken objects.
- `gitpulse_insights` after the deletions: all facets `ok`. The same 9 live sessions and 12
  worktrees are present, and every worktree still has its branch. No collisions.
- `ci:local` was **not run**. main did not change, so there was nothing new to test. Running it in
  the shared main checkout would also have tested other sessions' uncommitted edits
  (`verification_driver*.rs`).
- Gap: the DevMap MCP server failed to connect (ENOENT on its binary path), and the GitPulse code
  graph is analyzer-stale. No DevMap impact checks were run, since no merges happened.
