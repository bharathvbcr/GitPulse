# MCP and Agents

`waylode-mcp` (legacy alias `gitpulse-mcp`) is Waylode's control plane for coding agents. Its read tools (`readOnlyHint: true`) never mutate git or task state: they never check out a branch, write a repository file, or take a task lease. Its writes are strictly restricted to the task-board tools and the `waylode_worktree_*` lifecycle and lane-coordination tools below, all gated behind explicit repository trust (`repository_trust::require`) and the Manvi policy harness (`harness::guard_command` for git mutations). Legacy `gitpulse_*` tool calls canonicalize to `waylode_*` before schema validation, and setting `WAYLODE_MCP_EXPOSE_LEGACY=1` advertises the 55 deprecated `gitpulse_*` aliases in `tools/list` when a host requires explicit listing.

It exposes Waylode-owned surfaces (worktrees, ledger, collisions) and selected **DevCouncil** modules (in-process DevMap code graph, task/lease reads). Policy and agent hosting go through **Manvi**, not this server.

Protocol: [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) (`server/discover`, per-request `_meta`, cacheable `tools/list`). Legacy `initialize` (2024-11-05 / 2025-11-25) still works.

## Connect an agent (app install)

Open **Settings → Agents → Connect your agent**. It is also the setup wizard's last step and the first-run card. It shows a plan first and changes nothing until you confirm. On confirm it:

- symlinks the app's bundled `waylode-mcp` and `waylode-hook` into `~/.local/bin` (plus legacy `gitpulse-mcp` / `gitpulse-hook` compat links where needed), never overwriting a file or a link that points elsewhere;
- registers the `waylode` server with Claude Code (`claude mcp add --scope user waylode -- <full path>`) and Codex (`codex mcp add waylode -- <full path>`) through their own CLIs.

An existing different registration or link is left alone. The plan shows the commands to switch it yourself (remove, then add). No source checkout or Rust toolchain is needed.

**Plugin or direct registration, not both.** If the Waylode Claude Code plugin is installed, it already provides the `waylode` server and the hooks, so Connect marks the Claude Code registration as done instead of adding a second one. The plugin starts `waylode-mcp` and `waylode-hook` by bare name, so `~/.local/bin` must be on the `PATH` of the shell Claude Code starts from. Connect does not install the plugin or the hooks:

```sh
claude plugin marketplace add bharathvbcr/GitPulse
claude plugin install waylode@waylode
```

**Copy setup prompt** (same panel) gives your agent the same steps to run itself. Use it if you run Claude Code with a non-default `CLAUDE_CONFIG_DIR`, because Connect runs the `claude` CLI with its default profile.

The hook is the binary that fails quietly. A client that cannot spawn the server shows a failed MCP connection; a host that cannot spawn a hook records a non-blocking error and runs the tool call anyway, so an absent `waylode-hook` leaves the collision guard and command gate permanently disabled while every session looks normal.

### Settings copy buttons

The packaged app also carries `plugins/waylode/` in `Contents/Resources/plugin`. The Agents card in Settings resolves `waylode-mcp` and `waylode-hook` beside the app, says `not reachable` for a hook binary it cannot find, and warns when either is not on `PATH`. Its `.codex-plugin/plugin.json`, `.mcp.json` and `hooks.json` buttons copy the manifests with those absolute paths filled in, for a client you wire by hand instead of using Connect. In the copied `hooks.json` the collision guard and command gate refuse the call when the binary is missing, and session start says the hooks are off.

### Upgrading from `bun run mcp:install`

`bun run mcp:install` is for a source checkout only: it `cargo install`s the binaries into `~/.cargo/bin` and records the source digest `bun run mcp:doctor` compares against. If you ran it before and now use the app, `~/.cargo/bin/waylode-mcp` and `waylode-hook` (or older `gitpulse-mcp` / `gitpulse-hook` copies) may shadow the app's copies on `PATH`, and an existing registration may point at the cargo copy. Connect leaves those alone and shows them. To switch to the app's copies, run `cargo uninstall waylode` (and `cargo uninstall gitpulse` for pre-rename installs) and the remove/add commands the plan shows. If you develop Waylode from source, keep the cargo copies, re-run `bun run mcp:install` after pulling, and use `bun run mcp:doctor`, which reports each binary separately as **absent**, **unresponsive**, **stale** or **matching** and exits non-zero if either is missing.

