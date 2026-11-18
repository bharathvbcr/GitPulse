# GitPulse documentation

GitPulse brings Git, code exploration, tasks, and repository insights into one
local-first desktop workspace with 11 specialized sections across 4 views. This is the index for maintained documentation.

[Download the 0.2.0 beta](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1) ·
[Website](https://gitpulse.vbcr.dev/) · [Project overview](../README.md) ·
[Release notes](../CHANGELOG.md) · [0.2.0 notes](RELEASE_NOTES_v0.2.0.md)

## Start here

| I want to… | Read |
| --- | --- |
| Install the app or update it | [Installation](INSTALLATION.md) |
| Open a repository and review my first change | [Getting started](GETTING_STARTED.md) |
| Understand the walkthrough or OS permissions | [Onboarding and permissions](ONBOARDING.md) |
| See what GitPulse does, by area | [Features](FEATURES.md) |
| Look up a view or section in detail | [View reference](VIEW_REFERENCE.md) |
| Run agents on a task board, with guards and collision warnings | [Agents guide](AGENTS_GUIDE.md) |
| Compare GitPulse with other tools | [Why GitPulse](WHY_GITPULSE.md) |
| Find a command or keyboard shortcut | [Command palette](COMMAND_PALETTE.md) · [Keyboard shortcuts](../wiki/Keyboard%20Shortcuts.md) |
| Explore topics, tags and SEO | [Topics and SEO](../README.md#topics-tags--discoverability) |

## Everyday workflows

| Guide | Covers |
| --- | --- |
| [Tasks and workspaces](TASKS_AND_WORKSPACES.md) | Quick add, boards, saved workspaces, drafting, archive, agent handoff |
| [Zero-copy agentic worktrees](ZERO_COPY_AGENTIC_WORKTREES.md) | Sparse `--cone` checkouts, zero-copy DevMap overlays, `gitpulse-rustc` build cache, parallel agent radar |
| [Terminal](TERMINAL.md) · [Terminal host](TERMINAL_HOST.md) | Sessions, split panes, search, export, `gitpulse-ptyd` host & crash survival |
| [Coverage](COVERAGE.md) | Report formats, generation, missing toolchains, diagnostics |
| [Repository hygiene](REPOSITORY_HYGIENE.md) | Storage scopes, cleanup previews, scheduling |
| [Native menus and status icon](MACOS_MENUS.md) | Desktop menus, contextual actions, macOS status popover |
| [macOS appearance](MACOS_APPEARANCE.md) | Glass surfaces, theme presets, saved themes, window wallpapers, accessibility fallbacks |

## Integrations and trust

- [Security and repository trust](SECURITY.md): execution authority, local state,
  network boundaries, MCP access, and vulnerability reporting.
- [MCP and agents](../wiki/MCP%20and%20Agents.md): installation, compatible hosts,
  repository context, and the read-only plus trust-gated MCP surface.
- [Module integration](MODULE_INTEGRATION.md): GitPulse as host, Manvi as wrapper,
  and DevCouncil as independently updatable components.
- [Policy and AI](../wiki/Policy%20and%20AI.md): policy outcomes and the distinction
  between local assistance and explicitly launched agent sessions.

## Build and maintain

| Reference | Purpose |
| --- | --- |
| [Contributing](../CONTRIBUTING.md) · [Trademarks](../TRADEMARKS.md) | Setup, commands, test contracts, DCO sign-off, release procedure, trademark policy |
| [Architecture](ARCHITECTURE.md) | Source ownership, IPC, state, execution paths |
| [Good first issues](GOOD_FIRST_ISSUES.md) | Scoped contribution ideas |
| [Dependency health](DEPENDENCY_HEALTH.md) | Dependency maintenance and unresolved advisories |
| [Performance](PERFORMANCE.md) | Performance design and measurement boundaries |
| [Zero-copy agentic worktrees](ZERO_COPY_AGENTIC_WORKTREES.md) | Sparse cones, DevMap overlays, `gitpulse-rustc` build cache, parallel agent radar |
| [Overview hardening](OVERVIEW_HARDENING.md) | Worktree identity and incomplete-state contracts |
| [Native notifications](NATIVE_NOTIFICATIONS_ADAPTER.md) | Adapter contract and platform qualification |
| [Contracts](../contracts/README.md) | Cross-process lease and operation schemas |

## Implementation and evidence

Plans, design drafts, spikes, and qualification ledgers are not kept on `main`;
[open qualification](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/QUALIFICATION.md)
and the [agentic workspace plan](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/AGENTIC_WORKSPACES_PLAN.md)
(an implementation and acceptance ledger where a planned item is not a shipped
capability) are preserved at commit `90362c2e`. Dated audits,
benchmarks, and migration records are preserved at
[commit 16483b7d](https://github.com/bharathvbcr/GitPulse/blob/16483b7de2b5c74003c7e7af046f7142ece7699b/docs/archive/README.md).
[Archive separation](ARCHIVE_SEPARATION.md) explains that boundary.

Use user guides for current workflows and architecture for implementation
contracts. Read the date, platform, input, and limitations before using an
archived result as evidence. A local test, installed-app check, and live-provider
check establish different things.

## Keeping this set current

Keep installation instructions here rather than duplicating release numbers in
wiki summaries or marketing copy. Add maintained guides to this index, link from
the relevant feature section, and preserve existing anchors where practical.
Historical release sections and audit records retain their original context.

The public website is owned by the separate Portfolio repository, in
`src/pages/GitPulse.jsx` and `src/pages/GitPulse.css`; its catalog summary lives in
`src/data/projectData.js` and route metadata in `src/lib/routes.js`. Its screenshots
live under `public/assets/`. The page links back to these canonical guides. Updating
this repository does not publish the website or synchronize the GitHub wiki.
