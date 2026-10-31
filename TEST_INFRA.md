# E2E Test Infrastructure & Verification Architecture (GitPulse & DevCouncil)

## 1. Test Philosophy

The GitPulse & DevCouncil E2E testing framework is engineered around rigorous, opaque-box, requirement-driven verification designed to evaluate performance, concurrency, storage efficiency, and multi-lane workflow separation without depending on white-box implementation internals.

### Core Principles
1. **Opaque-Box Verification**: Tests operate exclusively through public CLI surfaces (`devmap`, `git`, `cargo`, `node`), IPC message boundaries (`cmd_get_status`, `cmd_devmap_maybe_refresh`, `cmd_list_branches`), and file/data storage invariants (SQLite WAL, Schema 29 tables, zstd compression frames).
2. **Benchmark Integrity Mode**: Zero external pre-built frameworks or open-source logic copied from external repositories. All test harnesses, oracles, and test suites are natively implemented within the repository using standard toolchains (`bun`, `vitest`, `cargo`).
3. **Progressive Testability & Independence**: Every test is completely self-contained. Tests manage their own temporary Git repositories, isolated SQLite databases, and memory structures via `createTempManager()`, with deterministic cleanup in `afterAll`/`afterEach` hooks. Tests never depend on execution order.
4. **Authoritative Expected Output Derivation**: For every test case, expected outputs are derived from authoritative specifications in `PROJECT.md`, `ORIGINAL_REQUEST.md`, and reference oracles (`scripts/check-schema-parity.mjs`, SQLite WAL pragma contracts, zstd frame magic headers).

---

## 2. Feature Inventory Mapping

All 15 features inventoried in `PROJECT.md § Feature Inventory` are mapped to dedicated test suites across all 4 tiers (+ Tier 5 adversarial hardening):

