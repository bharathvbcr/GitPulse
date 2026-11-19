---
name: gitpulse-terminal
description: Orchestrate scoped child terminal sessions and split-pane file previews inside GitPulse over MCP or gitpulse-hook term. Use when an agent running in a GitPulse PTY wants to spin up a child shell or sub-agent (Claude Code, Antigravity, Codex, Manvi, Grok), wait on semantic prompt/agent idle or a dev-server URL without sleep-polling, read secret-redacted 2D terminal screen output, send policy-gated input, or open a repository file in a side-by-side split pane.
license: MIT
compatibility: Requires a running GitPulse desktop window on macOS, a trusted git repository, and caller session credentials (GITPULSE_TERMINAL_SESSION or GITPULSE_SESSION_ID, plus GITPULSE_SESSION_TOKEN) inherited from a GitPulse PTY.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# GitPulse terminal orchestration (Option A)

Coding agents launched inside a GitPulse terminal automatically inherit session capability environment variables:

- `GITPULSE_TERMINAL_SESSION` (and `GITPULSE_SESSION_ID`, set to the same value) — the caller's terminal session ID.
- `GITPULSE_SESSION_TOKEN` — a 256-bit per-session capability token (`gpst_<64-hex>`), stored only as a `SHA-256` digest in memory and revoked the instant the session exits. Never print, log, or commit `GITPULSE_SESSION_TOKEN`.

## Option A lineage boundary

A parent session may **only** `list`, `wait`, `read`, `send`, or `close` child sessions that **it spawned itself** (`parent_session_id == caller_session_id`). At most 8 active child sessions may run concurrently per parent (`MAX_CHILDREN_PER_PARENT = 8`). User-opened shells (`parent_session_id == null`), the caller's own session, and sibling sessions owned by other parents are unreachable by design and return a uniform refusal.

## MCP tools (preferred)

Pass `repo_path` as an absolute filesystem path on every call. When the MCP server inherits `GITPULSE_TERMINAL_SESSION` (or `GITPULSE_SESSION_ID`) and `GITPULSE_SESSION_TOKEN`, `caller_session_id` and `caller_token` may be omitted; otherwise pass them explicitly from your shell environment without echoing the token.

| Tool | Purpose |
| --- | --- |
| `gitpulse_terminal_exec` | Run a policy-gated command in an owned `'shell'` session (or spawn a fresh shell when `child_session_id` is omitted, with `keep_open` defaulting to `true` or `false` for ephemeral runs), wait for `OSC 133;D` completion/exit, and return secret-redacted output + exact `exit_code`. Supports multiple parallel shells and standalone MCP callers (`mcp-<pid>`). |
| `gitpulse_terminal_spawn` | Spawn a scoped child PTY (`launcher`: `"shell"` \| `"claude"` \| `"agy"` \| `"codex"` \| `"manvi"` \| `"grok"`). Optional `cwd` (sandboxed inside `repo_path`), `command` (for `"shell"`, gated through `signal_guard` + command policy), `prompt` (for agent launchers), `title`, `split` (defaults to `true`, opening a right-hand split pane beside the parent), `rows`, `cols`. Returns `child_session_id`, `gate_checked`, `gate_notice`, and `baseline_seq`. |
| `gitpulse_terminal_wait` | Block until `child_session_id` exits, finishes an `OSC 133;D` shell command, reports `OSC 7501` `herdr_status` (`"idle"` / `"done"` / `"blocked"` / `"error"`), matches `pattern` on the rendered 2D screen, or opens/prints a loopback `dev_server_url` (detected both from stdout and silently via kernel socket-table inspection), bounded by `timeout_ms` (default `10000`, max `25000`). Optional `since_seq` (defaults to the latest `baseline_seq` from `spawn` / `send` / `wait`) ensures stale previous-turn statuses never wake a later wait early. Returns `matched`, `timed_out`, `matched_reason`, `event_seq`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, and `dev_server_url`. |
| `gitpulse_terminal_read` | Read up to `max_lines` (default `80`, max `1000`, `64 KiB`), with optional `offset_lines` paging over `2000`-row scrollback, `grep`, and `last_command_only` extraction from the headless `vt100` 2D screen buffer (handling `\r` progress bars, cursor motion, wrapped lines, and alternate screen cleanly). Every line is scrubbed for secrets and `gpst_` capability tokens (`<redacted>`) before filtering/return. Supports owned children and opted-in repository shells (`owned: false`). Returns `text`, `lines_returned`, `total_lines`, `offset_lines`, `truncated`, `owned`, `last_command`, `last_command_found`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, and `dev_server_url`. |
| `gitpulse_terminal_send` | Send plain-text `input` (plus trailing newline unless `append_newline: false`) to `child_session_id`. Rejects NUL, `ESC` (`0x1b`), and `Ctrl+Z` (`0x1a`) bytes and runs every non-empty line through `signal_guard` + command policy with bypass disabled. Returns `child_session_id`, `bytes_sent`, `gate_checked`, `gate_notice`, and `baseline_seq`. |
| `gitpulse_terminal_close` | Terminate `child_session_id`, immediately free its per-parent concurrency slot (`MAX_CHILDREN_PER_PARENT = 8`), and close its split pane in the GitPulse window. Idempotent on already-exited child sessions (`already_ended: true`). Returns `child_session_id` and `already_ended`. |
| `gitpulse_terminal_list` | List active and recently ended child sessions owned by this caller (plus read-only repository `"shell"` tabs with `owned: false` when `include_repo_shells: true` and `allow_agent_shell_reads` is enabled in Settings -> Agents), returning `shell_reads_enabled`, `total`, and `children` (`child_session_id`, `launcher`, `cwd`, `title`, `owned`, `ended`, `exit_code`, `command_running`, `last_exit_code`, `herdr_status`, `dev_server_url`). |
| `gitpulse_terminal_open_file` | Open a repository-relative `path` (at optional 1-based `line` and `column`) in a right-hand `TerminalFilePane` split alongside the caller's terminal pane (`CodeViewer` or `MarkDevViewer`) without stealing keyboard focus. |

