# Running agents in GitPulse

[Documentation index](README.md) · [Getting started](GETTING_STARTED.md) · [Why GitPulse](WHY_GITPULSE.md) · [Tasks and workspaces](TASKS_AND_WORKSPACES.md) · [MCP and Agents (wiki)](../wiki/MCP%20and%20Agents.md)

GitPulse launches coding agents, watches them, guards them and lands their work. It does not replace
the agents: Claude Code, Codex, Antigravity, Grok and Manvi still run as their own CLIs, under their own
permissions. This guide covers the parts that GitPulse adds around them. The tool-by-tool MCP reference
is the wiki page [MCP and Agents](../wiki/MCP%20and%20Agents.md).

Contents: [Connect an agent](#connect-an-agent) · [Launch from the task board](#launch-from-the-task-board) ·
[Guards and trust](#guards-and-trust) · [Agent-to-agent messaging](#agent-to-agent-messaging) ·
[The collision workflow](#the-collision-workflow) · [Agents file follow-up tasks](#agents-file-follow-up-tasks) ·
[Limits](#limits-to-know)

## Connect an agent

Open **Settings → Agents → Connect your agent**. It shows a plan first and changes nothing until you apply
it. On apply it links the app's bundled `gitpulse-mcp` and `gitpulse-hook` into `~/.local/bin` and registers
the `gitpulse` MCP server with Claude Code and Codex through their own `mcp add` commands. If the GitPulse
Claude Code plugin is installed, it already provides the server and the hooks, so use the plugin or the
direct registration, not both.

The MCP server gives an agent the same facts the UI shows you: `gitpulse_insights` for worktrees, live
sessions, collisions, ledger and code-graph health, plus `gitpulse_sessions`, `gitpulse_collision_risk`,
`gitpulse_change_context`, the task-board tools, the `devmap_*` code-graph tools and the worktree
coordination tools. The server exposes 66 tools and 4 prompts. The hooks are what enforce the guards, so a
missing `gitpulse-hook` leaves them off while sessions look normal. `bun run mcp:doctor` (source checkouts)
and the Settings card both report that.

## Launch from the task board

The board is where you start agents. In **Work → Tasks**, or the global **Tasks** board, choose
**Send to agent** on a card. The form asks for:

- **Agent**: Claude, Codex, Grok or Antigravity, and how it runs: **Terminal** (a task-bound session in the
  dock) or **Managed** (Manvi supervises the run and shows its requests in GitPulse; the form says which
  agents support it). Shell sessions are for your own use.
- **Working checkout**: an open repository tab, or the folder beside that repository's git directory.
- **Permission mode**: remembered for next time, except `Bypass permissions`, which you must choose again,
  with its acknowledgement, for every attempt.
- **Model, effort and advisor for this launch** (terminal handoffs): Claude Code takes model, effort and
  advisor; Antigravity takes effort. A blank field keeps the default from **Settings → Agents**. The
  values are recorded on the attempt and are not remembered for the next launch. A Manvi managed attempt
  takes none of these, because Manvi sets its model.
- **Run in a new worktree**: GitPulse creates `.gitpulse/worktrees/<task>-<id>` on a new
  `gitpulse/<task>-<id>` branch, hidden from `git status` through `.git/info/exclude`. When the selected
  checkout already holds an agent, the form says which task holds it and selects a new worktree for you.

Each launch is one tracked attempt. A terminal attempt opens a task-bound session in the terminal dock;
finishing the process does not accept the task. The agent's brief names the task on its `Task:` line, so
the agent can read it back with `gitpulse_get_task`.

**Done means verified.** When the agent calls `gitpulse_complete_task` with `done`, the card moves to Done
only if Manvi's verification passed for the head the agent's worktree is still on. Otherwise the card
moves to Review with the reason (`held_for_review`), and no argument changes that. An agent never reopens
a card that is already Done.

You can run several attempts at once. The limit is **Settings → Agents → Agents running at once**, from 1
to 64 (8 until you change it); terminal sessions are capped separately, 32 across repositories by default.
When a launch is refused or a worktree is not needed after all, GitPulse removes the worktree and branch it
created for the attempt. An attempt holds its checkout until its process is known to have ended.

You can also start sessions directly from the dock, without a task: the launchers are Shell, Claude,
Manvi, Codex, Grok and Antigravity, plus a Browser tab for local dev servers.

![The Agents view: every agent session with its checkout, presence and what needs your attention](assets/agents-tree.webp)

## Guards and trust

**Per-repository trust.** Opening a checkout asks for **Trust and Open**. Trust is per repository (bound to
its common Git directory), is stored outside the repository, and can be revoked from the tab. Only the
desktop can grant it: the MCP server refuses writes in an untrusted repository (`untrusted_repository`)
and cannot grant trust itself. Trust permits the repository's hooks and project tools to run under your
account. It is an execution decision, not an OS sandbox.

![The Trust this repository? prompt, shown when a new checkout is opened](assets/trust-prompt.webp)

**The Manvi command gate.** Where the plugin's hooks are registered, each agent shell command passes
`gitpulse-hook command-gate` before it runs, and each file write passes `collision-guard`. The gate is
Manvi's policy engine. It returns one of three actions (`allow`, `warn`, `deny`), and GitPulse renders every
decision as one of eight statuses, defined in [`contracts/verdict.schema.json`](../contracts/verdict.schema.json):

| Status | Meaning |
| --- | --- |
| Allowed | The rules passed with nothing overriding them (`clean` in the contract) |
| Warned | Proceeds with an advisory recorded |
| Demoted | A posture or gate mode downgraded a denial to an allow |
| Granted | An override cleared a soft block |
| Widened | The write was allowed by scope the executor appended to its own task |
| Degraded | A check could not run, so the decision is not the one a full check would reach |
| Blocked | Refused (`denied` in the contract); the reason names the rule |
| Unchecked | No decision exists, for example the harness is missing. Never rendered as allowed |

Each rule has a severity. Hard rules (for example force push, a protected reset, a path outside the root,
a secret path, or command substitution and heredocs inside a gated command) cannot be cleared by any grant.
Soft rules (task-scope and lease rules) can be demoted by posture or cleared by a grant. When a session is bound to a task scope, writes into
another checkout of the same repository are refused too. Hooks never answer `allow` on their own, so they
cannot override your own permission rules, and a hook failure never blocks a tool call.

**Other guards.** The secret gate (`dc-redact`) scans commits and pushes that GitPulse makes; it does not
catch an agent's own `git commit`. Allowed agent Git mutations record pre-operation snapshots under
`refs/gitpulse/ops/`, which you can restore with a checked undo from HEAD moves. Opt-in turn checkpoints
under `refs/gitpulse/turns/` capture the working tree at a turn's start and end. A signal guard stops
agents from killing GitPulse.

![A commit from GitPulse's composer blocked by the secret scan: nothing was committed, and the finding names the file and line](assets/guard-secret-blocked.webp)

## Agent-to-agent messaging

Agents on different worktrees can talk to each other instead of overwriting each other's work. The peer
inbox tools are:

| Tool | Use |
| --- | --- |
| `gitpulse_agent_register` | Register a live session (Claude, Antigravity or Codex) with a role and a wake mode (`boundary` or `auto`) |
| `gitpulse_agent_peers` | List live and resumable peers across the repository's worktrees |
| `gitpulse_agent_send` | Send a message to one peer, up to 16 KiB. An ambiguous target is refused rather than guessed |
| `gitpulse_agent_inbox` | Read unread messages, deduplicated; acknowledging them is optional |
| `gitpulse_agent_wait` | Block for up to 45 seconds for a reply, with at most 4 waiters at a time per server process |

A session also sees unread peer messages when it starts and after tool calls, through the `peer-inbox`
hook. Separately, the `gitpulse_worktree_*` family carries lane-level handoff messages
(`gitpulse_worktree_send_message`, `gitpulse_worktree_inbox`), for example an interface-change notice to
every sibling lane. You can send an agent a message yourself with **Send to agent** on a live session;
GitPulse shows a preview and waits for your confirmation before delivering it.

The bus keeps the newest 256 messages. When it is full, the oldest message goes first, except that open
asks and each recipient's unread direct messages from the last 2 hours (up to 8 per recipient, 64 in
all) are kept for as long as anything else can go instead. A body may hold line breaks and tabs but no
other control characters, and the 16 KiB limit counts the body as stored, where a quote or backslash
takes two bytes. If the bus file ever outgrows its 8 MiB limit, the next post or acknowledgement drops
the oldest rows to bring it back under, logs a warning, and never reuses a sequence number.

![The Agents inbox with an overlap notice from one agent session to another](assets/agent-inbox.webp)

## The collision workflow

Two agents editing one file in different worktrees is the case GitPulse warns about before the merge does.

1. **Look.** `gitpulse_insights` shows worktrees, live sessions and overlapping files. In the UI, Work →
   Overview and the Agents plane (`⌘⇧A`) show the worktrees, terminals and task attempts.
2. **Radar.** `gitpulse_worktree_radar` combines three layers: file and symbol claims, one-hop call-graph
   neighbours from DevMap across dirty worktrees, and a quarantined `git merge-tree` over committed work.
   `gitpulse_collision_risk` lists files dirty in more than one worktree; pass `path` to ask about one file
   before the first edit, because a clean worktree sees no overlap on its own.
3. **Claim.** Before editing in a parallel lane, an agent calls `gitpulse_worktree_claim` with the files
   and symbols it intends to change, checks `gitpulse_worktree_inbox` for notices, and calls
   `gitpulse_worktree_release` when done. A card's planned files are claimed when its run starts.
4. **Guard.** The `collision-guard` hook asks the user when another worktree holds uncommitted changes to the
   file an agent is about to write, and refuses writes outside the bound task scope.
5. **Message.** The agent that finds the overlap messages the other through the peer inbox instead of
   proceeding.
6. **Predict the merge.** `gitpulse_merge_conflicts` reports, pair by pair, whether the worktrees' committed
   work would merge, running in an object quarantine that is deleted afterwards.

An empty overlap list is not proof of no collision when `unscanned_worktrees` is non-zero or `sessions_ok`
is false: check each facet's `ok` field. Two sessions in the same checkout never show as an overlap,
because there is only one worktree to compare.

## Agents file follow-up tasks

An agent that finds work outside its task should not bury it in a transcript. It files it on your board
with `gitpulse_add_task`, and the card appears in Backlog while GitPulse's window is on screen (and when the
window is next shown otherwise).

- The board refuses a new card that looks like open work (two shared title words, or one and a shared
  label) until the agent has reviewed the related cards. It then folds its work into one of them, or names
  every one to say its task is separate. Done cards never count.
- Edits use `expected_revision`, so a change you made after the agent read the card is refused rather than
  replaced. Fields you locked stay as you set them.
- `gitpulse_import_tasks` puts a repository's `tasks/*.md` briefs on the board. It is not duplicate-checked.
- `gitpulse_merge_tasks` and `gitpulse_delete_task` keep the board small. Both require a reason, which is
  kept in the card's history, and an agent cannot delete or merge away a card another agent is running.
  MCP has no undelete; a deleted card is recoverable from the archive's Deleted view.
- Writes require a trusted repository and never start an agent run.

## Limits to know

- GitPulse is tested on macOS. Windows and Linux builds exist but are not yet tested.
- The hooks exist only where the plugin's hooks are registered. An agent under another profile, or one
  started outside GitPulse, has no gate until that profile registers them.
- Antigravity has no `PreToolUse` hooks from GitPulse, so its shell commands and file writes are not gated
  by the command gate or the collision guard. It keeps the session brief, the peer inbox and notifications.
- A worktree is not an OS sandbox. A process that leaves the agent's process group writes with no tool call
  for a hook to judge.
- Commands whose write targets are not in the command text (`git apply`, `xargs`, `find -exec`) cannot be
  judged against a task scope, so with a scope bound they are refused.
