//! `gitpulse_merge_tasks`: what a merge leaves on the board, every refusal
//! writing nothing, and the invariant under interruption and contention —
//! every source's work stays on a live card.

use super::merge::{merge_with, Step};
use super::tests::{
    filed_with, git_repo, items, live, person_edits, prepare_run, profile, registered, task,
};
use super::*;

fn named(task_id: &str) -> MergeTask {
    MergeTask {
        task: task_id.into(),
        expected_revision: None,
    }
}

fn card(key: &str, title: &str, description: &str) -> ExternalTask {
    let mut card = task(key, title);
    card.description = description.into();
    card
}

fn merge(
    store: &Store,
    repo: &tempfile::TempDir,
    into: &str,
    sources: &[&str],
    reason: &str,
) -> Result<Value, WorkbenchError> {
    let sources: Vec<MergeTask> = sources.iter().map(|s| named(s)).collect();
    merge_tasks(
        store,
        repo.path().to_str().unwrap(),
        &named(into),
        &sources,
        reason,
        false,
    )
}

/// Every task the repository has ever had, deleted or not, at its last
/// revision: what "nothing was written" is checked against.
fn board(store: &Store, repository: &Value) -> Vec<(String, i64, bool)> {
    let mut all: Vec<(String, i64, bool)> = items(store, repository)
        .iter()
        .map(|item| {
            (
                item["id"].as_str().unwrap().to_owned(),
                item["revision"].as_i64().unwrap(),
                false,
            )
        })
        .collect();
    all.sort();
    all
}

#[test]
fn a_merge_folds_each_source_into_the_target_and_deletes_it() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let mut target = card(
        "gp-export",
        "Exporter writes CSV",
        "Write the CSV exporter.",
    );
    target.acceptance_criteria = vec!["Exports a header row".into()];
    target.labels = vec!["export".into()];
    target.status = Some("in_progress".into());
    let target = filed_with(&store, &repo, target);
    let mut first = card(
        "gp-export-quotes",
        "Exporter quotes commas",
        "Quote fields that hold commas.",
    );
    first.acceptance_criteria = vec!["Exports a header row".into(), "Quotes commas".into()];
    first.labels = vec!["Export".into(), "csv".into()];
    first.priority = Some(0);
    first.severity = Some("high".into());
    first.due = Some("2026-11-01".into());
    let first = filed_with(&store, &repo, first);
    let mut second = card("gp-export-bom", "Exporter writes a BOM", "");
    second.acceptance_criteria = vec!["Excel opens it".into()];
    second.logs = Some("trace from the failing run".into());
    let second = filed_with(&store, &repo, second);

    let merged = merge(
        &store,
        &repo,
        &target,
        &[&first, "gp-export-bom"],
        "Three cards for one exporter.",
    )
    .unwrap();
    assert_eq!(merged["ok"], true, "{merged:#}");
    assert_eq!(merged["outcome"], "merged");
    assert_eq!(merged["added"]["sections"], 2);
    assert_eq!(merged["added"]["acceptance_criteria"], 2);
    assert_eq!(merged["added"]["labels"], 1);
    assert_eq!(merged["logs_kept_in_history"], json!([second]));
    let fields: Vec<&str> = merged["escalated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["priority", "severity", "due_at"]);

    let kept = live(&store, &target).unwrap();
    let description = kept["description"].as_str().unwrap();
    assert!(
        description.starts_with("Write the CSV exporter."),
        "{description}"
    );
    assert!(description.contains(&format!("## Merged from {first}: Exporter quotes commas")));
    assert!(description.contains("Quote fields that hold commas."));
    assert!(description.contains(&format!("## Merged from {second}: Exporter writes a BOM")));
    assert!(description.contains("_No description._"));
    assert_eq!(
        kept["acceptance_criteria"],
        json!(["Exports a header row", "Quotes commas", "Excel opens it"])
    );
    assert_eq!(
        kept["labels"],
        json!(["export", "csv"]),
        "labels fold case-insensitively, the target's spelling first"
    );
    assert_eq!(
        (kept["priority"].clone(), kept["severity"].clone()),
        (json!(0), json!("high"))
    );
    assert_eq!(
        kept["due_at"],
        json!(parse_due(Some("2026-11-01")).unwrap().unwrap())
    );
    // What stays the target's own.
    assert_eq!(
        (kept["title"].clone(), kept["status"].clone()),
        (json!("Exporter writes CSV"), json!("in_progress"))
    );
    let logs = kept["logs"].as_str().unwrap();
    assert!(
        logs.contains("--- Agent merged 2 tasks into this one ("),
        "{logs}"
    );
    assert!(
        logs.contains("Three cards for one exporter.")
            && logs.contains(&first)
            && logs.contains(&second)
    );

    for source in [&first, &second] {
        assert!(live(&store, source).is_none(), "{source} is deleted");
        let last = last_revision(&store, source).unwrap().unwrap();
        assert_eq!(last["deleted"], true);
        assert_eq!(
            why_gone(&last),
            Gone::Merged {
                into: target.clone(),
                reason: "Three cards for one exporter.".into()
            }
        );
        // An agent launched on a merged task is pointed at where it went.
        let error = complete_task(&store, repo_path, source, "done", None, None).unwrap_err();
        assert_eq!(error.code, "task_merged");
        assert!(error.message.contains(&target), "{}", error.message);
    }
    // Its own logs are still in its history, not lost.
    assert!(last_revision(&store, &second).unwrap().unwrap()["logs"]
        .as_str()
        .unwrap()
        .starts_with("trace from the failing run"));
}