### Install from a source checkout

```sh
bun run mcp:install
bun run mcp:doctor
```

`mcp:install` puts this tree's `waylode-mcp` **and** `waylode-hook` on `PATH`. Both are required.

Portable MCP config (`plugins/waylode/.mcp.json`):

```json
{
  "mcpServers": {
    "waylode": {
      "type": "stdio",
      "command": "waylode-mcp"
    }
  }
}
```

## Hooks

`plugins/waylode/hooks/hooks.json` registers nineteen hooks across twelve lifecycle events (including `WorktreeCreate` and `WorktreeRemove` via `plugins/waylode/hooks/waylode-worktree.sh`), all spawning `waylode-hook`:

| Event | Subcommand | What it does |
| --- | --- | --- |
| `PreToolUse` on `Edit` / `MultiEdit` / `Write` / `NotebookEdit` | `collision-guard` | Escalates to the user (`ask`) when another worktree holds uncommitted changes to the same file, or refuses (`deny`) when outside the bound task scope. |
| `PreToolUse` on `Bash` | `command-gate` | Refuses (`deny`) a command the Manvi harness blocks, such as a force push or an out-of-scope file write. |
| `SessionStart` | `session-brief`, `peer-inbox` | Adds a repository snapshot (asking the running Waylode app first before scanning worktrees directly) and unread peer inbox messages to the session's context. |
| `WorktreeCreate`, `WorktreeRemove` | `worktree-create`, `worktree-remove` | Creates or removes Claude Code `.claude/worktrees/<name>` checkouts through Waylode's CoW cache, `.worktreeinclude`, `post_create` hook, and DevMap clone path, falling back to plain `git` when untrusted or unavailable. |
| `Notification`, one entry per matcher | `notify <matcher>` | Tells a running Waylode that this agent wants the user, and why. |
| `StopFailure` | `notify error` | Same, for a turn that ended on an error. |
| `PermissionRequest`, `PostToolUse` and `PostToolUseFailure`, `UserPromptSubmit`, `Stop`, `SessionEnd` | `notify permission_request`, `tool_finished`, `prompt_submitted`, `turn_finished`, `session_ended` (plus `peer-inbox` on `PostToolUse` and `UserPromptSubmit`) | State reports for a running Waylode: an approval is pending or a turn finished, and the resolutions that clear them (the user answered, a tool ran, the session ended), plus synchronous peer-inbox delivery. |
| `Stop`, a second entry with `asyncRewake` and a 1800 s timeout | `deliver` | Waits, in the background, for a message a person previewed and confirmed in Waylode's **Send to agent**, and wakes the idle session with it (exit 2, text on stderr). |

**Antigravity (`plugins/waylode-agy/hooks.json` and `plugins/waylode/hooks.json`):** Antigravity IDE 2.32.1 denied any `PreToolUse` tool call whose hook returned `{}` without `"decision":"allow"`, and answering `"allow"` would override the user's own approval settings. Waylode therefore registers **no `PreToolUse` hooks** (`collision-guard` or `command-gate`) for Antigravity; Antigravity keeps `SessionStart` (`session-brief`, `peer-inbox`), `PreInvocation` (`peer-inbox`), `PostInvocation` (`peer-inbox`), `PostToolUse` (`notify tool_finished`), and `Stop` (`stop-or-notify`), and informational hook notices without a permission decision render as `{"reason": ...}` rather than `{"decision": "ask"}`.

The reason travels as the subcommand's argument, not in the payload: the hook reference documents `Notification`'s matchers (`permission_prompt`, `idle_prompt`, `agent_needs_input`, `agent_completed`, `elicitation_dialog`, `elicitation_url_dialog`) but publishes no input field naming which one fired. Registering one entry per matcher makes the reason a fact about which hook the host chose to run rather than a string parsed out of a message.

Four properties are deliberate:

- **No hook ever answers `allow`.** An approval Waylode did not earn would override the user's own permission rules. The gate can refuse or escalate; it cannot wave anything through.
- **Every hook exits 0, except one deliberate case.** Exit 2 blocks a tool call whatever the JSON says, and a hook that crashed must never be the reason an edit is refused. Failures degrade to "no decision". The exception is `deliver`: it is registered on `Stop` with `asyncRewake`, where Claude Code runs it in the background and wakes the session on exit 2, so exit 2 there is a delivered message and blocks nothing. Every failure of it, and a timeout, is still exit 0.
- **A check that could not run says so.** Anything that did not scan, scanned partially, or could not reach the harness emits a `systemMessage` naming the gap, rather than the silence that means "clean".
- **`notify` is the one exception, because it is not a check.** Waylode being closed is its ordinary case, and a `systemMessage` on every turn would be noise the user cannot act on. Where an undelivered report is visible instead is **Settings → Notifications → Agent session notifications**, which reports whether the socket is listening and how many reports it has accepted — a place a user looks when they wonder why nothing arrives, not one that interrupts them when they do not.

### The notification socket

`notify` writes one small JSON report to a Unix socket in Waylode's own configuration directory (`agent-notify.sock`; `WAYLODE_NOTIFY_SOCKET` and legacy `GITPULSE_NOTIFY_SOCKET` override the path, `WAYLODE_SESSION_ID` / `GITPULSE_SESSION_ID` names the terminal tab it is running in). It exists only while Waylode is running and only while **Accept reports from agent hooks** is on; it is not a platform Waylode offers on Windows, which has no Unix socket.

- The directory is `0700` and the socket `0600`, and every peer's uid is checked — a connection whose uid cannot be read is refused, not trusted.
- A report is a closed vocabulary: a protocol version, one of twelve event names, one of six agent names, and an optional session key, working directory and message. Anything else is refused and counted.
- Reports are rate limited and coalesced per session alongside every other signal, and the most a report can do is raise a banner and focus a terminal tab.
- The budget is 1.5 s, well inside a host's own hook timeout, and a socket that is not there costs the turn nothing.
- **A reply that misses its bound is counted, not retried, for every event.** The hook gives up at the budget (300 ms for `SessionEnd`, the report that clears a pending approval; 1.5 s for the rest) and abandons the send; the report may or may not have arrived. The socket has its own deadline of 1.5 s for every event, so for the events other than `SessionEnd` the two are the same length and for `SessionEnd` the outer budget is the shorter; whichever of the two ends the wait is the same outcome and the same single count. A peer that answered and closed (a bridge at capacity writes its refusal and closes without reading the report) is read for its reply, and a reply that was read decides the outcome however the read then ended: Linux follows the reply with a connection reset and macOS with end of stream, and the refusal is reported as itself on both. A peer that closed with nothing to say is counted the same way: no reply came, so the report is not confirmed delivered. A worker thread that panicked or could not start is reported as that, at warn, and not counted. The hook process counts it in `dropped` / `dropped_scope` (`this_process`), the shape the ledger uses for its own drops, and says so on stderr. A hook is a one-shot process, so that count is that run's and nothing wider; what reaches a person is the record each miss also appends, one line (`{"v":1,"at":<unix ms>,"event":...,"session":...}`, event name and time only) to `<socket path>.drops` beside the socket the hook resolved. A record that cannot be written adds "not recorded: ..." to the stderr line and changes nothing else. The app reads it in `alerts::status()` (current file and `.prev`; it renames the file to `.prev` above 32 KiB, the hook stops appending at 64 KiB) and Settings shows "N hook reports in the last 7 days got no reply from Waylode in time" apart from the refused-report count, and never as "lost": the app may have applied a report the hook stopped waiting for.

Where the transport does not exist (Windows has no Unix socket), `notify` reports `unsupported` once per invocation at info and exits 0, before it looks for a configuration directory, so an unset `APPDATA` is not a warning. That is a third outcome, not a delivery and not a failure; the bridge is not offered on Windows (ft-959d4d26).

