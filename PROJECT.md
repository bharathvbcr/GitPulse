# Project: GitPulse & DevMap Performance, Workflows, UI/UX, and Tasks Initiative

## Architecture
GitPulse is an intelligent, high-performance developer workbench built on Svelte 5 / TypeScript and a Tauri v2 / Rust backend. It uses Manvi for policy, workbench tasks, and agent hosting, and DevCouncil components (`devmap` CLI, vendored `devmap-*` crates, verification reads) for code intelligence.

This project delivers end-to-end performance optimizations, storage layer efficiency, language resolution accuracy, multi-lane workflow separation, UI/UX virtualization and feature integration, and a comprehensive task backlog review.

### System Decoupling Architecture (5 Workflow Lanes)
1. **Lane 1: Active Interactive Lane (UI / User Input)**
   - Uncapped, prioritized dispatch for user clicks, keyboard navigation, and terminal I/O.
   - Svelte 5 state-driven reactive DOM retention without destructive `{#key}` tearing down active view sections.
2. **Lane 2: Reactive Git Monitoring Lane**
   - Decomposes monolithic 7-IPC `loadSnapshot` bursts into 3 independent facets (`FileStatusFacet`, `BranchGraphFacet`, `MetadataFacet`).
   - Limits git child process spawning on typical edits to 1 command, preventing `SpawnGate` token depletion.
3. **Lane 3: Background Code Intelligence Lane (DevMap / Vault)**
   - Upstream DevCouncil and GitPulse synchronized to schema 29.
   - Decoupled live refresh IPC from status probes (`wave/devmap-ipc-stall`).
   - Per-repository build concurrency replacing the global `LiveBuildSlot` singleton.
   - Elimination of the $O(N)$ full-view JSON deserialization in `devmap-store/src/db/write.rs`.
4. **Lane 4: Ledger & Ingest Lane**
   - Non-polling `Condvar` synchronization in `ingest/mod.rs` replacing 10ms thread sleeping spin-locks.
   - Batched SQLite WAL transactions for reflog and transcript events.
5. **Lane 5: Workbench & Task Processing Lane**
   - Centralized adaptive heartbeat dispatcher batching component polling.
   - Evidence-gated task completion tracking and priority scale reconciliation (`ft-7e812b3f`).

---

## Feature Inventory
Every feature from the Step 0 Survey is inventoried and mapped to an assigned milestone:

| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| 1 | Schema 29 Parity Sync | Sync DevCouncil schema 29 to GitPulse vendored crates, PATH binaries, and pin files | M1 | Survey (Explorer 1) |
| 2 | Storage Write Latency Optimization | Eliminate $O(N)$ JSON deserialization in `write.rs:2407`, batch FTS5 inserts, optimize unresolved calls | M1 | Survey (Explorer 1) |
| 3 | Manifest & Code Graph I/O Optimization | Avoid redundant 37+ MB `code_graph.json` disk rewrites on routine watcher ticks; generate on demand | M1 | Survey (Explorer 1) |
| 4 | DevMap Live Refresh Decoupling | Decouple devmap live-refresh IPC from status probes (`wave/devmap-ipc-stall`) | M1 | Survey (Explorer 1 & 3) |
| 5 | Language Accuracy & Resolution | Enhance local receiver propagation for method chains and builder patterns in `devmap-resolve` | M1 | Survey (Explorer 1) |
| 6 | Faceted Hydration Pipeline | Decompose monolithic 7-IPC `loadSnapshot` into FileStatus, BranchGraph, and Metadata facets | M2 | Survey (Explorer 2) |
| 7 | Per-Repository Indexing Concurrency | Replace global `LiveBuildSlot` mutex with per-repository build concurrency in `src-tauri/src/devmap/live.rs` | M2 | Survey (Explorer 2) |
| 8 | Centralized Heartbeat Dispatcher | Consolidate independent component timers into a single coordinated tick dispatcher | M2 | Survey (Explorer 2) |
| 9 | Ledger Ingestion Condvar Synchronization | Replace 10ms thread::sleep spin-lock in `src-tauri/src/ingest/mod.rs` with Condvar | M2 | Survey (Explorer 2) |
| 10 | TaskBoard & FleetView Virtualization | Virtualize card lists and repo rows using `VirtualList.svelte` to prevent DOM thrashing | M3 | Survey (Explorer 3) |
| 11 | View DOM Retention on Tab Switch | Remove `{#key $repoStore.currentPath}` in `ViewSectionPanel.svelte` to preserve view DOM | M3 | Survey (Explorer 2 & 3) |
| 12 | Integrate 8 Parked UI Branches | Merge and verify 57 audited commits from `.claude/worktrees/` (`ui/*` branches) | M3 | Survey (Explorer 3) |
| 13 | Task Backlog Review & Priority Drift Fix | Structured deliverable of 255 tasks, P0 triage, priority drift fix (`ft-7e812b3f`), task board fixes | M4 | Survey (Explorer 3) |
| 14 | E2E Testing Suite (Tiers 1-4) | Comprehensive 4-tier opaque-box test suite verifying all requirements | Test Track | Dual Track |
| 15 | Final Verification & Adversarial Hardening | 100% E2E test pass, write latency benchmark verification, and Tier 5 adversarial hardening | M5 | Dual Track |

