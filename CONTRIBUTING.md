# Contributing to GitPulse

Thank you for contributing to GitPulse! We welcome pull requests, bug reports, and feature proposals.

---

## 1. Development Prerequisites

Ensure you have the following tools installed on your development machine:

| Tool | Version | Why this floor |
| --- | --- | --- |
| **Node.js** | `22.x` or later | The version CI runs (`.github/workflows/ci.yml`). Vite 6 and Vitest 3 require `>=20`; 22 is what release builds are verified against. |
| **Rust** | `stable`, edition 2021 | Needs the `clippy` and `rustfmt` components — CI fails on either. `rustup component add clippy rustfmt` |
| **cargo-llvm-cov** | latest | Generates the Rust LCOV report that `npm run ci:local` enforces coverage floors against. `rustup component add llvm-tools-preview` then `cargo install cargo-llvm-cov --locked` |
| **actionlint** | latest | Lints the GitHub Actions workflows in `npm run ci:local`. `release.yml` runs only on a `v*` tag, so this is the only gate that reads it before a release. `brew install actionlint` |
| **Git** | any maintained release | Not just for version control: GitPulse shells out to `git` for every repository operation, so the binary on your `PATH` is part of the runtime. |
| **GitHub CLI** (`gh`) | optional | Only the GitHub panel (PRs, issues, workflow runs, Dependabot alerts) uses it. Everything else works without it. |

**Platform toolchains** — Tauri builds a native binary, so each OS needs its own:

- **macOS**: Xcode Command Line Tools (`xcode-select --install`). Universal release builds additionally need the `aarch64-apple-darwin` and `x86_64-apple-darwin` targets.
- **Windows**: the MSVC build tools and the WebView2 runtime (preinstalled on Windows 11 and current Windows 10).
- **Linux (Ubuntu/Debian)**:
  ```sh
  sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev pkg-config
  ```

---

## 2. Getting Started

Clone the repository and launch the development environment:

```sh
# 1. Clone the repo
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse

# 2. Install frontend dependencies
npm install

# 3. Start Tauri in development mode (hot-reloads Rust & Svelte)
npm run tauri dev
```

> [!NOTE]
> The dev launcher automatically finds a free Vite port (5173, falling back through 5174–5193). Set `GITPULSE_DEV_PORT=<port>` to pin a custom port.

---

## 3. Running the Tests

**One command runs everything CI runs:**

```sh
npm run ci:local
```

That is the gate. If `npm run ci:local` is green, `.github/workflows/ci.yml` and
`.github/workflows/coverage.yml` will both be green on all three platforms. Run it
before opening a pull request.

It expands to the full suite — frontend type check, Vitest under V8 coverage, Vite
build, `cargo fmt`, `cargo clippy -D warnings`, the Rust suites under `cargo llvm-cov`,
and `npm run check:coverage` to enforce the floors against the two LCOV reports those
runs just produced. It regenerates both reports rather than trusting whatever is left
on disk, so a stale `lcov.info` can never be mistaken for a passing check. While
iterating you will usually want the narrower commands instead:

| Command | Scope | Typical runtime |
| --- | --- | --- |
| `npm test` | Vitest suite (2,000+ tests across `src/`) | seconds |
| `npx vitest run src/lib/graph` | One directory | sub-second |
| `npx vitest watch` | Re-runs on save | continuous |
| `npm run coverage` | Vitest with V8 coverage into `coverage/` | ~1 min |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Rust unit + integration suites (850+ tests) | ~1 min |
| `cargo test --manifest-path src-tauri/Cargo.toml updates::` | One Rust module | seconds |
| `cargo test --manifest-path src-tauri/Cargo.toml --test ipc_bridge_integration` | Commands driven through the real IPC bridge on Tauri's MockRuntime | seconds |
| `npm run check:coverage` | Validate both LCOV reports and enforce floors (needs a prior `npm run coverage` and `cargo llvm-cov` run) | seconds |

### Test conventions

- **Frontend tests sit next to the code** — `foo.ts` is tested by `foo.test.ts`, or by
  `__tests__/foo.test.ts` where a directory has many. Both layouts are in use; match
  the directory you are editing.
- **Suffixes carry meaning.** `.stress.test.ts` covers scale and pathological
  topologies, `.fuzz.test.ts` feeds randomized input, `.property.test.ts` asserts
  invariants rather than examples. A change to the graph renderer or lane solver is
  expected to keep the existing stress suites green, not to weaken them.
