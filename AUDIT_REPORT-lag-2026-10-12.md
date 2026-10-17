# GitPulse lag audit: performance and macOS (2026-10-12)

This is a read-only audit of `main` @ `a92e40fb`, aimed at the lag. The app is slow, the terminal included, while other apps on the same Mac are fine. The audit covered all eight dimensions: architecture, typing, validation, async errors, testability, performance, correctness and security.

Every confirmed finding is now a card on the GitPulse board. No source, config or dependency was changed. **No build work has started; it waits for your go-ahead.**

`AUDIT_REPORT.md` next to this file belongs to a second audit (correctness and hardening) that another session ran at the same time. This audit did not overwrite it. The two audits share the board label `audit-2026-10-12`. This audit's cards have `gp-audit-*` task ids and `epic-*` labels.

## Method

1. **Orientation.**
   - DevMap: status, search, explore, impact, dead_symbols, clones.
   - GitPulse Insights snapshot.
   - The existing board, then 89 open cards, plus the audits already settled in memory.
   - Runtime evidence from the installed app: process stats, the slow-command log, warnings, and the spawn-budget file.
2. **Sonnet auditors.** Seven slices in two waves (at most four at once): A terminal, B frontend, C Rust background work, D boundaries, E macOS platform, F agents/ledger/ingest, G analyzers/forge/docs.
3. **Opus verification.** Every cited `file:line` was re-read in source. Verdicts are in the rejected list below.
4. **Fable's ruling.** Fable set priority, grouping, sequencing and the Swift/GPU/NPU questions, and personally re-read C1, C7, C2/E1, A1, A2, C6, D1, D6, F1 and A3. Fable and Opus disagreed in five places, listed below. None of them changes scope enough to require asking you.
5. **Filing.** One card per confirmed dossier row, with the epic carried as a label. Two folds went into existing cards. A duplicate sweep followed.

## Where the lag comes from (short answer)

**The lag is not a missing accelerator. It is the app waking itself up and re-running its own work.** The top causes, measured on the installed 1.4.0:

| Cause | Evidence | Card |
|---|---|---|
| The live code index rebuilds back to back. A productive build *clears* its own echo cooldown, so steady agent edits keep `devmap build` running near 100% of the time. | `devmap/live.rs:265-267` clears the cooldown on every `build.ok`; `liveIndex.ts:137-143` rests only 1 s. The devmap child ran at 50–78% CPU with 1.3 GB RSS. `devmap_maybe_refresh` averaged 53 s (max 821 s, n=33). | ft-fa689914 (P0) |
| Every 4 KiB of terminal output becomes one global event: base64, then JSON, then a `format!`-generated JS string evaluated in every webview. | `terminal/mod.rs:111`, `:1543-1547`; `framework/tauri/src/event/mod.rs:199-205` | ft-b93b397e |
| Every PTY reader thread wakes every 100 ms, forever. | `terminal/input.rs:23` `poll(..,100)` | ft-0f696602 |
| Background git and devmap children run at default QoS on P-cores. The only QoS call *raises* the sampler, and App Nap is disabled even when host-load is off. | `git_cli.rs:2111-2113` (an admission label only); `sysload/sampler.rs:317-347`, `probe.rs:136-168` | ft-8db24db8 |
| A tab switch destroys and rebuilds the sidebar, the whole view tree and the status bar, then re-fires every IPC call. | `App.svelte:1159,1170,1220` `{#key currentPath}`; warm-switch settle p50 549 ms | ft-32b92274 |
| The 8/s spawn cap is silently reset to a full burst of 192, because two binary versions disagree on the record size (24 vs 25 bytes). | `shared_budget.rs:150-182`; 138 resets in about 3 h | ft-31d82cd4 |
| Analyzers redo everything on every trigger: insights takes up to 118 s, list_worktrees up to 44 s, language stats hit their 10 s deadline, docs refresh takes up to 50 s, and catch_up takes up to 72 s with only 2 ms spent queued. | slow-command log | ft-c242f35b, ft-28c58695, ft-9cc9813f, ft-c34b8b7f, ft-dd012f8b |

**Parallel cores.** The app already uses many threads: 117 in the main process, a tokio blocking pool of up to 512, and child processes. The problem is the opposite of too little parallelism:
- Background work competes with the UI at the same QoS.
- Some hot loops are serial where they should be bounded-parallel: language stats (G1) and docs refresh (G3).
- The blocking pool itself is unbounded (C10).

The cards fix all three: QoS tiering, bounded-parallel reads, and per-class pool limits.

**GPU.** WKWebView already composites on the GPU (the GPU process is 203 MB), but the terminal uses xterm's DOM renderer.
- The `@xterm/addon-webgl` renderer is filed at P2 as an **owner decision**, because it is a new dependency. It comes only after the terminal benchmark shows what it buys (ft-406d1f4a).
- The translucent window stack (transparent window, vibrancy, a non-opaque webview, a 3-layer translucent terminal) has an unmeasured cost. It gets a measure-first A/B test (ft-cc4be67b).
- The `additionalBrowserArgs` GPU flags in the macOS config do nothing, because they apply to WebView2 only (ft-1f516ac5).

**NPU.** No. Nothing in this workload is a model. The Apple Intelligence bridge stays scoped to task drafting.

**Swift.** No native rewrite or native island this cycle. None of the measured costs live in AppKit or WebKit compositing; they are wakeups, duty cycle, IPC volume and remounts, and moving to Swift would fix none of them.
- Fable's trigger to revisit: after Waves 1–3, the compositing A/B test shows the translucent stack costs more than 15% GPU time on the terminal view.
- If that happens, the change is an opaque native terminal surface, not a rewrite.
- The status popover is fixed in Rust (ft-004c7877), not by moving it to NSPopover.

