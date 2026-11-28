# Waylode features

[Documentation index](README.md) · [Getting started](GETTING_STARTED.md) · [Installation](INSTALLATION.md) · [Why Waylode](WHY_WAYLODE.md) · [Agents guide](AGENTS_GUIDE.md) · [View reference](VIEW_REFERENCE.md)

This page lists what Waylode does, by area. Everything outside the [Coming](#coming) section is in the
code on `main` today. For controls, prerequisites and limits of each view, see the
[view reference](VIEW_REFERENCE.md). Waylode is tested on macOS. Windows and Linux builds are produced by
release CI but have not been tested, so some features below may behave differently or be missing there.

Waylode has 5 views and 11 sections: **Work** (Overview, Remote, Tasks), **Mavi** (a single pane: agent conversations and their changes), **Code** (Explorer, Map),
**History** (Graph, Diff) and **Insights** (Pulse, Coverage, Health, Storage). Fleet, the Agents plane and
the terminal dock sit beside them. The native layer exposes 330+ commands to the frontend.

Contents: [1. One workspace](#1-one-workspace-for-every-project) · [2. Agent control plane](#2-agent-control-plane) ·
[3. Agent guards](#3-agent-guards-with-manvi) · [4. Code intelligence](#4-devcouncil-and-devmap-code-intelligence) ·
[5. Task board](#5-task-board) · [6. Remote and GitHub](#6-remote-and-github) · [7. Git client](#7-git-client) ·
[8. Files and code](#8-files-and-code) · [9. Insights](#9-insights) · [10. Load scheduler](#10-load-scheduler) ·
[11. Terminal](#11-terminal) · [12. Customization](#12-customization) · [13. Platform](#13-platform) ·
[Coming](#coming)

## Signature Differentiators

Jump straight to the six capabilities that, combined, distinguish Waylode from the ADEs, editors, terminals, Git clients and agent tools checked on `2026-11-27` and `2026-11-28` (see [Why Waylode](WHY_WAYLODE.md)). The merge train is upcoming in 0.2.0, not yet on `main`:

| Differentiator | Core Shipped Mechanisms | Section |
| --- | --- | --- |
| **Code Index Integration (*Whale Sonic*)** | `StackedStore` (`stack_lease.json` + sealed `0444` `base.sqlite` + lazy Tree-sitter `delta.sqlite`), layered blast radius + `unattributed` honesty, regression suspects, `0.9`/`0.4` dead symbols, affected tests + scoped `CI:local` | [4. Code intelligence](#4-devcouncil-and-devmap-code-intelligence-whale-sonic) |
| **Cross-Vendor A2A & 3-Layer Radar (*Whale Song* / *Whale Pod*)** | Unlaunched live session discovery, cross-vendor peer mailbox (`waylode_agent_*`), structured asks (`waylode_inbox_*`), lane bus, **Send to agent**, 3-layer CSR/ODB collision radar + `PreToolUse` guard | [2. Agent control plane](#2-agent-control-plane) |
| **Pure-ODB Merge Train & `git replay` Stacks** | AIMD batched Land Queue (`merge-tree --stdin`; branches verified before submit, the combined batch is not re-tested; upcoming 0.2.0, not yet on `main`), pure-ODB Conflict Proposals (`refs/gitpulse/proposals/`, created by the `waylode-worktree` CLI; the app opens them), `git replay` stack restacking with disjoint-dirty fast path | [7. Git client](#7-git-client) |
| **Load & Agent Scheduler** | Agent launches refused under memory pressure (or any input you set to refuse), with a human-only **Admit anyway**; heavy cargo/go builds queue for build slots, one-click idle session parking, macOS QoS isolation + shared git spawn gate, live host-load strip + APFS CoW caches | [10. Load scheduler](#10-load-scheduler) |
| **Verification-Gated Task Board** | Five-column cross-repo Kanban (up to 2,000 cards per fill), launch-from-card tracking, `waylode_add_task` dup-gated + `expected_revision`, `Done` gated on passed `HEAD` verification | [5. Task board](#5-task-board) |
| **Manvi Guards, Provenance & Native Git GUI** | 8-status command gate, `HostScope` fences, desktop-only `.git` inode-bound trust, `dc-redact`, CAS pre-op undo, Turn Checkpoints, WAL provenance, persistent `waylode-ptyd` + Browser pane, `~41 ms` 100k-commit lane solve | [3. Agent guards](#3-agent-guards-with-manvi) · [7. Git client](#7-git-client)

## 1. One workspace for every project

- **Repository tabs**, up to 1,000 open, with collapsible groups that survive a restart. At most 24
  repositories are watched at once (the rest are parked; the native layer allows 32 watch sessions, and the
  difference is room for evictions), and 24 recent
  repositories are remembered. Source: `MAX_OPEN_TABS`, `MAX_WATCHES` and `MAX_RECENT_REPOS` in the code.
- **Worktree tab stacks**: every checkout of a repository shares one tab, with a switcher.
- **Fleet** (`⌘⇧F`, part of *Whale Pod* · Fleet & Agents Tree): a grid that compares repositories on churn, sync and health. Every cell is a value,
  *not scanned* or *could not read*; totals name what they could not count.
- **Work rows**: Work → Overview shows one row per place work is happening, blocked worktrees first.
- **Boards across repositories**: the global and saved-workspace task boards, from the **Tasks** button
  beside Fleet.
- **Repository logos** on tabs, with a picker, and a file explorer that can dock in the sidebar.

## 2. Agent control plane

- **Launchers**: six (Shell, Claude, Codex, Manvi, Grok, Antigravity) plus a Browser tab (macOS), from the dock.
  Each launcher has a default model, effort, advisor and commit model in **Settings → Agents**.
- **Live session detection (*Whale Pod*)**, including sessions Waylode did not launch (Claude, Codex, Antigravity,
  DevCouncil and Manvi; off macOS, unobservable kinds are disclosed via `UNOBSERVABLE_KINDS` rather than implied empty), over MCP as `waylode_sessions`. The Agents plane (`⌘⇧A`) draws the checkouts,
  terminals and task attempts your repositories hold.
- **Agent tree**: parent and child sessions nested, collapsible and filterable.
- **A worktree per agent**: APFS `clonefile` copy-on-write `node_modules` and `target` caches (measured once on Waylode's own repo, 2026-11-14: one cloned `target` held 1.8 MB private bytes out of 66 GB logical vs 26.7 GB cold build), `.worktreeinclude`, and the
  DevMap `StackedStore` (`stack_lease.json` + sealed `0444` `base.sqlite` + lazy Tree-sitter `delta.sqlite` overlays) for worktrees Waylode creates for agents, seeded from the main checkout (worktrees made in the Worktrees panel or by `waylode_worktree_create` get their own copy unless zero-copy is turned on) (`waylode_worktree_overlay_sync`; legacy `base_store_ref.json` / `overlay.json` formats are retired and refused). The `claude --worktree` flow goes through Waylode's hooks.
- **Agent-to-agent communication (*Whale Song* · A2A Peer Bus)**: cross-vendor peer registry and mailbox (`waylode_agent_register`, `_peers`, `_send` with 16 KiB cap and hop limits, `_inbox`, `_wait` with bounded kqueue waits up to 45s), structured asks (`waylode_inbox_post`, `_read`, `_withdraw`), worktree lane messages (`waylode_worktree_send_message`, `waylode_worktree_inbox`), and **Send to agent** (`0600` flock atomic mailbox + `waylode-hook deliver`).
- **3-layer collision radar, file claims and the collision guard hook**: Layer 1 declared/observed file and symbol claims (`waylode_worktree_claim` / `_release`), Layer 2 1-hop CSR call-graph semantic conflicts, and Layer 3 ODB `git merge-tree` check of committed work, which agents run on demand (`waylode_worktree_radar`). Separately, the `PreToolUse` collision hook asks before an edit to a file another worktree has uncommitted changes to; claims held in other worktrees are notices unless `shared_checkout_guard` is set to enforce in a shared checkout.
- **Follow agent**: a split pane that follows an agent's file edits live.
- **Turn checkpoints** (`refs/gitpulse/turns/`, opt-in, `waylode_turn_checkpoints` with omissions disclosure), the **ledger** and **provenance** (`waylode_ledger_events`, `waylode_provenance`, `waylode_landed`), and **merge-conflict prediction** between worktrees in a quarantine (`waylode_merge_conflicts`).
- **Notifications**, an attention inbox and a menu-bar status glyph.
- **Managed runs that survive quitting** (hosted `manvi serve`), on Unix.
- **Mavi threads**: structured Claude Code conversations (local or SSH) with inline approvals, live per-turn changed files (`MaviFileDiff`), explicit `AskUserQuestion` choice cards (unanswered or declined questions are never treated as consent), child terminal tab cards, and cleanup via `cmd_mavi_close_thread_children` on forget.
- **MCP server for agents**: 66 tools (55 `waylode_*` — with legacy `gitpulse_*` aliases accepted and `WAYLODE_MCP_EXPOSE_LEGACY=1` to advertise them — and 11 `devmap_*`, or 55 with `WAYLODE_MCP_NO_DEVMAP=1`), 4 prompts and 12 `waylode://`
  resource facets (plus `gitpulse://` alias), with plugins for Claude Code, Codex, Cursor and Antigravity. See the [agents guide](AGENTS_GUIDE.md).
- **Agent reads of shells you opened** (opt-in, off by default): `allow_agent_shell_reads` lets MCP callers
  read secret-scrubbed output from your shell sessions in the same trusted repository.

## 3. Agent guards with Manvi

- **Command gate** on every agent shell command where the hooks are registered. Each decision is one of eight
  statuses: Allowed, Demoted, Granted, Widened, Degraded, Warned, Blocked or Unchecked
  (`contracts/verdict.schema.json` defines them and the order they are evaluated in).
- **Scope fences**: with a task scope bound, writes into another checkout (`sed -i`, `tee`, `rm`, `mv` and
  similar) are refused.
- **Per-repository trust**: only the desktop can grant it; MCP cannot.
- **Secret gate** on commits and pushes Waylode makes (`dc-redact`). It does not catch an agent's own `git commit`.
- **Pre-operation undo snapshots** (`refs/gitpulse/ops/`) with a compare-and-swap-checked restore, from HEAD moves.
- **Signal guard**: agents cannot kill Waylode.
- **Limits**: Antigravity has no `PreToolUse` guards from Waylode, and a worktree is not an OS sandbox.

## 4. DevCouncil and DevMap code intelligence (*Whale Sonic*)

- **Stacked worktree code graph (`StackedStore`):** `stack_lease.json` + sealed (`0444`) `base.sqlite` snapshot + lazy per-worktree Tree-sitter `delta.sqlite` overlays (zero write-on-read; `8 MiB` per-file / `10,000` dirty-file overlay bounds).
- **Map**: navigator, files, symbols, subsystems, docs, doc graph and links (Code → Map).
- **Blast radius** with a rung filter and `unattributed`-line disclosure (`impact_layered` / `devmap_impact` / `devmap_explore`; History → Diff).
- **Affected tests**, and **CI:local** scoped to them, failing closed to the full suite when index freshness degrades and saying so.
- **Regression suspects** via backward outbound call walk and line-level blame (`waylode_codeintel_suspects`; History → Graph → Suspects).
- **Confidence-tiered dead symbols** (`0.9` act-on vs `0.4` unconfirmed, with exemptions), **clones** (`signed_symbols` / `unsigned_symbols` coverage honesty), **trace**, and workspace-wide symbol search through the palette (`:` active repo, `::` all workspace repos, `~` TF-IDF name ranking).
- **Unsaved-edit preview (`devmap preview`):** `waylode-mcp` is intentionally parser-free, so its `devmap_preview` tool returns `available: false` and directs callers to `devmap preview` or the standalone `devmap mcp` server.
- **DevCouncil suite panel and installer** in Settings.
- **Explorer search** through `dcgrep` when a compatible binary is installed (falling back to `git grep --untracked` + `git ls-files`).

## 5. Task board

- Five columns (Backlog, Ready, In progress, Review, Done), swimlanes, WIP limits and filters. Filling a column
  pages in cards until the board holds 2,000, a budget its columns share.
- Cross-repository and saved-workspace boards.
- Quick add, compose from notes, due dates, relations (blocks, depends, related, duplicate) and merging duplicates.
- **Manvi assist**: refine, suggest and field locks.
- **Launch an agent from a card**: model, effort and advisor per launch, one tracked attempt per run, in its own worktree.
- **Verification chips.** `waylode_complete_task(done)` needs a passed Manvi verification for the worktree's
  current head; otherwise the card goes to Review.
- **GitHub issue to task**, and an archive dock with a Deleted view.
- **Agents file tasks over MCP**, with duplicate gating, `expected_revision` protection and
  `waylode_import_tasks` for `tasks/*.md` briefs.

## 6. Remote and GitHub

- **Pull requests**: view, review, approve, merge, check out, open as a worktree, create.
- **Issues**, and issue to task.
- **Actions**: workflows, runs, dispatch, rerun, cancel and timelines.
- **Releases** and tag publishing with a preflight.
- **Firebase App Hosting** rollouts.
- **GitHub avatars** on the graph.
- **Dependabot and code-scanning alerts**, in Health.

## 7. Git client

- **Commit graph** with a lane solver, avatars and hover cards. A synthetic 100,000-row history takes about
  41 ms to lane-solve and about 58 ms to assemble (minimum timings; [performance notes](PERFORMANCE.md)).
- **Branch stack tree**: stacked branches as a tree, with `git replay` ODB stack restacking (parent-before-child with disjoint-dirty fast path and rollback).
- **AIMD batched Land Queue / Merge Train**: `cmd_worktree_merge_batch` / `waylode-worktree land-batch` using `ObjectQuarantine` `git merge-tree --stdin`, SQLite WAL FIFO tickets and green-ref hooks. Each branch is verified before it is submitted; the combined batch is not re-tested (the scratch step, `verify_in_scratch`, is off by default and runs no tests). Upcoming 0.2.0; the app's batch-landing UI is not yet on `main`.
- **Pure Git ODB Conflict Proposals**: persists conflict resolutions under `refs/gitpulse/proposals/` with `ProposalSweeper` and `decide_land` safety gating (`cmd_load_conflict_proposal` / `cmd_build_resolved_proposal_tree` / `waylode-worktree proposals`).
- **Diff**: split or unified, word diff, image diff, a History rail, and Shift-click range compare.
- **Hunk and line staging**, a commit composer, and Apple Intelligence commit drafting on macOS.
- **3-way conflict editor**, which leads Overview while a merge is parked.
- **HEAD moves (reflog)** with undo.
- **Cherry-pick, revert, reset, interactive rebase, stash, remotes and submodules.**
- **Branch and tag cleanup** with tip backups and one-click restore.

## 8. Files and code

- File tree with git status, a virtualized viewer, and editing with compare-and-save.
- Markdown through MarkDev, plus media and hex viewers.
- **Blame** toggle with a code-age timeline.
- **Repository-wide content search** (Code → Explorer, the contents toggle in the search field) that reports capped or cancelled results.
  There is no separate chord for it: `⌘F` searches the open file in Code, and searches commits elsewhere.

## 9. Insights

- **Pulse**: heatmap, rhythm, punch card, code age, DORA, hotspots, knowledge map and period compare.
- **Coverage**: 7 formats parsed (LCOV, Cobertura, Go cover, Istanbul, SimpleCov, JaCoCo, Clover). A
  coverage.py `.coverage` database is detected but not parsed; the panel asks you to export `coverage.xml` or `lcov.info`.
- **Health**: 13 sections (summary, plan, vulnerabilities, Dependabot, code scanning, secrets, issues,
  outdated packages, packages, code graph, dead code, terminal incidents and host load).
- **Secrets scan** runs in-process; Kingfisher is an optional second opinion on an explicit Rescan.
- **Storage and hygiene**: commit-graph write, a cache inventory and a build cleaner.

## 10. Load scheduler

- **Memory-pressure governor**: refuses an agent launch under critical memory pressure or compressor thrash,
  or under any other input you set to refuse, and says which; a human-only **Admit anyway** overrides it.
- **Heavy-command gate** in the hook, and a shared spawn budget for git.
- **Build leases**: slots and a queue for heavy builds, a wrapper on the agent's `PATH`, and the queue
  position on the terminal pill.
- **Idle agent sessions** are classified and can be parked with one click.
- **Resource profile** that sizes GPU contexts and caches to the machine.
- **Host-load section** in Health, a pressure dot in the status bar, and a strip with CPU, GPU, memory,
  thermal and storage indicators and a live-graph popover.

## 11. Terminal

- A dock beneath every view (`` Ctrl+` ``), per repository and expandable.
- Tabs, **nested splits** (rows and columns, up to 8 panes) and split groups, tab and session search, Find
  and text size.
- Sessions grouped by repository in the session list.
- **Sessions that survive reload and app quit** (`waylode-ptyd`, protocol 5), with transcript recovery, hooked/screen permission prompt detection (`needs_input`), and resume, on Unix.
- **Agent child spawn flags & shell-scoped port probe**: `waylode_terminal_spawn` accepts `model`, `mode` (`inspect`/`ask`/`edit`/`auto_review`/`preapproved`), and repo-relative `settings` for agent children; dev servers launched through an agent's shell remain visible in port probing while agent CLI listeners stay excluded.
- WebGL rendering, synchronized output and Unicode 11. Command blocks (OSC 133).
- **Browser pane** for localhost dev servers, which agents can open (macOS).
- Select-to-copy, OSC 52, "Copy last command output", "Run in new split shell", launcher icons and per-tab counts.
- Status-bar terminal pill and a shortcut guide.

## 12. Customization

- 21 theme presets (4 Cobalt **Way-Arc** brand presets — *Waylode Cobalt*, *Waylode Lodestar*, *VBCR Oxblood*, *Waylode Gilded* — plus Standard, Developer, and Sci-Fi), a My themes shelf (24), and import and export.
- 3 brand logo styles (*Way-Arc*, *Folded Ribbon*, *Lode Compass*) and 4 brand logo palettes (*Cobalt*, *Lodestar*, *Oxblood*, *Gilded*).
- 11 macOS wallpapers plus your own image, glass appearance, and opt-in wallpaper motion.
- **Command palette** with 8 modes (`>` `/` `%` `#` `@` `:` `::` `?`) opened with `⌘K`.
- 9 settings sections with search, and a status bar that is full, minimal or hidden.
- A product tour and a setup wizard.

## 13. Platform

- **Tested on macOS only.** Release CI builds five targets: macOS universal, Linux x64 and arm64, and
  Windows x64 and arm64. The Linux and Windows builds have not been tested. macOS builds are ad-hoc signed,
  not notarized, and Windows builds are unsigned.
- On Windows, without a process scan, some live agent sessions show as unknown rather than zero.
- Five core binaries: `waylode`, `waylode-mcp`, `waylode-hook`, `waylode-ptyd` and `wayloded` (plus standalone `waylode-worktree` and `waylode-rustc`, with legacy `gitpulse*` compatibility links).
- Local-first: repository and task state stay on your machine. Git remotes, GitHub, release checks you opt
  into and configured agent providers use the network; see [Security](SECURITY.md).

## Coming

These are not in the app today.

- Load governor stages 3 and 4: a pressure verdict in the command gate and a load line in the session brief.
- A cross-repository execution queue, two-way GitHub issue sync and DAG task planning.
- Notifications on Windows and Linux, and CI and due-date reminders.
- Browser pane element-to-source mapping, then a Chromium driver.
- User-defined keyboard shortcuts and internationalization groundwork.
- "Ask Manvi to organise the board".
- Posting `devcouncil verify` results to GitHub.
- Windows and Linux test passes, so each can move from "built" to "tested". Notarized macOS builds.
- **Waylode Primary (`MIT OR Apache-2.0`, public `bharathvbcr/GitPulse`):** All local ADE features, local DevCouncil (`devmap`, `dcstore`, `dcverify`, `dcgrep`, `dcredact`, public OSS PR Gate), local Manvi (harness, policy enforcement, JSON audit export, solo AI spend limits), local MarkDev/Graft, and **BYOK** compute (SSH runners, `cloud_vms`, user model keys) are 100% free with zero sign-in required.
