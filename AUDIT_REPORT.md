# GitPulse deep audit 2: Scion and herdr integration (2026-10-13)

This is a read-only audit of `main` @ `8026dd7e`. Every verified finding is now on the GitPulse task board.

- No source, config or dependency file was changed.
- The only writes were GitPulse board writes (`gitpulse_add_task`, with overwrite for folds) and this file.
- **No build work has started.** It waits for the owner's go-ahead.

The ideas behind this audit come from two sources:
- [IDEAS-herdr-scion-2026-10-13.md](IDEAS-herdr-scion-2026-10-13.md): Scion (*Redesigning source control for agent scale*) and herdr (a terminal runtime for coding agents).
- Round 2 of the same file: Claude Managed Agents and Perplexity Projects.

## Method

1. **Map.**
   - `gitpulse_insights`: 3 worktrees, 6 live Claude sessions all sharing the main checkout, 0 contended files.
   - `devmap status`, then DevMap dead, clones, deps and impact.
   - Every open card on the board (133 at baseline) was read as the dedupe baseline.
2. **Audit.** Six Sonnet auditors, run at most four at a time, each took one slice:
   - S1: attention and hooks
   - S2: PTY and resume
   - S3: coordination and MCP
   - S4: history engine
   - S5: merge flow
   - S6: ledger

   Every finding came with `file:line` evidence and a failure scenario.
3. **Verify.** Opus re-read every cited line, its callers and any upstream guards, then set the final severity and priority. The S5 verifier also reproduced the merge-conflict behaviour with real git, using throwaway repositories under the gitignored `tmp/`. Opus also checked, criterion by criterion, each claim that an existing card was "already done".
4. **Prioritize.** Fable grouped the findings into one card per concern, decided fold-vs-new, set priorities and severities, and planned the build waves. Round-2 ideas were placed the same way.
5. **File.** The board was re-pulled right before filing. NEW cards were filed by Sonnet agents with stable `task_id`s (`audit-2026-10-13-card-NN`). Folds used `get_task` immediately before an overwrite that sent every field. Opus then checked the results:
   - **Folds.** 16 folds were checked programmatically against each agent's own before and after `get_task` reads. The 17th (ft-c7bf0c93) was the round-trip test, checked by a direct before and after read. For every fold:
     - the description still starts with the original byte for byte
     - logs are identical
     - old criteria are kept, either verbatim or as tag + original text
     - labels are a superset
     - status is unchanged
     - priority and severity were not lowered

     All 17 passed.
   - **NEW cards.** A script (`tmp/audit-2026-10-13/check-new.mjs`) compared the 34 created `gitpulse_add_task` payloads, taken from the filers' transcripts, against Fable's plan blocks:
     - title, including the round-2 retitles
     - the description starts with the plan text
     - criteria count
     - priority, severity and kind
     - the audit and idea-round2 labels
     - status

     All 34 passed.
   - **The final board.** It holds 176 cards. Every new item_id is on it, and there are no other new cards and no duplicate titles. The in-progress and user-written cards have the same revision as at baseline.

**Index caveats.** Where the tools below could not answer, the gap was recorded and not reported as clean.
- DevMap was degraded throughout: generation 4133, `analyzer_freshness:false`, 98 files not fully parsed, about 37% call resolution, and every answer `walk_incomplete`. Its empty answers were treated as "unknown". Findings were confirmed with `rg -uu` and source reads.
- `gitpulse_insights` sometimes returned `ok:false` on a facet under spawn-budget load.

## Results at a glance

| | Count |
|---|---|
| Findings audited, then Opus-verified | 79 |
| Rejected outright | 0. Several premises or sub-claims were dropped; see "Rejected and dropped" below. |
| Final severity | 6 high, 38 medium, 35 low |
| New audit cards | 29 (CARD 1–29) |
| New round-2 idea cards | 5 (CARD 30–34) |
| Existing cards folded into | 13 (F1–F13) |
| Annotation-only updates to existing cards | 4 |
| Board size | 142 open before filing → 176 after |

## Severity × dimension (79 findings, after verification)

| Dimension | high | medium | low | total |
|---|---|---|---|---|
| architecture | 0 | 10 | 9 | 19 |
| typing | 0 | 0 | 1 | 1 |
| validation at trust boundaries | 0 | 1 | 3 | 4 |
| async error boundaries | 0 | 5 | 3 | 8 |
| deterministic testability | 0 | 0 | 1 | 1 |
| performance | 1 | 5 | 3 | 9 |
| correctness | 5 | 16 | 9 | 30 |
| security | 0 | 0 | 3 | 3 |
| cross-platform | 0 | 1 | 3 | 4 |
| **total** | **6** | **38** | **35** | **79** |

The six high findings are S3-01, S4-02, S5-01, S6-01, S6-03 and S6-13.

## Top 10 risks

1. **A conflicting wave merge parks your main checkout mid-merge, and the wave keeps launching.** Verified with real git:
   - A plain merge leaves `MERGE_HEAD` plus `UU` files.
   - A squash merge leaves `SQUASH_MSG`, and `merge --abort` fails; only `reset --merge` restores the checkout.

   This violates ft-e7153292's own criterion. Fixed by CARD 22 now; CARD 23 then moves integration out of the checkout.
2. **The pre-edit collision guard is silent on the first edit of a file another worktree already holds dirty.** Headless agents are then blocked one edit late. CARD 1.
3. **Each PGP-signed commit costs about 10 s in Pulse and in commit details** (`%G?` verifies every signature). Around 1,800 signed commits in the window would hit the 90 s timeout. SSH-signed commits read as unsigned. CARD 16.
4. **Every ledger row is stamped with ingest time, not event time.** "Who changed this, and when" is wrong after any offline period, and task ids are resolved at replay time. CARD 24.
5. **Work in nested `.claude/worktrees` is recorded as main-checkout activity.** It gets two rows, or a main-only row after the worktree is removed. This is pervasive for `isolation: worktree` agents. CARD 26.
6. **One oversized tool result, or any resource document over 4 MiB, ends the agent's whole MCP session.** The server exits 0 and sends no error response. CARD 10.
7. **A load-shed `git worktree list` inside the hook becomes a hard Deny with a policy-sounding reason,** exactly when many agents are running. CARD 13 must land before ft-369f85a3 adds more shedding.
8. **Reloading the webview and then clicking "Go to" mints a fake conversation id that shadows the real one.** The incident Resume would then run `claude --resume <random>`. CARD 7, a sibling of in-progress ft-0ac24164.
9. **A late async PostToolUse erases "Finished", and the socket acknowledges reports it dropped.** Hook state is silently wrong under load. CARD 2.
10. **Committed lane work is invisible to every coordination surface, so nothing predicts a merge conflict before it lands.** CARD 9 + CARD 23, both behind owner decision 2 and git 2.38.

## New cards (34)

Each card is filed as `audit-2026-10-13-card-NN`. Every one is labelled `audit-2026-10-13`; CARD 30–34, plus CARD 11 and CARD 23, also carry `idea-round2`.

| Card | item_id | P | Severity | Wave | Findings | Concern |
|---|---|---|---|---|---|---|
| 1 | ft-9cfd557403d57dca25e3cfdb97144689 | 1 | high | 2 | S3-01, S3-08, S3-09 | Pre-edit collision check misses a file dirty in another worktree: path-scoped query, `-u` status, focus before cap |
| 2 | ft-9cfd547403d57c1725e3ccdb97144170 | 2 | medium | 3 | S1-01, S1-08, S1-12 | Hook-report protocol honesty: optional seq, ack only what was accepted, refusals carry reasons |
| 3 | ft-9cfd537403d57a6425e3cddb97144323 | 2 | medium | 4 | S1-04, S1-05 | Attention standing lifecycle: errors stand forever, replay after reload, inflated tray count |
| 4 | ft-9cfd527403d578b125e3d2db97144ba2 | 3 | low | 5 | S1-02, S1-03 | Attention semantics (owner decision): working, idle-is-not-waiting, mark seen |
| 5 | ft-9cfd517403d576fe25e3d3db97144d55 | 2 | medium | 6 | S1-07, S1-15 | Notify hook coverage for every GitPulse-launched claude; quiet exit before reading stdin |
| 6 | ft-9cfd507403d5754b25e3d0db9714483c | 3 | low | 9 | S1-09, S1-13, S1-14 | Notify boundary: `Host::knows`, vocabulary guards, Cf/bidi sanitizer |
| 7 | ft-9cfd4f7403d5739825e3d1db971449ef | 1 | medium | 2 | S2-01, S2-02, S2-04, S2-06, S2-08 | Hosted conversation records: no fake ids, no forgetting, honest Resume count |
| 8 | ft-9cfd4e7403d571e525e3d6db9714526e | 2 | medium | 3 | S2-03, S2-05, S2-07 | Reopen agents after quit, crash or reboot; one Rust owner for resume argv |
| 9 | ft-9cfd4d7403d5703225e3d7db97145421 | 2 | medium | 4 | S3-02 | Committed-but-unmerged lane work: head/ahead/behind, plus a quarantined `merge-tree` conflict engine |
| 10 | ft-9cfa507403d33f7425e754db97175e5f | 2 | medium | 1 | S3-03, S6-14 | Oversized tool result or slow stdout ends the MCP session; budget the results |
| 11 | ft-9cfa517403d3412725e753db97175cac | 2 | medium | 7 | S3-05, S3-06 + idea 10 | File claims, declared and observed: attribution and collision checks before anything is dirty |
| 12 | ft-9cfa527403d342da25e756db971761c5 | 2 | medium | 8 | S3-04 | `gitpulse_sessions` read tool: identity, run_id, sourced state |
| 13 | ft-9cfa537403d3448d25e755db97176012 | 2 | medium | 5 | S3-07 | Load-shed worktree listing becomes a hard Deny: classify the deferral, say "did NOT run" |
| 14 | ft-9cfa4c7403d338a825e750db97175793 | 2 | medium | 3 | S3-10 | Entity collision verdict joins on bare names and ignores addition-only hunks |
| 15 | ft-9cfa4d7403d33a5b25e74fdb971755e0 | 2 | medium | 6 | S3-11, S6-05, S6-06, S6-07 | Provenance agents can trust: `landed_on_base`, newest-first indexed ledger queries, `gitpulse_landed` |
| 16 | ft-9cfa4e7403d33c0e25e752db97175af9 | 1 | high | 1 | S4-02, S4-01 | Pulse/DORA walks: `%G?` costs 10 s per PGP commit; DORA prunes at the first old committer date |
| 17 | ft-9cfa4f7403d33dc125e751db97175946 | 2 | medium | 4 | S4-03, S4-04 | Graph and stack loads spawn redundant topo walks |
| 18 | ft-9cfa487403d331dc25e75cdb97176bf7 | 3 | low | 7 | S4-05 | Commit-graph awareness: read-only detector, Health hint, benchmark; a gated write is an owner decision |
| 19 | ft-9cfa497403d3338f25e75bdb97176a44 | 2 | medium | 6 | S4-06 | Branch churn keyed by base commit: key by (merge_base, tip, inputs) |
| 20 | ft-9d04427403db7c2f25dce2db970e4824 | 3 | low | 3 | S4-10, S4-12, S5-10 | One owner for the default base and divergence (no hard-coded main/master) |
| 21 | ft-9d04417403db7a7c25dce3db970e49d7 | 2 | medium | 5 | S4-09 | Ownership report blames the first 128 files by path (0 source files here) |
| 22 | ft-9d04447403db7f9525dce4db970e4b8a | 1 | high | 1 | S5-01, S5-04, S5-05, S5-06, S5-11, S5-13 + idea 7 (contract) | `merge_and_teardown` contract: roll back a conflict, never report a landed merge as "Not merged", bind tip and base |
| 23 | ft-9d04437403db7de225dce5db970e4d3d | 2 | medium | 6 | S5-02 + idea 7 | Integrate lanes in the object database: `merge-tree --write-tree` → `commit-tree` → CAS `update-ref` → ff-only only when clean |
| 24 | ft-9d043e7403db756325dcdedb970e4158 | 1 | high | 2 | S6-01, S6-04 | Ledger event time (`occurred_at`) and `binding_at(worktree, occurred_at)` |
| 25 | ft-9d043d7403db73b025dcdfdb970e430b | 3 | medium | 10 | S6-02 | Commit-to-session link: delete or wire the dead writer, infer the link with stated quality |
| 26 | ft-9d04407403db78c925dce0db970e44be | 1 | high | 1 | S6-03 | Nested worktrees attributed to main: longest family path wins |
| 27 | ft-9d043f7403db771625dce1db970e4671 | 2 | medium | 7 | S6-08, S6-09, S6-10 | Reflog replay semantics: multiplicity, 200-entry window, `before_ref`, two writers |
| 28 | ft-9d044a7403db89c725dcdadb970e3a8c | 2 | medium | 11 | S6-12 | Only Claude transcripts are ingested, though the docs claim Codex too: fix the claim, then a source trait |
| 29 | ft-9d04497403db881425dcdbdb970e3c3f | 2 | high | 9 | S6-13, S6-11 | Ledger over MCP has no freshness, coverage or loss signal |
| 30 | ft-9d00bc7403d862a625e068db971161ad | 2 | medium | 12 | idea 8 | Agent git operation log with undo (`refs/gitpulse/ops/<n>`, CAS restore) |
| 31 | ft-9d00bd7403d8645925e067db97115ffa | 2 | medium | 12 | idea 9 | "Previous attempts" section in the next attempt's brief |
| 32 | ft-9d00ba7403d85f4025e066db97115e47 | 3 | low | 12 | idea 11 | Wave budgets: wall-clock now, token cap later; the wave pauses at the cap |
| 33 | ft-9d00bb7403d860f325e065db97115c94 | 3 | low | 13 | idea 12 | Fork a lane: worktree at HEAD, `stash create`/`apply`, forked session |
| 34 | ft-9d00c07403d8697225e064db97115ae1 | 3 | low | 13 | idea 13 | Optional repository digest, in Health; never posted outside the app |

