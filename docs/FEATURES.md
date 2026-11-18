# GitPulse features

[Documentation index](README.md) · [Getting started](GETTING_STARTED.md) · [Installation](INSTALLATION.md) · [Why GitPulse](WHY_GITPULSE.md) · [Agents guide](AGENTS_GUIDE.md) · [View reference](VIEW_REFERENCE.md)

This page lists what GitPulse does, by area. Everything outside the [Coming](#coming) section is in the
code on `main` today. For controls, prerequisites and limits of each view, see the
[view reference](VIEW_REFERENCE.md). GitPulse is tested on macOS. Windows and Linux builds are produced by
release CI but have not been tested, so some features below may behave differently or be missing there.

GitPulse has 4 views and 11 sections: **Work** (Overview, Remote, Tasks), **Code** (Explorer, Map),
**History** (Graph, Diff) and **Insights** (Pulse, Coverage, Health, Storage). Fleet, the Agents plane and
the terminal dock sit beside them. The native layer exposes 330+ commands to the frontend.

Contents: [1. One workspace](#1-one-workspace-for-every-project) · [2. Agent control plane](#2-agent-control-plane) ·
[3. Agent guards](#3-agent-guards-with-manvi) · [4. Code intelligence](#4-devcouncil-and-devmap-code-intelligence) ·
[5. Task board](#5-task-board) · [6. Remote and GitHub](#6-remote-and-github) · [7. Git client](#7-git-client) ·
[8. Files and code](#8-files-and-code) · [9. Insights](#9-insights) · [10. Load scheduler](#10-load-scheduler) ·
[11. Terminal](#11-terminal) · [12. Customization](#12-customization) · [13. Platform](#13-platform) ·
[Coming](#coming)

## 1. One workspace for every project

- **Repository tabs**, up to 1,000 open, with collapsible groups that survive a restart. At most 24
  repositories are watched at once (the rest are parked; the native layer allows 32 watch sessions, and the
  difference is room for evictions), and 24 recent
  repositories are remembered. Source: `MAX_OPEN_TABS`, `MAX_WATCHES` and `MAX_RECENT_REPOS` in the code.
- **Worktree tab stacks**: every checkout of a repository shares one tab, with a switcher.
- **Fleet** (`⌘⇧F`): a grid that compares repositories on churn, sync and health. Every cell is a value,
  *not scanned* or *could not read*; totals name what they could not count.
- **Work rows**: Work → Overview shows one row per place work is happening, blocked worktrees first.
- **Boards across repositories**: the global and saved-workspace task boards, from the **Tasks** button
  beside Fleet.
- **Repository logos** on tabs, with a picker, and a file explorer that can dock in the sidebar.

## 2. Agent control plane

- **Launchers**: six (Shell, Claude, Codex, Manvi, Grok, Antigravity) plus a Browser tab (macOS), from the dock.
  Each launcher has a default model, effort, advisor and commit model in **Settings → Agents**.
- **Live session detection**, including sessions GitPulse did not launch (Claude, Codex, Antigravity,
  DevCouncil and Manvi), over MCP as `gitpulse_sessions`. The Agents plane (`⌘⇧A`) draws the checkouts,
  terminals and task attempts your repositories hold.
- **Agent tree**: parent and child sessions nested, collapsible and filterable.
- **A worktree per agent**, with copy-on-write `node_modules` and `target` caches, `.worktreeinclude`, and the
  DevMap store seeded from the main checkout. The `claude --worktree` flow goes through GitPulse's hooks.
- **Agent-to-agent communication**: the peer inbox (`gitpulse_agent_register`, `_peers`, `_send`, `_inbox`,
  `_wait`), worktree lane messages, and **Send to agent** from the UI.
- **Collision radar, file claims and the collision guard hook**: warnings before two agents' edits meet.
- **Follow agent**: a split pane that follows an agent's file edits live.
- **Turn checkpoints** (`refs/gitpulse/turns/`, opt-in), the **ledger** and **provenance** (which session
  changed what, and how a commit landed), and **merge-conflict prediction** between worktrees in a quarantine.
- **Notifications**, an attention inbox and a menu-bar status glyph.
- **Managed runs that survive quitting** (hosted `manvi serve`), on Unix.
- **MCP server for agents**: 66 tools (55 `gitpulse_*` and 11 `devmap_*`), 4 prompts and `gitpulse://`
  resources, with plugins for Claude Code, Codex, Cursor and Antigravity. See the [agents guide](AGENTS_GUIDE.md).
- **Agent reads of shells you opened** (opt-in, off by default): `allow_agent_shell_reads` lets MCP callers
  read secret-scrubbed output from your shell sessions in the same trusted repository.

## 3. Agent guards with Manvi

- **Command gate** on every agent shell command where the hooks are registered. Each decision is one of eight
  statuses: Allowed, Demoted, Granted, Widened, Degraded, Warned, Blocked or Unchecked
  (`contracts/verdict.schema.json` defines them and the order they are evaluated in).
- **Scope fences**: with a task scope bound, writes into another checkout (`sed -i`, `tee`, `rm`, `mv` and
  similar) are refused.
- **Per-repository trust**: only the desktop can grant it; MCP cannot.
- **Secret gate** on commits and pushes GitPulse makes (`dc-redact`). It does not catch an agent's own `git commit`.
- **Pre-operation undo snapshots** (`refs/gitpulse/ops/`) with a compare-and-swap-checked restore, from HEAD moves.
- **Signal guard**: agents cannot kill GitPulse.
- **Limits**: Antigravity has no `PreToolUse` guards from GitPulse, and a worktree is not an OS sandbox.

## 4. DevCouncil and DevMap code intelligence

- **Map**: navigator, files, symbols, subsystems, docs, doc graph and links (Code → Map).
- **Blast radius** with a rung filter (History → Diff).
- **Affected tests**, and **CI:local** scoped to them, falling back to the full suite and saying so.
- **Regression suspects** (History → Graph → Suspects).
- **Dead symbols, clones, trace** and workspace-wide symbol search through the palette (`:` and `::`).
- **DevCouncil suite panel and installer** in Settings.
- **Explorer search** through `dcgrep` when a compatible one is installed.

## 5. Task board

- Five columns (Backlog, Ready, In progress, Review, Done), swimlanes, WIP limits and filters. Filling a column
  pages in cards until the board holds 2,000, a budget its columns share.
- Cross-repository and saved-workspace boards.
- Quick add, compose from notes, due dates, relations (blocks, depends, related, duplicate) and merging duplicates.
- **Manvi assist**: refine, suggest and field locks.
- **Launch an agent from a card**: model, effort and advisor per launch, one tracked attempt per run, in its own worktree.
- **Verification chips.** `gitpulse_complete_task(done)` needs a passed Manvi verification for the worktree's
  current head; otherwise the card goes to Review.
- **GitHub issue to task**, and an archive dock with a Deleted view.
- **Agents file tasks over MCP**, with duplicate gating, `expected_revision` protection and
  `gitpulse_import_tasks` for `tasks/*.md` briefs.

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
- **Branch stack tree**: stacked branches as a tree, with restack parent-before-child and rollback.
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
- **Repository-wide content search** (Code → Explorer, Contents mode) that reports capped or cancelled results.
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

- **Memory-pressure governor**: admits, queues or sheds agent launches under host memory pressure, with a
  human **Admit anyway**. It acts on memory pressure only.
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
- **Sessions that survive reload and app quit** (`gitpulse-ptyd`), with transcript recovery and resume, on Unix.
- WebGL rendering, synchronized output and Unicode 11. Command blocks (OSC 133).
- **Browser pane** for localhost dev servers, which agents can open (macOS).
- Select-to-copy, OSC 52, "Copy last command output", "Run in new split shell", launcher icons and per-tab counts.
- Status-bar terminal pill and a shortcut guide.

## 12. Customization

- 17 theme presets (Standard, Developer, Sci-Fi), a My themes shelf (24), and import and export.
- 11 macOS wallpapers plus your own image, glass appearance, and opt-in wallpaper motion.
- **Command palette** with 8 modes (`>` `/` `%` `#` `@` `:` `::` `?`) opened with `⌘K`.
- 9 settings sections with search, and a status bar that is full, minimal or hidden.
- A product tour and a setup wizard.

## 13. Platform

- **Tested on macOS only.** Release CI builds five targets: macOS universal, Linux x64 and arm64, and
  Windows x64 and arm64. The Linux and Windows builds have not been tested. macOS builds are ad-hoc signed,
  not notarized, and Windows builds are unsigned.
- On Windows, without a process scan, some live agent sessions show as unknown rather than zero.
- Five binaries: `gitpulse`, `gitpulse-mcp`, `gitpulse-hook`, `gitpulse-ptyd` and `gitpulsed`.
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
- **Labs preview (not in this release):** remote runners over ssh, an encrypted secrets vault, snapshot
  import, device and settings sync, and a team kill switch exist on a development branch behind a build
  flag. No release build enables them. Issue-tracker integrations beyond GitHub are not built.