## Summary

- **35 new cards** across 8 epics, plus **2 folds** into existing cards (B8 and F8, below).
- The table counts one card per row, by its primary dimension and severity, with the 2 folds excluded.

| severity \ dimension | architecture | typing | validation | async-errors | testability | performance | correctness | security | total |
|---|---|---|---|---|---|---|---|---|---|
| critical | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 1 |
| high | 0 | 0 | 0 | 1 | 0 | 9 | 1 | 1 | 12 |
| medium | 0 | 1 | 0 | 1 | 1 | 7 | 1 | 0 | 11 |
| low | 1 | 0 | 1 | 0 | 0 | 5 | 3 | 0 | 10 |
| none (spike) | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 1 |
| **total** | 1 | 1 | 1 | 2 | 1 | 23 | 5 | 1 | **35** |

**Epics** (as labels; the board has no parent link):

| Epic | Label | Done when |
|---|---|---|
| Performance evidence | `epic-perf-evidence` | Every perf card has a committed before/after: interleaved A/B, min-of-N |
| Terminal path | `epic-terminal-path` | About one wakeup per output burst and one IPC per batch; 0 idle wakeups per session |
| Background duty cycle | `epic-background-duty` | No 100% duty cycle; spawn-budget resets stay at 0 over 3 h |
| macOS platform | `epic-macos-platform` | Background work runs at BACKGROUND QoS; App Nap is defeated only while the sampler is on |
| Frontend switch cost | `epic-frontend-switch` | Warm switch settles in ≤150 ms; hydrate is single-flight |
| Analyzers and forge | `epic-analyzers` | Analyzers are incremental and bounded |
| Ledger and ingest | `epic-ledger-ingest` | `ledger_status` ≤100 ms warm; `catch_up` ≤5 s end to end |
| Boundaries and typing | `epic-boundaries` | Webview-supplied paths, refs and payloads are validated at the boundary; errors are typed |

## GitPulse task IDs

Priority runs 0 = urgent to 3 = low. All cards are in **backlog**.

| # | item_id | task_id | pri | sev | wave | depends on | owner decision |
|---|---|---|---|---|---|---|---|
| 1 | ft-fa689914923eabe52b92ff0f39f0831e | gp-audit-live-index-duty-cycle | 0 | critical | 1 | land before/with ft-1f30a461 | — |
| 2 | ft-31d82cd40ccae1033edde59c1c206828 | gp-audit-spawn-budget-record | 1 | high | 1 | — | — |
| 3 | ft-4577fa0ec1f95ea1614c26c4830d82b6 | gp-audit-cow-sync-target | 1 | high | 1 | — | — |
| 4 | ft-28c58695e2ef855fe569058370ce756a | gp-audit-list-worktrees-depth | 1 | high | 1 | — | — |
| 5 | ft-c0d9313fdcd52966cb834c1c1c85b807 | gp-audit-bench-baseline | 1 | medium | 1 | — | — |
| 6 | ft-c242f35b3aceccb7956d8cecc337e70a | gp-audit-insights-snapshot-deadline | 1 | high | 2 | 4 | — |
| 7 | ft-9cc9813fdcd8f54de981941d02962590 | gp-audit-language-stats-incremental | 1 | high | 1 | — | — |
| 8 | ft-c34b8b7f930c63e06f7e99d25b97e969 | gp-audit-docs-refresh-incremental | 1 | high | 1 | — | — |
| 9 | ft-03361c21ade28e47777f58fa398b9664 | gp-audit-ledger-family-cache | 1 | high | 2 | ft-3bbfaed4 landed | — |
| 10 | ft-dd012f8b6f1f8baba3a3e02f3969acba | gp-audit-catch-up-bounded | 1 | high | 2 | ft-3bbfaed4, 9 | — |
| 11 | ft-b93b397e24c2577c657c0271a9ef2473 | gp-audit-terminal-output-channel | 1 | high | 3 | 5; ft-42081f16, ft-0ac24164 landed; lanes off `terminal/mod.rs` | — |
| 12 | ft-0f6966021433cffbd31ea19f8a55ef8a | gp-audit-terminal-reader-kqueue | 1 | high | 3 | 5; same lane as 11 | — |
| 13 | ft-8db24db87ba7de9c2f698ebd51529ac7 | gp-audit-qos-tiering | 1 | high | 3 | ft-5d8b201f; hx/w10-governor landed | confirm the sampler's own QoS stays |
| 14 | ft-32b92274b42bc5c9b55fff995e9a2b26 | gp-audit-tab-switch-no-remount | 1 | high | 2 | 5 | — |
| 15 | ft-5993339a85360dff2439648b57b07d88 | gp-audit-hydrate-single-flight (+B8) | 2 | medium | 4 | 14 | — |
| 16 | ft-cc4be67b4b8cdd4402535818106da361 | gp-audit-compositing-ab | 2 | medium | 2 | 5 | keep glass if cost < threshold |
| 17 | ft-e781a22a88577945c4515e900ae823fa | gp-audit-webview-memory-eviction | 2 | medium | 4 | ft-30d0d11d landed | — |
| 18 | ft-233571a8b215818b1335d90ee4e5f05c | gp-audit-always-on-pollers | 2 | medium | 3 | 5; ft-1f30a461 landed | — |
| 19 | ft-d00811d5000b91a94b92c0daf8188a9a | gp-audit-blocking-pool-bound | 2 | medium | 3 | 1 | — |
| 20 | ft-4183fa71faf7229f4ea6f68fd4cf5a06 | gp-audit-alerts-tiers | 2 | medium | 4 | ft-a2505f1b landed | — |
| 21 | ft-fa9dbbe10ff652a1174fd7c0dd6c1e4c | gp-audit-commit-details-cache | 2 | low | 2 | — | — |
| 22 | ft-67e8a5d9145e1a2dc6a8b81b04d2a920 | gp-audit-ref-name-rules | 2 | medium | 2 | — | **widen the ref allowlist** |
| 23 | ft-5c1313c91c7d799d3ad74f7e5c0b2f40 | gp-audit-typed-ipc-errors | 2 | medium | 4 | ft-3b404cb4; ft-b4eb6688 | **IPC error shape** |
| 24 | ft-406d1f4abaaa7d0af4cf2dbaf9f88c51 | gp-audit-xterm-webgl | 2 | medium | 4 | 11 (numbers first) | **new dependency** |
| 25 | ft-143d3a7709955023f403aee1d7e11e02 | gp-audit-transcript-cursor-shared | 2 | medium | 4 | 10 | **ledger schema migration** |
| 26 | ft-57bd9f3aa32e6018778d45013d649da5 | gp-audit-terminal-small-fixes | 3 | low | 4 | 11 | — |
| 27 | ft-e95bbc830873de7ef7911fdd2757556d | gp-audit-terminal-kill-grace | 3 | low | 4 | ft-42081f16; 12 | — |
| 28 | ft-004c7877067de8643e649beb789a674f | gp-audit-popover-republish | 3 | low | 2 | — | — |
| 29 | ft-1f516ac5e686e8bd0530357134eaa91e | gp-audit-macos-config-dead-flags | 3 | low | 2 | — | — |
| 30 | ft-7eb6cc435d849818f274c5911dfda707 | gp-audit-mailbox-probe-log | 3 | low | 2 | — | — |
| 31 | ft-7450105de62cc0e4a3f0a23e3d9ed25f | gp-audit-git-status-assist-spike | 3 | none | 4 | 5 | **repo-config consent** |
| 32 | ft-0eb00c5c21669ed34e5e9fd2346ef0ca | gp-audit-publish-cost | 3 | low | 4 | 15 | — |
| 33 | ft-cbfe4a735ab4b06c9aca704c6f151af7 | gp-audit-metric-floors | 3 | low | 4 | 7 | — |
| 34 | ft-1b8a7623cced898cbb67770bd17adcb5 | gp-audit-payload-caps | 3 | low | 4 | prefer after 23 | — |
| 35 | ft-fdfea4c754d6647d78e5c005d1d3ddee | gp-audit-git-cli-seams | 3 | low | 5 | **ft-2967fd9d lands first** | **rework size** |