Wave-1 cards are in **ready**: CARD 10, 16, 22 and 26. All others are in **backlog**.

## Folded into existing cards (13)

Every fold kept the card's title, column, logs and existing criteria. It appended an "Audit 2026-10-13 additions" section and `[S#-##]` criteria. Priority and severity were never lowered.

| Fold | Card | Findings added | Wave | Change of note |
|---|---|---|---|---|
| F1 | ft-bdb253cfb494e1ebb855392c07ed1912 | S1-10, S1-11 | 10 | Accept-loop survival. AC "total deadline" is tagged MET and AC "second report not delayed" NOT MET. Stale problem text and the struck `ingest/mod.rs` are noted as corrections. Cross-reference to ft-79394019 (accept-poll cadence, filed after the baseline). |
| F2 | ft-038e6ba5a137e7747f52a6a026ba775b | S1-06 | 10 | Only `\r`, Esc, option digits or a hook Resolve count as an answer. The "wired" criterion is tagged MET AS WRITTEN, with a correction. |
| F3 | ft-9f4ba7105b9c98135409023f76f0999a | S2-09 | 5 | Quit guard: a typed unknown reason. AC2 is tagged MET on macOS/Linux, NOT MET on Windows. |
| F4 | ft-1c36bec1d71ba700f65fbfbb12015e71 | S2-10, S2-11 | 11 | PTY-host coupling inventory, plus an interim "detached output was not kept" marker |
| F5 | ft-c7bf0c93baff13d4bba8d0383db190f7 | S3-12 | 10 | MCP contract: derived `read_only`/`gitWrites`, a recursive `.git` read-only test. The read-only criterion is tagged NOT MET. Severity none → low. |
| F6 | ft-96175ecc96682ebec0ae69c52319db97 | S4-07 | 2 | deadbranch marks "upstream gone" as merged and pre-selects it for deletion |
| F7 | ft-fa9dbbe10ff652a1174fd7c0dd6c1e4c | S4-08 | 8 | An oid-only details cache must key on the diff-input fingerprint and must not freeze `%G?` |
| F8 | ft-2b2f3e90cda1e50df7029a46ec72944a | S4-11 | 9 | Shallow clones: counts look exact |
| F9 | ft-8d992fb6c1cc83b65df94c769dcfed3d | S4-13 | 7 | One `git_home()` resolver on Windows |
| F10 | ft-984a42c46d598814956047acd0f5c8c3 | S6-15 | 8 | Windows: a `share_mode(0)` lock reads as a version mismatch |
| F11 | ft-007a2b4bf613b5c24e8cb6215150c98b | S5-03, S5-08, S5-09, S5-12 | 4 | Vacuous re-verify (base == head), serial verify. C1 is tagged PARTIALLY MET and C2 ADDRESSED IN SOURCE. |
| F12 | ft-d902fcb4b6513a279d9356fa1f72d5a0 | S5-07 | 8 | No free-disk admission or byte cap for lanes. Disk as a deny fact is owner decision 4. |
| F13 | ft-c0d9313fdcd52966cb834c1c1c85b807 | bench rows | continuous | Benchmark rows for this audit's performance findings. Each row lands with its fix card. |

## Verified-stale and "already done" claims

| Card | Verdict on main 8026dd7e | Written to the card |
|---|---|---|
| ft-bdb253cf | Criterion 1 (total deadline) MET: `bridge.rs:263` DeadlineStream, commit c4155156. Criterion 2 NOT MET: accept and handle are serial, and the 2 s ceiling exceeds the hook's 1.5 s budget. The problem text is stale. | yes (F1) |
| ft-9f4ba710 | AC2 MET on macOS/Linux (`quitGuard.ts:48-73`). NOT MET on Windows: `terminal/mod.rs:1304-1307` returns a default context. | yes (F3) |
| ft-007a2b4b | C1 PARTIALLY MET: do not close. The fallback exists, but the re-verify is vacuous and there is no two-member real-worktree test. C2 is addressed in source with residuals and has not been live-verified. | yes (F11) |
| ft-6060bbd7 | All six criteria re-verified MET. The B-agents-02 criteria remain open. | yes (note) |
| ft-038e6ba5 | "reportAttentionAnswered wired" is MET AS WRITTEN, but "typed" means any byte, so arrows and Esc answer a permission prompt. | yes (F2) |
| ft-c7bf0c93 | "No read-only claim" is NOT MET: `McpInfo.read_only` is the literal `true` (`insights/mod.rs:2178`). | yes (F5) |
| ft-143d3a77 | Premise corrected: ingest reads only `~/.claude/projects`; no Codex reader exists. `CHANGELOG.md:143` makes the same false claim. | yes (note) |
| ft-e7153292 | "A merge conflict stops that task" is VIOLATED (S5-01, real git). | yes (tag) |
| ft-45b8099a | Needs rows for `--since-as-filter` (git 2.37) and `merge-tree --write-tree` (git 2.38) against MIN_GIT_VERSION 2.23. | yes (note) |
| ft-38ec498d | E-integrations-09 confirmed by S2-07: three Claude-home resolvers. Criterion unchanged. | report only |
| ft-d37c7225 | Criterion 3 stays MET. The 13-entry object-only allow-list still hides the insights, provenance and task_view schemas. | report only |
| ft-9fa261d9 | Truncation criterion unchanged. Selection, memo and mailmap belong to CARD 21. | report only |

Nothing was moved to done. Done needs Manvi verification.

## Build order (Fable's waves)

Each wave holds at most 4 cards, and no two cards in a wave share a planned file. The bottleneck files are, in order:
- `engine/git_reader.rs` (at most one card per wave)
- `hooks/mod.rs` (all of its cards come after in-progress ft-42081f16)
- `mcp/mod.rs`
- `alerts/bridge.rs`
- `ingest/mod.rs`
- `engine/worktree.rs`

| Wave | Cards | Gate |
|---|---|---|
| 1 | CARD 22 (merge-teardown contract), CARD 16 (Pulse/DORA walks), CARD 26 (nested worktrees), CARD 10 (bounded MCP results) | Touches no file held by an in-progress card. Not blocked by any owner decision. |
| 2 | CARD 1 (collision path query), CARD 7 (hosted records), CARD 24 (event time; detail_json interim only), F6 (deadbranch) | After ft-42081f16 and ft-0ac24164 land |
| 3 | CARD 2 (report protocol), CARD 20 (default-base owner), CARD 8 (reopen agents), CARD 14 (entity verdict) | CARD 8 after CARD 7 |
| 4 | CARD 9 (merge-tree engine; decision 2), CARD 3 (standing lifecycle), CARD 17 (graph walks), F11 (wave runner) | F11 after CARD 22. CARD 3 after ft-5d8b201f. |
| 5 | CARD 13 (deferral), CARD 21 (ownership), CARD 4 (attention semantics; decision 7), F3 (quit guard) | CARD 13 before ft-369f85a3 |
| 6 | CARD 5 (notify coverage), CARD 15 (provenance; decisions 5 and 10), CARD 19 (churn key), CARD 23 (object-database integration; decisions 2, 13, 14) | CARD 23 after 9, 22 and F11. CARD 5 after 2. |
| 7 | CARD 11 (file claims), CARD 27 (reflog replay), CARD 18 (commit-graph spike), F9 (git_home) | CARD 11 after ft-7ba9b4b2 and CARDs 1, 13 |
| 8 | CARD 12 (`gitpulse_sessions`), F7 (cache inputs), F12 (disk admission; decision 4), F10 (Windows daemon lock) | CARD 12 after 11 |
| 9 | CARD 29 (ledger freshness), CARD 6 (notify boundary; decision 6), F8 (shallow) | |
| 10 | CARD 25 (commit↔session), F1 (accept loop), F5 (MCP contract), F2 (keystroke answers) | CARD 25 after 24 and 27 |
| 11 | CARD 28 (Codex source; decision 8), F4 (PTY host) | |
| 12 | CARD 30 (op log), CARD 31 (previous attempts), CARD 32 (wave budgets) | After CARDs 24/25, 23/F11, and 28 |
| 13 | CARD 33 (fork a lane), CARD 34 (digest) | CARD 33 after 30. CARD 34 after 9, 12, 18 and 29. |
| — | F13 (bench rows) | Continuous |

