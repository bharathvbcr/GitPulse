use super::advice::{strings, unset_argv, UnsetKey};
use super::*;

const NEW_GIT: GitEnv = GitEnv {
    version: Some((2, 54, 0)),
    fsmonitor_measured: true,
};

fn key(report: &mut RepoTuningReport, lower: &str, value: Option<&str>, scope: ConfigScope) {
    let display = TUNING_KEYS.iter().find(|(l, _)| *l == lower).unwrap().1;
    let slot = report.keys.iter_mut().find(|k| k.key == display).unwrap();
    slot.value = value.map(str::to_string);
    slot.origin = value.map(|_| scope);
}

/// A readable, untuned repository of `files` tracked files and `packs` packs.
fn report(files: u64, packs: u64, loose: u64) -> RepoTuningReport {
    RepoTuningReport {
        maintenance: MaintenanceState {
            registered: Some(false),
            ..MaintenanceState::default()
        },
        midx: Some(false),
        objects: ObjectPressure {
            packs: Some(packs),
            loose_objects: Some(loose),
            loose_exact: true,
            gc_auto: Some(GC_AUTO_DEFAULT),
            loose_over_gc_auto: Some(loose > GC_AUTO_DEFAULT),
        },
        index: IndexInfo {
            state: IndexReadState::Present,
            version: Some(2),
            entries: Some(files),
            entries_exact: true,
            skip_hash: Some(false),
            note: None,
        },
        partial_clone: Some(false),
        sparse_checkout: Some(false),
        keys: TUNING_KEYS
            .iter()
            .map(|(_, display)| TuningKeyValue {
                key: (*display).to_string(),
                value: None,
                origin: None,
            })
            .collect(),
        config_note: None,
        notes: Vec::new(),
    }
}

fn offered(report: &RepoTuningReport, env: GitEnv) -> Vec<TuningChange> {
    recommend(report, env)
        .into_iter()
        .map(|o| o.change)
        .collect()
}

#[test]
fn nothing_is_offered_below_any_threshold() {
    let below = report(
        FSMONITOR_MIN_TRACKED_FILES.min(MANYFILES_MIN_TRACKED_FILES) - 1,
        5_000,
        MAINTENANCE_MIN_LOOSE_OBJECTS - 1,
    );
    assert_eq!(recommend(&below, NEW_GIT), Vec::new());
    // And a tiny repository, an empty one, and one with no objects at all.
    for files in [0, 1, 500, 9_999] {
        assert_eq!(
            offered(&report(files, 1, 0), NEW_GIT),
            Vec::new(),
            "{files} files"
        );
    }
}

#[test]
fn each_offer_starts_exactly_at_its_threshold() {
    let at = |files, packs, loose| offered(&report(files, packs, loose), NEW_GIT);
    assert!(!at(FSMONITOR_MIN_TRACKED_FILES - 1, 1, 0).contains(&TuningChange::Fsmonitor));
    assert!(at(FSMONITOR_MIN_TRACKED_FILES, 1, 0).contains(&TuningChange::Fsmonitor));
    assert!(!at(MANYFILES_MIN_TRACKED_FILES - 1, 1, 0).contains(&TuningChange::ManyFiles));
    assert!(at(MANYFILES_MIN_TRACKED_FILES, 1, 0).contains(&TuningChange::ManyFiles));
    // A pile of packs alone is never offered for (one incremental repack did
    // not consolidate them).
    assert!(!at(10, 5_000, 0).contains(&TuningChange::Maintenance));
    assert!(!at(10, 1, MAINTENANCE_MIN_LOOSE_OBJECTS - 1).contains(&TuningChange::Maintenance));
    assert!(at(10, 1, MAINTENANCE_MIN_LOOSE_OBJECTS + 1).contains(&TuningChange::Maintenance));
}