**Folds into existing cards:**
- **B8** went into ft-5993339a (card 15) as a new acceptance criterion. B8: `invoke` has no in-flight join for identical read-only calls.
- **F8** went into ft-845b7f1b (revision 3 → 4) as a new section and acceptance criterion. F8: `workbench.sqlite.pre-schema-11.bak`, 70,860,800 bytes, is unmanaged.
  - That card's own fix creates these backups.
  - Deletion is user-confirmed opt-in only, never a hygiene tick, so it is not a data-loss decision.

## Fable's build order

Within a wave, launch at most about 4 lanes at once. Run `gitpulse_collision_risk` when each lane starts.

- **Wave 1 (start now):**
  - Cards: 1 live-index duty cycle, 2 spawn-budget record, 3 cow-sync target, 4 list-worktrees depth, 5 benchmarks baseline, 7 language stats, 8 docs refresh.
  - Why first: this is the P0 plus the highest payoff per day. Each card except 5 already has an oracle, the slow-command log.
  - None of these touch the files that live lanes currently hold. Card 1 must land before or with ft-1f30a461, which routes more edits into the live index.
- **Wave 2:**
  - Cards: 6 insights, 9 ledger family cache, 10 catch_up, 14 tab switch, 16 compositing A/B, 21 commit details, 22 ref-name rules (after your yes), 28/29/30 small macOS and log chores.
  - Gates: 6 needs card 4. Cards 9 and 10 need `hx/store-reliability` (ft-3bbfaed4) landed. Card 14 is the visible win and has the nav probe to prove it.
- **Wave 3 (terminal and QoS, gated on other lanes):**
  - Cards: 11 output channel and 12 kqueue reader, in **one lane** because they share a file and a thread. Also 13 QoS tiering, 18 pollers, 19 blocking pool.
  - Gates: ft-42081f16 and ft-0ac24164 have landed. Lanes `agent-acbf7b083c38a595e`, `agent-af9707fda247b64c1` and `hx/w10-governor` are no longer dirty on `terminal/mod.rs`. `hx/w10-governor` has landed before card 13, and ft-1f30a461 has landed before card 18.
