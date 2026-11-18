# GitPulse launch plan: video, slides, docs and screenshots

Drafted 2026-11-17 against local `main` at `16eba11f3` (version `1.99.0`, the 2.0 beta). It was
re-checked at `4c25d1d13` after the same-day merge train, which landed load leases, parking and the
resource profile.
This is the plan. The deck, the video and the doc edits come after the recording gate in §10.

Every feature row below carries a status. Only **Shipped** rows, plus the **In flight** rows listed
in §10 as "must land before recording", may appear on screen.

| Status | Meaning |
|---|---|
| **Shipped** | Committed on `main` at `16eba11f3`, with source evidence |
| **In flight** | Exists only as uncommitted work on `main` or on an unmerged branch (another session owns it) |
| **Labs** | Built on the `labs` branch behind the `cloud-labs` Cargo feature; no release build enables it |
| **Not found** | Asked for or documented, but no code was found |

Evidence came from four passes: a code inventory of HEAD, a Labs/roadmap pass over `labs` and
`land/*`, a competitor pass over official docs, and re-checks of the disputed rows. Nobody has run
the app against this list yet. The rehearsal in §10 is where each demo path gets confirmed.

---

## 1. Positioning

### The one-liner

> **GitPulse is the free, local-first command centre for agentic coding.** It runs Claude Code, Codex
> and other agents side by side across all your repositories. Each agent gets its own worktree, a
> policy guard and a code map, and the load scheduler keeps the laptop responsive. Every git tool
> you need sits in the same window.

### The core idea: a workspace, not a chat window

> **GitPulse is built for the agentic era. It gives the same information and the same tools to the
> people and to the agents doing the work.** Most agent apps are organised around a chat transcript:
> one agent, one conversation, and you scroll to find out what happened. GitPulse is organised around
> the work itself: repositories, worktrees, tasks, sessions, code and machine load. It shows that state
> to you in the UI, and to your agents through the same facts over MCP (65 tools).

Show this as a two-column contrast, on slide 4 and in the 0:20–0:45 part of the video:

| Chat-centred agent apps | GitPulse |
|---|---|
| One transcript per agent; history is scroll-back | One live map of every repo, worktree, task and session |
| The agent sees the code it opened | Agents query the code graph, collisions, tasks and load (`gitpulse_insights`, `devmap_*`) |
| Agents work alone | Agents claim files, message peers and file tasks for each other |
| You read what the agent says it did | You see what it actually changed: ledger, provenance, turn checkpoints, diffs |
| The machine is invisible | The load scheduler shows the machine's state and acts on it |

Wording rule: say "chat-centred" or "built around a transcript", not "they are just chat tools".
The comparison slide names the real strengths of each product, and an unfair line undercuts it.

### Why it exists when Claude Code, T3 Code, VS Code and Cursor do

Those tools are **agents, or editors that host agents**. GitPulse is the **layer above them**. It
does not replace Claude Code or Codex; it launches them, watches them, guards them and lands their
work. The pitch is about what happens when you run five agents across three repositories at once:

1. **You lose the map.** Which agent is in which repo and worktree, and which one is waiting for you?
   GitPulse shows every live session, including ones it did not launch, in one tree.
2. **Agents collide.** Two agents edit the same file in different worktrees. GitPulse's collision radar
   and claims warn before the merge does, and agents can message each other through the peer inbox.
3. **Agents do damage.** They run commands outside their checkout, commit secrets, or force a reset.
   Manvi's command gate, per-repository trust, the secret gate and pre-operation undo snapshots cover this.
4. **Agents don't know the code.** DevMap gives them, and you, the blast radius and the affected tests
   for a change.
5. **The laptop melts.** Six `cargo build`s at once leaves you a beachball. The load scheduler admits,
   queues or sheds heavy work by host pressure.
6. **You still need git.** You still need the graph, the branch stack tree, diffs, conflicts, reflog,
   PRs and Actions. They are in the same window, not a second app.

### The "free" claim, worded so it holds

T3 Code is also free and MIT-licensed, so we must **not** say "the only free ADE". Use this instead:

> **No other free agentic workspace puts a full git client, a code graph, agent guards, a load
> scheduler, a task board agents write to, and agent-to-agent messaging in one app.**
>
> Lead with the four capabilities none of the ten researched products showed: cross-agent file
> collision warnings, agents filing follow-up tasks, code-graph impact and affected tests, and
> load-aware scheduling.

The comparison table in §2 is what backs this sentence. Until the uniqueness rows are verified per
product (§2, "Before publishing"), the deck says "we have not found another…", not "no other…".

### "Agentic era" framing

Every tool before this assumed one person, one checkout and one terminal. GitPulse assumes many
agents, many worktrees and many repositories, with a person supervising. That is the core message.

---

## 2. Comparison table (DRAFT)

This table needs verification before publishing. Legend: ✅ stated by the official source · ◐ partial
(the limit is named) · ❌ the source says no · **?** not found in the pages read. A ? is **not** a no.

