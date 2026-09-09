CREATE TABLE work_attention (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision>0),
    source_sequence INTEGER NOT NULL UNIQUE REFERENCES work_events(sequence),
    task_id TEXT NOT NULL REFERENCES work_items(id),
    body TEXT NOT NULL CHECK(json_valid(body)),
    read_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.read_at')) STORED,
    dismissed_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.dismissed_at')) STORED,
    snoozed_until INTEGER GENERATED ALWAYS AS (json_extract(body,'$.snoozed_until')) STORED
);
CREATE INDEX work_attention_order ON work_attention(source_sequence DESC);
CREATE INDEX work_attention_task ON work_attention(task_id,source_sequence DESC);
CREATE INDEX work_attention_unread ON work_attention(read_at,dismissed_at,source_sequence DESC);
CREATE INDEX work_attention_visible ON work_attention(dismissed_at,source_sequence DESC);
UPDATE work_meta SET version=6 WHERE id=1;
