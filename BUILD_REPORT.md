# GitPulse backlog build: 2026-10-17 / 18

**State.**
- `main` started at `cc33ddc8`, the commit the release session verified. It is now at the tip recorded at the end of this report.
- Every lane was merged to `main` only after its adversarial pass and an Opus review. The owner asked twice to merge, run the next wave and delete branches. The second time also asked to commit.
- Nothing was pushed. No tag was created or moved.
- The coordinator's running log, with every SHA and gate output path, is in `tmp/build-1017/LOG.md` (gitignored).

**Process for each card.**
- An Opus brief, then a Sonnet implementer in an isolated worktree, with tests that fail first.
- A separate Sonnet adversarial pass. Every break went back to the implementer.
- An Opus review against the brief and its callers. The review sent work back three times: the `normalizeRepoPath` fail-open, the `gitpulse_landed` object writes, and the hook-replay fixture.
- Fable made the sequencing and judgment calls. Merges were no-ff `merge-tree`/`commit-tree` commits on `b17/integrate-1017`, then a fast-forward of `main`. The gate refuses git in other worktrees, so that is how integration was done.

Labels: **verified** means a command was run here and its output read. **inferred** means it follows from reading but was not run. **unverified** means it was not checked.

---

## 1. Tasks moved to review (merged to `main`)

Done is evidence-gated by Manvi. Each card below is in **review**, because no Manvi `work.verify.task` record exists for its head.

