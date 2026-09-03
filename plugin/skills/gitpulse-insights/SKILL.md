---
name: gitpulse-insights
description: Read GitPulse repository insights over MCP — worktrees, agent sessions, uncommitted changes, overlapping dirty files, ledger verdicts, and code-graph availability. Use when an agent needs a honest snapshot of a checkout before editing, when multiple worktrees or coding agents are in flight, or when the user asks what is happening in a GitPulse-managed repo.
license: MIT
compatibility: Requires the gitpulse-mcp binary on PATH (or GITPULSE_MCP_PATH) and an absolute git repository path.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# GitPulse insights

GitPulse's MCP server is **read-only**. It never checks out a branch, writes a file, or takes a task lease. Ask it what is true, then mutate through the agent that already holds the writer lease.

## Before you edit

1. Call `gitpulse_insights` with the absolute `repo_path`.
2. Read every facet's `ok` / `error` / `available` field. An empty list with `ok: false` means the check did not run — not that the repository is clean.
3. If `collisions.overlapping_files > 0`, or `collisions.unscanned_worktrees > 0`, call `gitpulse_collision_risk` and `gitpulse_change_context` for the worktree you are about to touch.
4. Prefer `gitpulse_change_context` over guessing the branch, dirty files, or parked merge/rebase of a worktree.

## Tool map

| Tool | When |
| --- | --- |
| `gitpulse_insights` | First look at a repo |
| `gitpulse_change_context` | About to edit one worktree |
| `gitpulse_collision_risk` | Parallel agents or several worktrees |
| `gitpulse_active_changes` | Need the file list, not just counts |
| `gitpulse_status` | Narrower status (ledger + worktrees + codeintel) |
| `gitpulse_ledger_events` | Policy verdicts and recorded mutations |
| `gitpulse_task_view` | DevCouncil tasks and leases, when a store exists |
| `gitpulse_codeintel_*` | Symbol search, impact, dependencies, trace, dead symbols |
| `gitpulse_provenance` | Verification freshness for a commit |

## Honesty rules this server already enforces

- Unscanned worktrees are counted, never implied clean.
- A missing code graph is `available: false` with a reason, not an empty hit list.
- Agent worktrees are recognised from `/.<agent>/worktrees/` layout, never from a branch name.

Pass `repo_path` as an absolute filesystem path on every call. The protocol is MCP `2026-07-28`: include `_meta.io.modelcontextprotocol/protocolVersion` and `clientCapabilities` on each request. Legacy `initialize` still works for older clients.
