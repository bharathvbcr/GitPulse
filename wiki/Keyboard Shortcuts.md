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
| Move Tab Left / Right | `Ctrl+⇧+←` / `Ctrl+⇧+→` | `Ctrl+Shift+←` / `Ctrl+Shift+→` |
| Jump to Tab 1–9 (9 is the last) | `⌘1–9` | `Ctrl+1–9` |
| Preferences | `⌘,` | `Ctrl+,` |

## Views

| View | macOS | Windows / Linux |
| --- | --- | --- |
| Work | `F10` | `F10` |
| Code | `⌥⌘1` | `Ctrl+Shift+1` |
| History | `⌥⌘2` | `Ctrl+Shift+2` |
| Insights | `⌥⌘3` | `Ctrl+Shift+3` |
| Fleet | `⌘⇧F` | `Ctrl+Shift+F` |
| Agents plane | `⌘⇧A` | `Ctrl+Shift+A` |
| Terminal dock | `⌃\`` | `Ctrl+\`` |

The terminal dock has its own shortcuts, listed in its shortcut guide (splits, pane focus, Find, text size); for example `⌘D` splits right and `⌘⇧D` splits down on macOS.

Sections (Work: Overview/Remote/Tasks; Code: Explorer/Map; History: Graph/Diff; Insights: Pulse/Coverage/Health/Storage) are switched with `⌥` + the section's digit while that view is active, or by name in the command palette.

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

`⌘⇧F` is **Fleet**, not search. `⌘F` is the only search chord: in **Code** it searches the open file (Explorer, including when the Blame toggle is active), in History → Diff it searches the diff, and from other views it focuses History's commit filter. Repository-wide content search has no chord of its own; open it in Code → Explorer with the field's Contents mode, or find files by name with the palette's `/` mode.

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
