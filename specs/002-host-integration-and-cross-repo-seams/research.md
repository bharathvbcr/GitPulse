# Research & Design Rationale: 002 - Host Integration & Cross-Repo Seams

## 1. Verified Overlap & Drift Audit (2026-11-26)

1. **Waylode Connect (`agent_connect.rs`) vs `agy_plugin.rs` vs `devmap integrate` (`devmap/integrate.rs`)**:
   - *Evidence*:
     - `src-tauri/src/agent_connect.rs:1-35`: links `waylode-mcp` and `waylode-hook` (plus legacy `gitpulse-mcp` / `gitpulse-hook` compat links) into `~/.local/bin`, registers `SERVER_NAME = "waylode"` via `claude mcp add` / `codex mcp add`, and removes `LEGACY_SERVER_NAME = "gitpulse"`.
     - `src-tauri/src/workbench/agy_plugin.rs:1-34`: syncs `plugins/waylode/` into `~/.gemini/config/plugins/waylode/` with `./bin/waylode-{mcp,hook}` wrappers and `.waylode-managed` sentinel, retiring legacy `~/.gemini/config/plugins/gitpulse`.
     - `src-tauri/src/devmap/integrate.rs:1-45`: invokes `devmap integrate <host> --json` (`Claude`, `Cursor`, `Codex`) to preview/apply repo `AGENTS.md`, `CLAUDE.md`, `.cursor/rules/devmap.mdc`, `.mcp.json`, skills, hooks, and user global `devmap` MCP registration.
   - *Overlap Diagnosis*: Waylode Connect and `devmap integrate` both mutate `~/.claude.json` / Codex global MCP state from different Settings surfaces without sharing a combined preflight report or detecting cross-tool state cleanly. Meanwhile `cmd_devcouncil_init` (`src-tauri/src/commands/intel.rs:460-472`) cleanly isolates machine-only `.git/info/exclude` setup from `cmd_devmap_integrate`.

2. **Vendored Crate Parity & 11-Tool Subset (`devmap_parity.rs`)**:
   - *Evidence*: `src-tauri/vendored/VENDOR.json` pins `16` crates (`12` DevCouncil at commit `84b97f0bfd976c9bb437541e6be8ce3f72bbd6b3` + `4` MarkDev). `src-tauri/src/mcp/devmap_parity.rs:1-44` explains why `devmap-serve` is unvendored (`hyper`/HTTP dep weight) and exposes `11` `devmap_*` tools checked by `scripts/check-schema-parity.mjs:335-377`.

3. **`parse_git_version` Divergence (`preflight.rs:66-92`)**:
   - *Evidence*: Direct source inspection confirms 3 behavioral divergences from `dc_proc::GitVersion::parse` (overflow `None` vs `unwrap_or(0)`, `-rc1` hyphen splitting before patch parse, major-only `git version 3` -> `(3, 0, 0)`).

4. **`manvi serve` & Read-Only Contract Mirroring**:
   - *Evidence*: `src-tauri/src/harness/sidecar.rs:1-22` confirms `MANVI_HARNESS_INIT_ENABLED=false` and scratch `MANVI_STATE_DIR`; `scripts/check-schema-parity.mjs:27-29` verifies `contracts/serve.ops.schema.json` byte equality with Manvi.

5. **Wave A Code-vs-Spec Audit Findings (`2026-11-26`, `DevCouncil` @ `5beb1e54`, `Waylode` @ `5fcea3dbc`)**:
   - **`OV-INTEGRATE-01` & `GAP-HOST-01`**: `src-tauri/src/agent_connect.rs:1-35, 287-289, 415-460, 654-780` (`src/lib/tools/agentConnect.ts:62-66`) and `src-tauri/src/workbench/agy_plugin.rs:1-34, 429-525` detect CLIs and mutate global MCP/plugin configs independently from `src-tauri/src/devmap/integrate.rs:300-350` (`src/lib/components/AgentIntegrationPanel.svelte:1-40`), while `src-tauri/src/workbench/claude_hook.rs:1-90` injects `7` hook events via `--settings` at launch.
   - **`OV-SKILLS-01`**: `plugins/waylode/skills/` (`9` Waylode-only skills) has `0` overlap with `DevCouncil/rust/devmap-cli/skills/` (`5`) or `DevCouncil/backend/go_orchestrator/devcouncil/skills/library/` (`18`), while `.agents/skills/` (`23` skills) holds repo-local dogfooding copies only.
   - **`DRIFT-VENDOR-01`**: `src-tauri/vendored/VENDOR.json:10` pins all `12` DevCouncil crates at `84b97f0bfd976c9bb437541e6be8ce3f72bbd6b3` (`DevCouncil` `HEAD` is `5beb1e54027af55b29ea4df83f1257bc3b0470f5`), while `bun run check:schema-parity` passes (`7 match`, `2 absent` for unbuilt/uninstalled local `waylode-mcp` / `Waylode.app`).
   - **No Unauthorized Engine Duplication**: Verified `0` parallel turn loops, `0` `dcverify` stub reimplementations (`verification_driver.rs` / `verify_status.rs` wrap Manvi `work.verify.task`), and `0` parallel SQLite task stores (`src-tauri/src/tasks/file_tasks.rs` imports Markdown briefs only).
   - **`DRIFT-DOC-01`**: Pre-rename `GitPulse` module comments remain in `src-tauri/src/mcp/devmap_parity.rs:1`, `src-tauri/src/harness/mod.rs:3`, `src-tauri/src/mavi/mod.rs:3`, and `src-tauri/src/workbench/claude_hook.rs:1` (queued for source cleanup in `tasks.md`).

## 2. Alternatives Considered (EGM)

| Decision | Option A (Chosen) | Option B (Rejected) | Why Option B Was Rejected |
| --- | --- | --- | --- |
| Serve 11 `devmap_*` tools in-process via `codeintel` | Expose `11` parity-checked tools in `waylode-mcp` and pin names via `check-schema-parity.mjs` | Vendor `devmap-serve` or shell out to `devmap mcp` on every call | Vendoring `devmap-serve` pulls `hyper` into the desktop binary; spawning `devmap mcp` per request adds startup latency and loses multi-repo session caching. |
| Keep `parse_git_version` local | Retain tested helper in `preflight.rs:76` and document divergence explicitly | Link `dc-proc` directly into `waylode` crate | `dc-proc` saturation/hyphen behavior would reject valid `2.45.1-rc1` Git for `--filter`/`clone` support and mask overflow inputs. |
| Separate `cmd_devcouncil_init` from `cmd_devmap_integrate` | Run silent `init` on `.git/info/exclude` only; preview `integrate` and `agent_connect` explicitly | Auto-run `devmap integrate` when opening a repo | Opening a repository tab must never dirty `git status` or modify `~/.claude.json` without user consent. |
