CREATE TABLE work_enhancements (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision>0),
    task_id TEXT NOT NULL REFERENCES work_items(id),
    body TEXT NOT NULL CHECK(json_valid(body)),
    state TEXT GENERATED ALWAYS AS (json_extract(body,'$.state')) STORED,
    created_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.created_at')) STORED,
    expires_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.expires_at')) STORED,
    automatic INTEGER GENERATED ALWAYS AS (json_extract(body,'$.automatic')) STORED
);
CREATE INDEX work_enhancements_task ON work_enhancements(task_id,created_at,id);
CREATE INDEX work_enhancements_order ON work_enhancements(created_at,id);
CREATE INDEX work_enhancements_pending ON work_enhancements(state,expires_at);
CREATE INDEX work_enhancements_quota ON work_enhancements(automatic,created_at);
UPDATE work_meta SET version=2 WHERE id=1;
