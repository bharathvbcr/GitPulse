# DevMap staleness and agent navigation audit — 8 September 2026

## Verified finding

Index staleness does not disable DevMap navigation. A persisted generation remains readable while source files change. Search, dependencies, impact, trace, and explore can answer about that generation; newly introduced symbols are absent until a rebuild or watcher drain. Stored source coordinates are used only after verifying the indexed content hash, so changed or deleted files retain their symbol identity with an explicit unavailable-source reason and no misleading current snippet.

This is a snapshot limitation, not evidence of a broken index. Availability, source freshness, and graph completeness are separate claims. An unverified or stale map must not be used to conclude that a new symbol has no callers or that no tests need to run.

The initial GitPulse CLI probe reported `is_fresh: false`, `pending_count: 0`, and `query_ready: true`; MCP search still returned stored symbols. An empty watcher queue therefore did not mean that source bytes matched. The installed GitPulse MCP omitted the freshness information, which made the distinction hard for agents to see.

## Repairs

| Defect reproduced before repair | Change |
| --- | --- |
| GitPulse discarded query `source_freshness`, including the explicit null for an unchecked tree. | Preserve the field through the shared adapter, every response constructor, Rust/TypeScript wire types, and MCP output schema. |
| GitPulse status and session briefs hid the store's degraded freshness reason. | Expose freshness, pending count, and the reason while keeping stored navigation available. CLI, daemon, and embedder delegate to one verdict on `StoreStatus`. |
| A corrupt status row caused affected-test selection to continue with `fail_closed: false` and an empty test list. | Propagate the actual status error and refuse to certify test selection. The regression inserts a malformed coverage row into a real SQLite store and verifies the distinction from an empty graph answer. |
| A watcher event arriving during a manual build was permanently discarded after `skip_building`. | Retain the dirty event with at most 30 paced retries. Preserve queue fairness, visibility rules, reset/close cancellation, and a visible exhaustion state. |
| A source snippet truncated to an empty string by a small budget lost its omitted-byte count. | Preserve `source_span_omitted_bytes` across the adapter, separately from source-read failures. |
| Successful navigation responses emitted `reason: null` against a string-only MCP output schema. | Declare the nullable wire type and validate the actual serialized fields in a regression. |
| The catalog validator ignored a non-string `type` declaration, including the nullable union now needed by freshness. | Validate declared type alternatives and reject empty, malformed, or unknown alternatives. Existing input null-as-absence behavior stays covered. |

No dependencies, database schema, parser behavior, access permissions, or Git mutations were added. DevMap's common verdict is changed upstream in DevCouncil; its GitPulse copy is generated with `node scripts/vendor-crates.mjs --crate=devmap-store`.

