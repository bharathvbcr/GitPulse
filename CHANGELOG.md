# Changelog

All notable changes to GitPulse are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building, so every
released version needs a heading of the form `## [x.y.z] - YYYY-MM-DD` here
before that tag is pushed.

## [Unreleased]

### Added

- Automatic macOS glass styling for window chrome, sidebar, menus, and dialogs; fluid view-selection and dialog transitions with reduced-motion, reduced-transparency, and contrast fallbacks. In-app glass preserves existing distribution options.

### Changed

- **Stack draws the chain as a chain.** A stack *is* its shape, and a flat list with "based on X" on every row made the reader rebuild it in their head. Rows are a tree now, each joined to what the branch list already knows: commits ahead of its parent, commits behind the default branch, tracking state (`↑`/`↓`, `upstream gone`, or `untracked` — never `0↑ 0↓`, which is what a pushed and current branch looks like), and when it last moved and by whom. A branch the progressive stats pass has not measured contributes nothing rather than a confident zero.
- **Updating a stack carries the stack.** Rebasing one branch moves every branch above it off the commit it was cut from — and, because the hierarchy is tip-anchored, moves them out of the tree at the same time, so a single restack stranded the rest of the stack invisibly. The plan is computed from the tree on screen *before* the first rewrite, which is the last moment those fork points exist; the confirmation names every branch it will touch, because "restack 4 branches" does not say which four and this rewrites commits on all of them. Steps run parent-before-child, each an independently gated and independently rolled-back `cmd_restack`, and a cascade that stops names what moved and what did not.
- **Work's Overview says where you are standing.** It described every worktree in flight and never the one checked out in front of the reader — not the branch, not whether it had drifted from its remote, not what was uncommitted in it. A strip above the rows carries the branch, its tracking and base comparison, and the working tree split into staged, unstaged and conflicted, with the parked operation getting its own line into Resolve. A branch the branch list has not reached yet reads *"sync not measured yet"* rather than as parity with a remote nobody has asked about, and an operation probe that could not run says so instead of leaving the same empty space as an idle worktree.
- **Overview's counts became doors.** "3 blocked" over a list sorted by weight was a number the reader then had to go find. Each tile selects exactly the rows it counted — strip and list share one predicate, so a tile saying three can never sit above a list showing none of them — and a filter box matches on branch, path, task and pull request. Rows carry the age of the most recent commit on their branches, because a worktree nobody has touched in three weeks and one from ten minutes ago read identically without it and want opposite remedies. A filter matching nothing says so and offers to clear itself, rather than borrowing the wording of a repository with nothing in flight; narrowing is dropped on a repository switch.
- **Remote is two columns, not one ragged grid.** Five listings shared a grid whose rows are as tall as their tallest cell, so twenty open pull requests left a screen of white space beside a three-line releases card — and Workflows sat a grid row away from the runs it produces. Pull requests and issues take the wide column because they are what a reader acts on; workflows, runs and releases sit together in a CI rail. The queue's own counts are the way into it (`All / Awaiting review / Failing / Drafts`, each filtering the list it counted, plus a search across number, title and both refs), issues carry when they were last updated and are searchable, runs carry their age and narrow to the checked-out branch, and the header stamps how long ago the context was fetched — a listing with no timestamp reads as current however long it has been sitting there. *Failing* means a red verdict only: a run still going and a repository whose checks never start are neither passing nor failing, and neither is folded into the other. The CI:local report is a card the reader folds up or dismisses instead of a full-width banner that pushed every listing down and could not be put away.
- **The diff page is rebuilt around the diff itself, not around the last thing you clicked.** The header printed `selectedFilePath` whatever the body held, so a commit diff was labelled with one file's name, one file's language icon and the *whole commit's* line count — `.github/workflows/codeql-analysis.yml`, `2,405 lines`, above a body showing `server.go`. Identity is parsed from the diff now (`diff/outline.ts`): the title is the file's path when the diff covers exactly one and `N files` when it does not, the churn is `+19,250 −2,217` read off the diff's own lines, and the language badge is dropped entirely for a multi-file diff rather than borrowed from whichever file happened to be selected. Below it a context strip names the file, hunk and enclosing function the reader is *currently inside*, updated as the list scrolls, so a 200-file commit stops being 25,000 anonymous lines. The file stepper reads `4/200` instead of `–/26`.
- **Split view puts a replaced line beside its replacement.** The old builder walked the line list holding one pending deletion, so a block of D deletions and A additions came out as D−1 rows with an empty right column, one paired row, then A−1 rows with an empty left — the two sides offset by the size of the block, which is exactly the case split view exists for. Pairing is now a shared model (`diff/rowModel.ts`) that both layouts read: row *k* of a replacement block holds `del[k]` beside `add[k]`, the longer side spills into rows with one empty cell, context is the same object on both sides, and file headers and hunk headers span both columns instead of leaving a column-height hole. The same model backs the unified list, so the two views can no longer disagree about which line replaced which — the intra-line word-diff highlight used to depend on which view you had opened first.
- **One horizontal scrollbar for the diff, not one per row.** Every row was its own scroll container, so a wide diff drew a grey bar under each line, the rows scrolled independently of one another, and the line-number gutter scrolled away with the code. The rows now share the list's single scroller: the gutter is `position: sticky` with an opaque backing (a translucent tint let code slide visibly underneath it), and `VirtualList` grew an opt-in `contentWidth` so a row's background tint spans the full content width rather than stopping at the viewport edge when scrolled right.
- **Diff rows are syntax-highlighted, in each file's own language.** The file viewer beside it has had a tokenizer the whole time; the diff printed one colour per line type. Three overlapping layers — syntax, the word diff's changed spans, and the search hit — do not nest, so they are flattened into one span list per line (`diff/highlight.ts`), cached per line object and skipped above 2,000 characters. Crucially the language is resolved per *section*: a commit diff is a stack of files in different languages, and one global language coloured a 200-file commit's Rust with the JSON tokenizer, so `//` comments came out as plain text and the commas were confidently picked out as punctuation.
- **The minimap maps the list that is on screen.** It built its ticks from the unified line list and then wrote the resulting offset into whichever pane was showing, so in split view — a different list of a different length — every click landed off by the ratio between the two. It also mapped a click to `ratio × contentHeight`, which top-aligns: clicking the last tick scrolled past the end, clamped, and left the thing you aimed at off screen. It now takes the tones of the list actually being drawn and centres the target, and it draws the viewport band and the file boundaries that turn a decoration into a map. A bucket holding both additions and deletions is a modification, not growth.
- **The file rail tells its files apart.** Two hundred rows of `manuscript_citation_hy…` and `manuscript_citation_m…` name nothing. Rows are disambiguated against each other (the shared `disambiguateLabels`), so each shows the shortest directory prefix that makes it unique; there is a filter with a live `33 of 200 files` count, a list/tree toggle that reuses the repository file-tree builder, a drag- and keyboard-resizable width (a real ARIA window splitter: arrows step, Home/End jump to the bounds), and virtualization past 60 rows so a 200-file commit renders 44 buttons rather than 200.
- **Find in a diff is the file viewer's search, hardened.** Both had their own loop; they are now one module (`text/lineSearch.ts`). It refuses a pattern whose nesting can backtrack catastrophically instead of running it — `(a+)+c` against 28 characters took **111 seconds** in the old path, and a JavaScript regex cannot be interrupted once started — bounds the scan with a deadline checked every 256 lines, caps the match list, and reports `2 of 5+` rather than pretending the cap is the total. `⌘F` now belongs to the diff when the diff is what is on screen, instead of always opening the commit filter.
- **The frame survives an empty diff, an image, and a fetch in flight.** Each of those used to replace the whole pane, so a clean merge or a `.png` left no way to reach the next file except by leaving for the Graph. The rail, toolbars and file stepper are now outside the branch. A diff being fetched is a first-class state (`selectedDiffPending`) rather than the previous file's rows sitting there looking current, and the wait is announced — the region reports `aria-busy` and the message is a live status, because a spinner is a picture of nothing to a screen reader.

### Fixed

