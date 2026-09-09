CREATE INDEX work_attention_delivery_time ON work_attention(json_extract(body,'$.created_at'),source_sequence);
CREATE TABLE work_notification_settings (
    id TEXT PRIMARY KEY CHECK(id='profile'),
    revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body))
);
INSERT INTO work_notification_settings VALUES('profile',1,json_object(
    'id','profile','revision',1,'updated_at',0,'profile_id',lower(hex(randomblob(16))),
    'enabled',json('false'),'sound',json('false'),'background',json('false'),
    'after_sequence',0,'quiet_start',NULL,'quiet_end',NULL,
    'muted_workspace_ids',json('[]'),'muted_repository_ids',json('[]'),'muted_task_ids',json('[]')));
CREATE TABLE work_notification_deliveries (
    id TEXT PRIMARY KEY REFERENCES work_attention(id),
    revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    activated_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.activated_at')) STORED,
    acknowledged_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.acknowledged_at')) STORED
);
CREATE INDEX work_notification_activations ON work_notification_deliveries(acknowledged_at,activated_at,id);
UPDATE work_meta SET version=7 WHERE id=1;
