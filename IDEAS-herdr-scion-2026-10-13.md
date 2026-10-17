# What GitPulse could take from herdr and Scion (2026-10-13)

Sources:
- Anthropic, *Redesigning source control for agent scale* (Scion): https://www.anthropic.com/engineering/redesigning-source-control
- herdr (a terminal runtime for coding agents): https://github.com/herdrdev/herdr, plus its docs on socket-api, add-herdr-support, session-state and concepts.

**How this was checked.** DevMap was degraded this session: the source differs from the indexed generation, 98 files were not fully parsed, and every answer came back `walk_incomplete`. I used `devmap_ask_evidence` to find where things live, then `rg` and source reads to confirm them. A citation below means I read that line. "Absent" means `rg -uu` found nothing in `src-tauri/src`, `src` and `scripts`, with `framework/` excluded. It does not mean the graph confirmed it.

## Already covered (not proposed again)

| Idea from the sources | GitPulse today |
|---|---|
| Scion: a server answers history and provenance questions, so a client doesn't need a checkout | `gitpulse-mcp`: insights, provenance, change_context, ledger, codeintel |
| Scion: persisted derived data, so expensive answers aren't recomputed | `engine/churn_store.rs`, the ref cache, the blame timeline, DevMap |
| herdr: agent states (blocked / working / done / idle) | `src/lib/terminal/sessionActivity.ts:41-60`: needs-you / finished / error / signalled, plus clear. herdr's `blocked` is our `needs-you` and its `done` is our `finished`. There is no separate `working` state, but `clear` ("Working again") does that job. Different shape, not a gap. |
| herdr: detaching leaves agents running | Closing the window hides it (`src-tauri/src/desktop/mod.rs:149-173`), so terminals keep running. Only Quit ends them. |
| herdr: worktree create/open/remove | `engine::worktree`, including setup and teardown that fail closed |
| herdr: notifications when an agent finishes or needs input | `src-tauri/src/alerts/`: PTY scanner, launch flags and hook socket |

## Gaps, ranked by value for the effort

### 1. Collision risk can't see committed work. Predict merge conflicts across agent branches. (High value, medium effort)
- **Scion:** many changes land at once, and conflicts between them are expected. They check after merging and have Claude fix the conflicts.
- **GitPulse:** `gitpulse_collision_risk` only reports files with *uncommitted* changes in more than one worktree. The tool's own description says so (`src-tauri/src/mcp/mod.rs:486`). Once an agent commits on its branch, its files drop out of the scan, and that is the normal end state of an agent lane. **Verified.**
- **Proposal:** for each live agent branch, run `git merge-tree --write-tree <base> <branch>`, and do the same for each pair of branches. Report the real textual conflicts, not just overlapping paths. GitPulse already uses this command: `src-tauri/src/engine/deadbranch.rs:68` calls `merge-tree` to detect squash merges.
- **Design question for you:** `--write-tree` writes objects into the repository's object database. The MCP server promises it "never mutates git state". The options are a temporary `GIT_OBJECT_DIRECTORY` that reads the real one through alternates, or deciding that unreferenced loose objects don't count as git state.

### 2. Resume only works for Claude Code. Let each agent report its own resume command. (High value, small to medium effort)
- **herdr:** each agent reports `resume_argv` through its hook, for example `codex resume <id>` or `cursor-agent --resume <id>`. The rules are a plain command name with no path, at most 64 arguments and 8 KiB, and no quote or control characters. The resume command lives with the agent's integration, not in a table inside herdr. herdr resumes about 18 agents this way.
- **GitPulse:** `src-tauri/src/workbench/conversation.rs:47` refuses with "Only a Claude Code conversation can be resumed". `taskTerminal.ts:346` hard-codes `provider: "claude"`. Meanwhile the hook bridge accepts reports from six agents (`src-tauri/src/alerts/bridge.rs` `AGENTS`). **Verified.**
- **Proposal:** add an optional `resume` argv to the hook report and validate it with herdr's rules. Hand-written Codex support is the first case.
- **Cost:** the report protocol is `v: 1` with at most 12 fields. Adding a field either uses one of the spare slots or means a version bump. The usual count guards also apply.

