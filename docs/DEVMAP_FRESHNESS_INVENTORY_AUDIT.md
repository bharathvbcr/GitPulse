# DevMap freshness and inventory audit — 10 September 2026

Implemented locally in isolated `codex/devmap-freshness-audit` branches in GitPulse and DevCouncil. This side audit addresses the selected MCP freshness and bounded marker-inventory gaps. It does not replace the parent Git-client release audit. Nothing was pushed, installed, or deployed.

## Verified defects and fixes

| Reproduced against the pre-fix code | Canonical repair |
| --- | --- |
| Parser-free status returned analyzer uncertainty before checking missing/changed source files. | `Store::status` checks source bytes independently, combines both reasons, and exposes nullable `source_freshness` and `analyzer_freshness` through CLI, daemon and GitPulse/MCP adapters. |
| A tracked Cargo package beyond eight directory levels was absent from marker inventory. | Git repositories reuse the bounded Git path-list command, with the marker inventory's own exclusions and limits. The filesystem depth ceiling remains only for non-Git fallback. |
| An unreadable directory disappeared with `computed: true`, no errors, and no truncation. | Directory-open, entry and metadata errors are counted and sampled; incomplete scans cannot claim `inventory_complete`. |
| Malformed `package.json` looked like a valid manifest declaring no scripts. | Syntax errors, invalid document/scripts shapes and non-string selected scripts are reported. Empty scripts are not advertised as commands. |
| Replacing a discovered manifest with a FIFO blocked the subsequent open. The child exceeded its two-second deadline. | Manifest reads reuse the existing nonblocking, descriptor-checked source reader with a tighter 256-KiB ceiling, including bounded reads and replacement checks. |

Each defect had a failing regression before its repair. The existing depth, directory, symlink-loop, unreadable/oversized manifest, queue, HEAD-provenance and source-snippet tests were retained.

## Contracts and limits

- Aggregate freshness still requires a committed generation, no pending edits or store degradation, and both freshness checks passing. `null` means unverified. Matching source bytes do not certify a parser identity that the binary cannot compare.
- Marker metadata reports source, completeness, examined entries, eligible Git-path total (unknown for the fallback), and unreadable counts alongside at most 64 error samples. Computation is not completeness.
- Git discovery: the existing 30-second/64-MiB command bounds, then at most 50,000 eligible paths and a cooperative five-second metadata deadline. Root declarations take priority before nested paths when capped.
- Non-Git fallback: depth eight, at most 20,000 opened/pending directories, 200,000 entries, and a cooperative five-second deadline. The pending frontier is bounded as well as completed visits.
- Both retain the marker skip policy for dot-directories and dependency/build output. Marker completeness does not establish parser, call-graph, language, or dynamic-dispatch completeness. Command detection remains a supported-marker heuristic rather than full build-system interpretation.
- Cooperative deadlines cannot interrupt an OS filesystem call already in progress. Filesystem freshness remains an observation, not an atomic filesystem snapshot.

No new dependency, database schema, authentication, authorization, or permission scope was introduced. Source verification uses the existing repository discovery and source-read policy. The parser-free dependency tree was checked and contains no tree-sitter dependency.

## Verification

| Check | Result |
| --- | --- |
| GitPulse full native default suite | Passed; the log reports 2,280 passing test results and 14 ignored results, including subprocess result groups. Ignored tests are not counted as passes. |
| Parser-free `devmap-query` suite | Passed: 213 tests, zero failures or ignores, including the final rerun. |
| Updated query unit and inventory regression suites | Passed, including deep Git packages, unreadable paths, invalid JSON/shapes, entry/frontier/deadline bounds, 1,000-error sampling and FIFO replacement. |
| Source/HEAD and subprocess policy regressions | Passed. The structural subprocess audit was rerun after moving the child-process fixture into a test source directory. |
| GitPulse source-change stress | 100 cycles; each checks edit, restore, add, delete and restore states: 500 status assertions through the real parser-free adapter. Analyzer compatibility remains unknown throughout. |
| Rebuilt MCP on the isolated GitPulse repository | Ten sequential source checks: median 183.9 ms, maximum 282.5 ms. Eight concurrent requests completed in 676.7 ms. All reported source current, analyzer unverified and overall freshness false. |
| CLI versus embedded reader | The parser-enabled CLI reported both checks true and overall freshness true for generation 2. The rebuilt MCP reported source true and analyzer null for that same generation. |
| Real marker inventory (generation 2, before this report) | All 1,950 eligible paths examined, `inventory_complete: true`, no truncation or unreadable/oversized entries. The graph separately reports seven SQL import-blind files. |
| GitPulse IPC/type/vendor-schema checks, Svelte/TypeScript, codeintel client tests, production build | Passed. |
| Format and strict Clippy | Passed in both workspaces; GitPulse was rechecked after the test-fixture integration fixes. |
| Vendoring | The three changed DevMap crates match the isolated canonical upstream and their generated local hashes. Existing drift in dc-store/devmap-resolve and unavailable external framework comparisons remain disclosed by the normal `--allow-drift` check. |

The first full upstream workspace run reported 2,497 passing results, two failures and three ignored results. One failure was the new child-process test's placement in a production source file; it was repaired, and that target and affected query tests passed on rerun. The other failure was reproduced unchanged in a separately reconstructed pre-audit snapshot:

```
-p devmap-extract --test test_phase2_hardening
scope_locals_are_keyed_by_the_identity_calls_report_as_their_caller
closure parameters bind names, typed or not: ["bare", "handler"]
```

That pre-existing closure-parameter test failure remains an upstream release gate. This audit does not claim a green full upstream suite. The original failed logs are retained alongside the focused repair results.

## Remaining qualification and integration

The native default run ignored provider/tool installation probes, a live update check, manual document-refresh timing, deep graph fuzzing and real-repository smoke tests. The directly relevant candidate CLI embedding test was subsequently run explicitly and passed; its result is recorded in `candidate-cli-embedding.log`. Upstream's external plugin validator and long daemon-storm test were not run; the ignored pruning child is a subprocess fixture. Full frontend/browser/coverage CI, Windows/Linux runtime qualification, network-filesystem stalls, power-loss behavior and a long production soak were not established by this side audit.

No installed app or MCP was replaced. The active parent worktree and canonical source files were left untouched. These changes need integration into the parent release branch before release qualification. GitPulse's crate-level vendor wrapper also carried the upstream snapshot's pre-existing changes in `devmap-extract/src/wiring.rs` and `devmap-query/src/guides.rs`; they are visible in the consumer patch and are not claimed as fixes from this audit.

## Local evidence

All evidence is under `/Users/bharath/.codex/worktrees/devmap-freshness-audit/evidence/`:

- `DevCouncil-baseline.patch`, `GitPulse-baseline.patch`: captured starting changes, separate from this audit.
- `DevCouncil-audit.patch`, `GitPulse-audit.patch`, `audit-delta.json`: reviewable changes relative to those captured baselines.
- `upstream-workspace-tests.log`, `upstream-baseline-parser-failure.log`, `upstream-final-checks.log`: full run, baseline reproduction and focused fixes.
- `parser-free-query-final.log`, `gitpulse-native-tests.log`, `gitpulse-final-lint.log`, `gitpulse-contract-checks.log`, `candidate-cli-embedding.log`.
- `mcp-runtime-probe.json`, `manifest-coverage.json`: actual protocol responses, measurements and inventory metadata.

The workspaces reused isolated APFS copies of existing dependency/build caches; this was not a hermetic clean build. Original worktree snapshots and baseline patches are retained for attribution and safe review.
