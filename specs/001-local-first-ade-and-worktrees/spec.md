# Feature: 001 - Local-First ADE, Git Client & Worktree Orchestration

> **Status (2026-11-26)**: Implemented on `main` (`5fcea3dbc`) with Waylode Phases 1–4 brand/runtime rename, Cobalt Way-Arc brand & 21 presets, 5 views / 11 sections (`Work`, `Mavi`, `Code`, `History`, `Insights`), parallel worktree orchestration, `waylode-ptyd` protocol 5, `waylode-mcp` (66 tools + `WAYLODE_MCP_EXPOSE_LEGACY=1`), and host-load governor/steward. Canonical owner: **Waylode** (`/Users/bharath/Code/devtools/GitPulse`).

## 1. Problem Statement & Motivation

Multi-agent coding breaks down when a developer switches between detached Git GUIs, scattered terminals, isolated worktrees, and opaque AI prompts without shared visibility into collisions, policy status, or host resource pressure. Waylode provides a single local-first desktop Agentic Development Environment (ADE) on Tauri 2 + Svelte 5 + Rust so both the developer and coding agents operate over the same trusted repository facts without cloud telemetry or silent background side effects.

This specification establishes Waylode's owned product boundary within the three-repo stack (**DevCouncil** modules, **Manvi** harness/workbench wrap, **Waylode** desktop ADE) so Waylode's UI, Git sandbox, PTY host, MCP control plane, and worktree tooling never re-implement or drift from upstream DevCouncil or Manvi engines.

## 2. Canonical Ownership & Non-Overlap Boundary

| Boundary | Surfaces & Responsibilities | Source of Truth |
| --- | --- | --- |
| **Owns** | Tauri 2 desktop shell, Svelte 5 routerless 5-view/11-section UI (`Work`, `Mavi`, `Code`, `History`, `Insights`) + global `Fleet`, `Tasks`, `Agents`, Terminal Dock; Cobalt Way-Arc brand mark & 21 theme presets (`waylode-theme/1`); Git CLI/diff/graph sandbox (`src-tauri/src/engine/`, `src-tauri/src/graph/`); zero-copy worktree lifecycle, `.gitpulse/worktrees/` lanes, `<git-common-dir>/gitpulse/lanes/` coordination bus, and `crates/waylode-worktree` (`waylode-worktree`, `waylode-rustc`); `waylode-ptyd` (protocol 5, `compat_v4`, `port_probe`); `waylode-mcp` (55 `waylode_*` tools + 11 `devmap_*` parity tools, `WAYLODE_MCP_EXPOSE_LEGACY=1`); `waylode-hook`, `wayloded`; Mavi UI/session bridge (`src-tauri/src/mavi/`); `sysload` host sensor, leases & pressure relief; repository trust & local `secrets` / `analyzer` / `storage/hygiene` execution | `/Users/bharath/Code/devtools/GitPulse` (`src/`, `src-tauri/`, `crates/waylode-worktree/`) |
| **Delegates To** | Code-graph indexing, `StackedStore`, `devmap seed`, CSR/SQLite query logic, and `devmap_*` schema/semantics → **DevCouncil** (`devmap` CLI + vendored `devmap-*`/`dc-*` crates). Policy decision evaluation (`allow`/`warn`/`deny`), `dc-store` task/run state mutations, and managed agent runtime (`manvi serve`) → **Manvi**. Markdown fragment parsing & highlighting → **MarkDev** (`src-tauri/vendored/markdev-*`). | Upstream `DevCouncil`, `Manvi`, `MarkDev` |
| **Non-Goals / Forbidden Overlaps** | Never fork or diverge vendored `src-tauri/vendored/*` crates locally; never add an alternate SQLite task/run database bypassing `dc-store` / `manvi serve`; never declare policy verdicts as authoritative without Manvi evaluation (`Unchecked` must stay visible); never write repository-tracked `AGENTS.md`/`CLAUDE.md` files on tab open without explicit user confirmation; never silently run `devmap build` or policy overrides on untrusted checkouts; never gate local ADE or BYOK workflows behind sign-in or paywalls. | Cross-repo contract (`specs/002-host-integration-and-cross-repo-seams/`, `DevCouncil/specs/003-stack-boundaries-and-non-overlap/`) |

### Verified Overlap & Drift Ledger