#[test]
fn the_fsmonitor_threshold_is_the_measured_forty_thousand() {
    // ft-7450105d: slower below ~10k, faster from 40k. Moving this needs a new
    // committed measurement in docs/PERFORMANCE.md.
    assert_eq!(FSMONITOR_MIN_TRACKED_FILES, 40_000);
}

#[test]
fn an_unknown_input_offers_nothing_rather_than_guessing() {
    let mut big = report(200_000, 500, 500_000);
    big.config_note = Some("boom".to_string());
    assert!(recommend(&big, NEW_GIT).is_empty(), "unreadable config");

    let mut index = report(200_000, 1, 0);
    index.index.state = IndexReadState::Unknown;
    index.index.entries = None;
    assert!(!offered(&index, NEW_GIT).contains(&TuningChange::Fsmonitor));
    assert!(!offered(&index, NEW_GIT).contains(&TuningChange::ManyFiles));

    let mut inexact = report(200_000, 1, 0);
    inexact.index.entries_exact = false;
    assert!(
        offered(&inexact, NEW_GIT).is_empty(),
        "a partial count is not a count"
    );

    let mut registered_unknown = report(10, 1, 50_000);
    registered_unknown.maintenance.registered = None;
    assert!(offered(&registered_unknown, NEW_GIT).is_empty());

    let unprobed = GitEnv::default();
    assert!(
        offered(&report(200_000, 500, 0), unprobed).is_empty(),
        "git not probed"
    );

    let mut packs_unknown = report(10, 1, 50_000);
    packs_unknown.objects.loose_objects = None;
    packs_unknown.objects.loose_over_gc_auto = None;
    assert!(offered(&packs_unknown, NEW_GIT).is_empty());
}

#[test]
fn a_value_the_person_set_is_respected_whatever_it_is() {
    for value in ["false", "true", "/usr/local/bin/watchman-hook"] {
        let mut r = report(200_000, 1, 0);
        key(&mut r, "core.fsmonitor", Some(value), ConfigScope::Global);
        assert!(
            !offered(&r, NEW_GIT).contains(&TuningChange::Fsmonitor),
            "{value}"
        );
    }
    let mut r = report(200_000, 1, 0);
    key(
        &mut r,
        "feature.manyfiles",
        Some("false"),
        ConfigScope::Local,
    );
    assert!(!offered(&r, NEW_GIT).contains(&TuningChange::ManyFiles));
    // gc turned off, or maintenance turned off: the person runs their own.
    let mut gc_off = report(10, 1, 50_000);
    gc_off.objects.gc_auto = Some(0);
    assert!(offered(&gc_off, NEW_GIT).is_empty());
    let mut m_off = report(10, 1, 50_000);
    m_off.maintenance.auto = Some("false".to_string());
    assert!(offered(&m_off, NEW_GIT).is_empty());
    let mut registered = report(10, 1, 50_000);
    registered.maintenance.registered = Some(true);
    assert!(offered(&registered, NEW_GIT).is_empty());
}

#[test]
fn many_files_is_not_offered_to_an_index_that_already_has_what_it_adds() {
    let mut r = report(100_000, 1, 0);
    r.index.version = Some(4);
    r.index.skip_hash = Some(true);
    assert!(!offered(&r, NEW_GIT).contains(&TuningChange::ManyFiles));
}

#[test]
fn fsmonitor_needs_a_measured_platform_and_git_after_2_35_1() {
    let big = report(100_000, 1, 0);
    let unmeasured = GitEnv {
        fsmonitor_measured: false,
        ..NEW_GIT
    };
    assert!(!offered(&big, unmeasured).contains(&TuningChange::Fsmonitor));
    for old in [(2, 35, 1), (2, 30, 0), (2, 0, 0)] {
        let env = GitEnv {
            version: Some(old),
            ..NEW_GIT
        };
        assert!(
            !offered(&big, env).contains(&TuningChange::Fsmonitor),
            "{old:?}"
        );
    }
    let env = GitEnv {
        version: Some((2, 35, 2)),
        ..NEW_GIT
    };
    assert!(offered(&big, env).contains(&TuningChange::Fsmonitor));
}

