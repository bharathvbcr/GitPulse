# Why Waylode

[Documentation index](README.md) · [Features](FEATURES.md) · [Agents guide](AGENTS_GUIDE.md)

**DRAFT. Comparison data fetched 2026-11-17.** The comparison tables below are not final. Every cell
comes from a product's own pages, read through a summarizing fetcher, and each ✅ needs re-verifying
against the live page before it is quoted anywhere. Prices and plan names change. Cells marked **?** mean
the capability was not found in the pages read. A **?** is not a "no".

## A workspace, not a chat window

Waylode is built for the agentic era. It gives the same information and the same tools to the people
and to the agents doing the work.

Most agent apps are organised around a chat transcript: one agent, one conversation, and you scroll to
find out what happened. Waylode is organised around the work itself: repositories, worktrees, tasks,
sessions, code and machine load. It shows that state to you in the UI, and to your agents through the
same facts over MCP (66 tools).

| Chat-centred agent apps | Waylode |
|---|---|
| One transcript per agent; history is scroll-back | One live map of every repository, worktree, task and session |
| The agent sees the code it opened | Agents query the code graph, collisions, tasks and load (`gitpulse_insights`, `devmap_*`) |
| Agents work alone | Agents claim files, message peers and file tasks for each other |
| You read what the agent says it did | You see what it changed: ledger, provenance, turn checkpoints, diffs |
| The machine is invisible | The load scheduler shows the machine's state and acts on it |

The chat-centred tools are good at what they do. This is a difference in what each is organised around,
not a claim that they are only chat.

### Where Waylode sits

Claude Code, Codex, T3 Code, VS Code and Cursor are agents, or editors that host agents. Waylode is the
layer above them. It does not replace Claude Code or Codex. It launches them, watches them, guards them
and lands their work. The case for it appears when you run several agents across several repositories at once:

1. **You lose the map.** Which agent is in which repository and worktree, and which one is waiting for
   you? Waylode shows every live session, including ones it did not launch, in one tree.
2. **Agents collide.** Two agents edit the same file in different worktrees. The collision radar and file
   claims warn before the merge does, and agents can message each other through the peer inbox.
3. **Agents do damage.** They run commands outside their checkout, commit secrets or force a reset.
   Manvi's command gate, per-repository trust, the secret gate on commits Waylode makes and
   pre-operation undo snapshots cover this.
4. **Agents do not know the code.** DevMap gives them, and you, the blast radius and the affected tests
   for a change.
5. **The laptop melts.** Six `cargo build`s at once leave you a beachball. The load scheduler admits,
   queues or sheds heavy work by host pressure.
6. **You still need git.** The graph, the branch stack tree, diffs, conflicts, reflog, pull requests and
   Actions are in the same window, not a second app.

Every tool before this assumed one person, one checkout and one terminal. Waylode assumes many agents,
many worktrees and many repositories, with a person supervising.

### What this does and does not claim

We compare capabilities, not prices: every agent here needs its own plan or API key, whichever workspace
runs it. We have not found another agentic workspace that puts a full git client, a code graph, agent guards, a load scheduler, a task board
that agents write to, and agent-to-agent messaging in one app. "Have not found" is the claim, not "none
exists". Four capabilities in particular were not shown by any of the ten products researched: cross-agent
file collision warnings, agents filing follow-up tasks, code-graph impact with affected tests, and
load-aware scheduling.

### Platforms

Waylode is tested on macOS. Windows and Linux builds are produced by release CI but have not been
tested. Do not read them as supported yet.

### Architecture, not a footprint claim

Waylode is a Rust core on the system webview (Tauri 2), with no bundled Chromium. T3 Code's desktop app
and GitKraken Desktop are Electron apps (sources below). We publish no size, memory or speed comparison
between them, because none was measured.

## Comparison: agent apps (DRAFT, fetched 2026-11-17)

Legend: ✅ stated by the official source · ◐ partial (the limit is named) · ❌ the source says no ·
**?** not found in the pages read.