### 3. Hook reports have no sequence number. (Small fix, likely real)
- **herdr:** each report carries `--seq`, a number that must increase per source, even across restarts. Reports with a number that isn't higher are ignored, so late reports can't overwrite newer state.
- **GitPulse:** `parse_report` (`bridge.rs:470`) has no sequence or timestamp field, and `rg -uu '\bseq\b'` over `src-tauri/src/alerts` finds nothing. Each hook is its own `gitpulse-hook` process, and they connect to the socket concurrently. A late `prompt_submitted` ("Working again") could clear a newer "Waiting for you". **Inferred:** the missing field is verified, but I haven't seen a reordering happen or a test that shows one.
- **Proposal:** the hook stamps `seq` with a monotonic nanosecond clock, and the hub drops anything not newer per (session, agent). This needs a regression test that delivers two reports out of order.

### 4. GitPulse doesn't use git's commit-graph. (Medium value, small effort)
- **Scion:** what made history queries fast was derived data built once per commit: skip lists for ancestry, plus path history and blame indexes. Its closing section asks for these ideas to be applied to git without leaving git.
- **Git's version of this:** the commit-graph file, with generation numbers (fast merge-base and ahead/behind) and changed-path Bloom filters (fast `git log -- <path>` and blame). GitPulse runs these queries often: ahead/behind for about 96 branches, the path filter in `analyzer/filter.rs`, and the blame timeline.
- **GitPulse:** `rg -uu 'commit-graph|changed-paths|commitGraph'` finds nothing. **Verified absent.**
- **Proposal:** add a hygiene check that reports when a repository has no commit-graph, or has one without Bloom filters. Offer one explicit action that runs `git commit-graph write --reachable --changed-paths`. This is a write to `.git`, so it must stay a button the user clicks, never run in the background. Measure the speedup with interleaved A/B runs before claiming any.

### 5. Terminals die when the app quits. Move PTYs into a server process. (High value, large effort: your call)
- **herdr:** a background server owns every terminal, and clients attach to it. Updates can pass live terminals to the new server (experimental). After a real restart, herdr restores the layout and resumes agents by their session id. The processes themselves do not survive a restart of herdr's own server either.
- **GitPulse:** the PTYs live in the app process (`src-tauri/src/terminal/mod.rs` `TerminalSessions`). Quit, a crash or an update ends every agent; `quitGuard.ts` only warns first. `src-tauri/src/bin/gitpulsed.rs` says plainly that it is not a second request surface. So this would be a new process or a change to what gitpulsed is for, not an extension.
- **Cheaper partial step:** once item 2 lands, restore tabs on launch and resume each agent's conversation by session id. That is what herdr does after a restart, and the backend already exists (`resumeTaskConversation`, `incident.ts resumeAll`).

### 6. Let agents wait on, and prompt, other agents over MCP. (Possible, but only on your explicit go-ahead)
- **herdr:** `agent.wait --until done|blocked`, and `agent.prompt` with a built-in wait so there's no race between sending and waiting. `agent.wait` is tied to whichever agent occupies the pane, so a replacement agent can't satisfy someone else's wait.
- **GitPulse:** MCP is read-only apart from the task board, and that is deliberate. Typing into another agent's terminal would be a new write surface that crosses agents. **Security impact:** any MCP client could then drive any GitPulse terminal. A read-only `gitpulse_session_state`, plus a wait that only blocks until another session is blocked or finished, would get most of the coordination value without that risk.

## Considered and not recommended

- **Scion's sparse and virtual checkouts for agent worktrees.** It would help the per-lane disk budget, but most of that budget is build output, not the checkout. Sparse checkouts also break tools that expect the whole tree.
- **Scion's "comment-only vs logic change" approval rules.** These suit a review tool. GitPulse could label hunks with DevMap's parsers, but nothing in the repo does this today, and I didn't size it.
- **Scion's "superfans"** (an agent that watches one component's telemetry). This is an ops pattern for a service fleet, not a desktop app.
- **herdr's plugins, remote machines over SSH, and tmux-style keys.** They're outside GitPulse's scope, and remote machines would need a separate transport design.

No new dependencies are needed for items 1 to 4.

---

# Round 2: Claude Managed Agents and Perplexity Projects

Sources:
- https://platform.claude.com/agent-quickstart. This is only a sign-in page; it has no technical content. The substance comes from the docs it links to:
  - https://platform.claude.com/docs/en/managed-agents/overview
  - https://platform.claude.com/docs/en/managed-agents/multiagent-orchestration
  - https://www.anthropic.com/engineering/managed-agents
- https://www.perplexity.ai/hub/products/projects

**How this was checked.** I used `rg` and source reads, the same method as round 1. DevMap is still degraded (gen 4133, `walk_incomplete`). The round-2 items have not been through the audit's Opus verification.

