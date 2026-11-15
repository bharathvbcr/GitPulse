use super::*;
use crate::sysload::Binding;

fn verdict(tier: Tier, admit: bool) -> Load {
    Load::Verdict(PublishedVerdict {
        tier,
        binding: Some(Binding::Swap),
        admit,
        shed_background: tier >= Tier::Elevated,
        shed_reactive: tier >= Tier::Severe,
    })
}

fn record(pid: i32, session: &str, priority: Priority, enqueued: u64) -> LeaseRecord {
    LeaseRecord {
        pid,
        start_us: 1,
        session_key: session.into(),
        priority,
        terminal_session: None,
        run_id: None,
        cwd: "/work".into(),
        label: "cargo build".into(),
        enqueued_mono_ns: enqueued,
        granted_mono_ns: None,
        tier_at_grant: None,
        verdict_unavailable_reason: None,
        overflow: false,
    }
}

fn waiter(pid: i32, session: &str, priority: Priority, enqueued: u64) -> LeaseWaiter {
    LeaseWaiter {
        name: format!("{enqueued}-{pid}.json"),
        record: record(pid, session, priority, enqueued),
    }
}

fn holder(slot: usize, pid: i32, session: &str) -> LeaseHolder {
    LeaseHolder {
        slot,
        pid,
        record: Some(record(pid, session, Priority::Observed, 1)),
    }
}

fn pids(waiters: &[LeaseWaiter]) -> Vec<i32> {
    waiters.iter().map(|w| w.record.pid).collect()
}

#[test]
fn elevated_halves_slots_severe_grants_none() {
    let clear = capacity(4, &verdict(Tier::Clear, true));
    assert_eq!((clear.slots, clear.refuses, clear.note), (4, false, None));

    let elevated = capacity(4, &verdict(Tier::Elevated, true));
    assert_eq!(elevated.slots, 2);
    assert!(!elevated.refuses);
    assert!(elevated.note.unwrap().contains("halved from 4"));
    // Never below one: an elevated host still builds, one at a time.
    assert_eq!(capacity(1, &verdict(Tier::Elevated, true)).slots, 1);
    assert_eq!(capacity(3, &verdict(Tier::Elevated, true)).slots, 1);

    let severe = capacity(4, &verdict(Tier::Severe, true));
    assert_eq!((severe.slots, severe.refuses), (0, true));
    // Launches not admitted is the same refusal at any tier.
    let refused = capacity(4, &verdict(Tier::Clear, false));
    assert_eq!((refused.slots, refused.refuses), (0, true));
    assert!(refused.note.unwrap().contains("not admitting"));
}

#[test]
fn no_verdict_keeps_the_setting_and_says_why() {
    let unknown = capacity(4, &Load::Unknown("the sampler last wrote 90s ago".into()));
    assert_eq!(unknown.slots, 4, "the setting applies");
    assert!(!unknown.refuses, "a missing reading is never a refusal");
    assert_eq!(unknown.tier, None);
    assert_eq!(
        unknown.note.as_deref(),
        Some("slots not lowered for load: the sampler last wrote 90s ago")
    );
    // The setting is still bounded.
    assert_eq!(capacity(0, &Load::Unknown(String::new())).slots, 1);
    assert_eq!(capacity(99, &Load::Unknown(String::new())).slots, MAX_SLOTS);
}

#[test]
fn a_session_holding_a_slot_ranks_behind_one_holding_none() {
    // `busy` queued first and holds a slot already; `idle` queued later and
    // holds none. The later one is served first.
    let holders = [holder(1, 100, "agent:busy")];
    let mut waiters = vec![
        waiter(201, "agent:busy", Priority::Observed, 10),
        waiter(202, "agent:idle", Priority::Observed, 20),
    ];
    order(&mut waiters, &holders);
    assert_eq!(pids(&waiters), [202, 201]);
    // Without the holder it is plain first come, first served.
    order(&mut waiters, &[]);
    assert_eq!(pids(&waiters), [201, 202]);
}

