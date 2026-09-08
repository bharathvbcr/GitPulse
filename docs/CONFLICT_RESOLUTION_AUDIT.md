# Resolve and merge conflict audit

Local audit: 2026-09-08. Checkout: `main`, base `a4a1d33`.

Scope: Resolve UI, parser, draft lifecycle, native save/staging transaction,
and the continue-operation integrity check. The checkout also contains
concurrent changes outside this scope. No commit, release or installation is
implied by these results.

## Findings and fixes

| Verified defect or gap | Delivered behavior | Evidence |
| --- | --- | --- |
| External edits or restarted operations invalidate loaded choices. | Native snapshots bind choices to the repository, operation identity, exact index stages, working bytes and Git mode; revalidation follows filters. | Stale source, abort, identical merge restart, changed stages and filter-driven external edit tests. |
| Writing and later staging could select different bytes; an index lock could leave an unexpected partial save. | A private index is prepared under Git's index lock before working-entry replacement. Publication and cleanup check lock ownership; replacement locks are preserved. Unrelated staged entries remain intact. | Lock refusal/replacement, exact stage-zero blob, unrelated staging, eight simultaneous saves with one winner. |
| Ancestor links and replacement races could redirect writes. | Unix I/O uses a pinned parent directory and no-follow operations. Displaced source is retained if publication fails. | Symlink-ancestor refusal, pinned-parent replacement and deterministic publication failure. |
| The gate judged `git add` while different commands executed; file authorization always declared modification. | The canonical mutation owner builds argv once for both judging and execution. Separate file authorization derives create/modify/delete from the fresh source and selected resolution. | Hash-object/update-index and create/modify/delete/staged-deletion denial tests preserve the working file and index; command/file-fidelity contracts. |
| Malformed markers could be finalized. | Previews retain damaged source; saves reject malformed/leftover markers, forged counts and chunk identities. | Before/after regressions for unclosed, nested, duplicate, mismatched and pasted markers. |
| Diff3 previews reordered source; a selected blank physical line disappeared. | Base/current/incoming order and physical EOL information are preserved. | Regressions, mixed EOL tests and 392 marker-width/content/EOL round trips. |
| Clean filters could insert markers after a clean preview. | The prepared index is checked before working-file replacement, respecting Git's configured marker width. | A marker-producing clean filter is refused with the original file and conflict stages intact. |
| The text-only flow missed binary, invalid UTF-8, executable, link, deletion and gitlink cases. | Whole-side choices preserve Git objects/modes. External resolution and staging retries preserve bytes without rewriting. Gitlinks update only the recorded commit. Already-present content reports no working-file write. | Binary/non-UTF-8, mode, symlink, deletion, missing file, submodule, CRLF/filter, SHA-256, linked-worktree and unchanged-content timestamp tests. |
| View changes or new sources discarded drafts. | A process-lifetime owner retains choices/history; local storage enables reload recovery. Changed sources are archived and never automatically reapplied. | Remount/reload, repository identity, corrupt/duplicate storage and changed-source regressions. |
| Older callbacks could clear or attach receipts to newer choices. | Completion and receipt transitions compare the saved revision and state. Accepted saves survive unmount through the shared save queue. | Stale completion/receipt tests and browser unmount-during-save checks. |
| File-editor updates cleared conflict drafts from quit protection. | The unsaved registry tracks editor owners independently; quit waits for accepted saves and flushes recovery. | Registry collision regression, quit wiring and queue contracts. |
| Unbounded metadata, custom lines, history and lists increased resource risk. | Native budgets, bounded recovery, 25-block pages and complete plain-text fallbacks bound work. Limit failures preserve existing edits. | Oversized labels/flags/custom lines, 2,001-block refusal, cache/disk/memory saturation and large comparison cases. |
| Bulk actions, recovery and completion lacked clear controls. | Undo/redo, preserve-existing bulk choices, explicit replace-all, keyboard navigation, draft export, lazy recovery, binary choices and final staged review. | Production-component checks in Chrome and WKWebView. |

The old `conflictSave` planner and its three tests were retired together (79 lines removed). Its
former caller now consumes native structured outcomes; success, refusal and
partial staging are checked through the active native and browser paths. No
dependency or coverage threshold was added or relaxed.

## Canonical owners

- `src-tauri/src/diff/conflict.rs`: parser, diagnostics, budgets and marker validation.
- `src-tauri/src/diff/conflict_session.rs`: source identity, private index, exact command gating, filters and publication outcome.
- `src-tauri/src/diff/conflict_fs.rs`: pinned-directory I/O, replacement and displaced-source recovery.
- `src/lib/diff/conflictSession.ts`: draft persistence, history, receipts and registry ownership.
- `src/lib/components/ConflictEditor.svelte`: lifecycle, decisions, preview, recovery and staged review.
- `src/lib/components/ConflictComparison.svelte`: source comparison, line numbers and synchronized scrolling.