| Card | What landed | Adversarial breaks found and fixed | Still open on the card |
|---|---|---|---|
| **ft-576fb6dc** schema-parity extract row | `check:schema-parity` compares devmap-extract `EXTRACTION_SCHEMA_VERSION` on every reader; the gitpulse-mcp manifest reports it | 3 (comment-blind constant reader ×2, NaN judged stale) | `devmap --version` must print the extract schema (DevCouncil change); reinstall the binaries |
| **ft-007a2b4b** run in waves | The post-merge gate runs on the pre-merge tip; the merged set is re-verified; members verify concurrently; `verifying_merge` is persisted; Stop is signal-bound; overlap uses identityKey | 6 (S5-09 crash window, hex base resolved as a branch, dot segments) plus 2 from Opus review (`normalizeRepoPath` fail-open, UNC climb) | Passing `maxPriority` from TaskBoard (ft-329789a9 holds TaskBoard.svelte); Manvi behaviour on base==head; no live sidecar E2E |
| **ft-9cfa5374** CARD 13, non-hook half | A load-shed resolution is `deferred_worktree`; failed facets carry `deferred` plus `retry_after_ms`; MCP reads retry once with jitter | 5 (hostile path spoofing a deferral, quoted marker, ok:true marked, parent marked, schemas) | **Hook half** (Deny becomes "did NOT run"), gated on ft-42081f16, and reproduced live in this session; `mcp/resources.rs` readers |
| **ft-9d044174** CARD 21 ownership | A churn-ranked, area-stratified sample taken at HEAD; a memo keyed by HEAD and the mailmap inputs; single-flight; one mailmap rule. **Root cause beyond the card:** `git blame` rejects `--use-mailmap`, so the report was empty on every repo with a `.mailmap` | 6 (rename lost rank, gitlink truncation, no single-flight, silent churn cap, `(root)` collision, header-shaped rename) | Follow-up: a load flake this card introduced (`the_ownership_sample_includes_the_highest_churn_files_that_sort_last`); the 60 s TTL |
| **ft-04c0ab7e** remotes (security) | Credentials are redacted before IPC (display URL plus `has_credentials`); Set URL keeps or drops stored credentials; fetch, pull and push only reach configured remotes | 6, including **HIGH**: a membership bypass through the push branch slot; and **MEDIUM-HIGH**: a stored secret in the judged argv and the webview verdict | `record_gate` row not tested separately (it records the redacted argv) |
| **ft-bdb253cf** half 1 | Linear XML entity decode (49 s to 3 ms at 1M `&`), byte-identical to the old decoder over 120k fixed-seed cases; bounded no-follow manifest and lockfile reads | 3 plus 1 accepted (hardlink) | **Half 2:** bridge.rs (S1-10, S1-11, E-integrations-11 criterion 2) |
| **ft-f815889e** MCP background shed | The diagnosis was corrected (see §3). An agent's MCP read waits up to 2 s for the shared budget instead of being shed at 0 s; it never spends the reserve | 2 (a starved waiter held the last slot; 76 shared-record rewrites/s) | Live criterion not met: the drain comes from hooks (filed as ft-da85cd55) |
| **ft-cd18575e** HEAD/base cache | Verification sweeps over unchanged cards launch no git: **7.65 to 0.38 launches/s** (23 cards, 60 s windows) | 1 (a symref outside refs/heads served stale), 1 accepted | AC1, top-5 launch sources: needs a `git_cli.rs` source tag; `cmd_resolve_git_root` |
| **ft-9cfa4d74** CARD 15, S3-11 | Provenance `landed_on_base`/`on_base`/`ahead`/`behind`, null plus a reason when unmeasurable; `ledger_events` order plus filters with two indexes | Fable split the card; `gitpulse_landed` had 2 High and 5 Medium breaks and moved to ft-df6bf5ea | S6-05 (OD 5), S6-07 (OD 10), event-time filter (OD 1) |
| **ft-adc5dca5** parity half | Validating `*_argv` builders shared by each `cmd_*` and its writer; a harness asserting the spawned argv equals the judged argv; delete_tag and push_tag tests | 1 (a refused name still reached the gate and ledger) | **Owner decision:** remove `stash_pop` / `cmd_stash_pop` |
| **ft-b4eb6688** run errors | One timeout classifier and one unborn-HEAD classifier; a typed `RunError` view; damaged refs give Err; lock retry replays only git's own `index.lock` failure | 1 HIGH (commits no longer retried on a real lock), 4 Low/Med; Opus returned one fixture that had re-opened A-git-13 | A hook that prints git's full lock block verbatim is still replayed |
| **ft-ade6e999** ref_cache half | An injectable test clock for the TTL and the young-stamp guard; production is byte-identical | 0 (a TTL mutation is caught) | Watcher half (ft-1f30a461 / ft-626b25fc hold the watcher) |
| **ft-22bcf053** sweep race (filed this run) | A concurrent sweep's delete counts as done; the marker is dropped only once its aside is confirmed gone | 1 (marker loss orphaning an aside) | none |
| **ft-541eafaf** budget residuals | Every ledger string field is bounded in one place; arrays stay arrays; every cut is recorded in additive `verdict_truncated` / `data_json_truncated`; the budget probe is derived from the registry | 7 (silent number-array cut, emptied array, schema-violating marker, unrecorded cut, an uncapped 277 KB verdict, a forged record, dropped groups) | Unbudgeted desktop ledger reads, by design |
| **ft-c0d321c1** proposals | **Stale.** All 7 criteria were already on `main` (8156f0a2, 802999bb, 53e95963, 2026-10-12). Verified and re-tested; no code changed | n/a | Manvi held done; the macOS screenshot was not taken |
| **ft-8f398bb6** write primitives | Writes into a nested repo's real gitdir are refused at any depth; deleting or moving a folder that holds one is refused by a bounded downward walk (refuses at its cap); `.git` spellings that git refuses are refused, including `\` (CVE-2019-1354 shape); Overwrite is byte-exact over mixed-EOL or invalid-UTF-8 files; DiffViewer skips the digest | 3, including **HIGH**: `delete .modules` wiped a nested repo's history; and **MEDIUM**: `.git\config` was accepted | Item 2 (owner decision, widens file mode); Windows path unverified; moving a folder holding a work tree whose `.git` points outside it |

| **ft-5c1313c9** typed IpcError (wave run on 2026-10-18 after the owner's 4th "merge + next wave") | `IpcError {kind, message, detail?}` across ~240 commands, with byte-identical messages. Kinds are typed at their source (spawn-gate deferral, trust refusal with `{path, common_dir}`, missing identity, stale stash). A plain String is `unclassified` and never re-parsed. The deferral retry is limited to 24 reviewed read commands and stops once no caller waits. The classifier contract scan follows provenance | v1: **3 HIGH** (a mutation's wrapped deferral retried; hook stderr forging a deferral; an attacker-named branch raising the trust prompt). v2: 4 MED plus a writer audit (4 "read" commands found to write). **Fable ruling (A)**: decouple retry eligibility from admission; revert the admission changes; fix and merge | Helper-thread deferrals are not typed (fail-safe; pinned; **ft-e2c10b99**). Allowlist identity uses the closure type name. Trust `detail` is not yet consumed in the UI |