#[test]
fn the_same_merge_again_is_unchanged_and_writes_nothing() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-a", "Alpha", "A"));
    let source = filed_with(&store, &repo, card("gp-b", "Alpha twin", "B"));
    merge(&store, &repo, &target, &[&source], "Duplicate.").unwrap();
    let revision = live(&store, &target).unwrap()["revision"].clone();

    for sources in [vec![source.as_str()], vec!["gp-b"]] {
        let again = merge(&store, &repo, &target, &sources, "Duplicate.").unwrap();
        assert_eq!(
            (again["ok"].clone(), again["outcome"].clone()),
            (json!(true), json!("unchanged")),
            "{again:#}"
        );
        assert_eq!(again["sources"][0]["outcome"], "already_merged");
        assert_eq!(live(&store, &target).unwrap()["revision"], revision);
    }
}

#[test]
fn every_refusal_writes_nothing() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let repository = registered(&store, &repo);
    let target = filed_with(&store, &repo, card("gp-t", "Target", "T"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "S"));
    let mut done = card("gp-done", "Done target", "D");
    done.status = Some("done".into());
    let done = filed_with(&store, &repo, done);
    let busy = filed_with(&store, &repo, card("gp-busy", "Busy source", "B"));
    prepare_run(&store, &repo, &busy, "run-busy");
    let locked = filed_with(&store, &repo, card("gp-locked", "Locked target", "L"));
    person_edits(&store, &locked, |input| {
        input.insert("locked_fields".into(), json!(["description"]));
    });
    let huge = filed_with(
        &store,
        &repo,
        card(
            "gp-huge",
            "Huge",
            &"x".repeat(file_tasks::MAX_TASK_DESCRIPTION - 100),
        ),
    );
    let elsewhere = filed_with(&store, &repo, card("gp-elsewhere", "Merged elsewhere", "E"));
    let other_target = filed_with(&store, &repo, card("gp-other", "Other target", "O"));
    merge(
        &store,
        &repo,
        &other_target,
        &[&elsewhere],
        "Belongs there.",
    )
    .unwrap();
    let before = board(&store, &repository);

    let pinned = |revision| MergeTask {
        task: source.clone(),
        expected_revision: Some(revision),
    };
    let too_many: Vec<MergeTask> = (0..=MAX_MERGE_SOURCES)
        .map(|i| named(&format!("gp-n{i}")))
        .collect();
    type Call<'a> = Box<dyn Fn() -> Result<Value, WorkbenchError> + 'a>;
    let cases: Vec<(&str, Call)> = vec![
        (
            "invalid_input",
            Box::new(|| merge(&store, &repo, &target, &[&source], "   ")),
        ),
        (
            "invalid_input",
            Box::new(|| {
                merge(
                    &store,
                    &repo,
                    &target,
                    &[&source],
                    &"r".repeat(MAX_REASON_CHARS + 1),
                )
            }),
        ),
        (
            "invalid_input",
            Box::new(|| merge(&store, &repo, &target, &[], "No sources.")),
        ),
        (
            "invalid_input",
            Box::new(|| {
                merge_tasks(
                    &store,
                    repo_path,
                    &named(&target),
                    &too_many,
                    "Too many.",
                    false,
                )
            }),
        ),
        (
            "invalid_input",
            Box::new(|| merge(&store, &repo, &target, &[&target], "Itself.")),
        ),
        (
            "invalid_input",
            Box::new(|| {
                merge(
                    &store,
                    &repo,
                    &target,
                    &[&source, "gp-s"],
                    "Twice, by key and by id.",
                )
            }),
        ),
        (
            "not_found",
            Box::new(|| {
                merge(
                    &store,
                    &repo,
                    &target,
                    &[&source, "gp-nope"],
                    "One is missing.",
                )
            }),
        ),
        (
            "not_found",
            Box::new(|| merge(&store, &repo, "gp-nope", &[&source], "Missing target.")),
        ),
        (
            "target_done",
            Box::new(|| merge(&store, &repo, &done, &[&source], "Into the archive.")),
        ),
        (
            "task_in_use",
            Box::new(|| merge(&store, &repo, &target, &[&source, &busy], "Busy.")),
        ),
        (
            "field_locked",
            Box::new(|| merge(&store, &repo, &locked, &[&source], "Locked.")),
        ),
        (
            "merge_too_large",
            Box::new(|| merge(&store, &repo, &huge, &[&source, &target], "Too big.")),
        ),
        (
            "task_merged",
            Box::new(|| {
                merge(
                    &store,
                    &repo,
                    &target,
                    &[&source, &elsewhere],
                    "Already gone.",
                )
            }),
        ),
        (
            "task_merged",
            Box::new(|| merge(&store, &repo, &elsewhere, &[&source], "Into a merged card.")),
        ),
        (
            "revision_conflict",
            Box::new(|| {
                merge_tasks(
                    &store,
                    repo_path,
                    &named(&target),
                    &[pinned(99)],
                    "Stale read.",
                    false,
                )
            }),
        ),
        (
            "revision_conflict",
            Box::new(|| {
                merge_tasks(
                    &store,
                    repo_path,
                    &MergeTask {
                        task: target.clone(),
                        expected_revision: Some(99),
                    },
                    &[named(&source)],
                    "Stale read.",
                    false,
                )
            }),
        ),
    ];
    for (index, (code, call)) in cases.iter().enumerate() {
        let error = call().expect_err(&format!("case {index} ({code}) was not refused"));
        assert_eq!(error.code, *code, "case {index}: {}", error.message);
        assert_eq!(
            board(&store, &repository),
            before,
            "case {index} ({code}) wrote something"
        );
    }
    // The target that was refused because a source is busy is merged once
    // the caller says the running agent is itself.
    let forced = merge_tasks(
        &store,
        repo_path,
        &named(&target),
        &[named(&busy)],
        "I am that agent.",
        true,
    )
    .unwrap();
    assert_eq!(forced["outcome"], "merged");
}

