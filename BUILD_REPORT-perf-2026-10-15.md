# Build report: performance backlog, 2026-10-15

Integration branch `pf/integrate-1015` sits on top of `bl/integrate-1013` @ 0f0e0b4b.

Sequencing came from Fable, design and review from Opus, and implementation and adversarial passes from Sonnet lanes. **Nothing is merged.** Every lane is a `pf/*` branch based on `bl/integrate-1013`, which is itself unmerged, so the branches land only after it does. Merging is an owner decision; see the end of this report.

## Tasks moved

| Card | From → to | Branch (tip) | Result |
|---|---|---|---|
| ft-434d7bef (hub rename perf slice) | backlog → in_progress → **review** | `pf/434d7bef-adv` @ ed284f71 | hub rename 150,106 ms → 44,642 ms; `git add` runs 5,000 → 2 |
| ft-79394019 (always-on pollers) | in_progress → **review** | `pf/79394019-bridge-adv` @ e69b721d, `pf/79394019-external-adv` @ b22ac1ff | bridge 6.99 → 2.20 wakeups/s; external_changes (hidden) 2.80 → 2.20/s |
| ft-1de1ff48 (ledger redactor) | → in_progress → **review** | `pf/1de1ff48-redact-adv` @ 126d0e5d | redact::text −26 to −29% per row, byte-identical output |
| ft-626b25fc (full-suite flakes) | stays in_progress (bl session's slice) | `pf/626b25fc-output-emits` @ 185d9ab9 | `terminal_output_emits` made deterministic: 15/15 failing → 70/70 passing under load, 6/6 mutations killed. Section 5 adds load flakes measured on base vs branch. |

None was requested as `done`. Done is evidence-gated on Manvi verification of a merged head, and these heads are not merged.

## Bench deltas

Every number is min-of-N with interleaved A/B runs on the same host. The host was heavily loaded (load average 40-650 from about 10 concurrent sessions), so absolute times are inflated; the ratios are the signal. Rows were added on unmodified code first.

| Bench row | Before | After | Δ |
|---|---|---|---|
| idle_wakeups process floor | 2.40/s | n/a | n/a |
| idle_wakeups: idle alerts bridge | 6.99/s | 2.20/s | −69% |
| idle_wakeups: external_changes, hidden window | 2.80/s | 2.20/s | −21% (pass counter 3 → 0) |
| idle_wakeups: external_changes, visible window | 1 Hz | 1 Hz | unchanged by design (UX latency contract) |
| idle_wakeups: idle host-load sampler | 2.80/s | not changed | kept (see Deferred) |
| hub rename (docs/rename.rs) | 150,106 ms | 44,642 ms | −70% |
| redact::text, 4-field row | 32.17 ms | 23.70 ms | −26% |
| redact::text, 7-field row | 60.17 ms | 42.72 ms | −29% |
| redact::text, transcript-edit row | 38.94 ms | 28.17 ms | −28% |
| transcript_pass, 100k lines | 4.376 s | 3.606 s | −18% |
| transcript_pass, 10k cwds | 441.3 ms | 360.7 ms | −18% |
| transcript_pass, 10k departed cwds | 497.4 ms | 403.9 ms | −19% |

**Regression check (>5%).** No steady-state row regressed. The redactor's one-time first-call cost (+1.2–1.6 ms, found in Wave 3) was removed by the deferred gate in b02159aa: the first call now matches base. One gap: the bench gate for the policy path in ft-434d7bef is a no-op, so the per-file policy-call cost there is unmeasured. It is logged on the card.

## Per-task notes

### ft-434d7bef: hub rename (git_writer.rs, docs/rename.rs, commands/knowledge.rs)
- **Batched staging.** `stage_files_attributed` stages in batches of 2,500. It runs `check-ignore` first, then halves a failing batch, with at most 64 extra runs.
- **Non-ASCII paths.** `--pathspec-from-file` silently skips NFD names on macOS with git 2.54.0, so non-ASCII paths go as argv chunks of at most 128 paths / 12 KiB.
- **Policy checks.** `harness::guard_files` / `judge_in_order` check up to 8 files in flight. Verdicts are recorded and gated in order, and a refusal stops new checks. The `cfg!(test)` switch was replaced by `lend_test_serial`. The sidecar `call_policy` retry now calls `clear_respawn_backoff()` rather than `reset()`, so it no longer kills a child that other in-flight callers still wait on.
- **Verification.** Full suite 5,672 passed / 0 failed / 42 ignored; clippy and fmt clean.
- **Still open on the card:**
  - Criteria 1, 3 and 4.
  - Policy calls are still one per file: a Manvi multi-path op is needed (cross-repo, owner decision).
  - The remaining time is mostly F_FULLFSYNC, about 8.9 ms per note.

### ft-79394019: idle pollers
- **B1, the bridge accept loop.**
  - `Wake` moved to `local_socket.rs` as its one owner, and the copy in instance.rs is deleted. The loop blocks with no timeout. Wake-pipe failure fails closed, with an error shown and no polling fallback.
  - The adversarial pass fixed:
    - a start/stop race that left `listening` set with no loop (266/300 rounds on the old code);
    - POLLNVAL being swallowed, which caused a spin and also affected the mailbox;
    - a retired loop clearing its successor's state.
- **B2, external_changes.**
  - The loop is a Condvar beacon. Its hidden wait is bounded by min(VERIFY_EVERY 30 s, retention `next_check_in`), and shutdown wakes it.
  - The adversarial pass fixed a lost-wakeup race.
- **Verification.**
  - Full suites failed only in load-sensitive tests in files the lanes do not touch; each passes alone. They are folded into ft-626b25fc.
  - Clippy and fmt are clean.
  - Windows was not compile-checked: the libsqlite3-sys cross-compile fails on this host.
- **Already landed, not redone:** the git_cli child-wait poll backoff and the frontend createAdaptiveTimer.
- **Windows check (2026-10-16), run as a scratch crate with `-D warnings`:**
  - B1 adds no Windows or Linux errors.
  - It found a pre-existing break on main. The test `a_peer_dripping_bytes_cannot_hold_the_connection_past_its_ceiling` is not unix-gated, so the Windows `--all-targets` build fails. CI's windows-latest clippy leg likely fails today; that is inferred, not observed.
  - Fixed on `pf/79394019-bridge-adv` @ e69b721d with a one-line `#[cfg(unix)]`. The build failed before the fix and passes after, and the test still runs on macOS.

### ft-1de1ff48: ledger redactor
- **The change, in redact.rs only.** ledger/mod.rs and the vendored dc-redact are untouched.
  - Each string is parsed once, not twice.
  - One `CONTEXTUAL_PATTERNS` table builds both the 15-regex chain and a RegexSet gate. When no pattern matches, the chain is skipped.
  - Copy-on-write through every pass, so a pass that changes nothing allocates nothing.
  - The per-segment dc-redact call is skipped when `contains_secret(seg)` is false. Opus checked that this is the identity against the vendored source (lib.rs:1345-1434).
- **Security impact.** Output is byte-identical to 0f0e0b4b. No pattern was removed or weakened, and the pub surface is unchanged.
- **Evidence.**
  - A verbatim reference oracle, confirmed by mechanical diff.
  - A corpus and a trigger-pair sweep.
  - A 220k-case generator in release builds; 10k under `debug_assertions`, because the debug run took 182.8 s.
  - A gate-vs-chain property covering Unicode folding, CRLF, U+2028 and inputs up to 500k characters.
  - Cases for collision ordinals, 20k equal-length keys and nesting depth 28-36.
  - 8 mutations, all killed. One, a gate that skips long inputs, survived the lane's tests and was closed by the adversarial pass.
- **Verification.**
  - Full debug suite: 5,662 passed / 3 failed / 42 ignored. The 3 failures are watcher tests that fail at a similar rate on base in interleaved runs; they are folded into ft-626b25fc.
  - clippy and fmt are clean.
- **Not done.**
  - `cargo llvm-cov` and the one-time RegexSet cost were measured in Wave 3 (below).
  - CI exercises 10k cases, not 220k.
  - Further gains need a cheaper pre-filter in dc-redact, which is DevCouncil's to change.

### Wave 3 (Fable, 2026-10-16)
- **Lane 1: `terminal_output_emits`.** The fix is in the test file only. All three root causes were in the test:
  - a fixed 300 ms sleep before the repay;
  - two exit listeners racing;
  - a message-count ceiling that depended on scheduling.

  The fix uses a sentinel file, acks under the drain lock, and a single exit listener.
  - **Accepted trade:** the 24-per-MiB ceiling applies only on a quiet host (`coverage_relaxed`). Under load the test instead asserts the largest message ≥ 32 KiB, which still kills the per-read defect.
  - **Residual:** the quiet threshold is 1.0 per CPU and uses the lagging 1-minute load.
- **Lane 2: hub rename branch.**
  - **Merge:** `bl/integrate-1013` merged in at 9610a636. The only conflict was in `docs/PERFORMANCE.md`, and both sections were kept. merge-tree is now clean.
  - **Counting bench (95298469, previously 34564f19):** the policy gate now counts and times its calls. A hub rename makes 5,002 policy calls.
    - At 0 ms per call, the gate costs nothing.
    - At a simulated 2 ms per call, it takes 18.6 s (18.4% of the rename) with 1 worker and 2.4 s (2.9%) with 8 workers: 7.76x faster.
  - **Trailer:** the commit's co-author line read Sonnet, not Opus. The owner approved an amend. It was rebuilt as 95298469 with an identical tree.
- **Lane 3: Windows check of B1.** Clean. It found the pre-existing unix-only test on main; fixed at e69b721d (see above).
- **Found on bl's head:** `hermetic_contract` was red on `bl/integrate-1013` @ 9103bda4. I reported it to the bl session, which fixed it at ccb83efa.
- **Redactor verification:**
  - **Coverage run (CI profile):** 5,650 passed / 15 failed. None of the failures is redactor-reachable (load flakes in storage hygiene, terminal and watcher tests), and `hermetic_contract` passed.
  - **First call per process:** it gets slower, from about 3.0 ms to 4.1–4.7 ms (+1.2–1.6 ms), measured at load 70–90. That is the one-time RegexSet build. Steady-state calls stay 26–29% faster.
  - **Short-lived binaries (correction, 2026-10-16):** a source reading had concluded that `gitpulse-hook` redacts only on warning/error paths. A runtime trace with a probe in release binaries **disproved** this. Every hook invocation redacts on its normal path, because `logging::init`'s durable sink writes a "session start" marker through `safe_log_line` → `redact::text` (1 call on most paths, 6 on command-gate).
    - The measured whole-process cost is 0–0.8 ms (min) and 0.2–2 ms (median), within noise on processes that take 7–40 ms.
    - Following Fable's condition, a lane is making the RegexSet build lazy before the redactor is handed over.
- **Board incident:** a `gitpulse_add_task` overwrite reset ft-626b25fc from in_progress to inbox. The bl session had moved it between my read and my write. I restored it with `complete_task` and logged a correction; any description edits made at revisions 7-8 may be lost.

## Deferred, and why

| Item | Reason |
|---|---|
| C: ft-8db24db8, macOS QoS | Depends on ft-5d8b201f (in review) and on main going green (ft-ae39d654). git_cli.rs is serialized by the bl integrator. |
| E: governor cards ft-d902fcb4, ft-369f85a3, ft-40271e71, ft-93537f05 | Fable sequenced them after the integrator's git_cli.rs waves. |
| ft-cc4be67b, translucency | Needs attended GPU measurement; it cannot be benchmarked headless. |
| ft-79394019 sampler slice (2 s process scan) | Gated on ft-ae39d654 and ft-5d8b201f closing. Its cost, about 0.4 wakeups/s above the floor, is named on the card. |
| Cards held by the live bl session | Not touched: Scion CARD 17/19/21 (ft-9cfa4f74, ft-9cfa4974, ft-9d044174), F1 ft-bdb253cf, ft-02b836b8, and the git_cli.rs waves. |

## New tasks filed, and folds
- **No new cards.** `pf-2026-10-15-hub-rename-residual` was refused with related_tasks_exist. Its residuals went into ft-434d7bef's logs.
- **ft-626b25fc** (full-suite flakes) gained section 5, with counts on base 2925f6c9 vs branch:
  - `terminal_output_emits` failed 13/15 on base and 12/15 on the branch.
  - `terminal_ipc_latency` failed 5/15 on base and 2/15 on the branch.
  - Four watcher tests measured on base 0f0e0b4b vs branch. `measure_target_directory_event_rate` failed 5/5 on base, with FSEvents delivering 0 events in 4 of those runs.
  - Also folded in: the `stored_incidents_are_bounded` timeout, and the bridge start/stop race as a likely cause of its section-2 cascade.

## Fable's decisions
1. **Wave 0:** the idle_wakeups rows for the bridge and the sampler, on unmodified code. external_changes needed a seam, which B2 added.
2. **Wave 1:**
   - B1, the bridge self-pipe;
   - B2, the external_changes cadence;
   - D, only the perf slice of ft-434d7bef.
3. **Wave 2:** A, the redactor, in redact.rs only. ledger/mod.rs was held by another branch.
4. **Landed slices:** the git_cli backoff and the frontend timers are marked landed, not redone.
5. **Deferred:** C, E and the translucency card, with the reasons above. The sampler slice is kept until its gating cards close.
6. **Base:** stay on `bl/integrate-1013`. pf work lands only after bl lands.

## Handover to the bl integrator (Fable, 2026-10-16)
Fable reversed the earlier plan to wait for bl to land. The pf branches are handed to the bl/integrate-1013 integrator one at a time, each at a frozen SHA, to merge into bl. These rules apply to every handover:
- **Merge commits only**, never squash.
- **PERFORMANCE.md conflicts** are resolved by keeping both sections.
- **A red gate sends the branch back to pf**, rather than being fixed forward inside bl.
- **Cards stay in review** until main is verified.

All handed branches were merge-tree clean against bl@5fa26843 at handover.

| Tier | Branch @ tip | State |
|---|---|---|
| 1 | `pf/626b25fc-output-emits` @ 185d9ab9 | **merged into bl at 24897789**, green |
| 1 | `pf/626b25fc-signal-guard-fp` @ 239a8fae | **merged into bl at c3ef66a3**, green |
| 2 | `pf/79394019-bridge-adv` @ e69b721d | **merged into bl at ed557abc**. One new flake, `an_idle_accept_loop_does_not_wake`, failed 1 of 9 at threads=6.
- **Cause:** the global `accept_wakes` counter counted a predecessor loop's retirement wake.
- **Fix:** `pf/79394019-bridge-idle-flake` @ 906daa05 (test-only, cfg(test)) gives each generation its own counter.
- **Evidence:** a deterministic reproduction that fails on the old counter, and a mutation check where a 200 ms poll fails the test.
- **Original flake not reproduced:** 0/30 before and after the fix.
- **Merged into bl at b91c79fc**, gates green. |
| 2 | `pf/79394019-external-adv` @ b22ac1ff | **merged into bl at 0cad5891**, green |
| 2 | `pf/7d5029c8-cancel-timer-adv` @ a5908fd8 | **sent back** on a red `supervised_threads` gate. It is a scanner false positive: `raw_starts` does not recognize `pub(crate) mod tests`, so a test-only spawn counted as production. The scanner now recognizes test modules of any visibility (c8c8019e), with a test that fails on the old matcher. bl's head was merged in (a15d754e), and the branch was **re-handed at a15d754e**: merge-tree clean against bl@86dd1e6f. **Merged into bl onto 86dd1e6f**, gates green; the integrator approved the scanner change. |
| 3 | `pf/1de1ff48-redact-adv` @ b02159aa | **merged into bl** after CARD 18 (ec182865), gates green (442/442 ledger/secrets/logging/alerts). Handed over 2026-10-16; merge-tree clean against bl@e8d209ee. b02159aa defers the gate: the first 256 contextual calls run ungated (base behaviour, byte-identical), and the set is built from call 257. 256 is the measured break-even, ≈1.45 ms ÷ ≈5.6 µs. First `redact::text` call: base 2.66/2.71 ms, eager build 3.84/3.97 ms, now 2.65/2.72 ms (min/median), so the one-time regression is gone. A hook makes 0–2 calls, so it never builds the set. No separate adversarial pass was run on this structural change; the oracle and generator suites cover both regimes. |
| 4 | `pf/434d7bef-adv` @ 95298469 | **merged into bl** after bl's ft-3a32f751 lane landed (both touch git_writer.rs); 303 passed, 0 failed. Handed over 2026-10-16. The owner approved amending the trailer; the commit was rebuilt with `commit-tree` from 34564f19 with an identical tree. |
| 2 | `pf/7d5029c8-tool-capability-adv` @ e5cefe93 | **merged into bl onto bcf1b25a**, gates green: 194/194, supervised_threads 9/9, hermetic_contract 10/10. Handed over 2026-10-16. Changes: Present answers re-checked by len/mtime on every hit; Absent answers expire after a 60 s TTL; one probe per stale entry. The adversarial pass fixed a break where an invalidation landing mid-probe was overwritten by that probe's stale answer; fixed with a per-tool epoch (dca21c4c). 5/5 mutations killed. Hit cost went from ~12 ns to ~1.3–2.2 µs (one `stat`), accepted because every caller is about to spawn a process. |

### Wave 4 lanes
- **ft-7d5029c8 C-mcp-07: shared deadline timer** (`codeintel/mod.rs`).
  - A shared timer replaces the 30 s sleeper thread per query. Thread count after 1000 queries went from 1004 to 4.
  - The adversarial pass fixed two cases where a query was left without a deadline: the supervisor giving up, and token reuse.
  - Mutations: 5 of 5 killed.
- **ft-626b25fc §4: `signal_guard_live`** (test file only).
  - The test now compares strictly: a process is excused only if it changed between the snapshots, plus a one-shot re-ask of the disputed processes.
  - The adversarial pass broke the first version with argv-rewrite and exec false failures, which the reworked version fixed.
  - Result: base failed 30 of 30 runs; the reworked test passed 40 of 40 under hogs, churn and an argv flipper. 5 of 5 mutations killed.
  - Not reproduced: the exec race and the `-u` miss.
  - No second adversarial pass was run; the rework re-ran the adversarial generators and mutations instead.

**State at end of run (2026-10-16):** every pf branch is merged into `bl/integrate-1013` and none is on main.
- **Branches merged:** tiers 1–4, the tool-capability branch and the bridge idle-flake fix.
- **Cards:** none moved to done. Done is evidence-gated on main.
- **Not run by pf:** the integrator flagged that its own lane gate had skipped `ipc-type-coverage-contract.test.ts`; that gap is now fixed. No pf branch changes an IPC command signature or struct, so pf did not run that test. This was verified by diff: every pf production diff adds zero `#[tauri::command]` and zero `Serialize`/`Deserialize` derives. Changed signatures on existing commands were not checked.

### Wave 5 (Fable, 2026-10-16)
- **Base:** bl@b91c79fc. All 9 pf tips are ancestors of it.
- **One lane:** measure the real Manvi sidecar round-trip behind the hub-rename policy gate (ft-434d7bef), as an ignored arm next to the simulated ones. This decides owner decision #2:
  - real call ≈10–20 ms → the multi-path op is justified;
  - real call ≤2 ms → not worth building.
- **Then stop.** Fable's reason: the remaining pf-doable perf work is exhausted, and the rest is owner-gated, held by bl, or gated on cards that cannot close until bl reaches main.
- **Corrections logged:**
  - The "saved-config path returned without checking" residual on ft-7d5029c8 was wrong. `tool_config::saved_binary` already requires `is_file()`.
  - The codeintel degraded-note residual is narrower than first stated. It is cleared when the timer is replaced, not on a later successful registration.
- **Handed to the bl session:** the EPIPE race hypothesis for ft-626b25fc §1.
- **Result:** branch `pf/434d7bef-sidecar-rtt-real` @ d1f7c9b3, on bl@b91c79fc. merge-tree is clean. The change is test-only: `#[ignore]` live arms plus `cfg(test)` hooks.
  - **Per-call cost:** the real Manvi round-trip is p50 0.15 ms at 1 worker and about 1 ms at 8. The sidecar is nearly serial, so the 8-worker fan-out saves about 0.6 s per hub rename, not the 16 s the simulation implied.
  - **Gate share:** 3.5–4.6% of the rename. That holds unbound, with a bound glob scope, and with a 20k-event ledger.
  - **Adversarial re-measure:** every verdict was really evaluated, and there were 0 respawns across 25 renames. This pass ran in the lane's own worktree because the disk was too full for a separate target, so it is less independent than the other adversarial passes.
  - **Outlier:** a scope that lists all 5,000 planned files re-sends 125 KB per call, which puts the gate at 11.2%.
  - **Recommendation for owner decision #2:** close the Manvi multi-path op as not worth building. It would save at most about 2%.
- **Handover blocked:** the bl integrator session (pid 2558) had exited by the time this branch was ready. The branch is frozen at d1f7c9b3 and waits for the next bl integrator or the owner. pf did not merge into bl itself.

## Pending owner decisions
1. **Merging to main** (superseded in part by the handover above: pf branches now reach main inside bl). Original plan: merge the `pf/*` branches after `bl/integrate-1013` lands on main: `pf/434d7bef-adv`, `pf/79394019-bridge-adv`, `pf/79394019-external-adv` and `pf/1de1ff48-redact-adv`. Merging to the default branch needs the owner.
   - **Merge-tree check against `bl/integrate-1013` @ 9103bda4 (2026-10-16), read-only:**
     - `bridge-adv`, `external-adv` and `redact-adv` merge cleanly.
     - `434d7bef-adv` conflicted only in `docs/PERFORMANCE.md`. It is now merged with bl (9610a636), and merge-tree is clean.
     - `pf/626b25fc-output-emits` is new. It is based on bl 9103bda4 and touches only the test file.
   - **Suggested order:** redact-adv, external-adv, bridge-adv, then 434d7bef-adv.
2. **Manvi multi-path policy op.** Measured in wave 5: the real gate is 3.5–4.6% of a hub rename, so the recommendation is to close this as not worth building and re-scope ft-434d7bef criteria 1/3/4. Cutting ft-434d7bef's per-file policy calls to one needs a new Manvi op. That is a cross-repo contract change.
3. **Shared-name supervised threads.** The one_of_many vs spawn_once choice per thread (ft-626b25fc section 3) changes what Diagnostics reports.

## Unverified, or not run
- Windows compile of B1 beyond the scratch-crate check: `instance.rs` is macOS-only, and clippy-only lints were not cross-checked.
- The DevMap index at `pf-integrate` answers `walk_incomplete`, and `devmap_affected_tests` refuses as stale. Blast radius was checked by source reading, not confirmed by the graph.
- The policy-call bench for ft-434d7bef now measures the real sidecar round-trip as well as the simulated latency (wave 5). It has not been measured on a real user profile or with ambient `MANVI_*` policy flags.

## Session close (2026-10-16)
- **Branches.** Every pf branch except one is merged into `bl/integrate-1013` and has since been deleted locally; its commits live in bl. The exception is `pf/434d7bef-sidecar-rtt-real` @ d1f7c9b3, which is test-only and was never handed over because the bl integrator session had exited. It merges cleanly into b91c79fc. `pf/434d7bef-sidecar-rtt` and `-adv` both point at its ancestor 0e357b70 and can be deleted.
- **Worktrees.** All pf lane worktrees are removed. Scratch evidence (briefs, diffs, logs, measurement scripts) stays in the gitignored `tmp/pf-1015/`.
- **Board.** Cards are in review, with merge commits and corrections logged: ft-434d7bef, ft-79394019, ft-1de1ff48, ft-7d5029c8. ft-626b25fc is the bl session's card; pf appended log lines only.
- **Process lapses, recorded:**
  - A `gitpulse_add_task` overwrite reset ft-626b25fc's status. It was restored.
  - A retracted "saved-config path" residual had been taken from the audit text without verifying it.
  - The second signal_guard pass and the sidecar re-measure ran without a fully independent adversarial worktree. For signal_guard, the rework reused the adversarial generators; for the sidecar, the disk was too full for a separate target.
