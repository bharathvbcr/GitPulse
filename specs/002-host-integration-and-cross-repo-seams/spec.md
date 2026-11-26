# Feature: 002 - Host Integration & Cross-Repo Seams

> **Status (2026-11-26)**: Active production seams on `main` (`5fcea3dbc`) with verified deliberate exceptions (`devmap_*` 11-tool subset, `parse_git_version`) and one audited host-registration overlap (`agent_connect.rs` / `agy_plugin.rs` vs `devmap/integrate.rs`). Canonical owner: **Waylode** (`/Users/bharath/Code/devtools/GitPulse`), aligned with `DevCouncil/specs/003-stack-boundaries-and-non-overlap` and `Manvi/specs/001`–`002`.

## 1. Problem Statement & Motivation

Waylode integrates two independently updatable upstream ecosystems: **DevCouncil** (vendored Rust read-side/analysis crates plus the `devmap` CLI) and **Manvi** (`manvi serve` NDJSON sidecar for policy evaluation, workbench tasks, and managed runs). Without strict host-seam boundaries, downstream consumers drift into forked vendored crates, conflicting MCP/hook installers, silent schema skew (`CURRENT_SCHEMA_VERSION = 29`, `SCHEMA_VERSION`, `WORKBENCH_SCHEMA`, `EXTRACTION_SCHEMA_VERSION`, `STACK_FORMAT_VERSION`), or divergent policy verdict classification.

This specification defines Waylode's host integration architecture, codifies every cross-repo boundary and deliberate exception (`devmap_*` 11-tool subset parity, `parse_git_version`), audits the host-registration overlap between Waylode Connect and `devmap integrate`, and establishes a concrete closure plan.

## 2. Canonical Ownership & Non-Overlap Boundary

| Boundary | Surfaces & Responsibilities | Source of Truth |
| --- | --- | --- |
| **Owns** | Vendoring & build parity scripts (`scripts/vendor-crates.mjs`, `scripts/check-schema-parity.mjs`, `scripts/check-vendor-schema.mjs`); `src-tauri/src/codeintel/` + `src-tauri/src/devmap/` CLI/daemon orchestration; `src-tauri/src/mcp/devmap_parity.rs` (`11` `devmap_*` MCP tools); `src-tauri/src/harness/` (`manvi serve` stdio NDJSON client with `MANVI_HARNESS_INIT_ENABLED=false` and isolated `MANVI_STATE_DIR`); `src-tauri/src/agent_connect.rs`, `src-tauri/src/workbench/agy_plugin.rs`, `src-tauri/src/hooks/`, `plugins/waylode/`, `plugins/waylode-agy/`; `src-tauri/src/engine/git_cli/preflight.rs::parse_git_version` | `/Users/bharath/Code/devtools/GitPulse` (`src-tauri/`, `scripts/`, `plugins/`) |
| **Delegates To** | Upstream implementation of `src-tauri/vendored/` (`12` `dc-*`/`devmap-*` crates + `4` `markdev-*` crates), `devmap` CLI, `devmap-serve::mcp::TOOL_NAMES` (`21` upstream tools), root schema constants, shared `event`/`lease`/`verdict` contracts, canonical host-wiring table (`devmap integrate`), and DevMap/DevCouncil skills → **DevCouncil**. Runtime policy enforcement, `manvi serve` protocol (`contracts/serve.ops.schema.json`), agent turn loop, and `dc-store` workbench host writes → **Manvi**. | `DevCouncil` (`rust/`, `specs/003-stack-boundaries-and-non-overlap`), `Manvi` (`manvi/serve/`, `specs/001`, `specs/002`) |
| **Non-Goals / Forbidden Overlaps** | Never hand-edit `src-tauri/vendored/*` (fix upstream and run `bun run vendor`); never edit `contracts/*` or `contracts/CHECKSUMS` locally (sync from upstream `DevCouncil`/`Manvi`); never vendor `devmap-serve` (`hyper` HTTP stack) into `waylode`; never let `manvi serve` mutate `.gitignore` or `.devcouncil/` inside a user repository (`MANVI_HARNESS_INIT_ENABLED=false`); never silently drop schema-skew or missing-sidecar errors; keep all local + BYOK integration seams free of sign-in or paywalls. | `src-tauri/vendored/VENDOR.json`, `contracts/README.md`, `scripts/check-schema-parity.mjs` |

