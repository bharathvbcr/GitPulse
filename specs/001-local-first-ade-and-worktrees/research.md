# Research & Design Rationale: 001 - Local-First ADE, Git Client & Worktree Orchestration

## 1. Verified Overlap & Drift Audit (2026-11-26)

1. **Brand Rename Surface (`5fcea3dbc`) vs Compatibility Identifiers**:
   - *Evidence*: `src-tauri/tauri.conf.json` (`productName: "Waylode"`, `mainBinaryName: "waylode"`, `identifier: "dev.vbcr.waylode"`), `src-tauri/Cargo.toml` (`waylode`, `wayloded`, `waylode-mcp`, `waylode-hook`, `waylode-ptyd`), `crates/waylode-worktree/Cargo.toml` (`waylode-worktree`, `waylode-rustc`), `src-tauri/src/desktop/identity_migration.rs` (migrating `dev.vbcr.gitpulse` and `com.gitpulse.desktop`), and `src-tauri/src/tool_config.rs` (`platform_config_dir("Waylode", "waylode")` migrating from `GitPulse/`).
   - *Finding*: Active docs still mixed pre-rename `gitpulse-*` binaries, `GitPulse.app`, `Application Support/GitPulse`, and stale Portfolio path prefixes until Phase 1 resolved 34 tracked docs + 3 gitignored docs and brought `devmap doc-drift --json` to `0` findings in maintained in-scope files.
   - *Intentionally Retained Identifiers*: `gitpulse-menu` (`src-tauri/src/desktop/mod.rs`), `gitpulse-session-attention`, `GITPULSE_TRANSCRIPT_ROOT`, `GITPULSE_CODEX_TRANSCRIPT_ROOT`, `GITPULSE_DISABLE_APPLE_INTELLIGENCE`, `GITPULSE_DEVMAP_BIN`, `GITPULSE_MANVI_BIN`, `.gitpulse/`, `refs/gitpulse/{ops,turns}/*`, `<git-common-dir>/gitpulse/{lanes,build-cache}/`, `gitpulse.lockedBranch`, `gitpulse-theme/1` fallback, and `WAYLODE_MCP_EXPOSE_LEGACY=1`.

2. **5-View + Mavi + Appearance Surface Alignment**:
   - *Evidence*: `src/lib/views/viewRegistry.ts` registers `5` views (`work`, `mavi`, `code`, `history`, `insights`) and `11` sections (`Mavi` is a single pane with no sub-sections); `src/lib/health/sections.ts` defines `13` Health sub-sections; `brand/waylode-mark.json` + `src/lib/ui/appearance/presets.ts` + `backdrops.ts` define `21` presets (`4` Waylode Brand + `17` existing) and `11` wallpapers.
   - *Finding*: `README.md`, `docs/VIEW_REFERENCE.md`, `wiki/Home.md`, `docs/MACOS_APPEARANCE.md`, and `wiki/Architecture.md` lagged at `4 views` / `17 presets` / `328 handlers` (actual `bun run check:ipc` is `370` handlers across `141` contracts / `372` structs / `2614` fields). All were synchronized and pinned via `documented-counts-contract.test.ts`.

3. **Terminal Host Protocol 5, Port Probe, Spawn Flags & Mavi Safety**:
   - *Evidence*: `src-tauri/src/terminal/host/proto.rs:26` (`PROTOCOL = 5`), `compat_v4.rs`, `src-tauri/src/terminal/port_probe.rs`, `src-tauri/src/terminal/orchestrate.rs`, and `src-tauri/src/mavi/` + `src-tauri/src/commands/terminals.rs`.
   - *Design Rationale*: Agent CLIs (`claude`, `codex`, `grok`, `manvi`, `agy`) spawn child shells or hold their own stdio sockets; excluding `AGENT_PROCESS_NAMES` and stopping traversal at `SHELL_PROCESS_NAMES` prevents misattributing unrelated background ports to the terminal dock while keeping dev-server port badges accurate. Refusing `bypass` mode and dangerous `settings` keys on `waylode_terminal_spawn` prevents an AI caller from elevating its child's permission ceiling.

## 2. Alternatives Considered (EGM)

| Decision | Option A (Chosen) | Option B (Rejected) | Why Option B Was Rejected |
| --- | --- | --- | --- |
| Rename compatibility keys | Keep `.gitpulse/`, `refs/gitpulse/*`, `WAYLODE_MCP_EXPOSE_LEGACY=1` opt-in aliases, and automatic `Application Support/Waylode` migration | Hard-rename every on-disk directory, git ref, and MCP name at once | Hard cutover would strand live user worktrees, break undo history (`refs/gitpulse/ops/*`), and break active Claude/Codex agent plugins mid-session. |
| Terminal daemon protocol upgrade | Bump `waylode-ptyd` to `PROTOCOL = 5` and keep `compat_v4.rs` read/stop-only until idle | Accept unversioned JSON frames or force-kill older daemons on startup | Silent frame drops lose `needs_input` prompt notifications (`screen` channel), while killing old daemons destroys user sessions. |
| Hide vs unmount `Fleet` view | Swap `Fleet` and repo pane via CSS hiding (`fleet-surface-contract.test.ts`) | Route `Fleet` through `ViewTab` inside `ViewSectionPanel` | `ViewSectionPanel` tears down and rebuilds per repository tab, killing active PTYs inside the dock. |
