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

---

# Waves 3–4 landing and wave 4 (2026-10-22)

**State.**
- **Wave 3 is merged** (`main` → `795ca31d`). Another session's uncommitted work in the shared checkout (70 files, idle for 14 h) blocked the merge. With the owner's approval it was **parked byte-exact on `parked/other-session-1022`** (verified against the working tree) before the checkout was cleaned. Nothing of it was edited. Rebase that branch onto main to resume it; expect a conflict in `src-tauri/src/lib.rs`.
- **Wave 4 is merged** (`main` → `a8fecaa6`).
- Nothing is pushed. All wave-3 and wave-4 branches and worktrees are removed. Other sessions' branches are untouched.

**Owner decisions in wave 4:**
- Low disk is **advise-only**.
- `compressor_thrash` stays a deny fact.
- ft-d1c290f1 is **deferred**.
- The tree-sitter JS parser is **approved**.
- Adding **sha2** as a direct dependency is **approved**.
- Parking the other session's work is **approved**.

## Cards moved to review (all on main)

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-5c0f0262** test time bombs (filed this wave) | One `GIT_MAINTENANCE_PIN` helper (maintenance.auto=false, reflog expiry never) for every test that uses its own git config, plus `$HOME/.gitconfig`. A source-scan guard enforces it. A fixture dated one year back keeps its full reflog (130/130; 30/130 without the helper). 264 short sleeps were reviewed; one was converted. | Test infrastructure; Opus-reviewed. |
| **ft-d902fcb4** governor 1b, S5-07 (low disk, advise-only) | An injectable free-space reader: a panicking or unreadable reader means unknown. Copy accounting uses allocated size and the destination's `f_frsize` (APFS reports `f_bsize` = 1 MiB). An 8 GiB cap and a 10 GiB floor, whole-or-nothing through a typed `CopyStop` and a staging drop guard. Free space is re-checked every 64 MiB, including mid-file. A per-volume in-process reservation stops concurrent copies from jointly crossing the floor. Advisories are kept per repo, deduped to one per 10 min, and a cache error is never hidden. No deny path exists anywhere. | 2 rounds: 11 breaks, then 8 more; all fixed. |
| **ft-6a6432c1** Codex code-mode parsed (tree-sitter-javascript) | Two direct dependency lines; Cargo.lock gains edges only, no new packages. Only straight-line calls with literal arguments are recorded. Everything else is counted with a reason: conditionals, optional chains, unreachable code, syntax errors (nothing recorded), alias, eval or Function strings. Bounds: 256 KiB, 250 ms and 200k nodes. A 256 KiB program takes about 40 ms in release. | 1 round: 5 gaps fixed. |
| **ft-001bf7fc** ledger transcript dedup | The transcript-row ULID is **deterministic**: the line's own time plus 80 bits of SHA-256 over a canonical identity. The existing UNIQUE index dedups every replay path at O(log n), replacing a 30 s full scan. A content mismatch is counted, never silently held. Consolidation no longer aborts on `legacy_ulid_conflict`. | 2 rounds; FNV replaced by SHA-256 (owner-approved sha2). |
| **ft-02a20716** stale cursor growth | A recorded root confirmed absent (ENOENT only) for 5 passes is retired, and forgetting resumes. The marker stays bounded at 8 roots. A corrupted counter never panics. | Covered by both dedup rounds. |

## Gates (verified)

- **Gate 6, full suites on `b22/integrate` a4988026.** Only integration drift failed: rustfmt order, a test spawning outside the locked seams, three TS wire fields, the `LowDiskCause` union, and the documented field count. Fixed in a8fecaa6.
- **Gate 7, on a8fecaa6.** fmt, clippy `-D warnings`, `spawn_seam` 14, `hermetic_contract` 13, `ingest` 327, `bun run check`, `check:types` (0 drift) and `check:ipc` all OK. vitest: 10837 passed, 1 failed. The failure is `markdevRender` "does not slow quadratically" at 8.9 vs 8: a load flake in a file wave 4 does not touch, which passes 3/3 alone.
- The first gate 7 run died with **ENOSPC**: the disk was at 132 MiB free with about 10 lane worktrees and 3 gate trees alive. They were removed after verifying each was merged, freeing 102 GiB.

## Benchmarks

- **Dedup append path:** UNMEASURED. The host load stayed between 13 and 76; a noisy debug run showed +14% (min of 7), which is not claimed. A release run on a quiet host is still needed.
- **tree-sitter:** a 256 KiB program takes a median of 42.6 ms in release.
- **Low disk:** on macOS a byte copy now walks the source twice. Not measured.

## Still open

