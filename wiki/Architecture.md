# Architecture

GitPulse is a Tauri 2 desktop app: **Rust owns privileged work**, **Svelte 5 owns rendering**, and they meet at one machine-checked seam: `invoke("cmd_*")`.

**Product stack.** DevCouncil is components and modules. Manvi wraps them. GitPulse uses Manvi (`manvi serve`) for policy, workbench, and agent hosting, and selected DevCouncil crates plus the `devmap` CLI for code intelligence. See [Module integration](https://github.com/bharathvbcr/GitPulse/blob/main/docs/MODULE_INTEGRATION.md).

```mermaid
flowchart TB
    subgraph Frontend["Svelte 5 + TypeScript"]
        direction TB
        Views["4 Views · 18 Sections<br/>(Work, Code, History, Insights)"]
        Canvas["GPU Canvas Graph"]
        Dock["Terminal Dock (portable-pty)"]
        Stores["Domain Stores & SWR Cache"]
        Guards["Async Cancellation Guards"]
        Views --> Canvas
        Views --> Dock
        Views --> Stores --> Guards
    end

    subgraph IPC["Tauri 2 IPC"]
        Bridge["invoke('cmd_*') — 291 handlers, check:ipc"]
    end

    subgraph Backend["Rust Backend (Tauri 2 / Rayon)"]
        direction TB
        Git["Git Sandbox & CLI Reader"]
        Graph["Topological Lane Solver"]
        Analyzers["Coverage, Health & LOC Analyzers"]
        Secrets["Secrets Scanner (Kingfisher)"]
        AI["AI Phrasing & Commit Brief"]
        Ledger["Event Ledger WAL"]
        MCP["gitpulse-mcp & gitpulsed"]
        DevMapSubsys["DevMap Query & Gate Subsystem"]
    end

    subgraph Integrations["Modular Sidecars & Local Tools"]
        direction TB
        subgraph ManviBox["Manvi Harness"]
            Manvi["manvi serve --posture host<br/>(5-verdict policy ladder & dc-store workbench)"]
        end
        subgraph DevCouncilBox["DevCouncil & DevMap"]
            Vendored["Vendored Readers (devmap-store, devmap-query)"]
            DevMapCLI["devmap CLI & daemon watcher socket"]
        end
        subgraph LocalBox["Local Host"]
            LocalTools["git CLI · gh CLI · loopback LLMs"]
        end
    end

    Guards --> Bridge
    Bridge --> Backend
    Backend --> ManviBox
    Backend --> DevCouncilBox
    Backend --> LocalBox
```

The 291-handler count is the registered `tauri::generate_handler!` list in `src-tauri/src/lib.rs`, enforced by `bun run check:ipc`.

## Layout

```
GitPulse/
├── src/                  Svelte 5 + TypeScript
│   ├── lib/stores/       repo, graph, filter, theme, …
│   ├── lib/components/   UI
│   ├── lib/canvas/       commit-graph renderer
│   ├── lib/views/        routerless view registry (4 views)
│   └── lib/<domain>/     pure logic: files, diff, coverage, health, …
└── src-tauri/src/        Rust core
    ├── commands/         the only IPC entry points
    ├── engine/           git sandbox, worktrees, routes, hooks, CoW sync
    ├── graph/            lanes, mainline pin, filter simplification
    ├── analyzer/         languages, LOC, coverage, deps, supply chain
    ├── secrets/          Kingfisher secrets scanner
    ├── ai/               commit briefs & on-device phrasing
    ├── harness/          MANVI policy gate
    ├── mcp/              MCP surface (reads, plus the task-board tools)
    ├── ledger/           durable WAL
    └── bin/              gitpulsed, gitpulse-mcp, gitpulse-hook, gitpulse-ptyd
```

There is no virtual-DOM router. View ids are the `ViewTab` union in `src/lib/repos/persist.ts`, registered in `src/lib/views/viewRegistry.ts`, rendered from `App.svelte`. Adding a view means changing all three.

Errors across IPC carry strongly typed `CommandError` instances directly from runners to the command edge, eliminating ambient thread-local error state. For PTY management under the `gitpulse-ptyd` backend, `Host::knows` evaluates session membership directly from client-side vouched snapshots, eliminating IPC round-trip overhead. Background-class processes on macOS enter Darwin's background band in the child process before `exec` (`procguard::spawn_background`), while the frontend terminal holds recently hidden WebGL contexts and enforces DEC 2026 synchronized output boundaries. In the workbench, the verification sweep driver batches claimed card queries via `latest_many` to minimize background polling overhead.

## Why Fleet is not a view

A `ViewTab` is stored on the **active repository's** session and its panel content is keyed on `currentPath` (in `ViewSectionPanel`). Fleet answers a question about the **workspace**. If it were a view it would be scoped wrong and destroyed on every repo switch — taking the live terminal PTY with it.

Fleet is swapped by **hiding, never unmounting**. Open state is a UI preference, not part of the persisted workspace blob.

## DevCouncil, Manvi, and DevMap integration

DevCouncil supplies components and modules, Manvi wraps them, and GitPulse integrates them through in-process readers, the CLI, and a background sidecar.

```mermaid
flowchart LR
    subgraph DC["DevCouncil (Modules)"]
        DCLib["Vendored crates<br/>(devmap-*, dc-*)"]
        DCCLI["devmap CLI / serve"]
    end
    subgraph MV["Manvi (Wrap)"]
        MVSidecar["manvi serve (NDJSON)"]
        MVPolicy["Policy engine"]
        MVWorkbench["dc-store wrap"]
        MVPolicy --> MVSidecar
        MVWorkbench --> MVSidecar
    end
    subgraph GP["GitPulse (Host)"]
        Rust["Rust core (Tauri 2)"]
        Gate["devmap gate & probe"]
        Client["Harness client"]
        UI["Svelte 5 frontend"]
        Gate --> Rust
        Client --> Rust
        Rust --> UI
    end

    DCLib --> Rust
    DCCLI <--> Gate
    MVSidecar <--> Client
```

## Headless binaries

| Binary | Transport | Role |
| --- | --- | --- |
| `gitpulse-mcp` | MCP JSON-RPC over stdio | **Read** the control plane. Never mutates git; its only writes are the task-board tools. |
| `gitpulsed` | NDJSON, interval loop | **Write** attribution catch-up into the ledger when the GUI is closed. Serves no requests and never takes a lease. |
| `gitpulse-hook` | Host hook JSON on stdin/stdout | Agent hook dispatcher; failures report no decision and leave host permissions in control. |
| `gitpulse-ptyd` | Unix socket to the app | Optional terminal host: holds terminal sessions when "Keep terminals running when GitPulse quits" is on. See [Terminal host](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TERMINAL_HOST.md). |

```sh
gitpulsed --interval 300 /path/to/repo
```

Catch-up is idempotent against a ledger watermark; interrupting a cycle is safe.

Each pass leaves its outcome in the ledger's `catch_up_state` table (one row per worktree), which is how a read-only process such as `gitpulse-mcp` reports `last_catch_up`, `stale` and what the pass covered. The transcript walk reads up to five directories below the root (the `slug/session/subagents/workflows/wf_*/` layout) and 5000 files; anything it sees past those bounds is counted as `transcripts_unlisted` (`unlisted_depth`, `unlisted_cap`), marks the pass `truncated`, and keeps the walk from forgetting any transcript offset.

## Profile tasks and workspaces

Global, saved-workspace and repository boards share Manvi-owned task records.
`cmd_workbench_request` bounds requests and runs storage off the UI thread; the
`dc-store` workbench API owns schema, revisions, membership and receipts. These
records are separate from read-only repository execution tasks and leases.

Saved briefs bind revisions. Task editor copying distinguishes saved records from
new unsaved drafts, and Manvi title/description proposals need selected acceptance.
Terminal handoffs and managed runs retain separate permission and lifecycle
contracts. See [Tasks and workspaces](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TASKS_AND_WORKSPACES.md).

## Vendored crates

DevCouncil (`dc-*`, `devmap-*`) and MarkDev (`markdev`) crates are copied into `src-tauri/vendored/` so a lone GitPulse checkout builds. Manvi is the wrap around those components at runtime (`manvi serve`), not a vendor origin. Do not edit those copies. Fix upstream, then `bun run vendor`. `bun run vendor:check` reports an upstream that is not checked out as **unavailable**, never **matches**. `bun run check:vendor-schema` pins the vendored store schema against the installed `devmap` CLI when present.

## Contracts

The IPC, serde field, version-manifest, coverage-floor, and a large set of drift tests under `scripts/` exist because types alone cannot see a renamed event, a gate that does not wrap a writer, or a count in the docs that no longer matches the code.

Canonical write-up: [docs/ARCHITECTURE.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/ARCHITECTURE.md) and [[Development]].
