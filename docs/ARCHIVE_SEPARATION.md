# Archive separate from Done

A task's *archived* state is its own stored flag, independent of its status.
Finishing a task leaves it in the Done column; **Archive** files a task away
from the board in whatever column it is in; **Restore** brings it back to the
column it left, with its status untouched. Until dc-store schema 11 the two
were the same thing — a task was archived exactly when its status was `done`
— and this document was the plan for separating them. It now records what
shipped and why it is shaped the way it is.
[`scripts/archive-separation-contract.test.ts`](../scripts/archive-separation-contract.test.ts)
pins each claim below to the vendored source.

## The store (dc-store schema 11)

`dc-store` is vendored from DevCouncil
([`src-tauri/vendored/VENDOR.json`](../src-tauri/vendored/VENDOR.json), crate
`dc-store` at commit `e57f3d2`). The change was made upstream on DevCouncil's
`feat/workbench-schema-11` branch, corrected on `fix/workbench-schema-11-audit`
(below), and re-vendored with
`node scripts/vendor-crates.mjs --crate=dc-store`, never edited here.

Schema version is **11**. The 10 → 11 rung (`workbench/items.sql`):

- adds `archived` and `completed_at` to `work_items` as **VIRTUAL** generated
  columns over the body. The original plan said STORED, but SQLite cannot
  `ALTER TABLE … ADD COLUMN` a stored generated column; a virtual one can be
  indexed just the same. `coalesce(…,0)` makes a body written before the field
  existed read as not archived and not completed, so `archived=0` is a total
  predicate from the first migrated read;
- indexes them (`work_items_archive`, `work_items_completed`);
- adds `work_item_links` for task links (see [QUALIFICATION.md](QUALIFICATION.md));
- **migrates every Done task to archived.** Every reader before schema 11 saw
  completed work in the archive; without this every profile's archive would
  empty itself on upgrade while the Done column doubled — the same work,
  reported two different ways. A reader who wants a Done task back on the
  board restores it, which is a normal action rather than a repair;
- recovers each migrated task's `completed_at` from its history: the time of
  the earliest revision of its *current* Done streak (the first revision after
  the last one whose status was not Done) — that revision's own time, not the
  earliest time in the streak, which a host with a slower clock can put before
  the task was Done — falling back to `updated_at` when there is no such
  history. No revision is spent, so a host holding a pre-upgrade revision
  can still save;
- gives **every other row** the same keys a schema 11 write gives it —
  `archived: false`, `completed_at: null`, an empty `checklist` and no
  `links` — so a reader sees one shape for a task written before the upgrade
  and one written after. Upgrading a real profile showed why: every open task
  otherwise read back with `archived` missing, which the board defaults but an
  agent reading over MCP would have to guess at.

`src-tauri/src/workbench/profile_upgrade_tests.rs` upgrades a copy of a real
profile through the host and checks each of these against the pre-upgrade
file read directly (see its header for how to run it; it refuses the live
profile's path).

The ladder is a hard fence: a host built against schema 10 refuses a schema 11
profile (`schema_unsupported`). That is deliberate — a host that builds a task
body without `archived` would silently unarchive every task it saved. GitPulse,
DevCouncil and Manvi each link dc-store, so all three must be rebuilt from a
schema 11 dc-store before any of them opens a migrated profile.

`items.put` accepts `archived`, `checklist` and `links`. Each follows the
`locked_fields` rule: a request that **omits** the field keeps the stored
value; only an explicit value changes it. An agent's status move, a merge or
an older code path that does not know the field therefore never unarchives a
task. `completed_at` is the store's alone — set when the task enters Done,
kept while it stays there, cleared when it leaves — and a request naming it is
refused.

`items.list` accepts `archived` (omitted: both sides), `deleted` (read deleted
tasks instead of live ones) and `order`: `board` (position), `completed` (most
recently completed first) or `updated` (most recently changed first). Each
order has its own cursor prefix, so a cursor is refused under another order
rather than resuming at a position that means something else.

`items.restore` brings a soft-deleted task back at the next revision, checked
against the revision its deletion produced. Deleting a workspace leaves
already-deleted tasks as their deletion left them, so that revision and the
deleted list's order both hold; a home workspace deleted meanwhile is dropped
on restore. A link the task held survives its target's deletion (shown as
deleted in the brief); a new link must name a live task. A new parent link is
refused when the chain it would make — the parent's ancestors, the parent, the
task and its deepest subtask — holds more than 256 tasks; a parent the task
already has is not checked again, so another task's edit never makes it
unsaveable.

`dcstore --version` reports the `workbench_schema` it opens. GitPulse hands
Manvi only a `dcstore` that reports this build's schema (`MANVI_STORE_BINARY`)
and shows any other as needing an update: both 0.2.4 builds answered the same
version, and a schema 10 one on `PATH` refused every workbench request.

## GitPulse

[`src/lib/workbench/taskArchive.ts`](../src/lib/workbench/taskArchive.ts) is
still the one module that says what archived means:

| Function | Now |
| --- | --- |
| `isArchived(task)` | `task.archived === true` |
| `archiveAction()` | `{ changes: { archived: true } }` |
| `restoreAction()` | `{ changes: { archived: false } }` — no restore-target picker; a restored task keeps its status |
| `ARCHIVE_RULE` | names the Archive and Restore actions, not a column |
| `refreshPages` | re-reads the dock's loaded pages in place after a write |

`boardPresence` and `offersArchive` are gone: the archive is no longer a
second reading of the Done column, so there is nothing to say about whether
that column is drawn. Every board column reads `archived:false`
(`TaskBoard.svelte`), the header badge is the archive's own server total, and
the dock lists `archived:true` most recently completed first, with a Deleted
view beside it for restoring deleted tasks.