- **ft-92734a91** (owner decision): unbound worktrees have no task-scope fence.
- **ft-b595921a** (owner decision): the typed-error migration.
- Blocked on held hooks and terminal files: ft-9cfd5574, ft-9cfd5474, the hook half of ft-da85cd55 (the spawn-budget drain still blocks commands under load), ft-9cfd5374 and ft-a2936457.
- ft-cc4be67b (translucency A/B): needs a quiet host.
- The installed `gitpulse-mcp` and `gitpulse-hook` predate waves 3 and 4, since the owner chose not to reinstall. Run `bun run mcp:install` and then `mcp:doctor` when convenient.

---

# Wave 5 (2026-10-23)

**State.**
- **Wave 5 is merged** (`main` → `68ef1882`, a fast-forward of `b23/integrate`).
- Nothing is pushed (owner: no push this run).
- **Installed binaries were NOT reinstalled**, by owner decision. dc-store 10 migrates stores on a read-write open, so the reinstall waits until Jarvis is re-pinned.

**Owner decisions in wave 5:**
- Stale holds freed: ft-42081f16 (hooks files), ft-0ac24164 (terminal/mod.rs), ft-626b25fc (flakes taken over).
- Wave 5 was a fresh bug/perf audit plus finishing the taken-over cards.
- Priorities were rewritten with a log entry.
- The re-vendor reported its deltas before landing.
- **dc-store 9→10: merge, hold the reinstall.**
- The Manvi half of ft-329789a9 is run by the owner (`tmp/build-1020/manvi-handoff.md`).
- Seven stale processes were killed with the owner's approval, by explicit PID:
  - a 29 h orphaned `cargo test --lib browser::` and its test binary, which was holding a cargo slot;
  - five wait loops that could never end, because they matched their own command line with `pgrep -f`.

## Cards moved to review

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-42081f16** signal guard + hook on launched sessions | All ACs were already on main (8c03e89e, 6fc50cb2, 59858ec1), re-verified with file:line. **Partly met:** with the plugin enabled, the hook decision runs twice and only the advisory is deduped. | Card verification only. |
| **ft-da85cd55** hook half, AC2 (spawn drain) | The worktree family is remembered across hook processes in a private per-user file. The file is an *order hint only*: membership must equal the live registry and the anchor is the metadata-proved primary. Opens use O_NOFOLLOW\|O_NONBLOCK with an fstat check; staging is exclusive; the directory is bounded at 256 records. Spawns: Bash gate 1→0; task-bound edit 5→0 with the app attached and 5→4 without (the scan floor; misses the ≤3 target). AC5 (live, 4 agents) is UNMEASURED. | 1 round, 13 tests, 5 breaks. The **security break** was a same-uid forged file moving the anchor and widening the family. Also: reorder, FIFO hang, staging pid reuse, unbounded directory. All fixed; 13/13 pass, plus a guard-path forgery test. |
| **ft-626b25fc** full-suite flakes (taken over) | See the flake summary below. | 1 round, 7 targets, 4 gaps (a missing skipped counter, a missed routing mutation, the CPU ratio missing n²/64, a shared fixture identity). All fixed and shown by mutation. |
| **ft-329789a9**, **ft-7e812b3f**, **ft-f8a297ff** | Already on main; re-verified per AC. The Manvi half of ft-329789a9 is the owner's handoff. ft-f8a297ff's push ACs are unmet (no push). | — |
| **Re-vendor** (W5-L5) | DevCouncil crates moved to devmap store 27, **dc-store 10** and workbench 14. The six dc-* crates moved 0.2.4→0.2.5 (Cargo.lock: those 6 entries only). The Manvi serve contract was copied. Fixtures now plant tasks via workbench items. | Gate-verified. |

**ft-626b25fc flake summary.** One flake was a **product bug**: two processes creating one ledger raced on `PRAGMA journal_mode=WAL`, which can answer BUSY without calling the busy handler, so the loser never opened. Fixed in `enter_wal` (bounded 10 s retry). Before/after:
- WAL race test: 10/10 fail → 30/30 pass.
- Reflog replay: 2/60 → 0/100.
- `CatchUp.reflog_skipped` is new, so the reflog test asserts the exact sum again.
- Instance broken-pipe: 1/60 → 0/100, now covered by two tests.
- Trust-storage HOME race: replaced by a thread-local seam.
- ten_thousand_binds: measured on thread CPU time; 6/20 → 20/20.
- markdevRender: CPU ratio at 80k vs 320k; the quadratic mutation fails it 5/5, and unmutated passes 20/20.
- Write budgets: entity_collision 484 → 156 MiB, storm 291 → 174 MiB at a reduced scale with the same shape.

## dc-store 10, what it does to a store (read from the vendored 0.2.5)

- **Read-write open** of a schema-9 store migrates it, in one transaction:
  - each legacy `tasks` row becomes a workbench item with the same id;
  - `DROP TABLE tasks`;
  - the store is stamped 10.
