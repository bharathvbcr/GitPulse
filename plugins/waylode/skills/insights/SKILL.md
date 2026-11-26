---
name: insights
description: Read Waylode repository insights over MCP — worktrees, agent sessions, uncommitted changes, overlapping dirty files, ledger verdicts, and code-graph availability. Use when an agent needs a honest snapshot of a checkout before editing, when multiple worktrees or coding agents are in flight, or when the user asks what is happening in a Waylode-managed repo.
license: MIT
compatibility: Requires the waylode-mcp binary on PATH and an absolute git repository path.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# Waylode insights

Waylode's MCP read tools (`waylode_insights`, `waylode_sessions`, `waylode_status`, `waylode_ledger_events`, `waylode_worktree_inbox`, `waylode_worktree_radar`, and the rest of the `readOnlyHint: true` set) never touch git or task state: they create and migrate nothing. Writes on the server are restricted to five task-board tools (`waylode_add_task`, `waylode_import_tasks`, `waylode_complete_task`, `waylode_delete_task`, `waylode_merge_tasks`) and nine repository-trust-gated worktree lifecycle and coordination tools (`waylode_worktree_create`, `waylode_worktree_switch`, `waylode_worktree_isolate`, `waylode_worktree_overlay_sync`, `waylode_worktree_claim`, `waylode_worktree_release`, `waylode_worktree_send_message`, `waylode_worktree_merge`, `waylode_worktree_prune`), with git mutations additionally gated through `harness::guard_command`.

It surfaces Waylode-owned facts and selected **DevCouncil** modules (in-process DevMap). Policy and agent hosting go through **Manvi**, not this server.

## Before you edit

1. Call `waylode_insights` with the absolute `repo_path`.
2. Read every facet's `ok` / `error` / `available` field. An empty list with `ok: false` means the check did not run — not that the repository is clean.
3. If `collisions.overlapping_files > 0`, or `collisions.unscanned_worktrees > 0`, or `collisions.shared_worktree_files > 0`, call `waylode_collision_risk` and `waylode_change_context` for the worktree you are about to touch.
4. Prefer `waylode_change_context` over guessing the branch, dirty files, or parked merge/rebase of a worktree.

## Tool map

| Tool | When |
| --- | --- |
| `waylode_insights` | First look at a repo |
| `waylode_sessions` | Who is running here, which session is me (`is_caller`), `run_id`, and live `state` (`working`/`blocked`/`done`/`idle`/`unknown`) without spawning git |
| `waylode_change_context` | About to edit one worktree |
| `waylode_collision_risk` | Parallel agents or several worktrees |
| `waylode_active_changes` | Need the file list, not just counts |
| `waylode_merge_conflicts` | Whether the worktrees' committed work would merge, pair by pair (`clean`, `conflict` with paths, `unknown` with a reason; detached worktrees included). Read-only: the merge runs in an object quarantine deleted afterwards. `ok: false` with `truncated` above 64 pairs: the counts are a floor |
| `waylode_status` | Narrower status (ledger + worktrees + codeintel) |
| `waylode_ledger_events` | Policy verdicts and recorded mutations |
| `waylode_turn_checkpoints` | This worktree's turn checkpoints (files at each agent turn boundary, when the person enabled `turn_checkpoints`): the list, or with `id` a bounded patch. A checkpoint with `omissions.untracked_omitted` above 0 is partial. Read-only; nothing is restored |
| `waylode_task_view` | DevCouncil tasks and leases, when a store exists |
| `waylode_codeintel_*` | Symbol search, impact, dependencies, trace, dead symbols |
| `devmap_*` (11 tools: `status`, `search`, `dependencies`, `impact`, `trace`, `neighbors`, `dead_symbols`, `clones`, `preview`, `explore`, `affected_tests`) | The same code graph under DevMap's own tool names, for parity with the standalone `devmap mcp` server; pass `repo_path` on every call |
| `waylode_worktree_inbox`, `waylode_worktree_radar` | Read-only (`readOnlyHint: true`) parallel-agent coordination: read direct/broadcast lane messages since `since_seq`, or run the 3-layer collision radar (file/symbol claims + 1-hop CSR call-graph neighbor hazards + quarantined `git merge-tree`) |
| `waylode_worktree_create`, `waylode_worktree_switch`, `waylode_worktree_isolate`, `waylode_worktree_overlay_sync`, `waylode_worktree_claim`, `waylode_worktree_release`, `waylode_worktree_send_message`, `waylode_worktree_merge`, `waylode_worktree_prune` | Repository-trust- and harness-gated worktree lifecycle, zero-copy DevMap delta overlay compilation, `<git-common-dir>/gitpulse/lanes/` claim/message coordination, ODB-quarantined merge, and 6-point-verified worktree pruning |
| `waylode_add_task`, `waylode_import_tasks`, `waylode_complete_task`, `waylode_merge_tasks`, `waylode_delete_task` | Repository-trust-gated task-board writes (see the `waylode:tasks` skill) |
| `waylode_browser_open` | Show the person a local dev server's page: asks the running Waylode (macOS) to open a Browser session on a loopback URL (`localhost`, `127.0.0.0/8`, `[::1]`) in a trusted repository's terminal dock, reusing and reloading a session already on that address. Brings Waylode and the repository forward. Opens only — it cannot read, drive or close the page. Fails when Waylode is not running or predates browser sessions |
| `waylode_terminal_spawn`, `waylode_terminal_exec`, `waylode_terminal_wait`, `waylode_terminal_read`, `waylode_terminal_send`, `waylode_terminal_close`, `waylode_terminal_list`, `waylode_terminal_open_file` | Scoped child terminal orchestration and right-hand split-pane file preview inside a running Waylode window (macOS), using `WAYLODE_TERMINAL_SESSION` + `WAYLODE_SESSION_TOKEN` inside a Waylode PTY or caller-scoped standalone identity (plus opt-in read-only repository shell inspection via `allow_agent_shell_reads`; see the `waylode:terminal` skill) |
| `waylode_provenance` | Verification freshness for a commit, and where it sits relative to the base (`landed_on_base`, `ahead`, `behind`). `verified` is true only for a note recording a pass of the whole suite, and `confidence` is null whenever it is false: no note, or a note recording a failure, an affected-tests-only pass (`passed-affected`) or an unrecognised verdict (read `verification.verdict` for which); a verification does not survive a rewrite |
| `waylode_landed` | Whether a commit's work reached a base, and how (`ff`, `merge`, `squash`, `rebase`, `tree`). `false` only when absence is provable, otherwise null with a `reason` code; `reverted` from a revert commit by ancestry; `landed_at` from the base branch's reflog. Refuses in a partial clone. Writes nothing (the content check runs in a quarantined object directory) |