#[test]
fn a_task_linked_to_another_repository_is_never_merged_away() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let (repo, other) = (git_repo(), git_repo());
    let (mine, theirs) = (registered(&store, &repo), registered(&store, &other));
    let target = filed_with(&store, &repo, card("gp-t", "Target", "T"));
    let shared = filed_with(&store, &repo, card("gp-shared", "Shared", "S"));
    person_edits(&store, &shared, |input| {
        input.insert("repository_ids".into(), json!([mine["id"], theirs["id"]]));
    });
    let error = merge(&store, &repo, &target, &[&shared], "Shared.").unwrap_err();
    assert_eq!(error.code, "shared_task", "{}", error.message);
    assert!(live(&store, &shared).is_some());
    // And a task of the other repository is not this one's to name.
    let theirs_only = filed_with(&store, &other, card("gp-theirs", "Theirs", "X"));
    assert_eq!(
        merge(&store, &repo, &target, &[&theirs_only], "Not mine.")
            .unwrap_err()
            .code,
        "not_found"
    );
}

/// Folding work into a card by its board id, after that card was merged
/// away, used to file a brand-new card under the dead id as its key — a
/// duplicate of exactly what the merge removed. It is now refused, naming
/// where the card went.
#[test]
fn folding_into_a_merged_card_points_at_its_target_instead_of_filing_a_twin() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap();
    let repository = registered(&store, &repo);
    let target = filed_with(&store, &repo, card("gp-t", "Exporter", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Exporter twin", "Twin body"));
    merge(&store, &repo, &target, &[&source], "Twins.").unwrap();
    let before = items(&store, &repository).len();

    for replace in [true, false] {
        let mut fold = card(&source, "Exporter twin, extended", "More work");
        fold.labels = vec!["export".into()];
        let error = add_task(
            &store,
            repo_path,
            fold,
            replace,
            std::slice::from_ref(&target),
        )
        .unwrap_err();
        assert_eq!(
            error.code, "task_merged",
            "replace={replace}: {}",
            error.message
        );
        assert!(error.message.contains(&target), "{}", error.message);
        assert_eq!(
            items(&store, &repository).len(),
            before,
            "replace={replace}: no new card"
        );
    }
}

/// The bounds hold at their edge: the most sources one call takes merge in
/// one write, and a done target takes done sources (housekeeping the archive),
/// just not open ones.
#[test]
fn the_largest_merge_one_call_takes_goes_through() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let sources: Vec<String> = (0..MAX_MERGE_SOURCES)
        .map(|k| {
            let mut source = card(
                &format!("gp-s{k}"),
                &format!("Source {k}"),
                &format!("token-{k}-"),
            );
            source.acceptance_criteria = vec![format!("criterion {k}"), "shared criterion".into()];
            filed_with(&store, &repo, source)
        })
        .collect();
    let named_sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let merged = merge(&store, &repo, &target, &named_sources, "All one concern.").unwrap();
    assert_eq!(merged["ok"], true, "{merged:#}");
    assert_eq!(merged["added"]["sections"], MAX_MERGE_SOURCES);
    let kept = live(&store, &target).unwrap();
    let description = kept["description"].as_str().unwrap();
    for k in 0..MAX_MERGE_SOURCES {
        assert_eq!(
            description.matches(&format!("token-{k}-")).count(),
            1,
            "{k}"
        );
    }
    assert_eq!(
        kept["acceptance_criteria"].as_array().unwrap().len(),
        MAX_MERGE_SOURCES + 1
    );
    // Created at revision 1; one write merged all of them.
    assert_eq!(kept["revision"], 2, "{kept}");
    assert!(sources.iter().all(|id| live(&store, id).is_none()));

    let mut done = card("gp-done", "Done target", "D");
    done.status = Some("done".into());
    let done = filed_with(&store, &repo, done);
    let mut also_done = card("gp-done-2", "Done duplicate", "D2");
    also_done.status = Some("done".into());
    let also_done = filed_with(&store, &repo, also_done);
    assert_eq!(
        merge(&store, &repo, &done, &[&also_done], "Same finished work.").unwrap()["outcome"],
        "merged"
    );
}