- A row that cannot convert refuses the whole migration.
- **Backup** `<db>.pre-tasks-retire.bak` (VACUUM INTO) is written only when the table has rows. If the backup fails, the migration is refused.
- **Read-only open** refuses a schema-9 store and changes nothing.
- After migration, **older readers refuse the store**.
- The real stores surveyed read-only are all at 9:
  - DevCouncil: 17 rows
  - Jarvis: 10 rows
  - MLSystemsLab: 1 row
  - the rest: 0 rows
  - six stores would not open read-only (unknown state).
- The Jarvis pin is stale (dc-store 9), so Jarvis would refuse a migrated store. Re-pin Jarvis before reinstalling GitPulse.

## Gates (verified)

**Gate 8, full suites on `b23/integrate` 496c64da.** fmt, clippy `-D warnings`, `bun run check`, `check:types`, `check:ipc` and `check:vendor-schema` all OK. Failures:
- **cargo `--tests --no-fail-fast`: 2 failures.**
  - `supervised_threads`: integration drift. The adversarial test module's name did not end in `tests.rs`, so the thread scan counted its raw spawns.
  - `agent_layout_stays_linear_on_hostile_input`: 31 s under load 30–60, and 1.15 s alone. Its code is untouched by wave 5. Filed in ft-fbd79cba.
- **vitest: 10837 passed, 1 failed.** The documented field count 1815 → 1816 (the new `reflog_skipped`).
- **`check:schema-parity`: 4 match, 3 stale, 1 unverifiable.** Stale: gitpulse-mcp on PATH and GitPulse.app (reinstall held), and the Jarvis pin. Unverifiable: `devmap --version` does not print the extraction schema (DevCouncil).

The drift fixes are in 68ef1882. Re-run on that tree: supervised_threads 9/9, documented-counts 20/20, worktree adversarial plus agent_layout 14/14.

## Benchmarks

- Hook spawn counts are asserted exactly by tests; see the table above.
- No wall-clock benchmark was run, because the host load stayed at 13–100 throughout.

## Still open

- **Reinstall** (`bun run mcp:install`, `mcp:doctor`, rebuild the app) is held until Jarvis is re-pinned to a DevCouncil with dc-store 10. The Jarvis bump is a pin change in another repository and needs its own session.
- **Manvi half of ft-329789a9**: the owner's handoff.
- New cards:
  - **ft-eb5034ce**: churn_store has the same WAL-switch race; one helper should own the WAL switch.
  - **ft-fbd79cba**: the storm test misses coalesce and ignore regressions (pre-existing), plus load flakes in desktop::instance, git_reader memo and agent_layout.
- Still waiting: ft-92734a91 and ft-b595921a (owner decisions); ft-9cfd5574 and ft-a2936457 (wave 6); terminal-side cards (wait for `term/integrate`); ft-cc4be67b (needs a quiet host).

---

# Wave 6, part 1 (2026-10-23)

**State.**
- **Merged:** `main` → `95a1f515`, a fast-forward of `b24/integrate`, carrying lanes W6-L1, W6-L2 and W6-L4.
- **Split out:** W6-L3 (WAL owner plus the test residue) is still in its second adversarial round and lands separately.
- Nothing is pushed, and the installed binaries are still not reinstalled (held until Jarvis is re-pinned).

**Coordinator rulings this wave (Fable authority):**
- `gitpulse_collision_risk` without `path` now omits the `Option` `entity` verdict instead of attaching one computed for row 0. This is a correctness fix inside the existing contract, not a shape break.
- Vendored DevCouncil code is never edited here. The slice guard scans it in report-only mode.
- No `cfg(test)` forks of production timing or caching. Applied to W6-L3, which is now being reworked.

## Cards moved to review

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-9cfd5574** pre-edit collision blind spot (high) | One owner, `insights::collision_for_path`: a pathspec-limited `status -z -u -- :(literal)` per other worktree, used by the hook, change_context and preflight. One shared `STATUS_ARGV` with showUntrackedFiles=all pinned. Optional `path` on `gitpulse_collision_risk`. No row-0 fallback. The incoming Edit/Write is placed on HEAD lines, and an unlocatable edit asks. | 30 tests; 4 breaks, all fixed: **(security)** a binary or unparsed diff read as Disjoint, now fails closed; a case-variant untracked file on APFS; a target under a gitlink; the scan cap's unread tail, now INCOMPLETE. The 18-worktree slow-git deadline answered in 4.25 s with a partial notice. |
| **ft-a2936457** ledger honesty fold | `LedgerView` folded into `LedgerStatus`. `status_readonly` reports `recording:false`. The MCP wire is byte-identical against a golden captured on base. check:types learned `serde(flatten)`. session_brief briefs "readable (…)" from the actual freshness basis, keeps drops and caveats, and the unreachable branch was deleted with callers cited. | 5 breaks, all in the brief: a false catch-up claim, drops hidden, caveats hidden, the dead branch. All fixed. The wire and checker held (16 mutations caught). |
| **ft-181d43a9** non-ASCII slice panics (filed from the W6-A audit) | `fs_entry::strip_suffix_ascii_ci`, plus a third panic found in commit_graph `decode_hex` (non-ASCII `.git/HEAD`). `tests/str_tail_slices.rs` guards the class, and scans the vendored crates in report-only mode. | 8 guard breaks, all fixed. The oracle proptest against the old implementation is identical. |