- Exclude iPhones, iPads, and desktop-mode iPads from Mac-specific window chrome and appearance.
- **The Stack page could only ever show stacks that needed nothing done to them.** `build_stack_hierarchy` records a parent only while a branch's first-parent walk lands on another branch's *literal current tip*, so one extra commit on `feat-a` made `feat-b` report `main` as its base and the stack silently became two siblings. The button was the wrong way round in both directions: **Restack** was offered exactly when it was a no-op — the branch already sat on its parent's tip — and hidden exactly when it was needed. This is not a gap in the backend to close by inference. Git stores no "cut from" edge, and once the parent moves nothing on disk distinguishes a drifted child from an unrelated branch sharing history; the two are structurally symmetric, and picking a direction would be fabricated hierarchy. So the page states the limit instead, on every populated stack rather than only on the empty one, and lists the local branches the walk placed nowhere under **On no stack** — a stack that has fallen apart must not read as a repository that never had one.
- **A cascading restack replays each child from the parent tip the stack was read at, not from a recomputed one.** Once a parent is rebased, `merge-base(parent, child)` has collapsed back to the trunk, so replaying from it tells git to re-apply the parent's own commits on top of the parent. `merge-base --fork-point` normally rescues that from the parent's reflog — and does not in a fresh clone, in a bare repository (`core.logAllRefUpdates` defaults off there), or once `gc.reflogExpire` has run, which is the ordinary state of a long-lived stack. `cmd_restack` takes an optional `fork_point`, refuses one that is not an ancestor of the branch rather than quietly widening the rewrite the caller planned, and is covered by a test that builds exactly that repository: with the reflog expired and the parent's commits revised during the update, the computed plan drags the parent's stale pre-image into the child and conflicts on a file the child never touched.
- **A cascade that stopped part-way no longer erases its own report.** The failure path set a banner naming which branches had been rebased and which were still on their old base, then reloaded the tree — and the reload cleared the banner on the way past, leaving a half-rebased repository with a screen that said nothing had happened. Clearing belongs to the next attempt, which does it before its first await; a watcher tick is not an attempt.
- **`gp-pill` was written by a dozen call sites across the Remote, MANVI and Stack panes and defined nowhere.** Every one of them rendered as bare inline text, with the `!bg-…`/`!text-…` overrides beside them landing on nothing. Defined once in `app.css` rather than replaced at each call site.
- **CI verdicts carried one theme's shade.** `text-green-400` / `text-red-400` / `text-amber-400` are tuned for the dark theme and sit near 2:1 against the light theme's near-white card — on precisely the labels a reader opens the page to check. Every verdict carries both shades now, in the panel and in `ciStepClass`.
- **Three copies of the git path parser disagreed.** `patchBuilder` and `wordDiff` each carried their own unquoting and prefix-stripping, and octal escapes in a quoted path were decoded as *characters* rather than as bytes, so `"sp ace/\303\251.ts"` came out mangled instead of as `sp ace/é.ts`. There is one parser now (`diff/gitPaths.ts`), decoding octal escapes as UTF-8 bytes and splitting a bare/bare `diff --git` header at the ` b/` where both sides name the same file.
- **A source file with a raw NUL byte is invisible to search.** ripgrep and `grep` sniff for NUL, classify the whole file as binary and skip it in silence, so a search for a symbol that *is* in the file reports that the file does not use it. `DiffViewer.svelte` carried four (cache keys joined on a literal NUL), and `fileRail.ts`, `FleetView.svelte` and `operation.test.ts` one each. All five are written as escapes now, and a new `greppable-source-contract` walks the tree so the next one fails the suite instead of quietly shrinking every future search.
- **The shortcuts modal still listed `⌘1–9` for nine views that no longer exist.** It reads `⌘1–3 … Code, History, Insights`, matching what the native menu actually binds, and gained the diff's own chord list.

## [0.0.5] - 2026-09-04

### Changed

- **Fifteen views are four.** The header had grown a button per panel — four tabs plus an *Inspect* and a *More* dropdown — and most of what sat behind them was not a destination at all. Diff carried its own commit picker so you would not have to walk back to Graph for the commit you had just selected; Blame carried its own explorer rail *and* its own path box so you would not have to walk back to Files for the file you already had open. Those pickers are the tell: a tab that has to rebuild the previous tab's context is a lens on a shared subject, not a place. Each subject now owns one view, and its lenses are **sections** switched by a segmented control inside it, so the subject survives the switch. **Work** (Overview · Resolve · Remote · Stack · Policy) shares the worktree row; **Code** (Explorer · Blame) shares `selectedFilePath`; **History** (Graph · Diff · Reflog) shares `selectedCommitId`; **Insights** (Pulse · Coverage · Health · Storage) shares the repository. The terminal became a dock beneath whichever view is on screen — the PTY always had to outlive a view switch, so it was already mounted once and hidden thereafter, which is a dock wearing a tab's clothes. Fleet stays workspace-scoped and is deliberately not a view.
- **A retired view lands where its content went, not at the start of the app.** Sessions persist a `viewTab`, and eleven of those ids no longer exist. `migrateViewTab` used to send every unrecognised id to Work, which would have stranded anyone whose last session was on Diff, Coverage or Blame. Retirements are a map now (`RETIRED_VIEWS`), each naming the view *and the section* its content became, so a session written by 0.0.5 reopens on Code showing **Blame**, not on Code's default pane — landing on the right view showing the wrong thing reads to the user as the content having been deleted. The same map drives the tests: the check that every retired view kept a command-palette door is derived from it rather than hand-listed, so retiring a view without giving it a door fails the suite instead of silently removing the only route a keyboard user had.
- **The header's dropdown machinery is deleted, not left standing beside the tabs.** Menus existed because fifteen views could not fit a title bar; four can, so the last group emptied and the dropdown branch became unreachable. Rather than keep a grouping layer with one group, the portal, the capture-phase dismissal, the `viewNav` group layer and `menuGroup` on every registration are gone — four regression tests about *dismissing* that menu were replaced by one asserting it stayed deleted, since a reintroduced menu would bring those regressions back with it. View-hiding in Settings is unaffected and now renders one flat list instead of headings describing a tabs-versus-menu distinction that no longer exists. Digit accelerators close up to ⌘1–⌘3 with no gap, which the native menu asserts.
- **The language strip became a status-bar segment.** It was a 32px full-width bar that re-fetched `cmd_get_language_stats` on its own, next to a comment in the metric registry saying the headline LOC number and the language bar are two readings of one scan "and deriving them separately is how they came to disagree". It reads `locMetric` now, so the two cannot drift, its failures land in diagnostics instead of blanking silently, and the breakdown moved into a popover — reclaiming a strip that cost every session and paid occasionally.

### Added

- **Fleet: one surface for every open repository, and every recent one.** A workspace of two dozen tabs could only be inspected one tab at a time — "is anything unsaved anywhere", "which of these has an agent running", "what is all this costing on disk" were unanswerable without visiting each in turn. Fleet (`Shift+F10`, the leftmost chip in the repository strip, or the command palette) puts them on one grid: changes, sync, conflicts, stash, parked operations, worktrees, agent sessions and last activity, plus lines of code, disk usage, dependency audits and coverage on demand. It is deliberately **not** a sixteenth view — a `ViewTab` is stored on the active repository's session and its pane lives inside `{#key currentPath}`, so a view would be scoped to the wrong thing and rebuilt on every repository switch. It sits beside the repository pane instead, and the two swap by hiding rather than unmounting, because that subtree holds the live terminal PTY.
- **Every Fleet cell is a value, *not scanned*, or *could not read* — never a reassuring zero.** The failure a workspace dashboard invites is reporting a fleet of clean, empty, vulnerability-free repositories that nobody ever scanned. So the three states are distinct in the model, in the markup and in the totals: a repository with no audit shows "not scanned", an audit that ran but could not finish is marked as a floor, a ledger that could not be opened fails its four cells rather than emptying them, and a total says "1.50 GB — counted across 14 of 21, 1 failed, 6 not scanned" rather than a bare number implying the whole workspace. A repository whose sweep failed is reported *unknown*, never *clean* — but never at the cost of downgrading one that has real conflicts.
- **Expensive scans stay opt-in, on the same posture as automatic coverage and the release check.** Storage walks up to 250,000 files behind a 20-second deadline and the dependency audit spawns `npm audit` / `cargo audit` with a 90-second timeout; neither ever runs from an effect. The cheap tier costs nothing at all — changes, sync and conflicts are already hydrated in memory — and the middle tier is a new `cmd_fleet_snapshot`, two `git` spawns per repository in one rayon-parallel round trip, deliberately narrower than `cmd_insights_snapshot` (which probes every worktree and cross-scans up to 16 for collisions: correct for one repository on screen, several hundred subprocesses across a workspace). Family sweeps run two at a time for the expensive families and report successes, failures and skips separately.
- **Scan results are cached in each repository's own ledger, with their age.** A new additive `fleet_metrics` table records one row per repository, every family carrying its own value *and* its own timestamp, so "never scanned" is `NULL` rather than `0` and a displayed number can always be dated. Families are independent: a storage scan cannot blank last week's audit. Reads go through a read-only, no-create SQLite open, so rendering a row for a repository that is merely in the recents list never writes a `.devcouncil/` directory into a repository the user has not opened.

