---
name: terminal
description: Orchestrate scoped child terminal sessions and split-pane file previews inside Waylode over MCP or waylode-hook term. Use when an agent running in a Waylode PTY wants to spin up a child shell or sub-agent (Claude Code, Antigravity, Codex, Manvi, Grok), wait on semantic prompt/agent idle or a dev-server URL without sleep-polling, read secret-redacted 2D terminal screen output, send policy-gated input, or open a repository file in a side-by-side split pane.
license: MIT
compatibility: Requires a running Waylode desktop window on macOS, a trusted git repository, and caller session credentials (WAYLODE_TERMINAL_SESSION or WAYLODE_SESSION_ID, plus WAYLODE_SESSION_TOKEN; legacy GITPULSE_* aliases also supported) inherited from a Waylode PTY.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# Waylode terminal orchestration (Option A)

Coding agents launched inside a Waylode terminal automatically inherit session capability environment variables:

- `WAYLODE_TERMINAL_SESSION` (and `WAYLODE_SESSION_ID`, plus legacy `GITPULSE_*` aliases set to the same value) — the caller's terminal session ID.
- `WAYLODE_SESSION_TOKEN` (plus legacy `GITPULSE_SESSION_TOKEN`) — a 256-bit per-session capability token (`gpst_<64-hex>`), stored only as a `SHA-256` digest in memory and revoked the instant the session exits. Never print, log, or commit `WAYLODE_SESSION_TOKEN` or `GITPULSE_SESSION_TOKEN`.

## Option A lineage boundary

A parent session may **only** `list`, `wait`, `read`, `send`, or `close` child sessions that **it spawned itself** (`parent_session_id == caller_session_id`). At most 8 active child sessions may run concurrently per parent (`MAX_CHILDREN_PER_PARENT = 8`). User-opened shells (`parent_session_id == null`), the caller's own session, and sibling sessions owned by other parents are unreachable by design and return a uniform refusal.

## MCP tools (preferred)

Pass `repo_path` as an absolute filesystem path on every call. When the MCP server inherits `WAYLODE_TERMINAL_SESSION` (or `WAYLODE_SESSION_ID` / legacy `GITPULSE_*`) and `WAYLODE_SESSION_TOKEN`, `caller_session_id` and `caller_token` may be omitted; otherwise pass them explicitly from your shell environment without echoing the token.

| Tool | Purpose |
| --- | --- |
| `waylode_terminal_exec` | Run a policy-gated command in an owned `'shell'` session (or spawn a fresh shell when `child_session_id` is omitted, with `keep_open` defaulting to `true` or `false` for ephemeral runs), wait for `OSC 133;D` completion/exit, and return secret-redacted output + exact `exit_code`. Supports multiple parallel shells and standalone MCP callers (`mcp-<pid>`). |
| `waylode_terminal_spawn` | Spawn a scoped child PTY (`launcher`: `"shell"` \| `"claude"` \| `"agy"` \| `"codex"` \| `"manvi"` \| `"grok"`). Optional `cwd` (sandboxed inside `repo_path`), `command` (for `"shell"`, gated through `signal_guard` + command policy), `prompt` (for agent launchers), `model` (agent launchers with a model flag: `claude`, `agy`, `codex`, `grok`), `mode` (`"inspect"` \| `"ask"` (default) \| `"edit"` \| `"auto_review"` \| `"preapproved"`; `"bypass"` is refused over MCP), `settings` (a repo-relative path to an existing settings file inside the repository, `claude` and `agy` only; a file that sets `permissions.defaultMode` to `bypassPermissions` or `disableAllHooks` is refused), `title`, `split` (defaults to `true`, opening a right-hand split pane beside the parent), `rows`, `cols`. `model`, `mode` and `settings` are errors with `launcher: "shell"`. Returns `child_session_id`, `gate_checked`, `gate_notice`, and `baseline_seq`. |
| `waylode_terminal_wait` | Block until `child_session_id` exits, finishes an `OSC 133;D` shell command, reports `OSC 7501` `herdr_status` (`"idle"` / `"done"` / `"blocked"` / `"error"`), matches `pattern` on the rendered 2D screen, or opens/prints a loopback `dev_server_url` (detected both from stdout and silently via kernel socket-table inspection), bounded by `timeout_ms` (default `10000`, max `25000`). Optional `since_seq` (defaults to the latest `baseline_seq` from `spawn` / `send` / `wait`) ensures stale previous-turn statuses never wake a later wait early. Returns `matched`, `timed_out`, `matched_reason`, `event_seq`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, and `dev_server_url`. |
| `waylode_terminal_read` | Read up to `max_lines` (default `80`, max `1000`, `64 KiB`), with optional `offset_lines` paging over `2000`-row scrollback, `grep`, and `last_command_only` extraction from the headless `vt100` 2D screen buffer (handling `\r` progress bars, cursor motion, wrapped lines, and alternate screen cleanly). Every line is scrubbed for secrets and `gpst_` capability tokens (`<redacted>`) before filtering/return. Supports owned children and opted-in repository shells (`owned: false`). Returns `text`, `lines_returned`, `total_lines`, `offset_lines`, `truncated`, `owned`, `last_command`, `last_command_found`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, and `dev_server_url`. |
| `waylode_terminal_send` | Send plain-text `input` (plus trailing newline unless `append_newline: false`) to `child_session_id`. Rejects NUL, `ESC` (`0x1b`), and `Ctrl+Z` (`0x1a`) bytes and runs every non-empty line through `signal_guard` + command policy with bypass disabled. Returns `child_session_id`, `bytes_sent`, `gate_checked`, `gate_notice`, and `baseline_seq`. |
| `waylode_terminal_close` | Terminate `child_session_id`, immediately free its per-parent concurrency slot (`MAX_CHILDREN_PER_PARENT = 8`), and close its split pane in the Waylode window. Idempotent on already-exited child sessions (`already_ended: true`). Returns `child_session_id` and `already_ended`. |
| `waylode_terminal_list` | List active and recently ended child sessions owned by this caller (plus read-only repository `"shell"` tabs with `owned: false` when `include_repo_shells: true` and `allow_agent_shell_reads` is enabled in Settings -> Agents), returning `shell_reads_enabled`, `total`, and `children` (`child_session_id`, `launcher`, `cwd`, `title`, `owned`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, `dev_server_url`). |
| `waylode_terminal_open_file` | Open a repository-relative `path` (at optional 1-based `line` and `column`) in a right-hand `TerminalFilePane` split alongside the caller's terminal pane (`CodeViewer` or `MarkDevViewer`) without stealing keyboard focus. |

