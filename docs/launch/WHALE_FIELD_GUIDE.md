# Whale field guide

[Why Waylode](../WHY_WAYLODE.md) · [Features](../FEATURES.md) · [Migrating from Conductor](MIGRATING_FROM_CONDUCTOR.md)

The launch plan, Show HN kit and social posts mentioned below are local-only files (untracked on 2026-11-28).

This guide owns two things for launch copy: the **"Whale" nickname** and the **Whale feature names**, and the **claims
we can still make** about what Waylode does that other agentic development environments (ADEs) do not. It replaces
the scattered rules in the launch files. Where another file disagrees, this one wins, then that file gets fixed.

Research date: **2026-11-28**. Waylode code references are to `main` at `HEAD` `33c6e0862`.

---

## 1. The nickname

**Waylode is the product name and the trademark. "Whale" is what people call it out loud.** It is a nickname, not a
rebrand: nothing is renamed, nothing is registered, and the logo stays the Cobalt Way-Arc mark.

Why it exists: "Waylode" is new, and people stumble on it in speech ("Way-load? Way-lode?"). "Whale" sounds close,
is one syllable, and is easy to remember.

**Rules**

| Do | Don't |
|---|---|
| First mention in a piece: **Waylode** (also called **"Whale"**) | Lead with "Whale" or write "Whale (Waylode)" |
| Use "Whale" in speech, video narration, talks, podcasts, casual replies | Put "Whale" in the logo, app name, bundle id, domain, binary or package names |
| Write "say *Whale*" as a pronunciation aid | Draw a whale mascot (proposed; owner to confirm. Docker's Moby is the whale developers already know) |
| Use feature nicknames **after** the canonical name: "DevMap code graph (*Whale Sonic*)" | Use a feature nickname alone, without the canonical name, in docs |
| Keep nicknames *italic* in docs | Treat a nickname as a trademark claim (™) |

**Known "Whale" names in software, and why the nickname stays spoken only:**
- Naver's **Whale** web browser.
- Docker's whale logo.
- **Whale**, a Singapore workplace-software company (meetwhale.ai / whale.sg) whose products include **Echo** and **Harbor**.
- A **Whale Echo** AI note-taker app, sold by Whale Tech Pte. Ltd. (possibly the same company; not established).

None of these stops a spoken nickname. All of them are reasons never to promote "Whale" to a product name.

**Pronunciation line** for the website footer, video description and README:

> Waylode is pronounced *WAY-lode*. It is also called **Whale**.

---

## 2. The pod: Whale feature names

Every name passes one test: **a listener can guess the feature from what the whale does.** Each name stands for a
canonical feature that already exists. It never stands for a new product.

### Core three (already in the narration, README and launch kit: keep as is)

| Nickname | Canonical feature | Why the whale fits | One-line pitch |
|---|---|---|---|
| *Whale Sonic* | **DevMap code graph**: navigation (search, explore, trace, neighbours), blast radius, affected tests, regression suspects, dead symbols | Whales find their way by sound. Sonic is echolocation for your codebase. | "Agents see what a change touches before they make it." |
| *Whale Song* | **A2A Peer Bus**, plus the collision radar and file claims | Whales talk to each other across distance. | "Agents from different vendors talk to each other instead of overwriting each other." |
| *Whale Pod* | **Fleet & Agents tree**, with live detection of sessions Waylode did not launch | A pod is the group that travels together. | "Every agent, every worktree, every repository, in one view." |

### Proposed additions (owner to approve or veto)

| Nickname | Canonical feature | Why the whale fits | One-line pitch |
|---|---|---|---|
| *Whale Tide* | **Host-Load Governor**: memory-pressure launch admission, build slot leases, idle-session parking | The tide decides how much water comes in. | "Work comes in at the rate your laptop can take." |
| *Whale Migration* | **Land Queue merge train**: AIMD batching, `git replay` stack restacking, ODB conflict proposals | Whales migrate in groups and arrive together. | "Finished branches travel together and land in main as one verified batch." |
| *Whale Baleen* | **Manvi guards**: 8-status command gate, desktop-only repository trust, secret gate, Signal Guard, undo snapshots | Baleen filters what the whale takes in. Water passes, the harmful parts stay out. | "Agents' commands are filtered before they run." |
| *Whale Bubble Net* | **Task board**: agents file follow-up tasks; `done` waits for passed verification | Humpbacks hunt together by blowing a ring of bubbles that herds the catch into one place. | "People and agents herd the work onto one board, and nothing is done until it is verified." |
| *Whale Wake* (optional) | **Ledger, provenance and turn checkpoints** | A wake is the trail a whale leaves behind. | "The trail every agent leaves: what changed, why, and how to go back." |

### Considered and rejected

| Name | Why not |
|---|---|
| *Whale Echo* (collision radar) | Already a shipping product name (the Whale Echo AI note-taker), and a "Whale" company sells an "Echo" product. The radar stays under *Whale Song*. |
| *Whale Sonar* | SonarQube / SonarSource own "Sonar" for developers. |
| *Whale Splash* (blast radius) | Splits *Whale Sonic*. Blast radius stays under Sonic, as the launch copy already says. |
| *Whale Breach* | "Breach" reads as a security incident, in a product that sells guards. |

### Names to screen before adopting a proposed one

- *Whale Tide*: **Tide** is an older Rust web framework (`http-rs/tide`). The risk is low for a spoken nickname, but say "Whale Tide", never "Tide" alone.
- *Whale Migration*: developers also say "migration" for schema changes. Always pair it with "merge train" on first use.
- *Whale Baleen*: small text-processing projects use "Baleen" (`dstl/baleen`). No developer-tool conflict found.

**Rule of use.** The core three can appear in headlines. The proposed names appear only on feature pages, in
narration and in talks, once per piece, in parentheses after the canonical name. Never put more than three Whale
names in one paragraph.

---

## 3. What Waylode does that the others do not (re-checked 2026-11-28)

The 2026-11-27 copy said several capabilities were "not found in the 18 products we checked". The 2026-11-28 pass
added the code-index tools, coordination tools and merge queues listed in §5, and several of those claims no longer
hold as worded. Here is what still holds, worded so it stays true.

### Claims that hold

1. **One code graph shared by the worktrees Waylode creates for agents, with each worktree's changes layered on top** (*Whale Sonic*).
   - **How it works:** a sealed, read-only base index is shared through `stack_lease.json`. Each worktree adds a lazy Tree-sitter overlay of everything that differs from the base: branch commits plus uncommitted and untracked files ([worktree_overlay.rs:8](../../src-tauri/src/codeintel/worktree_overlay.rs)).
   - **Scope:** this covers agent-launched worktrees and Claude Code's worktree-create hook. Worktrees made in the Worktrees panel or by `waylode_worktree_create` get their own full copy unless zero-copy is turned on.
   - **Closest found:** codebase-memory-mcp maps a worktree's uncommitted changes, but indexes linked worktrees as *separate projects* with no shared base. CodeGraph treats a worktree that resolves the main checkout's index as a hazard and warns about it. No vendor ADE documents index sharing across worktrees.
   - **Say:** "We found no other tool that layers each agent's uncommitted edits over one shared code index."
2. **Collision warnings that look past file names** (*Whale Song*). Layer 2 checks 1-hop callers and callees in the call graph; see the tests `radar_detects_cross_file_1hop_caller_callee_collision_over_zero_copy_csr` and `radar_is_strictly_read_only_and_detects_live_1hop_csr_without_writing_overlays` in `src-tauri/src/engine/agent_bus.rs`. Layer 3 runs a quarantined `git merge-tree`. Layer 1 is file and symbol claims.
   - **Closest found:**
     - MCP Agent Mail: advisory glob reservations and a pre-commit hook. It matches file paths only.
     - Claude Code Agent Teams: the docs say two teammates editing one file overwrite each other.
     - Sculptor: flags merge conflicts but does not say how.
   - **How agents use it:** they run the radar on demand (`waylode_worktree_radar`). Layer 3 sees committed work only. The `PreToolUse` hook does not call the radar. It asks before an edit to a file another worktree has uncommitted changes to, and claims held in other worktrees are notices unless enforce mode is on in a shared checkout.
   - **Say:** "The only collision check we found that looks at the call graph, not just the file name."
3. **Agent work is not `done` until it is verified, built in** (*Whale Bubble Net*).
   - **How it works:** an agent's `done` is held in Review until verification passes on the worktree's current `HEAD` ([intake.rs:1966](../../src-tauri/src/workbench/intake.rs)). `waylode_add_task` is duplicate-gated and revision-checked.
   - **Closest found:**
     - Agents filing tasks is **not** unique. The Vibe Kanban MCP server gives agents tools for issues and workspaces, and Beads is an issue tracker built for agents.
     - Claude Code's `TaskCompleted` hook can block completion if you write the hook yourself.
     - Vibe Kanban's maker, bloop, shut down on 2026-04-10, and the project is now community-maintained (its notice).
   - **Say:** "Agents add to the same board you work from, and they cannot mark work done on an unverified commit."
4. **Local batch landing for agent branches, before anything is pushed** (*Whale Migration*).
   - **How it works:** AIMD-sized batches are trial-merged through `git merge-tree --stdin` in an object quarantine ([land_queue.rs:1466](../../src-tauri/src/engine/land_queue.rs)). Each branch is verified before it is submitted; **the combined batch is not re-tested** (the scratch step passes a no-op verifier and is off by default). The batch-landing UI and MCP tools are in the working tree, not yet on `main`. This also covers plus `git replay` stack restacking and ODB conflict proposals ([land_proposals.rs:1419](../../src-tauri/src/engine/land_proposals.rs)). It is exposed as `waylode-worktree land-batch` and the desktop command `cmd_worktree_merge_batch`.
   - **Closest found:**
     - Hosted merge queues already batch PRs **and test the combined batch** on the server after push: GitHub's merge queue, Graphite's stack-aware queue, and Mergify. Waylode does not re-test the batch, so it complements them rather than replacing them.
     - Copilot Agent HQ offers one-click conflict resolution.
     - None of the agent workspaces checked lands agent worktrees locally.
   - **Say (once it ships):** "Land ten agent branches on your machine, trial-merged together, before you push."
5. **Admission control before launch, not cleanup after** (*Whale Tide*).
   - **How it works:** under critical memory pressure Waylode refuses to start an agent, says why, and offers a human-only **Admit anyway**. Heavy builds queue for slots.
   - **Closest found:** Claude Code's supervisor stops *idle* sessions when memory stays low. That reacts after the fact. Codex and Jules use fixed thread or plan caps.
   - **Say:** "Waylode decides whether your laptop can take another agent before it starts one."
6. **Detecting agents it did not launch is shared, not unique** (*Whale Pod*). This moved out of "claims that hold" in the second audit.
   - **How it works:** Claude Code, Codex, Antigravity, DevCouncil and Manvi sessions are found by process scan and registry.
   - **Also does it:** GitKraken Desktop's Agents view (help page dated November 2026) shows sessions started outside it, for Claude Code, Codex CLI, Copilot CLI, Cursor CLI, Gemini CLI and OpenCode. Claude Code lists its own sessions that bind an inbox socket (not bare-mode sessions).
   - **Say (as a feature, not a first):** "Waylode sees the agents you started in a terminal, not just the ones it launched."
7. **Regression suspects and confidence-tiered dead code** (*Whale Sonic*). These are a backward call walk plus line blame ([suspects.rs:193](../../src-tauri/src/codeintel/suspects.rs)), and dead symbols at 0.9 (act on it) versus 0.4 (unconfirmed). No "which recent change likely caused this failure" feature was found in any product checked.

### Claims to drop or narrow

| Old claim (2026-11-27 copy) | What we found | Replace with |
|---|---|---|
| A cross-vendor A2A bus was "not found in the 18 products" | **MCP Agent Mail** (MIT licence with a rider restricting OpenAI and Anthropic) offers a mailbox across Claude Code, Codex CLI, Gemini CLI and Copilot CLI | "Cross-vendor messaging **built into the workspace that also sees the sessions, worktrees and tasks**" — a feature, not a first |
| AST blast radius with affected tests was "not found" | **codebase-memory-mcp** (`detect_changes`; experimental C/C++ test selection) and **CodeGraph** (`codegraph_impact` as an opt-in MCP tool; `codegraph affected` on the CLI) cover parts of this locally | "No major ADE we checked documents blast radius plus affected tests for its agents (Cursor, Windsurf, Copilot, Claude Code, Kiro, JetBrains, Sourcegraph). Open-source MCP servers do. Waylode builds it into the workspace, over a stacked index." |
| A "3-layer cross-agent file collision warning" was "not found" | Agent Mail has file reservations plus a commit hook | Claim 2 above: the call-graph layer is what we did not find |
| Claude Code: load-aware scheduling ❌ | Its supervisor stops idle sessions when memory stays low | ◐ "reactive: stops idle sessions under low memory" |
| Claude Code: unlaunched detection ◐ "background sessions only" | It lists live local Claude Code sessions that bind an inbox socket, so not bare-mode sessions (v2.1.224+; v2.1.234+ on native Windows) | ◐ "its own sessions only (Claude Code)" |
| Waylode is unusual in detecting agents it did not launch | GitKraken Desktop shows outside-started sessions for six vendors | Claim 6 above: a feature, not a first |
| Land queue "verified in a CoW scratch checkout" | The scratch step passes a no-op verifier and is off by default (`land_queue.rs` ~664 and ~891 at `HEAD`); the batch UI is not on `main` | Claim 4 above: branches verified before submit, batch not re-tested, coming in 0.2.0 |
| `PreToolUse` hook "enforces" the 3-layer radar | The hook does not call the radar; it asks on same-file uncommitted overlap, and claims are notices unless enforce mode is on | Claim 2 above: radar on demand, hook asks |
| Governor "admits, queues or sheds" launches, "on memory pressure only" | Launches are refused (not queued) under memory pressure or any input set to refuse; only cargo/go builds queue | Whale Tide card and claim 5 wording |
| "10 worktrees cost 1.8 MB"; "Run 8 agents"; "Six builds" | One manual measurement of one cloned `target`; no source for 8 or 6 | Scoped the 1.8 MB figure; dropped the counts |
| Conductor Cloud "shut down `2026-11-20`" | Conductor's v0.94.2 changelog (`2026-11-13`) says it isn't shutting down; the farewell page is gone | Removed from every doc; the migration guide no longer rests on a shutdown |
| Claude Code: A2A ✅ "Agent Teams mailbox (Claude-only)" | Also cross-session messaging between your own Claude Code sessions, including other machines and cloud | ✅ "Claude Code sessions only" |
| Agents filing follow-up tasks was "not found" | The Vibe Kanban MCP server has issue tools; Beads is an agent-first issue tracker | Claim 3 above: verification-gated `done`, built in |
| Merge train "not found in the 18 products" | GitHub, Graphite and Mergify run hosted merge queues for PRs | Claim 4 above: local, pre-push batch landing of agent worktrees |
| "Whale Song" read as the A2A protocol | A2A is an open Linux Foundation protocol for agent-to-agent delegation | Add once: "Whale Song is a local peer bus between agents on your machine. It does not implement the A2A protocol." |

**Applied on 2026-11-28** in the `main` checkout's working copy, not committed: `README.md`, `wiki/Home.md`,
`docs/WHY_WAYLODE.md`, `docs/launch/LAUNCH_PLAN.md`, and `docs/launch/SHOW_HN_AND_ESSAY.md`, which also gained FAQ
entries 8–9 on Agent Mail and code-graph MCP servers. `MIGRATING_FROM_CONDUCTOR.md` had no "not found" claims.
The list below is kept so the a2a-enhance branch's copies can be brought in line when they merge.

**Where these sentences lived.** Line numbers are from the `main` checkout's working copy on
2026-11-28. The `train/2026-11-28-primary` worktree has a different copy of `README.md`, `docs/WHY_WAYLODE.md` and
`docs/FEATURES.md`, and the a2a-enhance session holds claims on `README.md`, `WHY_WAYLODE.md`, `FEATURES.md` and
`LAUNCH_PLAN.md`. So search for the phrase rather than trusting the line numbers:
- `README.md`, lines 45–49
- `docs/WHY_WAYLODE.md`, lines 38 and 52–56, line 109, and the Claude Code column
- `docs/launch/LAUNCH_PLAN.md`, lines 77 and 134
- `wiki/Home.md`, lines 27–31
- `docs/launch/SHOW_HN_AND_ESSAY.md` and `docs/launch/MIGRATING_FROM_CONDUCTOR.md`, wherever they repeat "not found" or "Claude-only"

Replace "the 18 products we checked" with "the products listed in the Whale field guide (checked 2026-11-28)".

---

## 4. Copy kit

**Tagline options**
- *Waylode — call it Whale. One workspace for every agent you run.*
- *Your agents, in one pod.*
- *Sonic sees the code. Song lets agents talk. Pod shows who is where.*

**Short pitch**
> Waylode (also called "Whale") is a free, local-first workspace for running many coding agents at once. It maps
> the code, warns before agents collide, keeps your laptop responsive and lands the work.

**Long pitch**
> Waylode, also called Whale, is the open-source command centre for parallel coding agents. Its code graph
> (*Whale Sonic*) shows every agent the blast radius of an edit across all your worktrees. Its peer bus
> (*Whale Song*) lets Claude Code, Codex and others message each other, and it warns before their edits meet.
> Then it lands their branches together, verified, while the load governor keeps the laptop responsive.

**Feature cards (website)**

| Card | Headline | Body | Proof |
|---|---|---|---|
| *Whale Sonic* · DevMap | Know what breaks before you edit | One code graph shared by the worktrees Waylode creates for agents, with each worktree's changes layered on top. Blast radius, affected tests, regression suspects. | `devmap_impact`, `waylode_codeintel_suspects` |
| *Whale Song* · A2A Peer Bus | Agents that talk, across vendors | Agents from different vendors message each other and claim files. Agents can ask the radar whether their edits meet another agent's in the same file or in the call graph, and a hook asks before an edit to a file another worktree has changed, where Waylode's hooks are registered (not Antigravity). | `waylode_agent_*`, `waylode_worktree_radar` |
| *Whale Pod* · Fleet & Agents | See every agent, even ones you didn't start | Live sessions from every vendor, nested by parent, beside a grid of every repository. | `waylode_sessions`, `⌘⇧A`, `⌘⇧F` |
| *Whale Tide* · Load Governor | Parallel builds, no beachball | Waylode refuses to start another agent while memory is under pressure (you can admit it anyway), and queues heavy cargo/go builds for a slot. | **Admit anyway** in Health |
| *Whale Migration* · Land Queue | Land ten branches, not one at a time | Batches of finished, already-verified branches are trial-merged together in Git's object store and landed, all before you push (coming in 0.2.0). | `waylode-worktree land-batch` |
| *Whale Baleen* · Manvi guards | Filter what agents run | Where Waylode's hooks are registered, every agent command gets one of eight verdicts. Repository trust is granted only from the desktop, and commits Waylode makes are checked for secrets. | `refs/gitpulse/ops/` undo snapshots |
| *Whale Bubble Net* · Task board | Agents add to your board | Agents file follow-up tasks you can launch from a card. "Done" waits for verification on the current commit. | `waylode_add_task`, `waylode_complete_task` |

**Narration line (video)**
> "Waylode — also called Whale — gives your agents three senses: Sonic to see the code, Song to talk to each
> other, and Pod so you can see all of them at once."

**Honesty lines to keep next to any claim**
- Tested on macOS only. Windows and Linux builds are produced by CI but have not been tested.
- "Not found" means not found in the pages we read on the date given, not "no other product has it".
- Waylode is not an editor, and in this release it has no cloud agents.

---

## 5. Research record (2026-11-28)

Legend: ✅ stated by an official source · ◐ partial · ❌ the source says no · **?** not found in the pages read
(a **?** is not a "no").

### Code index and code graph

| Product | Index kind | Blast radius | Affected tests | Shared across worktrees | Exposed to other vendors' agents |
|---|---|---|---|---|---|
| Windsurf / Devin Desktop (Codemaps) | ◐ embeddings index; Codemaps are agent-written flow maps | ? | ? | ? | ? |
| Augment Context Engine | ◐ semantic search | ? | ? | ? (remote indexes default branches) | ✅ MCP, GA 2026-02-06 |
| Cursor | ◐ Instant Grep + older embedding index | ❌ not described | ? | ? | ? |
| Sourcegraph MCP / Amp | ✅ SCIP | ◐ single-hop `find_references` | ? | ? | ✅ MCP |
| Claude Code | ◐ LSP via plugins | ◐ references, call hierarchy | ? | ? | n/a |
| JetBrains MCP / Junie / Air | ✅ IDE PSI index | ◐ `analyze_calls` | ◐ run configurations | ? | ✅ MCP |
| Kiro | ✅ Tree-sitter + optional LSP | ❌ no call hierarchy | ? | ? | ? |
| GitHub Copilot (VS Code) | ✅ semantic index + Usages | ◐ Usages only | ? | ? | ? |
| Serena | ✅ LSP | ◐ referencing symbols | ❌ | ? | ✅ MCP |
| Greptile | ✅ graph | ✅ review-time impact | ◐ | ? | ◐ review-focused MCP |
| codebase-memory-mcp | ✅ Tree-sitter graph + own type resolution | ✅ `detect_changes` | ◐ `scope: "tests"` (experimental, C/C++ harnesses) | ◐ worktrees indexed as separate projects | ✅ MCP |
| CodeGraph (colbymchenry) | ✅ Tree-sitter, SQLite | ✅ `codegraph_impact` (opt-in MCP tool) | ◐ `codegraph affected` (CLI, import-based) | ❌ warns when a worktree borrows the main checkout's index | ✅ MCP |
| Zed, Google Antigravity | ? | ? | ? | ? | ? |

### Multi-agent coordination

| Product | Agent ↔ agent messages | Claims / collision warning | Sees sessions it did not launch | Board; agents add tasks | Merge queue | Load-aware |
|---|---|---|---|---|---|---|
| MCP Agent Mail | ✅ cross-vendor (Claude Code, Codex, Gemini, Copilot CLI) | ◐ advisory glob leases + commit hook (paths only) | ◐ registered agents | ◐ pairs with Beads | ? | ? |
| Claude Code | ✅ Claude Code sessions only | ❌ task-claim lock only; docs warn same-file edits overwrite | ✅ its own sessions (not bare mode) | ◐ task list, no board | ? | ◐ stops idle sessions under low memory |
| Google Antigravity | ✅ Google agents only | ❌ none described | ? | ◐ `/tasks` is shell work | ? | ? |
| Warp (Oz) | ✅ parent/child | ? | ? | ? | ? | ? |
| OpenAI Codex | ❌ subagents report to parent | ❌ docs warn of conflicts | ? | ? | ? | ◐ thread cap |
| GitHub Copilot Agent HQ | ? | ◐ one-click conflict resolution | ◐ across GitHub surfaces | ◐ assign to several vendors | ? | ? |
| Windsurf 2.0 | ? | ? | ? | ◐ Agent Command Center (human-facing) | ? | ? |
| Vibe Kanban | ? | ? | ? | ✅ board; MCP server with issue tools; community-maintained since 2026-04-10 | ? | ? |
| Beads | ? | ? | ? | ◐ agent-first issue tracker, no board found | ? | ? |
| GitHub merge queue, Graphite, Mergify | n/a | n/a | n/a | n/a | ✅ hosted, for PRs after push (Graphite and Mergify stack-aware) | n/a |
| Superset / Paradev | ◐ via CLI/MCP | ❌ docs: changes can still conflict | ? | ◐ Linear intake | ? | ? |
| Sculptor | ? | ◐ flags merge conflicts (method not stated) | ? | ? | ? | ? |
| GitKraken Desktop | ? | ◐ branch/worktree name checks only | ✅ outside sessions, 6 vendors (Nov 2026) | ◐ Launchpad | ? | ? |
| T3 Code | ◐ outside agents message threads over MCP | ? | ? | ◐ thread sidebar | ? | ? |
| Emdash, Nimbalyst, Claude Squad, container-use, Factory, Devin, Kilo Code, Conductor | ? or isolation only | ? | ◐ Emdash: installed CLIs only | ◐ intake from trackers | ? | ? |

**Not covered:** primary docs for Jules and Roo; Antigravity's and Zed's code-index docs; the Cursor 3.x changelog
entries (read from search summaries only). Terragon shut down on 2026-02-09, and Crystal was deprecated in favour of
Nimbalyst in February 2026.

**A2A, the protocol.** A2A is an open Linux Foundation protocol (Apache-2.0, spec v1.0) for agents to hand work to
each other. Its site says MCP connects an agent to tools, and A2A connects agents to other agents. Waylode's
"A2A Peer Bus" is a local feature with that purpose. It is not an implementation of the protocol.

### Sources

- **Code index:**
  - [Devin Desktop Codemaps](https://docs.devin.ai/desktop/codemaps), [Windsurf local indexing](https://docs.windsurf.com/context-awareness/local-indexing)
  - [Augment Context Engine MCP](https://docs.augmentcode.com/context-services/mcp/overview) ([GA changelog](https://www.augmentcode.com/changelog/context-engine-mcp-in-ga))
  - [Cursor indexing](https://cursor.com/docs/context/codebase-indexing), [Cursor worktrees](https://cursor.com/docs/configuration/worktrees)
  - [Sourcegraph MCP](https://sourcegraph.com/mcp)
  - [Claude Code tools](https://code.claude.com/docs/en/tools-reference), [Claude Code code intelligence](https://code.claude.com/docs/en/plugins/code-intelligence)
  - [JetBrains MCP server](https://www.jetbrains.com/help/idea/mcp-server.html), [Kiro code intelligence](https://kiro.dev/docs/tools/code-intelligence/), [VS Code workspace context](https://code.visualstudio.com/docs/copilot/reference/workspace-context)
  - [Serena](https://github.com/oraios/serena), [Greptile graph](https://www.greptile.com/docs/how-greptile-works/graph-based-codebase-context), [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp), [CodeGraph](https://github.com/colbymchenry/codegraph)
- **Coordination:**
  - [MCP Agent Mail (Rust)](https://github.com/Dicklesworthstone/mcp_agent_mail_rust)
  - Claude Code: [cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging), [agent view](https://code.claude.com/docs/en/agent-view), [agent teams](https://code.claude.com/docs/en/agent-teams), [hooks](https://code.claude.com/docs/en/hooks)
  - [Antigravity subagents](https://antigravity.google/docs/subagents), [Warp multi-agent runs](https://docs.warp.dev/agent-platform/cloud-agents/orchestration/multi-agent-runs/), [Codex subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents), [GitHub Agent HQ](https://github.blog/news-insights/company-news/welcome-home-agents/)
  - [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) ([MCP server](https://vibekanban.com/docs/integrations/vibe-kanban-mcp-server), [shutdown notice](https://vibekanban.com/blog/shutdown)), [Beads](https://github.com/gastownhall/beads), [Graphite merge queue](https://graphite.com/docs/graphite-merge-queue), [Mergify stacks and queue](https://mergify.com/blog/stacked-prs-without-a-new-git-workflow), [Superset](https://github.com/superset-sh/superset), [Emdash](https://github.com/generalaction/emdash), [Nimbalyst](https://github.com/Nimbalyst/nimbalyst), [Sculptor](https://imbue.com/sculptor/), [Claude Squad](https://github.com/smtg-ai/claude-squad), [container-use](https://github.com/dagger/container-use)
  - [A2A protocol](https://a2a-protocol.org/latest/)
- **Name screening:** web searches on 2026-11-28 for "Whale Echo", "Whale Tide", "Whale Wake", "Whale Sonic" and "Baleen".

Verified first-hand on 2026-11-28 (page opened, quoted):
- Agent Mail's vendor list, glob reservations and commit hook
- Claude Code cross-session messaging (v2.1.224+, Claude Code sessions only)
- Claude Code agent view's low-memory supervisor
- codebase-memory-mcp's `detect_changes`, test scope and worktree handling
- CodeGraph's impact and affected tests
- Vibe Kanban's sunsetting and the bloop shutdown notice (2026-04-10)
- Second audit: Conductor's v0.94.2 "isn't shutting down" entry, T3 Code's multi-repo projects doc, GitKraken's outside-session rows (6 vendors), and CodeGraph's borrowed-index warning (`src/sync/worktree.ts`)

Second-audit corrections taken from an auditor's reading, not reopened by hand: Agent Mail's licence rider, codebase-memory-mcp's experimental C/C++ test selection, CodeGraph's opt-in MCP impact tool, Claude Code's LSP call hierarchy, Tower's seven PR hosts, and the VS Code harness page.

Still unsourced in the comparison tables (flagged, not changed): the Electron runtime cells for Claude Desktop, the Codex app and Conductor, and the "no MCP server mode found" cells for VS Code, Cursor and Conductor.

From search results only, not opened:
- the Vibe Kanban MCP server's tool scope
- the Graphite and Mergify merge queues
- Beads (its page summary was empty; its role as an agent-first tracker comes from its tagline and the Agent Mail docs)

The other cells come from research agents' reading of the linked pages.
