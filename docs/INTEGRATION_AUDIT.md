# Coordinated tool integration audit — 2026-09-08

This audit covers the local GitPulse, DevCouncil, Manvi and MarkDev checkouts.
It includes the pending GitPulse graph, terminal, conflict, diagnostics and
work-overview changes, and the canonical library changes those features consume.
It is local verification, not a release or proof that every possible defect has
been eliminated. The integration contract is in [MODULE_INTEGRATION.md](MODULE_INTEGRATION.md).

The coordinated GitPulse snapshot includes 166 non-Markdown files with 14,787
lines added and 2,257 removed, including work pending before this audit.
The refreshed GitNexus staged analysis classified its impact as HIGH: 990
indexed symbols and 14 execution flows. The affected Git execution, document
rename and repository-refresh paths were covered by the full local gate.

## Reproduced failures and fixes

| Boundary | Evidence before the fix | Final behavior and regression coverage |
| --- | --- | --- |
| Vendor drift detection | Deleted upstream files and inherited Cargo manifest changes were missed. | Compare the complete prepared file set and resolved manifest. Deleted files, additions, local edits and unavailable sources remain distinguishable. |
| Vendor replacement | A preparation error could damage the current snapshot; concurrent updates were not excluded. | Prepare all selected crates first, lock updates, replace with a recoverable previous tree, and preserve the current tree on preparation failure. Scoped updates preserve unrelated crates. |
| Vendor source traversal | A symlink could include content outside its crate. | Reject symlinks and non-regular source entries at the canonical traversal boundary. No new application access is granted. |
| Idle Manvi sidecar output | A real child remained alive after flooding stdout while no request consumed it. The regression failed before the fix. | One stdout pump serves production and the test harness. Eight queued frames, each at most 4 MiB, bound retained output; overflow kills and reaps the child. The same test passed after the fix. |
| Standalone devmap HTML | The query crate's HTML included an asset outside the reusable crate. | Both HTML renderers use the asset inside `devmap-query`. A fresh external Rust consumer compiled with only that direct dependency, and the installed CLI rendered an interactive map. |
| MarkDev feature configurations | `cargo test --no-default-features --no-run` failed with unresolved optional FFI/highlight imports in integration tests. | Each optional test target declares its required features. All four library configurations are now tested, with the full default suite preserved. |
| Fresh and migrated devmap repositories | Three regressions reproduced a hardcoded legacy database path, a legacy-first graph resolver, and the wrong fresh repository-map default. A real standalone CLI index was unavailable to the old MCP reader. | Database, graph and repository-map adapters share the CLI's canonical path resolver through the existing query-library dependency. Standalone, legacy-only, mixed directories and isolated `DEVMAP_HOME` overrides are covered. |
| Replaceable Manvi adapters | A custom adapter could return nil status or nil query data/index and receive `ok: true`; malformed stock producer envelopes could also be accepted. | Validate adapter results at the host boundary, preserve kind-specific completeness evidence and refuse missing or malformed results. |
| Query error classification | A caller's invalid depth was classified as `E_DEPENDENCY`. | Shared query validation distinguishes malformed requests (`E_BAD_REQUEST`) from unavailable/incompatible producers (`E_DEPENDENCY`). |
| Polling-latency regression test | The test passed alone but repeatedly failed in the concurrent library suite, where only its bounded path shared spawn-admission contention. | Move the timing check into its own integration executable through the existing public capture API, preserving all twelve alternating samples and the exact 8 ms allowance. A real flat-15 ms polling-loop mutation still fails it. |
| Extraction progress classification | The full GitPulse index reported 83 failed files and invalid progress, while its stored coverage correctly reported zero parse failures. SQLite showed all 83 were `NotApplicable` prose/data files. | Use canonical `Extraction::is_parse_failure()` for progress, preserving genuine failure counts and valid cached-file accounting. Separate cold and legacy-cache regressions failed before correction. |
| Native Map coverage summary | The installed app rendered four gaps in every category by counting envelope keys, while the real status had one import gap and zero in the other categories. | Interpret producer counts at the shared repository-map boundary, preserving zero, truncated and unavailable evidence instead of counting metadata keys. |

The vendor suite passed 29 tests, including twelve update/delete cycles,
idempotence, inherited metadata, scoped preservation, preparation failures,
locking and symlink rejection. The five new vendor regressions failed on the
original updater before implementation. The sidecar suite passed 17 tests.

