CREATE TABLE work_automation (
    id TEXT PRIMARY KEY CHECK(id='profile'),
    revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body))
);
INSERT INTO work_automation(id,revision,body) VALUES(
    'profile',1,
    '{"id":"profile","revision":1,"enabled":true,"provider":null,"model":null,"updated_at":0}'
);
CREATE TABLE work_enhancement_queue (
    task_id TEXT PRIMARY KEY REFERENCES work_items(id),
    not_before_ms INTEGER NOT NULL CHECK(not_before_ms>=0 AND not_before_ms<=9007199254740990),
    enqueued_request_id TEXT NOT NULL
);
CREATE INDEX work_enhancement_queue_due ON work_enhancement_queue(not_before_ms,task_id);
UPDATE work_meta SET version=4 WHERE id=1;
