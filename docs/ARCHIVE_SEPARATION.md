# Separating Archive from Done

Today a task is archived exactly when its status is `done`. This document is
the plan for making *archived* a dimension of its own — a task that is Done
but not archived, or archived straight out of Ready — and the reason it is a
plan rather than a change.

It is written to be executed later, by someone with the DevCouncil checkout in
hand. [`scripts/archive-separation-contract.test.ts`](../scripts/archive-separation-contract.test.ts)
pins every claim it makes about the vendored store to the vendored source, so
the day upstream lands the field this document stops being true and the suite
says so.

## Why it is not already done

`dc-store` is vendored from DevCouncil
([`src-tauri/vendored/VENDOR.json`](../src-tauri/vendored/VENDOR.json), crate
`dc-store` at commit `a31918e`). The vendor manifest is explicit: change these
crates upstream and re-vendor, never in this repository. Three separate parts
of that crate would have to change, and none of them can be worked around from
the GitPulse side:

| What | Where | Why it blocks |
| --- | --- | --- |
| No column to store it | `work_items` in `workbench/schema.sql` | The table's stored generated columns are `title`, `description`, `status`, `position`. There is no `archived`. |
| No field to write it | `put_item` in `workbench/mod.rs` | `input.fields(…)` lists eighteen accepted names and **rejects** every other field, so an `archived` key in the request is an error, not an ignored extra. The body is assembled by `json_object(…)` from those names, so an unknown key could not reach the body even if it were accepted. |
| No filter to read it | `list_items` in `workbench/mod.rs` | `input.fields(…)` accepts exactly `limit`, `cursor`, `workspace_id`, `repository_id`, `status`, `query`. There is nowhere to say "archived" in a query. |

`STATUSES` is also a six-element `const` in that crate, so "add a seventh
status" is the same blocked change wearing a different hat.

### Why the client cannot fake it

The tempting shapes all fail on the same point, and it is the point this panel
exists to get right: **the server's `total` must stay true.**

- **A local archived set** (profile preference, `localStorage`) — `items.list`
  would still count and page unarchived rows. "Showing 30 of 412" would be
  counting something other than what is on screen, and there is no cursor
  arithmetic that fixes it short of fetching the whole profile. It would also
  be invisible to the headless worker, which cannot read `localStorage`.
- **A reserved label** — `items.list` filters on `status` and a full-text
  `query` over title and description. Labels are not filterable, so this has
  the same broken `total`, and it spends a user-facing field.
- **A reserved "Archive" workspace** via `home_workspace_id` — filterable, but
  it destroys the task's real workspace to store one bit, and a task's home
  workspace is shown and edited in the task sheet.

Each of these turns a number the panel currently gets right into one it gets
wrong. That is a worse defect than the one being fixed.

## The upstream change

Schema version is **10**; this is the migration to 11.

### 1. `rust/dc-store/src/workbench/archive.sql` (new)

```sql
ALTER TABLE work_items ADD COLUMN archived INTEGER
    GENERATED ALWAYS AS (coalesce(json_extract(body,'$.archived'),0)) STORED;
CREATE INDEX work_items_archive ON work_items(deleted,archived,status,position,id);
UPDATE work_meta SET version=11 WHERE id=1;
```

`coalesce(…,0)` is what makes this safe for rows written before the field
existed: every existing task reads as not archived rather than as SQL `NULL`,
so `archived=0` is a total predicate from the first migrated read. The index
mirrors `work_items_board` with `archived` ahead of `status`, because the
common query is "archived, any status" and "not archived, one status".

`work_workspaces` already carries `archived` the same way, so this is the
existing pattern rather than a new one.

### 2. `rust/dc-store/src/workbench/mod.rs`

Migration ladder — one more rung in the established shape, and the
`version != 10` terminal check becomes `version != 11`:

```rust
if version == 10 {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
    if version == 10 {
        conn.execute_batch(include_str!("archive.sql"))?;
        version = 11;
    }
    tx.commit()?;
}
```

`put_item` — add `"archived"` to the accepted fields, read it, and carry it
into the body. It must follow the `locked_fields` precedent, not the plain
`json_object` one: an older host that does not know the field omits it, and
omission has to **preserve** the stored value rather than silently unarchive
every task that host touches.

```rust
let body: String = input.conn.query_row(
    "SELECT json_set(?1,'$.archived',json(coalesce(json_extract(?2,'$.archived'),\
     (SELECT json_extract(body,'$.archived') FROM work_items WHERE id=?3),'false')))",
    params![body, input.raw, id], |r| r.get(0))?;
```

`list_items` — accept `archived` and pass it to `item_query`:

```rust
let archived = input.boolean("archived")?;   // None = either
…
if let Some(archived) = archived {
    filters.push(format!("t.archived={}", if archived { 1 } else { 0 }));
}
```

`status` and `archived` stay independent filters. That independence *is* the
feature: `{status:"done", archived:false}` is the Done column once the archive
is separate, and `{archived:true}` is the archive across every status.

### 3. Re-vendor

`npm run vendor` from this repository with `GITPULSE_DEVCOUNCIL_ROOT` set,
then `npm run vendor:check` and `npm run check:vendor-schema`.

## The GitPulse side

One module changes. [`src/lib/workbench/taskArchive.ts`](../src/lib/workbench/taskArchive.ts)
is the seam, and it was written to be one:

| Function | Now | After |
| --- | --- | --- |
| `isArchived(task)` | `task.status === ARCHIVE_STATUS` | `task.archived === true` |
| `archiveAction()` | `{ changes: { status: ARCHIVE_STATUS } }` | `{ changes: { archived: true } }` |
| `restoreAction(status)` | `{ changes: { status } }` | `{ changes: { archived: false } }`, and the restore-target picker goes away — a restored task keeps the status it had |
| `ARCHIVE_RULE` | names the Done column | names the Archive action |
| `boardPresence(hidden)` | whether Done is drawn | deleted; the archive is no longer a second reading of a column |

Everything else already asks this module rather than the status. The rest of
the change is mechanical:

- `TaskArchive.svelte` — its single `listTasks(scope, ARCHIVE_STATUS, …)` call
  becomes the `archived` filter, and the "Restore to" select becomes a plain
  Restore button.
- `TaskBoard.svelte` — every column query gains `archived:false`, so an
  archived task leaves the board without changing status. The header badge and
  `offersArchive` follow.
- `taskArchive.ts` grows `ARCHIVE_STATUS` only as long as something still
  needs it; after the switch nothing does.
- `TaskChanges` in `taskActions.ts` gains `archived?: boolean`, validated the
  same way the other fields are.

### Migration for existing profiles

`UPDATE work_items SET body=json_set(body,'$.archived',json('true'))
WHERE status='done'` as part of the same migration is the honest default: it
preserves what every reader currently sees, which is that completed work lives
in the archive. Without it, every profile's archive appears to empty itself on
upgrade while the Done column doubles — the same work, reported two different
ways, which is the failure this codebase treats as the serious one.

That `UPDATE` is the only lossy step: a reader who then wants a Done task back
on the board restores it, which is a normal action rather than a repair.

## What shipped instead

The archive is now operable within the current model, which was the larger
part of the complaint that started this: there was no Archive verb anywhere in
the product, and the panel's own Restore/Delete bar rendered below its fold.
See the Archive section of
[TASKS_AND_WORKSPACES.md](TASKS_AND_WORKSPACES.md).