| # | Feature | Milestone | Description | Tier 1 Tests | Tier 2 Boundaries | Tier 3 & 4 Linkage |
|---|---------|-----------|-------------|--------------|-------------------|-------------------|
| 1 | Schema 29 Parity Sync | M1 | Sync DevCouncil schema 29 to GitPulse vendored crates, PATH binaries, and pin files | 1.1 – 1.5 (5 tests) | 1.1-B – 1.5-B (5 tests) | Pair 2, Pair 8, Scenario 4 |
| 2 | Storage Write Latency Optimization | M1 | Eliminate $O(N)$ JSON deserialization in `write.rs:2407`, batch FTS5, optimize unresolved calls | 2.1 – 2.5 (5 tests) | 2.1-B – 2.5-B (5 tests) | Pair 2, Scenario 2 |
| 3 | Manifest & Code Graph I/O Optimization | M1 | Avoid redundant 37+ MB `code_graph.json` rewrites on routine watcher ticks; generate on demand | 3.1 – 3.5 (5 tests) | 3.1-B – 3.5-B (5 tests) | Pair 6, Scenario 4 |
| 4 | DevMap Live Refresh Decoupling | M1 | Decouple devmap live-refresh IPC from status probes (`wave/devmap-ipc-stall`) | 4.1 – 4.5 (5 tests) | 4.1-B – 4.5-B (5 tests) | Pair 1, Scenario 2, Scenario 5 |
| 5 | Language Accuracy & Resolution | M1 | Enhance local receiver propagation for method chains and builder patterns in `devmap-resolve` | 5.1 – 5.5 (5 tests) | 5.1-B – 5.5-B (5 tests) | Pair 6, Scenario 4 |
| 6 | Faceted Hydration Pipeline | M2 | Decompose monolithic 7-IPC `loadSnapshot` into FileStatus, BranchGraph, and Metadata facets | 6.1 – 6.5 (5 tests) | 6.1-B – 6.5-B (5 tests) | Pair 1, Scenario 1, Scenario 2 |
| 7 | Per-Repository Indexing Concurrency | M2 | Replace global `LiveBuildSlot` mutex with per-repository build concurrency in `live.rs` | 7.1 – 7.5 (5 tests) | 7.1-B – 7.5-B (5 tests) | Pair 3, Scenario 1, Scenario 2 |
| 8 | Centralized Heartbeat Dispatcher | M2 | Consolidate independent component timers into a single coordinated tick dispatcher | 8.1 – 8.5 (5 tests) | 8.1-B – 8.5-B (5 tests) | Pair 3, Scenario 3 |
| 9 | Ledger Ingestion Condvar Synchronization | M2 | Replace 10ms thread::sleep spin-lock in `src-tauri/src/ingest/mod.rs` with Condvar | 9.1 – 9.5 (5 tests) | 9.1-B – 9.5-B (5 tests) | Pair 4, Scenario 3, Scenario 5 |
| 10 | TaskBoard & FleetView Virtualization | M3 | Virtualize card lists and repo rows using `VirtualList.svelte` to prevent DOM thrashing | 10.1 – 10.5 (5 tests) | 10.1-B – 10.5-B (5 tests) | Pair 4, Scenario 1, Scenario 3 |
| 11 | View DOM Retention on Tab Switch | M3 | Remove `{#key $repoStore.currentPath}` in `ViewSectionPanel.svelte` to preserve view DOM | 11.1 – 11.5 (5 tests) | 11.1-B – 11.5-B (5 tests) | Pair 5, Scenario 1 |
| 12 | Integrate 8 Parked UI Branches | M3 | Merge and verify 57 audited commits from `.claude/worktrees/` (`ui/*` branches) | 12.1 – 12.5 (5 tests) | 12.1-B – 12.5-B (5 tests) | Pair 5 |
| 13 | Task Backlog Review & Priority Drift Fix | M4 | Structured deliverable of 255 tasks, P0 triage, priority drift fix (`ft-7e812b3f`), task board fixes | 13.1 – 13.5 (5 tests) | 13.1-B – 13.5-B (5 tests) | Pair 7, Scenario 3 |
| 14 | E2E Testing Suite (Tiers 1-4) | Test Track | Comprehensive 4-tier opaque-box test suite verifying all requirements | 14.1 – 14.5 (5 tests) | 14.1-B – 14.5-B (5 tests) | Pair 7, Infrastructure |
| 15 | Final Verification & Adversarial Hardening | M5 | 100% E2E test pass, write latency benchmark verification, and Tier 5 adversarial hardening | 15.1 – 15.5 (5 tests) | 15.1-B – 15.5-B (5 tests) | Pair 8, Scenario 5, Tier 5 |

---

## 3. Test Architecture & Directory Layout

### Execution Framework
- **Test Runner**: Vitest 5.0.5 via Bun (`bun x vitest`) or Node (`npx vitest`) with custom configuration `vitest.e2e.config.ts`.
- **Fast Shell Runner**: `./scripts/run-e2e.sh` for one-command test invocation.
- **Backend Rust Verification**: `cargo test` in `src-tauri` and `DevCouncil/rust`.

### Directory Layout
```
/Users/bharath/Code/devtools/GitPulse/
├── vitest.e2e.config.ts                       # Dedicated Vitest E2E configuration
├── scripts/
│   ├── run-e2e.sh                             # Executable test runner script
│   └── check-schema-parity.mjs                # Schema parity inspection oracle
└── tests/
    └── e2e/
        ├── helpers/
        │   └── test-utils.ts                  # Shared temp dir manager, git fixtures, CLI runner, oracles
        ├── tier1-feature-coverage.test.ts     # Tier 1: 75 feature coverage tests (5 per feature)
        ├── tier2-boundary-corner.test.ts      # Tier 2: 75 boundary & corner case tests (5 per feature)
        ├── tier3-cross-feature.test.ts        # Tier 3: 8 pairwise cross-feature interaction tests
        ├── tier4-application-scenarios.test.ts# Tier 4: 5 realistic end-to-end workflow scenarios
        └── tier5-adversarial-hardening.test.ts# Tier 5: 5 fault-injection & adversarial hardening tests
```

