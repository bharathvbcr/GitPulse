# GitPulse Features & View Catalog

GitPulse provides 4 application views — **Work**, **Code**, **History** and **Insights** — all four of them header tabs. Each holds the lenses on one subject as sections rather than as separate destinations, and the terminal is a dock beneath whichever view is on screen.

On macOS, GitPulse automatically uses [glass surfaces and liquid transitions](MACOS_APPEARANCE.md), with opaque content panes and accessibility fallbacks.

```mermaid
flowchart TD
    subgraph ViewGroup["The four views"]
        Work["<b>Work</b> (<code>work</code>)<br/>Overview · Resolve · Remote · Stack · Policy"]
        Code["<b>Code</b> (<code>code</code>)<br/>Explorer · Blame — two lenses on one file selection"]
        History["<b>History</b> (<code>history</code>)<br/>Graph · Diff · Reflog — three lenses on one commit selection"]
        Insights["<b>Insights</b> (<code>insights</code>)<br/>Pulse · Coverage · Health · Storage — four scans of the repository"]
    end

    subgraph NotViews["Not views — available under every view"]
        Terminal["<b>Terminal dock</b> (<code>⌃`</code>)<br/>Embedded PTY beneath the active view"]
        Fleet["<b>Fleet</b> (<code>⇧F10</code>)<br/>Every open and recent repository at once"]
    end