| Item | Observed Code Reality | Classification | Resolution / Status |
| --- | --- | --- | --- |
| Deliberate `.gitpulse` on-disk & git-config identifiers | `.gitpulse/worktrees/`, `<git-common-dir>/gitpulse/lanes/`, `<git-common-dir>/gitpulse/build-cache/`, `refs/gitpulse/{ops,turns}/*`, `gitpulse.lockedBranch`, `gitpulse-menu`, `gitpulse-session-attention`, `gitpulse-theme/1` fallback, `WAYLODE_MCP_EXPOSE_LEGACY=1` aliases | Intentional compatibility contract | Preserved in code (`docs/WAYLODE_RENAME_PLAN.md`, `scripts/brand-rename-contract.test.ts`); documented consistently with canonical `Waylode` branding. |
| `crates/waylode-worktree` legacy `DevMap` overlay paths | Legacy `base_store_ref.json` / `overlay.json` retired in favor of upstream `StackedStore` (`devmap seed --stacked`), checked in `src-tauri/src/codeintel/worktree_overlay.rs` | Closed overlap | `base_store_ref.json` refused fail-closed; delegates stacked seeding to `devmap-store`/`devmap-query`. |
| `scripts/e2e-driver-smoke.mjs` & `scripts/linux-podman.mjs` internal harness IDs | Build `--bin waylode --bin waylode-ptyd` while retaining internal Podman image/volume names (`gitpulse-linux-*`) and `GITPULSE_E2E_*` test env keys | Internal test harness IDs | Non-user-facing test constants; tracked in `tasks.md` if renamed later. |
| Active doc/wiki stale `GitPulse` & path drift | Resolved across 34 tracked docs/wiki files + `PROJECT.md`, `SUBSCRIPTION_IMPLEMENTATION_PLAN.md`, `DESIGN_LANGUAGE.md` | Closed (Phase 1) | `devmap doc-drift --json` reports 0 findings on active maintained docs (`docs/launch/ADOPTION_GAPS.md` deferred due to external session claim `eb42bd66`). |

## 3. User Stories & Acceptance Scenarios

### US1: Routerless 5-View ADE, Cobalt Brand & Truthful Failure Surfaces (Priority: P1)
As a developer managing multiple repositories in Waylode,
I want five unified views (`Work`, `Mavi`, `Code`, `History`, `Insights`) plus workspace-scoped `Fleet`, `Tasks`, `Agents`, and `Terminal` dock with Cobalt Way-Arc theming and fail-closed indicators,
So that switching views or repositories preserves running terminals and missing/unchecked signals never appear clean or zero.

**Acceptance Scenarios**:
- **Scenario 1: View Switching, Fleet Preservation & Contrast-Guarded Presets**
  - **Given** a repository tab with an active terminal session and `VIEW_REGISTRY` (`src/lib/views/viewRegistry.ts`) defining `5` views and `11` sections (with `13` Health sub-sections in `src/lib/health/sections.ts`)
  - **When** the user switches between `Work`, `Mavi`, `Code`, `History`, `Insights`, toggles `Fleet`, or selects one of the `21` presets / `11` wallpapers (`src/lib/ui/appearance/presets.ts`, `brand/waylode-mark.json`)
  - **Then** `Fleet` hides rather than unmounts the active PTY subtree, appearance presets validate against WCAG contrast floors under `waylode-theme/1` (and legacy `gitpulse-theme/1`), and any unreadable coverage/audit/policy check renders explicit `not scanned`, `could not read`, or `unchecked` status.

### US2: Zero-Copy Worktrees, 3-Layer Collision Radar & `waylode-rustc` Cache (Priority: P1)
As a developer or agent running parallel tasks across sibling checkouts,
I want isolated worktree creation under `.gitpulse/worktrees/<task>-<id>`, three-layer collision detection, and clonefile-backed Rust artifact caching without corrupting anchor checkouts or leaking `OUT_DIR`,
So that concurrent lanes start quickly and warn before merge or edit collisions occur.

**Acceptance Scenarios**:
- **Scenario 1: Isolated Task Lane Provisioning, Bus Locking & Sound Build Cache**
  - **Given** a trusted repository with clean or active sibling worktrees and `crates/waylode-worktree/`
  - **When** Waylode provisions a task lane, syncs build caches via `cow_clone.rs`, or evaluates `waylode_worktree_radar` / `waylode_collision_risk`
  - **Then** manual worktree creation inside `.gitpulse/worktrees/` is refused (`refuse_gitpulse_lane_target`), `<git-common-dir>/gitpulse/lanes/` serializes claims under `flock(2)` on `.bus.lock`, and `waylode-rustc` verifies `.d` dependencies and `# env-dep:` variables before reusing Stage-2 artifacts.

### US3: Persistent `waylode-ptyd` Protocol 5, Spawn Hardening & Mavi Thread Control (Priority: P1)
As a developer supervising interactive shells, hooked coding CLIs, and structured Mavi threads,
I want PTY sessions to survive app restarts via `waylode-ptyd` (protocol 5), prompt notifications and child port probes to stay accurate, terminal launches to constrain permission modes, and Mavi threads to handle questions and child cleanup safely,
So that unattended agents cannot silently escalate permissions, stall undetected on prompts, or leave stray confirmation assumptions.

