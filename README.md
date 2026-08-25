# GitPulse

High-performance, native Git desktop client built with Rust & Svelte.

GitPulse is a Tauri 2 app: a Rust backend does the heavy lifting (graph lane solving, diffs,
coverage scanning, dependency analysis) while a Svelte 5 + TypeScript frontend renders it.
It runs entirely on your machine; GitHub features go through your locally installed `gh` CLI.

## Features

- **Multi-repo workspace** — tabbed repositories with pinning, recents, drag-and-drop opening,
  and native OS menu integration. Workspace state persists across launches.
- **Commit history graph** — canvas-rendered graph; lanes, branch folding, and ref decoration
  are solved on the Rust side.
- **Diff viewer** — file, commit, and range diffs with word-level intra-line highlighting
  and image diffs. Selective patch staging is backend-complete (`cmd_stage_selective_patch`);
  UI wiring is pending.
- **Staging & commits** — stage/unstage files, commit with amend, AI-assisted commit messages.
- **Conflict resolution** — parse and resolve merge conflicts in a dedicated editor.
- **Blame** — per-line authorship viewer.
- **Coverage** — discovers coverage artifacts in the repository and shows per-file line coverage.
- **Dependency health** — npm manifest analysis with audit/vulnerability and outdated-package reports,
  plus open GitHub Dependabot alerts (via `gh`) unified in the Health view. Copy the whole report
  as text, or send it through the MANVI harness's local model for a remediation plan (advisory
  only — nothing is applied automatically).
- **Stacked branches** — visualize and manage stacked branches (`stack` view).
- **Reflog** — browse the reference log.
- **Policy-gated mutations & local AI** — an optional [MANVI](#the-manvi-harness) harness sidecar
  vets mutating git actions and answers AI prompts against a local model. Everything degrades
  gracefully when it is not installed.
- **MANVI view** — one surface for everything MANVI: guarded pull/push shortcuts, conservative
  merged-branch cleanup plans, outgoing commit-message review with explicit coverage counts,
  bounded GitHub issue monitoring and reporting, release-tag publication with clean/synchronized/
  default-branch preflights, plus the harness connection, local model servers, branch naming, and
  the agent activity journal (copyable as a log). The header badge is a status indicator that
  leads here. The OS View menu's numbered tab shortcuts stop at Reflog; reach MANVI through the
  header tab bar's More menu or the command palette ("Open MANVI View").
- **GitHub integration** — PR context for the current branch and one-click PR checkout via the
  `gh` CLI; GitHub Enterprise hosts are supported via remote-URL detection.

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
nearby ports). Set `GITPULSE_DEV_PORT=<port>` to pin it — Tauri's `devUrl` is rewritten to match.

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
| `npm test` | Frontend unit tests (Vitest) |
| `npm run coverage` | Vitest with v8 coverage |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | Rust format check |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | Rust lint |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Rust unit & integration tests |

CI (`.github/workflows/ci.yml`) runs the frontend checks and Rust checks above on Linux, macOS,
and Windows. Clippy treats warnings as errors; `svelte-check` reports warnings without failing.

## Architecture

Two codebases meet at a single IPC seam — every `cmd_*` handler in the Rust registry
(`src-tauri/src/lib.rs:33`) is checked against every frontend `invoke()` call site by
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
        ▲ │                            stack/      stacked branches
        └─┴── invoke() ──► cmd_* ──►   github/ watcher/ harness/ ai/ desktop/
```

- **No router.** Screens are members of the `ViewTab` union (`src/lib/repos/persist.ts`);
  `src/App.svelte` maps each to one component. Adding a screen touches three places: the union,
  the header button, and the `{#if}` branch.
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
speaking NDJSON over stdio. It provides two things:

1. **Policy verdicts** — every mutating git command passes through a command gate; verdicts
   (allow/deny/warn) are recorded centrally by `runMutating()` in the repo store and surfaced in
   the UI badge.
2. **Local AI** — commit messages, commit explanations, and branch-name suggestions answered by a
   locally configured model, with token budgets planned by the harness.

With no `manvi` binary installed, all features still work: mutating commands proceed, but their
verdicts are recorded as `unchecked` — explicitly distinct from an allow — and AI features report
themselves unavailable. Set `GITPULSE_MANVI_BIN` to point at a specific binary.

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
    stores/               repoStore (workspace + mutations), graph/filter/theme/density/harness
    repos/                Tab model, persistence schema, path identity
    canvas/  motion/      Commit-graph rendering and paint scheduling
    diff/ filter/         Word-level diffing; commit query language
    async/guard.ts        Cancellation guards for stale IPC responses
    desktop/              Native menu/event wiring, window title sync
    coverage/ health/     Formatting and types for the analyzers' output
scripts/                  Dev-port resolution wrappers around vite/tauri CLIs
src-tauri/
  src/lib.rs              Command registry, watcher, native menu
  src/commands/mod.rs     Definitions of the cmd_* IPC handlers
  src/engine/…            Subsystems (see Architecture)
  tests/                  Rust integration suites
.github/workflows/        CI, coverage, release
```
