-- Schema 11: task archive, completion time, checklists and task links.
--
-- SQLite cannot ADD a STORED generated column to an existing table, so both
-- new columns are VIRTUAL. That is enough: they are indexed, and an index on a
-- virtual column stores its value like any other.
--
-- `archived` is independent of `status`. coalesce(…,0) makes a body written
-- before the field existed read as not archived rather than as SQL NULL, so
-- `archived=0` is a total predicate from the first migrated read.
ALTER TABLE work_items ADD COLUMN archived INTEGER
    GENERATED ALWAYS AS (coalesce(json_extract(body,'$.archived'),0)) VIRTUAL;
-- When the task last entered Done, in Unix seconds; 0 for a task that is not
-- Done. The store maintains it: no request can set it.
ALTER TABLE work_items ADD COLUMN completed_at INTEGER
    GENERATED ALWAYS AS (coalesce(json_extract(body,'$.completed_at'),0)) VIRTUAL;
CREATE INDEX work_items_archive ON work_items(deleted,archived,status,position,id);
CREATE INDEX work_items_completed ON work_items(deleted,archived,completed_at,id);

-- One row per outbound link a task's body names, so the other end can be
-- found without scanning every body. Replaced with the body on every write
-- that carries `links`; the body stays the record of truth.
CREATE TABLE work_item_links (
    item_id TEXT NOT NULL REFERENCES work_items(id),
    kind TEXT NOT NULL CHECK(kind IN ('parent','blocks','related','duplicate_of')),
    target_id TEXT NOT NULL REFERENCES work_items(id),
    position INTEGER NOT NULL CHECK(position>=0),
    PRIMARY KEY(item_id,kind,target_id)
);
CREATE INDEX work_item_links_target ON work_item_links(target_id,kind,item_id);

-- Every reader before this version treated Done as the archive. Carry that
-- over rather than empty every archive into the Done column on upgrade.
--
-- Completion time is recovered from history: the earliest revision of the
-- task's current Done streak, i.e. the first revision after the last one
-- whose status was not Done. A task with no such history falls back to its
-- own `updated_at`. The revision is not bumped: this restates what the row
-- already meant, and bumping it would fail every host's pending write.
UPDATE work_items SET body=json_set(body,
    '$.archived',json('true'),
    '$.completed_at',coalesce(
        (SELECT min(json_extract(r.body,'$.updated_at')) FROM work_revisions r
          WHERE r.entity_type='item' AND r.entity_id=work_items.id
            AND json_extract(r.body,'$.status')='done'
            AND r.revision>coalesce((SELECT max(x.revision) FROM work_revisions x
                WHERE x.entity_type='item' AND x.entity_id=work_items.id
                  AND json_extract(x.body,'$.status')<>'done'),0)),
        json_extract(body,'$.updated_at')))
WHERE status='done';
UPDATE work_meta SET version=11 WHERE id=1;