Coordinator commits made directly on `main`:
- `adfb63c8`: RELEASE_REPORT.md (owner: "commit").
- `e53886cc`: CARD 28 docs half. The ingest module doc and the 1.5.0 notes no longer claim Codex ingest.
- `94d94acc`: rustfmt.
- `99e179f6`: the hermetic wall-clock fix.

## 2. Regressions this run put on `main`, all fixed forward

These were caught after merge. Each one is a gap in my merge gate, now closed.

1. **Deferred verification cards were never retried.**
   - **Cause:** CARD 13 anchored `is_deferred_under_load`, and the verification driver wraps the refusal in its own prefix. The caller audit missed the driver, and my post-merge check ran selected modules.
   - **Fixed:** `8ecb81e5`.
   - **Gate now:** the full `cargo test --lib` whenever a shared helper changes.
2. **The hermetic and supervised-thread contracts broke** (ft-f815889e, ft-04c0ab7e, ft-cd18575e tests).
   - **Fixed:** `59b2f3c4`, and again `99e179f6` after ft-b4eb6688.
   - **Gate now:** the full `cargo test --tests --no-fail-fast`, and both contracts are always-run lane gates.
3. **The documented contract-test table and 5 TS typecheck errors** (ft-576fb6dc).
   - **Fixed:** in the remotes merge `a79beca5` / `0230604e`.
   - **Gate now:** the four IPC/doc contract tests plus `bun run check` on every merge.
4. **`cargo fmt --check` failed** (ft-ade6e999 adversarial tests).
   - **Fixed:** `94d94acc`.
   - **Gate now:** fmt and clippy on every merged tree.

5. **Two vitest contracts broke** (bare `setInterval` in WavePanel, from ft-007a2b4b; an unscaled limit in the parity adversarial test, from ft-576fb6dc).
   - **Found by:** the final full vitest run.
   - **Fixed:** `67809283`.
   - **Gate now:** the full vitest suite, not selected files.

**Caveat.** This session cannot check out a merged tree. So the gate runs right after the fast-forward, not before it, and a red gate is fixed forward.

## 3. Root causes this run found beyond the cards

- **`gitpulse_insights` was blind all session ("deferred under load after 0.000s").**
  - It is not a starved per-process MCP bucket. The live `spawn-budget.v2` shared record is drained to 0 by edit-time hook processes, about 15 git spawns each, under multi-agent editing. The 191-token record the card cited was the stale v1 file.
  - Filed as **ft-da85cd55** (P1/high). It is gated on ft-42081f16, which holds the hook files.
- **CARD 13's hook half was reproduced live.** The coordinator's own Write was refused with `task.scope_unavailable / [invalid_worktree] … deferred under load after 2.002s`.
- **The ownership report was empty on every repo with a `.mailmap`,** because `git blame --use-mailmap` is rejected (CARD 21).
- **Verification sweeps launched git at 7.67/s** because `SWEEP_PERIOD` is 3 s, not 11 s (ft-cd18575e).

## 4. Deferred or blocked, and why

