CREATE TABLE work_decisions (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision > 0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    run_id TEXT GENERATED ALWAYS AS (json_extract(body,'$.run_id')) STORED REFERENCES work_runs(id),
    state TEXT GENERATED ALWAYS AS (json_extract(body,'$.state')) STORED,
    created_at INTEGER GENERATED ALWAYS AS (json_extract(body,'$.created_at')) STORED,
    CHECK(json_extract(body,'$.id')=id AND json_extract(body,'$.revision')=revision)
);
CREATE INDEX work_decisions_run_state ON work_decisions(run_id,state,created_at,id);
CREATE UNIQUE INDEX work_decisions_provider_request ON work_decisions(
    run_id,json_extract(body,'$.provider_thread_id'),json_extract(body,'$.provider_turn_id'),json_extract(body,'$.protocol_request_id')
);
UPDATE work_meta SET version=8 WHERE id=1;
