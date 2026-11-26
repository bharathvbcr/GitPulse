# Waylode Philosophy

## 1. Thesis

**A workspace, not a chat window.**

*Waylode is where you work. Manvi runs the agent. DevCouncil knows the code and checks the result.*

Most AI coding tools center on a single chat transcript and leave the developer to track which branch an agent edited, whether two sessions are colliding on the same file, or why a background compile just saturated the machine. Waylode organizes around the work itself—repositories, worktrees, tasks, live sessions, the code graph, and host load—on top of a native Git client.

## 2. Core Principles

1. **Human-agent information symmetry.**
   Waylode exposes the same repository state, worktree inventory, collision signals, task board, and DevMap queries to the developer in the desktop UI and to coding agents over `waylode-mcp` (`src-tauri/src/bin/waylode-mcp.rs`). Agents do not read a separate shadow index or guess from terminal scrollback: read-only MCP calls (`readOnlyHint: true`) inspect the same `live_sessions.rs`, `entity_collision.rs`, `codeintel`, and ledger facts shown on screen.
2. **Observe heterogeneous agents without vendor lock-in.**
   Claude Code, Codex, Grok, Antigravity, Manvi, and plain shell sessions run as their own CLIs under their own accounts and permission rules. Waylode detects live sessions by executable and launch environment (`WAYLODE_SESSION_ID` / `WAYLODE_RUN_ID`, with legacy `GITPULSE_*` fallbacks) in `src-tauri/src/sysload/procscan.rs`, tracks them across panes in `src-tauri/src/terminal/` and `src-tauri/src/mavi/`, and supervises sessions it did not start instead of forcing every workflow into one proprietary runner.
3. **Prevent collisions and damage before merge.**
   Concurrent work gets its own isolated checkout under `.gitpulse/worktrees/<task>-<id>` (`src-tauri/src/engine/worktree.rs`, `cow_clone.rs`), `<git-common-dir>/gitpulse/lanes/` intent claims serialized via `flock(2)` (`src-tauri/src/engine/agent_bus.rs`), and three-layer collision radar across dirty files, DevMap one-hop neighbors, and quarantined `git merge-tree`. Allowed agent Git mutations record undo snapshots under `refs/gitpulse/ops/` (`git_writer.rs`), optional turn checkpoints live under `refs/gitpulse/turns/` (`turn_checkpoint.rs`), and in-process secret scanning (`leak_scan`) + explicit policy verdicts (`harness::guard_command`) gate destructive actions before they land.
4. **Treat host resources as a first-class concern.**
   Running multiple agent build trees on one workstation fails when every subshell competes for memory compressors and Cargo/Go slots. Waylode's desktop sampler (`src-tauri/src/sysload/snapshot.rs`) publishes an `816`-byte lock-free `host-load.v1` snapshot that `waylode-hook`, `waylode-mcp`, and POSIX record-locked build leases (`src-tauri/src/sysload/lease.rs`, `src-tauri/src/hooks/lease_wrapper.rs`) read without polling the UI, while `sysload::relief` slows or pauses runaway subtrees without killing processes solely for footprint.
5. **Fail closed and name every incomplete check.**
   A missing `devmap` index, an absent `manvi` sidecar (`Unchecked`), a shell/worktree scan cut short by caps, or a declined Mavi `AskUserQuestion` card (`QUESTION_DECLINED` + `AGENT_GUIDANCE`) never renders as `0` or `Allowed`. `waylode-terminal` spawn rules (`src-tauri/src/terminal/orchestrate.rs`) refuse `bypass` permission mode and unsafe settings files, `port_probe.rs` bounds descendant port discovery at `SHELL_PROCESS_NAMES`, and `waylode-ptyd` protocol `5` (`src-tauri/src/terminal/host/proto.rs`) separates live `needs_input` prompt signals from silent idle shells.
6. **Zero local paywalls.**
   Everything Waylode does on your machine, and on compute you bring (SSH runners, `cloud_vms`, your own model keys), is free and needs no sign-in.
7. **Native local-first execution without bundled browser weight.**
   Waylode is a Rust + Tauri 2 desktop binary using the OS webview (`WKWebView` / WebKitGTK / WebView2) and a Svelte 5 frontend (`src/lib/views/viewRegistry.ts`, `brand/waylode-mark.json`) with no Waylode cloud account or telemetry server in Primary. Repository metadata stays local; external AI/GitHub checks use explicit user-configured local tools (`gh`, loopback LLM endpoints, or Manvi) and keep `.gitpulse/` disk/config identifiers and `WAYLODE_MCP_EXPOSE_LEGACY=1` aliases intact so existing checkouts upgrade cleanly (`scripts/brand-rename-contract.test.ts`).

## 3. Deliberate Trade-offs & What We Refuse to Do

- **We refuse to become an autonomous agent harness or turn loop.**
  Multi-turn LLM orchestration, model routing, and runtime policy enforcement live in **Manvi**, driven over `manvi serve` NDJSON with `MANVI_HARNESS_INIT_ENABLED=false` (`src-tauri/src/harness/sidecar.rs`) so inspecting or opening a repository never mutates user files or `.gitignore`.
- **We refuse to become a parallel code-graph engine or forked store.**
  Waylode embeds DevCouncil crates read-only in `src-tauri/vendored/` (`VENDOR.json`), exposes the `11`-tool `devmap_*` stdio subset (`src-tauri/src/mcp/devmap_parity.rs`) without pulling `devmap-serve`'s HTTP dependencies, and refuses legacy `base_store_ref.json` overlays in favor of upstream `StackedStore` (`src-tauri/src/codeintel/worktree_overlay.rs`). Local deviations are rare, explicitly scoped, and verified—such as `src-tauri/src/engine/git_cli/preflight.rs::parse_git_version` for fail-closed Git capability checks.
- **We refuse to hide uncertainty or silently rewrite tracked repository files.**
  `cmd_devcouncil_init` writes only `.git/info/exclude` and DevMap state; repo-tracked agent guides (`AGENTS.md`, `CLAUDE.md`, `.mcp.json`) require explicit preview and confirmation (`cmd_devmap_integrate`), and schema/contract parity is enforced mechanically against upstream (`scripts/check-schema-parity.mjs`, `contracts/*`).

## 4. How This Fits the Stack

- **Master cross-repo boundaries**: `DevCouncil/specs/003-stack-boundaries-and-non-overlap` ([specification](https://github.com/bharathvbcr/DevCouncil/blob/main/specs/003-stack-boundaries-and-non-overlap/spec.md)) and [Waylode Specifications](../specs/README.md) (`001` ADE & worktrees, `002` host integration & seams).
- **Sibling philosophies**:
  - **DevCouncil** (*Deterministic truth and modular engines before the edit*): [DevCouncil/docs/PHILOSOPHY.md](https://github.com/bharathvbcr/DevCouncil/blob/main/docs/PHILOSOPHY.md)
  - **Manvi** (*An embeddable, local-model-first agent harness under explicit policy*): [Manvi/docs/PHILOSOPHY.md](https://github.com/bharathvbcr/Manvi/blob/main/docs/PHILOSOPHY.md)
- **Related architecture guides**: [Architecture](ARCHITECTURE.md) · [Module integration](MODULE_INTEGRATION.md) · [Why Waylode](WHY_WAYLODE.md)
