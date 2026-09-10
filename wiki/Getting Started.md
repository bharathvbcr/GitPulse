# Getting Started

## Open a repository

1. Launch GitPulse.
2. **Open Repository…** (`⌘O` / `Ctrl+O`) or **Clone Repository…** (`⌘⇧O` / `Ctrl+Shift+O`).
3. The repository opens as a tab. Open several; switch with `Ctrl+Tab`.

The command palette (`⌘K` / `Ctrl+K`) is the fastest way to jump: views, branches (`@`), commits (`#`), and actions (`>`).

Press `?` or `⌘/` for the in-app shortcuts sheet. Full list: [[Keyboard Shortcuts]].

## The four views

| View | Shortcut | What it is for |
| --- | --- | --- |
| **Work** | `F10` | What is in flight: worktrees, PRs, conflicts, stacks, policy and tasks |
| **Code** | `⌘1` / `Ctrl+1` | Files, blame, and the code/docs Map |
| **History** | `⌘2` / `Ctrl+2` | Graph, diff, and reflog on one selected commit |
| **Insights** | `⌘3` / `Ctrl+3` | Pulse, coverage, health, and storage — on-demand scans |

Sections live in each view's header. Picking a commit in Graph and switching to Diff keeps that commit. Opening a file in Explorer and switching to Blame keeps that file. Code → Map is the DevMap / docs navigator (not a third reading of the open file). A blocked worktree row in Work → Overview opens Resolve on the worktree that is actually stuck.

Full catalog: [[Views]].

## Beside the views

- **Terminal** (`Ctrl+\``) — native PTY docked under the current view. Hiding the dock does not kill the session; closing the repository does. Local AI suggestions and the policy sidecar do not access ordinary shells; explicit agent launches use their own sessions.
- **Fleet** (`Command/Ctrl+Shift+F`) — every open and recent repository in one grid. Not a view: it sits above the tab strip so it survives switching repos. Cells are a value, *not scanned*, or *could not read* — never a fake zero.

- **Tasks** — global and saved-workspace boards over one persistent task store. **Work → Tasks** filters it to the active repository. See [Tasks and workspaces](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TASKS_AND_WORKSPACES.md).

## First useful paths

**See the graph.** History → Graph. The default branch is pinned to the leftmost column. `⌘F` filters commits (`author:`, `sha:`, `path:`, `type:`, free text). Filters keep the graph connected: dropped commits hand their lineage to the survivors.

**Commit.** Stage from Code or Diff (including hunk/line staging). `⌘Enter` / `Ctrl+Enter` opens the commit composer. Mutating Git goes through the [[Policy and AI|policy gate]].

**Inspect health.** Insights → Health. Audits run when you ask; an ecosystem that could not be scanned is named, not implied clean.

**Talk to an agent.** Install `gitpulse-mcp` and point Codex / Claude Code / an MCP client at it. The server is read-only. See [[MCP and Agents]].

## GitHub panel

Work → Remote uses your local `gh` CLI. GitPulse never stores GitHub tokens. If the panel is empty, run `gh auth status` in a terminal.

## macOS appearance

On macOS, chrome uses glass over a transparent, desktop-blurring window. Code, diffs, and the graph stay opaque. Reduce transparency / increase contrast fall back to solid surfaces. Details: [docs/MACOS_APPEARANCE.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/MACOS_APPEARANCE.md).