/// A crash at any step leaves every source's work on a live card, and the
/// same call run again finishes the merge without copying anything twice.
#[test]
fn an_interrupted_merge_is_finished_by_running_it_again() {
    for stop_at in ["checked", "target_written", "before_second_delete"] {
        let (_dir, path) = profile();
        let store = Store::open(&path).unwrap();
        let repo = git_repo();
        let repo_path = repo.path().to_str().unwrap();
        let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
        let first = filed_with(&store, &repo, card("gp-1", "First", "token-first"));
        let second = filed_with(&store, &repo, card("gp-2", "Second", "token-second"));
        let sources = [named(&first), named(&second)];
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            merge_with(
                &store,
                repo_path,
                &named(&target),
                &sources,
                "Crash test.",
                false,
                &mut |step, _| {
                    let here = match step {
                        Step::Checked => "checked",
                        Step::TargetWritten => "target_written",
                        Step::BeforeDelete(id) if id == second => "before_second_delete",
                        Step::BeforeDelete(_) => "",
                    };
                    if here == stop_at {
                        panic!("the host went away at {here}");
                    }
                },
            )
        }));
        assert!(crashed.is_err(), "{stop_at}: the hook did not fire");
        for token in ["token-first", "token-second"] {
            let on_live_card = [&target, &first, &second]
                .iter()
                .filter_map(|id| live(&store, id))
                .any(|item| item["description"].as_str().unwrap().contains(token));
            assert!(on_live_card, "{stop_at}: {token} is on no live card");
        }

        let finished = merge_tasks(
            &store,
            repo_path,
            &named(&target),
            &sources,
            "Crash test.",
            false,
        )
        .unwrap();
        assert_eq!(finished["ok"], true, "{stop_at}: {finished:#}");
        assert!(
            live(&store, &first).is_none() && live(&store, &second).is_none(),
            "{stop_at}"
        );
        let description = live(&store, &target).unwrap()["description"]
            .as_str()
            .unwrap()
            .to_owned();
        for token in ["token-first", "token-second"] {
            assert_eq!(
                description.matches(token).count(),
                1,
                "{stop_at}: {token} copied once: {description}"
            );
        }
    }
}

