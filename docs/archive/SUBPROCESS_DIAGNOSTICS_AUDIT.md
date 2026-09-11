# Subprocess and diagnostics hardening audit

Date: 2026-09-08. Base: `a4a1d33d93d353ba03be52658e028d6182b62548`.
Worktree: `/Users/bharath/.codex/worktrees/4aa5/GitPulse`.

## Scope and evidence

The supplied GitPulse 0.0.8 report contains repeated output-reader channel
timeouts for diff and file enumeration, one missing-HEAD blame path, and an
empty durable-log section. This audit covers the finite subprocess lifecycle,
its parsing consumers, native blame, and diagnostic completeness. It is not a
claim that every subsystem or every possible operating-system failure is proven
correct.

**Verified:** regressions reproduced lost output, unbounded admission and input
waits, renewed retry budgets, incomplete or failed output accepted by parsers,
and an unbounded installer drain. **Inferred:** these defects can explain the
reported timeout family. The historical report lacks enough command/repository
context to reproduce each individual incident. **Unverified:** why that running
app produced an empty durable log. Current logging initialization, sink errors,
rotation and unavailable-state reporting were inspected and remain covered by
the existing tests; no logging defect was reproduced in this audit.

The main checkout contains extensive concurrent changes. It was inspected for
overlap but not edited. Its pending native-blame patch was reused after its
regressions failed against this worktree's original implementation.

## Contracts and fixes

| Contract | Reproduced failure | Canonical fix |
| --- | --- | --- |
| Keep every captured prefix and label missing EOF | Retained descendants yielded empty channel results | Unix waiter owns nonblocking descriptors; fallback workers share bounded captured data |
| Reading can recover from interruption | Interrupted read returned empty output | Retry `Interrupted`; bound each Unix drain pass |
| Queueing consumes the command deadline | A 50 ms request executed after a 300 ms admission delay | Deadline-aware, cancellable admission; expired requests never spawn |
| Input cannot suspend the waiter | Parent exit left stdin join blocked for about three seconds | Unix nonblocking input; bounded fallback cancellation; incomplete delivery cannot accompany success |
| Retry budgets cannot renew | A 150 ms request ran for 630 ms | Command attempts and backoff consume one elapsed-time budget |
| Memory and diagnostics stay bounded | Caller could bypass the global cap; stderr cap was silent; failure text carried 10,000 bytes | Validate caps/input before spawn, label capped stderr, bound either failure stream to 2,000 bytes plus marker |
| Cancellation includes output collection | Installer waited four seconds for inherited pipes | Installer delegates to the shared runner; remove its separate reader threads and blocking receives |
| Parsers only accept complete evidence | Valid JSON prefix accepted without EOF; generic capture lost stderr completeness | Typed completeness for both streams; one `require_complete` boundary for machine-output callers |
| Verification requires a successful command | Failed version command passed the schema handshake | Require exit success and complete output for probes and handshakes |
| Numeric and digest evidence is validated | Schema 4,294,967,315 became 19; non-hex 64-character digest accepted | Checked integer conversion and the existing checksum parser |
| Cleanup helpers are bounded | A two-second helper ignored a 100 ms budget | Deadline, kill and reap around Windows `taskkill` |
| New-file blame is explicit and bounded | Untracked and unborn-repository text failed with missing HEAD path | Git status classification, bounded regular-file read, explicit uncommitted identity, object-format-aware zero OID |
| Blame cannot silently return a prefix | Existing API returned partial records with no completeness field | Refuse incomplete/capped blame; preserve committed attribution; the new-file fallback refuses corrupt HEAD, binary data and invalid text |

Command budgets must be positive and at most 30 minutes; stdout and input may
not exceed 64 MiB; stderr retains at most 4 MiB plus short diagnostic markers.
Unix reads/writes at most sixteen 16 KiB chunks per stream per pass. The default
admission limit remains twice the CPU count, clamped to 4–16.

The command budget excludes a separate, shared two-second EOF cleanup grace.
Windows additionally allows a bounded 50 ms I/O-cancellation settle period.
Cancellation cannot always force a platform worker to finish: in that case its
permit remains held until it exits, bounding retained threads, input and pipe
descriptors. An unfinished worker cannot be reported as a complete read.

Progress callbacks receive the captured prefix, up to each stream's cap, and
must return promptly. UI progress is best effort; it does not certify complete
output. Installation success still depends on its existing post-install checks.

Security impact: incomplete or malformed verification evidence is rejected.
No access policy, authorization, dependency, credential or distribution trust
setting was broadened.

## Verification

Red regressions were recorded before their corresponding production fixes.
Logs are retained under `/tmp/gitpulse-4aa5-*` for this session. Important red
logs include `audit-before`, `retry-before`, `boundaries-before`,
`final-boundaries-before`, `stderr-before`, `tree-killer-before`, and
`blame-before`.