### Verified Overlap & Drift Ledger

| Seam / Item | Observed Code Reality | Classification | Closure / Tracking |
| --- | --- | --- | --- |
| **Waylode Connect + `agy_plugin` vs `devmap integrate` (`OV-INTEGRATE-01`)** | `agent_connect.rs` (`cmd_agent_connect_{plan,apply}`) links `waylode-{mcp,hook}` (+ legacy `gitpulse-*` links) and registers `waylode` in Claude/Codex, `agy_plugin.rs` manages `~/.gemini/config/plugins/waylode/`, and `claude_hook.rs` injects `--settings`, while `src-tauri/src/devmap/integrate.rs` (`cmd_devmap_integrate`) runs `devmap integrate <host> --json` for `Claude`/`Cursor`/`Codex` (`AGENTS.md`, `CLAUDE.md`, `.mcp.json`, skills, hooks). Target ownership puts host config tables in DevCouncil `devmap integrate` (`OV-INTEGRATE-01`). | Active overlap (`OV-INTEGRATE-01`) | Audited in `research.md`; closure tracked in `tasks.md` `[T-06]`–`[T-07]` aligned with `DevCouncil/specs/003` (`T-008`). |
| **Product-scoped skill packaging (`OV-SKILLS-01`)** | `plugins/waylode/skills/` ships `9` Waylode-only skills (`collisions`, `commit-and-merge`, `insights`, `load`, `merge-prs`, `merge-train`, `tasks`, `terminal`, `wrap-up`) with zero overlap against DevCouncil's `5` `devmap` and `18` `devcouncil` skills; `.agents/skills/` holds `23` repo-local dogfooding copies separately. | Active boundary (`OV-SKILLS-01`) | `tasks.md` `[T-07]` enforces single-product skill boundaries (no sibling `SKILL.md` vendoring) aligned with `DevCouncil/specs/003` (`T-009`). |
| **`devmap_*` MCP tool parity (`11` vs `21`)** | `src-tauri/src/mcp/devmap_parity.rs` exposes `11` read/preview `devmap_*` tools (`TOOL_NAMES` L32–44) dispatched through `crate::codeintel`, deliberately omitting `devmap-serve` (`hyper` HTTP deps). | Intentional host subset | `scripts/check-schema-parity.mjs` asserts every Waylode `devmap_*` name is in upstream `devmap-serve::mcp::TOOL_NAMES` at the `VENDOR.json` commit. |
| **`parse_git_version` local helper** | `src-tauri/src/engine/git_cli/preflight.rs:76` kept local vs `dc_proc::GitVersion::parse` because `waylode` does not link `dc-proc` directly and enforces 3 fail-closed/compat rules (overflow fails closed `None`, `2.45.1-rc1` keeps patch `1`, `git version 3` parses `(3,0,0)`). | Deliberate exception | Documented in `preflight.rs:66-75`; tested in `preflight.rs` and `preflight_adv_l1_tests.rs`. |
| **`contracts/` local copy parity** | `contracts/{README.md, CHECKSUMS, event.schema.json, lease.schema.md, serve.ops.schema.json, verdict.cases.json, verdict.schema.json}` are read-only downstream copies from DevCouncil/Manvi. | Clean shared contract | Checked byte-for-byte by `scripts/check-schema-parity.mjs` and `scripts/check-schema-parity.test.ts`. |

## 3. User Stories & Acceptance Scenarios

