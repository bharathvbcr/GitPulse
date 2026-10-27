# MCP and Agents

`gitpulse-mcp` is GitPulse's control plane for coding agents. It answers read questions and never mutates git: it never checks out a branch, writes a repository file, or takes a task lease. Its only writes are the task-board tools below, which write the GitPulse task profile behind the repository trust gate. Ask it what is true; mutate through the agent that already holds the writer lease.

It exposes GitPulse-owned surfaces (worktrees, ledger, collisions) and selected **DevCouncil** modules (in-process DevMap code graph, task/lease reads). Policy and agent hosting go through **Manvi**, not this server.

Protocol: [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) (`server/discover`, per-request `_meta`, cacheable `tools/list`). Legacy `initialize` (2024-11-05 / 2025-11-25) still works.

## Install the server

From a GitPulse checkout:

```sh
bun run mcp:install
bun run mcp:doctor
```

`mcp:install` puts this tree's `gitpulse-mcp` **and** `gitpulse-hook` on `PATH`. `mcp:doctor` reports each separately, distinguishing **absent**, **unresponsive**, and **stale** from **matching**. A missing binary must not look like a current one.

Both are required, and the hook is the one that fails quietly. A client that cannot spawn the server shows a failed MCP connection; a host that cannot spawn a hook records a non-blocking error and runs the tool call anyway, so an absent `gitpulse-hook` leaves the collision guard and command gate permanently disabled while every session looks normal. `mcp:doctor` exits non-zero if either is missing.

The packaged app copies `plugins/gitpulse/` into `Contents/Resources/plugin`. Settings resolves `gitpulse-mcp` and `gitpulse-hook` beside the app, says `not reachable` for a hook binary it cannot find, warns when either is not on `PATH`, and copies `.mcp.json` and `hooks.json` with the resolved absolute paths. A released app puts neither on `PATH`, which is why those copies, not the bare names in the plugin's own manifests, are what to install there. In the copied `hooks.json` the collision guard and command gate refuse the call when the binary is missing, and session start says the hooks are off.

Portable MCP config (`plugins/gitpulse/.mcp.json`):

```json
{
  "mcpServers": {
    "gitpulse": {
      "type": "stdio",
      "command": "gitpulse-mcp"
    }
  }
}
```

## Hooks

`plugins/gitpulse/hooks/hooks.json` registers sixteen hooks, all spawning `gitpulse-hook`:

| Event | Subcommand | What it does |
| --- | --- | --- |
| `PreToolUse` on `Edit` / `MultiEdit` / `Write` / `NotebookEdit` | `collision-guard` | Escalates to the user (`ask`) when another worktree holds uncommitted changes to the same file. |
| `PreToolUse` on `Bash` | `command-gate` | Refuses (`deny`) a command the MANVI harness blocks, such as a force push. |
| `SessionStart` | `session-brief` | Adds a repository snapshot to the session's context. |
| `Notification`, one entry per matcher | `notify <matcher>` | Tells a running GitPulse that this agent wants the user, and why. |
| `StopFailure` | `notify error` | Same, for a turn that ended on an error. |
| `PermissionRequest`, `PostToolUse` and `PostToolUseFailure`, `UserPromptSubmit`, `Stop`, `SessionEnd` | `notify permission_request`, `tool_finished`, `prompt_submitted`, `turn_finished`, `session_ended` | State reports for a running GitPulse: an approval is pending or a turn finished, and the resolutions that clear them (the user answered, a tool ran, the session ended). |

The reason travels as the subcommand's argument, not in the payload: the hook reference documents `Notification`'s matchers (`permission_prompt`, `idle_prompt`, `agent_needs_input`, `agent_completed`, `elicitation_dialog`, `elicitation_url_dialog`) but publishes no input field naming which one fired. Registering one entry per matcher makes the reason a fact about which hook the host chose to run rather than a string parsed out of a message.

Four properties are deliberate:

