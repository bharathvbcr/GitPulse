# Waylode Rename & Logo Change Master Plan

> [!IMPORTANT]
> **Unified Integration Branch (`rename/waylode-all`):** Phase 1 (`waylode-brand`), Phase 1B (Logo & Palette), Phase 2 (Surfaces, Binaries, MCP & Env Vars), and Phase 3 (Saved Data, OS Identity & Packaging) are unified onto `rename/waylode-all` (`.gitpulse/worktrees/waylode-rename-all`) off `main`.

---

## 1. Locked-In Executive Decisions

The following decisions are **locked in** for `rename/waylode-all`:

1. **Waylode Logo Concept & Palette (Phase 1B):**
   - **Logo Concept:** **Concept 2 — "Lodestone Way-Arc"** — Keeps the open outer ring while replacing the ECG pulse with a rising forward-way crest toward the upper-right lodestar (`M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8` + star at `23.8, 9.0`), preserving continuity for existing users and `tray.rs`.
   - **Palette:** **Lodestar gold -> teal -> sky** (`#fbbf24 -> #2dd4bf -> #38bdf8`) on `#151d2b -> #0a0d14`, updating `--brand-gradient` in `src/app.css` and contrast/contract tests accordingly.
2. **Branch & Scope Strategy (Phases 1 + 1B + 2 + 3 Unified):**
   - Execute **Phases 1 + 1B + 2 + 3** together on unified branch **`rename/waylode-all`** (absorbing `waylode-brand` and `waylode-aliases` as the baseline).
   - Rename `tauri.conf.json` `productName` to `"Waylode"` (with `wix.upgradeCode` pinned before any Windows Waylode release) and rename `docs/WHY_GITPULSE.md` -> `docs/WHY_WAYLODE.md` (+ link updates).
3. **Daemon Binary Names (Phase 2):** **`wayloded`** (1:1 parity with `gitpulsed`) and **`waylode-ptyd`** (1:1 parity with `gitpulse-ptyd`).
4. **MCP Legacy Tool Bridge (Phase 2):**
   - Canonicalize `gitpulse_* -> waylode_*` **before** schema validation (`validate_arguments`), board canonicalization (`with_current_board_status`), and CLI lookup (`find_tool()`), **and** provide opt-in `WAYLODE_MCP_EXPOSE_LEGACY=1` to advertise the 55 `gitpulse_*` aliases in `tools/list` while keeping default `tools/list` lean at 66 tools.
5. **On-Disk `.gitpulse/` & `gitpulse.lockedBranch` (Phase 3):**
   - Keep **`.gitpulse/` permanently unrenamed** on disk (to avoid breaking active linked worktrees registered in `.git/worktrees/*/gitdir` and repo `.gitignore`/exclude rules) and **indefinitely dual-read `gitpulse.lockedBranch`** (plus `gitpulse.{gate,trainWorktree,gateDiskGb}`).
6. **Windows Upgrade & Relay Scope (Phase 2/3):**
   - Pin `bundle.windows.wix.upgradeCode` (extracted from `GitPulse_0.2.0-beta.1_*_en-US.msi`) + NSIS legacy uninstaller hook before any Windows Waylode build ships; leave `MarkDev` (`relay.gitpulse.dev`) + `Graft` (`graft.gitpulse.*` phone-bridge protocol) for a separate post-Phase-2 relay/protocol cutover.
7. **Plan Location:** Checked in at `docs/WAYLODE_RENAME_PLAN.md` on `rename/waylode-all`.

---

## 2. Phase 0: Name, Namespace & Trademark Gate

Before shipping any public Waylode artifact:
- **Search Snapshot:** Web checks across GitHub, crates.io, npm, Homebrew, and WIPO/India trademark databases turned up no active software packages, repositories, or registered marks named `waylode` (only archaic Middle English glossary entries). Live registry and formal trademark filing checks still apply.
- **External Claims Checklist (User Action):**
  - Register `waylode.dev` (and `waylode.app` if desired; current plugin manifests use `https://gitpulse.vbcr.dev`); keep `waylode.com` backorder active through redemption.
  - Reserve `waylode` GitHub organization/username and `waylode` package names on **npm** and **crates.io** (plus Homebrew cask/tap name if planned).
  - Complete WIPO Global Brand Database + IP India Classes **9** (software) and **42** (software services) clearance. **Hard stop:** if India shows a `"Wayload"` mark in Class 9/42 or a confusingly similar dev-tool class, stop before Phase 1B/2 public release.

---

## 3. Phase 1: Brand Text & Docs Reconciliation (`rename/waylode-brand`)