## Already covered (not proposed again)

| Idea from the sources | GitPulse today |
|---|---|
| Managed Agents: each subagent runs in its own isolated thread | Each agent gets its own worktree, which is stronger isolation. Managed Agents subagents share one filesystem. |
| Managed Agents: thread status events (running, idle with a stop reason, terminated) | `src/lib/terminal/sessionActivity.ts`, the hook bridge |
| Managed Agents: subagent permission requests are cross-posted to the primary thread | `src/lib/agents/plane.ts` gathers needs-you across tabs, and the tray count. *Inferred from file roles; not re-read this round.* |
| Managed Agents: wake a session from its durable log | A Claude attempt can be resumed by its run id (`workbench/terminal_command.rs:1669`) |
| Managed Agents: advisor consultations | Already part of how you route models (Sonnet implements, Opus verifies, Fable advises) |
| Perplexity: project instructions and Skills | `AGENT_GUIDANCE` in every brief, plus repo skills |

## Gaps, ranked

### 7. Integrate lanes without touching the main checkout. (Highest value; fixes a git drawback at its root)
- **The drawback:** `git merge` needs a working tree. GitPulse's teardown therefore merges inside your main checkout:
  - `engine/worktree.rs:1954` runs `merge <branch> -m …`
  - `git_writer.rs:1539-1542` builds the plain, ff-only, no-ff and squash merge commands
- **What goes wrong:** the audit reproduced this with real git (finding S5-01). A conflict leaves `MERGE_HEAD` and `UU` files in your checkout. In squash mode, `merge --abort` cannot recover; only `reset --merge` can. The wave keeps launching after the conflict. Six live Claude sessions share that checkout right now.
- **Managed Agents' lesson:** the sandbox is "cattle", not a "pet". In GitPulse terms, integration should not depend on the one checkout a person is working in.
- **Proposal:** compute the merge in the object database instead of a working tree:
  1. `git merge-tree --write-tree --name-only <target> <branch>`. A conflict is just output; no files change.
  2. `git commit-tree`
  3. `git update-ref refs/heads/<target> <new> <expected-old>`. This compare-and-swap refuses if anything moved the target in the meantime, which also closes S5-05.
  4. Only then, if the main checkout is clean and on `<target>`, bring it up to date with `merge --ff-only`. If it isn't clean, leave it alone and say so.
- **Effects:**
  - Conflicts become a report: "member b conflicts on these paths". They no longer leave a parked checkout.
  - Squash merges become a one-parent `commit-tree`.
  - The same step serves as the pre-merge check S5-02 asks for.
- **Requirements:** git 2.38 or later, which needs the version floor in ft-45b8099a. The app makes the writes, never MCP.
- **Evidence:** the merge sites are verified. The design is a proposal.

### 8. An operation log for agent git writes, with undo. (High value; second git drawback)
- **The drawback:** the reflog is per ref and expires. Nothing records "this agent's last command moved these three refs and changed these files."
- **Inspiration:** jj's operation log, and Managed Agents' append-only session log, which is the source of truth. Neither of those is a process.
- **GitPulse today:** the ledger has a `before_ref` column (`ledger/mod.rs:68`). Only one writer fills it: the branch-delete journal (`git_writer.rs:2379`). **Verified.**
- **Proposal:**
  - The pre-tool hook already sees every gated git write. For those commands it records a snapshot of all refs, plus a working-tree snapshot from `git stash create`. `git stash create` writes a commit without touching the working tree or the stash list.
  - Snapshots are kept under `refs/gitpulse/ops/<n>`, with a size limit and expiry.
  - Undo becomes "restore the refs from snapshot N" in one click, reviewed in the UI, using compare-and-swap `update-ref`.
- **Owner decision:** the hook would write refs. Today it only decides allow or deny.
- **Depends on:** S6-01 (the ledger records ingest time, not event time) should land first.

### 9. Earlier attempts' outcomes in the next attempt's brief. (High value; small to medium effort)
- **Inspiration:** Perplexity's "Brain" learns from earlier sessions between tasks. Managed Agents keeps the whole session log so later turns can reread it.
- **GitPulse today:** a task brief contains only:
  - guidance
  - title
  - repositories
  - description
  - acceptance criteria
  - raw logs
  - checklist
  - linked tasks

  (Source: `src-tauri/vendored/dc-store/src/workbench/briefs.rs:78-209`.) **Verified.** A retry knows nothing about why the last attempt failed.