- **No hook ever answers `allow`.** An approval GitPulse did not earn would override the user's own permission rules. The gate can refuse or escalate; it cannot wave anything through.
- **Every hook exits 0.** Exit 2 blocks a tool call whatever the JSON says, and a hook that crashed must never be the reason an edit is refused. Failures degrade to "no decision".
- **A check that could not run says so.** Anything that did not scan, scanned partially, or could not reach the harness emits a `systemMessage` naming the gap, rather than the silence that means "clean".
- **`notify` is the one exception, because it is not a check.** GitPulse being closed is its ordinary case, and a `systemMessage` on every turn would be noise the user cannot act on. Where an undelivered report is visible instead is **Settings → Notifications → Agent session notifications**, which reports whether the socket is listening and how many reports it has accepted — a place a user looks when they wonder why nothing arrives, not one that interrupts them when they do not.

### The notification socket

`notify` writes one small JSON report to a Unix socket in GitPulse's own configuration directory (`agent-notify.sock`; `GITPULSE_NOTIFY_SOCKET` overrides the path, `GITPULSE_SESSION_ID` names the terminal tab it is running in). It exists only while GitPulse is running and only while **Accept reports from agent hooks** is on; it is not a platform GitPulse offers on Windows, which has no Unix socket.

- The directory is `0700` and the socket `0600`, and every peer's uid is checked — a connection whose uid cannot be read is refused, not trusted.
- A report is a closed vocabulary: a protocol version, one of twelve event names, one of six agent names, and an optional session key, working directory and message. Anything else is refused and counted.
- Reports are rate limited and coalesced per session alongside every other signal, and the most a report can do is raise a banner and focus a terminal tab.
- The budget is 1.5 s, well inside a host's own hook timeout, and a socket that is not there costs the turn nothing.
- **A reply that misses its bound is counted, not retried, for every event.** The hook gives up at the budget (300 ms for `SessionEnd`, the report that clears a pending approval; 1.5 s for the rest) and abandons the send; the report may or may not have arrived. The socket has its own deadline of 1.5 s for every event, so for the events other than `SessionEnd` the two are the same length and for `SessionEnd` the outer budget is the shorter; whichever of the two ends the wait is the same outcome and the same single count. A peer that answered and closed (a bridge at capacity writes its refusal and closes without reading the report) is read for its reply, and a reply that was read decides the outcome however the read then ended: Linux follows the reply with a connection reset and macOS with end of stream, and the refusal is reported as itself on both. A peer that closed with nothing to say is counted the same way: no reply came, so the report is not confirmed delivered. A worker thread that panicked or could not start is reported as that, at warn, and not counted. The hook process counts it in `dropped` / `dropped_scope` (`this_process`), the shape the ledger uses for its own drops, and says so on stderr. A hook is a one-shot process, so that count is that run's and nothing wider; what reaches a person is the record each miss also appends, one line (`{"v":1,"at":<unix ms>,"event":...,"session":...}`, event name and time only) to `<socket path>.drops` beside the socket the hook resolved. A record that cannot be written adds "not recorded: ..." to the stderr line and changes nothing else. The app reads it in `alerts::status()` (current file and `.prev`; it renames the file to `.prev` above 32 KiB, the hook stops appending at 64 KiB) and Settings shows "N hook reports in the last 7 days got no reply from GitPulse in time" apart from the refused-report count, and never as "lost": the app may have applied a report the hook stopped waiting for.

Where the transport does not exist (Windows has no Unix socket), `notify` reports `unsupported` once per invocation at info and exits 0, before it looks for a configuration directory, so an unset `APPDATA` is not a warning. That is a third outcome, not a delivery and not a failure; the bridge is not offered on Windows (ft-959d4d26).

Off macOS the report would have nowhere to go even if the socket existed: desktop notifications are a macOS implementation, and every other platform's arm returns `unsupported`. The hooks stay installed and harmless, and GitPulse says so rather than offering a switch that does nothing. See [platform coverage](https://github.com/bharathvbcr/GitPulse/blob/main/docs/QUALIFICATION.md#platform-coverage).

