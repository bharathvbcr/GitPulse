//! Event-storm bounds for the watch pipeline: several repositories watched at
//! once while build tools write tens of thousands of files into ignored
//! directories, thousands of tracked paths change, and git churns its own
//! object store. Measured against the real notify backend, not a classifier
//! loop, because the claims are about what the whole pipeline costs: emitted
//! `repo-changed` events, process CPU, and peak resident memory.
//!
//! The default test runs a reduced storm so the suite stays quick;
//! `cargo test --lib watcher::storm -- --ignored --nocapture` runs the full
//! one (3 repositories, 50k ignored + 5k tracked + 2k object writes each).

use super::*;
use crate::procguard::LockedSpawn;
use std::sync::atomic::AtomicU32;
use std::thread;
use tempfile::TempDir;

#[derive(Clone, Copy)]
struct Scale {
    repos: usize,
    /// Files per repository written under ignored build directories.
    ignored: usize,
    /// Tracked rewrites per repository: top-level files, plus the same count
    /// under `src/`, which a non-recursive worktree watch never sees.
    tracked: usize,
    /// Loose-object-sized writes under `.git/objects` per repository.
    objects: usize,
}

fn cpu_and_rss() -> (Duration, u64) {
    // SAFETY: `rusage` is plain data and `getrusage` only writes into it.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut usage), 0);
        usage
    };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, (t.tv_usec as u32) * 1000);
    // macOS reports ru_maxrss in bytes, Linux in KiB.
    let rss = if cfg!(target_os = "macos") {
        usage.ru_maxrss as u64
    } else {
        usage.ru_maxrss as u64 * 1024
    };
    (tv(usage.ru_utime) + tv(usage.ru_stime), rss)
}

fn init_repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let out = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output_locked()
        .expect("git init");
    assert!(out.status.success());
    crate::test_support::trust_repo(dir.path());
    let canonical = dir.path().canonicalize().unwrap();
    (dir, canonical)
}

/// One repository's worth of build-tool and git churn. Returns when done.
fn storm(root: &Path, scale: Scale) {
    let deps = root.join("target").join("debug").join("deps");
    let nm = root.join("node_modules").join("pkg");
    let objects = root.join(".git").join("objects");
    let src = root.join("src");
    for dir in [&deps, &nm, &src] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let tracked_every = (scale.ignored / scale.tracked).max(1);
    let object_every = (scale.ignored / scale.objects).max(1);
    for i in 0..scale.ignored {
        let dir = if i % 5 == 0 { &nm } else { &deps };
        std::fs::write(dir.join(format!("f{i}.o")), b"x").unwrap();
        if i % tracked_every == 0 {
            let n = i / tracked_every;
            std::fs::write(root.join(format!("tracked{}.txt", n % 200)), n.to_string()).unwrap();
            std::fs::write(src.join(format!("m{}.rs", n % 200)), n.to_string()).unwrap();
        }
        if i % object_every == 0 {
            let shard = objects.join(format!("{:02x}", i % 256));
            std::fs::create_dir_all(&shard).unwrap();
            std::fs::write(shard.join(format!("obj{i}")), b"x").unwrap();
        }
    }
}

struct Watched {
    stop: Arc<StopSignal>,
    emits: Arc<AtomicU32>,
    stamps: Arc<Mutex<Vec<Instant>>>,
    handle: thread::JoinHandle<()>,
}

fn watch(root: &Path) -> Watched {
    let git_dir = resolve_git_dir(root).expect("git dir");
    let watcher = RepoFileWatcher::watch_repo(&git_dir, Some(root), None).expect("watcher");
    let stop = StopSignal::new();
    let emits = Arc::new(AtomicU32::new(0));
    let stamps = Arc::new(Mutex::new(Vec::new()));
    let (t_stop, t_emits, t_stamps) = (stop.clone(), emits.clone(), stamps.clone());
    let ctx = WatchLoopContext {
        git_dir: root.to_path_buf(),
        internal_roots: vec![root.join(".git")],
        emit_path: root.to_string_lossy().into_owned(),
        timer_wakes: Default::default(),
    };
    let handle = thread::spawn(move || {
        let session_stop = t_stop.clone();
        run_watch_loop(&watcher, ctx, t_stop, None, session_stop, move |_| {
            t_emits.fetch_add(1, Ordering::SeqCst);
            t_stamps.lock().unwrap().push(Instant::now());
        });
    });
    Watched {
        stop,
        emits,
        stamps,
        handle,
    }
}

