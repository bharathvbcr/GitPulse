# Performance diagnostics

Open **Diagnostics** from the header bug icon or command palette and copy its
report after reproducing a slow action. Keep the time, repository size, open
view, and action with the report; they help correlate UI observations with
native commands.

- `performance:ui`: the visible, focused UI timer ran at least 250 ms late.
  The report records delayed samples, maximum lateness, the attribution of the
  worst sample (`worst: cause=… view=… commands=[…] watcher_events=…`) and a
  tally of causes (`causes: …`). A cause is `command:<name>` when IPC answers
  were handled during the late interval, `watcher-burst` when `repo-changed`
  events were at least as many, and otherwise `view:<surface/tab>`. It is
  what the UI thread handled inside the late interval
  (`src/lib/diagnostics/activity.ts`), a correlation and not a profile; it
  carries command and view names only, never arguments, paths or results. A
  `(activity log wrapped …)` note means the 256-event log overflowed inside
  the interval and the counts are lower bounds. It does not measure FPS, and
  it does not see the native AppKit main thread, which is a separate process
  from the web view. Hidden and unfocused windows are not sampled; unreported
  samples are dropped when leaving the foreground. Very long gaps are labelled
  as potentially including system sleep or suspension.
- Backend `[performance]`: a command through the shared blocking wrapper took
  at least one second. `queue_ms` measures waiting for a blocking worker;
  `work_ms` includes everything inside that command, including subprocess and
  internal lock waits. `outcome` describes the latest completed call.
- `slow_calls_since_report`, `max_queue_ms`, and `max_work_ms` summarize slow
  calls accumulated since the previous report for that operation. Each
  operation reports at most once per 30 seconds, when another slow call
  finishes. Maxima can come from different calls. At application exit,
  aggregates still held by that pacing are written as `slow command summary
  at exit`, so a session's last slow calls are not lost.
- `operation still running`: a command through the same wrapper has been
  waiting or working for 5 s, written while it is still running, and again
  each time its age doubles. One line per operation: `calls` past the budget,
  `waiting_for_worker` (queued behind a saturated blocking pool, never
  started) and `oldest_ms`. Calls are registered when queued, so pool
  starvation shows here too. At most eight operations per one-second pass;
  the rest are one counted line. `operation no longer running` follows when
  reported calls end; `operation unfinished at exit` lists every call still
  registered when the app quits. Over 1,024 concurrent calls, the excess is
  counted (`in-flight table full`), not tracked. A call that hangs the whole
  process before the watchdog's next pass can still leave no record; take a
  `sample` as below.
- `docs-refresh`: a background document rebuild failed, or the bounded queue
  could not accept additional repositories. An empty log is not proof that
  all documents refreshed.

On macOS the durable app log is `~/Library/Logs/GitPulse/gitpulse.log`; its
previous generation is `gitpulse.log.1`. Each is bounded to 1 MiB. The
Diagnostics report includes the actual path and any read/write degradation.
The new performance records contain no command arguments or result bodies.

```sh
tail -n 200 "$HOME/Library/Logs/GitPulse/gitpulse.log"
```

For a native stall that does not finish, identify the running GitPulse PID
and take a bounded sample using macOS `sample <PID> 5 -file <output-path>`.
Native stack sampling includes waiting threads and does not profile the
separate WebKit JavaScript process. Interpret sample counts as stack
observations, not percentages of exclusive CPU time.

The [archived September 9 diagnostics audit](archive/DIAGNOSTICS_SYMLINK_AUDIT.md) covers the
symlink diff failure and subsequent metric-scheduling regressions. Automatic
LOC, coverage, and storage refreshes now follow the same visibility and active
repository scope as document and code-index refreshes. Repeated Rescan requests
coalesce behind a running measurement instead of launching overlapping scans.
These are scheduling guarantees, not measured application-wide FPS gains.

## September 7 audit and measured changes

**Verified:** the installed 0.0.8 process showed approximately 54–56% CPU in
separate `ps` snapshots. Native inspection showed ten repository tabs open.
Five-second samples included document-vault rebuilds, branch/tag/status reads,
code-index refreshes, subprocess creation, and macOS allocator fork locks.
The original durable log contained nine startup lines and no timing records.
Samples include waiting threads and do not establish one cause for every lag.

