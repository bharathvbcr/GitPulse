# GitPulse backlog build: 2026-10-20 / 21 (bugs and performance)

**State.**
- `main` is unchanged at `20843543`.
- All approved work is on **`b20/integrate` @ `a51bd015`**: 46 files, about +8.4k / −0.4k lines against `main`, including tests.
- Merging to `main` waits for the owner. Nothing was pushed, and no tag was created or moved.
- The coordinator's running log is `tmp/build-1020/LOG.md` (gitignored).

**Process for each card.**
1. An Opus brief.
2. A Sonnet implementer in an isolated worktree.
3. A separate Sonnet adversarial pass. Every break went back to the implementer.
4. An Opus review against the brief, the acceptance criteria and the callers (DevMap impact plus `rg -uu`).
5. A full-suite integration gate on the merged tree.

Fable made the sequencing calls and three judgment calls (below). At most 4 Sonnet agents ran at once.

**Labels.** **verified** means a command was run here and its output read. **inferred** means it was read but not run. **unverified** means it was not checked.

---

## 1. Tasks moved to review (integrated on `b20/integrate`, not on `main`)

Done is evidence-gated by Manvi, and no Manvi `work.verify.task` record exists for these heads. `dev verify <card>` returns `task … not found`, because these are GitPulse board cards, not DevCouncil tasks. That gate therefore **could not run**, and it is not reported as passed. Every card is in **review**.

| Card | What landed | Adversarial breaks found and fixed | Still open |
|---|---|---|---|
| **ft-45b8099a** git capability preflight (bug, P1 high) | **Owner decision: the floor is raised.** One `GIT_FEATURES` table; `MIN_GIT_VERSION = hard_floor_of(table)` = **2.36**. Outdated messages name the missing features, and an overflowing version is Broken, not invented. Force push now sends `--force-with-lease --force-if-includes`: a real-git two-clone test shows auto-fetch, then amend, then force push leaves the remote's commit intact, while the bare lease destroys it. The refusal separates too old, deferred and could-not-check, and `cmd_push` marks the user action before the probe. Insights `--since-as-filter` has a guarded fallback that is count-bounded and equivalent. The loose 2.27 commit-graph constant is now a table row. | 6 + 1 review + 2 gate. **HIGH (pre-existing):** `+main` in the push branch slot was a force push with **no lease**, so `push_planned` overwrote `origin/main`; it is now refused. Others: the "could not check" message lied; overflow parsing; `hard_floor` was untestable; the Outdated message claimed a 2.34 git was older than 2.30; an inline test module silently disabled the gate-fidelity contracts; a doc comment tripped the commit-graph argv scan. | A version-gated wrapper test is not possible (no seam to swap the git binary). Converge the insights fallback with git_reader's private `DateWalk`. |
| **ft-487108b9** sidecar EOF reap (bug, P3) | **The defect is real, with a different cause than the card guessed.** In `terminate()` nobody drains the stdout queue. A child answering stdin EOF with more than 8 lines made the pump call `force_kill`, which blocked on the child lock held through the grace. The pump stopped reading and the honest child was SIGKILLed (test fails on main with signal 9). Fix: a `shutting_down` flag, and the overflow kill and `await_answer`'s liveness check never block on the child lock (`try_lock`). Hostile floods are still killed and bounded. | 2: the race the lane had accepted, and a **pre-existing** `await_answer` deadline overrun (a 300 ms call returned after 1.8 s while terminate held the lock). | none |
| **ft-da85cd55** spawn-budget drain, budget slice (bug, P1 high) | Refusals name `(shared\|local bucket: N token(s), needs more than F, burst B)` after the unchanged `deferred under load after Xs:` text. A clean denial no longer rewrites the shared record, and waiters nap until the next token is due. **No-mint proof:** a model check of 200 seeded runs × 3000 ops against an always-write oracle. **Premise corrected:** one edit-hook invocation (no app, 2 linked worktrees) spends **4** git spawns, not ~15. | 1: a 1 ms spin on a stale shared ask-time. 1 gate: the `governor_sites` contract pinned the old reader name. | **AC2** (cut hook spawns) and **AC5** (live 4-agent evidence) wait on ft-42081f16, which holds `hooks/mod.rs` and `gitpulse-hook.rs`. The drain must come from hook paths not yet measured (bound-task scope lookup, session scan, app-attached). |
| **ft-c63ec4df** one ahead-behind listing (perf/refactor, P3) | `RefQuery` + `list_against` + a strict `AheadBehindListing::rows()` parser serve `list_branches`, `list_tags`, `tip_counts_against` (its own narrow query, the same argv as main) and `batch_divergence`. A malformed row is an error, never a dropped ref. Characterization tests were committed first, on unmodified code. | 1: **cold `tip_counts_against` was 2.7× slower** (51 → 138 ms at 2000 refs) because it used the wide branch format; fixed. Freshness after every kind of ref move, odd names, degradation and concurrency all HELD. | none |
| **ft-9d044974** ledger honesty over MCP (bug, P2 high) | **Owner decision: interim (a), no schema change.** Every MCP ledger surface serves `LedgerView`: `recording:false` on a read-only open, plus `recording_state`, `dropped_scope`, `as_of`, `last_catch_up(+basis)`, `stale(+reason)`, a `catch_up` summary and the legacy caveat; `change_context` gets `ledger_freshness`. All fields are additive and schema-declared. The walk is breadth-first and reads depth 5. A dev/inode loop guard is in place, hard links are deduplicated, and symlinks out of the corpus are refused. Unlisted files are counted by depth and by cap. `forget` runs only on a truly complete walk. | 11, including a **MUST-FIX (pre-existing)** `forget` breach: a 0444 directory was walked as empty and its transcripts' offsets were forgotten. Also: a deadline-cut pass refreshed freshness; unbounded `catch_up_failures`; walks escaped the corpus through symlinks to `/` and to ancestors (privacy); sticky rows from removed worktrees; one skewed row vetoed freshness. 1 review: L5 had edited `hooks/mod.rs` (a held file), reverted per Fable. 1 gate: TS type drift. | `LedgerStatus` itself is unchanged; that is ft-a2936457. A failed attempt after an old completion shows as a dormant row (no `attempted_at` column). |
| **ft-e2c10b99** helper-thread deferrals (bug-ish refactor, P2) | A deferral raised on `cmd_get_commit_graph`'s scoped helper is now handed back at the join through `ipc_error::with_records`, a wrapper over the existing `snapshot`/`absorb` pair that the single-flight now also uses. So an allowlisted read deferred under load is retried again; the test fails on base with Unclassified. All 10 production `thread::scope` sites have verdicts. `REPLAY_SAFE_READS` is unchanged. | 1: `with_records` returned the whole thread record, so a leader leaked earlier records to its waiters. | **AC3** (delete the thread-local) is split to ft-b595921a, a ~100-file migration that needs an owner decision. |