```

---

## 1. Work Views

### 1.0 Work (`work`)
- **One Row Per Place Work Is Happening**: Joins linked worktrees, open pull requests, workflow runs, policy verdicts and grants into one row each. Everything else in GitPulse shows one of these; this shows how they relate.
- **Keyed On What Exists Here**: With a DevCouncil store, the unit is the task. Without one — the ordinary case for a repository driven by Claude Code or by hand — the unit is the **worktree**, because that is where a branch, its uncommitted changes, its parked operation and its pull request actually live. Keying on task regardless collapsed the whole repository into a single row labelled "Not bound to a task".
- **Agent Worktrees Are Named As Such**: A worktree under `/.<agent>/worktrees/` (Claude Code, Cursor, Codex, and any other tool using that layout) is marked, because a stale agent session and a stale hand-made checkout want opposite remedies — resume or merge, versus prune. Detection reads the directory layout, never the branch name, so a human naming a branch `claude/…` is not mislabelled. Git's own `.git/worktrees/` metadata is never labelled an agent session.
- **Blocked Worktrees Sort First**: A worktree parked mid-merge, rebase, cherry-pick or revert is the one thing on the screen that cannot progress without a person, so it outranks rows with more pull requests, and clicking it opens that worktree in the Resolve view.
- **Uncommitted Is Counted, Unscanned Is Not Claimed**: A worktree past the scan cap shows nothing rather than `0`, which would report it as verified clean.
- **Remotes, Submodules And Stash**: Folded in as a collapsed section rather than a separate view — the same repository, reference material rather than work in flight. Remotes can be added, renamed, re-pointed, pruned, or removed; submodules can be initialized, URL-synced, or deinitialized (never force-discarded). A remote, tag, or submodule listing cut by a cap says so, instead of looking complete.
- **Recorded Joins Only**: A worktree is placed on the task the ledger *bound* it to, never on a branch-name coincidence — two worktrees can hold the same branch. Pull requests and runs join through a worktree's branch, because that is the only link GitHub knows about; one matching no worktree stays in the unbound bucket rather than being guessed at. Verdicts and grants carry their own `task_id`, recorded when the gate judged.
- **A Branch on Two Tasks Appears on Both**: Assigning it to one would hide the work from the other with nothing on screen to say so.
- **Where You Are Standing**: A strip above the rows for the checked-out branch itself — tracking state against its upstream, how far behind the default branch it is, and the working tree split into staged, unstaged and conflicted. A branch the progressive branch-stats pass has not reached yet reads *"sync not measured yet"* rather than as `0↑ 0↓`, which is what a pushed, current branch looks like. A parked operation here gets its own line into Resolve, and a probe that could not run says so instead of leaving the same empty space as an idle worktree.
- **The Counts Are Doors**: Each tile in the strip selects exactly the rows it counted, and a filter box matches on branch, path, task and pull request. Strip and list share one predicate, so a tile saying three can never sit above a list showing none of them. A filter matching nothing says so — and offers to clear itself — rather than borrowing the wording of a repository with nothing in flight. Narrowing is dropped on a repository switch.
- **How Long It Has Been Sitting**: Each row carries the age of the most recent commit on its branches. A worktree nobody has touched in three weeks and one from ten minutes ago read identically without it and want opposite things done; a branch the branch list has not measured shows nothing rather than the epoch.
- **Verdict Tally**: Per-row counts of every policy status, with `allowed` folded into the total rather than shown as a chip, so the exceptions are what you see.
- **Unreadable Is Its Own State**: A verdict this build cannot parse is counted as `unreadable`, never as `allowed` — a check that could not be read must never render as one that ran and passed.
- **Incomplete Screens Say So**: Each of the five sources can be present, empty, or unreadable, and a row assembled from an unreadable source looks exactly like one assembled from an empty source. A banner above the rows names what could not be read, and distinguishes "this repository has no DevCouncil store" (ordinary) from "its store could not be opened" (a problem). The absence itself is never the headline: a reader who does not run one is told what *is* here, not what is missing.
- **Shortcut**: `F10`.

### 1.1 Code (`code`)

Two lenses on one subject — a file — switched by the segmented control in the
view's own header, which also names the file both sections are reading.
Explorer and Blame were two top-level views keyed off the same
`selectedFilePath`, and the split showed: Blame carried its own explorer rail
*and* its own path box so a reader would not have to walk back to Files for
the file they already had open. As sections the selection survives the switch,
so the editor's **Blame** button changes lens instead of teleporting.

#### Explorer
- **IDE File Explorer**: Recursive directory tree navigation with real-time Git status markers (staged, unstaged, untracked, ignored).
- **Virtualized Code Viewer**: High-performance line-virtualized code viewer supporting tokenized syntax highlighting across 60+ programming languages.
- **In-File Search & Filter**: Search with case-sensitivity toggle (`Aa`), regular expression support (`.*`), match count badges, and keyboard navigation (`Enter` / `Shift+Enter`).
- **Go To Line**: Fast modal overlay to jump directly to any line number (1–N).
- **Line Selection & Range Inspection**: Single-click line selection, shift-click line range highlighting, indentation style detection, and file status bar.
- **Inline Editor**: Instant toggle between read-only syntax viewing and direct in-memory text editing with save feedback.
- **Copy & Formatting Tools**: One-click whole-file or line-range copying with persistent feedback, whitespace character rendering toggle (`·` / `→`), and zoom font scaling (`⌘+` / `⌘-` / `⌘0`).
- **Specialized Media & Binary Previews**:
  - **Markdown / MarkDev**: Rendered document preview with syntax-highlighted code blocks and task lists.
  - **Images & Media**: Visual viewer with dimensions, aspect ratios, and format inspection.
  - **Binary Hex Viewer**: Formatted byte-offset hex dump with ASCII decoded gutters for compiled and binary artifacts.
- **Live Pulse Dashboard**: Uncommitted churn overview, active branch status, and instant staging accelerators.
- **Language Logo Vector Icons**: High-fidelity vector SVG logos for 34+ programming languages, configuration formats, and markup types rendered across the file tree, tab bar, diff toolbar, and dashboard.
- **Path Hierarchy Formatting**: Dimmed directory hierarchy prefixes with prominent filenames in the sidebar and commit details for scannable navigation.
- **Interactive Language Bar**: Live breakdown of repository language distribution with click-to-filter navigation into Code → Explorer.

### 1.2 History (`history`)

Three lenses on one subject — what happened to this repository — switched by
the segmented control in the view's own header, which also carries the commit
filter. Graph, Diff and Reflog were three top-level tabs; they share
`selectedCommitId`, so switching lens keeps the commit you were looking at.
The split was expensive in a way the code admitted: the Diff tab had to grow
its own commit picker purely so you would not have to walk back to Graph for
the commit you had just selected.

#### Graph
- **GPU Canvas Rendering**: High-performance commit graph capable of rendering repositories with 100,000+ commits smoothly.
- **Topological Lane Solver**: Rust-powered stable-column lane solving with nogap lookback guarantees to avoid visual discontinuities. The default branch (`main`, or the repository's own default; `origin/main` when it is ahead) is pinned to the leftmost column in one colour for the whole loaded window, so merged feature branches peel off and close back into a straight mainline instead of displacing it. Hovering the rail names the branch it belongs to.
- **Author Avatars & Badges**: Automatic display of author avatars or initials with one-click filter isolation.
- **Branch & Tag Ref Badges**: Visual indicators for local heads, tracking remotes, and release tags. The sidebar tag list names a failed or capped read rather than presenting a partial set as the whole history.
- **Commit Search**: ⌘F (and native Search Commits) focuses the commit filter in History's section bar, which filters all three sections from one walk. From any other view it switches to History first; in Code, ⌘F still searches the open file — in both sections, because Blame's lines are that same file's lines.
- **Filters That Keep The Graph Connected**: Every filter term — `author:`, `sha:`, `type:` or a `fix:`-style prefix, free text, and `path:` — is applied by the backend before lanes are solved. A commit the filter drops hands its lineage to its children, the way `git log --parents -- path` rewrites parents, so the survivors stay connected to their nearest kept ancestors, a survivor with no kept ancestors becomes a root of the filtered view, and the straight main-branch rail stays straight, anchored on the first surviving commit of the default branch's chain. A fading stub therefore always means one thing: the parent is past the loaded window, and the tooltip says so.
- **Cherry-pick & Revert**: Context-menu actions on a commit row replay or invert that commit onto the current branch, parking in the Resolve view if a conflict results.

#### Diff
- **Embedded File Rail & Commit Picker**: Browse changed files and move between recent commits without leaving the section. Uncommitted changes are prioritized as first-class entries, history truncation is explicitly surfaced, and state uses the commits the graph already drew with zero IPC overhead.
- **Precision Word Wrap & Normal-Flow Reflow**: Toggleable word-wrapping that gracefully disables row virtualization (`virtualize={false}`) up to `WRAP_MAX_LINES`, allowing long lines to reflow naturally without clipping, row overlap, or pushing split-view columns off-screen.
- **Intra-Line Word Highlighting**: Pinpoints exact character and token changes within modified lines.
- **Selective Patch Staging**: Stage or unstage individual hunks or selected line ranges directly from the diff view.
- **Image Diffs**: Side-by-side, 2-up, and swipe comparison modes for image assets.

#### Reflog
- **Reference Log Browser**: Full history of HEAD movements, checkouts, commits, rebases, and resets.
- **Recovery Points**: Instant checkout or branch creation from detached reflog entries to recover discarded commits.

### 1.3 Resolve (`conflict`)
- **3-Way Conflict Editor**: Clear visual distinction between *ours*, *theirs*, and *base* revisions.
- **One-Click Resolution**: Quick actions to accept current, incoming, or combined changes.
- **Marker Navigation**: Jump directly between unresolved conflict markers across changed files.

#### Blame
- **Line Authorship Viewer**: Interactive gutter displaying commit author, relative timestamp, and commit SHA for every line.
- **Commit Age Heatmaps**: Visual recency coloration highlighting fresh additions versus mature, historical lines.
- **Coverage Gutter**: Per-line hit counts beside the authorship gutter, and an explicit *Coverage unavailable* marker when the lookup fails — a file with no coverage data and a coverage read that failed must not look the same.
- **Commit Navigation**: One-click navigation from any blamed line directly to its full commit diff and history details.
- **Uncommitted Lines Named**: Worktree-only lines carry an all-zero OID and render as `uncommitted` rather than as a link to a commit that does not exist.

---

## 2. Inspect Views

### 2.1 Insights (`insights`)

Four scans of one subject — this repository — behind one segmented control.
They were four separate header entries, and every one of them is empty until
someone runs it: over half the Inspect menu costing attention every session
and paying occasionally. They also share a shape, which is the real reason to
gather them: each is an on-demand measurement that must say when it was capped
rather than presenting a floor as a total.

#### Pulse
- **Contribution heatmap**: 53-week calendar of local-day activity, toggling commit count vs churn. Includes unpushed and all-branch commits. Click a day to filter Graph with `date:YYYY-MM-DD`.
- **Rhythm**: current streak, longest run and longest gap in the last 90 days, plus active-day rate. A bounded history is labelled as such; a gap is never an artifact of where the scan stopped.
- **Punch card**: hour-of-week grid with after-hours share. Defaults to every author on every local and remote branch; an author filter is required before reading it as personal.
- **Line changes and LOC**: weekly additions vs deletions, reconstructed LOC trend from today's language-scan total walking numstat backwards, and churn-by-extension from the same walk. A partial or failed language scan is not shown as `0` LOC.
- **Commit hygiene**: conventional-commit rate (same type set the backend parser accepts), median non-merge churn, signed-commit rate, merge rate, co-author rate from `Co-authored-by:` trailers in the commit body.
- **Hotspot risk**: files ranked by churn × coverage. Unscanned coverage is "unknown", not "untested".
- **Knowledge and age**: blame-bounded bus factor, orphaned files, line-age distribution. Truncation is visible.
- **Local DORA**: deploy frequency and lead time from tags and `git describe --contains`. Change-failure rate and restore time are labelled approximations; a missing estimate is "—" not a invented number.
- **Export card**: a standalone SVG summary of the same window, sized for a README. Every tile carries its own definition rather than a bare label, each caveat sits on the tile it applies to (`CAPPED` commit scan, `PARTIAL` language or blame scan), and a metric whose scan did not run renders as an em dash with the reason — [an unscanned card](assets/screenshot-pulse-card-unscanned.png) and [a single-commit repository](assets/screenshot-pulse-card-solo.png) show both. The commit count and its active days always come from one population, so an author filter cannot leave the card mixing two.
- **Honesty**: payload-budget truncation is data, not an error. Scan Deeper raises the commit cap only when the byte budget was not the limiter. No `.mailmap` is announced, because per-author tiles are otherwise split across emails.

#### Coverage
- **Universal Format Scanner**: Discovers coverage reports across all major formats:
  - **LCOV** (`lcov.info`, `coverage.lcov`)
  - **Cobertura XML** (`cobertura.xml`, `coverage.xml`)
  - **Go Cover** (`cover.out`, `profile.out`)
  - **Istanbul / NYC JSON** (`coverage-final.json`, `coverage-summary.json`)
  - **JaCoCo XML** (`jacoco.xml`)
  - **Clover XML** (`clover.xml`)
- **Per-File Line Coverage**: Displays hit counts, uncovered branches, and line gutter markers.
- **Toolchain Installation & Detection**: Automatically detects missing coverage generators (`cargo-llvm-cov`, `pytest-cov`, `vitest`, `nyc`, etc.) and provides 1-click install suggestions.
- **Failure Recovery Hints**: Surfaces actionable diagnostic explanations when test coverage generation fails.
- **Report & Diagnostics Copying**: Persistent copy action to export sanitized coverage metrics directly to your clipboard.
- **MANVI AI Test Generator**: Analyzes coverage gaps and suggests runnable test scripts for Rust, TypeScript/JavaScript, Python, Go, Swift, Dart, Java, etc.

#### Health
- **Multi-Ecosystem Audits**: Automatically detects and scans project manifests:
  - `npm audit` / `npm outdated` (Node.js)
  - `cargo-audit` (Rust)
  - `pip-audit` (Python requirements.txt)
  - `govulncheck` (Go)
  - `composer audit` (PHP)
  - `bundler-audit` (Ruby)
  - GitHub Dependabot alerts (via local `gh` CLI)
- **AI Remediation**: Generates step-by-step upgrade plans with dependency version bump recommendations.

#### Storage
- **Git Internals Audit**: Analyzes disk usage across packfiles, loose objects, reflogs, LFS assets, and submodules.
- **Build & Cache Auditor**: Detects build directories (`target/`, `node_modules/`, `dist/`, `.venv/`, `.build/`) and unignored cache artifacts.
- **Historical Snapshots**: Records repo size history to plot trend sparklines ("+180 MB this week").

### 2.3 Stack (`stack`)
- **The Chain, As A Chain**: The hierarchy renders as a tree — parents above, children indented under them — rather than a flat list with "based on X" on every row, which made the reader rebuild the shape in their head.
- **Each Branch, Joined To What Is Known About It**: How far it is ahead of its parent and behind the default branch, its tracking state (`↑`/`↓`, `upstream gone`, or `untracked` — never `0↑ 0↓` for a branch with no upstream), and when it last moved and by whom.
- **Updating A Stack Cascades**: Rebasing one branch moves every branch above it off the commit it was cut from, so a single restack silently strands the rest of the stack. The action plans the whole subtree from the tree on screen *before* the first rewrite — the last moment those fork points exist — names every branch it will touch in the confirmation, and runs the steps in parent-before-child order. Each step is an independently gated, independently rolled-back rebase; a cascade that stops part-way reports which branches were rebased and which are still on their old base, then reloads so a second attempt cannot plan from a stale tree.
- **Fork Points Are Recorded, Not Recomputed**: Once a parent has been rebased, `merge-base` collapses back to the trunk and would replay the parent's own commits onto the parent. `cmd_restack` accepts the parent tip the stack was read at, refuses one that is not an ancestor of the branch (rather than silently widening the rewrite), and so does not depend on the reflog — which a fresh clone, a bare repository, or `gc.reflogExpire` will not have.
- **What The Hierarchy Cannot See, It Says**: A branch appears as a child only while it sits on its parent's *current* tip. Git records no "cut from" link, so a branch left behind by a rebase of its parent reappears as its own root, not as a stale child — stated on the page, with the local branches the walk placed on no stack listed by name. Otherwise a stack that fell apart reads as a repository that never had one.

---

## 3. System & Ops Views

### 3.1 Terminal — a dock, not a view (`⌃\``)

