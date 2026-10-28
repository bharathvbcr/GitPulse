# Performance diagnostics

> **Evidence names.** Log and evidence file names quoted in this document (`*.log`,
> `*.json`) name local run artifacts that were never committed and are not
> retained; the figures quoted beside them are the record. They are not paths a
> reader can open. `scripts/documented-counts-contract.test.ts` rejects any new
> `/tmp/...` evidence path.

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
- `performance:nav`: a switch of surface, repository or view settled 250 ms
  or more after it started, or was still changing the page after 10 s
  (`src/lib/diagnostics/navigation.ts`). Settled is the last DOM change
  before 500 ms without one, so it includes a cold repository's hydrate or a
  lazy view's chunk, and any background update landing in that window; a
  view waiting more than 500 ms on an IPC answer with nothing changing is
  timed too early. Cold is the first visit to that destination this session.
  Each entry names the worst navigation (`settled_ms`, `first_frame_ms`) and
  carries the session's cold and warm p50/p95/max, so a fast session writes
  nothing; at most one entry per 30 s. Navigations in a hidden document are
  not timed. It carries view names only, never repository paths.
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

The [archived September 9 diagnostics audit](https://github.com/bharathvbcr/GitPulse/blob/16483b7de2b5c74003c7e7af046f7142ece7699b/docs/archive/DIAGNOSTICS_SYMLINK_AUDIT.md) covers the
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
(`gitpulse-followup-native-build.log`). It has not been packaged, signed,
installed, or used for a native rendering comparison.

Whole-tree gate status is recorded in [Current gate status](#current-gate-status),
not here: the frontend coverage failures this paragraph once described (two
wall-clock budgets and an editor-draft contract, in a shared
checkout) are superseded by the run recorded there.

Local evidence: `gitpulse-docs-timing-before.log`,
`gitpulse-docs-timing-after.log`, `gitpulse-docs-verified.log`,
`gitpulse-priority-runner-tests.log`, `gitpulse-followup-native-all.log`,
`gitpulse-background-browser.log`, `gitpulse-background-clippy.log`,
`gitpulse-followup-coverage-final.log` and
`gitpulse-exit-observation-baseline.log`.

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

## Current gate status

`bun run ci:local` on branch `hx/w8-final` (commit `3299c074`, 2026-10-11, one
checkout, one Mac, other agents running): **green, 17 of 17 gates passed, none
skipped.**

- Frontend unit tests with coverage: 9,769 passed, one skipped. Lines 96.59%
  (floor 90%), branches 89.93% (floor 85%).
- Rust tests with coverage (`cargo llvm-cov … --no-fail-fast --test-threads=1`):
  4,408 passed, 0 failed, 33 ignored. Lines 87.97% (floor 80%). The LCOV carries
  no branch data, so Rust branches are reported as not gated.
- Headless Chrome and WKWebView regressions, Svelte/TypeScript check, Vite
  build, `cargo fmt`, clippy, IPC/type/release/workflow contracts, the vendored
  crate check and the gusset engine check (parity, match-any, panic firewall)
  passed.
- Two conditions of that run, stated so it is not over-read. `check:vendor-schema`
  compares against the `devmap` on `PATH`; the run put a `devmap` built from
  DevCouncil `9add99c2` first, because the installed `~/.local/bin/devmap`
  (0.2.4, store schema 26) predates the vendored schema 27 and fails the gate.
  The gusset gate ran the nested-worktree resolution that landed as `18d0f87c`
  during the run.
- The previous run of the same gate (`787f8dd5`) failed
  `spawn_rate_cap_bounds_a_concurrent_storm`: under coverage its window is
  stretched to 5 s while its cap assumed 500 ms. Fixed in `b8fa95e5` by
  bounding the count by the measured window; reproduced and verified with
  `CARGO_LLVM_COV=1`, and a gate with its rate cap removed still fails.

**Since that run, main is red again.** The host-load governor merge (`0d5f63a1`,
ft-5d8b201f) was branched before the wave-4/5 contract guards, and on the merged
tree (`c2fc9679`) four Rust guards fail (4,474 passed, 4 failed) and the
`uncommitted` browser harness fails in Chrome and WebKit:
`acl_contract` (the frontend calls `cmd_host_load_settings`,
`cmd_host_load_settings_save`, `cmd_host_load_status` and
`cmd_host_load_terminate_orphan`, none granted to the main window),
`hermetic_contract` twice (`tests/host_load_soak.rs` is not hermetic;
`sysload/snapshot.rs:1139` has a raw wall-clock loop), `supervised_threads`
(`sysload/sampler.rs` starts a raw thread), and the harness's "No unexpected
commands" check (`cmd_host_load_status`). Tracked on the board as
gp-host-load-governor-integration.

**Fixed 2026-10-11 (ft-ae39d654).** The four host-load commands are granted.
Terminate goes behind the native confirm; see `docs/LOAD_GOVERNOR.md` → Orphans.
The soak declares `mod common;`, the snapshot loop uses `coverage_relaxed`, and
the sampler runs under `supervisor::spawn_service_with(Policy::RESTART)`. A new
test failed on the raw thread (1 tick) and passes on the supervised one. The
`uncommitted` harness mocks `cmd_host_load_status` from the typed
`harness/hostLoadFixture.ts`.

`ci:local` on that tree came back 13/17, but none of the four failures touch
the change:
- Vitest failed two tests that read gitignored local state, present since
  2026-10-09: `.agents/skills/*` and `docs/PROMO.md`. CI never has those files.
- The Rust step failed two live scanner tests (`deps_live_scan`) on network
  timeouts: a TLS handshake to proxy.golang.org, and npm audit at 90 s. They
  still failed alone. Their offline skip does not recognise those shapes.
- The Rust step also failed one hook test under load (`over_the_app`). It
  passed alone, 5/5.
- Coverage floor followed from the Rust step.
- WebKit failed five harnesses. `scroll-cue` stalls at module load ("did not
  finish within 60 seconds", 25 requests), alone and identically at the
  pre-change HEAD in a detached worktree.

The four previously failing guard crates, the `uncommitted` harness
(128/128), clippy, fmt, svelte-check and Chrome `browser:all` are green.

Not covered by `ci:local` at all: see the README's `ci:local` row (no `bun
audit`, `cargo deny`, Go jobs, scheduled workflows, or Linux/Windows builds).

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
- **Unverified (as of that run; superseded by [Current gate status](#current-gate-status)):** the complete `ci:local` gate was not green because of the
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

**Release build in an isolated profile, 2026-10-09/10 (partial).** The
release build of `f1854185` plus the `performance:nav` probe (build id
`d4124c1d`, arm64, Apple M5 Pro, 64 GB, macOS 27.0.1) ran as a copy with
bundle id `com.gitpulse.desktop.perfqual` and `HOME=/private/tmp/gpe-perf`,
so the user's installed 1.4.0 and its profile were not touched. Its
workspace held the first ten of the user's thirteen tabs, and their trust
records were copied in after a first run refused every repository command
with `REPOSITORY_TRUST_REQUIRED`. Without the user's `gh` login,
`github-alerts:check` failed throughout. Stage Manager was on.

- **Launch, verified:** process spawn (launchd) to WebKit's
  `DidFirstMeaningfulPaint`, both millisecond timestamps from the unified
  log: 394 ms for the first launch of the new bundle, then 414, 384 and
  396 ms. A fifth launch, after trust was copied in, took 360 ms. File
  caches were not purged (no `sudo`), so the first is not a cold-boot launch.
  This is the shell's first paint, not the ten tabs hydrated.
- **Repository navigation, warm only, verified:** four repository switches
  were timed by `performance:nav`: settled p50 549 ms, max 762 ms (worst
  `repository/work`, first frame 118 ms). Each switch was an open request
  sent to the running instance (`open -a … <repo>`), 4 s apart. Cold is
  **not measured**: the first pass ran while the window sat in the Stage
  Manager strip, where the document is hidden, so those visits were
  remembered but not timed, and the later visits counted as warm. View switches
  (Work/History/Code) were not driven; this host blocks synthetic input.
- **Frame pacing, not measured:** no `performance:ui` entry was recorded, but
  the window was focused for seconds, not minutes. The probe samples only
  a focused window, so this is an unobserved period, not a clean one.
- **Idle CPU/memory, not measured:** the one ten-minute idle sample ran
  while every repository command was refused for trust. It read 120 MiB and
  0.03 % CPU for an app doing no repository work, and is not an idle reading.
  The trusted run was quit from the keyboard (AppKit `performKeyEquivalent:`
  then `terminate:`, 02:33:51) before its idle phase began. While hydrating,
  shortly after the trusted launch, the app process read 140 MiB and 5.9 % CPU.
- **Eight-hour soak, not run.**

The sampler cannot see WKWebView content processes. Its companion here took
the set difference of WebKit process ids before and after launch: the GPU,
WebContent and Networking processes read 40, 109 and 15 MiB during the
untrusted idle. `scripts/native-sample.mjs` records the installed app's
resident memory and CPU over a soak (`--duration 8h --interval 60 --out
soak.jsonl`) and prints the least-squares memory slope. It keeps the app, its
bundled helpers and the processes it hosts (agent CLIs and shells in terminal
tabs) apart, and it cannot see the WKWebView content processes, which launchd
starts. Frame pacing comes from the app's own responsiveness monitor
(`performance:ui` entries in the diagnostics log during the soak) and
navigation from `performance:nav`. A remaining run needs a person at the Mac:
a launch from a background shell is not activated (macOS cooperative
activation), and a window in the Stage Manager strip hydrates nothing and is
not timed. An open request to the running instance (`open -a <app> <repo>`)
brings it on stage. A two-second smoke
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


Evidence from this workstation is retained in `gitpulse-spawn-benchmark.log`,
`gitpulse-macos-audit-sample.txt`, `gitpulse-audit-native-tests.log`,
`gitpulse-audit-runner-final.log`, `gitpulse-audit-lib-final.log`, `gitpulse-audit-real-repo.log`,
`gitpulse-audit-pulse-real.log`, `gitpulse-audit-deep-fuzz.log`, and
`gitpulse-audit-coverage.log`. These are local run artifacts, not a CI claim.

GitNexus review of the complete dirty checkout reported 63 changed files,
192 symbols and 38 affected execution flows at CRITICAL risk. That includes
other ongoing work; it is not the scope of this performance patch alone.
The Git launcher and shared runner were separately impact-checked before
editing, and their broad read/write callers were covered by the native suites.

Current cumulative scope: selected performance source, tests, harness and this
report add 2,381 lines and remove 213, excluding shared App/command wiring and
architecture documentation. This performance work adds no dependencies.

## 2026-10-10: hook binding lookup, read-only; one family resolution per gated action

Every `gitpulse-hook` run (each agent Bash call, each file edit) resolved the
checkout's task binding through the app's creating path: `git worktree list`
and a legacy-ledger consolidation pass *before* checking whether any ledger
existed, then a read-write ledger open — directory and file chmod checks, WAL
and synchronous pragmas, the full schema DDL — against a ledger the app is
writing. In the app, a guarded command or write resolved the worktree family
twice (scope lookup, then the gate row).

**Changed.** `ledger::bindings::resolve_binding_readonly` is the hook's lookup
(`harness::hook_scope`). When the repository's own metadata
(`engine::worktree::AdmittedPair::metadata_checkouts`: the primary checkout
plus the linked-worktree registry, read without Git) shows no ledger in any
checkout, it answers "unbound" with no Git child at all. Otherwise it
authenticates with `git worktree list` as before and reads the binding through
a read-only connection (`ledger::latest_binding_readonly`) that is never
cached and cannot chmod, set pragmas or run DDL. It never consolidates: if a
sibling ledger holds rows the anchor has not imported, the binding cannot be
ruled out and the lookup fails closed (`legacy_ledger_unconsolidated`) until
GitPulse has consolidated. Missing, untrusted, unrelated and foreign-row
outcomes are unchanged. The app's `guard_command`/`guard_file` resolve the
family once (`repository_address`) and hand that answer to both the scope
lookup (`bindings::binding_at`) and `record_gate`. A commit is still two gated
actions (add, then commit), so it now resolves the family twice, not four times.

**Verified (tests; each fails against the previous code, checked by
reverting the change locally):** `ledger::bindings::readonly_tests` — zero Git
children on an uninitialised ledger from a main or linked checkout; no
read-write open, no chmod (0644 stays 0644), no WAL switch (a DELETE-journal
ledger stays DELETE), no DDL (a dropped index stays dropped); unimported legacy
history refuses, then resolves once consolidated; untrusted, missing, foreign
row and unregistered gitfile each keep their code.
`harness::scope_tests::a_gated_action_resolves_its_family_once` (exactly one
`git worktree list` per `guard_file` and per `guard_command`, was two), and
`the_hook_scope_*`. `hook_protocol_stress::sixty_four_concurrent_hooks_read_a_live_ledger`
runs 64 concurrent `command-gate` processes against an initialised ledger
while the test process keeps appending to it. Every one exits 0 (a SIGBUS would
fail it), every one answers `task.scope_unavailable` (proving it read the
binding), and the ledger's 0644 mode survives. Passed 4 of 4 runs.

**Measured (release `gitpulse-hook`, interleaved A/B, alternating order per
pair, min over rounds; same commit with and without this change).** The
machine was heavily loaded by other work (load average 40–52 throughout), so
absolute numbers are well above the quiet-machine 25 ms in the 2026-10-09
audit:

| `command-gate`, payload `ls` | rounds × N | median before → after | p95 before → after |
| --- | ---: | ---: | ---: |
| trusted repo, no ledger anywhere | 7 × 30 | 35.67 → **24.03** ms | 52.95 → **33.42** ms |
| this repository (38 MB live ledger, 4 MB WAL) | 7 × 30 | 40.58 → 42.68 ms | 63.84 → 61.33 ms |
| same, second run | 10 × 40 | 52.51 → 47.53 ms | 104.28 → 100.70 ms |

The repository with no ledger loses its `git worktree list` child and is about
a third faster. On the live ledger the change removes the pragmas, DDL and
chmod checks but keeps the authenticating `git worktree list`, and the
difference is inside the noise at this load: not established either way. The
proposed ≤ 15 ms command-gate target (A19) is not met. What remains per call is
process start, the `git worktree list` on initialised repositories, and the
per-call `manvi serve --posture host` sidecar, out of scope here.

**Rulings (Fable, 2026-10-10).** D1: the zero-spawn shortcut stays. A
hand-written gitfile into a repository with no ledger anywhere is answered
"unbound": no binding can exist, trust is still required first, and the app
gate (`repository_address`) still refuses it. D2: unimported legacy sibling
history keeps failing closed (`legacy_ledger_unconsolidated`). One accepted
difference from the app: a sibling ledger file with no schema is skipped by
the hook (it holds no rows), where consolidation reports `legacy_read_failed`.
D3: this change closes without the ≤ 15 ms target (A19). The target moves,
with a per-component breakdown measured on a quiet machine, to
ft-cb0d70b8 (long-lived `manvi serve` over the app socket). D4: that sidecar
work is its own card rather than a note on ft-5a4e25c8.

## 2026-10-10: command-gate over the app mailbox (ft-cb0d70b8, Fable D9)

Every `gitpulse-hook command-gate` used to start its own `manvi serve`. While
the app is running, the hook now asks it over the single-instance mailbox
(`instance.sock` in the profile directory) and the app judges the command on a
long-lived sidecar of its own.

**Shape.** `Request::GateCommand { id, cwd, command }` is answered on its own
connection by the sink `launch::establish` attaches (`hooks::serve_gate`),
which runs the same `hooks::judge_command` the hook runs for itself, on the
app's hook lane (`harness::sidecar::Lane::Hook`). The hook (`judged_by_app`)
gives the app `GATE_APP_DEADLINE` (750 ms, inside the 5 s budget and the host's
hook timeout) and otherwise judges the command itself, exactly as before.

**Isolation from ft-6b72c703.** The hook lane is a separate `Slot`, so a
transport fault, timeout or respawn backoff on hook verdicts drops only the hook
lane's child, and a slow `work.runs.managed.prepare` on the profile sidecar or a
slow gated commit on the shared one cannot stall or tear down hook verdicts. The
lane makes one attempt (no retry-and-reset): the hook's own path is its retry.
Hook verdicts are capped at 16 in flight (`MAX_GATE_HANDLERS`), on top of the 8
relaunch handlers, so hook traffic cannot starve a relaunch; the 17th is told
"busy" at once and its hook judges the command itself.

**Measured** (`src-tauri/tests/hook_gate_bench.rs`, release `gitpulse-hook`,
real `manvi`, interleaved and rotated per round, 7 rounds x 30 calls per case,
min and median of the per-round medians, load average 22 to 27, so not a quiet
machine):

| case | min median | median median |
| --- | --- | --- |
| `start` (process alone, empty command) | 9.14 ms | 10.18 ms |
| `bare/own` (no repository, hook's own `manvi serve`) | 30.70 ms | 32.23 ms |
| `bare/app` | 9.61 ms | 11.02 ms |
| `repo/own` (repository, no ledger) | 30.51 ms | 32.16 ms |
| `repo/app` | 9.69 ms | 11.16 ms |
| `ledger/own` | 30.08 ms | 32.60 ms |
| `ledger/app` | 9.85 ms | 11.10 ms |

Per call the sidecar start is about 21 ms of the 31 ms; with the app the gate is
within 0.5 to 1 ms of the bare process start, under the 15 ms target (A19) even
at this load. Not established: the benchmark runs each hook under a fresh
`HOME`, so its repositories are untrusted there and the binding lookup and
`git worktree list` are skipped on both sides; the `ledger/*` rows therefore do
not exercise a live ledger, and the card's "38 MB live ledger" figure is not
re-measured. In the app path those costs move into the app process (off the
hook's critical path but not removed); the conditional follow-on of replacing
`git worktree list` with a registry read is not needed to reach 15 ms and was not
taken.

### `gitpulse-hook notify` by stdin size (S1-15)

The plugin runs `notify` on every tool call of every Claude Code session, and
GitPulse now injects the same notification hooks into every `claude` it
launches, so what `notify` costs before it has anything to say is paid a lot.
It used to read and parse up to 4 MiB of stdin first, and refused anything
larger, losing the `tool_finished` that answers a question. It now reads no
stdin at all when the event is unknown or is a state or a resolution outside a
GitPulse terminal, and otherwise reads once (up to 16 MiB) and builds only the
fields it uses.

**Measured** (`src-tauri/benches/notify_stdin_bench.rs`, then an `#[ignore]`d
test in `tests/` and a registered `[[bench]]` since; release hooks, A is the
hook built from the tree before the change and B the one after, interleaved and
rotated per round, 9 rounds x 20 calls per case, min and median of the
per-round medians, load average 4 to 5, macOS). The A rows are the bench's
`(base)` rows, present only when `GITPULSE_BENCH_BASE_HOOK` is set:

```text
cargo build --release --bin gitpulse-hook    # on the tree before the change
cp target/release/gitpulse-hook /tmp/gitpulse-hook-base
# apply the change, rebuild
GITPULSE_BENCH_BASE_HOOK=/tmp/gitpulse-hook-base \
GITPULSE_BENCH_HOOK=target/release/gitpulse-hook \
cargo bench --manifest-path src-tauri/Cargo.toml --bench notify_stdin_bench
```

| case | A min median | B min median | A median median | B median median |
| --- | --- | --- | --- | --- |
| `outside`, 0 B | 5.92 ms | 5.75 ms | 6.37 ms | 5.98 ms |
| `outside`, 100 KiB | 5.84 ms | 5.73 ms | 6.01 ms | 5.97 ms |
| `outside`, 4 MiB | 7.40 ms | 5.79 ms | 7.51 ms | 5.83 ms |
| `inside`, 0 B | 6.06 ms | 6.01 ms | 6.45 ms | 6.16 ms |
| `inside`, 100 KiB | 8.99 ms | 8.90 ms | 9.00 ms | 9.02 ms |
| `inside`, 4 MiB | 9.03 ms | 8.99 ms | 9.11 ms | 9.03 ms |

These are the numbers for the code that ships: B validates every skipped field
(a number out of range or a bad escape refuses the payload, as the whole parse
did), measured after that change, load average 5, same N, A first. `outside` is
a `tool_finished` with no `GITPULSE_SESSION_ID`; `inside` is the same from a
GitPulse terminal, delivered to a socket that acknowledges it. At 4 MiB
`outside` saves about 1.6 ms per call (the 4 MiB of stdin A read), to the
process-start floor of about 5.8 ms; at 0 B and 100 KiB there is nothing to
save, and the 0.1 to 0.2 ms B leads by there is within the spread of A against
itself. `inside` at 4 MiB agrees to 0.04 ms, so validation costs the delivered
case nothing measurable at this resolution and no win is claimed there. The
`inside` rows at 100 KiB and 4 MiB are dominated by the bench socket's accept
loop (1 ms poll), not by the hook. A first version of B that walked the pipe
byte by byte through `serde_json`'s reader measured 17.5 ms at `inside`, 4
MiB, twice A, and was replaced by reading once and walking the slice. Not
measured: a payload past 4 MiB on A, which A refuses and so has no
delivered-report cost to compare.

## Security impact: who may speak on the mailbox

The mailbox already accepted `Open`, `Reveal` and `Notification` from any
process of the same user. `GateCommand` adds a caller path to the command gate,
and it can only ask for a verdict:

- It returns what `check_command_in_scope` would return in the hook's own
  process: the app re-derives the git root, trust and bound scope itself from
  `cwd` and `command`; the caller supplies no scope, allowlist or root, so a
  peer cannot ask for the unscoped answer of a bound checkout.
- It never changes a repository, a binding or the ledger, and it records
  nothing.
- A verdict that is not a real one (harness missing, busy, faulted, scope
  unresolved to an unmappable reason) is an error on the app side, never a
  reply, and the hook then reports exactly what it reports with no app.
- The hook refuses an answer whose id is not its request's, that lacks a verdict,
  that does not decode, or that says `checked: false` / `Unchecked`. Any refusal,
  timeout or absence falls back to the per-call path; nothing falls open.
- Peers must be the same uid on both ends (`peer_uid`, which now also guards the
  connecting side), in a 0700 profile directory, with the socket 0600.
- Residual risk, stated plainly: a same-uid process that binds the mailbox path
  in place of the app could lie with a well-formed allow. That process can
  already replace `manvi`, the trust store or the hook binary, so the socket
  does not widen what same-user code can do; it is not defended against.

Differential evidence: `hooks::tests::over_the_app` compares the app's answer
with the hook's own for 8 commands across a bound, an unbound and a
repository-free directory (a mutation making the app answer `bound: false`
fails it), and feeds the hook seven kinds of wrong answer plus a control
(`a_peer_that_answers_wrongly_is_not_believed`).

## 2026-10-17: collision-guard over the app mailbox

`gitpulse-hook collision-guard` runs on every Edit/Write of an agent GitPulse
launched, as a fresh process, and each run read `git worktree list` and then
`git status --porcelain -z` in every worktree (up to `MAX_COLLISION_SCANS`,
16). Sampled on the live app that was 127 hook runs and at least 297 git
children in 60 s, more than the app itself spawned, in the window where every
exec was queueing behind syspolicyd. A fresh process can keep nothing between
calls, so no in-process cache could help.

**Shape.** `Request::CollisionScan { id, repo, path }` is answered on its own
connection, like `GateCommand`, by the sink `launch::establish` attaches
(`hooks::serve_collision`). The app answers from `insights::collision_memo`:
the worktree listing and each worktree's changed paths are kept for `TTL`
(5 s) and filled one caller at a time per key, so a burst of hooks costs one
scan. The hook (`scan_collisions`) gives the app `COLLISION_APP_DEADLINE` (2 s)
and otherwise scans every worktree itself, exactly as before.

**Why 2 s and not the gate's 750 ms.** A cold scan reads up to 16 worktrees (a
status read was about 150 ms on the loaded machine this was sampled on), where a
verdict is one sidecar round trip. The app finishes the scan whether or not the
hook is still waiting, so the next ask is answered from memory either way. This
was reasoned from that sample, not benchmarked end to end; the bound is inside
the hook's 5 s budget.

**What a stale answer can and cannot do.** collision-guard turns a positive
into an `ask`, which a headless agent's host turns into a deny, so the two
directions are not alike:

- A **negative** may be up to `TTL` old: a sibling that dirtied the same file in
  the last few seconds is missed. The unmemoized scan has the same blind spot
  over its own duration, so the memo widens a window that already existed.
- A **positive is never served from memory.** A scan that finds the file dirty
  elsewhere is re-read in exactly the worktrees it names before it is believed
  (`a_collision_that_has_ended_is_not_reported_from_memory`). Only the symbol
  verdict that refines it is kept for `TTL`.
- A read that failed is never kept, and a failure beside a remembered listing
  throws the listing away and scans once more.
- The reply carries `age_ms`; the hook refuses an answer older than twice `TTL`,
  one that read no worktree, and one that does not decode. Any refusal, timeout,
  busy slot (`MAX_GATE_HANDLERS` is shared with gate verdicts) or an app that
  predates the request (the frame does not parse, so it is refused) falls back
  to the hook's own full scan. An empty list never stands in for a scan that
  could not run.

**Measured** (`hooks::tests`, 5 guarded edits of one file over a repository and
6 sibling worktrees, git children counted per working directory):

| case | before | after |
| --- | --- | --- |
| uncontended file: `worktree list` / `status` / other | 5 / 35 / 0 = 40 (8 per edit) | 1 / 7 / 0 = 8 (1.6 per edit) |
| file dirty in all 6 siblings: `worktree list` / `status` / other | 5 / 35 / 60 = 100 (20 per edit) | 1 / 37 / 12 = 50 (10 per edit) |

"Other" is the symbol check's `ls-files` and `rev-parse` per party. In the
contended case the status count does not fall (37 against 35): the positive is
re-read each time on purpose. The saving there is the listing and the symbol
check, once per burst instead of once per edit.

**Not measured:** the live app. `gitpulse-hook` and the app must both be
rebuilt and reinstalled before a re-sample, and the MANVI gate does not allow
writes to `/Applications` from an agent session. An older hook against a newer
app, or the reverse, scans for itself and behaves as before.

**Security impact.** The request is read-only and adds no write path. The
caller supplies a worktree path and a relative file; the app re-derives trust
and the listing itself (`validate_repo`), so an untrusted repository is refused
exactly as it is in the hook's process. The reply discloses the paths of dirty
files in a repository GitPulse is trusted in, to any process of the same user
on the mailbox, which can run `git status` itself. The reply is trimmed to the
requested file's row and the counts so it fits one frame. Same-uid, 0700 profile
and 0600 socket rules are the gate's.

## 2026-10-12: benchmark harness and the loaded-host baseline (ft-c0d9313f)

The audit's perf cards need a number to be measured against. Before this there
were three hand-run benches that printed prose, no machine-readable output, no
baseline and no scheduled run. Every bench in `src-tauri/benches` that is listed
below now runs on one harness (`benches/common`) and is run, compared and
uploaded by `.github/workflows/benches.yml` (nightly 05:37 UTC and
`workflow_dispatch`; never on a pull request).

### The harness

`cargo bench --manifest-path src-tauri/Cargo.toml --bench <name> -- [--smoke] [--json <path>]`

* **Hermetic.** Each bench includes `src/test_support/env.rs`, so `HOME`, the XDG
  roots, the git config and the trust store are a per-process temp directory
  before `main`. The developer's `~/.gitconfig`, `~/.claude` sessions and trust
  store are never read, and nothing is written to them. Fixtures are generated
  once per run (`common/fixture.rs`) with fixed content, names, dates and
  identities. `GITPULSE_BENCH_REPO=<path>` points `mcp_protocol` at a real
  checkout instead, explicitly, and the report says so.
* **Interleaved, min-of-N.** `interleave` runs one sample of every case per
  round and rotates which goes first, so drift in host load lands on every case
  alike. `min` is the figure to compare between builds; p50, p95 and max are
  reported because they are what a user feels. An A/B between two *builds* is two
  JSON files from two binaries run back to back (A B A B) and compared with
  `scripts/bench-compare.mjs`: one process cannot hold two builds of the library.
* **Work counters vs metrics.** A counter (rows laid out, segments emitted, git
  processes started, events admitted, bytes received) is deterministic for a
  fixed fixture and is asserted equal across every sample of a row; a quantity
  that legitimately varies (bytes with a duration in them, MB/s, wakeups/s,
  messages under a race) is a *metric*: reported, never gated. Git process
  counts come from git's own `trace2` (`trace2.eventTarget` in the hermetic global
  config; `GIT_TRACE*` is stripped from every child GitPulse starts, the config
  key is not), in a separate untimed pass, because the log append would add to
  every process.
* **Host-load snapshot with every run.** `host.start` and `host.end` are the
  repo's own `sysload` sensor (load average, cores, memory pressure, thermal,
  power), plus `cpu_count`, `commit`, `dirty`, `os`, `arch`, `runner_class`, and
  `smoke`. The table header prints the load ratio and says when it is above 1.
  `--smoke` cuts every row to 3 samples; fixtures keep their size, so counters
  are identical to a full run's. The `--json` path is relative to `src-tauri`
  (cargo runs a bench from the package root); the workflow passes an absolute
  `GITPULSE_BENCH_JSON_DIR`.
* **A degraded answer is never timed.** `mcp_protocol`, `command_timings` abort,
  naming the tool and the facet path, on `isError`, on any `"ok": false` object
  in a payload, on a deadline-expired insights snapshot, a truncated language
  scan, a partial docs vault or a truncated `catch_up`. The language scan runs
  unbounded (`get_repo_language_stats_bounded(.., None)`): the production 10 s
  deadline is a property of the host's load, and a scan it cut short would be
  timed as if it were the whole work. Facets that report unavailability without an
  `ok` flag (`codeintel.available`, `ledger.error`) are printed, not gated;
  `agents.live` reads the host's real process table whatever `HOME` is.
* **`mcp_protocol` runs unthrottled** (`run_process_with_unlimited_spawn_rate`
  first in `main`, as the stress tests do). Before, 220 git-spawning samples spent
  the production gate's 192-spawn burst and then refilled at 8/s, so the
  repository rows could time the rate limiter or a deferred snapshot.

### The gate

`scripts/bench-compare.mjs --results <dir> --baseline src-tauri/benches/baselines/<runner class>.json`
(self-test: `scripts/bench-compare.test.ts`, run by `npx vitest run scripts/bench-compare.test.ts`):

* any **work counter** that changed (or appeared or vanished) fails;
* a row whose **min-of-N exceeds the baseline's by more than 1.5x and by more
  than 50 us** fails (the floor keeps a sub-microsecond row from failing on
  scheduler jitter); rows marked `time_gated: false` (the PTY and wakeup rows,
  whose time is a property of the OS scheduler) keep their counters in the gate
  and their wall clock out of it;
* a baselined bench or row missing from the run fails;
* a run from another runner class is refused (exit 2), never compared; times are
  only comparable within one class;
* **no baseline for the class is exit 2, never 0**: a check that could not run is
  not a check that passed. Seed a class by running `benches.yml` by hand,
  downloading the `bench-<runner>` artifact and
  `node scripts/bench-compare.mjs --results <dir> --write-baseline src-tauri/benches/baselines/<runner>.json`.

Exit codes: 0 no regression, 1 a regression, 2 the comparison could not run. The
report prints the host-load ratio next to every bench; **a time failure on a host
the JSON shows loaded (ratio above 1) is a reason to re-run, not a finding**.
Counter failures are not load-dependent.

Committed baselines: `local-macos-aarch64` (below, re-seeded after the lag-audit Waves 1 and 2). `ubuntu-latest` and
`macos-latest` have **no committed baseline yet**; until the first
`workflow_dispatch` seeds them, the scheduled job runs every bench in smoke mode
(which has to pass), prints a notice that no baseline is committed, and compares
nothing (`benches.yml`, `4797eb40`). The `bench-<runner>` artifact is that run's
measurement.

### What each bench measures, and the audit card it is the baseline for

| bench | rows | baseline for |
| --- | --- | --- |
| `mcp_protocol` | protocol layer, validation, rejected call, `gitpulse_status` / `collision_risk` / `insights` on a generated 400-file, 3-worktree repository | G-tests-perf-03; C-mcp-07 (protocol overhead only; the codeintel deadline overhead needs a DevMap store and is not measured) |
| `graph_diff` | `compute_word_diff` at 100 / 10k / 60k chars; `LaneSolver::solve` linear and merge-heavy at 10k and 100k rows | G-tests-perf-05 |
| `watcher` | `record_event_signal` over a 10k-event burst, `IgnoreRules` populated with 600 prefixes, events/s | G-tests-perf-05 (through `watcher::bench_seam`, below) |
| `repo_read` | `get_status` at 1k and 10k files, clean and dirty, with git processes | A-git-05; git-status-assist-spike |
| `command_timings` | insights snapshot, `list_worktrees listing`, `list_worktrees full`, language stats, docs refresh, `catch_up` (steady and cold), commit details on an 18-worktree fixture | the lag audit's snapshot / listing / stats / docs / catch-up / commit-details cards; `list_worktrees listing` (`list_worktrees_scanned` at `ScanDepth::Listing`, the `cmd_list_worktrees` default) is the baseline for ft-28c58695 (card 4), and `list_worktrees full` (`ScanDepth::Full`, the Worktrees panel's explicit opt-in) is the baseline for the Worktrees panel path |
| `terminal_pty` | MB/s through a real PTY, IPC messages per MiB, keystroke echo p50 idle and during a flood in another session | terminal-output-channel; terminal-reader-kqueue |
| `alerts_worker_throughput` | sustained notices/s through the intake, the 64-slot queue and the real notification worker: hook state reports and renderer answers over 64 sessions, 50,000 per sample, a full queue retried and counted. Time is gated like every bench (the producer/worker hand-off is the work); counters gated: `notices`, `sessions`, and `drained`, 1 when the worker took a probe offered after the last notice | ft-9cfd5474 (S1-08; row in ft-c0d9313f) |
| `notify_stdin_bench` | `gitpulse-hook notify` per call (median of 20) at 0 B, 100 KiB and 4 MiB of stdin, outside a GitPulse terminal and inside one against a socket that acknowledges; `(base)` rows beside each when `GITPULSE_BENCH_BASE_HOOK` names a hook built before a change (printed, never written to the JSON, so never in a baseline). Time is **recorded, not gated** (a process start); counters gated: `calls`, `stdin_bytes` and `reports_acked` (the reports the socket acknowledged). A hook that delivers but is slower is not caught: a 100 ms sleep added to it compared `OK` | ft-9cfd5174 (S1-15) |
| `idle_wakeups` | process interrupt wakeups/s with 0, 1, 8, 32 idle terminal sessions, an idle mailbox, 8 / 24 idle watches, an idle alerts bridge and the host-load sampler | terminal-reader-kqueue; always-on-pollers; ft-79394019 (Wave 0) |
| `live_index` | duty cycle of the live index under a 40-tick, 250 ms-cadence edit storm against a stub `devmap` | live-index-duty-cycle |
| `worktree_teardown` | `remove_worktree` (lock-held) and `merge_and_teardown_worktree` on a worktree with 100k ignored files, and the merge with 0 entries as the control; `contender_wait_ns` of a `lock_worktree` issued mid-teardown | CARD 22 S5-11 (ft-9d04447403db7f9525dce4db970e4b8a) |
| `graph_spawns` | `cmd_get_commit_graph` at the default 5,000-row cap on 6,000 commits; `cmd_get_stack_hierarchy` with 20 branches beyond its 2,000-commit window, cold and with refs unmoved; each with git processes | ft-9cfa4f74 (S4-03, S4-04) |

`src/watcher/bench_seam.rs` is a `#[doc(hidden)]` module that builds the same
`IgnoreRules` (through the same `parse_ignored_directories`) and calls the same
`record_event_signal` the watch loop calls. It was needed because both are
private and a bench is a separate crate; it adds no logic and no behaviour.

**Not measured, and why.** Tab-switch settle (warm and cold) and the compositing
A/B (GPU-process CPU and frame time) need the installed app and its webview; they
cannot be reproduced from `cargo bench` and are left to the installed-app
qualification (ft-9167572a) rather than faked. `decode_xml_entities` on an 8 MiB
artifact (E-integrations-01), transcript ingest over 400 MB (E-integrations-10), a
devmap preview via a stub binary other than the live-index stub, one task search
over 100k tasks, and the hook command gate (`tests/hook_gate_bench.rs`, still a
hand-run `#[ignore]`d test) have no bench here. The echo rows measure a keystroke
in a session *beside* a flooding one, not in the flooded session itself. The
wakeup counter is per process, so the per-session cost is the slope across the
four rows, and a thread class not tied to a session (host sampler, watcher,
live-index poller) is in the app process and not started by the library from a
bench. Spawns for rows other than the ones listed come from `trace2` and exist
only where a row lists `git_processes`.

### Baseline, `local-macos-aarch64`, after the lag-audit Waves 1 and 2

This is the committed baseline. It was re-seeded on 2026-10-12 from `main` at
`43f54d7f`, in full mode (not `--smoke`).

The machine and toolchain are the same as for the earlier baseline below:
Apple M5 Pro, 18 logical CPUs, macOS 27.0.1, rustc 1.99.0, git 2.56.0.

Load was much lower than in the first run, but it was not idle:
- 6.0–9.6 on 18 CPUs (0.3–0.5x) for every bench except `repo_read`.
- Other sessions' builds pushed load to 21 by the end of the run.
- In that run, `repo_read`'s 1k-file `get_status` rows recorded a min of 22 ms.
  A re-run measured 12.2 ms and then 8.3 ms, so the outlier came from the load
  spike, not from the code.
- `repo_read` was therefore re-run on its own, at load 22.5 (1.2x), and that
  JSON is the one committed.

Every bench JSON carries its own host snapshot.

Against the pre-fix baseline below, the comparator flagged only the intended
work-counter changes and the renamed row:

| row | before (`a92e40fb`) | after (`43f54d7f`) | card |
| --- | --- | --- | --- |
| insights snapshot, git processes | 37 | **21** | ft-c242f35b |
| MCP `tools/call insights`, git processes | 11 | **6** | ft-c242f35b |
| commit details, git processes | 2 | **1** | ft-fa9dbbe1 |
| language stats, git processes | 1 | 2 (adds `check-attr` for linguist attributes) | ft-9cc9813f |
| live-index storm, builds started for 40 ticks | 40 (duty cycle 1.00) | **1 (duty cycle 0.04)** | ft-fa689914 |
| `list_worktrees` | one row, `ScanDepth::Full`, 39 git processes | split: `full` 38, **`listing` 1** | ft-28c58695 |

`command_timings` uses N = 15, and N = 5 for the cold catch-up. Its fixture has
600 files, 61 commits and 18 worktrees, 17 of them dirty. Load was 8.9.

| row | min | p50 | p95 | counters |
| --- | --- | --- | --- | --- |
| insights snapshot | 165.0 ms | 248.8 ms | 270.8 ms | 18 worktrees, 7 facets ok, 21 git processes |
| list_worktrees full | 349.1 ms | 435.7 ms | 489.3 ms | 18 worktrees, 38 git processes |
| list_worktrees listing | 7.3 ms | 23.3 ms | 24.6 ms | 18 worktrees, 1 git process |
| language stats | 17.6 ms | 32.3 ms | 39.3 ms | 600 of 600 files, 8 languages, 2 git processes |
| docs refresh | 387.3 us | 446.1 us | 757.4 us | 75 notes |
| catch_up steady | 34.3 ms | 48.7 ms | 53.6 ms | 0 recorded |
| catch_up cold | 61.0 ms | 69.8 ms | 71.6 ms | 485 recorded, 60 transcripts, 5 reflog entries |
| commit details | 123.8 us | 156.2 us | 205.1 us | 1 parent, 1 git process (repeat views are cache hits) |

Other benches:
- **`live_index`**: duty cycle **0.04**, 1 build started and 39 ticks skipped by
  the cooldown, over 40 ticks.
- **`mcp_protocol`**:
  - `tools/call insights`: min 29.3 ms, 6 git processes.
  - `collision_risk`: min 25.1 ms.
  - `tools/list`: min 341.3 us, 57.7 KB (57,695 bytes; the timing is from the run before the typed-argument change).
- **`repo_read`**, from the re-run at load 22.5:
  - 1k files: clean 8.3 ms, dirty (50) 23.9 ms.
  - 10k files: clean 25.3 ms, dirty (500) 70.8 ms.
- **`graph_diff`**:
  - lane_solver: linear 100k in 34.5 ms, merge-heavy 100k in 22.9 ms.
  - word_diff: 10,000 chars in 356.6 us.
- **`watcher`**: 10,000 events in 13.1 ms.
- **`terminal_pty`**: the cards that change these numbers are terminal cards 11
  and 12. They landed after this baseline was taken (the `hx/lag3-terminal`
  merge), so these numbers are from before them; the baseline has not been
  re-seeded.
  - Flood: 30.5 MB/s, still 1,024 IPC messages per MiB.
  - Echo: 20.8 us idle and 7.6 us beside a flood.
- **`idle_wakeups`**: 2.2/s with no sessions, 12.2/s with 1, 80.8/s with 8 and
  315.4/s with 32. That is still about 9.8 wake-ups per second for each idle
  session, the reader's poll. Card 12, the kqueue reader, is what brings it
  down; it landed after this baseline, which has not been re-measured.
  - **2026-10-15 re-measure with the pollers (`0f0e0b4b`, bench rows added on
    unmodified production code; ft-79394019, Wave 0).** Three runs of N = 3
    windows of 5 s, `ri_interrupt_wkups`, `ri_pkg_idle_wkups` 0 throughout. The
    figure is each run's median of its 3 windows; the harness does not keep
    per-window values, so "min" below is the min of the three runs' medians.
    The host was loaded throughout (load average 38.3 / 21-86 / 67-86 on 18
    cpus, thermal `fair`, pressure `normal`), so absolute wakeups are an upper
    bound on a quiet machine; the floor and the rows moved together.

    | row | run 1 | run 2 | run 3 | min | over the floor |
    | --- | --- | --- | --- | --- | --- |
    | idle 0 sessions (process floor) | 2.40 | 2.40 | 2.40 | 2.40/s | n/a |
    | idle 1 / 8 / 32 sessions | 2.20 | 2.20 | 2.20 | 2.20/s | none (kqueue reader) |
    | idle mailbox | 2.20 | 2.00 | 2.20 | 2.00/s | none |
    | idle 8 / 24 watches | 2.20 / 2.40 | 2.20 / 2.20 | 2.20 / 2.40 | 2.20/s | none |
    | **idle alerts bridge** | 6.99 | 7.00 | 7.19 | **6.99/s** | about +4.6/s, the 200 ms `wait_readable` accept loop (5/s) |
    | idle alerts bridge, after the self-pipe accept loop (`pf/79394019-bridge`) | 2.20 | 2.20 | 2.20 | **2.20/s** | none: the accept loop blocks in `wait_readable_or_woken` and is woken only by a connection or by `stop`/`start`; 6.99/s before. Measured at load average 56.8 / 78.1 / 23.7 on 18 cpus, with the floor at 2.40 and the idle mailbox at 2.20/2.20/2.20 in the same runs |
    | **idle host-load sampler** | 2.80 | 2.80 | 2.80 | **2.80/s** | about +0.4/s, the 2 s tick |
    | workbench `external_changes`, window hidden | see the table below | | | | |
    | workbench `external_changes`, window visible | see the table below | | | | |

    The bridge row binds the listener under a throwaway `HOME` with no
    clients; the sampler row is `sysload::start` with `SamplerOptions::new`
    (`host_load` enabled, 2 s cadence, a full process scan per tick) after
    its first tick has settled. The sampler's 2 s scan adds only about one
    wakeup per tick; the CPU it costs is not what this bench counts. The
    external_changes watcher (1 s visible, 2 s hidden) had no row in wave 0:
    `run` needed a Tauri `AppHandle` with a real `main` window. Its schedule is
    now `external_changes::drive` over an `Env` trait, and
    `external_changes::bench_seam::IdleLoop` runs it on a real clock and a
    private profile with the window as a flag; that is what the rows below
    measure.

    **external_changes cadence, before and after (ft-79394019, wave 1, lane
    B2).** Before is `503761fd` (the seam as a pure refactor: 2 s hidden sleep,
    1 s visible sleep); after is `c0c037eb` (hidden loop blocks on a beacon,
    bounded by the 30 s window check and retention's 10 min clock). Three full
    runs each, N = 3 windows of 5 s, `ri_interrupt_wkups`, `ri_pkg_idle_wkups` 0
    throughout; each cell is that run's median of its 3 windows. Host load
    average (1 min) when each run started: before 142 (taken before its build;
    the bench itself reported 26.7 once running) / 52 / 51, after 81 / 45 / 75
    on 18 cpus (the runs were not alternated one-for-one: before first, after second,
    because each needs its own release build).

    | row | before runs 1 / 2 / 3 | after runs 1 / 2 / 3 | floor in the same runs (0 sessions) |
    | --- | --- | --- | --- |
    | idle external_changes (hidden) | 2.80 / 2.80 / 2.80 | 2.20 / 2.20 / 2.20 | 2.40 / 2.40 / 2.40 before; 2.40 / 2.40 / 2.20 after |
    | idle external_changes (visible) | 3.20 / 3.20 / 3.20 | 3.20 / 3.20 / 3.20 | same |

    Hidden: min 2.80/s down to 2.20/s, from +0.4/s over the floor (the 2 s
    sleep, 0.5/s computed) to none; the row's own round counter (last window of
    each run) went from 3 to 0 per 5 s, as the loop now wakes about twice a
    minute. Visible: unchanged at +0.8/s, the
    1 Hz pass the UX latency contract needs (an external task appears within
    about a second); it is deliberately not slower. One wakeup is 0.2/s at
    this window length, so differences below 0.4/s are one event.

### Pre-fix baseline (`a92e40fb`, loaded host), superseded

This is kept as the record of the numbers before Waves 1 and 2. The committed
JSON is the baseline above.

Machine: Apple M5 Pro (Mac17,8), 18 logical CPUs, 64 GiB, macOS 27.0.1, rustc
1.99.0, git 2.56.0, release profile (`cargo bench`). Library code is main
`a92e40fb` plus the watcher seam; benches at `9789a8e6` with the fixture and
format fixes made afterwards (the JSON says `dirty: true`; they are committed
as `6a24c3b1`, and change no counter or measured path). A `--smoke` run
(N = 3) compared against this full-mode baseline on the same loaded host passed
every counter and failed 3 of 40 time rows (`catch_up steady`, `commit details`,
`docs refresh`, 1.7x to 2.3x): min-of-3 against min-of-15 under load. CI classes
should therefore be seeded from the workflow's own smoke artifact, so the
baseline and the run have the same N.

**The host was not quiet.** Other repositories' builds and agents were running:
the one-minute load average was 43.7 at the start of the run and 24.0 at the end
(2.4x down to 1.3x of 18 CPUs), and was above 300 during the first compile. Every
time below is a loaded-host time; min-of-N is the least inflated statistic but is
still inflated. Treat the *counters* as exact and the *times* as an upper bound
for this machine, and re-seed this class from a quiet run when one is available.
N is the sample count per row (full mode, not `--smoke`).

`graph_diff` (N = 100 word diff, 30 lane solver 10k, 7 lane solver 100k):

| row | min | p50 | p95 | counters |
| --- | --- | --- | --- | --- |
| word_diff 100 chars | 3.83 us | 4.88 us | 9.79 us | 5 / 5 segments, 4 changed |
| word_diff 10,000 chars | 374.6 us | 465.5 us | 560.3 us | 74 / 74 segments (500-token cap) |
| word_diff 60,000 chars | 1.62 us | 2.92 us | 5.21 us | 1 / 1 segment (past the 50,000-byte guard) |
| lane_solver linear 10k | 2.45 ms | 2.89 ms | 3.34 ms | 10,000 rows, 9,999 connections, width 1 |
| lane_solver merge-heavy 10k | 1.88 ms | 2.33 ms | 2.81 ms | 9,997 rows, 8,330 connections, 1,666 merges, width 4 |
| lane_solver linear 100k | 41.07 ms | 43.12 ms | 45.32 ms | 100,000 rows, 99,999 connections |
| lane_solver merge-heavy 100k | 24.83 ms | 25.82 ms | 29.00 ms | 99,601 rows, 83,000 connections, 16,600 merges, width 4 |

`watcher` (N = 60): `record_event_signal` over 10,000 events: min 15.14 ms, p50
18.21 ms, p95 20.17 ms; 549k events/s at p50-ish (metric); 7,000 admitted, 3,000
dropped, 64 paths recorded (the window's bound; `truncated`), 3 change classes.

`repo_read` (N = 15):

| row | min | p50 | p95 | rows | git processes |
| --- | --- | --- | --- | --- | --- |
| get_status 1k files clean | 11.63 ms | 14.54 ms | 31.51 ms | 0 | 1 |
| get_status 1k files dirty (50) | 34.12 ms | 49.69 ms | 67.98 ms | 50 | 2 |
| get_status 10k files clean | 31.22 ms | 36.52 ms | 42.20 ms | 0 | 1 |
| get_status 10k files dirty (500) | 96.64 ms | 110.10 ms | 145.72 ms | 500 | 2 |

Opening and saving a file (ft-f16fad35), `repo_read` rows added in the same
bench. One text file in a clean 10-file repository; the save alternates two
same-size texts so the publish is real. Three interleaved A/B pairs (before,
after, before, ...) of N = 15 each, min over the three, on the shipped build
(integration head c13d0af8, with the per-block lane mixing and the `as_chunks`
loop), host load 6.5 to 21.9 on 18 CPUs. "Before" is the release bench binary
built without the base token; "after" adds the base token a save is judged
against: one digest pass over the bytes `get_file_blob` reads, and one over the
bytes the save's entry transaction reads. The `get_file_blob` gap held in every
pair (1 MB: 0.966/1.239, 0.966/1.251, 1.117/1.235 ms; 8 MB: 7.64/9.22,
7.64/9.20, 8.46/9.62 ms), while the save rows moved in both directions.

| row | before (min) | after (min) | change |
| --- | --- | --- | --- |
| get_file_blob 1 KB | 129.9 us | 134.6 us | +3.6% |
| get_file_blob 1 MB | 0.966 ms | 1.235 ms | +27.9% |
| get_file_blob 8 MB | 7.64 ms | 9.20 ms | +20.4% |
| save_edited_text 1 KB | 7.63 ms | 6.97 ms | within run-to-run spread |
| save_edited_text 1 MB | 8.74 ms | 8.58 ms | -1.9% |
| save_edited_text 8 MB | 18.40 ms | 17.87 ms | within run-to-run spread |

An earlier build without the per-block lane mixing measured +14% / +10%; the
mixing that closes the same-lane collision (see `digest` in
`diff/conflict_fs.rs`) costs the difference.
The cost applies only to the editor open (`cmd_get_file_blob`, whose base names
the bytes the user saw). View-only reads (`get_file_content`, the code graph's
file text) use `get_file_blob_view` and skip the digest pass.

The diff viewer's image reads pass `withDigest: false` to `cmd_get_file_blob`
and take the `get_file_blob_view` path. `repo_read` carries a
`get_file_blob_view` row beside each `get_file_blob` row (2026-10-18, load
average 25.9 on 18 CPUs, 15 samples, min): 1 MB 1.22 ms to 1.08 ms, 8 MB
9.47 ms to 8.09 ms, 1 KB 146 us to 79 us. The digest is gone from that path.
Single run, not interleaved A/B, on a loaded host.

The open path is over the 5% budget at 1 MB and 8 MB, and that is accepted as
the cost of correctness. Without a base, a save writes the whole draft over
whatever an agent or `git pull` put on disk while the file was open, with no
prompt (D-frontend-03); with one, the backend refuses it. The base is a digest
of the bytes and has to be computed from the bytes the open read, because the
editor's textarea returns LF text while the save re-adds CRLF, so only the
backend can say what is on disk; a stat (mtime and size) misses a same-size
rewrite inside one timestamp tick and reports a conflict for a bare `touch`.
The remaining cost is one pass over memory already read, about 1.6 ms per 8 MiB
(roughly 5 GB/s), once per file opened in the editor. It is bounded by `MAX_FILE_BYTES` (8 MiB). The first digest,
FNV-1a, cost +130% (7.58 ms to 17.5 ms at 8 MiB) because its multiply is a
serial chain per byte; the shipped digest takes four lanes a word at a time.
It is an accidental-change detector, not collision-resistant against a crafted
input (see `digest` in `diff/conflict_fs.rs`).

Ledger redactor (ft-1de1ff48), `repo_read` rows `redact::text 4 fields`, `7
fields` and `transcript edit` (2,000 redactions per sample; the transcript edit
is the object path plus the 4-field `detail_json`), and `transcript_pass`. Five
interleaved A/B pairs (base, new, base, ...) of the release bench binary in
`--smoke` mode (3 samples per row per run), so N = 15 per side, min over all
15, host load 38 to 95 on 18 CPUs (so the absolute times are inflated; the
ratio held in every pair). "Base" is `1b3175dd` (the bench row added to the
unmodified redactor); "new" parses each string once, gates the 15-pattern
contextual chain with one `RegexSet`, and copies on write through the chain and
the vendor-token pass.

| row | base (min) | new (min) | change |
| --- | --- | --- | --- |
| redact::text 4 fields | 32.17 ms (16.1 us/call) | 23.70 ms (11.9 us/call) | -26% |
| redact::text 7 fields | 60.17 ms (30.1 us/call) | 42.72 ms (21.4 us/call) | -29% |
| redact::text transcript edit | 38.94 ms (9.7 us/call) | 28.17 ms (7.0 us/call) | -28% |
| transcript_pass 100k lines | 4.376 s | 3.606 s | -18% |
| transcript_pass 10k cwds | 441.3 ms | 360.7 ms | -18% |
| transcript_pass 10k departed cwds | 497.4 ms | 403.9 ms | -19% |

This is about a quarter off, not an order of magnitude: the remaining cost is
dominated by dc-redact's per-segment `contains_secret` (its own pattern set,
vendored, not editable here) and by the one failed JSON parse per leaf. The
vendor-token skip is exact (`!contains_secret(segment)` implies
`redact_secrets(segment) == segment` for a segment with no whitespace; see the
property test `a_segment_dc_redact_does_not_detect_is_returned_unchanged`), so
a cheaper pre-filter has to come from dc-redact itself.

`command_timings` (N = 15, cold N = 5; 600 files, 61 commits, 18 worktrees, 17
dirty):

| row | min | p50 | p95 | counters |
| --- | --- | --- | --- | --- |
| insights snapshot | 485.0 ms | 547.2 ms | 613.6 ms | 18 worktrees, 7 facets ok, 37 git processes |
| list_worktrees | 541.6 ms | 579.7 ms | 642.0 ms | 18 worktrees, 39 git processes |
| language stats | 55.4 ms | 75.7 ms | 86.2 ms | 600 of 600 files, 8 languages, 1 git process |
| docs refresh | 11.4 ms | 20.4 ms | 23.1 ms | 75 notes |
| catch_up steady | 35.8 ms | 69.7 ms | 86.6 ms | 0 recorded, 0 transcripts (watermark) |
| catch_up cold | 76.2 ms | 97.5 ms | 112.1 ms | 485 recorded, 60 transcripts, 5 reflog entries |
| commit details | 16.1 ms | 40.8 ms | 50.4 ms | 1 parent, 2 git processes |

`mcp_protocol` (N = 200 protocol rows, 30 work rows): `tools/list` min 349.8 us
(57.7 KB, 57,695 bytes; the timings on this line are from the run before the
typed-argument change, the sizes are after it), `server/discover` 3.3 us, `resources/read server/manifest` 605.8 us
(122.8 KB), validation 0.4 to 0.8 us; `gitpulse_status` min 69.3 ms,
`collision_risk` 36.0 ms, `insights` 118.8 ms with 11 git processes and 7 facets
ok. `tools/list` is 339x cheaper than one insights call.

`terminal_pty` (N = 9, not time-gated): 16 MiB flood in min 548.7 ms, **25.6
MB/s**, **1,024 IPC messages per MiB** (16,384 for the flood; one message per
1 KiB read); keystroke echo p50 of a 12-key sample **29.0 us idle** (min) and
**15.2 us beside a flood** in another session (2.5 MiB flowing during the
keystrokes), worst key 0.05 ms: a flood in one session did not delay another
session's echo on this host.

`idle_wakeups` (N = 3 windows of 5 s, not time-gated), `ri_interrupt_wkups`:
**2.2/s with no sessions, 12.2/s with 1, 80.9/s with 8, 317.0/s with 32: about
9.8 wakeups per second per idle session** (the reader's poll). `ri_pkg_idle_wkups`
read 0 throughout.

`live_index` (N = 3 storms of 40 ticks, 250 ms apart, stub build 300 ms): **duty
cycle 1.00, 40 builds started for 40 ticks**, 40 status probes. On main, every
`repo-changed` tick of a storm starts a build, so the indexer is busy for the
whole storm. This is the figure the live-index governor (lane
`hx/lag1-live-index`) should bring down; the bench goes through the public
`devmap::live::maybe_refresh` only.

`worktree_teardown` (N = 5, one fresh worktree per sample holding 100 000
ignored files, 1000 directories of 100 hard links of one 512 B file): before
any lock split, **`remove_worktree` holds the repository mutation lock for
4.38 s (min), and a `lock_worktree` issued 2 ms into it waited for the whole
removal** (`contender_wait_ns` about 4.66 s, median). `merge_and_teardown` takes
4.30 s in all against 0.12 s with no entries, so the entries are about 97% of
it; 22 git processes in the merge path, 4 in the plain removal. The decision
rule (more than about 50 ms under the lock at 100k) called for the rename-aside
split.

After the split (the lock is held across the rename and the record removal;
the files are deleted once it is released), the row is timed to the moment the
lock is released: **13.5 ms (min, N = 5) at 100k entries**, a `lock_worktree`
issued mid-teardown waiting 22 ms (median), and the deletion itself, 3.66 s
(`delete_ns`, median), now outside the lock. `merge_and_teardown` total is
3.74 s against 0.11 s for the control: the merge path still waits for the
deletion before it returns (the hooks run after it), it just no longer blocks
the repository's other mutations while it does.

## 2026-10-13: `git status` against user-enabled `core.untrackedCache` / `core.fsmonitor` (spike ft-7450105d)

Question: is there a case for a Health hint that tells a user how to turn these
two git features on? **Owner decision, unchanged by this spike:** GitPulse never
writes repo config and never passes `-c core.untrackedCache` or
`-c core.fsmonitor`. Two reasons stand independent of the numbers:

* every git child runs with `GIT_OPTIONAL_LOCKS=0` (`engine/git_cli.rs`), which
  stops `git status` from refreshing the index, and the untracked cache is a
  feature of the index, so `-c core.untrackedCache=true` could neither help nor
  persist;
* `-c core.fsmonitor=true` starts a long-lived `git fsmonitor--daemon` per
  repository that procguard and the spawn gate do not own, and that outlives
  GitPulse.

`storage/mod.rs` passes `core.fsmonitor=false` on `ls-files`, so a read there
cannot start the daemon. `tests/git_config_posture.rs` scans the production
source and fails on any `git config` write argv, and on any
`core.untrackedCache` / `core.fsmonitor` that is not `=false`.

### How it was measured

`node scripts/bench-git-status-config.mjs --repo <source> [--synthetic <files>]
[--untracked <n>] [--rounds <n>] [--json <file>]`. Each source is cloned four
times into `tmp/` (default, untrackedCache, fsmonitor, both) and the user's
configuration is applied to the clone with `git config --local`; the source repo
is never touched. `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1`
keep the default variant default. Per round every variant runs once, the order
rotated; the figure is min-of-N (and p50) of the warm rounds, after two plain
`git status` runs (the user's own, which populate the cache and start the
daemon). Two invocations: **gitpulse** (`STATUS_ARGV` with
`GIT_OPTIONAL_LOCKS=0`, what GitPulse runs) and **plain** (a terminal). Cold is
the first GitPulse-style status after configuring, with the OS page cache not
dropped, so it is indicative only. Every clone's daemon is stopped with `git
fsmonitor--daemon stop` before its directory is removed, on normal exit and on
SIGINT / SIGTERM; the script counts the daemons it started by the IPC socket
each holds open under its own clones, and fails if any outlives the run
(**leaked after cleanup: 0** in every run below).

Host: macOS arm64, 18 cpus, git 2.56.0, **loaded** (load average 10 to 44 at the
start of the runs; other agent sessions were building). Min-of-N and the
interleaving are the mitigation; read ratios, not single milliseconds. Sources:
this repository (2,798 tracked files), DevCouncil (6,220) and Manvi (977) as
read-only clone sources, plus **synthetic** repositories (flat `src/pkgNNNN/`
trees of one-line files) standing in for repositories too large to keep around.

### Result: warm `git status`, min of N, GitPulse invocation

| repository | tracked | default | untrackedCache | fsmonitor | both |
| --- | --- | --- | --- | --- | --- |
| Manvi | 977 | 9.6 ms | 9.0 (0.94x) | 11.6 (**1.21x**) | 10.2 (1.07x) |
| GitPulse | 2,798 | 13.2 ms | 13.2 (1.00x) | 18.2 (**1.38x**) | 13.8 (1.05x) |
| DevCouncil | 6,220 | 22.1 ms | 21.7 (0.98x) | 20.1 (0.91x) | 19.5 (0.88x) |
| synthetic | 40,000 | 49 to 51 ms | 48.9 to 52.0 (1.00 to 1.02x) | 27.8 to 39.0 (**0.55 to 0.80x**) | 29.0 to 30.9 (0.59 to 0.63x) |
| synthetic | 100,000 | 115.1 ms | 113.3 (0.98x) | 64.6 (**0.56x**) | 65.1 (0.57x) |

(The 40,000-file row spans three runs, with and without 3,000 extra untracked
files; the 6,220 / 2,798 / 977 rows are from the 15-round run. The plain
invocation gives the same picture: fsmonitor 1.29x slower at 977 files, 1.73x at
2,798, 0.91x at 6,220, 0.49 to 0.63x at 40,000 and above.) The untracked cache
was verified to be in effect (the `UNTR` extension is present in the index of
the two variants that enable it) and made no measurable difference at any size
here, with or without 3,000 untracked files, in either invocation.

Daemon cost, per repository with the daemon on: **one extra process, about 8 MB
resident, about 0.02 s of CPU over the whole run, 0.0 idle wakeups per second
sampled with `top` on a quiet tree** (low confidence: `top`'s IDLEW column, not
`ri_interrupt_wkups`, and a quiet tree is the best case; a build that writes
continuously costs the daemon one event per write).

### Decision

* **No Health hint is shipped.** `core.fsmonitor` only pays above roughly 10k
  tracked files (parity at 6k, 20 to 45 percent faster at 40k, 44 percent at
  100k), and it **costs 20 to 40 percent more** on the 1k to 3k-file repositories
  that are the common case, because every status then waits on a round trip to the
  daemon. The absolute saving at the top of the range, 20 to 50 ms per status,
  is on a background read. `core.untrackedCache` showed no effect, and under
  `GIT_OPTIONAL_LOCKS=0` GitPulse's own status cannot refresh it.
* **If a hint is ever added**, it is for `core.fsmonitor` only, shown only when
  the repository has at least 40,000 tracked files, `core.fsmonitor` is unset,
  and git is new enough to ship the built-in daemon; it shows the `git config`
  command for the user to run themselves and writes nothing. Never for
  `core.untrackedCache`.
* **Self-echo (cookie write) risk is covered, not eliminated.** With the daemon
  on, every `git status` writes a cookie under `.git/fsmonitor--daemon/cookies`.
  The watcher classifies those writes (and a linked worktree's) as noise
  (`watcher/mod.rs`, tests `fsmonitor_daemon_writes_are_noise_but_lookalikes_are_not`,
  `a_linked_worktrees_fsmonitor_writes_are_noise_to_the_main_worktree`,
  `a_status_read_under_fsmonitor_does_not_announce_itself`), so enabling it does
  not create a refresh loop; the daemon process itself is the user's to manage,
  and `git fsmonitor--daemon stop` is the only thing that ends it.
* **Not measured:** installed-app behaviour (the app's own status is spawn-gated
  and admission-controlled); Linux and Windows (the built-in daemon differs);
  repositories with very large ignored trees (where the untracked cache is
  documented to help, and which the synthetic fixtures do not model); the page
  cache is never dropped, so "cold" is indicative only.

## 2026-10-13: the pulse walk no longer verifies signatures (ft-9cfa4e7403d33c0e25e752db97175af9)

`%G?` runs the user's signing program for every signed commit it prints. The
pulse walk carried it, so its cost was the number of signed commits times the
verifier (10 s for one PGP commit, measured). The walk now reads whether each
commit carries a signature from the commit objects and starts no verifier; the
selected commit is verified alone by `cmd_verify_commit_signature` (30 s limit,
cached for 60 s against the config files, every file the config includes, the
allowed-signers and revocation files and the keyring, and coalesced so two
selects of one commit start one signing program). Commit details keep
`git_processes = 1`.

### How the walk reads signatures

Git has no placeholder for "is signed" that does not also verify, so the header
is read from the object. A first version fed the log's ids to one
`git cat-file --batch` after the log; that put a whole extra process on the
critical path (the adversarial pass measured +31 to +34 ms, about +90%, on the unsigned 5,000-commit walk). The walk now runs
`git rev-list --header` over the same refs **beside** the log, on its own
thread under the caller's admission class, and parses its stream as it arrives
(`ProcessObserver::drains_stdout`: each chunk is dropped once the parser has
seen it, the byte cap still counts it). Per commit only the id and the
signature kind are kept; per record only the id line and the header run the
classifier reads (256 KiB at most). `--pretty=raw` would skip `--use-mailmap`,
so the log keeps its own format. A commit the stream does not hold (output cut
at its 32 MiB budget, git failed or timed out, refs moved between the two
reads) is `unknown`, never `none`.

### A/B

Release builds, `command_timings` rows, N = 5, min-of-N, on a loaded host (load
15 to 33 on 18 CPUs; absolute times are inflated, ratios are the figure). A is
the base commit `8026dd7e` with the same bench file
(`GITPULSE_BENCH_ALLOW_VERIFIER=1`, which the bench otherwise refuses without,
because a walk that starts the signing program at all would be timed as the
verifier). B is this change. Runs were B, A, B, A (columns are run 1 / run 2).

| row | A min | B min | B vs A (min of both runs) | verifier calls A / B | git processes A / B |
| --- | --- | --- | --- | --- | --- |
| pulse unsigned 500 | 7.82 / 9.49 ms | 10.96 / 10.91 ms | +3.1 ms (+40%) | 0 / 0 | 1 / 2 |
| pulse unsigned 5,000 | 28.16 / 29.26 ms | 32.46 / 29.50 ms | +1.3 ms (+4.8%) | 0 / 0 | 1 / 2 |
| pulse unsigned 25,000 | 119.74 / 132.26 ms | 120.62 / 118.21 ms | -1.5 ms (-1.3%) | 0 / 0 | 1 / 2 |
| pulse signed 0 | 25.13 / 31.21 ms | 25.57 / 41.75 ms | +0.4 ms (+1.8%) | 0 / 0 | 1 / 2 |
| pulse signed 50 | 2.06 / 2.20 s | 29.43 / 32.04 ms | 70x faster | 50 / 0 | 1 / 2 |
| pulse signed 500 | 21.06 / 22.72 s | 28.15 / 46.84 ms | 750x faster | 500 / 0 | 1 / 2 |

A grows with the signed count (about 40 ms per signed commit with the 20 ms
stand-in verifier, process start-up included); B does not depend on it.
`commit details` is unchanged (A 139 / 129 us, B 175 / 135 us, 1 process each,
loaded-host noise).

**The unsigned rows are within 5% of A at 5,000 and 25,000 commits and at 0
signed, and not at 500.** The 500-commit walk takes 8 ms, and the extra
`rev-list` child costs about 3 ms of process start-up and gate admission that
the walk cannot hide behind a log that short (+3.1 ms, +40%). Above 5,000
commits the log is long enough to cover it. It is a fixed cost per walk, not a
per-commit one (5,000 commits: +1.3 ms; 25,000: none), it is paid concurrently
with the log rather than after it, and removing it means not reading signatures
at all, which the Signed card needs. Accepted. (The first version, a batch read
after the log, was measured at +31 to +34 ms at 5,000 commits by the adversarial pass.)

### Memory

`peak_rss_mb` is this process's high-water mark, so it is read from
`GITPULSE_BENCH_ONLY="pulse unsigned 25000"` runs that did nothing else
(a 500-commit row in the same harness peaks at 12.3 to 13.1 MB, the harness
baseline). 25,000 commits, two runs each: A 76.1 / 70.3 MB, B 70.7 / 78.0 MB.
B is within the run-to-run spread of A. The parser keeps an id and a kind per
commit and one bounded record, and a test feeds it 16 MiB of message and
asserts the record buffer stays under 257 KiB; the earlier version that
buffered the whole `rev-list --header` text was measured at about 87 MB against 46 MB for A by the adversarial pass (not re-measured here)
in that harness. The figure includes the harness's own fixture (the import stream, the
count pass), which is why it is far above the 12 MB baseline for both builds;
git's own memory is not in it.

### DORA

`--since-as-filter` walks past the first old committer date instead of stopping
there, so the scan reads commits plain `--since` skipped. **On a dormant
repository the filter walks the whole history to find nothing:** with an empty
window and 25,000 commits the adversarial pass measured about 60 ms where plain `--since` costs
about 10 ms. That is bounded by the 60 s DORA deadline and by the history size,
and is the price of not losing the in-window commits behind one inverted date.
On git older than 2.37 the scan is count-bounded (`-n cap+1`, window applied
here); a row past the cap is reported as a possibly cut window, because without
a date bound nothing says the window does not continue behind it, and the
window's true size is not claimed.

## 2026-10-15: hub rename stages in one batch (ft-434d7befe170abdb85cb4e91264d9e6e)

Renaming a note that 5,000 notes link to ran one `git add` per rewritten file
(`GitWriter::stage_file`, about 16 ms each) and one `guard_file` policy call per
file. Staging is now one `git add --pathspec-from-file=-` per 2,500 files
(`GitWriter::stage_files_attributed`, which reuses `change_index_with`), and the
rewrites are judged together (`harness::guard_files`).

### Measured

`docs_rename_benchmark` row `target=hub`, 5,000-note corpus, 5,000 rewrites,
release build, **min of 3** end to end (`rename_doc`, gate `|_| Ok(())`), on a
host with other agents building and testing at the same time. Same benchmark
body for both rows (only the hub run count and a `runs=` column were added).

| build | `rename_end_to_end_ms` (hub) | `git add` processes |
| --- | --- | --- |
| before (`0f0e0b4b`) | 150,106 | 5,000 |
| after | 44,642 | 2 |

The leaf row (1 rewrite, min of 7) is unchanged within noise: 131.5 ms before,
152.1 ms after.

### What the policy calls cost (counting gate)

The rows above ran with a gate of `|_| Ok(())`, which says nothing about the
per-file policy cost. `docs_rename_benchmark` now runs the rename through a
counting gate (`counting_gate_rename`, `docs/rename_tests.rs`) that makes the
calls `commands::knowledge::docs_rename_guarded` makes (the move, the source
delete, the destination write, then every other rewritten note through the
fan-out `harness::guard_files` uses, `judge_in_order`) and counts and times
each one. A simulated per-call latency is slept inside every call to stand in
for the sidecar round trip. **The latency is a stand-in, not a measurement of
the sidecar**, and the gate does not do the real scope lookup or IPC; it
measures the call count, the fan-out and what that fan-out buys.

Release build, 5,000-note corpus, hub target, **min of 5**, the four arms run
interleaved in each round, on a host with other agents building at load (the
hub rename takes 84 to 101 s here against 44.6 s above, so read the shares, not
the absolute rename times):

| latency per call | workers | policy calls | files judged | gate wall (ms) | sum of call times (ms) | rename (ms) | gate share |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0 ms | 1 | 5,002 | 5,001 | 0.56 | 0.19 | 97,315 | 0.0% |
| 0 ms | 8 | 5,002 | 5,001 | 2.00 | 6.62 | 92,792 | 0.0% |
| 2 ms | 1 | 5,002 | 5,001 | 18,635 | 18,622 | 101,509 | 18.4% |
| 2 ms | 8 | 5,002 | 5,001 | 2,401 | 19,109 | 83,781 | 2.9% |

- One rename is 5,002 policy calls: the `git mv` line, the source delete, the
  destination write, and 4,999 rewritten notes (the moved note's own rewrite is
  judged as the destination). 5,001 distinct files, each once; the bench
  asserts no file is judged twice.
- With a free gate the policy is nothing (0.0% of the rename). With 2 ms a call
  one worker costs 18.6 s (18.4% of the rename); the production 8 workers cost
  2.4 s (2.9%), a **7.76x** shorter gate (the bench asserts it beats one worker
  within `coverage_relaxed`). What `GUARD_FILES_CONCURRENCY` buys is therefore
  about 16 s per hub rename at 2 ms a call, and it scales with the sidecar's
  real latency, which this bench does not know.
- Leaf target (no linking notes): 3 calls, 2 files; at 2 ms the gate is 7.6 ms
  with 1 worker and 24 ms with 8 (the workers do not overlap anything there and
  the spawn plus scheduling noise dominates). No speed claim is made for it and
  the bench asserts none.
- Not measured: the real sidecar's round-trip latency, the scope lookup, and
  the ledger recording `guard_files` does per verdict. (Measured below.)

### The real sidecar round trip (2026-10-16, base b91c79fc)

`docs_rename_live_sidecar_benchmark` (rename_tests.rs, `#[ignore]`d) runs the
same 5,000-note hub through the real policy path against a live `manvi serve`
sidecar: `harness::guard_file` for the source and destination, then
`guard_files`' production walk (scope once, judge, record in the ledger, gate)
at 8 workers and at 1. It reaches the real path through two `#[cfg(test)]`
hooks in harness/mod.rs (`guard_files_with_workers_for_bench`,
`time_file_checks_for_bench`); no production code changed. The sidecar runs
under the test build's hermetic HOME. The `git mv` line is not judged in the
live arms (4,999 rewrites plus 2 = 5,001 calls against the simulated arms'
5,002). Release build, **min of 5**, six arms interleaved and rotated each
round.

- manvi: `/Users/bharath/.local/bin/manvi`, v0.1.0, revision
  142fd8f67f2ca470decbd65b8d9387c472258820 (clean). First call (spawn plus
  call) 34.9 ms, excluded from the figures.
- Host load: load average 19.8 on 18 cores at the start, 12.2 at the end, other
  agents building throughout. Absolute rename times (39 to 48 s) are noisy;
  read the shares.

| arm | calls | per-call p50 | p95 | max | gate wall (min) | rename (best run) | gate share |
| --- | --- | --- | --- | --- | --- | --- | --- |
| no-op gate (0 ms, 8 workers): harness overhead | 4,999 | n/a | n/a | n/a | 1.5 ms | 41,485 ms | 0.0% |
| simulated 2 ms, 8 workers (control) | 4,999 | 2 ms (slept) | n/a | n/a | 1,526 ms | 48,101 ms | 3.2% |
| live, 8 workers, full gate incl. ledger | 4,999 | n/a | n/a | n/a | 1,162 ms | 39,082 ms | 3.0% |
| live, 1 worker, full gate incl. ledger | 4,999 | n/a | n/a | n/a | 1,770 ms | 43,080 ms | 5.0% |
| live calls only (no ledger, no rename), 1 worker | 4,999 | 0.18 ms | 0.39 ms | 0.95 ms | 1,133 ms | n/a | n/a |
| live calls only, 8 workers | 4,999 | 1.14 ms | 1.66 ms | 53.8 ms | 807 ms | n/a | n/a |

Per-call figures are the min over the 5 rounds of each statistic. The worst
single round for the 1-worker arm had p50 0.45 ms, p95 0.84 ms, max 8.8 ms; for
8 workers p50 2.14 ms, p95 3.49 ms, max 108 ms (queueing behind one sidecar).

- The real round trip is **about 0.2 to 0.5 ms** (p50, one call in flight),
  an order of magnitude under the 2 ms stand-in and far under the 10 to 20 ms
  that would justify a new op. The harness's own overhead is 1.5 ms for the
  whole walk, so the figures above are the sidecar, not the test.
- The whole gate, with scope lookup and a ledger record per verdict, is 1.2 s
  with 8 workers and 1.8 s with 1: **3.0% and 5.0%** of a hub rename, and the
  rename is dominated by the durable writes in the next section.
- 8 workers overlap only about 1.5x on the calls alone (807 ms against
  1,133 ms) and per-call latency rises to about 1.1 ms: the sidecar serves them
  nearly serially. The fan-out saves about 0.6 s per hub rename, not the 16 s
  the 2 ms simulation implied.
- Decision input: at the measured cost a multi-path policy op could save at most
  about 1.2 s of a 39 s rename (3%), so it does not clear the 15 to 30% bar.
  The caveat on this run was an unbound checkout and an empty ledger; the next
  section re-measures with both changed.

### The same measurement under realistic state (2026-10-16)

`docs_rename_realistic_state_benchmark` (rename_tests.rs, `#[ignore]`d) builds
five hub repositories and runs, per round and rotated, the calls-only arm at 1
and 8 workers and the full gate inside a hub rename at 8 workers in each. The
sidecar is the same manvi (v0.1.0, 142fd8f6) behind a counting wrapper that logs
every start. Release, **min of 5**. Hermetic HOME, never the user's profile.
Host load: 31 (peaking above 50) at the start, 5.9 at the end, on 18 cores,
so rename times (32 to 38 s here) and the 8-worker maxima are noisy.

| state | scope bytes per request | calls p50 / p95 / max, 1 worker | calls p50 / p95 / max, 8 workers | gate wall (min) | rename (best run) | gate share |
| --- | --- | --- | --- | --- | --- | --- |
| unbound, empty ledger | 0 | 0.15 / 0.19 / 0.44 ms | 0.98 / 1.13 / 53.8 ms | 1,145 ms | 35,271 ms | 3.5% |
| bound, glob scope (`notes/**`, `moved/**`) | 186 | 0.15 / 0.20 / 0.45 ms | 0.92 / 1.11 / 1.8 ms | 1,255 ms | 34,462 ms | 4.0% |
| bound, all 5,000 notes listed as planned files | 125,186 | 1.16 / 1.46 / 2.79 ms | 5.73 / 7.15 / 8.4 ms | 4,044 ms | 37,532 ms | 11.2% |
| unbound, 20,000 prior ledger events | 0 | 0.14 / 0.18 / 0.47 ms | 0.89 / 1.06 / 6.5 ms | 1,092 ms | 32,385 ms | 4.6% |
| bound glob scope, 20,000 prior ledger events | 186 | 0.16 / 0.22 / 0.59 ms | 1.03 / 1.21 / 29.1 ms | 1,205 ms | 37,848 ms | 3.6% |

- **The sidecar evaluated policy.** Every one of the 4,999 verdicts per state
  came back `checked=true`, `gate_failed=false`, `degraded=[]`. Unbound, all are
  `Demoted` with rule `task.absent` (the task rung was reached and the host
  posture demoted it); bound, all are `Allowed` with the declared task id. The
  harness errors on any check that could not run, so none is counted as fast.
- **A bound scope costs nothing at a realistic size.** A declared scope of a few
  entries adds 186 bytes to each request and no measurable time. Only a scope
  that lists every note (125 KB per request, 626 MB through the pipe per hub
  rename) moves the gate: 4.0 s, 11.2%.
- **A 20,000-event ledger costs nothing measurable** (1.09 and 1.20 s gates, the
  same as the empty ledger within noise).
- **No respawn.** One sidecar start across the whole run, including 25 full hub
  renames (`sidecar_starts_during=0` on each). The call timeout is 15 s
  (`DEFAULT_CALL_TIMEOUT`) against a worst observed call of 108 ms, and a
  timeout or transport fault is what arms the 30 s `RESPAWN_BACKOFF`, so a
  respawn needs a call to stall 140x longer than anything seen.
- **Rule set.** Manvi's file ladder is code (hard rules, scope rung) with three
  booleans read when the sidecar starts (hard rules, neighbour scope, same-dir),
  not a rule file that grows with a repository, so there is no larger rule set
  to load. The bound states exercise the scope rung. Whether an ambient
  `MANVI_*` flag changes the rung count was not tested.
- **Flip threshold.** With about 34 s of non-gate rename, 15% needs a 5.9 s gate:
  about 1.2 ms of effective cost per call at the 8-worker rate, about 4.6x the
  measured 0.25 ms (1.2 s over 5,001 calls). Only one state gets near it, the
  all-listed scope at 11.2%, and that is a payload cost (125 KB of scope
  re-sent with every call), not a round-trip cost.
- **Best case for a multi-path op.** One request carrying all 5,001 paths pays
  one round trip and sends the scope once. The gate keeps Manvi's per-path
  evaluation (not measurable from here, so it is not subtracted) and
  GitPulse's per-verdict ledger record (the full gate is about 0.45 s above the
  calls-only wall of 0.68 s unbound). So it saves at most about 0.7 s (2%) in
  the realistic states and at most about 3.6 s (10%) in the all-listed state,
  where the saving is the repeated scope payload (inferred from the 186-byte
  against 125,186-byte rows, not measured in isolation).

### What is left in the 44.6 s

It is not git and not policy. It is the durable write of each rewritten note:
`write_regular_expecting` calls `sync_all` on the temporary file and on the
directory, which on macOS is `F_FULLFSYNC`. A throwaway measurement of that
shape alone (file full-fsync, rename, directory full-fsync) costs 7.95 ms per
file against 0.27 ms for plain `fsync`, which matches 44.6 s / 5,000 = 8.9 ms.
Amortizing the directory syncs across files in one folder would need a batched
variant of that shared write transaction; it was not changed here because the
conflict-resolution saves rely on it.

### Bounds

- Git processes: `ceil(rewrites / 2,500)` for a clean batch, plus one per 128
  notes whose path is not ASCII. Those go as arguments, not on stdin: on macOS
  `git add --pathspec-from-file` exits 0 and stages nothing for a decomposed
  (NFD) spelling, which would have reported such a note staged while it was not
  (the old per-file `git add` was unaffected). The same fix covers the
  file-tree selection that shares `change_index_with`. A batch that fails
  is first cleared of ignored paths (`git add` exits 1 on one but still stages
  the rest, so those are named by `git check-ignore`, outside the budget), then
  halved to find the file at fault, at most 64 extra runs per rename; past
  that the files still ungrouped each carry the batch's error and say it was
  not attributed to one path. A held `index.lock` is never halved.
- Policy: one scope lookup, then at most 8 file checks in flight; the first
  refusal, or a check that could not run, stops new checks. Verdicts are
  recorded and gated in order, so the rename refuses on the same file it did
  before. The sidecar protocol has no batch op (`policy.check.file` takes one
  path), so each rewrite is still judged on its own.

## 2026-10-15: graph and stack history walks (ft-9cfa4f74, S4-03 and S4-04)

Bench: `cargo bench --manifest-path src-tauri/Cargo.toml --bench graph_spawns`.
Rows and fixtures are described in the bench's header. The cold rows move a tag
before every timed call so the ref cache cannot answer; the warm row reads a
repository whose refs have been idle for longer than the cache's racy window.

**The spawn counters are the gate.** This host runs at load 40 to 200 on 18
cores, so the time columns are recorded with the load beside them and do not
pass or fail anything.

### Baseline (unmodified `cd52ff3c`)

| row | git processes | min of 2 runs | run 1 min / p50 | run 2 min / p50 | load1 at start (run 1, run 2) |
| --- | --- | --- | --- | --- | --- |
| S4-04 graph default page, 5,000 rows of 6,000 commits | **12** | 61.3 ms | 70.0 / 91.1 ms | 61.3 / 73.6 ms | 49.3, 49.5 |
| S4-03 stack refresh, 20 branches beyond the window, cold | **28** | 237.6 ms | 237.6 / 286.6 ms | 264.6 / 315.8 ms | 49.3, 49.5 |
| S4-03 stack refresh, refs unmoved (warm) | **20** | 236.3 ms | 236.3 / 289.7 ms | 255.4 / 295.6 ms | 49.3, 49.5 |

N = 9 per row after one warmup, two runs. The warm stack row still spawns 20
processes: the first-parent repairs bypass the ref cache, so an idle repository
pays one walk per unresolved branch on every tick, and nothing else (the branch
listing, the history read and the default-branch probes are answered from the
cache). The cold row's 28 are 20 walks and 8 others. A spawn log of the same
refresh in a unit test names 7 of the 8: the default-branch probes (`config
--list`, two `symbolic-ref`, `remote`, a `for-each-ref` over the conventional
names), the branch listing and the history read. The eighth is not identified
(trace2 counts every process, including ones git starts itself).

**Counting the warm row.** `count_git_processes` rewrites the hermetic global
git config, and the ref cache stamps that file, so the first call inside a
counted block is always cold and nothing is kept until the new file is older
than the 3 s racy window. The warm count is the difference between two counted
blocks that wait that window out: one call, and two calls. (The first version
of this bench counted the warm row in one block and printed the cold figure,
28, beside a warm time; the pre-fix test
`a_stack_refresh_with_no_ref_movement_spawns_nothing` independently saw 20
`rev-list` walks and nothing else on a warm tick.)

Baselines were built from the unmodified tree at `cd52ff3c` plus this bench file
only (`src-tauri/src` and `src-tauri/tests` checked out from `HEAD`), and run
interleaved with the fixed build (A B A B) so host load lands on both.

### After

| row | git processes, before to after | min of 2 runs, before to after | after, run 1 min / p50 | after, run 2 min / p50 | load1 at start (run 1, run 2) |
| --- | --- | --- | --- | --- | --- |
| S4-04 graph default page | 12 to **11** | 61.3 to 35.4 ms | 40.7 / 63.0 ms | 35.4 / 45.6 ms | 47.8, 43.6 |
| S4-03 stack refresh, 20 branches beyond the window, cold | 28 to **9** | 237.6 to 19.4 ms | 23.4 / 27.5 ms | 19.4 / 22.3 ms | 47.8, 43.6 |
| S4-03 stack refresh, refs unmoved (warm) | 20 to **0** | 236.3 to 2.1 ms | 2.1 / 3.5 ms | 2.2 / 3.0 ms | 47.8, 43.6 |

The counters are the result; the times are recorded beside load 44 to 50 on 18
cores and gate nothing. What changed:

* **S4-04.** A kept history chunk is `chunk` rows plus the first row of the next
  one, so a window of n rows needs ceil((n - 1) / chunk) chunks. The default
  page is 5,000 rows plus the has_more probe: one `git log -n5001` instead of
  `-n5000` and a second `--skip=5000 -n5000` walk for one row. Chunks stay
  aligned and kept, so growing the window still reads only the chunks it has
  not held (`chunk_windows_around_every_boundary_read_the_fewest_walks` pins
  c, c+1, 2c, 2c+1 and 2c+2, and the existing grow test is unchanged). The
  one process saved is the 5,000-row walk and parse, not the whole page; the
  other 11 are not touched by this change (inferred: the graph's ref, HEAD and
  default-branch reads, and one more).
* **S4-03.** The 20 per-branch `rev-list --first-parent --max-count=100000`
  walks are one `rev-list --first-parent --parents` over every unresolved tip,
  read through the ref cache over `refs/heads`, with a 20 s deadline and the
  same 100,000-commit cap shared across the tips. The cap is read newest first
  across every tip, so a deep unrelated branch can spend it before a shallow
  branch's fork point is reached; a chain the shared cap cut therefore gets its
  own single-tip pass over the deadline that is left, as the per-branch walks
  had (only after the shared cap is exhausted, so it costs nothing in the
  common case). An idle repository's next tick
  is answered without a process. `--not` is deliberately not used: excluding
  the other tips also excludes whatever a tip sitting mid-chain can reach
  (`excluding_other_tips_would_cut_a_chain_a_mid_chain_tip_shares`), and the
  window's own commits cannot be excluded for the same reason. A branch the pass could not
  finish (cap, deadline, a failed git) is counted in the payload as
  `unresolved_branches`, with `walk_incomplete`, and shown in the Stack view;
  it was previously left parentless without saying so.
* **Not changed.** The default-branch probes (`config --list`, two
  `symbolic-ref`, `remote`, one `for-each-ref`) run again after any ref moves,
  5 of the 9 remaining cold spawns. They are outside this card.

## 2026-10-15: branch churn survives a default-branch advance (ft-9cfa497403d3338f25e75bdb97176a44, S4-06)

`churn_store` keyed a row `(repo, inputs, base, tip)`, so one commit on the
default branch missed every branch's row and `branch_stats` ran one
`diff --shortstat` per branch again (96 a call, serial, 5 s budget). The row is
now keyed `(repo, inputs, tip)` and records the base it was measured against.

The rule for keeping an answer over a moved base lives with the value
(`churn_store::Kept::carried`): the new base must descend from the recorded
one, and the tip's ahead count against the new base (from the one
`tip_counts_against` listing, always fresh) must be the recorded count. The
set of a branch's own commits can only shrink as the base grows, so an equal
count is an equal set, hence the same merge base and the same diff. Behind is
taken from the listing; the carried answer is re-kept against the new base. The
descent costs one `merge-base --is-ancestor` per distinct stored base, at most
`MAX_CARRY_BASES` (4) a call, and none when every row is already against the
current base. A rewound or replaced base, a branch the base absorbed (fully or
in part), a tip the listing did not count, a git without the listing, and any
changed input (the fingerprint is still in the key) are measured as before.

**Persisted data.** The table's primary key changed, so `SCHEMA_VERSION` is 2.
The file is now `churn.v2.sqlite`: named for the schema, so an older build
keeps its own `churn.v1.sqlite` and the two never drop each other's table
(sharing one name, every start of either build emptied the other's cache and
the loser fell back to memory;
`builds_of_different_schema_versions_do_not_rebuild_each_others_cache`). A
same-named file of another version is still dropped and rebuilt empty by
`ensure_schema` (`a_cache_from_before_the_base_left_the_key_is_rebuilt`). The
file is derived data, so the cost is one measurement per branch on the first
launch; the orphaned `churn.v1.sqlite` is left for the older build. No
DevCouncil-owned schema is involved.

Bench `churn_spawns` (new): 32 branches, each one commit ahead; `main` gets an
unrelated commit; the call after it. Process counts come from trace2 on two
repository copies (advance only, advance and call), so priming and the advance
do not count. Interleaved pre-fix / post-fix binaries, two rounds each, on a
host at load 91-104 on 18 cores; the counters are the gate, the times are
recorded.

| | shortstat | merge-base | ahead-behind listing | all processes | min (round 1, 2) | p50 (round 1, 2) |
|---|---|---|---|---|---|---|
| before | 32 | 0 | 1 | 44 | 650 ms, 666 ms | 874 ms, 731 ms |
| after | 0 | 1 | 1 | 13 | 162 ms, 181 ms | 212 ms, 253 ms |

The 13 after are the call's fixed reads (config, ref listings, the base
probes, the `for-each-ref` ahead-behind listing) and the one ancestry check.
`merge-base` is 0 before because `diff --shortstat <base>...<tip>` finds its
own merge base inside the shortstat process.

## 2026-10-16: `local-macos-aarch64` baseline brought up to the integration head (ft-f069aa425202f3abe84fe2a573dbbf6a)

The committed baseline had drifted from `bl/integrate-1013` (`c462eda6`), so the
gate failed on counters. A fresh `command_timings` run at that head against the
old baseline reported exactly one counter change; the two new benches had no
rows at all. Every counter that moved, and what moved it:

| bench / row | counter | before | after | cause |
| --- | --- | --- | --- | --- |
| `command_timings` / list_worktrees full | git_processes | 38 | **42** | CARD 20 (ft-9d044274, merge `94443dfb`): destructive callers read the default base fresh, not from the cache |
| `command_timings` / commit details | git_processes | 1 | **1** (no baseline change) | the bench counted the one-time `git version` probe as 2; the warm-up below removes it |
| `graph_spawns` (3 rows) | git_processes 11, 9, 0 | no row | added | CARD 17 (ft-9cfa4f74): the rows this card's own section above records as its "After" |
| `churn_spawns` (1 row) | git_processes 13 | no row | added | CARD 19 (ft-9cfa4974): the "after" row of the churn table above |

**list_worktrees full, 38 to 42.** Measured, not inferred: the same bench with
`src-tauri/src` taken from `94443dfb^1` (the integration head without CARD 20)
counts 38, and at `c462eda6` it counts 42. The `git` argv list at 42 is 18
`status --porcelain -z`, 17 `diff --no-ext-diff HEAD --shortstat`, one
`worktree list`, one `for-each-ref` over the ahead/behind listing and the
default-branch names, and the four default-base reads: `config --list -z`,
`symbolic-ref --quiet HEAD`, `symbolic-ref --quiet refs/remotes/origin/HEAD`
and `remote`. The listing is intended to start those four: the default-branch
protection (`is_default_branch`) and the default base now read config and HEAD
straight from git (`Reads::Fresh`) so an `include.path` change or a moved HEAD is
seen at once. The cached `default_base` path is unchanged.

**commit details, and the `git version` probe.** The old baseline said 1 and the
integration head's bench reads 2 without a warm-up (measured with the warm-up
removed, `--smoke`: the two processes are `git version` and the `git show`). The
second process is `git version`, the process-wide probe `git_preflight` runs once and
caches when it passes. `GitReader::get_commit_details` reaches it through
`git_supports(DIFF_MERGES_OPTION)`, so whichever row calls it first in a process
pays for it; the counted commit details pass was the first. Production does not
pay it per call: it is one process per run of the app, and a later call finds the
cached answer. The bench now calls `git_preflight` before the first counted pass
and asserts the status is Ok (a failed probe is never cached, so a broken git
would put the probe back inside a counted row), and the commit details pass
records its argvs and asserts none of them is `git version` (trace2's argv starts
with the git binary and the app's `-c` pairs, so the subcommand is matched as a
whole word after argv[0]). With the warm-up removed the assertion fails and
prints both argvs; with it restored the counter is 1.

**New rows.** `graph_spawns` and `churn_spawns` are seeded into the baseline
with `scripts/bench-compare.mjs --write-baseline` run over a directory holding
only their two reports, and those two benches spliced into the committed file
(load 0.4 to 0.5x when they ran). The existing `command_timings` times were not
re-seeded: that run was at load 19.5 on 18 cores (1.1x), and a baseline taken
then would loosen the time gate for every row. Only the `list_worktrees full`
counter was changed in that bench. Its min-of-N (349.1 ms) was left as it is;
the fresh run measured 321.2 ms with the extra four processes, so the time gate
is not weakened by the counter change.

`.github/workflows/benches.yml` runs a fixed `BENCHES` list that does not name
`graph_spawns` or `churn_spawns`. A class with a committed baseline that carries
them would fail the compare with "baselined but produced no report". Only
`local-macos-aarch64` carries a baseline today, and it is compared by hand; add
both benches to that list in the same change that seeds `ubuntu-latest` or
`macos-latest`.


## 2026-10-19: merge-tree per pair on a 2,000-file repository (ft-9cfd4d7403d5703225e3d7db97145421)

`gitpulse_merge_conflicts` merges every pair of worktree HEADs, so its cost is the
pair count (capped at 64) times one `merge_tree_candidate`. The figures below came
from a harness bench (the same fixture and rows, since moved into the crate as an
ignored test; the harness version printed p95 and the process count too). Run it
now with `cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --exact
insights::merge_conflicts_bench_tests::bench_merge_tree_per_pair --nocapture`, which
prints min and median per row and the load. 2,000 tracked files in
50 directories, three one-file lanes off `main`, rows interleaved, N = 15 per row
after 2 warm-up rounds, min-of-N. Load average at the start 17.15 (1 min) on 18 cores,
16.60 at the end: the host was busy with other lanes throughout, so read the ratios
and not the absolute milliseconds.

| row | n | min ms | p50 ms | p95 ms | git processes |
| --- | --- | --- | --- | --- | --- |
| `merge_tree_candidate` clean pair | 15 | 33.26 | 49.03 | 68.39 | 5 |
| `merge_tree_candidate` conflict pair | 15 | 33.18 | 54.22 | 85.76 | not counted |
| raw `git merge-tree --write-tree` (control) | 15 | 9.99 | 11.99 | 13.38 | 1 |

The five processes of a lone `merge_tree_candidate` are the two `git config` reads
that tell a partial clone, two `rev-parse` operand resolutions and the merge. The
conflict pair costs the same as the clean one: the merge itself is a few milliseconds
on this tree, and the rest is process spawns. The tool does not pay the guard per
pair: it tells a partial clone once per call and passes the worktrees' full HEAD
oids, so each pair is one `merge-tree` (plus the quarantine directory's create and
delete) through `merge_tree_resolved`. That per-pair figure is inferred from the
code path and the 10 ms control, not measured: the bench reaches only the public
entry point. The control writes its merged tree into the fixture's object store,
which is the write the quarantine exists to prevent.

## 2026-10-16: word diff annotates a bounded window (ft-02b836b8)

`DiffViewer` used to word-diff the whole del-run plus add-run of a replacement block
when its first row rendered (`annotateRange` over `replacementBlockBounds`), one LCS
per pair on the UI thread. `annotateAround` (`src/lib/diff/wordDiff.ts`) now annotates
the row's own pair plus up to `WORD_DIFF_PAIR_HALO` (24) pairs each side, nearest
first, within `WORD_DIFF_CELL_BUDGET` (1,000,000) LCS cells per call. Other pairs
stay plain until their own row renders.

`bun scripts/bench-word-diff.ts --whole` (2000 pairs, 3,895 chars per line, load 58
on 18 cores, min-of-N, first visible row):

| path | runs | min ms | pairs annotated |
| --- | --- | --- | --- |
| `annotateRange` whole block (before) | 2 | 2728.92 | 2000 |
| `annotateAround` (after) | 5 | 3.28 | 3 |

Not yet a row in the Rust bench harness above: this is a TypeScript bench with its
own JSON line, kept beside the harness until that harness can ingest it.

## 2026-10-16: commit-graph off, on, and with Bloom filters (ft-9cfa487403d331dc25e75cdb97176bf7, S4-05)

GitPulse now reads whether a repository has a commit-graph (`storage/commit_graph.rs`, surfaced under Git internals in the Storage view with the command to run). It reads the graph files and HEAD from the git directory and spawns nothing; writing or refreshing a graph from the app was Owner Decision 3, approved 2026-10-19 and built as a user-clicked, gated action (see "Update" below). This section measures what a missing graph costs.

`cargo bench --manifest-path src-tauri/Cargo.toml --bench commit_graph_history` (`-- --smoke`, `-- --json <path>`). Argv mirrors `engine/git_reader.rs` (topo-order history page, `--parents` path log, `for-each-ref %(ahead-behind:)`, `rev-list --count`, `blame --line-porcelain`), run through `git_cli::git_text` so the readers' caches do not hide the walk. Fixture: bare repositories built with `git fast-import`, 5k and 50k linear commits on `main`, a path (`rare/target.txt`) touched by 50 of them, and 20 branches three commits ahead of fork points along `main`. Each size is cloned three times (graph-off; `commit-graph write --reachable --no-changed-paths`; `--changed-paths`) and the 3 states are measured interleaved, 9 samples per row after 1 warm-up round, warm page cache. Each copy's state is asserted with the new detector, and each operation's output is asserted identical across the three, so a row cannot get faster by answering differently. The graph writes are fixture setup only.

**Load: 46 to 60 on 18 cores (2.6x to 3.3x oversubscribed; other agent sessions were building), git 2.54.0 (Apple Git-157), macOS arm64.** Absolute times are inflated and p95 is noise; read the min column and the ratios.

| operation (min of 9, ms) | 5k off | 5k graph | 5k +bloom | 50k off | 50k graph | 50k +bloom |
| --- | --- | --- | --- | --- | --- | --- |
| history page, first | 21.4 | 8.5 (2.5x) | 9.1 | 168.1 | 15.5 (10.8x) | 14.6 (11.6x) |
| history page, deep (skip n/2) | 20.1 | 8.1 (2.5x) | 9.2 | 162.6 | 17.6 (9.2x) | 14.6 (11.2x) |
| path log (rare path) | 25.0 | 12.9 (1.9x) | 10.9 (2.3x) | 260.0 | 115.4 (2.3x) | 38.7 (6.7x) |
| ahead/behind, 20 branches | 19.9 | 9.0 (2.2x) | 8.3 (2.4x) | 168.7 | 17.8 (9.5x) | 18.1 (9.3x) |
| `rev-list --count` | 10.8 | 7.0 (1.5x) | 8.0 | 50.1 | 11.2 (4.5x) | 10.2 (4.9x) |
| blame (rare path) | 21.8 | 13.9 (1.6x) | 13.1 (1.7x) | 209.0 | 91.1 (2.3x) | 44.5 (4.7x) |

Parenthesised figures are off divided by the state. Where the +bloom column carries none, the difference from `graph` is inside the noise of a loaded host (a Bloom filter does not help a walk that never filters by path).

Reading it: a missing graph costs a roughly linear walk, so it is invisible at 5k commits (about 10 ms to 20 ms extra) and large at 50k (150 ms on every history page and every ahead/behind listing, roughly 5x on path-limited reads). Changed-path Bloom filters matter only for path-limited reads, where they are worth another 3x on top of the graph at 50k (path log 115 to 39 ms, blame 91 to 44 ms). That is the shape the hint follows: it is advice only, it is shown for a missing graph, a graph without HEAD, and a graph without Bloom filters, and it names a single command (`git commit-graph write --reachable --changed-paths`).

Not measured: a repository where auto-maintenance keeps the graph fresh (this repository, 1,398 commits, topo 5000 in 29 ms, cannot show a gap); cold page cache; split chains versus a single file; the cost of writing a graph; repositories with many merges (the fixture is linear, which is the best case for the no-graph walk).

**Not in the committed baseline.** `benches/baselines/` is untouched and the bench is not wired into the scheduled `benches.yml` gate; whether to baseline it is a separate decision. The numbers above are a one-off run.

**Update 2026-10-19: the write (Owner Decision 3, approved; part 2 of this card).** The table above is the case for it: at 50k commits no graph costs about 10x on history pages and ahead/behind. The Storage panel now offers "Write commit-graph..." when the detector reports `missing`, `stale` or `no_bloom`, and runs exactly `git commit-graph write --reachable --changed-paths` after the person has seen that line and the common directory it writes under and confirmed it. `--changed-paths` is in the argv on these numbers: Bloom filters are another 3.0x on a path log (115.4 to 38.7 ms) and 2.0x on blame (91.1 to 44.5 ms) at 50k, file history and blame are screens GitPulse opens constantly, and without the flag a graph that lacks Bloom data (`no_bloom`) could never be fixed from the button. Blame's gain is 2x rather than 3x, so this is a judgement on a one-off, loaded-host run, not a measured need; dropping the flag is a one-line change to `COMMIT_GRAPH_WRITE_ARGV` and the contract that pins it. No `--split` (one file, no chain to maintain) and no git config write: `core.commitGraph` and `gc.writeCommitGraph` are untouched. The cost of the write itself is still not measured (listed under "Not measured" above); the run is bounded at 600 s, cancellable, and one per common directory.

## 2026-10-14: xterm WebGL renderer, built and NOT yet measured (ft-406d1f4a)

**Status: merged to main on 2026-10-16 on the owner's instruction, before the
A/B ran.** The owner approved `@xterm/addon-webgl` "if it's better", and the
win rule below decides "better"; that run is still owed. Until it meets the
rule, the renderer ships unmeasured, on by default, with Settings ›
Appearance › GPU terminal rendering as the way off. If a run misses the
rule, the renderer and the dependency are removed.

### What the branch carries

* `@xterm/addon-webgl` **0.19.0, pinned exact** (as `addon-search` is). It
  declares no peer range. npm shows it was published on 2025-12-22, 13:51 UTC,
  the same day as `@xterm/xterm` 6.0.0 (13:50 UTC), `addon-fit` 0.11.0 and
  `addon-search` 0.16.0. The harness checks below load it against 6.0.0 in
  Chrome and in WKWebView. `@xterm/addon-canvas` is not an option: nothing
  exists for xterm 6.
* `src/lib/terminal/gpuRenderer.ts` owns the lifecycle, and
  `TerminalSession.svelte` drives it from the same `onscreen` signal it
  reports to `terminalAttendance`:
  * A session takes a context only while it is on screen (the selected tab,
    or either half of a split, in an open dock on the active repository). It
    disposes the context the moment it is hidden.
  * Contexts come from one pool capped at `MAX_VISIBLE_SESSIONS` (8). Today
    the panel can put at most two sessions on screen, so the cap is
    insurance, not the bound in practice.
  * Context loss is handled through the canvas's own `webglcontextlost`
    event, captured on the session's host element. The addon's
    `onContextLoss` is kept as a backstop. Reading `addon-webgl.mjs` shows the
    addon `preventDefault`s the loss and waits **3 s** for a restore before it
    fires `onContextLoss`, with a blank canvas on screen the whole time.
    Disposing on the event itself is what keeps the pane from blanking.
  * The session is then marked `lost` (`data-renderer="lost"` on its
    container) and is never attached again. A context that cannot be created
    (no WebGL2) is final in the same way.
* Settings › Appearance › **GPU terminal rendering**, on by default. It is
  the escape hatch for a driver that draws wrongly, and it is how the A/B
  switches arms on one build.

### Verified

* `src/lib/terminal/gpuRenderer.test.ts`: attach, hide and show; synchronous
  dispose on loss with no re-attach across five hide/show rounds; a failed
  create treated as final; the cap of 8 across 11 sessions.
* `harness/terminal.html` passes 137 of 137 checks in headless Chrome 155 and
  137 of 137 in WKWebView (macOS 27), 2026-10-14. The GPU block checks that:
  * the on-screen session draws through WebGL, and the hidden tab and the
    other repository's session hold no canvas;
  * hiding the dock releases the context, and showing it attaches a fresh one;
  * `WEBGL_lose_context.loseContext()` leaves no canvas, the session reads
    `lost`, and DOM rows carry text two frames after the loss event (within
    1 s, not 3 s);
  * a hide/show cycle does not re-attach it, while another session still gets
    WebGL;
  * turning the setting off returns a live WebGL session to DOM rows.

  This is what proves the brief's inferred claim that xterm reverts to its
  DOM renderer when the addon is disposed. With the capture listener removed,
  the run fails at "a lost context is disposed at once".
* A host with no GPU was reproduced with `--disable-gpu`. Chrome then
  refused WebGL2 and the page stopped at its WebGL2 check. With
  `--enable-unsafe-swiftshader`, which the runner now always passes, the page
  passed 137 of 137 on software GL. This was not run on the CI Linux runner
  itself.
* The other checks on that page read xterm's DOM rows, so the page runs them
  with the setting off and turns it on only for the GPU block. As a result,
  hover, link and search behaviour **under WebGL** is not exercised.

### The bench, and why it has not run

`scripts/tui-redraw-bench.mjs` runs inside a GitPulse terminal session, so
its bytes take the production path: PTY → reader → per-session output
Channel → xterm → ack. The bench does not use the legacy events sink that
`terminal_pty` measures.

* `--mode redraw` repaints every cell at 60 Hz for 30 s.
* `--mode flood` writes 100 MiB and is timed to the DSR answered after the
  last byte.
* `--mode idle` waits 60 s.

Each run samples the test app's WebContent process with `ps -M`. `ps -M`
reads per-thread CPU of a WebContent process from a normal user shell; this
was checked on 2026-10-14. Each run appends one JSON line to the results
file. `--summarize` applies the win rule and fails closed:

* exit 0: ships;
* exit 1: misses;
* exit 2: undecided, with every missing piece named.

**The frame probe.** `redraw` also records `dropped_frame_fraction` and
`frame_p95_ms` from inside the page. The bench asks the session's terminal
over a private OSC: `OSC 7700 ; frames-start [; period=<ms>]`, then
`OSC 7700 ; frames-stop`. The terminal answers through the PTY as a terminal
reply, as it does for a cursor-position report
(`src/lib/diagnostics/frameProbe.ts`).

* Before the CPU sample starts, 2 s of idle frames measure the page's own
  rAF period. WebKit can run rAF slower than the display (60 Hz on a 120 Hz
  panel), so the display's refresh rate would be the wrong yardstick.
* A dropped frame is an interval longer than 1.5× that period, the
  definition ft-cc4be67b uses. That card can drive the same probe.
* The harness checks the probe answers exactly once, through the PTY, and
  draws nothing on the grid (139 of 139 in Chrome and in WKWebView). With
  the reply removed, the check fails.
* What a program in a terminal can learn this way: the page's frame-interval
  statistics over a window of at most 120 s. It gets no content, no other
  session's data and no input.

It has not run, for two reasons:

1. **`allowTransparency` is undecided.** ft-cc4be67b is still in backlog. Both
   arms must use the value it decides, and that card has to run first.
2. **It needs an attended window and a quiet host.** The installed app must
   be quit, and no other WebKit host app can be running. On 2026-10-14 the
   load average was 32 on 18 cores, with 10 agent sessions live.

Known limits of the apparatus, as built:

* The "main thread" is the first thread `ps -M` lists. That is inferred from
  creation order; `ps` prints no thread names.
* The echo figure is the round trip of a DSR (`ESC [6n`) sent every 250 ms
  during the redraw. It is a **proxy** for keystroke-echo p95: it covers the
  PTY, the Channel and the parse on the main thread, but not the keydown or
  the final paint.
* `--arm` and `--transparency` are declared by the person running the bench.
  The script cannot observe them.

### Procedure, once ft-cc4be67b has decided

1. Set `allowTransparency` in `TerminalSession.svelte` to the decided value
   and build. Copy the bundle with `cp -c` under a different bundle id. Pass
   `--env HOME=/private/tmp/gpe-bench` on every `open`.
2. Quit the installed app and every other WebKit host app. Size the window to
   1280×850, and record the display refresh rate.
3. To find the test app's WebContent pid, run
   `pgrep -f com.apple.WebKit.WebContent` before and after launching it. The
   new pid is the one.
4. Open one terminal session and keep it visible. Use Settings › GPU terminal
   rendering to pick the arm. A session marked `lost` is a failed run: close it
   and open a fresh one.
5. Run seven rounds of each mode, alternating which arm goes first (A B, B A,
   …):

   ```
   node scripts/tui-redraw-bench.mjs --mode redraw --arm <dom|webgl> --transparency <decided> \
        --webcontent-pid <pid> --out tmp/tui-bench.jsonl
   ```

   Then `--mode flood` and `--mode idle` the same way.
6. Run `node scripts/tui-redraw-bench.mjs --summarize tmp/tui-bench.jsonl`.
   Add the figures and the verdict to this section. Ship the branch only on
   exit 0.

**The win rule** (owner decision, 2026-10-12, set by Fable):

* WebContent main-thread CPU-seconds in `redraw` must be at least 2× lower
  with WebGL.
* None of these may be worse:
  * keystroke-echo p95 (here, the DSR proxy);
  * dropped-frame fraction;
  * flood throughput;
  * 60 s idle CPU, which must stay within 1 pp of a core.
* Any miss means no dependency.

| row | DOM | WebGL | verdict |
| --- | --- | --- | --- |
| all | not yet run | not yet run | undecided |

## 2026-10-15: WebContent WebKit Malloc, continued soak of the pre-wave install (ft-bc59566cd39b)

The 2026-10-13 baseline (WebKit Malloc 290–291 MB dirty, phys_footprint 486–547 MB, peak 941 MB) was this same installed process at 13.5 h. The captures below are that process still running. They are not a post-wave build, and they are not a fixed workload taken an hour after a reinstall. Quitting the process, or replacing `/Applications/GitPulse.app`, would end the live session, so that sample was not taken. There is no "after" number for a rebuilt app.

### Workload and host

Process start, confirmed still alive while the figures below were read: Mon Oct 12 22:53:27 2026.

* App pid 53224, `/Applications/GitPulse.app/Contents/MacOS/gitpulse`. This binary predates the lag-wave merges. `GRAPH_CACHE_LIMIT` (6) and the scrollback defaults are in this source tree and are not in that binary.
* WebContent pid 53227. GPU pid 53226. WebContent is not a child of the app; `ps` does not attribute it.
* The session is the user's real one. Repository count and terminal-tab count were not controlled, and the app's direct child processes are not a tab count, so this note states neither N nor M.
* Host memory is 64 GB (`68719476736` bytes). Load while the captures were parsed, with those three processes still up: `{ 61.66 41.30 35.41 }`. `memory_pressure` then: pages free 63753, swapins 554850, swapouts 1028827 (page size 16384). The footprint files do not record load. The machine was not idle.

### `footprint -v` (page size 16384)

Two captures of pid 53227, minutes apart, printed by `node scripts/memory-footprint.mjs --footprint`. Category rows other than WebKit Malloc are from the same summary block in each file.

| | first | second |
| --- | --- | --- |
| phys_footprint | 1094 MB | 1103 MB |
| phys_footprint_peak | 1905 MB | 1905 MB |
| WebKit Malloc dirty | 632 MB (140 MB reclaimable, 201 regions) | 669 MB (10 MB reclaimable, 209 regions) |
| Untagged dirty | 257 MB | 266 MB |
| Owned physical footprint (unmapped) (graphics) dirty | 148 MB (174 MB reclaimable) | 109 MB (32 MB reclaimable) |
| JS JIT Generated Code dirty | 34 MB | 35 MB |
| Malloc Small dirty | 12 MB | 12 MB |

Graphics belongs to ft-cc4be67b and is not part of WebKit Malloc. Untagged is not folded into it either. RSS of pid 53227 at the later reading was 777152 KB; RSS is not phys_footprint.

The other two processes at the same time, so they are not mistaken for the WebContent heap:

* App pid 53224: phys_footprint 268 MB, peak 1165 MB, WebKit Malloc dirty 5.19 MB (6 regions).
* GPU pid 53226: phys_footprint 170 MB, peak 510 MB, WebKit Malloc dirty 1.66 MB (8 regions).

Against the 2026-10-13 baseline, this pre-wave WebContent grew. WebKit Malloc dirty went from 290–291 MB to 632 MB and then 669 MB. phys_footprint went from 486–547 MB to 1094 MB and then 1103 MB. Peak went from 941 MB to 1905 MB. Between the two captures, reclaimable WebKit Malloc fell from 140 MB to 10 MB while dirty rose, so the second capture is not a smaller heap.

### What was not attributed

No Web Inspector heap snapshot of this WebContent was taken. The running release view was not started inspectable, and a second GitPulse or `tauri dev` cannot sit beside the single-instance app. A Node heap is not WebKit Malloc. The top five retainers of the 669 MB are not named here because they were not measured.

The class costs below are V8, from `node --expose-gc scripts/memory-footprint.mjs` on this run. The terminal figure does not call `term.open()`, so it has no DOM.

* Graph, a first page of 5000 rows: scholarlmConfluence retains 2.85 MB (597 bytes a row, from 87 rows); devprismMainline retains 3.08 MB (645 bytes a row, from 240). Six such pages are about 18 MB, which is what `GRAPH_CACHE_LIMIT` is for. That cap is not in the sampled binary.
* One xterm, scrollback 5000, 200 columns: 5050 rows hold 11.56 MB of cells; heap +2.38 MB, ArrayBuffers +12.12 MB. Scrollback 1000: 2.40 MB of cells, heap +0.55 MB, ArrayBuffers +2.91 MB. Scrollback 5000 at 120 columns: 6.92 MB of cells.
* The DOM renderer keeps a row element per viewport row (`DomRenderer._refreshRowElements`), not per scrollback row. Cell bytes and WebKit Malloc were not measured in the same process, so this note does not assign the 669 MB to terminals or the Untagged 266 MB to cell buffers.

### Bound

What still grows with time, and with how many repository tabs have hosted a dock: each strip keeps up to `MAX_STRIP_TABS` (128) tabs, ended ones included, and each mounted tab holds an xterm. Repository tabs have no ceiling. Live PTY sessions are already capped (`DEFAULT_TERMINAL_SESSIONS` 32, `MAX_TERMINAL_SESSIONS` 128). One buffer plateaus at its scrollback. The accumulation is finished tabs left mounted.

`shouldReleaseEndedBuffer` is true only for status `exited` while off screen. `releaseEndedScrollback` sets scrollback to 0, which is when xterm drops the rows. The last screen stays in the viewport. A plain-text tail of `ENDED_TAIL_CHARS` (32768), at most `MAX_ENDED_TAILS` (32) process-wide, is written back only when that viewport is empty, so it is not appended onto the screen the trim kept. The replay has no color. A live session, hidden or not, is not trimmed. The component stays mounted: unmounting calls `leave()`, which detaches or closes a process that may still be running. Ended tabs past the tail cap, and every ended tab's rows above the last screen, are dropped on purpose.

`endedBuffer.test.ts` fills a 200-column xterm past a 5000-row scrollback, requires more than 8 MB of cells, and requires under 200 KB after the release, including after the option is raised to 5000 again. Raising the option does not allocate the rows back until new output, and an ended session produces none. `TerminalSession.test.ts` requires that call and the empty-viewport guard.

Shared reads (`src/lib/ipc/sharedReads.ts`, the map in `invoke.ts`) join an in-flight read. They are not a retained result cache. This pass did not re-audit the oid-keyed churn cache that `cmd_branch_stats` fills.

### Footprint parser

`node scripts/memory-footprint.mjs --footprint <file>` prints phys_footprint, peak, and WebKit Malloc dirty, reclaimable, and regions. It does not need `--expose-gc` and it does not run the microbench. A category the file does not contain is reported absent, not as zero. Covered by `scripts/footprint-summary.test.ts`.

## 2026-10-24: notification worker msgs/sec (ft-9cfd547403d57c1725e3ccdb97144170, S1-08; row in ft-c0d9313f)

`alerts_worker_throughput` (N = 9, release, 18 cpus): 50,000 notices per sample over 64 sessions through `alerts::try_offer`, the 64-slot queue and the real worker, a full queue retried. Measured as an ignored test (`cargo test --release --manifest-path src-tauri/Cargo.toml --test alerts_worker_throughput -- --ignored --nocapture`), because `Cargo.toml` was held. It is now a `[[bench]]` on the shared harness (`cargo bench --manifest-path src-tauri/Cargo.toml --bench alerts_worker_throughput`) and is in `benches.yml`'s `BENCHES`; `ubuntu-latest` and `macos-latest` have no committed baseline at all (so the scheduled compare step notices that and compares nothing, per `4797eb40`), and `local-macos-aarch64` does not carry this bench (a by-hand compare against it notes "ran but has no baseline" and does not fail). Its first baseline is the `bench-<runner>` artifact of a scheduled or manual `benches.yml` run, committed with `scripts/bench-compare.mjs --write-baseline`.

| row | min | p50 | notices/s at min |
| --- | --- | --- | --- |
| hook state reports (`turn_finished` / `tool_finished` alternating) | 111.29 ms | 112.68 ms | **449,280** |
| renderer answers (`prompt_submitted`) | 91.72 ms | 92.59 ms | **545,151** |

The rate to compare between builds is `50000 / min`. The producer retries a full queue (millions of retries across the samples; it outruns the worker), so the figure is the worker's own rate, less the last 64 notices still queued when the span ends (under 0.2%). One machine's reading; there is no row from before the intake change, because the test calls `try_offer`, which that change introduced.