---

## 4. Tier 4 Real-World Application Scenarios

The Tier 4 suite simulates full user and system workflows across the 5 system lanes:

### Scenario 1: Multi-Repository Fleet Navigation & Live Editing
- **Lanes Exercised**: Lane 1 (Active Interactive UI) + Lane 2 (Reactive Git Monitoring) + Lane 3 (DevMap Indexing).
- **Workflow**: User navigates 3 repositories within a 1,000-repository fleet. Modifies a file in Repo 1 (triggering single-command `FileStatusFacet`). While background indexing starts on Repo 1, user immediately switches to Repo 2, scrolls the diff view, and triggers indexing on Repo 2. The per-repository build coordinator enables both repositories to index concurrently. Switching back to Repo 1 demonstrates complete DOM retention: scroll offsets, expanded file tree directories, and active diff selections are perfectly preserved.

### Scenario 2: High-Velocity Commits & Branch Switching Under DevMap Indexing
- **Lanes Exercised**: Lane 2 (Git Monitoring) + Lane 3 (Background Code Intel) + Lane 4 (Ledger Ingest).
- **Workflow**: Simulates a high-frequency developer commit stream (5 rapid git commits). File status is tracked incrementally. Simultaneously, live indexing commits generations to SQLite WAL at Schema 29. Branch switching triggers `BranchGraphFacet` independently in <10ms without waiting for status probes or blocking the write transaction.

### Scenario 3: Large TaskBoard Triage and Evidence-Gated Completion Under Active Ingest
- **Lanes Exercised**: Lane 4 (Ledger & Ingest) + Lane 5 (Workbench & Tasks) + Lane 1 (UI Virtualization).
- **Workflow**: Loads a realistic backlog of 2,000 tasks on TaskBoard during active background ledger ingestion. VirtualList bounds DOM nodes to <= 40 items, ensuring zero frame rate drop. Task priority drift is reconciled into normalized P0–P3 ranks. An agent attempting to complete an unverified task is retained in `review` under evidence-gated rules until `work.verify.task` returns `passed`.

### Scenario 4: Cold Repository Discovery, Schema 29 Sync, and Manifest Generation
- **Lanes Exercised**: Lane 3 (DevMap Storage & Resolver) + Upstream DevCouncil Seam.
- **Workflow**: A fresh repository is imported. Schema parity is validated across `devmap-store`, `dc-store`, `workbench`, and `devmap-extract`. Routine builds execute without writing 37MB `code_graph.json`. Upon user request, `--manifest` generates `repo_map.json` and `code_graph.json` on demand, correctly resolving multi-hop method chains and builder patterns.

### Scenario 5: Adversarial Multi-Lane Failure Injection & Recovery
- **Lanes Exercised**: All 5 Lanes under fault injection.
- **Workflow**: Simulates simultaneous corrupt zstd extraction payloads, hung git status subprocesses, ledger lock contention, and worker panics. Proves that decoupled error isolation routes errors to the diagnostics panel without process crashes, Condvar ingestion locks recover cleanly on timeout without permanent deadlock, and SQLite WAL transactions roll back cleanly.

---

## 5. Coverage Thresholds & Acceptance Gates

To guarantee production readiness and prevent regressions, all test runs must satisfy these strict gates:

1. **Test Pass Floor**: 100% pass rate (168 / 168 tests must pass). Zero failing tests permitted.
2. **Feature Coverage Floor**: Minimum 5 test cases per feature in Tier 1 and 5 boundary/corner cases in Tier 2 across all 15 features.
3. **Execution Latency Floor**:
   - Total E2E suite execution time under 5.0 seconds (currently completing in **1.17 seconds**).
   - Live refresh IPC response time < 50ms (decoupled from status probes).
   - Write transaction duration < 50ms.
4. **Benchmark Integrity**: No external libraries imported; all tests run hermetically on local macOS workspace.
