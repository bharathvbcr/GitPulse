# GitPulse backlog build: 2026-10-13 / 14

## Where the work is

- **Integration branch.** Every approved card is on its own `bl/*` branch and is merged with `--no-ff` into `bl/integrate-1013` (worktree `.claude/worktrees/bl-integrate`), head **7c440982** (waves 1–7, 2026-10-16). Integration was cut from local `main` @ `8026dd7e`, and `main` has not moved since, so `main` can fast-forward to it.
- **Not on `main` yet.** This session's gate refuses git in the main checkout, so the fast-forward is one command for the owner (see "Merge to main" below). `origin/main` is 522 commits behind local `main`. Nothing was pushed.
- **Process for each card.** An Opus brief, then a Sonnet implementation in an isolated worktree with failing tests first, then a Sonnet adversarial pass. Every break went back to the implementer, then to an Opus review that approved or returned the card. Fable made the sequencing and judgment calls.
- **Board status.** Cards are in `review`, not `done`. `done` is evidence-gated by Manvi verification of a head that contains the change, so a card moves to `done` after `main` fast-forwards and Manvi verifies it.

## Merge to main

Run from the main checkout:

```
cd /Users/bharath/Code/devtools/GitPulse && git merge --ff-only bl/integrate-1013
```

The main checkout's uncommitted files (2026-10-16) are another session's `src-tauri/src/workbench/verification_driver*.rs`, plus `AUDIT_REPORT.md`, `AUDIT_REPORT-lag-2026-10-12.md`, `BRANCH_REPORT.md`, `BUILD_REPORT-perf-2026-10-15.md` and `IDEAS-herdr-scion-2026-10-13.md`. `git diff --stat 8026dd7e 7c440982` over those seven paths is empty, and `main` (8026dd7e) is an ancestor of the head, so the fast-forward is clean and does not disturb them.

## Final verification on the integration head

### After wave 7 (2026-10-16)

The integration head is **7c440982**. `ci:local` ran on 609605ce, and the only commit after it adds explicit vitest budgets to two tests. Logs: `tmp/build-1013/verify/ci-local-609605ce.log`, `coverage-7c440982.log` and `cargo-tests-ccb83efa.log`. Host load was 60–117 on 18 cores throughout, from about 11 concurrent sessions.