Schema handling follows the [JSON Schema type-union contract](https://json-schema.org/understanding-json-schema/reference/type). Security impact: malformed schema declarations no longer bypass type checking; no authentication, authorization, or permission scope changes.

## Verification contracts

- Real CLI edit/rebuild probes cover 20 same-size edit cycles, unchanged-file snippets during unrelated edits, add/rename/delete/restore, and stable search/dependency/impact/trace/explore availability.
- Native adapter regressions cover unknown freshness, status reasons, failed status reads, and stale-source refusal. Hook tests distinguish unverified freshness from a current generation.
- Live-index tests cover busy-writer recovery, bounded exhaustion, fairness across repositories, hidden-window suspension, late completions after close/reset, and 2,000 continuously arriving watcher events.
- Existing kernel suites cover durable transactions, concurrent generation reads, watcher reconciliation, interrupted builds, source-size bounds, and daemon lifecycle recovery. Capped impact/affected results remain lower bounds; they are not used to skip these suites.

## Measured verification

| Check | Verified result |
| --- | --- |
| Pre-fix characterization | 922 kernel store/query/serve tests and 19 GitPulse adapter tests passed before new regressions were added. |
| Failures reproduced before repair | Real corrupt SQLite status row authorized an empty affected-test result; adapter freshness and source omission were absent; busy-writer events lost their refresh; session brief omitted freshness; nullable schema checks failed. Logs: `/tmp/gitpulse-staleness-red.log`, `gitpulse-liveindex-red.log`, `gitpulse-brief-red.log`, `gitpulse-source-cap-red.log`, `gitpulse-staleness-schema-red.log`. |
| Focused final tests | 24 adapter tests, 42 hook tests, 18 live-index tests, and 113 MCP tests passed. These overlap the broader suites and are not extra unique coverage. |
| Isolated GitPulse `npm run ci:local` | Passed: 5,182 frontend tests across 393 files; 24 browser checks; 2,065 native tests across 60 targets; IPC/type/vendor/release/workflow checks; Svelte/TypeScript; production build; format; strict Clippy; coverage floors. `/tmp/gitpulse-staleness-isolated-ci.log`. |
| Coverage from that full run | Frontend lines 96.00% (9,332/9,721), branches 88.60% (8,277/9,342); Rust lines 84.83% (52,761/62,197). The final nullable-schema follow-up was added afterward and separately verified by all 113 MCP tests and format/Clippy, so these percentages are not claimed for that later delta. |
| DevMap default workspace suite | 2,289 passed, 0 failed, 3 ignored; format and strict Clippy passed. Two cold graph digests matched. `/tmp/devmap-staleness-verify-final.log`. |
| Daemon storm | 40 cycles passed in 367.47 seconds, including 10,000-file creation storms, directory rename/delete/recreation, 1,000 chained renames per applicable cycle, kill/restart, and cold-build graph equality. RSS half-means after warm-up: 117,875 to 119,700 KiB (+1.5%). `/tmp/devmap-staleness-pinned-storm40.log`. |
| Remaining kernel gates | Memory-model, five-build storage plateau, and five-cycle incremental/cold equivalence passed. The smoke equivalence gate does not assert long-run storage growth. `/tmp/devmap-staleness-remaining-gates.log`. |
| Vendoring | All nine vendored crate entries matched their canonical source on the final vendor check. |

The isolated source checkout is `/Users/bharath/.codex/worktrees/devmap-staleness-validation/GitPulse`, based on `2fb8efc`, with this audit's changes only. Existing dependencies and the native target cache were reused; it was not a hermetic build environment. The canonical GitPulse and DevCouncil working trees contain separate concurrent work, which was preserved. DevMap and GitNexus impact queries were supplemented by actual call-site inspection: aggregate shared-tree change detection was high risk because it included that other work; capped graph results were not treated as complete scope.

The frontend skip concerns an absent local promotional draft. Nine native tests were explicitly ignored by the default project command: a document-refresh timing workload, a live update-provider probe, deep graph fuzzing, one real-repository pulse-reader probe, and five real-repository graph/status smoke tests. They are not counted as passing. DevMap's ignored daemon storm and external plugin validator were explicitly run and passed; its remaining ignored pruning child is a subprocess helper exercised by the parent. Workspace-wide mutation testing and platform CI were not run.

## Failed runs and remaining gate

The DevMap `verify.sh` result is **not all green**. Its unchanged 10-second self-build gate failed at 15,331 ms, then 10,957 ms, then 11,522 ms after this audit's native CI ended while other host work continued. The last run built 1,638 files: cold database 166 MiB against 255 MiB budget, peak RSS 724 MiB against the stricter 888 MiB scaled budget. Size and memory passed; build latency remains an open gate. Shared-host load is a possible contributor, not a proven sole cause. The remaining gates were executed separately without raising or bypassing their thresholds.

Earlier attempts failed with `No space left on device`; only verified untracked, unused Cargo incremental caches were reclaimed. A shared-tree watcher stress test timed out and passed unchanged on a focused rerun. Another shared frontend run failed in a concurrently edited terminal launch-request test; the isolated full run passed. A concurrent Cargo cleanup removed the daemon binary mid-soak, so that interrupted run was discarded and the successful stress run used a pinned executable outside the shared cache. None of these failed or interrupted attempts is counted as a passing full run. The upstream audit records the pinned executable and test-source SHA-256 values in DevCouncil's `rust-port/STALENESS_AUDIT_2026-09-08.md`.

## Operational limits

`devmap build` updates a snapshot. `devmap serve` maintains it while watching the tree. GitPulse's own refresh scheduler runs only for the eligible visible repository; inactive edits can remain pending until the repository becomes active. A build that finishes during continued edits is not a promise that the filesystem has stopped changing.

GitPulse deliberately links DevMap without parser packages. That reader can navigate and verify individual source snippets, but cannot compare the stored grammar identity against a compiled parser it does not contain. Status reports that uncertainty and affected-test selection remains conservative. The full CLI has the parsers needed for that comparison. Neither interface's freshness verdict proves complete static analysis of dynamic dispatch, reflection, unsupported syntax, or languages with incomplete extractors.

All filesystem freshness results are observations that later writes can invalidate. This audit does not establish an atomic filesystem snapshot, power-loss recovery, every operating system, a multi-week production soak, or correctness for every possible program.