/// A person saving the target between the merge's read and its write keeps
/// their edit: the merge folds in on top of it.
#[test]
fn a_person_editing_the_target_mid_merge_keeps_their_edit() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-source"));
    let result = merge_with(
        &store,
        repo.path().to_str().unwrap(),
        &named(&target),
        &[named(&source)],
        "Edit race.",
        false,
        &mut |step, store| {
            if step == Step::Checked {
                person_edits(store, &target, |input| {
                    input.insert(
                        "description".into(),
                        json!("Target body, edited by the person"),
                    );
                });
            }
        },
    )
    .unwrap();
    assert_eq!(result["outcome"], "merged", "{result:#}");
    let description = live(&store, &target).unwrap()["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        description.starts_with("Target body, edited by the person"),
        "{description}"
    );
    assert_eq!(description.matches("token-source").count(), 1);
}

/// A source saved after it was copied is copied again before it is deleted,
/// so the edit is never lost with it.
#[test]
fn a_source_edited_after_it_was_copied_is_copied_again_not_lost() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-v1"));
    let mut edited = false;
    let result = merge_with(
        &store,
        repo.path().to_str().unwrap(),
        &named(&target),
        &[named(&source)],
        "Source race.",
        false,
        &mut |step, store| {
            if matches!(step, Step::BeforeDelete(_)) && !edited {
                edited = true;
                person_edits(store, &source, |input| {
                    input.insert("description".into(), json!("token-v1 then token-v2"));
                });
            }
        },
    )
    .unwrap();
    assert_eq!(result["outcome"], "merged", "{result:#}");
    assert_eq!(
        result["sources"][0]["copied_revision"], 2,
        "deleted at the edited revision"
    );
    let description = live(&store, &target).unwrap()["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        description.contains("token-v1 then token-v2"),
        "{description}"
    );
    assert!(live(&store, &source).is_none());
}

/// The target deleted on the board mid-merge: nothing more is deleted, and
/// the call says it stopped.
#[test]
fn a_target_deleted_mid_merge_stops_before_any_source_is_deleted() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-source"));
    let result = merge_with(&store, repo.path().to_str().unwrap(), &named(&target), &[named(&source)], "Target race.", false, &mut |step, store| {
        if step == Step::TargetWritten {
            let item = live(store, &target).unwrap();
            query(store, "items.delete", &json!({"id": target, "request_id": "person-delete", "expected_revision": item["revision"]}).to_string()).unwrap();
        }
    })
    .unwrap();
    assert_eq!(
        (result["ok"].clone(), result["outcome"].clone()),
        (json!(false), json!("partial")),
        "{result:#}"
    );
    assert_eq!(result["target_live"], false);
    assert_eq!(result["sources"][0]["outcome"], "not_merged");
    assert!(
        result["next_step"].as_str().unwrap().contains("deleted"),
        "{result:#}"
    );
    assert!(live(&store, &source).is_some(), "the source keeps its work");
}

/// A failure after the first write is not a bare error: the target already
/// holds the copy, so the call reports how far it got, and the source that
/// could not be finished is still on the board with its work.
#[test]
fn a_failure_after_the_first_write_is_reported_as_partial() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-source"));
    let result = merge_with(
        &store,
        repo.path().to_str().unwrap(),
        &named(&target),
        &[named(&source)],
        "Fill race.",
        false,
        &mut |step, store| {
            if step == Step::TargetWritten {
                // Its logs fill up after it was copied, so the merge block no
                // longer fits on it.
                person_edits(store, &source, |input| {
                    input.insert("logs".into(), json!("y".repeat(file_tasks::MAX_TASK_LOGS)));
                });
            }
        },
    )
    .unwrap();
    assert_eq!(
        (result["ok"].clone(), result["outcome"].clone()),
        (json!(false), json!("partial")),
        "{result:#}"
    );
    assert_eq!(result["sources"][0]["outcome"], "not_merged");
    let next = result["next_step"].as_str().unwrap();
    assert!(
        next.contains("invalid_input") && next.contains("full"),
        "{next}"
    );
    assert!(live(&store, &source).is_some(), "the source keeps its work");
    assert!(live(&store, &target).unwrap()["description"]
        .as_str()
        .unwrap()
        .contains("token-source"));
}