The DevCouncil update additionally covers progress cancellation and output
discipline, extraction-cache integrity, generation publication, malformed HTML
endpoints, duplicate IDs, confidence boundaries, deterministic projection and a
50,000-link ceiling. Counts and truncation remain explicit; capped projections
are not presented as complete graphs.

## Reusable contracts

Devmap status advertises host contract version 1, binary/store compatibility,
reader readiness, query readiness and actual parser-derived capabilities.
Freshness remains separate from schema compatibility. An older executable
does not advise downgrading a newer database.

Manvi modules register or explicitly replace handlers before a server starts.
Registration is per server and freezes before serving; `hello` remains reserved
and reports the resulting operation set. Hosts can inject the devmap client.
The process module exposes status and advanced queries with producer evidence,
including generation, counts and incomplete-walk fields.

These are source-library and process contracts. Compatible process tools can
be selected through executable configuration. Rust and Go library changes
require recompiling their host; arbitrary ABI compatibility is not promised.

## Verified local evidence

- **GitPulse:** the complete `npm run ci:local` passed with 389 frontend test
  files and 5,090 tests, plus 2,014 Rust parent tests (9 explicitly ignored by
  the normal suite). The isolated environment child passed separately, 1/1.
  Chrome passed 24/24 checks. Formatting, strict Clippy,
  Svelte/type checking, release/workflow consistency, 187 IPC handlers and
  50 wire contracts passed. Coverage was 95.99% frontend lines, 88.56%
  frontend branches and 84.27% Rust lines. Vite's entry chunk exceeded its
  500 kB advisory threshold; the production build passed.
  All nine normally ignored Rust tests then passed explicitly under cached
  `cargo llvm-cov --workspace --no-report -- --ignored --test-threads=1`, with
  both `GITPULSE_SMOKE_REPO` and `GITPULSE_PULSE_REPO` set to this checkout.
  Eight exercised local document refresh, default-scale deep lane fuzzing and
  real-repository readers; one successfully contacted the upstream release
  repository. The knowledge sample remained explicitly truncated (128 of
  1,097 files). This supplemental run did not regenerate the coverage report.
  Evidence: `/tmp/gitpulse-ignored-rust-llvmcov-20260908.log`.
- **DevCouncil:** canonical `rust-port/verify.sh` completed with `ALL GATES
  GREEN`: formatting, strict Clippy, workspace tests, hostile fixtures,
  concurrency, determinism, fuzz smoke, memory and incremental/cold parity.
  The self-build indexed 1,626 files in 8.462 seconds with a 152 MiB database
  and 720 MiB peak RSS against the gate's 867 MiB limit. Five incremental
  cycles matched cold builds. Optional mutation testing was skipped.
  The final progress-classification correction then passed 187 store tests
  (one ignored), 570 extraction tests, formatting and strict Clippy. A real
  GitPulse rebuild at generation 189 completed all 1,120 files with 1,119 cache
  hits, zero failures, valid progress and 100% completion. A separate malformed
  notebook fixture still reported one genuine failure. The repository index
  remains fresh/query-ready with one explicit Swift import-extraction gap.
- **MarkDev:** 334 default-feature tests and 869 test executions across the
  other three feature configurations passed, with zero ignored tests. Strict
  Clippy, formatting and 166 release-contract tests passed. The reusable core
  built as a universal archive containing both `arm64` and `x86_64`.
- **Manvi:** final `./verify.sh` passed the checks it could execute: 3,990 Go
  tests accounted for (2 TUI subtests skipped), 82.2% statement coverage,
  306 tests against real store/DevCouncil binaries, 139 Rust tests, 775 shared
  Go/Rust glob cases and 256 command cases against Python. It verified all
  27 declared fuzz targets are reachable, 88 actual shell command lines,
  400 generated command lines and 869 benchmark/instrument checks. The local
  model endpoint also passed a real wire-contract request. `golangci-lint`,
  `govulncheck` and `nilaway` were outside that run's PATH; their existing
  executables were subsequently located under `/Users/bharath/go/bin`.
  Re-running with that task-local PATH exposed six new lint-debt violations;
  after correction, all 21 enforced linters and ten debt ratchets passed.
  The reproduced pre-fix result is retained at
  `/tmp/manvi-analyzer-debt-failure-a828c01.log`.
  `govulncheck` found zero reachable vulnerabilities and zero advisories in
  imported packages, while reporting 21 advisories in required modules that
  this call graph does not reach. `nilaway` remained at its existing 78-finding
  ceiling. The analyzer-enabled full verification then passed with the live
  hosted-provider checks still unexecuted. Its evidence is
  `/tmp/manvi-verify-final-with-analyzers-20260908.log`.
  Anthropic, Gemini and xAI were
  exercised through scripted servers, not live hosted endpoints. The log is
  `/tmp/manvi-verify-final-20260908.log`. That run also reported a graph/index
  generation mismatch after live interoperation tests advanced the index;
  post-run synchronization restored matching graph/database generation 28,
  with 666 file paths and zero missing files.