### US1: Vendored Crate & 5-Schema Wire Parity (Priority: P1)
As a Waylode maintainer or automated CI gate,
I want all `12` vendored DevCouncil crates (`dc-*`, `devmap-*`), `4` MarkDev crates, compiled `waylode-mcp` binary, and PATH `devmap`/`dcstore` tools checked against upstream constants,
So that a stale vendored crate, outdated PATH binary, or mismatched `contracts/serve.ops.schema.json` fails immediately instead of corrupting stores.

**Acceptance Scenarios**:
- **Scenario 1: Fail-Closed Five-Schema & Upstream Contract Parity**
  - **Given** `src-tauri/vendored/VENDOR.json` (`16` vendored crates) and `scripts/check-schema-parity.mjs` checking `CURRENT_SCHEMA_VERSION` (`29`), `SCHEMA_VERSION`, `WORKBENCH_SCHEMA`, `EXTRACTION_SCHEMA_VERSION`, `STACK_FORMAT_VERSION`, and `serve.ops.schema.json`
  - **When** a developer runs `bun run vendor:check`, `bun run check:vendor-schema`, or `bun run check:schema-parity`
  - **Then** any modified vendored file is rejected, any stale or unverifiable reader yields explicit `stale` / `unverifiable` (never `match`), and `contracts/serve.ops.schema.json` matches upstream Manvi byte-for-byte.

### US2: `devmap_*` 11-Tool Parity Without HTTP Bloat (Priority: P1)
As an agent calling `waylode-mcp`,
I want the `11` essential `devmap_*` code-intelligence tools served directly over stdio with shared `WalkBounds` (`MCP_DEFAULT_WALK_DEPTH = 3`),
So that every `devmap_*` tool name matches `devmap-serve::mcp::TOOL_NAMES` without pulling `hyper` into Waylode or opening unpinned repositories.

**Acceptance Scenarios**:
- **Scenario 1: Subset Parity Against `devmap-serve::mcp::TOOL_NAMES`**
  - **Given** `src-tauri/src/mcp/devmap_parity.rs` defining `TOOL_NAMES` (`devmap_status`, `devmap_search`, `devmap_dependencies`, `devmap_impact`, `devmap_trace`, `devmap_neighbors`, `devmap_dead_symbols`, `devmap_clones`, `devmap_preview`, `devmap_explore`, `devmap_affected_tests`)
  - **When** `waylode-mcp` responds to `tools/list` and `scripts/check-schema-parity.mjs::toolNamesRowAt` compares against upstream `devmap-serve` at `VENDOR.json`'s `devmap-query` commit
  - **Then** all `11` names match upstream `TOOL_NAMES` verbatim and every query routes through `crate::codeintel` requiring an explicit `repo_path`.

### US3: Clean `manvi serve` Sidecar & Read-Only Contract Enforcement (Priority: P1)
As a developer opening untrusted or clean repositories in Waylode,
I want the `manvi serve` sidecar isolated from modifying repository files, correlating requests by ID without cross-call lock stalls, and rendering all `8` verdict statuses faithfully,
So that policy gating and workbench storage never dirty user repositories or downgrade unchecked/degraded outcomes to `Allowed`.

**Acceptance Scenarios**:
- **Scenario 1: Scratch-Isolated Sidecar & Eight-Status Verdict Fidelity**
  - **Given** `src-tauri/src/harness/sidecar.rs`, `src-tauri/src/harness/policy.rs`, and `contracts/verdict.schema.json`
  - **When** Waylode spawns `manvi serve` or evaluates a command/file gate
  - **Then** the child starts inside a scratch directory with `MANVI_HARNESS_INIT_ENABLED=false` and isolated `MANVI_STATE_DIR`, concurrent request IDs do not block unrelated runs, and missing/timed-out sidecars report `Unchecked` / `Degraded` rather than `Allowed`.