## CLI fallback (`waylode-hook term`)

Supports `spawn`, `exec`, `open-file`, `list` (`--include-repo-shells`), `wait`, `read` (`--max-lines` up to `1000`, `--offset-lines`, `--grep`, `--last-command-only`), `send`, and `close`. Uses `WAYLODE_TERMINAL_SESSION` + `WAYLODE_SESSION_TOKEN` inside a Waylode PTY or a caller-scoped standalone `ext-<ppid>` identity outside one.

When MCP tools are unavailable in a subshell, invoke `waylode-hook term`, which automatically reads `WAYLODE_TERMINAL_SESSION` (or `WAYLODE_SESSION_ID` / legacy `GITPULSE_*`) and `WAYLODE_SESSION_TOKEN` from the process environment without exposing them in argv:

```sh
waylode-hook term spawn --launcher shell --command "cargo test" --title "Tests" --split
waylode-hook term wait <child-session-id> --timeout-ms 15000
waylode-hook term read <child-session-id> --max-lines 80
waylode-hook term send <child-session-id> --input "npm run lint"
waylode-hook term close <child-session-id>
waylode-hook term list
waylode-hook term open-file src/main.rs --line 42
```

`waylode-mcp call waylode_terminal_* --write ...` (the one-shot form the `waylode:insights` skill describes) acts for `WAYLODE_TERMINAL_SESSION` inside a Waylode terminal, as the MCP tools do. Outside one, each call is a new `mcp-<pid>` caller that cannot reach a child an earlier call left open, so only a self-contained call is useful there: `waylode_terminal_exec` with `keep_open: false`, or `waylode_terminal_open_file`. `waylode-hook term` outside a Waylode terminal acts for its parent shell (`ext-<ppid>`), so spawn, wait, read and close from one shell script reach the same child; separate agent shell commands are separate parents.

## Workflow guidelines

0. **Shell execution, paging, and opt-in shell reads:** Use `waylode_terminal_exec` to execute commands across fresh or persistent parallel shells without polling. Pass `max_lines` (up to `1000`, `64 KiB`), `offset_lines`, `grep`, or `last_command_only: true` on `waylode_terminal_read`. User-opened repository `"shell"` tabs can be inspected read-only (`owned: false`) with `waylode_terminal_list` (`include_repo_shells: true`), `waylode_terminal_wait`, and `waylode_terminal_read` only when `allow_agent_shell_reads` is enabled in Settings -> Agents; `waylode_terminal_send`, `waylode_terminal_close`, and targeted `waylode_terminal_exec` remain strictly restricted to sessions owned by this caller.

1. **Event-driven waiting:** After `waylode_terminal_spawn` or `waylode_terminal_send`, call `waylode_terminal_wait` rather than looping on `sleep` + `read`. Inspect `matched_reason` (`"ended"`, `"herdr_status"`, `"command_finished"`, `"pattern"`, `"dev_server_ready"`, or `"timeout"`).
1a. **Prefer the agent launcher over a shell.** To run another agent (Antigravity `agy`, Claude Code, Codex, Grok), spawn it with `launcher: "agy"` (or the matching launcher) and pass `prompt`, `mode`, `model` and `settings` as arguments. Do not spawn `launcher: "shell"` and type `agy --mode … -i …` by hand: Waylode then cannot tell the tab is an agent, applies no launch policy, and its permission prompts look like a working shell.
1b. **Watch for a child that needs the user.** After spawning an agent child, call `waylode_terminal_wait`. When it returns `matched_reason: "needs_input"` the child is stopped on a permission dialog or a question that only the user can answer: tell the user which tab (its `title` and `child_session_id`) needs them and what it is asking (`read` the screen), rather than waiting again or answering for them. Never approve a permission prompt on the user's behalf by `send`.
1c. **Allow rules must match the exact command shape.** A settings file's allow rule matches the command as written: `git -C <path> log` is not matched by `Bash(git log:*)`. When a brief mandates a command shape (a `-C <path>` form, an absolute path, a wrapper), write the allow rule for that shape, or tell the child the shape the rules allow.
2. **Honest gate telemetry:** On `spawn` and `send`, check `gate_checked` and `gate_notice` — if the policy sidecar was unreachable, `gate_checked` is `false` rather than silently claiming a clean policy check.
3. **Side-by-side context:** Use `waylode_terminal_open_file` when surfacing plans, diffs, or failing source lines so the user sees the file at the exact line right beside the terminal session.
