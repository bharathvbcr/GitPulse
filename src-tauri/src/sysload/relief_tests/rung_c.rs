// Rung C (ending a paused tree) and offers: a child of `relief_tests`, so it
// shares that file's rigs and fakes.

use super::*;

mod ending {
    use super::*;

    fn opted_in(rig: &mut Rig) {
        rig.relief.terminate_opt_in = Box::new(|| true);
    }

    fn frozen_standard(rig: &mut Rig, table: &ReliefTable) {
        live_all(rig, table);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), table);
        assert_eq!(rig.fake.stops(), [102, 103]);
    }

    /// Swap keeps climbing, so only the cap can end the freeze.
    fn to_the_cap(rig: &mut Rig, table: &ReliefTable) {
        for n in 0..400usize {
            rig.tick(&severe(9000 + 100 * n as u64), table);
            let moved = rig
                .fake
                .calls()
                .iter()
                .any(|c| matches!(c, Call::Cont(_) | Call::Term(_)));
            if moved {
                return;
            }
        }
        panic!("the cap never came");
    }

    fn after_freeze(rig: &Rig) -> Vec<Call> {
        rig.fake
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Cont(_) | Call::Term(_) | Call::Kill(_)))
            .collect()
    }

    fn no_freeze_entries(rig: &Rig) -> bool {
        read_journal(rig.dir.path())
            .unwrap()
            .is_none_or(|j| j.entries.iter().all(|e| e.rung != JournalRung::Freeze))
    }

    #[test]
    fn opted_in_the_cap_continues_then_terminates_then_kills_survivors_and_never_the_agent_or_shell(
    ) {
        let table = standard();
        let mut rig = Rig::new(SupervisorMode::Act);
        opted_in(&mut rig);
        frozen_standard(&mut rig, &table);
        to_the_cap(&mut rig, &table);
        assert_eq!(
            after_freeze(&rig),
            [
                Call::Cont(103),
                Call::Cont(102),
                Call::Term(102),
                Call::Term(103)
            ],
            "continued first (a signal to a stopped process waits), then asked to end"
        );
        let last = rig.relief.status.last_action.clone().unwrap();
        assert_eq!(last.kind, ActionKind::Terminate, "{last:?}");
        assert_eq!(rig.relief.status.cap_hits, 1);
        assert!(rig.relief.status.frozen.is_empty());
        assert_eq!(rig.relief.status.ended.len(), 1);
        assert_eq!(rig.relief.status.ended[0].command, "cargo");
        // The grace is measured on the tick and never waited for: nothing is
        // killed before it has run out.
        for _ in 0..(TERMINATE_GRACE.as_secs() / STEP.as_secs() - 1) {
            rig.tick(&severe(20_000), &table);
            assert!(
                !after_freeze(&rig)
                    .iter()
                    .any(|c| matches!(c, Call::Kill(_))),
                "killed inside the grace"
            );
        }
        rig.tick(&severe(20_000), &table);
        let calls = after_freeze(&rig);
        assert_eq!(
            &calls[4..],
            [Call::Kill(102), Call::Kill(103)],
            "survivors are killed once the grace is over"
        );
        for pid in [100, 101] {
            assert!(
                !rig.fake.calls().iter().any(|c| matches!(
                    c,
                    Call::Stop(p) | Call::Cont(p) | Call::Term(p) | Call::Kill(p) if *p == pid
                )),
                "pid {pid} (the agent / its tool shell) is never signalled"
            );
        }
        assert!(
            no_freeze_entries(&rig),
            "the journal is clear once it is over"
        );
    }

    #[test]
    fn not_opted_in_the_cap_still_only_resumes() {
        let table = standard();
        let mut rig = Rig::new(SupervisorMode::Act);
        frozen_standard(&mut rig, &table);
        to_the_cap(&mut rig, &table);
        rig.ticks(10, |_| severe(20_000), &table);
        assert_eq!(after_freeze(&rig), [Call::Cont(103), Call::Cont(102)]);
        let last = rig.relief.status.last_action.clone().unwrap();
        assert_eq!(last.kind, ActionKind::CapResume, "{last:?}");
        assert!(rig.relief.status.ended.is_empty());
    }

    #[test]
    fn a_process_somebody_else_stopped_is_never_continued_or_terminated() {
        let table = standard();
        let mut rig = Rig::new(SupervisorMode::Act);
        opted_in(&mut rig);
        live_all(&rig, &table);
        rig.fake.stop_by_hand(103);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert_eq!(rig.fake.stops(), [102], "103 was somebody else's");
        to_the_cap(&mut rig, &table);
        rig.ticks(10, |_| severe(20_000), &table);
        let calls = after_freeze(&rig);
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, Call::Cont(103) | Call::Term(103) | Call::Kill(103))),
            "{calls:?}"
        );
        assert!(rig.fake.is_stopped_now(103));
    }

    #[test]
    fn a_root_that_somebody_continued_is_not_ended() {
        let table = standard();
        let mut rig = Rig::new(SupervisorMode::Act);
        opted_in(&mut rig);
        frozen_standard(&mut rig, &table);
        // Continued by hand: not frozen by us any more.
        rig.fake.stopped.lock().unwrap().remove(&102);
        to_the_cap(&mut rig, &table);
        assert!(
            !after_freeze(&rig)
                .iter()
                .any(|c| matches!(c, Call::Term(_) | Call::Kill(_))),
            "{:?}",
            after_freeze(&rig)
        );
    }

    fn request(rig: &Rig, pid: u32, start: u64, at: u64) {
        file::request_target_in(
            rig.dir.path(),
            file::TargetKind::Terminate,
            "health",
            pid,
            start,
            at,
        )
        .unwrap();
    }

    #[test]
    fn a_terminate_request_ends_only_the_named_frozen_tree_and_needs_no_opt_in() {
        let table = two_sessions();
        let mut rig = publishing_rig(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert_eq!(rig.relief.frozen.len(), 2);
        // A tree that is not frozen, a start time that is not the frozen
        // process's, and an expired request: all ignored.
        request(&rig, 999, 1_000_999, file::now_ms());
        rig.tick(&severe(9000), &table);
        request(&rig, 202, 1_000_202 + 1, file::now_ms());
        rig.tick(&severe(9000), &table);
        request(
            &rig,
            202,
            1_000_202,
            file::now_ms() - file::REQUEST_TTL_MS - 5_000,
        );
        rig.tick(&severe(9000), &table);
        assert_eq!(after_freeze(&rig), [], "nothing was signalled");
        assert_eq!(rig.relief.frozen.len(), 2);
        assert_eq!(
            rig.relief.status.last_action.as_ref().unwrap().kind,
            ActionKind::Refused
        );
        // The named one.
        request(&rig, 202, 1_000_202, file::now_ms());
        rig.tick(&severe(9000), &table);
        assert_eq!(
            after_freeze(&rig),
            [Call::Cont(202), Call::Term(202)],
            "only the named tree; the other session's stays paused"
        );
        assert_eq!(rig.relief.frozen.len(), 1);
        assert_eq!(rig.relief.frozen[0].root.0, 212);
        assert_eq!(
            rig.relief.status.last_action.as_ref().unwrap().kind,
            ActionKind::Terminate
        );
        assert!(rig.fake.is_stopped_now(212));
        assert!(!rig
            .dir
            .path()
            .join(file::TERMINATE_REQUEST_FILE_NAME)
            .exists());
        // The published file names what was ended, for the hook.
        let status = published(&rig);
        assert_eq!(status.ended.len(), 1);
        assert_eq!(status.ended[0].pid, 202);
        assert_eq!(status.paused_count(), 1);
    }

    #[test]
    fn a_resume_during_the_grace_cancels_the_kill() {
        let table = standard();
        let mut rig = publishing_rig(SupervisorMode::Act);
        opted_in(&mut rig);
        frozen_standard(&mut rig, &table);
        to_the_cap(&mut rig, &table);
        file::request_resume_in(rig.dir.path(), "banner", file::now_ms()).unwrap();
        rig.ticks(10, |_| severe(20_000), &table);
        assert!(
            !after_freeze(&rig)
                .iter()
                .any(|c| matches!(c, Call::Kill(_))),
            "a person's resume ends the pending kill: {:?}",
            after_freeze(&rig)
        );
    }

    fn observed() -> ReliefTable {
        let mut table = standard();
        table.sessions[0].launched = false;
        table
    }

    fn pause_request(rig: &Rig, pid: u32, start: u64) {
        file::request_target_in(
            rig.dir.path(),
            file::TargetKind::Pause,
            "banner",
            pid,
            start,
            file::now_ms(),
        )
        .unwrap();
    }

    #[test]
    fn an_observed_sessions_heaviest_tree_is_an_offer_never_frozen_and_a_pause_request_freezes_it()
    {
        let table = observed();
        let mut rig = publishing_rig(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
        assert!(rig.fake.stops().is_empty(), "never frozen automatically");
        let status = published(&rig);
        assert_eq!(status.rung, file::ReliefRung::Slow);
        assert_eq!(status.offers.len(), 1, "{:?}", status.offers);
        let offer = &status.offers[0];
        assert_eq!((offer.pid, offer.start_us), (102, 1_000_102));
        assert!(offer.advised_only);
        assert_eq!(offer.why, Some(file::OfferWhy::Observed));
        assert_eq!(status.paused_count(), 0);

        pause_request(&rig, 102, 1_000_102);
        rig.tick(&severe(9000), &table);
        assert_eq!(rig.fake.stops(), [102, 103]);
        let journal = read_journal(rig.dir.path()).unwrap().unwrap();
        let entries: Vec<_> = journal
            .entries
            .iter()
            .filter(|e| e.rung == JournalRung::Freeze)
            .collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.was_running));
        let status = published(&rig);
        assert_eq!(status.paused_count(), 1);
        assert!(status.offers.is_empty());
        // It follows every normal resume rule: pressure easing resumes it.
        rig.ticks(
            SEVERE_BREAK_SAMPLES as usize,
            |_| input(Level::Normal, 0),
            &table,
        );
        assert_eq!(rig.fake.conts(), [103, 102]);
    }

    #[test]
    fn a_pause_request_for_something_that_was_not_offered_is_ignored() {
        let table = observed();
        let mut rig = publishing_rig(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
        // The agent's own shell was never offered, and neither was a stranger.
        pause_request(&rig, 101, 1_000_101);
        rig.tick(&severe(9000), &table);
        pause_request(&rig, 4242, 1_004_242);
        rig.tick(&severe(9000), &table);
        assert!(rig.fake.stops().is_empty());
        assert_eq!(
            rig.relief.status.last_action.as_ref().unwrap().kind,
            ActionKind::Refused
        );
    }

    #[test]
    fn an_untrusted_offer_is_advice_only_and_has_no_pause_path() {
        let table = standard();
        let mut rig = publishing_rig(SupervisorMode::Act);
        *rig.trusted.lock().unwrap() = false;
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
        let status = published(&rig);
        assert_eq!(status.offers.len(), 1);
        assert_eq!(status.offers[0].why, Some(file::OfferWhy::Untrusted));
        pause_request(&rig, 102, 1_000_102);
        rig.tick(&severe(9000), &table);
        assert!(
            rig.fake.stops().is_empty(),
            "no pause for an untrusted tree"
        );
        assert_eq!(
            rig.relief.status.last_action.as_ref().unwrap().kind,
            ActionKind::Refused
        );
        // Even once the repository is trusted, the offer was an untrusted one.
        *rig.trusted.lock().unwrap() = true;
        pause_request(&rig, 102, 1_000_102);
        rig.tick(&severe(9000), &table);
        assert!(rig.fake.stops().is_empty());
    }

    #[test]
    fn a_pause_request_respects_the_host_limit() {
        let table = observed();
        let mut rig = publishing_rig(SupervisorMode::Act);
        live_all(&rig, &table);
        rig.ticks(SEVERE_SAMPLES as usize + 5, rising(100), &table);
        rig.relief.frozen = (0..MAX_FROZEN_PER_HOST)
            .map(|n| Frozen {
                info: FrozenSubtree {
                    session_pid: 900 + n as i32,
                    advised_only: false,
                    why: None,
                    ..rig.relief.offers[0].info.clone()
                },
                session: (900 + n as i32, 1),
                root: (950 + n as i32, 1),
                members: Vec::new(),
                since: rig.now,
                holder: false,
                left: BTreeSet::new(),
            })
            .collect();
        pause_request(&rig, 102, 1_000_102);
        rig.tick(&severe(9000), &table);
        assert!(rig.fake.stops().is_empty());
        rig.relief.frozen.clear();
    }
}