**Acceptance Scenarios**:
- **Scenario 1: Protocol 5 Prompts, Shell-Scoped Port Probes, Spawn Constraints & Mavi Consent**
  - **Given** a Unix host running `waylode-ptyd` (`src-tauri/src/terminal/host/proto.rs` `PROTOCOL = 5`), `waylode-hook`, or an active `Mavi` session (`src-tauri/src/mavi/`)
  - **When** a hooked or `agy` agent emits `permission_prompt` / screen prompt notices, `waylode_terminal_spawn` validates `model`/`mode`/`settings`, `port_probe.rs` inspects child ports, or a Mavi `AskUserQuestion` is declined/unanswered or a thread is forgotten
  - **Then** protocol 5 relays the `screen` notice channel (`compat_v4.rs` downgrades protocol-4 hosts to read/stop-only until idle), `note_prompt_notice` wakes `waylode_terminal_wait` with `matched_reason: "needs_input"`, `port_probe.rs` excludes `AGENT_PROCESS_NAMES` and stops at `SHELL_PROCESS_NAMES`, `waylode_terminal_spawn` rejects `bypass` mode and unsafe `settings`, unanswered/declined `AskUserQuestion` sends `QUESTION_DECLINED` + `AGENT_GUIDANCE`, and `cmd_mavi_close_thread_children` closes unprotected child tabs.

### US4: Host-Load Governor, Build Leases & `waylode-mcp` Control Plane (Priority: P2)
As an agent host operating alongside heavy builds on macOS/Unix,
I want `sysload` (`host-load.v1`), `waylode-hook` build leases, and the 66-tool `waylode-mcp` server to coordinate load and repository actions under explicit repository trust,
So that multi-agent builds do not exhaust memory compressors and untrusted repositories cannot mutate Git or tasks.

**Acceptance Scenarios**:
- **Scenario 1: Trust-Gated MCP Control Plane & Record-Locked Build Leases**
  - **Given** the desktop sampler publishing `host-load.v1` under `Application Support/Waylode` (`src-tauri/src/sysload/snapshot.rs`), build leases under `<config>/Waylode/leases/`, and `waylode-mcp` (`src-tauri/src/bin/waylode-mcp.rs`)
  - **When** an agent calls read-only vs mutating MCP tools or launches `cargo`/`go` builds inside a Waylode terminal (`WAYLODE_SESSION_ID` / `WAYLODE_RUN_ID`, legacy `GITPULSE_*` fallbacks)
  - **Then** read-only tools never mutate Git/task state, writes enforce `repository_trust::require` + Manvi `harness::guard_command`, `WAYLODE_MCP_EXPOSE_LEGACY=1` exposes deprecated `gitpulse_*` aliases only on opt-in, and `waylode-hook` gates heavy build verbs through POSIX record-locked slots without killing processes solely for footprint.

## 4. Success Metrics & Testability Criteria

- **SC-001**: `bun run check:ipc` verifies all `370` registered `cmd_*` handlers in `src-tauri/src/lib.rs` match frontend callers with zero orphans.
- **SC-002**: `bun run check:types` verifies `2614` fields across `372` structs in `141` IPC contracts with zero wire-type mismatches.
- **SC-003**: `bun run vitest run scripts/brand-rename-contract.test.ts scripts/architecture-docs-contract.test.ts scripts/documented-counts-contract.test.ts scripts/archive-separation-contract.test.ts scripts/verdict-contract.test.ts scripts/agent-guidance.test.ts` passes cleanly after doc updates.
- **SC-004**: `waylode-mcp` serves exactly `66` MCP tools (`55` `waylode_*` + `11` `devmap_*`) and `4` prompts, with `WAYLODE_MCP_EXPOSE_LEGACY=1` tested for legacy `gitpulse_*` alias resolution.
- **SC-005**: `waylode-ptyd` protocol `5` handshake, `compat_v4.rs` fallback, `port_probe.rs` shell scoping, and `orchestrate.rs` spawn flag rejection (`bypass`, invalid `settings`) are covered by Rust unit/integration suites.
- **SC-006**: `devcouncil spec validate 001 --strict --project-root /Users/bharath/Code/devtools/GitPulse` passes with zero blocking or advisory findings.

## 5. Edge Cases, Exclusions & Open Questions

- **Edge Case — Protocol-4 Host Coexistence**: After an app upgrade while an older protocol-4 `waylode-ptyd` / `gitpulse-ptyd` still holds live sessions, `compat_v4.rs` keeps listing, reading, and stopping sessions while refusing new protocol-5 spawns and retiring the old host once idle.
- **Edge Case — Apple Platform Binary Environment Visibility**: On macOS, `KERN_PROCARGS2` hides environments on Apple-signed `/bin/zsh` / `/bin/sh` binaries; `sysload::procscan` relies on parent-process ancestry and launch snapshot command-line arguments rather than assuming env inheritance after launchd reparenting.
- **Exclusion — Windows & Linux Desktop Runtime Coverage**: Windows and Linux desktop builds exist in CI/release workflows (`docs/CROSS_PLATFORM_TESTING.md`), while macOS remains the primary verified daily-driver platform; `waylode-ptyd` persistence is `#[cfg(unix)]` (Windows uses ConPTY).
- **Exclusion — Non-Scope Files**: `contracts/*` checksum parity belongs to upstream DevCouncil/Manvi and must not be modified directly in Waylode; `docs/launch/ADOPTION_GAPS.md` remains excluded in this docs wave due to an active external worktree claim (`eb42bd66`).