- **Rust tests are inline `#[cfg(test)] mod tests`** in the module they cover.
  Anything touching the filesystem uses `tempfile` and sets an explicit git identity —
  CI runners have no global `user.name`, and a test that assumes one fails only there.
- **Every bug fix ships with a test that fails against the unfixed code.** This is the
  one review comment you can count on receiving.

---

## 4. Contract Verification

GitPulse enforces strict contracts between the Rust backend and TypeScript frontend.
These are not style checks — each one catches a class of drift that types alone cannot:

```mermaid
flowchart TD
    subgraph PreCommit["Pre-Commit Verification Suite"]
        FrontendChecks["Frontend: <code>npm run check</code> & <code>npm test</code>"]
        RustChecks["Rust: <code>cargo fmt</code> & <code>cargo clippy</code> & <code>cargo test</code>"]
        IPCCheck["IPC Contract: <code>npm run check:ipc</code>"]
        TypeCheck["Type Contract: <code>npm run check:types</code>"]
        ReleaseCheck["Release Manifests: <code>npm run check:release</code>"]
        CoverageCheck["Coverage Floors: <code>npm run check:coverage</code>"]
        WorkflowCheck["Workflow Lint: <code>npm run check:workflows</code>"]
    end

    FrontendChecks --> AllPass{"All Checks Pass?"}
    RustChecks --> AllPass
    IPCCheck --> AllPass
    TypeCheck --> AllPass
    ReleaseCheck --> AllPass
    CoverageCheck --> AllPass
    WorkflowCheck --> AllPass

    AllPass -->|Yes| ReadyPR["Ready for Pull Request"]
    AllPass -->|No| FixCode["Fix Drift / Errors"]
```

| Command | Purpose |
| --- | --- |
| `npm run check` | Runs `svelte-check` and `tsc` type validation |
| `npm test` | Runs the Vitest frontend unit and integration test suite (2,000+ tests) |
| `npm run check:ipc` | Verifies the Rust `cmd_*` registry (132 handlers) and frontend `invoke()` calls match with zero untracked orphans, and that every `#[tauri::command]` in the crate is actually registered |
| `npm run vendor:check` | Verifies no vendored crate has been edited here, and compares each against its upstream when that repository is present — reporting *not compared* when it is not |
| `npm run check:types` | Verifies that Rust serde structs match their TypeScript interfaces field-for-field and wire-type-for-wire-type, across 46 contracts (705 fields) |
| `npm run check:release` | Asserts all version manifests (`package.json`, `package-lock.json`, `tauri.conf.json`, `Cargo.toml`, `Cargo.lock`) are in sync |
| `npm run check:coverage` | Validates both LCOV reports structurally and enforces the coverage floors (frontend 90% lines / 85% branches, Rust 80% lines); a report that cannot be parsed fails loudly rather than passing by default. `--json` emits the same verdict for a machine |
| `npm run check:workflows` | Lints every workflow with actionlint; a missing actionlint exits 2 (could not run) rather than 1 (workflows are faulty) |
| `npm run release:notes -- --tag vX.Y.Z` | Prints the changelog section the release workflow will use as the release body; exits 1 if that tag has no section |
| `npm run ci:local` | Executes the complete local CI suite (format, clippy, tests, builds, coverage floors) in one command |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | Rust linting (warnings treated as errors) |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Rust backend test suite |

### Contracts enforced by tests rather than scripts

The `check:*` commands above are the gates you run by name. A second set of
contracts is enforced by tests under `scripts/`, which run with `npm test`.
They exist because each guards a class of drift that no type check can see —
several were added after the drift had already happened.