#[test]
fn the_queue_is_first_come_first_served_within_a_priority_and_terminals_go_first() {
    let mut waiters = vec![
        waiter(3, "run:a", Priority::Run, 5),
        waiter(1, "agent:x", Priority::Observed, 1),
        waiter(4, "terminal:t", Priority::Terminal, 9),
        waiter(2, "agent:y", Priority::Observed, 2),
        waiter(5, "terminal:u", Priority::Terminal, 7),
    ];
    order(&mut waiters, &[]);
    assert_eq!(pids(&waiters), [5, 4, 3, 1, 2]);
}

#[test]
fn only_as_many_waiters_as_there_are_free_slots_may_take_one() {
    let cap = capacity(3, &verdict(Tier::Clear, true));
    // Three slots, one held: the first two waiters may go, the third may not.
    assert!(may_take(0, 1, &cap));
    assert!(may_take(1, 1, &cap));
    assert!(!may_take(2, 1, &cap));
    // All held: nobody.
    assert!(!may_take(0, 3, &cap));
    // More held than the lowered count allows: nobody, and no underflow.
    let lowered = capacity(4, &verdict(Tier::Elevated, true));
    assert!(!may_take(0, 3, &lowered));
    let severe = capacity(4, &verdict(Tier::Severe, true));
    assert!(!may_take(0, 0, &severe));
}

#[test]
fn a_session_is_named_by_its_terminal_then_its_run_then_its_agent_then_its_parent() {
    let parent = (77, 5);
    assert_eq!(
        session_of(Some("term-1"), Some("run-1"), Some((9, 9)), parent),
        ("terminal:term-1".into(), Priority::Terminal)
    );
    assert_eq!(
        session_of(None, Some("run-1"), Some((9, 9)), parent),
        ("run:run-1".into(), Priority::Run)
    );
    assert_eq!(
        session_of(None, None, Some((9, 8)), parent),
        ("agent:9-8".into(), Priority::Observed)
    );
    assert_eq!(
        session_of(None, None, None, parent),
        ("process:77-5".into(), Priority::Observed)
    );
    // A value that is not a plain token names nothing.
    assert_eq!(
        session_of(Some("bad id"), Some(""), None, parent).0,
        "process:77-5"
    );
}