#[test]
fn a_boolean_fsmonitor_is_refused_on_git_2_35_1_and_older_at_the_plan() {
    let big = report(100_000, 1, 0);
    for version in [Some((2, 35, 1)), Some((2, 20, 0)), None] {
        let env = GitEnv { version, ..NEW_GIT };
        let refused = plan_apply(&big, env, "/r/.git", &[TuningChange::Fsmonitor]).unwrap_err();
        assert!(refused.contains("2.35.2"), "{version:?}: {refused}");
        assert!(refused.contains("hook path"), "{refused}");
    }
}

#[test]
fn split_index_and_many_files_are_refused_in_both_orders() {
    // many_files requested over a split index
    let mut split = report(100_000, 1, 0);
    key(
        &mut split,
        "core.splitindex",
        Some("true"),
        ConfigScope::Global,
    );
    let refused = plan_apply(&split, NEW_GIT, "/r/.git", &[TuningChange::ManyFiles]).unwrap_err();
    assert!(refused.contains("add-then-commit"), "{refused}");
    assert!(!offered(&split, NEW_GIT).contains(&TuningChange::ManyFiles));
    assert!(split_index_conflict(&split, &["feature.manyfiles"]).is_some());
    // a split index requested over manyFiles already on
    let mut many = report(100_000, 1, 0);
    key(
        &mut many,
        "feature.manyfiles",
        Some("true"),
        ConfigScope::Local,
    );
    assert!(split_index_conflict(&many, &["core.splitindex"]).is_some());
    // both requested at once
    let clean = report(100_000, 1, 0);
    assert!(split_index_conflict(&clean, &["feature.manyfiles", "core.splitindex"]).is_some());
    // each alone, with the other absent, is not a conflict
    assert!(split_index_conflict(&clean, &["feature.manyfiles"]).is_none());
    assert!(split_index_conflict(&clean, &["core.splitindex"]).is_none());
    assert!(split_index_conflict(&clean, &[]).is_none());
    // false is not on
    let mut off = report(100_000, 1, 0);
    key(
        &mut off,
        "core.splitindex",
        Some("false"),
        ConfigScope::Global,
    );
    assert!(split_index_conflict(&off, &["feature.manyfiles"]).is_none());
    // The UI says so while nothing is offered.
    assert!(warnings(&split)
        .iter()
        .any(|w| w.contains("core.splitIndex")));
    assert!(warnings(&many)
        .iter()
        .any(|w| w.contains("feature.manyFiles")));
}

#[test]
fn no_apply_argv_can_set_core_split_index() {
    assert!(!CONFIG_WRITE_KEYS
        .iter()
        .any(|k| k.eq_ignore_ascii_case("core.splitIndex")));
    for change in TuningChange::ALL {
        let argv = change_argv(change);
        assert!(!argv
            .iter()
            .any(|a| a.to_ascii_lowercase().contains("splitindex")));
        assert!(is_apply_argv(&argv));
    }
    let hostile: [&[&str]; 8] = [
        &["git", "config", "--local", "core.splitIndex", "true"],
        &["git", "config", "--global", "feature.manyFiles", "true"],
        &["git", "config", "--local", "feature.manyFiles", "false"],
        &["git", "config", "--local", "core.fsmonitor", "/evil/hook"],
        &["git", "config", "--local", "core.hooksPath", "/evil"],
        &["git", "maintenance", "stop"],
        &[
            "git",
            "config",
            "--local",
            "feature.manyFiles",
            "true",
            "extra",
        ],
        &[],
    ];
    for argv in hostile {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        assert!(!is_apply_argv(&argv), "{argv:?}");
    }
}

