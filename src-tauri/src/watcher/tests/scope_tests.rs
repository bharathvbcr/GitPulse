//! The narrowed registration, through the real stream and the real loop.
//!
//! A child of `watcher::tests`, so it shares that module's fixtures (`git_init`,
//! the priming and announcement helpers, the counting poll). What is asserted
//! here needs `fseventsd`, so it is macOS only except for the one check that
//! says the scope changes nothing elsewhere.

use super::*;
#[cfg(target_os = "macos")]
use crate::engine::git_reader::FileStatus;

#[cfg(target_os = "macos")]
fn row(path: &str) -> FileStatus {
    FileStatus {
        path: path.to_string(),
        old_path: None,
        status_code: "??".to_string(),
        is_staged: false,
        is_conflicted: false,
        additions: 0,
        deletions: 0,
        staged_additions: None,
        staged_deletions: None,
        unstaged_additions: None,
        unstaged_deletions: None,
        warnings: Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn registration_of(state: &WatcherState, key: &str) -> Arc<debouncer::Registration> {
    state
        .lock_sessions()
        .unwrap()
        .get(key)
        .and_then(|session| session.registration.clone())
        .expect("a live session has a registration")
}

/// Rules as the loop would load them, for a test that drives the registration.
#[cfg(target_os = "macos")]
fn rules_of(root: &Path) -> Arc<IgnoreRules> {
    Arc::new(IgnoreRules::load(root))
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;

    /// A real watch with its loop, on a repository `setup` prepared before the
    /// stream existed.
    struct Live {
        rx: std::sync::mpsc::Receiver<RepoChangedPayload>,
        stop: Arc<StopSignal>,
        root: PathBuf,
        filter: Arc<filter::EventFilter>,
        registration: Arc<debouncer::Registration>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Live {
        fn finish(mut self) {
            self.stop.stop();
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
        }
    }

    fn spawn_scoped(dir: &Path, scope: WatchScope, setup: impl FnOnce(&Path)) -> Live {
        git_init(dir, false);
        let root = dir.canonicalize().unwrap();
        setup(&root);
        let git_dir = resolve_git_dir(&root).expect("git dir");
        let watcher = RepoFileWatcher::watch_repo_scoped(&git_dir, Some(&root), None, scope)
            .expect("watcher");
        let stop = StopSignal::new();
        stop.arm(watcher.waker());
        let filter = watcher.filter();
        let registration = watcher.registration();
        let (thread_stop, session_stop) = (stop.clone(), stop.clone());
        let emit_path = root.to_string_lossy().into_owned();
        let loop_root = root.clone();
        let internal_roots = vec![root.join(".git")];
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = thread::Builder::new()
            .name("watch-scope-test".into())
            .spawn(move || {
                run_watch_loop(
                    &watcher,
                    WatchLoopContext {
                        git_dir: loop_root,
                        internal_roots,
                        emit_path,
                        timer_wakes: Default::default(),
                    },
                    thread_stop,
                    None,
                    session_stop,
                    move |payload| {
                        let _ = tx.send(payload);
                    },
                );
            })
            .expect("spawn loop");
        // The backend installs its stream, and the loop's first pass runs.
        thread::sleep(Duration::from_millis(600));
        Live {
            rx,
            stop,
            root,
            filter,
            registration,
            handle: Some(handle),
        }
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn ignore_target(root: &Path) {
        write(root, ".gitignore", "target/\n");
        write(root, "target/debug/seed.o", "x");
        write(root, "a.txt", "one\n");
    }

    /// Waits until no callback has arrived for two seconds.
    fn quiet_callbacks(filter: &filter::EventFilter) {
        let mut last = u64::MAX;
        let mut since = Instant::now();
        let quiet = crate::test_support::coverage_relaxed(Duration::from_secs(2));
        while since.elapsed() < quiet {
            thread::sleep(Duration::from_millis(200));
            let now = filter.callbacks();
            if now != last {
                last = now;
                since = Instant::now();
            }
        }
    }

    struct StormResult {
        growth: u64,
        sentinel: bool,
        narrowed: bool,
        excluded: Vec<String>,
    }

    /// 10,000 files under an ignored `target/`, then a top-level sentinel.
    fn target_storm(scope: WatchScope) -> StormResult {
        let temp = TempDir::new().unwrap();
        let live = spawn_scoped(temp.path(), scope, ignore_target);
        quiet_callbacks(&live.filter);
        let before = live.filter.callbacks();
        for i in 0..10_000u32 {
            let dir = live.root.join("target").join(format!("d{}", i % 50));
            if i < 50 {
                std::fs::create_dir_all(&dir).unwrap();
            }
            std::fs::write(dir.join(format!("f{i}.o")), "x").unwrap();
        }
        let payload = write_until_announced(
            &live.rx,
            PRIME_DEADLINE,
            "sentinel",
            |name| std::fs::write(live.root.join(name), "x").unwrap(),
            names_probe,
        );
        let sentinel = payload
            .change
            .paths
            .iter()
            .any(|path| path.starts_with("sentinel-"));
        quiet_callbacks(&live.filter);
        let result = StormResult {
            growth: live.filter.callbacks() - before,
            sentinel,
            narrowed: live.registration.narrowed(),
            excluded: live.registration.excluded(),
        };
        live.finish();
        result
    }

    /// FAILS on the code before the scope existed: there the stream covered the
    /// whole root and every one of these writes was a callback.
    #[test]
    fn a_narrowed_watch_receives_nothing_from_a_build_directory_storm() {
        let narrow = target_storm(WatchScope::Narrow);
        eprintln!("target-storm narrow: callbacks grew by {}", narrow.growth);
        assert!(narrow.narrowed, "the stream must leave `target/` out");
        assert_eq!(narrow.excluded, vec!["target".to_string()]);
        assert!(
            narrow.sentinel,
            "the sentinel must be delivered, else the zero below is a dead stream"
        );
        assert!(
            narrow.growth < 50,
            "callbacks grew by {} over a 10,000-file storm in an excluded directory",
            narrow.growth
        );
    }

    /// The control: the same body on the whole-root scope (what every watch did
    /// before) is flooded.
    #[test]
    fn the_same_storm_on_a_whole_scope_watch_reaches_the_callback_thousands_of_times() {
        let whole = target_storm(WatchScope::Whole);
        eprintln!("target-storm whole: callbacks grew by {}", whole.growth);
        assert!(!whole.narrowed);
        assert!(whole.sentinel);
        assert!(
            whole.growth >= 1_000,
            "callbacks grew by only {}; the control must show the flood",
            whole.growth
        );
    }

    #[test]
    fn a_nested_checkout_directory_delivers_nothing_while_its_neighbour_is_watched() {
        let temp = TempDir::new().unwrap();
        let live = spawn_scoped(temp.path(), WatchScope::Narrow, |root| {
            write(root, ".gitignore", ".claude/worktrees/\n");
            write(root, ".claude/settings.json", "{}\n");
            write(root, ".claude/worktrees/w1/.git", "gitdir: /elsewhere\n");
            write(root, ".claude/worktrees/w1/src/x.rs", "x");
        });
        assert!(live.registration.narrowed());
        assert_eq!(
            live.registration.excluded(),
            vec![".claude/worktrees".to_string()]
        );
        quiet_callbacks(&live.filter);
        let before = live.filter.callbacks();
        for i in 0..300u32 {
            write(
                &live.root,
                &format!(".claude/worktrees/w1/src/f{i}.rs"),
                "x",
            );
        }
        thread::sleep(Duration::from_secs(1));
        quiet_callbacks(&live.filter);
        let grown = live.filter.callbacks() - before;
        assert!(grown < 20, "{grown} callbacks from an excluded directory");
        // A registered neighbour inside the split ancestor is still delivered.
        let payload = write_until_announced(
            &live.rx,
            PRIME_DEADLINE,
            "settings",
            |name| write(&live.root, ".claude/settings.json", name),
            |payload, _| {
                payload
                    .change
                    .paths
                    .iter()
                    .any(|path| path == ".claude/settings.json")
            },
        );
        assert!(payload
            .change
            .paths
            .contains(&".claude/settings.json".to_string()));
        live.finish();
    }

    /// A narrowed watch with a session, for the tests that poll.
    fn narrowed_session<T>(
        setup: impl FnOnce(&Path),
        body: impl FnOnce(
            &WatcherState,
            &str,
            &Path,
            &Arc<std::sync::atomic::AtomicUsize>,
            &std::sync::mpsc::Receiver<RepoChangedPayload>,
            &Arc<debouncer::Registration>,
        ) -> T,
    ) -> T {
        watched_repo_counting_status_reads(|state, path, root, reads| {
            setup(root);
            let (tx, rx) = std::sync::mpsc::channel();
            start_watch_observed(
                state,
                path.to_string(),
                move |payload| {
                    let _ = tx.send(payload);
                },
                |_| {},
            )
            .unwrap();
            let registration = registration_of(state, path);
            assert!(registration.narrowed(), "the fixture must narrow");
            wait_for_a_still_epoch(skip_of(state, path).skip());
            body(state, path, root, reads, &rx, &registration)
        })
    }

    #[test]
    fn a_new_top_level_file_is_announced_and_the_next_poll_reads() {
        narrowed_session(
            ignore_target,
            |state, path, root, reads, rx, registration| {
                poll(state, path);
                poll(state, path);
                assert_eq!(reads_of(reads), 1, "the idle poll is served");
                std::fs::write(root.join("brand-new.txt"), "n").unwrap();
                let deadline = Instant::now() + PRIME_DEADLINE;
                loop {
                    let left = deadline
                        .checked_duration_since(Instant::now())
                        .expect("the new file was never announced");
                    let payload = rx
                        .recv_timeout(left)
                        .expect("the new file was never announced");
                    if payload.change.paths.iter().any(|p| p == "brand-new.txt") {
                        break;
                    }
                }
                assert_eq!(registration.reregistrations(), 1);
                let rows = poll(state, path);
                assert_eq!(reads_of(reads), 2, "the announcement moved the epoch");
                assert!(rows.iter().any(|r| r.path == "brand-new.txt"), "{rows:?}");
            },
        );
    }

    #[test]
    fn an_atomic_save_over_a_top_level_file_is_delivered_without_a_re_registration() {
        narrowed_session(
            |root| {
                ignore_target(root);
                git_in(root, &["add", "a.txt", ".gitignore"]);
                git_in(root, &["commit", "-m", "seed"]);
            },
            |_, _, root, _, rx, registration| {
                let before = registration.reregistrations();
                write(root, ".a.txt.tmp", "two\n");
                std::fs::rename(root.join(".a.txt.tmp"), root.join("a.txt")).unwrap();
                let deadline = Instant::now() + PRIME_DEADLINE;
                loop {
                    let left = deadline
                        .checked_duration_since(Instant::now())
                        .expect("the atomic save was never announced");
                    let payload = rx
                        .recv_timeout(left)
                        .expect("the atomic save was never announced");
                    if payload.change.paths.iter().any(|p| p == "a.txt") {
                        break;
                    }
                }
                // The kqueue has had time to look at the directory.
                thread::sleep(Duration::from_secs(2));
                assert_eq!(
                    registration.reregistrations(),
                    before,
                    "a temporary name that came and went is not a change of names"
                );
                // The new inode is still watched by its own path.
                let payload = write_until_announced(
                    rx,
                    PRIME_DEADLINE,
                    "again",
                    |name| std::fs::write(root.join("a.txt"), name).unwrap(),
                    |payload, _| payload.change.paths.iter().any(|p| p == "a.txt"),
                );
                assert!(payload.change.paths.contains(&"a.txt".to_string()));
            },
        );
    }

    #[test]
    fn a_new_top_level_directory_is_covered_once_the_registration_has_moved() {
        narrowed_session(ignore_target, |_, _, root, _, rx, registration| {
            std::fs::create_dir(root.join("fresh")).unwrap();
            wait_for("the registration to take in `fresh`", || {
                registration.reregistrations() >= 1
                    && registration
                        .registered_paths()
                        .iter()
                        .any(|path| path.ends_with("fresh"))
            });
            let payload = write_until_announced(
                rx,
                PRIME_DEADLINE,
                "inside",
                |name| std::fs::write(root.join("fresh").join(name), "x").unwrap(),
                |payload, probe| {
                    payload
                        .change
                        .paths
                        .iter()
                        .any(|path| *path == format!("fresh/{probe}"))
                },
            );
            assert!(payload.change.kinds.contains(&ChangeKind::Worktree));
        });
    }

    #[test]
    fn a_file_force_added_under_an_excluded_directory_is_seen_when_edited() {
        narrowed_session(
            |root| {
                ignore_target(root);
                write(root, "target/x.rs", "one\n");
            },
            |state, path, root, reads, _, registration| {
                assert_eq!(registration.excluded(), vec!["target".to_string()]);
                poll(state, path);
                git_in(root, &["add", "-f", "target/x.rs"]);
                git_in(
                    root,
                    &["commit", "-m", "track a file under an ignored directory"],
                );
                // The index write asks (a) and (b) again, at most once per window.
                wait_for("the revalidation to drop `target`", || {
                    !registration.narrowed()
                });
                wait_for_a_still_epoch(skip_of(state, path).skip());
                poll(state, path);
                poll(state, path);
                let (epoch, before) = (skip_of(state, path).skip().epoch_now(), reads_of(reads));
                std::fs::write(root.join("target/x.rs"), "two\n").unwrap();
                wait_for("the edit to reach the watch", || {
                    skip_of(state, path).skip().epoch_now() != epoch
                });
                let rows = poll(state, path);
                assert_eq!(reads_of(reads), before + 1);
                assert!(
                    rows.iter()
                        .any(|r| r.path == "target/x.rs" && r.status_code.contains('M')),
                    "{rows:?}"
                );
            },
        );
    }

    #[test]
    fn a_status_row_under_an_excluded_directory_widens_the_watch_for_good() {
        narrowed_session(ignore_target, |state, path, root, _, _, registration| {
            let filter = skip_of(state, path);
            let first =
                status_through_watch(state, path, true, || Ok(vec![row("target/x.rs")])).unwrap();
            assert_eq!(first.len(), 1, "the rows are still returned");
            assert!(!registration.narrowed(), "the whole root, at once");
            assert!(
                filter.skip().begin(Instant::now(), true).served.is_none(),
                "that read was not kept"
            );
            // Whole is permanent: a rules reload does not narrow it again.
            registration.apply_rules(&rules_of(root));
            assert!(!registration.narrowed());
            // Only an explicit scope from the caller re-arms it.
            registration.set_scope(WatchScope::Whole);
            registration.set_scope(WatchScope::Narrow);
            assert!(registration.narrowed());
        });
    }

    #[test]
    fn a_convicted_narrowed_watch_goes_whole() {
        narrowed_session(ignore_target, |state, path, _, _, _, registration| {
            let filter = skip_of(state, path);
            // The same silent-stream pattern as the whole-root conviction test,
            // with the disagreeing read supplied by the test.
            filter
                .skip()
                .set_timings(Duration::from_millis(400), Duration::from_millis(200));
            let read = |rows: Vec<FileStatus>| status_through_watch(state, path, true, || Ok(rows));
            read(Vec::new()).unwrap();
            thread::sleep(Duration::from_millis(450));
            assert_eq!(read(vec![row("plain.txt")]).unwrap().len(), 1);
            assert!(registration.narrowed(), "suspect is not convicted");
            thread::sleep(Duration::from_millis(250));
            read(vec![row("plain.txt")]).unwrap();
            assert!(filter.skip().is_convicted(), "the conviction stays");
            assert!(!registration.narrowed(), "and the watch is whole");
        });
    }

    #[test]
    fn a_live_watch_is_re_registered_when_a_different_scope_is_asked_for_and_back() {
        narrowed_session(ignore_target, |state, path, root, _, rx, registration| {
            let git_dir = root.join(".git");
            let again = |scope| {
                start_watch_scoped(state, path.to_string(), scope, |_| {}, |_| {}).unwrap();
            };
            again(None);
            assert!(registration.narrowed(), "no preference changes nothing");
            assert_eq!(registration.reregistrations(), 0);
            again(Some(WatchScope::Narrow));
            assert_eq!(registration.reregistrations(), 0, "the same scope again");
            let epoch = skip_of(state, path).skip().epoch_now();
            again(Some(WatchScope::Whole));
            assert!(
                skip_of(state, path).skip().epoch_now() > epoch,
                "a re-registration moves the poll-skip epoch, so the next poll reads"
            );
            // A root-only event is not a change to announce (see the classifier),
            // so the loop emits nothing here: the poll is what reads.
            let emitted = rx.recv_timeout(Duration::from_millis(1500)).is_ok();
            eprintln!("re-registration to whole: loop emitted a payload = {emitted}");
            assert_eq!(registration.scope(), WatchScope::Whole);
            assert!(!registration.narrowed());
            assert_eq!(registration.reregistrations(), 1);
            assert_eq!(
                registration.registered_paths(),
                vec![git_dir, root.to_path_buf()]
            );
            again(Some(WatchScope::Narrow));
            assert_eq!(registration.scope(), WatchScope::Narrow);
            assert!(registration.narrowed());
            assert_eq!(registration.reregistrations(), 2);
        });
    }

    #[test]
    fn the_whole_scope_registers_exactly_the_git_directory_and_the_root() {
        let temp = TempDir::new().unwrap();
        git_init(temp.path(), false);
        let root = temp.path().canonicalize().unwrap();
        ignore_target(&root);
        let git_dir = root.join(".git");
        let whole = RepoFileWatcher::watch_repo_scoped(
            &git_dir,
            Some(&root),
            Some(&git_dir),
            WatchScope::Whole,
        )
        .unwrap();
        assert_eq!(
            whole.registration().registered_paths(),
            vec![git_dir.clone(), root.clone()]
        );
        // The default is the narrowed set: no root, the files and directories in it.
        let narrow = RepoFileWatcher::watch_repo(&git_dir, Some(&root), None).unwrap();
        let paths = narrow.registration().registered_paths();
        assert!(narrow.registration().narrowed());
        assert!(!paths.contains(&root) && !paths.contains(&root.join("target")));
        assert!(paths.contains(&git_dir) && paths.contains(&root.join("a.txt")));
        // A registration over a non-recursive root is never narrowed.
        let flat = RepoFileWatcher::watch_repo_with(
            &git_dir,
            Some(&root),
            None,
            notify::RecursiveMode::NonRecursive,
        )
        .unwrap();
        assert_eq!(flat.registration().registered_paths(), vec![git_dir, root]);
    }

    /// Startup costs one listing: the loop reuses the registration's.
    #[test]
    fn the_loop_reuses_the_rules_the_registration_listed() {
        let temp = TempDir::new().unwrap();
        let live = spawn_scoped(temp.path(), WatchScope::Narrow, ignore_target);
        assert_eq!(live.filter.loads(), 1, "one published load, not two");
        assert!(
            live.registration.take_initial_rules().is_none(),
            "taken by the loop"
        );
        live.finish();
    }

    /// Per scope, the callbacks one 50,000-file storm in `target/` costs, with
    /// a Whole-against-Whole null pair. Run on a quiet host:
    /// `cargo test --lib watcher::tests::scope_tests::live::measure_callback_volume_by_watch_scope -- --ignored --nocapture`.
    /// A run that starts or ends above load 30 is printed as CONTAMINATED.
    #[test]
    #[ignore = "measurement; 50,000 file creations per arm on a quiet host"]
    fn measure_callback_volume_by_watch_scope() {
        const FILES: u32 = 50_000;
        const REPS: usize = 5;
        let load = || {
            let mut load = [0f64; 3];
            // SAFETY: `getloadavg` writes at most `3` doubles into the array.
            let got = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
            if got >= 1 {
                load[0]
            } else {
                f64::NAN
            }
        };
        let load_start = load();
        let arms = [
            ("whole", WatchScope::Whole),
            ("narrow", WatchScope::Narrow),
            ("whole-null", WatchScope::Whole),
        ];
        let mut rows: Vec<Vec<u64>> = vec![Vec::new(); arms.len()];
        for _ in 0..REPS {
            for (arm, (_, scope)) in arms.iter().enumerate() {
                let temp = TempDir::new().unwrap();
                let live = spawn_scoped(temp.path(), *scope, ignore_target);
                // An arm that did not get the registration it names measures
                // the wrong thing: refuse to run it.
                assert_eq!(
                    live.registration.narrowed(),
                    *scope == WatchScope::Narrow,
                    "the {scope:?} arm has the wrong registration"
                );
                for shard in 0..50 {
                    std::fs::create_dir_all(live.root.join("target").join(format!("d{shard}")))
                        .unwrap();
                }
                quiet_callbacks(&live.filter);
                let before = live.filter.callbacks();
                for i in 0..FILES {
                    std::fs::write(
                        live.root
                            .join("target")
                            .join(format!("d{}", i % 50))
                            .join(format!("f{i}.o")),
                        "x",
                    )
                    .unwrap();
                }
                quiet_callbacks(&live.filter);
                rows[arm].push(live.filter.callbacks() - before);
                live.finish();
            }
        }
        let load_end = load();
        let contaminated = load_start > 30.0 || load_end > 30.0 || load_start.is_nan();
        for ((name, _), row) in arms.iter().zip(&rows) {
            eprintln!(
                "MEASURE {name}: callbacks per {FILES}-file storm in target/ n={REPS}: {row:?} min={} max={}",
                row.iter().min().unwrap(),
                row.iter().max().unwrap()
            );
        }
        eprintln!(
            "MEASURE load 1m start={load_start:.1} end={load_end:.1} {}",
            if contaminated {
                "CONTAMINATED (above 30): not a result"
            } else {
                "clean"
            }
        );
    }
}

/// Linux and Windows register what they always did, whatever scope is asked for.
#[cfg(not(target_os = "macos"))]
#[test]
fn the_scope_changes_no_registration_where_the_root_watch_is_not_recursive() {
    let temp = TempDir::new().unwrap();
    git_init(temp.path(), false);
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    let git_dir = root.join(".git");
    for scope in [WatchScope::Narrow, WatchScope::Whole] {
        let watcher =
            RepoFileWatcher::watch_repo_scoped(&git_dir, Some(&root), None, scope).unwrap();
        assert!(!watcher.registration().narrowed());
        assert_eq!(
            watcher.registration().registered_paths(),
            vec![git_dir.clone(), root.clone()]
        );
        watcher.registration().set_scope(WatchScope::Narrow);
        assert_eq!(watcher.registration().reregistrations(), 0);
    }
}
