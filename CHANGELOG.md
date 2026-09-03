# Changelog

All notable changes to GitPulse are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building, so every
released version needs a heading of the form `## [x.y.z] - YYYY-MM-DD` here
before that tag is pushed.

## [Unreleased]

## [0.0.4] - 2026-09-03

### Fixed

- **Storage report accuracy & hardening**:
  - **Premature truncation resolved**: Raised per-directory entry limits inside build artifact directories from 4,000 to 100,000, preventing Cargo dependency directories (`target/debug/deps`) with > 4,000 files from prematurely tripping scan truncation.
  - **Unix hard link deduplication**: Scoped `(st_dev, st_ino)` tracking on Unix for files with `nlink > 1`, ensuring Cargo hard links (`deps/libfoo-hash.a` to `libfoo.a`) are counted exactly once, eliminating gigabytes of phantom disk usage.
  - **Monolithic container roll-up**: Container build directories (`target`, `node_modules`, `.venv`) roll nested build outputs (`debug/build`, `.../out`) up into the parent scope rather than fragmenting into dozens of child rows.
  - **Source-tree false positive protection**: Paths inside `src/` no longer match generic build or cache directory names (e.g. `src/lib/coverage` remains recognized as source code rather than an unignored cache).
  - **Single-pass worktree traversal**: Merged large-file collection directly into the worktree walker using `WorktreeWalkContext`, eliminating the redundant second walk and halving disk I/O.
  - **Developer and agent caches classified**: Recognized `.devcouncil`, `.gitnexus`, `.claude`, `.cursor`, `.agents`, `.gemini`, and `.antigravity` under cache artifacts.

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