The terminal is **not** one of the 4 views. It renders as a resizable dock
*beneath* whichever view is on screen, reached with `⌃\``, the status bar's
Terminal chip, the command palette, or **View → Terminal**.

It was a view until the shape gave itself away: a PTY has to survive a view
switch, so the pane was already mounted once and hidden thereafter — a page you
could never leave without closing it. As a dock it is what it always behaved
like, and command output can be read *against* the thing that prompted it: a
Health remediation plan, a failing test, the diff you are about to commit.

- **Embedded PTY**: Native terminal emulator powered by `portable-pty` and `@xterm/xterm`.
- **Strict Isolation**: AI agents and sidecars have zero access to the user terminal PTY or keystrokes.
- **Diagnostic Preservation**: Preserves command output, exit status, and failure context across builds.
- **Lifecycle Supervision**: Clean process lifecycle teardown when closing tabs or switching repositories. Hiding the dock never ends the session — only closing the repository does.
- **Resizable**: Drag the separator or nudge it with `↑`/`↓`; the height is remembered, and clamped so the dock can never grow to swallow the view above it.

### 3.1a Agents (MCP 2.0 / Codex / Agent Plugins 1.0)
- **Protocol**: `gitpulse-mcp` implements [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) — `server/discover`, per-request `_meta`, `resultType` on results, cacheable tool lists. Dual-era: legacy `initialize` (2024-11-05 / 2025-11-25) still works.
- **Package**: one canonical package at `plugins/gitpulse/` — native Codex `.codex-plugin/plugin.json` + `.mcp.json`, Claude Code compatibility files, closed Agent Plugins 1.0 manifests, and one shared `skills/` tree. The Tauri app copies this package into `Contents/Resources/plugin`.
- **Tools**: `gitpulse_insights`, `gitpulse_collision_risk`, `gitpulse_change_context`, `gitpulse_active_changes`, plus ledger, tasks, codeintel, provenance. Read-only.
- **Work view**: insight strip (worktrees, agent sessions, blocked operations) and a collision banner that never treats a failed scan as “no overlap”.
- **Settings**: copies `.codex-plugin/plugin.json` / `.mcp.json` and names the binary path, or why it could not be found.