## W6-A audit (read-only)

- 4 findings. Opus verified the byte arithmetic of both panics.
- Filed as **ft-181d43a9** (done above) and **ft-eeb28bf6**: storage walks drop non-UTF-8 and non-permission errors uncounted, and mcp_info hides a malformed template.

## Gates (verified)

**Gate 9, full suites on `b24/integrate` 95a1f515.** fmt, clippy `-D warnings`, `bun run check`, `check:types`, `check:ipc` and `check:vendor-schema` all OK. vitest **10855 passed, 0 failed**.

- **cargo `--tests --no-fail-fast`:** 1 failure, `adversarial_repos::ref_name_validation_rejects_hostile_branch_names_without_side_effects`. It is the git spawn-rate limit deferring under load ("local bucket: 0 token(s)"), in a test wave 6 does not touch. The rerun on the same tree passed 16/16 (load 42).
- **`check:schema-parity`:** the same expected stale rows (installed gitpulse-mcp and app, Jarvis pin) and devmap unverifiable.

## Still open

- **W6-L3** (ft-eb5034ce WAL owner, ft-fbd79cba residue) is in its second round:
  - an injectable production ownership deadline;
  - `enter_wal` patience as a total bound (a busy handler currently stacks on top of it, so the ledger worst case is about 15 s against the stated 10 s);
  - the ref_cache test-build fork to be removed;
  - which thread runs the churn store initialiser.
- **ft-eeb28bf6:** storage partial totals and mcp_info.
- **Load-sensitive tests seen this wave:**
  - `adversarial_repos` ref-name (spawn bucket under load);
  - `entity_collision::the_join_over_256…`;
  - `git_reader` `a_carried_answer_is_not_spent_against_the_time_budget` and `adv_l4` torn-answer (both fail on base too).
- Carried from wave 5: the reinstall (after the Jarvis re-pin), the Manvi half of ft-329789a9, ft-92734a91 and ft-b595921a (owner decisions), the terminal-side cards (waiting on `term/integrate`), and ft-cc4be67b (needs a quiet host).

---

# Wave 6, part 2 (2026-10-23 to 2026-10-24)

**State.**
- **Merged:** three lanes, each fast-forwarded on its own after a full gate:
  - W6-L3 → `1900229e` (gate 10);
  - W6-L5 → `08875ad4` (gate 11);
  - W6-L6 → `4b640edd` (gate 12).
- Nothing is pushed. The installed binaries are still not reinstalled; that waits on the Jarvis re-pin.
- **Held files are now checked mechanically.** `tmp/build-1020/held-check.sh` intersects each lane's diff with `term-files.txt` before integration.
  - It refused W6-L3's `lib.rs` edit, which moved to `engine::sqlite_wal`.
  - It deferred one W6-L5 item, because `SettingsModal.svelte` is held.
  - Earlier waves had already changed 5 held files on main. The term/integrate handoff (`tmp/build-1020/term-integrate-conflicts.md`) lists them, along with how to resolve each.

**Coordinator rulings (Fable authority):**
- No `cfg(test)` forks of production timing or caching. Isolating a global per test thread is allowed.
- An unreadable location is never `ReclaimSafety::Safe`.
- Any subtree that was not counted marks a storage total as partial.
- Multi-threaded tests: the test thread holds `retention()` and then `env_serial()` across spawn and join, and the children stay lock-free. `tests/lock_order.rs` lints this order.