#[test]
fn the_lease_folder_leads_the_agent_path_once() {
    use std::ffi::OsStr;
    let bin = Path::new("/c/lease-bin");
    let joined = |p: &std::ffi::OsString| {
        std::env::split_paths(p)
            .map(|e| e.display().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        joined(&path_with_bin_first(bin, Some(OsStr::new("/usr/bin:/bin")))),
        ["/c/lease-bin", "/usr/bin", "/bin"]
    );
    // Already there, but not first (a login shell put the shim ahead of it):
    // moved to the front, not repeated.
    assert_eq!(
        joined(&path_with_bin_first(
            bin,
            Some(OsStr::new("/home/x/.claude-bin:/c/lease-bin:/bin"))
        )),
        ["/c/lease-bin", "/home/x/.claude-bin", "/bin"]
    );
    assert_eq!(joined(&path_with_bin_first(bin, None)), ["/c/lease-bin"]);
}

#[cfg(unix)]
#[test]
fn a_launched_session_is_pointed_at_the_links_only_when_they_exist_and_leases_are_on() {
    use std::ffi::OsStr;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path();
    let names = |v: &[(&'static str, Option<std::ffi::OsString>)]| {
        v.iter().map(|(n, _)| *n).collect::<Vec<_>>()
    };
    // No links yet: nothing is added, and a stale marker is removed.
    let none = launch_env(Some(config), Some(OsStr::new("/bin")), true);
    assert_eq!(none, [("GITPULSE_LEASE_BIN", None)]);
    let hook = temp.path().join("gitpulse-hook");
    std::fs::write(&hook, "x").unwrap();
    ensure_links(config, &hook).unwrap();
    let env = launch_env(Some(config), Some(OsStr::new("/bin")), true);
    assert_eq!(names(&env), ["PATH", "GITPULSE_LEASE_BIN"]);
    let path = env[0].1.clone().unwrap();
    assert_eq!(
        std::env::split_paths(&path).next().unwrap(),
        bin_dir(config)
    );
    // Switched off, or no config directory: nothing is promised.
    assert_eq!(
        launch_env(Some(config), None, false),
        [("GITPULSE_LEASE_BIN", None)]
    );
    assert_eq!(launch_env(None, None, true), [("GITPULSE_LEASE_BIN", None)]);
}

#[cfg(unix)]
#[test]
fn the_links_are_made_repaired_by_rename_and_refused_without_a_hook_to_point_at() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path();
    let hook = temp.path().join("gitpulse-hook");
    let missing = ensure_links(config, &hook).unwrap_err();
    assert!(missing.contains("does not exist"), "{missing}");
    std::fs::write(&hook, "x").unwrap();
    ensure_links(config, &hook).unwrap();
    let cargo = bin_dir(config).join("cargo");
    assert_eq!(std::fs::read_link(&cargo).unwrap(), hook);
    assert_eq!(
        std::fs::read_link(bin_dir(config).join("go")).unwrap(),
        hook
    );
    // A link to an older install, and a plain file where a link belongs.
    std::fs::remove_file(&cargo).unwrap();
    std::os::unix::fs::symlink("/old/gitpulse-hook", &cargo).unwrap();
    let go = bin_dir(config).join("go");
    std::fs::remove_file(&go).unwrap();
    std::fs::write(&go, "not a link").unwrap();
    ensure_links(config, &hook).unwrap();
    assert_eq!(std::fs::read_link(&cargo).unwrap(), hook);
    assert_eq!(std::fs::read_link(&go).unwrap(), hook);
    let leftovers = std::fs::read_dir(bin_dir(config)).unwrap().count();
    assert_eq!(leftovers, 2, "no temporary link is left behind");
}

#[cfg(unix)]
mod disk {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};

    const HOLD_ENV: &str = "GITPULSE_LEASE_TEST_HOLD";

    /// The body of the process that holds a slot or a ticket for a test: a
    /// record lock is invisible to its own process, so another one must hold
    /// it. Under a normal test run (no [`HOLD_ENV`]) it returns at once.
    #[test]
    fn hold_a_slot_or_a_ticket_process() {
        let Ok(spec) = std::env::var(HOLD_ENV) else {
            return;
        };
        let (root, what) = spec.split_once('|').unwrap();
        let dir = Dir::open(std::path::Path::new(root)).unwrap();
        let me = i32::try_from(std::process::id()).unwrap();
        let rec = record(me, "agent:held", Priority::Observed, 1);
        let held: Box<dyn std::any::Any> = match what {
            "ticket" => Box::new(dir.enqueue(&rec).unwrap()),
            "bare-slot" => {
                // A lock with no record beside it.
                use std::os::fd::AsRawFd;
                let file =
                    crate::fs_entry::open_private_record(&dir.root().join("slot-2")).unwrap();
                // SAFETY: plain data, valid zeroed.
                let mut lock: libc::flock = unsafe { std::mem::zeroed() };
                lock.l_type = libc::F_WRLCK as _;
                lock.l_whence = libc::SEEK_SET as _;
                lock.l_len = 1;
                // SAFETY: an open descriptor and a valid `flock`.
                assert_eq!(
                    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) },
                    0
                );
                Box::new(file)
            }
            slot => Box::new(
                dir.take(slot.parse().unwrap(), &rec)
                    .unwrap()
                    .expect("the slot is free"),
            ),
        };
        println!("held");
        std::io::stdout().flush().unwrap();
        // Until the parent closes our stdin or kills us.
        let mut sink = String::new();
        let _ = std::io::stdin().read_line(&mut sink);
        drop(held);
    }

    struct Holding {
        child: Child,
    }

    impl Holding {
        fn start(root: &std::path::Path, what: &str) -> Self {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sysload::lease::tests::disk::hold_a_slot_or_a_ticket_process",
                    "--nocapture",
                    "--test-threads=1",
                    "-q",
                ])
                .env(HOLD_ENV, format!("{}|{what}", root.display()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut lines = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                assert!(
                    lines.read_line(&mut line).unwrap() > 0,
                    "the holder exited before it held"
                );
                if line.trim() == "held" {
                    break;
                }
            }
            Self { child }
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.child.id()).unwrap()
        }

        fn kill(mut self) {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }

    fn dir() -> (tempfile::TempDir, Dir) {
        let temp = tempfile::tempdir().unwrap();
        let dir = Dir::open(&temp.path().join("leases")).unwrap();
        (temp, dir)
    }

    #[test]
    fn a_record_without_a_lock_is_not_a_holder() {
        let (_temp, dir) = dir();
        // The slot file exists and so does a record naming a build, but no
        // process holds the lock: the build died, or this is a leftover.
        crate::fs_entry::open_private_record(&dir.root().join("slot-1")).unwrap();
        std::fs::write(
            dir.root().join("slot-1.json"),
            serde_json::to_vec(&record(999_999, "agent:gone", Priority::Observed, 1)).unwrap(),
        )
        .unwrap();
        let board = dir.board(&Own::default());
        assert!(board.holders.is_empty(), "{:?}", board.holders);
        assert!(board.unreadable.is_empty());
        assert_eq!(Dir::lowest_free(&board, &Own::default()), Some(1));
    }

    #[test]
    fn a_lock_with_a_record_is_a_holder_and_one_without_is_a_holder_with_no_name() {
        let (_temp, dir) = dir();
        let named = Holding::start(dir.root(), "1");
        let bare = Holding::start(dir.root(), "bare-slot");
        // A record left by an earlier holder of the bare slot names another
        // pid: it is not shown as this holder's.
        std::fs::write(
            dir.root().join("slot-2.json"),
            serde_json::to_vec(&record(31337, "agent:earlier", Priority::Observed, 1)).unwrap(),
        )
        .unwrap();
        let board = dir.board(&Own::default());
        assert_eq!(board.holders.len(), 2, "{:?}", board.holders);
        let one = board.holders.iter().find(|h| h.slot == 1).unwrap();
        assert_eq!(one.pid, named.pid());
        assert_eq!(one.record.as_ref().unwrap().session_key, "agent:held");
        let two = board.holders.iter().find(|h| h.slot == 2).unwrap();
        assert_eq!((two.pid, two.record.as_ref()), (bare.pid(), None));
        assert_eq!(Dir::lowest_free(&board, &Own::default()), Some(3));
        named.kill();
        bare.kill();
        assert!(
            dir.board(&Own::default()).holders.is_empty(),
            "locks die with the holder"
        );
    }

    #[test]
    fn a_dead_waiters_ticket_is_removed() {
        let (_temp, dir) = dir();
        let alive = Holding::start(dir.root(), "ticket");
        let queue = dir.root().join(QUEUE_DIR);
        // Another waiter's ticket nobody holds the lock on.
        let dead = queue.join("00000000000000000005-31337.json");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let dead_file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&dead)
                .unwrap();
            serde_json::to_writer(
                &dead_file,
                &record(31337, "agent:dead", Priority::Observed, 5),
            )
            .unwrap();
        }
        assert!(dead.exists());
        let board = dir.board(&Own::default());
        assert_eq!(board.waiters.len(), 1, "{:?}", board.waiters);
        assert_eq!(board.waiters[0].record.session_key, "agent:held");
        assert!(!dead.exists(), "the dead ticket is deleted by the reader");
        assert!(board.unreadable.is_empty());
        // And when the live waiter is killed its place is freed too.
        alive.kill();
        assert!(dir.board(&Own::default()).waiters.is_empty());
        assert_eq!(std::fs::read_dir(&queue).unwrap().count(), 0);
    }

    #[test]
    fn a_ticket_that_enqueue_made_is_in_the_queue_and_never_probed_by_its_owner() {
        let (_temp, dir) = dir();
        let mut mine = record(std::process::id() as i32, "agent:me", Priority::Observed, 3);
        mine.start_us = 2;
        let ticket = dir.enqueue(&mine).unwrap();
        assert!(dir.still_queued(&ticket));
        // Read as another process would see it, from this one: its own
        // ticket is skipped, which is what keeps the lock.
        let board = dir.board(&ticket.own());
        assert!(board.waiters.is_empty());
        // A reader that did not skip it would find it unlocked (a process's
        // own lock never conflicts with itself) and delete a live ticket.
        let naive = dir.board(&Own::default());
        assert!(
            naive.waiters.is_empty(),
            "own lock is invisible to own probe"
        );
        assert!(!dir.still_queued(&ticket), "so the naive read deleted it");
        ticket.withdraw();
    }

    #[test]
    fn a_directory_another_user_could_write_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("leases");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = Dir::open(&root).unwrap_err();
        assert!(error.contains("not a private directory"), "{error}");
        // A symlink in its place is refused too.
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(Dir::open(&link).is_err());
    }
}