#[test]
fn the_written_keys_are_exactly_the_listed_ones() {
    let mut keys: Vec<String> = TuningChange::ALL
        .iter()
        .filter_map(|c| {
            let argv = change_argv(*c);
            (argv[1] == "config").then(|| argv[3].clone())
        })
        .collect();
    keys.sort();
    let mut listed: Vec<String> = CONFIG_WRITE_KEYS.iter().map(|k| k.to_string()).collect();
    listed.sort();
    assert_eq!(keys, listed);
}

#[test]
fn the_preview_names_argv_scope_undo_and_the_hazards() {
    let r = report(100_000, 1, 50_000);
    let plan = plan_apply(&r, NEW_GIT, "/r/.git", &TuningChange::ALL).unwrap();
    assert_eq!(plan.steps.len(), 3);
    let many = &plan.steps[0];
    assert_eq!(many.argv, change_argv(TuningChange::ManyFiles));
    assert_eq!(many.writes[0].scope, TuningScope::RepoConfig);
    assert_eq!(many.undo, vec![unset_argv(UnsetKey::ManyFiles)]);
    let note = many.notes.join(" ");
    assert!(note.contains("skipHash"), "{note}");
    assert!(note.contains("2.40"), "fsck floor: {note}");
    assert!(note.contains("2.13"), "refusal floor: {note}");

    let fs = &plan.steps[1];
    assert_eq!(fs.undo[0], strings(&["git", "fsmonitor--daemon", "stop"]));
    assert_eq!(fs.undo[1], unset_argv(UnsetKey::Fsmonitor));
    let note = fs.notes.join(" ");
    assert!(note.contains("long-lived"), "{note}");
    assert!(note.contains("does not own"), "{note}");
    assert!(note.contains("git fsmonitor--daemon stop"), "{note}");

    let m = &plan.steps[2];
    assert_eq!(m.argv, strings(&["git", "maintenance", "start"]));
    let scopes: Vec<TuningScope> = m.writes.iter().map(|w| w.scope).collect();
    assert_eq!(
        scopes,
        [
            TuningScope::GlobalConfig,
            TuningScope::RepoConfig,
            TuningScope::OsScheduler
        ]
    );
    assert_eq!(m.undo[0], strings(&["git", "maintenance", "unregister"]));
    assert_eq!(
        m.undo.len(),
        3,
        "unsets for the two keys it wrote: {:?}",
        m.undo
    );
    assert!(m.undo_note.as_deref().unwrap().contains("maintenance stop"));
    assert_eq!(plan.runnable().len(), 3);
    assert_eq!(plan.timeout_secs, TUNING_APPLY_TIMEOUT_SECS);
}

#[test]
fn undo_does_not_remove_a_key_the_person_already_had() {
    let mut r = report(10, 1, 50_000);
    r.maintenance.strategy = Some("geometric".to_string());
    let plan = plan_apply(&r, NEW_GIT, "/r/.git", &[TuningChange::Maintenance]).unwrap();
    let undo = &plan.steps[0].undo;
    assert!(undo.contains(&unset_argv(UnsetKey::MaintenanceAuto)));
    assert!(!undo.contains(&unset_argv(UnsetKey::MaintenanceStrategy)));
}

#[test]
fn a_change_already_in_place_is_skipped_and_reported() {
    let mut r = report(100_000, 1, 50_000);
    key(
        &mut r,
        "feature.manyfiles",
        Some("true"),
        ConfigScope::Global,
    );
    r.maintenance.registered = Some(true);
    let plan = plan_apply(
        &r,
        NEW_GIT,
        "/r/.git",
        &[
            TuningChange::ManyFiles,
            TuningChange::Fsmonitor,
            TuningChange::Maintenance,
        ],
    )
    .unwrap();
    assert!(plan.steps[0]
        .skipped
        .as_deref()
        .unwrap()
        .contains("already on"));
    assert!(plan.steps[0].argv.is_empty());
    assert!(plan.steps[1].skipped.is_none());
    assert!(plan.steps[2]
        .skipped
        .as_deref()
        .unwrap()
        .contains("already registered"));
    assert_eq!(plan.runnable(), vec![change_argv(TuningChange::Fsmonitor)]);
    // Everything in place: a plan with nothing to run, not an error.
    let all = plan_apply(
        &r,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::ManyFiles, TuningChange::Maintenance],
    )
    .unwrap();
    assert!(all.runnable().is_empty());
}

