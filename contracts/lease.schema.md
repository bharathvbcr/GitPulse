# Task + lease store v1

**Canonical owner:** Manvi — `crates/dc-store/src/schema.rs` owns the DDL.
**Store location:** `.devcouncil/state.sqlite`, per repository.
**Consumers:** DevCouncil (Python, SQLModel), GitPulse (links `dc-store` directly).

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
types, because the Python writer stores ISO-8601 strings. Compare them as
strings only in UTC ISO-8601 form, where lexical order matches chronological
order; parse before comparing anything else.

A NULL `expires_at` means **this lease does not expire**, not "expired" and not
"unknown". A consumer computing "safe to reclaim" from lease expiry must treat
NULL as *never reclaimable on this basis*, and fall back to an explicit
release.

---

## Writers, and who may be one

| Process | Access |
|---|---|
| DevCouncil (Python/SQLModel) | read + write |
| Manvi (`dc-store`) | read + write |
| **GitPulse** | **read only** |

GitPulse links `dc-store` to *read* leases and planned files. It must never
call `checkout_task`, `write_file`, `apply_patch`, or `graph_ingest`, and must
never acquire or release a lease: those contend with an active agent's writer
lease. A UI process that takes a lease can strand a task when the window
closes.

Migrations are owned by `dc-store`. The Python side is a consumer of the
schema, not a co-owner of it; new columns land in `schema.rs` first.
