//! Merging board tasks for an agent: the work of several cards folded into one
//! card that stays, and the others deleted.
//!
//! # Why it is not one write
//!
//! Each store request is its own transaction and nothing groups two, so a
//! merge is a sequence of writes that can be interrupted after any of them.
//! The order is what keeps it safe: the target is written first, carrying a
//! section per source; only then is each source deleted, and only at exactly
//! the revision whose content the target now holds (`record_and_delete` makes
//! both of its writes conditional on that revision). So at every point each
//! source's work is on a live card — its own or the target — and an
//! interruption leaves at worst a source that is both copied and still there.
//! Running the same call again finishes it: a section already in the target is
//! recognised by its text and not written twice.
//!
//! Two merges racing in opposite directions (A into B while B into A) cannot
//! delete both: whichever delete lands first, the other side's write to the
//! card it deleted fails, and that side stops holding both cards' content.
//!
//! # What the target gets
//!
//! A `## Merged from <item_id>: <title>` section per source, with its status,
//! priority and description; the union of every acceptance criterion and
//! label; the most urgent priority, the highest severity and the earliest due
//! date among them; and a log block naming the sources and the reason. Its
//! title, status, owner, type and placement stay as they were. Source logs
//! are not copied: they stay in each source's revision history, and the
//! response names the sources that had any.

use super::*;

/// Bound on the sources one call merges.
pub(crate) const MAX_MERGE_SOURCES: usize = MAX_RELATED;
/// Read-copy-delete rounds before sources that keep changing are reported
/// as not merged.
const MAX_ROUNDS: usize = 3;
const SECTION: &str = "## Merged from ";

/// A task named by an agent: its task_id or board item_id, and the revision
/// it read it at, when it wants the merge refused if the task changed since.
#[derive(Debug, Clone)]
pub(crate) struct MergeTask {
    pub task: String,
    pub expected_revision: Option<i64>,
}

/// Where a merge stands, for tests that change the board between its writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step<'a> {
    /// Everything was read and checked; nothing is written yet.
    Checked,
    /// The target holds every pending source's section.
    TargetWritten,
    /// About to delete this source.
    BeforeDelete(&'a str),
}

enum State {
    /// Live, still to be copied and deleted, as read this round.
    Pending(Value),
    /// Deleted by this call, at this revision (the one copied). `had_logs`:
    /// it carried logs of its own, which stay in its history.
    Merged {
        title: Value,
        copied_revision: i64,
        had_logs: bool,
    },
    /// Already deleted into this target, by an earlier call.
    AlreadyMerged { title: Value },
    /// Left live (or deleted by someone else); `detail` says why.
    NotMerged { title: Value, detail: String },
}

struct Source {
    requested: String,
    id: String,
    state: State,
}

/// The section a source becomes in the target's description. Its text is
/// also how a re-run recognises it: a source whose content changed renders
/// differently and is copied again, never lost.
fn section(id: &str, item: &Value) -> String {
    let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut meta = vec![
        format!("status {}", item["status"].as_str().unwrap_or("inbox")),
        format!("priority {}", item["priority"].as_i64().unwrap_or(2)),
        one_line(item["kind"].as_str().unwrap_or("feature")),
    ];
    if let Some(severity) = item["severity"].as_str() {
        meta.push(format!("severity {severity}"));
    }
    if let Some(owner) = item["owner"].as_str() {
        meta.push(format!("owner {}", one_line(owner)));
    }
    if let Some(due) = item["due_at"].as_i64() {
        let date = timestamp(due.saturating_mul(1000));
        meta.push(format!("due {}", date.get(..10).unwrap_or(&date)));
    }
    let description = item["description"].as_str().unwrap_or_default().trim();
    format!(
        "{SECTION}{id}: {}\n_{}_\n\n{}",
        one_line(item["title"].as_str().unwrap_or_default()),
        meta.join(" · "),
        if description.is_empty() {
            "_No description._"
        } else {
            description
        }
    )
}

/// `logs` without its last block, when that block opens with `marker`.
fn without_last_block<'a>(logs: &'a str, marker: &str) -> &'a str {
    let logs = logs.trim_end();
    match (last_block(logs, marker), logs.rfind(marker)) {
        (Some(_), Some(start)) => logs[..start].trim_end(),
        _ => logs,
    }
}

const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];