The command gate runs without a bound task scope, so it reaches the hard rungs (force-push, destructive commands) and not the scope rungs. That is a smaller gate than the desktop app's, and a real one.

### What the task-scope fence covers, and what it does not

In a checkout bound to a task, both hooks judge writes against the task's plan. The edges, each decided rather than forgotten:

- **Argument-writing commands.** The harness reads redirections and the files `sed -i`, `tee`, `cp`, `mv`, `rm`, `truncate`, `touch`, `dd of=`, `perl -i`, `install`, `ln`, `rsync` and `patch FILE` write, and judges each with the same file gate an `Edit` meets: against a plan that excludes the file they are refused as `scope.unplanned`. A write whose target is not in the command text (`git apply`, anything run through `xargs`, `find -exec`) cannot be judged against a scope, so with a scope bound it is refused as `command.substitution` with a reason naming the declared scope. This needs a Manvi whose DevCouncil is at or after `ec26e906`; an older harness reads only the first four programs and allows the rest. Reading a program's write targets is the harness's job (DevCouncil `policy`), and a second parser here would drift from it, so GitPulse does not fence them itself. The hook sends every command to the harness unaltered with the bound scope (one unit test covers every program above), so fencing one more is a harness change and nothing here changes. `tests/hook_scope_fence.rs` drives the real binary and fails when one stops being refused.
- **Descendants that escape.** A process that leaves the agent's process group (a daemonised child, `setsid`) writes with no tool call for a hook to judge. Hooks judge calls, not processes; GitPulse reaps what it launched and reports what it cannot, and does not claim more.
- **A second profile or an agent GitPulse did not launch.** The fence exists only where the plugin's hooks are registered. An agent under another Claude profile, or one started outside GitPulse, has none, and nothing here can fence it. The task binding is per checkout, so such an agent is judged the moment its profile registers the hooks: Settings -> MCP copies a `hooks.json` with absolute paths for that.

## Package layout