## Rejected and dropped

No finding was rejected outright. These premises, sub-claims and fixes were dropped or corrected by Opus or Fable:

| Item | Outcome | Reason |
|---|---|---|
| S5-06 premise: "lanes can be cut from a non-main checkout" | Rejected; residual kept at low/P3 | `checkoutCandidates` (`taskHandoff.ts:180-208`) and `openMembership.ts:34-43` only admit the main checkout. The residual is that the main checkout's branch can be switched between launch and merge, so CARD 22 records `base_oid`. |
| S6-13 "nothing launches gitpulsed" | Dropped | Running gitpulsed by hand is documented operation (`wiki/MCP and Agents.md:118-121`, `wiki/Architecture.md:128-132`). |
| S4-04 fix (a) | Dropped | It breaks the aligned-chunk reuse pinned by the test at `git_reader.rs:9065`. A one-row has-more probe is used instead. |
| S6-11 "skip forget" criterion | Dropped | A depth-bounded `complete:true` is intentional (`:307-308`) and safe for forget. The honesty gap (excluded files go uncounted) is kept in CARD 29. |
| S4-01 "cherry-pick triggers it" | Corrected | The committer date of a cherry-pick is now. The real triggers are `--committer-date-is-author-date`, imported or rewritten history, and skewed clocks. |
| S3-03 "about 5 KiB per ledger row" | Corrected | Measured average is about 100 B and maximum 16 KB, so the worst 1,000-row page is about 3.6 MB. Opus found a worse case instead: any resource over 4 MiB always exceeds the frame. |
| S1-13 "deleting SessionEnd goes uncaught" | Corrected | `scripts/plugin-contract.test.ts:112-123` catches it. Retargeting a Notification matcher or argument is what goes uncaught. |
| S2-09 "errors reach the quit dialog" | Corrected | They reach `diagnostics.warn` only (`App.svelte:370-372`). |
| S6-07 verification carry-over through `notes.rewriteRef` | Declined (recommended), owner decision 10 | It is a repository config write, which the hygiene posture refuses. |
| Downgrades | Accepted as final | S3-02, S3-03, S4-01, S5-02, S5-04 and S5-07 went from high to medium. The full list is in `tmp/audit-2026-10-13/verify-S*.md`. |

## Decisions made without asking

- **Preserved the previous audit report.** `AUDIT_REPORT.md` (the 2026-10-12 audit, untracked) is kept unchanged under "Previous audit (2026-10-12)" below. A backup is in `tmp/audit-2026-10-13/AUDIT_REPORT-2026-10-12.backup.md`.
- **Audit workspace.** Used the gitignored `tmp/audit-2026-10-13/`, because the MANVI gate refuses writes outside the repository, including the scratchpad. Subagents' write attempts were refused, so I saved their findings and verdicts myself.
- **No fold into in-progress cards** (ft-0ac24164, ft-42081f16, ft-f8a297ff, ft-329789a9, ft-7e812b3f, ft-2967fd9d) or user-written cards. Their findings went onto sibling cards (CARD 3, 5, 7, 8, 13) that are sequenced after them.
- **Accepted every Opus downgrade.** Fable reported no disagreements, so no stop condition applied.
- **Related-check handling.** `gitpulse_add_task`'s related check matches on shared words and listed 6 to 57 ids per card. The first filing pass stopped on every card. I changed the rule to the brief's own instruction:
  - read every listed card;
  - fold only if that card's criteria already require the same fix for the same finding;
  - otherwise retry with `reviewed_related`.

  Fable had already made the fold-vs-new decisions against the full baseline. No card met the fold condition. Each filer's report records why each listed id is separate: `tmp/audit-2026-10-13/filing-report-A.md` (CARD 1–10), `-B.md` (11–21) and `-C.md` (22–34).
- **Status.** Wave-1 cards are `ready` and everything else is `backlog`.
- **Annotation-only notes not written to three cards.** The notes for ft-38ec498d, ft-d37c7225 and ft-9fa261d9 are pure cross-references with no change to any criterion. They are recorded here rather than sent through an overwrite with no revision check, which would mean resending up to 10 KB of text per card.
- **CARD 22 and ft-e460b9c2.** ft-e460b9c2's teardown tip re-check criterion overlaps S5-05 and S5-11. CARD 22 keeps both findings and says "land together or close one against the other".
- **Round-2 ideas filed as cards.** You asked for the round-2 ideas to be filed as cards. Items 7 and 10 were folded into CARD 23/22 and CARD 11. Items 8, 9, 11, 12 and 13 became CARD 30–34. Their evidence lines were checked by the Opus coordinator, not by a separate verifier.
- **Path corrections recorded on the cards:**
  - F2 `sessionAlertsStore.ts` is at `src/lib/stores/`.
  - F6 cites `engine/ops.rs`, which does not exist. The plan probably means `src-tauri/src/ops.rs`; this is unconfirmed.
  - CARD 21's `PulseView.svelte` is at `src/lib/components/pulse/`.
  - F12's "sysload" is `src-tauri/src/sysload.rs`.

## Owner decisions needed before building

| # | Decision | Recommendation | Blocks |
|---|---|---|---|
| 1 | Ledger schema bump (ft-143d3a77 says "no schema migration"): a typed `occurred_at` column, a reflog watermark table, a UNIQUE reflog index (dedupe migration first), and a persisted CatchUp table | Interims now (detail_json and computed). Do the four changes as one scheduled `WRITER_VERSION` bump. Confirm under the cross-app rule that no DevCouncil, Manvi or Jarvis reader exists. | CARD 24 (typed column), 27, 29 (persisted), 25 (downstream), 30 |
| 2 | `merge-tree --write-tree` writes objects, but MCP promises it "never mutates git state" | A quarantined `GIT_OBJECT_DIRECTORY` with alternates, deleted afterwards. A test hashes `.git/objects`. readOnlyHint stays true. | CARD 9 (MCP side), and CARD 23 indirectly |
| 3 | A gated `git commit-graph write` (ft-7450105de says "never write repo config") | Detector and benchmark first. Write only if the benchmark shows a material win, and then only as a user-clicked, gated action. | CARD 18 part 2 |
| 4 | Disk as a deny fact (ft-d902fcb4 says "deny only on memory pressure") | Deny lane creation only | F12 |
| 5 | What provenance `confidence` means. Two tests contradict each other. | Separate fields: keep distance/is_fresh as measured, add verified/on_base/landed_on_base, and make confidence null when unverified | CARD 15 |
| 6 | S1-09 security-impact statement: `Host::knows(key)` plus per-session limits. The same-uid boundary does not widen. | Approve as P3 hardening | CARD 6 (S1-09 only) |
| 7 | Attention UX: does an idle prompt count as "waiting on you", is Working shown, does attending mark seen | Idle does not count, Working is shown, attending marks seen | CARD 4 |
| 8 | Codex transcript ingest | Build it after the docs fix ships | CARD 28 (feature half) |
| 9 | Interim session ledger vs the PTY host (ft-1c36bec1) | Interim row-per-tab now | CARD 8 scope |
| 10 | Verification carry-over across rewrites | Decline and document | CARD 15 (S6-07) |
| 11 | Backfill of mis-attributed nested-worktree rows | No backfill; record the cut-over id | nothing |
| 12 | A server-side wait over MCP | Defer; ship the read tool | nothing |
| 13 | Git floor: 2.37 and 2.38 features vs MIN_GIT_VERSION 2.23 (ft-45b8099a) | Raise to 2.38 if the clone floor already exceeds it; otherwise per-feature fallback | CARD 16 fallback shape, CARD 9 and 23 refusal text |
| 14 | Lane integration moves to the object database on both callers (wave and Worktrees panel). The in-checkout merge is kept only as the <2.38 fallback. | Yes | CARD 23's widened scope |
| 15 | Who writes `refs/gitpulse/ops`: the hook, or the app on a mailbox request | App-side; the hook stays decide-only. Set a cap and expiry. | CARD 30 |
| 16 | Who owns the "Previous attempts" brief section: dc-store (a DevCouncil change, re-vendored, consumers rebuilt) or a GitPulse append | dc-store; a marked append as the interim if DevCouncil main is mid-merge | CARD 31 |
| 17 | A narrow MCP claim write (`gitpulse_claim_files`) | No; claims come from `planned_files` and the hook | nothing |
| 18 | Token-cap semantics | Wall-clock now; tokens after CARD 28 and a usage parse | CARD 32 (token criterion) |
| 19 | Fork flag and copy semantics. Read the current Claude Code docs for the fork flag, and approve copying uncommitted changes. | Confirm the docs first | CARD 33 |
| 20 | Digest delivery and schedule | A Health section on the hygiene clock | CARD 34 |

There is no stop condition:
- **No active data loss.** S5-01 is a working-tree integrity hazard that `merge --abort` or `reset --merge` restores. S4-07 is mitigated by `-d` unless forced, `forceDelete` defaulting to off, and the backup tip.
- **No security exposure** needs a product decision before filing.
- **No breaking public API,** provided CARD 15 only adds fields.

**Waiting for your go-ahead before any build work.**

---

# Previous audit (2026-10-12), preserved unchanged

# GitPulse deep audit: correctness and hardening (2026-10-12)

Read-only audit of `main` @ `a92e40fb`. Every verified finding is on the GitPulse task board. No source, config or dependency was changed. **No build work has started**, pending the owner's go-ahead.

**Method.** DevMap first (status, dead, clones, deps/explore/impact via CLI). GitPulse Insights snapshot. 8 Sonnet auditors in 2 waves of 4, deduplicated against the 89-card board. 4 Opus verifiers re-read every cited line. Fable did the grouping, priorities and build order. Filing used `gitpulse-mcp` (`gitpulse_add_task`, with overwrite for folds).

**Index caveat.** DevMap was fresh but *degraded*: ~37.6% of call sites resolved, 46 files were pattern-recovered, and impact walks came back `walk_incomplete`. Blast radii are lower bounds, and Opus confirmed every defect by reading the source. Separately, `gitpulse_insights` reports the same store as stale (`analyzer_freshness=false`) while `devmap status` reports it fresh. That skew is itself a finding (C-mcp-01, folded into ft-8db7edeb).