#[test]
fn a_change_below_its_threshold_cannot_be_planned() {
    let small = report(500, 1, 0);
    for change in TuningChange::ALL {
        let refused = plan_apply(&small, NEW_GIT, "/r/.git", &[change]).unwrap_err();
        assert!(refused.contains("not offered"), "{change:?}: {refused}");
    }
    assert!(plan_apply(&small, NEW_GIT, "/r/.git", &[]).is_err());
    let mut unreadable = report(100_000, 1, 50_000);
    unreadable.config_note = Some("x".into());
    assert!(plan_apply(&unreadable, NEW_GIT, "/r/.git", &[TuningChange::ManyFiles]).is_err());
}

#[test]
fn duplicate_requests_make_one_step() {
    let r = report(100_000, 1, 0);
    let plan = plan_apply(
        &r,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::ManyFiles, TuningChange::ManyFiles],
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 1);
}

#[test]
fn advise_bundles_offers_and_warnings() {
    let advice = advise(report(100_000, 1, 50_000), NEW_GIT);
    assert_eq!(advice.offers.len(), 3);
    assert!(advice.warnings.is_empty());
    assert!(!advice.apply_in_flight);
    assert!(advice
        .offers
        .iter()
        .all(|o| !o.reason.is_empty() && !o.title.is_empty()));
}

#[test]
fn the_maintenance_preview_discloses_the_background_fetch_and_the_object_database_lock() {
    let r = report(10, 1, 50_000);
    let plan = plan_apply(&r, NEW_GIT, "/r/.git", &[TuningChange::Maintenance]).unwrap();
    let step = &plan.steps[0];
    let notes = step.notes.join(" ");
    // What the incremental strategy runs, as `git help maintenance` lists it.
    for word in [
        "commit-graph hourly",
        "prefetch hourly",
        "loose-objects daily",
        "incremental-repack daily",
    ] {
        assert!(notes.contains(word), "{word}: {notes}");
    }
    // The unattended network use, with the man page's own words and the opt-out.
    assert!(notes.contains("Background network"), "{notes}");
    assert!(notes.contains("all registered remotes"), "{notes}");
    assert!(notes.contains("refs/prefetch/"), "{notes}");
    assert!(notes.contains("credential"), "{notes}");
    assert!(notes.contains("skipFetchAll"), "{notes}");
    // The lock, as documented, without claiming more than the page says.
    assert!(
        notes.contains("lock on the repository's object database"),
        "{notes}"
    );
    assert!(
        notes.contains("does not take the lock in the same way"),
        "{notes}"
    );
    assert!(
        notes.contains("does not say whether ordinary reads wait"),
        "{notes}"
    );
    // The undo covers the prefetch for this repository.
    assert!(step
        .undo
        .contains(&strings(&["git", "maintenance", "unregister"])));
    assert!(
        notes.contains("`git maintenance unregister` stops it"),
        "{notes}"
    );
    assert!(step
        .undo_note
        .as_deref()
        .unwrap()
        .contains("prefetch included"));
    // The hint on the offer says the same in short.
    let offer = recommend(&r, NEW_GIT)
        .into_iter()
        .find(|o| o.change == TuningChange::Maintenance)
        .unwrap();
    assert!(
        offer.reason.contains("background fetch of every remote"),
        "{}",
        offer.reason
    );
    assert!(
        offer.reason.contains("object database lock"),
        "{}",
        offer.reason
    );
}
