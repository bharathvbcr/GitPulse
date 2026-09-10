# Keyboard Shortcuts

In the app, press `?` or `⌘/` (`Ctrl+/`) for the cheat sheet. Palette prefix `?` opens the same help.

macOS listed first; Windows / Linux in the second column.

## Workspace and tabs

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Open Repository… | `⌘O` / `⌘T` | `Ctrl+O` / `Ctrl+T` |
| Clone Repository… | `⌘⇧O` | `Ctrl+Shift+O` |
| Close Repository Tab | `⌘⇧W` | `Ctrl+Shift+W` |
| Reopen Closed Tab | `⌘⇧Y` | `Ctrl+Shift+Y` |
| Next / Previous Tab | `Ctrl+Tab` / `Ctrl+⇧Tab` | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Jump to Tab 1–9 | `Ctrl+⌥1–9` | `Ctrl+Alt+1–9` |
| Preferences | `⌘,` | `Ctrl+,` |

## Views

| View | macOS | Windows / Linux |
| --- | --- | --- |
| Work | `F10` | `F10` |
| Code | `⌘1` | `Ctrl+1` |
| History | `⌘2` | `Ctrl+2` |
| Insights | `⌘3` | `Ctrl+3` |
| Fleet | `⌘⇧F` | `Ctrl+Shift+F` |
| Terminal dock | `⌃\`` | `Ctrl+\`` |

Sections (including Work → Tasks, Explorer/Blame/Map, Graph/Diff/Reflog and Pulse/Coverage/Health/Storage) are switched with `⌥` + the section's digit while that view is active, or by name in the command palette.

## Fleet (focus in the grid, not in a text field)

| Action | Key |
| --- | --- |
| Filter repositories | `/` |
| Clear filter | `Esc` |
| Cycle sort | `s` |
| Toggle Fleet Pulse | `p` |
| Refresh | `r` |
| Jump to row 1–9 | `1`–`9` |
| Remove from Fleet | `Delete` / `Backspace` |

## Navigation and search

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Command Palette | `⌘K` | `Ctrl+K` |
| Search / Filter Commits | `⌘F` | `Ctrl+F` |
| Shortcuts Cheat Sheet | `?` or `⌘/` | `?` or `Ctrl+/` |
| Zoom In / Out / Reset | `⌘+` / `⌘-` / `⌘0` | `Ctrl++` / `Ctrl+-` / `Ctrl+0` |
| Toggle Dark / Light | `⌘⇧T` | `Ctrl+Shift+T` |

In **Code**, `⌘F` searches the open file (Explorer and Blame). From other views it focuses History's commit filter.

## Git operations

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Refresh Repository | `⌘R` | `Ctrl+R` |
| Fetch | `⌘⇧K` | `Ctrl+Shift+K` |
| Pull | `⌘⇧P` | `Ctrl+Shift+P` |
| Push | `⌘⇧U` | `Ctrl+Shift+U` |
| Quick Commit (Composer) | `⌘Enter` | `Ctrl+Enter` |

## Command palette prefixes

| Prefix | Mode |
| --- | --- |
| `>` | Commands (default) |
| `/` | Files: tracked and untracked, non-ignored paths |
| `%` | Repositories: open tabs, closed tabs and recents |
| `#` | Jump to commit (SHA, message or author in loaded history) |
| `@` | Jump to branch and checkout |
| `:` | Symbols in the active repository (DevMap) |
| `::` | Cross-repo symbols (open tabs registered in DevMap's workspace; append `~` for TF-IDF name search) |
| `?` | Help and shortcuts |

## Task boards

With focus inside a board, `/` searches, `n` creates a task, Command/Ctrl+A selects
visible loaded cards, and Command/Ctrl+Shift+C copies selected tasks for an agent.
On a card, Shift+F10 opens its context menu, `e` opens Quick Enhance, and Left/Right
move to the neighboring status. Delete/Backspace asks for confirmation. These are
scoped controls; see [Tasks and workspaces](https://github.com/bharathvbcr/GitPulse/blob/main/docs/TASKS_AND_WORKSPACES.md) for the complete table.