### 3.2 MANVI View (`manvi`)
- **Policy Monitor**: Displays real-time status of the MANVI command and file write gates.
- **Merged Branch Cleanup**: Identifies merged local branches and plans safe deletions without touching active or unmerged heads.
- **Commit Review**: Analyzes outgoing commits before pushing, reporting reviewed vs total counts.
- **Release Publisher**: Preflight checks (clean worktree, synchronized branch) before pushing SemVer tags.

### 3.3 Remote (`remote`) & Local CI
- **Two Columns, Not One Ragged Grid**: Pull requests and issues take the wide column because they are what a reader acts on; workflows, the runs they produce, and releases sit together in a CI rail. The five listings used to share one grid whose rows are as tall as their tallest cell, so twenty pull requests left a screen of white space beside a three-line releases card — and Workflows sat a row away from its own runs.
- **PR Management**: List repository PRs with one-click checkout, and a **New pull request** action that opens GitHub's compare form for the current branch onto the default branch.
- **The Queue's Counts Are The Way Into It**: `All / Awaiting review / Failing / Drafts` chips carry their own counts and filter the list, plus a search across number, title and both refs. Chip and list share one predicate. "Failing" means a red verdict only — a run still going and a repository whose checks never start are neither passing nor failing, and neither is folded into the other.
- **Issue List**: Open issues the context already fetched, with `issues_error` shown as a failure rather than an empty list, each carrying when it was last updated, and searchable by number, title, author or label.
- **Actions Dispatch**: View workflow runs and manually trigger `workflow_dispatch` events. Runs carry their age and can be narrowed to the checked-out branch; a run whose timestamp `gh` did not supply carries no age label rather than one dated to the epoch.
- **Fetched, Not Merely Present**: The header stamps how long ago the context on screen was fetched, and a listing hydrated from cache on a repository switch loses the stamp rather than inheriting a fetch that never happened.
- **CI:Local Runner**: Runs full repository CI pipeline locally before pushing commits:
  ```mermaid
  flowchart LR
      Manifests["Detect Manifests<br/>(package.json, Cargo.toml)"] --> Plan["Plan Step Matrix"]
      Plan --> Exec["Sequential Execution<br/>(Svelte Check → Tests → Clippy → Cargo Test)"]
      Exec --> Report["Honest Accounting<br/>(Passed / Failed / Skipped)"]
  ```