fn severity_rank(value: &Value) -> Option<usize> {
    value
        .as_str()
        .and_then(|s| SEVERITIES.iter().position(|known| *known == s))
}

/// What one round writes into the target.
struct Plan {
    /// Fields to set on top of the target as read; empty when nothing changes.
    fields: Map<String, Value>,
    /// Sources whose section this write appends.
    appended: Vec<String>,
}

fn strings(item: &Value, key: &str) -> Vec<String> {
    item[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// The target with every pending source folded in, checked against the
/// store's bounds before anything is written.
fn plan(
    target_id: &str,
    target: &Value,
    pending: &[(&str, &Value)],
    reason: &str,
    at_ms: i64,
) -> Result<Plan, WorkbenchError> {
    let mut fields = Map::new();
    let mut description = target["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let mut appended = Vec::new();
    let mut listed = Vec::new();
    for (id, item) in pending {
        let text = section(id, item);
        if description.contains(&text) {
            continue;
        }
        if !description.trim().is_empty() {
            description = format!("{}\n\n{text}", description.trim_end());
        } else {
            description = text;
        }
        appended.push((*id).to_owned());
        listed.push(format!(
            "- {id} (revision {}): {}",
            item["revision"],
            item["title"].as_str().unwrap_or_default()
        ));
    }
    if !appended.is_empty() {
        if description.len() > file_tasks::MAX_TASK_DESCRIPTION {
            return Err(WorkbenchError::new(
                "merge_too_large",
                format!("Merged, the description of {target_id} would be {} bytes; a task holds at most {}. Merge fewer tasks into it at once, or trim the descriptions first.", description.len(), file_tasks::MAX_TASK_DESCRIPTION),
            ));
        }
        if locked_fields(target).contains(&"description") {
            return Err(WorkbenchError::new(
                "field_locked",
                format!("The person locked the description of {target_id}, and merging appends to it. Merge into another of these tasks, or ask them to unlock it."),
            ));
        }
        fields.insert("description".into(), json!(description));
        let block = format!(
            "--- Agent merged {} task{} into this one ({}) ---\n{reason}\n{}",
            appended.len(),
            if appended.len() == 1 { "" } else { "s" },
            timestamp(at_ms),
            listed.join("\n")
        );
        let logs =
            append_log(target["logs"].as_str().unwrap_or_default(), &block).ok_or_else(|| {
                WorkbenchError::new(
                    "invalid_input",
                    format!(
                        "The logs of {target_id} are full, so the merge cannot be recorded there."
                    ),
                )
            })?;
        fields.insert("logs".into(), json!(logs));
    }

    // Unions keep the target's order first; a re-run adds nothing.
    let union = |key: &str, fold: fn(&str) -> String| {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for value in std::iter::once(target)
            .chain(pending.iter().map(|(_, item)| *item))
            .flat_map(|item| strings(item, key))
        {
            let trimmed = value.trim().to_owned();
            if !trimmed.is_empty() && seen.insert(fold(&trimmed)) {
                out.push(trimmed);
            }
        }
        out
    };
    let criteria = union("acceptance_criteria", str::to_owned);
    let labels = union("labels", str::to_lowercase);
    if criteria.len() > file_tasks::MAX_TASK_CRITERIA || labels.len() > file_tasks::MAX_TASK_LABELS
    {
        return Err(WorkbenchError::new(
            "merge_too_large",
            format!("Merged, {target_id} would have {} acceptance criteria and {} labels; a task holds at most {} and {}. Merge fewer tasks at once.", criteria.len(), labels.len(), file_tasks::MAX_TASK_CRITERIA, file_tasks::MAX_TASK_LABELS),
        ));
    }
    if criteria != strings(target, "acceptance_criteria") {
        fields.insert("acceptance_criteria".into(), json!(criteria));
    }
    if labels != strings(target, "labels") {
        fields.insert("labels".into(), json!(labels));
    }

    // The merged task is as urgent as the most urgent work in it.
    let all = || std::iter::once(target).chain(pending.iter().map(|(_, item)| *item));
    if let Some(priority) = all().filter_map(|item| item["priority"].as_i64()).min() {
        if target["priority"].as_i64() != Some(priority) {
            fields.insert("priority".into(), json!(priority));
        }
    }
    if let Some(rank) = all()
        .filter_map(|item| severity_rank(&item["severity"]))
        .max()
    {
        if severity_rank(&target["severity"]) != Some(rank) {
            fields.insert("severity".into(), json!(SEVERITIES[rank]));
        }
    }
    if let Some(due) = all().filter_map(|item| item["due_at"].as_i64()).min() {
        if target["due_at"].as_i64() != Some(due) {
            fields.insert("due_at".into(), json!(due));
        }
    }
    Ok(Plan { fields, appended })
}

/// Everything that refuses a merge, checked against the board as read now.
fn check(
    store: &Store,
    repository_id: &str,
    target_id: &str,
    target: &Value,
    pending: &[(&str, &Value)],
    reason: &str,
    even_if_running: bool,
) -> Result<(), WorkbenchError> {
    if target["status"] == "done" {
        if let Some((id, _)) = pending.iter().find(|(_, item)| item["status"] != "done") {
            return Err(WorkbenchError::new(
                "target_done",
                format!("{target_id} is done, and {id} is still open: merged, its work would disappear into the archive. Merge into an open task instead."),
            ));
        }
    }
    let marker = format!("{MERGE_MARKER}{target_id} (");
    for (id, item) in pending {
        refuse_shared(item, repository_id)?;
        refuse_in_use(store, id, item, even_if_running)?;
        let logs = item["logs"].as_str().unwrap_or_default();
        let block = format!("{marker}{}) ---\n{reason}", timestamp(now_millis()));
        if !ends_with_block(logs, &marker, reason) && append_log(logs, &block).is_none() {
            return Err(WorkbenchError::new(
                "invalid_input",
                format!("The logs of {id} are full, so the merge cannot be recorded on it. Shorten the reason."),
            ));
        }
    }
    Ok(())
}

/// How far a merge got: what the target is now, each source's state, and
/// whether anything was written yet.
struct Progress {
    target: Value,
    resolved: Vec<Source>,
    wrote: bool,
    sequence: Option<u64>,
    appended: Vec<String>,
    /// Why the merge stopped early, when it did.
    stopped: Option<String>,
}

/// The read-copy-delete rounds. Each round reads the target and every source
/// still pending, checks them, writes the target once, then deletes each
/// source at the revision just copied; a source that changed in between stays
/// pending for the next round. An error returns as it is: the caller decides
/// whether it is a refusal (nothing written) or a stop part-way.
fn rounds(
    store: &Store,
    repository_id: &str,
    target_id: &str,
    reason: &str,
    even_if_running: bool,
    progress: &mut Progress,
    step: &mut dyn FnMut(Step<'_>, &Store),
) -> Result<(), WorkbenchError> {
    let mut checked = false;
    for round in 0..MAX_ROUNDS {
        if round > 0 {
            match get_item(store, target_id)? {
                Some(item) => progress.target = item,
                None => {
                    progress.stopped = Some(format!("{target_id} was deleted during the merge."));
                    return Ok(());
                }
            }
            for source in &mut progress.resolved {
                if !matches!(source.state, State::Pending(_)) {
                    continue;
                }
                source.state = match get_item(store, &source.id)? {
                    Some(item) => State::Pending(item),
                    None => {
                        let body = last_revision(store, &source.id)?.unwrap_or(Value::Null);
                        match why_gone(&body) {
                            Gone::Merged { into, .. } if into == target_id => {
                                State::AlreadyMerged {
                                    title: body["title"].clone(),
                                }
                            }
                            _ => State::NotMerged {
                                title: body["title"].clone(),
                                detail: "It was deleted by someone else during the merge.".into(),
                            },
                        }
                    }
                };
            }
        }
        let plan = {
            let pending: Vec<(&str, &Value)> = progress
                .resolved
                .iter()
                .filter_map(|s| match &s.state {
                    State::Pending(item) => Some((s.id.as_str(), item)),
                    _ => None,
                })
                .collect();
            if pending.is_empty() {
                return Ok(());
            }
            let target = &progress.target;
            check(
                store,
                repository_id,
                target_id,
                target,
                &pending,
                reason,
                even_if_running,
            )?;
            plan(target_id, target, &pending, reason, now_millis())?
        };
        if !checked {
            checked = true;
            step(Step::Checked, store);
        }
        if !plan.fields.is_empty() {
            let revision = progress.target["revision"].as_i64().unwrap_or_default();
            let mut input = resend(&progress.target, target_id, revision, "merge");
            input.extend(plan.fields);
            match query(store, "items.put", &Value::Object(input).to_string()) {
                Ok(saved) => {
                    progress.wrote = true;
                    progress.sequence = saved["sequence"].as_u64().or(progress.sequence);
                    progress.target = saved["item"].clone();
                    progress.appended.extend(plan.appended);
                }
                // Someone saved the target between our read and our write:
                // read it again and fold in on top of their edit.
                Err(error) if error.code == "revision_conflict" => continue,
                Err(error) => return Err(error),
            }
        }
        step(Step::TargetWritten, store);

        let marker = format!("{MERGE_MARKER}{target_id} (");
        for source in &mut progress.resolved {
            let State::Pending(item) = &source.state else {
                continue;
            };
            step(Step::BeforeDelete(&source.id), store);
            // The copy is only as safe as the card holding it.
            if get_item(store, target_id)?.is_none() {
                progress.stopped = Some(format!(
                    "{target_id} was deleted during the merge, so nothing more was deleted."
                ));
                return Ok(());
            }
            let logs = item["logs"].as_str().unwrap_or_default();
            let recorded = ends_with_block(logs, &marker, reason);
            let block = format!("{marker}{}) ---\n{reason}", timestamp(now_millis()));
            match record_and_delete(store, &source.id, item, &block, recorded, "merge")? {
                Removal::Deleted(receipt) => {
                    progress.wrote = true;
                    progress.sequence = receipt["sequence"].as_u64().or(progress.sequence);
                    // Its own logs: what it had before this merge's block.
                    let own = if recorded {
                        without_last_block(logs, &marker)
                    } else {
                        logs
                    };
                    source.state = State::Merged {
                        title: item["title"].clone(),
                        copied_revision: item["revision"].as_i64().unwrap_or_default(),
                        had_logs: !own.trim().is_empty(),
                    };
                }
                // It changed after it was copied: the next round copies what
                // it is now, then deletes that.
                Removal::Changed { recorded } => progress.wrote |= recorded,
            }
        }
    }
    Ok(())
}

/// Merge `sources` into `into`. See the module documentation for the order of
/// writes and what each card ends up holding.
pub(crate) fn merge_tasks(
    store: &Store,
    repo_path: &str,
    into: &MergeTask,
    sources: &[MergeTask],
    reason: &str,
    even_if_running: bool,
) -> Result<Value, WorkbenchError> {
    merge_with(
        store,
        repo_path,
        into,
        sources,
        reason,
        even_if_running,
        &mut |_, _| {},
    )
}

/// [`merge_tasks`], calling `step` between its writes.
pub(crate) fn merge_with(
    store: &Store,
    repo_path: &str,
    into: &MergeTask,
    sources: &[MergeTask],
    reason: &str,
    even_if_running: bool,
    step: &mut dyn FnMut(Step<'_>, &Store),
) -> Result<Value, WorkbenchError> {
    let reason = check_reason(reason, "these tasks are being merged")?;
    if sources.is_empty() || sources.len() > MAX_MERGE_SOURCES {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("Name between 1 and {MAX_MERGE_SOURCES} tasks to merge."),
        ));
    }
    let repository = board_repository(store, repo_path)?;
    let repository_id = repository["id"].as_str().unwrap_or_default().to_owned();
    let (target_id, original) = find_live_task(store, &repository_id, &into.task)?;
    let target_revision = original["revision"].as_i64().unwrap_or_default();
    if into
        .expected_revision
        .is_some_and(|expected| expected != target_revision)
    {
        return Err(WorkbenchError::new(
            "revision_conflict",
            format!("{target_id} is at revision {target_revision}, not {}; someone changed it. Re-read it before merging into it.", into.expected_revision.unwrap_or_default()),
        ));
    }

    let mut resolved: Vec<Source> = Vec::new();
    for source in sources {
        let requested = source.task.trim().to_owned();
        let (id, state) = match find_task(store, &repository_id, &requested) {
            Ok((id, item)) => {
                let revision = item["revision"].as_i64().unwrap_or_default();
                if source
                    .expected_revision
                    .is_some_and(|expected| expected != revision)
                {
                    return Err(WorkbenchError::new(
                        "revision_conflict",
                        format!("{id} is at revision {revision}, not {}; someone changed it. Re-read it before merging it.", source.expected_revision.unwrap_or_default()),
                    ));
                }
                (id, State::Pending(item))
            }
            Err(error) if error.code == "not_found" => {
                match deleted_task(store, &repository_id, &requested)? {
                    // An earlier call merged it here: nothing left to do for it.
                    Some((id, body)) if matches!(why_gone(&body), Gone::Merged { ref into, .. } if *into == target_id) => {
                        (
                            id,
                            State::AlreadyMerged {
                                title: body["title"].clone(),
                            },
                        )
                    }
                    // Gone some other way: say where, rather than merge nothing.
                    Some(_) => {
                        return Err(find_live_task(store, &repository_id, &requested)
                            .err()
                            .unwrap_or(error))
                    }
                    None => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        if id == target_id {
            return Err(WorkbenchError::new(
                "invalid_input",
                format!("{requested:?} is the task being merged into; a task cannot be merged into itself."),
            ));
        }
        if let Some(twin) = resolved.iter().find(|s| s.id == id) {
            return Err(WorkbenchError::new(
                "invalid_input",
                format!(
                    "{requested:?} and {:?} name the same task, {id}; list each task once.",
                    twin.requested
                ),
            ));
        }
        resolved.push(Source {
            requested,
            id,
            state,
        });
    }

    let mut progress = Progress {
        target: original.clone(),
        resolved,
        wrote: false,
        sequence: None,
        appended: Vec::new(),
        stopped: None,
    };
    let ran = rounds(
        store,
        &repository_id,
        &target_id,
        reason,
        even_if_running,
        &mut progress,
        step,
    );
    // Before the first write a failure is a plain refusal: nothing changed.
    // After it, the board is part-way, and the report below says how far.
    if let Err(error) = ran {
        if !progress.wrote {
            return Err(error);
        }
        progress.stopped = Some(format!("{}: {}", error.code, error.message));
    }
    let Progress {
        target,
        resolved,
        wrote,
        sequence,
        appended,
        stopped,
    } = progress;

    let mut logs_kept = Vec::new();
    let rows: Vec<Value> = resolved
        .into_iter()
        .map(|source| {
            let (outcome, title, copied, detail) = match source.state {
                State::Merged { title, copied_revision, had_logs } => {
                    if had_logs {
                        logs_kept.push(source.id.clone());
                    }
                    ("merged", title, Some(copied_revision), None)
                }
                State::AlreadyMerged { title } => ("already_merged", title, None, None),
                State::NotMerged { title, detail } => ("not_merged", title, None, Some(detail)),
                State::Pending(item) => (
                    "not_merged",
                    item["title"].clone(),
                    None,
                    Some(stopped.clone().unwrap_or_else(|| "The board kept changing while it was being merged; it is still on the board.".into())),
                ),
            };
            json!({
                "task_id": source.requested, "item_id": source.id, "title": title,
                "outcome": outcome, "copied_revision": copied, "detail": detail,
            })
        })
        .collect();
    let ok = rows.iter().all(|row| row["outcome"] != "not_merged");
    let target_now = get_item(store, &target_id)?;
    let shown = target_now.as_ref().unwrap_or(&target);
    let changed = |key: &str| {
        (original[key] != shown[key])
            .then(|| json!({"field": key, "from": original[key], "to": shown[key]}))
    };
    let escalated: Vec<Value> = ["priority", "severity", "due_at"]
        .into_iter()
        .filter_map(changed)
        .collect();
    let added = |key: &str| {
        strings(shown, key)
            .len()
            .saturating_sub(strings(&original, key).len())
    };
    Ok(json!({
        "ok": ok,
        "outcome": if !ok { "partial" } else if wrote { "merged" } else { "unchanged" },
        "item_id": target_id,
        "title": shown["title"],
        "status": shown["status"],
        "priority": shown["priority"],
        "severity": shown["severity"],
        "due_at": shown["due_at"],
        "revision": shown["revision"],
        "target_live": target_now.is_some(),
        "repository": repository_summary(&repository),
        "sources": rows,
        "added": {
            "sections": appended.len(),
            "acceptance_criteria": added("acceptance_criteria"),
            "labels": added("labels"),
        },
        "escalated": escalated,
        // Merged sources that had logs: those stay in their own revision
        // history and are not copied into the target.
        "logs_kept_in_history": logs_kept,
        "next_step": if ok { Value::Null } else {
            json!(format!("Not every task was merged{}. Re-run the same call to finish: what is already merged is skipped, and nothing is copied twice.", stopped.as_deref().map(|s| format!(" ({s})")).unwrap_or_default()))
        },
        "sequence": sequence,
    }))
}
