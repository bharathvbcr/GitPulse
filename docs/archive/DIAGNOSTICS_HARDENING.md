# Diagnostics and runtime hardening audit

## Native crashes on 2026-09-09

Starting revision: `d2d8713`. The installed GitPulse 0.1.0 crashed at
15:13:19 CDT on macOS 27.0 build `26A5425a`.

- **Verified:** `gitpulse-2026-09-09-151321.ips` records `SIGABRT`,
  `panic_cannot_unwind`, and `tao::platform_impl::platform::app::send_event`
  on the main thread. The durable log records the same boundary panic.
  The macOS unified log records `NSCampoLightweightUIController.m:1429`
  asserting after “Mouse entered” immediately before the abort.
- **Inferred:** that AppKit assertion raised an Objective-C exception which
  hit TAO's non-unwinding `sendEvent:` callback. Native subprocess tests
  independently reproduce that abort mechanism through both actual TAO
  overrides. The private assertion's specific UI preconditions remain unverified.
- **Verified:** contemporaneous MCP crash reports and the durable log show
  `RingLogger::write_entry` panicking on `failed printing to stderr: Broken
  pipe (os error 32)`, then aborting when the panic hook writes there again.

The application and window forwarding callbacks now use `extern "C-unwind"`
in both their definitions and Objective-C registrations. Exceptions propagate
to the native catcher; they are not swallowed or retried. The Command-key
release and window-drag dispatch branches are retained. See
[Rust's FFI guidance](https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-unwinding)
and [objc2's method implementation type](https://docs.rs/objc2/0.6.4/objc2/runtime/type.Imp.html).

## Expanded adversarial audit

The audit followed the shared failure paths through native dispatch, logging,
panic recovery, durable files, hooks, MCP, daemon output, and preview shutdown.
It also ran the complete existing native and frontend suites and all seven
browser harnesses. This is a bounded qualification of those contracts, not
an assertion that every subsystem or every possible external failure is safe.

| Verified finding | Fix and regression evidence |
| --- | --- |
| Closed or nonblocking-full stderr could abort a native callback or cause a second panic during unwinding. | Fallible shared output, one disable notice in the ring/disk, and continued logging. Original OS-pipe subprocess tests aborted before the fix. |
| A blocking-full stderr pipe froze the logger even after write errors were handled. | `output::BoundedOutput` isolates host-owned writes with bounded admission and acknowledgement. Real full-pipe tests timed out before the change and now preserve normal operation and panic recovery. |
| The chained default panic hook printed the raw payload outside redaction. | The installed hook owns payload, location, and bounded-backtrace diagnostics end to end. Reinstating the original hook reproduced a leak using a recognized synthetic `access_token`. Panic propagation remains intact. |
| Native prose redaction had an incomplete name list despite the shared credential table already containing `token`. | Generate the three assignment regexes from the canonical names/suffixes. Table-driven tests failed on `token=value` before the fix and retain quoted/embedded, idempotence, and benign-neighbor checks. Header/PEM parsers retain their distinct grammars. |
| Log files followed symlinks and hardlinks and changed foreign bytes/permissions. FIFO targets blocked opening or reading. The directory itself could be a symlink. | Validate opened regular-file handles and link counts before I/O or permission changes; refuse final-component links and directory aliases. Unix opens are nonblocking. Child-process tests preserve foreign sentinels and exercise both log generations. |
| Failed rotation could truncate a substituted foreign target. | Disable disk writes with a recorded degradation instead of truncating. The original rotation body destroyed the test sentinel; the fixed body preserves it. |
| Independent loggers kept stale byte counts and file handles across shared rotations. | A stable sidecar lock coordinates cooperating processes; reopen and measure the current generation while holding it. A pre-fix test exceeded the active size bound between rotations. Eight simultaneous processes also verify 800 complete worker records without interleaving or omissions. |
| Diagnostic tails held the generation lock while decoding and redacting, starving simultaneous writers until their bounded lock wait expired. | Snapshot bounded bytes under the lock, then release it before decoding/redaction. A repeated eight-process test reproduced missing records and explicit lock-deadline failures; 30 repetitions after the fix preserved all 24,000 expected worker records. |
| A dense sub-megabyte legacy log decoded and redacted 100,001 lines before returning only the tail. | Cap line decoding/redaction at 500 per generation, preserving newest-first selection and final chronological ordering. The retained test failed on the original 100,001-line result and now verifies exact tail size and credential redaction. |
| Cleanup could label a completed run as interrupted when completion happened between reading history and probing the run lock. | Establish ownership before reading. Idle snapshots retain the run lease; busy snapshots may conservatively remain busy for one poll. The controlled interleaving test reproduced the false interrupted status before the fix. All 25 cleanup tests passed, followed by 100 forced interleavings and 480 actual cleanup runs. |
| A watcher test changed process-wide cwd while unrelated Git commands were being spawned, allowing a child clone to inherit a subsequently deleted temporary directory. | Run the existing cwd-sensitive assertions in a child with a 15-second deadline. The complete suite exposed `getcwd: cannot access parent directories` and a failed checkout; 30 repeated paired watcher/real-clone runs pass after isolation. Production Git behavior and watcher assertions remain intact. |
| Dropping a lock descriptor did not release the lock while forked/duplicated descriptors retained its open file description. | Use an explicit-unlock guard and surface release failures through the sink's degraded state. A retained duplicate reproduced the extended lock lifetime before the fix. |
| A 100 ms disk-lock admission budget permanently disabled a logger during healthy startup contention. | Allow one second for competing owners to finish, retaining bounded failure. A lock released after 250 ms failed the old regression; a permanently held lock still fails closed within the tested bound. This is a deliberate disk-admission policy change; the 100 ms stderr deadline is unchanged. |
| Hook input had no byte or wall-clock ceiling; missing/invalid input could panic when stderr was closed. | Bound complete JSON documents at 4 MiB and input waiting at five seconds. Reuse the existing hook budget helper with fallible thread creation. All diagnostic branches use the canonical logger; failures retain empty stdout and exit 0. Pre-fix real-process and oversized-document tests failed. |
| Daemon stdout could panic or stall on a closed/full pipe. | Use the shared bounded writer for help and reports; log failure and exit 1 without retries. Argument errors still exit 2 with closed stderr. Both old error paths exited 101 in the new regressions. |
| MCP stdout could stall request handling indefinitely; an asynchronous write failure could leave the main thread waiting on open stdin. | Bound wire writes and use a bounded input worker whose consumer observes output cancellation. The valid modern-request fixture asserts it takes the asynchronous path. The original input reader then timed out; the fixed path exits. Existing framing, EOF drain, concurrency and exactly-one-response tests remain applicable. |
| Preview cleanup raced late optimizer writes and failed with `ENOTEMPTY`. | Use Node's bounded removal retry support, retaining persistent errors. The original complete frontend run reproduced the failure; the lifecycle regression passes after the fix. |
| Documentation claimed 985 compared fields while the checker measured 989. | Correct the two tracked claims. The existing documented-counts contract failed before correction. |

Security impact: diagnostic output is more consistently redacted, and unsafe
log targets are refused. No authentication, authorization, repository mutation
policy, credential configuration, or access grant was widened. No dependency
was added. Hook failure continues to mean “no decision”; it never emits `allow`.

## Bounds and retained invariants

- The output worker owns no logger state and cannot recurse through logging.
  It accepts at most 64 queued records; records have an explicit byte cap.
  Stderr allows 32 KiB plus its newline and a 100 ms acknowledgement budget.
  Hook/daemon/MCP stdout allow 4 MiB per record and a one-second budget.
  An output failure or uncertain partial write permanently disables that
  writer. It spawns no replacement and retries no record. At most one blocked
  worker remains per inherited output until process exit.
- Ring and disk writes precede the optional stderr mirror. Panic-hook clones
  share its disabled state. No prior panic hook is chained; the owned hook
  records a redacted payload, location, and a labelled, capped backtrace.
- The in-memory ring retains 1,000 entries, diagnostic tails at most 500 lines,
  and individual log entries at most 32 KiB. Cooperating current writers use
  two generations of at most 1 MiB each and an empty lock sidecar. Lock
  acquisition waits at most one second, then reports degradation and disables disk
  writes. A tail that cannot acquire the lock reports itself unavailable. Raw
  snapshots are taken under the lock; decoding and redaction run after release,
  with at most 500 lines processed per generation. Explicit unlock prevents
  forked/duplicated descriptors from extending the lease; release failures are
  retained and refuse subsequent admission without recursive logging.
- Regular-file validation precedes reading, appending, or changing permissions.
  Unix checks descriptor link counts and uses `O_NOFOLLOW | O_NONBLOCK`.
  Windows opens reparse points themselves and checks handle metadata/link
  counts. Unix directory permissions are set through the opened directory.
- Hook input preserves multiline JSON, checks actual bytes beyond the cap,
  and rejects invalid documents without producing a decision. Its existing
  operation budget remains separate from the new input and output budgets.
- MCP input queues at most two 8 KiB chunks before the existing 4 MiB frame
  reader. Healthy idle connections remain open. Output failure cancels the
  consumer within its 100 ms polling interval. EOF still drains accepted work
  under the existing call deadline.

The shared output worker uses standard-library bounded channels and file
coordination uses [standard-library file locks](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
Preview removal uses [Node's documented retry options](https://nodejs.org/api/fs.html#fspromisesrmpath-options):
five retries with 100 ms linear backoff, at most 1.5 seconds of retry delay.

## Reproduction and qualification

```sh
GITPULSE_CODEINTEL_TEST_REPO="$PWD" cargo test --manifest-path src-tauri/Cargo.toml --locked --no-fail-fast
cargo test --manifest-path src-tauri/Cargo.toml --locked --test logging_process --test cli_io_process --test native_event_unwind
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
npm run check
npm test
npm run build
npm run test:browser
npm run test:webkit
```

Select other browser contracts with `npm run test:browser -- --harness NAME`:
`conflicts`, `uncommitted`, `coverage`, `branches`, `hygiene`, and `palette`.
Each harness rejects missing, incomplete, failed, or late verdicts.

The native exception regression uses two real Tauri-created Objective-C
receivers. Each now survives 256 injected exceptions interleaved with 256
normal events (1,024 total calls). The original ABI aborted both cases. The
probe restores superclass methods before disposing its isolated window. Other
platforms explicitly report the target as inapplicable. Inspection also found
that Wry replaces the original Tao content view; discarded probe experiments
against a presumed Tao view were fixture failures, not evidence of another
production exception defect. Other native callback families were not rewritten.

Before the final credential-table unification, the complete native run passed
**2,204 tests across 65 result-bearing targets, with 14 explicitly ignored**;
the library contributed 1,612 passed and six ignored. The separate native main-
thread harness passed both cases. The complete frontend run passed **5,626
tests in 427 files, one skipped**; Svelte/type checking and production build
also passed. Chrome browser counts were 29 diagnostics, 43 conflicts,
39 uncommitted, 44 coverage, 51 branches, 58 hygiene, and 44 palette: **308/308**.
Native WKWebView diagnostics additionally passed **29/29**.

The cleanup failure initially passed in isolation; subsequent parallel failures
and a controlled completion interleaving established the snapshot race above.
No completion assertion was weakened. Ten repeated fault rounds passed all
logging and CLI subprocess cases and both native cases, totaling 10,240 native
event forwards with 5,120 injected exceptions. Separate cleanup stress passed
100 forced completion interleavings and 20 repetitions of the 24-run real
worker/history test.

Ten opt-in tests were also qualified: the candidate DevMap CLI against disposable
indexes, deep graph fuzzing (6,800 generated DAGs), five real-repository graph
and status checks, one Pulse real-repository scan, one real Go cleanup against
an isolated temporary cache, and the repeated document-refresh workload. The
Pulse scan reported its knowledge sample honestly: 128 of 1,903 files. The
remaining four opt-in tests require live GitHub or explicitly selected installed
Manvi/Codex/Claude environments; they were not counted as passing.

A separate native run used the real-store test's default external DevCouncil
checkout while its index was empty (generation 1341, zero files/edges). Final
qualification uses the test's existing `GITPULSE_CODEINTEL_TEST_REPO` override
with this worktree's stable index; no external index was modified to make the
test pass.

IPC/wire-type, release-version, workflow lint, vendor-schema, Cargo formatting,
and all-target Clippy checks passed. Vendor checks confirm local snapshot
integrity while explicitly reporting external DevMap upstream drift and
unavailable framework upstream comparison. The two native patch hunks were
applied to the exact cached Tao 0.37.0 upstream files and reproduced the modified
files byte for byte. Existing vendored framework warnings remain.

The platform-neutral output module compiled to metadata for Windows, Linux,
and Intel macOS; Windows log-file validation also compiled independently.
These are type/conditional-compilation checks, not native runtime validation.
Evidence, including red tests and final transcripts, is retained locally under
`.build-evidence/crash-hardening-20260909/expanded/`.

## Current qualification limits

The installed application has not been replaced. This work does not reproduce
the private AppKit assertion's exact UI trigger, qualify every native callback,
or prove arbitrary AppKit state remains recoverable after every exception.
The fixed disk wait is an admission limit, not a promise of lossless logging
under arbitrary load; exceeding it remains an explicit degraded outcome.
Windows/Linux runtime, live external providers, signing/notarization, and
release installation still require their corresponding environments.

The file lock coordinates patched cooperating writers. Older binaries or
uncooperative writers do not honor it; it does not retroactively bound a
pre-existing oversized legacy file. Parent directories are trusted: final
component checks do not promise protection from a hostile same-user process
replacing ancestors concurrently. Regular-file operations remain synchronous,
so a stalled/network filesystem or hardware failure has no portable I/O deadline.
Rust treats invalid stderr handles as discarded successful writes on some
platforms; such cases retain ring/disk evidence without promising a mirror
failure notice. These limits are not counted as passing coverage.

## Earlier Code-pane audit

Audit date: 2026-09-08. Starting revision: `a4a1d33d93d353ba03be52658e028d6182b62548`.

The reported symptoms were a random Code-pane `each_key_duplicate` crash and
`fatal: no such path '.devcouncil/config.yaml' in HEAD` from Blame. This audit
traced Code rendering, keyed list identity, asynchronous request ownership,
native blame reads, and the diagnostics path. It also ran the repository's
broader verification gate. Other active tasks changed the same checkout during
verification; terminal, graph rendering, performance, and backend command
scheduling changes belong to those tasks.

## Findings and fixes

| Status | Finding | Fix and regression evidence |
| --- | --- | --- |
| Verified | The shared key allocator could emit a key already supplied literally: `a, a, a#1`. Per-base counters alone were insufficient. | Track emitted keys in the existing allocator. Five adversarial tests failed before the change, including 100,000 claims producing only 66,668 distinct keys. All rows now receive unique keys without dropping repeated data. |
| Verified | Seven keyed lists assumed identities that providers may repeat. Backlinks can legitimately share a path and line at different offsets; broken links can repeat a source/target pair. | Use the existing `keyedList` seam for Map search/broken links/workspace links, Markdown backlinks, operation warnings, CI steps, and storage recommendations. Seven tests evaluate the actual Svelte template expressions and failed against the original templates. |
| Verified | Blame cancelled its pending request after unrelated repository-store publications, then skipped replacement because the fingerprint was unchanged. Clearing a selection also retained the old filename. | Isolate effect tracking to a derived scalar fingerprint, retain stale-result guards, and clear pending/visible state on deselection. Real-browser tests exercise 50 unrelated publications and late completion after clearing. |
| Verified | Map requests used repository equality without sufficient request or subview identity. Old searches and graph/document loads could overwrite newer state in the same repository. | Give map, graph, documentation, search, links, and builds independent generations. Invalidate on repository/view/query changes and destruction; check freshness after every relevant await. Initial loads have one owner. Browser tests reverse search, graph-mode, and repeat-visit completion order and exercise 200 unrelated store publications. |
| Verified | A map status reload erased the build/refresh failure note immediately after assigning it. | Preserve the operation note through reload and record unsuccessful command outcomes through `code-map` diagnostics. A browser regression failed before this fix. |
| Verified | Native blame rejected files with no history, including untracked files and files on an unborn branch. | Try Git's native blame first. After failure, classify the exact path using Git's NUL-delimited status protocol. Only legitimate new files receive content-preserving uncommitted lines with a zero OID. Never stage a file or invent a committed author. Two initial native regression tests failed before the fix. |
| Verified | The blame API returned a capped prefix as an ordinary complete `Vec<BlameLine>`. | Return an explicit limit error because this API has no completeness field. The changed payload-budget assertion failed against the original implementation. The existing orphan-branch contract now asserts exact uncommitted content/OID instead of requiring an error for a readable new file. |
| Verified | Pane diagnostics captured too little evidence and were invoked from the fallback render snippet. | All eight application boundaries report through `onerror`. Capture pane, view, section, repository, selected file, stack head/tail, and recent navigation before deferring the diagnostics-store write. Tests verify snapshot timing, redaction, bounds, hostile getters, and the actual runtime crash canary. |
| Inferred | One of the duplicate-capable Code lists or the shared allocator could explain the reported random crash. | The supplied old report has no stack or component identity. It cannot prove which list triggered that particular event. New contextual reports are intended to resolve that uncertainty if it recurs. |

An AST inventory found 199 each blocks, 154 keyed, across 101 Svelte components
at the audit snapshot. This is an inventory, not a claim that every possible
producer payload was exhaustively tested. The seven changed template paths
have explicit adversarial fixtures; other Code paths were traced to their
existing identity owners.

## Preserved contracts and bounds

- Repeated provider rows remain visible; rendering does not deduplicate away
  warnings, findings, or backlinks. Existing display caps retain their counts:
  the browser fixture supplies 250 broken links and verifies 200 displayed.
- Stale responses cannot change the current Map view or Blame selection. UI
  cancellation invalidates results; it does not claim to abort native IPC.
  Existing native command deadlines still apply.
- Real Git blame retains committed attribution. New files use the repository's
  SHA-1 or SHA-256 zero-OID convention and the existing uncommitted display.
- Missing paths, corrupt history, directories, traversal, binary new files,
  invalid UTF-8 new files, and exceeded budgets remain visible errors.
- Working-tree reads retain repository path validation and the 8 MiB cap.
  New-file reads are descriptor-checked, capped during reading, and use
  `O_NOFOLLOW | O_NONBLOCK` on Unix. Serialized new-file blame is bounded by
  the 16 MiB blame budget, including metadata expansion from tiny source lines.
- Diagnostics use the existing local persisted ring and credential redaction.
  Each report fits the existing 2,000-character bound. Navigation retains eight
  bounded entries in memory and includes the latest two in a crash report.
  Stack truncation preserves both the runtime throw and application frames.
  Normal navigation adds no warning or error, and source file contents are not
  explicitly collected for these reports.
- Security impact: the new fallback reads only the already-authorized repository
  path and preserves containment checks. It does not widen permissions, change
  mutation authorization, or introduce a remote telemetry destination.

## Follow-up gaps and enhancements

The follow-up audit reproduced and addressed these additional issues:

| Status | Finding | Fix and evidence |
| --- | --- | --- |
| Verified | A second content edit can remain `M` with the same branch tip, leaving Blame's fingerprint unchanged. | The repository store publishes a separate content revision after current successful full refreshes, including byte-identical status snapshots. Global repository subscribers retain no-op suppression. Tests reject failed, superseded, and closed-session refreshes. |
| Verified | Repeated refreshes could restart slow Blame work or lose the loading state across A–B–clear–A selection changes. | One IPC pair runs at a time with one latest queued request. New selections invalidate old results, deselection clears the queue, and queued work remains visibly loading. The browser canary failed 23/24 before the loading-state correction. |
| Verified | A visible Map missed completed background index work; a refresh without a build outcome was labelled ready. | Successful index publications carry a monotonic revision, including completion while a follow-up is queued. Map subscribes to that revision; documentation views also react to full content refreshes. Missing outcomes are failures. |
| Verified | Runtime noise filtering hid genuine production module, WebView, and observer failures. | Retain these failures. Only identifiable development reload chatter is suppressed, and its session count is visible. |
| Verified | Failed local-storage reads/writes/clears were indistinguishable from healthy empty history. | Expose readiness, saved/memory-only status, redacted failure reasons, restoration completeness, and explicit save retry in the window and copied reports. Error storms attempt one failing automatic write; retry persists the full current ring. |
| Verified | Duplicate/exhausted saved IDs could crash the Diagnostics list; out-of-range saved dates could crash report formatting. | Repair safe-integer ID collisions without dropping valid events. Reject invalid dates and report the incomplete restoration. Both classes have pre-fix failing regressions. |
| Verified | Bare `token=value` assignments escaped one text-redaction path. | Reuse the credential-name table for plain assignments. Header-specific redaction keeps its own semantics; existing idempotence and structured-JSON tests remain passing. |
| Verified | Version alone could not identify a dirty build, and Map crash context omitted subview/request identity. | Stamp unique bundle IDs, preserve them across restart/coalescing, and capture scoped Map/Blame request IDs. Disposed owners cannot replace live context. |
| Verified | Minified crash frames lacked retained matching maps. | Retain exact chunks, SHA-256 hashes, source maps, and build metadata locally under `.build-evidence/<build-id>/`. Maps are removed from distributable output. An actual Vite-build integration test checks the mapping content, hashes, public inventory, and relative output paths. |
| Verified | The enum contract checker could not follow local TypeScript union aliases and could mistake payload strings for variants. | Use the existing TypeScript compiler AST, resolve bounded local aliases, distinguish payload objects from tagged variants, and fail on unresolved/cyclic/oversized expansion. Six new adversarial regressions and the repository enum contract failed before their fixes, including a 10,000-node work budget against repeated alias expansion. |
| Verified locally; remote CI unverified | Node tests did not execute Svelte's browser effects or the native macOS renderer. | Add dependency-free Chrome and WKWebView runners and CI jobs. Missing completion, early exit, oversized verdicts, failed assertions, and deadlines fail the gate. Temporary profiles and the local server are cleaned up. |

Current focused verification: **275 tests passed** across ten suites;
**24/24 browser assertions passed in Chrome and native macOS WKWebView**;
**74 native Git tests passed** across `git_ops`, `payload_budget_stress`, and
`adversarial_repos`. The browser checks include the real Diagnostics save-failure
and retry UI. These native renderer runs use IPC fixtures; the separate Rust
tests verify real temporary repositories. They do not claim the installed
application was replaced or that the original uninstrumented crash's exact
component has been recovered.

`npm run test:browser` is part of `ci:local`; CI also runs `test:webkit` on macOS.
See `harness/README.md` for prerequisites and interactive inspection. Build
evidence remains local and must be retained with the build being diagnosed;
no remote source-map upload or automatic release/install was added.

## Verification results

These results describe tested snapshots of a shared, changing checkout. They
are not a claim that one immutable release candidate passed every gate.

| Check | Verified result |
| --- | --- |
| Focused diagnostics, rendering, store, and native regressions | 275 frontend tests across ten suites, plus 74 native tests. |
| Final contract and diagnostics checks | 41 tests across six suites passed after the enum expansion budget was added; Node-script type checking and `git diff --check` also passed. |
| Real browser effects | 24/24 assertions in Chrome; repeated 24/24 in native macOS WKWebView. The final Chrome rerun also passed after another task extended the runner with a separate conflict harness. |
| Full native suite in an isolated build directory | 1,968 passed, zero failed, nine opt-in tests initially ignored, across 55 executables. Rust line coverage: 50,234/59,589 = 84.30%, above the 80% floor. |
| All nine native opt-in tests, using the same instrumented binaries | Nine passed: deep lane-solver fuzz, five real-repository graph/status checks, Pulse real-repository check, repeated documentation indexing, and the live update endpoint. Total native assertions reported as tests: 1,977 passed. |
| Earlier complete frontend coverage run | 5,048 tests in 388 files passed; 95.94% line coverage and 88.63% branch coverage. This run preceded the later conflict-editor implementation changes. |
| Build evidence | Production build passed; 54 matching emitted chunks and private source maps, zero distributed maps. Every private map parsed successfully; a sampled generated Map frame resolved to its Svelte source. |
| Static and repository checks | Earlier full Svelte/type checks passed with zero errors/warnings; IPC, wire types, release manifests, workflow lint, Rust formatting, and all-target Clippy with `-D warnings` passed at their tested snapshots. Node-script type checking passed again after the enum-checker change. |

The browser harness mounts production components in Svelte's actual client
runtime with explicit IPC fixtures. It exercises 2,000 hostile keys across 200
reconciliations, delayed and reordered requests, same-status content changes,
refresh storms, navigation, persistence failure/retry, and duplicate Markdown
backlinks. Its intentional duplicate-key canary must crash and record exactly
one contextual report; a clean run without that detection would fail.

Native fixtures cover untracked/staged/unborn/SHA-1/SHA-256 repositories,
ignored and unusual literal paths, Unicode, CRLF, empty files, missing final
newlines, binary/invalid encoding, oversized data, 200,000 empty source lines,
and damaged Git objects. The real-repository smoke tests report scan caps
explicitly; bounded samples are not complete repository coverage.

The monolithic `ci:local` attempt stopped on formatting being edited by another
task. A resumed native coverage build lost its shared target directory during
concurrent cleanup (`could not parse/generate dep info ... No such file or
directory`). Rebuilding and running with a task-specific target directory
succeeded. The shared frontend coverage report was also removed, so a combined
coverage-floor invocation correctly failed on the missing report.

The latest full frontend attempt then reported **5,035 passed, one failed test,
and one suite that could not compile** as conflict-editor work continued:

- `ConflictEditor.svelte`: `{@const}` is not an immediate child of an allowed
  Svelte block (`const_tag_invalid_placement`).
- `command-gate-fidelity-contract.test.ts`: `cmd_save_conflict` is neither
  compared nor documented as derived.

The latest full `npm run check` also reported the invalid const placement,
a conflict fixture missing required `diagnostics`, and two noninteractive
`tabindex` warnings in `ConflictComparison.svelte`. Those files belong to the
concurrent conflict-editor task and were left intact. These actual failed
checks are integration blockers; they are not presented as successful current
checks or excluded to manufacture a green run. The earlier enum and documented
field-count failures were fixed through the shared contract checker and docs.

Verification transcripts for this session:

- `/tmp/gitpulse-gaps-focused-final.log`
- `/tmp/gitpulse-browser-closure.log`, `/tmp/gitpulse-webkit-final.log`
- `/tmp/gitpulse-gaps-ci-final.log`
- `/tmp/gitpulse-native-isolated.log`, `/tmp/gitpulse-diagnostics-final.lcov.info`
- `/tmp/gitpulse-optin-{fuzz,repo,pulse,docs,network}-final.log`
- `/tmp/gitpulse-frontend-isolated-final.log`, `/tmp/gitpulse-check-closure.log`
- `/tmp/gitpulse-enum-alias-red.log`, `/tmp/gitpulse-closure-focused.log`

The isolated native build directory was removed after all checks completed,
reclaiming 5.1 GiB; the LCOV report and transcripts remain. These temporary
transcripts are not committed release evidence. Permanent reproduction commands
and fixtures live in `package.json`, `scripts/`, `harness/`, and the native tests.

At the final audit snapshot, the fourteen dedicated production-source files
(core diagnostics, native blame, affected list components, and build evidence;
excluding shared App/store/index changes) contain **635 added and 144 removed
lines**. Tests, browser runners, and documentation are excluded from that count.
Existing owners remain canonical; this audit added no dependency.

GitNexus reported HIGH upstream impact for repository hydration (four direct
callers, three flows, 46 affected symbols), and LOW for the indexed diagnostics,
key allocator, native blame, and enum checker owners. Svelte and newly added
symbols absent from the index were treated as UNKNOWN and source-traced. The
final shared-tree comparison reported CRITICAL risk across 93 files, 291
symbols, and 38 flows; this includes other tasks' Git launcher, conflict,
terminal, performance, and graph changes. Both risk warnings were surfaced.

## Verification limits

This is source and local-runtime evidence. The installed GitPulse application
has not been replaced or exercised with this build. Windows/Linux runtime
behavior, physical native IPC/UI integration, release signing/notarization, and
the exact historical crash trigger remain unverified. A production build also
retains Vite's main-chunk size advisory. Passing this bounded suite does not
establish that every possible repository, dependency failure, or future
concurrent edit is safe.

For a recurrence after installing the changes, copy Diagnostics before clearing
it. A `pane-crash` entry should include `Pane`, `Context`, `Stack`, and recent
navigation; caught Map failures should include their operation and repository
under the `code-map` source. Retain the report's version, build ID, timestamp, and matching private build
evidence so old persisted failures can be distinguished from a new incident.
If Diagnostics shows memory-only persistence, copy the report before closing
the app; use Retry saving once storage is available again.