---

## 3.5 Fleet — the whole workspace at once (`Shift+F10`)

Fleet is not a view. Every view in the catalog above answers a question about
*one* repository, is persisted on that repository's session, and lives inside
the pane keyed on the active repository. Fleet answers a question about the
**workspace**, so it sits above the repository tab strip and is reached from
the strip's leftmost chip, the command palette, or **View → Fleet**. Toggling
it hides the repository pane rather than unmounting it, so live terminal
sessions survive and nothing re-hydrates on the way back.

- **One row per repository, open and recent.** Open repositories show live
  state; a repository that is only in your recents list is dimmed, marked *not
  open*, and shows only what its own ledger already recorded — never a live
  number it cannot have.
- **Three tiers, priced honestly.** Changes, sync, conflicts, stash and parked
  operations are already in memory and cost nothing. Worktrees, agent sessions
  and last activity cost two `git` calls per repository and refresh whenever
  the set of open repositories changes. Lines of code, disk usage, dependency
  audits and coverage cost minutes and **never run on their own** — the same
  opt-in posture as automatic coverage generation and the release check.
- **Every cell has three states, never two.** A measured value, *not scanned*,
  or *could not read* with its reason. A repository nobody has audited shows
  "not scanned", never a reassuring zero; an audit that ran but could not
  finish is marked as a floor, so partial coverage cannot read as a clean bill
  of health.
