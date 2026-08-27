# GitPulse Architecture

GitPulse is built as a high-performance, local-first native desktop client combining a **Rust backend (Tauri 2)** with a **Svelte 5 + TypeScript frontend**.

```mermaid
flowchart TB
    subgraph Frontend["Svelte 5 + TypeScript Frontend"]
        direction TB
        UI["Views & Components<br/><code>src/lib/components/</code>"]
        Stores["State & Mutation Stores<br/><code>src/lib/stores/</code>"]
        Registry["View Registry & Routerless Nav<br/><code>src/lib/views/</code>"]
        Canvas["GPU-Accelerated Canvas<br/><code>src/lib/canvas/</code>"]
        Async["Async Guards & Debounce<br/><code>src/lib/async/</code>"]
        
        UI --> Stores
        UI --> Canvas
        Stores --> Async
        Registry --> UI
    end

    subgraph IPC["Tauri 2 IPC Seam (snake_case ↔ camelCase)"]
        direction TB
        Invoke["<code>invoke('cmd_*', args)</code>"]
        ContractCheck["Contract Enforced by <code>check:ipc</code>"]
        Invoke -.-> ContractCheck
    end

    subgraph Backend["Rust Backend (Tauri 2 / Tokio / Rayon)"]
        direction TB
        CmdRegistry["Command Registry (94 Handlers)<br/><code>src-tauri/src/commands/</code>"]
        
        subgraph Subsystems["Core Subsystems"]
            GitEngine["Git Engine & Sandbox<br/><code>src-tauri/src/engine/</code>"]
            GraphSolver["Graph Solver & Nogap Bounds<br/><code>src-tauri/src/graph/</code>"]
            Analyzers["Analyzers (LOC, Coverage, Health)<br/><code>src-tauri/src/analyzer/</code>"]
            StorageAuditor["Storage Auditor & History<br/><code>src-tauri/src/storage/</code>"]
            OpsPlanner["Ops Planner & Releases<br/><code>src-tauri/src/ops.rs</code>"]
            PtyTerminal["PTY Lifecycle & Terminal<br/><code>src-tauri/src/terminal/</code>"]
        end
        
        CmdRegistry --> Subsystems
    end

    subgraph External["Local System & Sidecars"]
        direction TB
        LocalGit["<code>git</code> CLI"]
        LocalGh["<code>gh</code> CLI (GitHub)"]
        LocalLLM["Local LLMs (Ollama / LM Studio)"]
        ManviSidecar["MANVI Harness Sidecar<br/>(<code>manvi serve</code> via stdio)"]
    end

    Async --> Invoke
    Invoke --> CmdRegistry
    GitEngine --> LocalGit
    Analyzers --> LocalGit
    Analyzers --> LocalGh
    Analyzers --> LocalLLM
    OpsPlanner --> LocalGh
    OpsPlanner --> ManviSidecar
    Subsystems --> ManviSidecar
```

---

## 1. Frontend Architecture

