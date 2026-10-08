//! Profile-scoped work items and durable repository groups. These tables are
//! separate from repository-local execution tasks and never acquire their leases.
//! Every accepted mutation, revision and event commits in one transaction.

mod attention;
mod automation;
mod briefs;
mod decisions;
mod enhancements;
mod input;
mod items;
mod notifications;
mod runs;

pub use briefs::{AGENT_GUIDANCE, AGENT_GUIDANCE_HEADING};
pub use runs::{DEFAULT_ACTIVE_RUNS, MAX_ACTIVE_RUNS_CEILING};

use crate::Store;
use input::Input;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::HashSet;

const SCHEMA: &str = include_str!("schema.sql");
/// The workbench schema this build reads and writes. A profile at any other
/// version is refused, so every host sharing a profile must be built at the
/// same one; `dcstore --version` reports it so a host can check that before
/// handing a profile to a store it did not build.
pub const WORKBENCH_SCHEMA: i64 = 11;
const MAX_INTEGER: i64 = 9_007_199_254_740_990;
// Reserve envelope/cursor space within a 2 MiB response. The collector stops
// fetching rows at this bound, including for full revision and event documents.
const MAX_PAGE_DOCUMENTS: usize = 2 * 1024 * 1024 - 4096;
const STATUSES: &[&str] = &["inbox", "backlog", "ready", "in_progress", "review", "done"];

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
type Result<T> = std::result::Result<T, Error>;

impl Error {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input",
            message: message.into(),
        }
    }
    fn missing() -> Self {
        Self {
            code: "not_found",
            message: "the requested record does not exist".into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            code: "store_error",
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Copy)]
enum Entity {
    Workspace,
    Repository,
    Item,
    Enhancement,
    Automation,
    Run,
    Attention,
    NotificationSettings,
    NotificationDelivery,
    Decision,
}
impl Entity {
    fn table(self) -> &'static str {
        match self {
            Self::Workspace => "work_workspaces",
            Self::Repository => "work_repositories",
            Self::Item => "work_items",
            Self::Enhancement => "work_enhancements",
            Self::Automation => "work_automation",
            Self::Run => "work_runs",
            Self::Attention => "work_attention",
            Self::NotificationSettings => "work_notification_settings",
            Self::NotificationDelivery => "work_notification_deliveries",
            Self::Decision => "work_decisions",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Repository => "repository",
            Self::Item => "item",
            Self::Enhancement => "enhancement",
            Self::Automation => "automation",
            Self::Run => "run",
            Self::Attention => "attention",
            Self::NotificationSettings => "notification_settings",
            Self::NotificationDelivery => "notification_delivery",
            Self::Decision => "decision",
        }
    }
    fn live(self) -> &'static str {
        match self {
            Self::Repository
            | Self::Enhancement
            | Self::Automation
            | Self::Run
            | Self::Attention
            | Self::NotificationSettings
            | Self::NotificationDelivery
            | Self::Decision => "1",
            _ => "deleted=0",
        }
    }
}

impl Store {
    /// Shared CLI/served boundary for the workbench. The Go host supplies a
    /// distinct profile database; no task operation reads or changes repo leases.
    pub fn workbench_request(&self, method: &str, raw: &str) -> Result<String> {
        let input = Input::new(&self.conn, raw)?;
        let writing = matches!(
            method,
            "workspaces.put"
                | "workspaces.delete"
                | "repositories.put"
                | "repositories.relink"
                | "items.put"
                | "items.delete"
                | "items.restore"
                | "enhancements.create"
                | "enhancements.complete"
                | "enhancements.accept"
                | "enhancements.dismiss"
                | "enhancements.undo"
                | "enhancements.claim"
                | "enhancements.recover"
                | "enhancements.revise"
                | "automation.put"
                | "automation.prepare"
                | "runs.prepare"
                | "runs.claim"
                | "runs.started"
                | "runs.protocol"
                | "runs.finish"
                | "runs.reconcile"
                | "runs.cancel"
                | "attention.update"
                | "notifications.settings.put"
                | "notifications.claim"
                | "notifications.finish"
                | "notifications.activate"
                | "notifications.ack"
                | "decisions.create"
                | "decisions.decide"
                | "decisions.claim"
                | "decisions.resolve"
        );
        if !writing
            && !matches!(
                method,
                "workspaces.list"
                    | "workspaces.get"
                    | "repositories.list"
                    | "repositories.get"
                    | "items.list"
                    | "items.get"
                    | "items.brief.get"
                    | "events.list"
                    | "items.history"
                    | "enhancements.get"
                    | "enhancements.list"
                    | "automation.get"
                    | "automation.list"
                    | "runs.get"
                    | "runs.list"
                    | "attention.get"
                    | "attention.list"
                    | "notifications.settings.get"
                    | "notifications.delivery.get"
                    | "notifications.pending.list"
                    | "notifications.activations.list"
                    | "decisions.get"
                    | "decisions.list"
            )
        {
            return Err(Error {
                code: "unknown_method",
                message: format!("unknown workbench method {method}"),
            });
        }
        ensure_schema(&self.conn)?;
        let behavior = if writing {
            TransactionBehavior::Immediate
        } else {
            TransactionBehavior::Deferred
        };
        let tx = Transaction::new_unchecked(&self.conn, behavior)?;
        let result = if writing {
            let now_ms = self.workbench_now_millis()?;
            mutate(&input, method, now_ms / 1000, now_ms)?
        } else {
            let now = if matches!(
                method,
                "attention.get"
                    | "attention.list"
                    | "notifications.pending.list"
                    | "decisions.get"
                    | "decisions.list"
            ) {
                self.workbench_now_millis()? / 1000
            } else {
                0
            };
            read(&input, method, now)?
        };
        tx.commit()?;
        Ok(result)
    }

    fn workbench_now_millis(&self) -> Result<i64> {
        let invalid = || Error::invalid("clock is outside the workbench timestamp range");
        let now = match &self.now_fn {
            Some(clock) => clock().checked_mul(1000).ok_or_else(invalid)?,
            None => i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| invalid())?
                    .as_millis(),
            )
            .map_err(|_| invalid())?,
        };
        if !(0..=MAX_INTEGER).contains(&now) {
            return Err(invalid());
        }
        Ok(now)
    }
}

fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if !foreign_keys {
        return Err(Error {
            code: "store_error",
            message: "foreign key enforcement is unavailable".into(),
        });
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_meta')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_meta')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            conn.execute_batch(SCHEMA)?;
        }
        tx.commit()?;
    }
    let mut version: i64 =
        conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
    if version == 1 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 1 {
            conn.execute_batch(include_str!("enhancements.sql"))?;
            version = 2;
        }
        tx.commit()?;
    }
    if version == 2 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 2 {
            conn.execute("UPDATE work_meta SET version=3 WHERE id=1", [])?;
            version = 3;
        }
        tx.commit()?;
    }
    if version == 3 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 3 {
            conn.execute_batch(include_str!("automation.sql"))?;
            version = 4;
        }
        tx.commit()?;
    }
    if version == 4 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 4 {
            conn.execute_batch(include_str!("runs.sql"))?;
            version = 5;
        }
        tx.commit()?;
    }
    if version == 5 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 5 {
            conn.execute_batch(include_str!("attention.sql"))?;
            version = 6;
        }
        tx.commit()?;
    }
    if version == 6 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 6 {
            conn.execute_batch(include_str!("notifications.sql"))?;
            version = 7;
        }
        tx.commit()?;
    }
    if version == 7 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 7 {
            conn.execute_batch(include_str!("decisions.sql"))?;
            version = 8;
        }
        tx.commit()?;
    }
    if version == 8 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 8 {
            // New run kinds and claim/protocol invariants require every host to
            // understand this contract before it may open the profile.
            conn.execute("UPDATE work_meta SET version=9 WHERE id=1", [])?;
            version = 9;
        }
        tx.commit()?;
    }
    if version == 9 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 9 {
            // Native process identity now precedes provider initialization;
            // failed initialization has no fabricated thread or configuration.
            conn.execute("UPDATE work_meta SET version=10 WHERE id=1", [])?;
            version = 10;
        }
        tx.commit()?;
    }
    if version == 10 {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        version = conn.query_row("SELECT version FROM work_meta WHERE id=1", [], |r| r.get(0))?;
        if version == 10 {
            // Archive, completion time, checklists and task links. A host that
            // builds a task body without them would unarchive and unlink every
            // task it saves, so older hosts must refuse this profile.
            conn.execute_batch(include_str!("items.sql"))?;
            version = 11;
        }
        tx.commit()?;
    }
    if version != WORKBENCH_SCHEMA {
        return Err(Error {
            code: "schema_unsupported",
            message: format!("workbench schema {version} is not supported"),
        });
    }
    ensure_index(conn, MEMBER_INDEX, MEMBER_INDEX_SQL)?;
    Ok(())
}

/// Lets a workspace's membership join check `deleted` and read `position`
/// without loading each member's whole task row. A query aid, not a contract,
/// so it is not a schema version: a host that does not know it is unaffected.
const MEMBER_INDEX: &str = "work_items_member";
const MEMBER_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS work_items_member ON work_items(id,deleted,position)";

/// Creates `name` when missing. Looked up first so opening an indexed profile
/// takes no write lock; created under an immediate transaction otherwise.
fn ensure_index(conn: &Connection, name: &str, create: &str) -> Result<()> {
    let present = || -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
            [name],
            |r| r.get(0),
        )?)
    };
    if present()? {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if !present()? {
        conn.execute_batch(create)?;
    }
    tx.commit()?;
    Ok(())
}

fn mutate(input: &Input<'_>, method: &str, now: i64, now_ms: i64) -> Result<String> {
    let request = input.id("request_id")?;
    let normalized = input.normalized()?;
    let prior: Option<(String, String, String)> = input
        .conn
        .query_row(
            "SELECT method,input,response FROM work_requests WHERE id=?1",
            [&request],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((old_method, old_input, response)) = prior {
        if old_method != method || old_input != normalized {
            return Err(Error {
                code: "idempotency_conflict",
                message: "request_id was already used for a different request".into(),
            });
        }
        // A mutation receipt can be replayed; permission to create an external
        // process cannot. Inspect runs.get after an uncertain claim response.
        if matches!(
            method,
            "runs.claim" | "notifications.claim" | "decisions.claim"
        ) {
            return Err(Error {
                code: "claim_consumed",
                message: "this launch claim was already consumed; inspect the run instead of spawning again".into(),
            });
        }
        return Ok(response);
    }
    let id = input.id("id")?;
    let expected = input.integer("expected_revision", None, MAX_INTEGER)?;
    // A restore is checked against the deleted row's revision, which is the
    // one the caller read from history; every other write against the live row.
    let live = if method == "items.restore" {
        items::refuse_live_restore(input.conn, &id)?;
        "deleted=1"
    } else {
        ""
    };
    let entity = match method {
        "workspaces.put" | "workspaces.delete" => Entity::Workspace,
        "repositories.put" | "repositories.relink" => Entity::Repository,
        "automation.put" => Entity::Automation,
        "attention.update" => Entity::Attention,
        "notifications.settings.put" => Entity::NotificationSettings,
        "decisions.create" | "decisions.decide" | "decisions.claim" | "decisions.resolve" => {
            Entity::Decision
        }
        "notifications.claim"
        | "notifications.finish"
        | "notifications.activate"
        | "notifications.ack" => Entity::NotificationDelivery,
        "runs.prepare" | "runs.claim" | "runs.started" | "runs.protocol" | "runs.finish"
        | "runs.reconcile" | "runs.cancel" => Entity::Run,
        "enhancements.create"
        | "enhancements.complete"
        | "enhancements.accept"
        | "enhancements.dismiss"
        | "enhancements.undo"
        | "enhancements.claim"
        | "enhancements.recover"
        | "enhancements.revise"
        | "automation.prepare" => Entity::Enhancement,
        _ => Entity::Item,
    };
    let current: Option<i64> = input
        .conn
        .query_row(
            &format!(
                "SELECT revision FROM {} WHERE id=?1 AND {}",
                entity.table(),
                if live.is_empty() { entity.live() } else { live }
            ),
            [&id],
            |r| r.get(0),
        )
        .optional()?;
    if current.unwrap_or(0) != expected {
        return Err(Error {
            code: "revision_conflict",
            message: format!(
                "expected revision {expected}; current revision is {}",
                current.unwrap_or(0)
            ),
        });
    }
    let revision = expected + 1;
    let affected = match method {
        method if matches!(entity, Entity::Decision) => {
            decisions::mutate(input, method, &id, revision, now)?;
            Affected::none()
        }
        "notifications.settings.put" => {
            notifications::settings(input, &id, revision, now)?;
            Affected::none()
        }
        method if matches!(entity, Entity::NotificationDelivery) => {
            notifications::mutate(input, method, &id, revision, now)?;
            Affected::none()
        }
        "attention.update" => {
            attention::update(input, &id, revision, now)?;
            Affected::none()
        }
        "workspaces.put" => {
            put_workspace(input, &id, revision, now)?;
            Affected::none()
        }
        "repositories.put" => {
            put_repository(input, &id, revision, now)?;
            Affected::none()
        }
        "repositories.relink" => {
            if current.is_none() {
                return Err(Error::missing());
            }
            relink_repository(input, &id, revision, now)?;
            Affected::none()
        }
        "items.put" => {
            put_item(input, &id, revision, now, now_ms)?;
            Affected::none()
        }
        "items.restore" => {
            items::restore(input, &id, revision, now)?;
            Affected::none()
        }
        "automation.put" => {
            automation::put(input, &id, revision, now)?;
            Affected::none()
        }
        "automation.prepare" => {
            automation::prepare(input, &id, revision, now, now_ms)?;
            Affected::none()
        }
        method if matches!(entity, Entity::Enhancement) => {
            enhancements::mutate(input, method, &id, revision, now)?;
            Affected::none()
        }
        method if matches!(entity, Entity::Run) => {
            runs::mutate(input, method, &id, revision, now)?;
            Affected::none()
        }
        _ => {
            input.fields(&["request_id", "id", "expected_revision"])?;
            if current.is_none() {
                return Err(Error::missing());
            }
            delete(input, entity, &id, revision, now)?
        }
    };
    let response = record_change(input.conn, entity, &id, revision, method, now, &affected)?;
    let response = if method == "items.put" {
        automation::receipt(input.conn, &response, &id, &request)?
    } else {
        response
    };
    input.conn.execute(
        "INSERT INTO work_requests(id,method,input,response,sequence) VALUES(?1,?2,?3,?4,json_extract(?4,'$.sequence'))",
        params![request, method, normalized, response],
    )?;
    Ok(response)
}

fn record_change(
    conn: &Connection,
    entity: Entity,
    id: &str,
    revision: i64,
    method: &str,
    now: i64,
    affected: &Affected,
) -> Result<String> {
    let body: String = if matches!(entity, Entity::Attention) {
        attention::body(conn, id, now)?
    } else if matches!(entity, Entity::Decision) {
        decisions::projection(conn, id, now)?
    } else {
        conn.query_row(
            &format!("SELECT body FROM {} WHERE id=?1", entity.table()),
            [&id],
            |r| r.get(0),
        )?
    };
    conn.execute(
        "INSERT INTO work_revisions(entity_type,entity_id,revision,body) VALUES(?1,?2,?3,?4)",
        params![entity.name(), id, revision, body],
    )?;
    let payload:String=conn.query_row("SELECT json_object('item',json(?1),'affected_item_ids',json(?2),'affected_item_count',?3,'affected_item_ids_truncated',json(CASE WHEN ?3>200 THEN 'true' ELSE 'false' END))",params![body,affected.ids,affected.total],|r|r.get(0))?;
    conn.execute(
        "INSERT INTO work_events(kind,entity_id,revision,at,payload) VALUES(?1,?2,?3,?4,?5)",
        params![method, id, revision, now, payload],
    )?;
    let sequence = conn.last_insert_rowid();
    attention::record_event(conn, method, &body, sequence, now)?;
    let response: String = conn.query_row(
        "SELECT json_set(?1,'$.ok',json('true'),'$.sequence',?2)",
        params![payload, sequence],
        |r| r.get(0),
    )?;
    Ok(response)
}

fn repository_ids(input: &Input<'_>, required: bool) -> Result<Vec<String>> {
    let ids = input.strings(
        "repository_ids",
        if required { 64 } else { 10_000 },
        128,
        required,
    )?;
    if required && ids.is_empty() {
        return Err(Error::invalid(
            "a saved task requires at least one repository",
        ));
    }
    let mut seen = HashSet::new();
    for id in &ids {
        if !seen.insert(id) {
            return Err(Error::invalid("repository_ids contains duplicates"));
        }
        let exists: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_repositories WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::invalid("a linked repository is not registered"));
        }
    }
    Ok(ids)
}