- **The package is now installable and connectable as a Claude Code plugin.** The Agent Plugins 1.0 package described the server, but no client could find it: Claude Code discovers a package through a `.claude-plugin/marketplace.json` at the repo root and then reads `<source>/.claude-plugin/plugin.json` and `<source>/.mcp.json`, none of which existed. Those files now sit alongside the Agent Plugins and Codex manifests in the one canonical package, sharing its single `skills/` tree, and the marketplace points at it. Verified end to end: `claude plugin install` records 0.0.5, the component inventory resolves both skills and the MCP server, and `claude mcp list` reports the server connected.
- **GitPulse is now a native Codex plugin, not only an MCP binary that happened to exist.** `plugins/gitpulse/.codex-plugin/plugin.json` declares the native surface, `.agents/plugins/marketplace.json` makes it installable from the repository, and the same canonical package is bundled into `Contents/Resources/plugin` for Settings discovery. The portable manifest still launches `gitpulse-mcp` from PATH; this host's Codex MCP registration uses the absolute installed path so desktop, CLI, and IDE launches do not depend on shell initialization. Verified with a fresh Codex process that loaded `gitpulse@gitpulse`, invoked `gitpulse_insights`, and received the live branch and change count.
- **`npm run mcp:install` and `npm run mcp:doctor`.** The MCP manifests spawn the bare token `gitpulse-mcp` off PATH — correct for a published plugin, but it means the server a client actually connects to is whatever is on PATH, which no build step owned. `mcp:install` puts it there through `cargo install`, so the binary is tracked and refreshable. `mcp:doctor` completes a real handshake and compares the version the server reports against this tree's, keeping *absent*, *unresponsive* and *stale* distinct from *matching* — the first of those is the one a naive check would report as a silent pass. Deliberately not in `ci:local`: CI has no reason to install the server, and a check that cannot run there must not be made to look like one that passed.

- **A repository that pins its own toolchain is told how to honour that pin.** Go, Swift, .NET and Dart still refuse when their runtime is absent — installing a language runtime is a host-wide change with no bounded, reversible command, and a coverage panel does not get to make that trade for you. But "install Go, then rescan" is a poor answer for a checkout that has already written down which Go it wants. When `mise.toml`, `.mise/config.toml` or `.tool-versions` is present, the refusal names the pin and the one command that honours it, and it names only a manager actually on PATH — a mise-only config never suggests asdf, which cannot read it. GitPulse still does not run it: the command writes outside the repository, so it stays on your side of the line.
- **Line count, coverage and storage now track the repository instead of the moment a panel was opened.** All three were one-shot fetches keyed on the repository *path* changing and nothing else, so a day of editing left the headline LOC number, the coverage report and the disk usage exactly as they were when the tab was first shown — with nothing on screen saying so. Meanwhile the backend already emitted `repo-changed` on every settled write and only `repoStore` listened. A new metric layer (`src/lib/metrics/freshness.ts`) owns freshness for all three: one measurement serves every panel, the watcher revalidates it, and each metric carries its own debounce and minimum interval derived from what its command actually costs — 20 s for LOC, 30 s for coverage, 120 s for the storage walk, so a build writing continuously into `target/` cannot pin a 20-second tree scan. Change count needed none of this: `cmd_branch_stats` was already refetched by `handleRepoChanged`, and re-measuring it here would have run the same git subprocesses twice per change.
- **A value that could not be refreshed no longer looks refreshed.** A failed revalidation keeps the last good report — panels must not blank out on a transient error — but the snapshot carries `value` and `stale` independently, and every panel reads both. Storage shows an "out of date" badge, the Pulse LOC tile degrades to *partial*, and a truncated or stale reading is never recorded as a point on the line-count trend.
- **C/C++ coverage, via an out-of-tree instrumented CMake build.** This family used to be a flat dead end on the grounds that GitPulse cannot add coverage flags to a project's build files. The premise was right and the conclusion too strong: CMake takes compiler and linker flags on the *configure command line*, into a *separate* build directory, so `cmake -S . -B build-gitpulse-coverage -DCMAKE_C_FLAGS=--coverage` → `cmake --build` → `ctest --test-dir` → `gcovr --lcov` produces coverage without touching `CMakeLists.txt` or the developer's own `build/`. gcovr installs into a project-local virtualenv, exactly as pytest already did. Make-only projects stay a dead end and say why: `make CFLAGS=--coverage` replaces a project's own flags rather than adding to them.
- **Storage now reports a reclaim audit, not just sizes.** The report published raw numbers and left every judgement to the reader — `gc_recommended` was a bare boolean, prunable worktree admin was not detected at all, and nothing said how many bytes were actually recoverable. Each row now carries the action that reclaims it, whether the bytes were measured or estimated, and whether a human has to decide. The headline counts *measured and safe* bytes only: estimates (a repack's saving depends on the content) and items needing review (committed build output, the reflog, a large file that may be someone's dataset) are carried as separate figures rather than folded into a total the repository cannot deliver. New detection: `.git/worktrees/<name>` left behind by a worktree deleted with `rm -rf`, which the old report counted as space belonging to a live worktree.

- **Manvi Control Plane & Operator Panel**:
  - `ManviHarnessPane`, `ManviOpsPanel`, and `HarnessBadge` integration for real-time agent session monitoring, task attribution, and operation gating.
  - Dedicated harness store (`harnessStore.ts`) with lease tracking, task status synchronization, and worktree association.
- **Durable WAL SQLite Ledger & Redaction**:
  - Embedded WAL SQLite database for durable append-only event logging across repositories.
  - Sensitive credential and secret redaction (`ledger/redact.rs`) before persisting mutation records.
  - Idempotent catch-up replay of agent transcripts and reflogs for automated attribution even when the app is closed via `gitpulsed`.
- **Diagnostics & Health Reporting**:
  - In-app Diagnostics modal displaying real-time system metrics, IPC command performance, crash logs, open file descriptor usage, and environment context.
  - Machine-readable and exportable diagnostics report generator for troubleshooting.
- **Enhanced Multi-Tab File Viewer & Editing**:
  - Multi-file editor tab management (`editorTabs.ts`) with persistent draft preservation across workspace switches (`editorDraftRegistry.ts`).
  - Serial file saving queue (`serialSave.ts`) preventing write collisions and index locking.
  - Interactive merge conflict editor (`ConflictEditor.svelte`) with visual diff resolution and instant staging.
  - MarkDev viewer and rich media viewers for markdown and media asset inspection.
- **LivePulse Analytics Dashboard**:
  - Dynamic interactive pulse dashboard (`LivePulseDashboard.svelte`) and export modal (`PulseExportModal.svelte`).
- **IPC & Wire-Type Safety**:
  - Expanded IPC surface to 136 verified Rust `cmd_*` handlers with zero untracked orphans.
  - 762 wire-type contract fields strictly synchronized between Rust Serde structs and TypeScript interfaces across 48 contracts.

### Fixed

