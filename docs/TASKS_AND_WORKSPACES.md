# Tasks and workspaces

GitPulse is the successor to the deprecated
[LiquiTask](https://github.com/bharathvbcr/LiquiTask) workbench. Use this app —
not LiquiTask — for new boards and agent handoff.

Use **Tasks**, beside Fleet above the repository tabs, to organize work across
repositories. Use **Work → Tasks** for the active repository. The command palette
also provides **Open Tasks — global and workspace Kanban boards** and
**Open repository Tasks — issues, bugs and features**.

## One task, several boards

| Scope | What appears |
| --- | --- |
| All | Tasks across the profile |
| Workspace | Tasks assigned to that home workspace or linked to a member repository |
| Repository | Tasks linked to that repository |

These scopes show the same saved records. Editing a task changes it in every
relevant board. Saved workspaces retain membership independently of open tabs;
adding an open repository through the workspace or task editor registers it when
necessary. Closing its tab does not remove its membership.

A saved task needs at least one linked repository and a primary repository.
An optional home workspace groups it without replacing those repository links.
Repository execution tasks and leases shown in **Work → Overview** are separate
from these profile task records.

## Create and organize

1. Choose a scope and select **New task**. Enter a title and description, or use
   notes to seed them. Link the repositories the task concerns.
2. Set its type, status and details: priority, severity, owner, due date, labels,
   acceptance criteria and optional home workspace. Repository membership and
   enhancement locks are available in the editor details.
3. Save, then use **Board** or **List** to organize the same results. Statuses are
   Inbox, Backlog, Ready, In progress, Review and Done.

Search uses the store's full-text query. **Filters** narrows loaded cards by
priority, type, owner, label and due date. **Due soon** means from now through the
next seven days; overdue tasks have their own filter. Facet choices and **Select
all** use loaded cards. Column totals and **Load more** expose additional results;
an empty filtered page does not establish that the whole profile has no matches.

Click a card to edit it. Command/Ctrl-click toggles selection; Shift-click selects
a visible range. Right-click a card, or press Shift+F10 on a focused card, for
Open, Quick Enhance, Duplicate, Copy, Move to, Set priority and Delete. Single-task
actions disappear for multiple selections. Duplicate opens a draft for review
and save. Right-click an empty column to create a task with that status.

Drag cards to change status or position. Bulk status/priority edits can partially
succeed; the board reports failures. They are not a transaction across all tasks.

## Draft and review with Manvi

In the editor, **Draft with Manvi** uses notes; **Improve with Manvi** uses the
task text. GitPulse saves the task first, then asks the configured Manvi provider
and model for title and/or description suggestions. A linked repository and a
title or notes are required. **Save task** also works with notes alone: the first
line supplies a missing title and the notes are appended to an existing
description. Extracted notes are cleared so a later save cannot reapply them over
accepted wording. Oversized combined descriptions are refused without discarding
the notes.

**Task model settings** lets you choose a provider and model for suggestions in
this editor and reload Manvi's configuration. This uses Manvi's task provider
configuration, independently of the application header's model selection. Missing
configuration is shown before generation; no model or endpoint is substituted.

The proposed title and description appear beside their editable fields. Choose
**Use this title**, **Use this description** or **Use both**. **Not now** hides the
suggestion while retaining it in Manvi history. Field locks prevent enhancement
of the locked title or description; acceptance requires saved edits and checks
the saved task revision. A running suggestion blocks another generation from the
same surface. If an action's reply is lost, **Retry pending action** reconciles
the same request before editing or closing. Once acceptance is confirmed, a failed
task refresh retries only the read.

**Quick Enhance** opens a separate sheet for an existing task. It exposes details
omitted from compact cards, lets you select unlocked fields, and shows proposal
history with **Apply enhancement** and dismissal controls. It shares the editor's
enhancement review and retry lifecycle. Automatic suggestion
settings are separate from this manual request; generated text still needs review.

## Copy or launch an agent

**Copy for agent** adds task instructions to a brief; it does not launch an agent.

| Copy source | What is copied |
| --- | --- |
| New, unsaved task | Current draft fields with extracted notes, explicitly labeled as an unsaved draft |
| Saved task editor | The saved revision; unsaved edits are excluded and the copy feedback says so |
| Board selection | Up to eight saved task briefs per action, with partial results reported |
| Context menu → Copy → Saved brief | The canonical saved brief without the extra agent instructions |

Saved briefs include the task, ordered repository references and home workspace
with their revisions. Stale or incomplete snapshots are refused. Save edits
before copying if they should be part of the agent's brief.

Task run controls prepare a saved revision and the selected checkout before
launch. A terminal handoff opens a dedicated task-bound session in the existing
[terminal dock](TERMINAL.md). It runs under that CLI's configured permissions;
finishing the process does not accept the task or mark it Done.

Managed Codex runs use a separate Manvi host protocol for configuration checks,
structured questions/approvals and completion receipts. A saved decision and its
delivery to the provider are separate states. Terminal handoffs do not produce
those structured callbacks. Managed Claude and full crash recovery remain
qualification gaps; see the [implementation contract](AGENTIC_WORKSPACES_PLAN.md)
and [architecture](ARCHITECTURE.md#agentic-workbench-implemented-core-broader-qualification-in-progress).

## Keyboard controls

These board shortcuts apply while focus is inside the board, outside editor
fields and dialogs. Card-specific shortcuts require a focused card.

| Key | Action |
| --- | --- |
| `/` | Focus task search |
| `n` | New task |
| Command/Ctrl+A | Select visible loaded cards |
| Command/Ctrl+Shift+C | Copy selected tasks for an agent |
| `o` | Open the single selected task |
| Shift+F10 / Context Menu key | Open the focused card's menu |
| `e` on a card | Quick Enhance |
| Left / Right on a card | Move to the neighboring status |
| Delete / Backspace | Ask to delete the selection or focused card |
| Escape | Dismiss the menu or clear selection |

## Deletion and verification limits

Deletion asks for confirmation and removes the task from every board. History
stays in the store and queued suggestions are dropped, but this screen cannot
restore the same task ID. Each board delete pass attempts at most 50 tasks;
failed and skipped tasks remain selected. An uncertain editor deletion offers
**Retry delete** to reconcile the same request before further editing.

This guide describes the current source paths in `src/lib/workbench/`,
`TaskBoard.svelte`, `TaskEditor.svelte`, `TaskManviAssist.svelte` and
`QuickEnhanceSheet.svelte`. Unit and source contracts cover helpers and wiring;
they do not prove native clipboard delivery, physical drag behavior, installed
provider permissions or OS notification delivery. The implementation contract
retains the broader planned features and their outstanding verification gates.
