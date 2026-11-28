# Why Waylode

[Documentation index](README.md) · [Features](FEATURES.md) · [Agents guide](AGENTS_GUIDE.md) · [Migrating from Conductor](launch/MIGRATING_FROM_CONDUCTOR.md)

**Comparison verified 2026-11-27** against `main` (`HEAD` `33c6e0862`, ahead of the [`v0.2.0-beta.1`](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1) pre-release). Every cell comes from each product's official documentation or public repository, checked directly on `2026-11-27`; claims about code-index tools, agent-coordination tools and merge queues were re-checked on `2026-11-28` (see the [Whale field guide](launch/WHALE_FIELD_GUIDE.md)). Cells marked **?** mean the capability was not found in the pages read; a **?** is not a "no".

## A workspace, not a chat window

Waylode — also called **"Whale"** out loud — is built for the agentic era. It gives the same information and the same tools to the people and to the agents doing the work.

Most agent apps are organised around a chat transcript: one agent, one conversation, and you scroll to find out what happened. Waylode is organised around the work itself: repositories, worktrees, tasks, sessions, code and machine load. It shows that state to you in the UI, and to your agents through the same facts over MCP (66 tools: 55 `waylode_*` and 11 `devmap_*`, or 55 with `WAYLODE_MCP_NO_DEVMAP=1`, plus 4 prompts and 12 resource facets).

| Chat-centred agent apps | Waylode |
|---|---|
| One transcript per agent; history is scroll-back | One live map of every repository, worktree, task and session (*Whale Pod*) |
| The agent sees the code it opened | Agents query the stacked code graph, collisions, tasks and load (`waylode_insights`, `devmap_*`) |
| Agents work alone | Agents claim files, message peers cross-vendor (*Whale Song*) and file tasks for each other |
| You read what the agent says it did | You see what it changed: WAL ledger, provenance, turn checkpoints, ODB proposals, diffs |
| The machine is invisible | The Host-Load Governor shows the machine's state and acts on it |

The chat-centred tools are good at what they do. This is a difference in what each is organised around, not a claim that they are only chat.

### Where Waylode sits

Claude Code, Codex, T3 Code, VS Code and Cursor are agents, or editors that host agents. Waylode is the layer above them. It does not replace Claude Code or Codex. It launches them, watches them, guards them and lands their work. The case for it appears when you run 4–10 agents across several repositories at once:

1. **You lose the map (*Whale Pod* · Fleet & Agents Tree).** Which agent is in which repository and worktree, and which one is waiting for you? Waylode shows every live session, including ones it did not launch (`claude`, `codex`, `antigravity`, `devcouncil`, `manvi`), in one collapsible tree beside Fleet (`⌘⇧F`).
2. **Agents collide (*Whale Song* · A2A Peer Bus & 3-Layer Radar).** Two agents edit the same file in different worktrees. The 3-layer CSR/ODB collision radar, file claims, and `PreToolUse` guard hook warn before the merge does, while agents message across vendors via the peer mailbox (`waylode_agent_*`), structured asks (`waylode_inbox_*`), worktree lane messages, and **Send to agent**.
3. **Agents do damage.** They run commands outside their checkout, commit secrets or force a reset. Manvi's 8-status command gate, desktop-only inode-bound repository trust, the `dc-redact` secret gate on commits Waylode makes, Signal Guard, and CAS pre-operation undo snapshots (`refs/gitpulse/ops/`) cover this.
4. **Agents do not know the code (*Whale Sonic* · DevMap Code Graph).** Linked in-process via `StackedStore` (`stack_lease.json` + sealed `0444` `base.sqlite` + lazy Tree-sitter `delta.sqlite` overlays), DevMap gives them and you layered blast radius (with `unattributed`-line disclosure), backward regression suspects, confidence-tiered dead symbols (`0.9` act-on vs `0.4` unconfirmed), and affected tests with scoped `CI:local`.
5. **The laptop melts.** Six `cargo build`s at once leave you a beachball. The Host-Load Governor refuses an agent launch under memory pressure (or any input you set to refuse), with a human-only **Admit anyway**, queues heavy cargo/go builds for slots, parks idle sessions, and isolates background spawns under macOS QoS.
6. **You still need Git.** The lane-solved commit graph, `git replay` branch stack tree, diffs, 3-way conflicts, pure-ODB conflict proposals (`refs/gitpulse/proposals/`, created by the `waylode-worktree` CLI), AIMD batched Land Queue (`git merge-tree --stdin`; upcoming 0.2.0), turn checkpoints, reflog, pull requests and Actions are in the same window, not a second app.

