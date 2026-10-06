---
name: gitpulse-tasks
description: File, import, list, read, complete and delete tasks on the GitPulse task board over MCP — the board a person sees in GitPulse and launches agents from. Use when an agent needs to record follow-up work, plan upcoming tasks, put Markdown task briefs from a repository's tasks/ folder onto the board, read a task's brief before implementing it, mark the task it was launched on done when the work is finished, or delete a duplicate or merged task with a recorded reason.
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
| `gitpulse_complete_task` | write | Move your task to `done` when the work is finished (or `review`, or `in_progress`), with a summary. |
| `gitpulse_delete_task` | write, destructive | Delete a task card that should not exist — a duplicate, or one merged into another — with a reason. |

Every write requires the repository to be **trusted** in GitPulse. An untrusted
repository is refused with `untrusted_repository`; ask the person to open it in
GitPulse and trust it. No write can start an agent run, and only
`gitpulse_delete_task` removes a card.

## Before you file a task

A person reads every card on the board. Thirty narrow cards from one review are
thirty things to triage; five that each own a concern are a plan. File the
fewest tasks that cover the work.

1. Call `gitpulse_list_tasks` with the absolute `repo_path` and read what is
   already open. `repository: null` means no task has ever been filed under
   this repository — not that its tasks are all done.
2. **Group before you file.** When you have several findings, sort them by the
   concern they share — the same subsystem, the same root cause, the same fix —
   and file one task per concern, each finding an acceptance criterion. One
   task per finding is the wrong shape.
3. **Fold into an open task** when your work belongs to one: read it with
   `gitpulse_get_task`, then call `gitpulse_add_task` with its `item_id` as
   `task_id` and `overwrite: true`. Every field you send replaces the card's,
   so send its existing title, description and criteria **plus** yours. Every
   field you leave out — status, priority, severity, owner, due date, labels,
   logs — stays exactly as the card had it, as do its column position and links.
4. File a new task only for work no open task covers.

`gitpulse_add_task` holds you to this. A new task that looks like open work —
two shared title words, or one shared title word and a shared label that is not
just that word again — is refused with `related_tasks_exist`, and nothing is
filed. The refusal lists the related open tasks (`item_id`, status, title, and
what they share). Read them, then either fold your work into one (step 3), or,
when it is genuinely separate work, call again with `reviewed_related` naming
**every** listed `item_id`. Naming some of them is refused again. A finished
(`done`) task never counts, and an overwrite is never checked. The success
answer carries `related_check`: how many related open tasks there were, how many
open tasks were compared, and whether that was all of them (`scan_complete`).

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
`overwrite: true` to replace the task's content — under the `task_id` it was
filed with, or under a board `item_id`, which is how you extend a task the
person made on the board. Its column position and links on the board are kept.
A task the person deleted on the board stays deleted (`deleted_on_board`);
choose another `task_id`.

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
| `overwrite` | no | Replace an existing task's content, by `task_id` or board `item_id`. |
| `reviewed_related` | no | At most 25 `item_id`s: every task a `related_tasks_exist` refusal listed, once you have read them and judged this separate. |

## Importing briefs from `tasks/`

`gitpulse_import_tasks` reads every `*.md` file in `tasks/` (or `tasks_dir`)
and puts each brief on the board under its key: the brief's `id`, else its file
name. It is safe to run again. Briefs already on the board are left alone unless
you pass `replace: true`.

Import is the person's bulk path and is **not** checked for related open tasks.
If you are the one writing the briefs, group them first, exactly as in
[Before you file a task](#before-you-file-a-task): one brief per concern.

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

## Finishing a task

When GitPulse launched you on a task, the brief's first lines name it:
`Task: <id> (revision N)`. When every acceptance criterion is met and your
verification passed, move it to `done`:

```json
{
  "repo_path": "/absolute/path/to/repo",
  "task_id": "<id from the Task: line>",
  "summary": "What changed, and the checks you ran and their results."
}
```

- `status` defaults to `done`. Use `review` instead when a person must judge
  the result before it counts as finished, and `in_progress` when you start.
- Only the status changes. The `summary` (at most 4,000 characters) is appended
  to the task's logs so the person sees what you did; nothing else is rewritten,
  and a person's edit made while you worked is kept.
- Pass `expected_revision` (the `N` from `Task: <id> (revision N)`) to be
  refused with `revision_conflict` if the task changed since you read it; then
  re-read it with `gitpulse_get_task` before deciding.
- The same call twice answers `unchanged`. A task already `done` is never
  reopened (`already_done`): ask the person.
- Do not mark a task done that you did not finish, or whose verification failed.
  Say what is left instead, and use `review`.

## Deleting a task

`gitpulse_delete_task` removes a card from the board — the same delete as the
board's own **Delete**. Use it only for a task that should not exist: a
duplicate, or one you merged into another. Never delete a task to finish it;
move it to `done` instead.

```json
{
  "repo_path": "/absolute/path/to/repo",
  "task_id": "gp-oauth-auth",
  "reason": "Merged into gp-auth-overhaul, which carries both sets of criteria."
}
```

- `task_id` is the key it was filed with, or an `item_id` from
  `gitpulse_list_tasks`. `reason` is required (at most 1,000 characters).
- The reason is appended to the task's logs first, so it is in the task's
  history and in the deletion itself. If the task changes between the two, it
  is not deleted and keeps the reason; with `expected_revision` that is refused
  with `revision_conflict`, without it the delete is retried on top of the
  person's edit and the reason is not appended twice.
- The delete is soft: the row and its history stay in the GitPulse profile and
  the id is never reused — filing the same `task_id` again is refused with
  `deleted_on_board`. There is **no undelete** over MCP; only the person can
  restore it.
- A task linked to other repositories too is refused with `shared_task`, since
  deleting it here deletes it there. Ask the person.
- The same call twice answers `unchanged`. A task of another repository is
  `not_found`.

## Guarantees

- **One board.** These tools read and write the store the task board renders,
  under the same repository record GitPulse registers when the person opens the
  repository. Linked worktrees share one record.
- **Trusted repositories only** for writes. A refused or invalid request
  changes nothing.
- **Idempotent.** Keys are stable per repository, concurrent writers converge on
  one card per key, and deletions on the board are respected.
- **Recorded.** Every write is a revision in the board's own event history.
