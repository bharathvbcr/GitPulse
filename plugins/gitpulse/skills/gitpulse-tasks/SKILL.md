---
name: gitpulse-tasks
description: File, import, list and read tasks on the GitPulse task board over MCP — the board a person sees in GitPulse and launches agents from. Use when an agent needs to record follow-up work, plan upcoming tasks, put Markdown task briefs from a repository's tasks/ folder onto the board, or read a task's brief before implementing it.
license: MIT
compatibility: Requires the gitpulse-mcp binary on PATH, an absolute git repository path, and a repository the person has trusted in GitPulse.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# GitPulse tasks

Tasks live on the **GitPulse task board** — the Tasks view a person opens in
GitPulse, where each card can be handed to an agent. The `gitpulse-mcp` server
writes to and reads from that board directly. A task you file appears there
within a couple of seconds while GitPulse's window is on screen, and as soon as the
window is shown otherwise.

| Tool | Kind | What it does |
| --- | --- | --- |
| `gitpulse_add_task` | write | File one task on the board for a repository. |
| `gitpulse_import_tasks` | write | Put every Markdown brief in the repository's `tasks/` folder onto the board. |
| `gitpulse_list_tasks` | read | List the board's tasks for a repository, in board order. |
| `gitpulse_get_task` | read | Read one task with its canonical agent brief. |

Both writes require the repository to be **trusted** in GitPulse. An untrusted
repository is refused with `untrusted_repository`; ask the person to open it in
GitPulse and trust it. Neither write can delete a task or start an agent run.

## Before you file a task

1. Call `gitpulse_list_tasks` with the absolute `repo_path` to see what is
   already there, so you do not file a duplicate under a different `task_id`.
2. `repository: null` in the answer means no task has ever been filed under
   this repository. It does not mean its tasks are all done.

## Filing a task

```json
{
  "repo_path": "/absolute/path/to/repo",
  "task_id": "gp-oauth-auth",
  "title": "Implement OAuth2 and passkey authentication",
  "description": "Add PKCE authorization code flow and passkey credentials support.",
  "status": "ready",
  "priority": 1,
  "severity": "high",
  "kind": "feature",
  "owner": "@alice",
  "due": "2026-05-01",
  "labels": ["auth", "security"],
  "planned_files": ["src/auth.rs", "src/tokens.rs"],
  "acceptance_criteria": [
    "Support PKCE code exchange flow",
    "Passkey registration and login endpoints verify signatures"
  ]
}
```

`task_id` is the task's stable key. Filing the same `task_id` again is refused
with `already_exists`, so a retried call never makes a duplicate card. Pass
`overwrite: true` to replace the task's content; its column position and links
on the board are kept. A task the person deleted on the board stays deleted
(`deleted_on_board`); choose another `task_id`.

| Parameter | Required | Rule |
| --- | --- | --- |
| `repo_path` | yes | Absolute path to the repository (any of its worktrees). |
| `title` | yes | One line, at most 300 characters. |
| `task_id` | no | Letters, digits, `-`, `_`, `.`; at most 96 bytes. Defaults to `gp-` plus the title's slug. |
| `description` | no | Markdown, at most 65,536 bytes including folded sections. |
| `status` | no | `inbox` (default), `backlog`, `ready`, `in_progress`, `review`, `done`. |
| `priority` | no | `0` urgent, `1` high, `2` normal (default), `3` low. |
| `severity` | no | `none` (default), `low`, `medium`, `high`, `critical`. |
| `kind` | no | Free text, at most 64 bytes; default `feature`. |
| `owner` | no | At most 300 bytes. |
| `due` | no | `YYYY-MM-DD`, or Unix seconds. |
| `labels` | no | At most 64, each at most 128 bytes. |
| `acceptance_criteria` | no | At most 128, each at most 4,096 bytes. |
| `planned_files` | no | At most 256. The board has no planned-files field, so they are kept as a `## Planned files` section of the description. |
| `repositories` | no | Other related repository names, kept as a `## Related repositories` section. |
| `logs` | no | Raw evidence kept verbatim, at most 256 KiB. |
| `overwrite` | no | Replace an existing task's content. |

## Importing briefs from `tasks/`

`gitpulse_import_tasks` reads every `*.md` file in `tasks/` (or `tasks_dir`)
and puts each brief on the board under its key: the brief's `id`, else its file
name. It is safe to run again. Briefs already on the board are left alone unless
you pass `replace: true`.

Every file is accounted for. Each entry in `entries` has an `outcome`:
`created`, `updated`, `unchanged`, `already_present`, `deleted_on_board`,
`invalid` (with the reason, e.g. an unknown status), or `failed`. `ok` is true
only when every file was placed. Read `invalid` before you report success.
Symbolic links, non-UTF-8 files, files over 1 MiB, and two files claiming one
key are reported, not read.

## Brief format

A brief is YAML frontmatter followed by an optional Markdown body. Where the two
disagree, the frontmatter wins; the body only fills fields the frontmatter left
out.

```markdown
---
id: gp-oauth-auth
title: "Implement OAuth2 and passkey authentication"
status: ready
priority: 1
severity: high
type: feature
labels: [auth, security]
planned_files:
  - src/auth.rs
acceptance_criteria:
  - Support PKCE code exchange flow
---

# Task brief v1

## Description
Add PKCE authorization code flow and passkey credentials support.

## Acceptance criteria
- [ ] Support PKCE code exchange flow
```

The frontmatter is a strict YAML subset: `key: value`, `key: [a, b]`, and
`- item` block lists. Quote a value that begins with a quote or `[`. Unknown
statuses, priorities and severities are errors, not guesses. A body without
frontmatter — what GitPulse's **Copy saved brief** produces, or a hand-written
`# Title` file — is accepted too.

## Reading a task

`gitpulse_get_task` takes the `task_id` you filed it with, or an `item_id` from
`gitpulse_list_tasks`. It returns the stored task and `brief`: the same Markdown
GitPulse hands an agent it launches on that task. Work from that brief.

## Guarantees

- **One board.** These tools read and write the store the task board renders,
  under the same repository record GitPulse registers when the person opens the
  repository. Linked worktrees share one record.
- **Trusted repositories only** for writes. A refused or invalid request
  changes nothing.
- **Idempotent.** Keys are stable per repository, concurrent writers converge on
  one card per key, and deletions on the board are respected.
- **Recorded.** Every write is a revision in the board's own event history.