## 2. Integration gate (verified, on the merged tree)

**Gate 2, Rust and full vitest, on `8bf9be05`** (L1 to L6 merged):
- `cargo fmt --check`: OK.
- `cargo clippy --all-targets -D warnings`: OK.
- `cargo test --tests --no-fail-fast`: 123 test binaries OK; **1 failure**, `terminal_output_emits::a_hidden_sessions_output_is_batched_at_least_ten_times_harder_and_still_whole`.
- That test **also fails 3/3 on `main` @ 20843543** (hidden 32–34 vs visible 150–152 messages, ~4.5× not 10×), and the run touched no terminal code. It is pre-existing; the evidence is logged on ft-b93b397e (the card that owns it). Don't move that card to done.

**Final tree `a51bd015`** (gate 2 plus a TS-only fix; no `.rs` change):
- `bun run check`: 0 errors.
- `check:types`: OK.
- `check:ipc`: OK.
- Full vitest: **772/772 files, 10834 passed**, 1 expected fail, 1 skipped.

Gate 1, on the earlier tree, caught 4 regressions that lane-scoped runs missed:
- an inline `mod tests` in `commands/remote.rs` cut the gate-fidelity contract parser, silently disabling 6 contract checks;
- the commit-graph argv scan;
- the `governor_sites` reader pin;
- (gate 2) TS type drift.

All are fixed.