One canonical package: [`plugins/gitpulse/`](https://github.com/bharathvbcr/GitPulse/tree/main/plugins/gitpulse)

- Codex: `.codex-plugin/plugin.json` + `.mcp.json`
- Cursor: `.cursor-plugin/plugin.json` (identity and logo only; no hooks entry, because Cursor's documented plugin `hooks/hooks.json` is a different format from ours and the fields ours rely on are undocumented there: [plugin hooks per agent host](https://github.com/bharathvbcr/GitPulse/blob/main/docs/QUALIFICATION.md#plugin-hooks-per-agent-host))
- Claude Code compatibility files
- Agent Plugins 1.0 manifests
- Shared `skills/`

## Tools

Pass an **absolute** `repo_path` on every call.

| Tool | When |
| --- | --- |
| `gitpulse_insights` | First look at a repo |
| `gitpulse_status` | Narrower: ledger + worktrees + codeintel |
| `gitpulse_change_context` | About to edit one worktree |
| `gitpulse_collision_risk` | Parallel agents or several worktrees |
| `gitpulse_active_changes` | File list, not just counts |
| `gitpulse_merge_conflicts` | Whether the worktrees' committed work would merge, pair by pair (`clean`, `conflict` with paths, `unknown` with a reason; detached worktrees included). Read-only: the merge runs in an object quarantine deleted afterwards. `ok: false` with `truncated` above 64 pairs: the counts are a floor |
| `gitpulse_ledger_events` | Policy verdicts and recorded mutations |
| `gitpulse_task_view` | DevCouncil tasks/leases, when a store exists |
| `gitpulse_codeintel_search` / `_impact` / `_dependencies` / `_trace` / `_dead_symbols` / `_suspects` | In-process DevMap code graph (vendored store schema 27). The same graph is also served by the `devmap_*` tools on this server (status, search, dependencies, impact, trace, neighbors, dead_symbols, clones, preview, explore, affected_tests). |
| `gitpulse_list_tasks` / `_get_task` | Read the GitPulse task board and a task's brief |
| `gitpulse_add_task` / `_import_tasks` / `_complete_task` | Write the task board: file a task, import a `tasks/` folder, mark the launched task done. The only writes this server makes |
| `gitpulse_delete_task` / `_merge_tasks` | Destructive board writes (both end in the store's soft delete) and annotated `destructiveHint: true` |
| `gitpulse_provenance` | Verification freshness for a commit, and where it sits relative to the base (`landed_on_base`, `ahead`, `behind`). `confidence` is null (and `verified` false) when the commit has no verification note. A verification is keyed to the commit's sha and does not survive a rewrite: a rebased, amended or squashed commit is a new commit with none, even when `gitpulse_landed` finds its work on the base |
| `gitpulse_landed` | Whether a commit's work reached a base, and how (`ff`, `merge`, `squash`, `rebase`, `tree`). `landed: false` only when absence is provable; otherwise null with a `reason` (`partial_clone_fetch_required`, `search_depth_exceeded`, `content_not_provable`, ...). `reverted` is true only when a revert commit is found by ancestry; `landed_at` comes from the base branch's own reflog (never `HEAD`'s). Never writes to the repository: the content comparison (`git merge-tree --write-tree`) runs in a quarantined object directory deleted afterwards |

Prompts: `gitpulse_preflight`, `gitpulse_collision_triage`, `gitpulse_handoff`, `gitpulse_session_brief`.

Resources use `gitpulse://<facet>{+repo_path}`; `gitpulse://server/manifest` describes the surface.

## Honesty rules the server already enforces

- Unscanned worktrees are counted, never implied clean.
- A missing or schema-mismatched code graph is `available: false` with a reason, not an empty hit list.
- Treat `walk_incomplete` / truncation fields as "floor, not complete" — the same honesty rule the Diff and Map panels enforce.
- Agent worktrees are recognised from `/.<agent>/worktrees/` layout, never from a branch name.
- Read every facet's `ok` / `error` / `available` field. Empty + `ok: false` means the check did not run.
- A ledger answer (`gitpulse_ledger_events`, the `ledger` resource, the `ledger` facet of `gitpulse_status` and `gitpulse_insights`) says how current it is. `as_of` is when the newest row was appended (null for an empty or unreadable ledger). `last_catch_up` is when a catch-up pass last completed, with the method in `last_catch_up_basis`: `catch_up_state` (a pass persisted it), `newest_row` (a lower bound from the newest row), or `none`. `stale: true` means no catch-up or recorded event within `stale_after_secs` (600): nothing launches `gitpulsed` for you, so a stale ledger is often just un-ingested. `catch_up` carries what the last passes covered, including `skipped_lines` and `transcripts_unlisted`.
- `recording` is true only for a process that is writing the ledger. The MCP server only reads, so it reports `recording: false` with `recording_state: "readable"`; `"unknown"` means no ledger or one that could not be opened. `dropped` counts appends dropped by the answering process only (`dropped_scope: "this_process"`): 0 from a reader is not "nothing was dropped anywhere".
- `caveat` / `caveat_code` (`legacy_ledger_unconsolidated`) on the ledger tool and resource, and `error` on the status facets, mean a sibling worktree still holds its own ledger: open the repository in GitPulse to consolidate. Rows recorded there are not in the answer.
- `gitpulse_change_context` names the ledger it read in `ledger_freshness` (same fields; `as_of` is the one to cite).

## Headless catch-up

When the GUI is closed, `gitpulsed` can still append attribution (transcripts, reflog) to the ledger:

```sh
gitpulsed --interval 300 /path/to/repo
```

It serves no MCP requests (that is `gitpulse-mcp`) and never takes a lease. See [[Architecture]].
