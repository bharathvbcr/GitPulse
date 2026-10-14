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
`workflow_dispatch` seeds them, the scheduled job fails loudly on those classes.

### What each bench measures, and the audit card it is the baseline for

| bench | rows | baseline for |
| --- | --- | --- |
| `mcp_protocol` | protocol layer, validation, rejected call, `gitpulse_status` / `collision_risk` / `insights` on a generated 400-file, 3-worktree repository | G-tests-perf-03; C-mcp-07 (protocol overhead only; the codeintel deadline overhead needs a DevMap store and is not measured) |
| `graph_diff` | `compute_word_diff` at 100 / 10k / 60k chars; `LaneSolver::solve` linear and merge-heavy at 10k and 100k rows | G-tests-perf-05 |
| `watcher` | `record_event_signal` over a 10k-event burst, `IgnoreRules` populated with 600 prefixes, events/s | G-tests-perf-05 (through `watcher::bench_seam`, below) |
| `repo_read` | `get_status` at 1k and 10k files, clean and dirty, with git processes | A-git-05; git-status-assist-spike |
| `command_timings` | insights snapshot, `list_worktrees listing`, `list_worktrees full`, language stats, docs refresh, `catch_up` (steady and cold), commit details on an 18-worktree fixture | the lag audit's snapshot / listing / stats / docs / catch-up / commit-details cards; `list_worktrees listing` (`list_worktrees_scanned` at `ScanDepth::Listing`, the `cmd_list_worktrees` default) is the baseline for ft-28c58695 (card 4), and `list_worktrees full` (`ScanDepth::Full`, the Worktrees panel's explicit opt-in) is the baseline for the Worktrees panel path |
| `terminal_pty` | MB/s through a real PTY, IPC messages per MiB, keystroke echo p50 idle and during a flood in another session | terminal-output-channel; terminal-reader-kqueue |
| `idle_wakeups` | process interrupt wakeups/s with 0, 1, 8, 32 idle terminal sessions | terminal-reader-kqueue; always-on-pollers |
| `live_index` | duty cycle of the live index under a 40-tick, 250 ms-cadence edit storm against a stub `devmap` | live-index-duty-cycle |
| `worktree_teardown` | `remove_worktree` (lock-held) and `merge_and_teardown_worktree` on a worktree with 100k ignored files, and the merge with 0 entries as the control; `contender_wait_ns` of a `lock_worktree` issued mid-teardown | CARD 22 S5-11 (ft-9d04447403db7f9525dce4db970e4b8a) |

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
  - `tools/list`: min 341.3 us, 55.6 KB.
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
(55.6 KB), `server/discover` 3.3 us, `resources/read server/manifest` 605.8 us
(120.7 KB), validation 0.4 to 0.8 us; `gitpulse_status` min 69.3 ms,
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
rule (more than about 50 ms under the lock at 100k) calls for the rename-aside
split.

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