### US4: Convergent Agent Host Registration & Hook Chain (Priority: P2)
As a developer connecting Claude Code, Codex, Cursor, or Antigravity (`agy`),
I want `waylode-hook`, `waylode-mcp`, plugins (`plugins/waylode/`, `plugins/waylode-agy/`), and `devmap integrate` to install cleanly with explicit previews and non-overlapping ownership,
So that agents do not receive duplicate `gitpulse`/`waylode` registrations or silent repository edits on tab open.

**Acceptance Scenarios**:
- **Scenario 1: Consent-Gated Registration & Antigravity Bundle Hygiene**
  - **Given** `src-tauri/src/agent_connect.rs`, `src-tauri/src/workbench/agy_plugin.rs`, `src-tauri/src/devmap/integrate.rs`, and `src-tauri/src/hooks/`
  - **When** opening a tab (`cmd_devcouncil_init`) vs previewing/applying agent setup (`cmd_agent_connect_{plan,apply}`, `cmd_devmap_integration_preview`, `cmd_devmap_integrate`)
  - **Then** `cmd_devcouncil_init` writes only `.git/info/exclude` and DevMap state, `agent_connect` removes legacy `gitpulse` MCP registrations while linking `waylode-{mcp,hook}`, `agy_plugin` syncs only with `.waylode-managed` safety checks (skipping `PreToolUse` on `agy`), and remaining overlap with `devmap integrate` is tracked cleanly for closure.

## 4. Success Metrics & Testability Criteria

- **SC-001**: `bun run vitest run scripts/check-schema-parity.test.ts` (`58` tests) and `bun run check:schema-parity` pass with `0` `stale` or `unverifiable` verdicts on present tools.
- **SC-002**: `bun run vitest run scripts/verdict-contract.test.ts scripts/plugin-contract.test.ts scripts/agent-guidance.test.ts` passes (`15` + `42` + `15` tests) verifying 8-status verdict rules and plugin manifest hygiene.
- **SC-003**: `src-tauri/src/mcp/devmap_parity.rs` tests verify all `11` `devmap_*` tools match `tool_definitions()` and dispatch with required `repo_path`.
- **SC-004**: `src-tauri/src/engine/git_cli/preflight.rs` and `preflight_adv_l1_tests.rs` verify all `parse_git_version` edge cases (`2.39.3 (Apple Git-146)`, `2.45.1-rc1`, `git version 3`, overflow `None`).
- **SC-005**: `contracts/*` and `contracts/CHECKSUMS` remain byte-for-byte identical to upstream `DevCouncil`/`Manvi` contracts (`git diff -- contracts/` empty).
- **SC-006**: `devcouncil spec validate 002 --strict --project-root /Users/bharath/Code/devtools/GitPulse` passes with zero blocking or advisory findings.

## 5. Edge Cases, Exclusions & Open Questions

- **Edge Case — Antigravity (`agy`) `PreToolUse` Semantics**: As documented in `src-tauri/src/workbench/agy_plugin.rs:28-33`, Antigravity IDE 2.32.1 denied any `PreToolUse` answer other than `"decision":"allow"`; therefore `plugins/waylode-agy/hooks.json` omits `PreToolUse` and records edits via `PostToolUse` instead of auto-returning `"allow"`.
- **Edge Case — Deliberate `parse_git_version` Exception**: `preflight.rs::parse_git_version` remains intentionally distinct from `dc_proc::GitVersion::parse` so `2.45.1-rc1` satisfies `MIN_CLONE_GIT_VERSION`, `git version 3` parses as `(3, 0, 0)`, and integer overflow returns `None`.
- **Exclusion — Upstream Contract Ownership**: Waylode consumes `contracts/*` read-only and never originates edits to `verdict.schema.json`, `event.schema.json`, `lease.schema.md`, or `serve.ops.schema.json` directly inside this repo.
- **Exclusion — Unvendored `devmap-serve`**: Full 21-tool HTTP/MCP surface stays in upstream `DevCouncil` (`devmap mcp`); Waylode maintains only the 11-tool stdio parity subset.