| Test | What breaks without it |
| --- | --- |
| `invoke-args-contract` | A renamed command argument. `check:ipc` proves the command exists; nothing proved the call sites send what it declares, and a wrong name fails at runtime with a deserialization error in whichever path calls it. 94 call sites. |
| `view-menu-contract` | A registered view missing from the native menu, in either direction. This happened three times — `tab-manvi`, then Storage and Reflog — because view ids are derived from the registry in TypeScript and hand-written as constants in Rust. |
| `event-contract` | An event name that drifts. An emit nobody hears looks like a feature that never fires; a listener for an event nobody sends waits forever. Neither produces an error. |
| `policy-status-contract` | A gate verdict the frontend does not know, which renders as the fallback — a refusal shown as something milder. |
| `verdict-contract` | Two consumers of the shared policy contract disagreeing about what a decision means. The harness reports `action: "allow"` for five different things — a clean pass, a posture demotion, a grant, an executor-widened scope, and a decision reached with rungs that could not run — and only one of them is a clean pass. It also pins the vendored `contracts/` copy against its checksums, so a contract edited here instead of at its source fails rather than forking. |
| `command-policy-contract` | A native mutation that reaches Git without passing the write gate. |
| `vendor-contract` | A path dependency that leaves the repository, which makes a lone checkout stop building — and nothing else notices until a fresh clone or a CI runner without the sibling repos fails. Also pins the distinction the vendor check exists for: an upstream that is not checked out is reported *unavailable*, never *matches*. |
| `provenance-verdict-contract` | A verdict written into a git note that the freshness badge does not recognise. The badge fails closed — an unrecognised verdict is never rendered as a pass — which is the safe behaviour and also a silent one: a writer that started emitting a new word would put a permanent amber badge on every verified commit with nothing to say why. Writers are found by scanning for `VerificationNote` constructions, so a second one is covered without anyone remembering. |
| `bundle-binary-contract` | The Tauri bundler choosing the wrong binary. `src/bin/*.rs` is auto-discovered, so adding `gitpulsed` gave the package three binaries and `GitPulse.app` shipped the headless daemon as its executable — an installed app that opens nothing. Every other gate passed, because each binary was individually fine; only the choice of which to bundle was wrong, and no test runs the bundler. |
| `pr-timing-contract` | `gh` being asked for a field it does not know, which fails the whole PR listing; and "not reviewed yet" collapsing into "reviewed instantly". Field parity moved to `check:types` once the interface left the component. |
| `wire-type-locality-contract` | A serde payload shape being declared inside a component, or inlined as an `invoke<{...}>` / `listen<{...}>` type argument, where `check:types` cannot reach it and a second copy can drift silently. Every instance found so far had already gone stale — TerminalRunResult, GitHubContext, ConflictChunk, CommitDetailsPayload, FileBlobPayload. |
| `gh-json-fields-contract` | A `gh ... --json` list drifting from either gh's vocabulary or the struct that parses the reply. Asking for a field gh does not know fails the entire listing; asking for too few fails nothing at all, and the unrequested field deserializes to a default, so a title renders empty forever. |
| `enum-variant-contract` | A serde enum variant being renamed on one side only. `check:types` compares struct fields and skips enums, so this drift is silent: TypeScript still compiles, the union still lists a valid string, and the comparison just stops matching. It also rejects a data-carrying variant listed as a bare string, which would typecheck and then fail to deserialize. |
| `ipc-type-coverage-contract` | The unchecked half of the IPC surface going unnoticed. `check:types` reports OK for the payloads it lists; this pins the ones it does not, with a reason each, so a new command cannot join the unchecked set silently. |
| `a11y-suppression-contract` | A bare `svelte-ignore`. A suppressed rule and a rule that passed look identical in `npm run check` output. |
| `terminal-isolation-contract` | An import that would give the AI or MANVI sidecar a route to the terminal PTY, which SECURITY.md says they cannot reach. |
| `update-privacy-contract` | The release check gaining a repository path or a credential flag, which SECURITY.md says it never sends. |
| `diagnostics-contract` | The crash log going quiet. The panic hook captures its logger at install time, so installing it before `logging::init()` binds nothing and every later panic is recorded where no one can read it — a hook that looks installed and is inert. It also pins the reverse gap: a new `[[bin]]` inherits neither the logger nor the hook and is absent from `LOGGED_BINARIES`, so it writes no durable log, and nothing about a silent binary looks wrong. Both lists are derived from Cargo.toml rather than restated here. |
| `plugin-contract` | The Agent Plugins 1.0 package drifting from the published schemas. A extra top-level field, a mismatched `$schema` version between `plugin.json` and `mcp.json`, or a `command` that is a shell string rather than one token, and a conformant client rejects or skips the package while Settings still copies it. |
| `documented-counts-contract` | A count in the docs drifting from the code. The Rust test total was understated fourfold before this existed. |
| `architecture-docs-contract` | The architecture docs describing a dependency the manifest does not have. |
| `cli-help-contract`, `cli-json-contract` | A script entry point losing `--help` or `--json`, or their exit codes diverging. |
| `release-workflow` | Release preflight losing a gate, or the release body reverting to a literal block. |

Adding a contract test is preferred over adding a `check:*` script unless the
check is something you would want to run on its own.

---

## 5. Architecture Orientation

GitPulse is a **Tauri 2** desktop app: a Rust core that owns every privileged
operation, and a Svelte 5 frontend that owns rendering and interaction. They meet at
exactly one seam — `invoke("cmd_*")` — and that seam is machine-checked (§4).

