-- Schema 17: cross-repository execution queue, two-way GitHub issue sync state,
-- and an opt-in confirmed durable outbox. Older hosts do not know these
-- tables, cannot enforce queue leases or outbox delivery invariants, and must
-- refuse this profile.

CREATE TABLE IF NOT EXISTS work_queue (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision > 0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    state TEXT GENERATED ALWAYS AS (json_extract(body,'$.state')) STORED,
    concurrency_limit INTEGER GENERATED ALWAYS AS (json_extract(body,'$.concurrency_limit')) STORED,
    created_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.created_at')) STORED,
    CHECK(json_extract(body,'$.id') = id AND json_extract(body,'$.revision') = revision),
    CHECK(concurrency_limit BETWEEN 2 AND 4),
    CHECK(state IN ('queued','leased','verifying','passed','failed','cancelled','timed_out'))
);
CREATE INDEX IF NOT EXISTS work_queue_state_order ON work_queue(state, created_at, id);

CREATE TABLE IF NOT EXISTS work_queue_members (
    queue_id TEXT NOT NULL REFERENCES work_queue(id),
    member_id TEXT NOT NULL,
    repository_id TEXT NOT NULL REFERENCES work_repositories(id),
    git_dir TEXT NOT NULL,
    base_oid TEXT NOT NULL,
    task_id TEXT REFERENCES work_items(id),
    run_id TEXT REFERENCES work_runs(id),
    position INTEGER NOT NULL CHECK(position >= 0),
    state TEXT NOT NULL CHECK(state IN ('queued','leased','running','verifying','passed','failed','blocked','cancelled','timed_out')),
    lease_held INTEGER NOT NULL DEFAULT 0 CHECK(lease_held IN (0,1)),
    depends_on TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(depends_on)),
    started_at INTEGER,
    active_seconds INTEGER NOT NULL DEFAULT 0 CHECK(active_seconds >= 0),
    last_active_at INTEGER,
    verification_json TEXT CHECK(verification_json IS NULL OR json_valid(verification_json)),
    PRIMARY KEY (queue_id, member_id)
);
CREATE INDEX IF NOT EXISTS work_queue_members_order ON work_queue_members(queue_id, position, repository_id, member_id);
CREATE INDEX IF NOT EXISTS work_queue_members_repo ON work_queue_members(repository_id, lease_held);
CREATE UNIQUE INDEX IF NOT EXISTS work_queue_member_repo_lease ON work_queue_members(repository_id) WHERE lease_held = 1;
CREATE UNIQUE INDEX IF NOT EXISTS work_queue_member_checkout_lease ON work_queue_members(git_dir) WHERE lease_held = 1;

CREATE TABLE IF NOT EXISTS work_issue_sync (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision > 0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    task_id TEXT UNIQUE REFERENCES work_items(id),
    repo_slug TEXT GENERATED ALWAYS AS (json_extract(body,'$.repo_slug')) STORED,
    issue_number INTEGER GENERATED ALWAYS AS (json_extract(body,'$.issue_number')) STORED,
    CHECK(json_extract(body,'$.id') = id AND json_extract(body,'$.revision') = revision),
    CHECK(issue_number > 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS work_issue_sync_issue ON work_issue_sync(repo_slug, issue_number);
CREATE INDEX IF NOT EXISTS work_issue_sync_task ON work_issue_sync(task_id);

CREATE TABLE IF NOT EXISTS work_outbox (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision > 0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    sync_id TEXT REFERENCES work_issue_sync(id),
    task_id TEXT REFERENCES work_items(id),
    op TEXT GENERATED ALWAYS AS (json_extract(body,'$.op')) STORED,
    state TEXT GENERATED ALWAYS AS (json_extract(body,'$.state')) STORED,
    idempotency_key TEXT NOT NULL UNIQUE,
    payload_digest TEXT NOT NULL,
    confirmed INTEGER NOT NULL CHECK(confirmed = 1),
    created_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.created_at')) STORED,
    next_attempt_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.next_attempt_at')) STORED,
    CHECK(json_extract(body,'$.id') = id AND json_extract(body,'$.revision') = revision),
    CHECK(op IN ('create_issue','update_issue')),
    CHECK(state IN ('pending','delivering','outcome_unknown','conflict','delivered','failed','cancelled'))
);
CREATE INDEX IF NOT EXISTS work_outbox_state_due ON work_outbox(state, next_attempt_at, created_at, id);
CREATE INDEX IF NOT EXISTS work_outbox_task ON work_outbox(task_id, state);

UPDATE work_meta SET version = 17 WHERE id = 1;
