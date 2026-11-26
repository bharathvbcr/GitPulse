// Fixes from the review of phase 1: each test here fails against the code
// before its fix. A child of `relief_tests`, sharing its rigs and fakes.

use super::*;

#[test]
fn advise_spends_nothing_and_switching_to_act_can_freeze_what_advise_only_showed() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Advise);
    live_all(&rig, &table);
    // Pinned alone (kernel Critical, nothing rising): the freeze advise shows
    // is the one that spends the episode's pinned-only allowance in act.
    rig.ticks(SEVERE_SAMPLES as usize, critical_flat, &table);
    assert_eq!(rig.relief.status.frozen.len(), 1);
    assert!(rig.relief.status.frozen[0].advised_only);
    assert!(rig.fake.stops().is_empty(), "advise signals nothing");
    *rig.mode.lock().unwrap() = SupervisorMode::Act;
    rig.ticks(SEVERE_SAMPLES as usize + 5, critical_flat, &table);
    assert_eq!(
        rig.fake.stops(),
        [102, 103],
        "what advise only showed is still act's to freeze"
    );
}

#[test]
fn a_candidate_somebody_else_stopped_does_not_starve_the_next_heaviest_subtree() {
    let mut table = standard();
    table.sessions[0].members.push(proc(107, 101, "go", 900));
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    // The heaviest build (102, 1100 MiB with its child) is a Ctrl-Z job.
    rig.fake.stop_by_hand(102);
    rig.ticks(SEVERE_SAMPLES as usize + 20, rising(100), &table);
    assert_eq!(
        rig.fake.stops(),
        [107],
        "the next heaviest is frozen, not the stopped one again and again"
    );
    assert!(
        rig.fake.is_stopped_now(102),
        "and the job stays its owner's"
    );
}

#[test]
fn what_a_process_that_backgrounded_itself_started_is_not_restored_as_an_inheritor() {
    let mut table = standard();
    // 124 is exempt (never slowed by name) under the cargo; 125 below it put
    // itself in the background.
    table.sessions[0].members.push(ReliefProc {
        exempt: true,
        ..proc(124, 102, "cc1", 50)
    });
    table.sessions[0].members.push(proc(125, 124, "cc1", 50));
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.fake.background.lock().unwrap().insert(125);
    rig.ticks(5, |_| input(Level::Warn, 0), &table);
    assert!(rig.relief.foreign.contains(&(125, 1_000_125)));
    // 126 was born after the pass, below 125, and inherited 125's own choice.
    let mut grown = table.clone();
    grown.sessions[0].members.push(ReliefProc {
        start_us: 9_000_126,
        ..proc(126, 125, "cc1", 50)
    });
    live_all(&rig, &grown);
    rig.fake.background.lock().unwrap().insert(126);
    rig.ticks(
        EXIT_SAMPLES as usize - 1,
        |_| input(Level::Normal, 0),
        &table,
    );
    rig.tick(&input(Level::Normal, 0), &grown);
    let restored = rig.fake.restored();
    assert!(restored.contains(&102), "{restored:?}");
    assert!(
        !restored.contains(&125) && !restored.contains(&126),
        "{restored:?}"
    );
}

#[test]
fn a_sweep_seeds_only_from_the_very_process_a_journal_entry_names() {
    let dir = tempfile::tempdir().unwrap();
    write_journal(
        dir.path(),
        &[JournalEntry {
            pid: 102,
            start_us: 1_000_102,
            session_id: "100-1".into(),
            at_unix_ms: 1_000,
            rung: JournalRung::Slow,
            was_running: true,
        }],
    )
    .unwrap();
    let fake = Fake::default();
    // After a crash and a reboot, pid 102 is an unrelated process that started
    // much later, and its child is in the background for reasons of its own.
    *fake.table.lock().unwrap() = vec![(102, 1, 7_777_777_000), (120, 102, 9_000_000_000)];
    fake.background.lock().unwrap().insert(120);
    let replay = replay_journal(dir.path(), dir.path(), &fake);
    assert!(
        !fake.restored().contains(&120),
        "an unrelated process's child was restored: {replay:?}"
    );
}

#[test]
fn a_late_joiner_somebody_stopped_while_the_journal_was_written_is_left_alone() {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    let mut grown = table.clone();
    grown.sessions[0].members.push(proc(120, 103, "cc1", 50));
    live_all(&rig, &grown);
    // The first read finds 120 running; by the time the journal is written and
    // the signal is next, a person has stopped it.
    rig.fake.flip_on_read.lock().unwrap().insert(120, 2);
    rig.tick(&severe(9000), &grown);
    assert_eq!(rig.fake.stops(), [102, 103], "120 was somebody else's");
    assert!(rig.relief.frozen[0].left.contains(&(120, 1_000_120)));
    assert!(
        !rig.relief
            .journal
            .iter()
            .any(|e| e.pid == 120 && e.rung == JournalRung::Freeze),
        "its entry is trimmed"
    );
    // Ending the freeze does not continue it.
    rig.ticks(FLAT_SAMPLES, |_| severe(5000), &grown);
    assert!(rig.fake.is_stopped_now(120));
    assert!(!rig.fake.conts().contains(&120));
}

