//! Durable native-delivery claims. An OS submission acknowledgment is not proof
//! of display, review, task completion, or coding-agent permission.
use super::{Error, Input, MAX_INTEGER, Result, attention, collect_page, page, page_response};
use rusqlite::{OptionalExtension, params};

// One canonical eligibility expression for queue reads and atomic claims.
// Quiet-hour minutes are supplied by the native host's current local calendar;
// expiry and snooze use the authoritative store clock, never caller timestamps.
fn eligible() -> String {
    format!("json_extract(s.body,'$.enabled')=1
      AND a.source_sequence>json_extract(s.body,'$.after_sequence')
      AND json_extract(a.body,'$.created_at')>=?1-3600
      AND a.read_at IS NULL AND a.dismissed_at IS NULL
      AND (a.snoozed_until IS NULL OR a.snoozed_until<=?1)
      AND json_extract(({}),'$.target_status')='current'
      AND NOT EXISTS(SELECT 1 FROM work_notification_deliveries d WHERE d.id=a.id)
      AND (json_extract(s.body,'$.quiet_start') IS NULL OR NOT CASE
        WHEN json_extract(s.body,'$.quiet_start')<json_extract(s.body,'$.quiet_end')
        THEN ?2>=json_extract(s.body,'$.quiet_start') AND ?2<json_extract(s.body,'$.quiet_end')
        ELSE ?2>=json_extract(s.body,'$.quiet_start') OR ?2<json_extract(s.body,'$.quiet_end') END)
      AND NOT EXISTS(SELECT 1 FROM json_each(s.body,'$.muted_task_ids') m WHERE m.value=a.task_id)
      AND NOT EXISTS(SELECT 1 FROM json_each(s.body,'$.muted_repository_ids') m JOIN work_item_repositories ir ON ir.repository_id=m.value WHERE ir.item_id=a.task_id)
      AND NOT EXISTS(SELECT 1 FROM json_each(s.body,'$.muted_workspace_ids') m WHERE m.value=t.home_workspace_id OR EXISTS(SELECT 1 FROM work_workspace_repositories wr JOIN work_item_repositories ir ON ir.repository_id=wr.repository_id WHERE wr.workspace_id=m.value AND ir.item_id=a.task_id))",attention::body_sql(1))
}
fn source() -> String {
    format!(
        "{} CROSS JOIN work_notification_settings s",
        attention::FROM
    )
}

pub(super) fn settings(input: &Input<'_>, id: &str, revision: i64, now: i64) -> Result<()> {
    input.fields(&[
        "id",
        "request_id",
        "expected_revision",
        "enabled",
        "sound",
        "background",
        "quiet_start",
        "quiet_end",
        "muted_workspace_ids",
        "muted_repository_ids",
        "muted_task_ids",
    ])?;
    if id != "profile" || revision == 1 {
        return Err(Error::missing());
    }
    let enabled = input.boolean("enabled", false)?;
    let sound = input.boolean("sound", false)?;
    let background = input.boolean("background", false)?;
    let minute = |key| -> Result<Option<i64>> {
        match input.kind(key)?.as_deref() {
            None | Some("null") => Ok(None),
            _ => Ok(Some(input.integer(key, None, 1439)?)),
        }
    };
    let start = minute("quiet_start")?;
    let end = minute("quiet_end")?;
    if start.is_some() != end.is_some() || start.is_some() && start == end {
        return Err(Error::invalid(
            "quiet hours require two different local minutes",
        ));
    }
    // Saved mutes may outlive their scopes. Keep known tombstone identities so
    // deleting a task or workspace cannot lock unrelated preference changes.
    // Delivery eligibility independently requires a current target.
    for (field, table) in [
        ("muted_workspace_ids", "work_workspaces"),
        ("muted_repository_ids", "work_repositories"),
        ("muted_task_ids", "work_items"),
    ] {
        let ids = input.strings(field, 64, 128, false)?;
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        if unique.len() != ids.len() {
            return Err(Error::invalid("duplicate mute scope"));
        }
        for scope in ids {
            let exists: bool = input.conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
                [scope],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::invalid("mute scope does not exist"));
            }
        }
    }
    input.conn.execute("UPDATE work_notification_settings SET revision=?1,body=json_set(body,'$.revision',?1,'$.updated_at',?8,'$.after_sequence',CASE WHEN json_extract(body,'$.enabled')=0 AND ?2=1 THEN (SELECT coalesce(max(sequence),0) FROM work_events) ELSE json_extract(body,'$.after_sequence') END,'$.enabled',json(CASE WHEN ?2 THEN 'true' ELSE 'false' END),'$.sound',json(CASE WHEN ?3 THEN 'true' ELSE 'false' END),'$.background',json(CASE WHEN ?4 THEN 'true' ELSE 'false' END),'$.quiet_start',?5,'$.quiet_end',?6,'$.muted_workspace_ids',json(coalesce(json_extract(?7,'$.muted_workspace_ids'),'[]')),'$.muted_repository_ids',json(coalesce(json_extract(?7,'$.muted_repository_ids'),'[]')),'$.muted_task_ids',json(coalesce(json_extract(?7,'$.muted_task_ids'),'[]'))) WHERE id='profile'",params![revision,enabled,sound,background,start,end,input.raw,now])?;
    Ok(())
}

