# Changelog

All notable changes to GitPulse are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building, so every
released version needs a heading of the form `## [x.y.z] - YYYY-MM-DD` here
before that tag is pushed.

## [Unreleased]

## [0.0.3] - 2026-09-02

### Added

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