#[cfg(unix)]
mod journal_trim {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The journal's own directory, so its atomic write cannot create a file.
    fn read_only(dir: &Path, on: bool) {
        let mode = if on { 0o500 } else { 0o700 };
        std::fs::set_permissions(dir.join(JOURNAL_DIR), std::fs::Permissions::from_mode(mode))
            .unwrap();
    }

    /// A rig frozen on `standard()` whose relief has taken 103 out of its own
    /// record (somebody else stopped it) and whose next journal write fails.
    fn failed_trim() -> Rig {
        let table = standard();
        let mut rig = Rig::new(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert_eq!(rig.fake.stops(), [102, 103]);
        rig.relief.journal.retain(|e| e.pid != 103);
        read_only(rig.dir.path(), true);
        assert!(rig.relief.sync_journal().is_err());
        rig
    }

    #[test]
    fn an_entry_whose_trim_failed_is_never_continued_by_a_replay() {
        let rig = failed_trim();
        read_only(rig.dir.path(), false);
        let fake = Fake::default();
        let replay = replay_journal(rig.dir.path(), &rig.ns(), &fake);
        assert_eq!(
            fake.conts(),
            [102],
            "103 is not ours, whatever the file still lists: {replay:?}"
        );
    }

    #[test]
    fn a_failed_trim_is_retried_on_the_next_tick() {
        let mut rig = failed_trim();
        assert!(rig.relief.journal_stale);
        read_only(rig.dir.path(), false);
        let table = standard();
        rig.tick(&severe(9000), &table);
        assert!(!rig.relief.journal_stale);
        let listed: Vec<i32> = read_journal(rig.dir.path())
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.rung == JournalRung::Freeze)
            .map(|e| e.pid)
            .collect();
        assert_eq!(listed, [102], "the file no longer lists 103");
        assert!(not_ours(rig.dir.path()).is_empty());
    }
}

#[test]
fn an_unreadable_sample_neither_advances_nor_resets_the_episode_end() {
    let mut trigger = Trigger::default();
    let mut now = Instant::now();
    // A freeze that rested on pinned alone spends the episode's allowance.
    for _ in 0..SEVERE_SAMPLES {
        now += STEP;
        if trigger.observe(now, &critical_sample(2048)).freeze == Step::Begin {
            trigger.began(now, true);
        }
    }
    assert!(trigger.frozen() && trigger.pinned_spent);
    let episode = trigger.episode();
    // A blind sensor never holds a freeze (it ends, as it always did), but a
    // blind spell far longer than the five samples that end an episode does
    // not end the *episode*: nothing it spent is forgiven.
    for _ in 0..8 * SEVERE_BREAK_SAMPLES {
        now += STEP;
        trigger.observe(now, &Sample::default());
    }
    assert!(!trigger.frozen(), "the freeze itself was let go");
    assert_eq!(trigger.episode(), episode, "but the episode went on");
    assert!(trigger.pinned_spent, "and what it spent is still spent");
    // Readable calm samples still end it, counted from where they begin.
    for _ in 0..SEVERE_BREAK_SAMPLES {
        now += STEP;
        trigger.observe(
            now,
            &Sample {
                tier: Some((Tier::Clear, None)),
                ..Sample::default()
            },
        );
    }
    assert!(!trigger.pinned_spent);
    assert_ne!(trigger.episode(), episode);
}

/// A freeze, a resume on flat swap, then a second rise: `grow` MiB heavier
/// below the same cargo.
fn second_rise(grow_mib: u64) -> Vec<i32> {
    let table = standard();
    let mut rig = Rig::new(SupervisorMode::Act);
    live_all(&rig, &table);
    rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
    assert_eq!(rig.fake.stops(), [102, 103]);
    rig.ticks(FLAT_SAMPLES + 1, |_| severe(5000), &table);
    assert_eq!(rig.fake.conts(), [103, 102], "resumed on flat swap");
    let mut grown = table.clone();
    for m in &mut grown.sessions[0].members {
        if m.pid == 103 {
            m.footprint = m.footprint.map(|f| f + grow_mib * MIB);
        }
    }
    live_all(&rig, &grown);
    rig.ticks(SEVERE_SAMPLES as usize + 2, rising(100), &grown);
    rig.fake.stops()
}

#[test]
fn a_spent_build_that_has_itself_grown_a_gibibyte_is_frozen_again_on_a_fresh_rise() {
    assert_eq!(
        second_rise(1536),
        [102, 103, 102, 103],
        "the build that drives the second rise is the one frozen again"
    );
}

#[test]
fn a_spent_build_that_has_not_grown_enough_stays_spent() {
    assert_eq!(
        second_rise(512),
        [102, 103],
        "512 MiB is under the 1 GiB it has to grow by"
    );
}