| Capability | **Waylode** | Claude Code | T3 Code | VS Code (Copilot) | Cursor | Conductor | Codex app |
|---|---|---|---|---|---|---|---|
| App runtime | **Rust core on the system webview (Tauri 2)** | ? (CLI; desktop app runtime not checked) | **Electron** ([desktop guide](https://www.mintlify.com/pingdotgg/t3code/guides/desktop-app), [DeepWiki](https://deepwiki.com/pingdotgg/t3code/4-desktop-application)) | ? | ? | ? | ? |
| Platforms | macOS tested; Windows and Linux built, not yet tested | macOS, Windows, Linux | macOS, Win, Linux, mobile, web | ? | ? | macOS | macOS, Windows (Linux unclear) |
| Many repos in one window | ✅ tabs, worktree stacks, Fleet | ◐ agent view lists sessions | ? | ✅ | ◐ multi-root | ? | ◐ |
| Parallel agents in worktrees | ✅ copy-on-write caches, DevMap seeding | ✅ `--worktree` | ✅ | ✅ | ✅ | ◐ | ✅ |
| Agents from several vendors | ✅ Claude, Codex, Grok, Antigravity, Manvi, shell | ❌ Claude only | ✅ widest (ACP) | ✅ Copilot, Claude, Codex | ? | ✅ | ❌ |
| Detects agents it did not launch | ✅ process scan + registry | ◐ background only | ? | ❌ | ? | ? | ? |
| Cross-agent file collision warning | ✅ radar, claims, guard hook | ◐ isolation only | ? | ❌ | ? | ❌ (warns in docs) | ? |
| Agent-to-agent messaging | ✅ peer inbox, `agent_send` / `agent_wait` | ✅ cross-session messaging | ? | ? | ? | ? | ? |
| Task board that launches agents | ✅ 5 columns, cross-repo | ❌ status list | ◐ inbox-style thread sidebar, not a board (source to confirm) | ? | ? | ? | ◐ automations |
| Agents file follow-up tasks for later | ✅ `gitpulse_add_task` with duplicate gating | ? | ? | ? | ? | ? | ? |
| Command/policy guard | ✅ Manvi gate + trust | ✅ hooks, permissions | ? | ? | ? | ? | ◐ sandbox |
| Code graph impact / affected tests | ✅ DevMap | ❌ not found | ? | ? | ? | ? | ? |
| Load-aware scheduling | ✅ memory-pressure governor, build slots | ❌ not found | ? | ? | ? | ? | ? |
| Full git GUI (graph, blame, conflicts, reflog) | ✅ | ❌ | ◐ diff/commit/PR | ? | ? | ◐ diff/checks | ◐ |
| GitHub PRs / Actions | ✅ review, merge, dispatch, rerun | ◐ PR/CI card | ◐ | ◐ | ? | ◐ | ? |
| Terminal with splits and sessions that survive quit | ✅ (Unix: `gitpulse-ptyd`) | ◐ | ? | ◐ | ? | ◐ | ◐ |
| MCP server exposed to agents | ✅ 66 tools | ? | ? | ? | ? | ❌ | ? |
| Local-first, no telemetry | ✅ local-first; "zero telemetry" is the README's claim (no analytics dependency found; not audited) | ◐ opt-out | ? | ? | ? | ◐ PostHog | ? |
| Cloud / remote execution | Not in this release | ✅ cloud, SSH | ✅ LAN/Tailscale | ◐ | ✅ cloud VMs | Cloud shutting down | ✅ |

## Comparison: editors, terminals and git clients (DRAFT, fetched 2026-11-17)

These pages were read through a summarizing fetcher. Re-verify every ✅ before printing it.

| Capability | **Waylode** | Zed | Warp | GitKraken Desktop | Tower |
|---|---|---|---|---|---|
| Category | Agentic workspace + git client | Editor with agent panel | Agentic terminal | Git GUI + agent sessions | Git GUI |
| App runtime | **Rust core on the system webview (Tauri 2), no bundled Chromium** | ? | ? | **Electron** ([GitKraken blog](https://gitkraken.com/blog/5-electron-apps)) | ? |
| Platforms | macOS tested; Windows, Linux built, not yet tested | macOS, Windows, Linux | macOS, Windows, Linux | Windows, macOS, Linux | macOS, Windows |
| Many repos in one window | ✅ | ✅ | ? | ◐ | ◐ |
| Parallel agents in worktrees | ✅ | ✅ opt-in per thread | ? | ✅ one per session | ? |
| Agents from several vendors | ✅ | ✅ via ACP | ✅ | ✅ | ◐ for AI commit messages only |
| Detects agents it didn't launch | ✅ | ◐ imports threads | ? | ◐ terminal-started agent shows on its worktree | ? |
| Cross-agent file collision warning | ✅ | ? | ? | ◐ branch/worktree names only | ? |
| Agent-to-agent messaging | ✅ | ? | ◐ parent spawns children | ? | ? |
| Task board that launches agents | ✅ | ? | ? | ◐ Launchpad starts a session from an issue | ? |
| Agents file follow-up tasks | ✅ | ? | ? | ? | ? |
| Command/policy guard | ✅ | ✅ `always_deny` / confirm / allow rules | ◐ agent profiles | ◐ Allow once / Deny | ? |
| Code-graph impact / affected tests | ✅ | ? | ◐ codebase indexing | ? | ? |
| Load-aware scheduling | ✅ | ? | ? | ? | ? |
| Full git GUI | ✅ | ◐ panel, blame, conflict buttons | ? | ✅ graph and blame | ✅ graph, blame, conflicts, reflog |
| GitHub PRs / Actions | ✅ both | ◐ links | ◐ cloud PR review | ✅ PRs; Actions ? | ✅ PRs (4 hosts); Actions ? |
| MCP server for agents | ✅ 66 tools | ? | ? | ✅ 22 tools | ? (`gittower` CLI + Agent Skill) |
| Cloud / remote execution | Not in this release | ? | ✅ Oz cloud agents | ◐ remote environments | ? |

### What the tables show

- Waylode is not the only app with agent guards (Zed, GitKraken), with worktree agents (Zed, GitKraken,
  Cursor, VS Code, Claude Code) or with an MCP server (GitKraken).
- The rows where no researched product showed the capability are cross-agent file collision warnings,
  agents filing follow-up tasks on a board, code-graph impact and affected tests, and load-aware
  scheduling. That means "not found in any of the products we checked". It does not mean "no other
  product has it".
- The case for Waylode is all of it in one app, alongside a full git client.

## Drawbacks, for each product, for multi-agent work

Each line names a limit the product's own documentation states. None is an opinion.

| Product | Drawback for running many agents across many repos | Source |
|---|---|---|
| Claude Code | Claude only; agent view is a research preview that shows only background sessions | code.claude.com/docs/en/agent-view |
| T3 Code | Electron app, early (alpha) desktop build; threads, not a task board; git limited to diff, commit and PR | t3code README and desktop guide |
| VS Code | Codex-extension sessions don't appear in the Agents window; its docs say a worktree "isn't a security boundary"; sessions don't isolate files on their own | code.visualstudio.com docs |
| Cursor | An editor first, not a cross-repo control plane | cursor.com/docs |
| Conductor | macOS only; joining Meta, with its cloud product shutting down; PostHog analytics; warns that agents in one workspace can edit the same files | conductor.build announcement, docs |
| Codex app | OpenAI agents only; Linux availability unclear | learn.chatgpt.com/docs/app |
| Zed | An editor; no commit graph found in its git docs; hosted AI and collaboration need server-side data | zed.dev/docs/git, zed.dev/docs/telemetry |
| Warp | A terminal, with no git GUI found; telemetry opt-out | docs.warp.dev agents, privacy |
| GitKraken | Electron; collision checks cover branch and worktree names only, not files | help.gitkraken.com |
| Tower | No agent launching, task board or guards found | git-tower.com features |

**Waylode's own limits.** It is tested on macOS only. It has no cloud agents in this release. It is not
an editor. Antigravity sessions get no command guard. The guards apply only where its hooks are
registered, so an agent started outside Waylode under another profile has none.

## Where the others are stronger

- **Zed** is a full editor with its own agent, edit prediction and real-time multiplayer.
- **Warp** has managed cloud agents (Oz) and scheduled or triggered automation.
- **GitKraken** has a mature git GUI, issue-tracker integration (Launchpad, Jira, Trello) and team and
  enterprise plans. Waylode imports GitHub issues as tasks; tracker integrations beyond GitHub are not
  in this release, so this stays a GitKraken strength.
- **Tower** is a long-polished git client with a PR manager across four hosts.
- **Claude Code and Codex** are the agents themselves. They have first-party depth, cloud sessions and
  scheduled routines that run with the laptop shut.
- **VS Code and Cursor** are editors: in-place editing, debugging and extensions. Cursor adds best-of-N
  and cloud VMs.
- **T3 Code** supports the most agent vendors and also runs on mobile and web.
- **Cloud products** win on always-on execution, shared sessions and team admin.

## Sources

- **Zed:** [external agents](https://zed.dev/docs/ai/external-agents), [parallel agents](https://zed.dev/docs/ai/parallel-agents), [tool permissions](https://zed.dev/docs/ai/tool-permissions), [git](https://zed.dev/docs/git)
- **Warp:** [download](https://www.warp.dev/download), [agents](https://docs.warp.dev/agents/), [harnesses](https://docs.warp.dev/platform/harnesses/)
- **GitKraken:** [agents](https://help.gitkraken.com/gitkraken-desktop/agents/), [worktrees](https://help.gitkraken.com/gitkraken-desktop/worktrees/), [MCP tools](https://help.gitkraken.com/mcp/mcp-tools-reference/), [Electron](https://gitkraken.com/blog/5-electron-apps)
- **Tower:** [features](https://www.git-tower.com/features/all-features), [release notes](https://www.git-tower.com/release-notes)
- **T3 Code:** [desktop guide](https://www.mintlify.com/pingdotgg/t3code/guides/desktop-app), [DeepWiki](https://deepwiki.com/pingdotgg/t3code/4-desktop-application). The "inbox-style thread sidebar" description comes from a [Digg summary](https://digg.com/tech/14rflh3u) of a post by Theo and has not been confirmed against the original.
- **Claude Code:** code.claude.com/docs/en/agent-view
- **VS Code, Cursor, Conductor, Codex app:** the pages named in the drawbacks table.

**Caveats.** The tables compare capabilities only, not prices. Kepler is a separate GitKraken product, and its features are not
credited to Desktop. Re-check Conductor's status (the Meta acquisition and the cloud shutdown) before
naming it publicly.

## Before this table is published

1. For each product, run a targeted search on the four rows where Waylode claims to be unique: collision
   warning, agents filing follow-up tasks, code-graph impact and load-aware scheduling. Fill each **?** or
   leave it.
2. Re-check Conductor's status.
3. Add "as of" with the publishing date under each table.