**Not run:** `cargo test --lib` as a separate full run. `--tests` covers the lib tests; each lane also ran its module filters. Also not run: the browser/WebKit harnesses, and a release build.

## 3. Benchmark deltas

All benches were interleaved A/B on a loaded host (load 6–17). The ranges overlap, so **no speedup is claimed**; the result is no regression.

| Path | Before | After | n |
|---|---|---|---|
| `list_branches` cold, 220 branches | best 39.2 ms | best 32.8 ms | 8×15, A/B/B/A |
| `list_tags` cold, 60 tags | 19.2 ms | 15.5 ms | same |
| worktree divergence cold, 8 worktrees | 151 ms | 134 ms | same |
| `tip_counts_against` cold, 2000 refs | 82.4 ms | 73.6 ms | 8×15 (after fixing the 2.7× regression the adversary found) |
| Shared spawn record writes, 4 denied waiters, gate level, 4/s | 17 writes/s | **4.0 writes/s** | 2 s, n=1 per arm |
| Shared spawn record writes, record level, 8/s, injected clock | 80 writes/s | **8.4 writes/s** (bound 8.4) | 10 s simulated, deterministic |

Git spawn counts per call are unchanged for the listings; branch stats no longer re-lists after a warm branch list. The ft-487108b9, ft-45b8099a, ft-9d044974 and ft-e2c10b99 paths are not hot paths and were not benchmarked (unverified).

## 4. Deferred or blocked (and why)

| Card | Why |
|---|---|
| ft-9cfd5574 pre-edit collision blind spot (P1 high) | Plans `hooks/mod.rs`, which in_progress ft-42081f16 holds; the owner confirmed that lane is live |
| ft-9cfd5474 hook-report protocol honesty | Same hold |
| ft-da85cd55 AC2 and AC5 | Same hold |
| ft-9cfd5374 attention standing lifecycle | Plans `terminal/mod.rs`, held by in_progress ft-0ac24164 |
| ft-8db24db8 macOS QoS tiering (P1 high perf) | Fable: wait until sensor ft-5d8b201f leaves review and host load is sane; its 10-minute soak and responsiveness ACs can't be met honestly at load 15–40 |
| ft-369f85a3 shed under pressure | Same sensor dependency. Re-scope first: a `Refusal::Shed` arm already exists in `git_cli.rs` |
| Every in_progress card | Not touched: ft-42081f16, ft-f8a297ff, ft-329789a9, ft-7e812b3f, ft-0ac24164, ft-626b25fc, ft-2967fd9d |

## 5. New tasks filed or updated

- **ft-a2936457** (backlog, P2): fold the MCP ledger view into `LedgerStatus`, and brief a read-only ledger as "readable" in `session_brief`. Blocked on ft-42081f16; the reverted patch is in the lane's `tmp/l5-hooks-followup.patch`.
- **ft-b595921a** (backlog, P3, owner decision): retire the `ipc_error` thread-local with a typed CommandError, staged git_cli → git_reader → commands (~100 files).
- **ft-b93b397e** (review): evidence appended that its own regression test fails 3/3 on `main`.

Found but not filed, as small residuals or in cards other lanes hold:
- `a_carried_answer_is_not_spent_against_the_time_budget` (`git_reader.rs:~12432`): a load flake that fails identically on `main`. It belongs to the in_progress flake card ft-626b25fc.
- `for-each-ref` can pair an old oid with new ahead-behind counts under a concurrent ref writer (a git race; 8/20 raw runs).
- `benches/commit_graph_history.rs:249` hardcodes the old narrow argv.
- The sidecar test fixture reads stderr with `BufReader::lines` while production uses `read_bounded_line`.

## 6. Fable's decisions

1. **Wave 1:** ft-45b8099a (narrowed off `worktree.rs`/`git_reader.rs`), ft-487108b9, the ft-da85cd55 budget slice, ft-c63ec4df. QoS was moved to wave 2. `--force-if-includes`, and refusing force push below 2.30, are within autonomy.
2. **Wave 2 order:** ft-9d044974 (interim a), then ft-8db24db8 (gated on the sensor and on load), then ft-e2c10b99, then ft-369f85a3 (after a re-scope).
3. **L5's `hooks/mod.rs` edit refused**, since the held-file rule has no "small" exemption. The honesty moves to an MCP-only view instead, with follow-up ft-a2936457. Writing the pre-existing `catch_up_state` table is data, not a schema change, so it is allowed.
4. **ft-e2c10b99:** take the interim handback, reusing `snapshot`/`absorb` (no third copy). Sweep every `thread::scope` site. Split AC3 to an owner-decision card at P3.

