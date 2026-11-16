-- Schema 16: the Inbox status is Backlog. The six task statuses become five.
--
-- `work_items.status` is generated from the body, so the body is what is
-- rewritten; every row, live, archived and deleted, because a restore or an
-- unarchive must not bring an `inbox` back. Position is one ordering across
-- the whole board (the board lists by `position, id` and a column is a status
-- filter over it), so the former Inbox tasks and the Backlog ones already sit
-- in the order their positions give and nothing is renumbered.
--
-- Like schema 11, this restates what a row already meant: the revision is not
-- bumped and `updated_at` is not touched. A bump would fail every host's
-- pending write against the revision it read, and would flip every attention
-- notice for the task to `task_changed`.
--
-- The same word is rewritten wherever the store keeps a copy of a task body it
-- hands back verbatim, so no history, event feed, replayed receipt or
-- enhancement proposal returns a status the vocabulary no longer has:
--   * the revisions of a task, and of an enhancement's `source` snapshot;
--   * the events that carry those bodies (`events.list`, and a retried
--     request's answer, which is rebuilt from its event);
--   * receipts written before schema 13, which keep their whole response;
--   * the enhancement rows themselves.
-- Request inputs (`work_requests.input`) are compared byte for byte to catch a
-- reused request_id, so a retry that still says `inbox` must keep matching its
-- own record; they are never returned. `work_run_inputs` is immutable and is
-- what a launch compares itself against, so a brief an agent was already handed
-- keeps the word it was handed.
UPDATE work_items SET body=json_set(body,'$.status','backlog')
 WHERE status='inbox';
UPDATE work_revisions SET body=json_set(body,'$.status','backlog')
 WHERE entity_type='item' AND json_extract(body,'$.status')='inbox';
UPDATE work_revisions SET body=json_set(body,'$.source.status','backlog')
 WHERE entity_type='enhancement' AND json_extract(body,'$.source.status')='inbox';
UPDATE work_enhancements SET body=json_set(body,'$.source.status','backlog')
 WHERE json_extract(body,'$.source.status')='inbox';
UPDATE work_events SET payload=json_set(payload,'$.item.status','backlog')
 WHERE json_extract(payload,'$.item.status')='inbox';
UPDATE work_events SET payload=json_set(payload,'$.item.source.status','backlog')
 WHERE json_extract(payload,'$.item.source.status')='inbox';
UPDATE work_requests SET response=json_set(response,'$.item.status','backlog')
 WHERE json_extract(response,'$.item.status')='inbox';
UPDATE work_requests SET response=json_set(response,'$.item.source.status','backlog')
 WHERE json_extract(response,'$.item.source.status')='inbox';
UPDATE work_meta SET version=16 WHERE id=1;