fn run_storm(roots: &[PathBuf], scale: Scale) -> Duration {
    let started = Instant::now();
    let writers: Vec<_> = roots
        .iter()
        .cloned()
        .map(|root| thread::spawn(move || storm(&root, scale)))
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    started.elapsed()
}

fn assert_storm_stays_bounded(scale: Scale) {
    let repos: Vec<_> = (0..scale.repos).map(|_| init_repo()).collect();
    let roots: Vec<PathBuf> = repos.iter().map(|(_, root)| root.clone()).collect();

    // Baseline: the identical writes with nothing watching.
    let baseline_repos: Vec<_> = (0..scale.repos).map(|_| init_repo()).collect();
    let baseline_roots: Vec<PathBuf> = baseline_repos.iter().map(|(_, r)| r.clone()).collect();
    let (cpu0, _) = cpu_and_rss();
    let baseline_wall = run_storm(&baseline_roots, scale);
    let (cpu1, rss_before) = cpu_and_rss();
    let baseline_cpu = cpu1 - cpu0;

    let watched: Vec<_> = roots.iter().map(|root| watch(root)).collect();
    thread::sleep(Duration::from_millis(500));
    let (cpu2, _) = cpu_and_rss();
    let wall = run_storm(&roots, scale);
    let storm_end = Instant::now();
    // Let every trailing emission settle, then count.
    thread::sleep(DEBOUNCE_MAX_WAIT + DEBOUNCE_QUIET + Duration::from_millis(1500));
    let (cpu3, rss_after) = cpu_and_rss();
    let watched_cpu = cpu3 - cpu2;

    let mut report = String::new();
    for (root, w) in roots.iter().zip(&watched) {
        let stamps = w.stamps.lock().unwrap().clone();
        let emits = w.emits.load(Ordering::SeqCst);
        report.push_str(&format!(
            "{}: emits={emits}\n",
            root.file_name().unwrap().to_string_lossy()
        ));
        // Storm length bounds the emission count: one per SCAN_COALESCE, plus
        // the first one and the trailing settle.
        let allowed = (wall.as_secs() / SCAN_COALESCE.as_secs()) as u32 + 3;
        assert!(
            emits <= allowed,
            "{emits} emissions for a {wall:?} storm (allowed {allowed})"
        );
        assert!(emits >= 1, "tracked writes must still announce themselves");
        for pair in stamps.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= SCAN_COALESCE - Duration::from_millis(250),
                "two emissions {:?} apart, under the {SCAN_COALESCE:?} coalesce window",
                pair[1].duration_since(pair[0])
            );
        }
        let last = *stamps.last().expect("an emission");
        assert!(
            last.saturating_duration_since(storm_end) <= DEBOUNCE_MAX_WAIT + Duration::from_secs(2),
            "the settled emission trailed the storm too long"
        );
    }
    for w in &watched {
        w.stop.stop();
    }
    for w in watched {
        w.handle.join().unwrap();
    }
    eprintln!(
        "storm repos={} ignored={} tracked={} objects={}\n\
         baseline wall={baseline_wall:?} cpu={baseline_cpu:?}\n\
         watched  wall={wall:?} cpu={watched_cpu:?} (incl. settle)\n\
         rss before={}MiB after={}MiB\n{report}",
        scale.repos,
        scale.ignored,
        scale.tracked,
        scale.objects,
        rss_before >> 20,
        rss_after >> 20
    );
    let extra = watched_cpu.saturating_sub(baseline_cpu);
    assert!(
        extra <= baseline_cpu + Duration::from_secs(3),
        "watching cost {extra:?} CPU on top of {baseline_cpu:?} of writes"
    );
    assert!(
        rss_after.saturating_sub(rss_before) <= 256 << 20,
        "peak RSS grew {} MiB under the storm",
        rss_after.saturating_sub(rss_before) >> 20
    );
}

/// Set in the re-executed test binary that runs the storm for real.
const STORM_CHILD: &str = "GITPULSE_STORM_CHILD";

/// Runs the storm in a process of its own.
///
/// The bounds are process-wide: `getrusage(RUSAGE_SELF)` and peak RSS count
/// every thread, so beside the other lib tests (default `cargo test` runs them
/// in parallel, as the test-parallel workflow does) the watched window, which
/// is several times longer than the baseline one, absorbed their CPU and
/// memory and failed with 37 s "of watching" on 10 s of writes, on
/// `hx/perf-reconcile` alone as well. The test binary is re-executed with this
/// one test selected; the child has nothing else running, so what it measures
/// is the watch pipeline.
fn isolated(test: &str, scale: Scale) {
    use crate::procguard::LockedSpawn;
    if std::env::var_os(STORM_CHILD).is_some() {
        return assert_storm_stays_bounded(scale);
    }
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test, "--include-ignored", "--nocapture"])
        .args(["--test-threads=1"])
        .env(STORM_CHILD, "1")
        .output_locked()
        .expect("re-executing the test binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && text.contains("1 passed"),
        "the isolated storm failed:\n{text}"
    );
}