## 7. Owner decisions still pending

1. **Merge `b20/integrate` (`a51bd015`) into `main`.** It is a clean merge-tree onto `main` @ 20843543, and the gate is above. Two behaviour changes to accept:
   - git below 2.36 is now **Outdated** (Ubuntu 22.04 and Debian 11). This was decided.
   - The **existing MCP `recording` field now reads `false`** on a read-only open, so its value changes while its type stays the same. Agents that keyed on it see "not recording" plus `recording_state: "readable"`.
2. **After the merge, rebuild and reinstall** `gitpulse-mcp` and `gitpulse-hook` (`bun run mcp:install`), then `bun run mcp:doctor`. The spawn-budget fix only reaches live agents once it is reinstalled: during this whole run, every `gitpulse_insights` git facet was `deferred under load` (verified at start and end).
3. **ft-b595921a:** approve or drop the ~100-file typed-error migration.
4. **Disk, URGENT.** Free space fell from 297 GiB to **42 GiB**, across 16 worktrees from this run: 12 agent lanes and adversarial passes, plus 4 gate and cache worktrees. A full disk breaks the running app (SQLite WAL). My notes say not to remove worktrees from a gated session. Suggested cleanup after the merge, run by you:
   - `git worktree remove` for each `.claude/worktrees/agent-*` from this run, plus `tmp/warm`, `tmp/gate`, `tmp/gate2` and `tmp/gate3`;
   - then `rm -rf` any leftover directories.
5. **Process note.** Agent `isolation: worktree` lanes branch from `origin/main`, which is 76 commits behind the unpushed local `main`. Every lane merged local `main` before its results counted. Pushing `main` would remove this hazard.

---

# Wave 3 (2026-10-21): after the owner said "merge all, clean branches, next wave"

**State.**
- Waves 1–2 are merged: `main` fast-forwarded to `ec818492`.
- 20 worktrees and 22 merged branches were removed. Free disk went from 42 to 183 GiB.
- `gitpulse-mcp` and `gitpulse-hook` were reinstalled; `mcp:doctor` is OK.
- Wave 3 is on **`b21/integrate`**: 36 files, about +7.3k / −0.3k lines against `ec818492`.
- Merging it to `main` waits for the owner.

**Owner decisions taken during wave 3:**
- Bindings: the conservative fix, later replaced by **B+** after the adversary showed the first design widened access.
- Accept the sensor card. Manvi held it in review because no verification record exists.
- Codex ingest: **yes**.
- File "unbound is unfenced" as its own card.