### Routerless View Architecture
GitPulse does not use a virtual DOM router. Views are members of the `ViewTab` union defined in [`src/lib/repos/persist.ts`](file:///Users/bharath/Code/devtools/gitpulse/src/lib/repos/persist.ts).

```mermaid
flowchart LR
    Persist["<code>ViewTab</code> Union<br/><code>persist.ts</code>"] --> ViewRegistry["<code>VIEW_REGISTRY</code> Record<br/><code>viewRegistry.ts</code>"]
    ViewRegistry --> HeaderTabs["Header Tab Bar<br/><code>ViewTabBar.svelte</code>"]
    ViewRegistry --> NativeMenu["OS Native Menu<br/><code>src/lib/desktop/</code>"]
    ViewRegistry --> CommandPalette["Command Palette<br/><code>CommandPalette.svelte</code>"]
    ViewRegistry --> AppRender["Render Branch<br/><code>App.svelte</code>"]
```

Every view is registered in [`src/lib/views/viewRegistry.ts`](file:///Users/bharath/Code/devtools/gitpulse/src/lib/views/viewRegistry.ts). TypeScript enforces that adding a view requires:
1. Adding the identifier to the `ViewTab` union in `persist.ts`.
2. Adding its metadata to `VIEW_REGISTRY` in `viewRegistry.ts`.
3. Adding the render branch in `App.svelte`.

### Svelte 5 Runes & Dependency Injection
- **Component State**: Uses modern Svelte 5 runes (`$state`, `$derived`, `$effect`) for local, reactive component state.
- **Store Architecture**: Domain stores (e.g. `repoStore`, `graphStore`, `filterStore`, `harnessStore`) are instantiated using factory functions with injectable dependencies (`createRepoStore(deps)`), enabling 100% headless unit testing without requiring Tauri runtime mocks.

### Async Hygiene & Cancellation Guards
When switching between repositories or triggering fast refilters, in-flight IPC calls could return out of order. GitPulse guards asynchronous calls using `createAsyncGuard` ([`src/lib/async/guard.ts`](file:///Users/bharath/Code/devtools/gitpulse/src/lib/async/guard.ts)). When a repository changes or a new query starts, pending promises from prior invocations are automatically invalidated and dropped.

---

## 2. Rust Backend & Subsystems

```mermaid
classDiagram
    class CommandRegistry {
        +94 Registered Handlers
        +Checked by scripts/check-ipc-contract.mjs
    }
    class GitEngine {
        +validate_repo()
        +git_text()
        +GitReader
    }
    class GraphSolver {
        +solve_lanes()
        +nogap_bounds()
        +avatar_resolution()
    }
    class Analyzers {
        +detect_languages()
        +scan_coverage()
        +audit_dependencies()
    }
    class StorageAuditor {
        +audit_repository()
        +track_snapshots()
    }
    class OpsPlanner {
        +plan_merged_branch_cleanup()
        +review_outgoing_commits()
        +plan_release_publish()
    }
    class HarnessSidecar {
        +guard_command()
        +guard_file()
        +probe_model()
        +prepare_prompt()
        +settle_reply()
    }

    CommandRegistry --> GitEngine
    CommandRegistry --> GraphSolver
    CommandRegistry --> Analyzers
    CommandRegistry --> StorageAuditor
    CommandRegistry --> OpsPlanner
    CommandRegistry --> HarnessSidecar
```

### Subsystem Responsibilities
- **`engine/`**: Git execution sandbox, output parsers, safe diff generation, blame readers, and repository status pollers.
- **`graph/`**: Native commit-history lane solver with branch folding, parent-child edge layout, and nogap lookback bounds.
- **`analyzer/`**: 
  - `language.rs`: Multi-language classifier (60+ languages), GitHub Linguist color mappings, and fast line-of-code breakdown.
  - `coverage.rs`: Universal coverage artifact scanner (LCOV, Cobertura, Go cover, Istanbul, JaCoCo, Clover) and file-level metrics.
  - `health.rs`: Ecosystem vulnerability checkers (`npm audit`, `cargo-audit`, `pip-audit`, `govulncheck`, `composer audit`, `bundler-audit`, GitHub Dependabot).
- **`storage/`**: Deep disk-usage auditor (packfiles, loose objects, reflogs, LFS, submodules, caches, oversized files) with time-series history tracking.
- **`ops.rs`**: Safe, read-only MANVI operation planners for merged branch cleanups, outgoing commit review, and release publishing.
- **`harness/`**: Sidecar client managing policy gates and local model communication via NDJSON stdio.

---

## 3. High-Performance Commit Graph Renderer

The commit graph utilizes a GPU-accelerated HTML5 Canvas with custom paint scheduling:

```mermaid
sequenceDiagram
    participant Git as Rust GitReader
    participant Solver as Rust Graph Solver
    participant Store as Svelte GraphStore
    participant Canvas as Canvas GraphRenderer
    participant GPU as WebGL/Canvas2D Context

    Git->>Solver: Raw commit log & parents
    Solver->>Solver: Solve topological lanes & nogap bounds
    Solver->>Store: Structured GraphPayload (commits, lanes, refs)
    Store->>Canvas: Virtual window viewport (visible rows + buffer)
    Canvas->>GPU: Draw curved branch lanes & rail connectors
    Canvas->>GPU: Render commit nodes & author avatars
    Canvas->>GPU: Paint branch/tag ref badges
```

- **Topological Lane Solver**: Runs natively in Rust using Rayon for parallel traversal when loading large commit histories.
- **Nogap Lookback Bounds**: Prevents disconnected lane lines across virtualized scrolling regions.
- **Author Avatars**: Fast on-canvas rendering with caching for author initials, identicons, and GitHub avatars.
- **Frame Scheduling**: Renders at 60/120 FPS using requestAnimationFrame batches, avoiding UI thrashing during rapid kinetic scrolling.

---

## 4. Strict IPC & Type Contracts

GitPulse enforces compile-time and pre-commit contract safety across the Rust/TypeScript boundary:

| Contract Tool | Command | Description |
| --- | --- | --- |
| **IPC Checker** | `npm run check:ipc` | Verifies all 94 Rust `cmd_*` handlers match frontend `invoke()` calls with zero untracked orphans. |
| **Type Sync Checker** | `npm run check:types` | Asserts Rust Serde structs match TypeScript interfaces field-for-field across 47+ data fields. |
| **Release Version Gate** | `npm run check:release` | Validates that `package.json`, `package-lock.json`, `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock` agree. |
