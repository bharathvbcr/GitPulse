CREATE TABLE work_runs (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    task_id TEXT GENERATED ALWAYS AS (json_extract(body,'$.task_id')) STORED REFERENCES work_items(id),
    repository_id TEXT GENERATED ALWAYS AS (json_extract(body,'$.repository_id')) STORED REFERENCES work_repositories(id),
    state TEXT GENERATED ALWAYS AS (json_extract(body,'$.state')) STORED,
    created_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.created_at')) STORED,
    expires_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.expires_at')) STORED,
    session_id TEXT GENERATED ALWAYS AS (json_extract(body,'$.session_id')) STORED
);
CREATE INDEX work_runs_order ON work_runs(created_at,id);
CREATE INDEX work_runs_task ON work_runs(task_id,created_at,id);
CREATE INDEX work_runs_repository ON work_runs(repository_id,state,expires_at);
CREATE INDEX work_runs_state ON work_runs(state,expires_at);
CREATE UNIQUE INDEX work_runs_session ON work_runs(session_id) WHERE session_id IS NOT NULL;
-- The large source snapshot is retained once, never copied into every lifecycle
-- event, receipt and revision. Only runs.get loads it.
CREATE TABLE work_run_inputs (
    run_id TEXT PRIMARY KEY REFERENCES work_runs(id),
    brief TEXT NOT NULL CHECK(json_valid(brief))
);
CREATE TRIGGER work_run_inputs_immutable BEFORE UPDATE ON work_run_inputs
BEGIN SELECT RAISE(ABORT,'run inputs are immutable'); END;
UPDATE work_meta SET version=5 WHERE id=1;