## Cards moved to review

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-eb5034ce** churn WAL switch, plus **ft-fbd79cba** residue | One owner for the WAL switch, `engine::sqlite_wal::enter_wal`: ledger patience 10 s, churn 2 s. Patience is now a total bound, with busy_timeout at 0 during the switch and restored on every path. The churn `ensure_schema` runs in one IMMEDIATE transaction, which was the second root cause. `OpenError::Busy` keeps the cache. The ownership deadline is a constant with a per-thread test override. | 2 rounds. Breaks found: the production ownership deadline could not be tested (a cfg fork), and busy_timeout stacked on top of the patience, a worst case of about 15 s against the stated 10 s. Both fixed. The ref_cache premise was refuted. |
| **ft-eeb28bf6** storage partial totals, mcp_info | `WalkBudget` owns walker errors: unreadable and non-UTF-8 entries are counted, and `ScanStats.unaudited` is added. Paths stay `OsString`, so labels are never used as git paths. An unreadable gitdir is never offered as Safe to reclaim. The orphan audit's ordering bug is fixed. `mcp_info` carries `plugin_error`. | 9 breaks. **HIGH:** an unreadable gitdir was offered as Safe to reclaim. The others: an orphan cap with no notice, EACCES not marked partial, a missing root reported complete, a non-UTF-8 label used as a git path, a double count, a false error for an optional hooks.json, and silent count-objects/tracked/canonicalize arms. 8 fixed. The Settings display of `plugin_error` is deferred, because SettingsModal is held (see the handoff). |
| **ft-d31af555** wave-6 load flakes and wall-clock bounds | Root cause: the user-level mailmap test rebound `HOME` for seconds without the environment lock, so neighbours missed their stored answers or read a mailmap they never wrote. Fixes: `unhurried()` and churn's `UseStore` hold the env lock; there is one injectable `Bound` seam for the three wall-clock bounds (the shipped constants are unchanged and asserted as literals); the mailmap test gets per-run temp files; git_reader fixtures read the pinned hermetic config; the entity_collision join is timed on thread CPU; the adversarial_repos spawn gate is unlimited; `supervised_threads::is_test_file` is exact. | 1 round, 4 breaks, all fixed. `sixteen_threads` called `unbounded()` without the env lock (7 lookups against 1). The `l4` tolerance admitted a transient +2; the test now checks every row against states the writer actually published, and a planted +2 fails it. The `test_support` prefix match was too loose. The constants test compared constants with themselves. Held: lock order, the seam being a no-op in production, the mailmap test, and the unmetered spawns. The mix battery went from 0/100 on base to 110/110 in round 1 and 50/50 under 40 CPU hogs in round 2. |

## Gates (verified)

- **Gate 10** (`1900229e`): all steps green.
- **Gate 11** (`08875ad4`): green after one drift fix. The str tail-slice guard flagged a `Vec<Component>` slice in `storage/mod.rs`, which is now allow-listed. A docs 200 ms timing test failed at load 100 and passes alone.
- **Gate 12** (`4b640edd`): fmt, clippy `-D warnings`, cargo `--tests --no-fail-fast` (130 suites, 0 failures; no load flake this time), `bun run check`, `check:types`, `check:ipc`, `check:vendor-schema` all OK. vitest **10855 passed, 0 failed**. `check:schema-parity`: the same expected stale rows (installed gitpulse-mcp and app at dc-store 9, Jarvis pin), and devmap does not report devmap-extract.

## Still open

- The reinstall, after Jarvis is re-pinned.
- The term/integrate handoff: 5 held-file conflicts, plus the SettingsModal `plugin_error` display (its test is in `21607735`).
- `ScanStats` is not in a check:types contract, because the script is held. The TS mirror was checked by hand only.
- 21 integration suites still lack the unmetered spawn call. Only adversarial_repos was switched.
- The env.rs `EnvBinding` fix (env.rs is held).
- Carried from earlier waves:
  - the Manvi half of ft-329789a9;
  - ft-92734a91 and ft-b595921a, which are owner decisions;
  - the terminal-side cards;
  - ft-cc4be67b, which needs a quiet host.

---

# Wave 7 (2026-10-24): the hook-report path

**State.**
- **Merged:** three lanes in sequence, each fast-forwarded after a full gate:
  - W7-L1 → `3944fb16` (gate 13);
  - W7-L2 → `6446002e` (gate 14);
  - W7-L3 → `46b40ea7` (gate 16).
- All three change the agent hook bridge (`alerts/bridge.rs`, `alerts/mod.rs`, the new `alerts/intake.rs`), so they ran one at a time, each on top of the last.
- Nothing is pushed. The reinstall is still held until Jarvis is re-pinned.

**Coordinator rulings (Fable authority):**
- **Ordering is kept by dropping, never by waiting.** Every report carries one order stamp from a system-wide monotonic clock: the hook's entry stamp, capped at the accept stamp. A report older than its session's newest is dropped and counted `stale`. A silent connection never delays another.
- **A superseded report is delivered.** It is `{"ok":false,"reason":"stale"}` on the wire, and Ok in the hook, with no warning.
- **Two identical reports inside the 500 ms twin window** may fold. The state after both equals the state after one, and the fold is counted.
- **`src-tauri/Cargo.toml` is held.** Benchmarks are ignored integration tests run with `--release --ignored`. The `[[bench]]` registration waits in the term/integrate handoff.