**Concurrent audit.** A second 2026-10-12 *performance* audit, in another session, filed ~20 cards ("build order N of 35") while this one ran. Overlaps were folded, not duplicated: the benchmark baseline → ft-c0d9313f, CoW clone → ft-4577fa0e, transcript streaming → ft-dd012f8b.

## Summary

- Raw findings: **88**. Opus: 44 confirmed, 30 downgraded, 14 rejected; 1 more dropped as a duplicate of an existing card.
- Kept and filed: **73**, as **14 new cards** and folds into **23 existing cards**. Another 14 cards got verification annotations because their criteria are already met in code.

| severity \ dimension | architecture | typing | validation | async-errors | testability | performance | bug | security | total |
|---|---|---|---|---|---|---|---|---|---|
| critical | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| high | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 2 |
| medium | 1 | 1 | 1 | 4 | 1 | 2 | 11 | 5 | 26 |
| low | 1 | 2 | 4 | 4 | 7 | 6 | 14 | 7 | 45 |

## Top 10 risks

1. **Preflight accepts git 2.23 but the engine hard-requires 2.25 (pathspec-from-file), 2.31 (--diff-merges) and 2.36 (worktree list -z)** (high, P1) → `ft-45b8099a`. Ubuntu 22.04 (git 2.34.1) or Debian 11 (2.30.2) or Ubuntu 20.04 (2.25.1): preflight reports Ok, but 'worktree list -z' fails with 'unknown switch z' (added in git 2.36). read_worktree_entries then errors, so the worktree panel, checked_out_branches, delete_bra…
2. **New File / New Folder in the explorer silently truncates an existing file (writes empty content over it)** (high, P1) → `ft-f16fad35`. Explorer > New File, type `README.md` (or `src/main.ts`, any name already in the tree, or a file the tree has not scrolled to). The frontend never checks the listed files; cmd_write_file_content is an overwrite primitive: it reads the existing file only to lea…
3. **collision-guard and task-scope fence are skipped (warning only) for any write whose absolute path is outside the cwd's worktree, e.g. the main checkout or a sibling lane** (medium, P1) → `ft-58cce43c`. An agent session whose cwd is the linked worktree /repos/app-wt-a (bound to a task with a declared scope) calls Edit/Write with file_path=/repos/app/src/engine/mod.rs (the main checkout, or /repos/app-wt-b where another agent has uncommitted edits to the same…
4. **gitpulse-hook drops any payload over 4 MiB (or otherwise unparseable) with no stdout notice, so an oversized Bash command or Write runs completely ungated and unannounced** (medium, P1) → `ft-6060bbd7`. PreToolUse stdin carries the whole tool_input. A Bash command containing a >4 MiB heredoc/padding followed by `git push --force origin main`, or a Write whose `content` is >4 MiB, makes read_input return Err; decide() logs to stderr and exits 0 with empty stdo…
5. **Vendored devmap extraction payload identity is compared by equality only, so a newer installed CLI silently marks the store 'obsolete' for GitPulse and nothing flags the skew** (medium, P1) → `ft-8db7edeb`. The store at .devcouncil is rebuilt by the installed devmap CLI 0.2.4 (stamps analyzer_version 'extract-v89'). GitPulse links vendored devmap at commit 9add99c2 whose identity is '...:extract-v87'. latest_generation_payload_is_current compares by equality, so…
6. **Confirm dialogs, name prompts and Undo toasts apply destructive repoStore mutations to whichever tab is active when they resolve, not the tab they were raised on** (medium, P1) → `ft-1629347b`. Tab A (repo X) and tab B (repo Y, e.g. a sibling worktree sharing objects/branch names). In A, Reflog > Reset here -> confirm dialog opens. Press Ctrl+Tab/Cmd+2 (window listener has no prompt guard), or a `gitpulse <path>` open event arrives, or a watcher 'rep…
7. **Editor save is last-writer-wins: the draft records its base text but never compares it with disk, so a save overwrites agent/pull changes made while the file was open** (medium, P1) → `ft-f16fad35`. Open src/lib.rs in the Code editor and start typing (draft with sourceContent=V1). A hosted agent in the same worktree (or `git pull`/checkout) rewrites the file to V2; the status poll flips the file's status code, the blob reloads, the draft stays. Cmd-S: the…
8. **Doc rename rewrites inline Markdown links to a vault-root path, but they resolve relative to the source note, so links in subfolders break** (medium, P1) → `ft-e023906b`. docs/Guide.md contains [plan](Roadmap.md). Renaming docs/Roadmap.md -> docs/Plan.md rewrites it to [plan](docs/Plan.md). The vault (and GitHub, and any Markdown renderer) resolve that from docs/Guide.md as docs/docs/Plan.md: the link now points nowhere, the ou…
9. **ci:local drops the unit-test filter when an integration target is also affected, so a narrowed run records a clean 'passed' note for tests it never ran** (medium, P1) → `ft-b5393529`. Changed files src/bar.rs and tests/foo.rs. DevMap affected-tests returns [tests/foo.rs, src/bar.rs]. plan_ci_from_affected lets it through (only >1 unit file fails closed), rust_test_args emits `cargo test --manifest-path M --test foo --` with the `bar` filter…
10. **Force push uses bare --force-with-lease while GitPulse auto-fetch keeps refreshing the remote-tracking ref, so the lease never protects against unseen commits** (medium, P2) → `ft-45b8099a`. Auto-fetch is on. A teammate pushes commit C to origin/feature. The background fetch moves refs/remotes/origin/feature to C. The user, who never saw C, amends or rebases their local feature and clicks Force push. 'git push --force-with-lease' without an explic…

## New cards

| card | title | P | severity | findings |
|---|---|---|---|---|
| `ft-45b8099a7fa8b91a717fae79570ae435` | Git capability preflight: MIN_GIT_VERSION matches the features the engine uses, and force push carries --force-if-includes | 1 | high | A-git-01, A-git-02 |
| `ft-f16fad357f5d722a035cedbf27147663` | Editor and explorer write primitives: exclusive create for New File/Folder, compare-and-save with the draft's base, and .git refused inside sandbox_join | 1 | high | D-frontend-01, D-frontend-03, A-git-10 |
| `ft-1629347b18bc4aab9c73cc3e3602ff32` | repoStore mutation contract: every mutation names its target session, every caller checks the outcome, and bulk fetch/pull goes through runMutating | 1 | medium | D-frontend-02, D-frontend-04, D-frontend-05 |
| `ft-e023906b27b02a768cd06965aefccb79` | Doc rename: rewrite links relative to each note, rewrite the moved note's own links, judge every rewritten file, write from fresh bytes, and report truncation | 1 | medium | E-integrations-05, E-integrations-06, E-integrations-07, E-integrations-08 |
| `ft-46bece0d36708fd4d84c3056c790d6d3` | Typed MCP tool arguments: one args struct per tool drives the advertised schema, validation and the handler; no as_i64/as_u64 fall-through | 1 | medium | C-mcp-02, C-mcp-06, H-architecture-10 |
| `ft-3a32f751e95ebdc3e9ce17ef7b949e7c` | Git writer/reader edge cases: checkout fallback detaches HEAD, [Drop, Squash] amends the onto commit, multi-commit sequencer undetected, refs/heads/ stripped repeatedly | 2 | medium | A-git-03, A-git-04, A-git-14, A-git-16 |
| `ft-04c0ab7eb8dadd2b77fe0ddbb168cb02` | Remotes: redact embedded credentials before the webview, and require fetch/pull/push targets to be configured remote names | 3 | low | A-git-11, A-git-12 |
| `ft-bdb253cfb494e1ebb855392c07ed1912` | Bounded input handling: O(1) XML entity decode, streamed transcript ranges, one deadline per hook-bridge connection, bounded no-follow manifest reads | 2 | medium | E-integrations-01, E-integrations-04, E-integrations-11 |
| `ft-d1c290f15d796c1ac8a9358c42812e99` | Outbound data only where the user chose: scrubbed environment and pinned config for npm/bun scans; explicit, owner-verified AI endpoint before sending a diff | 2 | medium | E-integrations-03, E-integrations-12 |
| `ft-ae3490e79efe36f2251ee5ba5fb57237` | Worktree binding authority: order bind/unbind by insertion sequence, not ts_utc, and stop importing sibling-authored bind rows as authoritative | 3 | low | C-mcp-03, C-mcp-04 |
| `ft-ade6e999465b77e223bae29d5f76a26b` | Deterministic time: injectable Clock for ref_cache TTL and the watcher debounce loop; prime/retry in the batched-registration watcher test | 2 | medium | G-tests-perf-06, G-tests-perf-07 |
| `ft-adc5dca55b017d9941d25df3b159401c` | Git writer parity and coverage: one *_argv builder per judged writer with a recording-gate parity harness; tests for delete_tag and push_tag; delete the dead stash_pop | 3 | low | G-tests-perf-08, G-tests-perf-09 |
| `ft-b4eb6688f4606799f0d5f92a37c1d33e` | Typed process outcomes: a RunError enum replaces substring classification of stderr (timeout, unborn HEAD, transient lock), rendered to String only at the IPC/MCP edge | 2 | low | H-architecture-12, A-git-13 |
| `ft-4ef246374ebac048f106db5e184fefbd` | Portless route detection: whole-label hostname matching, ::1 probing, checked u16/u32 casts, a locally defined stable hash, one routes.json read per listing | 3 | low | A-git-08, A-git-09 |

## Folded into existing cards