#[cfg(unix)]
mod rung_c_real {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::io::BufRead;

    /// Records each signal the relief sends, in order, and delivers it for real.
    #[derive(Clone, Default)]
    struct Recording {
        log: Arc<Mutex<Vec<(i32, &'static str)>>>,
    }

    impl Actuator for Recording {
        fn set_background(&self, pid: i32, start_us: u64, enable: bool) -> Result<bool, String> {
            OsActuator.set_background(pid, start_us, enable)
        }
        fn signal(&self, pid: i32, start_us: u64, signal: Signal) -> Result<Delivery, String> {
            let name = match signal {
                Signal::Stop => "STOP",
                Signal::Cont => "CONT",
                Signal::Term => "TERM",
                Signal::Kill => "KILL",
            };
            self.log.lock().unwrap().push((pid, name));
            OsActuator.signal(pid, start_us, signal)
        }
        fn is_background(&self, pid: i32, start_us: u64) -> Option<bool> {
            OsActuator.is_background(pid, start_us)
        }
        fn is_stopped(&self, pid: i32, start_us: u64) -> Result<bool, String> {
            OsActuator.is_stopped(pid, start_us)
        }
        fn process_table(&self) -> Vec<(i32, i32, u64)> {
            OsActuator.process_table()
        }
    }