```
GitPulse/
├── src/                  Svelte 5 + TypeScript frontend
│   ├── lib/stores/       Reactive state (repo, graph, filter, theme, toasts, modals)
│   ├── lib/components/   UI components; one .svelte + one .test.ts each
│   ├── lib/canvas/       GPU-accelerated commit-graph renderer
│   ├── lib/views/        View registry + navigation (routerless, 15 views)
│   └── lib/<domain>/     Pure logic: files, diff, filter, graph, coverage, health…
└── src-tauri/src/        Rust core
    ├── commands/         #[tauri::command] handlers — the ONLY IPC entry points (132 handlers)
    ├── engine/           git CLI wrapper: reader, writer, worktrees, sandboxing
    ├── graph/            Lane solver, mainline pinning, filter simplification, bezier geometry, ref decorations
    ├── analyzer/         Language detection, LOC, coverage, dependency health
    ├── harness/          MANVI policy gate, sidecar protocol
    ├── github/           gh CLI integration (PRs, issues, runs, Dependabot)
    ├── updates/          Opt-in release check
    ├── ledger/ tasks/ grants/ ingest/  Control plane: durable log, leases, overrides, attribution
    ├── bin/              Headless entry points (see below)
    ├── vendored/         Copies of the Manvi and DevCouncil crates (see below)
    └── terminal/ diff/ storage/ watcher/ stack/ ai/ desktop/
```

### Headless binaries

The desktop app is not the only way into the control plane. Two binaries build
from the same crate and share its modules, so neither can drift from what the
app enforces:

| Binary | Shape | What it is for |
| --- | --- | --- |
| `gitpulse-mcp` | JSON-RPC over stdio (MCP 2026-07-28) | Lets an agent *read* the control plane: insights snapshot, collisions, change context, ledger, task view, code graph, provenance. Dual-era: modern per-request `_meta` plus legacy `initialize`. Packaged as Agent Plugins 1.0 under `plugin/`. |
| `gitpulsed` | NDJSON on stdout, interval loop | *Writes* what nothing else was writing. Attribution catch-up — transcripts and reflog into the ledger — used to run only when the desktop app opened a repository, so hours of agent work with GitPulse closed left a hole in the record that nothing on screen reported. |

`gitpulsed` deliberately serves no requests (that is `gitpulse-mcp`'s job, and
a second surface answering the same questions from the same store would be a
second thing to keep in step) and never takes a lease, checks out a task, or
writes a file — those belong to DevCouncil and Manvi, and a background process
holding a writer lease would contend with the agent doing the work.

```bash
gitpulsed --interval 300 /path/to/repo
```

Interruption is safe by construction rather than by signal handling: each
append is one transaction against a WAL database, and catch-up is idempotent
against a watermark read back out of the ledger, so the next cycle re-does
whatever an interrupted one did not finish and writes it exactly once.

### Vendored crates

GitPulse links eight Rust crates it does not own — `dc-verify`, `dc-store` and
`dc-glob` from Manvi, and five `devmap-*` crates from DevCouncil. They used to
be reached by relative path (`../../../../../Manvi/crates/…`), which meant a
checkout of GitPulse alone did not build: it needed two unrelated repositories
present, at the right depth, on every machine and every CI runner.

They are copied into `src-tauri/vendored/` instead, with
`src-tauri/vendored/VENDOR.json` recording, per crate, where it came from, the
upstream commit, every manifest rewrite, and a hash of every file.

```bash
npm run vendor:check
```

**Do not edit these copies.** A fix belongs upstream, followed by:

```bash
npm run vendor
```

Three things the tooling is careful about, each of which was a way to get this
subtly wrong:

* **Inheritance is resolved, not carried.** Both upstreams use workspace
  inheritance, and they disagree — Manvi is edition 2024 / resolver 3,
  DevCouncil's rust-port is 2021 / resolver 2 — so a single workspace here
  could not serve both. Each vendored manifest gets the concrete values its own
  upstream would have supplied, every substitution is listed in `VENDOR.json`,
  and an inheritance form the script does not recognise is a hard failure
  rather than something passed through to fail later as a confusing cargo
  error.
* **`tests/` and `[dev-dependencies]` are not vendored.** GitPulse has never
  run these crates' tests — as path dependencies outside its workspace, cargo
  does not build them — and `devmap-store` dev-depends on `devmap-serve`, which
  is outside this closure. The omission is recorded per crate rather than left
  to be inferred from an absence.
* **An upstream that is not checked out is reported `unavailable`, never
  `matches`.** `vendor:check` verifies two different things: that no copy has
  been edited here, which always runs; and that each copy still matches
  upstream, which needs the sibling repository. When it is missing the run says
  so and states that it is not a clean bill of health.