| card | findings | change |
|---|---|---|
| `ft-4577fa0ec1f95ea1614c26c4830d82b6` (other-session card) | A-git-06, A-git-07 | criteria added |
| `ft-dd012f8b6f1f8baba3a3e02f3969acba` (other-session card) | E-integrations-10 | criteria added |
| `ft-c0d9313fdcd52966cb834c1c1c85b807` (other-session card) | G-tests-perf-03, G-tests-perf-04, G-tests-perf-05 | criteria added |
| `ft-58cce43cdc3adce16ba090d05d20861c` Hook scope fence leftovers from phase 2: argument-writing commands, es | B-agents-01, B-agents-06 | escalated to P1/high |
| `ft-6060bbd7e15118b8052af3c8f38c3a59` gitpulse-hook correctness: reap children on exit, one deadline under t | B-agents-02 | criteria added |
| `ft-6b72c70300b3be6743ec7c1eece96482` Profile sidecar: one slow work.runs.managed.prepare (>15 s) tears down | B-agents-03, B-agents-04 | criteria added |
| `ft-83412c1b6b6806b062e0440db1102f47` One panic disables subsystems until restart: hygiene try_lock reports  | B-agents-05 | criteria added |
| `ft-3321bb9c3cc54877d9e505b918df497e` Removing a worktree or releasing a slot checks only run rows: a dock a | B-agents-07 | criteria added |
| `ft-8db7edebc95dfcb925f9ce6390fd1a20` Land DevMap accuracy wave 8 (Rust trait dispatch, importlib handles; e | C-mcp-01 | escalated to P1/medium |
| `ft-dcc15c9eacf069cfb4e0e423172b2e8c` Task MCP tools keep their retry promises: idempotent retries with expe | C-mcp-05 | criteria added |
| `ft-38ec498d6879712db14863f8e80813ae` Insights misses agent sessions under other Claude config dirs (10 of 1 | E-integrations-09, C-mcp-08 | criteria added |
| `ft-b5393529abd3dee110708ff181008e08` Checks that could not run must not report clean: gate verdict folding, | E-integrations-02, F-ipc-security-01 | criteria added |
| `ft-845b7f1bcfaad7d52ca0ac21344c0418` Profile durability: workbench schema upgrade is one-way, unbacked and  | F-ipc-security-02 | criteria added |
| `ft-ed5d43e44d86defa343f9368ec8b91a1` CI hardening follow-through: first release proves SBOM/checksums, Code | F-ipc-security-03 | escalated to P2/medium |
| `ft-e460b9c2754fe5cf5199ef35e0adbca0` Bind destructive git actions to the state they checked: release tag, c | A-git-17, F-ipc-security-05 | criteria added |
| `ft-e6c6b4c496df19869c14a08e2fc485a5` Pin git's output format and environment: log.showSignature corrupts gr | A-git-18, H-architecture-13 | criteria added |
| `ft-7d5029c89e8097b35172fe4b463d4a1e` Backend performance: disk IO on the main thread, per-item git spawns ( | A-git-05, C-mcp-07, E-integrations-14 | criteria added |
| `ft-20eeffdf046268450d02a88518de9792` Supply chain: tool installer pulls releases/latest unpinned with a sam | E-integrations-13 | criteria added |
| `ft-538cd7bc62abcff83889f27a36c8e73f` Hermetic tests: suites read the developer's real git config, ~/.claude | G-tests-perf-01, G-tests-perf-11 | criteria added |
| `ft-036c84e750d9e8c34ab57da43728e310` Run the Rust suite in parallel honestly: CI enforces --test-threads=1, | G-tests-perf-02, G-tests-perf-12 | criteria added |
| `ft-038e6ba5a137e7747f52a6a026ba775b` Frontend: failed reads look like empty or success — duplicate issue im | D-frontend-06, D-frontend-07 | criteria added |
| `ft-2967fd9d7fc2bb04d35fad0c27588e03` Converge duplicate implementations (Rust and frontend) and start split | H-architecture-16 | criteria added |
| `ft-8fee86c7d33ad48d766412a75eba49d8` Replace source-text tests with behaviour tests: 79 component test file | G-tests-perf-10 | criteria added |

## Existing cards verified against the code

`[MET — verified in source, audit 2026-10-12]` marks a criterion the code already meets. Statuses were left alone: `done` is gated on Manvi verification, which this audit did not run.

- `ft-2b2f3e90cda1e50df7029a46ec72944a` **annotate**: Add 'Audit 2026-10-12 verification': the bullet 'four parsers use worktree list --porcelain without -z' is met (worktree.rs:368-381 read_worktree_entries is the single -z reader for git_writer.rs:2412, ops.rs:173, agent_worktree.rs:133; only tests in workbench/terminal_launch.rs:1280/1304 use the no
- `ft-83412c1b6b6806b062e0440db1102f47` **narrow**: AC1-AC4 verified met (hygiene/terminal/workbench poison recovery, supervisor degraded records, SingleFlight probes, FAILURE_TTL). Annotate those as met with the evidence lines and leave as the only open criterion the PTY-reader catch_unwind/finalize and per-session degradation key from B-agents-05 (
- `ft-6060bbd7e15118b8052af3c8f38c3a59` **narrow**: All six criteria verified met (reap_all, TOTAL_BUDGET/deadline, focus enrichment, DisjointSymbols as context, Cursor deny/ask rendering, FILE_WRITE_TOOLS matcher). Annotate as met and leave only the B-agents-02 criterion (no silent fail-open on read_input failure). Keep status review.
- `ft-3321bb9c3cc54877d9e505b918df497e` **narrow**: AC1-AC3 verified met (refuse_if_in_use at both call sites, Linux/macOS zombie states, reap_after_eof). Annotate as met and leave only the B-agents-07 criterion (re-check after the gate; spawn refuses a worktree with a removal intent). Keep status review.
- `ft-6b72c70300b3be6743ec7c1eece96482` **narrow**: AC1-AC5 verified met (slow prepare neither blocks nor kills, op_deadline/advertised_timeout, slot release after ensure, reconcile releases orphaned runs). Annotate as met and leave only the two folded residues: hosted-set shrink on observed run end with a bounded keep rule (B-agents-04, introduced b
- `ft-56ed18e4ee2ec6adc94defe67297b368` **narrow**: AC1-AC6 verified met in source (neighbors_envelope with available/reason/truncated/walk_incomplete/source_freshness, typed output schema, depth and min_confidence honoured, every devmap_* tool validated, OBJECT_ONLY_OUTPUT_SCHEMAS cleaned). Narrow to AC7: one live gitpulse-mcp devmap_neighbors call 
- `ft-d37c72257c4b49e1ed9ac75e2be2692c` **narrow**: AC1-AC3 and the matcher half of AC4 verified met (release_is_published HEADs SHA256SUMS, newest legacy protocol fallback, every advertised tool validated, hooks.json Notification matchers permission_prompt/idle_prompt). Narrow to the v1.3.5 tag: the local tag still exists and CHANGELOG.md has no [1.
- `ft-74adbb3d0039d98c601da882979bd6f9` **annotate**: AC1-AC5 verified met (build.rs AppManifest + status-popover capability, native confirms via desktop::confirm::native with set_binary/set_source_root only from app-resolved installs, navigation_allowed/on_navigation and remote_media:false, prepare_open trust+confirm, absolute-only PATH and sidecar en
- `ft-6015faa43fc2e30fc15adbe70b5cccfa` **annotate**: AC1-AC5 verified met (exact file+content approval with re-approval on change, every sh -c judged through guard_command with ledger, separate cowCaches/runHooks defaults false, parse error is an error, three tests). Add the verification section; nothing folded (the CoW copy quality it cites is aud-co
- `ft-20eeffdf046268450d02a88518de9792` **narrow**: AC1-AC3 verified met (empty PINNED_RELEASES refuses releases/latest, refuse_downgrade, HTTPS-only curl, pinned devmap/manvi revs, MIN_CLONE/SUBMODULE_GIT_VERSION enforced). Annotate as met; leave only the folded E-integrations-13 criterion (--max-filesize, size check, cleanup, atomic rename) and the
- `ft-183b29f56fbaf9c0f7b5e846a894ab91` **narrow**: AC1-AC3 verified met (repoMetrics.ts and graphStore.ts tested and off the exclusion list, every exclusion owned and dated with a contract test, cargo llvm-cov --branch in coverage.yml). Narrow to AC4 (function-coverage margin above 1 point), which needs a coverage run, and fix the stale numbers in t
- `ft-036c84e750d9e8c34ab57da43728e310` **narrow**: AC1-AC3 verified met (EnvGuard only from env_serial, test-parallel.yml at --test-threads=8 with the serial job kept, cross-lock TOOL_CONFIG_ENV reader test). Narrow to AC4 rewritten with the exact sites and scanner shapes from G-tests-perf-02 (folded). Keep status review.
- `ft-538cd7bc62abcff83889f27a36c8e73f` **narrow**: AC1-AC4 verified met (per-process hermetic root asserted by the five-resolver canary, empty global gitconfig + GIT_CONFIG_NOSYSTEM asked of git, set_var ban in hermetic_contract.rs; AC3 holds by construction though not proven by a run here). Annotate as met and leave only the two folded residues: th
- `ft-2967fd9d7fc2bb04d35fad0c27588e03` **annotate**: Add 'Audit 2026-10-12 verification': the commands/mod.rs half of the split criterion is done (109 lines, 18 production modules, largest commit_graph.rs at 602); the repoStore.ts half is not (3,235 lines, one createRepoStore closure); duplicate-convergence and test-only-export criteria not re-verifie

## Fable's build order (≤4 per wave, no shared files within a wave)

- **Wave 1:** `ft-f16fad357f5d722a035cedbf27147663`, `ft-e023906b27b02a768cd06965aefccb79`, `ft-46bece0d36708fd4d84c3056c790d6d3`, `ft-b5393529abd3dee110708ff181008e08`. Data loss and fail-closed first: the overwrite primitive (explorer truncation, editor clobber, .git writes), the rename that breaks links, the MCP typing hole that disables the revision guard, and ci:local recording 'passed' for tests it never ran. No shared files.
- **Wave 2:** `ft-45b8099a7fa8b91a717fae79570ae435`, `ft-1629347b18bc4aab9c73cc3e3602ff32`, `ft-6060bbd7e15118b8052af3c8f38c3a59`, `ft-6b72c70300b3be6743ec7c1eece96482`. Remaining P1 engine, frontend and agent-supervision work: git capability floor plus force-push lease, the repoStore target/outcome contract (after the FileTreePanel edits of wave 1), the hook's silent fail-open, and the sidecar hosted-set/temp-dir residues.
- **Wave 3:** `ft-8db7edebc95dfcb925f9ce6390fd1a20`, `ft-845b7f1bcfaad7d52ca0ac21344c0418`, `ft-e460b9c2754fe5cf5199ef35e0adbca0`, `ft-e6c6b4c496df19869c14a08e2fc485a5`. P1 review cards get their new criteria in one pass each: destructive-action binding (stash, release tag), git environment pinning (textconv, locale), tools.json patch-not-replace, and the devmap freshness skew that is misleading every agent today. git_cli.rs is only touched by ft-e6c6b4c4 here.
- **Wave 4:** `ft-3a32f751e95ebdc3e9ce17ef7b949e7c`, `ft-4577fa0ec1f95ea1614c26c4830d82b6`, `ft-bdb253cfb494e1ebb855392c07ed1912`, `ft-038e6ba5a137e7747f52a6a026ba775b`. P2 engine and bounded-input work: git edge cases (git_writer/git_reader), bounded readers (coverage/deps/ingest/bridge), the frontend status-poll and host-load residues (repoStore.ts now free), and CoW cache cloning (worktree.rs, not touched by anything else this wave).
- **Wave 5:** `ft-d1c290f15d796c1ac8a9358c42812e99`, `ft-ade6e999465b77e223bae29d5f76a26b`, `ft-58cce43cdc3adce16ba090d05d20861c`, `ft-38ec498d6879712db14863f8e80813ae`. Outbound-data controls (deps.rs now free), transcript attribution and config dirs (ingest now free), deterministic clock for ref_cache/watcher, and the fence-escape fold; the fence fold's terminal/mod.rs hunk starts only once the hx/w10 lanes have landed, otherwise it slides to wave 9 with ft-83412c1b.
- **Wave 6:** `ft-c0d9313fdcd52966cb834c1c1c85b807`, `ft-b4eb6688f4606799f0d5f92a37c1d33e`, `ft-3321bb9c3cc54877d9e505b918df497e`, `ft-dcc15c9eacf069cfb4e0e423172b2e8c`. Measurement and typing: the benchmark baseline (benches, Cargo.toml, workflows), the RunError enum (git_cli/git_reader/devmap cli, free this wave), the worktree-removal re-check, and the task-key collision fold (mcp/mod.rs free since wave 1).
- **Wave 7:** `ft-04c0ab7eb8dadd2b77fe0ddbb168cb02`, `ft-ed5d43e44d86defa343f9368ec8b91a1`, `ft-7d5029c89e8097b35172fe4b463d4a1e`, `ft-538cd7bc62abcff83889f27a36c8e73f`. Backend performance residues on git_reader/codeintel/tool_capability, remote credential redaction and remote-name membership (git_writer free), release-pipeline pinning in a release-free commit, and the hermetic env registry (hooks/mod.rs free).
- **Wave 8:** `ft-adc5dca55b017d9941d25df3b159401c`, `ft-4ef246374ebac048f106db5e184fefbd`, `ft-20eeffdf046268450d02a88518de9792`, `ft-036c84e750d9e8c34ab57da43728e310`. Lower-priority tests and small modules: writer argv parity and delete_tag/push_tag tests (git_writer, refs, staging), portless routes (worktree.rs free), the curl --max-filesize residue, and the wall-clock contract scan.
- **Wave 9:** `ft-ae3490e79efe36f2251ee5ba5fb57237`, `ft-83412c1b6b6806b062e0440db1102f47`, `ft-2967fd9d7fc2bb04d35fad0c27588e03`, `ft-8fee86c7d33ad48d766412a75eba49d8`. Last: the terminal reader catch_unwind fold (terminal/mod.rs, after all w10 lanes and after ft-58cce43c's hunk), the dc-glob convergence pair on the in_progress refactor card, TZ-pinned vitest legs, and binding-order semantics in the ledger (design decision pending).

## Rejected findings

- **A-git-15** prune_worktree runs 'worktree prune --expire now', deleting admin data of worktrees on unmounted volumes. *rejected*: The premise is false: plain 'git worktree prune' has no grace period (its expire defaults to 'everything'; the 3-month default is gc.worktreePruneExpire, used only by git gc). Reproduced with git 2.43.0: a worktree whose directory was moved away was removed by
- **B-agents-08** Hook 'notice once per session' markers live in a shared, predictable temp path: another local user can suppress the UNGATED notice, and the files are never cleaned. *rejected*: Speculative. Pre-planting a marker needs the FNV hash of the host's session UUID (hooks/mod.rs:1321), which another account cannot generally know. A foreign-owned directory makes create_new fail with PermissionDenied, and first_notice_in_session returns true f
- **F-ipc-security-04** Release artifacts carry no verifiable identity: macOS ad-hoc signed by config, no notarization, no Windows signing, checksums/SBOMs from the same workflow, no build attestation. *confirmed* / duplicate-of:ft-aa3086e64e8a1d7f52d60ed425a0ed46: The facts are accurate: tauri.conf.json signingIdentity '-', no attest-build-provenance step in release.yml, and INSTALLATION.md documents the ad-hoc signature. This is a documented limitation and an open owner decision. ft-aa3086e6 lists 'Release signing and 
- **H-architecture-01** git_cli.rs (9,299 lines, only 4,254 production) mixes five concerns; the spawn gate and process runner (lines 1745-3870) are not git-specific and are the natural first split. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: The line ranges are accurate (git_cli.rs is 9,299 lines; tests start at :2934/:4255), but the finding shows no concrete cost. git_cli.rs is not in any in-flight worktree's collision list (insights.json), and no merge conflict or drift is shown. Splitting the l
- **H-architecture-02** terminal/mod.rs hosts the Manvi action argv allowlist (~1,000 lines, 2135-3179) inside the PTY module, forcing analyzer/coverage and coverage_toolchain to depend on terminal. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: The planner/gate drift this would prevent was already fixed by coverage_toolchain.rs (its doc at :1-32 records the two past drifts and the shared constants plus contract test that closed them). coverage_toolchain.rs:167's `use crate::terminal::normalized_progr
- **H-architecture-03** git_reader.rs: `impl GitReader` is a 1,945-line namespace (506-2451) mixing reads, analytics reports, default-branch resolution and blob/blame plumbing. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: Size and namespace taste; git_reader.rs is on the board's largest-files list. An engine<->analyzer module cycle inside one crate has no compile or link cost, and no bug is attributed to it. The one concrete defect near this code (the is_unborn_head_error drift
- **H-architecture-04** analyzer/coverage.rs (4,203 prod lines) and analyzer/deps.rs (4,231 prod lines): pure format parsers and per-ecosystem scanners are bundled with orchestration. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: coverage.rs and deps.rs are already on the board's size list. 'A parser fix cannot be fuzzed in isolation' is false: the parse_* functions are pure free functions callable as they are. No drift or bug is shown.
- **H-architecture-05** ledger/mod.rs and github/mod.rs host unrelated domains: fleet metrics/pulse snapshots (~690 lines) in the ledger, dependabot/code-scanning alert loaders (~600 lines) in the GitHub core. *rejected*: Fleet and pulse writes share the ledger DB on purpose, through with_conn (ledger/mod.rs:2309,2591), so moving them to another file changes nothing about the lock or connection risk the finding cites. No drift or bug is shown; this is pure taste.
- **H-architecture-06** Frontend splits: repoStore.ts is one 2,960-line closure; TaskBoard.svelte is a 2,024-line script plus 690 lines of markup. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: Splitting repoStore.ts is literally the board task's acceptance criterion ('At least commands/mod.rs and repoStore.ts are split'), and TaskBoard.svelte is on its list. The new facts are corrected sizes (repoStore.ts 3,235, TaskBoard.svelte 2,860) and that the 
- **H-architecture-07** The crate has no layering: a 29-module dependency cycle, and the git engine reaches up into harness, ledger, graph, analyzer and diff. *rejected*: No demonstrated cost. Module cycles inside one crate do not change what recompiles or links, since the crate is the compilation unit. The hook/mcp/daemon binaries linking tauri is a crate-boundary question, and the only latency figure (docs/PERFORMANCE.md:676-
- **H-architecture-08** Three separate `static APP: OnceLock<tauri::AppHandle>` globals (ledger, devmap diagnostics, tool_install) make core modules depend on Tauri and untestable for their events. *rejected*: ledger/mod.rs:345-361 documents the announcement as best-effort, with a cursor payload ('a dropped notification costs nothing ... the next tail picks it up'). An untested emit therefore cannot lose data, and no missed-announcement bug is shown. Three OnceLock<
- **H-architecture-09** commands/commit_graph.rs contains domain logic (mainline resolution, history simplification, payload assembly) and integration tests import it through `commands::`. *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: Moving pure functions from commands/ to graph/ is taste; tests importing gitpulse_lib::commands is not a barrier. The real defect inside it, two is_unborn_head_error functions with different phrase lists, is verified and carried in H-architecture-12.
- **H-architecture-11** Status/kind/severity/state are `String` in ~60 serialized domain structs and compared by literal; ci_local's own comment documents the hazard. *rejected*: No drift has happened. ci_local::run_verdict already uses all(passed) (ci_local.rs:830), and the comment records the hazard as handled. An unknown Manvi severity makes is_unreadable_construct() false (harness/policy.rs:261-267), which falls through to enforcin
- **H-architecture-14** Two hand-rolled TOML parsers (worktree hooks, Cargo manifest shape) while the `toml` crate is already in the build closure. *rejected*: The headline failure is false. parse_toml_strings breaks out after a non-array string and then accepts a trailing `# comment` (worktree_hooks.rs:276-280: `!rest.starts_with('#')`), so `post_create = "npm ci" # install` parses. Multi-line arrays are refused wit
- **H-architecture-15** Small shared helpers re-implemented in several modules with diverging behaviour (validate_oid x2 + is_oid, fnv1a x2, now_* x7, shell quoting x2, hand-rolled base64, bounded reads x5). *rejected* / duplicate-of:ft-2967fd9d7fc2bb04d35fad0c27588e03: No drift is demonstrated. verify_status::is_oid demanding a full 40/64-hex head is intentional, since a verification record must bind a full object id; it is not a disagreement with validate_oid. The other copies (now_*, fnv1a, sh_quote, b64_encode) are not sh

## Decisions taken without asking (log)

- Followed the board's own rule (gitpulse-tasks skill): one card per *concern*, with each finding as an acceptance criterion. Not one card per finding.
- Folded work into existing review cards instead of opening duplicates, and marked met criteria instead of deleting them. No card was deleted, merged, or moved to done.
- Did **not** merge `a3224c3b` (your own card) into `ft-329789a9` as Fable proposed. A merge is irreversible over MCP, so it is listed below for you.
- Left the in-progress incident cards (signal blast-radius, mass session loss, priority/suggest) untouched. terminal/mod.rs work is sequenced after the hx/w10 lanes land.
- Retried `related_tasks_exist` refusals with `reviewed_related` only when every listed card had already been reviewed. The board was re-pulled before each batch to catch the concurrent session's new cards.
- The source snapshot was staged through the git-ignored `tmp/` folder for the cloud auditors and deleted right after.

## Owner decisions surfaced by Fable (needed before building those cards, not before filing)

- Git floor (aud-git-capability-preflight): raising MIN_GIT_VERSION to 2.36 drops Ubuntu 22.04 (git 2.34) and Debian 11 (2.30); the alternative is per-feature fallbacks (non -z worktree parsing, no --diff-merges, no --force-if-includes below 2.30, refusing force push). Product decision: supported-git policy.
- Data loss already shipped (aud-file-write-primitives): Explorer New File truncates an existing file and editor save overwrites agent/pull changes with no prompt. No decision needed to fix, but the owner should know these are live in the released app and may want a release note or hotfix cadence.
- Private registries (aud-outbound-endpoints): refusing dependency scans when a project .npmrc sets registry/_authToken/proxy will break scans for repos that legitimately use a private registry; the alternative is an explicit per-repo approval of the project registry. Product decision.
- AI endpoint consent (aud-outbound-endpoints): requiring a confirmed, pinned endpoint before the first AI call changes first-run UX (no silent auto-discovery). Product decision.
- MCP integer policy (aud-typed-mcp-args): choosing 'reject non-integral-typed numbers' changes the public MCP contract for clients that serialise 5 as 5.0 (JS/Python floats); 'accept whole floats and convert' is compatible but keeps a lenient schema. Recommend accept-and-convert for one release with a deprecation note, then reject.
- Release workflow (fold into ft-ed5d43e4): pinning actions to SHAs, persist-credentials:false, splitting the token-bearing upload step and adding rust-toolchain.toml touch release.yml and repository settings (Dependabot github-actions); the card's own rule is to land this in a release-free commit, and its 'ask first' note applies.
- Locale pin (fold into ft-e6c6b4c4): A-git-18 contradicts the card's 'LC_ALL=C is sound'; the recommendation is LC_MESSAGES=C + LANGUAGE= keeping LC_CTYPE, with hook-running subcommands untouched. Design decision because it changes every git child's environment.
- Binding authority (aud-binding-authority): deciding which processes may write worktree bindings and whether sibling-ledger bind rows are ever authoritative is a design decision; the fix proposed (insertion-order precedence, non-authoritative sibling imports) is conservative but changes agent task-scope semantics.
- Public-surface removal (aud-git-writer-parity-tests): deleting GitWriter::stash_pop and cmd_stash_pop removes a registered (unused, un-capability'd) IPC command and its generated permission.
- Folds into confirmed-stale cards: ft-6060bbd7, ft-6b72c703, ft-83412c1b, ft-3321bb9c, ft-20eeffdf and ft-538cd7bc are fully implemented on their original criteria; I narrowed them to the audit residue rather than closing them and filing six new cards, per 'if a task already exists, modify it'. If the owner would rather see the finished work reach done now, split the residue into one 'audit residue' card per subsystem and move the originals to done after Manvi verification.
- Disagreements with Opus placements: A-git-13 grouped with H-architecture-12 (aud-typed-run-errors) instead of ft-9fa261d9 (root cause is the classifier); B-agents-02 folded into ft-6060bbd7 instead of new; F-ipc-security-02 folded into ft-845b7f1b instead of new; E-integrations-01 kept in aud-bounded-readers instead of the coverage card. No disagreement with any Opus verdict on severity or reality.
- Merge a3224c3b into ft-329789a9: both are the owner's own cards; confirm before merging.
- In-flight collision: B-agents-05 and B-agents-06 edit terminal/mod.rs, which hx/w10-governor and hx/w10-proposals hold now. Both are scheduled last (waves 5 and 9) and gated on those lanes landing; nothing in this plan touches sysload/procscan.rs or the three in_progress incident cards.

---

# Reliability, stability, and performance audit (2026-10-13)

Read-only audit of `main` @ `8026dd7e`, focused on reliability, stability, and performance. The Scion/herdr report above and the 2026-10-12 report under it are unchanged. No source, config, or dependency file was changed. **No build work has started.**

## Method

1. **Map.** `gitpulse_insights` first, then `devmap status --json` (CLI and MCP), then a full `gitpulse_list_tasks` (176 cards, `has_more: false`) re-pulled to 178 immediately before the second filing batch. `gitpulse_get_task` was not run on every open card. Titles and labels are the dedupe baseline. Full briefs were read for the cards this audit folded or might have folded into: `ft-9fa261d9`, `ft-c0d9313f`, `ft-bdb253cf`, `ft-e6c6b4c4`, `ft-b5393529`, `ft-31d82cd4`, `ft-fdfea4c7`.
2. **Audit.** Four code-reader subagents, one slice each. This harness has no model switch on `spawn_subagent`, and code-reader cannot write files, so the "Sonnet writes a findings file" step did not happen. The parent re-read the cited lines (the Opus role) and did the grouping (the Fable role). There was no Fable/Opus disagreement.
3. **Slices filed.** Diff and hunk staging, git process and the shared spawn budget, coverage and dependency scans. The frontend status-stability slice (`01a13726-265d-7792-a733-ed180c050dbe`) was still running at filing (551 tool calls, about 75% of its context). Its findings are not on the board. They will be verified and filed only if that auditor returns with `file:line` evidence before build starts.
4. **Index.** DevMap generation 4136, schema 27, `query_ready` and `is_fresh` true on CLI and MCP status, and also degraded: `partial_parse` total 89 (listing truncated at 50), 47 pattern-recovered html/css files, Rust resolution net_permille 358. `gitpulse_insights` codeintel for the same generation says `analyzer_freshness: false` and `is_fresh: false` ("stored extraction payload is obsolete"). Call-graph answers are incomplete. Empty walks are unknown. The index was not rebuilt. Dead-symbol pass skipped. `devmap_clones` showed 23 of 1150 (`truncated: true`), mostly vendored; that sample is not evidence of "no first-party clones". Duplicate convergence stays on `ft-2967fd9d` (`in_progress`). Not touched.

**Lanes at map time.** Eight worktrees. Published collision: `src-tauri/src/engine/mod.rs` shared by `bl/card16-pulse-walks` and `bl/card22-merge-teardown`. Main's dirty set was the three untracked audit markdown files, shared by idle Claude sessions. `git_reader.rs` is in-flight risk for the Pulse card (`ft-9cfa4e`, `in_progress`). `conflict_session.rs` and `conflict_fs/windows.rs` are in-flight risk for merge teardown (`ft-9d044474`, `in_progress`). Neither in-progress card was overwritten.

## Severity × dimension

Twelve findings kept. Each is counted once, under the dimension of the failure.

| severity \ dimension | architecture | typing | validation | async-errors | testability | performance | correctness | security | total |
|---|---|---|---|---|---|---|---|---|---|
| critical | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| high | 0 | 0 | 1 | 1 | 0 | 1 | 1 | 0 | 4 |
| medium | 0 | 0 | 1 | 1 | 0 | 3 | 3 | 0 | 8 |
| low | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| total | 0 | 0 | 2 | 2 | 0 | 4 | 4 | 0 | 12 |

## Top 10 risks

1. **A denied shared-budget token is admitted when the record cannot be rewritten, and an 8-byte high-version buffer is treated as a newer layout.** High, P1. `ft-acff387700ee80d3475f38d7bfeaa7f6`. `take_at` returns `Take::Unavailable` when `write_state` fails even though the in-memory decision was Denied (`shared_budget.rs` around the denial write). `settle_shared` admits `Unavailable` (`git_cli.rs` settle path). `decode` treats `version > VERSION` as `Newer` as soon as the magic and version word are present, before the length check, so an 8-byte `GPSB` plus a newer version word fail-opens. A full-length newer record and a lock past `LOCK_PATIENCE` stay fail-open on purpose.
2. **One coverage or dependency scan can hold a blocking-pool thread for the sum of its 90-second tool timeouts, and a capped coverage scan is labeled Measured.** High, P1. `ft-04f5dd267cca23c37aa3e9e6ede6a238`. `off_thread` is `spawn_blocking` with no timeout. Deps auditors run one after another, each up to `AUDIT_TIMEOUT` (90s); npm alone is up to 6 roots × audit and outdated. `coverageScanStatus` returns Measured when `coverageScanReasons` is empty, and that function ignores `unresolved_files`. The agent prompt does mention the drop. The panel chip is the lie.
3. **The per-file commit diff of a merge commit omits `--diff-merges=first-parent`, so a clean merge can render as an empty success while the whole-commit diff shows hunks.** High, P1. `ft-dadcd71d9f79d90cc8d7c991e564366f`. `get_commit_diff` passes the flag (`git_reader.rs:1362-1376`). `get_commit_file_diff` does not (`git_reader.rs:1379-1400`). Git was not re-executed. The test comment at `git_reader.rs:8415-8418` says a plain `git show` of a clean merge renders as nothing. `get_range_diff` (`git_reader.rs:1403-1411`) is `git diff A...B`, not a merge show, and is out of this card. `get_commit_details` (`git_reader.rs:1527`) is numstat and also omits the flag; whether that empties a merge's file list was not verified, so it was not added.
4. **Cancel and deadline do not drain the child's pipes during `CHILD_GRACE` (500ms).** High, P1, folded. `ft-9fa261d9dbc034fed073e892286cb805` revision 3, status `ready`. After cancel or deadline the waiter calls `ask_then_stop_and_reap` and gives the output finisher `Duration::ZERO`. `stop_and_reap` sleeps on liveness and does not drain, then `kill_tree`. A child blocked on a full pipe cannot run its SIGTERM handler inside that grace. The inference that git unlinks `index.lock` from the signal handler was not verified. The missing drain was.
5. **Word diff annotates the whole replacement block on the UI thread.** Medium, P2. `ft-02b836b8ac1b0fc3f37e0b1cd87a3294`. `annotateAround` (`DiffViewer.svelte:1046-1059`) passes `replacementBlockBounds` for the entire del-run plus add-run. `annotateRange`'s pair loop has no pair cap (`wordDiff.ts:308-320`, `349-359`). Per-line `MAX_TOKENS` 500 and `MAX_LINE_CHARS` 50_000 stay. The bench row is on `ft-c0d9313f`.
6. **A coverage cache hit clones the whole `FileHitMaps`, and the stale-line check reads up to 2 MiB per resolved file before the 4,000-file trim.** Medium, P2, same card as risk 2. Cache hit clones the report and `entry.hit_maps` (`coverage.rs:604-607`) up to `CACHE_MAX_ENTRIES_PER_REPO` (250_000) before `file_coverage` keeps one path. `drop_stale_and_out_of_range` (`coverage.rs:680`) runs before the trim (`coverage.rs:740`) and `read_capped`s each resolved file to 2 MiB to count newlines (`coverage.rs:4156-4194`).
7. **A shared-read joiner waits on `Condvar::wait` with no timeout.** Medium, P2. `ft-24f6212d4b656e2e278fe0294e248997`. Followers in `run_read_shared` (`git_cli.rs:3135-3191`) do not read their `timeout` argument. The comment at 3175-3176 describes the leader. The child-timeout restart after the queue wait (`git_cli.rs:3686-3690`) is documented and was rejected as a bug.
8. **On Windows, a git pipe worker keeps the spawn permit after the write end closes.** Medium, P2. `ft-513113a31ae2b0f87807fd8548dfd2b7`. Non-Unix workers hold `Arc<SpawnPermit>` inside the thread. `Worker::drop` cancels and does not join a thread that is not finished. `OutputDrains::finish` waits out the deadline plus `CANCEL_GRACE` (50ms), then `take()`; a still-blocked worker returns `Stop::Undelivered` and the `Arc` stays in the detached thread. Ceiling 16. Unix is unaffected. The existing test that an unfinished worker must not release the permit early has to keep passing. Fix stays in `thread_io.rs`.
9. **Windows conflict saves drop the index executable bit for chunk and working-tree resolutions.** Medium, P2. `ft-2a2008e955775085649eeafd952a1802`. Parent re-read: `conflict_fs/windows.rs:104-109` hardcodes mode `100644`; `replace` allows only `missing` or `100644` (`windows.rs:240-250`). Chunk resolution copies `source.mode` (`conflict_session.rs:730-736`). Working-tree returns `source.clone()` (`739-770`). Ours/Theirs copy `stage.mode` (`692-696`). `prepare` writes that mode, then Windows remaps `100755` to `100644` only for the post-prepare worktree compare (`848-859`). The snapshot text for `120000` says whole-file choices preserve the link (`conflict_session.rs:346`). Not executed on Windows. In-flight against `ft-9d044474`.
10. **Hunk staging drops the no-newline-at-eof marker when it follows a context line.** Medium, P2. `ft-84a33d1225cd18f0c255f7a071646eb1`. The parser sets `noNewline` only when the previous row is add or del (`wordDiff.ts:249-251`). `patchBuilder.ts:128-149` already copies the flag onto context rows. `patch_builder.rs:157-174` already emits the marker for any flagged row. The bug is the parser. `git apply` was not executed.

`ft-e6c6b4c4` still has unmet `[H-architecture-13]`: `--no-ext-diff` is inserted and `--no-textconv` is not (`git_cli.rs:473-477`). Reconfirmed, not refiled. Do not build it in the same wave as the other `git_cli.rs` cards.

## New cards

Label on every new card: `audit-reliability-2026-10-13`. Status `backlog`. Stable `task_id` is `audit-reliability-2026-10-13-<slug>`.

| item_id | title | P | severity | kind |
|---|---|---|---|---|
| `ft-04f5dd267cca23c37aa3e9e6ede6a238` | Analyzer scans return by one deadline and do not report a partial coverage scan as Measured | 1 | high | bug |
| `ft-acff387700ee80d3475f38d7bfeaa7f6` | Shared spawn budget: a denied token stays denied when the record cannot be rewritten | 1 | high | bug |
| `ft-dadcd71d9f79d90cc8d7c991e564366f` | Per-file commit diff uses the same first-parent merge rendering as the whole commit | 1 | high | bug |
| `ft-24f6212d4b656e2e278fe0294e248997` | Shared git reads: a joiner returns by its own deadline | 2 | medium | bug |
| `ft-513113a31ae2b0f87807fd8548dfd2b7` | Windows git pipe workers release the spawn slot once the write end closes | 2 | medium | bug |
| `ft-2a2008e955775085649eeafd952a1802` | Windows conflict saves keep the index executable bit for chunk and working-tree resolutions | 2 | medium | bug |
| `ft-84a33d1225cd18f0c255f7a071646eb1` | Hunk staging keeps the no-newline marker when it follows a context line | 2 | medium | bug |
| `ft-02b836b8ac1b0fc3f37e0b1cd87a3294` | Word diff annotates a bounded window, not the whole replacement block, on the UI thread | 2 | medium | performance |

## Folded cards

| item_id | what changed |
|---|---|
| `ft-9fa261d9dbc034fed073e892286cb805` | Revision 3, sequence 3749. Status `review` → `ready` because a new unmet criterion must not sit on a card that looks finished. Priority 1 and severity high unchanged. Five original criteria prefixed `[MET — verified in source, reliability audit 2026-10-13]`. New criterion: during `CHILD_GRACE` the waiter keeps draining stdout and stderr. Logs were not resent. Not moved to `done`. |
| `ft-c0d9313fdcd52966cb834c1c1c85b807` | Revision 6, sequence 3748. Stays `review`, priority 1. One acceptance criterion added: interleaved min-of-N rows for the cache-hit copy, the 2 MiB EOF read versus a line-1 record, the deps scan when every auditor sleeps, and `annotateAround` on a 2,000-pair block. Existing criteria were not marked MET. No second benchmark card. |

## Verified-stale cards

- **`ft-9fa261d9` original five criteria, met in source, tests not re-run.** Cancel and deadline use `CHILD_GRACE` via `ask_then_stop_and_reap`. `notes_listing` returns Err on incomplete (`provenance.rs:780-786`). Blame over the cap sets `truncated` (`git_reader.rs:2087` and `2204`). A broken `.gitmodules` or a failed remote read is an error (`submodules.rs:212-218`, `remotes.rs:197-208`). `ask_shared` runs outside the gate mutex (`git_cli.rs:2716`). The under-5ms timing was not re-measured. The residual hole is the new drain criterion.
- **`ft-31d82cd4`** is the EMPTY-reset fix, in `review`, with reinstall and a 3-hour quiet window still open. Not reopened. The short high-version buffer and the denial-write failure are a different defect and are the new budget card.
- **`ft-e6c6b4c4`** color, external diff, and `--no-ext-diff` are the implemented half. Textconv is still open. Not rewritten.
- **`ft-bdb253cf`** already owns the uncapped manifest reads (`deps.rs` symlink-length check then an uncapped read of the canonical target, and `declared_dependency_sections`). Not touched. Not a coverage-chip card.
- **`ft-b5393529`** owns scanner *output* honesty (non-zero exit, empty stdout, unparseable JSON, npm/bun `{}`). It does not own the scan-level deadline or the Measured chip. Left as-is.
- **`ft-fdfea4c7`** is a pure move of the process seam, after the behavior cards. Not a place to fold the joiner or the permit leak.

## Build order

No two cards in one wave share a file. `git_cli.rs` and `wordDiff.ts` are bottlenecks, so those cards are ordered, not paired with each other. Benchmark rows on `ft-c0d9313f` land with the fix card that they measure. They are not a separate code wave.

- **Wave 1.** `ft-04f5dd26` (coverage.rs, deps.rs, commands/analysis.rs, explorer.ts), `ft-513113a3` (`thread_io.rs` only), `ft-2a2008e9` (windows.rs, conflict_session.rs), `ft-dadcd71d` (`git_reader.rs` only). In-flight flags: the merge-diff card against the Pulse lane, the Windows-mode card against merge teardown. The published collision is `engine/mod.rs`, which none of these edit.
- **Wave 2.** `ft-acff3877` (shared_budget.rs and the `settle_shared` path in git_cli.rs), then the newline card can run beside it: `ft-84a33d12` (wordDiff.ts, patchBuilder.ts, patch_builder.rs).
- **Wave 3.** `ft-9fa261d9` grace drain (git_cli.rs and procguard/mod.rs), after the budget card. Beside it, `ft-02b836b8` (wordDiff.ts and DiffViewer.svelte), after the newline card.
- **Wave 4.** `ft-24f6212d` joiner deadline, after the grace card, alone on `git_cli.rs`. `ft-e6c6b4c4` textconv also edits `git_cli.rs` and cannot share waves 2–4.

## Rejected or not refiled

- **R1-05 textconv.** Already unmet `[H-architecture-13]` on `ft-e6c6b4c4`.
- **R3-02 uncapped manifest reads.** Already `E-integrations-04` on `ft-bdb253cf`.
- **Child timeout restarts after the queue wait.** Documented at `git_cli.rs:3686-3690`.
- **Diff, search, and pipe caps already present.** `MAX_DIFF_BYTES` 8 MiB with `truncated` set. Content search has match, byte, and deadline caps. Word-diff per-line LCS is capped. Pipe drain keeps reading after the cap. Secrets scanning stays fail-closed. Not refiled.
- **Interactive class ignores the shared budget.** By design.
- **Undecodable record resets to EMPTY.** Implemented on `ft-31d82cd4`. The residual is the short high-version buffer, which is on the new budget card.
- **Shared-budget IO under the gate mutex.** Criterion 5 of `ft-9fa261d9`, now marked MET.
- **JaCoCo mapping, XML entity expansion, audit_complete skip, loc-counter symlinks, coverage cross-repo cache.** Already carded or not on this path (`ft-e5aae3b5`, `ft-bdb253cf`, `ft-b5393529`, `ft-9cc9813f`).
- **Clone card from the truncated DevMap sample.** Not filed.
- **Steady-state pipe deadlock, unbounded stdout, timeout reported as empty success, singleflight coalescing, lost shared token, `env` omitted from `SharedReadKey`.** Rejected after source reading. Stdout is capped at 64 MiB and stderr at 4 MiB.
- **`get_range_diff` missing `--diff-merges`.** Different command. Not added.

## Decisions made without asking

- Appended this section. Did not overwrite the Scion/herdr report or the 2026-10-12 report in this file.
- Scratch indexes and MCP payloads live under gitignored `tmp/audit-reliability-2026-10-13/`. The gate rejects heredocs and scripts outside the repo.
- Audit label is `audit-reliability-2026-10-13`, because `audit-2026-10-13` is the Scion/herdr label.
- One scan deadline is one `AUDIT_TIMEOUT` (90s) plus a small grace. Recorded on the analyzer card. A longer budget needs a reason on that card.
- `ft-9fa261d9` moved from `review` to `ready`. Priority and severity were not lowered. Not moved to `done`.
- R1-02 and R1-04 are two cards in two waves because they share `wordDiff.ts`.
- Keyword-overlap `related_tasks_exist` refusals were retried with `reviewed_related` naming every listed id, after title review of each list and a full read of the three cards that could have owned the work (`ft-b5393529`, `ft-31d82cd4`, `ft-fdfea4c7`). None of them own these defects. In-progress cards in those lists (`ft-9cfa4e`, `ft-9d044474`) were named and not overwritten.
- Did not delete, merge, or lower any card. Did not touch `ft-42081f16` or `ft-0ac24164`.
- Did not mark `ft-c0d9313f`'s older criteria MET.
- Model routing: parent verified and grouped. No second model was available.

## Owner decisions before building

None that block the eight new cards. The 90-second scan deadline is already chosen. Fail-open for `LOCK_PATIENCE` and for a full-length newer budget record stays. Raising the scan deadline, or treating a full-length newer record as deny, would be a product change and is not in these cards.

## Gaps

- `gitpulse_get_task` was not called on every open card. Dedupe used the full title list plus full briefs of the cards named above.
- Dead symbols were not queried. Clone results are truncated.
- DevMap call graph is degraded, and Insights disagrees with `devmap status` on freshness. Blast radius from the graph is a lower bound. The defects above were confirmed by reading the cited lines.
- `git apply` was not run for the newline marker. The Windows executable-bit path was not run on Windows. `git show` of a merge was not re-executed.
- Tests on `ft-9fa261d9` were not re-run. The 2026-10-10 review log says they passed on `01462332`.
- The frontend status auditor was still running when this section was written.

**Waiting for your go-ahead before any build work.** The frontend slice, if it returns with confirmed findings, gets a supplement on the board and a short addition here. It does not start a build.