fn put_workspace(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "name",
        "description",
        "icon",
        "color",
        "position",
        "archived",
        "pinned",
        "repository_ids",
    ])?;
    let name = input.required_text("name", 300)?;
    let description = input.text("description", 16_384)?.unwrap_or_default();
    let icon = input.text("icon", 64)?.unwrap_or_default();
    let color = input.text("color", 64)?.unwrap_or_default();
    let position = input.integer("position", Some(0), MAX_INTEGER)?;
    let archived = input.boolean("archived", false)?;
    let pinned = input.boolean("pinned", false)?;
    let ids = repository_ids(input, false)?;
    let body:String=input.conn.query_row(
        "SELECT json_object('id',?1,'revision',?2,'name',?3,'description',?4,'icon',?5,'color',?6,'position',?7,'archived',json(CASE WHEN ?8 THEN 'true' ELSE 'false' END),'pinned',json(CASE WHEN ?9 THEN 'true' ELSE 'false' END),'repository_ids',json(coalesce(json_extract(?10,'$.repository_ids'),'[]')),'updated_at',?11)",
        params![id,revision,name,description,icon,color,position,archived,pinned,input.raw,now],|r|r.get(0))?;
    put_body(input.conn, Entity::Workspace, id, revision, &body)?;
    replace_links(
        input.conn,
        "work_workspace_repositories",
        "workspace_id",
        id,
        &ids,
    )?;
    Ok(())
}

fn put_repository(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "name",
        "identity_key",
        "remote_url",
    ])?;
    let name = input.required_text("name", 300)?;
    let identity = input.required_text("identity_key", 4096)?;
    let remote = input.text("remote_url", 4096)?;
    let old: Option<String> = input
        .conn
        .query_row(
            "SELECT identity_key FROM work_repositories WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if old.as_ref().is_some_and(|old| old != &identity) {
        return Err(Error::invalid(
            "repository identity is immutable; relink its checkout instead",
        ));
    }
    let duplicate: bool = input.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_repositories WHERE identity_key=?1 AND id<>?2)",
        params![identity, id],
        |r| r.get(0),
    )?;
    if duplicate {
        return Err(Error::invalid("repository identity is already registered"));
    }
    let body:String=input.conn.query_row("SELECT json_object('id',?1,'revision',?2,'name',?3,'identity_key',?4,'remote_url',?5,'updated_at',?6)",params![id,revision,name,identity,remote,now],|r|r.get(0))?;
    put_body(input.conn, Entity::Repository, id, revision, &body)
}

