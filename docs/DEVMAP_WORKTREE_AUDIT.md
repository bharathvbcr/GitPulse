# DevMap concurrent-worktree audit

## Schema-20 qualification follow-up

The coordinated candidate includes worktree-root binding, durable revision acknowledgements, atomic migration, read-only navigation, Unix/Windows database alias checks, linked Git metadata observation, bounded watcher retirement and source/transport boundary fixes. The canonical implementation remains DevCouncil; the official scoped vendor check confirms all nine vendored crates match their sources.

The final extraction owner also rejects deadline or incomplete-walk overruns after scope collection and parser cleanup. Both fault-injection regressions failed before the fix; all 590 extraction tests and strict Clippy passed afterward. The corrected source is scoped into GitPulse through the official vendor script.

The final reader also includes the canonical nullable MCP contract correction. Its three regressions failed against the previous qualification snapshot (`expected string, got null` and invalid nullable/malformed type acceptance), then all 113 focused MCP tests passed. The rebuilt signed MCP passed real-store readback across all five advertised navigation schemas and six response envelopes. It returned 28 of 28 searched symbols, withheld all 28 stale snippets, reported impact as 80 of 4,999 with an incomplete walk, and refused a foreign repository root.

The staged release CLI passed 128 daemons / 256 editors with 257 source files per tree, 24,576 operations and 256 forced restarts; all 128 graphs equaled cold builds. A separate eight-tree workload used 4,097 source files and 126,979 graph edges per tree and passed all eight cold comparisons. All 16 copied live schema-17/18/19 stores passed migration, integrity, original-payload comparison, idempotence, navigation and rollback checks. The unchanged ten-second self-build gate passed at 8.599 seconds in hosted Linux CI and 3.269 seconds with the frozen Mac release. Earlier failures remain in the report.

Native capacity uses eight bounded measurement processes independently of the 128 daemons and 256 editors. Probe admission time and startup are included in native latency. A previous Windows run exhausted process creation with 256 unbounded probes and correctly failed without claiming cold-build coverage. Final DevCouncil CI passed on all three native platforms. Each completed 12,288 operations, 128 forced restarts and all 128 cold comparisons with 128 distinct private HEADs. Windows recorded 1,110 bounded filesystem-sharing retries. Final GitPulse CI also passed on all three platforms: macOS 2,071, Linux 2,070 and Windows 1,916 Rust tests passed with zero failures and ten explicit skips each. The candidate CLI integration passed separately against the exact staged binary.

The complete current evidence, exact commit/binary identities, measured envelopes, failed runs and cutover procedure are in DevCouncil's `docs/devmap/SCHEMA20_QUALIFICATION_2026-09-09.md` and `docs/devmap/AGENTIC_WORKTREE_QUALIFICATION.json`. Staged GitPulse source is `7b96c278098e583a9477f33db7871f1a8811265c`; it preserves unrelated active checkout edits outside the qualification snapshot. The local app is ad-hoc signed with strict/deep verification, not Developer ID signed or notarized.

**Live cutover remains pending.** Installed CLI/MCP readers still speak schema 19, and all 16 live stores were last verified at their original schema versions. Stop old clients, take fresh backups of current live generations, install matching CLI/daemon/application/MCP artifacts, run explicit `devmap repair --schema`, verify readback, then reconnect clients. Never restore an early rehearsal snapshot over later live work or run an old reader against schema 20. One inadvertent DevCouncil migration during diagnosis was restored after a full original-table comparison; the report retains the incident and restoration receipts.

These results qualify the measured local-filesystem workloads. They do not establish every OS/filesystem, hundreds of large monorepos per host, a universal latency/memory SLA, or installed end-to-end behavior before client reconnect. No dependency was added or access scope widened.

## Initial audit evidence (earlier candidates)

2026-09-09. GitPulse's embedded reader now validates the requested worktree against the store's durable owner before returning status or navigation answers. An index accidentally placed at another worktree's path is unavailable with an actionable reason, rather than answering with foreign symbols.

The canonical implementation and architecture are in DevCouncil's `docs/devmap/AGENTIC_WORKTREE_DESIGN.md`. Scoped vendoring updates `devmap-store` and `devmap-extract`; the generated copies remain the same implementation, not a second queue or indexer.

Schema 20 replaces timestamp-based queue acknowledgements with durable per-store revisions, binds each store to one root, makes migrations atomic, and prevents database aliases from bypassing writer ownership. The daemon takes ownership before reading its base; repair preserves newer events. Linked-worktree metadata and ignore changes are observed, and CLI queries resolve nested working directories to their Git worktree root.

Reader regressions first demonstrated acceptance of a foreign store, then verified refusal. The explicit candidate-CLI integration test builds two isolated maps through GitPulse's existing CLI adapter and verifies in-process search and impact from each. Run it with `GITPULSE_DEVMAP_BIN` pointing to the schema-20 candidate and `cargo test --manifest-path src-tauri/Cargo.toml --test devmap_embedding candidate_cli_maps -- --ignored`.

The 128-worktree harness runs 128 daemons and 256 editors, with bounded build workers. Its cold-build comparisons, crash recovery, metadata-only changes, navigation and memory measurements are recorded alongside the canonical design. These are small source fixtures; they do not prove hundreds of large monorepos fit the same machine.

Verified locally: 36,864 file operations, 3,072 IPC navigation requests, 512 metadata checks and 384 forced daemon restarts completed in 129.010 seconds. All 128 incremental graphs equaled cold builds; IPC query p95 was 204.218 ms. These are daemon measurements, not full MCP request latency. The frozen binary and raw counts are recorded in DevCouncil's `docs/devmap/AGENTIC_WORKTREE_VALIDATION.json`.

GitPulse validation: 1,515 library tests passed (two explicit skips: manual document-refresh timing and a live update-provider check), plus its subprocess assertion. The final vendored-store delta passed all 25 code-intelligence adapter tests; the final transport/embedding suites passed 28 tests with no skips, including the opt-in CLI integration against candidate `f0463d910a61de9beadb669ce8348d40ceb2b71419352b3df9d45fb3742be03a`. Formatting, Clippy with warnings denied, source-vendor consistency, and candidate schema agreement passed. The complete library run preceded the final database-alias and read-only fallback changes; targeted adapter, embedding and static checks cover that delta. No skipped check is counted as passing.

The installed CLI and MCP reader still speak schema 19. Their live stores were preserved. Install the CLI, daemon and GitPulse reader as a coordinated upgrade before migrating live stores. `check-vendor-schema.mjs` passes against the candidate CLI and is expected to reject an installed schema-19 CLI beside this schema-20 source. This distinction is a deployment gate, not a silent compatibility fallback.

Security impact is limited to filesystem integrity and correct worktree selection. No authentication, authorization, permissions or access scope is widened. Frontend behavior was not changed in this follow-up.