- **Totals say what they could not count.** "1.50 GB — counted across 14 of
  21, 1 failed, 6 not scanned" rather than a bare number that implies the
  whole workspace. A total covering everything says nothing extra.
- **A verdict is never made over a check that could not run.** A repository
  whose sweep failed is reported *unknown*, never *clean* — but a repository
  with real conflicts stays at conflicts, because unreadability must not
  downgrade a worse problem.
- **Sweeps report what actually happened.** "Scan all" for a family runs at a
  bounded width (two at a time for storage and audits, which walk the tree and
  spawn your package manager) and reports successes, failures and skips
  separately, attributing each failure to the repository and column it
  happened in.

---

## 4. Keyboard Shortcuts Reference

GitPulse provides comprehensive keyboard navigation accelerators across the entire application:

### 4.1 Workspace & Repository Tabs
| Action | macOS | Windows / Linux |
| --- | --- | --- |
| **Open Repository…** | `⌘ O` / `⌘ T` | `Ctrl+O` / `Ctrl+T` |
| **Clone Repository…** | `⌘ ⇧ O` | `Ctrl+Shift+O` |
| **Close Repository Tab** | `⌘ ⇧ W` | `Ctrl+Shift+W` |
| **Reopen Closed Tab** | `⌘ ⇧ Y` | `Ctrl+Shift+Y` |
| **Next Repository Tab** | `Ctrl Tab` | `Ctrl+Tab` |
| **Previous Repository Tab** | `Ctrl ⇧ Tab` | `Ctrl+Shift+Tab` |
| **Jump to Tab 1–9** | `Ctrl ⌥ 1–9` | `Ctrl+Alt+1–9` |
| **Preferences / Settings…** | `⌘ ,` | `Ctrl+,` |