/// Reduced storm: 3 watched repositories, 6k ignored-directory writes, 600
/// tracked and 240 object writes each (the full storm's 50:5:2 shape).
///
/// Sized by what it writes, not only by what it proves: every tiny file costs a
/// filesystem block, each repository is written twice (watched, and the
/// unwatched baseline), and at 10k/1k/400 the child process wrote 290.6 MiB,
/// over the 256 MiB per-test budget `scripts/test-disk-writes.mjs` holds every
/// test to. The bounds asserted (emissions per second of storm, CPU against
/// the unwatched baseline, RSS growth) are ratios of the storm to itself, so
/// they hold at any size long enough to span several coalesce windows.
#[test]
fn event_storm_across_watched_repos_stays_bounded() {
    isolated(
        "watcher::storm::event_storm_across_watched_repos_stays_bounded",
        Scale {
            repos: 3,
            ignored: 6_000,
            tracked: 600,
            objects: 240,
        },
    );
}

/// The full storm: 50k ignored-directory writes plus 5k tracked and 2k object
/// writes in each of 3 watched repositories.
#[test]
#[ignore = "full-size storm takes minutes; run explicitly with --ignored"]
fn full_event_storm_across_watched_repos_stays_bounded() {
    isolated(
        "watcher::storm::full_event_storm_across_watched_repos_stays_bounded",
        Scale {
            repos: 3,
            ignored: 50_000,
            tracked: 5_000,
            objects: 2_000,
        },
    );
}

/// The storm's own paths, classified by the real noise gate: everything the
/// build tools and git write is dropped, the tracked rewrites still count. The
/// storm test cannot see this: with filtering off the debounce alone keeps its
/// emission count inside the bound, so it stayed green.
#[test]
fn the_paths_a_storm_writes_are_classified_as_noise_or_signal() {
    let (_dir, root) = init_repo();
    let rules = IgnoreRules::builtin();
    let internal = vec![root.join(".git")];
    let classify = |path: PathBuf| {
        let event = notify::Event::new(notify::EventKind::Any).add_path(path);
        event_has_signal(&event, &internal, &root, None, &rules)
    };
    for noise in [
        root.join("target/debug/deps/f1.o"),
        root.join("node_modules/pkg/f5.o"),
        root.join(".git/index.lock"),
    ] {
        assert!(!classify(noise.clone()), "{} is noise", noise.display());
    }
    for signal in [root.join("tracked3.txt"), root.join("src/m3.rs")] {
        assert!(classify(signal.clone()), "{} is signal", signal.display());
    }
}

/// The coalesce window, driven on a clock the test owns: a write every 450 ms
/// is quiet for 400 ms each time, so the quiet rule alone releases a scan after
/// every write, and only the window keeps scans [`SCAN_COALESCE`] apart. The
/// storm test cannot see this: `DEBOUNCE_MAX_WAIT` equals the window, so the
/// max-wait rule spaces emissions the same way when the window is gone.
#[test]
fn scans_stay_a_coalesce_window_apart_under_writes_that_keep_going_quiet() {
    let origin = Instant::now();
    let step = Duration::from_millis(10);
    let write_every = Duration::from_millis(450);
    assert!(write_every > DEBOUNCE_QUIET && write_every < SCAN_COALESCE);
    let (mut pending, mut last_event, mut first_pending, mut last_scan) =
        (false, origin, None, None);
    let mut scans: Vec<Instant> = Vec::new();
    // 3,000 ticks of simulated time; nothing here waits on a real clock.
    for tick in 0..3_000u32 {
        let elapsed = step * tick;
        let now = origin + elapsed;
        if elapsed.as_millis().is_multiple_of(write_every.as_millis()) {
            if !pending {
                first_pending = Some(now);
            }
            pending = true;
            last_event = now;
        }
        if scan_is_due(pending, last_event, first_pending, last_scan, now) {
            scans.push(now);
            pending = false;
            first_pending = None;
            last_scan = Some(now);
        }
    }
    assert!(scans.len() >= 10, "writes must still announce: {scans:?}");
    for pair in scans.windows(2) {
        assert!(
            pair[1] - pair[0] >= SCAN_COALESCE,
            "scans {:?} apart, under the {SCAN_COALESCE:?} window",
            pair[1] - pair[0]
        );
    }
}