/// Point a registered repository at the checkout it now lives in.
///
/// The repository keeps its id, so every task link, workspace membership, run
/// and revision stays attached; only `identity_key` (and optionally the display
/// name) changes, and the previous identity remains in `work_revisions`.
///
/// Two distinct clones are never merged by this method. A target identity that
/// another repository already holds is refused, unless the caller names that
/// repository in `absorb_id` *and* it holds nothing: no task links, no
/// workspace memberships and no runs. That is the record a host registers on
/// its own the first time it opens the moved checkout, before anyone relinks.
/// It is removed in the same transaction, so the identity has one owner.
fn relink_repository(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "identity_key",
        "name",
        "absorb_id",
    ])?;
    let identity = input.required_text("identity_key", 4096)?;
    let name = input.text("name", 300)?;
    if name.as_deref().is_some_and(|name| name.trim().is_empty()) {
        return Err(Error::invalid("name must not be blank"));
    }
    let absorb = input.optional_id("absorb_id")?;
    if absorb.as_deref() == Some(id) {
        return Err(Error::invalid("a repository cannot absorb its own record"));
    }
    let old: String = input.conn.query_row(
        "SELECT identity_key FROM work_repositories WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if old == identity {
        return Err(Error::invalid(
            "the repository is already linked to that checkout",
        ));
    }
    if runs::active_on_repository(input.conn, id, now)? {
        return Err(Error {
            code: "repository_busy",
            message: "a run is active on this repository; finish or cancel it before relinking"
                .into(),
        });
    }
    let holder: Option<String> = input
        .conn
        .query_row(
            "SELECT id FROM work_repositories WHERE identity_key=?1 AND id<>?2",
            params![identity, id],
            |r| r.get(0),
        )
        .optional()?;
    match (holder, absorb) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(Error::invalid(
                "absorb_id does not hold the requested identity",
            ));
        }
        (Some(_), None) => {
            return Err(Error::invalid(
                "repository identity is already registered; name that record in absorb_id to absorb it if it is empty",
            ));
        }
        (Some(holder), Some(absorb)) if holder != absorb => {
            return Err(Error::invalid(
                "absorb_id does not hold the requested identity",
            ));
        }
        (Some(holder), Some(_)) => {
            let (links, members, runs): (i64, i64, i64) = input.conn.query_row(
                "SELECT (SELECT count(*) FROM work_item_repositories WHERE repository_id=?1),(SELECT count(*) FROM work_workspace_repositories WHERE repository_id=?1),(SELECT count(*) FROM work_runs WHERE repository_id=?1)",
                [&holder],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if links + members + runs > 0 {
                return Err(Error {
                    code: "repository_not_empty",
                    message: format!(
                        "the repository registered at that checkout holds {links} task link(s), {members} workspace membership(s) and {runs} run(s); distinct records are never merged"
                    ),
                });
            }
            input
                .conn
                .execute("DELETE FROM work_repositories WHERE id=?1", [&holder])?;
        }
    }
    let body: String = input.conn.query_row(
        "SELECT json_set(body,'$.identity_key',?2,'$.name',coalesce(?3,json_extract(body,'$.name')),'$.revision',?4,'$.updated_at',?5) FROM work_repositories WHERE id=?1",
        params![id, identity, name, revision, now],
        |r| r.get(0),
    )?;
    put_body(input.conn, Entity::Repository, id, revision, &body)
}

fn put_item(input: &Input<'_>, id: &str, revision: i64, now: i64, now_ms: i64) -> Result<()> {
    input.fields(&[
        "request_id",
        "id",
        "expected_revision",
        "title",
        "description",
        "kind",
        "status",
        "priority",
        "severity",
        "owner",
        "due_at",
        "labels",
        "acceptance_criteria",
        "repository_ids",
        "primary_repository_id",
        "home_workspace_id",
        "position",
        "locked_fields",
        "logs",
        "archived",
        "checklist",
        "links",
    ])?;
    enhancements::fields(input, "locked_fields", false)?;
    input.text("logs", input::MAX_LOGS_BYTES)?;
    let title = input.required_text("title", 1200)?;
    if title.chars().count() > 300 {
        return Err(Error::invalid("title exceeds 300 characters"));
    }
    let description = input.text("description", 65_536)?.unwrap_or_default();
    let kind = input.text("kind", 64)?.unwrap_or_else(|| "feature".into());
    if kind.trim().is_empty() {
        return Err(Error::invalid("kind must not be blank"));
    }
    let status = input.text("status", 32)?.unwrap_or_else(|| "inbox".into());
    if !STATUSES.contains(&status.as_str()) {
        return Err(Error::invalid("unknown task status"));
    }
    let priority = input.integer("priority", Some(1), 3)?;
    let position = input.integer("position", Some(0), MAX_INTEGER)?;
    let severity = input.text("severity", 32)?;
    if severity
        .as_deref()
        .is_some_and(|v| !["low", "medium", "high", "critical"].contains(&v))
    {
        return Err(Error::invalid("unknown severity"));
    }
    let owner = input.text("owner", 300)?;
    input.integer("due_at", Some(0), MAX_INTEGER)?;
    input.strings("labels", 64, 128, false)?;
    input.strings("acceptance_criteria", 128, 4096, false)?;
    let ids = repository_ids(input, true)?;
    let primary = input.id("primary_repository_id")?;
    if !ids.contains(&primary) {
        return Err(Error::invalid(
            "primary repository must be among repository_ids",
        ));
    }
    let home = input.optional_id("home_workspace_id")?;
    if let Some(home) = &home {
        let exists: bool = input.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_workspaces WHERE id=?1 AND deleted=0)",
            [home],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::invalid("home workspace does not exist"));
        }
    }
    let body:String=input.conn.query_row(
        "SELECT json_object('id',?1,'revision',?2,'title',?3,'description',?4,'kind',?5,'status',?6,'priority',?7,'severity',?8,'owner',?9,'position',?10,'primary_repository_id',?11,'home_workspace_id',?12,'repository_ids',json(json_extract(?13,'$.repository_ids')),'labels',json(coalesce(json_extract(?13,'$.labels'),'[]')),'acceptance_criteria',json(coalesce(json_extract(?13,'$.acceptance_criteria'),'[]')),'due_at',json_extract(?13,'$.due_at'),'updated_at',?14)",
        params![id,revision,title,description,kind,status,priority,severity,owner,position,primary,home,input.raw,now],|r|r.get(0))?;
    // Older hosts do not know field locks or logs. Omission must preserve them;
    // only an explicit empty array / empty string clears. This also keeps
    // ordinary edits from wiping evidence a previous revision stored.
    let body: String = input.conn.query_row("SELECT json_set(?1,'$.locked_fields',json(coalesce(json_extract(?2,'$.locked_fields'),(SELECT json_extract(body,'$.locked_fields') FROM work_items WHERE id=?3),'[]')),'$.logs',coalesce(json_extract(?2,'$.logs'),(SELECT json_extract(body,'$.logs') FROM work_items WHERE id=?3),''))", params![body,input.raw,id], |r|r.get(0))?;
    let deleted: bool = input.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND deleted=1)",
        [id],
        |r| r.get(0),
    )?;
    if deleted {
        return Err(Error::invalid(
            "deleted task IDs cannot be reused; restore the task instead",
        ));
    }
    let (body, links) = items::apply(input, id, &status, body, now)?;
    let text_changed: bool = input.conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND title=?2 AND description=?3)",
        params![id, title, description],
        |r| r.get(0),
    )?;
    input.conn.execute("INSERT INTO work_items(id,revision,body,home_workspace_id,primary_repository_id) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,body=excluded.body,home_workspace_id=excluded.home_workspace_id,primary_repository_id=excluded.primary_repository_id",params![id,revision,body,home,primary])?;
    replace_links(input.conn, "work_item_repositories", "item_id", id, &ids)?;
    if let Some(links) = &links {
        items::replace_links(input.conn, id, links)?;
    }
    automation::after_save(input, id, &body, text_changed, now_ms)?;
    Ok(())
}

