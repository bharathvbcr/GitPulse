# Task + lease store v1

**Canonical owner:** DevCouncil — `rust/dc-store/src/schema.rs` owns the DDL. Manvi builds the same crate through a symlink (`Manvi/crates/dc-store`); it does not own it.
**Store location:** `.devcouncil/state.sqlite`, per repository.
**Consumers:** DevCouncil and Manvi (`dc-store`), GitPulse (links `dc-store` directly).

This document describes the tables as *implemented*, not as intended. Where a
consumer's model disagrees with what follows, `schema.rs` wins and the consumer
is wrong.

---

## Why this is a Markdown document and not a JSON Schema

The other two contracts describe values crossing a wire, so a validator can
check them. This one describes a *database* two processes write concurrently.
The properties that matter here — which index provides mutual exclusion,
what a NULL in `expires_at` means, who is allowed to write — are not shapes a
validator can express. Encoding them as a schema would produce something that
passes while the invariant it exists to protect is broken.

---

## `tasks`

```sql
CREATE TABLE IF NOT EXISTS tasks (
    id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    description VARCHAR NOT NULL,
    requirement_ids_json VARCHAR NOT NULL DEFAULT '[]',
    acceptance_criterion_ids_json VARCHAR NOT NULL DEFAULT '[]',
    planned_files_json VARCHAR NOT NULL DEFAULT '[]',
    expected_tests_json VARCHAR NOT NULL DEFAULT '[]',
    allowed_commands_json VARCHAR NOT NULL DEFAULT '[]',
    forbidden_changes_json VARCHAR NOT NULL DEFAULT '[]',
    status VARCHAR NOT NULL DEFAULT 'planned',
    difficulty VARCHAR,
    agent_appended_expected_tests_json VARCHAR NOT NULL DEFAULT '[]',
    agent_appended_allowed_commands_json VARCHAR NOT NULL DEFAULT '[]',
    priority VARCHAR,
    agent_appended_planned_files_json VARCHAR NOT NULL DEFAULT '[]',
    PRIMARY KEY (id)
);
```

### The `agent_appended_*` columns are not a second copy of scope

`planned_files_json` is what the *planner* declared before the work started.
`agent_appended_planned_files_json` is scope an executor added to its own task
while it worked, by arguing a blocked write through the override seam.

Both are in scope for the gate. They are stored apart because they answer
different questions, and a single merged list cannot answer the second one at
all: *was this write authorised by the plan, or by the worker's own judgement
about itself?*

**A consumer that merges these two lists destroys the distinction the
`widened` field on a verdict exists to report.** GitPulse must read them
separately and render a write authorised by appended scope as `widened`, not
as `clean`. See `verdict.schema.json`, `$defs.classification`.

There is deliberately **no** agent-appended counterpart for
`requirement_ids_json`. An executor may widen its own *file* scope, because
which files a change touches is discovered while making it. Which requirement
a task satisfies is not discovered that way, and a task that could append there
could discharge a requirement by asserting it had.

---

## `task_leases`

```sql
CREATE TABLE IF NOT EXISTS task_leases (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    owner VARCHAR NOT NULL,
    agent VARCHAR,
    client_id VARCHAR,
    run_id VARCHAR,
    branch VARCHAR,
    lease_token VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    expires_at VARCHAR,
    released_at VARCHAR,
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS ix_task_leases_task_id ON task_leases (task_id);
CREATE INDEX IF NOT EXISTS ix_task_leases_status ON task_leases (status);

CREATE UNIQUE INDEX IF NOT EXISTS ux_task_leases_active
    ON task_leases (task_id) WHERE status = 'active';
```

### `ux_task_leases_active` is the mutual-exclusion primitive

It is a **partial** unique index: at most one `active` lease per `task_id`,
with no constraint on released or expired rows. Without it, two concurrent
acquires both pass their pre-check and both insert, and two agents hold the
same task.

**Applying the DDL is not evidence that it took.** `CREATE UNIQUE INDEX IF NOT
EXISTS` is a no-op against a database where an index of that name already
exists with a *different* definition — an older non-partial one, say, or one
built before the `WHERE` clause was added. The store therefore verifies the
index in the opened database rather than trusting that its own DDL ran;
`dc-store` exports the name as `EXCLUSION_INDEX` for exactly this.

A consumer that opens `state.sqlite` and intends to rely on lease exclusion
**must run that verification**, not assume it. This is the honesty invariant
applied to storage: a constraint that could not be established must never look
like one that holds.

### Timestamps are strings, and `expires_at` may be NULL

`created_at`, `expires_at` and `released_at` are `VARCHAR`, not SQLite date
types. Compare them as strings only in UTC ISO-8601 form, where lexical
order matches chronological order; parse before comparing anything else.