- **Installed Manvi process:** a separate seven-request probe against a fresh
  standalone `.devmap` repository passed `hello`, status and all four advanced
  query kinds (`explore`, `impact`, `trace`, `affected`). Status reported schema
  19, generation 1 and fresh/query-ready state; an invalid depth returned
  `E_BAD_REQUEST`. Evidence: `/tmp/manvi-installed-standalone-smoke.json`.
  The final installed revision also passed six requests against its repository
  at generation 28: `/tmp/manvi-installed-protocol-smoke-final.ndjson`.
- **Installed GitPulse MCP:** the replaced executable reports 0.0.8 and passes
  `npm run mcp:doctor`. Against the same standalone fixture, the previous
  0.0.7 process returned `available: false` because it searched the legacy
  location. A fresh 0.0.8 process returned `leaf` from `core.py`, with
  `available: true`, shown/total 1/1 and no truncation. Evidence:
  `/tmp/gitpulse-mcp-before-update.json` and
  `/tmp/gitpulse-mcp-after-update.json`.
- **Browser:** the installed devmap built a disposable repository, reported
  contract 1/schema 19/generation 1 with current, fresh, query-ready status,
  and returned the expected `leaf` symbol. Its offline HTML showed all 3 of 3
  nodes and 2 of 2 links. A real browser rendered the graph and clicking
  `core.py` showed its path, language and degree.
- **WebKit:** the 24-case native WebKit harness passed on retry. The first
  attempt timed out after 60 seconds under concurrent verification load;
  that failed attempt is retained as evidence.
- **Native navigation and Manvi:** the installed app loaded its existing ten
  repository tabs. The file map showed 1,120 nodes, 18,195 links and 130 groups,
  with one rejected projection record reported explicitly. Searching for
  `src-tauri/src/codeintel/mod.rs`, selecting it and opening it reached the
  actual source file in Explorer. The Manvi panel showed protocol 1, host
  posture, `/Users/bharath/.local/bin/manvi`, and both `devmap.status` and
  `devmap.query` among its negotiated operations.

The subsequent native coverage-summary correction passed all 5,095 frontend
tests in 389 files, 20 focused map tests, Svelte/TypeScript checks and 24/24
Chrome regressions. Final frontend coverage rounds to 96.00% lines (9,324/9,713)
and 88.59% branches (8,265/9,330); the combined coverage-floor gate passes.
Malformed counts now remain visibly unavailable;
known zero counts are omitted, and valid truncated envelopes retain their
complete total. GitNexus could not index the former Svelte-local helper;
file-level impact was LOW with zero indexed upstream dependants/processes.
The component's delegation to the shared helper was inspected directly.
Evidence: `/tmp/gitpulse-coverage-gap-summary-prefix-red.log`,
`/tmp/gitpulse-coverage-gap-malformed-prefix-red.log`,
`/tmp/gitpulse-coverage-gap-malformed-postfix-green.log`,
`/tmp/gitpulse-coverage-gap-malformed-coverage.log` and
`/tmp/gitpulse-coverage-gap-malformed-browser.log`.

An early final GitPulse attempt failed the Rust spawn-latency test: the
bounded path added 13.272625 ms over a bare 16.457 ms spawn/reap, exceeding
the test's 8 ms allowance. It then passed ten normal focused runs and one
coverage run. The second full attempt stopped at frontend coverage: the
document-statistics scaling ratio was 13.2941 against a limit of 13; that
test passed ten focused runs. A complete run with other heavy builds stopped
passed both unchanged, but after the path fix the timing test again failed in
the concurrent library suite (+18.426291 ms over a bare 10.938334 ms) and
immediately passed alone under coverage.