fn put_body(conn: &Connection, entity: Entity, id: &str, revision: i64, body: &str) -> Result<()> {
    if matches!(entity, Entity::Workspace) {
        let deleted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_workspaces WHERE id=?1 AND deleted=1)",
            [id],
            |r| r.get(0),
        )?;
        if deleted {
            return Err(Error::invalid("deleted workspace IDs cannot be reused"));
        }
    }
    conn.execute(&format!("INSERT INTO {}(id,revision,body) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,body=excluded.body",entity.table()),params![id,revision,body])?;
    Ok(())
}

fn replace_links(
    conn: &Connection,
    table: &'static str,
    key: &'static str,
    id: &str,
    repositories: &[String],
) -> Result<()> {
    conn.execute(&format!("DELETE FROM {table} WHERE {key}=?1"), [id])?;
    let mut insert = conn.prepare_cached(&format!(
        "INSERT INTO {table}({key},repository_id,position) VALUES(?1,?2,?3)"
    ))?;
    for (position, repo) in repositories.iter().enumerate() {
        let position =
            i64::try_from(position).map_err(|_| Error::invalid("too many repository links"))?;
        insert.execute(params![id, repo, position])?;
    }
    Ok(())
}

struct Affected {
    ids: String,
    total: i64,
}
impl Affected {
    fn none() -> Self {
        Self {
            ids: "[]".into(),
            total: 0,
        }
    }
}

fn delete(
    input: &Input<'_>,
    entity: Entity,
    id: &str,
    revision: i64,
    now: i64,
) -> Result<Affected> {
    let mut affected = Affected::none();
    if matches!(entity, Entity::Workspace) {
        // Live tasks only. A deleted task keeps the revision and time its
        // deletion gave it — what a restore is checked against and what the
        // deleted list is ordered by — and drops a home that no longer
        // exists when it is restored.
        affected.total = input.conn.query_row(
            "SELECT count(*) FROM work_items WHERE home_workspace_id=?1 AND deleted=0",
            [id],
            |r| r.get(0),
        )?;
        affected.ids=input.conn.query_row("SELECT json_group_array(id) FROM (SELECT id FROM work_items WHERE home_workspace_id=?1 AND deleted=0 ORDER BY id LIMIT 200)",[id],|r|r.get(0))?;
        input.conn.execute("INSERT INTO work_revisions(entity_type,entity_id,revision,body) SELECT 'item',id,revision+1,json_set(body,'$.home_workspace_id',NULL,'$.revision',revision+1,'$.updated_at',?2) FROM work_items WHERE home_workspace_id=?1 AND deleted=0",params![id,now])?;
        input.conn.execute("UPDATE work_items SET home_workspace_id=NULL,revision=revision+1,body=json_set(body,'$.home_workspace_id',NULL,'$.revision',revision+1,'$.updated_at',?2) WHERE home_workspace_id=?1 AND deleted=0",params![id,now])?;
        input.conn.execute(
            "DELETE FROM work_workspace_repositories WHERE workspace_id=?1",
            [id],
        )?;
    }
    input.conn.execute(&format!("UPDATE {} SET deleted=1,revision=?2,body=json_set(body,'$.deleted',json('true'),'$.revision',?2,'$.updated_at',?3) WHERE id=?1",entity.table()),params![id,revision,now])?;
    if matches!(entity, Entity::Item) {
        input
            .conn
            .execute("DELETE FROM work_enhancement_queue WHERE task_id=?1", [id])?;
    }
    Ok(affected)
}

fn read(input: &Input<'_>, method: &str, now: i64) -> Result<String> {
    match method {
        "decisions.get" => decisions::get(input, now),
        "decisions.list" => decisions::list(input, now),
        "workspaces.get" => get(input, Entity::Workspace),
        "repositories.get" => get(input, Entity::Repository),
        "items.get" => get(input, Entity::Item),
        "items.brief.get" => briefs::get(input),
        "enhancements.get" => get(input, Entity::Enhancement),
        "enhancements.list" => enhancements::list(input),
        "automation.get" => get(input, Entity::Automation),
        "automation.list" => automation::list(input),
        "runs.get" => runs::get(input),
        "runs.list" => runs::list(input),
        "notifications.settings.get" => get(input, Entity::NotificationSettings),
        "notifications.delivery.get" => get(input, Entity::NotificationDelivery),
        "notifications.pending.list" => notifications::pending(input, now),
        "notifications.activations.list" => notifications::activations(input),
        "attention.get" => attention::get(input, now),
        "attention.list" => attention::list(input, now),
        "items.list" => list_items(input),
        "workspaces.list" => list_workspaces(input),
        "repositories.list" => list_repositories(input),
        "events.list" => list_events(input),
        "items.history" => history(input),
        _ => Err(Error {
            code: "unknown_method",
            message: "unknown workbench read".into(),
        }),
    }
}

fn get(input: &Input<'_>, entity: Entity) -> Result<String> {
    input.fields(&["id"])?;
    let id = input.id("id")?;
    input.conn.query_row(&format!("SELECT json_object('ok',json('true'),'item',json(body)) FROM {} WHERE id=?1 AND {}",entity.table(),entity.live()),[id],|r|r.get(0)).optional()?.ok_or_else(Error::missing)
}

struct Page {
    limit: i64,
    position: i64,
    id: String,
}

/// The order a task list is read in. Each has its own cursor prefix, so a
/// cursor from one order is refused by the other instead of resuming at a
/// position that means something else there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Board order: `position`, then id, ascending. Cursor `1:`.
    Board,
    /// Most recently completed first: `completed_at` (0 for a task not
    /// Done), then id, both descending. Cursor `2:`.
    Completed,
    /// Most recently changed first: `updated_at`, then id, both descending.
    /// For deleted tasks that is when they were deleted. Cursor `3:`. Not
    /// indexed: it is meant for the deleted list, not a whole board.
    Updated,
}
impl Order {
    fn cursor(self) -> u8 {
        match self {
            Self::Board => 1,
            Self::Completed => 2,
            Self::Updated => 3,
        }
    }
    /// The task column the page is keyed and ordered on.
    fn key(self) -> &'static str {
        match self {
            Self::Board => "t.position",
            Self::Completed => "t.completed_at",
            Self::Updated => "coalesce(json_extract(t.body,'$.updated_at'),0)",
        }
    }
    /// How a row compares with the cursor to come after it.
    fn after(self) -> &'static str {
        match self {
            Self::Board => ">",
            Self::Completed | Self::Updated => "<",
        }
    }
    fn direction(self) -> &'static str {
        match self {
            Self::Board => "",
            Self::Completed | Self::Updated => " DESC",
        }
    }
}