## Local verification

- 5,083 frontend tests passed across 389 files. Coverage: 93.99% statements,
  88.56% branches, 95.55% functions and 95.98% lines. All frontend thresholds passed.
- Chrome and system WKWebView each passed 43/43 production-component checks.
  Default dark desktop and 600×700 light layouts were visually inspected.
- The final instrumented Rust workspace run passed 2,011 tests across 57
  suites, with zero failures and nine opt-in checks ignored. Rust line coverage
  is 84.31% (51,338/60,890), above the unchanged 80% floor. Focused coverage includes 31
  real-repository save tests, nine parser/budget/corpus tests, pinned-directory
  and publication-failure/lock-ownership unit tests, and existing operation/stress suites.
- IPC: 187 handlers, 181 invoked commands, 209 call sites; no missing/orphaned
  handlers. Types: 50 contracts, 123 structs, 881 fields; no drift.
- Svelte/TypeScript: no errors or warnings. Production Vite build passed;
  its existing large JavaScript chunk advisory remains.
- Rust formatting and warnings-as-errors Clippy passed.
- `git diff --check` and the coverage integrity/floor checker passed. The
  measured checkout includes concurrent changes; aggregate coverage is not
  a claim that every Resolve branch or supported platform was exercised.

Raw before/after and validation logs were captured under
`/tmp/gitpulse-conflicts-*.log`. These temporary logs are not release artifacts.
One instrumented full run failed the existing subprocess-latency test: measured
overhead was 15.13 ms against an 8 ms allowance under concurrent load. It passed
in isolation without changing the assertion; final native validation limits
concurrency to four test threads.

## Bounds and remaining verification gates

| Surface | Bound or status |
| --- | --- |
| Interactive source | 4 MiB UTF-8, 100,000 physical lines, 2,000 conflicts. Larger inputs have explicit whole-file/external options. |
| Preview/render | 8 MiB content/output; bounded lines/EOL metadata; capped diagnostics identify omitted findings. |
| Native file/index | 16 MiB file read/write; 64 MiB index snapshot. Larger files require external Git tooling. |
| External staging | Text may exceed the interactive limit within the native file bound; marker validation avoids building the editor representation. Binary/link bytes are preserved. |
| Comparison | Word highlighting: 40,000 characters/200 lines, with 2,000 characters per compared line. Larger blocks retain a complete plain-text view. |
| Recovery | At most 64 file records and eight previous source versions per file. Dirty records are not silently evicted. History: at most 50 steps, with older undo data pruned at a 1 MiB serialized budget. |
| Custom/storage data | A custom block is capped at 1,048,576 UTF-16 code units; additional state/native byte bounds apply. Disk recovery: 2 MiB serialized characters. Aggregate memory accounting: 32 MiB weighted characters/entries. Storage failures remain visible; active drafts can be copied. |
| macOS filesystem | Native tests cover private permissions, extended attributes, executable modes, links, pinned ancestors, deletion, failed publication and concurrency. |
| Linux/other Unix | Linux atomic exchange/no-replace code exists but was not compiled or run in this macOS session. Linux ACL/xattr parity with macOS metadata copying is not established. Unsupported Unix replacements fail explicitly. |
| Windows | Atomic working-entry replacement is unavailable here and is explicitly refused. External resolution followed by staging is the fallback; Windows runtime behavior remains unverified. |
| External writers/crashes | Working-entry replacement and index publication are separate filesystem operations, not a multi-file ACID transaction. Unsynchronized external writers cannot be universally excluded. Failures retain recovery information; stale locks/crash recovery require inspection, not destructive automatic cleanup. |
| Installed app/provider | Browser fixtures and real Git engine tests do not establish installed Tauri delivery, physical quit/clipboard/accessibility behavior, signing, or a live MANVI mutation. Those were not exercised. |
| Nine opt-in Rust checks | Not counted as passes: document-refresh timing, live upstream update, deep graph fuzz, a real-repository Pulse reader and five real-repository history/status smoke tests. These are outside Resolve. |

GitNexus's aggregate working-tree scan included unrelated concurrent work and
reported critical risk. It is not an isolated impact measurement for this
change. Parser/caller impacts were inspected before edits; unindexed Svelte and
new session symbols were manually traced. Changes remain uncommitted for review.