## Host load

The SessionStart brief that `waylode_insights` accompanies carries a `host load:` line read from the host-load snapshot (tier, what is binding, Claude and Codex sessions launched and observed). It is not a `waylode_insights` facet. Before a heavy build or test, and whenever a command is refused for load, follow the `waylode:load` skill.

## When the MCP tools are not available

The MCP connection can be missing (the server is not loaded in this session, the plugin is not visible to the host) or dead (the host dropped it mid-session). The same tools then answer from the shell, through the same `waylode-mcp` binary, one request per command (legacy `gitpulse_*` tool names and `gitpulse://` URIs also resolve as aliases):

```sh
waylode-mcp tools                      # the catalog, one line each, read or write
waylode-mcp describe waylode_insights  # one tool's description and input schema
waylode-mcp call waylode_insights repo_path=/abs/repo
waylode-mcp call waylode_collision_risk repo_path=/abs/repo path=src/lib.rs --compact
waylode-mcp read waylode://server/manifest # lists the resources and their URI templates
waylode-mcp read waylode://status/abs/repo  # a facet: its name, then the absolute repo path
```

`call` runs the server's own request path in-process: the same schema check, result budget, repository trust and policy gates. `key=value` is typed by the tool's schema (a string property takes the text as it is; integers, booleans, arrays and objects take JSON), and `--json '<object>'`, `--json -` (stdin) or `--json @file` passes the whole argument object. A tool that is not `readOnlyHint: true` runs only with `--write`. Pass `repo_path` explicitly as a literal absolute path (never `$PWD` or a relative path): a shell's working directory is not necessarily the worktree you mean. Terminal tools (`waylode_terminal_*`) act for the Waylode terminal the shell runs in; see the `waylode:terminal` skill for what works outside one.