fn page(input: &Input<'_>) -> Result<Page> {
    page_in(input, Order::Board)
}

fn page_in(input: &Input<'_>, order: Order) -> Result<Page> {
    let limit = input.integer("limit", Some(100), 200)?;
    if limit == 0 {
        return Err(Error::invalid("limit must be at least one"));
    }
    let (position, id) = if let Some(cursor) = input.text("cursor", 180)? {
        let prefix = format!("{}:", order.cursor());
        let Some((position, id)) = cursor
            .strip_prefix(prefix.as_str())
            .and_then(|c| c.split_once(':'))
        else {
            return Err(Error::invalid("invalid cursor"));
        };
        let position = position
            .parse::<i64>()
            .map_err(|_| Error::invalid("invalid cursor position"))?;
        if !(0..=MAX_INTEGER).contains(&position)
            || id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::invalid("invalid cursor"));
        }
        (position, id.to_string())
    } else if order != Order::Board {
        // Descending: start above every key. Ids are ASCII, so DEL sorts
        // after every one of them.
        (MAX_INTEGER, "\u{7f}".into())
    } else {
        (0, String::new())
    };
    Ok(Page {
        limit,
        position,
        id,
    })
}

struct PageRows {
    records: Vec<(String, i64, String)>,
    has_more: bool,
}

fn collect_page(rows: &mut rusqlite::Rows<'_>, limit: i64) -> Result<PageRows> {
    collect_page_with(rows, limit, |_| Ok(()))
}

/// [`collect_page`], also handing every fetched row to `each`, lookahead
/// included, so a query can carry a value beside its records.
fn collect_page_with(
    rows: &mut rusqlite::Rows<'_>,
    limit: i64,
    mut each: impl FnMut(&rusqlite::Row<'_>) -> Result<()>,
) -> Result<PageRows> {
    let mut records = Vec::new();
    let mut bytes = 0;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        each(row)?;
        if count == limit {
            return Ok(PageRows {
                records,
                has_more: true,
            });
        }
        let body: String = row.get(0)?;
        if bytes + body.len() + 1 > MAX_PAGE_DOCUMENTS {
            if records.is_empty() {
                return Err(Error {
                    code: "response_too_large",
                    message: "one record exceeds the page byte budget".into(),
                });
            }
            return Ok(PageRows {
                records,
                has_more: true,
            });
        }
        bytes += body.len() + 1;
        records.push((body, row.get(1)?, row.get(2)?));
        count += 1;
    }
    Ok(PageRows {
        records,
        has_more: false,
    })
}

fn page_response(conn: &Connection, page: PageRows, total: i64) -> Result<String> {
    page_response_in(conn, page, total, Order::Board)
}

fn page_response_in(conn: &Connection, page: PageRows, total: i64, order: Order) -> Result<String> {
    let PageRows { records, has_more } = page;
    let cursor = if has_more {
        records
            .last()
            .map(|(_, position, id)| format!("{}:{position}:{id}", order.cursor()))
    } else {
        None
    };
    let shown = i64::try_from(records.len()).map_err(|_| Error::invalid("page too large"))?;
    // Each document is generated by SQLite and stored under a json_valid CHECK.
    // Joining complete JSON values avoids reparsing every body in another engine.
    let documents = format!(
        "[{}]",
        records
            .into_iter()
            .map(|(body, _, _)| body)
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(conn.query_row("SELECT json_object('ok',json('true'),'items',json(?1),'total',?2,'shown',?3,'has_more',json(CASE WHEN ?4 THEN 'true' ELSE 'false' END),'next_cursor',?5)",params![documents,total,shown,has_more,cursor],|r|r.get(0))?)
}

fn list_workspaces(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor", "include_archived"])?;
    let p = page(input)?;
    let archived = input.boolean("include_archived", false)?;
    let total = input.conn.query_row(
        "SELECT count(*) FROM work_workspaces WHERE deleted=0 AND (?1 OR archived=0)",
        [archived],
        |r| r.get(0),
    )?;
    let mut stmt=input.conn.prepare("SELECT json_set(json_remove(body,'$.description','$.repository_ids'),'$.repository_count',json_array_length(body,'$.repository_ids')),position,id FROM work_workspaces WHERE deleted=0 AND (?1 OR archived=0) AND (position,id)>(?2,?3) ORDER BY position,id LIMIT ?4")?;
    let rows = collect_page(
        &mut stmt.query(params![archived, p.position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}

fn require_record(
    input: &Input<'_>,
    entity: Entity,
    id: Option<&str>,
    include_deleted: bool,
) -> Result<()> {
    if let Some(id) = id {
        let exists: bool = input.conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE id=?1 AND (?2 OR {}))",
                entity.table(),
                entity.live()
            ),
            params![id, include_deleted],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::missing());
        }
    }
    Ok(())
}

