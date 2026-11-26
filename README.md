# Waylode

<p align="center">
  <a href="https://gitpulse.vbcr.dev/"><img src="https://img.shields.io/badge/website-gitpulse.vbcr.dev-F97316?style=flat&logo=safari&logoColor=white" alt="Website"></a>
  <a href="https://github.com/bharathvbcr/GitPulse/actions/workflows/ci.yml"><img src="https://github.com/bharathvbcr/GitPulse/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/bharathvbcr/GitPulse/actions/workflows/coverage.yml"><img src="https://github.com/bharathvbcr/GitPulse/actions/workflows/coverage.yml/badge.svg" alt="Coverage"></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/bharathvbcr/GitPulse"><img src="https://api.scorecard.dev/projects/github.com/bharathvbcr/GitPulse/badge" alt="OpenSSF Scorecard"></a>
  <a href="https://github.com/bharathvbcr/GitPulse/releases"><img src="https://img.shields.io/github/v/release/bharathvbcr/GitPulse?include_prereleases&sort=semver" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
  <img src="https://img.shields.io/badge/tested%20on-macOS-lightgrey" alt="Tested on macOS; Windows and Linux builds are not yet tested">
  <img src="https://img.shields.io/badge/built%20with-Tauri%202%20%7C%20Rust%20%7C%20Svelte%205-orange" alt="Tech Stack">
</p>

<p align="center"><sub>Tested on macOS; Windows and Linux builds are available but not yet tested.</sub></p>

**A workspace, not a chat window. Waylode is the local-first command centre for agentic coding.**

Waylode is built for the agentic era. It gives the same information and the same tools to the
people and to the agents doing the work. It runs Claude Code, Codex and other agents side by side
across all your repositories. Each agent gets its own worktree, a policy guard and a code map, and
a load scheduler keeps the machine responsive. A full Git client sits in the same window. It is a
native Rust and Tauri 2 app with a Svelte 5 frontend, free and dual-licensed under `MIT OR Apache-2.0`.