Stdout carries only the JSON answer; warnings go to stderr. A `call` or `read` waits at most `--timeout-ms` (by default 60000 for a read, the server's own per-request budget, and 600000 for a write, since a write stopped midway is left half done), then stops the tool, takes its git children down and exits 4. The exit status keeps the honesty rules below:

| Exit | Meaning |
| --- | --- |
| `0` | The tool answered and no part of the answer carries an incompleteness flag (still read each facet) |
| `1` | The tool reported an error, including an argument its schema refused, or it panicked |
| `2` | The command line was wrong: unknown command, tool, option, key or resource, or a write without `--write` |
| `3` | The answer says it is incomplete: `ok: false`, `<facet>_ok: false`, `available: false` (DevMap could not look, e.g. an untrusted repository), a non-empty `error` string, `deferred: true`, `truncated: true`, `deadline_expired: true` or a non-empty `walk_incomplete` on any object in it (`worktrees.ok`, `agents.live.ok`), the result budget cut it, or a resource was cut at its size cap (`text/plain`). Inside an array, only the cut markers count, never a row's own `ok: false`, `available: false` or `error`, which are data. The partial answer is still printed and stderr names where; read its `error` fields |
| `4` | No answer within `--timeout-ms`. Nothing is printed; a write's outcome is unknown, so read the state back before retrying |

**Large repositories and `waylode_worktree_create`.** A one-shot `--write` create builds the new lane's DevMap index inline, and that build may run for up to 900 s (`BUILD_DEADLINE`, `devmap/cli.rs`), longer than the 600 s a write waits by default; `--timeout-ms` cannot be raised past 600000. On a large repository the call can therefore exit 4 while the worktree already exists, its outcome unknown. Do not retry the create: read the state back (`waylode_worktree_radar`, `git worktree list`) and, if the lane is there, run `waylode-worktree create` or `isolate` directly, which has no 600 s limit and reports the refresh outcome (`devmap_refresh.kind`: built, fresh, deferred, cooled_down, busy or failed; exit 3, "done but incomplete", when the worktree was made but its index may be stale; exit 1 means no worktree was made).

This replaces a missing transport, not a loaded host: a read deferred under load is deferred from the shell too.

**Permissions.** Over MCP the host asks for approval per tool, so a destructive tool (`waylode_worktree_prune`, `waylode_worktree_merge`, `waylode_delete_task`) is approved on its own. Through `waylode-mcp call --write` it is an ordinary Bash command, judged by the host's Bash permission rules. `--write` is a guard against accidents, not a security boundary. So do not allowlist `waylode-mcp call` (or `Bash(waylode-mcp:*)`) broadly: that approves every write tool at once. Allow the read form narrowly if at all, and leave `--write` calls to prompt.

**Which binary runs.** `waylode-mcp` on `PATH` is the first one found, not necessarily the newest install: `/Applications/Waylode.app`'s copy can come before `~/.cargo/bin`, so `bun run mcp:install` alone may not change what the shell runs. Check `command -v waylode-mcp` and `waylode-mcp --version` (a `commands:` line lists the one-shot commands it supports); when the app's copy is the one found and is stale, rebuild or reinstall the app.

## Honesty rules this server already enforces

- Unscanned worktrees are counted, never implied clean.
- A missing code graph is `available: false` with a reason, not an empty hit list.
- Agent worktrees are recognised from `/.<agent>/worktrees/` layout, never from a branch name.
- `agents.ok: false` is "the worktree listing failed", not "no agents"; `agents.truncated: true` means the counts came from a capped sample, so `agents.sessions` is a floor and a kind whose worktrees all fall past the cap is absent from `agents.kinds` rather than undercounted.
- An agent worktree is a *checkout*, not a running process: `agents.kinds` counts the sessions a repository has on disk and cannot tell a running one from one abandoned mid-change. `agents.live` can: it lists the running agent sessions per worktree (discovered via the host process scan across Claude Code, Codex, Antigravity, DevCouncil, and Manvi, enriched with Claude Code's session registry). On platforms without a process scan, any kind that keeps no readable registry is reported with `ok: false` and an error, so a zero there means "could not count", not "none".
- `waylode_sessions` spawns no `git` and reports each attributed session's `is_caller`, `run_id`, `tree`, `task_id`, `state`, `state_source` (`hub` | `registry` | `none` | `unknown`), and `state_age_s`, bounded by `(cap, total, returned, truncated)` (caller first). When the desktop app is not running, `app_state.ok` is `false` with a reason and the tool still answers from the process scan and Claude registry; Codex sessions with no hub entry report `state: "unknown"` (never `"idle"`), and sessions with no registry or hub entry report `state_source: "none"`.

Pass `repo_path` as an absolute filesystem path on every call. The protocol is MCP `2026-07-28`: include `_meta.io.modelcontextprotocol/protocolVersion` and `clientCapabilities` on each request. Legacy `initialize` still works for older clients.