A NULL `expires_at` means **this lease does not expire**, not "expired" and not
"unknown". A consumer computing "safe to reclaim" from lease expiry must treat
NULL as *never reclaimable on this basis*, and fall back to an explicit
release.

### An unreadable `expires_at` fails closed for its own task

A row whose `expires_at` is not a timestamp is neither live nor expired. It
fails closed for **its own task only**: `ready` leaves the task out and names
it in `unreadable_leases`, `health` answers `degraded: true` with the same list,
and `acquire` refuses it unless `--force` is given, in which case the lease is
retired (`status = 'stale'`, `released_at` stamped, the bad value left in the
row) and the reply carries `retired_unreadable_lease`. Every other task is
served normally. A consumer must not read "no timestamp" as "no expiry".

Lazy expiry is one conditional `UPDATE` naming the expiry that was observed,
so a renewal that lands first wins; `release` and `force` only ever touch a row
that is still `active`, and a `released` row is never rewritten to `stale`.

Fields added to replies are additive: `degraded`, `unreadable_leases`,
`retired_unreadable_lease`, and, on `gaps` / `evidence-list`, a per-row
`evidence_error` / `data_error` beside a fallback value when the stored JSON
text was not provably one value of the expected kind.

---

## Writers, and who may be one

| Process | Access |
|---|---|
| DevCouncil / Manvi (`dc-store`) | read + write |
| **GitPulse** | **read only** |

GitPulse links `dc-store` to *read* leases and planned files. It must never
call `checkout_task`, `write_file`, `apply_patch`, or `graph_ingest`, and must
never acquire or release a lease: those contend with an active agent's writer
lease. A UI process that takes a lease can strand a task when the window
closes.

Migrations are owned by `dc-store`. New columns land in `schema.rs` first.

---

## Workbench profile schema 12: refinement proposals

The workbench profile database (`work_meta.version`, distinct from the
repository lease store above) reached schema 12 with refinement proposals.
It changes no table. It changes what a host must understand, so it is a hard
fence: a build that knows only schema 11 compares the stored version with `!=`
and refuses a 12 profile with `schema_unsupported`. Upgrade every host that
shares a profile together. `dcstore --version` reports `workbench_schema`.

An enhancement proposal (`work_enhancements.body`) gains `kind`:

| `kind` | Proposes | `fields` names |
|---|---|---|
| `text` (absent before 12) | `title`, `description` | any of `title`, `description` |
| `refine` | acceptance criteria, splits, duplicate links | any of `acceptance_criteria`, `split_into`, `duplicate_of` |

A task's `locked_fields` may now also name `acceptance_criteria`. A `refine`
proposal over a locked `acceptance_criteria` is refused with `field_locked`
at create, claim and accept.

A `refine` proposal uses the ordinary `enhancements.*` methods and keeps their
invariants: it stores the complete source task snapshot (logs removed);
`create` refuses a stale `source_revision` and a locked field; at most one
unexpired pending or running generation exists per profile, whatever the kind;
`dismiss` rejects it and an unclaimed pending proposal expires. It is never
`automatic`, and it cannot be revised or undone.

`enhancements.complete` for a `refine` proposal supplies exactly the requested
parts (an empty array is "nothing to propose") and optional `rationale` and
`grounding` (an object of at most 16 KiB, opaque to the store):

| Part | Value | Bounds |
|---|---|---|
| `acceptance_criteria` | array of strings | at most 32; none repeating another or the task's own; at most 128 on the task after accept |
| `split_into` | array of `{title, description}` | at most 16; title nonblank, at most 300 characters |
| `duplicate_of` | array of `{id, reason}` | at most 16; each id a distinct live task, not this one, sharing a repository with it |

`enhancements.accept` takes `expected_revision` (proposal), `expected_task_revision`,
`fields` (the parts to apply) and, when `split_into` is selected, `split_ids`:
one new, never-used task identifier per proposed split. It adds and never
removes: criteria are appended, `duplicate_of` links are merged into the
task's `links`, and each split becomes a new `inbox` task in the same
repositories carrying a `parent` link to the refined task. The task changes in
one revisioned update (`applied_task_revision`), a stale task revision is
`revision_conflict`, a changed field the proposal was drafted from is
`field_conflict`, and a conflict consumes nothing. The accepted proposal lists
`created_item_ids`.

---

## Workbench profile schema 13: short request receipts

Schema 13 changes no table. A request's receipt (`work_requests.response`) now
keeps only the event `sequence` (marked `"replay":"event"`), and a retry
rebuilds the response from the event it names. A schema 12 host replays a stored
receipt verbatim, so it would answer a retry with a body-less response; it is
therefore a hard fence like 12: a build that knows only schema 12 refuses a 13
profile with `schema_unsupported`. Receipts stored in full by schema 12 keep
replaying as written. Upgrade every host that shares a profile together; the
upgrade runs only on a write, after the `VACUUM INTO` backup
(`<profile>.pre-schema-12.bak`).