| Card | Reason |
|---|---|
| ft-9cfd5574 (CARD 1), ft-9cfd5474 (CARD 2), ft-9cfd5174 (CARD 5), ft-9cfd5374 (CARD 3), ft-9cfa5174 (CARD 11), ft-9cfa5274 (CARD 12), ft-b3e3abe0, ft-93537f05, ft-7ba9b4b2, CARD 13 hook half, ft-da85cd55 | `hooks/mod.rs` and `harness/mod.rs` are held by in-progress incident cards ft-42081f16 and ft-0ac24164. These were never touched |
| ft-cb252563 transcript rotation | **Stopped at its schema gate.** It needs a new nullable column on `transcript_cursors`, which is OD 1. In review, marked NOT IMPLEMENTED |
| ft-df6bf5ea gitpulse_landed | Filed this run (split from CARD 15). Not started |
| ft-9d044374 (CARD 23), ft-9cfd4d74 (CARD 9) | OD 2, 13 and 14 (merge-tree object writes, git floor) |
| ft-723aab52, ft-9d043f74, ft-9d044974, ft-9d00bc74, ft-9d043d74 | OD 1 (ledger schema) and OD 15 |
| ft-d902fcb4 | OD 4 |
| ft-9cfd5074 | OD 6 |
| ft-9cfd5274 | OD 7 |
| ft-9cfd4e74 | OD 9 |
| ft-9d00bd74 | OD 16 |
| ft-9d00ba74 | OD 18 |
| CARD 28 feature half | OD 8 (Codex source) |
| ft-45b8099a | OD 13 |
| ft-8db24db8, ft-369f85a3, ft-40271e71 | Wait on the governor sensor ft-5d8b201f's acceptance |
| ft-5c1313c9 | Was gated on ft-b4eb6688, now merged. Not started this run |
| ft-fdfea4c7 | Waits on ft-2967fd9d (in progress) |
| ft-487108b9 | sidecar.rs is held by ft-329789a9 |
| ft-cd2a98d7, ft-5a4e25c8, ft-cc4be67b | Need a Windows host, real accounts, or a person to quit the installed app |
| ft-c63ec4df | Fable: its ≤5% A/B gate cannot be judged at this host's load |
| ft-8db7edeb, ft-971055ea | DevCouncil-side work |
| ft-d1c290f1, ft-ae3490e7 | Product or design decisions pending |
| ft-541eafaf follow-ups, ft-bdb253cf half 2, ft-ade6e999 watcher half | See §1 |

## 5. Benchmark deltas

Host load was 20 to 350 on 18 cores all run, so wall-clock numbers are noisy. Counter-based evidence is used wherever possible.

| Path | Before | After | Method |
|---|---|---|---|
| Verification sweep, rev-parse rate (ft-cd18575e) | 7.65/s | 0.38/s | 23 fixture cards, 60 s windows × 2, launch counter |
| Ownership report, 2k files (CARD 21) | cold 129 / warm 129 spawns | cold 132 / warm 2 | spawn counter; no wall-clock (load 230-350) |
| XML entity decode (ft-bdb253cf) | 49.1 s @ 1M `&` (slope 2.1) | 3.1 ms @ 1M, 21-130 ms @ 8 MiB (slope ≈ 1) | `rustc -O` copy, min of 5 |
| `ledger_events` newest-5 on 45k rows (CARD 15) | 4.86 s (paging) | 1.06 ms | debug, min of 7 |
| Ledger append (CARD 15 indexes) | 150.0 µs | 160.1 µs (**+6.7%**) | debug, 10k appends, min of 5 rotated; **over 5%, justified** in the schema comment: it buys newest-first by action and by since_ts without a history scan. Five indexes measured +37% and were cut by Fable's ruling |
| Insights snapshot spawns (ft-f815889e) | ~20 | ~20 | unchanged (the fix is a bounded wait, not fewer spawns) |
| `cmd_get_file_blob` without digest (ft-8f398bb6) | 8 MB 9.47 ms, 1 MB 1.22 ms | 8 MB 8.09 ms, 1 MB 1.08 ms | 15 samples, min, single run (not interleaved), load 26 |
| Nested-gitdir downward walk (ft-8f398bb6) | n/a | 8.7 ms on a 10k-entry folder | debug, single run; only on directory delete/move |

## 6. New tasks filed

| Card | Priority | What |
|---|---|---|
| ft-f815889e | P1/high | gitpulse-mcp insights shed at 0.000s (built this run; the diagnosis was corrected) |
| ft-da85cd55 | P1/high | Edit-time hooks drain the shared spawn budget (~15 spawns each); refusals don't name the bucket |
| ft-22bcf053 | P2/low | sweep_teardown_asides concurrent race (built this run) |
| ft-df6bf5ea | P2/medium | gitpulse_landed, split from CARD 15 with 8 break groups and Fable's semantics |
| ft-e2c10b99 | P2/medium | Thread typed errors through git_cli so helper-thread and single-flight deferrals keep their kind (follow-up to ft-5c1313c9) |

Follow-ups recorded on existing cards instead of new ones:
- CARD 21: the load flake it introduced.
- CARD 13: the regression note.
- ft-f815889e: the contract regression note.
- ft-cb252563: the OD 1 analysis.

