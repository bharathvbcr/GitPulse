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
