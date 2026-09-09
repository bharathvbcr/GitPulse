CREATE TABLE work_meta (id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL);
INSERT INTO work_meta VALUES(1,1);
CREATE TABLE work_workspaces (
    id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body)), deleted INTEGER NOT NULL DEFAULT 0,
    position INTEGER GENERATED ALWAYS AS (json_extract(body,'$.position')) STORED,
    archived INTEGER GENERATED ALWAYS AS (json_extract(body,'$.archived')) STORED
);
CREATE INDEX work_workspaces_order ON work_workspaces(deleted,archived,position,id);
CREATE TABLE work_repositories (
    id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body)),
    identity_key TEXT GENERATED ALWAYS AS (json_extract(body,'$.identity_key')) STORED UNIQUE
);
CREATE TABLE work_workspace_repositories (
    workspace_id TEXT NOT NULL REFERENCES work_workspaces(id),
    repository_id TEXT NOT NULL REFERENCES work_repositories(id),
    position INTEGER NOT NULL CHECK(position>=0),
    PRIMARY KEY(workspace_id,repository_id)
);
CREATE INDEX work_workspace_repository ON work_workspace_repositories(repository_id,workspace_id);
CREATE TABLE work_items (
    id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision>0),
    body TEXT NOT NULL CHECK(json_valid(body)), deleted INTEGER NOT NULL DEFAULT 0,
    home_workspace_id TEXT REFERENCES work_workspaces(id),
    primary_repository_id TEXT NOT NULL REFERENCES work_repositories(id),
    title TEXT GENERATED ALWAYS AS (json_extract(body,'$.title')) STORED,
    description TEXT GENERATED ALWAYS AS (json_extract(body,'$.description')) STORED,
    status TEXT GENERATED ALWAYS AS (json_extract(body,'$.status')) STORED,
    position INTEGER GENERATED ALWAYS AS (json_extract(body,'$.position')) STORED
);
CREATE INDEX work_items_board ON work_items(deleted,status,position,id);
CREATE INDEX work_items_order ON work_items(deleted,position,id);
CREATE INDEX work_items_home ON work_items(home_workspace_id,deleted);
CREATE TABLE work_item_repositories (
    item_id TEXT NOT NULL REFERENCES work_items(id),
    repository_id TEXT NOT NULL REFERENCES work_repositories(id),
    position INTEGER NOT NULL CHECK(position>=0),
    PRIMARY KEY(item_id,repository_id)
);
CREATE INDEX work_item_repository ON work_item_repositories(repository_id,item_id);
CREATE VIRTUAL TABLE work_items_fts USING fts5(title,description,content='work_items',content_rowid='rowid');
CREATE TRIGGER work_items_insert AFTER INSERT ON work_items BEGIN
    INSERT INTO work_items_fts(rowid,title,description) VALUES(new.rowid,new.title,new.description);
END;
CREATE TRIGGER work_items_update AFTER UPDATE OF body ON work_items
WHEN old.title IS NOT new.title OR old.description IS NOT new.description BEGIN
    INSERT INTO work_items_fts(work_items_fts,rowid,title,description) VALUES('delete',old.rowid,old.title,old.description);
    INSERT INTO work_items_fts(rowid,title,description) VALUES(new.rowid,new.title,new.description);
END;
CREATE TABLE work_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL,
    entity_id TEXT NOT NULL, revision INTEGER NOT NULL, at INTEGER NOT NULL,
    payload TEXT NOT NULL CHECK(json_valid(payload))
);
CREATE INDEX work_events_entity ON work_events(entity_id,sequence);
CREATE TABLE work_requests (
    id TEXT PRIMARY KEY, method TEXT NOT NULL, input TEXT NOT NULL,
    response TEXT NOT NULL CHECK(json_valid(response)), sequence INTEGER NOT NULL
);
CREATE TABLE work_revisions (
    entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, revision INTEGER NOT NULL,
    body TEXT NOT NULL CHECK(json_valid(body)),
    PRIMARY KEY(entity_type,entity_id,revision)
);