- **The commit graph drew branches it had no name for.** The history walk asked git for `--all` — every namespace under `refs/` — while the ref listing read only `refs/heads`, `refs/remotes` and `refs/tags`. Anything else opened a lane nothing in the UI could label. That is not a corner case: agent harnesses keep a ref per turn (`refs/cmux/last-turn/*`, `refs/codex/turn-diffs/*`), `git maintenance` writes `refs/prefetch/remotes/*`, CI mirrors write `refs/pull/*`. On one repository here, 18 turn-checkpoint refs — each a stash-shaped pair forked from the *same* base commit — turned 65 commits of straight history into 101 rows and 35 lanes: 34 anonymous rails descending thirty-odd rows to a shared parent far below the fold, and at the default graph width 21 of those rows drew their node off-canvas with nothing on screen to say so. The lane solver was not at fault and is unchanged; its output was internally consistent throughout. Which refs the graph is about is now one decision (`graph/ref_scope.rs`) answering both the walk and the labels, with a contract test pinning that they agree — the drift that made unnameable lanes possible cannot recur. The default scope is branches, remote-tracking branches, tags and HEAD; **All refs** in Settings → Graph restores the old reach and labels those refs by their full path instead of leaving them blank. Same repository, default scope: 65 rows, 8 lanes, every node on screen.
- **History the graph does not draw is now reported rather than simply absent.** A narrower walk that says nothing is the same failure as a check that could not run reporting a pass. Each load counts the commits reachable only from refs outside the walked set and names the namespaces holding them — *"36 commit(s) reachable only from refs outside branches, remotes and tags are not drawn. Those refs live in: refs/archive/\* (1 ref(s)), refs/cmux/\* (18 ref(s))"* — as a diagnostics breadcrumb, not a banner. It stays silent when nothing is missing, including the common case of a custom ref that merely points at a commit already on a branch: that commit is drawn, so there is nothing to report. Both probes are bounded, and the ref scan runs only when the commit probe has already found something.
- **Hardening pass over the ref scope, driven by tests written to break it.** Six defects, each now covered by a test that fails against the code as it stood. (1) The named-ref test matched a string prefix, not path components, so `refs/headsfoo/x`, `refs/tags-archive/x` and friends were classified as branches git does not walk — their commits were neither drawn *nor* reported, the worst of both. The classifier is now checked against git itself over a table of adversarial names rather than against our belief about `--branches`. (2) That misclassification could leave the report claiming hidden commits with no namespace behind them, printing a sentence that trailed off after "live in:". (3) The bounded namespace list was ordered alphabetically, so a repository with ten tiny namespaces and one holding ten thousand refs named the ten and hid the one that explained the graph; it ranks by size now. (4) `refs/stash` was described as `refs/stash/*`, a directory it does not have. (5) The all-refs scope produced one decoration per ref with no ceiling — a CI mirror's `refs/pull/*` would have shipped six figures of chips over IPC on every graph load. It is capped, and the cap is *reported*, which also closes a pre-existing hole: tags have been silently truncated at 200 since the cap was written, so a repository with 250 of them showed 200 chips and said nothing. (6) A 209-character agent-checkpoint ref path drawn whole pushed the commit summary off the row; chips fold to the namespace and keep the full path in the title.
- **The "Refs drawn" setting did nothing.** Changing it updated the preference and stopped there: the graph's fetch scheduler keys requests on path, revision and query, so the key never moved, the scheduler saw a request it had already served, and nothing reloaded until an unrelated event happened to refresh the pane. The scope is part of the request identity now. The scheduler's own 200-iteration fuzz harness could not have caught this — its no-loss invariant compared the settled load's key against the latest key, both from the same key function, so a key that fails to distinguish two different requests satisfied it trivially. It now compares the request itself, and reproduces the bug at seed 1.
- **A degradation that appeared while history stood still was never reported.** `loadGraph` short-circuits on a structurally identical payload to keep canvas caches alive, and that return came *before* warnings were handled. Warnings are not part of the rendered-history signature, so a ref listing that started failing, or a namespace that started hiding commits, was silently swallowed on every quiet repository. Whether history changed and whether the load degraded are two different questions, and they are asked in that order now. Warnings are also logged when the SET changes rather than on every load: the watcher reloads on every settled write, the diagnostics ring only coalesces *consecutive* repeats, and two persistent warnings alternate — filling the ring with the same pair forever and burying everything else.
- **The real-repository smoke check labelled under a different scope than it walked.** It walked under the scope being tested and then called `list_ref_decorations` with `RefScope::Named` hard-coded, reproducing the original defect inside the harness meant to detect it: an all-refs dump of MarkDev showed 35 lanes and 7 labels. Same scope on both sides now — that dump carries 26 refs, and every lane-opening tip has one.
- **Pulse metrics counted machine-written commits as work.** The analytics walk was the same `--all`, so on a repository with agent-harness namespaces every contributor and churn figure included commits no person wrote — a metric measuring the tooling. It now walks the same named set as the graph.
- **The real-repository smoke check ran a walk the app does not perform.** `real_repo_smoke.rs` shelled out to its own `git log --all`, making it a *third* independent copy of "which refs the graph is about" — so it could not have caught this, and its committed fixtures described a pipeline that no longer existed. It goes through `GitReader` now.

- **`parse().unwrap_or(0)` made three more counts read as zero when they were enormous.** Same class as the `--shortstat` bug and found by sweeping for it: `git rev-list --left-right --count` (a wildly diverged branch reporting as *in sync*), `git count-objects -v` (the largest repositories reporting as holding no git data), and the three `--numstat` parse sites. All now saturate through one shared helper that keeps the other half of the rule intact — `--numstat` writes `-` for a binary file, and there zero really is the right answer, so text that is not a number stays absent rather than saturating to the maximum. (`cvss_to_severity` was checked too and is already correct: an unparseable score falls back to `high`, not to zero.)
- **A metric subscription could attach to a cell that had just been evicted.** Opening one more repository than the tracking bound, while every existing entry was being watched, evicted the cell that had just been created — the only one with no listeners yet, because `subscribe` registers its listener after the cell exists. The subscriber then held a reference to something no longer in the map: it never fired again, and the panel read *idle* forever. The bound is now soft, and the cell being created is never a candidate. Found by a randomized soak test, not by a hand-written case.
- **Two watcher tests carried their own 8-second budget instead of the suite's shared one.** `panicking_on_change_cannot_leak_the_watch_slot` failed once under `cargo test --workspace` and passed 3/3 in isolation: FSEvents delivery plus the 400 ms debounce stretches far past its idle latency when the whole workspace runs at once, which the neighbouring test already documents and allows 20 s for. Both now use `PRIME_DEADLINE`, raised to match. Costs nothing on a passing run — every user is a loop that breaks on success — and only lengthens how long a genuinely broken backend takes to be declared broken.

- **The repository's headline line count was wrong for every language with block comments.** `LocCounter` classified a line by one test — does it start with this language's single comment prefix — so a four-line `/* … */` counted as four lines of *code*, a Python module docstring counted as code, and a CSS or HTML comment counted exactly one comment line (the opener) with the rest as code. The `_ => "//"` catch-all was also wrong for PowerShell, CMake, Git Config, GDScript, OCaml, PureScript and WebAssembly, and a UTF-8 BOM hid the first comment of any file an editor had written one into. Measured on GitPulse's own source: 10,557 of 188,004 "code" lines — 5.6%, across 339 of 719 files — were comments. Replaced with a per-language scanner that tracks block-comment nesting, docstrings and string literals, so a `"/*"` inside a string can no longer open a comment that swallows the rest of the file. The bound that makes it safe: a single-line string cannot cross a newline, so one unbalanced apostrophe in YAML prose can no longer reclassify every comment below it. `language.rs` no longer keeps a second comment table that could disagree with the first — it did, returning `Some("/*")` for CSS, a *block* opener applied as a line prefix.
- **Go, JavaScript and PHP coverage claimed a tool was ready without ever probing it.** Go was the plain case: it was the only ready-producing planner in the file that probed nothing at all, so a `go.mod` on a machine without the Go toolchain published a Run button that failed at spawn — a check that never ran, reported exactly like a check that ran and passed. JavaScript emitted `npm run …` and PHP emitted `vendor/bin/phpunit` on the same unexamined assumption. All three now probe, and a shared test asserts no planner may return a plan that is simultaneously "ready" and unrunnable.
- **A `git diff --shortstat` count too large for `usize` parsed as zero.** `unwrap_or(0)` meant the largest possible diff reported as *no change* — a reading a caller cannot tell apart from a genuinely empty diff. Counts now saturate.