---

## Milestones

| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| 1 | `devmap_core` | DevMap performance, storage write optimization, schema 29 sync, IPC stall decoupling, language accuracy | none | DONE |
| 2 | `workflow_separation` | GitPulse 5-lane workflow separation: faceted hydration, per-repo indexing, Condvar sync, heartbeat dispatcher | M1 | DONE |
| 3 | `ui_ux_features` | TaskBoard/FleetView virtualization, ViewSectionPanel DOM retention, 8 parked UI branches integration (57 commits) | M2 | PLANNED |
| 4 | `task_backlog_review` | Structured review of 255 tasks, 5 P0 triage, review column congestion analysis, priority drift fix (`ft-7e812b3f`) | none | PLANNED |
| 5 | `final_verification` | Pass 100% of E2E test suite (Tiers 1-4), benchmark latency verification, Tier 5 adversarial coverage hardening | M1, M2, M3, M4, Test Track | PLANNED |

---

## Interface Contracts

### DevCouncil ↔ GitPulse (devmap Storage & Schema 29)
- **Database Schema**: Schema version 29.
  - `file_payloads.extraction_json`: Compressed `BLOB` using zstd compression.
  - `generation_files.entry_wiring_json`: JSON string encoding entry point wiring.
  - Reader minimum schema version: 29. Both DevCouncil and GitPulse must compile against `CURRENT_SCHEMA_VERSION = 29`.
- **CLI Commands**:
  - `devmap build [--manifest] [--since <rev>]`: Exit code 0, writes to SQLite WAL database.
  - `devmap status --json`: Returns JSON status object including schema version, generation id, and file counts.

### GitPulse Backend ↔ Frontend (Workflow IPC Contracts)
- **Faceted Hydration IPC**:
  - `cmd_get_status({ repoPath })`: Returns `FileStatus[]`. Used by `FileStatusFacet`.
  - `cmd_list_branches({ repoPath })`: Returns `BranchInfo[]`. Used by `BranchGraphFacet`.
  - `cmd_list_tags({ repoPath })`, `cmd_stash_list({ repoPath })`: Returns metadata. Used by `MetadataFacet`.
- **Live Index IPC**:
  - `cmd_devmap_maybe_refresh({ repoPath, force })`: Asynchronous, non-blocking trigger. Does not hang on status probes.

---

## Code Layout

### DevCouncil Core Surfaces
- `rust/devmap-store/src/db/write.rs`: SQLite write transactions, payload persistence, FTS5 updates, generation commits.
- `rust/devmap-store/src/schema.rs`: Schema definitions, migrations, schema version constants.
- `rust/devmap-resolve/src/resolver/`: Cross-file reference binding, receiver type propagation.
- `rust/devmap-cli/src/`: Command-line interface binaries.

### GitPulse Surfaces
- `src-tauri/vendored/`: Vendored DevCouncil crates (`devmap-store`, `devmap-extract`, `devmap-resolve`, `devmap-analyze`, `devmap-query`).
- `src-tauri/src/devmap/`: `live.rs` (live indexing coordinator, `LiveBuildSlot`), `cli.rs` (subprocess runner).
- `src-tauri/src/engine/git_cli.rs`: `SpawnGate`, process admission classes, token bucket rate limiter.
- `src-tauri/src/ingest/mod.rs`: Ledger ingestion, `serialize()` serialization lock.
- `src/lib/stores/repoStore.ts`: Repository state store, hydration logic, event handling.
- `src/lib/components/TaskBoard.svelte`: Task board component, card lists, search filter.
- `src/lib/components/FleetView.svelte`: Multi-repository fleet view, repo table rows.
- `src/lib/components/ViewSectionPanel.svelte`: Leaf section container, tab switching wrapper.
