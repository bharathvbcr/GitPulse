---
name: gitpulse-tasks
description: Manage GitPulse tasks over MCP — add, list, and inspect tasks in the repository tasks directory following the GitPulse Markdown Task Brief v1 format. Use when an agent needs to create or record tasks, plan upcoming work items, track issues or subtasks in GitPulse, or read existing repository tasks before implementing features.
license: MIT
compatibility: Requires the gitpulse-mcp binary on PATH and an absolute git repository path.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# GitPulse tasks

GitPulse manages repository tasks on disk in the `tasks/` directory as markdown files using the `# Task brief v1` format with YAML frontmatter.

The `gitpulse-mcp` server provides three tools for tasks:
- `gitpulse_add_task`: Mutating tool (policy-gated via harness, recorded in the audit ledger) to create or update a task brief.
- `gitpulse_list_tasks`: Read-only tool to list task summaries, with optional status filtering and priority-based sorting.
- `gitpulse_get_task`: Read-only tool to retrieve full task details and raw file content.

## Before you add or edit tasks

1. Call `gitpulse_list_tasks` with the absolute `repo_path` to inspect existing tasks and avoid duplicate IDs or conflicting work.
2. Read the `ok` field of the response. An empty task list with `ok: false` indicates an error (e.g. invalid repo path or unreadable directory), not an empty task board.
3. If checking a specific task, call `gitpulse_get_task` with `repo_path` and `task_id`.

## Adding a task

Use `gitpulse_add_task` to record a new task:

```json
{
  "repo_path": "/absolute/path/to/repo",
  "title": "Implement OAuth2 and passkey authentication",
  "description": "Add PKCE authorization code flow and passkey credentials support.",
  "status": "ready",
  "priority": 1,
  "severity": "high",
  "kind": "feature",
  "owner": "@alice",
  "due": "2026-05-01",
  "labels": ["auth", "security"],
  "repositories": ["GitPulse"],
  "planned_files": ["src/auth.rs", "src/tokens.rs"],
  "acceptance_criteria": [
    "Support PKCE code exchange flow",
    "Passkey registration and login endpoints verify signatures",
    "Comprehensive unit and integration tests passing"
  ]
}
```

### Parameters for `gitpulse_add_task`

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `repo_path` | string | Yes | Absolute path to the git repository. |
| `title` | string | Yes | Short, clear goal of the task (max 300 characters). |
| `task_id` | string | No | Unique task identifier (e.g. `gp-oauth-auth`). If omitted, auto-generated from title with prefix `gp-`. |
| `description` | string | No | Background context and problem description (max 65,536 characters). |
| `status` | string | No | Task status: `inbox` (default), `backlog`, `ready`, `in_progress`, `review`, `done`. |
| `priority` | integer | No | Priority level: `0` (Urgent), `1` (High), `2` (Normal, default), `3` (Low). |
| `severity` | string | No | Issue severity: `critical`, `high`, `medium`, `low`. |
| `kind` | string | No | Task type: `feature`, `bug`, `refactor`, `chore`, `task`, `spike`, `hotfix`. |
| `owner` | string | No | Assignee or owner handle (e.g. `@alice`). |
| `due` | string | No | Due date in ISO format (e.g. `2026-05-01`). |
| `labels` | string[] | No | Categorical tags or labels (max 64 items, each max 64 characters). |
| `repositories` | string[] | No | Relevant repository names (max 32 items). |
| `planned_files` | string[] | No | Planned file paths for modification (max 256 items). |
| `acceptance_criteria` | string[] | No | List of conditions for completion (definition of done, max 128 items). |
| `logs` | string | No | Raw error logs or execution trace evidence. |
| `overwrite` | boolean | No | Allow replacing an existing task file. Default is `false` (fails if task already exists). |

## Listing tasks

Call `gitpulse_list_tasks`:
- `repo_path` (required): Absolute path to the repository.
- `status` (optional): Filter by task status (`inbox`, `backlog`, `ready`, `in_progress`, `review`, `done`).

Tasks are returned ordered by priority ascending (`0` Urgent before `3` Low), then by status, then by title.

## Reading a task

Call `gitpulse_get_task`:
- `repo_path` (required): Absolute path to the repository.
- `task_id` (required): Unique identifier of the task (e.g. `gp-oauth-auth`).

Returns parsed metadata, acceptance criteria, planned files, logs, and raw markdown content.

## GitPulse Task Brief v1 format specification

Task files reside in `<repo>/tasks/<task-id>.md` and conform to the following format:

```markdown
# Task brief v1

---
id: gp-oauth-auth
title: Implement OAuth2 and passkey authentication
status: ready
priority: 1
severity: high
type: feature
owner: "@alice"
due: 2026-05-01
labels:
  - auth
  - security
repositories:
  - GitPulse
planned_files:
  - src/auth.rs
  - src/tokens.rs
acceptance_criteria:
  - Support PKCE code exchange flow
  - Passkey registration and login endpoints verify signatures
  - Comprehensive unit and integration tests passing
---

## Title
Implement OAuth2 and passkey authentication

## Description
Add PKCE authorization code flow and passkey credentials support.

## Acceptance Criteria
- [ ] Support PKCE code exchange flow
- [ ] Passkey registration and login endpoints verify signatures
- [ ] Comprehensive unit and integration tests passing

## Planned Files
- src/auth.rs
- src/tokens.rs
```

## Safety and policy guarantees

- **Policy Harness Gated**: `gitpulse_add_task` checks repository trust and mutation permissions via `harness::guard_file`. Untrusted checkouts or denied paths fail closed.
- **Audit Ledger Recorded**: Every task creation or modification is logged with timestamp, file path, and tool name in the repository ledger.
- **Atomic Creation & Overwrite Protection**: Task files are written atomically. Without `overwrite: true`, attempts to overwrite an existing task fail immediately to prevent accidental data loss.
- **Path Traversal Guard**: Task IDs are strictly validated to prevent directory traversal or creation outside the `tasks/` directory.
