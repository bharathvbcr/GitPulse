# GitPulse

[![CI](https://github.com/bharathvbcr/GitPulse/actions/workflows/ci.yml/badge.svg)](https://github.com/bharathvbcr/GitPulse/actions/workflows/ci.yml)
[![Coverage](https://github.com/bharathvbcr/GitPulse/actions/workflows/coverage.yml/badge.svg)](https://github.com/bharathvbcr/GitPulse/actions/workflows/coverage.yml)
[![Release](https://img.shields.io/github/v/release/bharathvbcr/GitPulse?include_prereleases&sort=semver)](https://github.com/bharathvbcr/GitPulse/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

High-performance, native Git desktop client built with Rust & Svelte.

GitPulse is a Tauri 2 app: a Rust backend does the heavy lifting (graph lane solving, diffs,
coverage scanning, dependency analysis) while a Svelte 5 + TypeScript frontend renders it.
It runs entirely on your machine; GitHub features go through your locally installed `gh` CLI.

## Contents

- [Install](#install) · [Requirements](#requirements) · [Getting started](#getting-started)
- [Features](#features) · [Architecture](#architecture) · [Project layout](#project-layout)
- [Development](#development) · [Releasing](#releasing)
- [Contributing](#contributing) · [Security](#security) · [License](#license)

## Install

Download the installer for your platform from the
[latest release](https://github.com/bharathvbcr/GitPulse/releases/latest).

| Platform | Asset | Notes |
| --- | --- | --- |
| macOS (Apple Silicon & Intel) | `.dmg` | Universal binary. **Unsigned** — see [below](#macos-builds-are-unsigned). |
| Linux | `.AppImage`, `.deb` | Built on Ubuntu 22.04, so glibc 2.35+ |
| Windows | `.msi`, `.exe` | |

macOS quarantines unsigned downloads, so after dragging the app to `/Applications`:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

Prefer to build it yourself? See [Getting started](#getting-started).

## Features

- **Multi-language stack & LOC analysis** — detects 60+ programming, markup, and data languages
  (TypeScript, TSX, Rust, Go, C, C++, Java, Swift, Kotlin, JavaScript, Python, C#, F#, Scala, Ruby,
  PHP, Dart, Zig, Julia, Groovy, Shell, and more) using official GitHub Linguist colors, manifest
  recognition (`Cargo.toml`, `go.mod`, `package.json`, `pom.xml`, `Package.swift`, etc.), shebang
  sniffing, and language-aware comment parsing for accurate LOC breakdowns.
- **Commit history graph** — GPU-accelerated canvas-rendered graph with avatar rendering, lane
  smoothing, nogap lookback bounds, branch folding, and ref decorations solved natively in Rust.
- **Diff viewer** — file, commit, and range diffs with word-level intra-line highlighting
  and image diffs. Patches can be staged or unstaged selectively straight from the diff view
  (`cmd_stage_selective_patch`).
- **Staging & commits** — stage/unstage files, commit with amend, AI-assisted commit messages.
- **Conflict resolution** — parse and resolve merge conflicts in a dedicated editor.
- **Blame** — per-line authorship viewer.
- **Coverage & diagnostics** — discovers coverage artifacts across ecosystems (LCOV, Cobertura,
  Go cover, Istanbul JSON, JaCoCo, Clover) and displays per-file line coverage. One-click copy
  for failed coverage diagnostics, script errors, and rescans. MANVI local AI provides prioritized
  remediation plans and executable coverage scripts via a purpose-limited allowlist runner.
- **Storage** — disk-usage audit of the whole repository: git internals (packfiles vs loose
  objects, reflogs, LFS, submodule stores), build-output and cache directories across
  ecosystems, hygiene gaps (artifact directories not covered by `.gitignore`, or ignored ones
  still holding committed files), oversized working-tree files, linked-worktree sizes, and
  merged-stale branch weight that links into MANVI's cleanup plan. Every completed scan records
  a per-repository snapshot locally, so growth is visible over time ("+180 MB this week") via a
  trend sparkline and deltas. Walks are budgeted and never follow symlinks; a hostile or huge
  repository degrades into an honest "partial scan" instead of a hang.
- **Dependency health** — multi-ecosystem vulnerability and staleness scanning: `npm audit`/`outdated`,
  `cargo-audit`, `pip-audit` (pinned requirements files), `govulncheck`, `composer audit`, and
  `bundler-audit`, each used when its CLI is present, plus open GitHub Dependabot alerts (via `gh`)
  unified in the Health view. Copy the whole report as text, or send it through the configured
  local model for a remediation plan. Nothing runs merely because the model suggested it: each
  visible step or the explicit run-all control is a user confirmation, and the backend enforces a
  health-only command allowlist before execution.
- **Stacked branches** — visualize and manage stacked branches (`stack` view).
- **Worktrees** — linked-worktree panel in the sidebar: list with HEAD, branch, dirty-file
  counts, and prunable state; add (with branch and start point), remove, and lock/unlock.
- **Reflog** — browse the reference log.
- **Policy-gated mutations & local AI** — an optional [MANVI](#the-manvi-harness) harness sidecar
  vets mutating git actions and answers AI prompts against a local model. Everything degrades
  gracefully when it is not installed.
- **MANVI view** — one surface for everything MANVI: guarded pull/push shortcuts, conservative
  merged-branch cleanup plans, outgoing commit-message review with explicit coverage counts,
  bounded GitHub issue & release monitoring and reporting, release-tag publication with clean/synchronized/
  default-branch preflights, plus the harness connection, local model servers, branch naming, and
  the agent activity journal (copyable as a log). The header badge is a status indicator that
  leads here. The OS View menu's numbered tab shortcuts stop at Reflog; reach MANVI through the
  header tab bar's More menu or the command palette ("Open MANVI View").
- **GitHub integration** — repo-wide open-PR list with one-click PR checkout, workflow run status,
  Dependabot alerts, issue creation, and live release monitoring, all through the locally installed
  `gh` CLI. Remote-URL detection recognizes github.com, `*.github.com`, and `*.ghe.com`;
  self-hosted GHES on arbitrary domains is intentionally not matched.
- **GitHub CI/CD actions** — the GitHub view lists the repository's Actions workflows
  (`gh workflow list`), dispatches any active one against a chosen branch or tag
  (`workflow_dispatch`), and re-runs or cancels recent runs in place — every action policy-gated
  like the rest of GitPulse's mutations. The repository's own release workflow is dispatchable:
  `.github/workflows/release.yml` accepts a manual `tag` input as well as tag pushes.
- **CI:local** — one button runs this repository's CI pipeline on the current machine before you
  push: the frontend checks and Rust checks of `.github/workflows/ci.yml`, planned from the
  manifests actually present (`package.json`, `Cargo.toml`), executed sequentially with hard
  per-step timeouts, capped output tails, stop-on-first-failure, and honest passed/failed/skipped
  accounting.

## Requirements

| Tool | Version | Notes |
| --- | --- | --- |
| Node.js | 22+ | Frontend build and test tooling |
| Rust | stable (edition 2021) | Backend build via Cargo |
| Git | any recent version | The engine shells out to the `git` CLI at runtime |
| Tauri system deps | — | Linux needs `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, etc. — see `.github/workflows/ci.yml` for the exact package list |

## Getting started

```sh
npm install          # frontend dependencies
npm run tauri dev    # builds the Rust backend and opens the app window
```

The dev scripts resolve a free Vite port automatically (preferred: 5173, falling back through
5174–5193). Set `GITPULSE_DEV_PORT=<port>` to pin it — Tauri's `devUrl` is rewritten to match.
Nuance: under `tauri dev`, a busy pinned port that can't be reclaimed autoports past the pin;
the bare Vite wrapper (`npm run dev`) fails loudly instead.

### Production build

```sh
npm run tauri build  # bundles installers for the current platform
```

## Development

| Command | What it does |
| --- | --- |
| `npm run dev` | Vite dev server only (no app shell) |
| `npm run tauri dev` | Full desktop app with hot reload |
| `npm run build` | Frontend production bundle (`vite build`) |
| `npm run check` | Type-check the frontend (`svelte-check`) and node-side config/scripts (`tsc`) |
| `npm run check:ipc` | Verify the Rust `cmd_*` registry and every frontend `invoke()` call site stay in lockstep |
| `npm run check:types` | Verify coverage serde structs match TypeScript interfaces field-for-field |
| `npm run check:release` | Verify the five version manifests agree; add `-- --tag vX.Y.Z` to also check a release tag |
| `npm run ci:local` | Run full local CI pipeline (type-check, tests, build, clippy, cargo test) |
| `npm test` | Frontend unit tests (Vitest) |
| `npm run coverage` | Vitest with v8 coverage |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | Rust format check |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | Rust lint |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Rust unit & integration tests |

CI (`.github/workflows/ci.yml`) runs the frontend checks and Rust checks above on Linux, macOS,
and Windows. Clippy treats warnings as errors; `svelte-check` reports warnings without failing.

GitPulse also runs its own CI on your machine: **ci:local** (`cmd_ci_local`, wired to the GitHub
panel) plans the same steps from the manifests it finds and runs them sequentially, stopping at
the first failure and reporting everything after it as *skipped* rather than as a pass.

## Releasing

Releases are cut by pushing a tag; `.github/workflows/release.yml` does the rest.

```sh
npm run check:release -- --tag v0.1.2   # must pass before you tag
git tag v0.1.2 && git push origin v0.1.2
```

The pipeline is deliberately hard to misuse:

- **Every job checks out the tag**, not the branch, and asserts `HEAD` is the tagged commit — a
  `workflow_dispatch` for a tag that does not exist fails at checkout instead of building the
  branch head and publishing it under that tag's name.
- **The version gate runs first.** The Git tag, `src-tauri/tauri.conf.json`, `Cargo.toml`,
  `Cargo.lock`, `package.json` and `package-lock.json` must all name one version. `tauri.conf.json`
  supplies both `__VERSION__` in the release name and the bundle version, so a stale manifest would
  otherwise publish a release tagged `vX` whose installers are all `vY`.
- **Pre-flight mirrors CI step-for-step**, so a tag whose commit CI never covered cannot ship.
- **A verify job gates completeness.** The build matrix is `fail-fast: false`, so a `verify` job
  fails the run when any platform did not succeed *and* independently inventories the draft
  release's assets for a per-platform installer. A green matrix that uploaded nothing is still a
  failed release.
- Concurrency is keyed on the tag with cancellation **off**: a cancel between two asset uploads
  would leave a draft holding a partial, plausible-looking asset set.

Releases are created as **drafts** and are published by hand after the assets are checked.

### macOS builds are unsigned

There is no Apple signing identity in CI, so the `.dmg` is unsigned and un-notarized. macOS
quarantines it on download and Gatekeeper refuses to open it. To run a release build locally:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

Configuring `APPLE_SIGNING_IDENTITY` / `APPLE_CERTIFICATE` / `APPLE_ID` as repository secrets is
what removes this step for everyone else.

## Architecture

Two codebases meet at a single IPC seam — every `cmd_*` handler in the Rust registry
(`src-tauri/src/lib.rs:34`) is checked against every frontend `invoke()` call site by
`npm run check:ipc`, which fails on either direction of drift: a UI command the backend never
registered (guaranteed runtime crash) or a registered handler no view ever calls. Handlers that
are intentionally Rust-only carry a justification in the checker's allowlist:

```
Svelte 5 + TS frontend                 Rust backend
─────────────────────                  ─────────────────────────────────────
lib/components/  one view per screen   engine/     git CLI + write sandbox
lib/stores/      workspace state       graph/      lane solving, folding
lib/repos/       tab model, persist    diff/       diffs + conflicts
lib/desktop/     menus, drag-drop      analyzer/   language, LOC, coverage,
                                                 deps health
         ▲ │                           stack/      stacked branches
         └─┴── invoke() ──► cmd_* ──►  storage/    disk usage + history
                                       ops.rs      MANVI cleanup/review planning
                                       github/ watcher/ harness/ ai/ desktop/
```

- **No router.** Screens are members of the `ViewTab` union (`src/lib/repos/persist.ts`);
  `src/App.svelte` maps each to one component. Adding a screen touches three places: the union +
  `VIEW_TABS` in `persist.ts`, one entry in the view registry (`src/lib/views/viewRegistry.ts`)
  — from which the header tabs, native menu, and command palette all derive — and the render
  branch in `App.svelte`. TypeScript's `Record<ViewTab, …>` on the registry rejects anything less.
- **State** lives in classic Svelte stores built by factory functions with injectable
  dependencies (`createRepoStore(deps)`, …), which keeps them unit-testable without Tauri.
  Components use Svelte 5 runes (`$state`, `$derived`, `$effect`) for local UI state.
- **IPC boundary** is snake_case over the wire (`repo_path`) and camelCase inside TypeScript
  (`currentPath`). All disk access goes through custom `cmd_*` Rust commands — no Tauri fs plugin;
  writes are policy-checked and confined to the open repository.
- **Async hygiene**: views guard in-flight `invoke()` calls with `createAsyncGuard()`
  (`src/lib/async/guard.ts`) so responses that arrive after a repo switch are discarded.
- **Styling** is Tailwind utilities over CSS design tokens (`src/app.css`, wired into Tailwind by
  `tailwind.config.js`); dark/light themes swap variables on `html.dark` / `html.light`.

### The MANVI harness

`src-tauri/src/harness/` embeds the MANVI coding-agent harness as a `manvi serve` sidecar
speaking NDJSON over stdio. The live protocol exposes the policy and local-model planes, not
MANVI's native agent-tool catalogue or a PTY. It provides two things:

1. **Policy verdicts** — mutating git commands pass through a command gate (low-risk index,
   stash, and clone operations excepted). Verdicts land on a five-step ladder — allowed, demoted,
   warned, blocked, unchecked; unknown actions fail closed to blocked. They are recorded centrally
   by `runMutating()` in the repo store and surfaced in the header badge.
2. **Local AI** — commit messages, commit explanations, branch-name suggestions, dependency-health
   remediation plans, and coverage-report analyses answered by a locally configured model, with
   token budgets planned by the harness.

Degradation is asymmetric by design. With no `manvi` binary installed, mutating commands proceed
but their verdicts are recorded as `unchecked` — explicitly distinct from an allow. With the
harness installed but wedged or unreachable, mutations are refused rather than proceeding
unchecked. Local AI does not require the harness: it answers against whatever local model server
is configured; the harness only plans token budgets when available. Set `GITPULSE_MANVI_BIN` to
point at a specific binary.

The interactive Terminal shell is user-owned and intentionally outside the harness: MANVI never
receives its PTY handle or keystrokes. Model-authored Health and Coverage commands use a separate
`cmd_manvi_run_action` IPC seam instead. That seam accepts only direct argv (never a shell), rejects
arbitrary executables, URLs, outside-repository paths and symlink escapes, applies a purpose-specific
health/coverage allowlist, sends every accepted command through the MANVI command gate, and bounds
argv size, timeout and captured output. This is scoped, user-confirmed command execution—not an
autonomous terminal or general app-control API.

The **MANVI view** groups the highest-frequency repository operations without bypassing their
canonical owners, and hosts the harness and local-AI controls in a second pane. Branch cleanup is
review-first and only offers local branches Git reports merged
into the default branch; current/default/worktree/unmerged branches stay protected. Commit review
reports both reviewed and total counts. Issue monitoring reports failed or capped checks instead of
showing them as an empty, complete result. Publishing a release creates or resumes an annotated
SemVer tag and pushes the fully qualified tag ref, which triggers the repository's existing release
workflow.

## Project layout

```
src/
  App.svelte              Root shell: header tabs, welcome screen, view switching
  app.css                 Design tokens, scrollbars, animation helpers
  lib/
    components/           One component per view plus chrome (Sidebar, CommandPalette, …)
    stores/               repoStore (workspace + mutations), graph/filter/theme/density/
                          harness/interface/modal stores
    views/                Single view catalog (VIEW_REGISTRY): nav, menus, palette derive from it
    repos/                Tab model, persistence schema, path identity
    canvas/  motion/      Commit-graph rendering and paint scheduling
    diff/                 Word-level diffing, patch building, conflict save
    filter/               Commit query language (parse + memoized filtering)
    branches/ rebase/     Branch grouping/flattening; interactive-rebase planning
    ops/  agents/         Types for the Rust ops planner; agent activity journal model
    coverage/ health/     Formatting and types for the analyzers' output
    storage/ github/      Storage-scan formatting/history; shared GitHub types
    async/                Cancellation guards and debouncing for stale IPC responses
    desktop/              Native menu/event wiring, clipboard, window title sync
    ui/                   Focus trap, z-index layers, error formatting, webview shortcuts
    keyboard/ dom/        IME-safe shortcuts; portals, tooltips, virtual windows
    language/             Language-bar statistics
scripts/                  Dev-port wrappers around vite/tauri CLIs; IPC contract checker
src-tauri/
  src/lib.rs              Command registry, watcher, native menu
  src/commands/mod.rs     Definitions of the cmd_* IPC handlers
  src/engine/…            Subsystems (see Architecture)
  src/ops.rs              Read-only MANVI ops planning: cleanup plans, commit review, releases
  tests/                  Rust integration suites
.github/workflows/        CI, coverage, release
```

## Contributing

Issues and pull requests are welcome.

Before opening a PR, run the same checks CI runs — GitPulse can do this for you via the
**CI:local** button in the GitHub view, or from a shell:

```sh
npm ci
npm run check && npm test && npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

Two contract checks run inside `npm test` and will fail the build on drift, so it is worth
knowing what they mean:

- `check:ipc` — a `cmd_*` handler and its `invoke()` call sites disagree.
- `check:types` — a Rust serde payload struct and its TypeScript twin disagree field-for-field.
- `check:release` — the version manifests disagree with each other or with a release tag.

Clippy runs with `-D warnings`; `svelte-check` warnings do not fail the build.

## Security

GitPulse runs entirely on your machine. It does not phone home, and it has no server component.
GitHub features shell out to your own authenticated [`gh`](https://cli.github.com) CLI, so
GitPulse never sees or stores a GitHub token. Mutating git actions can additionally be gated
behind the optional [MANVI harness](#the-manvi-harness).

The webview runs under a restrictive CSP (`src-tauri/tauri.conf.json`): `default-src 'self'`,
no remote scripts, and `connect-src` limited to the Tauri IPC channel.

Found a vulnerability? Please open a
[security advisory](https://github.com/bharathvbcr/GitPulse/security/advisories/new) rather than
a public issue.

## License

[MIT](LICENSE) © 2026 Bharath Chandra Vaddaram