[Download the 0.2.0 beta](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1) ·
[Getting started](docs/GETTING_STARTED.md) ·
[Why Waylode](docs/WHY_WAYLODE.md) ·
[Agents guide](docs/AGENTS_GUIDE.md) ·
[Documentation](docs/README.md) ·
[Website](https://gitpulse.vbcr.dev/) ·
[Release notes](CHANGELOG.md) ·
[0.2.0 notes](docs/RELEASE_NOTES_v0.2.0.md)

![Waylode on macOS: a Claude Code agent fixing two wiki pages in its own worktree, with Follow agent showing each file it edits beside its terminal](docs/assets/hero-agents.webp)

## Why Waylode

Most agent apps are organised around a chat transcript: one agent, one conversation, and you scroll
to find out what happened. Waylode is organised around the work itself: repositories, worktrees,
tasks, sessions, code and machine load. It shows that state to you in the UI, and to your agents
through the same facts over MCP (66 tools).

When you run several agents across several repositories, these are the problems Waylode is built for:

- **You lose the map.** Live session detection finds agent sessions across your repositories, including ones Waylode did not launch, and the agent tree nests them.
- **Agents collide.** The collision radar and file claims warn before two agents' edits meet, and agents can message each other through a peer inbox.
- **Agents do damage.** Manvi's command gate, per-repository trust, the secret gate on commits Waylode makes, and pre-operation undo snapshots limit what an agent can do.
- **Agents do not know the code.** DevMap gives you, and them, the blast radius and the affected tests for a change.
- **The laptop melts.** The load scheduler admits, queues or sheds heavy work by host pressure.
- **You still need Git.** The commit graph, branch stack tree, diffs, conflicts, reflog, pull requests and Actions are in the same window.

The task board is part of this. An agent that finds out-of-scope work files it with `gitpulse_add_task`,
and it lands on your board instead of scrolling away in a transcript. You can launch an agent from any
card.

Waylode does not replace Claude Code, Codex or your editor. It launches, watches, guards and lands their
work. [Why Waylode](docs/WHY_WAYLODE.md) has the dated comparison with other tools, including where
they are stronger. The [agents guide](docs/AGENTS_GUIDE.md) covers running agents.

**Platforms.** Waylode is tested on macOS. Windows and Linux builds are produced by release CI but have
not been tested.

## Waylode 0.2.0 (beta)

Waylode 0.2.0 is out in beta as [`v0.2.0-beta.1`](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1),
a GitHub pre-release for macOS; the stable 0.2.0 release follows. Versions restart at 0.x because
only macOS is tested so far, and 1.0.0 is kept for the first release tested on all three systems.
Releases come on two channels: betas are tagged `vX.Y.Z-beta.N`, stable releases `vX.Y.Z`. See the
[0.2.0 release notes](docs/RELEASE_NOTES_v0.2.0.md) and [versioning](docs/INSTALLATION.md#versions-and-channels).
The [feature reference](docs/FEATURES.md) lists every area.

- **Agents:** six launchers (Shell, Claude, Codex, Manvi, Grok, Antigravity) plus a Browser tab (macOS), live session detection, an agent tree, a worktree per agent with copy-on-write dependency caches, a peer inbox for agent-to-agent messages, collision radar, file claims, and turn checkpoints. The `gitpulse-mcp` server exposes 66 tools (55 `gitpulse_*`, 11 `devmap_*`) and 4 prompts.
- **A task board agents write to:** five columns (Backlog, Ready, In progress, Review, Done), cross-repository and saved-workspace boards, up to 2,000 cards shared across a board's columns per fill, launch an agent from a card, and `gitpulse_add_task` with duplicate gating. `gitpulse_complete_task` marks a task done only after a passed verification; otherwise the card moves to Review.
- **Guards:** Manvi's command gate classifies each decision into one of eight statuses (see [Security](docs/SECURITY.md)), repository trust is per repository and only the desktop can grant it, and allowed agent Git mutations take pre-operation snapshots (`refs/gitpulse/ops/`) with a checked undo.
- **Code intelligence:** DevMap map, blast radius, affected tests, regression suspects, dead symbols, clones and symbol search through the palette.
- **Load scheduler:** a memory-pressure governor for agent launches, build leases with a queue for heavy builds, parking of idle agent sessions, and a host-load strip with CPU, GPU, memory, thermal and storage indicators.
- **Terminal:** nested splits (up to 8 panes), split groups, command blocks, a Browser pane for local dev servers, and sessions that survive quitting the app on Unix (`gitpulse-ptyd`).
- **Four views, 11 sections:** Work (`Overview · Remote · Tasks`), Code (`Explorer · Map`), History (`Graph · Diff`) and Insights (`Pulse · Coverage · Health · Storage`), plus Fleet and an Agents plane.
- **Git client:** lane-solved commit graph with avatars, branch stack tree with restack, hunk and line staging, a 3-way conflict editor, HEAD moves (reflog) with undo, and GitHub pull requests, issues, Actions and releases.
- **Customization:** 17 theme presets, a My themes shelf (24), 11 macOS wallpapers plus your own image, a command palette with 8 modes, and repository logos.

## Get started

1. [Install Waylode](docs/INSTALLATION.md) and Git for your platform.
2. Open a repository and review **Trust and Open**. Trust allows its hooks,
   helpers, and project tools to run with your account permissions.
3. Use **Work** for changes in flight, **History** to review a commit, and
   **Code** to explore files. Open the command palette with `⌘K` / `Ctrl+K`.
4. Replay the guided tour any time from **Settings → Appearance → Guided walkthrough**.

GitHub CLI, DevMap, Manvi, and local model servers are optional; install the
ones needed for the features you use. The [first repository guide](docs/GETTING_STARTED.md)
walks through a review, task creation, and optional setup.

## Quick install & agent setup

If you installed the packaged desktop app, **Settings → Agents → Connect** (or its **Copy setup prompt**) symlinks the bundled `gitpulse-mcp` and `gitpulse-hook` into `~/.local/bin` without needing a source checkout or Rust toolchain, and **Help → Set Up Optional Tools** installs DevMap, the analysis suite, or Manvi interactively.

To build and wire up **Waylode**, **DevCouncil** (`devmap`, `dcstore`, `dcverify`, `dcgrep`, `devcouncil`), **Manvi** (`manvi`), and MCP integrations from source (requires **Git**, **Bun 1.4.2**, **Rust 1.100+**, and **Go 1.27.4+**), paste this prompt into Claude Code, Codex, Antigravity, or Cursor (or run the fenced commands directly):

````text
Install and wire up the complete GitPulse, DevCouncil, Manvi, and MCP toolchain on this machine, then verify every binary, schema, and host integration without skipping failing checks:

1. Preflight tools and checkouts:
   - Confirm `git`, `bun`, `rustc`/`cargo`, and `go` are on `PATH` (and `~/.local/bin` / Cargo / Go bin dirs are reachable).
   - Check whether sibling `GitPulse`, `DevCouncil`, and `Manvi` checkouts already exist; otherwise ask where to clone `https://github.com/bharathvbcr/GitPulse.git` (and optionally `DevCouncil` / `Manvi`), and do not install system packages outside Cargo/Go/Bun user bins without asking.

2. Install Waylode MCP, hooks, and worktree/cache CLIs (`waylode-mcp`, `waylode-hook`, `waylode-worktree`, `waylode-rustc`):
   - From a `GitPulse` checkout (or after `git clone https://github.com/bharathvbcr/GitPulse.git && cd GitPulse`):
     ```sh
     bun install --frozen-lockfile
     bun run mcp:install
     cargo install --path crates/waylode-worktree --locked --force
     ```
   - Skip rebuilding `gitpulse-mcp` / `gitpulse-hook` from source if I already linked them from the packaged app via Settings -> Agents -> Connect in `~/.local/bin`.

3. Install all DevCouncil components (`devmap`, `dcstore`, `dcverify`, `dcgrep`, `devcouncil`):
   - If a local `DevCouncil` checkout is present, run `bash scripts/install.sh`; otherwise install from GitHub:
     ```sh
     cargo install --git https://github.com/bharathvbcr/DevCouncil.git --locked --force devmap-cli
     cargo install --git https://github.com/bharathvbcr/DevCouncil.git --locked --force --bin dcstore dc-store
     cargo install --git https://github.com/bharathvbcr/DevCouncil.git --locked --force --bin dcverify dc-verify
     cargo install --git https://github.com/bharathvbcr/DevCouncil.git --locked --force --bin dcgrep dc-grep
     go install github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/cmd/devcouncil@latest
     ```

4. Install Manvi (`manvi` policy harness, workbench host, and managed agent runner):
   - From a local `Manvi` checkout run `go -C manvi install ./cmd/manvi`, or install remotely:
     ```sh
     go install github.com/bharathvbcr/Manvi/manvi/cmd/manvi@latest
     ```

5. Configure MCP servers and agent plugins cleanly (preview before writing; preserve existing user-owned configs):
   - Register **either** the GitPulse plugin (`plugins/gitpulse/` for Claude Code/Codex/Cursor or `plugins/gitpulse-agy/` for Antigravity) **or** a direct `gitpulse` MCP registration (`gitpulse-mcp`), **never both** — the plugin already registers the `gitpulse` server (repo insights, worktree coordination, collision radar, task board, and in-process `devmap_*`).
   - Run `devmap integrate <host> --dry-run` (and `devcouncil integrate <host> --dry-run` if using the orchestrator MCP) for my active agent client(s) (`claude`, `codex`, `cursor`, `antigravity`), show me the planned file changes (noting any `not_ours` protected files), and apply only with my approval (`--apply`) so `devmap mcp` (code graph) and `devcouncil mcp` (orchestrator/leases) sit cleanly alongside `gitpulse-mcp`.

6. Verify end-to-end and report exact tool outputs:
   ```sh
   bun run mcp:doctor
   bun run check:schema-parity
   devmap doctor && devcouncil doctor && manvi doctor
   dcstore --version && dcverify --version && dcgrep --help >/dev/null
   ```
   - Ensure `mcp:doctor` reports matching digests for `gitpulse-mcp` and `gitpulse-hook`, and `check:schema-parity` agrees across vendored crates, `devmap`, `dcstore`, `gitpulse-mcp`, and `manvi`.
   - In the target repository, run `devmap paths --json` and `devmap status --json` (and `devmap build --manifest` if missing or stale).

Report what succeeded, what could not run, exact paths of installed binaries, and any MCP/plugin config changes made or awaiting approval.
````

## Five views, one workspace

Waylode has 5 application views with 11 specialized sections. Each view keeps related sections together.

| View | What you do here | Sections |
| --- | --- | --- |
| **Work** | Track work in flight, resolve conflicts, organize tasks | Overview · Remote · Tasks |
| **Mavi** | Work with a coding agent as a conversation, here or on an SSH host: approve each tool call against the change it proposes, and review what every turn changed | One pane: threads · conversation · changes |
| **Code** | Browse, search and blame files, explore structure | Explorer (with Search and Blame toggles) · Map |
| **History** | Follow commits, review changes, find recovery points | Graph (commits, HEAD moves — the reflog — or regression suspects) · Diff |
| **Insights** | Inspect activity, coverage, dependencies, secrets, and disk usage | Pulse · Coverage · Health · Storage |

Graph's three lists and Diff share the selected commit; Explorer and Blame share the selected
file; Overview embeds branch stack hierarchy. **Fleet** compares repositories across the workspace (up to 1,000 open repository tabs, with at most 24 repositories watched at once and 24 recent repositories remembered),
**Tasks** opens global and saved-workspace boards with swimlanes and WIP limits, and the **Dock** stays available across views.

## From a change to the next step

- **Review and commit.** Canvas commit graph (the lane solver handles a synthetic 100,000-row history in about 41 ms, and assembling the window takes about 58 ms; see [performance notes](docs/PERFORMANCE.md)), unified and side-by-side diffs,
  word highlighting, symbol-grouped diff hunks, image comparisons, selective staging,
  stash previews, a three-way conflict resolver, and structured patch-brief commit
  phrasing with on-device Apple Intelligence fallback.
- **Manage worktrees and branches.** Zero-round-trip branch churn carry-forward on branch mutations,
  DevMap-guided `--cone` sparse worktrees, `<512 B` zero-copy DevMap base pointers with Blake3/CAS delta overlays,
  copy-on-write dependency cache sync (`node_modules`, `target`, `.venv`), an opt-in `waylode-rustc` (`RUSTC_WRAPPER`)
  shared crate cache in the separately installed `waylode-worktree` CLI (the app itself does not use it), parallel agent coordination bus and 3-layer CSR/ODB collision radar (`waylode-worktree` CLI & `waylode-mcp`),
  dead-branch cleanup with durable WAL tip backups and authorized delete actions, macOS pre-exec background QoS band isolation,
  named localhost routes via portless, and trust-gated lifecycle hooks.
- **Organize the work.** Quick add parses priority, labels, owner, repository,
  due date, and notes. Board views with swimlanes (grouped by owner or label),
  saved views, column WIP limits, column "Load all" paging until the board holds 2,000 cards, with
  persistent reload depth, named hidden repository notices, task checklists with done states, cross-task links
  (parent, blocks, related, duplicate), the Archive dock with Deleted recovery,
  review drafting suggestions, and agent terminal handoff with per-launch model selection.
- **Explore and search the code.** File navigation and blame are built in. High-speed
  repository-wide text search (`git grep` over working tree or revisions with honest
  cancellation and truncation reporting). Add DevMap for structural navigation,
  symbol search, symbol-level collision detection, dependencies, impact, and candidate tests.
- **Inspect the repository.** Universal coverage reports across 7 formats (LCOV, Cobertura,
  Go cover, Istanbul, JaCoCo, Clover, SimpleCov; a coverage.py `.coverage` database is
  detected and asks for an export), multi-ecosystem dependency audits (`cargo-audit`,
  `npm audit`, `pip-audit`, `govulncheck`) with `cargo deny` and `cargo crev` parsers,
  redacted secrets scanning (in-process by default, Kingfisher as the deep scan) that locates each finding against Git, storage
  cleanup previews, and Pulse activity summaries distinguish results from scans that did not run.
  Capped results identify their limits.
- **Work from the desktop.** Native menus, a command palette (`⌘K`), the Dock (shell, agents, files and console)
  with expanded view isolation, WebGL context caching, DEC 2026 frame synchronization, and configurable bell prompts that reports what each session is running, reflog recovery (`reset --keep`), GitHub pull
  request actions (review, comment, approve, merge pinned to viewed commit), colour-coded repository
  tabs and collapsible inactive groups that survive a restart, and an optional macOS menu-bar status popover keep
  frequent actions close. Background windows stop their timers instead of polling.

The [feature reference](docs/FEATURES.md) describes each surface and its limits.
The [changelog](CHANGELOG.md) records release changes; [open qualification](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/QUALIFICATION.md)
tracks remaining platform, provider, and performance checks.

![DevMap dependency map as a globe, with main.rs selected and its connections traced](docs/assets/devmap-globe.webp)

| Task board | Agents |
| --- | --- |
| ![Task board with backlog, ready, in-progress, review and done columns](docs/assets/tasks-board.webp) | ![Agents view listing every agent session and its checkout](docs/assets/agents-tree.webp) |
| Agent inbox | Secrets check on commit |
| ![Peer inbox with an overlap notice between two agent sessions](docs/assets/agent-inbox.webp) | ![A commit blocked by the secret scan before anything was committed](docs/assets/guard-secret-blocked.webp) |
| Blast radius | Local CI on affected tests |
| ![Diff view with the blast radius of a change](docs/assets/blast-radius.webp) | ![CI:local report running only the one affected test file](docs/assets/ci-local-affected.webp) |
| Conflict editor | Terminal splits |
| ![Three-way conflict editor resolving a README merge conflict](docs/assets/conflicts.webp) | ![Dock split into three terminals](docs/assets/terminal-splits.webp) |

Screenshots are captures from the macOS app; appearance varies by release and platform. Names of
private repositories are blurred.

## Local state and explicit permissions

Repository and task state are stored locally. The desktop app has no remote
telemetry. Git remotes, GitHub operations, Firebase App Hosting checks you ask for,
optional tool downloads, release checks, and configured agent providers can use the
network.

Built-in AI (commit messages, explanations, branch names, health and coverage plans)
runs on whatever Manvi itself is set to (`manvi use`), through Manvi's
`work.assist.complete`: a local model server in local mode, the Claude API in cloud
mode, and only in cloud mode does a diff leave the machine. Cloud mode has to be
chosen: with nothing saved, Manvi reports its built-in default (cloud) and Waylode
does not follow it. It uses its own loopback client instead, which asks before it
sends anything to a server it found, until a mode is chosen in the Manvi pane or with
`manvi use`. The loopback client is also the fallback when Manvi is not installed or
is too old. Task enhancement and agent
runs use their configured provider; supported Mac builds can also use Apple
Intelligence for task titles and descriptions. These are distinct execution paths.

Repository trust is an execution decision. A worktree is not an OS sandbox.
Policy verdicts preserve **unchecked** separately from **allowed**, and the
MCP server cannot grant repository trust. Read the
[security and trust model](docs/SECURITY.md) before configuring agents.

## Architecture

DevCouncil is **components and modules**. Manvi wraps them. Waylode uses Manvi
for policy, workbench, and agent hosting, and selected DevCouncil components
for code intelligence. Modules can be updated independently.

```mermaid
flowchart TB
    subgraph Frontend["Svelte 5 Frontend (Runes & TS)"]
        direction LR
        Views["5 Views · 11 Sections<br/>(Work, Mavi, Code, History, Insights)"]
        Canvas["GPU HTML5 Canvas<br/>Commit Graph"]
        TerminalDock["PTY Dock<br/>(portable-pty)"]
    end

    subgraph IPC["Tauri 2 IPC Seam"]
        Bridge["invoke('cmd_*') — 330+ Handlers<br/>Zero-Drift Pre-Commit & CI Contracts"]
    end

    subgraph Backend["Rust Backend (Tauri 2 / Rayon)"]
        direction TB
        GitEngine["Git Sandbox & CLI Reader"]
        LaneSolver["Topological Lane Solver"]
        Analyzers["Coverage, Health & LOC"]
        Ledger["Durable WAL Event Ledger"]
        Headless["Headless Binaries<br/>(gitpulse-mcp · gitpulsed · gitpulse-hook · gitpulse-ptyd)"]
    end

    subgraph Integrations["Modular Integrations & Local Tools"]
        direction TB
        subgraph ManviWrap["Manvi (Wraps DevCouncil Modules)"]
            ManviSidecar["manvi serve --posture host<br/>(policy gate: 8 decision statuses & workbench DB)"]
        end
        subgraph DevCouncilMods["DevCouncil & DevMap"]
            Vendored["Vendored Readers<br/>(devmap-store, devmap-query, dc-store)"]
            DevMapCLI["devmap CLI & daemon socket<br/>(build --manifest, search, impact)"]
        end
        subgraph LocalTools["Local Machine"]
            CLI["git & gh CLI"]
            LocalLLM["Loopback-Only LLMs<br/>(Ollama / LM Studio)"]
        end
    end

    Frontend --> Bridge
    Bridge --> Backend
    Backend --> ManviWrap
    Backend --> DevCouncilMods
    Backend --> LocalTools
```

See [architecture](docs/ARCHITECTURE.md) for ownership and contracts, and
[module integration](docs/MODULE_INTEGRATION.md) for the boundaries between projects.
Waylode succeeds the deprecated [LiquiTask](https://github.com/bharathvbcr/LiquiTask)
workbench; this does not imply complete feature parity.

## Build and contribute

Use Bun 1.4.2, Node.js 22.x (22.12 or newer), Rust 1.100+ (stable), Go 1.27.4 (for `gusset:check`), Git, and your platform's native
build tools. The [contributor guide](CONTRIBUTING.md) covers setup and platform prerequisites.

```sh
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse
bun install --frozen-lockfile
git config core.hooksPath .githooks
bun run tauri dev
```

| Command | Purpose |
| --- | --- |
| `bun run check` | Svelte and TypeScript checks |
| `bun run test` | Frontend and contract tests |
| `bun run test:tz` | Local-time frontend tests across `America/New_York`, `Pacific/Auckland`, and `Asia/Kolkata` |
| `bun run check:ipc` | Match frontend calls to the native command registry |
| `bun run check:types` | Compare Rust and TypeScript wire contracts |
| `bun run check:release` | Check version manifest consistency |
| `bun run check:workflows` | Lint the GitHub Actions workflow definitions |
| `bun run test:browser:all` | Browser regressions under headless Chrome |
| `bun run test:webkit:all` | The same regressions under WKWebView (macOS only) |
| `bun run build` | Build the frontend bundle |
| `bun run tauri build` | Build native bundles for the host |
| `bun run ci:local` | The gates in `scripts/ci-local.mjs`: contracts, both renderers, format, clippy, Rust and frontend tests, coverage floors and the gusset check. It deletes the ~50 GiB instrumented coverage build when it is done with it; set `CI_LOCAL_KEEP_COVERAGE_BUILD=1` to keep it. It does not run `bun audit`, `cargo deny` (`bun-audit.yml`), the Go `vet` and `-race` jobs, the scheduled deep and parallel-test workflows, or the Linux and Windows builds, which only CI runs |

The browser regressions run twice in CI — under Chrome on Linux and under
WKWebView on macOS — because the two engines disagree often enough to matter: a
fixture can pass under one and fail under the other. `ci:local` runs both for
that reason, so prefer it over the individual commands before opening a pull
request.

A frontend build alone does not qualify the native application. Follow the
[release procedure](CONTRIBUTING.md) before publishing a build.

## Documentation

The [documentation index](docs/README.md) organizes user guides, integration
references, contributor contracts, and historical evidence. Start with
[installation](docs/INSTALLATION.md), [your first repository](docs/GETTING_STARTED.md),
[feature reference](docs/FEATURES.md), [tasks and workspaces](docs/TASKS_AND_WORKSPACES.md),
or [zero-copy agentic worktrees](docs/ZERO_COPY_AGENTIC_WORKTREES.md).

Bug reports, documentation, design, tests, and code contributions are welcome.
See [CONTRIBUTING.md](CONTRIBUTING.md) and [good first issues](docs/GOOD_FIRST_ISSUES.md).

### Getting help

Questions and open-ended proposals go to
[Discussions](https://github.com/bharathvbcr/GitPulse/discussions) — ask in
**Q&A**, where an answer can be marked accepted, or start in **Ideas**. Bugs,
scoped feature requests, and adapter proposals go through
[issues](https://github.com/bharathvbcr/GitPulse/issues/new/choose), which has a
template for each. Suspected vulnerabilities are the exception: report those
through GitHub Security Advisories as described in the
[security and trust model](docs/SECURITY.md), not as a public issue.

The bug template asks for a diagnostics report: open the command palette and
run **Open Diagnostics**, then use its copy button, which collects the version,
platform, and recent log lines. Read it before pasting — it can contain
repository paths.

### Contributors

Every kind of contribution is recognised here, not only code — documentation,
design, bug reports, testing, reviews, and ideas all count under the
[all-contributors](https://github.com/all-contributors/all-contributors)
specification. The list below is generated from `.all-contributorsrc`.

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<!-- ALL-CONTRIBUTORS-LIST:END -->
<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

To add someone (including yourself), comment on any issue or pull request:

```
@all-contributors please add @username for code, doc
```

The bot opens a pull request updating this section and `.all-contributorsrc`.
See the [emoji key](https://allcontributors.org/docs/en/emoji-key) for contribution
types.

---

## Topics, Tags & Discoverability

| Category | Metadata / Values |
| --- | --- |
| **GitHub Topics** | `git`, `git-client`, `git-gui`, `tauri`, `tauri-v2`, `rust`, `svelte5`, `svelte`, `local-first`, `code-intelligence`, `developer-tools`, `devtools`, `desktop-app`, `mcp`, `commit-graph`, `merge-conflict-resolver`, `reflog`, `test-coverage`, `terminal`, `privacy-first`, `zero-telemetry` |
| **Tech Stack** | Rust (Tauri 2, Rayon), Svelte 5 (Runes), TypeScript, Tailwind CSS, portable-pty, DevMap, Manvi |
| **Target Platforms** | Tested on macOS (universal build). Release CI also builds Linux x64 and arm64 (`.AppImage`, `.deb`) and Windows x64 and arm64 (`.msi`, `.exe`); those builds are not yet tested. |
| **License & Privacy** | Dual-licensed (`MIT OR Apache-2.0`) · 100% on-device · Zero telemetry |

---

## License & Acknowledgements

Dual-licensed under either the [MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE) (`MIT OR Apache-2.0`), at your option — see [LICENSE](LICENSE). See [NOTICE](NOTICE) for third-party and vendored crate attributions. The Waylode name and logo are not covered by the MIT or Apache-2.0 licenses; see [TRADEMARKS.md](TRADEMARKS.md).

- **[The Tauri Programme (`tauri-apps`)](https://github.com/tauri-apps/tauri)** (`tauri`, `wry`, `tao`, `muda` — Apache-2.0 OR MIT) — Desktop shell runtime and patched crates under `src-tauri/framework/`.
- **[webkit2gtk-rs](https://github.com/tauri-apps/webkit2gtk-rs)** (`The Gtk-rs Project Developers` & `Antoni Boucher`, MIT), **[libappindicator-rs](https://github.com/qDot/libappindicator-rs)** (`Kyle Machulis / qDot`), and **[notify](https://github.com/notify-rs/notify)** (`Félix Saparelli & contributors`).
- **[MarkDev](https://github.com/bharathvbcr/MarkDev)** (`bharathvbcr/MarkDev`, MIT) — Embedded Rust Markdown/math/diagram rendering pipeline under `src-tauri/vendor/markdev/`.

© 2026 Bharath Chandra Vaddaram