/// A into B while B into A, interleaved at the worst point: one card is
/// always left, holding both cards' work.
#[test]
fn opposite_merges_never_delete_both_cards() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repo_path = repo.path().to_str().unwrap().to_owned();
    let a = filed_with(&store, &repo, card("gp-a", "Alpha", "token-a"));
    let b = filed_with(&store, &repo, card("gp-b", "Alpha too", "token-b"));
    let mut inner = None;
    let outer = merge_with(
        &store,
        &repo_path,
        &named(&a),
        &[named(&b)],
        "A wins.",
        false,
        &mut |step, store| {
            if matches!(step, Step::BeforeDelete(_)) && inner.is_none() {
                inner = Some(merge_tasks(
                    store,
                    &repo_path,
                    &named(&b),
                    &[named(&a)],
                    "B wins.",
                    false,
                ));
            }
        },
    )
    .unwrap();
    let inner = inner.unwrap();
    let survivors: Vec<Value> = [&a, &b].iter().filter_map(|id| live(&store, id)).collect();
    assert_eq!(survivors.len(), 1, "outer {outer:#}\ninner {inner:?}");
    let description = survivors[0]["description"].as_str().unwrap();
    assert!(
        description.contains("token-a") && description.contains("token-b"),
        "{description}"
    );
    assert!(
        outer["ok"] == false || inner.as_ref().map_or(true, |v| v["ok"] == false),
        "exactly one side cannot have finished: outer {outer:#} inner {inner:?}"
    );
}

/// splitmix64: a seeded sequence for the fuzz, with no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Real contention: threads, each with its own connection to one profile,
/// merging random cards into each other, editing them as a person would and
/// re-running merges, all at once. Whatever interleaving the scheduler
/// produces, no card's work is lost, at least one card is left, nothing
/// outside the expected refusals goes wrong, and every merge's own report
/// agrees with the board.
#[test]
fn concurrent_merges_and_edits_never_lose_a_cards_work() {
    let cards = 12;
    let threads = 8;
    let operations = 60;
    for seed in [1_u64, 2, 3, 4] {
        let (_dir, path) = profile();
        let repo = git_repo();
        let repo_path = repo.path().to_str().unwrap().to_owned();
        let ids: Vec<String> = {
            let store = Store::open(&path).unwrap();
            (0..cards)
                .map(|k| {
                    filed_with(
                        &store,
                        &repo,
                        card(
                            &format!("gp-{k}"),
                            &format!("Card {k}"),
                            &format!("token-{k}-"),
                        ),
                    )
                })
                .collect()
        };
        let handles: Vec<_> = (0..threads)
            .map(|worker| {
                let (path, repo_path, ids) = (path.clone(), repo_path.clone(), ids.clone());
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    let mut rng = Rng(seed * 1_000 + worker as u64);
                    let mut seen = Vec::new();
                    for op in 0..operations {
                        let into = ids[rng.below(ids.len())].clone();
                        match rng.below(5) {
                            // An agent launched on the card reports progress,
                            // or finishes it, while others merge it.
                            1 => {
                                let status = ["in_progress", "review", "done"][rng.below(3)];
                                let summary = format!("progress-{worker}-{op}");
                                match complete_task(&store, &repo_path, &into, status, None, Some(&summary)) {
                                    Ok(report) => seen.push(format!("complete-{}", report["outcome"].as_str().unwrap())),
                                    Err(e) => {
                                        assert!(
                                            ["task_merged", "task_deleted", "revision_conflict", "already_done"].contains(&e.code.as_str()),
                                            "seed {seed} worker {worker} op {op}: complete: {}: {}", e.code, e.message
                                        );
                                        seen.push(e.code);
                                    }
                                }
                            }
                            // A person adds a line, never removes one.
                            0 => {
                                if let Some(item) = get_item(&store, &into).unwrap() {
                                    let mut input = resend(&item, &into, item["revision"].as_i64().unwrap(), "fuzz-edit");
                                    let text = format!("{} edit-{worker}-{op}", item["description"].as_str().unwrap());
                                    input.insert("description".into(), json!(text));
                                    match query(&store, "items.put", &Value::Object(input).to_string()) {
                                        Ok(_) => {}
                                        Err(e) if e.code == "revision_conflict" || e.message.contains("bound") => {}
                                        Err(e) => panic!("edit: {} {}", e.code, e.message),
                                    }
                                }
                            }
                            _ => {
                                let count = 1 + rng.below(3);
                                let sources: Vec<MergeTask> = (0..count).map(|_| named(&ids[rng.below(ids.len())])).collect();
                                match merge_tasks(&store, &repo_path, &named(&into), &sources, "Fuzz merge.", false) {
                                    Ok(report) => {
                                        assert!(["merged", "unchanged", "partial"].contains(&report["outcome"].as_str().unwrap()), "{report:#}");
                                        for row in report["sources"].as_array().unwrap() {
                                            if row["outcome"] == "merged" {
                                                assert!(get_item(&store, row["item_id"].as_str().unwrap()).unwrap().is_none(), "reported merged but live: {report:#}");
                                            }
                                        }
                                        seen.push(report["outcome"].as_str().unwrap().to_owned());
                                    }
                                    Err(e) => {
                                        assert!(
                                            ["invalid_input", "task_merged", "task_deleted", "not_found", "merge_too_large", "revision_conflict", "target_done"].contains(&e.code.as_str()),
                                            "seed {seed} worker {worker} op {op}: unexpected {}: {}", e.code, e.message
                                        );
                                        seen.push(e.code);
                                    }
                                }
                            }
                        }
                    }
                    seen
                })
            })
            .collect();
        let outcomes: Vec<String> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let store = Store::open(&path).unwrap();
        let survivors: Vec<Value> = ids
            .iter()
            .filter_map(|id| get_item(&store, id).unwrap())
            .collect();
        assert!(
            !survivors.is_empty(),
            "seed {seed}: every card was deleted: {outcomes:?}"
        );
        for k in 0..cards {
            let token = format!("token-{k}-");
            assert!(
                survivors
                    .iter()
                    .any(|item| item["description"].as_str().unwrap().contains(&token)),
                "seed {seed}: {token} was lost; survivors {survivors:#?}"
            );
        }
        for item in &survivors {
            assert!(
                item["description"].as_str().unwrap().len() <= file_tasks::MAX_TASK_DESCRIPTION
            );
        }
        // Every deleted card went into a card that holds its work.
        for id in &ids {
            if get_item(&store, id).unwrap().is_none() {
                let last = last_revision(&store, id).unwrap().unwrap();
                assert!(
                    matches!(why_gone(&last), Gone::Merged { .. }),
                    "seed {seed}: {id} deleted without a merge block"
                );
            }
        }
        assert!(
            outcomes.iter().any(|o| o == "merged"),
            "seed {seed}: the fuzz merged nothing: {outcomes:?}"
        );
    }
}