    /// A build that ignores SIGTERM: the trap is set (and says so) before the
    /// test freezes it, and `exec` leaves no orphan behind.
    struct Stubborn(std::process::Child);

    impl Stubborn {
        fn spawn() -> Self {
            let mut child = std::process::Command::new("sh")
                .args(["-c", "trap '' TERM; echo ready; exec sleep 60"])
                .stdout(std::process::Stdio::piped())
                .spawn_locked()
                .expect("sh");
            let out = child.stdout.take().expect("stdout");
            let mut line = String::new();
            std::io::BufReader::new(out).read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "ready");
            Self(child)
        }
        fn pid(&self) -> i32 {
            i32::try_from(self.0.id()).unwrap()
        }
        fn start_us(&self) -> u64 {
            crate::sysload::process_start_us(self.0.id()).expect("a start time")
        }
    }

    impl Drop for Stubborn {
        fn drop(&mut self) {
            // SAFETY: signals the child this test started, before reaping it.
            unsafe { libc::kill(self.0.id() as i32, libc::SIGCONT) };
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_real_child_that_ignores_sigterm_is_continued_terminated_then_killed_in_that_order() {
        let mut build = Stubborn::spawn();
        let table = ReliefTable {
            sessions: vec![ReliefSession {
                members: vec![
                    proc(8_400_101, 8_400_100, "zsh", 10),
                    ReliefProc {
                        pid: build.pid(),
                        ppid: 8_400_101,
                        start_us: build.start_us(),
                        name: "sh".into(),
                        footprint: Some(900 * MIB),
                        exempt: false,
                    },
                ],
                ..session(8_400_100, true, Vec::new())
            }],
        };
        let mut rig = Rig::new(SupervisorMode::Act);
        let recording = Recording::default();
        rig.relief.actuator = Box::new(recording.clone());
        rig.relief.terminate_opt_in = Box::new(|| true);
        rig.ticks(SEVERE_SAMPLES as usize, rising(100), &table);
        assert_eq!(
            recording.log.lock().unwrap().as_slice(),
            [(build.pid(), "STOP")]
        );
        for n in 0..400usize {
            rig.tick(&severe(9000 + 100 * n as u64), &table);
            if recording.log.lock().unwrap().len() > 1 {
                break;
            }
        }
        // Ignoring TERM, it is still there after the grace has begun.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            build.0.try_wait().unwrap().is_none(),
            "TERM alone does not end it"
        );
        for _ in 0..(TERMINATE_GRACE.as_secs() / STEP.as_secs()) {
            rig.tick(&severe(20_000), &table);
        }
        let status = build.0.wait().expect("it was killed");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(
            recording.log.lock().unwrap().as_slice(),
            [
                (build.pid(), "STOP"),
                (build.pid(), "CONT"),
                (build.pid(), "TERM"),
                (build.pid(), "KILL")
            ]
        );
    }
}