## Cards moved to review

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-b93b397e**: red test `terminal_output_emits` hidden batching | **Not a code bug.** This host's 10 ms sleeps take about 66 ms, so the producer stretched to 10 s; the same code passed on 10-17. The test now paces by clock spin, with the 10× assertion unchanged. 10/10 pass. | Opus-reviewed (test-only change). The assertion stays meaningful. |
| **ft-8db24db8**: macOS QoS tiering | Background-class children and `devmap build` are demoted from the parent with `setpriority(PRIO_DARWIN_PROCESS, pid, PRIO_DARWIN_BG)`, which keeps the posix_spawn fast path; `pre_exec` measured +35–95%. Rayon gets `RAYON_NUM_THREADS = ncpu/2`. Background scopes run at UTILITY QoS and are restored afterwards; user-facing scopes nested inside one are lifted back. The sampler's App Nap Activity is held only while `host_load.enabled`. | 1 break fixed: a nested user-facing scope stayed at UTILITY. MCP read latency showed no difference beyond noise (max 0.33 s). **Live ACs unmeasured** (load 12–118): soak, responsiveness harness, pmset. |
| **ft-369f85a3**: governor 1c shedding | Closed by evidence: commit 73163a50 already implements every AC on main. No code change. | n/a |
| **ft-ae3490e7**: binding authority | **B+**: insertion-order precedence everywhere, including ingest attribution history. A sibling's legacy status is decided once, at the anchor's initial pass; this is crash-safe, and an upgrade from an old build counts present siblings as legacy. Later siblings and appended rows are history-only. The demotion count surfaces on the MCP view and status. The birth-time check is gone. | Round 1: the approved design **widened access**, because unbound means unfenced; 0/5 legacy bindings resolved, and birth time was forgeable. Owner chose B+. Round 2: crash, no-epoch upgrade and fresh-install windows, plus notice reach; all fixed. The gate then found a **descriptor leak** (fresh SQLite connections per call) and bisected it here; fixed. |
| **ft-9d044a74**: Codex transcript ingest | A `TranscriptSource` trait: Claude is unchanged (digest pinned), and Codex covers the structured shapes from the codex-rs source. **Code-mode JS is counted, not parsed** (Fable B): the hand lexer both lost and fabricated calls. So **this machine records 0 Codex rows today**, because all local rollouts are code-mode. | 3 rounds. Fixed: forget on an unreadable root, `..` attribution (a byte-prefix bug shared with Claude), phantom commands, a 153 s hostile line, resume cwd loss, root-switch replay, ownership. Round 3: the forget invariant HELD in 11/11 attacks. |
| **Red reflog tests** (no card; filed under the gate) | **Not a code bug.** git 2.54's auto-maintenance expires reflog entries older than 30 days. The fixtures' fixed epoch 2026-09-21 crossed 30 days on 10-21. The hermetic test git config now disables it; production is unaffected. | Root cause shown with a git-only repro (51 vs 251 entries). |

## Gate (verified)

**Gate 5 on `b21/integrate` @ `45ec47b6`, all green:**
- fmt, clippy `-D warnings`;
- `cargo test --tests --no-fail-fast`: **125/125 binaries OK**;
- `bun run check`, `check:ipc`;
- full vitest: **772/772 files, 10834 passed**.

The run was at host load 94–118.

Gate 4 had caught the descriptor leak above. Its other failures were proven load flakes by interleaved runs against main:
- `ten_thousand_binds`: min as_of 0.64 s on the lane vs 0.99 s on main;
- the two vitest script tests pass 37/37 alone on both trees.

## Benchmarks

- **QoS:** +~0.5 ms per *background* spawn; non-background spawns add no syscall. MCP read p50/p95 shows no difference beyond noise. A heavy `git diff --stat` runs about 1.8× slower when banded.
- **Codex ingest, Claude-only pass:** 20.7 ms on the lane vs 24.5 ms on base (min of 6), but host noise exceeded 5%, so no claim is made.

## New cards filed

- **ft-92734a91** (owner decision): an unbound worktree has no task-scope fence, so any path to "no binding" widens access.
- **ft-001bf7fc**: the ledger does not dedup transcript rows after a cursor reset. Pre-existing; affects Claude too.
- **ft-02a20716**: stale transcript cursors accumulate once a source has recorded 2 or more roots. Liveness only.
- **ft-a2936457** (from wave 2): its reverted patch now lives in the card's logs.

## Still open

- **ft-d902fcb4** (governor 1b): every AC is met by main except S5-07, the low-disk check.
- Blocked on held files (ft-42081f16 `hooks/mod.rs`; ft-0ac24164 `terminal/mod.rs`): ft-9cfd5574, ft-9cfd5474, the hook half of ft-da85cd55, ft-9cfd5374 and ft-a2936457.
- **ft-cc4be67b** (translucency A/B): measurement-only, and meaningless at this host load.

## Owner decisions pending

1. **Merge `b21/integrate` to `main`.**
2. **Codex code-mode:** approve a JS parser dependency (option C) if you want code-mode rollouts recorded. Today they are counted only.
3. **ft-92734a91:** should unbound worktrees be fenced?
4. **QoS behaviour to confirm:** every `gitpulse-mcp` and `gitpulsed` child is banded, because those processes' default class is Background.
5. **Host:** an orphaned TERM-ignoring busy loop (pid 36903, from DevCouncil corpora) ran at 99% CPU for 5 h. It is not ours, and it was not killed. Host load reached 118 during this wave.