## 7. Fable's decisions

- **Wave 1:** ft-576fb6dc, ft-c0d321c1, CARD 21, and CARD 13's non-hook half. The non-hook half displaced ft-04c0ab7e because it is the live defect that blinds collision checks.
- **CARD 13 hook reason text:** accept `[deferred_worktree]` (same Deny, rule and severity), conditional on nothing parsing it. The adversarial pass found no parser.
- **Fill order after the first merge:**
  1. bdb253cf
  2. f815889e (ahead of b4eb6688: P1, and an easier rebase)
  3. cd18575e
  4. CARD 15
  5. b4eb6688
  6. 541eafaf
  7. 8f398bb6

  Hold a slot rather than start a collision.
- **Not built this run:** ft-c63ec4df (bench gate unjudgeable at this load), ft-cc4be67b code half (no measurement), ft-b3e3abe0 (shares check-mcp-install.mjs). CARD 28 docs half as one coordinator commit.
- **CARD 15 split:**
  - Land S3-11 and move `gitpulse_landed` to its own card.
  - Do not accept +37% append cost; keep only the `ts` and `action` indexes.
  - `landed` semantics: false only when provable, null plus a reason otherwise; squash before cherry; revert detected by ancestry only; `landed_at` never from HEAD's reflog.
  - Refuse in partial clones until OD 2 is decided.

## 8. Owner decisions still pending

