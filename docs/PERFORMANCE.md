# Performance diagnostics

Open **Diagnostics** from the header bug icon or command palette and copy its
report after reproducing a slow action. Keep the time, repository size, open
view, and action with the report; they help correlate UI observations with
native commands.

- `performance:ui`: the visible UI timer ran at least 250 ms late. The report
  records delayed samples and maximum lateness. It does not identify the
  cause or measure FPS. Hidden windows are not sampled; very long gaps are
  labelled as potentially including system sleep or suspension.
- Backend `[performance]`: a command through the shared blocking wrapper took
  at least one second. `queue_ms` measures waiting for a blocking worker;
  `work_ms` includes everything inside that command, including subprocess and
  internal lock waits. `outcome` describes the latest completed call.
- `slow_calls_since_report`, `max_queue_ms`, and `max_work_ms` summarize slow
  calls accumulated since the previous report for that operation. Each
  operation reports at most once per 30 seconds, when another slow call
  finishes. Maxima can come from different calls. Pending aggregate counts
  are not flushed at shutdown, so these are bounded observations, not an
  exhaustive trace. A stuck command has no completion record.
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

Still open in the overall audit: finer-grained stutter recording and unfinished
operation diagnostics, path-aware watcher invalidation, and measured native
macOS rendering/idle-power behavior with the updated build. Scope deferral
does not yet govern repository-state or subscribed metric refreshes. Physical
Windows/Linux execution and native Mac sleep/wake/display testing remain
separate from these deterministic queue and filesystem tests.

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

The semaphore saturation test initially used the global gate and interfered
with unrelated concurrent tests. It now drives the same production runner
with a private gate; the deadline assertion remains unchanged.

Platform limits: native execution was tested on macOS. Windows/Linux runtime
validation remains a CI/physical gate. Unix stdin cancellation is implemented;
a blocked non-Unix writer can outlive the caller's bounded settle window until
the pipe closes. Filesystem input budgets cannot guarantee a response deadline
from a stalled network filesystem. Source byte caps do not bound all parsed
index memory or the lifetime of snapshots retained by active readers.


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