pub(super) fn pending(input: &Input<'_>, now: i64) -> Result<String> {
    input.fields(&["minute_of_day", "limit", "cursor"])?;
    let minute = input.integer("minute_of_day", None, 1439)?;
    let p = page(input)?;
    let from = source();
    let predicate = eligible();
    let total: i64 = input.conn.query_row(
        &format!("SELECT count(*) FROM {from} WHERE {predicate}"),
        params![now, minute],
        |r| r.get(0),
    )?;
    let position = if p.id.is_empty() {
        MAX_INTEGER
    } else {
        p.position
    };
    let mut stmt=input.conn.prepare(&format!("SELECT {},a.source_sequence,a.id FROM {from} WHERE {predicate} AND (a.source_sequence,a.id)<(?3,?4) ORDER BY a.source_sequence DESC,a.id DESC LIMIT ?5",attention::body_sql(1)))?;
    let rows = collect_page(
        &mut stmt.query(params![now, minute, position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}

pub(super) fn mutate(
    input: &Input<'_>,
    method: &str,
    id: &str,
    revision: i64,
    now: i64,
) -> Result<()> {
    if method == "notifications.claim" {
        input.fields(&["id", "request_id", "expected_revision", "minute_of_day"])?;
        if revision != 1 {
            return Err(Error::invalid("a notice can only be claimed once"));
        }
        let minute = input.integer("minute_of_day", None, 1439)?;
        let source:Option<(String,String)>=input.conn.query_row(&format!("SELECT a.task_id,json_extract(s.body,'$.profile_id') FROM {} WHERE {} AND a.id=?3",source(),eligible()),params![now,minute,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((task, profile)) = source else {
            return Err(Error {
                code: "not_eligible",
                message: "notice is no longer eligible for native delivery".into(),
            });
        };
        let native = format!("gitpulse.{profile}.{id}");
        input.conn.execute("INSERT INTO work_notification_deliveries(id,revision,body) VALUES(?1,1,json_object('id',?1,'revision',1,'task_id',?2,'native_id',?3,'state','uncertain','created_at',?4,'updated_at',?4,'activated_at',NULL,'acknowledged_at',NULL))",params![id,task,native,now])?;
        return Ok(());
    }
    if revision == 1 {
        return Err(Error::missing());
    }
    match method {
        "notifications.finish" => {
            input.fields(&["id", "request_id", "expected_revision", "state"])?;
            let state = input.required_text("state", 20)?;
            if !matches!(state.as_str(), "submitted" | "failed") {
                return Err(Error::invalid(
                    "only a definite OS callback can finish delivery",
                ));
            }
            let current: String = input.conn.query_row(
                "SELECT json_extract(body,'$.state') FROM work_notification_deliveries WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            if current != "uncertain" {
                return Err(Error::invalid("delivery already has a terminal outcome"));
            }
            input.conn.execute("UPDATE work_notification_deliveries SET revision=?2,body=json_set(body,'$.revision',?2,'$.state',?3,'$.updated_at',?4) WHERE id=?1",params![id,revision,state,now])?;
        }
        "notifications.activate" => {
            input.fields(&["id", "request_id", "expected_revision", "native_id"])?;
            let native = input.required_text("native_id", 200)?;
            let current:String=input.conn.query_row("SELECT json_extract(body,'$.native_id') FROM work_notification_deliveries WHERE id=?1",[id],|r|r.get(0))?;
            if native != current {
                return Err(Error::invalid(
                    "notification belongs to a different profile or delivery",
                ));
            }
            // Repeated callbacks cannot reopen an acknowledged activation.
            input.conn.execute("UPDATE work_notification_deliveries SET revision=?2,body=json_set(body,'$.revision',?2,'$.activated_at',coalesce(activated_at,?3),'$.updated_at',?3) WHERE id=?1",params![id,revision,now])?;
        }
        "notifications.ack" => {
            input.fields(&["id", "request_id", "expected_revision"])?;
            let activated: bool = input.conn.query_row(
                "SELECT activated_at IS NOT NULL FROM work_notification_deliveries WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            if !activated {
                return Err(Error::invalid(
                    "notification has no activation to acknowledge",
                ));
            }
            input.conn.execute("UPDATE work_notification_deliveries SET revision=?2,body=json_set(body,'$.revision',?2,'$.acknowledged_at',coalesce(acknowledged_at,?3),'$.updated_at',?3) WHERE id=?1",params![id,revision,now])?;
        }
        _ => return Err(Error::invalid("unsupported delivery mutation")),
    }
    Ok(())
}

pub(super) fn activations(input: &Input<'_>) -> Result<String> {
    input.fields(&["limit", "cursor"])?;
    let p = page(input)?;
    let total:i64=input.conn.query_row("SELECT count(*) FROM work_notification_deliveries WHERE activated_at IS NOT NULL AND acknowledged_at IS NULL",[],|r|r.get(0))?;
    let mut stmt=input.conn.prepare("SELECT body,activated_at,id FROM work_notification_deliveries WHERE activated_at IS NOT NULL AND acknowledged_at IS NULL AND (activated_at,id)>(?1,?2) ORDER BY activated_at,id LIMIT ?3")?;
    let rows = collect_page(
        &mut stmt.query(params![p.position, p.id, p.limit + 1])?,
        p.limit,
    )?;
    page_response(input.conn, rows, total)
}
