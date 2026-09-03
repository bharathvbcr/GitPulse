# Changelog

All notable changes to GitPulse are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building, so every
released version needs a heading of the form `## [x.y.z] - YYYY-MM-DD` here
before that tag is pushed.

## [Unreleased]

### Changed

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

### Removed

- The unused branch-folding engine and topology index, and the client-side
  copy of the filter language. The graph payload no longer carries `folds`
  (nothing ever read it); older payloads that still carry the field
  deserialize as before.

## [0.0.3] - 2026-09-02

### Added

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

### Internal

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

[Unreleased]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.3...HEAD
[0.0.3]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/bharathvbcr/GitPulse/releases/tag/v0.0.2