## CLI fallback (`gitpulse-hook term`)

Supports `spawn`, `exec`, `open-file`, `list` (`--include-repo-shells`), `wait`, `read` (`--max-lines` up to `1000`, `--offset-lines`, `--grep`, `--last-command-only`), `send`, and `close`. Uses `GITPULSE_TERMINAL_SESSION` + `GITPULSE_SESSION_TOKEN` inside a GitPulse PTY or a caller-scoped standalone `ext-<ppid>` identity outside one.

When MCP tools are unavailable in a subshell, invoke `gitpulse-hook term`, which automatically reads `GITPULSE_TERMINAL_SESSION` (or `GITPULSE_SESSION_ID`) and `GITPULSE_SESSION_TOKEN` from the process environment without exposing them in argv:

```sh
gitpulse-hook term spawn --launcher shell --command "cargo test" --title "Tests" --split
gitpulse-hook term wait <child-session-id> --timeout-ms 15000
gitpulse-hook term read <child-session-id> --max-lines 80
gitpulse-hook term send <child-session-id> --input "npm run lint"
gitpulse-hook term close <child-session-id>
gitpulse-hook term list
gitpulse-hook term open-file src/main.rs --line 42
```

`gitpulse-mcp call gitpulse_terminal_* --write ...` (the one-shot form the gitpulse-insights skill describes) acts for `GITPULSE_TERMINAL_SESSION` inside a GitPulse terminal, as the MCP tools do. Outside one, each call is a new `mcp-<pid>` caller that cannot reach a child an earlier call left open, so only a self-contained call is useful there: `gitpulse_terminal_exec` with `keep_open: false`, or `gitpulse_terminal_open_file`. `gitpulse-hook term` outside a GitPulse terminal acts for its parent shell (`ext-<ppid>`), so spawn, wait, read and close from one shell script reach the same child; separate agent shell commands are separate parents.

## Workflow guidelines

0. **Shell execution, paging, and opt-in shell reads:** Use `gitpulse_terminal_exec` to execute commands across fresh or persistent parallel shells without polling. Pass `max_lines` (up to `1000`, `64 KiB`), `offset_lines`, `grep`, or `last_command_only: true` on `gitpulse_terminal_read`. User-opened repository `"shell"` tabs can be inspected read-only (`owned: false`) with `gitpulse_terminal_list` (`include_repo_shells: true`), `gitpulse_terminal_wait`, and `gitpulse_terminal_read` only when `allow_agent_shell_reads` is enabled in Settings -> Agents; `gitpulse_terminal_send`, `gitpulse_terminal_close`, and targeted `gitpulse_terminal_exec` remain strictly restricted to sessions owned by this caller.

1. **Event-driven waiting:** After `gitpulse_terminal_spawn` or `gitpulse_terminal_send`, call `gitpulse_terminal_wait` rather than looping on `sleep` + `read`. Inspect `matched_reason` (`"ended"`, `"herdr_status"`, `"command_finished"`, `"pattern"`, `"dev_server_ready"`, or `"timeout"`).
2. **Honest gate telemetry:** On `spawn` and `send`, check `gate_checked` and `gate_notice` — if the policy sidecar was unreachable, `gate_checked` is `false` rather than silently claiming a clean policy check.
3. **Side-by-side context:** Use `gitpulse_terminal_open_file` when surfacing plans, diffs, or failing source lines so the user sees the file at the exact line right beside the terminal session.