fn list_repositories(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor", "workspace_id", "ungrouped"])?;
    let p = page(input)?;
    let workspace = input.optional_id("workspace_id")?;
    let ungrouped = input.boolean("ungrouped", false)?;
    if ungrouped && workspace.is_some() {
        return Err(Error::invalid(
            "workspace and ungrouped scopes are exclusive",
        ));
    }
    require_record(input, Entity::Workspace, workspace.as_deref(), false)?;
    const FILTER: &str = "(?1 IS NULL OR EXISTS(SELECT 1 FROM work_workspace_repositories m WHERE m.repository_id=r.id AND m.workspace_id=?1)) AND (NOT ?2 OR NOT EXISTS(SELECT 1 FROM work_workspace_repositories m WHERE m.repository_id=r.id))";
    let total = input.conn.query_row(
        &format!("SELECT count(*) FROM work_repositories r WHERE {FILTER}"),
        params![workspace, ungrouped],
        |r| r.get(0),
    )?;
    let mut stmt=input.conn.prepare(&format!("SELECT r.body,coalesce(m.position,0) AS sort_position,r.id FROM work_repositories r LEFT JOIN work_workspace_repositories m ON m.repository_id=r.id AND m.workspace_id=?1 WHERE {FILTER} AND (coalesce(m.position,0),r.id)>(?3,?4) ORDER BY sort_position,r.id LIMIT ?5"))?;
    let rows = collect_page(
        &mut stmt.query(params![workspace, ungrouped, p.position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}

fn list_items(input: &Input<'_>) -> Result<String> {
    input.fields(&[
        "limit",
        "cursor",
        "workspace_id",
        "repository_id",
        "status",
        "query",
        "archived",
        "order",
        "deleted",
    ])?;
    let order = match input.text("order", 32)?.as_deref() {
        None | Some("board") => Order::Board,
        Some("completed") => Order::Completed,
        Some("updated") => Order::Updated,
        Some(_) => {
            return Err(Error::invalid("order must be board, completed or updated"));
        }
    };
    let p = page_in(input, order)?;
    // None reads both; the archive and the board each name the side they show.
    let archived = match input.kind("archived")?.as_deref() {
        None | Some("null") => None,
        Some(_) => Some(input.boolean("archived", false)?),
    };
    // Deleted tasks instead of live ones: what a restore is chosen from.
    let deleted = input.boolean("deleted", false)?;
    let workspace = input.optional_id("workspace_id")?;
    let repo = input.optional_id("repository_id")?;
    if workspace.is_some() && repo.is_some() {
        return Err(Error::invalid(
            "workspace and repository scopes are exclusive",
        ));
    }
    require_record(input, Entity::Workspace, workspace.as_deref(), false)?;
    require_record(input, Entity::Repository, repo.as_deref(), false)?;
    let status = input.text("status", 32)?;
    if status
        .as_deref()
        .is_some_and(|status| !STATUSES.contains(&status))
    {
        return Err(Error::invalid("unknown task status"));
    }
    let query = input.text("query", 512)?;
    let fts = query.filter(|q| !q.trim().is_empty()).map(|q| {
        let tokens = q
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{s}\"*"))
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            "\"\"".into()
        } else {
            tokens.join(" AND ")
        }
    });
    let filter = ItemFilter {
        status: status.as_deref(),
        archived,
        deleted,
    };
    let (query, values) = item_query(
        workspace.as_deref(),
        repo.as_deref(),
        filter,
        fts.as_deref(),
        false,
    );
    if workspace.is_some() || (repo.is_some() && fts.is_some()) {
        return list_scoped_items(input, &query, values, p, order);
    }
    let total = match fts.as_deref() {
        Some(fts) if repo.is_none() && status.is_none() && archived.is_none() && !deleted => {
            global_search_total(input.conn, fts)?
        }
        _ => input.conn.query_row(
            &format!("SELECT count(*) {query}"),
            rusqlite::params_from_iter(values.iter()),
            |r| r.get(0),
        )?,
    };
    // A result set that fits the maximum page stays driven by indexed hits.
    // For larger global searches, materialize the FTS row IDs once and scan
    // the covering board index until the page fills. Reopening MATCH for every
    // ordered candidate is substantially slower, especially for absent hits.
    let (query, mut values) =
        if fts.is_some() && workspace.is_none() && repo.is_none() && total > 200 {
            item_query(None, None, filter, fts.as_deref(), true)
        } else {
            (query, values)
        };
    let bound = values.len();
    // Select at most a page plus one lookahead before reading/formatting bodies.
    // FTS and membership candidates may arrive in reverse board order, causing
    // a top-N sorter to replace every candidate. The materialization fence
    // prevents discarded candidates from paying JSON projection costs.
    let (key, after, dir) = (order.key(), order.after(), order.direction());
    let mut stmt = input.conn.prepare(&format!(
        "WITH page AS MATERIALIZED (
            SELECT t.rowid AS item_rowid,{key} AS position,t.id {query}
            AND ({key},t.id){after}(?{},?{}) ORDER BY {key}{dir},t.id{dir} LIMIT ?{}
        ) SELECT json_remove(t.body,{CARD_OMITS}),page.position,page.id
          FROM page CROSS JOIN work_items t ON t.rowid=page.item_rowid
          ORDER BY page.position{dir},page.id{dir}",
        bound + 1,
        bound + 2,
        bound + 3
    ))?;
    values.extend([
        rusqlite::types::Value::Integer(p.position),
        rusqlite::types::Value::Text(p.id),
        rusqlite::types::Value::Integer(p.limit + 1),
    ]);
    let rows = collect_page(
        &mut stmt.query(rusqlite::params_from_iter(values.iter()))?,
        p.limit,
    )?;
    page_response_in(input.conn, rows, total, order)
}

/// Body fields a card leaves out: read in full with `items.get`.
const CARD_OMITS: &str = "'$.description','$.acceptance_criteria','$.logs','$.checklist','$.links'";

/// The filters a task list applies besides its scope and search.
#[derive(Clone, Copy)]
struct ItemFilter<'a> {
    status: Option<&'a str>,
    archived: Option<bool>,
    /// Read deleted tasks instead of live ones.
    deleted: bool,
}

/// Live tasks matching an unscoped, unfiltered search.
///
/// Every task row has exactly one full-text row: the insert trigger adds it,
/// the update trigger replaces it, and tasks are only ever soft-deleted. So
/// MATCH alone counts every hit, live or deleted, from the index, and the
/// deleted hits are subtracted. Checking each hit's own row for `deleted`
/// instead read every matching task row, which at 100,000 hits was most of a
/// broad search. The deleted side walks only deleted rows in the board index
/// and builds the hit set only if there is one.
fn global_search_total(conn: &Connection, fts: &str) -> Result<i64> {
    let hits: i64 = conn.query_row(
        "SELECT count(*) FROM work_items_fts WHERE work_items_fts MATCH ?1",
        [fts],
        |r| r.get(0),
    )?;
    let deleted: i64 = conn.query_row(DELETED_SEARCH_HITS, [fts], |r| r.get(0))?;
    Ok(hits - deleted)
}

/// Deleted tasks that match `?1`, found from the deleted side: two ranges of
/// the board index (`deleted<>0` would scan every task), each probing the hit
/// set. The unary `+` keeps the planner from driving the other way — from
/// every hit to its task row, the very cost this count exists to avoid.
const DELETED_SEARCH_HITS: &str = "SELECT count(*) FROM work_items t
    WHERE (t.deleted<0 OR t.deleted>0)
      AND +t.rowid IN(SELECT rowid FROM work_items_fts WHERE work_items_fts MATCH ?1)";