The timing test now runs in its own integration executable. Its production
entry point, `capture_command`, already delegates to the same gated runner;
no new public API or production polling change was needed. The twelve-sample
method and 8 ms allowance remain. A deliberately inserted flat 15 ms sleep
in the production polling loop failed the isolated test (+10.646125 ms),
and the original production bytes were restored before its passing rerun.
A weaker mutation of only the starting backoff constant passed because an
already-exited child can be reaped before sleeping; this did not reproduce
the intended unconditional-poll regression and was not reported as a detected
fault. Failed full attempts remain failures in this record.

Local logs retained for this audit include
`/tmp/gitpulse-ci-local-final-isolated-timing-20260908.log` (final complete
passing gate), `/tmp/gitpulse-ci-local-final-cleanhost-20260908.log` (earlier
passing gate before the canonical-path fix),
`/tmp/gitpulse-ci-local-final-20260908.log` and
`/tmp/gitpulse-ci-local-final-rerun-20260908.log` (failed attempts),
`/tmp/gitpulse-vendor-red.log`, `/tmp/gitpulse-vendor-stress.log`,
`/tmp/gitpulse-sidecar-prefx-red-20260908.log`,
`/tmp/gitpulse-sidecar-tests-20260908.log`,
`/tmp/markdev-feature-baseline.log`, `/tmp/markdev-feature-matrix.log`,
`/tmp/markdev-core-gate.log` and `/tmp/markdev-core-release.log`.
Path-discovery failures are retained in
`/tmp/gitpulse-devmap-path-prefix-codeintel.log`,
`/tmp/gitpulse-devmap-path-prefix-viz.log` and
`/tmp/gitpulse-devmap-path-prefix-repomap.log`.
The polling control and restored run are retained in
`/tmp/gitpulse-process-spawn-timing-flat15-loop-red.log` and
`/tmp/gitpulse-process-spawn-timing-restored-green.log`.
Progress-classification regressions are retained in
`/tmp/devmap-progress-regression-red-20260908.log`,
`/tmp/devmap-progress-cached-regression-red-20260908.log` and
`/tmp/devmap-progress-regression-green-20260908.log`.
The complete targeted follow-up and real-workload results are in
`/tmp/devmap-progress-store-green-20260908.log`,
`/tmp/devmap-progress-extract-green-20260908.log`,
`/tmp/gitpulse-devmap-progress-fixed-20260908.json` and
`/tmp/devmap-supported-failure-fixed-20260908.json`.

The only vendored source change after GitPulse's complete gate was
`devmap-store/src/extract_cache.rs`; every other crate file still matched its
tested SHA-256. That module and its exports require `devmap-store/parse`, which
GitPulse's resolved Cargo feature tree disables. Therefore the final progress
fix changes the installed devmap builder and packaged source inventory, while
the compiled GitPulse reader/desktop/MCP paths retain their tested code.
Final-inventory verification also passed strict all-target Clippy, formatting,
the two real-store integration tests and the feature assertion above:
`/tmp/gitpulse-final-vendor-native-focused-20260908.log`.

## Limits and recovery

The vendor directory lock excludes other vendor updates, not Cargo readers.
A process killed between directory renames can leave the previous tree in
`src-tauri/.vendor-lock/previous`; inspect and restore it before clearing the
lock. This is not a power-loss-atomic transaction.

The test runs exercise bounded hostile workloads. They cannot establish
absolute confidence, every third-party provider, every operating system, or
unbounded workload behavior. MarkDev's core checks do not prove physical
AppKit behavior. Local application installation does not constitute signing,
notarization or publication of a trusted release.

Manvi retains governed static-analysis debt, including 78 potential nil
findings. That count matches the baseline and does not establish which warnings
are runtime defects. Passing the ratchet means the count did not increase; it
does not mean every existing finding was resolved or the code is free of defects.

Freshness also does not establish full language coverage. The refreshed four
repositories report their gaps explicitly. DevCouncil's own corpus includes
unsupported-language fixtures, an unparsed minified bundle, and a generated
30.7 MB grammar file refused by the 1 MiB source ceiling; its status correctly
remains partial. Import extraction is unavailable for some languages,
including Swift. No complete-language-coverage claim is made.

The native build also retains two non-fatal advisories: Vite's entry-chunk size
and Tauri's deprecated `STATIC_VCRUNTIME` setting. Windows/Linux execution,
trusted release signing/notarization and live hosted-provider behavior were
not established by the local macOS checks.