- **Credentials named by a JSON object key reached the ledger, the durable log and the diagnostics report in full.** Both redactors — `ledger/redact.rs` (write path for ledger rows, every `logging.rs` line, and `gitpulsed`) and `diagnostics.ts` (the copyable report) — traversed JSON objects by value and discarded the key. An opaque token therefore matched no pattern and was written through unchanged, so `{"access_token": "…"}`, the shape of every OAuth response and most API error bodies, was stored and displayed intact while the identical secret one syntax away (`access_token=…`, `--access-token …`) was redacted. Object keys are now consulted against the same credential-name table the CLI-flag path already used, normalized across `client_secret`, `clientSecret`, `X-Api-Key` and `ACCESS_TOKEN`, with compound names such as `github_token` covered by suffix and non-credential names such as `public_key` and `cache_key` deliberately left alone. Non-string values under a credential key fail closed rather than being recursed into.
- **A credential used as a JSON object key was never scanned at all.** The other half of the same blindness: a vendor-shaped token in key position — a cache or rate-limit map keyed by the credential — was emitted verbatim while the same token in value position was redacted. Keys are now scanned through the same boundary, and a rename that would collide with an existing key is disambiguated rather than allowed to overwrite, so no entry is silently dropped.
- **The two credential-name tables had nothing binding them together.** `SECRET_FIELD_NAMES` exists once in Rust and once in TypeScript, and `redact.rs`'s own comment warns that drifting tables are "discovered by the leak" — but no contract test compared them. `scripts/diagnostics-contract.test.ts` now derives both lists from the source that owns them and fails on any divergence, on a missing key-consult or key-scan on either side, and on the bare word `key` entering the suffix table (which would redact `public_key` and `cache_key` and gut the report).
- **Discarding an untracked file was gated and recorded as a modification, not a deletion.** `cmd_discard_changes` declared `op = "modify"` unconditionally, but `GitWriter::discard_changes` runs both `git restore` and `git clean -f` against the path: for a tracked file the restore acts, for an **untracked** one the clean acts and the file is removed. `op` is sent to the policy sidecar as `policy.check.file`'s `op` and recorded in the ledger as `file.<op>`, so every discard of an untracked file asked the gate to judge — and the durable record to state — a gentler act than the one that ran. The operation is now resolved from the index via a new `GitReader::is_tracked` (`:(literal)` pathspec, so a path of `*` cannot be answered for by some other file), and an unreadable index fails closed to `delete` rather than to the gentler claim.
- **Two new contracts bind the gate to the command that actually runs.** `command-policy-contract.test.ts` proved a gate was *present*; nothing proved it was told the truth. `file-gate-fidelity-contract.test.ts` derives the destructive writers from source and fails when a command that may delete declares a fixed `modify`/`write`/`create`. `command-gate-fidelity-contract.test.ts` compares, for 17 guarded commands, every literal flag handed to `guard()` against the flags its `GitWriter` can actually pass — following private helpers such as `commit_inner` transitively — and requires every command it cannot compare to be documented as derived, so a new command can neither drift nor fall silently out of the contract. Audited all 38 guarded commands: no argv drift existed beyond the discard case above.
- **The five local-AI features were exercised by no test in CI.** `generate_commit_message`, `explain_commit`, `suggest_branch_name`, `fix_health` and `coverage_report` were reachable only through `local_ai_live.rs`, which is opt-in behind `GITPULSE_LIVE_AI=1` and needs a real model server — so 482 lines of `ai/mod.rs`, including prompt assembly, the token budget and the reply path, ran in no automated check. They are now driven end to end against a loopback model server over a real socket, with the MANVI binary forced absent so the assertions land on the *degraded* path — the ordinary case for a user who has never installed the harness. The pipeline must still produce a result AND say the harness never parsed the reply: degrading is correct, degrading silently is not. The first version of this test was itself machine-dependent — a real `manvi` on the developer's PATH answered `chat.prepare`/`chat.settle`, so it silently exercised the harness-present path and asserted nothing about the degraded one; the binary override makes the outcome the same on every machine.
- **Every codeintel read surface reported success on a repository whose code map was never built.** `search`, `impact`, `dependencies`, `dead_symbols` and `trace_between` opened the devmap database, found no indexed generation, and returned `available: true` with an empty list — so a repository mid-index answered "no callers", "no dependencies" and "no dead symbols", which reads as verified-clean rather than not-yet-known. `status()` reported the *same* repository as unavailable at the same moment. All five now go through a new `open_indexed_store` that requires a generation; `status` deliberately keeps the looser gate so it can still tell "no database", "no generation yet" and "database unreadable" apart, since those call for three different actions.
- **Nothing stopped two native menu items from claiming the same accelerator.** A collision is silent — muda binds one item and the other's shortcut simply never fires — and `build_native_menu` cannot be unit tested to catch it: `muda::MenuChild` can only be constructed on the main thread, so calling it from a test harness panics in the platform layer (verified by running it, not assumed from the comment). `view-menu-contract.test.ts` now derives every accelerator from the menu source, resolving `&str` constants and skipping `PredefinedMenuItem` title arguments, and fails on any duplicate. It is cfg-aware, because Settings binds `CmdOrCtrl+,` twice on purpose — once under `cfg(target_os = "macos")` and once under `cfg(not(...))` — and those never coexist in one binary.
- **Redaction cost is now linear in the number of object keys.** Disambiguating a redacted key that collides with another probed for a free name linearly from `#2`, which is quadratic on precisely the input that is cheapest to construct: N distinct tokens of equal length all redact to identical text, so key *n* paid *n* probes. Measured at 20,000 such keys: 31s before, 277ms after, with all 20,000 entries preserved. Guarded in both languages by a budget that always runs.
- **A complexity check silently skipped itself on fast machines and failed on loaded ones.** `markDevParser`'s quadratic-growth test compared wall-clock timings behind `if (small > 5)`, so when the small case rounded under 5 ms the assertion never ran and the test reported green having asserted nothing; when the machine was loaded the same tiny denominator made the ratio explode and the build failed on machine speed rather than on complexity (observed 13.7 against a threshold of 12, with the correct implementation). The always-running absolute budget is the real guard — measured, the unbounded pattern misses it by 2.5x — and complexity is now asserted structurally: every asymmetric bracket scan (`[^\]]`, `[^)]`) must carry an explicit bound, while the symmetric delimiters that fail fast are correctly left alone.
- **Timing-based tests failed on a busy machine, for reasons that had nothing to do with the code.** Ten suites asserted absolute wall-clock budgets, and seven more relied on Vitest's 5-second default timeout; both encode the speed of the machine they were written on. Measured under a concurrent build (load average 45 on 18 cores): ten test files failed, `GraphRendering.rails` taking 31,420 ms where it takes 2,198 ms on a quiet machine — a 14x spread on an identical tree, and every failure a false alarm. Raising the numbers would have bought quiet by weakening the guard on a fast machine, so budgets are now expressed as multiples of work the machine can do *right now*: `src/lib/__tests__/perfBudget.ts` times a fixed-instruction reference loop immediately after the measured work, and the ratio cancels the machine out of the assertion, leaving the algorithmic claim the test actually means. The unit counts were read off a `GITPULSE_PERF_REPORT=1` run rather than guessed — the tightest case had only 3.6x headroom and is now at 11x. Stress and fuzz cases whose assertions are invariants rather than speed carry an explicit `STRESS_TIMEOUT_MS` instead, which weakens no check: a real non-termination still fails, just later. Verified by saturating the machine to load average 117 with 54 CPU hogs on 18 cores and running the whole suite: 3,355 of 3,355 passed.
- **`build_native_menu` is now reachable from a test, and the recent-repositories branch with it.** `muda::MenuChild` can only be constructed on the process main thread, and a libtest harness runs every case on a worker — so the largest function in the desktop module, carrying every menu entry and accelerator, ran in no test. The constraint is on the thread, not on testability: `src-tauri/tests/native_menu_main_thread.rs` is declared `harness = false`, giving it a `fn main` that *is* the main thread. Reaching the populated branch also meant extracting `set_recent_menu` from `cmd_set_recent_menu`, whose `#[tauri::command]` signature had bound the cap, the state write and the menu rebuild to the concrete Wry handle no test can construct; the command is now the thin wrapper it should have been, and the cap, its ordering, degenerate paths and the return to the empty placeholder are all covered.

- **The plugin manifest advertised a version the binary did not have, and the release gate could not see it.** `plugin/plugin.json` said `0.0.4` while `package.json`, `package-lock.json`, `tauri.conf.json`, `Cargo.toml` and `Cargo.lock` all said `0.0.5`; `check:release` gated exactly those five files and printed *OK: all version sources agree on 0.0.5* while the drift sat one directory away. The MCP binary answers `initialize` with the crate version, so a client would have recorded an 0.0.4 install against an 0.0.5 server with nothing in the handshake to reveal it — confirmed by the install, which now records 0.0.5 only because the manifest was corrected first.
- **The release gate now *discovers* plugin manifests instead of naming them.** One package ships a manifest per agent client (`plugin.json`, `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`) and that set grows whenever a client is added, so a hand-maintained list stops covering the newest manifest — the one most likely to be wrong. The gate walks `plugins/<name>/`, requires `plugin.json` in each package so a deletion cannot read as "nothing to check", fails closed when no package exists at all, and checks every optional client manifest it finds. It went from 6 version sources to 9 without a path being added by hand, the Codex manifest among them.
- **`gitpulse-mcp` on PATH was an untracked copy that would have gone stale in silence.** The binary at `~/.cargo/bin/gitpulse-mcp` had been placed by hand: byte-identical to the release build at the time, but absent from Cargo's install records, so nothing tied it to the repo and the next rebuild would have left the connected server serving an older tree while still answering handshakes normally. `cargo install` reported it as replacing package `unknown`, confirming the missing provenance. It is now a tracked install, refreshable with `npm run mcp:install` and checkable with `npm run mcp:doctor`.
- **A Claude Code manifest under `plugin/` could never have been committed.** `.gitignore`'s broad `.claude*` rule matches `.claude-plugin/` at any depth, and only the root marketplace and the canonical package carry negations — so a `.claude-plugin/plugin.json` placed beside the legacy package was invisible to git and would have been absent from a fresh clone, failing there as "missing manifest" rather than "ignored file", far from the cause. The package converged on the one canonical directory, and `plugin-contract.test.ts` now asserts that every file under a marketplace source survives `git check-ignore`.
- **The contract-test table's exemption list is derived rather than hand-kept.** `documented-counts-contract.test.ts` held a literal set of "tests that belong to a script", so every new script test had to be added by hand or the table would demand a contract row for a plain unit test. A `foo.test.ts` beside a `foo.mjs` is now recognized as that script's unit test, with `vite-config` — whose subject is `vite.config.ts` — the one documented exception.
- **Blame had two ways to name a file, and one of them could disagree.** It carried a path box because it was reachable with nothing selected; the store's `selectedFilePath` was the other. As a section of Code — with Explorer one click away and sharing that selection — the box is gone. Its one irreplaceable job came back explicitly: retrying a failed blame used to mean re-typing the path and pressing Enter, and is now a **Retry** button in the error state.
- **A latent startup panic in the native View menu.** The menu was built from nine hand-unrolled indexes into `VIEW_TAB_BINDINGS`, so shortening that list — exactly what consolidation does — would have panicked at launch, and lengthening it would have dropped the extra view with nothing to say so. It is built from the list itself now.
- **A stale-session race in `applyToSession`.** Callers spread a session captured *before* an await and wrote it back after, so a concurrent update landing in between was silently overwritten. It accepts a patch function receiving the live session, and the three call sites that switch to History's diff section use it.

## [0.0.4] - 2026-09-03

### Fixed