| Capability | **GitPulse** | Claude Code | T3 Code | VS Code (Copilot) | Cursor | Conductor | Codex app |
|---|---|---|---|---|---|---|---|
| Price | **Free, MIT** | Paid plan ($20/mo and up) | Free, MIT | ? | Free tier, paid $8–200 | Free (joining Meta) | ? |
| App runtime | **Rust core on the system webview (Tauri 2)** | ? (CLI; desktop app runtime not checked) | **Electron** ([T3 Code desktop guide](https://www.mintlify.com/pingdotgg/t3code/guides/desktop-app), [DeepWiki](https://deepwiki.com/pingdotgg/t3code/4-desktop-application)) | ? | ? | ? | ? |
| Platforms | macOS tested; Windows and Linux built, testing pending | macOS, Windows, Linux | macOS, Win, Linux, mobile, web | ? | ? | macOS | macOS, Windows (Linux unclear) |
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
| MCP server exposed to agents | ✅ 65 tools | ? | ? | ? | ? | ❌ | ? |
| Local-first, no telemetry | ✅ local-first; "zero telemetry" is the README's claim (no analytics dependency found; not audited) | ◐ opt-out | ? | ? | ? | ◐ PostHog | ? |
| Cloud / remote execution | **Labs** (ssh runners) | ✅ cloud, SSH | ✅ LAN/Tailscale | ◐ | ✅ cloud VMs | Cloud shutting down | ✅ |

### Editors, terminals and git clients (DRAFT, fetched 2026-11-17)

These pages were read through a summarizing fetcher. Re-verify every ✅ before printing it.

| Capability | **GitPulse** | Zed | Warp | GitKraken Desktop | Tower |
|---|---|---|---|---|---|
| Category | Agentic workspace + git client | Editor with agent panel | Agentic terminal | Git GUI + agent sessions | Git GUI |
| App runtime | **Rust core on the system webview (Tauri 2), no bundled Chromium** | ? | ? | **Electron** ([GitKraken blog](https://gitkraken.com/blog/5-electron-apps)) | ? |
| Price | **Free, MIT** | Free personal; Pro $10/mo; Business $30/seat | Free; Build $20/mo; Max $200; Business $50/user | Free Community; paid prices not shown on the page | No free plan; $69–149/yr |
| Platforms | macOS tested; Windows, Linux built, untested | macOS, Windows, Linux | macOS, Windows, Linux | Windows, macOS, Linux | macOS, Windows |
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
| MCP server for agents | ✅ 65 tools, free | ? | ? | ✅ 22 tools, Pro and up | ? (`gittower` CLI + Agent Skill) |
| Cloud / remote execution | Labs (own machines free) | ? | ✅ Oz cloud agents | ◐ remote environments (Pro+) | ? |

**What this changes in the pitch:**
- GitPulse is **not** the only app with agent guards (Zed, GitKraken), with worktree agents (Zed,
  GitKraken, Cursor, VS Code, Claude Code) or with an MCP server (GitKraken).
- The rows where **no researched product showed the capability** are cross-agent file collision
  warnings, agents filing follow-up tasks on a board, code-graph impact and affected tests, and
  load-aware scheduling.
- Lead with those four, plus "all of it, in one free app, alongside a full git client."

**Sources:**
- **Zed:** [pricing](https://zed.dev/pricing), [external agents](https://zed.dev/docs/ai/external-agents), [parallel agents](https://zed.dev/docs/ai/parallel-agents), [tool permissions](https://zed.dev/docs/ai/tool-permissions), [git](https://zed.dev/docs/git)
- **Warp:** [pricing](https://www.warp.dev/pricing), [download](https://www.warp.dev/download), [agents](https://docs.warp.dev/agents/), [harnesses](https://docs.warp.dev/platform/harnesses/)
- **GitKraken:** [pricing](https://www.gitkraken.com/pricing), [agents](https://help.gitkraken.com/gitkraken-desktop/agents/), [worktrees](https://help.gitkraken.com/gitkraken-desktop/worktrees/), [MCP tools](https://help.gitkraken.com/mcp/mcp-tools-reference/)
- **Tower:** [pricing](https://www.git-tower.com/pricing), [features](https://www.git-tower.com/features/all-features), [release notes](https://www.git-tower.com/release-notes)

**Caveats:**
- GitKraken's paid prices are not shown on its page, so don't quote any.
- Tower's prices are read as annual (an inference).
- Kepler is a separate GitKraken product; don't credit its features to Desktop.

### Trade-offs: each product's drawbacks for multi-agent work

Every drawback below comes from the sources already cited in §2. Keep the wording factual. Each line
names a limit the product's own docs state, not an opinion.

| Product | Drawback for running many agents across many repos | Source |
|---|---|---|
| Claude Code | Claude only; no free plan includes it; agent view is a research preview that shows only background sessions | claude.com/pricing; code.claude.com/docs/en/agent-view |
| T3 Code | Electron app, early (alpha) desktop build; threads, not a task board; git limited to diff, commit and PR | t3code README and desktop guide |
| VS Code | Codex-extension sessions don't appear in the Agents window; its docs say a worktree "isn't a security boundary"; sessions don't isolate files on their own | code.visualstudio.com docs |
| Cursor | Cloud agents need the Premium plan or higher; an editor first, not a cross-repo control plane | cursor.com/docs, cursor.com/pricing |
| Conductor | macOS only; joining Meta, with its cloud product shutting down; PostHog analytics; warns that agents in one workspace can edit the same files | conductor.build announcement, docs |
| Codex app | OpenAI agents only; Linux availability unclear | learn.chatgpt.com/docs/app |
| Zed | An editor; no commit graph found in its git docs; hosted AI and collaboration need server-side data | zed.dev/docs/git, zed.dev/docs/telemetry |
| Warp | A terminal, with no git GUI found; cloud agents and higher limits on paid tiers; telemetry opt-out | warp.dev/pricing, docs.warp.dev privacy |
| GitKraken | Electron; its MCP server needs Pro or higher; remote environments need Pro or higher; collision checks cover branch and worktree names only, not files; paid prices not shown | gitkraken.com/pricing, help.gitkraken.com |
| Tower | No free plan ($69–149/yr); no agent launching, task board or guards found | git-tower.com/pricing, features |

GitPulse's own limits go on the same slide. It is tested on macOS only, there are no cloud agents
outside Labs, it is not an editor, and Antigravity sessions get no command guard.

### Where the others are genuinely stronger (say this on a slide; it builds trust)

- **Zed** is a full editor with its own agent, edit prediction and real-time multiplayer.
- **Warp** has managed cloud agents (Oz) and scheduled or triggered automation.
- **GitKraken** has a mature git GUI, issue-tracker integration (Launchpad, Jira, Trello) and team/enterprise plans. GitPulse's answer is the Labs issue-tracker integrations (§5, coming soon); until they ship, this stays a GitKraken strength.
- **Tower** is a long-polished git client with a PR manager across four hosts.

- **Claude Code / Codex** are the agents themselves. They have first-party depth, cloud sessions and
  scheduled routines that run with the laptop shut.
- **VS Code / Cursor** are editors: in-place editing, debugging and extensions. Cursor adds best-of-N
  and cloud VMs.
- **T3 Code** supports the most agent vendors and also runs on mobile and web.
- **Cloud products** win on always-on execution, shared sessions and team admin.

### Before publishing the table

1. For each product, run a targeted search on the three rows where GitPulse claims to be unique:
   collision warning, code-graph impact and load-aware scheduling. Fill every ? or leave it as ?.
2. Re-check prices on the live pages on the day you publish. Cursor's tier names had changed.
3. Before naming Conductor, re-check its status (Meta acquisition, cloud shutdown).
4. Run the four "only GitPulse" rows (collision warnings, agents filing tasks, code-graph impact,
   load-aware scheduling) as targeted searches against all ten products. Until then, the slide says
   "not found in any of the products we checked".
5. Add the date "as of <date>" under the table.

---

## 3. Every feature, by area

Demo locations come from code. Each one gets confirmed during rehearsal.

### 3.1 One workspace for every project
| Feature | Status | Where to show it |
|---|---|---|
| Repository tabs (up to 1,000 open) with collapsible groups | Shipped | Tab bar; `⌘O` / `⌘T`, `Ctrl+Tab` |
| Worktree tab stacks: every checkout of a repo in one tab with a switcher | Shipped | Open two worktrees of one repo |
| Fleet: a grid comparing repos on churn, sync and health | Shipped | `⌘⇧F` |
| Work rows: one row per place work is happening; blocked worktrees first | Shipped | Work → Overview |
| Global and saved-workspace task boards across repos | Shipped | Tasks button beside Fleet |
| Repository logos on tabs, with a picker | In flight | Tab bar |
| File explorer docked in the sidebar or terminal | In flight | Sidebar |

### 3.2 Agent control plane
| Feature | Status | Where to show it |
|---|---|---|
| Six launchers: Shell, Claude, Codex, Manvi, Grok, Antigravity (`agy`), plus a Browser tab | Shipped | Dock → new session type |
| Per-launcher default model, effort, advisor and commit model | Shipped (commit model on `main`) | Settings → Agents |
| Live session detection, including sessions GitPulse did not launch (Claude, Codex, Antigravity, DevCouncil, Manvi) | Shipped | Agents view, `gitpulse_sessions` |
| **Agent tree**: parent and child sessions nested, collapsible, filterable | Shipped | Agents view (`AgentsView.svelte`) |
| Worktree per agent: copy-on-write `node_modules`/`target`, `.worktreeinclude`, DevMap store seeded from the main checkout | Shipped (no seed-time figure until it is measured) | Work → Overview; `claude --worktree` |
| **Agent-to-agent communication**: peer inbox, `agent_register` / `send` / `wait` / `peers`, worktree messages, Send to agent | Shipped | Agent inbox; two agents coordinating |
| Collision radar, file claims and the collision guard hook | Shipped | Two agents editing one file |
| Follow agent: a split pane that follows the agent's file edits live | Shipped | Dock toolbar toggle |
| Turn checkpoints (`refs/gitpulse/turns/`) | Shipped (opt-in) | `gitpulse_turn_checkpoints` |
| Ledger and provenance: which session changed what, and how a commit landed | Shipped | `gitpulse_provenance`, `gitpulse_landed` |
| Merge-conflict prediction between worktrees, in a quarantine | Shipped | `gitpulse_merge_conflicts` |
| Notifications, attention inbox, menu-bar status glyph | Shipped | Menu bar; Settings → Layout |
| Managed runs that survive quit (hosted `manvi serve`) | Shipped (Unix) | Quit with an agent running |
| MCP server for agents: **65 tools**, 4 prompts, `gitpulse://` resources; plugins for Claude, Codex, Cursor and Antigravity | Shipped | Agent calling `gitpulse_insights` |
| Agents reading output from shells the user opened (opt-in) | In flight | Settings |

### 3.3 Agent guards with Manvi
| Feature | Status | Note for the script |
|---|---|---|
| Command gate on every agent shell command (Allowed, Demoted, Granted, Widened, Degraded, Warned, Blocked, Unchecked) | Shipped | Don't say "5 verdicts"; the code has 8 statuses |
| Scope fences: refuses writes into another checkout (`sed -i`, `tee`, `rm`, `mv` …) | Shipped | Good live demo |
| Per-repository trust; only the desktop can grant it | Shipped | Trust prompt on first open |
| Secret gate on commit and push (`dc-redact`) | Shipped for commits GitPulse makes | Don't claim it catches an agent's own `git commit` |
| Pre-operation undo snapshots (`refs/gitpulse/ops/`) with CAS-checked restore | Shipped | History → HEAD moves |
| Signal guard: agents cannot kill GitPulse | Shipped | Mention only |
| Limits to state honestly | n/a | Antigravity has no PreToolUse guards; Labs runner sessions run ungated |

### 3.4 DevCouncil and DevMap code intelligence
| Feature | Status | Where to show it |
|---|---|---|
| Map: navigator, files, symbols, subsystems, docs, doc graph, links | Shipped | Code → Map |
| Blast radius with a rung filter | Shipped | History → Diff |
| Affected tests; CI:local scoped to them, falling back to the full suite | Shipped | Work → Remote → CI:local |
| Regression suspects | Shipped | History → Graph → Suspects |
| Dead symbols, clones, trace, workspace-wide symbol search | Shipped | Palette `:` / `::` |
| DevCouncil suite panel and installer | Shipped | Settings |
| Explorer search via `dcgrep` | In flight | Code → Explorer |

### 3.5 Task board
| Feature | Status |
|---|---|
| Five columns (Backlog, Ready, In progress, Review, Done), swimlanes, WIP, filters, fills to 2,000 cards | Shipped |
| Cross-repo and saved-workspace boards | Shipped |
| Quick add, compose from notes, due dates, relations (blocks/depends), merge duplicates | Shipped |
| Manvi assist: refine, suggest, field locks | Shipped |
| **Launch an agent from a card**: model, effort and advisor per launch; one tracked attempt per run | Shipped |
| Verification chips; `complete_task(done)` needs a passed verification, otherwise the card goes to Review | Shipped |
| GitHub issue → task; archive dock | Shipped |
| Agents file tasks over MCP, with duplicate gating | Shipped |

**Why this board matters for agents.** Other agent apps keep work in threads, so a gap an agent notices
mid-run lives in a transcript and is lost when the thread scrolls away. T3 Code, for example, organises
threads in an inbox-style sidebar rather than a board (per a [Digg summary](https://digg.com/tech/14rflh3u)
of a Theo post; confirm the original before quoting). GitPulse's board is a durable, shared work queue
that **agents write to as well as read from**:

- **Gaps become cards, not lost lines.** An agent that spots out-of-scope work files it with
  `gitpulse_add_task`, and it lands on your board for later. The board refuses a near-duplicate until
  the agent has reviewed the related cards (`reviewed_related`), and `expected_revision` protects
  concurrent edits. `gitpulse_import_tasks` turns a repo's `tasks/*.md` briefs into cards.
- **The board launches the agent.** Pick a card, choose an agent, model and effort, and it runs in its
  own worktree as a tracked attempt. The brief names the task, so the agent can read it back with
  `gitpulse_get_task`.
- **Done means verified.** `gitpulse_complete_task(done)` needs a passed verification on HEAD;
  otherwise the card moves to Review.
- **The board stays clean.** Duplicates merge (`gitpulse_merge_tasks`), and deletions carry a recorded reason.
- **It works across repositories.** Cross-repo and saved-workspace boards, relations (blocks/depends)
  and DAG launch readiness.

Positioning line: **"Your agents don't forget what they found. They put it on the board."**

### 3.6 Remote and GitHub
We read "Remote" two ways and cover both. The GitHub section ships today. Remote runners over ssh
are Labs (§5).

| Feature | Status |
|---|---|
| PRs: view, review, approve, merge, check out, open as a worktree, create | Shipped |
| Issues and issue → task | Shipped |
| Actions: workflows, runs, dispatch, rerun, cancel, timelines | Shipped |
| Releases and tag publishing with preflight | Shipped |
| Firebase App Hosting rollouts | Shipped |
| GitHub avatars on the graph | Shipped |
| Dependabot and code-scanning alerts | Shipped (Health) |

### 3.7 Git client
| Feature | Status |
|---|---|
| Commit graph with lane solving, avatars and hover cards | Shipped |
| **Branch stack tree**: stacked branches as a tree, restack parent-before-child with rollback | Shipped |
| Diff: split/unified, word diff, image diff, History rail, Shift-click range compare | Shipped |
| Hunk and line staging; commit composer; Apple Intelligence commit drafting on macOS | Shipped |
| 3-way conflict editor that leads Overview while a merge is parked | Shipped |
| HEAD moves (reflog) with undo | Shipped |
| Cherry-pick, revert, reset, interactive rebase, stash, remotes, submodules | Shipped |
| Branch and tag cleanup with tip backups and one-click restore | Shipped |

### 3.8 Files and code
| Feature | Status |
|---|---|
| File tree with git status, virtualized viewer, editing with compare-and-save | Shipped |
| Markdown via MarkDev; media and hex viewers | Shipped |
| Blame toggle with code-age timeline | Shipped |
| Repository-wide content search that reports capped or cancelled results | Shipped |

### 3.9 Insights
| Feature | Status |
|---|---|
| Pulse: heatmap, rhythm, punch card, code age, DORA, hotspots, knowledge map, period compare | Shipped |
| Coverage: 7 formats parsed (LCOV, Cobertura, Go, Istanbul, SimpleCov, JaCoCo, Clover) | Shipped |
| Health: 12 sections (vulnerabilities, secrets, outdated packages, dead code, terminal incidents …) | Shipped |
| Secrets scan in-process, with Kingfisher as an optional second opinion | Shipped |
| Storage and hygiene: commit-graph write, cache inventory, build cleaner | Shipped |

### 3.10 Load scheduler
| Feature | Status | Note |
|---|---|---|
| Memory-pressure governor: admits, queues or sheds agent launches; human "Admit anyway" | Shipped | Acts on memory pressure only |
| Heavy-command gate in the hook; shared spawn budget for git | Shipped | |
| Build leases: slots and a queue for heavy builds; the wrapper on the agent's PATH; queue position on the terminal pill | Shipped (merged in `d81e6a811`) | |
| Idle agent sessions classified, and parked with one click (reopen mark kept) | Shipped (Health) | |
| Resource profile sizing GPU contexts and caches to the machine | Shipped (`d9d88f0f6`) | |
| Host load section in Health (build slots, idle sessions); pressure dot in the status bar | Shipped | |
| Live CPU/GPU/memory/thermal strip with a graph popover | In flight | Needed for the best version of scene 8 |

### 3.11 Terminal for power users
| Feature | Status |
|---|---|
| Dock beneath every view (`` Ctrl+` ``), per repository, expandable | Shipped |
| Tabs; **nested splits** (rows and columns, up to 8 panes); tab and session search; Find; text size | Shipped |
| **Split groups** of panes (`SplitGroup` in the uncommitted `splitTree.ts`) | In flight |
| Sessions grouped by repository in the session list | Shipped |
| Sessions that survive reload and app quit (`gitpulse-ptyd`), with transcript recovery and resume | Shipped (Unix) |
| WebGL rendering, synchronized output, Unicode 11 | Shipped |
| Command blocks (OSC 133) | Shipped |
| Browser pane for localhost dev servers, which agents can open | Shipped |
| Status-bar terminal pill; shortcut guide | Shipped |
| Select-to-copy, OSC 52, "Copy last command output", "Run in new split shell" | In flight |
| Launcher icons and per-tab counts | In flight |

### 3.12 Customization
| Feature | Status |
|---|---|
| 17 theme presets (Standard, Developer, Sci-fi), My themes (24), import/export | Shipped |
| 11 macOS wallpapers plus your own image; glass appearance | Shipped |
| Wallpaper motion (opt-in) | In flight |
| Command palette with 8 modes (`>` `/` `%` `#` `@` `:` `::` `?`) | Shipped |
| 9 settings sections with search; status bar full, minimal or hidden | Shipped |
| Product tour and setup wizard | Shipped |
| User-defined keyboard shortcuts | Planned |

### 3.13 Platform
**Tested on macOS only. Windows and Linux builds are produced in CI but have not been tested.** Say
exactly this everywhere until each platform has a recorded test pass. Never call Windows or Linux
"supported" before then. Record and screenshot on macOS only. macOS builds are ad-hoc signed (not notarized),
and Windows builds are unsigned. Release CI builds five targets: macOS universal, Linux x64/arm64 and
Windows x64/arm64. On Windows without a process scan, some live agent sessions show as unknown
rather than zero. There are
five binaries (`gitpulse`, `gitpulse-mcp`, `gitpulse-hook`, `gitpulse-ptyd`, `gitpulsed`). The README
claims zero telemetry. No analytics dependency was found in `Cargo.toml` or `package.json`, but no
one has audited the network calls. Audit them before the claim goes on a slide.

### Metrics allowed on slides, the site and in the video

**Rule:** a number goes public only if it has a source in this table. Benchmarks, timings and test
counts are excluded until a committed benchmark or a run shows them. That rules out the old
"100K commits at 60/120 FPS" claim, the "~1.2 s DevMap seeding" figure from the release notes, and the
Labs test counts (those were grep counts, not test results).

| Metric | Value | Evidence (HEAD) |
|---|---|---|
| MCP tools for agents | 65 (54 `gitpulse_*` + 11 `devmap_*`) | Key count of `src-tauri/src/mcp/tool_input_schemas.snapshot.json` |
| MCP prompts | 4 | `src-tauri/src/mcp/prompts.rs` |
| Native commands | 330+ | `generate_handler!` in `src-tauri/src/lib.rs`; two counting methods gave 332 and 335, so publish "330+" |
| Views and sections | 4 and 11 | `src/lib/views/viewRegistry.ts` |
| Launchers | 6 (Claude, Codex, Manvi, Grok, Antigravity, shell) | `src/lib/terminal/tabs.ts:15` |
| Worktree coordination tools | 11 | the `gitpulse_worktree_*` family in the same snapshot |
| Theme presets / wallpapers | 17 / 11 | `src/lib/ui/appearance/presets.ts`, `backdrops.ts` |
| Palette modes | 8 | `src/lib/palette/modes.ts:26-34` |
| Coverage formats parsed | 7 | `src-tauri/src/analyzer/coverage.rs` parser dispatch |
| Health sections | 12 | `src/lib/components/health/sections.ts:56` |
| Task board capacity | 2,000 cards per board fill | `fillBoard` / `MAX_CANDIDATES` |
| Open repository tabs | up to 1,000 | `src/lib/repos/tabModel.ts:26` (`MAX_OPEN_TABS`) |
| Lane solve on 100,000 commits | ~41 ms (assembly ~58 ms) | `docs/PERFORMANCE.md:1367,1379`. Usable only with "lane solve" in the wording; never as a frame rate |

Recount every row on the release commit and update the table before publishing.

### "Light": architecture only

**Decision (2026-11-17): no app-versus-app footprint comparison.** The benchmark was dropped before
its counted runs. Say only the sourced architecture line: "A native Rust core on the system webview,
with no bundled Chromium." The sources are `src-tauri/` and `tauri.conf.json` for GitPulse, and the
GitKraken blog and T3 Code desktop docs for their Electron builds. Publish no size, RAM or "N× lighter"
figure, for GitPulse or for anyone else.

---

## 4. Coming improvements

These are roadmap slides only. Never present them as shipped.

**Landing next (code exists, uncommitted or on a branch)**
- Terminal split groups, a docked explorer sidebar, copy actions, launcher icons and tab counts
- Live host-load strip (CPU, GPU, memory, thermal) with a graph popover
- Repository logos, wallpaper motion, refreshed app icon
- `dcgrep` search in the explorer
- Opt-in agent reads of user shells

**Planned (on the task board)**
- Load governor stages 3–4: a pressure verdict in the command gate, and a load line in the session brief
- Cross-repo execution queue, two-way GitHub issue sync, DAG task planning
- Notifications on Windows/Linux; CI and due-date reminders
- Browser pane M3/M4: element-to-source mapping, then a Chromium driver
- Customizable shortcuts and i18n groundwork
- "Ask Manvi to organise the board"
- Posting `devcouncil verify` results to GitHub
- Windows and Linux test passes, so each can move from "built" to "tested"
- Windows CI green; an 8-hour soak test; notarized macOS builds

**Ideas**
- "Fork a lane"
- Feeding an earlier attempt's outcome into the next brief

---

## 5. GitPulse Labs (future paid features)

All of these are built on the `labs` branch (114 commits ahead of `main`). Each sits behind the
`cloud-labs` Cargo feature, a per-feature entry in `labs.json`, and "Show Labs features" in
Settings → Features. **No release build enables them.**

**Pricing rule (your decision, 2026-11-17, refined the same day):** you charge **only where GitPulse
carries an ongoing infrastructure cost**: relay traffic and storage (device and settings sync, team
pause and presence), hosted compute (future hosted runners), and a few other non-local
integrations (Jira, Trello and others). Everything local or on the user's own systems is free, and
so is signing in. The account is only an identity, and the cost sits in the features that move
data through the relay. This matches the `labs` branch's tier notes (sync is Pro, team is Team,
runners and the vault are free). Prices are still to be set.

| Labs feature | What the user gets | Maturity | Tier |
|---|---|---|---|
| Remote runners | Run an agent or a task attempt on **your own** Mac mini or Linux box over ssh + tmux; bring the work back | Built with tests; provisioned runners not built; **sessions run ungated** | **Free** (your own machines) |
| Secrets vault | Per-repo encrypted `.env`/keys, sealed by the Secure Enclave, injected only into launches you start, `op://` support | Built with tests; some hardware behaviour unverified | **Free** (local) |
| Snapshot import | Import a branch bundle into quarantine, review risks and diff, accept or reject | Built with tests | **Free** (local); it is also the receiving end of paid sync |
| Sign in with GitHub (account) | Identity on the GitPulse relay; required by the relay-backed features | Built with tests; needs the relay | **Free**: sign-in alone costs nothing to run; only the features that move data through the relay are paid |
| Device sync | End-to-end-encrypted branch snapshots between your devices; safety-code pairing; recovery key | Built with tests, including against a real relay; custom crypto needing external review | **Paid: Pro** |
| Settings sync | Preferences follow you; risky changes need a native confirm on the receiving device | Built; rides on device sync | **Paid: Pro** |
| Team kill switch | An admin pauses every agent across a team's repos | Built with tests; a member from another account cannot clear a pause yet | **Paid: Team** |
| Team presence | n/a | Design only | **Paid: Team** |
| Hosted runners | Compute GitPulse would operate | Design seam only (`runner/provision.rs`) | **Paid** (future) |
| **Issue-tracker integrations: Jira, Trello and more** | One issue inbox across trackers; start a task or an agent attempt from any tracker's issue; status written back | **Coming soon** (not built; GitHub issues → tasks already ship free in core) | **Paid** (third-party hosted services) |

Free Labs features (runners, vault, snapshot import) can graduate into the free app once they pass
review. Paid ones need the relay, the account and external crypto review first.

**How to present Labs:** one slide, labelled "Labs preview — not in this release". In the video, a
10-second teaser at most, showing only screens from a Labs build that was actually recorded. Never
demo sync, the vault or the kill switch as shipped; the branch's own docs require external crypto
review first.

---

## 6. Demo videos

### The two videos (your decisions, 2026-11-17)

| | **Short** | **Long** |
|---|---|---|
| Length | 75 s | about 8 min |
| Purpose | Social, website hero, README | YouTube, launch post, docs |
| Structure | Hook → five beats → CTA (script below) | 14 scenes (table below) |
| Voice | TTS added later by you (Google or Grok), from the narration text below | Same |
| Captions | Burned in, matching the narration | Burned in, matching the narration |
| Build | **Recorded only after the in-flight features are committed and merged** | Same |

**What I deliver for each video:**
- a silent edited cut (1080p, 60 fps H.264) with burned-in captions;
- a timecoded narration file (plain text, one block per scene, cut to fit its slot at about
  150 words per minute) ready for Google or Grok TTS;
- the raw takes.

You add the voice, then I re-time the cuts to the generated audio.

### Recording setup (your decisions: real workspace, real agents)

- **Your real workspace.** Before anything leaves this Mac, review every frame for:
  - tokens, keys and `.env` contents;
  - email addresses;
  - private repo names you don't want shown;
  - notification banners;
  - Claude/Codex account names.

  Blur in the edit, or retake. Mute notifications except GitPulse's during takes.
- **Capture:** `screencapture -v -V<seconds> -k -R<x,y,w,h>` on the GitPulse window only (not the whole
  display), with clicks shown. I drive the app through computer use. Take one 20-second pilot clip of
  scene 3 for your approval before shooting the rest.
- **One theme throughout** (Neural HUD or Midnight IDE), plus a light theme only in the customization scene.
- **You click** every trust prompt, "Admit anyway" and the scene-9 quit and relaunch.

**Agent tasks for the live scenes** (approved 2026-11-17). Each runs in a fresh worktree of GitPulse,
and the diffs stay there for you to keep or drop:

| Agent | Task | Shows |
|---|---|---|
| Claude #1 | README: "6 formats" of coverage → 7 (adding SimpleCov) | Launch from a card; agent tree; worktree |
| Codex | `wiki/Security.md` and `wiki/Architecture.md`: Kingfisher is an optional second opinion, not the scanner | Second vendor; cross-vendor detection |
| Claude #2 | README: drop the "100K+ commits at 60/120 FPS" line | Edits README too, which triggers the collision warning against Claude #1 |
| Claude #1 (mid-run) | Files a follow-up card: "INSTALLATION.md: platform list and macOS-only testing" via `gitpulse_add_task` | Agents fill the board (scene 4b) |

The filed card stays on your real board, and MCP has no undelete.

**Narration files for TTS:** [`narration-short.txt`](narration-short.txt) and
[`narration-long.txt`](narration-long.txt), with timecoded blocks. The long file marks the sentences
that depend on in-flight features with `[IF LANDED]`.

### Short video script (75 s)

| Time | On screen | Narration (TTS) |
|---|---|---|
| 0:00–0:06 | Repo tabs, Agents tree filling with live sessions | "Many agents. Many repositories. One person." |
| 0:06–0:14 | Fast cut: scattered chat windows, then GitPulse's one live view | "Most agent tools give you a chat window. GitPulse gives you the whole workspace." |
| 0:14–0:26 | Task card → Launch Claude → worktree created → Codex launched on a second card | "Write a task, pick an agent, launch. Each one gets its own worktree." |
| 0:26–0:36 | Claude files a follow-up card; it appears in Backlog live | "When an agent finds more work, it files it on your board. Nothing gets lost." |
| 0:36–0:46 | Collision warning between the two Claude sessions; peer message | "Two agents on one file? GitPulse warns you first, and they can message each other." |
| 0:46–0:54 | Manvi gate blocks a write into another checkout | "Claude Code and Codex commands pass a policy gate before they run." |
| 0:54–1:02 | Diff with blast radius → CI:local running affected tests | "DevMap shows what a change touches, and runs only the tests it affects." |
| 1:02–1:09 | Montage: branch stack tree, conflict editor, terminal splits, themes | "Plus a full git client and a terminal built for power users." |
| 1:09–1:15 | Logo, "Free · open source · runs on your Mac", URL | "GitPulse. Free, open source, built for the agentic era." |

### Long video: script and shot flow

| # | Time | Scene | On screen | Voice-over (draft) | Depends on |
|---|---|---|---|---|---|
| 1 | 0:00–0:20 | Cold open | Pull back from one terminal to GitPulse with 5 agents live across 3 repo tabs; the Agents tree fills | "This is what coding looks like now: five agents, three repositories, one person. GitPulse is the place you run all of it." | Shipped |
| 2 | 0:20–0:45 | The problem | Quick montage: scattered chat transcripts and terminals, two agents editing one file, Activity Monitor in the red; then cut to GitPulse's one live map | "Most agent tools give you a chat window per agent. But agents collide, wander outside their checkout and melt your laptop, and a transcript won't show you that. GitPulse shows the work itself, to you and to your agents." | n/a (b-roll) |
| 3 | 0:45–1:30 | One workspace | Repo tabs → worktree stack switcher → Fleet `⌘⇧F` → Work → Overview with blocked worktree first | "Every repository, every worktree, every agent, in one window. Fleet compares them all. Overview puts what's blocked first." | Shipped |
| 4 | 1:30–2:30 | Task → agent | Tasks: quick add → Manvi refine → Launch: pick Claude, model and effort → new worktree created and seeded → second card launched with Codex | "Write the task, let Manvi tighten it, and launch. Each agent gets its own worktree, with build caches cloned and the code map already built." Measure the DevMap seed time at rehearsal; only quote a number you measured. | Shipped |
| 4b | (inside 4, +20 s) | Agents fill the board | Mid-run, the Claude agent notices an unrelated bug, calls `gitpulse_add_task`, and the card appears in Backlog live; a duplicate attempt is refused until related cards are reviewed | "When an agent finds something outside its task, it doesn't bury it in a transcript. It files it on the board, so nothing gets lost." | Shipped |
| 5 | 2:30–3:30 | Control plane | Agents tree with a parent and child; a session started outside GitPulse appears; collision warning as both agents touch one file; agent A messages agent B via the peer inbox; Follow agent split | "GitPulse sees your Claude Code, Codex and Antigravity sessions, even ones it didn't start. It warns before two of them collide, and lets them message each other instead of overwriting each other's work." | Shipped |
| 6 | 3:30–4:15 | Guards | A Claude session tries to write into another checkout → blocked by the Manvi gate; trust prompt; a commit made **in GitPulse's composer** with a planted fake key → secret gate stops it; HEAD moves → undo snapshot restores | "Claude Code and Codex shell commands go through Manvi's gate before they run. GitPulse checks its own commits and pushes for secrets, and the git operations it allows an agent to run can be undone." | Shipped; rehearse the exact blocked command |
| 7 | 4:15–5:00 | DevMap | Code → Map subsystems; History → Diff → blast radius; Work → Remote → CI:local runs only affected tests | "DevMap knows your code. See the blast radius of a change, and run just the tests it touches, for you and for your agents." | Shipped |
| 8 | 5:00–5:30 | Load scheduler | Kick off 4 agent builds → build slots queue them (queue place on each terminal pill) → Health shows slots and idle sessions → park an idle agent → host-load strip climbs | "Six parallel builds don't have to mean a frozen machine. The load scheduler gives heavy builds slots, queues the rest, and parks idle agents." | Leases and parking shipped; the strip is **in flight**. The governor's refusal only appears under real memory pressure, so show it only if it fires naturally |
| 9 | 5:30–6:15 | Terminal | Nested splits → split group → quit GitPulse, reopen, sessions still running → browser pane on localhost → command blocks → copy last output | "A terminal for power users: splits, groups, and sessions that survive quitting the app." | Splits/persistence shipped; groups and copy **in flight** |
| 10 | 6:15–7:00 | Git, fully | Graph with avatars → branch stack tree and restack → word diff and line staging → conflict editor → PR review and merge → Actions rerun | "And it's a complete git client: graph, stacks, diffs, conflicts, pull requests and Actions, all in the same place." | Shipped |
| 11 | 7:00–7:20 | Insights montage | Pulse heatmap, Coverage gutter, Health verdict, Storage | "Pulse, coverage, health and storage, all computed on your machine." | Shipped |
| 12 | 7:20–7:40 | Make it yours | Theme gallery, switching through 3 themes, wallpaper, palette `⌘K` modes | "Seventeen themes, your own wallpapers, a command palette for everything." | Shipped |
| 13 | 7:40–8:05 | Free + Labs + next | Comparison table card; Labs preview card; roadmap card | "GitPulse is free and open source, and it runs on your machine." Then one Labs line, worded to match decision 2 in §11 (for example "In Labs: remote runners, device sync and a secrets vault"). | Slides |
| 14 | 8:05–8:15 | Call to action | Logo, URL, GitHub | "Download GitPulse at gitpulse.vbcr.dev." | n/a |

---

## 7. Slide deck (16 slides)

1. **Title:** GitPulse, the command centre for agentic coding
2. **The shift:** from one dev/one branch to many agents, many worktrees, many repos
3. **Five problems:** lost map, collisions, damage, blind agents, melting laptop
4. **A workspace, not a chat window:** the two-column contrast from §1, then the layer diagram (Claude/Codex/Grok/agy → GitPulse → git/GitHub/DevMap/Manvi), with "the same facts for people (UI) and agents (MCP)"
5. **One workspace:** tabs, stacks, Fleet, Overview (screenshot)
6. **A board agents write to:** launch from a card; an agent filing a gap for later; verified Done (screenshot plus the five bullets from §3.5)
7. **Control plane:** agent tree, collision radar, agent-to-agent messaging (screenshot)
8. **Guards:** Manvi gate, trust, secret gate, undo snapshots
9. **DevMap:** blast radius, affected tests, CI:local
10. **Load scheduler:** governor and build slots (state "memory pressure today; CPU/GPU next" if the strip hasn't landed)
11. **Terminal:** splits, persistence, browser pane
12. **A full git client:** graph, branch stack tree, conflicts, PRs/Actions
13. **Insights and customization:** two-up
14. **Comparison:** the §2 table, dated, with the "where others are stronger" footnote
15. **Labs preview and roadmap:** clearly labelled future
16. **Free, local-first, open source:** numbers (65 MCP tools, 6 launchers …), download link

Build it with the Slides artifact type or a `.pptx`, whichever you prefer (§11, decision 4).

---

## 8. Documentation refresh

**Fix these stale claims first.** Each one was found to be wrong against the code. The locations are
tracked files at HEAD (found with `git grep`). The gitignored `FEATURES.md` and `docs/PROMO.md` repeat
most of them.

| Claim | Where (tracked) | Correct version |
|---|---|---|
| "100K+ commits at 60/120 FPS" | `README.md:131`, `docs/FEATURES.md:385`, `docs/ARCHITECTURE.md:475` | No frame-rate benchmark backs it. Quote `docs/PERFORMANCE.md:1367,1379` (100k-row lane solve ~41 ms, assembly ~58 ms) or drop it |
| "scaling to 1,000 repositories … 24 active watches" | `README.md:126` | 1,000 open tabs; 32 watched repos; 24 recents |
| "5-verdict ladder" | `README.md:232`, `docs/MODULE_INTEGRATION.md:52`, `docs/SECURITY.md:26`, `wiki/Architecture.md:40` | The code has 8 statuses; check `contracts/verdict.schema.json` before restating |
| Kingfisher as the secrets scanner | `wiki/Security.md:33`, `wiki/Architecture.md:77` | In-process scan; Kingfisher is an optional second opinion |
| "6 formats" of coverage | `README.md:151` | 7 parsed (+ coverage.py detected) |
| Search on `⌘⇧F` | gitignored `FEATURES.md` | That chord is Fleet; confirm the Search chord at rehearsal |
| Platform list | `docs/INSTALLATION.md` | Add Linux arm64 and Windows arm64 to match `release.yml`; state "tested on macOS; Windows and Linux builds available, not yet tested" |

**Files to update.** Edit the tracked ones. Root `FEATURES.md`, `docs/PROMO.md` and `docs/MANVI.md` are
gitignored, so changes there never ship.
- `README.md`: new hero line (§1), "Why GitPulse" block, comparison link, new hero screenshot, refreshed 2.0 bullets
- `docs/FEATURES.md`: restructure to §3's areas, with status columns removed for released items
- New `docs/WHY_GITPULSE.md`: positioning plus the dated comparison table and sources
- New `docs/AGENTS_GUIDE.md`, or extend `wiki/MCP and Agents.md`: launching, guards, agent-to-agent messaging, collision workflow
- `docs/GETTING_STARTED.md`: a "run your first two agents" path
- `wiki/Home.md`, `wiki/Views.md`, `wiki/Keyboard Shortcuts.md`: new screenshots and current shortcuts
- New `docs/LABS.md`: only after decision 1 in §11

---

## 8b. Website overhaul (gitpulse.vbcr.dev)

The page lives in the **Portfolio** repo (`~/Code/web/Portfolio`):
- `src/pages/GitPulse.jsx` (331 lines) and `src/pages/GitPulse.css`
- the catalog entry in `src/data/projectData.js` and route metadata in `src/lib/routes.js`
- screenshots under `public/assets/` (PNG and WebP)

Updating GitPulse does not publish the site.

**What's wrong today:**
- It pitches a git client ("A clearer view of your work") and has no agent story.
- It still lists retired sections (Resolve, Policy, Reflog as a section).
- Its screenshots are the September set.

**New page structure:**
1. **Hero:** the one-liner from §1, the hero-agents screenshot or a looping 15-second clip from the
   teaser, and Download plus GitHub buttons. Platform line: "For macOS. Windows and Linux builds are available but not yet tested."
2. **A workspace, not a chat window:** the two-column contrast.
3. **Five problems, five answers:** map, collisions, guards, code graph, load. Each has a short clip or a still.
4. **A board your agents write to:** the §3.5 story.
5. **Built for power users:** terminal, full git client, insights, themes.
6. **Comparison:** the dated table with its sources, linking to `docs/WHY_GITPULSE.md`.
7. **Numbers:** only rows from the metrics table in §3.
8. **Free vs Labs:** "Everything local or on your own machines is free." Then the Labs preview: free
   Labs (runners, vault) and paid Labs (sync, team, Jira/Trello and other issue-tracker integrations),
   marked "coming". No prices until decided.
9. **Docs hub:** refresh the existing guide cards (getting started, tasks, terminal, MCP and agents,
   plus the new agents guide).
10. **Footer:** MIT, the privacy statement (use the wording only after the telemetry audit in §3.13), changelog.

**Process:** work in the Portfolio repo, in a session rooted there. Regenerate WebP from the new PNGs
and check the page at phone width. Publish after the GitPulse docs PR lands, so the links resolve.

---

## 9. Screenshot shot list (replaces `docs/assets/*`, last updated 2026-09-15)

Take these from the real app on your real workspace, with the same frame review as the videos. Use harness pages, served by the `gitpulse-harness`
launch config (`vite.harness.config.ts`, port 5188), only for states you can't stage. Never caption a
harness capture as live data.

| File | Shot |
|---|---|
| `hero-agents.png` | 3 repo tabs, Agents tree with 5 sessions, terminal dock with a split |
| `overview.png` | Work → Overview: blocked worktree first, stack hierarchy, branch status strip |
| `tasks-board.png` | 5-column board with a card mid-launch |
| `task-launch.png` | Launch sheet: agent, model, effort, worktree |
| `agents-tree.png` | Nested parent/child sessions with attention filter |
| `collision.png` | Collision radar or guard notice between two agents |
| `agent-inbox.png` | Peer messages between two agents |
| `guard-blocked.png` | Manvi gate refusing an out-of-checkout write |
| `devmap-map.png` | Code → Map subsystems (replaces `DevMap.png`) |
| `blast-radius.png` | Diff with the blast radius panel |
| `graph.png` | Graph with avatars and hover card (replaces `screenshot-graph.png`) |
| `branch-stack.png` | Stack tree mid-restack |
| `conflicts.png` | 3-way conflict editor |
| `remote.png` | PR detail with checks and Actions rail |
| `terminal-splits.png` | 4-pane nested split, command blocks |
| `host-load.png` | Host-load strip and popover (after it lands) |
| `insights.png` | Pulse heatmap; also refresh coverage/health/storage |
| `themes.png` | Theme gallery, 2 themes side by side |
| `social-banner.jpg` | Rebuild from `hero-agents.png` |

---

## 10. Recording gate and production checklist

You said: commit and merge everything, then launch a live preview and record. The gate is concrete:

1. **Land the in-flight work** the videos depend on. **You chose to wait for this** (2026-11-17).
   As of `4c25d1d13`, none of it is committed on any local or remote branch. It exists only as
   uncommitted changes in the main checkout (about 234 files), held by other live sessions:
   - terminal split groups and copy actions (scene 9)
   - the host-load strip (scene 8)
   - repo logos, launcher icons and the new app icon (every scene)
   - the explorer sidebar, `dcgrep` search, wallpaper motion and agent shell reads
   
   Leases, parking and the resource profile have already landed.
2. **Land or drop `land/commit-model`** (`16eba11f3`). Keep `labs` separate.
3. **Go green:** `bun run ci:local` and `bun run check`. Then `bun run mcp:install` and
   `bun run mcp:doctor` (all OK), and `bun run check:schema-parity`. Build the app with
   `bun run tauri build` and install it, then relaunch GitPulse into it. Record from the installed
   build, not `tauri dev`, and write that build's commit into each video's notes. The app running on
   2026-11-17 dated from Nov 15, older than the binary on disk, so relaunching matters.
4. **Push `main`.** It is 210 commits ahead of `origin/main`, and the README images load from GitHub's `main`.
5. **Rehearse every scene** on the installed build and your real workspace. Confirm each shortcut and
   demo path in §3, the exact command the gate blocks in scene 6, and that a session survives quit
   in scene 9. Fix anything that fails before recording.
6. **Record:** a 20-second pilot of scene 3 first, for your approval. Then each scene as its own take,
   so any one can be re-shot. Edit the silent cuts with captions. You generate the TTS from the
   narration files, and I re-time the cuts to the audio.
7. **Publish order:** GitPulse docs and screenshots PR → website (Portfolio repo) → deck → video → posts.

---

## 11. Open decisions for you

1. **Labs tiers.** Decided: you charge only where GitPulse carries infrastructure cost (relay sync and
   team, hosted runners, and a few non-local integrations). Local, user-owned and sign-in are free (§5).
   Still open: the Pro and Team prices.
2. **Labs in this launch?** Recommendation: preview only. No release build enables `cloud-labs`, and
   sync, the vault and the kill switch need external crypto review first.
3. **Competitors.** Ten are researched: Claude Code, T3 Code, VS Code, Cursor, Conductor, Codex,
   Zed, Warp, GitKraken and Tower. Recommendation: put all ten in `WHY_GITPULSE.md` and on the site.
   The slide shows the six closest (Claude Code, T3 Code, Cursor, Zed, Warp, GitKraken), dated and
   sourced, with the "where others are stronger" row. The video shows one table card.
4. **Deck format and voice:** a Slides artifact vs `.pptx`; your own voice vs captions only.
5. **Website.** Decided: in scope, as a full overhaul (§8b).
6. **What to record from.** You asked for a live preview. Recommendation: rehearse in the live
   preview (`bun run tauri dev`), but record from the installed release build. The dev build differs
   in startup, icons and signing prompts, and you only want to record once.
