# Views

GitPulse has **four views**. Each holds sections (lenses) on one subject. Tasks, Fleet and the terminal dock are workspace surfaces outside those four views.

**Product stack.** DevCouncil is components and modules. Manvi wraps them. Work → Policy and Tasks go through Manvi; Code → Map uses DevCouncil's `devmap` module.

![Files view: explorer, syntax-highlighted viewer, and uncommitted status](https://raw.githubusercontent.com/bharathvbcr/GitPulse/main/docs/assets/screenshot-files.png)

The complete catalog lives in [docs/FEATURES.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/FEATURES.md). This page is the map.

## Work (`F10`)

Everything in flight, keyed on the **worktree** (or a DevCouncil task when a store exists).

| Section | Purpose |
| --- | --- |
| **Overview** | One row per place work is happening: linked worktrees, PRs, runs, policy verdicts. Blocked operations sort first. Agent worktrees are detected from `/.<agent>/worktrees/` layout, never from a branch name. |
| **Resolve** | 3-way conflict editor: ours / theirs / base, marker jump, accept current / incoming / both. |
| **Remote** | PRs and issues in the wide column; workflows, runs, and releases in a CI rail. **CI:local** runs this repo's test matrix on your machine — affected tests when the map can prove coverage, otherwise the full suite (named as such; never badged as affected when fail-closed). |
| **Stack** | Branch hierarchy as a tree. Restack plans the whole subtree before the first rewrite and rebases parent-before-child. |
| **Policy** | Manvi wrap of DevCouncil modules: gate status, merged-branch cleanup, outgoing commit review, release preflight. |
| **Tasks** | Repository-scoped board/list over the shared profile task store: editing, selection, Manvi suggestions and agent handoff. |

Uncommitted counts that could not be scanned show nothing rather than `0`. A verdict this build cannot parse is `unreadable`, never `allowed`.

## Code (`⌘1` / `Ctrl+1`)

Explorer and Blame are two lenses on one **file** (selection survives the switch). **Map** is the structural navigator — DevMap repo map, code/doc graph, and tracked-markdown search — not a third reading of the open file. Sections: `⌥1` / `⌥2` / `⌥3`.

| Section | Purpose |
| --- | --- |
| **Explorer** | File tree with live Git status, dual-backend highlighting (MarkDev tree-sitter for six languages; regex for the rest), in-file search, go-to-line, inline edit, MarkDev markdown / image / hex previews. Commit composer shows pre-commit blast radius from `devmap preview`. |
| **Blame** | Per-line author, age heatmap, coverage gutter. Uncommitted lines render as `uncommitted`, not as a link to a fake commit. Coverage unavailable is marked separately from uncovered. |
| **Map** | Subsystems / entry points from the `repo_map` resolved by `devmap paths --json` (default `.devmap/repo_map.json`, legacy `.devcouncil` supported), code and doc graph canvas, docs search and broken links, cross-repo link candidates. Build/Refresh via the `devmap` CLI; watcher refreshes a stale index. Caps, `walk_incomplete`, and schema mismatch are named. |

The graph filters include **Hide notes & Markdown**, off by default. It hides
documentation nodes and their edges from the canvas, node browser, and
connections. In the subsystem view, only areas made entirely of documentation
are hidden; mixed areas stay visible. **Clear filters** restores the loaded
nodes. Filters survive a refresh and reset when switching repositories or map
views. Coverage and truncation counts still describe the original payload.

## History (`⌘2` / `Ctrl+2`)

Three lenses on one **commit**.

| Section | Purpose |
| --- | --- |
| **Graph** | GPU canvas graph with a topological lane solver. Default branch pinned left. Filters (`author:`, `sha:`, `type:`, `path:`, text) run in Rust before lanes are solved, so the graph stays connected. |
| **Diff** | Unified or true side-by-side from one row model. Syntax under word-diff, find-in-diff, hunk/line staging, image diffs. Layered blast radius by hop over the change set; rung filter on flat impact; preview markers on the file rail. The header names what the patch actually contains. |
| **Reflog** | HEAD movements; checkout or branch from an entry to recover discarded commits. |

![Diff view: unified commit diff](https://raw.githubusercontent.com/bharathvbcr/GitPulse/main/docs/assets/screenshot-diff.png)

## Insights (`⌘3` / `Ctrl+3`)

Four on-demand scans of **this repository**. Each must say when it was capped rather than presenting a floor as a total.

| Section | Purpose |
| --- | --- |
| **Pulse** | Heatmap, rhythm, punch card, LOC trend, commit hygiene, hotspots, bus factor, local DORA, exportable SVG card. Unscanned tiles are an em dash with a reason. |
| **Coverage** | LCOV, Cobertura, Go cover, Istanbul/NYC JSON, JaCoCo, Clover. Toolchain hints and failure recovery. |
| **Health** | `npm` / `cargo-audit` / `pip-audit` / `govulncheck` / `composer` / `bundler-audit` plus Dependabot and code scanning via `gh` when a repository opens. Critical and high findings warn. |
| **Storage** | Packfiles, loose objects, LFS, submodules, build caches, size history. |

![Coverage scanner](https://raw.githubusercontent.com/bharathvbcr/GitPulse/main/docs/assets/screenshot-coverage.png)

## Not views

### Terminal dock (`Ctrl+\``)

Resizable PTY under whichever view is on screen. Local AI suggestions and the policy sidecar do not read/write ordinary shells. Explicit Claude, Manvi, Codex and task launches create their own sessions. Hiding the dock keeps sessions; closing the repository ends them. See [Terminal](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TERMINAL.md).

### Tasks

The Tasks button beside Fleet opens global and saved-workspace scopes over the same records as Work → Tasks. Saved workspace membership survives closing repository tabs. Board/list layouts, filters over loaded cards, context actions, Quick Enhance and agent copying are described in [Tasks and workspaces](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TASKS_AND_WORKSPACES.md).

### Fleet (`Command/Ctrl+Shift+F`)

Workspace grid: every open repository and every recent one.

- **Tier 0** (free): changes, sync, conflicts, stash, parked operations.
- **Tier 1** (cheap git): worktrees, agent sessions, commit rhythm, last activity.
- **Tier 2** (opt-in): LOC, language mix, disk, audits, coverage — never run on their own.

Every cell is a value, *not scanned*, or *could not read*. Totals name what they could not count. `/` filters, `s` cycles sort, `p` toggles Pulse, `r` refreshes, `1`–`9` jump to a row.

See [[Architecture]] for why Fleet is hidden rather than unmounted (live PTYs).
