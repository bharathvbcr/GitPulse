# Tasks and workspaces

GitPulse is the successor to the deprecated
[LiquiTask](https://github.com/bharathvbcr/LiquiTask) workbench. Use this app —
not LiquiTask — for new boards and agent handoff.

Profile tasks and briefs are **Manvi** workbench modules. Repository execution
tasks and leases shown in **Work → Overview** are **DevCouncil** store modules.
GitPulse presents both; it does not reimplement them.

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

A workspace with no member repositories still creates tasks. The board says so
rather than withdrawing the action — the navigator marks the workspace
**· Empty**, the header repeats what the sheet will ask for, and the sheet's
first control is where a repository is linked. **New task**, the empty board's
own button, the column **+**, quick add and the sheet all read one answer, so
none of them can offer what another refuses.

## Create and organize

The fastest way in is the **quick add** line above the board. Type a title and
tag the rest inline; the preview under the field shows what will be created
before you press Return:

| Marker | Sets | Example |
| --- | --- | --- |
| `!` | Priority | `!urgent`, `!high`, `!1` |
| `#` | Label (repeatable) | `#ci #flake` |
| `@` | Owner | `@ada` |
| `~` | Task type | `~bug` |
| `^` | Repository | `^gitpulse` |
| `due:` | Due date | `due:friday`, `due:tomorrow`, `due:2026-10-02`, `due:+3d` |
| `::` | Description | `Fix the retry loop :: it drops the last attempt` |

Markers alone never create a task — a line with no title is refused and says so.
Shift+Return opens the full editor with everything already parsed, and `a`
focuses the field from anywhere on the board. Where the scope supplies no
repository — an empty workspace — the line needs `^name`, and the refusal names
both that marker and Shift+Return rather than only failing.

The line has two modes, and the choice is remembered per profile and restored by
**Reset board view**. **Manual** saves what you typed. **Draft** saves exactly
the same task and *then* asks the configured model for a title and description,
opening **Quick Enhance** on the new task so you can accept or reject them. Three
things follow from that order, and the preview states them before you press
Return:

* The card reaches the board carrying your own words. A line with no title is
  refused in this mode too, so a placeholder task never exists.
* The markers are never in scope for the model. It can propose a title and a
  description and nothing else, so priority, labels, owner, type, repository and
  due date are whatever you typed.
* A model that is not configured, cannot be reached, or is refused costs you
  nothing: the task is already saved, and the sheet says what went wrong with
  the same **Retry Manvi configuration** and **Start again** controls the editor
  has. Closing the sheet keeps the suggestion in history.

For anything longer:

1. Choose a scope and select **New task**. **Repositories** leads the sheet,
   because a task cannot be saved without one. Then enter a title and
   description, or use notes to seed them.
2. Set its type, status and details: priority, severity, owner, due date, labels,
   acceptance criteria and optional home workspace. They sit in the second
   column, beside the description, for a draft and a saved task alike.
3. Save, then use **Board** or **List** to organize the same results. Statuses are
   Inbox, Backlog, Ready, In progress, Review and Done.

### Choosing repositories

One control links repositories and picks the primary among them. It is a
dropdown, and its closed label is the answer: "2 repositories linked · primary
GitPulse", or "No repository linked yet — a task needs one." Open it to check a
repository to link it, then choose **Primary** on any linked row; the label
updates without the dropdown closing, so linking several in a row is one
gesture. Past six repositories it offers a filter; a linked repository is never
hidden by it, and is marked **Linked** when it is only on screen because you
linked it. Repositories keep catalog order, so a row cannot move out from under
the pointer that just checked it.

The dropdown closes on Escape — without closing the task behind it — on a click
outside, and when the sheet scrolls, because a popover that outlives the control
it is anchored to lies about what it is editing.

When the task has a home workspace, its member repositories are marked **In
workspace**. Linking one that is not a member is allowed and the sheet says so —
on the sheet itself, not inside the dropdown, because it is a consequence of the
current links and something to act on rather than something to go looking for —
with **Add to workspace** to close the gap. Nothing is joined silently. A
membership the sheet could not read is reported as unread rather than drawn as
an empty workspace.

The navigator's **Repositories +** adds to the profile, and in a workspace scope
it also joins what it adds to that workspace — its label names the workspace, as
does the confirmation. It offers the repositories already open in GitPulse, the
registered ones that workspace has not joined yet, and a folder picker.

A saved task's sheet has two panes. **Task** is the whole task in two columns:
on the left, repository links, title, description and acceptance criteria, with
the model's suggestions under the field each one would replace; on the right,
status, priority, type, severity, due date, owner, labels, home workspace,
notifications, and the drafting controls and suggestion history that produce
those suggestions. Writing a task, scheduling it and asking for better wording
are one sitting, and they used to be three panes — with the AI pane's output
drawn on the Task pane, so you pressed a button on one pane and read the result
on another. On a narrow window the two columns become one, in that order.

**Agent** hands the saved revision to a coding agent and lists that task's runs;
it stays separate because launching something that will act on the work is a
different decision from writing it. A new task has only the first pane and no
tab strip.

**View** above the board chooses the layout, how tightly cards pack, which of the
six columns are on screen and which chips a card carries. The choices are saved
per profile and **Reset board view** restores all of them. Hiding a column does
not hide its work silently: a banner names how many tasks are in the columns
currently off screen, and the board refuses to hide its last column.

**Archive** files a task away. It is on a card's right-click menu beside Delete
and on the selection bar for a batch, and it does exactly one thing: moves the
task to Done, which is what "archived" means here. The row names that column,
and is disabled for a task already archived rather than spending a revision to
store the status it already has — a mixed selection archives only the part that
is not. There is no separate archived flag to set: the vendored store has no
field for one, and [ARCHIVE_SEPARATION.md](ARCHIVE_SEPARATION.md) is the plan
for making the two independent, with the upstream change that needs.

**Archive**, the panel beside the Inbox, holds the scope's archived tasks. It
states that rule above the list whether or not the list is empty. It has its
own search and **Load more**, and its header badge is the store's total for the
scope rather than the page on screen. Select rows to restore them to any other
status or delete them; both go through the same confirm-and-retry dialog as a
bulk change from the board, so a lost reply is reconciled the same way. A write
reloads the dock from its first page — the store's cursor only runs forward, so
pages already scrolled past cannot be refreshed in place. The archive is a
second way to read completed work, not a move: it says whether the Done column
is still on the board and offers the same hide the View menu owns, and once
Done is hidden the board's banner offers the archive by name. Tasks are listed
in the board's own order and stamped with their last update, because the store
keeps no completion time to sort by. While the window is in the background the
query is deferred, and the panel says that rather than showing an empty archive.

The rows are the only part of that panel that scrolls, and the actions a
selection enables are pinned below them: ticking a row shows Restore and Delete
without scrolling for them.

Search uses the store's full-text query. **Filters** narrows loaded cards by
priority, type, owner, label and due date. **Due soon** means from now through the
next seven days; overdue tasks have their own filter. Facet choices and **Select
all** use loaded cards. Column totals and **Load more** expose additional results;
an empty filtered page does not establish that the whole profile has no matches.

Click a card to edit it. Command/Ctrl-click toggles selection; Shift-click selects
a visible range. Right-click a card, or press Shift+F10 on a focused card, for
Open, Quick Enhance, **Send to agent**, Duplicate, Copy, Move to, Set priority,
**Due**, **Owner**, **Labels**, **Archive** and Delete. A value submenu lists every choice with
the current one ticked and disabled, so the menu says what a task is as well as
what it could become; typing jumps to a row. Single-task actions disappear for
multiple selections. Duplicate opens a draft for review and save. Right-click an
empty column to create a task with that status.

Drag cards to change status or position. Bulk status/priority edits can partially
succeed; the board reports failures. They are not a transaction across all tasks.

## Draft and review with Manvi or Apple Intelligence

In the editor, **Draft with Manvi** uses notes; **Improve with Manvi** uses the
task text. GitPulse saves the task first, then asks the configured Manvi provider
and model for title and/or description suggestions. A linked repository and a
title or notes are required. **Save task** also works with notes alone: the first
line supplies a missing title and the notes are appended to an existing
description. Extracted notes are cleared so a later save cannot reapply them over
accepted wording. Oversized combined descriptions are refused without discarding
the notes.

On a Mac running Apple Intelligence, an engine picker appears beside the drafting
controls and **Apple Intelligence** writes the text on the device instead: nothing
to install, and no text reaches a socket. It is an engine for the same proposal,
not a different feature — the suggestion is reviewed, accepted, undone and kept in
history exactly as a Manvi one is, and the history records which engine wrote each
revision. A build without Apple's Foundation Models framework shows no picker at
all; a Mac that has Apple Intelligence switched off, still preparing the model, or
that cannot run it shows the option with that reason and keeps Manvi selected.
Very long tasks are refused before the model runs, with the limit named.

The assist section names the model it would use and links to **Local model
servers** to change it. This uses Manvi's task provider configuration,
independently of the application header's model selection. Missing configuration
is shown before generation; no model or endpoint is substituted.

The proposed title and description appear beside their editable fields. Choose
**Use this title**, **Use this description** or **Use both**. **Not now** hides the
suggestion while retaining it in Manvi history. Field locks prevent enhancement
of the locked title or description; acceptance requires saved edits and checks
the saved task revision.

Every attempt a task has had is in the **Suggestion** dropdown beside them, each
row naming its number, state, model, time and the task revision it was written
against — enough to tell two runs of one model apart. Changing the selection
always shows that attempt's text straight away; when the accept buttons are
already beside the fields the review shows the difference without offering a
second way to accept it, so the two cannot disagree. An attempt that cannot be
applied — superseded by a newer task revision, already accepted, or blocked by a
field lock — says which of those it is instead of quietly having no button. The
list loads the 30 newest and reports how many of how many are on screen;
**Load more** pages without moving your selection. A running suggestion blocks another generation from the
same surface. If an action's reply is lost, **Retry pending action** reconciles
the same request before editing or closing. Once acceptance is confirmed, a failed
task refresh retries only the read.

**Quick Enhance** opens a separate sheet for an existing task. It exposes details
omitted from compact cards, lets you select unlocked fields, and shows proposal
history with **Apply enhancement** and dismissal controls. It is also where a
quick-added task lands when the line was added in **Draft** mode, with its
suggestion already being written. It shares the editor's
enhancement review and retry lifecycle. Automatic suggestion
settings are separate from this manual request; generated text still needs review.

## Copy or launch an agent

**Copy for agent** adds task instructions to a brief; it does not launch an agent.

Those instructions are the whole prompt: a run started from the handoff form
sends only the task identity to Manvi, and a clipboard copy is pasted into a
session that has never seen this repository. They say three things — how to
read the fields, that the author's wording is evidence to keep rather than
paraphrase, and which of this project's own tools to orient with (the
`gitpulse-*` and `devmap-*` skills, and the `gitpulse_*` and `devmap_*` MCP
tools, each called with the repository's absolute `repo_path`). The skills
named there are asserted against the directories that ship them, so the list
cannot fall behind.

| Copy source | What is copied |
| --- | --- |
| New, unsaved task | Current draft fields with extracted notes, explicitly labeled as an unsaved draft |
| Saved task editor | The saved revision; unsaved edits are excluded and the copy feedback says so |
| Board selection | Up to eight saved task briefs per action, with partial results reported |
| Context menu → Copy → Saved brief | The canonical saved brief without the extra agent instructions |

An unsaved draft can hold more text than a saved task's 64 KiB description,
because notes are kept whole until the save boundary reports them. When a
draft copy has to cut one, the packet says how much it kept rather than ending
mid-sentence.

Saved briefs include the task, ordered repository references and home workspace
with their revisions. Stale or incomplete snapshots are refused. Save edits
before copying if they should be part of the agent's brief.

**Send to agent** on a card, and the **Agent** pane of a saved task, are the same
form: agent, connection, working checkout and permission mode. The checkout is
offered rather than typed — a repository tab GitPulse already has open, or the
folder beside that repository's git directory, marked as derived. The form
re-reads the saved task and refuses a revision that changed elsewhere, locks
every control while a preparation outcome is unknown so a retry replays that
exact request, and remembers the agent, connection and permission mode for the
next launch. `Bypass permissions` is the one setting never remembered: it has to
be chosen again, with its acknowledgement, for every attempt.

Task run controls prepare a saved revision and the selected checkout before
launch. A terminal handoff opens a dedicated task-bound session in the existing
[terminal dock](TERMINAL.md). It runs under that CLI's configured permissions;
finishing the process does not accept the task or mark it Done.

Managed runs use a separate Manvi host protocol for configuration checks,
structured questions/approvals and completion receipts. A saved decision and its
delivery to the provider are separate states. Terminal handoffs do not produce
those structured callbacks. Full crash recovery remains a qualification gap; see
the [implementation contract](AGENTIC_WORKSPACES_PLAN.md) and
[architecture](ARCHITECTURE.md#agentic-workbench-implemented-core-broader-qualification-in-progress).

### Providers with a managed lane

Codex and Claude Code. Grok and Antigravity are terminal-only, and the limit is
mechanical rather than editorial: a managed run needs an adapter in the harness
that speaks that provider's own session protocol. The three places that decide
this — the renderer's choice, the Rust workbench's gate, and the harness's
adapter set — are held in agreement by `managed-provider-parity-contract`.

Two things differ between the two managed providers, and both are visible in a
run's recorded effective configuration:

- **Sandbox.** Codex runs inside its own OS sandbox (`read-only`,
  `workspace-write`, or full access) and the recorded configuration names it.
  Claude Code has no sandbox, so the record says `none` with network access, and
  `inspect` is enforced by Claude Code's `plan` permission mode instead. A
  managed Claude run is *supervised*, not *confined*.
- **Model.** Codex reports its model when the thread is created, before the run
  is recorded. Claude Code names its model only once a turn is running, which is
  after the host has written the configuration and the store has made it
  immutable — so the field is empty for a managed Claude run, and the build
  identity (`claude-code/<version>`) is recorded in its place, read from the
  executable and then checked against the live session.

Repository settings files (`.claude/settings.json` and the `.local` variant) are
not loaded for a managed Claude run — only the operator's user settings are. A
checkout cannot widen the permissions of the run inspecting it, or empty the
approval surface the managed lane exists to provide. `CLAUDE.md` and other
project context are unaffected; only settings files are scoped.

> [!NOTE]
> Managed runs are verified on macOS only. Neither adapter uses a Unix-specific
> API, but both end-to-end tests are `#[cfg(unix)]`, so Windows is untried
> rather than known-good. See [platform coverage](QUALIFICATION.md#platform-coverage).

## Keyboard controls

These board shortcuts apply while focus is inside the board, outside editor
fields and dialogs. Card-specific shortcuts require a focused card.

| Key | Action |
| --- | --- |
| `/` | Focus task search |
| `n` | New task |
| `a` | Focus quick add |
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
`src/lib/ai/appleIntelligence.ts`, `src-tauri/src/ai/apple.rs`, `TaskBoard.svelte`,
`TaskEditor.svelte`, `TaskRepositoryPicker.svelte`, `TaskQuickAdd.svelte`, `TaskViewMenu.svelte`,
`TaskHandoffForm.svelte`, `TaskAgentPanel.svelte`, `TaskManviAssist.svelte` and
`QuickEnhanceSheet.svelte`. Whether a scope can hold a new task lives in
`taskCreation.ts`, the sheet's repository picker in `taskRepositories.ts`, and
the workspace membership write both the board and the sheet perform in
`openMembership.ts`. Unit and source contracts cover helpers and wiring;
the repository picker, the empty-workspace path and the workspace join are
driven through the real components by the `tasks` browser harness. None of them
prove native clipboard delivery, physical drag behavior, installed
provider permissions or OS notification delivery. The implementation contract
retains the broader planned features and their outstanding verification gates.