### The layers, and what each one owns

**1. Rust core — the only thing that touches the system.**
No frontend code shells out, reads a file, or opens a socket. Every such operation is
a `cmd_*` handler. `engine/git_cli.rs` is the canonical owner of process execution: it
strips ambient `GIT_*` configuration, disables terminal prompts and interactive
credential helpers, and bounds both output size and wall-clock time. New code that
runs a process goes **through** it, not beside it.

**2. The IPC seam — narrow, typed, and verified.**
A command is added in four places, in this order: implement in
`src-tauri/src/commands/`, register in `src-tauri/src/lib.rs`, invoke from a store or
component, then run `npm run check:ipc`. A handler with no caller and a call with no
handler both fail the build.

**3. Svelte 5 stores — the state machines.**
State lives in stores, not components. `repoStore` owns the repository lifecycle
(open, tabs, status polling, mutations); `graphStore` owns history loading and lane
layout; `filterStore`, `themeStore`, `interfaceStore`, `modalStore`, and `toastStore`
own their slices. Components subscribe and render; they do not own durable state.

**4. Views — routerless.**
There is no router. `src/lib/views/viewRegistry.ts` is the single source of truth for
which views exist and how they are reached. Adding a view means adding a registry
entry, not a route.

**5. The canvas renderer — the hot path.**
The commit graph is drawn on a canvas, not in the DOM, because thousands of rows in
the DOM will not hold 60fps. Everything under `src/lib/canvas/` is performance-
sensitive and covered by stress and fuzz suites. Treat a regression there as a bug
even when the picture still looks right.

### Async hygiene

Repository switching is the source of most race conditions in this codebase: a slow
response for repository A must never paint into repository B. Any component-level
async work uses `createAsyncGuard` (`src/lib/async/guard.ts`), which discards results
whose request is no longer current. This is not optional — it is the reason tab
switching is safe.

For the full treatment — runes and dependency injection, subsystem responsibilities,
renderer internals — see **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**.

---

## 6. Coding Standards

1. **One canonical owner.** Do not stand up a second registry, utility, or command
   path beside an existing one. Extend the view registry, the command handlers, and
   `git_cli` rather than duplicating them.
2. **Strict IPC contracts.** See §4 and §5. `npm run check:ipc` must report zero drift.
3. **Async hygiene.** Use `createAsyncGuard` for component-level async work.
4. **No unchecked casts.** No `any`, no loose casts. Model every IPC payload with a
   strict type; the Rust struct and the TypeScript interface must agree field for
   field.
5. **A check that could not run must not report success.** When an operation cannot be
   performed — no `gh`, no network, no manifest — say so explicitly. "Could not check"
   and "checked, nothing found" are different answers, and the codebase keeps them
   distinct (see `warnings` / `error` fields on the GitHub and coverage payloads).
6. **Bound every external interaction.** Timeouts, output caps, and result limits are
   required, not defensive extras. Report truncation rather than silently returning a
   partial list as if it were complete.
7. **Atomic changes.** One coherent concern per pull request.

---

## 7. Submitting a Pull Request

1. Branch from `main`.
2. Make the change, with tests.
3. Run `npm run ci:local` until it is green.
4. Open the PR and fill in
   [the template](.github/PULL_REQUEST_TEMPLATE.md) — it is a short checklist, not
   paperwork.
5. Use [Conventional Commits](https://www.conventionalcommits.org/) for commit
   subjects (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, `perf:`). GitPulse parses
   these in its own commit filter, so the repository holds itself to them.

### Looking for something to work on?

- **[docs/GOOD_FIRST_ISSUES.md](docs/GOOD_FIRST_ISSUES.md)** — a curated backlog of
  scoped, self-contained tasks with the files to touch and how to verify each one.
- Issues labelled [`good first issue`](https://github.com/bharathvbcr/GitPulse/labels/good%20first%20issue)
  and [`help wanted`](https://github.com/bharathvbcr/GitPulse/labels/help%20wanted).

### Recognition

Every kind of contribution counts — code, documentation, design, bug reports, testing,
ideas. GitPulse uses the [All Contributors](https://allcontributors.org/)
specification; contributors are listed in the README. To add someone (including
yourself), comment on any issue or PR:

```
@all-contributors please add @username for code, doc
```

---

## 8. Reporting Security Issues

Do **not** open a public issue for a security vulnerability. Follow
**[docs/SECURITY.md](docs/SECURITY.md)**.
