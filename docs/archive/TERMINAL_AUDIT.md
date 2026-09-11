# Terminal audit and hardening

Audit date: 2026-09-07. Scope: the terminal dock, tabs, xterm transport, native PTY
ownership, output flow control, and Console retention. This is a source and test
audit of a dirty shared checkout, not a release certification. Unrelated concurrent
changes were preserved. No app installation, commit, release, or deployment was
performed by this pass.

At the final source review, the terminal modules, three terminal components and
their tests, including the new native stress test, differed from HEAD by 2,294
added lines and 314 removed lines (11 new files). This includes the preceding
usability pass and excludes shared startup/IPC glue, preferences, dependency
metadata, browser fixtures and documentation; it is not the whole-checkout diff.

## Verified failures and fixes

| Failure or gap | Canonical fix | Regression evidence |
| --- | --- | --- |
| First output could arrive before the first listener pair was ready | `ptyBus.prepare()` precedes spawn; temporary listener leases cover adoption | A failing listener-order test now passes; real PTY immediate Unicode output and exit 37 survive |
| Partial listener failure leaked an attachment or produced an unhandled rejection | Paired attachment unwinds partial success; five-second timeout also disposes late listeners | Failure, timeout, late-success, and malformed-event tests |
| Early output eviction was silent | Bounded replay records and reports incomplete output | Eviction disclosure test failed before the fix |
| Large paste could exceed native write limits or interleave input | Single-flight input queue, 16 KiB chunks, 1 MiB atomic admission, no uncertain-write retries | 300 KB Unicode/bracketed paste, 10,000 queued keystrokes, oversize refusal, timeout and disposal tests |
| Legacy mouse bytes could be UTF-8 re-encoded | xterm `onBinary` uses an ordered binary stream and validated native byte conversion | Frontend ordering test and real PTY exact bytes `1b 5b 4d 80 ff` |
| Isolated UTF-16 surrogates could reach Rust's Unicode-only JSON boundary | Reject malformed Unicode before queuing any bytes | Red test admitted the malformed paste; green test refuses it atomically and still accepts valid surrogate pairs |
| Repeated start/restart and late spawn could create duplicate or orphan processes | One lifecycle owner serializes operations and retains unresolved capacity | 100 concurrent starts/restarts; late adoption, listener-pending disposal, 15-second spawn watchdog and late reclamation |
| Close failures could appear to free a still-owned session | Keep id and capacity with a retry in the global registry | Failed-close and five-second timeout tests; no replacement spawn until close succeeds |
| Old queued renderer work could cross a restart | Drain the old parser before resetting and spawning | Regression originally observed the replacement before the drain resolved |
| Resize storms could flood native IPC | Coalesce to one in-flight resize plus the latest dimensions | 10,000 resize requests deliver the final 1000 by 1000 bound |
| A blocked PTY write held the registry lock | Clone the session writer under the registry lock; write outside it and off the async executor | Real blocked writer while another session spawns, resizes, and closes |
| Output producers could outrun the webview | A 256 KiB acknowledgement window; xterm acknowledges only after parsing | Real 1 MiB pattern flood is checked byte for byte; one million flow-control cycles |
| Closing at full capacity could immediately hit a false limit | Reaping releases the native reservation before close succeeds | Red 16-session replacement test reported `Terminal session limit reached (16)`; all 16 replacements now pass |
| EOF could precede child exit and make Close unable to reap it | Keep ownership through reaping; poll `try_wait` without holding a blocking child wait lock | Real child closes stdio but remains alive, then Close reaps it |
| A full tty output queue could stall cleanup after termination | Stop delivery and drain kernel output until EOF | Real full-window close regression now completes |
| Find could preserve stale case-sensitive result counts | Clear addon decorations when the query/options identity changes | Browser case-toggle, Unicode, wrapped text, next/previous, no-match and empty-query checks |
| ResizeObserver could trigger a browser resize loop | Schedule one fit per animation frame and cancel it on teardown | Observed browser loop error disappeared; runtime error list is empty |
| A short stacked split with Find could collapse its terminal to zero height | Minimum pane size and scrollable short layouts | Saved browser regression failed at 200px; both grids now remain readable without overlap |
| Titles, history, and console results could accumulate unbounded data | Bounded custom/program titles, 100-command history and 100-result / 8 MiB retention | Oversized title, history, UTF-8 byte-budget and scroll-following tests |
| Preferences and global capacity were hard to understand | Validated saved launcher/font size; repository-wide session manager; visible unread/ended/error state | Preference reload/corruption tests, 100 global start attempts accept exactly 16, browser manager spans two repositories |

## Ownership and resource contracts

- `TerminalDock` preserves hosted repository panels while hidden. Keyed tab
  components preserve xterm and PTY identity through navigation and reordering.
- `sessionLifecycle.ts` owns each native process from reservation through confirmed
  close, including when its view has been unmounted. A failed close remains in
  `sessionRegistry.ts`; a timeout does not imply native cancellation or success.
- The event bus keeps one listener pair. Unclaimed output is limited to 32 ids and
  128 chunks per id; chunks larger than 8 KiB of base64 are rejected. Buffer
  eviction within a session is disclosed. These are explicit bounds, not a claim
  that unlimited early output can be retained.