### 3.1 Bugs Found in `waylode-brand`'s `scripts/brand-rename.mjs`
1. **`TOKEN` regex rejects trailing escape sequences** (`scripts/brand-rename.mjs:36`): `(?![A-Za-z0-9_/\\-])` includes `\\`, so literals with `\n` or `\'` immediately after `GitPulse` were skipped:
   - [tray.rs](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/src/desktop/tray.rs#L99): `"GitPulse\n{agents}{}\n{}"` stayed `"GitPulse..."`.
   - `src/lib/ui/copy/vocabulary.ts:45`: `"GitPulse's checks"` stayed unrenamed while line 42 became `"Waylode's own gate"`.
2. **`body === OLD` blanket skip** (`scripts/brand-rename.mjs:128`): Skipping every standalone `"GitPulse"` string missed real UI-visible surfaces:
   - [Logo.svelte](file:///Users/bharath/Code/devtools/GitPulse/src/lib/components/Logo.svelte#L25): `title = "GitPulse"` (breaks `aria-label="Waylode"`).
   - `src/lib/desktop/windowChrome.ts:39` and `src-tauri/src/desktop/hibernate.rs:643`: fallback window title `"GitPulse"`.
   - `src/lib/desktop/menuContract.ts:487,496`, `src/lib/desktop/menuState.ts:736,769`, `src-tauri/src/desktop/state.rs:75`: default `trayDetail` / `StatusCard.repository = "GitPulse"`.
   - `src-tauri/src/workbench/notifications/graft.rs:31`: `const APP: &str = "GitPulse";` (keep `SOURCE: "gitpulse"` for Phase 2).
3. **Inline `#[cfg(test)]` modules in non-`*_tests.rs` files were mutated** (`TEST_FILE` only filters by filename):
   - **Broken Rust test:** `src-tauri/src/workbench/review.rs:462` now asserts `"Blocked by Waylode"`, while [harness/mod.rs](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/src/harness/mod.rs#L2078) (in `RUST_EXCLUDED`) still emits `"Blocked by GitPulse"`.
   - **Broken Rust test:** [tray.rs](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/src/desktop/tray.rs#L809) kept `repo_label: "GitPulse"` (`body === OLD`), while `:826` rewrote the assertion to `"• ClaudeCode · Waylode — Needs your permission now\n"`.
   - **Mutated test fixtures:** `git config user.name` fixtures (`"GitPulse Contract"` / `"GitPulse Tester"`) in `git_writer.rs`, `stash.rs`, `ops.rs`, `worktree.rs`, and `"dependent": "GitPulse"` in `src-tauri/src/analyzer/deps.rs:5170,5176`.
4. **Agent-facing leaks (`RUST_EXCLUDED` incomplete):** `src-tauri/src/workbench/terminal_command.rs:1553` (agent CLI prompt) and `src-tauri/src/workbench/agy_plugin.rs:266` (hook block JSON message) were rewritten even though Phase 1 is brand-only.
5. **Other Phase 1 gaps:**
   - `scripts/brand-rename.mjs:11` cites a non-existent `scripts/brand-rename.contract.test.ts`.
   - `src/lib/ui/copy/sentenceCase.ts:43`: `PROPER_NOUNS` lacks `"Waylode"`, so `toSentenceCase()` lowercases mid-sentence `"Waylode"` to `"waylode"`.
   - [tauri.conf.json](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/tauri.conf.json#L87): `longDescription` still starts with `"GitPulse is a high-performance..."`, and `:3` `productName` is `"GitPulse"` (leaving macOS app menu + `AboutMetadata` at [menu.rs](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/src/desktop/menu.rs#L137-L144) showing `"GitPulse"`).
   - `scripts/architecture-docs-contract.test.ts:126-128` was updated to `/Waylode[\s*]*uses[\s*]*Manvi/i`, while `:112` test title still says `GitPulse`.

### 3.2 Un-Updated Tests Still Asserting `"GitPulse"`
- **Frontend unit tests:** `StatusPopover.test.ts:218,220`, `AttentionInbox.test.ts:54`, `ToastContainer.test.ts:65`, `SessionNotificationBridge.test.ts:74`, `NativeNotificationBridgeView.test.ts:67,92` (`aria-label="GitPulse"`), `windowChrome.test.ts:41,58,60,108`, `nativeActions.test.ts:203`, `sentenceCase.test.ts:202`, `scripts/menu-state-contract.test.ts:195,202`.
- **E2E tests:** `tests/e2e-r1-r4/tier1-r1-r3.test.ts:423,429,470,529`, `helpers/fixtures.ts:330,337`, `tests/e2e-r1-r4/browser/*.html:6`.
- **Rust tests:** `workbench/review.rs:462`, `desktop/tray.rs:744,755,759,764,809,826,843`, `notifications/graft/tests.rs:42,53,532`, `alerts/tests.rs:92,162,1109`, plus tests in `ipc_error`, `ledger/schema_bump_adv_tests.rs`, `ai/tests/manvi_route_tests.rs`, `storage/repo_tuning/advice_tests.rs`, `engine/git_writer_commit_graph_adv_tests.rs`, `harness/hosted*_tests.rs`, and `workbench/*_tests.rs` where user-facing prose changed.

### 3.3 Exact Collision Map & Safe Reconciliation Order
Across the 7 colliding worktrees (`waylode-brand`, `win-lane-B..E`, `great-williamson-1cdf4d`, `m5-verify-wt`):
- **Live dirty-only overlaps:** **28 files** (**23** with `waylode-brand` + **5** sibling-only).
- **Including `win/lane-D`'s committed diff (`merge-base(main, HEAD)..HEAD`):** **37 files** (**29** with `waylode-brand` + **8** sibling-only, including 3-way `src-tauri/src/workbench/notifications.rs` across `waylode-brand` + `win/lane-D` + `m5-verify-wt`).

**Recommended Reconciliation Order (Zero 3-Way Merge Conflicts for Phase 1):**
1. **Land `win-lane-B`, `win-lane-C`, `win-lane-D`, `win-lane-E` first** (functional Windows fixes; resolve intra-lane overlaps `mcp/mod.rs`, `test_support.rs`, `ci.yml`, `engine/process/tests.rs`).
2. **Land or retire `great-williamson-1cdf4d` (446 behind `main`) & `m5-verify-wt` (493 behind `main`) next.**
3. **Re-apply fixed `scripts/brand-rename.mjs --write` + small curated UI/test patch on fresh `main` last**, rather than resolving 29 text-rename conflicts by hand.

---

## 4. Phase 1B: Waylode Logo Change Plan

### 4.1 Complete Visual Asset & Code Inventory
| Surface | File / Location | Current State & Notes |
|---|---|---|
| Canonical Svelte Component | [Logo.svelte](file:///Users/bharath/Code/devtools/GitPulse/src/lib/components/Logo.svelte#L1-L135) | `viewBox="0 0 32 32"`, `stroke-width="3.4"`, single `<path pathLength="100">` + `<circle>`, `gp-*` UIDs, `.gitpulse-trace` |
| Direct `<Logo>` Consumers (9) | `App.svelte:1346,1407`, `StatusPopover.svelte:126,167`, `ToastContainer.svelte:143`, `AttentionInbox.svelte:102`, `NativeNotificationBridgeView.svelte:82,85`, `SessionNotificationBridge.svelte:103`, `harness/OnboardingHost.svelte:32` | Sizes 16px–68px, all `variant="badge"` (`translate(3.04 3.04) scale(0.81)`) |
| HTML Boot & Favicon | `index.html:6,60-209` | `/gitpulse-mark.svg` favicon + `#gp-boot` inline SVG (`svg.gp-boot-logo`) |
| Static SVGs | `public/gitpulse-mark.svg` (32×32 `rx="10"`), `public/gitpulse-icon.svg` (1024×1024 square plate) | Must match `Logo.svelte` stroke, node, gradient, and `badge` transform |
| Desktop Rasters & Bundles | `src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png`, `icon.icns`, `icon.ico` | Currently opaque square `#151d2b` corners (lacks macOS squircle padding in `.icns`!) |
| Windows / Linux Installers | [tauri.conf.json](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/tauri.conf.json#L65-L85) | No custom WiX/NSIS bitmaps or `.desktop` template; bundler derives from `icon.ico` & `icons/*.png` |
| Plugin Assets | `plugins/gitpulse/assets/logo.png` (256×256 transparent corners), `plugins/gitpulse-agy/assets/logo.svg` | Bundled via `tauri.conf.json:89`; SHA256 asserted in `notifications.rs` & E2E tests |
| Tray & Notifications | `src-tauri/src/desktop/tray.rs:9,99,167-278,294,375`, `menu.rs:138-144`, `notifications/macos.rs:84-88` | 36×36 4× supersampled alpha-only template icon; Attention dot at `(30.5, 5.5)` with 1px knockout |
| Docs & Social | `README.md`, `wiki/{Home,Views}.md`, `docs/assets/gitpulse-social-banner.jpg`, `LAUNCH_PLAN.md` | Tracked for post-land screenshot/banner refresh |
| **External Repo Copies** | `/Users/bharath/Code/web/Portfolio` (`public/assets/gitpulse/gitpulse-{mark,icon}.svg`, `GitPulse.jsx:324`, `company.js:60`, `GitPulse.css:17`) & `/Users/bharath/Code/web/bharathvbcr` (`assets/GitPulse.png`, `README.md:165`) | Exact copies of current mark; update in separate PRs post-Phase 2 |

### 4.2 Design Constraints & 3 Concrete Waylode SVG Concepts
All three fit `Logo.svelte` (`viewBox="0 0 32 32"`, `stroke-width="3.4"`, `stroke-linecap="round"`, `stroke-linejoin="round"`), stay inside `[5.5, 26.5]` centerlines (avoiding clipping in `mark` `scale(1.15)`), preserve `>= 5.9u` stroke clearance for 16px (`1.38px` stroke) legibility, use a single traceable `<path pathLength="100">`, and leave `(30.5, 5.5)` clear in `tray.rs` for the Attention dot:

1. **Concept 1 — "Lodestar W" (Parallel Worktree Rails + North Star):**
   - `<path d="M5.8 9.8V19.6L10.8 25.0L16.0 15.2L21.2 25.0L26.2 19.6V9.8" ... />`
   - `<circle cx="16.0" cy="7.2" r="2.7" ... />`
   - *Tradeoff:* Unmistakable `W` monogram, trivial Rust segment rasterizer; less abstract/compass-like.
2. **Concept 2 — "Lodestone Way-Arc" *(Locked Selection)*:**
   - `<path d="M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8" ... />`
   - `<circle cx="23.8" cy="9.0" r="2.8" ... />`
   - *Tradeoff:* Evolves the open magnetic ring into an ascending forward-way crest; min gap `6.1u`; reuses `tray.rs` arc + polyline rasterizer; subtly echoes a `C`/`G` silhouette at 16px.
3. **Concept 3 — "Waystone Diamond" *(Recommended for small-size crispness)*:**
   - `<path d="M22.8 10.2L16.0 5.4L6.2 16.0L16.0 26.6L25.8 16.0H12.8" ... />`
   - `<circle cx="22.8" cy="10.2" r="2.7" ... />`
   - *Tradeoff:* Pure non-letterform faceted compass crystal + inward track; largest negative space (`10.6u` apex-to-meridian) for 16px & 18pt template legibility; departs furthest from curved ring heritage.

### 4.3 Single-Source Pipeline & Non-Loosening Test Upgrade
- **Pipeline (`scripts/generate-brand-assets.mjs` + `--check` CI mode):**
  - Input: ONE canonical spec (`brand/waylode-mark.json` or `brand/waylode-master.svg`).
  - Deterministic cross-platform renderer: `@resvg/resvg-js` + pure-Node PNG/`icns`/`ico` packager (no macOS-only `sips`/`iconutil` variance).
  - **Platform geometry rule:** Generate `src-tauri/icons/icon.icns` from a **macOS Apple HIG squircle-padded** 1024×1024 plate (`~824×824` rounded rect, `~100px` margin, `rx ≈ 185/1024`), while keeping `waylode-icon.svg` full-bleed square for `icon.ico` (`16, 24, 32, 48, 64, 256` @ 32bpp) and Linux `hicolor` PNGs, and `waylode-mark.svg` (`rx="10"`) for favicon + 256×256 `plugins/*/assets/logo.png` (transparent `(0,0)` corners).
- **Contract Tests (Update Geometry Assertions — Never Loosen):**
  - [Logo.contract.test.ts](file:///Users/bharath/Code/devtools/GitPulse/src/lib/components/Logo.contract.test.ts): Update `trunk()` start-anchor regex (`^M[\d.]+ [\d.]+(V|A|L)`), replace RGBA pixel probes at `:173-181` & `:195-206` with exact stroke/node hits **plus** negative-space background probes.
  - `tests/e2e-r1-r4/tier{1..4}*.test.ts`, `browser/*.ts`, `helpers/env.ts:545-574`: Update `CANONICAL_PATH_*`, move current G-ring SHA256 (`1cf0341b1cc7...`) into `RETIRED_LOGO_SHA256S`, and forbid both legacy 6-point polyline and retired G-ring constants in `tray.rs`.
  - `src-tauri/src/desktop/tray.rs:783-799` & `workbench/notifications.rs:1715-1727`: Update `region_ink` + empty-interior/Attention-clearance assertions and retired-SHA256 checks.
- **Phase Placement & Token Boundary:**
  - **Phase 1B (Artwork swap):** Swap artwork + `Logo.svelte` `title="Waylode"` + `tray.rs` glyphs/tooltips, keeping `public/gitpulse-*.svg` filenames, `plugins/gitpulse*` dirs, `gp-*` gradient IDs, `.gitpulse-trace`, and `#gp-boot`.
  - **Phase 2 (Identifier/file rename):** Rename `public/gitpulse-*.svg -> public/waylode-*.svg`, `plugins/gitpulse* -> plugins/waylode*`, and `Logo.svelte` scoped `gp- -> wl-` / `.gitpulse-trace -> .wl-trace`.
  - **Keep internal indefinitely:** `--gp-*` CSS tokens, `.gp-btn`/`.gp-card`, and `#gp-boot` (enforced across the Svelte tree by `design-lint.mjs` and foundation/motion contract tests).

---

## 5. Phase 2: User & Agent Surfaces (Compatibility Aliases)

### 5.1 Exact Binary Inventory (5 Shipped + 2 Worktree Crate = 7 `[[bin]]` Targets)
| Current (`Cargo.toml`) | New Name | Role & Packaging | Compatibility Bridge |
|---|---|---|---|
| `gitpulse` (`src-tauri/Cargo.toml:31`) | `waylode` | GUI (`mainBinaryName`) | Post-install `gitpulse` compat link |
| `gitpulsed` (`:35`) | `wayloded` *(locked)* | Attribution catch-up daemon | `gitpulsed` compat link + cross-lock probe |
| `gitpulse-mcp` (`:39`) | `waylode-mcp` | MCP stdio server & CLI | `gitpulse-mcp` compat link; update `SERVER_NAME` + `check-mcp-install` |
| `gitpulse-hook` (`:43`) | `waylode-hook` | Hook + `term` + `lease` wrapper | `gitpulse-hook` compat link; **fix `lease.rs:505` prefix check** |
| `gitpulse-ptyd` (`:47`) | `waylode-ptyd` | Unix PTY host daemon | Internal (`current_exe().parent()`), update `check-ptyd-release.mjs` |
| `gitpulse-worktree` (`crates/gitpulse-worktree/Cargo.toml:16`) | `waylode-worktree` | Standalone worktree CLI | Compat link in `~/.cargo/bin` / `~/.local/bin` |
| `gitpulse-rustc` (`:20`) | `waylode-rustc` | Rustc wrapper | Compat link in `~/.cargo/bin` / `~/.local/bin` |

**Critical Binary/Shim Pitfalls:**
- **Windows shims:** **Never** create `.cmd`/`.bat` shims (`terminal/mod.rs:1772-1776` documents `cmd.exe` `& | < > ^ %` metacharacter hazards) and **never** rely on Unix-style symlinks (require Admin/Dev Mode). Create post-install **NTFS hardlinks (`std::fs::hard_link`) or `.exe` copies** in user `PATH` (`%USERPROFILE%\.cargo\bin` / `%LOCALAPPDATA%\Programs\Waylode`).
- **`lease.rs:505-515` & `check-mcp-install.mjs:488`:** `identity_serves_lease` checks `stdout.starts_with("gitpulse-hook ")` and `check-mcp-install.mjs` checks `^gitpulse-(mcp|hook)\s+(\S+)$`. Both must accept `waylode-hook ` / `waylode-mcp ` (and `lease.rs` should accept both during transition) so `lease-bin/{cargo,go}` is not deleted.

### 5.2 MCP Tools (66 Total: 55 `gitpulse_*` + 11 `devmap_*`) & Prompts (4)
- **Doc note:** `docs/launch/LAUNCH_PLAN.md:243` says "65 tools" (stale); `src-tauri/src/mcp/tool_input_schemas.snapshot.json` and `docs/FEATURES.md:53` confirm **66 tools** (**55** `gitpulse_*` + **11** `devmap_*`) and **4** prompts (`prompts.rs:41-70`).
- **3 Code Bugs if Legacy Tools Are Accepted Unlisted Naively:**
  1. `src-tauri/src/mcp/mod.rs:3687-3690`: `validate_arguments` returns `Ok(())` when a tool name is missing from `catalog()` — **bypassing all JSON Schema validation** for `gitpulse_*` calls.
  2. `src-tauri/src/mcp/mod.rs:4024,4031`: `with_current_board_status` matches `["gitpulse_add_task", "gitpulse_list_tasks"]` literally, skipping `"inbox" -> "backlog"` canonicalization.
  3. `src-tauri/src/bin/gitpulse-mcp.rs:1239-1244,1265`: `find_tool(catalog, name)` rejects unlisted names in CLI `describe` / `call` with exit 2.
- **Hardened Bridge:** Canonicalize `gitpulse_* -> waylode_*` (and legacy prompt names) at the very entry of `handle_tool_call`, `validate_arguments`, `with_current_board_status`, and `find_tool()`, **and** support opt-in `WAYLODE_MCP_EXPOSE_LEGACY=1` to include the 55 deprecated `gitpulse_*` aliases in `tools/list` for hosts/subagents that reject unlisted `tools/call`.

### 5.3 URIs, Env Vars & Plugins
- **URIs:** `src-tauri/src/mcp/uri.rs:24` (`resources.rs:58-124`) serves **2** fixed docs + **10** RFC 6570 repo templates. Advertise `waylode://`, accept both `waylode://` and `gitpulse://` in `uri::parse` + `complete.rs`, and echo the requested scheme in `contents[].uri`. Update Svelte drag-and-drop MIME `application/x-gitpulse-*` and `links.test.ts:58` (`x-gitpulse://open`); no OS deep-link protocol is registered.
- **Env Vars (107+ `GITPULSE_*` across 3 classes; zero Rust `env!("GITPULSE_*")`):**
  1. *Runtime reads (~80+):* Shared `env_compat::var_compat("WAYLODE_...", "GITPULSE_...")` helper (new wins, legacy fallback).
  2. *Child PTY / hook / agent exports (15+ vars):* **Export BOTH `WAYLODE_*` and `GITPULSE_*`** (`SESSION_TOKEN`, `TERMINAL_SESSION`, `SESSION_ID`, `NOTIFY_SOCKET`, `LEASE_BIN`, `RUN_ID`, `AGENT_KIND`, shell integration) during transition, and **MUST scrub BOTH `WAYLODE_*` and `GITPULSE_*` secret/session vars** in `terminal/mod.rs:1744-1762` `build_pty_command` so child shells never inherit a parent session token!
  3. *Sibling-owned vars:* `GITPULSE_RELAY_TOKEN`, `GITPULSE_PR_GATE_APP_SECRET`, `DEVCOUNCIL_GITPULSE_ROOT`, `MANVI_GITPULSE_MCP_BINARY` stay coordinated via sibling PRs.
- **Plugins:** Rename `plugins/gitpulse/` (20 files) -> `plugins/waylode/`, `plugins/gitpulse-agy/` (4 files) -> `plugins/waylode-agy/`, and `.claude-plugin/marketplace.json` (`"gitpulse"` -> `"waylode"`). Update `tauri.conf.json:89`.

### 5.4 Verified Sibling Surfaces & Wave Rollout Order
*(Counts are tracked/config files outside `.git/`, `target/`, `node_modules/`, worktrees, `.devmap/`, and `.devcouncil/codeintel/`)*:
- **Wave 1 (`GitPulse -> Waylode` Phase 2 PR):** Ship all 7 `waylode*` bins + compat shims, `waylode_*` MCP + `canonical_tool_name` + `WAYLODE_MCP_EXPOSE_LEGACY=1`, `waylode://` dual-parse, `WAYLODE_*` dual-export/scrub, `plugins/waylode*`, and legacy `~/.gemini/config/plugins/gitpulse` cleanup in `agy_plugin.rs`.
- **Wave 2 (`DevCouncil` [64 files] -> `Manvi` [9 files]):**
  - `DevCouncil`: Update `scripts/sync-agent-defs.mjs` (`MCP_TOOL_PREFIXES` adding `mcp__plugin_waylode_waylode__` and `mcp__waylode__`), `rust/devmap-cli/src/agents.rs`, `rust/devmap-query/src/guides.rs` (`GITPULSE_STEP`), `rust/dc-store/src/workbench/agent_guidance.md`, `devmap*` skills, `antigravity/config/`, and `consumers.go` (`DEVCOUNCIL_WAYLODE_ROOT` -> `DEVCOUNCIL_GITPULSE_ROOT`); re-run `sync-agent-defs.mjs`.
  - `Manvi`: Update `manvi/cmd/manvi/serve.go:496` (`MANVI_WAYLODE_MCP_BINARY`/`waylode-mcp` with `gitpulse-mcp` fallback), `manvi/serve/managed.go`, and bump `Manvi/scripts/module-pins.txt` to the new `DevCouncil` commit.
- **Wave 3 (`Waylode` re-vendor -> `Jarvis` [2] -> `gusset` [34] & `Graft` [16]):**
  - Re-vendor `dc-*`/`devmap-*` (`bun run vendor` -> `VENDOR.json`), copy `serve.ops.schema.json` from `Manvi`, run `bun run check:schema-parity` + `bun run mcp:doctor`.
  - Bump `Jarvis/upstream.lock.json`; update `gusset` and `Graft` skills/rules/`.cursor/mcp.json`.
  - Keep `MarkDev` (`markdev-sync`, 4 files) and `Graft` (`graft.gitpulse.*` phone-bridge protocol) untouched until the dedicated relay/protocol phase.
- **Global `~/.claude/CLAUDE.md`:** User-controlled update after Wave 1/2.

---

## 6. Phase 3 & Phase 4: Saved Data, OS Identity, Packaging & Crates

### 6.1 Multi-Hop Bundle ID, WebView & TCC Migration
- **Bundle ID (`dev.vbcr.gitpulse` -> `dev.vbcr.waylode`):**
  - **Ordered multi-source hazard:** [identity_migration.rs](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/src/desktop/identity_migration.rs#L24) currently checks one `LEGACY_IDENTIFIER = "com.gitpulse.desktop"`. For Waylode, it **must** check `["dev.vbcr.gitpulse", "com.gitpulse.desktop"]` **in that exact order** so users who previously migrated to `dev.vbcr.gitpulse` do not inherit stale `com.gitpulse.desktop` data.
  - **Windows & Linux gap:** `identity_migration` is currently gated to `#[cfg(any(target_os = "macos", test))]` (`desktop/mod.rs:7-8`, `lib.rs:169-172`). Extend pre-webview copy to **Windows** (`%LOCALAPPDATA%\<id>\EBWebView`) and **Linux** (`$XDG_DATA_HOME/<id>` + `$XDG_CACHE_HOME/<id>`) before `Builder::run` initializes `"main"`.
  - **macOS TCC resets:** Bundle ID change resets SIP-protected Full Disk Access, Automation, and Notification Center permissions (leaving an orphaned `dev.vbcr.gitpulse` entry). Surface release-note + Health-check guidance.

### 6.2 App Support, Daemon Coexistence, Keychain & Install Identity
- **Profile / Cache / Logs (`tool_config.rs:1820-1870`, `logging.rs:760-784`):**
  - Config/App Support (`GitPulse`/`gitpulse` -> `Waylode`/`waylode`), Cache (`GitPulse/Cache`/`gitpulse`), Logs (`GitPulse/logs`/`gitpulse`).
  - **Split-brain & SQLite corruption guard:** Before copying `GitPulse/ -> Waylode.migrating -> Waylode`, check BOTH `GitPulse/instance.lock`, `GitPulse/gitpulsed/gitpulsed.lock`, and `GitPulse/pty-host/ptyd-v*.lock` so no live pre-upgrade `gitpulsed`/`gitpulse-ptyd`/`gitpulse-mcp` writes to `workbench.sqlite` mid-copy. Copy regular files & dirs only (no symlinks; exclude `*.lock` and `*.sock`), rename atomically, and leave `GitPulse/` untouched for rollback. Have `wayloded` also probe `GitPulse/gitpulsed/gitpulsed.lock` to detect orphaned legacy daemons.
- **Keychain (`src-tauri/src/harness/claude_key.rs:27-31`):** 3-tier lookup `["dev.vbcr.gitpulse.anthropic-api-key", "com.gitpulse.desktop.anthropic-api-key"] -> "dev.vbcr.waylode.anthropic-api-key"`.
- **MCP install ledger capitalization mismatch (`scripts/install-identity.mjs:144-153`):** Writes `mcp-install-v1.json` under `~/.config/GitPulse` on Linux (capitalized), whereas `tool_config.rs:1839` uses `~/.config/gitpulse`. Normalize Phase 3 to lowercase `waylode` on Linux (falling back to `GitPulse/mcp-install-v1.json` if `waylode/mcp-install-v1.json` is absent).

### 6.3 LaunchAgents, Single-Instance, Config, Browser Storage & Agent Installers
- **Hygiene LaunchAgent (`storage/hygiene/background.rs:9-16,75-81`):** Retire **both** `dev.vbcr.gitpulse.hygiene` and `com.gitpulse.hygiene` (accepting legacy `OWNER = "<!-- GitPulse repository hygiene v1 -->"` so `owned_as` doesn't refuse retirement).
- **Login Autostart (`desktop/launch_agent.rs:114-143`, `lib.rs:209-212`):** `tauri_plugin_autostart` keys off `productName`, leaving stale `~/Library/LaunchAgents/GitPulse.plist` (macOS), `HKCU\...\Run\GitPulse` (Windows), and `~/.config/autostart/GitPulse.desktop` (Linux). Migrate enabled state to `Waylode` and remove the old `GitPulse` login entry on all 3 OSes.
- **Single-Instance (`desktop/launch.rs:59,385-392`):** Watch `[own, "dev.vbcr.gitpulse", "com.gitpulse.desktop"]`; on Windows/Linux also check legacy single-instance mutex/socket before startup.
- **Git Config & Browser Storage:**
  - Dual-read `gitpulse.lockedBranch` (`deadbranch.rs:55,75`, `ops.rs:1404`, `mcp/mod.rs:13249`) and `gitpulse.{gate,trainWorktree,gateDiskGb}` indefinitely.
  - **Browser storage actual count (not 8):** **39** fixed `localStorage` keys (36 active + 3 legacy, including `gitpulse_editor_tabs_v1` at `editorTabPersist.ts:17`), **5** dynamic prefix families, and **1** IndexedDB database (`gitpulse-wallpaper`) = **45 storage entries**. Keep all 45 internal keys unrenamed (WebView store copy preserves them automatically). For exported user theme JSONs (`themeSchema.ts:15`), dual-accept `"gitpulse-theme/1"` and `"waylode-theme/1"` on import.
- **Agent Config Installers:**
  - `agy_plugin.rs`: Stage `~/.gemini/config/plugins/waylode` (`.waylode-managed`), and delete `~/.gemini/config/plugins/gitpulse` when `.gitpulse-managed` is present (prevents duplicate hooks/MCP tools).
  - `agent_connect.rs`: Register `SERVER_NAME = "waylode"` and detect/prompt cleanup of legacy `"gitpulse"` entries in `~/.claude.json` and `~/.codex/config.toml` (plus `~/.local/bin/` shims). Note `agent_connect` / `agy_plugin` are Unix-only today.

### 6.4 Windows WiX/NSIS Upgrade Hazard & CI/Release Updates
- **CRITICAL — Duplicate Side-by-Side Windows Installs:** [tauri.conf.json](file:///Users/bharath/Code/devtools/GitPulse/src-tauri/tauri.conf.json#L65-L69) does **not** pin `bundle.windows.wix.upgradeCode`. Tauri derives `UpgradeCode` via UUIDv5 from `productName`, so changing `"GitPulse" -> "Waylode"` generates a brand-new GUID and installs Waylode **beside** GitPulse. Extract the exact `UpgradeCode` from `GitPulse_0.2.0-beta.1_*_en-US.msi` and pin it in `tauri.conf.json`; add an NSIS pre-install hook to uninstall prior `GitPulse` `-setup.exe` installs.
- **CI / Release Scripts:** Update `.github/workflows/release.yml` (`GitPulse.app`, `gitpulse-ptyd`, `gitpulse.cargo.cdx`, CI caches), `scripts/check-release-assets.mjs` (`Waylode_${version}_*` + `Waylode.app/Contents/MacOS/waylode`), `scripts/check-release-version.mjs` (`CRATE_NAME`), and `scripts/release-state.mjs`.

### 6.5 Phase 4: Internal Crates & Repo Rename (Optional, Last)
- `src-tauri/Cargo.toml` (`gitpulse`, `gitpulse_lib`) and **standalone workspace** `crates/gitpulse-worktree/Cargo.toml` (`gitpulse-worktree`, `gitpulse_worktree`, separate `Cargo.lock` + Kani workflow in `.github/workflows/deep-checks.yml`).
- Because CI runs `--locked`, **both** `src-tauri/Cargo.lock` and `crates/gitpulse-worktree/Cargo.lock` must be regenerated in the same commit, along with `windows-test-manifest-contract.test.ts` and `check-shard-coverage.test.ts`.
- Renaming GitHub `bharathvbcr/GitPulse -> waylode/waylode` preserves git redirects until the old name is reused, but requires explicit updates to `scripts/cloud/setup.sh:62` (`RAW_BASE`), `plugins/*/skills/merge-train/failset/go.mod`, `Cargo.toml`/`package.json` repository URLs, and GitHub Actions/OAuth apps.

---

## 7. Verification & "Finished Means Installed" Gate Matrix

| Gate / Phase | Exact Verification Commands & Criteria |
|---|---|
| **Phase 1 & 1B (Brand + Logo)** | `bun run check` • `bun run design:gate` • `bun run check:brand-assets` (new) • `bun run test` (incl. `Logo.contract.test.ts`, `documented-counts-contract.test.ts`, `architecture-docs-contract.test.ts`, `l5-cursor-docs-claims.test.ts`) • `cargo test --manifest-path src-tauri/Cargo.toml --locked` |
| **Phase 2 (Surfaces & Parity)** | `bun run mcp:install` • `bun run mcp:doctor` (passes for `waylode-mcp` / `waylode-hook` & `waylode://server/manifest`) • `bun run check:schema-parity` • `bun run check:ptyd-release` • Compat check: `gitpulse-mcp call gitpulse_insights repo_path=$(pwd)` & `waylode-mcp call waylode_insights repo_path=$(pwd)` both succeed; `build_pty_command` scrubs both `WAYLODE_*` and `GITPULSE_*` secrets |
| **Phase 3 (Upgrade Matrix)** | **From `com.gitpulse.desktop` AND `dev.vbcr.gitpulse`:** (1) Precedence test proves `dev.vbcr.gitpulse` beats stale `com.gitpulse.desktop` on macOS, Windows (`EBWebView`), and Linux (`XDG_DATA_HOME`); (2) Keychain loads `anthropic-api-key`; (3) `Application Support/GitPulse` copies atomically to `Waylode` with live-daemon lock check; (4) `launchctl list` / Win `Run` / Linux autostart show only `Waylode` + `dev.vbcr.waylode.hygiene`; (5) Windows MSI/NSIS upgrade leaves **1** entry in Add/Remove Programs; (6) Antigravity/Claude/Codex have no duplicate `gitpulse` + `waylode` plugins |
| **Phase 4 (Crates & CI)** | `cargo test --manifest-path src-tauri/Cargo.toml --locked` • `cargo test --manifest-path crates/gitpulse-worktree/Cargo.toml --locked` • `bun run check:release` • `release.yml` dry-run |
