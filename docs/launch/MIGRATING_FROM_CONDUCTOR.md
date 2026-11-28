# Migrating from Conductor to Waylode

[Website](https://waylode.vbcr.dev/) · [Try live in browser](https://waylode.vbcr.dev/waylode-app/index.html) (*no install, works on Linux & Windows too*) · [Download `v0.2.0-beta.1`](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1) · [Why Waylode](../WHY_WAYLODE.md)

Conductor announced in November 2026 that it is joining Meta, then withdrew its shutdown notice: its changelog for v0.94.2 (`2026-11-13`) says Conductor isn't shutting down ([conductor.build/changelog](https://www.conductor.build/changelog)). This guide is for people who want a local-first, open-source alternative, or want to run both side by side.

**Waylode** (also called **"Whale"** out loud) is an open-source (`MIT OR Apache-2.0`), local-first macOS desktop workspace and full Git client built in **Rust (Tauri 2)** and **Svelte 5** with zero cloud server dependency.

---

## 1. Conductor -> Waylode concept map

| Conductor concept | Waylode equivalent | Notes |
|---|---|---|
| **Local macOS desktop app** (Electron + Rust server) | **Native Rust core + system webview (Tauri 2)** | No bundled Chromium; tested on macOS (Linux/Windows CI builds exist, untested). |
| **Multi-repo workspace & per-agent worktrees** | **Repository tabs (up to 1,000), Worktree tab stacks, Fleet (`⌘⇧F`)** | Each agent runs in its own git worktree with APFS `clonefile` dependency caches (`node_modules`, `target/`). |
| **Multi-vendor CLIs** (Claude Code, Codex, Cursor, OpenCode) | **6 launchers** (Shell, Claude, Codex, Manvi, Grok, Antigravity) + Browser pane | Uses your own CLIs and subscriptions; discovers unlaunched `claude`, `codex`, `antigravity`, `devcouncil`, and `manvi` sessions automatically. |
| **`.worktreeinclude` & worktree lifecycle scripts** | **`.worktreeinclude` + trust-gated lifecycle hooks** | Opening a repo requires explicit **Trust and Open** in the desktop UI (`.git` path/inode/birth-time bound). |
| **Diff & PR panel** | **History (`Graph · Diff`) + Work (`Overview · Remote · Tasks`)** | Full Git GUI in the same window: lane-solved commit graph, 3-way conflict editor, reflog with undo, GitHub PRs & Actions. |
| **Chaining / multiplayer threads** | **A2A Peer Bus (*Whale Song*) & Mavi structured threads** *(Mavi on `main` / upcoming 0.2.0)* | Cross-vendor `0600`-flocked peer mailbox for Claude Code, Antigravity, Codex and Manvi (`waylode_agent_*`), structured asks (`waylode_inbox_*`), lane messages, **Send to agent**, plus Mavi threads. |
| **PostHog analytics** | **Local-first; no analytics dependency added** | No account, no sign-in, no PostHog/analytics dependency in `Cargo.toml` or `package.json` (full network audit pending). |
| **Conductor Cloud** | **No cloud runners in this release** | Local-first by design; BYOK SSH/remote compute (`Mavi` SSH threads) uses your own hosts, and Labs relay features are not in this release. |

---

## 2. What Waylode adds beyond Conductor

1. **Code Index Integration (*Whale Sonic* · DevMap `StackedStore`)** *(on `main` / upcoming 0.2.0)*:
   Conductor relies on search/grep inside each worktree. Waylode links DevMap in-process via `StackedStore` (`stack_lease.json` + sealed `0444` `base.sqlite` + lazy Tree-sitter `delta.sqlite` overlays, for the worktrees it creates for agents) so both you and your agents get layered blast radius with `unattributed`-line honesty, backward regression suspects, confidence-tiered dead symbols (`0.9` act-on vs `0.4` unconfirmed), affected tests with scoped `CI:local`, and `:` / `::` / `~` workspace symbol search.
2. **Cross-Vendor A2A (*Whale Song*) & 3-Layer Collision Radar (*Whale Pod*)**:
   Conductor's own docs warn that agents in the same workspace can edit the same files. Waylode detects unlaunched external sessions across vendors and gives agents a **3-layer collision radar** to check on demand (`waylode_worktree_radar`): (1) declared/observed file and symbol claims (`waylode_worktree_claim`, advisory unless enforce mode is on), (2) 1-hop CSR call-graph semantic overlap, and (3) ODB `git merge-tree` conflict prediction on committed work. A `PreToolUse` hook also asks before an edit to a file another worktree has uncommitted changes to.
3. **Load & Agent Scheduler**:
   Running 6 `cargo build` or test suites simultaneously freezes laptops. Waylode refuses an agent launch under memory pressure (or any input you set to refuse), with a human-only **Admit anyway** override, and adds build slot leases with queue position on the terminal pill, one-click idle session parking, macOS `PRIO_DARWIN_PROCESS` background QoS isolation, a shared git spawn gate, live CPU/GPU/memory/thermal/storage indicators, and APFS `clonefile` dependency caches (one cloned `target` held `1.8 MB` private bytes out of `66 GB` logical vs a `26.7 GB` cold build, measured once on Waylode's own repo; `worktrunk` is an honorable peer for CLI CoW caches).
4. **Pure-ODB Merge Train & `git replay` Stack Restacking** *(merge train upcoming 0.2.0; not yet on `main`)*:
   Instead of merging 4–10 finished worktrees by hand, Waylode batches landings through an AIMD Land Queue (`cmd_worktree_merge_batch` / `waylode-worktree land-batch` via `ObjectQuarantine` `git merge-tree --stdin`; each branch verified before submit, the combined batch is not re-tested), stores unclean merges as pure-ODB Conflict Proposals (`refs/gitpulse/proposals/`, via the `waylode-worktree` CLI), and restacks branch hierarchies via `git replay` with a disjoint-dirty fast path.
5. **Full Native Git GUI, Verification-Gated Task Board & Manvi Guards**:
   - **5 views, 11 sections**: Work (`Overview · Remote · Tasks`), Mavi, Code (`Explorer · Map`), History (`Graph · Diff`), and Insights (`Pulse · Coverage · Health · Storage` — **13 Health sections**, **7 parsed coverage formats**), plus persistent `waylode-ptyd` terminal splits and a localhost Browser pane.
   - **Agent-writable task board**: 5-column cross-repo Kanban (up to 2,000 cards per fill); agents file out-of-scope bugs via duplicate-gated `waylode_add_task`, and `waylode_complete_task` requires passed Manvi verification on `HEAD` before moving to `Done`.
   - **Safety primitives**: Manvi's 8-status command gate, `HostScope` checkout fences, `dc-redact` secret gate on commits Waylode makes, CAS pre-op undo snapshots (`refs/gitpulse/ops/`), Turn Checkpoints (`refs/gitpulse/turns/`), WAL provenance ledger, and **66 MCP tools** (`55 waylode_*` + `11 devmap_*`), **4 prompts**, **12 resource facets**, and `330+` native commands.

---

## 3. Honest limits before you switch

- **macOS tested only:** Windows and Linux release CI builds exist, but have not been tested yet. (You can still inspect the UI from any OS in the [live browser demo](https://waylode.vbcr.dev/waylode-app/index.html).)
- **No cloud runners in this release:** If you specifically relied on Conductor Cloud VMs, Waylode runs locally on your Mac (with SSH support for Mavi Claude threads on your own hosts).
- **Ad-hoc signed beta:** For `v0.2.0-beta.1` on macOS, drag `Waylode.app` to `/Applications` and run `xattr -dr com.apple.quarantine /Applications/Waylode.app` if you choose to trust the build.
- **Not an OS sandbox:** Worktrees isolate checkouts, not OS syscalls; Antigravity sessions get no `PreToolUse` guard from Waylode; `dc-redact` checks commits Waylode makes, not an agent's own raw `git commit`.
- **Telemetry posture:** Local-first with no account required and no PostHog or analytics dependency added in `Cargo.toml` or `package.json`, though a full network audit is still pending (git remotes, GitHub, and your configured model CLIs still use the network).

---

## 4. Five-minute switch

1. **Try before installing (optional):** Open the [live browser demo](https://waylode.vbcr.dev/waylode-app/index.html) or start with the **Headless MCP Wedge**: link `waylode-mcp` (66 tools) and `waylode-hook` into Claude Code or Codex to get collision radar and *Whale Sonic* code-graph queries in your existing CLI.
2. **Install the macOS beta:** Download [`v0.2.0-beta.1`](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1), move `Waylode.app` to `/Applications`, clear quarantine if trusted, and walk through **Help → Privacy & Permissions…**.
3. **Open your existing repositories:** Your existing `.worktreeinclude`, `claude`, and `codex` logins work as-is. Click **Trust and Open** on each repo, then run **Settings → Agents → Connect your agent** to wire `waylode-mcp` and `waylode-hook` into `~/.local/bin`.
4. **Launch parallel work:** Open **Tasks** or **Dock → Claude / Codex** to spawn worktrees with APFS CoW caches, watch **Agents (`⌘⇧A`)** and **Health → Host load**, and land finished lanes via **Work → Overview** or the **Land Queue**.