## Cards moved to review

| Card | What landed | Adversarial history |
|---|---|---|
| **ft-bdb253cf** half 2: hook bridge accept loop | Accept errors are classified. Transient errors back off, 10 ms doubling to 1 s, and keep the socket. Fatal errors rebind up to 3 times, then report `listening:false` with the cause. Each connection is read on its own supervisor worker, at most 100 open, and the 101st is refused unread. The per-connection ceiling is 1.2 s, under the hook's 1.5 s budget. Accept stamps and a per-session order book restore arrival order. | Round 1 BROKE ordering: same-session reports reversed 11–27 per 200 on the lane, 0 of 600 on base. It also broke two backoff status bugs. All fixed; the order tests pass 200/200 per shape. Held: slot leaks (8 threads × 300 connections of mixed churn), rebind against stop (80 rounds), an EMFILE storm (9 accepts in 3 s; a planted spin fails at 4000). |
| **ft-9cfd5474** hook-report protocol honesty | An optional `seq` in v1, capped at accept. One owner, `alerts/intake.rs`: the order book, bounded tombstones for every `session_ended` (PTY exit included), and typed `Offer` results. Queue class caps: bells 32 and answers 16, both coalesced per session, leaving 16 reserved for hook reports. Closed-set refusal `reason` plus `max_v`. The hook prints reasons through a bidi- and zero-width-safe filter. Clipping is by serialized size. | 20 adversarial tests, 5 BROKE and all fixed: PTY exit left no tombstone, answers could starve hook reports, a dead worker read as "queue full", a wrong-typed `agent` was accepted, and bidi characters reached hook stderr. Held: wire compatibility both ways, a forged `seq` capped, 10k-session eviction, counters reconcile, 400 clip cases. |
| **ft-9cfd5174** notify hook coverage | Six report-only notify events are injected into every GitPulse-launched claude through the existing `--settings` seam. Plugin and injected twins fold at intake, over the last 8 queued reports per session, keyed by the whole `Notice`. A twin never moves the high-water mark. notify quiet-exits before reading stdin when it has nothing to say. It extracts only its fields, with accept/reject identical to the old path. A 5 MiB `tool_response` still reports. Run budgets are per event, from the injected timeout (SessionEnd 1 s, so it never raises Claude Code's exit budget). | 6 BROKE and all fixed: interleaved parallel twins, `subagent` missing from the key, a refused report remembered, a late twin making a real report stale, the reader more lenient than before, and the read budget equal to the host timeout. Held: no second PreToolUse decision path, and every injected event name is valid in the current Claude Code hooks docs. Cost: an injected hook is 6.6 / 11.2 ms (min / median), async, and does not block the agent. |

## Benchmarks (committed, min-of-N)

- **`tests/alerts_worker_throughput.rs`:** 449k hook state reports/s and 545k renderer answers/s, N=9, release. There is no main baseline, because this path did not exist before.
- **`tests/notify_stdin_bench.rs`:** A (base) → B, release, 9×20.
  - Outside a session, 4 MiB: 7.40 → 5.79 ms.
  - Inside a session, 4 MiB: 9.03 → 8.99 ms. This is within noise, so no win is claimed.
  - 0 B and 100 KiB: no difference.

## Gates (verified)

- **Gate 13:**
  - cargo had 2 failures. supervised_threads was real: the adversary's test files were not named `*_tests.rs`, and a drift fix renamed them. assemble_tests was a load flake.
  - Everything else was green.
- **Gate 14:**
  - cargo had 1 failure, `assemble_tests::slow_command…`, which also failed in gate 13. A concurrent global `performance::flush` wrote an "unfinished at exit" line while the test's 1.1 s call was running (`oldest_ms=982`), and the loose selector picked it.
  - The drift fix selects the completion record (`outcome=`). It did not reproduce in isolation (0/12), so the fix rests on the log evidence.
- **Gate 15** (first W7-L3 candidate): cargo fully green (134 suites); vitest failed 2 in `scripts/plugin-contract.test.ts`, which reads the hook budget out of the Rust source and could not parse the new per-event budget. Fixed by plain per-class budget constants (`NOTIFY_BUDGET`, `SESSION_END_NOTIFY_BUDGET`) that both the Rust test and the contract read. Lanes now run full vitest when touching hooks, IPC or plugin files.
- **Gate 16** (`46b40ea7`): fully green. fmt, clippy, cargo `--tests --no-fail-fast` (134 suites, 0 failures), bun check, check:types, check:ipc, check:vendor-schema, vitest (10855 passed, 0 failed).
- **Throughout:** vitest 10855 passed, 0 failed. `check:schema-parity` shows only the expected stale installed rows (gitpulse-mcp and the app at dc-store 9, the Jarvis pin) and devmap's missing extraction schema.

## Still open

- **Hook-less bell opt-in (ft-9cfd5174 S1-07):** the count is `asksForReader` in `sessionActivity.ts`, but the opt-in setting can only live in held settings files. It is in the term/integrate handoff.
- **`stale` stays off the wire** until the held `sessionAlertsStore.ts` is free.
- **The plugin's own `hooks.json` SessionEnd entry has `timeout: 5`.** That raises Claude Code's exit budget to 5 s for plugin sessions. The hook ends within 1 s, but the host still waits up to 5 s if it hangs. Lowering it to 1 is a one-line plugin change to make with a plugin-contract test.
- **Cursor hooks.json loading is unverified;** its docs do not state the path or format. Codex is verified (it loads from the plugin root after a trust step).
- **ft-d1c290f1 (outbound data controls)** is blocked on two owner decisions: private registries, and consent for the AI endpoint.
- **Carried:**
  - the reinstall;
  - the term/integrate handoff;
  - the Manvi half of ft-329789a9;
  - ft-92734a91 and ft-b595921a (owner decisions);
  - ft-cc4be67b (needs a quiet host);
  - 21 integration suites without the unmetered spawn call.

---

# Wave 8 (2026-10-24): notify boundary and outbound data, and the run's wrap-up

**State.**
- **Merged:**
  - W8-L1 → `5b3c82b9` (gate 17).
  - W8-L2 and W8-L3 together → `9be2dbf8` (fixes to `d99c8072`) (gate 18).
- **Owner decisions this wave** (AskUserQuestion, 2026-10-24):
  - S1-09: approved as hardening.
  - Private registries: refuse and report.
  - AI endpoint: confirm before the first send.
- **Wrap-up.** The owner asked to wrap up: finish the running lanes, start no new agents, have Opus verify, merge, and clean every branch. W8-L2 therefore got an Opus review instead of an adversarial Sonnet pass.

## Cards

| Card | What landed | Verification |
|---|---|---|
| **ft-9cfd5074** S1-13 and S1-14 (W8-L1) | **Notify routes are guarded both ways** from one fixture (`scripts/fixtures/notify-routes.json`), read by Rust and by plugin-contract.test.ts; 12 kinds of hooks.json mutation were all caught. **One generated hidden-character table:** `scripts/hidden-codepoints.mjs` writes `fixtures/hidden-codepoints.json` (Unicode 17, Cc/Cf/Co), which drives both Rust `alerts::text` and TS `clean()`, with parity over all 1,112,064 scalars and a drift test. **Every display channel goes through the owner:** banner heading (a U+202E cwd now renders in order), session brief, menu, tray, commit-graph note, process name, model excerpt. `parseAttention` of an unknown event gives null, with a capped warning and a rate-limited summary. Plugin SessionEnd timeout went from 5 to 1. | Adversarial pass, 3 breaks fixed: 32 unassigned code points diverged between Rust and TS, two channels bypassed the owner, and warnings went silent after the cap. Held: no over-stripping; SessionEnd delivered 100/100 under a CPU hog. The held `tabs.ts` gap is pinned as `it.fails`. |
| **ft-9cfd5074** S1-09 (W8-L2) | `Host::knows` checks against the live terminal registry (`SessionHost` → `terminal::list_sessions`, failing closed). Hook reports for unknown sessions are refused as `unknown_session` and counted in `bridge_rejected`. A per-session token bucket (burst 256, 64/s, LRU-capped) refuses as `rate_limited`. Reports keyed `hook-…` share one bucket (32, 4/s). The registration-window and post-exit cases are defined and tested. **Security-impact statement:** reports for unknown sessions are refused, each session is rate-limited, same-uid remains the connection boundary, and nobody gains access. | Opus review in lieu of an adversary, per the owner. 6 of 9 new tests failed with screening disabled. **Accepted gap:** `knows()` can block up to the terminal host's 10 s `CALL_DEADLINE` when the host is hung. |
| **ft-d1c290f1** outbound data (W8-L3) | Scanner children run on an allowlisted environment (`capture_command_scrubbed`). Only `${VAR}` names that the pinned user `.npmrc` references pass through. npm userconfig, globalconfig and registry are pinned. `parse_npmrc` parses as npm does, and every directory up to the repo root is checked. A repo-set registry, token, proxy, TLS (`strict-ssl`/`ca`/`cafile`/`certfile`/`keyfile`) or `noproxy`/`tokenHelper` setting is refused as "registry overridden by repository config". bunfig fails closed (no TOML dependency). AI: `resolve_target` is the one gate, raising `ai_endpoint_unconfirmed`; the confirmation names the runtime and port; the saved selection is re-checked (60 s TTL); there is one shared pending confirmation; model names are cleaned through the shared owner. | Adversarial pass: 14 Rust and 2 vitest breaks, all fixed (several confirmed against real npm 11.20). Held: the env allowlist drops NODE_OPTIONS, npm_config_* and DYLD_*; real `npm audit` never touched a lockfile's `resolved` host. Lappi is confirmed to be an explicit opt-in, defaulting to false. |

## Gates
- **Gate 17** (W8-L1): green apart from three load flakes in untouched code, all of which passed alone (`adv_w3l2` 10/10, `vite-watch` 5/5).
- **Gate 18** (W8-L2 and W8-L3): fmt, clippy, bun check, check:types, check:ipc, check:vendor-schema and vitest (10884 passed, 0 failed) were green, and cargo passed 133 of 134 suites. The one failure was a real interaction with S1-09: `alerts::bridge::tests::concurrent_apply_leaves_listening_and_the_socket_in_agreement` failed 16 of 300 rounds with `answers=false`. Its probe is a session-less report, which now draws on the process-wide `hook-…` rate bucket (32, 4/s) that other tests also drain, so a live loop correctly answered `rate_limited`. The test asserted acceptance where it meant liveness; it now accepts any `{"ok":bool}` reply (d1969a12, rustfmt d99c8072). Rechecked: fmt, clippy, the guard suites and alerts:: 199/0. Other bridge tests that expect `ok:true` for session-less reports share the same latent exposure; it is recorded on ft-e7c644de.

## Wrap-up actions (owner-directed)
- **Branches deleted:**
  - 15 term-session agent branches, all contained in `term/integrate`;
  - `fix/bypass-gate`, already in main;
  - `parked/other-session-1022` (1c5b3ea1);
  - `term/fable-diffs` and `term/fable-host` (abcc6b76), with their uncommitted edits saved as patches;
  - `term/integrate` (849f8133; 111 commits not in main).
  The SHA audit (`tmp/build-1020/branch-audit-pre-cleanup.txt`) was deleted with the rest of `tmp/` later the same evening.
- **term/integrate was recovered, then dropped.** It was bundled and its worktree edits saved as patches, and then the owner had those backups deleted too.
  - Another session recovered it on the owner's "Recover and land" choice. It passed full gates at `b6bb352f`: 115 commits, 134 files, +30,319/−294.
  - It was held back from landing because three of its files overlap main's uncommitted docs.
  - Late on 2026-10-24 the owner had the branch, the `keep/term-integrate` tag and a verified bundle of both deleted. Seven term follow-up briefs in `tmp/term-tasks/` (never on the board) went with `tmp/`.
  - None of that work is in main. Tip `b6bb352f` survives only as unreachable objects until git garbage-collects them.
- **`tmp/` was emptied** at the owner's direction, along with both old stashes (`216b22e3` and `de250041`).
- **Gap cards filed:**
  - ft-67f95e2d: fixes deferred while the files were held, which are now free on main;
  - ft-24c70de8: reinstall and re-pin;
  - ft-e7c644de: load-sensitive tests and 21 suites without the unmetered spawn gate;
  - ft-878db72b: hook coverage gaps.

## Incident: the board went down mid-wrap-up
- **Symptom:** from about 16:25 every gitpulse MCP board call (read or write) failed with `unsupported DevCouncil schema version 10; this binary knows 9`.
- **What did not change:** the installed `gitpulse-mcp`, `dcstore` and `devmap` binaries (all Oct 21), and every DevCouncil `state.sqlite` on disk (main files from Oct 7–11, empty WALs).
- **Not identified:** which store reports 10. The command gate blocks reading a store's schema version.
- **Fix:** the reinstall in ft-24c70de8 (a dc-store-10 gitpulse-mcp), or finding the store.
- **Board writes owed,** to apply once the board answers again:
  1. **Overwrite ft-67f95e2d.** Drop part A (the term/integrate merge-conflict table, which is moot now that the branch is dropped). Add B8: S1-09 `knows()` can block for up to the terminal host's 10 s `CALL_DEADLINE`; bound it by the connection budget in `terminal/host/client.rs`.
  2. **Move ft-9cfd5074 to review.** S1-13/14 merged at 5b3c82b9 and S1-09 at 8d921144. Include the security statement and the accepted `knows()` latency gap from this wave.
  3. **Move ft-d1c290f1 to review.** It merged at 9be2dbf8.
  4. **Add to ft-e7c644de:** the bridge tests that expect `{"ok":true}` for session-less reports draw on the process-wide `hook-…` bucket (32, 4/s). The affected files are `alerts/bridge/tests/wire_adv_tests.rs`, `adv_tests.rs:72` and `protocol_tests.rs`. Give each a session key or a per-test bucket.