- Native input remains capped at 64 KiB per write, dimensions at 1–1000, sessions
  at 16. PTY program, argv and environment inputs are checked for malformed and
  oversized values before allocating a session. Multiline user-authored PTY
  scripts remain valid.
- Native output uses 4 KiB reads, a 256 KiB unacknowledged window, and a 30-second
  acknowledgement deadline. Close wakes blocked flow-control waits. Acknowledgements
  cannot release zero bytes or more bytes than are outstanding.
- Unix close signals the still-owned child and its verified foreground tty process
  group, then waits up to two seconds for registry removal. It does not enumerate
  arbitrary descendants or claim to terminate intentionally detached jobs. Windows
  uses portable-pty's child termination implementation.
- Normal application exit invokes PTY cleanup before the runtime disappears and
  logs incomplete cleanup. Abrupt host/process termination is not covered by a
  graceful-exit callback. Unresolved starts and failed closes remain explicit
  failure states; the UI does not fabricate cleanup success.
- Shell input remains user-controlled. Console and MANVI action authorization
  continue through their existing owners. This pass does not widen authorization,
  change the MANVI gate, or treat an interactive shell as policy-checked.

## Reproducible verification

Use the repository's package scripts. The focused frontend command is:

```sh
npm test -- src/lib/terminal src/lib/components/TerminalSession.test.ts src/lib/components/TerminalPanel.test.ts src/lib/stores/__tests__/interfaceStore.test.ts scripts/check-ipc-contract.test.ts scripts/documented-counts-contract.test.ts
```

The native tests execute real local shells in temporary Git repositories. Tauri's
MockRuntime supplies only the event sink; it does not mock PTY spawn/write/resize/
kill. The stress integration suite is Unix-only.

```sh
CARGO_BUILD_JOBS=2 cargo test --manifest-path src-tauri/Cargo.toml --lib terminal::
CARGO_BUILD_JOBS=2 cargo test --manifest-path src-tauri/Cargo.toml --test terminal_integration --test terminal_pty_stress
CARGO_BUILD_JOBS=2 cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run check
npm run check:ipc
npm run check:types
npm run build
npm run coverage
```

The browser fixture at `/harness/terminal.html` mounts the real Svelte components
and xterm using Tauri's official mock IPC. Its interaction checks cover two
repositories, full capacity, keyboard focus, preservation, Find, theme, font bounds,
tab rename/reorder, split panes, and minimum dock height. The separate input stress
checks verify exact 300,000-byte delivery, 16 KiB writes, atomic oversize rejection,
and suppression of repeated close shortcuts. Mocked exports do not write files.

## Verification limits

| Completed check | Observed result |
| --- | --- |
| Focused frontend command above | 227 tests passed in 16 files |
| Rust terminal unit tests | 67 passed |
| Real PTY stress integration | 8 passed |
| One-shot terminal integration | 10 passed |
| Rendered browser interaction / input stress | 36 + 4 passed; zero runtime errors; exact 300,000-byte paste in 19 writes |
| Svelte / TypeScript | Passed; zero Svelte errors or warnings |
| IPC registry contract | 187 handlers, zero missing/orphaned commands |
| Wire type contract | 49 contracts, 119 structs, 859 fields, zero drift |
| Rust Clippy, all targets, warnings denied | Passed |
| Scoped Rust formatting and diff whitespace | Passed |
| Production frontend build | Passed |

Verified locally on macOS: focused frontend regressions, terminal Rust unit tests,
real Unix PTY stress and one-shot integration tests, rendered browser interactions,
Clippy, Svelte/TypeScript, IPC/type contracts, and the production frontend build.
The full coverage run reached 5,032 passing tests with four failures in concurrently
edited Work projection and Conflict editor code; that run is not a green global
gate and does not establish a passing coverage floor.

The four observed global failures were `does not require a working-tree scan for
bare repositories` (`expected 1 to be +0`), two Conflict editor source contracts
(`reloads only when the conflicted file or repo actually changes` and `routes a
landed parse through adoptResolutions instead of assigning raw`), and the CSS
token contract reporting `--font-mono` and `--side-color` in ConflictEditor. Those
files were being edited concurrently and were not changed to make this pass green.

Unverified: a freshly built native webview end-to-end session, the physical native
save dialog and filesystem export, physical IME and full-screen TUI workflows,
Windows/Linux builds and native process behavior, release signing/notarization,
and release CI. Browser checks use mocked IPC; native PTY checks use mocked event
delivery. Together they test both sides but do not replace a physical integrated
desktop run. No claim of absolute confidence or universal platform proof follows
from these tests.

The complete `ci:local` pipeline, workspace-wide Rust coverage, and release checks
were not run for this terminal change. Focused real-process verification and an
all-targets Clippy build were used for native changes; the failing global frontend
coverage run and concurrent edits prevent a clean whole-application certification.

GitNexus's per-symbol checks were low or medium risk for indexed terminal paths;
the index could not resolve the Svelte handlers or new modules. Direct source and
rendered behavior were reviewed for those paths. Whole-checkout `detect-changes`
reported critical risk across 83 files, 252 indexed symbols and 38 processes,
including unrelated concurrent changes. That result must not be represented as
a clean terminal-only change set or as authorization to commit the shared tree.
