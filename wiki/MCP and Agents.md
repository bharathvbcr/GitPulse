# MCP and Agents

`gitpulse-mcp` is GitPulse's **read-only** control plane for coding agents. It never checks out a branch, writes a file, or takes a task lease. Ask it what is true; mutate through the agent that already holds the writer lease.

Protocol: [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) (`server/discover`, per-request `_meta`, cacheable `tools/list`). Legacy `initialize` (2024-11-05 / 2025-11-25) still works.

## Install the server

From a GitPulse checkout:

```sh
npm run mcp:install
npm run mcp:doctor
```

`mcp:install` puts this tree's `gitpulse-mcp` on `PATH`. `mcp:doctor` distinguishes **absent**, **unresponsive**, and **stale** from **matching**. A missing server must not look like a current one.

The packaged app copies `plugins/gitpulse/` into `Contents/Resources/plugin`. Settings can copy the Codex / MCP manifests and will name the binary path, or why it could not be found.

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

## Package layout

One canonical package: [`plugins/gitpulse/`](https://github.com/bharathvbcr/GitPulse/tree/main/plugins/gitpulse)

- Codex: `.codex-plugin/plugin.json` + `.mcp.json`
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
| `gitpulse_ledger_events` | Policy verdicts and recorded mutations |
| `gitpulse_task_view` | DevCouncil tasks/leases, when a store exists |
| `gitpulse_codeintel_search` / `_impact` / `_dependencies` / `_trace` / `_dead_symbols` | In-process DevMap code graph (schema 20). The GUI also exposes neighbors, explore, affected tests, clones, and layered impact — those are Tauri commands today, not MCP tools. |
| `gitpulse_provenance` | Verification freshness for a commit |

Prompts: `gitpulse_preflight`, `gitpulse_collision_triage`, `gitpulse_handoff`, `gitpulse_session_brief`.

Resources use `gitpulse://<facet>{+repo_path}`; `gitpulse://server/manifest` describes the surface.

## Honesty rules the server already enforces

- Unscanned worktrees are counted, never implied clean.
- A missing or schema-mismatched code graph is `available: false` with a reason, not an empty hit list.
- Treat `walk_incomplete` / truncation fields as "floor, not complete" — the same honesty rule the Diff and Map panels enforce.
- Agent worktrees are recognised from `/.<agent>/worktrees/` layout, never from a branch name.
- Read every facet's `ok` / `error` / `available` field. Empty + `ok: false` means the check did not run.

## Headless catch-up

When the GUI is closed, `gitpulsed` can still append attribution (transcripts, reflog) to the ledger:

```sh
gitpulsed --interval 300 /path/to/repo
```

It serves no MCP requests (that is `gitpulse-mcp`) and never takes a lease. See [[Architecture]].