**Verified macOS optimization:** Git commands changed PATH while launching a
bare executable name. Rust's current implementation explicitly falls back to
fork for this combination. Git now resolves the same effective PATH to an
absolute executable, preserving precedence, non-UTF-8 Unix path bytes, helper
environment, process groups, and cleanup. Relative PATH entries retain the OS
fallback; Windows launch behavior is unchanged. See the [Rust command
contract](https://doc.rust-lang.org/std/process/struct.Command.html) and
[Unix spawn implementation](https://doc.rust-lang.org/src/std/sys/process/unix/unix.rs.html).

The existing `process_spawn` benchmark now includes the GUI launch shape:
changed PATH, working directory, piped output, process group, and a resident
128 MiB parent. Run `cargo bench --manifest-path src-tauri/Cargo.toml --bench process_spawn`.

| 200 samples, 20 warmups per case | Median | p95 | Worst |
| --- | ---: | ---: | ---: |
| Bare executable + changed PATH | 3.643 ms | 4.859 ms | 10.020 ms |
| Absolute executable + changed PATH | 1.893 ms | 2.633 ms | 2.885 ms |

This is a **48.0% median spawn-and-wait reduction in one controlled run**, not
an application-wide FPS or CPU improvement. It isolates launch overhead using
`/usr/bin/true`, not Git's repository work.

The following behavioral regressions failed before their corresponding fixes:

| Reproduction | Original behavior | Updated behavior |
| --- | --- | --- |
| 32 repositories change together | 32 background scans admitted together | one active scan per background service |
| Files change during a build | follow-up lost | one coalesced follow-up retained |
| Continuous index events | indefinitely postponed builds | bounded debounce eligibility and recovery interval |
| Reset while an index build runs | new calls overlap; old results return | running slot retained; old results discarded |
| 1,000 repository events | unbounded per-repo timers | 64 pending keys, one timer, explicit overflow |
| Subprocess gate saturated | command deadline has not started | queue wait expires before launch |
| Descendant retains stdin after parent exits | unbounded writer join | bounded settlement; Unix writer cancellation |
| Search/graph against cached documents | global cache mutex held during query | query owns a snapshot outside the mutex |
| 34 one-MiB documents | all 34 MiB retained | at most 32 MiB, marked partial |
| Ten document repositories opened | all retained indefinitely | eight cached vaults |
| Query overlaps invalidation and refresh | old query restores stale cache | detached slots cannot republish; cold loads share a slot |
| Tracked document symlink outside repo | outside text indexed | read refused and counted unreadable |

The timing regression runs an actual blocking command that fails after about
1.1 seconds and verifies its diagnostic record excludes the error payload.
Scheduling tests cover failures, recovery, saturation, continuous changes,
reset races, and visibility/sleep handling. They measure admission and state,
not rendered FPS. Existing commit-table virtualization, frame coalescing,
graph paint caching, macOS materials, and reduced-motion/transparency rules
were inspected; this change adds no speculative GPU switches or visual redesign.

## Follow-up: background work and document reuse

The docs and code-index queues now follow the active repository and window
visibility. Inactive/hidden dirty keys consume no timers or native scans;
activation resumes coalesced work after a 200 ms rendering grace period.
Closed repositories lose their queued work and cannot publish late index
results after reopening. The app's 24-tab cap remains below each queue's
64-key bound. Explicit document queries keep their immediate path.

Renderer timers follow the same rule. While the window is hidden or unfocused,
the status poll, enhancement refresh, task-run polling, hygiene and cleaner
refreshes, Manvi operations, and the status popover stop entirely instead of
waking the renderer every period to return early. Their next run is stretched
when the event loop is already late, so a busy renderer is not given more work
to be late for.

Automatic index builds and watcher document refreshes also carry background
subprocess priority. They can occupy at most one quarter of the process slots
(at least one); interactive operations retain the rest. When no background
child is running, a waiting background class reserves the next available slot
so continuous foreground traffic cannot postpone the whole class forever.
This does not preempt already-running work or change macOS thread QoS.

Document refresh still checks tracked paths and actual bounded file bytes.
Unchanged notes reuse parsed metadata; a completely unchanged source set and
scan status reuse the indexed snapshot. Exact comparison catches same-size
edits with restored timestamps. Deletes, renames, invalid UTF-8 and files
replaced by outside symlinks remain visible in the rebuilt content/status.
The Git filter now derives all supported Markdown extensions from MarkDev and
matches their case variants; the pre-fix regression discovered only 3 of 10
tracked supported variants.

The manual debug-build workload (`docs::tests::repeated_document_refresh_workload`)
uses 100 tracked notes and 15 unchanged refresh samples. On this Mac, before/after
median time was 1,007,932 / 38,559 microseconds; sampled p95 was 1,188,446 /
92,582 microseconds. These local synthetic timings are not release-build or
whole-app frame-rate measurements. Debug builds emit `docs_refresh` counts for
admitted, parsed and reused notes and whether the whole index was reused;
release builds retain the existing slow-command observations.

Regression evidence includes 50,000 background events without scans/timers,
close/reopen while native work remains active, resumption cooldown, 1,200 mixed
priority admissions across 24 threads, priority restoration through panic,
and 24 file-churn rounds comparing incremental results with fresh rebuilds.
The new scope/extension/admission tests failed before their respective fixes.

Follow-up validation: the complete native library/integration run passed
1,968 tests across 55 suite summaries with nine explicit ignores. The manual
document benchmark was run separately. All-target Clippy with warnings denied,
Svelte/TypeScript, 187-command registration, 49 wire contracts, release-version
and workflow checks passed. The production frontend built successfully with
its existing large-chunk advisory. The project's Chromium diagnostics harness
passed 24 checks; it does not establish native WKWebView frame pacing.
The updated arm64 release executable also built via
`npm run tauri -- build --no-bundle --no-sign --ci`
(`/tmp/gitpulse-followup-native-build.log`). It has not been packaged, signed,
installed, or used for a native rendering comparison.

The full frontend coverage reruns are not a green whole-tree gate: the latest
completed run passed 5,049 tests and failed three (the live-port and hostile
diagnostic-context wall-clock budgets, and an editor-draft contract while its
owner API was changing in this shared checkout). The earlier live-port failure
also exposed two real test-helper defects: signal termination was not recognized
as completion, and a timed-out exit observation resolved as success. Both have
regressions and fixes; no test timeout was increased. Further integrated
verification is required after the remaining audit changes and shared edits settle.
After the broad suites finished, all 50 tests in those three failing files
passed with one frontend worker (`/tmp/gitpulse-failed-cases-serial.log`). This
is an isolated pass, not a replacement for the failing full coverage run.

Local evidence: `/tmp/gitpulse-docs-timing-before.log`,
`/tmp/gitpulse-docs-timing-after.log`, `/tmp/gitpulse-docs-verified.log`,
`/tmp/gitpulse-priority-runner-tests.log`, `/tmp/gitpulse-followup-native-all.log`,
`/tmp/gitpulse-background-browser.log`, `/tmp/gitpulse-background-clippy.log`,
`/tmp/gitpulse-followup-coverage-final.log` and
`/tmp/gitpulse-exit-observation-baseline.log`.

Still open in the overall audit: measured native macOS rendering/idle-power
behavior with the updated build (stutter attribution and still-running
operation records closed on 2026-10-08, below). Physical Windows/Linux execution and native Mac
sleep/wake/display testing remain separate from these deterministic queue and
filesystem tests.

Closed since (2026-10-07):

- **Scope deferral covers repository state and metrics.** Repository-state
  refreshes follow `src/lib/repos/watcherRefresh.ts`: the active tab of a shown
  window after the 200 ms debounce; a background tab at most once per 30 s
  (once per 5 s across all tabs) so its tab-strip badges stay roughly current —
  bounded, deliberately not zero; nothing while the window is hidden, owed once
  when it is shown; activation hydrates in full. Subscribed metrics measure
  automatically only for the visible, active repository
  (`canMeasureAutomatically` in `src/lib/metrics/freshness.ts`) and resume on
  activation. The sentence this replaces predated both.
- **Watcher invalidation is path-aware.** `repo-changed` carries what moved —
  `refs`, `index`, `config`, `ignore`, `objects`, `git_state`, `worktree`
  (with up to 64 top-level paths), `documents`, or `unknown` — classified in
  `src-tauri/src/watcher/mod.rs` from the same paths the noise gate admitted,
  so fsmonitor cookies and split-index touches still record nothing.
  `src/lib/repos/changeScope.ts` routes it: a `git fetch` (refs and objects)
  refreshes repository state and disk usage, not line counts, coverage, the
  code index or the document vault; a source edit skips the vault. An index
  write counts as content (git writes it whenever it rewrites tracked files),
  and an unknown or unreadable change refreshes everything. The worktree watch
  stays non-recursive, so a path list means "at least these".

## Earlier validation and remaining gates

- **Verified:** the full native library/integration/stress run reported 1,941
  passing tests and eight explicit ignores. Five ignored real-repository graph
  and status tests, one real-repository Pulse test, and the deep graph fuzz test
  were then run explicitly and passed. Deep fuzz exercised 6,800 generated
  graphs. The network-dependent upstream-update test was not run.
- **Verified:** after the cache race fix, a fresh full library run passed 1,446
  tests with one explicit ignore. After stdin readiness polling, all 85 command
  runner tests passed, including an 8 MiB bidirectional pipe transfer,
  saturation deadlines, process fan-out, early exits, descendant-held pipes,
  executable lookup, and injected-environment defenses.
- **Verified:** all 4,920 frontend tests in the coverage run passed. Line
  coverage was 94.74%, branches 87.79%, and functions 94.11%. The coverage
  command **failed** its 95% function floor. The new paced queue and live-index
  controller each had 100% function coverage. The largest gap was the
  concurrently added terminal lifecycle module (27 unexercised functions).
  Thresholds and exclusions were not changed by this performance work.
- **Verified:** focused checks passed 310 tests. Svelte/TypeScript checks found
  zero errors/warnings. IPC checks found 185 handlers, 179 invoked commands,
  and no missing/orphaned commands. All 49 wire contracts passed. Clippy with
  warnings denied passed all targets. Release versions and workflow contracts
  passed. The vendor check passed with the configured `--allow-drift`; it
  reported upstream drift and is not proof that vendors match upstream.
- **Verified:** the production frontend build passed, retaining its existing
  advisory about a chunk over 500 kB. The full-tree formatting/whitespace check
  reported concurrent terminal edits; the Rust files owned by this performance
  change passed formatting. Earlier WorkView test failures were resolved before the passing full
  frontend run. A later rerun during additional terminal edits passed 4,936
  tests and failed nine: command registration/count drift, documented handler
  and field counts, and five terminal component contract expectations. The
  current whole-tree frontend gate therefore remains failing.
- **Unverified:** the complete `ci:local` gate is not green because of the
  function-coverage floor and shared-tree formatting. Rust LCOV was not
  regenerated. The final arm64 macOS release executable built successfully through the Tauri CLI with
  the macOS configuration merge. Packaging/signing were explicitly skipped;
  installation and comparison of the updated native UI remain unverified.

The installed application was sampled and inspected but **not replaced**.
Updated native UI smoothness with the same ten repositories remains
unverified. A previous browser run of `harness/responsiveness.html` detected a
900 ms deliberate block (526 ms timer lateness). The broader browser stress
canary could not be evaluated in this pass: the in-app browser repeatedly
returned `target closed while handling command`, and Chrome automation was
unavailable. That is an unavailable check, not a passing canary.

**Browser harnesses, 2026-10-07** (Chromium in the desktop app's browser pane,
`bunx vite --config vite.harness.config.ts`; the harness served the main
checkout, which then held another session's uncommitted workbench edits):

- `harness/responsiveness.html`: the deliberate 900 ms block was reported as
  `1 delayed UI timer sample(s); max_delay_ms=404`. The probe measures timer
  lateness, not the block's length; the detector is live.
- Stress canary (`harness/stress.html`, `tabs=5`, `cycles=12` — a short run,
  not the 40–45-cycle sweep): `LoopCanary` tripped first (`depth=1`,
  `effect_update_depth_exceeded`; it needs at least two tabs, and at `tabs=1`
  it reads 0, which is a dead detector, not a pass). Then, each with
  `depthExceeded=0`, `mountError=null`, `otherCrashes=[]`: `PulseView/chaos`,
  `StoragePanel/switch`, `HealthPanel/chaos`, `CoverageViewer/chaos`,
  `FleetView/chaos`, `StatusBar/chaos`.
- **Not clean:** `ManviOpsPanel/chaos` reported `otherCrashes: ["TypeError:
  Cannot read properties of undefined (reading 'unregisterListener')"]` —
  probably the harness's Tauri event mock lacking unlisten internals, but not
  established, so this component is unexamined, not clean.
- **Did not arm:** `TerminalPanel/termtabs` ended with 0 terminal tabs and 0
  xterm screens. The harness mounts the panel with no repository ("Open a
  repository to start a shell") and the scenario's opener filter skips buttons
  with an `aria-label`, which the launcher now has. No terminal tab was ever
  opened, so its `depthExceeded=0` says nothing.

**Installed build, not measured.** Native WKWebView frame pacing, cold/warm
navigation, idle CPU/memory and the eight-hour soak were not run: the release
build was not installed over the user's app (their choice), and no number here
stands in for them. `scripts/native-sample.mjs` records the installed app's
resident memory and CPU over a soak (`--duration 8h --interval 60 --out
soak.jsonl`) and prints the least-squares memory slope. It keeps the app, its
bundled helpers and the processes it hosts (agent CLIs and shells in terminal
tabs) apart, and it cannot see the WKWebView content processes, which launchd
starts. Frame pacing comes from the app's own responsiveness monitor
(`performance:ui` entries in the diagnostics log during the soak); cold and
warm navigation need a stopwatch or an instrumented build. A two-second smoke
run against the running 1.4.0 app read 167 MiB resident for the app process —
a wiring check, not an idle measurement: agents were working in its tabs.

## 2026-10-08: stall attribution, still-running records, canary, limits

**Verified (tests):** `performance:ui` samples are attributed to the command,
watcher burst or view handled during the late interval
(`src/lib/diagnostics/{activity,responsiveness}.test.ts`; the four new probe
tests fail against the previous probe). Native commands are visible while
running and at exit: `commands::assemble_tests::running_command_is_visible_in_diagnostics_before_it_finishes`
drives a real blocked `off_thread` call and waits for its `operation still
running` and `operation no longer running` lines; `exit_flush_writes_paced_slow_calls_and_unfinished_calls`
proves a paced second slow call and a running call are written by `flush`,
which `RunEvent::Exit` calls before terminal shutdown. Unit tests in
`logging/performance.rs` cover the doubling schedule, grouping, the 1,024-call
and eight-line caps, and that drained aggregates are not repeated.
The IPC observer forwards each answer through a new promise rather than a
side `.then`, because any reaction marks a promise handled and would hide
unhandled rejections from callers without a `catch`. That costs callers
exactly one microtask, pinned by `activity.test.ts`. It cannot sit lower:
Tauri's `invoke` is an `async` wrapper over a non-writable
`__TAURI_INTERNALS__.invoke`.

**Stress canary at full length (Chromium, headless).** Commit `0d3e9f77`
(after 737183a7), Chrome 154.0.8037.98, `tabs=5`, `cycles=44`, each scenario
twice in a fresh browser and profile with background-timer throttling
disabled, verdicts read from the posted `data-gp-result` and checked against
the requested component/scenario. All 18 runs completed 44 cycles with
`armed: true`, `mountError: null`, and identical results across both runs:

| Scenario | depthExceeded | otherCrashes | Notes |
| --- | ---: | --- | --- |
| LoopCanary/chaos | 7 | `["updated at"]` | tripped as designed; the entry is Svelte's own debug trace logged with the loop error |
| PulseView/chaos | 0 | none | `stripMatchesTabs: true` |
| StoragePanel/switch | 0 | none | |
| HealthPanel/chaos | 0 | none | |
| CoverageViewer/chaos | 0 | none | |
| FleetView/chaos | 0 | none | |
| StatusBar/chaos | 0 | none | |
| ManviOpsPanel/chaos | 0 | none | the earlier `unregisterListener` crash is gone |
| TerminalPanel/termtabs | 0 | none | opened 44 tabs, peak 31 xterm screens |

`stripMatchesTabs` is a real check only for PulseView; the others render no
repository strip and read `n/a`. LoopCanary has no documented scenario, so
`chaos` was chosen. This is Chromium, not WKWebView or the installed app.

**Startup fan-out, ten repositories (verified on this Mac, debug build).**
`watcher::registration_timing::watcher_registration_timing` (ignored; run with
`cargo test --manifest-path src-tauri/Cargo.toml --lib watcher_registration_timing -- --ignored --nocapture --test-threads=1`)
times the production `RepoFileWatcher::watch_repo` and the whole
`start_watch_inner` body against ten fresh repositories, nine interleaved
rounds with rotated order:

| Native `watch_repo`, ms | min | median | max |
| --- | ---: | ---: | ---: |
| one registration alone | 14.05 | 15.53 | 18.27 |
| ten serial, total | 122.05 | 152.42 | 177.46 |
| ten concurrent (ten threads), total | 128.65 | 149.64 | 203.27 |
| one concurrent call | 12.86 | 82.18 | 203.10 |

Registrations serialize: ten concurrent take as long as ten serial
(median ratio 0.98), and a concurrent call waits about 5.3× a lone one. A
second, busier run gave the same ratios. Startup already registers one
repository at a time (`repoStore.ts` restore loop awaits each watch before the
next tab's hydrate), so serialization costs nothing extra today: about 15 ms
per repository, roughly 150 ms for ten, interleaved with hydrates. Running
registrations in parallel would gain essentially nothing; no change was made.
Not established: whether the lock is in `fseventsd` or in the in-process
CoreServices client, and the split of the 15 ms between stream creation and
`FSEventStreamStart`.

**Bundle: Code and History split out; the 500 kB advisory accepted.** The
2026-10-08 production build measured the entry chunk `main` at 777,830
bytes, 2,170 under the enforced `MAX_PRODUCTION_CHUNK_BYTES` (then 780,000)
and up from 543 KB after the first lazy-view split. By source map,
`src/lib/components` was 415 KB of it. Following the config's own rule (defer
views not on screen at startup rather than raise the ceiling), CodeView and
HistoryView now load through `LazyView` (pinned in `src/App.test.ts`). The
rebuilt `main` is **563,540 bytes** (−214 KB); the new chunks are HistoryView
130.5 KB and CodeView 43.0 KB, carrying DiffViewer, CommitTable, CommitRow,
FileViewer and the file tree. Components shared with the eager Work tab and
sidebar (BranchList, WorktreesPanel, CodeViewer, GraphRenderer) stay in `main`.
A session restored into Code or History pays one local chunk fetch and the
skeleton once, as every other lazy view does. The ceiling is now 640,000
bytes, so a tens-of-KB leak still trips it. `main` stays above Vite's generic
500 kB warning because the default Work tab is deliberately eager; that
advisory is accepted, and the enforced budget is the ceiling. `LazyView`'s
loader type now erases props, as `LazyMount` already did, because Code and
History are the first lazy views with required props. The CI build-cache
revisit (`docs/GOOD_FIRST_ISSUES.md`) is unchanged: the build takes 5–10 s.

**Structural limits, kept with reasons:**

- *Blocked non-Unix stdin writer.* Unix writers are cancelled by stdin
  readiness polling. Windows anonymous pipes do not support overlapped
  I/O, so cancelling a blocked synchronous write needs `CancelSynchronousIo`
  against the writer thread, through new Windows FFI. That cannot be
  validated here (no Windows hardware; physical Windows runs are an open
  gate), and an untested cancellation path is worse than a documented bound.
  The runner still returns at its deadline; only the writer thread can
  outlive the settle window until the pipe closes.
- *Stalled network filesystem.* A `read`/`stat` blocked in the kernel on a
  hung mount cannot be interrupted from user space, so input budgets cannot
  give a response deadline. Such a call now appears in Diagnostics as
  `operation still running` while it is stuck, instead of leaving no record.
- *Parsed index memory.* Byte caps bound the source admitted (32 MiB of
  documents, per-note caps), not the parsed structures or snapshots held by
  active readers. A hard memory bound would need allocation accounting inside
  the vendored MarkDev/DevMap parsers, which this repository does not own.
- *Slow-call aggregates at shutdown:* fixed (above).

The semaphore saturation test initially used the global gate and interfered
with unrelated concurrent tests. It now drives the same production runner
with a private gate; the deadline assertion remains unchanged.

Platform limits: native execution was tested on macOS. Windows/Linux runtime
validation remains a CI/physical gate. Unix stdin cancellation is implemented;
a blocked non-Unix writer can outlive the caller's bounded settle window until
the pipe closes. Filesystem input budgets cannot guarantee a response deadline
from a stalled network filesystem. Source byte caps do not bound all parsed
index memory or the lifetime of snapshots retained by active readers. The
reasons each is kept are in the 2026-10-08 section above.


Evidence from this workstation is retained in `/tmp/gitpulse-spawn-benchmark.log`,
`/tmp/gitpulse-macos-audit-sample.txt`, `/tmp/gitpulse-audit-native-tests.log`,
`/tmp/gitpulse-audit-runner-final.log`, `/tmp/gitpulse-audit-lib-final.log`, `/tmp/gitpulse-audit-real-repo.log`,
`/tmp/gitpulse-audit-pulse-real.log`, `/tmp/gitpulse-audit-deep-fuzz.log`, and
`/tmp/gitpulse-audit-coverage.log`. These are local run artifacts, not a CI claim.

GitNexus review of the complete dirty checkout reported 63 changed files,
192 symbols and 38 affected execution flows at CRITICAL risk. That includes
other ongoing work; it is not the scope of this performance patch alone.
The Git launcher and shared runner were separately impact-checked before
editing, and their broad read/write callers were covered by the native suites.

Current cumulative scope: selected performance source, tests, harness and this
report add 2,381 lines and remove 213, excluding shared App/command wiring and
architecture documentation. This performance work adds no dependencies.
