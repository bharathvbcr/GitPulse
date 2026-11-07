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
            split_index: Some(false),
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
        family: WorktreeFamily::default(),
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
        FSMONITOR_MIN_TRACKED_FILES.min(UNTRACKED_CACHE_MIN_TRACKED_FILES) - 1,
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
    assert!(
        !at(UNTRACKED_CACHE_MIN_TRACKED_FILES - 1, 1, 0).contains(&TuningChange::UntrackedCache)
    );
    assert!(at(UNTRACKED_CACHE_MIN_TRACKED_FILES, 1, 0).contains(&TuningChange::UntrackedCache));
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
    assert!(!offered(&index, NEW_GIT).contains(&TuningChange::UntrackedCache));

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
        "core.untrackedcache",
        Some("false"),
        ConfigScope::Local,
    );
    assert!(!offered(&r, NEW_GIT).contains(&TuningChange::UntrackedCache));
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
fn feature_many_files_already_turns_the_untracked_cache_on() {
    // manyFiles includes the cache, so offering it again would be a no-op write.
    let mut r = report(100_000, 1, 0);
    key(
        &mut r,
        "feature.manyfiles",
        Some("true"),
        ConfigScope::Global,
    );
    assert!(!offered(&r, NEW_GIT).contains(&TuningChange::UntrackedCache));
    let plan = plan_apply(&r, NEW_GIT, "/r/.git", &[TuningChange::UntrackedCache]).unwrap();
    assert!(plan.runnable().is_empty());
    assert!(plan.steps[0]
        .skipped
        .as_deref()
        .unwrap()
        .contains("already on"));
    // manyFiles is detected and never offered or written, at any size.
    let small = report(100_000, 1, 0);
    assert!(recommend(&small, NEW_GIT)
        .iter()
        .all(|o| !o.title.contains("manyFiles")));
}

#[test]
fn fsmonitor_needs_a_measured_platform_and_git_2_36_or_newer() {
    let big = report(100_000, 1, 0);
    let unmeasured = GitEnv {
        fsmonitor_measured: false,
        ..NEW_GIT
    };
    assert!(!offered(&big, unmeasured).contains(&TuningChange::Fsmonitor));
    for old in [(2, 35, 2), (2, 35, 1), (2, 30, 0), (2, 0, 0)] {
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
        version: Some((2, 36, 0)),
        ..NEW_GIT
    };
    assert!(offered(&big, env).contains(&TuningChange::Fsmonitor));
}