1. **Rebuild and reinstall the binaries.** `bun run mcp:install` plus the app. The installed hook and gitpulse-mcp predate every fix here (the hook still prints `[invalid_worktree]`). `check:schema-parity` stays red on three rows until then.
2. **DevCouncil:** print `extract schema N` in `devmap --version` (ft-576fb6dc AC5). This is a sibling-repo change.
3. **OD 1, ledger schema.** New items this run:
   - a nullable `transcript_cursors` fingerprint column (ft-cb252563);
   - a typed event-time column (CARD 15's event-time filter).
4. **OD 2:** merge-tree object writes. This now also covers `gitpulse_landed`'s content route (ft-df6bf5ea).
5. **New: new-file mode** (ft-8f398bb6 item 2). Files are created 0600; matching umask, typically 0644, widens access.
6. **New: remove `GitWriter::stash_pop` / `cmd_stash_pop`** (ft-adc5dca5). This is a public IPC surface removal.
7. **Branch and worktree cleanup.**
   - Run `bash tmp/build-1017/cleanup-worktrees.sh`. It removes finished lane worktrees and their merged branches, plus three half-deleted ~30 GB directories.
   - `b17/cd18575e-adv` needs `-D`. Its tests are on `main`, but its history has a conflict-marker commit.
   - `b17/9cfa4d74-adv` must be kept for ft-df6bf5ea.
8. **v1.5.0 tag.** RELEASE_REPORT.md names `cc33ddc8` as the ci:local-verified commit. `main` has moved since. Decide whether the tag stays on `cc33ddc8` or a new `ci:local` run qualifies the current tip.
9. **Still open from earlier audits:** OD 3-20 except 11, as listed in AUDIT_REPORT.md.

## 9. Final verification on `main`

Run on `e7fd6e48` (all lanes merged); the log is `tmp/build-1017/final-e7fd6e48.log`. Host load was 64-85.

| Check | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -D warnings` | clean |
| `cargo test --tests --no-fail-fast` | **122 integration targets ok**, including `hermetic_contract` and `supervised_threads`. The lib target had 4922 passed and 6 failed. **All 6 pass when re-run alone**: the catch_up trust-storage pair, a knowledge rename, a commit-graph shared pass, a git_reader time budget, and the entity_collision timing test. These are load flakes; ft-626b25fc owns that family |
| `bun run check` | 0 errors, 0 warnings |
| Full vitest (764 files) | 10,695 passed and **2 failed**. Both were real contract violations from this run's merges: a bare `setInterval` in WavePanel (ft-007a2b4b) and an unscaled limit in the parity adversarial test (ft-576fb6dc). **Fixed in `67809283`**, and both pass |
| `ci:local` | **not run.** It takes about 65 min on this host; the checks above cover its Rust, TS and contract gates but not the browser harnesses on the final tip |
| `dev verify` | **could not run**: there is no DevCouncil state in this repo, as in the last build. This is not a pass |
| Manvi verification | no `work.verify.task` record for any card, so every card is held in **review** |

**Later wave (2026-10-18), on `ef961ae5`:**
- ft-5c1313c9 merged. Another session had meanwhile fast-forwarded `main` with its Graft notification forwarding (`c00431ab`); that tip was health-checked green.
- fmt, clippy and `bun run check` are clean.
- Full vitest: **768 files, 10,782 passed, 0 failed.**
- `cargo test --tests --no-fail-fast`: **122 integration targets ok**. Lib: 4989 passed, 1 failed (the catch_up trust-storage load flake, which passes alone).

**Final `main`:** the commit after this report update. It is many commits ahead of `origin/main`. Nothing was pushed.

**Not verified on the final tip:** the Chrome/WebKit browser harnesses, `ci:local` as a whole, and the installed app. The binaries are not rebuilt (§8.1).

## Process notes

- **Owner rule:** commit before any branch switch or checkout. It was relayed to every lane, written into the lane rules, and saved to memory.
- **Worktree removal from this gated session half-deletes directories.** The MANVI gate protects each worktree's `.git` file. Cleanup is therefore handed to the owner (memory updated).
- **`gitpulse_add_task`'s related check pages 25 ids at a time** and needs the union of every listed id in one call. 41 were accepted (memory corrected).
- **Disk** went from 246 to 111 GiB free mid-run, briefly hit 100% (other sessions plus lane targets), and recovered to 189 GiB.
- **DevMap's own MCP server** failed to connect for most lanes. They used the gitpulse-mcp `devmap_*` tools or `rg -uu`, and said so. Impact answers carried `walk_incomplete` throughout, so they were treated as lower bounds.

---

# Continuation: 2026-10-19 / 20 (owner decisions approved, Waves A and B)

**Trigger.** The owner approved every pending owner decision on 2026-10-19 ("I approve all, choose best"). The chosen options are in `tmp/build-1017/OWNER_DECISIONS_2026-10-19.md`. Two items were not acted on:
- the v1.5.0 tag, because the standing rule is that agents never touch tags;
- the DevCouncil `devmap --version` change, which is a sibling-repo change and was handed over.

The owner then asked for two waves and, on 2026-10-20, to wrap up.

**Process.** It was unchanged:
- an Opus brief;
- a Sonnet implementer working tests-first;
- a separate Sonnet adversarial pass, with every break sent back;
- Opus review;
- Fable rulings on judgment calls;
- a full gate on main after each merge.

Every Wave B merge was a fast-forward of `main`. Nothing was pushed, and no tag was created or moved.

## A1. Merged and moved to review

| Card | What landed | Adversarial breaks fixed |
|---|---|---|
| **ft-adc5dca5** | Removed the dead `GitWriter::stash_pop` and `cmd_stash_pop`. UI pops go through `cmd_stash_action` with the listed OID. | (a pure deletion; the proof it was dead stood in for an adversarial pass) |
| **ft-df6bf5ea + CARD 15** (ft-9cfa4d74) | `gitpulse_landed` follows Fable's semantics: `false` only when provable, otherwise null with a reason. New `engine/object_quarantine.rs` (OD 2): object writes go to a private temp dir with alternates. `confidence` is null when unverified (OD 5). Carry-over is declined and documented (OD 10). | 10: partial-clone promisor spellings; a net-empty cherry-pick; unrelated identical tree; **textconv writing objects and a notes ref**; merge revert target; deleted-file squash; revert-chain limit. Plus a stale-quarantine sweep and the tag/branch name case. |
| **ft-8f398bb6 item 2** | New user files and folders follow the process umask, matching `git checkout`. Overwrites keep their mode. Temp files stay 0600. | Exec bits under umask 026/006/011. Umask tests now run in child processes so the shared test process is never touched. |
| **OD 1** (host ft-9d043e74) | One additive ledger bump, WRITER_VERSION 1→2: `occurred_at`, `reflog_ordinal`, the `transcript_cursors.fingerprint` column, the `reflog_watermarks` and `catch_up_state` tables, and a partial UNIQUE reflog identity index. The migration is **non-destructive**: only duplicates get ordinals. | 6: second-opener busy timeout; same-pass duplicates; legacy import collapse; index trusted by name; refusal before WAL; live generation-1 writer (documented). |
| **ft-723aab52** | Ledger lineage UUID plus a checkout **site** (inode, plus the APFS volume UUID on macOS) inside the existing anchor record. There is no new table and no write into `.git`. A sibling clone and a history-merging repo are refused. **`cmd_adopt_ledger`** heals an inode-changing move: trust check, OS confirmation, and an audit row in the same transaction. | 6: APFS `st_dev` renumbering; an old-binary strip resetting lineage; uuid format; occupancy recheck inside the transaction; manual-clock leak. |
| **CARD 27** (ft-9d043f74) + **ft-cb252563** | Reflog replay by watermark, with `truncated` and pending/unreached counts (partial, not failed). `before_ref` is filled. Failed records are counted. Family worktree HEADs are read in a rotating window of 16, plus the default branch once. Rows and watermark go in one transaction. Transcript cursors get a v3 fingerprint (head and tail windows, with a len/mtime fast path); a prefixed file resumes after its old tail. | 4 fixed: a running generation-1 double-record; future-dated entries after a crash; worktrees past the bound; false-positive rotation. 2 kept as **known limits**: a backdated twin outside the window, and renumbering after `git reflog delete`. |
| **CARD 18 part 2** (ft-9cfa4874) | A user-clicked, gated `git commit-graph write --reachable --changed-paths` (OD 3; the bench showed a material win). It previews the exact argv, makes no config writes, runs one write per common dir, and refuses git older than 2.27 and partial clones. **It never removes a lock.** | 4: **the sweep deleted a foreign lock**; a partial clone lazy-fetching; a cancel after publish reported as "nothing written"; three UI races. |
| **CARD 9** (ft-9cfd4d74) | `merge_tree_candidate`, Clean / Conflict{paths} / Unknown, never a false clean. `is_branch_merged_by_tree` is tri-state. `WorktreeSummary` gains head/ahead/behind for detached worktrees, cached by (repo, head, base, shallow boundary), with zero spawns in steady state. New read tool `gitpulse_merge_conflicts`. OD 13: the git floor stays 2.23, with per-feature fallback (2.45.1 is a clone floor only). | 3: stale counts in a shallow clone; `ok:true` when every pair was unknown; non-UTF-8 paths collapsing. |
| (found by the gate) | The `ref_cache` coalescing test flake was a **test race**, not a bug: the waiter thread became the leader. Fixed with a test-only follower count. | n/a |

## A2. Fable's rulings this continuation

- **Wave B order.** `ledger/mod.rs` had a single holder, so CARD 27 and ft-cb252563 were one lane. ft-723aab52 was confined to `anchor.rs`. CARD 29, CARD 28, CARD 23 and CARD 31 were deferred.
- **ft-723aab52.** No WRITER_VERSION 3 and no file in `.git`. The inode-site veto was accepted only together with an explicit adopt action that writes an audit row.
- **CARD 18.** `--changed-paths` accepted, on three conditions: git 2.27 or later, the preview says Bloom data persists (quoted from `man git-commit-graph`), and no speedup figures in the UI.
- **CARD 9.**
  - Three test-only lines in `hooks/mod.rs` were accepted as a narrow mechanical touch. ft-42081f16 holds that file, so a note was left on it.
  - The narrowed "≤64 rev-list" invariant was rejected. A steady-state snapshot must spawn nothing.

## A3. Benchmarks

| Bench | Before → after | Verdict |
|---|---|---|
| Ledger append, branch vs main (min of 7, load 59-98) | session 0.977×, reflog 0.917× | no regression |
| OD 1 migration, 1M rows | 9.4 s debug, about 5.4 s optimized-SQL replica | bounded; a second opener waits up to 120 s |
| catch_up, 5k-entry reflog, steady state | 18.2 → 18.4 ms | within noise |
| catch_up, 2,000 unchanged transcripts | **1.29×** (about 2 µs per file, no I/O) | **over 5%, accepted with justification:** it pays for rewrite detection; follow-up below |
| merge_tree_candidate, 2,000-file repo | 33.6 ms per pair (raw merge-tree 9.5 ms) | new; recorded in `docs/PERFORMANCE.md` |

## A4. Follow-ups found, not fixed (recorded here and on the cards)

- **Transcript fingerprint cost (1.29×).** Cutting it needs a cursor-map shape change.
- **`run_observed`** reports `cancelled` for a child that exited 0. It is handled in the commit-graph writer; `storage/hygiene/mod.rs:606` and `tool_install/mod.rs:1047` have the same race.
- **Duplicate partial-clone check.** The commit-graph detector has a second partial-clone check beside `object_quarantine::is_partial_clone`. Converge the two.
- **Adopt button.** It is offered on any history error. Narrow it to a typed `repository_mismatch` kind (needs `ledger/mod.rs`).
- **Default-branch rows.** A main-worktree commit's `refs/heads/<default>` row is refused as a duplicate of its HEAD row, so a query by `ref == refs/heads/main` misses it.
- **`catch_up_state`** is written by nothing yet. That is CARD 29, deferred.
- **`analyzer::deps::adv_hardlink_to_an_outside_file_is_refused`** fails under `--ignored`. It is outside this run's scope.
- **Ledger append bench.** Needs a re-run on a quiet machine for a figure under 5%.

## A5. Still deferred

- CARD 29 (ledger freshness over MCP; it is next now that CARD 27 has merged).
- CARD 28 (Codex ingest, OD 8).
- CARD 23 (lane integration in the object DB, OD 14).
- CARD 31 interim (OD 16).
- CARD 4, 6, 8, 30, 32, 33, 34; ft-45b8099a; ft-d902fcb4.

Cards needing files held by in-progress ft-42081f16, ft-0ac24164, ft-329789a9, ft-2967fd9d or ft-626b25fc stay blocked.

## A6. Owner actions

1. **Rebuild GitPulse.app and run `bun run mcp:install` together, and restart a running `gitpulsed`.** The ledger is now WRITER_VERSION 2. The first new binary to open it migrates it, and from then on generation-1 binaries refuse to write. A running generation-1 process is not stopped by the stamp and must be restarted.
2. **Reconnect the `gitpulse` MCP server (`/mcp`).** This session's server process is old, so Insights answered "deferred under load" all run.
3. **Run `bash tmp/build-1017/cleanup-wave-b.sh`.** It removes this wave's six finished worktrees and their merged branches. Wave A's were cleaned by `cleanup-wave-a.sh`. The epic-volhard, graft-notify and vendor-devmap worktrees have live sessions and were kept. `seed` was kept as a seed source.
4. **Remove the gate worktree:** `git worktree remove tmp/final-gate`. It is a clean detached checkout used for the final gate.
5. **Another session's uncommitted dependency bump** (`package.json`, `bun.lock`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.cargo/`) sits in the main checkout. It was not touched or committed, and it needs its owner's decision.