Every tool before this assumed one person, one checkout and one terminal. Waylode assumes many agents, many worktrees and many repositories, with a person supervising.

### What this does and does not claim

We compare capabilities, not prices: every agent here needs its own plan or API key, whichever workspace runs it. We have not found another agentic workspace that puts a full Git client with a pure-ODB merge train, an AST code graph across stacked worktrees, cross-vendor A2A messaging, agent guards, a load scheduler, and a verification-gated task board that agents write to in one app. "Have not found" is the claim, not "none exists". Some pieces exist elsewhere on their own: cross-vendor agent messaging (MCP Agent Mail), blast radius with affected tests over MCP (codebase-memory-mcp, CodeGraph), agents filing tasks (Vibe Kanban's MCP server, Beads) and hosted merge queues (GitHub, Graphite, Mergify). What we did not find anywhere on `2026-11-28`: collision warnings that check the call graph, one code index shared across worktrees with each worktree's uncommitted edits layered on top, local pre-push batch landing of agent branches, load-based admission before an agent starts, and a built-in `done` gate on passed verification.

### Platforms

Waylode is tested on macOS. Windows and Linux builds are produced by release CI but have not been tested. Do not read them as supported yet.

### Architecture, not a footprint claim

Waylode is a Rust core on the system webview (Tauri 2), with no bundled Chromium. T3 Code's desktop app, Claude Desktop, VS Code, Cursor, Conductor, Codex app, and GitKraken Desktop are Electron apps (sources below). We publish no size, memory or speed comparison between them, because none was measured.

## Six capabilities other ADEs lack (or don't combine)

Chat-first and single-vendor ADEs work well for one agent in one checkout, and break down when you run **4–10 parallel agents** across repositories: transcripts serialize what is actually a parallel worktree graph, vendor-locked teams cannot coordinate across Claude/Codex/Manvi/Antigravity, uncoordinated `cargo build` or test runs exhaust RAM, and merging 4–10 branches falls back to manual terminal cleanup. Across the products we checked on `2026-11-27` and `2026-11-28` (listed below and in the [Whale field guide](launch/WHALE_FIELD_GUIDE.md)), none combined these six shipped `main` capabilities in one local-first native window:

1. **Code Index Integration (*Whale Sonic* · DevMap `StackedStore`)** — For the worktrees Waylode creates for agents: linked in-process via `stack_lease.json`, a sealed (`0444`) `base.sqlite` snapshot, and lazy per-worktree Tree-sitter `delta.sqlite` overlays of each worktree's changes (zero write-on-read; `8 MiB` per-file / `10,000` dirty-file overlay bounds). Exposes layered blast radius (`impact_layered` / `devmap_impact`) with `unattributed`-line honesty, backward regression suspects (`waylode_codeintel_suspects`), confidence-tiered dead symbols (`0.9` act-on vs `0.4` unconfirmed), affected tests + scoped `CI:local` (failing closed to the full suite when freshness degrades), and `:` / `::` / `~` palette symbol search. *Why others break:* grep and embeddings miss call-graph edges and either re-index every worktree or drift stale across parallel checkouts. Open-source MCP servers (codebase-memory-mcp, CodeGraph) offer blast radius and affected tests, but none of the major ADEs we checked documents it for its agents, and no tool we checked layers each worktree's uncommitted edits over one shared index (codebase-memory-mcp indexes linked worktrees as separate projects).
2. **Cross-Vendor A2A & 3-Layer Collision Radar (*Whale Song* & *Whale Pod*)** — Live process/registry discovery of unlaunched `claude`, `codex`, `antigravity`, `devcouncil`, and `manvi` sessions (`waylode_sessions`); cross-vendor `0600`-flocked peer mailbox (`waylode_agent_register`, `_peers`, `_send`, `_inbox`, `_wait` up to 45s kqueue), structured asks (`waylode_inbox_post`, `_read`, `_withdraw`), worktree lane bus (`waylode_worktree_send_message` / `_inbox`), **Send to agent** (`waylode-hook deliver`), and 3-layer collision detection (Layer 1 declared/observed file and symbol claims via `waylode_worktree_claim`/`_release`, Layer 2 1-hop CSR call-graph semantic overlap, Layer 3 ODB `git merge-tree` of committed work), which agents check on demand (`waylode_worktree_radar`). A `PreToolUse` hook separately asks before an edit to a file another worktree has uncommitted changes to; claims are advisory unless enforce mode is on in a shared checkout. *Why others break:* single-vendor agent teams cannot talk across providers, and worktree isolation alone only hides overlapping edits until merge time. MCP Agent Mail already offers a cross-vendor mailbox with advisory path-glob reservations enforced by a commit hook. A collision warning that checks the call graph, not just file paths, was **not found** in the products we checked. Waylode's "A2A Peer Bus" is a local feature; it does not implement the Linux Foundation A2A protocol.
3. **Pure-ODB Merge Train & `git replay` Stack Restacking** *(merge train upcoming 0.2.0; not yet on `main`)* — AIMD batched Land Queue (`cmd_worktree_merge_batch` / `waylode-worktree land-batch`) executing `ObjectQuarantine` `git merge-tree --stdin` with SQLite WAL FIFO tickets and green-ref hooks; each branch is verified before it is submitted, and the combined batch is not re-tested. Pure-ODB Conflict Proposals (`refs/gitpulse/proposals/` with `ProposalSweeper` and `decide_land` safety gating) are created by the `waylode-worktree` CLI and opened in the app; and `git replay` branch stack restacking (parent-before-child with disjoint-dirty fast path and rollback). *Why others break:* landing 4–10 finished worktrees manually—or colliding on disk checkouts—turns parallel coding into serial merge babysitting. Hosted merge queues (GitHub, Graphite, Mergify) batch PRs and test the combined batch on the server after push; Waylode does not re-test the batch. Local, pre-push batch landing of agent worktrees was **not found** in the agent workspaces we checked.
4. **Load & Agent Scheduler (Host-Load Governor & APFS CoW Worktrees)** — Agent launches refused under memory pressure (or any input you set to refuse), with a human-only **Admit anyway** override; build slot leases and a queue for heavy cargo/go builds (showing queue position on the terminal pill), one-click idle agent session parking, macOS `PRIO_DARWIN_PROCESS` background QoS isolation + shared git spawn gate, live CPU/GPU/memory/thermal/storage host-load strip, and APFS `clonefile` `node_modules` / `target` dependency caches (one cloned `target` held `1.8 MB` private bytes out of `66 GB` logical vs a `26.7 GB` cold build on Waylode's repo, measured once on 2026-11-14). *Honest credit:* `worktrunk` already ships APFS `clonefile` CoW caches cleanly in a focused Rust CLI. *Why others break:* 6 parallel `cargo build` or test runs thrash RAM/swap and beachball the machine. Claude Code's supervisor stops idle sessions once memory stays low, and Codex and Jules cap thread counts; both act after the fact or by a fixed number. Admission control before an agent or build starts was **not found** in the products we checked.
5. **Verification-Gated, Agent-Writable Task Board** — Five-column cross-repository and saved-workspace Kanban board (Backlog, Ready, In progress, Review, Done; paging up to 2,000 cards shared per fill), one-click agent launch from any card, `waylode_add_task` with duplicate gating and `expected_revision` optimistic concurrency protection, and `waylode_complete_task` requiring passed Manvi verification on the worktree's current `HEAD` (otherwise routing to `Review`). *Why others break:* out-of-scope discoveries get buried in chat transcripts, and agents mark tasks done on stale or unverified trees. Agents can file tasks elsewhere (Vibe Kanban's MCP server, Beads), and Claude Code's `TaskCompleted` hook can block completion if you write the hook yourself. A built-in `done` gate on passed verification of the current `HEAD` was **not found** in the products we checked.
6. **Manvi Guards, Provenance & Full Native Git GUI in Rust/Tauri 2** — Manvi's 8-status command gate (*Allowed*, *Demoted*, *Granted*, *Widened*, *Degraded*, *Warned*, *Blocked*, *Unchecked*), `HostScope` checkout fences, desktop-only `.git` path/inode/birth-time trust, `dc-redact` secret gate, Signal Guard, CAS pre-op undo snapshots (`refs/gitpulse/ops/`), opt-in Turn Checkpoints (`refs/gitpulse/turns/`), WAL event ledger + provenance (`waylode_ledger_events`, `waylode_provenance`), persistent `waylode-ptyd` (protocol 5) + localhost Browser pane, and a lane-solved native Git GUI (`~41 ms` lane solve / `~58 ms` assembly on 100k synthetic commits). *Why others break:* Electron chat/editor shells lack a full commit graph, reflog, CAS undo, WAL provenance, and deterministic 8-status command gating, while native Git GUIs lack agent orchestration, code graphs, and load governors.

## Comparison: agentic workspaces & CLIs (As of `2026-11-27`)

Legend: ✅ stated by the official source · ◐ partial (the limit is named) · ❌ the source says no / absent from docs · **?** not found in the pages read.

| Capability | **Waylode** (`HEAD`) | Claude Code | T3 Code | VS Code (Copilot) | Cursor | Conductor | Codex app |
|---|---|---|---|---|---|---|---|
| App runtime | **Rust + system webview (Tauri 2)** | CLI + **Electron** (Claude Desktop) | **Electron** + local server | **Electron** | **Electron** (VS Code fork) | **Electron** + local Rust server | **Electron** + bundled core |
| Platforms | macOS tested (Win/Linux CI built, untested) | macOS, Win, Linux | macOS, Win, Linux, web, mobile | macOS, Win, Linux, Web | macOS, Win, Linux | macOS only | macOS, Win (Linux not stated) |
| Many repos in one window | ✅ tabs (1,000), stacks, Fleet | ◐ Agent View (background only) | ✅ multi-repo projects | ✅ multi-root | ◐ multi-root | ✅ | ◐ threads per repo |
| Parallel agents in worktrees | ✅ APFS CoW caches, `StackedStore` DevMap | ✅ `--worktree` | ✅ per-thread + handoff | ✅ | ✅ (single-root) | ✅ | ✅ |
| Multi-vendor agents | ✅ Claude, Codex, Manvi, Grok, Antigravity, shell | ❌ Claude only | ✅ widest (ACP/adapters) | ✅ Copilot, Claude, Codex | ❌ Cursor loop only | ✅ Claude, Codex, Cursor, OpenCode | ❌ OpenAI only |
| Detects unlaunched agents | ✅ process scan + registry | ◐ its own Claude Code sessions only | ? | ◐ extension-hosted only | ❌ | ? | ? |
| Cross-agent file collision warning | ✅ 3-layer CSR/ODB radar, claims, guard hook | ❌ (docs warn same-file edits overwrite) | ? (worktree isolation) | ❌ | ❌ | ❌ (warns in docs) | ❌ |
| A2A messaging | ✅ peer inbox, asks, lane bus (cross-vendor) | ✅ Agent Teams + cross-session messaging (Claude Code only) | ◐ outside agents message threads over MCP | ❌ | ❌ (parent→subagent only) | ◐ chaining/multiplayer | ❌ |
| Task board launching agents | ✅ 5-col cross-repo Kanban | ❌ Agent Teams file task list | ◐ Thread Sidebar inbox | ❌ | ❌ | ❌ | ◐ Automations + Triage inbox |
| Agents file follow-up tasks | ✅ `waylode_add_task` (dup-gated) | ◐ shared task list, no board | ? | ◐ external MCP trackers only | ❌ | ? | ? |
| Command / policy guard | ✅ Manvi 8-status gate + desktop trust | ✅ hooks + permissions | ◐ approval prompts | ◐ tool/terminal approvals | ◐ allowlists / hooks | ◐ human approvals | ◐ OS sandbox + approvals |
| Code-graph impact & affected tests | ✅ DevMap AST graph + suspects | ◐ LSP references + call hierarchy (plugins) | ❌ | ❌ (search/embeddings) | ❌ (`Instant Grep` + embeddings) | ❌ | ❌ |
| Load-aware scheduling | ✅ memory-pressure governor + build slots | ◐ stops idle sessions under low memory | ❌ | ❌ | ❌ | ❌ | ❌ |
| Full Git GUI | ✅ graph, blame, conflicts, reflog, `git replay` stacks, Land Queue | ❌ | ◐ diff, commit, PR | ◐ source control graph (full via GitLens) | ❌ (no graph/reflog) | ◐ diff, PR | ◐ diffs, PR review |
| GitHub PRs / Actions | ✅ PRs + Actions | ◐ | ◐ PRs; Actions ? | ◐ via extensions | ◐ Cloud Agent PRs | ◐ | ◐ |
| Persistent PTY across quit | ✅ `waylode-ptyd` (Unix) | ◐ | ? | ◐ shell reconnect | ❌ | ◐ | ◐ |
| MCP server exposed | ✅ 66 tools (`55 waylode_*` + `11 devmap_*`) | ✅ `claude mcp serve` | ✅ `/mcp` endpoint | ? no server mode found | ? no server mode found | ? no server mode found | ❌ client only (`codex mcp-server` removed) |
| Telemetry posture | Local-first (no analytics dependency added; network audit pending) | ◐ opt-out (`DISABLE_TELEMETRY=1`) | ◐ Axiom/analytics opt-out | ◐ opt-out + OTel opt-in | ◐ Privacy Mode / OTLP | ◐ PostHog | ◐ OTel configurable |
| Cloud / remote execution | Not in this release (Labs preview) | ✅ Managed Agents / remote | ✅ T3 Connect / LAN / Tailscale | ◐ SSH, Dev Tunnels, GH agent | ✅ Cloud VMs | ? (Cloud shutdown notice withdrawn `2026-11-13`) | ✅ Cloud tasks + SSH |

## Comparison: editors, terminals, git clients & 2026 specialists (As of `2026-11-27`)

| Capability | **Waylode** (`HEAD`) | Zed | Warp | GitKraken Desktop | Tower |
|---|---|---|---|---|---|
| Category / Runtime | Native ADE + Git GUI (**Rust + Tauri 2**) | Native editor (**Rust + GPUI**) | Native terminal (**Rust**) | Git GUI + Agents (**Electron**) | Native Git GUI (**Swift/AppKit, Win native**) |
| Many repos / Worktrees | ✅ / ✅ | ✅ / ✅ (per-thread) | ◐ / ✅ (Codebase Context) | ◐ / ✅ (1 per session) | ◐ / ◐ (human-driven) |
| Multi-vendor / Unlaunched detect | ✅ / ✅ | ✅ (ACP) / ◐ (imports) | ✅ / ❌ | ✅ / ✅ (outside sessions, 6 vendors) | ◐ (commit msg CLIs) / ❌ |
| Collision radar / A2A / Agent board | ✅ / ✅ / ✅ | ❌ / ❌ / ❌ | ❌ / ◐ (subagents) / ❌ | ◐ (branch/PR only) / ❌ / ◐ (Launchpad) | ❌ / ❌ / ❌ |
| Command guard / Code graph / Load sched. | ✅ / ✅ / ✅ | ✅ (`tool_permissions`) / ❌ / ❌ | ◐ (profiles) / ◐ (context index) / ❌ | ◐ (Allow/Deny) / ❌ / ❌ | ❌ / ❌ / ❌ |
| Full Git GUI / PRs & Actions | ✅ / ✅ both | ◐ / ◐ links | ❌ / ◐ Oz PRs | ✅ / ✅ both | ✅ / ✅ PRs (Actions ❌) |
| MCP server exposed | ✅ 66 tools | ❌ client only | ❌ client only | ✅ (14 primary in `mcp.so` snapshots / 22+ cited) | ❌ (`gittower` CLI + skill) |

### Additional 2026 ADEs & Specialists Checked (`2026-11-27`)

- **Worktrunk (`max-sixty/worktrunk`):** Rust CLI that **already uses APFS `clonefile`/CoW** for worktree dependency caches (`node_modules`, `target/`), lifecycle hooks, and port allocation. Waylode's differentiation is combining CoW caches with a desktop ADE, `StackedStore` DevMap graphs, a host-load governor, and an AIMD pure-ODB merge train.
- **Windsurf (Cognition Wave 13):** Parallel Cascade worktrees + MCP client; no AST blast radius/affected tests, no cross-vendor peer bus, no host-load governor, and no ODB merge train.
- **Google Antigravity:** Planner-Executor split, `invoke_subagent`, local/cloud Linux sandboxes, integrated browser, and semantic indexing; no cross-vendor local peer bus, Git GUI/merge train, CoW caches, or host-load governor.
- **Kiro:** Spec-driven waves (`tasks.md`), Kiro Crew, Tree-sitter + LSP, and `PreToolUse` hooks; no full Git GUI, cross-vendor peer mailbox, ODB merge train, or host-load governor.
- **Sourcegraph Amp:** Cloud Orbs (sleep/wake), Sourcegraph multi-repo search, and shareable team threads; cloud-centric rather than a local Rust Git/load control plane.
- **OpenCode / Paradev / Claude Squad / GitButler:** OpenCode (75+ providers, LSP, `AGENTS.md` permissions), Paradev (formerly Superset; Electron orchestrator for 10–100+ CLI worktrees), Claude Squad (`git worktree` + `tmux`), and GitButler (virtual/stacked branches in one checkout) each solve slices of parallel coding, without combining an AST code graph, 3-layer collision radar, A2A peer bus, host-load governor, and ODB merge train.
- **Added `2026-11-28` (see the [Whale field guide](launch/WHALE_FIELD_GUIDE.md) for the full tables):**
  - **MCP Agent Mail:** a cross-vendor mailbox (Claude Code, Codex CLI, Gemini CLI, Copilot CLI) with advisory path-glob file reservations and a commit hook. It matches file paths only, not the call graph.
  - **codebase-memory-mcp:** a local open-source MCP server that maps uncommitted changes to blast radius. Its affected-test selection is experimental and limited to supported C/C++ harnesses, and it indexes linked worktrees as separate projects.
  - **CodeGraph:** a local open-source MCP server with impact analysis (opt-in MCP tool) and a CLI `codegraph affected` for tests. It warns when a worktree silently borrows the main checkout's index rather than sharing one by design.
  - **Vibe Kanban:** its MCP server gives agents issue tools. Its maker, bloop, shut down on `2026-04-10`, and the project is community-maintained.
  - **Beads:** an issue tracker built for agents.
  - **GitHub merge queue, Graphite, Mergify:** hosted merge queues that batch PRs after push.
  - **A2A protocol:** the Linux Foundation protocol for agent-to-agent delegation. Waylode's A2A Peer Bus is a local feature and does not implement it.

### What the tables show

- Waylode is not the only app with agent guards (Zed, GitKraken, Claude Code, Kiro), with worktree agents (Zed, GitKraken, Cursor, VS Code, Claude Code, Windsurf, Worktrunk), or with an MCP server (GitKraken, Claude Code, T3 Code).
- No product we checked, including the `2026-11-28` additions, showed these: collision warnings that check the call graph, one code index shared across worktrees with uncommitted edits layered on top, local pre-push batch landing of agent branches, load-based admission before an agent starts, and a built-in `done` gate on passed verification. That means "not found in the products we checked", not "no other product has it".
- The case for Waylode is all of it in one native app, alongside a full Git client and pure-ODB merge train.

## Drawbacks, for each product, for multi-agent work

Each line names a limit the product's own documentation states. None is an opinion.

| Product | Drawback for running many agents across many repos | Source |
|---|---|---|
| Claude Code | Claude only; Agent View is background-only; Agent Teams coordination stays within Claude | code.claude.com/docs/en/agent-view, /agent-teams |
| T3 Code | Electron app, early desktop build; threads rather than a Kanban board; git limited to diff, commit and PR | github.com/pingdotgg/t3code, mintlify.com/pingdotgg/t3code |
| VS Code | Codex-extension sessions don't appear in the Agents window; docs call a worktree "not a security boundary"; no MCP server mode found | code.visualstudio.com/docs/agents/concepts/agent-harnesses |
| Cursor | An editor first, not a cross-repo control plane; no MCP server mode found | cursor.com/docs/configuration/worktrees |
| Conductor | macOS only; announced it is joining Meta, then withdrew its shutdown notice (v0.94.2, `2026-11-13`); PostHog analytics; warns agents in one workspace can edit the same files | conductor.build/changelog |
| Codex app | OpenAI agents only; MCP client only (`codex mcp-server` removed) | openai.com/index/introducing-the-codex-app, developers.openai.com/codex |
| Zed | An editor; no commit graph found in its git docs; hosted AI and collaboration send server-side data | zed.dev/docs/git, /telemetry |
| Warp | A terminal, with no git GUI found; telemetry opt-out | docs.warp.dev/agents/, /platform/harnesses/ |
| GitKraken | Electron; collision checks cover branch and worktree names only, not files | help.gitkraken.com/gitkraken-desktop/ |
| Tower | No agent launching, task board or guards found; no GitHub Actions UI | git-tower.com/features/all-features |

**Coming from Conductor?** See the [Conductor migration guide](launch/MIGRATING_FROM_CONDUCTOR.md) for a 1:1 concept map, what Waylode adds, and honest platform/release limits.

**Waylode's own limits.** It is tested on macOS only. It has no cloud agents in this release. It is not an editor. Antigravity sessions get no `PreToolUse` command guard from Waylode. Worktrees are not an OS sandbox, and guards apply only where hooks are registered.

### Free and open source

- **Waylode Primary (Free & Open Source, `MIT OR Apache-2.0`):** Zero sign-in and zero local paywalls across the desktop ADE, local DevCouncil engines (`devmap`, `dcstore`, `dcverify`, `dcgrep`, `dcredact`, public OSS PR Gate), local Manvi harness/policy enforcement/JSON audit export, local MarkDev/Graft, and **BYOK** compute (SSH runners, `cloud_vms`, your own model keys).

## Where the others are stronger

- **MCP Agent Mail** is a focused mail and reservation server any MCP agent can use, with its own TUI and web UI (MIT licence with a rider restricting use by OpenAI and Anthropic).
- **codebase-memory-mcp and CodeGraph** give any MCP agent a local code graph with impact analysis, without installing a desktop app.
- **Worktrunk** is a focused Rust CLI that already does APFS `clonefile`/CoW dependency caches, lifecycle hooks, and port allocation cleanly in the terminal.
- **Zed** is a full editor with its own agent, edit prediction and real-time multiplayer.
- **Warp** has managed cloud agents (Oz) and scheduled or triggered automation.
- **GitKraken** has a mature git GUI, issue-tracker integration (Launchpad, Jira, Trello) and team and enterprise plans. Waylode imports GitHub issues as tasks; tracker integrations beyond GitHub are not in this release, so this stays a GitKraken strength.
- **Tower** is a long-polished git client with a PR manager across seven hosts.
- **Claude Code and Codex** are the agents themselves. They have first-party depth, cloud sessions and scheduled routines that run with the laptop shut.
- **VS Code and Cursor** are editors: in-place editing, debugging and extensions. Cursor adds best-of-N and cloud VMs.
- **T3 Code** supports the most agent vendors and also runs on mobile and web.
- **Cloud products** win on always-on execution, shared sessions and team admin.

## Sources (Verified `2026-11-27`)

- **Claude Code:** [worktrees](https://code.claude.com/docs/en/worktrees), [agent teams](https://code.claude.com/docs/en/agent-teams), [mcp](https://code.claude.com/docs/en/mcp), [monitoring usage](https://code.claude.com/docs/en/monitoring-usage), [data usage](https://code.claude.com/docs/en/data-usage), [agent view](https://code.claude.com/docs/en/agent-view), [desktop](https://code.claude.com/docs/en/desktop)
- **T3 Code:** [GitHub](https://github.com/pingdotgg/t3code), [desktop guide](https://www.mintlify.com/pingdotgg/t3code/guides/desktop-app)
- **VS Code:** [agents overview](https://code.visualstudio.com/docs/copilot/agents/overview), [worktrees](https://code.visualstudio.com/docs/copilot/agents/worktrees), [mcp servers](https://code.visualstudio.com/docs/copilot/agent-customization/mcp-servers), [monitoring agents](https://code.visualstudio.com/docs/copilot/agents/guides/monitoring-agents), [telemetry](https://code.visualstudio.com/docs/configure/telemetry)
- **Cursor:** [agent](https://cursor.com/docs/agent), [worktrees](https://cursor.com/docs/configuration/worktrees), [mcp](https://cursor.com/docs/context/mcp), [opentelemetry export](https://cursor.com/docs/enterprise/opentelemetry-export), [cloud agents](https://cursor.com/help/ai-features/cloud-agents)
- **Conductor:** [announcement](https://www.conductor.build/announcement), [changelog](https://www.conductor.build/changelog)
- **Codex app:** [introducing the codex app](https://openai.com/index/introducing-the-codex-app), [codex docs](https://developers.openai.com/codex), [codex cli](https://learn.chatgpt.com/codex/cli)
- **Zed:** [external agents](https://zed.dev/docs/ai/external-agents), [parallel agents](https://zed.dev/docs/ai/parallel-agents), [tool permissions](https://zed.dev/docs/ai/tool-permissions), [git](https://zed.dev/docs/git), [telemetry](https://zed.dev/docs/telemetry)
- **Warp:** [download](https://www.warp.dev/download), [agents](https://docs.warp.dev/agents/), [harnesses](https://docs.warp.dev/platform/harnesses/)
- **GitKraken:** [agents](https://help.gitkraken.com/gitkraken-desktop/agents/), [worktrees](https://help.gitkraken.com/gitkraken-desktop/worktrees/), [GitHub Actions](https://help.gitkraken.com/gitkraken-desktop/github-actions/), [terminal](https://help.gitkraken.com/gitkraken-desktop/terminal/), [MCP tools](https://help.gitkraken.com/mcp/MCP-tools-reference/), [CLI security](https://help.gitkraken.com/gitkraken-cli/gk-cli-security/), [Electron blog](https://gitkraken.com/blog/5-electron-apps)
- **Tower:** [features](https://www.git-tower.com/features/all-features), [worktrees](https://www.git-tower.com/features/worktrees), [worktrees mac](https://www.git-tower.com/help/guides/worktrees/overview/mac), [PRs](https://www.git-tower.com/help/guides/pull-requests/working-with-pull-requests/windows), [CLI mac](https://www.git-tower.com/help/guides/integration/cli-tool/mac), [privacy](https://www.git-tower.com/legal/privacy-policy)
- **2026 Specialists:** [Worktrunk](https://github.com/max-sixty/worktrunk) ([worktrunk.dev](https://worktrunk.dev)), [Windsurf](https://windsurf.com/blog), [Antigravity](https://antigravity.google/docs), [Kiro](https://kiro.dev/docs), [Sourcegraph Amp](https://ampcode.com), [OpenCode](https://opencode.ai/docs), [Paradev](https://github.com/ParadevAI/Paradev), [Claude Squad](https://github.com/smtg-ai/claude-squad), [GitButler](https://gitbutler.com)

**Caveats.** The tables compare capabilities only, not prices. Kepler is a separate GitKraken product, and its features are not credited to Desktop.