- **Storage report accuracy & hardening**:
  - **Premature truncation resolved**: Raised per-directory entry limits inside build artifact directories from 4,000 to 100,000, preventing Cargo dependency directories (`target/debug/deps`) with > 4,000 files from prematurely tripping scan truncation.
  - **Unix hard link deduplication**: Scoped `(st_dev, st_ino)` tracking on Unix for files with `nlink > 1`, ensuring Cargo hard links (`deps/libfoo-hash.a` to `libfoo.a`) are counted exactly once, eliminating gigabytes of phantom disk usage.
  - **Monolithic container roll-up**: Container build directories (`target`, `node_modules`, `.venv`) roll nested build outputs (`debug/build`, `.../out`) up into the parent scope rather than fragmenting into dozens of child rows.
  - **Source-tree false positive protection**: Paths inside `src/` no longer match generic build or cache directory names (e.g. `src/lib/coverage` remains recognized as source code rather than an unignored cache).
  - **Single-pass worktree traversal**: Merged large-file collection directly into the worktree walker using `WorktreeWalkContext`, eliminating the redundant second walk and halving disk I/O.
  - **Developer and agent caches classified**: Recognized `.devcouncil`, `.gitnexus`, `.claude`, `.cursor`, `.agents`, `.gemini`, and `.antigravity` under cache artifacts.

- **Windows: cloning a repository failed.** The clone destination was resolved with `canonicalize`, which on Windows always answers with a verbatim `\\?\C:\...` path. git refuses that as a working tree (`could not create work tree dir ...: Invalid argument`), and the prefix leaked into the refusal text users saw. Resolution now produces a spelling external tools accept.
- **Windows: cloning into an existing directory resolved onto the source repository.** The clone's directory name was derived by splitting the URL on `/` alone, so a local Windows path (`C:\src\repo`) survived whole and the drive-colon split returned a *rooted* `\src\repo` — and joining a rooted path discards the destination. Both path separators are now split on, and the derived name is guaranteed to be a single component.
- **Windows: external tools resolved to the wrong file.** Tool lookup tried the bare program name before any executable suffix, so `npm` matched Node's extension-less shell script instead of `npm.cmd` and failed with "%1 is not a valid Win32 application". Suffixed spellings are now tried first, in the order Windows itself uses.

### Changed

- CI runs the Rust suite with `--no-fail-fast`. It previously stopped at the first failing test binary, so a failure in the library suite silently skipped every integration suite — 43 binaries that had never run on Windows at all — while reporting a single failure. A check that could not run must not read the same as one that passed.

## [0.0.3] - 2026-09-03

### Added

- **Pulse view** (`pulse`): local contribution heatmap, streaks and after-hours punch card, weekly line changes, reconstructed LOC trend, churn-by-extension, commit hygiene, churn×coverage hotspots, knowledge/bus-factor, code age, and tag-based DORA. One bounded `git log --numstat` walk plus optional blame/tag scans. Truncation, payload-budget cuts, and failed language scans are visible states, never a quiet `0`.
- MCP 2.0 (`2026-07-28`) on `gitpulse-mcp`: mandatory `server/discover`, per-request `_meta` (protocol version + client capabilities), `resultType` on results, cacheable `tools/list` (`ttlMs` / `cacheScope`). Dual-era: legacy `initialize` still answers 2024-11-05 / 2025-11-25 clients.
- Agent Plugins 1.0 package at `plugin/` (`plugin.json`, `mcp.json`, skills). Settings copies the manifests; Work view surfaces agent-session counts and overlapping dirty files.
- Read-only insight tools: `gitpulse_insights`, `gitpulse_active_changes`, `gitpulse_collision_risk`, `gitpulse_change_context`, `gitpulse_codeintel_dead_symbols`.
- Work view insight strip and collision banner. Health view states a failed dead-code check separately from “no unreferenced symbols”.

- GitHub panel lists open issues (already fetched by `cmd_github_context`) and a **New pull request** action that opens GitHub's compare form for the current branch onto the default branch.
- Cherry-pick and revert from the commit context menu, using the store commands that previously had no discoverable UI.
- Remotes panel: add, rename, set URL, prune, and remove, with a two-step confirm that names the cost for prune/remove/set-url.
- Tag create/delete/checkout from the branch list. A capped or unreadable tag list says so instead of looking like the whole history.
- Submodule sync and deinit (working-copy remove) alongside initialize. Deinit never passes `--force`.
- Zero-dependency vector language logo icon system covering 34+ programming languages, configuration formats, and markup types (`LanguageLogo.svelte`, `languageLogos.ts`).
- Integrated language logos across LanguageBar, FileTreePanel, FileViewer editor tabs, Sidebar staged/unstaged changes, CommitDetails changed files list, DiffViewer toolbar, LivePulse dashboard, and CommandPalette symbol/file searches.
- File path hierarchy formatting (`formatPathParts`) in Sidebar and CommitDetails, dimming directory paths and highlighting filenames for improved scannability.
- Interactive LanguageBar pills that switch directly to the Files tab and dispatch custom filter events.
- Diff view file rail and commit picker (`DiffFileRail.svelte`, `commitRail.ts`): browse changed files and move between recent commits directly within the Diff view without returning to the Graph view, with uncommitted changes prioritized, commit truncation indicators, and zero IPC overhead.

- Control plane: GitPulse now records what agents do to a repository and judges it.
  A durable WAL SQLite ledger at the Rust guard seam records every mutation with the
  verdict it ran under, redacted before insert. Worktrees bind to DevCouncil tasks so
  a write outside a task's plan is refused and recorded. Agent transcripts and the
  reflog are replayed idempotently on open, and by `gitpulsed` on an interval, so the
  history is attributed even for sessions run with the app closed.
- Work view (`F10`): tasks, worktrees, pull requests, workflow runs, policy verdicts
  and grants joined into one row per task. Each of its five sources reports whether
  it could be read, and a verdict the build cannot parse is counted as unreadable
  rather than allowed.
- Git-native provenance. A completed `CI:local` run against a clean working tree is
  recorded as a verification note under `refs/notes/gitpulse/`, and branches, pull
  requests and commits carry a freshness badge that decays with distance from the
  default branch. A commit whose notes could not be read is never shown as verified,
  and an unverified commit at the tip is never shown as fresh.
- Code intelligence answered in-process from DevCouncil's persisted map — impact,
  symbol search and dead-code detection — with no daemon and no parser. The Health
  view states whether a code graph exists at all, so three features going quiet is
  distinguishable from a graph that looked and found nothing.
- `gitpulse-mcp`, an MCP server exposing the control plane read-only to an agent,
  held in step by a contract deriving both its advertised tools and its dispatch arms
  from its own source.
- A crash now leaves evidence behind. The backend's diagnostics ring was memory-only,
  so the panic hook recorded the one entry explaining a crash into a buffer that died
  with the process producing it. Entries are now also appended to a per-binary log
  under the platform log directory (`GITPULSE_LOG_DIR` overrides it), rotated at 1 MB
  across two generations, and read back through `cmd_diagnostic_persisted_log` — so
  after a relaunch the previous session's last words are still there. The panic hook
  also records a bounded backtrace, and the release profile now strips debuginfo only,
  keeping the symbols that make those frames name anything. Nothing leaves the machine.
- `gitpulsed` and `gitpulse-mcp` install the logger and the panic hook the GUI installs.
  Both ran with neither: a panic in either left a closed pipe or a stalled ingest loop
  and no record anywhere of why. `scripts/diagnostics-contract.test.ts` derives the
  binary list from `Cargo.toml`, so a fourth binary cannot join them silently.
- Failed clones and rebases reach the diagnostics report. Both modals caught their
  errors, showed a banner and recorded nothing, leaving `clone` and `rebase` declared
  in `PanelSource` and used by no one; the contract test now fails on any panel source
  that is declared and silent.
- IDE-style file viewer, promoted to the primary work tab.

### Changed

- Pulse export card rewritten so it can be read away from the app: each tile
  states what its number means instead of carrying a bare label, tiles are
  grouped by where the number comes from (commit log vs. blame and working
  tree), and each caveat sits on the tile it applies to — `CAPPED` commit scan,
  `PARTIAL` language or blame scan. The card also carries an accessible
  `<title>`/`<desc>`, emits pure ASCII, namespaces its stylesheet so inlining it
  cannot restyle the host page, and no longer depends on CSS features
  (`text-transform`, geometry `rx`) that standalone SVG renderers ignore.

- Commit graph: the default branch is one straight rail. The lane solver now
  reserves the default branch's first-parent chain (`main`, or the repository's
  own default; `origin/main` when it is ahead of the local branch; HEAD when
  none of those is loaded) before walking any row and pins it to the leftmost
  column in one colour for the whole loaded window. Previously whichever chain
  reached a shared ancestor first in `--topo-order` owned it, and because git
  lists a merged feature's commits above the main commits they forked from,
  `main` jogged into the feature's column at every such merge. Feature chains
  now always close into the mainline column; at a window cut the rail ends with
  a stub instead of continuing into a merged-in branch, so rows keep their
  columns when older history is loaded. Rows carry `is_mainline`, the payload
  carries `mainline_id` and `mainline_name`, and the graph tooltip and the
  keyboard-focus announcement name the rail.
