# Code pane diagnostics and hardening audit

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