/// A workspace's tasks, or a repository's narrowed by search.
///
/// Finding these candidates is the cost — the workspace membership union, the
/// full-text hit set — and counting them and selecting a page each paid it in
/// full, in two statements. One statement materializes them once; the total is
/// counted from that set and the page cut from it, so the work is done once.
/// Bodies are still read only for the page and its lookahead row.
fn list_scoped_items(
    input: &Input<'_>,
    query: &str,
    values: Vec<rusqlite::types::Value>,
    p: Page,
    order: Order,
) -> Result<String> {
    let bound = values.len();
    let (key, after, dir) = (order.key(), order.after(), order.direction());
    let mut stmt = input.conn.prepare(&format!(
        "WITH candidates AS MATERIALIZED (
            SELECT t.rowid AS item_rowid,{key} AS position,t.id {query}
        ), page AS MATERIALIZED (
            SELECT item_rowid,position,id FROM candidates
            WHERE (position,id){after}(?{},?{}) ORDER BY position{dir},id{dir} LIMIT ?{}
        ) SELECT json_remove(t.body,{CARD_OMITS}),page.position,page.id,
                 (SELECT count(*) FROM candidates)
          FROM page CROSS JOIN work_items t ON t.rowid=page.item_rowid
          ORDER BY page.position{dir},page.id{dir}",
        bound + 1,
        bound + 2,
        bound + 3
    ))?;
    let mut paged = values.clone();
    paged.extend([
        rusqlite::types::Value::Integer(p.position),
        rusqlite::types::Value::Text(p.id),
        rusqlite::types::Value::Integer(p.limit + 1),
    ]);
    let mut total = None;
    let rows = collect_page_with(
        &mut stmt.query(rusqlite::params_from_iter(paged.iter()))?,
        p.limit,
        |row| {
            if total.is_none() {
                total = Some(row.get(3)?);
            }
            Ok(())
        },
    )?;
    // An empty page carries no row to read the total from. That is a cursor
    // past the end or no match at all, so counting again is rare.
    let total = match total {
        Some(total) => total,
        None => input.conn.query_row(
            &format!("SELECT count(*) {query}"),
            rusqlite::params_from_iter(values.iter()),
            |r| r.get(0),
        )?,
    };
    page_response_in(input.conn, rows, total, order)
}

// One query owner supplies both total and page selection. Tests measure the
// actual generated SQL with the same bundled engine used by the public API.
fn item_query(
    workspace: Option<&str>,
    repo: Option<&str>,
    filter: ItemFilter<'_>,
    fts: Option<&str>,
    ordered_search: bool,
) -> (String, Vec<rusqlite::types::Value>) {
    let mut values = Vec::new();
    let mut bind = |value: &str| {
        values.push(rusqlite::types::Value::Text(value.into()));
        format!("?{}", values.len())
    };
    // A literal: one of two program constants, which keeps the board index usable.
    let deleted = i64::from(filter.deleted);
    let mut filters = vec![format!("t.deleted={deleted}")];
    let source = if let Some(workspace) = workspace {
        let parameter = bind(workspace);
        // UNION deduplicates home membership and every matching linked repo.
        // CROSS JOIN keeps indexed members as the outer loop: an absent or
        // small workspace must not walk the entire profile's live tasks.
        format!(
            "(SELECT id FROM work_items WHERE home_workspace_id={parameter} AND deleted={deleted} UNION SELECT ir.item_id FROM work_workspace_repositories wr CROSS JOIN work_item_repositories ir ON ir.repository_id=wr.repository_id WHERE wr.workspace_id={parameter}) selected CROSS JOIN work_items t ON t.id=selected.id"
        )
    } else if let Some(repo) = repo {
        filters.push(format!("selected.repository_id={}", bind(repo)));
        "work_item_repositories selected CROSS JOIN work_items t ON t.id=selected.item_id".into()
    } else if fts.is_some() && !ordered_search {
        "work_items_fts CROSS JOIN work_items t ON t.rowid=work_items_fts.rowid".into()
    } else {
        "work_items t".into()
    };
    if let Some(status) = filter.status {
        filters.push(format!("t.status={}", bind(status)));
    }
    if let Some(archived) = filter.archived {
        // A literal, not a parameter: it is one of two program constants, and
        // the planner can then use the archive index for it.
        filters.push(format!("t.archived={}", i64::from(archived)));
    }
    if let Some(fts) = fts {
        let parameter = bind(fts);
        filters.push(if workspace.is_some() || repo.is_some() || ordered_search {
            // Evaluate MATCH once. A correlated rowid constraint still reopens
            // the virtual-table search for every member, and its work is not
            // reflected in SQLite's outer VM-step counter.
            format!("t.rowid IN(SELECT rowid FROM work_items_fts WHERE work_items_fts MATCH {parameter})")
        } else {
            format!("work_items_fts MATCH {parameter}")
        });
    }
    (
        format!("FROM {source} WHERE {}", filters.join(" AND ")),
        values,
    )
}

#[cfg(test)]
mod enhancement_lifecycle_tests;
#[cfg(test)]
mod item_query_tests;
#[cfg(test)]
mod logs_tests;

fn list_events(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "after"])?;
    let limit = input.integer("limit", Some(100), 200)?;
    if limit == 0 {
        return Err(Error::invalid("limit must be at least one"));
    }
    let after = input.integer("after", Some(0), MAX_INTEGER)?;
    let total: i64 = input.conn.query_row(
        "SELECT count(*) FROM work_events WHERE sequence>?1",
        [after],
        |r| r.get(0),
    )?;
    let mut stmt=input.conn.prepare("SELECT json_object('sequence',sequence,'kind',kind,'entity_id',entity_id,'revision',revision,'at',at,'payload',json(payload)),sequence,'' FROM work_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
    let rows = collect_page(&mut stmt.query(params![after, limit + 1])?, limit)?;
    series_response(input.conn, rows, total, after)
}

fn history(input: &Input<'_>) -> Result<String> {
    input.fields(&["id", "limit", "after_revision"])?;
    let id = input.id("id")?;
    require_record(input, Entity::Item, Some(&id), true)?;
    let limit = input.integer("limit", Some(100), 200)?;
    if limit == 0 {
        return Err(Error::invalid("limit must be at least one"));
    }
    let after = input.integer("after_revision", Some(0), MAX_INTEGER)?;
    let total:i64=input.conn.query_row("SELECT count(*) FROM work_revisions WHERE entity_type='item' AND entity_id=?1 AND revision>?2",params![id,after],|r|r.get(0))?;
    let mut stmt=input.conn.prepare("SELECT body,revision,'' FROM work_revisions WHERE entity_type='item' AND entity_id=?1 AND revision>?2 ORDER BY revision LIMIT ?3")?;
    let rows = collect_page(&mut stmt.query(params![id, after, limit + 1])?, limit)?;
    series_response(input.conn, rows, total, after)
}

fn series_response(conn: &Connection, page: PageRows, total: i64, after: i64) -> Result<String> {
    let cursor = page
        .records
        .last()
        .map(|(_, sequence, _)| *sequence)
        .unwrap_or(after);
    let shown = i64::try_from(page.records.len()).map_err(|_| Error::invalid("page too large"))?;
    let docs = format!(
        "[{}]",
        page.records
            .into_iter()
            .map(|(body, _, _)| body)
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(conn.query_row("SELECT json_object('ok',json('true'),'items',json(?1),'total',?2,'shown',?3,'next_cursor',?4,'has_more',json(CASE WHEN ?5 THEN 'true' ELSE 'false' END))",params![docs,total,shown,cursor,page.has_more],|r|r.get(0))?)
}