- Commit filters run in the backend and keep the graph connected. Every
  filter term (`author:`, `sha:`, `type:` and `fix:`-style prefixes, free
  text, and `path:`) is now applied by `cmd_get_commit_graph`. Previously only
  `path:` reached git; the other terms removed rows in the client after lanes
  were solved, so a filtered graph was a field of fading stubs. A dropped
  commit now hands its lineage to its children — the parent rewriting git does
  for `--parents` with a pathspec — so survivors connect to their nearest kept
  ancestors, a survivor whose ancestors were all dropped becomes a root of the
  filtered view, and the straight mainline re-anchors on the first survivor of
  the default branch's chain and keeps its name. A filter or branch change
  over cached rows shows the progress bar while the new view loads, and a
  query that differs only in whitespace never re-walks history.

- Work view keys on worktrees when there is no DevCouncil task store, so a repository driven by Claude Code, Cursor, or by hand is one row per checkout instead of a single "Not bound to a task" bucket. Remotes, stash and submodules fold into a collapsed section on that screen; the separate Repo view is gone.
- Agent worktrees are recognised by the `/.<agent>/worktrees/` layout, not only Claude Code's `.claude/` directory. Git's own `.git/worktrees/` metadata is never labelled an agent session.
- `gitpulse_status` reports worktrees, matching the description it already advertised.
- GitPulse builds from a checkout of GitPulse. The eight Rust crates it links from
  Manvi and DevCouncil are vendored under `src-tauri/vendored/` instead of reached by
  relative path, so a lone clone no longer needs two sibling repositories present.
  `npm run vendor:check` reports edits made here and drift from upstream, and says
  "not compared" rather than "matches" when a sibling is absent.
- Coverage floors are enforced rather than merely reported. `npm run check:coverage`
  validates both LCOV reports structurally before trusting any number and applies
  explicit floors (frontend 90% lines / 85% branches, Rust 80% lines). An unparseable
  report exits `2`, distinct from `1` for a missed floor, so a check that could not
  run never reports the same result as one that ran and passed.
- Workflow linting via `npm run check:workflows`. `release.yml` runs only on a `v*`
  tag, so nothing else in CI ever read it; actionlint now covers all workflows in
  both `ci.yml` and `ci:local`.
- The IPC type contract checker compares wire types, not just field names. A shared
  field whose normalized Rust type or backend-required presence disagrees with its
  TypeScript declaration is now a failure.
- Opt-in release checks: GitPulse can check for a newer tag when asked. It does not
  auto-update and does not phone home on its own.
- Coverage tooling: missing toolchains are installed on request, completeness
  reporting is hardened, and reports can be copied out persistently.
- Repository recovery: GitPulse now identifies parked merge, rebase, cherry-pick,
  revert, `am`, and bisect operations and offers the appropriate continue, abort,
  or skip path instead of treating a conflicted repository as idle.
- Repository surfaces: inspect and manage remotes, browse and safely act on stashes,
  inspect and initialize submodules, and coordinate workspace-wide fetch/pull
  actions with a work-in-progress summary.
- Branch health verdicts in the sidebar. Each branch is classified from data already
  fetched — upstream gone, merged, diverged, stale, behind, unpublished — and only
  branches worth acting on draw an indicator, so the exceptions stay visible. The
  staleness threshold is a parameter with a documented default rather than a fixed
  number.
- Pull-request review velocity in the GitHub panel: how long each pull request has
  been open, how long it waited for its first review, and a median across the queue.
  Drafts are excluded and the median is used, so one pull request left open for a year
  does not become the headline figure.
- A commit cadence sparkline in the status bar, bucketed by local calendar days so a
  daylight-saving transition does not shift the boundaries. It reads the commits the
  graph already loaded and costs no extra fetch.
- Developer tooling: every script entry point answers `--help` with usage and exits 0,
  the contract checkers and the coverage floor checker emit `--json` for machines, and
  report columns align from their labels instead of hand-counted padding.
- A dev container (`.devcontainer/`) that installs the same Linux dependencies CI uses
  plus actionlint and cargo-llvm-cov, so `npm run ci:local` is runnable in it.
- Native Git mutations (`stage`, `unstage`, `fetch`, `stash save`, `stash pop`) route
  through the harness write gate and return the policy verdict alongside their output.
- Stash actions are reverified against both the stash index and object ID under the
  repository lock, so concurrent worktrees cannot target the wrong entry.
- Repository refreshes surface watcher failures and perform a compensating full poll,
  so stale state is not mistaken for a healthy watch. Git operations also use a
  non-interactive editor to prevent repository configuration from causing hangs.
- Dependency health parsing preserves the dependency type across the different audit
  shapes emitted by supported npm versions.
- The release workflow verifies draft assets against an exact per-platform manifest
  instead of matching filename patterns, and preflight runs every contract gate.

### Fixed

- Python coverage could not run on Windows: GitPulse refused every real
  project virtualenv there. Two causes. `resolve_on_path` joined a bare
  program name, so it never matched `python3.exe` and the PATH-identity check
  could not succeed on Windows at all; it now also tries the `.exe` spelling
  (only `.exe`, so it cannot resolve a `.bat`/`.cmd` shim). And the trust rule
  refused any interpreter resolving inside the repository — which on Windows is
  every virtualenv, because `venv` does not symlink the interpreter out as it
  does on Unix. Measured on CPython 3.12.10: it installs a 274,424-byte
  launcher where the base interpreter is 104,952, byte-identical to
  `<install>\Lib\venv\scripts\nt\python.exe`. A repository-resident
  interpreter is now admitted, but only when its bytes equal the host
  interpreter or a launcher that host installation ships, found by scanning
  that directory rather than assuming a file name. `pyvenv.cfg` must exist as
  the virtualenv marker but is never read: it is repository content, and
  nothing a repository writes should nominate the bytes GitPulse executes.
- Submodule pathspec validation was fail-open on Windows. The rooted-path
  refusal was built on `Path::is_absolute`, which is false for
  `/absolute/path` there (no drive), so a pathspec refused on Unix reached
  argv on Windows; the traversal check split on `/` only, so
  `vendor\..\..\etc` passed it on both. Rooting is now tested directly
  (leading separator, drive prefix) and both separators are split.
- Watcher: an event naming the git directory itself now classifies as
  noise. It says only that something under it moved, which the recursive
  watch on that directory already reports in detail. Windows delivers that
  event for every git-internal write via the non-recursive worktree-root
  watch, so lockfile churn alone was reading as repository change there.
- Eight Windows-only test failures that the newly-running Rust job exposed:
  a Windows path embedded unescaped in a JSON fixture, `/`-pinned path
  assertions, POSIX-only filename bytes (`*`, `"`, `?`) that the Win32
  filename parser rejects outright, and `core.autocrlf` rewriting a fixture
  on checkout.
- Release asset manifest updated for `tauri-action@v1`, which versions the
  macOS updater archive (`GitPulse_0.0.3_universal.app.tar.gz`) where v0
  emitted it bare. All three build jobs were green and had uploaded a
  complete set, so the manifest check was the only thing that noticed.
- The Windows Rust jobs had never compiled. `Rust Cargo Clippy` and
  `Rust Unit & Integration Tests` sat behind the Vitest step that failed
  first, so a step that never ran had been reading as a step that passed.
  Three tests called `std::os::unix::fs::symlink` outside any `cfg(unix)`,
  and `sandbox_security` imported `git_text` for a Unix-only caller. The
  two symlink-only coverage tests are now Unix-gated; the pytest `--ignore`
  confinement test keeps its absolute and `..` cases on Windows and gates
  only the symlinked one, so the contract is still checked there.
- The export card no longer renders an unmeasured metric as `0`. A failed
  language scan, and a blame scan that has not completed, now reach the card as
  null and render as an em dash with the reason, matching what the Pulse view
  already said on screen.
- The card's commit count and its active-day count now come from one
  population. With an author filter applied it previously paired the whole
  scan's commit total with that one author's active days.
- Windows CI: three `scripts/*.test.ts` suites failed there only. Two used a
  `file:` URL's `pathname` as a filesystem path, which is "/D:/a/repo/..." on
  Windows — one threw ENOENT on a doubled drive letter, the other silently
  scanned nothing and asserted against an empty list. The third compared a
  `path.relative` result against a slash-separated expectation. Repo-relative
  paths in the IPC contract report are now slash-form on every platform, and
  `portable-paths.contract` fails the build if the `pathname` shape returns.