- **Proposal:** add a "Previous attempts" section with up to N entries. Each entry gives:
  - the outcome
  - the Manvi verify verdict and its failing criteria
  - commands that were refused, and their rule ids
  - the conflict paths from item 7
  - the branch and head

  Everything comes from the run receipts GitPulse already keeps.
- **Ownership:** dc-store owns the brief, so this is a DevCouncil change that GitPulse re-vendors (cross-app rule). The alternative is for GitPulse to append the section after export in `terminal_run.rs`.

### 10. File claims: say what you will edit before editing it. (High value for coordination)
- **GitPulse today:** collision risk only reacts. It sees a file once another worktree has it dirty. The audit also found three blind spots:
  - S3-01: a file dirty in only one *other* worktree is missed.
  - S3-02: committed work is missed.
  - S3-05: lanes sharing one checkout are missed.
- **Proposal:** a bounded, expiring claim that a lane or its brief declares, for example `src/lib/workbench/waveLaunch.ts`. Claims would come from a card's `planned_files`, which the board already has, and from a claim made at the edit hook. Collision checks and Fable-style wave planning would read the claims as well as the dirty files. Two waves that claim the same file would be flagged before either starts.
- **Ownership:** this is the claim state S3-05 already calls for. Claims would live in GitPulse's ledger. The MCP rule would need a narrow claim write, so this is an owner decision.

### 11. Budgets for waves and runs. (Medium value; small effort)
- **Managed Agents:** a session budget caps spend, workflow runs pause at the cap, and a run can carry a time limit.
- **GitPulse today:** `waveLaunch.ts` and `waveHost.ts` have no time or spend limit. Their only timeouts are the worktree prepare and verify windows. **Verified by `rg`.**
- **Proposal:**
  - A per-wave wall-clock limit.
  - An optional per-wave token cap, computed from transcript usage. The ledger already reads transcripts, though Codex is not covered (S6-12).
  - When a cap is reached, the wave pauses: it launches no new batch, and live lanes keep running.
  - The cap appears in WavePanel.

### 12. Fork a lane. (Medium value; small effort)
- **Inspiration:** Perplexity lets you fork a shared thread without inheriting the author's access. The Claude Code CLI can fork a resumed conversation.
- **Proposal:** "Fork this lane" does three things:
  1. Creates a new worktree at the lane's HEAD.
  2. Carries the lane's uncommitted changes across with `stash create` and `stash apply`.
  3. Resumes the same conversation as a new session.
- **Use:** try two approaches from the same point, then merge the winner with item 7.
- **Evidence:** `rg -i 'fork-session|forkSession'` finds nothing. **Verified absent.**
- **Caveat:** I have not checked the current Claude Code docs for the exact name of the CLI's fork flag. Check it before building.

### 13. Optional scheduled repo digest. (Low to medium value)
- **Inspiration:** Perplexity automations that maintain a status document in the background.
- **Proposal:** a daily or on-demand summary:
  - lanes idle for more than N hours
  - branches whose merge-tree against main now conflicts (item 7)
  - cards marked done without a Manvi verdict
  - the ledger's catch-up age and dropped counts (S6-13)
  - a commit-graph that is missing or stale (S4-05)
- **Delivery:** in the app or as a board card, never posted outside it.

## The audit's git drawbacks, for cross-reference
These were verified in the running audit, with cards about to be filed:
- the main checkout parked by a merge conflict (S5-01)
- committed lane work invisible to collision checks (S3-02)
- untracked directories collapsed by `status` without `-u`, which hides new files (S3-08)
- 10 s per PGP-signed commit in the Pulse walk, and SSH-signed commits read as unsigned (S4-02)
- `--since` pruning by commit date in DORA (S4-01)
- "upstream gone" treated as "merged" and pre-selected for deletion (S4-07)
- ledger rows stamped with ingest time, not event time (S6-01)
- no commit-graph detection (S4-05)

## Considered and not recommended
- **Managed Agents' vault and proxy credentials.** They protect a sandbox from the model's own code. GitPulse agents run as you on your machine, so the boundary they defend doesn't exist here. *Inferred.*
- **Lazy sandbox provisioning.** Agents edit right after launch. Starting inference before the worktree is ready would need a measurement first; launch latency is unmeasured.
- **Perplexity's Slack and Teams binding, and its 400 connectors.** These are outside GitPulse's scope.
- **Perplexity's Concepts, Entities and Workstreams views.** DevMap and the board already cover this ground.