Off macOS the report would have nowhere to go even if the socket existed: desktop notifications are a macOS implementation, and every other platform's arm returns `unsupported`. The hooks stay installed and harmless, and Waylode says so rather than offering a switch that does nothing. See [platform coverage](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/QUALIFICATION.md#platform-coverage).

The command gate runs without a bound task scope, so it reaches the hard rungs (force-push, destructive commands) and not the scope rungs. That is a smaller gate than the desktop app's, and a real one.

### What the task-scope fence covers, and what it does not

In a checkout bound to a task, both hooks judge writes against the task's plan. The edges, each decided rather than forgotten:

- **Argument-writing commands.** The harness reads redirections and the files `sed -i`, `tee`, `cp`, `mv`, `rm`, `truncate`, `touch`, `dd of=`, `perl -i`, `install`, `ln`, `rsync` and `patch FILE` write, and judges each with the same file gate an `Edit` meets: against a plan that excludes the file they are refused as `scope.unplanned`. A write whose target is not in the command text (`git apply`, anything run through `xargs`, `find -exec`) cannot be judged against a scope, so with a scope bound it is refused as `command.substitution` with a reason naming the declared scope. This needs a Manvi whose DevCouncil is at or after `ec26e906`; an older harness reads only the first four programs and allows the rest. Reading a program's write targets is the harness's job (DevCouncil `policy`), and a second parser here would drift from it, so Waylode does not fence them itself. The hook sends every command to the harness unaltered with the bound scope (one unit test covers every program above), so fencing one more is a harness change and nothing here changes. `tests/hook_scope_fence.rs` drives the real binary and fails when one stops being refused.
- **A file in another checkout (outside the session's cwd checkout).** The file tools judge the checkout that owns the file, found by walking up from the file's nearest existing ancestor (the file may not exist yet; both sides are canonicalised). A session bound to a declared task scope that writes into another checkout of the same repository (the main checkout or a sibling worktree) is refused as `scope.other_checkout`: a plan names files of the checkout it is bound to, so the same relative spelling in the plan never authorises another checkout's file. A session with no binding is held to the owning checkout's own binding, and the collision scan runs against the owning checkout, so a sibling that holds the file dirty is named. A file in an unrelated repository is judged by that repository's own binding and collision rows; the session's task says nothing about it. A file outside every Git repository keeps the "did NOT run ... not inside the worktree" notice, and so does a file whose owner or repository could not be determined (with the reason), never a clean pass. A file under the cwd checkout is judged as before, including one inside a worktree nested in it. This is covered by non-ignored unit tests in `src/hooks/mod.rs`; `tests/hook_scope_fence.rs` is the ignored real-harness run.
- **Descendants that escape.** A process that leaves the agent's process group (a daemonised child, `setsid`) writes with no tool call for a hook to judge. Hooks judge calls, not processes; Waylode reaps what it launched and reports what it cannot, and does not claim more.
- **A second profile or an agent Waylode did not launch.** The fence exists only where the plugin's hooks are registered. An agent under another Claude profile, or one started outside Waylode, has none, and nothing here can fence it. The task binding is per checkout, so such an agent is judged the moment its profile registers the hooks: Settings -> MCP copies a `hooks.json` with absolute paths for that.

## Package layout

One canonical package: [`plugins/waylode/`](https://github.com/bharathvbcr/GitPulse/tree/main/plugins/waylode) (and Antigravity bundle under `plugins/waylode-agy/`):

- Codex: `.codex-plugin/plugin.json` + `.mcp.json`
- Cursor: `.cursor-plugin/plugin.json` (identity and logo only; no hooks entry, because Cursor's documented plugin `hooks/hooks.json` is a different format from ours and the fields ours rely on are undocumented there: [plugin hooks per agent host](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/QUALIFICATION.md#plugin-hooks-per-agent-host))
- Claude Code compatibility files
- Agent Plugins 1.0 manifests
- Shared `skills/`

## Tools

Pass an **absolute** `repo_path` on every call. All 55 `waylode_*` tools also accept legacy `gitpulse_*` names (and `WAYLODE_MCP_EXPOSE_LEGACY=1` advertises the deprecated aliases in `tools/list`).

| Tool | When |
| --- | --- |
| `waylode_insights` | First look at a repo (includes `agents.live` per-worktree running sessions and `served_by` when answered from the running app's shared reads) |
| `waylode_sessions` | Who is running here, which session is me (`is_caller`), `run_id`, and live `state` (`working`/`blocked`/`done`/`idle`/`unknown`) without spawning `git` |
| `waylode_status` | Narrower: ledger + worktrees + codeintel |
| `waylode_change_context` | About to edit one worktree |
| `waylode_collision_risk` | Parallel agents or several worktrees |
| `waylode_active_changes` | File list, not just counts |
| `waylode_merge_conflicts` | Whether the worktrees' committed work would merge, pair by pair (`clean`, `conflict` with paths, `unknown` with a reason; detached worktrees included). Read-only: the merge runs in an object quarantine deleted afterwards. `ok: false` with `truncated` above 64 pairs: the counts are a floor |
| `waylode_ledger_events` | Policy verdicts and recorded mutations |
| `waylode_task_view` | DevCouncil tasks/leases, when a store exists |
| `waylode_codeintel_search` / `_impact` / `_dependencies` / `_trace` / `_dead_symbols` / `_suspects` | In-process DevMap code graph (vendored store schema 29, including zero-copy `base_store_ref.json` worktree readers). The same graph is also served by the `devmap_*` tools on this server (status, search, dependencies, impact, trace, neighbors, dead_symbols, clones, preview, explore, affected_tests). |
| `waylode_worktree_inbox` / `_radar` | Read-only (`readOnlyHint: true`) parallel-agent coordination: read direct/broadcast messages from `<git-common-dir>/gitpulse/lanes/messages.json` since `since_seq`, or run the 3-layer collision radar (file/symbol claims + 1-hop CSR call-graph neighbor hazards + quarantined `git merge-tree`) |
| `waylode_worktree_create` / `_switch` / `_isolate` / `_overlay_sync` | Trust- and harness-gated worktree lifecycle: create full, `--zero-copy`, or DevMap-expanded `--cone` sparse worktrees; locate or create branch checkouts; provision CoW dependency caches and DevMap state (results report what happened: `cache_shared`, `devmap_outcome`, `overlay_error` and `problems` name any part that failed, and a failing `execute_command` still returns the worktree it ran in); compile dirty files into `overlay.json` + Blake3 CAS ASTs + `RoaringBitmap` CSR dirty masks |
| `waylode_worktree_claim` / `_release` / `_send_message` | Trust-gated `<git-common-dir>/gitpulse/lanes/` coordination: atomically claim/release files and symbols (`.bus.lock`) and send bounded (16 KiB) direct or broadcast handoff/interface-change messages |
| `waylode_worktree_merge` / `_prune` | Destructive (`destructiveHint: true`), trust- and harness-gated integration and cleanup: ODB-quarantined squash (the default) or merge-commit pipeline. By default (`remove_worktree: true`) merge then tears down the source worktree and branch, if the branch has not moved and the worktree is still clean. That teardown terminates any process still running inside the worktree, with no opt-in, and a failed termination is not reported; pass `remove_worktree: false` to keep the worktree. Prune removes worktrees and deletes branches that the 6-point check verifies as integrated (a worktree with a live process inside is skipped with the reason unless `reap_processes: true` opts in to terminating them; every worktree gets its own outcome, so one failure never hides the rest) |
| `waylode_list_tasks` / `_get_task` | Read the Waylode task board and a task's brief |
| `waylode_add_task` / `_import_tasks` / `_complete_task` | Trust-gated task-board writes: file a task, import a `tasks/` folder, mark the launched task done |
| `waylode_delete_task` / `_merge_tasks` | Destructive board writes (both end in the store's soft delete) and annotated `destructiveHint: true` |
| `waylode_provenance` | Verification freshness for a commit, and where it sits relative to the base (`landed_on_base`, `ahead`, `behind`). `verified` is true only when the commit's verification note records a pass of the whole suite; `confidence` is null (and `verified` false) when there is no note or the note records a failure, an affected-tests-only pass (`passed-affected`) or an unrecognised verdict, which `verification.verdict` names. A verification is keyed to the commit's sha and does not survive a rewrite: a rebased, amended or squashed commit is a new commit with none, even when `waylode_landed` finds its work on the base |
| `waylode_landed` | Whether a commit's work reached a base, and how (`ff`, `merge`, `squash`, `rebase`, `tree`). `landed: false` only when absence is provable; otherwise null with a `reason` (`partial_clone_fetch_required`, `search_depth_exceeded`, `content_not_provable`, ...). `reverted` is true only when a revert commit is found by ancestry; `landed_at` comes from the base branch's own reflog (never `HEAD`'s). Never writes to the repository: the content comparison (`git merge-tree --write-tree`) runs in a quarantined object directory deleted afterwards |
| `waylode_browser_open` | Ask the running desktop app over the single-instance mailbox to open a loopback URL in a supervised Browser session (refuses untrusted repos and non-loopback URLs) |
| `waylode_terminal_spawn` / `_open_file` / `_list` / `_wait` / `_read` / `_send` / `_close` | Supervised PTY sessions inside Waylode's terminal dock: spawn shells (`model`, `mode`, `settings` options for agent children) or open files (`_open_file`), list, wait on output/exit, read bounded buffers, send input, and close |
| `waylode_kickoff_agent` / `waylode_agent_register` / `_peers` / `_send` / `_inbox` / `_wait` | Orchestrate supervised agent sessions (`_kickoff_agent`) and bounded peer-to-peer A2A messaging across worktrees |
| `waylode_campaign_audit` | Inspect cross-worktree campaign readiness, active sessions, and lane state |

Prompts: `waylode_preflight`, `waylode_collision_triage`, `waylode_handoff`, `waylode_session_brief` (legacy `gitpulse_*` prompt names also resolve).

Resources use `waylode://<facet>{+repo_path}` (with `gitpulse://` accepted as a legacy alias); `waylode://server/manifest` describes the surface.

## Honesty rules the server already enforces

- Unscanned worktrees are counted, never implied clean.
- A missing or schema-mismatched code graph is `available: false` with a reason, not an empty hit list.
- Treat `walk_incomplete` / truncation fields as "floor, not complete" — the same honesty rule the Diff and Map panels enforce.
- Agent worktrees are recognised from `/.<agent>/worktrees/` layout, never from a branch name.
- Read every facet's `ok` / `error` / `available` field. Empty + `ok: false` means the check did not run.
- A ledger answer (`waylode_ledger_events`, the `ledger` resource, the `ledger` facet of `waylode_status` and `waylode_insights`) says how current it is. `as_of` is when the newest row was appended (null for an empty or unreadable ledger). `last_catch_up` is when a catch-up pass last completed, with the method in `last_catch_up_basis`: `catch_up_state` (a pass persisted it), `newest_row` (a lower bound from the newest row), or `none`. `stale: true` means no catch-up or recorded event within `stale_after_secs` (600): nothing launches `wayloded` for you, so a stale ledger is often just un-ingested. `catch_up` carries what the last passes covered, including `skipped_lines` and `transcripts_unlisted`.
- `recording` is true only for a process that is writing the ledger. The MCP server only reads, so it reports `recording: false` with `recording_state: "readable"`; `"unknown"` means no ledger or one that could not be opened. `dropped` counts appends dropped by the answering process only (`dropped_scope: "this_process"`): 0 from a reader is not "nothing was dropped anywhere".
- `caveat` / `caveat_code` (`legacy_ledger_unconsolidated`) on the ledger tool and resource, and `error` on the status facets, mean a sibling worktree still holds its own ledger: open the repository in Waylode to consolidate. Rows recorded there are not in the answer.
- `waylode_change_context` names the ledger it read in `ledger_freshness` (same fields; `as_of` is the one to cite).

## Headless catch-up

When the GUI is closed, `wayloded` (legacy alias `gitpulsed`) can still append attribution (transcripts, reflog) to the ledger:

```sh
wayloded --interval 300 /path/to/repo
```

It serves no MCP requests (that is `waylode-mcp`'s job) and never takes a lease. See [[Architecture]].