### 4.2 View Switching
| View | macOS | Windows / Linux |
| --- | --- | --- |
| **Work** | `F10` | `F10` |
| **Code** | `⌘ 1` | `Ctrl+1` |
| **History** | `⌘ 2` | `Ctrl+2` |
| **Insights** | `⌘ 3` | `Ctrl+3` |
| **Fleet** | `⇧ F10` | `Shift+F10` |
| **Terminal dock** | `⌃ \`` | `Ctrl+\`` |

Sections within a view — Code's Explorer / Blame, History's Graph / Diff /
Reflog, Insights' Pulse / Coverage / Health / Storage — are switched by that
view's segmented control,
or by name from the command palette.

### 4.3 Navigation & Search
| Action | macOS | Windows / Linux |
| --- | --- | --- |
| **Command Palette** | `⌘ K` | `Ctrl+K` |
| **Search / Filter Commits** | `⌘ F` | `Ctrl+F` |
| **Shortcuts Cheat Sheet** | `?` or `⌘ /` | `?` or `Ctrl+/` |
| **Zoom In** | `⌘ +` or `⌘ =` | `Ctrl++` or `Ctrl+=` |
| **Zoom Out** | `⌘ -` | `Ctrl+-` |
| **Reset Zoom** | `⌘ 0` | `Ctrl+0` |
| **Toggle Dark / Light Theme** | `⌘ ⇧ T` | `Ctrl+Shift+T` |

### 4.4 Git Operations & Workflow
| Action | macOS | Windows / Linux |
| --- | --- | --- |
| **Refresh Repository** | `⌘ R` | `Ctrl+R` |
| **Fetch from Remote** | `⌘ ⇧ K` | `Ctrl+Shift+K` |
| **Pull from Remote** | `⌘ ⇧ P` | `Ctrl+Shift+P` |
| **Push to Remote** | `⌘ ⇧ U` | `Ctrl+Shift+U` |
| **Quick Commit (Composer)** | `⌘ Enter` | `Ctrl+Enter` |
| **Dismiss Modal / Overlay** | `Esc` | `Esc` |
| **Navigate List Items** | `↑` / `↓` | `↑` / `↓` |
| **Select / Execute Item** | `Enter` | `Enter` |

### 4.5 Command Palette Modes
| Prefix | Mode | Description |
| --- | --- | --- |
| `>` | **Commands** (default) | Run any application action, open views, switch themes, or run audits. |
| `#` | **Jump to Commit** | Instantly search and jump to a commit by SHA prefix or commit message. |
| `@` | **Jump to Branch** | Search local and remote branches and checkout with a single keystroke. |
| `?` | **Help & Shortcuts** | View available keyboard shortcuts and documentation. |