/// Make every soft delete fail with a store error — not a conflict — while
/// leaving ordinary saves alone: the reason `put` writes `body`, the delete
/// writes `deleted`.
fn deletes_fail(store: &Store) {
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER IF NOT EXISTS test_deletes_fail BEFORE UPDATE OF deleted ON work_items
             WHEN new.deleted=1 BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;",
        )
        .unwrap();
}

/// The reported defect: with nothing left to write to the target (it already
/// holds the source's section), the source's reason `put` succeeded, its
/// delete then failed with a non-conflict error, and the call returned that
/// error bare — reading as "nothing changed" while the source now carried a
/// merge block. It is a partial merge, and must say so.
#[test]
fn a_delete_that_fails_after_the_reason_is_recorded_is_a_partial_merge_not_a_bare_error() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-source"));
    deletes_fail(&store);
    // First call: the target is written, then the delete fails.
    let first = merge(&store, &repo, &target, &[&source], "Same work.").unwrap();
    assert_eq!(first["outcome"], "partial", "{first:#}");
    // A person tidies the source's logs, taking the reason block off it, so
    // the next call has to record it again — and has nothing to write to the
    // target, which already holds the section.
    person_edits(&store, &source, |input| {
        input.insert("logs".into(), json!(""));
    });
    let second = merge(&store, &repo, &target, &[&source], "Same work.")
        .expect("a recorded reason and a failed delete are a partial merge, not a bare error");
    assert_eq!(
        (second["ok"].clone(), second["outcome"].clone()),
        (json!(false), json!("partial")),
        "{second:#}"
    );
    let row = &second["sources"][0];
    assert_eq!(row["outcome"], "not_merged");
    let detail = row["detail"].as_str().unwrap();
    assert!(
        detail.contains("reason is recorded") && detail.contains("injected delete failure"),
        "{detail}"
    );
    assert!(second["next_step"].as_str().unwrap().contains("Re-run"));
    // The source is live, with its work and the reason on it.
    let live_source = live(&store, &source).expect("the source is still on the board");
    assert!(live_source["logs"].as_str().unwrap().contains("Same work."));

    // Once deletes work again, the same call finishes the merge.
    store
        .connection()
        .execute_batch("DROP TRIGGER test_deletes_fail")
        .unwrap();
    let third = merge(&store, &repo, &target, &[&source], "Same work.").unwrap();
    assert_eq!(third["outcome"], "merged", "{third:#}");
    assert!(live(&store, &source).is_none());
}

/// `gitpulse_delete_task` shares the two writes: a failed delete after the
/// reason is recorded names both facts instead of only the error.
#[test]
fn a_delete_task_whose_delete_fails_says_the_reason_was_recorded() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let id = filed_with(&store, &repo, card("gp-d", "Doomed", "Body"));
    deletes_fail(&store);
    let error = delete_task(
        &store,
        repo.path().to_str().unwrap(),
        &id,
        None,
        "Not needed.",
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, "store_error");
    assert!(
        error.message.contains("reason was recorded"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("still on the board"),
        "{}",
        error.message
    );
    assert!(live(&store, &id).is_some());
}