- macOS CI: the grandchild-holding-pipes regression test budgeted 8s for a
  span that is two `DRAIN_JOIN_GRACE` windows plus `spawn_gate()` queueing,
  which parks with no timeout and is unbounded under the parallel harness. It
  took 8.9s on the runner. The budget is now expressed in terms of the grace
  constant plus a scheduling allowance, still far below the 30s that would
  mean the hang it guards against had returned.

- Resource exhaustion under multi-repository fan-out. Nothing bounded how many
  child processes the git engine kept alive at once, and a GUI launch inherits a
  soft `RLIMIT_NOFILE` of 256 from launchd. A workspace-wide fetch refreshed up
  to 64 repositories with a plain `Promise.all`, each refresh issuing five
  git-spawning commands, so hundreds of `git` processes and their pipe
  descriptors, drain threads and output buffers existed simultaneously. The
  process ran out of descriptors and every later spawn failed with
  `Too many open files (os error 24)` — a state it never recovered from, because
  the UI retried into the same wall. Three changes close it: `engine::git_cli`
  now admits at most `2 x cores` (4-16) concurrent children through a spawn gate;
  startup raises the soft descriptor limit toward 16384 and says plainly in the
  log when it could not; and every IPC fan-out goes through one bounded pool
  (`lib/async/pool.ts`) instead of `Promise.all`.
- `ci_local`'s working-tree and HEAD probes spawned `git` directly, bypassing the
  engine's timeout, output cap, environment hardening, and the new spawn gate.
- Unbudgeted IPC payloads. `MAX_OUTPUT_BYTES` is a 64 MiB backstop against a
  runaway process, and the diff, blame and file-content readers were treating it
  as a payload size. Measured on one real commit that rewrote a 400k-line file:
  a 43.7 MB diff cost 144 MB of RSS in the backend (bytes, lossy `String` copy,
  then JSON) and 346 MB in the webview (string plus 533k parsed row objects) —
  ~490 MB for one click, on a viewer that renders at most 300k rows of it. The
  new `engine::budget` module gives every content-driven payload a budget taken
  from what its surface can render (diffs 8 MiB, blame 16 MiB, file content
  8 MiB), cut on a line boundary so no half-row parses as a whole one. The same
  commit now costs 37 MB and 136 MB. Diffs carry a `truncated` flag end to end:
  the viewer says which cut happened and disables hunk staging, because staging
  from a prefix stages less than the rows on screen imply.
- `stash show -p` was the one diff read still crossing IPC unbudgeted.
- Startup bundle. The plugin/MCP, insights and Work-view pass pushed the entry
  chunk to 853 KB, past the 780 KB ceiling the `gitpulse-bundle-budget` plugin
  enforces. The comparison that ceiling's own comment prescribes — vendor chunks
  unchanged means app code, changed means a leaked dependency — said app code,
  so the fix was to stop shipping views nobody has opened rather than to raise
  the number. Eleven tab views (Coverage, Health, Storage, Terminal, Code stack,
  GitHub, Manvi ops, Reflog, Blame, Conflict editor, Pulse) now load as their own
  chunks through a new `LazyView`, which caches each resolved view so only the
  first visit shows a pending state, and names the view if its chunk fails
  instead of leaving a blank pane. Work stays eager: it is the default tab, so
  deferring it would only add a round trip to launch. Entry chunk 853 → 543 KB,
  and because `TerminalPanel` went with them the 334 KB xterm runtime left
  startup entirely — 204 KB transferred on launch, down from ~590 KB. The split
  is pinned by `src/App.test.ts` so a plain import cannot quietly undo it.
- A latent race in the test suite. The harness sidecar is one process-global
  slot, so a test that installs a recording or refusing fake installs it for
  every thread — and only one of the six tests that reach it serialized. The
  observed failure was a scope assertion reading another test's request frame,
  but the same race passes just as easily: an "allow everything" fake makes a
  gating assertion succeed while proving nothing. Serialization now happens at
  the consumer funnel (`call_policy`) rather than being each test's to remember,
  through a reentrant guard that `set_test_binary` takes by reference, so
  installing a fake without holding it does not compile.

- Diff viewer word wrap: word wrap now opts out of fixed-height row windowing (`virtualize={false}` up to `WRAP_MAX_LINES`) so wrapped lines render in natural flow without clipping, line overlap, or pushing split-view columns off-screen.
- Diff sidebar commit picker component test suite (`DiffFileRail.test.ts`).
- A parked merge/rebase on a task-keyed Work row is shown and sorts first. Task mode previously left `operation` null, so a DevCouncil repository mid-rebase rendered as idle.
- An operation probe that throws degrades the Work screen rather than looking like an idle worktree. Bare worktrees no longer steal probe slots from later checkouts.
- A persisted `repo` view tab opens Work (its successor), not Graph.
- Work view refreshes when the repository's status generation changes, so a parked rebase or dirty count cannot sit stale next to a sidebar that already updated.
- Removing a worktree never passes `--force` for an unscanned tree (`dirty_files === null` is not zero). A scanned dirty tree is force-removed only after the armed confirm names the discard cost.
- Menu and palette "Pop stash" pop the listed top entry by object id through `cmd_stash_action`. The unaddressed `git stash pop` of `stash@{0}` is no longer on that path.
- A newly opened repository, and a restored tab with no saved view, lands on Work. The commit-search bar is shown only on Graph, Diff, Blame, Stack, and Reflog. ⌘F and native Search Commits switch to Graph from Work (and other non-filter views) instead of no-opping; Files still uses ⌘F for in-file search.
- `cmd_list_tags`, `cmd_list_remotes`, and `cmd_list_submodules` return a truncated flag. A listing cut by the cap is no longer indistinguishable from a complete one.
- The shortcuts cheat sheet lists Open (`⌘O` / `⌘T`), Clone (`⌘⇧O`), and Work (`F10`) to match the native menu.
- MCP `gitpulse_task_view` requires only `repo_path`, matching the handler.
- A failed clone no longer claims a cleanup it did not perform. git materializes
  `<dest>/.git` before transferring objects, so a failed clone leaves a skeleton that
  blocks every retry; the error said it had removed that skeleton whether or not the
  removal succeeded, sending the user into a retry that dies at the existence check
  with "Already cloned at ..." having just been told the path was clear. The message
  now reports the removal that happened, and names the leftover path when it could not.
- Accessibility: the commit context menu is reachable by keyboard (ContextMenu key
  and Shift+F10), and seven modal dialogs no longer suppress a11y rules to keep an
  event-plumbing click handler. Remaining suppressions state why they are correct.
- `SettingsModal` and `DiagnosticsModal` called an optional `onClose` without a guard.
- macOS: the application exits when the main window is closed.
- Terminal and diagnostics failures preserve their full context instead of truncating
  it across builds.
- Dependency coverage is reported accurately in the health panel.
- File-status churn warnings now remain visible in the UI with an uncertainty marker
  and explanation instead of looking like verified counts.
- GitHub remote credentials are stripped before URLs or CLI arguments are displayed
  or used, while remotes containing userinfo remain discoverable without exposing it.
- Syntax highlighting and tokenization are bounded so pathological files terminate
  without freezing or exhausting the process.
- The architecture diagrams no longer imply a direct Tokio dependency. Tokio reaches
  the build transitively through Tauri; `rayon` is the only direct concurrency
  dependency, and blocking work leaves the IPC thread via
  `tauri::async_runtime::spawn_blocking`.
- Windows clippy warnings from dead code and unused imports in the test harness.

### Removed

- The unused branch-folding engine and topology index, and the client-side
  copy of the filter language. The graph payload no longer carries `folds`
  (nothing ever read it); older payloads that still carry the field
  deserialize as before.

### Internal

- Dependabot groups merged: GitHub Actions (`actions/checkout` v4 → v7,
  `actions/setup-node` v4 → v7, `actions/upload-artifact` v4 → v7,
  `tauri-apps/tauri-action` v0 → v1) and Cargo (`rfd` 0.15 → 0.17, `base64`
  0.22 → 0.23). The npm group is held: it raises `typescript` to 7, which no
  released `svelte-check` accepts, and `tailwindcss` to 4, whose PostCSS
  plugin moved to a separate package and whose theme model the 65 components
  using `surface`/`textPrimary`/`accent` would have to migrate to.

- Integration coverage for the `terminal`, `github`, `updates`, and `desktop` modules,
  which had inline tests but nothing exercising them through their public surface
  against real repositories and real child processes.
- The Vite build-caching question (sharing `dist/` across CI legs) was investigated and
  declined; the measurements and the reasoning are recorded above the build step in
  `ci.yml`.
- Cross-language contract coverage now includes command registration and arguments,
  serde variants, events, GitHub CLI fields, and repository-surface payloads, with
  integration and stress suites exercising real repositories and child processes.

## [0.0.2] - 2026-08-26

Initial tagged release: the Rust/Tauri 2 backend, the Svelte 5 frontend, the commit
graph renderer, and the cross-language contract checks that guard the IPC boundary.

[Unreleased]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.4...HEAD
[0.0.4]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/bharathvbcr/GitPulse/releases/tag/v0.0.2