## A7. Final verification

This ran on **`d861f0e5`**, with every Wave A and Wave B lane merged. It used a clean detached worktree, `tmp/final-gate`, so the main checkout's foreign uncommitted dependency bump was excluded. Host load was 7-12 at the end. Logs: `tmp/build-1017/final-gate*.log`.

| Check | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -D warnings` | clean |
| `GITPULSE_TEST_WRITE_BUDGET=0 cargo test --tests --no-fail-fast` | **122 targets ok**, including `hermetic_contract` and `supervised_threads`. The lib target had 5282 passed and 2 failed: `assemble_tests::slow_command_is_visible…` and `git_reader::a_second_ownership_call…spawns_no_blame`. **Both pass when re-run alone.** They are known load flakes owned by ft-626b25fc and CARD 21. |
| `bun run check` | 0 errors, 0 warnings |
| Full vitest | **772 files, 10,829 passed**, 0 failed |
| Browser harnesses | not re-run on the final tip. Lane 3 ran `test:browser:all` (60/60) on its merged tree. |
| `ci:local`, `dev verify`, Manvi verification | not run, or no record exists, as in §9. Every card is held in **review**. |
| Installed binaries | `mcp:install` ran on 2026-10-19 before OD 1. The app and binaries need the rebuild in A6.1. |

Each merge also had its own full gate on `main` (`tmp/build-1017/gate-*.log`). Every failure was a load flake that passed when re-run alone, except the `ref_cache` coalescing race, which was fixed.