- **Wave 4 (decisions and follow-ons):**
  - Cards: 15, 17, 20, 23, 24 (only after card 11's numbers and your dependency yes), 25, 26, 27, 31, 32, 33, 34.
- **Wave 5:**
  - Card 35, git_cli seams, after ft-2967fd9d lands.

Today's collisions:
- `terminal/mod.rs` is dirty in 3 lanes.
- `sysload/procscan.rs` is dirty in 2 lanes.
- `sysload/sampler.rs` and `git_cli.rs` are dirty in `hx/w10-governor`.

No Wave 1 card touches any of these files.

## Top 10 risks (Fable)

1. **Live-index duty cycle (1).** Fixing the cooldown without changing the trigger policy in `liveIndex.ts` leaves the loop re-armed from the frontend. Both halves must ship together, and ft-1f30a461 will add more triggers.
2. **Spawn-budget record (2).** A stale *installed* hook or MCP binary keeps writing the old layout. "Done" therefore means rebuild, reinstall and `mcp:doctor`. Otherwise the flip-flop comes back.
3. **Terminal output channel (11).** Three lanes hold `terminal/mod.rs`. Also, verify the Channel semantics of the vendored, patched tauri before designing around them.
4. **QoS tiering (13).** A user-initiated git write that is mis-classed as background would starve at BACKGROUND QoS. The admission class must be the single source of truth, and the sampler's accepted 10-minute hidden-soak criterion must not regress.
5. **Tab switch without remount (14).** Removing `{#key}` exposes every effect that relied on a remount to reset its state. It needs the real-DOM harness, because vitest never runs `$effect`.
6. **catch_up bounded (10).** A deadline that cuts consolidation mid-page must not report partial results as complete. Carry both numbers.
7. **cow_sync target (3).** Validating with the full worktree scan would create a new 8 s command. Use the porcelain listing only.
8. **Insights snapshot (6).** Sharing one `git status` across three consumers changes collision semantics. It needs a test that an edit made mid-snapshot is still reported.
9. **kqueue reader (12).** EVFILT_PROC can miss NOTE_EXIT on a reparented or zombie child; macOS returns EPERM on an all-zombie group. Keep a slow `waitpid` fallback and confirm idle wakeups are 0.
10. **Benchmarks baseline (5).** On a loaded host, the noise hides the fixes. Interleaved min-of-N is mandatory, and each run must record the host-load snapshot alongside it.

## Opus/Fable disagreements (none changes scope materially, so you were not asked)

- **F1** was narrowed in two places:
  - The busy-timeout sub-claim is refuted: rusqlite 0.40.2 sets busy_timeout 5000 in `Connection::open` (`inner_connection.rs:118`). Fable and Opus both confirmed this.
  - The reflog sub-claim is already fixed on `hx/store-reliability`.
- **C2/E1 part (c)** is accepted design on ft-5d8b201f, not a defect: the sampler's own USER_INITIATED QoS plus its App Nap assertion. Card 13 fixes only (a), BACKGROUND QoS for background work, and (b), the Activity held while host-load is disabled.
- **C7** was raised from medium to high severity, because it defeats the spawn cap that every other host-protection card relies on.
- **A3 (WebGL)** was lowered from P1 to P2: it is a new dependency with no measurement yet.
- **B5+A6**, "contributors to the 975 MB WebContent", stays *inferred*. Card 17 must measure before it evicts anything.

## Rejected findings

| Finding | Reason |
|---|---|
| B11 `$state` deep proxy cost | A lead only; no site was inspected. |
| B12 CSS animation inventory | No defect. Compositing is already limited (`app.css:999-1033`), and the terminal chain has no `backdrop-filter`. |
| E7 menu no-op short-circuit | Already gated (`desktop/mod.rs:574-590`). |
| E8 ACL follow-up | ft-74adbb3d's five acceptance criteria are already met in review. Not reopened. |
| E9 custom allocator | No profile evidence, and it would be a new dependency. |
| G6 commit graph lacks a commit-graph file | Contradicted: this repo has `.git/objects/info/commit-graphs`. |
| F1 sub-claim: no `busy_timeout` | Refuted: rusqlite sets 5000 ms by default. |
| F1 sub-claim: reflog replay, one transaction per entry | Already fixed on the ft-3bbfaed4 branch. |
| C2/E1 (c): sampler USER_INITIATED plus App Nap assertion | An accepted acceptance criterion on ft-5d8b201f. |
| Dead-code dimension | DevMap returned 415 candidates, all at confidence ≤0.66 from a partial walk. The 5 "dead" clusters are injectable timer seams (DI), not dead code. |
| Two `manvi serve` children | By design: separate profile and policy lanes. |
| C12, D10 (gate extraction and injectable clock; clone groups) | Duplicates of ft-2967fd9d, which is in progress. Carried as notes on card 35. |
| E5 | Absorbed into A1 (card 11). |

## Assumptions made without asking (log)

1. **The evidence comes from two different builds.**
   - Runtime evidence (process stats, slow-command log, warnings, spawn-budget file) is from the **installed 1.4.0, built Oct 11, about 1 day behind `main`**.
   - Code evidence is from `main` @ `a92e40fb`.
   - Each card cites both where they differ. Line numbers should be re-checked before editing.
2. **D1, cow_sync with an unvalidated target, was not stopped for.**
   - It is a security finding, but the fix only *narrows* access: the target must be one of the repo's worktrees.
   - The exposure needs a compromised webview.
   - There is no product choice to make, so it was filed at P1 rather than raised as a question.
3. **F8 is not a data-loss decision.** The card requires opt-in, user-confirmed deletion of the `.bak` and never automatic sweeping.
4. **D6 and D3 were filed now as owner decisions, not held back.** D6 widens the ref allowlist; D3 changes the error shape. Neither blocks any other card, and both cards carry a recommended answer, so the decision is a yes or no.
5. **The B8 fold target was changed from Fable's ruling.** Fable assigned B8 to ft-3b404cb4. That card's work is already merged on `main` (`4800a3db`, checked with `git merge-base --is-ancestor`) and the card is in **review**. Fable assumed it was still open. Adding scope there would block it from closing, so B8 went to card 15, which already proposed keyed single-flight in `invoke`.
6. **The report file is separate.** The `AUDIT_REPORT.md` that already existed is the concurrent correctness and hardening audit's deliverable. It was not overwritten; this file is the lag audit's report.
7. **The concurrent session touches this audit's cards.** It shares the label `audit-2026-10-12`.
   - It appended findings to ft-dd012f8b, ft-c0d9313f and ft-4577fa0e, all now at revision 2. This audit's content was preserved, as verified by `gitpulse_get_task`.
   - Overlaps that should be coordinated rather than merged, each noted in the card:

     | This audit's card | Concurrent session's card |
     |---|---|
     | card 23 | ft-b4eb6688 (typed RunError) and ft-46bece0d (typed MCP args) |
     | cards 15 and 32 | ft-1629347b (repoStore mutation contract) |
     | card 8 | ft-e023906b (doc rename) |
     | card 35 | ft-ade6e999 (injectable clock) |
     | card 10 | ft-bdb253cf (bounded input), whose transcript streaming was folded into card 10 |

   - The duplicate sweep across the backlog, inbox and ready columns found no duplicates.
8. **DevMap gaps.**
   - The index (gen 4080/4081) is *degraded*: call extraction is partial, and 97 files are not fully parsed.
   - Most impact walks returned `walk_incomplete`, so every blast radius on the cards is a lower bound. Each defect was confirmed by reading the source instead.
   - The `gitpulse_insights` collision facet was `truncated`, with 2 of 18 worktrees unscanned. "Clear" therefore means clear in the inspected lanes only.
9. **Two stale-looking cards were left untouched.**
   - **ft-82a19393** says "WebGL context-loss", but the graph canvas is Canvas 2D (`gpuContext.ts`).
   - **ft-e6c6b4c4**: git env pinning looked implemented at `git_cli.rs:319-425`. The concurrent audit has since added new criteria to it (textconv, locale), so it may no longer be stale.
10. **No benchmark was run during the audit.** Performance numbers come from the slow-command log and process sampling, not from a committed benchmark. Card 5 creates the benchmarks.
11. **A bad id was entered once.** A fabricated id, `ft-a2505f1b9c4f5a0d6f3d0d6a5c1a2b3e`, went once into a `reviewed_related` list. It is harmless: it only acknowledges a related card, and nothing was written to it. The real ft-a2505f1b is still unchanged.
12. **Board conventions used.** Priority 0 means highest, matching the board's convention (ft-f8a297ff is 0 and critical). The spike card uses severity `none`.

## Owner decisions (resolved 2026-10-12)

The owner gave the go-ahead for Wave 1, then Wave 2. The owner's rules were:
- New dependencies are approved where they are better.
- Translucency goes opaque if its cost is significant. Any glass that stays uses Apple's own implementation.
- Fable rules on everything else.

Each ruling below is recorded on its card, which is now labelled `owner-decided`.

| Card | Ruling |
|---|---|
| 22 ft-67e8a5d9 | **Approved.** Git's check-ref-format rules are reimplemented in Rust, with no spawn per validation. They are cross-checked in tests against `git check-ref-format --branch`. A leading `-` is still rejected, and `]` is now allowed. |
| 23 ft-5c1313c9 | **`IpcError { kind, message, detail? }`, with no specta.** tauri-specta supports Tauri 2 only as an rc, and the registry `tauri` would conflict with `framework/tauri`. `From<String>` maps to `kind: "unclassified"`, so an unknown error fails closed. This card is hard-gated on ft-b4eb6688. |
| 24 ft-406d1f4a | **Dependency approved only if it is better.** It must cut WebContent main-thread CPU by at least 2× on the TUI-redraw bench, with nothing else worse. The canvas addon is not an option for xterm 6. |
| 25 ft-143d3a77 | **Goal approved; the re-key is rejected.** A family-wide pass reads each file once and fans lines out to worktrees. There is no schema migration. |
| 31 ft-7450105d | **Never write repo config, and no `-c` for these keys.** Under `GIT_OPTIONAL_LOCKS=0`, the untracked cache has no effect, and fsmonitor would start a stray daemon. The spike is re-scoped to a scratch clone and a possible Health hint. |
| 35 ft-fdfea4c7 | **Process seam only, plus the one call that breaks the reader↔cli cycle.** The fs_sandbox and validate moves become a follow-up card. The gate is narrowed to ft-2967fd9d's remaining `engine/git_cli*` work. |
| 13 ft-8db24db8 | **The sampler keeps USER_INITIATED.** Its Activity runs only while host-load is enabled. New acceptance criterion: sampler CPU below 1% of one core and tick p95 below 50 ms over the soak. Correction to Fable: hx/w10-governor's work is still uncommitted, so this card's dependency is still open. |
| 16 ft-cc4be67b | **Three-arm A/B: current, fully opaque, and Apple-material-only (NSVisualEffectView via Tauri `windowEffects`).** "Significant" means p95 frame time at least 15% worse, or GPU+WindowServer CPU at least 15% and 5 pp of a core worse, on any scenario. Glass comes only from Apple's material, never CSS alpha. Throttle the main window and Suspend the popover. |

## Landing status (updated 2026-10-12, evening)

All merges are local to `main` and **not pushed**. Reinstalling the app and binaries is deferred at the owner's request, so every runtime acceptance criterion is still unverified: slow-command log windows, `mcp:doctor`, and the `spawn-budget.v2` string check on installed binaries.

### Wave 1

| Card | Branch → merge | Board | Verified on merged `main` |
|---|---|---|---|
| 1 ft-fa689914 live index | `hx/lag1-live-index` → `d8c3df7d` | review | cargo devmap::live/cli 163 passed; vitest; svelte-check |
| 2 ft-31d82cd4 spawn budget | `hx/lag1-spawn-budget` → `fbdc6bd1` | review | cargo shared_budget 20 passed (record v2) |
| 7 ft-9cc9813f language stats | `hx/lag1-language-stats` → `1798e00e` | review | cargo language/git_reader 296 passed; sandbox 18 passed |
| 8 ft-c34b8b7f docs refresh | `hx/lag1-docs-refresh` → `a3f34e81` | review | cargo docs:: 22 passed |
| 3 ft-4577fa0e cow-sync, 4 ft-28c58695 listing depth | `hx/lag1-worktrees` → `2dcafd37` | review | cargo cow_clone/worktree/commands/agent_worktree: 124 passed; acl_contract: 12 passed; vitest 500/500; check:ipc OK; svelte-check 0 errors |
| 5 ft-c0d9313f benchmarks | `hx/lag1-bench-baseline` → `ac08a8af` | review | all 8 benches smoke-run exit 0 on merged main; comparator self-test 20/20 |

### Wave 1 leftovers

- `sysload/snapshot.rs:5` still names `spawn-budget.v1`. The live `hx/w10-governor` lane holds that file.
- `workbench/agent_worktree.rs:237` only logs `cache_error`. `hx/w10-governor` holds that file too.
- target/ clone (card 3): GitPulse's own Cargo workspace is `src-tauri/`, so the anchor-level `target/` clone never fires for this repository. The rule still applies to repos with a root workspace.
- Main's build outputs reference `.claude/worktrees/seed/` (9 rustc-link-search lines). Removing the `seed` worktree would break incremental builds until a clean rebuild.
- `0ae2caca` on `worktree-agent-a25f8a16de7349fe0` is superseded by card 2. Do not merge it.
- **Unmerged parallel branch `hx/perf-hardening`** (from another session; last commit today at 15:58) changes the same files as Wave 1: `devmap/live.rs`, `devmap/cli.rs`, `shared_budget.rs`, `git_reader.rs` (language-stats rescan), `docs/mod.rs`, `engine/worktree.rs` (clean worktrees skip the diff) and others. 28 files, +2404/−281. It is not an ancestor of `main`. Merging it now would conflict with, and partly duplicate, cards 1, 2, 4, 7 and 8. It needs reconciling by its owner before it lands.

### Wave 2

| Lane | Cards | Branch → merge | Board | Verified on merged `main` |
|---|---|---|---|---|
| Ref names | 22 ft-67e8a5d9 | `hx/lag2-ref-names` → `e63ebe06` | review | git_writer/deadbranch/git_reader/policy/sidecar: 317 passed |
| Commit details | 21 ft-fa9dbbe1 | `hx/lag2-commit-details` → `2cbb57b1` | review | git_reader/ref_cache/budget/git_writer: 242 passed; vitest 50 passed; svelte-check 0 errors; check:ipc OK |
| Ledger/ingest | 9 ft-03361c21, 10 ft-dd012f8b, 25 ft-143d3a77 | `hx/lag2-ledger-ingest` → `e3fca07b` | review | **full lib suite 3395 passed, 0 failed**; bins passed; check:ipc OK |
| Insights | 6 ft-c242f35b | `hx/lag2-insights` → `c600b5f4` | review | insights/worktree/repo_op/hooks/mcp/harness/ledger/ingest/commands: 758 passed; repository_trust 22, worktree_concurrency 7 |
| macOS chores | 28 ft-004c7877, 29 ft-1f516ac5, 30 ft-7eb6cc43 | `hx/lag2-macos-chores` → `fe5cab34` | review | desktop/local_socket 119 passed; acl/desktop/menu integration 33 passed; vitest 37 passed |
| Tab switch | 14 ft-32b92274 | `hx/lag2-tab-switch` → `0eedb8af` | review | full vitest 9863 passed (only serve-ops fails); svelte-check 0 errors; harnesses tab-switch 79/79, terminal 118/118, uncommitted 128/128 |

### Regressions the merges caused, and their fixes

Each lane's own tests were green, but the merges together broke repository-wide policy tests. Targeted per-lane runs never execute those tests; the full suite on merged `main` caught them. Every one is fixed:

| Test | Cause | Fix (merge) |
|---|---|---|
| `tests/supervised_threads.rs` | Raw threads in `devmap/live.rs` (card 1), `ingest/flight.rs` (card 10) and `engine/git_reader.rs` (card 7) | The live build and catch-up workers start through `supervisor::spawn_once`. A panicking build frees the slot and records a failure. The scoped language-scan helpers get a documented `ALLOWED` entry (`16d5eba5`). |
| `scripts/documented-counts-contract.test.ts` | New wire fields | CONTRIBUTING.md and ARCHITECTURE.md now say 1661 fields (`16d5eba5`). |
| `tests/hermetic_contract.rs` | Raw wall-clock asserts in `live.rs` and `docs/mod.rs` | Both bounds go through `coverage_relaxed` (`98eb36b2`). |
| `tests/devcouncil_auto_init.rs` | Two tests raced for card 1's single live-build slot, which is process-wide by design. The code was right; the test lacked isolation. | The tests are serialized on a file-local mutex (`98eb36b2`). |
| `tests/stress_test.rs` | The injection suite encoded the old shell reject list | Re-aimed at git's rules. The four names that became legal are proven inert by execution: a canary file plus a positive `sh -c` control (`98eb36b2`). |

| `branches` browser harness | Card 14's reset closed an open merge dialog, bypassing the dialog's own "repository changed" guard. Before the merge (`16d5eba5`) the harness passed 102/102; after it, 91/92. I first labelled this "likely pre-existing", which was wrong; the advisor caught it. | The reset no longer closes the dialog (`bd04176e`). |

**Final state of `main` (`bd04176e`):**
- **Rust:** full suite (`cargo test --no-fail-fast`, 108 binaries) **4628 passed, 0 failed**, measured at `98eb36b2`. Since then only Svelte and harness files have changed.
- **Frontend:** full vitest **9863 passed**. The one failure, `serve-ops-contract`, predates this work (Manvi sibling drift).
- **Browser harnesses:** `test:browser:all` **32/32**.
- **Checks:** svelte-check 0 errors; `check:ipc` OK.
- **CHANGELOG:** `[Unreleased]` covers every merged card.
- **This report:** untracked (`??`) and not committed.

Card 16 (the compositing A/B) is deferred. It needs installed app builds, and the reinstall is on hold.

Ruling made during review:
- **Ref names:** C1 control characters (U+0080–U+009F) are still refused, as a second documented rule stricter than git. xterm.js treats them as controls; U+009B is CSI.
- **Insights:** the card's "warm repeat spawns 0" status cache was refused. A HEAD or index stamp cannot see a working-tree edit, so the cache would report clean right after the edit the collision facet exists to catch.

### Bench evidence (`command_timings`/`live_index` smoke, generated 18-worktree fixture)

Work counters are deterministic and comparable across runs. Wall-clock times are not: the baseline host was at 1.3–2.4× load and the post-fix run at 0.4×.

| Row | Baseline `a92e40fb` | Merged `main` `c600b5f4` |
|---|---|---|
| Live-index storm (40 ticks) | 40 builds, duty cycle 1.00 | **1 build, duty cycle 0.04** |
| Insights snapshot | 37 git processes, 485 ms | **21 git processes**, 148 ms |
| Commit details | 2 git processes, 16 ms | **1 git process**, 0.13 ms (cache hit) |
| catch_up steady / cold | 36 / 76 ms | 30 / 57 ms |
| Language stats | 1 git process, 55 ms | 2 git processes (`check-attr` added), 11 ms |
| list_worktrees, `ScanDepth::Full` | 39 git processes | 38 git processes |

Follow-ups:
- The list_worktrees row benches the Full opt-in path. Card 4's default path (`Listing`) has no row yet.
- ~~The list_worktrees row benches the Full opt-in path.~~ Done in `43f54d7f`: the bench now has `list_worktrees full` (38 git processes) and `list_worktrees listing`, which is card 4's default path (1 git process, about 22 ms).
- ~~The committed baseline is pre-fix.~~ Re-seeded from `43f54d7f` in full mode in `97b9b2f0`. The pre-fix numbers stay in `docs/PERFORMANCE.md` as history.

### Waves 3–5 (owner: "complete all waves")

Fable's sequencing ruling, with owner answers given 2026-10-12:
- **Terminal work.** Lanes build on `hx/integrate-all` but do not land it. That branch is `hx/tio-final` merged with main.
- **Card 23.** Held and reported as blocked; ft-b4eb6688 is not pulled into scope.
- **`hx/perf-hardening`.** Left to its owner.

| Lane | Base | Cards |
|---|---|---|
| `hx/lag3-small` | main `97b9b2f0` | 20 alerts tiers → 34 payload caps (boundary-only) → 33 metric floors → 31 git-status spike |
| `hx/lag3-frontend` | main | 17 WebContent memory + 26-A10 → 15 hydrate single-flight → 32 publish cost |
| `hx/lag3-pollers` | main | 18, Rust half: watcher, mailbox, external_changes; 19 blocking pool, plus card 13's rayon slice |
| `hx/lag3-terminal` | `hx/integrate-all@639474c3` | 12 kqueue reader → 11 output Channel → 27 kill grace → 26-A7 watchdog wake. Merges to main only after integrate-all lands. |

Held, with what holds each:

| Card | Held until |
|---|---|
| 13 QoS | `hx/w10-governor` (git_cli.rs, sampler.rs), `hx/tio-final` (procguard) and `hx/perf-hardening` (devmap/cli.rs) land. Every planned file is held by one of them. |
| 35 git_cli seams | Governor lands, and ft-2967fd9d's preflight pair lands. |
| 23 typed IPC errors | ft-b4eb6688 (typed RunError) exists. Owner chose to hold. |
| 24 xterm WebGL | Card 11's numbers exist, card 16 settles `allowTransparency`, and a real-window bench is possible. |
| 16 compositing A/B | The code half (rAF probe, the three arms as flags, background throttling) can be built. The measurement needs a window with the installed 1.4.0 quit. |
| 18, remaining slices | The frontend half lands with perf-hardening, which already has it in `9080cfd2`. The git_cli child-wait waits for governor. alerts/bridge.rs waits for perf-hardening. |

Review corrections: card premises that were wrong, caught at Opus review of `hx/lag3-frontend` (`c52899ce`) and sent back:
- **Card 15, quiet tier.** The audit assumed the live watcher covers file edits, and asked for "idle repo with live watch ≤ 1 poll per 60 s". That assumption is false.
  - The worktree watch is non-recursive (`watcher/debouncer.rs:65-70`, `:151-154`). An unstaged nested edit writes nothing under `.git`, so the status poll is its primary detector.
  - A 60 s quiet tier would delay ordinary edits from 6 s to up to 60 s, so the AC is retired. The cost-adaptive rest, max(6 s, 10 × last), stays and meets the duty-cycle AC.
- **Card 15, B8 join.** "Join identical in-flight reads" is unsound as written: a read asked after a write or watcher event could join one started before it and return pre-change state.
  - The backend's `run_read_shared` is sound only because it shares reads that have not started.
  - The fix is a join generation, bumped by every non-allowlisted command (at dispatch and at settle) and by watcher events.
- **Card 26-A10, lineHeight.** The "fractional → blurry rows" premise is false: xterm floors cell height in device pixels, so the AC already held at 1.15. The change to 1 (13% denser) was reverted, and the AC is recorded as already met.
- **Card 26-A10, Option-as-Meta.** On by default as the card asks, but now a setting: it blocks Option-typed glyphs on non-US layouts.
- **Card 32.** Closed as a no-op under its own escape clause: 50 tabs cost about 6 µs more per publish than 1 tab.
- **Disk.** Parallel lanes drove the volume to 257 MB free, and the pollers lane failed with ENOSPC before writing code. A stale 59 GB `tmp/w1-target` was removed, and lanes now stop below 8 GB free.

Wave 4 landing (main, not pushed):

| Merge | Lane | Cards |
|---|---|---|
| `f2e5d2e9` + `636c566a` | `hx/lag3-small` | 20, 34, 33, 31 (spike: no Health hint; fsmonitor 1.21–1.38x slower on 1k–3k-file repos, about 0.56x on 100k) |
| `de14913b` + `af334802` | `hx/lag3-frontend` | 17, 26-A10, 15, 32 (no-op). `af334802` injects the shared-read list from `main.ts`, so the popover ACL scan stays exact. |
| `be7325f3` + `7da0f9ce` | `hx/lag3-pollers` | 18 Rust half, 19, 13 rayon slice. tokio declared at `=1.53.1`, with no new package. |
| `fd05ba07` | `hx/lag3-cache-evict` | Follow-up from the Wave 2 ledger merge: LRU instead of clear-all at 64 anchors and 256 families, and a scoped test reset. |

Bench evidence on `be7325f3`/`af334802`, smoke run at load 15–23 on 18 CPUs:
- **Counters.** Every work counter is identical to the `97b9b2f0` baseline (command_timings, repo_read, watcher, idle_wakeups). The time flags (insights 1.84x, language 2.13x, get_status 1k 4.29x) are load noise: 1k clean came out slower than 1k dirty, and the 10k rows were faster than the baseline.
- **Idle wakeups, interrupt wakeups per second.**

| Row | Before (lane's unfixed run) | After |
|---|---|---|
| idle mailbox | 4.65 | 2.65 |
| 8 idle watches | 39.9 | 2.65 |
| 24 idle watches | 117.3 | 2.66 |

The process floor is 2.66, and the session rows are unchanged.

Regressions caught on merged main, with their fixes:
- **acl_contract popover scan.** `sharedReads.ts` literals were reachable from `status.ts`. Fixed by injection.
- **architecture-docs tokio claim.** The docs and the contract were rewritten to pin the exact version.
- **Spike script typecheck.** It had 40 TS7006 errors. Fixed by adding JSDoc types.

The terminal lane is done but NOT MERGED, by owner decision: `hx/lag3-terminal@eb4d4a9f` sits on `hx/integrate-all@639474c3`.

| Card | Commit | Result |
|---|---|---|
| 12 | `3d1c9bae` | The reader waits on kqueue and epoll, with 0 idle reader wakeups per session. |
| 11 | `3b1cf774` | Raw-byte Channel: 16 messages per MiB, 64x fewer than the audit's state. Hidden sessions get 18.75x fewer. |
| 27 | `34b1524a` | Close waits on NOTE_EXIT: 2 process-table reads instead of 32, and the child mutex is free during the wait. |
| 26-A7 | `1fba50fe` | The watchdog is woken only when parked: 1 wake instead of 50. |

Unverified on the terminal branch:
- The Channel fetch path for payloads of 1 KiB or more, which needs a real webview.
- Linux at runtime.
- The `terminal_pty` bench, which measures the legacy events sink, not the Channel.

Landing it on main means a doc-count recount (main has 1663 fields, the branch 1666) and a CHANGELOG merge. There are no code conflicts.

De-flake lane merged at `3218d885`:

| Test | Cause | Fix | Before → after |
|---|---|---|---|
| daemon_lock | A real production bug: a fork on another thread kept the flock's open file description, so a dropped lock still read as held. | Explicit `LOCK_UN` | 9/40 → 0/40 |
| gh probe | The thread-local forced-failure seam was bypassed by the process-wide `GH_PROBES` cache. | Cache bypass while the seam is active; test-only | Fails pre-fix → passes |
| output writer record | Writers share a subsystem name, so one writer's clean exit cleared another writer's panic record. | `spawn_one_of_many` | 2/10 → 0/10 |

Still flaky, filed as `ft-626b25fc`:
- `desktop::instance` broken-pipe test (2/10, and also seen pre-fix).
- `alerts::bridge` cross-test socket state (1/10).
- Five other shared-name supervised threads; whether to move them to `spawn_one_of_many` is a Diagnostics-semantics decision.

Final main `3218d885`, verified 2026-10-13:
- **Rust:** 4691 tests passed, 0 failed, 36 ignored, across 95 unfiltered result lines. All 94 `tests/*.rs` targets ran. The suite also prints 13 child re-exec summaries. Earlier "109 binaries" totals summed those lines too, which is why the count looked like it dropped to 108.
- **vitest:** 9944 passed. The only failure is `serve-ops-contract` (Manvi sibling drift, known).
- **Static checks:** `npm run check` gave 0 errors and `check:ipc` was OK.

- **Browser:** `test:browser:all` passed 32/32 harnesses on `3218d885`.
- **Benches, full mode at load 0.3x on `3218d885`:**
  - `command_timings`: every row is inside the gate. The smoke-run flags on insights, language and catch_up are gone.
  - `repo_read`, `get_status 1k dirty`: 42.90 ms (1.80x) on one run, then 24.82 ms (1.04x) on an immediate re-run.
  - Counters are identical to baseline throughout, and no Wave 3–5 change touches `get_status`. Calling this timing noise is INFERRED, not verified.

Earlier intermittent full-suite failures, now handled by the `hx/lag3-deflake` lane:
- `daemon_lock` second-daemon test: 3 of 9 full-lib runs.
- `github` probe test: the forced failure is thread-local, but `GH_PROBES` is process-wide.
- `output` panicking-writer test: 2 of 4 runs.

Overlaps:
- `lane/catchup-scan`'s uncommitted `scan_bench.rs` calls `read_range`, which card 10 deleted. It must move to `lines::LineReader`.
- `hx/w10-audit-fixes` has uncommitted edits to `ledger/mod.rs` that will conflict with card 9.