Final results, verified against the local source:

| Check | Result |
| --- | --- |
| Native workspace | 1,956 regular tests passed: 1,456 library tests and 500 other native tests |
| Normally ignored native checks | All eight passed separately: real-repository Pulse, five graph/status smoke checks, live update transport and deep graph fuzz |
| Repeated subprocess stress | Five rounds at eight test threads, 100 tests per round: 500 passes |
| Deep graph fuzz | 6,800 seeds, including 800 graphs of 600 nodes, passed |
| Frontend coverage | 372 files, 4,828 tests passed; one optional documentation fixture skipped |
| Frontend coverage floors | Lines 95.84% (8,309/8,670); branches 88.46% (7,118/8,047) |
| Rust coverage floor | Lines 84.18% (49,933/59,315), above the 80% floor |
| Remaining project checks | Contract checks, Svelte/type checks, frontend production build, formatting, Clippy with warnings denied, and diff whitespace checks passed |

The full workspace run passed every integration target. Its final outstanding
failure was an EOF fixture in the library suite; after repairing that fixture,
the entire library suite passed under coverage. Coverage was regenerated from
the combined workspace data. This reports the verified component results, not
an uninterrupted successful run of the original `npm run ci:local` command.
That command initially stopped at an unused import, subsequently corrected.

Evidence logs: `native-coverage-verified.log`, `library-final-pass.log`,
`coverage-floor.log`, `stress-delivery-1.log` through `stress-delivery-5.log`,
`clippy-delivery.log`, `real-repo-smoke.log`, `live-update.log`, `deep-fuzz.log`,
and `full-ci.log`, all with the `/tmp/gitpulse-4aa5-` prefix. Documentation
contract tests were rerun after the documentation edits: 23 passed and one
optional fixture skipped.

The real-checkout Pulse report returned 300 commits with no truncation flags.
The knowledge reader deliberately sampled **128 of 1,097 candidate files**
(23,842 lines) and reported `truncated=true`; this is not complete file coverage.

Stress exposed two test-fixture assumptions, neither hidden by loosening a
timeout: a concurrent fork can temporarily inherit an unrelated pipe before
`exec`, and acquiring every global permit one at a time can starve unrelated
tests. Closure and interrupted-read fixtures now wait for actual EOF and verify
ownership and kernel `BrokenPipe`. The expired-deadline fixture runs in an
isolated test process to establish that all writer copies are already closed.
The admission test exercises the same production runner through an isolated
gate. Older orphan-blame and oversized-output fixtures now assert explicit
uncommitted attribution and the stdin boundary while retaining their original
attribution and output-completeness checks.

The production runner core, pipe modules and process-guard module type-checked
in an isolated harness for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`. The harness uses the
project's existing locked libc/log versions. This is compilation evidence, not
a Windows or Linux runtime test or a full cross-platform Tauri build.

Primary code owners: `src-tauri/src/engine/git_cli.rs:1462` (lifecycle),
`src-tauri/src/engine/git_cli.rs:1747` (retries),
`src-tauri/src/engine/git_reader.rs:951` (blame),
`src-tauri/src/tool_install/mod.rs:774` (installer adapter),
`src-tauri/src/tool_install/release.rs:228` (digest boundary), and
`src-tauri/src/procguard/mod.rs:773` (tree-kill deadline).

The Windows cancellation contract was checked against Microsoft's
[`CancelSynchronousIo` documentation](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelsynchronousio).
Cancellation is a request; the worker must finish before joining or releasing
its resource permit.

Rust footprint, including tests and both new pipe modules: **1,911 lines added,
306 removed**. The separate installer runner and blocking handoffs were removed;
no project dependency was added.

## Remaining verification limits

- Native execution was performed on Apple Silicon macOS. Windows I/O
  cancellation, process-tree termination and Linux runtime behavior still need
  the repository's native CI matrix. The blocking worker implementation is also
  exercised on macOS with real concurrent stdin/stdout/stderr.
- No packaged app was installed or launched. No release, signing, notarization,
  publishing, or live installed-app recovery is claimed.
- Kernel process creation, termination and reap syscalls are outside userspace
  scheduling guarantees. The implementation does not claim a hard realtime
  bound if the operating system itself stops making progress.
- Existing Svelte canvas accessibility and Vite chunk-size warnings remain
  outside this subprocess change. No frontend behavior was edited.

GitNexus initially rated the shared runner/result changes critical; the final
comparison also reports critical reach through Git operations and tool flows.
Its comparison omits untracked files, so the two new pipe modules are reviewed
and tested separately rather than counted as absent changes.