#[test]
fn a_boolean_fsmonitor_is_refused_before_git_2_36_at_the_plan() {
    let big = report(100_000, 1, 0);
    for version in [Some((2, 35, 2)), Some((2, 35, 1)), Some((2, 20, 0)), None] {
        let env = GitEnv { version, ..NEW_GIT };
        let refused = plan_apply(&big, env, "/r/.git", &[TuningChange::Fsmonitor]).unwrap_err();
        assert!(refused.contains("2.36.0"), "{version:?}: {refused}");
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
    let refused =
        plan_apply(&split, NEW_GIT, "/r/.git", &[TuningChange::UntrackedCache]).unwrap_err();
    assert!(refused.contains("add-then-commit"), "{refused}");
    assert!(!offered(&split, NEW_GIT).contains(&TuningChange::UntrackedCache));
    assert!(split_index_conflict(&split, &["core.untrackedcache"]).is_some());
    // a split index requested over manyFiles already on
    let mut many = report(100_000, 1, 0);
    key(
        &mut many,
        "core.untrackedcache",
        Some("true"),
        ConfigScope::Local,
    );
    assert!(split_index_conflict(&many, &["core.splitindex"]).is_some());
    // both requested at once
    let clean = report(100_000, 1, 0);
    assert!(split_index_conflict(&clean, &["core.untrackedcache", "core.splitindex"]).is_some());
    // each alone, with the other absent, is not a conflict
    assert!(split_index_conflict(&clean, &["core.untrackedcache"]).is_none());
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
    assert!(split_index_conflict(&off, &["core.untrackedcache"]).is_none());
    // The UI says so while nothing is offered.
    assert!(warnings(&split)
        .iter()
        .any(|w| w.contains("core.splitIndex")));
    assert!(warnings(&many)
        .iter()
        .any(|w| w.contains("untracked cache is already on")));
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
    let hostile: [&[&str]; 9] = [
        &["git", "config", "--local", "core.splitIndex", "true"],
        &["git", "config", "--local", "feature.manyFiles", "true"],
        &["git", "config", "--global", "core.untrackedCache", "true"],
        &["git", "config", "--local", "core.untrackedCache", "false"],
        &["git", "config", "--local", "core.fsmonitor", "/evil/hook"],
        &["git", "config", "--local", "core.hooksPath", "/evil"],
        &["git", "maintenance", "stop"],
        &[
            "git",
            "config",
            "--local",
            "core.untrackedCache",
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
    let cache = &plan.steps[0];
    assert_eq!(cache.argv, change_argv(TuningChange::UntrackedCache));
    assert_eq!(cache.writes[0].scope, TuningScope::RepoConfig);
    // The undo list: unset the key, then take the extension out of the index.
    assert_eq!(
        cache.undo,
        vec![
            unset_argv(UnsetKey::UntrackedCache),
            strings(&["git", "update-index", "--no-untracked-cache"]),
        ]
    );
    assert!(cache
        .undo_note
        .as_deref()
        .unwrap()
        .contains("leaves the untracked cache in the index"));
    let note = cache.notes.join(" ");
    assert!(note.contains("Nothing changes at the click"), "{note}");
    assert!(note.contains("next ordinary `git status`"), "{note}");
    assert!(note.contains("GIT_OPTIONAL_LOCKS=0"), "{note}");

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
        "core.untrackedcache",
        Some("true"),
        ConfigScope::Global,
    );
    r.maintenance.registered = Some(true);
    let plan = plan_apply(
        &r,
        NEW_GIT,
        "/r/.git",
        &[
            TuningChange::UntrackedCache,
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
        &[TuningChange::UntrackedCache, TuningChange::Maintenance],
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
    assert!(plan_apply(
        &unreadable,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache]
    )
    .is_err());
}

#[test]
fn duplicate_requests_make_one_step() {
    let r = report(100_000, 1, 0);
    let plan = plan_apply(
        &r,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache, TuningChange::UntrackedCache],
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

#[test]
fn a_split_index_in_the_index_file_or_a_sibling_is_never_offered_many_files() {
    let mut carried = report(100_000, 1, 0);
    carried.index.split_index = Some(true);
    assert!(!offered(&carried, NEW_GIT).contains(&TuningChange::UntrackedCache));
    let refused = plan_apply(
        &carried,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache],
    )
    .unwrap_err();
    assert!(refused.contains("split index"), "{refused}");
    assert!(warnings(&carried).iter().any(|w| w.contains("split index")));

    let mut unknown = report(100_000, 1, 0);
    unknown.index.split_index = None;
    assert!(!offered(&unknown, NEW_GIT).contains(&TuningChange::UntrackedCache));

    let mut sibling = report(100_000, 1, 0);
    sibling.family.split_index_in = vec!["/work/sibling".to_string()];
    assert!(!offered(&sibling, NEW_GIT).contains(&TuningChange::UntrackedCache));
    let refused = plan_apply(
        &sibling,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache],
    )
    .unwrap_err();
    assert!(refused.contains("/work/sibling"), "{refused}");

    for family in [
        WorktreeFamily {
            unverified_indexes: vec!["/work/odd".to_string()],
            ..WorktreeFamily::default()
        },
        WorktreeFamily {
            truncated: true,
            ..WorktreeFamily::default()
        },
    ] {
        let mut r = report(100_000, 1, 0);
        r.family = family;
        assert!(plan_apply(&r, NEW_GIT, "/r/.git", &[TuningChange::UntrackedCache]).is_err());
    }
    // A sibling that is fine, or has only a config.worktree to be read at plan
    // time, does not block the offer.
    let mut fine = report(100_000, 1, 0);
    fine.family = WorktreeFamily {
        worktrees: 2,
        with_worktree_config: vec!["/work/cone".to_string()],
        ..WorktreeFamily::default()
    };
    assert!(offered(&fine, NEW_GIT).contains(&TuningChange::UntrackedCache));
}

#[test]
fn the_wording_says_the_next_ordinary_git_status_writes_the_cache() {
    let r = report(100_000, 1, 0);
    let plan = plan_apply(&r, NEW_GIT, "/r/.git", &[TuningChange::UntrackedCache]).unwrap();
    let notes = plan.steps[0].notes.join(" ");
    assert!(notes.contains("Nothing changes at the click"), "{notes}");
    assert!(notes.contains("next ordinary `git status`"), "{notes}");
    assert!(
        notes.contains("trusts the file system's directory timestamps"),
        "{notes}"
    );
    let offer = recommend(&r, NEW_GIT)
        .into_iter()
        .find(|o| o.change == TuningChange::UntrackedCache)
        .unwrap();
    assert!(
        offer.reason.contains("next ordinary git status"),
        "{}",
        offer.reason
    );
    assert!(
        !offer.reason.contains("smaller"),
        "no index-size claim: {}",
        offer.reason
    );
    // It does not claim to speed GitPulse's own reads.
    assert!(
        offer.reason.contains("does not speed `git status -u`"),
        "{}",
        offer.reason
    );
    assert!(offer.reason.contains("GitPulse's own"), "{}", offer.reason);
}

#[test]
fn a_split_index_refusal_says_what_was_reproduced_and_what_is_inferred() {
    let mut split = report(100_000, 1, 0);
    key(
        &mut split,
        "core.splitindex",
        Some("true"),
        ConfigScope::Global,
    );
    let refused =
        plan_apply(&split, NEW_GIT, "/r/.git", &[TuningChange::UntrackedCache]).unwrap_err();
    assert!(refused.contains("was reproduced breaking"), "{refused}");
    assert!(refused.contains("did not reproduce it"), "{refused}");
    assert!(refused.contains("was not isolated"), "{refused}");
    assert!(
        refused.contains("instead of inferring it is safe"),
        "{refused}"
    );
    // The index-file and sibling refusals carry the same account.
    let mut carried = report(100_000, 1, 0);
    carried.index.split_index = Some(true);
    let refused = plan_apply(
        &carried,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache],
    )
    .unwrap_err();
    assert!(refused.contains("did not reproduce it"), "{refused}");
    let mut sibling = report(100_000, 1, 0);
    sibling.family.split_index_in = vec!["/w/s".to_string()];
    let refused = plan_apply(
        &sibling,
        NEW_GIT,
        "/r/.git",
        &[TuningChange::UntrackedCache],
    )
    .unwrap_err();
    assert!(
        refused.contains("/w/s") && refused.contains("was not isolated"),
        "{refused}"
    );
}
