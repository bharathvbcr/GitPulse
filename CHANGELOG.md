# Changelog

All notable changes to GitPulse are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building, so every
released version needs a heading of the form `## [x.y.z] - YYYY-MM-DD` here
before that tag is pushed.

## [Unreleased]

### Added

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

### Changed

- Native Git mutations (`stage`, `unstage`, `fetch`, `stash save`, `stash pop`) route
  through the harness write gate and return the policy verdict alongside their output.
- The release workflow verifies draft assets against an exact per-platform manifest
  instead of matching filename patterns, and preflight runs every contract gate.

### Fixed

- Accessibility: the commit context menu is reachable by keyboard (ContextMenu key
  and Shift+F10), and seven modal dialogs no longer suppress a11y rules to keep an
  event-plumbing click handler. Remaining suppressions state why they are correct.
- `SettingsModal` and `DiagnosticsModal` called an optional `onClose` without a guard.
- macOS: the application exits when the main window is closed.
- Terminal and diagnostics failures preserve their full context instead of truncating
  it across builds.
- Dependency coverage is reported accurately in the health panel.

## [0.0.3] - 2026-08-28

### Added

- IDE-style file viewer, promoted to the primary work tab.

### Fixed

- Windows clippy warnings from dead code and unused imports in the test harness.

## [0.0.2] - 2026-08-26

Initial tagged release: the Rust/Tauri 2 backend, the Svelte 5 frontend, the commit
graph renderer, and the cross-language contract checks that guard the IPC boundary.

[Unreleased]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.3...HEAD
[0.0.3]: https://github.com/bharathvbcr/GitPulse/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/bharathvbcr/GitPulse/releases/tag/v0.0.2