/// Archived is its own flag: work merged into an archived card would leave
/// the board, so the merge is refused and nothing is written.
#[test]
fn work_on_the_board_is_never_merged_into_an_archived_task() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "Source body"));
    person_edits(&store, &target, |input| {
        input.insert("archived".into(), json!(true));
    });
    let before = board(&store, &repository);
    let error = merge(&store, &repo, &target, &[&source], "Into the archive.").unwrap_err();
    assert_eq!(error.code, "target_archived", "{}", error.message);
    assert_eq!(
        board(&store, &repository),
        before,
        "a refusal writes nothing"
    );
    // Archived into archived is fine: nothing leaves the board.
    person_edits(&store, &source, |input| {
        input.insert("archived".into(), json!(true));
    });
    assert_eq!(
        merge(&store, &repo, &target, &[&source], "Both archived.").unwrap()["outcome"],
        "merged"
    );
}

/// A source's checklist and links are not left behind in its history: they
/// join the target's, minus links to the cards being merged.
#[test]
fn checklists_and_links_are_folded_into_the_target() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "Source body"));
    let blocker = filed_with(&store, &repo, card("gp-b", "Blocker", "Blocker body"));
    let epic = filed_with(&store, &repo, card("gp-e", "Epic", "Epic body"));
    person_edits(&store, &target, |input| {
        input.insert(
            "checklist".into(),
            json!([{"text": "Shared step", "done": true}]),
        );
    });
    person_edits(&store, &source, |input| {
        input.insert(
            "checklist".into(),
            json!([{"text": "Shared step", "done": false}, {"text": "Source step", "done": false}]),
        );
        input.insert(
            "links".into(),
            json!([
                {"kind": "blocks", "item_id": blocker},
                {"kind": "related", "item_id": target},
                {"kind": "parent", "item_id": epic}
            ]),
        );
    });
    let result = merge(&store, &repo, &target, &[&source], "Same work.").unwrap();
    assert_eq!(result["outcome"], "merged", "{result:#}");
    let merged = live(&store, &target).unwrap();
    // The target's own entry, and its done state, wins over the source's.
    assert_eq!(
        merged["checklist"],
        json!([{"text": "Shared step", "done": true}, {"text": "Source step", "done": false}])
    );
    // A link to the target itself is dropped, and the source's parent with it:
    // the target keeps its own (none) rather than inherit a second one.
    assert_eq!(
        merged["links"],
        json!([{"kind": "blocks", "item_id": blocker}])
    );
}

/// The board's Merge is this merge: same writes, same refusals, and every
/// card named at the revision the board drew it at.
#[test]
fn the_board_merge_runs_the_same_merge_and_refuses_stale_cards() {
    let (_dir, path) = profile();
    let store = Store::open(&path).unwrap();
    let repo = git_repo();
    let repository = registered(&store, &repo);
    let target = filed_with(&store, &repo, card("gp-t", "Target", "Target body"));
    let source = filed_with(&store, &repo, card("gp-s", "Source", "token-source"));
    let at = |id: &str| live(&store, id).unwrap()["revision"].as_i64().unwrap();
    let request = |source_revision: i64| {
        json!({
            "repository_id": repository["id"],
            "into": {"id": target, "expected_revision": at(&target)},
            "sources": [{"id": source, "expected_revision": source_revision}],
            "reason": "Duplicate cards.",
        })
        .to_string()
    };
    let before = board(&store, &repository);
    let stale = merge_request(&store, &request(at(&source) + 1)).unwrap_err();
    assert_eq!(stale.code, "revision_conflict");
    assert_eq!(
        board(&store, &repository),
        before,
        "a refusal writes nothing"
    );
    for bad in [
        json!({}).to_string(),
        json!({"repository_id": repository["id"], "into": {"id": target}, "sources": [], "reason": "x"}).to_string(),
        "not json".to_owned(),
    ] {
        assert_eq!(merge_request(&store, &bad).unwrap_err().code, "invalid_input", "{bad}");
    }
    let result = merge_request(&store, &request(at(&source))).unwrap();
    assert_eq!(result["outcome"], "merged", "{result:#}");
    assert!(live(&store, &source).is_none());
    assert!(live(&store, &target).unwrap()["description"]
        .as_str()
        .unwrap()
        .contains("## Merged from"));
}