| Check | Result |
|---|---|
| `bun run ci:local` @ 609605ce | **14 of 17 gates pass.** The 3 failures are Rust tests with coverage, Frontend unit tests with coverage, and the coverage floor they block. Every failing test is a load flake that passes alone, or serve-ops (Manvi drift), except the FSEvents-stall family below |
| Rust tests with coverage (`--test-threads=1`) | lib **4,453 passed, 7 failed** (all `watcher::`), 21 ignored. Integration targets: `watcher_storm_keeps_session_responsive` and one `terminal_output_emits` test failed (that file's fix sits unmerged on `pf/626b25fc-output-emits`); everything else ok |
| `cargo test --tests --test-threads=6` @ ccb83efa | Found a **deterministic** contract failure: `supervised_threads` (see "Process gaps"), fixed at 609605ce. Every other failure passed alone |
| Contract tests after the fixes | `hermetic_contract` 10/10, `supervised_threads` 8/8 |
| Frontend unit tests | 10,433 passed, 26 failed in 11 files. Rerun alone, 10 files passed (147/147); the eleventh is serve-ops. Two of the timeouts were this build's own tests at vitest's 5 s default; they now carry 60 s budgets (7c440982) |
| Frontend coverage (serve-ops excluded, `--coverage.reportOnFailure`) @ 7c440982 | 10,437 passed, 2 failed (`vite-watch` nested-tsconfig, an FSEvents stall that also fails alone at load ~77; this build never touched it) |
| `check:coverage` | **Floors hold**: frontend lines 96.70% (≥ 90), branches 90.24% (≥ 85); Rust lines 89.71% (≥ 80) |
| Browser regressions (Chrome) and native renderer (WKWebView) | Pass |
| `check`, Svelte and TypeScript, IPC and coverage-type contracts, clippy, fmt, actionlint, vendored crates and schema | Pass |
| Benches | `bench-compare` reports no counter change against the updated macOS baseline. Times were inflated 2–14× by load, which the comparator itself notes |
| `git fsck --no-dangling` | Clean |
| GitPulse Insights | Every facet ok. Contended files are only `BRANCH_REPORT.md` (untracked) and `verification_driver*.rs` (another session's). The code graph is stale (generation 8 behind the source), so no DevMap answer was taken as complete this wave |
| `dev verify` | **Could not run**: no DevCouncil state. Not a pass |

**What is not green, and why it does not block the fast-forward.** Every failing test is in code this build did not change, except the two vitest budgets this build added (fixed) and the fsmonitor test, which is now bounded rather than hanging. The watcher, `vite-watch` and `watcher_storm` failures share one shape: the OS delivered no file events under load (`raw_events=0`, "control reload never observed"). ft-626b25fc §7 records them, with a criterion that each must tell an OS stall from a GitPulse defect. That is an inference from the failure messages and the base-vs-branch counts; nobody has shown that FSEvents is the cause.

### After wave 4 (2026-10-14), kept for reference

The integration head was **5dcb4255** (waves 1–4 plus the CARD 14 test fix). Logs: `tmp/build-1013/verify/ci-local-5dcb4255.log` and `coverage-5dcb4255.log`.

| Check | Result |
|---|---|
| `bun run ci:local` @ 5dcb4255 (default settings) | 15 of 17 gates pass. The 2 failures are the serve-ops test and the coverage floor it blocks (see the notes below) |
| Rust tests with coverage @ 5dcb4255 | lib 4,262 passed, 0 failed, 20 ignored; every integration target ok |
| Browser harnesses @ 5dcb4255 | Chrome and WebKit gates pass |
| Frontend unit tests @ 5dcb4255 | 10,370 passed, 1 failed (serve-ops), 1 skipped |
| Frontend coverage, serve-ops excluded, @ 5dcb4255 | 10,351 passed, 0 failed (the serve-ops file holds the other 20 tests) |
| `check:coverage` on that run | Floors hold: frontend lines 96.66% (≥ 90), branches 90.17% (≥ 85); Rust lines 89.71% (≥ 80) |
| `git fsck --no-dangling` @ 5dcb4255 | Clean |
| GitPulse Insights @ 5dcb4255 | All facets ok. The only contended file is `BRANCH_REPORT.md`, untracked both here and in the main checkout, so it is not committed |
| Earlier run @ a7390b3b | 14 of 17. The extra failure was CARD 14's random-seed property test (see "Process gaps") |

The rows below are from the wave 1–3 head (d8b51eed), kept for reference:

| Check | Result |
|---|---|
| `bun run ci:local` @ d8b51eed | 15 of 17 gates pass, with the same 2 failures |
| Frontend coverage, serve-ops excluded | 10,228 passed, 0 failed |
| `check:coverage` on that run | Floors hold: frontend lines 96.63% (≥ 90), branches 90.12% (≥ 85); Rust lines 89.65% (≥ 80) |
| `cargo test --no-fail-fast -- --test-threads=1` @ c13d0af8 | 119 targets ok; 5,600 passed, 0 failed (lib 4,230) |
| `cargo fmt --check`, `clippy -D warnings` | Clean |
| `bun run test` | 10,247 passed, 1 failed (serve-ops, pre-existing) |
| `check`, `check:types`, `check:ipc` | OK: 99 contracts, 247 structs, 1,733 fields; 282 handlers |
| Browser harnesses (Chrome and WebKit) | 85/85 in each |
| `check:schema-parity` | 6 match, 2 stale. Both stale rows predate this build, which changes nothing under `contracts/` or `src-tauri/vendored/`. They are listed under owner decisions |
| DevMap after the last wave | Rebuilt; status fresh. 8 vendored `.sql` files are import-blind |
| GitPulse Insights after the last wave | All facets ok; 6 worktrees, 0 contended files |

Notes on the two `ci:local` failures:
- **Frontend unit tests with coverage** failed only on `scripts/serve-ops-contract.test.ts`, the known Manvi drift that also fails on `main`.
- **Coverage floor** had no frontend report to read, because vitest writes none when a test fails. That run was repeated without serve-ops; its result is the second and third rows of the table.

## Tasks moved to review

| Card | Task | Branch @ sha | Highlights |
|---|---|---|---|
| CARD 22 | ft-9d04447403db7f9525dce4db970e4b8a | bl/card22-merge-teardown @ f5dd9d65 + bl/card22-s511 @ b35b55d4 | A conflicting merge is rolled back, but only one this call started, and only with the gate's consent. A landed merge is never reported as "Not merged". Tip and base binding, typed errors, the wave halts. S5-11: lock held 4.38 s → 6.5–16 ms at 100k entries via rename-aside |
| CARD 16 | ft-9cfa4e7403d33c0e25e752db97175af9 | bl/card16-pulse-walks @ bab7d194 | No `%G?` in bulk walks (a verify-on-select command with a cache). 500 signed commits: 21 s → 28 ms. DORA uses `--since-as-filter`, or a count-bounded fallback below git 2.37 |
| CARD 26 | ft-9d04407403db78c925dce0db970e44be | bl/card26-nested-worktrees @ 26b1178f | The longest family path wins. Departed worktrees keep their own path. The off-cap owner keeps its own row. An unresolved family writes nothing |
| ft-46bece0d | ft-46bece0d36708fd4d84c3056c790d6d3 | bl/46bece0d-typed-mcp-args @ e9962c86 | One `tool_args!` declaration per tool drives both the schema and the extraction. Whole floats are exact up to 10^15−1. The revision guard is honoured |
| CARD 14 | ft-9cfa4c7403d338a825e750db97175793 | bl/card14-entity-verdict @ da3e73de | Qualified-name join with no File symbol. Insertion hunks counted. Touching edits are never Disjoint. Staged and worktree diffs against HEAD. 3+ parties |
| ft-e023906b | ft-e023906b27b02a768cd06965aefccb79 | bl/e023906b-doc-rename @ 13b87213 | Links resolved relative to each note, including the moved note's own. Compare-and-swap writes; each rewritten path is gated; truncation is reported |
| CARD 24 | ft-9d043e7403db756325dcdedb970e4158 | bl/card24-event-time @ c079596e | `occurred_at` and `task_basis` in detail_json (a validated post-redaction splice). `binding_as_of` history. Catch-up runs before bind |
| CARD 10 | ft-9cfa507403d33f7425e754db97175e5f | bl/card10-mcp-budget @ fb284c52 | Over 4 MiB: answered with an error, wire stays open. Write window bounded (10 s default, 10 min clamp). Compact text mirror, full structuredContent. Ledger pages cut in-frame. Unfollowable pages refused, never `has_more` with a null cursor. Ledger `object` capped at 2048 B after redaction, with hash and marker; one dedupe key. Adversarial pass: 8 breaks, all fixed. Opus: 3 rounds |
| ft-f16fad35 | ft-f16fad357f5d722a035cedbf27147663 | bl/f16fad35-write-primitives @ 57fbedd2 | Exclusive create. Compare-and-save against the draft's base (Reload / Compare / Overwrite). `.git` refused on every write path. Repo root refused for delete, move and create. Symlinks moved and deleted as links. Adversarial pass: 6 breaks, all fixed, 2 of them pre-existing and dangerous (`delete_path("./.")` ran `git rm -r -- ./.`; moving a symlink moved its target). Opus: 2 rounds |
| (fix) | — | bl/card16-tabswitch-fixture @ 481247ff | The tab-switch harness lacked a `cmd_verify_commit_signature` fixture after CARD 16 |
| ft-1629347b | ft-1629347b18bc4aab9c73cc3e3602ff32 | bl/1629347b-repostore-contract @ ed0922c0 (merged 28e6bcc8) | One repoStore mutation contract: a `MutationTarget` per call, a prompt before a tab switch that would strand a mutation, `reportFailure` and `offerBranchUndo`, a per-path queue in `runMutating`, one echo rule with replay. A TypeScript-compiler-API scanner (`src/lib/stores/mutationScan.ts`) enforces the rule. Adversarial pass: 7 breaks, all fixed. Opus: 2 rounds. bun 10,370 passed; browser 33/33 in Chrome and WebKit. AC14 (`dev verify`) unmet: the gate could not run (see Deferred) |
| CARD 20 | ft-9d04427403db7c2f25dce2db970e4824 | bl/card20-default-base @ 3f5f290e (merged 94443dfb) | `default_base` goes through `ref_cache`, reading the ref stamp once per call (`through_stamped`). Destructive callers read fresh: the null-target merge, deadbranch clean, `is_default_branch`. Nested default branches (`refs/heads/release/main`) are seen at once. Removed `DefaultBaseStamp`, `MemoizedBase`, `main_ref`, `MainRef`, `is_main_name`. The divergence fallback uses `refs/heads/`, and its label is now `base↕:` |
| ft-9fa261d9 | ft-9fa261d9dbc034fed073e892286cb805 | bl/9fa261d9-grace-drain @ 27a16826 (merged a7390b3b) | R2-02: `procguard::stop_and_reap_waiting`. `git_cli` `ask_and_drain` keeps draining the child's pipes during `CHILD_GRACE` and closes stdin just after the stop request. A cleanup that reads to EOF or writes a lot still finishes inside the grace. Full cargo: 4,243 passed, 0 failed |
| (fix) | — | bl/card14-deterministic-property @ 4707c2c6 (merged 5dcb4255) | CARD 14's `run_property` drew a fresh random seed on every run and failed when no case was Disjoint: 4 of 40 runs failed. Each test now uses a fixed ChaCha seed, and the vacuity guard is kept. 0 of 40 runs failed |
| ft-acff3877 (wave 5) | ft-acff387700ee80d3475f38d7bfeaa7f6 | bl/acff3877-denied-stays-denied @ 8715cb53 + bl/acff3877-adv @ 2a8fce4d (merged 5fc8af55) | A denied shared-budget token stays denied when its record cannot be rewritten (`Take::DeniedUnrecorded`, logged once per report window). A record is read as Newer only at the full record length. Adversarial pass: an unrecorded denial no longer lets the next writer refill twice. Full lib 4,273 passed, 0 failed. Residuals in ft-d41b8491 |
| ft-dadcd71d (wave 5) | ft-dadcd71d9f79d90cc8d7c991e564366f | bl/dadcd71d-merge-file-diff @ 9ec8ef1b + bl/dadcd71d-adv @ 5397806e (merged 0f0e0b4b) | One owner for how a merge commit is rendered: `GitReader::merge_render_args` and `commit_show_args`, used by the commit diff, the per-file diff, the file list and the details. `--diff-merges=first-parent` from git 2.31, `-m --first-parent` below it. 52 targeted tests pass on the merged head. Residuals in ft-ccf03e33 |
| ft-04f5dd26 (wave 5) | ft-04f5dd267cca23c37aa3e9e6ede6a238 | bl/04f5dd26-scan-deadline @ 1b91f9a6 + bl/04f5dd26-adv @ 8b7c364d (merged d14568c1) | One `ScanDeadline` (90 s `AUDIT_TIMEOUT`) bounds dependency and coverage scans. Each command gets what is left as its timeout. A cut scan is marked truncated, names what it skipped, and is not cached. A refused probe-cap check now truncates. The end-of-file check reads only to the highest recorded line (8,000 reads per scan). Cache hits share the hit maps. Adversarial pass: 4 breaks, all fixed (a deadline-refused probe read as "Install cargo-audit"; own failures labelled as cuts; a cut step counted complete; negative TS counts). Opus sent it back once to reuse `git_cli::is_timed_out`. Lane full lib 4,285 passed, 1 failed (watcher flake, passes alone); merged-head targeted 550 passed |
| ft-43cfe399 (wave 5) | ft-43cfe39937d2fa33d00809c43999c7e4 | bl/43cfe399-repo-reanchor @ fbe3e99b + bl/43cfe399-adv @ c395700c (merged cd52ff3c) | A moved or renamed repository keeps recording. The ledger records its identity (root commits plus normalised remotes, redacted), and a first pass after a move writes one `ledger.reanchor` row naming the old paths. A different repository's ledger is still refused. No schema or writer-version change. Adversarial pass: 6 breaks, all fixed (a credential in a remote made a moved repo look like a fork; query strings kept; a `.GIT` suffix; a 10,000-path forged row broke the query; relative or control-character paths accepted; one busy ledger stalled every repo's settle). Lane full lib 4,300 passed, 0 failed. **The owner chose to merge with two cases accepted** (see Owner decisions). `command_timings` counters: catch-up and insights rows unchanged, so re-anchoring starts no git process on measured paths |
| ft-84a33d12 (wave 6) | ft-84a33d1225cd18f0c255f7a071646eb1 | bl/84a33d12-nonewline @ cfedc808 + bl/84a33d12-adv @ 6e28f02b (merged 2c1fe098) | The no-newline marker is flagged on a context row too. Adversarial pass found an older selective-staging bug: with a partial selection the marker could land mid-hunk and git joined lines (`a` → `a\nb`, stage only `+b`, gave `ab`). Both renderers now write the marker only where its side ends. 20 real-git stage/unstage tests over every selection subset; fuzz 58/192 failures → 0/1,120. The TS serializer has no production caller (a test mirror) |
| ft-24f6212d (wave 6) | ft-24f6212d4b656e2e278fe0294e248997 | bl/24f6212d-joiner-deadline @ 1feae707 + bl/24f6212d-adv @ 0b7464bf (merged e346ca99) | A follower of a shared read returns by its own deadline with the solo-call timeout error, and reruns within its budget when the leader timed out. Adversarial pass: 5 breaks fixed (12 joiners became 13 children after a leader timeout, now 2; a lost prefix; a wrong budget in the message; a rerunning joiner counted as coalesced; a hook-running command could be shared and so ignore cancel) |
| ft-513113a3 (wave 6) | ft-513113a31ae2b0f87807fd8548dfd2b7 | bl/513113a3-pipe-permit @ 21effc2b + bl/513113a3-adv @ efd68985 (merged fc088f9d) | **Done on the Unix harness, execution-unverified on Windows** (ft-cd2a98d7 is the gate). std's Windows child pipes are overlapped, so `CancelSynchronousIo` never cancelled them. Now an overlapped read/write waits on its I/O event and a sticky cancel event; on cancel, `CancelIoEx` then a drain. Fable sent back two designs (a 10 ms poll; a parent-held handle) before this one |
| ft-2a2008e9 (wave 7) | ft-2a2008e955775085649eeafd952a1802 | bl/2a2008e9-conflict-mode @ 1e195c5f + bl/2a2008e9-adv @ 45f7aff1 (merged 86a41ffb) | Windows conflict saves keep the index executable bit: one `staged_mode` owner, and a symlink conflict is refused before any write instead of claiming the link is kept. The mode rule matches real `git merge` in every conflicting (base, ours, theirs) case. Verified on a Unix simulation, not on Windows |
| ft-d41b8491 (wave 7) | ft-d41b8491a0dc3b133de6526703717614 | bl/d41b8491-budget-residuals @ 07eaa3b6 + bl/d41b8491-adv @ 4a067184 (merged 71d41a54) | A torn budget record can no longer over-credit (it was up to +103 tokens); the layout and file name are unchanged. Denials that could not be saved get their own counter. **Owner ruling:** keep the fail-open when record writes fail permanently |
| CARD 17 (wave 6) | ft-9cfa4f7403d33dc125e751db97175946 | bl/9cfa4f74 bench @ 50110cb9, fix @ 947a2827 + adversarial @ 99c32651 (merged 3806c4ae; doc count 88801492) | A stack refresh makes one shared first-parent pass, with a per-tip fallback when the shared cap is cut; history fetches cap + 1 rows in one log. Adversarial pass: a shared-cap overrun no longer leaves shallow stack branches unresolved. Spawn counters: stack cold 28 → 9, refs unmoved 20 → 0 |
| ft-d61584de (wave 7) | ft-d61584de48c5cab38ef481a3ac55bb94 | bl/d61584de @ 440e79fa + 4318b370, adversarial @ 3d7a06e3 (merged 1e71c710) | Worktree prune and cow-sync, file move, delete and create, branch cleanup and the palette now go through `runMutating`. Adversarial pass: the scanner now reads constants, prefixes and unreadable bridge calls; a create refusal is path-scoped; a clean result for another repo is no longer drawn over the repo in front |
| ft-ae9db92a (wave 7) | ft-ae9db92a9189bf3c2881f720d7c03271 | bl/ae9db92a @ 93995cd2, adversarial @ a08acf59, ea929dc3, 2314e5b0 (merged 67d64b4e) | Worktree add, remove, lock, unlock, merge teardown and the attempt branch delete go through `runMutating`. Adversarial pass: `targetForPath` resolves a symlinked spelling and refuses a tab that is gone; an attempt opens the main checkout only after approval; remove refuses once its repository is no longer in front; a trust check that throws is reported, not left as an unhandled rejection. Merged head: `bun run check` clean, vitest 3,314 passed |
| CARD 19 (wave 7) | ft-9cfa497403d3338f25e75bdb97176a44 | bl/9cfa4974-churn-key @ fa77dc2c, c51bb658, c522a121 + bl/9cfa4974-adv @ 98632bec, 215d0a81 (merged 10ba9b63; test fix c462eda6) | Branch churn is keyed by tip and records its base. It is carried over a default-branch advance when the new base descends from the old one and the ahead count is unchanged (equal ahead sets imply the same merge base and diff). At most 4 ancestry checks a call; anything unproven is measured. Bench: 44 → 13 processes, shortstat 32 → 0. The derived cache moved to schema 2 **in a new file, `churn.v2.sqlite`**. The adversarial pass found that one shared file name made a v1 and a v2 build drop each other's table on every start. The v1 file is left untouched for the older build, so no stored data is migrated or dropped. Oracle: 8 seeded random histories of 22 steps each; none differed from fresh git. A mutation of the carry rule made the oracle fail. The orchestrator fixed a flaky cap test on the merged head (two tips shared one base, so the count depended on HashMap order); it then passed 10 of 10 |
| ft-f069aa42 (wave 7) | ft-f069aa425202f3abe84fe2a573dbbf6a | bl/f069aa42-baseline @ 7ed20518 + e8328137 (merged 9103bda4) | The `command_timings` counter pass warms `git_preflight` first, so commit details counts 1 git process, not 2. Opus sent it back once: the first pin compared `argv[0]`, which trace2 records as the git binary's path, so it could never fire. Now, with the warm-up removed, it fails, showing `git version` in the counted pass. `list_worktrees full` rebaselined 38 → 42, attributed to CARD 20 by A/B. `graph_spawns` and `churn_spawns` were seeded into the macOS baseline. On the merged head, `bench-compare` reports no counter change; times failed 2–14× at load 78–117, which the comparator marks as inflated. No separate adversarial lane (Fable): the change is bench and docs only, and the pin's mutation proof was the attack |
| (fix) | — | ccb83efa | `hermetic_contract::no_wall_clock_assertion_bypasses_the_scaling_helper` was failing on 9103bda4. ft-43cfe399's and ft-04f5dd26's adversarial commits added three bare `Duration` limits. All three now go through `coverage_relaxed`, each still below the wait it rules out at the 4× load cap (the auditor limit 15 s → 7 s; the busy-ledger hold now scales with the limit). Reported by the pf/integrate-1015 session |
| ft-ccf03e33 (wave 7) | ft-ccf03e330fa4156eb3c774c94e8fda2b | bl/ccf03e33-rename @ 5b8df4f7 + 88a5fa20, bl/ccf03e33-adv @ bb4ef1fc + 39b09f7f (merged 15df37f5; banner wording 57e566f0) | Residual 1 only. A file's one-path commit diff now pairs renames and copies the way the file list counts them, on merges, plain commits and first-parent changes alike: plain renames had the same bug. Only a file that comes back added pays for the pairing read: a modified file costs 1 `show`, a renamed one 3. Output is filtered to the requested file's own sections. Opus sent it back once (cost, scope). Adversarial pass: 5 breaks, all fixed (C-quoted names, the binary notice, E2BIG on a 6,000-file directory rename, a copy whose edited source used up the cap, a silent cut rename list). Seeded oracle over 14 seeds; a mutation gave 117 problems. Merged head: `engine::git_reader` 204 passed. **Criterion 2 (the pre-2.31 fallback) is unmet, gated on OD 13.** Residuals: the banner's "showing the first N lines" is wrong for a cut rename list over 4 MiB; a directory view shows an unpaired deletion for a file renamed out of it |
| ft-626b25fc slice (wave 7) | ft-626b25fc45ff369aaf2b5fa1574cb6c3 | bl/626b25fc-bounded @ 7aab0c58 (merged bf89c241) | The two tests that hung (29 and 43 minutes) are now bounded through `git_cli::run_bounded`, via a new `test_support::run_within`. **fsmonitor cause verified by sampling:** `git status` blocks in `fsmonitor_ipc__send_query` `read()`, waiting on a daemon whose `handle_client` waits on a condition variable that is never signalled. Why the daemon never wakes is unverified. The `hdiutil` cause is inferred: it was the only unbounded wait. A simulated hang (`sleep 600`) fails in 11.9 s with a message. The fsmonitor test still *fails* 7 of 10 under load, now at a bound instead of a hang. The card stays in progress for its other sections |
| (fix) | — | 609605ce | `supervised_threads::every_long_lived_thread_starts_through_the_supervisor` was failing on the integration head, found by the `--tests` sweep. Its scan stopped at the first `mod tests` line. ft-513113a3 had put one inside `thread_io.rs`'s Unix module, so the scan stopped short of the production pipe worker and reported the exemption as stale. A nested test module is now skipped to its closing brace (pinned by a test that counts 1 under the old scan instead of 2). `ledger/anchor.rs`'s one-shot deadline probe (ft-43cfe399) is listed with the `teardown.rs` reason |
| ft-02b836b8 (wave 8) | ft-02b836b8ac1b0fc3f37e0b1cd87a3294 | bl/02b836b8-word-diff @ 82155a40 + bl/02b836b8-adv @ 4640ded6, 1bcc20b8 | Word diff annotates the rendered row's pair plus up to 24 neighbours each side within a 1M-cell budget, not the whole replacement block. First visible row of a 2,000-pair block: 2,729 ms → 3.3 ms (TS bench). Adversarial pass: 1 break, fixed. A del row after an add row joined the previous block and had no highlights when it was the only row rendered; a 150-seed oracle found it, but only in partial-scroll order. Diff browser harness 68/68. Merged head: `bun run check` clean, 2,583 diff, component and store tests pass. The work is bounded per call, not moved off the UI thread (the card allows this) |
| ft-7ba9b4b2 (merged on request) | ft-7ba9b4b2 (the main-checkout session's card) | gitpulse/gitpulse-hook-collision-guard-pays-a-col-e12136e7 @ 8365f620 (merged 8966ce1c) | The owner asked, through the main-checkout consolidation session, for this branch to land on integration rather than `consolidate/1015`. `observe_sessions` (used by `gitpulse-hook` and the insights facet) reads a process's arguments only when its name or resolved path could be an agent. Opus checked the filter against `classify`: every agent shape it recognises is still read. A failed table read stays an error, and an unread footprint is Unavailable, not zero. Gates on the merged head: fmt, clippy (lib, tests, benches), 76 `sysload::` tests, the changed terminal test, `hermetic_contract` and `supervised_threads`. The card stays with its owning session; this build did not move it. Not run here: its own adversarial pass and the `host_load` bench |
| CARD 7 (merged on request) | ft-9cfd4f74 (the main-checkout session's card) | audit/card7-hosted-records @ c558599e (merged 5fa26843) | The owner asked, through the main-checkout session, for this merge now rather than after wave 8. Hosted Claude records are only what was launched, and Resume offers only conversations Claude Code saved (`cmd_terminal_conversations`). Exit events say whether GitPulse ended the session (`ended_by_gitpulse`, set by every Rust, TS and harness constructor). Conflicts were count lines only, resolved inside the conflict regions, with counts from the checkers: 283 handlers; 100 contracts, 248 structs, 1,740 fields. **Security impact** (in the merge commit): one new IPC command that answers found, absent or unknown per canonical-UUID session id, never content, at most 512 per call, reading only Claude Code's project directories. Gates on the merged head: contract tests, `bun run check`, clippy, 192 Rust terminal and conversation tests, 816 frontend terminal tests, `hermetic_contract`, `supervised_threads`, `terminal_incident`. The coverage browser harness, which hung 5 of 5 on an earlier merge of this branch at load 100–700, passed 163/163 in Chrome and in WebKit at load 22–45. The card stays with its owning session |
| CARD 18 (wave 8) | ft-9cfa487403d331dc25e75cdb97176bf7 | bl/9cfa4874-commit-graph @ d8bad8d6 + bl/9cfa4874-adv @ d82bd4a6 (merged ec182865) | Read-only half. It detects present / no Bloom / stale / missing / no history / unknown from the commit-graph files with bounded seeks, starting no git beyond the shared cached `config --list`. The Storage panel shows the exact command for the person to run; writing is OD 3 and was not built. Bench: at 50k commits, no graph costs about 10× on history pages and ahead/behind, and Bloom filters add about 3× on path log and blame. Adversarial pass: a 600-mutation fuzz against `git commit-graph verify` found 14 hostile files read as valid (now 0), plus chain, hash-version, Bloom-pairing and `core.commitGraph=false` breaks. Opus sent it back once to replace a hand-written git config parser with the canonical `config_snapshot` (net −48 lines). Two type-contract gaps were fixed at the merge: `CommitGraphReport`, where a layout enum had drifted from an inline union, and ft-3a32f751's `CheckoutOutcome`, which had been unchecked since f967968f. Counts: 102 contracts, 250 structs, 1,749 fields |
| ft-3a32f751 (wave 8) | ft-3a32f751e95ebdc3e9ce17ef7b949e7c | bl/3a32f751-git-edges @ b1076250 + bl/3a32f751-adv @ 706adc9a (merged f967968f) | Checkout never detaches HEAD: one `switch`, judged and run as the same argv (re-derived under the repo lock); remote rows land on a local tracking branch; detach is its own gate-judged command (`cmd_checkout_detached`). A squash or fixup with no kept commit before it is refused, with one plan table checked against Rust, the modal and real git (17 plans). A multi-commit cherry-pick or revert is detected from `.git/sequencer`. `refs/heads/` is stripped once. Adversarial pass: 5 breaks fixed (judged-vs-run TOCTOU, four remote-mapping bugs, an oversized sequencer todo misread as a cherry-pick, the missing ACL grant). Security impact recorded in the merge commit: narrower than before. Merged head: 284 handlers; 348 engine tests; acl, hermetic and supervised contracts; 753 vitest; palette harness 48/48. Merged before ft-4ef24637 against Fable's order, because it finished first; the two share no files |
| ft-4ef24637 (wave 8) | ft-4ef246374ebac048f106db5e184fefbd | bl/4ef24637-portless @ 275b10be + bl/4ef24637-adv @ 29bf7b36 | Whole-label hostname matching through one DNS-label parser; `::1` probing; checked port and pid conversions; FNV-1a port hash pinned to published vectors; one `routes.json` read per listing. Adversarial pass: a FIFO at `routes.json` blocked forever; URL syntax in a label linked to another host; Unicode case folding matched the Kelvin sign; a silent 256-route cap; serial probes up to 25.6 s (now up to 32 scoped threads under a 1 s budget, with counted omissions). Merged head: 206 portless and worktree tests and both contracts |
| pf tiers 1, 2 and 4 (handed over) | ft-626b25fc §4/§5, ft-79394019, ft-434d7bef (the perf session's cards) | pf/626b25fc-output-emits (24897789), pf/626b25fc-signal-guard-fp (c3ef66a3), pf/79394019-bridge-adv (ed557abc), pf/79394019-external-adv (0cad5891), pf/434d7bef-adv (86dd1e6f) | Handed over by the pf/integrate-1015 session with Fable's approval, already Opus-reviewed there. Rule: a red gate goes back, never fixed forward. Gates after each tier: fmt, clippy (lib, tests, benches), the touched modules (192 tier 2 tests, 303 tier 4 tests), `hermetic_contract`, `supervised_threads`, `acl_contract`, and 3 of 3 runs of both tier 1 tests. **`pf/7d5029c8-cancel-timer-adv` was sent back**: `supervised_threads` failed on a raw thread start in `codeintel/mod.rs`, so the merge was reset off the unpushed local tip. The flagged start turned out to be test-only: the scanner did not recognise `pub(crate) mod tests`. The re-handed tip (a15d754e) fixes the scanner to recognise test modules of any visibility and to require the opening brace, which is stricter than before. Opus reviewed it here and checked that every production start in that file goes through `supervisor::spawn_service`. Merged; `supervised_threads` 9/9, `codeintel::` 76/76. Then `pf/7d5029c8-tool-capability-adv` @ e5cefe93 (tool capability cache: a Present answer is revalidated by len/mtime stamp, an Absent answer expires after 60 s, one probe per stale entry, and an invalidation epoch stops a stale in-flight probe from storing its answer): merged, with 194 tool and sidecar tests and both contracts passing. A cache hit now costs about 2 µs instead of 12 ns, in front of a process spawn. Tier 3, `pf/1de1ff48-redact-adv` @ b02159aa: the ledger redactor parses each string once and skips its 15-regex chain behind a RegexSet gate built only after 256 calls. Steady state is −26 to −29% per `redact::text` row. That session reports byte-identical output against 0f0e0b4b; this build did not re-run its oracle in release. The fix for the bridge flake, `pf/79394019-bridge-idle-flake` @ 906daa05, gives each accept loop its own wake count. A loop that was still retiring had been counting against the idle test, now reproduced deterministically. Merged at b91c79fc: 442 ledger, secrets, logging and alerts tests, and both contracts. **New flake from tier 2:** `an_idle_accept_loop_does_not_wake` failed 1 of 9 runs; the owning session was asked to harden it. Card status is left to that session |
| (fix) | — | 7c440982 | Two of this build's own vitest tests timed out at vitest's 5 s default in the end-of-wave `ci:local` (load ~90): the real-git `noNewlineSelection.contract` cases (ft-84a33d12) and `repoStoreOutcome.adversarial` attack 3 (ft-1629347b). Both pass alone. They now carry explicit 60 s budgets, the repo's pattern for slow tests; nothing they assert changed |

## Deferred or blocked (and why)

| Card | Reason |
|---|---|
| CARD 1 ft-9cfd5574 | Gated on the in-progress incident cards ft-42081f16 and ft-0ac24164 (never touched). CARD 7 was on this row too; another session built it, and it was merged on the owner's request on 2026-10-16 (see above) |
| ft-7ba9b4b2 | hooks/mod.rs; gated on ft-42081f16 and ft-6060bbd7 |
| ft-45b8099a | Owner Decision 13 (git floor) is open |
| ft-5a4e25c8 | Needs a person with real accounts; not an agent task |
| F6 ft-96175ecc | Its referenced lane implementation is in none of the worktrees |
| `dev verify` on every card (AC14 on ft-1629347b) | `devcouncil verify TASK_ID` answers "DevCouncil state is unavailable in this directory". Recorded as a gate that could not run, not a pass |

## Benchmark deltas (release, min-of-N, interleaved A/B)

| Card | Row | Before | After |
|---|---|---|---|
| CARD 16 | pulse signed 500 | 21.06 s | 28 ms |
| CARD 16 | pulse unsigned 5,000 / 25,000 | | +4.8% / −1.3% |
| CARD 16 | pulse unsigned 500 | | +40% (+3 ms; justified: one extra process start-up, documented in PERFORMANCE.md) |
| CARD 22 S5-11 | teardown lock-held, 100k entries | 4.38 s | 6.5–16 ms |
| CARD 22 S5-11 | contender wait | 4.66 s | ~22 ms |
| CARD 26 | transcript_pass 100k lines / 10k cwds | | +0.4% / −0.2% |
| CARD 26 | 10k departed cwds (new row) | | +9.8% (accepted: those rows used to be written wrongly, as NULL) |
| CARD 24 | transcript_pass 100k / 10k cwds / 10k departed | | +2.2% / +2.3% / +3.6% |
| CARD 10 | tools/call insights / collision_risk / codeintel status | 36.4 / 26.6 / 45.8 ms | −3.6% / −2.3% / −7.7% (10 pairs, min-of-10; an earlier pass at load 24–34 read +11–20%, which was noise) |
| CARD 10 | tools/call rejected argument / resources/read manifest | 2.4 µs / 560 µs | −8.8% / −1.0% |
| ft-f16fad35 | get_file_blob 1 KB / 1 MB / 8 MB (editor open) | 0.130 / 0.966 / 7.64 ms | +3.6% / **+27.9% / +20.4%** (+0.27 ms / +1.6 ms). The gap held in all 3 pairs. Justified in PERFORMANCE.md: the base digest is what refuses a save over an outside edit. Paid once per editor open; view-only reads skip it. Measured by the orchestrator on the shipped build (d8b51eed) |
| ft-f16fad35 | save_edited_text 1 KB / 1 MB / 8 MB | 7.63 / 8.74 / 18.40 ms | within run-to-run spread / −1.9% / within spread |
| ft-46bece0d | validate rows | | −7 to −11% |
| ft-46bece0d | tools/list | | +0.3% |
| ft-e023906b | docs_refresh | | within noise |
| ft-e023906b | rename pre-check | | ~8 ms (~10% of a leaf rename; a correctness check on a rare action) |
| CARD 20 | A/A/B (base, base again, card), 5 rounds, min-of-N | | Within noise. Base vs base swung −17.4% to +38.7% per row, and card vs base −15.2% to +22.8%; no row moved beyond the base's own spread. Cold `list_worktrees` starts 42 git processes instead of 38, because destructive callers now read fresh. Justified |
| ft-9fa261d9 | A/A/B, 4 rounds, min-of-N | | Within noise. Base vs base swung −23.6% to +26.5%, and card vs base −12.4% to +22.3%. The new code runs only on cancel or timeout, off the measured path. Justified |
| CARD 14 | entity join | 0.18 ms | 10 ms at 4,000 hunks (debug build, linear; the old figure came from code that gave wrong answers) |
| ft-04f5dd26 | deps scan with `npm audit` hung 5 s | 5.04 s | 1.00 s (1 s deadline in the after build; the before build has no deadline, so this row is not like for like) |
| ft-04f5dd26 | coverage cold scan, 300 sources of 256 KiB | 107.35 ms | 91.34 ms |
| ft-04f5dd26 | coverage cache hit / `file_coverage` cache hit | 39.10 / 42.19 ms | 39.77 / 41.75 ms (flat; fingerprinting dominates, and the shared-map criterion is proven by a `ptr_eq` test) |
| ft-acff3877, ft-dadcd71d | — | | No bench row (Fable): off the measured path |
| CARD 17 | graph_spawns stack cold / refs unmoved (spawn counters) | 28 / 20 | 9 / 0 |
| CARD 19 | churn_spawns, branch_stats after an unrelated default-branch advance, 32 branches (spawn counters) | 44 processes, 32 shortstat | 13 processes, 0 shortstat. The adversarial fix only renamed the cache file, so the bench was not rerun after it |

## New tasks filed

- **ft-cb252563:** transcript cursor rotation is detected by length only.
- **ft-434d7bef:** doc rename residuals: an empty destination dir, an ~80 s hub rename, asset links.
- **ft-43cfe399 (P1):** a moved, renamed or copied repo stops recording transcripts (repository_mismatch).
- **ft-1de1ff48:** the ledger redactor costs ~3.5 µs per field.
- **ft-8f398bb6:** write-primitive residuals:
  - a nested gitdir under another name;
  - 0600 mode on new files;
  - HFS-ignorable `.git` spellings;
  - Overwrite on mixed-EOL or invalid-UTF-8 files;
  - Windows unverified;
  - DiffViewer still pays for the digest.
- **ft-541eafaf:** CARD 10 residuals:
  - uncapped LedgerEvent string fields;
  - verdict and evidence shape at size;
  - two cut suffixes;
  - probe coverage.
- **ft-d61584de:** mutation paths outside `runMutating`: WorktreesPanel prune and cow-sync, FileTreePanel move, delete and create, BranchCleanupModal, and the palette's `checkOrigin`.
- **ft-c63ec4df:** S4-05, consolidating the ahead-behind listing.
- **ft-487108b9:** the sidecar EOF grace has no drain. This is the shape ft-9fa261d9 fixed in `git_cli`.
- **Folded into CARD 1 (ft-9cfd5574):** the pre-edit verdict ignores the incoming Edit/Write.
- **Folded into ft-626b25fc (flakes):**
  - signal_guard_live process churn;
  - conflict_save trust storage;
  - devmap::live refresh timing;
  - the vitest fixture-author-hook-contract;
  - added 2026-10-15:
    - a second trust-storage test (`an_empty_repository_graph_is_a_notice_not_a_fault`);
    - a watcher retirement timeout;
    - an orphan-branch test;
    - `adv_cross_device_worktree_gets_a_sibling_aside_and_marker`, which hung for more than 29 minutes (a new criterion requires it to be bounded);
    - the test write budget aborting repeated-run loops.
  - **Correction:** `entity_collision` `blank_separators` was first filed here as a load flake. It was a defect this build introduced, and it is now fixed (see "Process gaps").
- **Wave 5:**
  - **ft-d41b8491 (P3):** shared-budget residuals. A torn record has no checksum; the grant side fails open under a permanent write failure (an owner question); there is no separate counter.
  - **ft-ccf03e33 (P3):** a file renamed on a merge's second parent shows 0/0 in the commit's file list but all-added in its diff; the pre-2.31 fallback has never run on a real old git (tied to OD 13).
  - **ft-723aab52 (P3, OD 1):** a stored repository ID so a re-anchor cannot absorb a sibling clone's or a history-merging repository's ledger. It also carries the review residuals: the old-path check runs inside the write transaction (up to 5 s), a hung mount leaves one stuck thread per retry, and the 30 s retry has no test clock.
  - **Folded into ft-626b25fc (flakes):** a third trust-storage test (`a_cache_sync_into_a_linked_worktree_copies_and_scans_nothing`); `ten_thousand_binds_load_and_answer_within_a_pass_budget` (1M in-memory lookups took 40.5 s against 5 s at load average about 300, failing alone too; a wall-clock budget in a unit test); `an_allowed_rename_judges_the_move_and_each_path_it_writes`; `concurrent_binds_all_land_and_each_failed_catch_up_is_counted_once`.
- **Wave 6 and 7:**
  - **ft-f069aa42:** `command_timings` counters drift from the committed macOS baseline. `list_worktrees full` 38 → 42 (CARD 20, justified but never rebaselined) and `commit details` 1 → 2 (a one-time cached `git version` probe from ft-dadcd71d). The bench gate would fail on the integration head until this lands.
  - **ft-cd2a98d7:** run the Windows CI leg for ft-513113a3; that card may not move to done before it is green.
  - **ft-ae9db92a:** `WorktreesPanel` still calls add, remove, lock, unlock and merge-teardown directly, and the mutation scanner cannot see commands repoStore does not own.
- **Folded into ft-45b8099a (git floor), as A-git-03:** `insights/mod.rs:1125` passes `--since-as-filter` (git 2.37) without a capability check. This predates CARD 16 and is on main.

## Fable's decisions (summary)

- **Wave order.** The 2026-10-13 order governs, and still-ready 2026-10-12 cards fill free slots.
  - ft-46bece0d took CARD 10's wave-1 slot, and CARD 10 followed on its typed structs.
  - CARD 14 was pulled into wave 2.
  - After the usage-limit interruptions, three lanes ran concurrently.
- **CARD 22.**
  - A squash whose commit is refused stays parked.
  - The rollback honours a gate refusal. This reverses an earlier overrule, after the Opus security review.
  - S5-11: measure first, then split. `cleanup_error` is a typed field. One helper is shared by the merge and the panel delete.
- **CARD 16.** Keep MIN_GIT_VERSION. Below 2.37, use a count-bounded fallback, not a refusal.
- **CARD 26.**
  - String-only departed detection.
  - The departed cursor is written.
  - Off-cap rows keep their own path and task.
  - An unresolved family skips the cycle.
- **CARD 24.**
  - Retention, ordering, dedupe and binding order stay on `ts_utc`. This deviates from the criterion and is pinned by a test.
  - Catch-up is best-effort, with its own counter.
  - An untrusted time is unresolved.
  - A post-redaction splice of 3 validated keys.
  - History reads are scoped to the family.
- **CARD 10.**
  - Unfollowable pages are refused with "retry with a smaller limit" rather than cut. This keeps the `gitpulse_list_tasks` contract unchanged, so no owner decision is needed.
  - Array-heavy payloads converge.
  - Both env timeouts are clamped to 10 min.
  - Keeping two cut suffixes is accepted, because the genuine-marker check needs the fixed append suffix.
- **ft-f16fad35.**
  - Fix the pre-existing root-delete and symlink-move bugs here, since this lane owns those primitives.
  - Keep the hand-rolled digest as the single owner (no new dependency), documented as an accidental-change detector.
  - The editor-open cost is accepted as a correctness cost.
  - A draft with no base fails closed.
- **tab-switch harness.** Fixed on integration. Process gap: CARD 16 had merged without running the browser harnesses, so every later lane ran them.
- **Wave 4 file ownership.** `WorktreesPanel.svelte` went to CARD 20. ft-1629347b kept its new session argument optional, so existing callers still compile, and left that panel alone. The panel's remaining paths are ft-d61584de.
- **ft-1629347b.** Skip the generation checks, because a tab switch bumps them. Keep Undo available after a failed restore.
- **CARD 20.**
  - Ruling 1: a merge with no target reads the base uncached, so a destructive action never acts on a cached answer. This departs from the card's wording, following the existing rule for destructive actions.
  - Read the ref stamp once per `default_base` call, by extending the existing function rather than copying it.
  - No 60-second sleep test for the TTL boundary; it waits for clock injection (ft-ade6e999).
  - The merge waited for an A/A/B bench, because a plain A/B on this host swung too far to judge.
- **ft-9fa261d9.** Close stdin just after the stop request, not before it. Shrink the test output so the cleanup fits well inside the 500 ms grace. Drop an unneeded `#[allow(unused_mut)]`.
- **Wave 5.** ft-04f5dd26, ft-dadcd71d, ft-acff3877 and ft-43cfe399, chosen for disjoint files. CARD 2 and 13 waited on hooks/mod.rs (ft-42081f16); CARD 17, 19, 21 and ft-c63ec4df share git_reader.rs; ft-487108b9's sidecar.rs is held by ft-329789a9.
  - ft-43cfe399: no WRITER_VERSION bump. Identity is root commits plus remotes; refuse a genuinely different repository.
  - ft-acff3877: leave `ask_then_stop_and_reap` and `audit_lock_retries_share_one_total_deadline` alone.
  - ft-04f5dd26: one 90 s deadline, not a second budget.
  - ft-43cfe399's merge gate (amended): two clones of one upstream share the identity the card defines, so that case does not block on OD 1. A repository with a different root commit would. The adversarial pass found the history-merge case (extra roots), so it went to the owner, who chose to merge and file ft-723aab52.
- **Wave 6.** ft-84a33d12, ft-24f6212d, ft-513113a3 and CARD 17 ft-9cfa4f74, merged in that order.
  - ft-513113a3 stays inside `thread_io.rs` because ft-24f6212d edits `git_cli.rs` in the same wave. Windows is checked by a cross-target compile; runtime is unverified on this host.
  - Only CARD 17 carries bench rows (default-page graph load at 5k; stack refresh with 20 branches beyond the window). On this host the spawn counters are the gate; times are recorded with the load.
  - ft-ccf03e33 does not fold into CARD 17 (same file, different concern). It runs in wave 7, residual 1 only; residual 2 waits on OD 13.
  - Wave 7: ft-2a2008e9, ft-d61584de, CARD 19 if disjoint, ft-ccf03e33 and ft-d41b8491. F11 is dropped: `fix/tasks-refresh-storm` now holds the verification_driver files. ft-626b25fc gets triage, not a wave slot.
  - **Wave 8 (2026-10-16).** 58 open cards were read; 14 were workable and 44 excluded (19 owner decisions, 10 gated on incident cards, 6 waiting on the load governor, and the rest on dependencies, verification_driver, cross-repo work or a person).
    - Picked: ft-3a32f751 (the only `git_reader.rs` card this wave; oldest open audit slot), CARD 18 ft-9cfa4874 (read-only half; the write is OD 3), ft-4ef24637 (portless) and ft-02b836b8 (word diff, frontend only). Merge order: word diff, portless, CARD 18, ft-3a32f751.
    - `git_reader.rs` queue: ft-3a32f751, then CARD 21, then ft-b4eb6688 (it unblocks ft-5c1313c9), then ft-8f398bb6 (needs Windows CI), then ft-c63ec4df.
    - P1 cards with no wave do not jump the audit order. ft-c0d321c1 is the first card of wave 9, with CARD 21 and ft-04c0ab7e. ft-cc4be67b is held: without the installed app quit and a baseline, its A/B cannot be measured.
    - Deferred: ft-ade6e999 (clock injection) until ft-1f30a461 and the watcher part of ft-626b25fc land; it is then the cleanest fix for the FSEvents flake family. ft-487108b9 waits until it is reproduced.
    - Disk was down to 52 GiB at the start of the wave (other sessions), so CARD 18, which adds a release-built bench, starts when the first lane frees its worktree.
- **Final merge.** Fast-forward `main` to the verified integration head rather than re-merging each lane. The per-lane `--no-ff` merges and their conflict resolutions are already inside it, and a re-merge would produce a different, unverified tree.

## Process gaps

- **A randomly seeded property test with a vacuity guard reached integration.** CARD 14 passed its adversarial pass and its Opus review. The test failed only on unlucky draws (4 in 40), so every run those stages made passed. It surfaced in the full `ci:local` on a7390b3b. It was first misfiled as a load flake, then reproduced on its own and fixed at 4707c2c6. **Rule from here:** a property test that asserts its generator hit a case must use a fixed seed, and review asks for a repeated-run count, not one green run.
- **Browser harnesses were skipped by CARD 16** (see the tab-switch fix above). Every later lane ran them.
- **A second integration line exists.** Another session cut `pf/integrate-1015` from this branch at 0f0e0b4b (wave 5, partway) and runs its own lanes on it (ft-79394019 idle pollers, ft-434d7bef doc renames). Its files do not overlap this build's waves. The two lines must be merged before `main` moves past either; whichever lands second merges the other, it does not rebase.
- **Implementers can stop mid-gate.** On 2026-10-15 an API session limit stopped the ft-84a33d12 implementer after clippy and before its tests. It was resumed after the reset. No lane is reviewed until its own report lists every gate.
- **Merged-head checks ran `--lib` only.** From wave 5 to wave 7, the orchestrator's check after each merge ran the library suite and targeted tests, not the integration test targets. Two contract violations sat on the integration head as a result. A `hermetic_contract` one stayed across two merges until the pf/integrate-1015 session reported it. A `supervised_threads` one was found by the first `--tests` sweep (ccb83efa). **Rule from here:** every merged-head check also runs `cargo test --tests`.
- **The merge gate ran the IPC and counts contracts, but not `ipc-type-coverage-contract`.** ft-3a32f751 added an IPC payload type, `CheckoutOutcome`, with no type contract, and that test stayed red from f967968f until the CARD 18 merge (ec182865). **Rule from here:** a merge that adds or changes a `cmd_*` handler or an IPC payload type runs all four contract tests: `check-ipc-contract`, `documented-counts-contract`, `ipc-type-coverage-contract` and `enum-variant-contract`.
- **The in-process lib suite cannot complete at default settings on this host.** The 40 MiB/s write watchdog aborts it. Per-test measurement puts three tests over the 256 MiB per-test budget (two CARD 14 entity-collision property tests at 484 and 372 MiB, and a watcher storm test at 291 MiB). The full result on CARD 19's merge came from `scripts/test-disk-writes.mjs`, which runs every test alone. Filed on ft-626b25fc.
- **Host load.** During wave 5 and 6, the load average reached 300–450 on 18 cores. Wall-clock unit tests fail at that load even when run alone (`ten_thousand_binds`), and bench times are recorded but not gated.

## Owner decisions still pending

1. **Fast-forward `main`** with the command above. Then run Manvi verification, so cards can move to `done`.
2. **Owner Decision 1, reworded.** Either make `ts_utc` the event time, or keep it as the append clock and add a typed optional `occurred_at` (a DevCouncil contract addition). The second is recommended.
3. **Owner Decision 13: git floor.** Not blocking; see A-git-03 in ft-45b8099a.
4. **ft-46bece0d.** Whether to later reject whole floats outright. They are currently accepted and converted up to 10^15−1.
5. **ft-e023906b.**
   - Filing the upstream MarkDev rename defect is outward-facing, so it was not done. Locations: vendored markdev-vault rename.rs:226-234, :207-210, :707-767; upstream core/crates/markdev-vault/src/rename.rs:226.
   - Reference-definition links are reported, not rewritten. The options are to accept that, add a direct pulldown-cmark dependency, or fix it upstream.
6. **CARD 26, Owner Decision 11.** No backfill, as decided. The cut-over is the first ledger row written by the installed build.
7. **ft-43cfe399 (decided 2026-10-15).** You chose to merge with two cases accepted: a ledger copied between two clones of one upstream once the source is gone, and a ledger carried into a repository that merged the source's history. ft-723aab52 (a stored repository ID, OD 1) closes both.
8. **ft-d41b8491 (decided 2026-10-15).** You chose to keep the fail-open: if the shared budget record can never be written, each process falls back to its own bucket (8 processes: 1,536 reactive spawns against 192).
9. **Schema parity (pre-existing).**
   - Jarvis pins DevCouncil 3b58dd7c, whose devmap store is 23, while DevCouncil writes 27. Bumping the pin is a Jarvis change that needs your sign-off.
   - GitPulse's copy of Manvi's serve contract differs from Manvi's. This is the serve-ops failure.

## Known pre-existing failure

`scripts/serve-ops-contract.test.ts` fails on unmodified `main`: `contracts/serve.ops.schema.json` differs from the sibling Manvi checkout. It is unrelated to this build.
