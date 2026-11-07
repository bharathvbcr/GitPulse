//! End-to-end `gitpulse_worktree_create` on a trusted fixture, with a per-phase
//! timeline.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench worktree_create`
//! (`-- --smoke` for 3 samples).
//!
//! The create tool (`mcp/mod.rs`, `gitpulse_worktree_create`, default arguments)
//! calls `engine::worktree::add_worktree_with_devmap` with the default
//! `CacheSelection` and `DevmapMode::Copy`; this bench calls the same function
//! with the same arguments. Rows, interleaved so none always follows the
//! heaviest neighbour:
//!
//! * `create e2e (share_cache)`: the whole call. Its `t_*` metrics are the
//!   per-phase timeline, in milliseconds after `.git` appeared in the new
//!   worktree, sampled by a poller thread every 2 ms (so each is a "first seen"
//!   with up to 2 ms of resolution): checkout (last tracked file present), store
//!   (DevMap sqlite), csr, cas, overlay (`overlay.json`), node_modules, nested
//!   target. A phase that never appeared is absent from the metrics and the
//!   counter `phases_seen` says how many did.
//! * `cp -c -R node_modules (baseline)`: one `cp -c -R` of the fixture's
//!   node_modules into a scratch directory on the same volume, the strongest
//!   available baseline for the cache clone.
//! * `clone_ignored_caches (all caches)`: the cache clone alone, on a worktree
//!   made with `add_worktree` only.
//! * `overlay sync (provisioned worktree)`: `sync_worktree_overlay` alone on a
//!   worktree whose store was provisioned and whose caches are already cloned.
//!
//! Fixture: 3 000 tracked source files plus 400 files of Rust functions (so the
//! DevMap store has symbols), an ignored `node_modules` of 25 000 small files,
//! and a nested `sub/Cargo.toml` with an ignored `sub/target`. It needs the
//! `devmap` CLI on PATH (or `GITPULSE_DEVMAP_BIN`) to build the anchor's store;
//! without one the bench stops rather than time a create that skips seeding.

mod common;

use common::fixture::{build_repo, git, trust_repo, Repo, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::cow_clone::{clone_ignored_caches, CacheSelection};
use gitpulse_lib::engine::worktree::{add_worktree, add_worktree_with_devmap, DevmapMode};
use gitpulse_lib::procguard::LockedSpawn;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SPEC: Spec = Spec {
    files: 3000,
    dirs: 60,
    extra_commits: 1,
    touch: 2,
    linked_worktrees: 0,
};

const NODE_DIRS: usize = 2500;
const NODE_PER_DIR: usize = 10;
const NODE_FILES: usize = NODE_DIRS * NODE_PER_DIR;
const TARGET_FILES: usize = 300;
const RUST_FILES: usize = 400;

fn text(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}

fn write_tree(root: &Path, dirs: usize, per_dir: usize, label: &str) {
    for d in 0..dirs {
        let dir = root.join(format!("pkg{d:04}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..per_dir {
            std::fs::write(
                dir.join(format!("file{f}.js")),
                format!("// {label} {d} {f}\nmodule.exports = {};\n", d * 31 + f),
            )
            .expect("write cache file");
        }
    }
}

/// The fixture: tracked tree, a store built by the `devmap` CLI, ignored caches.
fn build_fixture(root: &Path) -> Repo {
    let repo = build_repo(root, &SPEC);
    let main = &repo.main;
    let exclude = main.join(".git/info/exclude");
    let mut ignore = std::fs::read_to_string(&exclude).expect("read exclude");
    ignore.push_str("node_modules/\ntarget/\n");
    std::fs::write(&exclude, ignore).expect("write exclude");
    std::fs::create_dir_all(main.join("src")).expect("mkdir src");
    for i in 0..RUST_FILES {
        let mut body = String::new();
        for k in 0..10 {
            body.push_str(&format!(
                "pub fn f{i}_{k}(x: u32) -> u32 {{ x.wrapping_add({k}) }}\n"
            ));
        }
        std::fs::write(main.join(format!("src/m{i:04}.rs")), body).expect("write rust file");
    }
    std::fs::create_dir_all(main.join("sub/src")).expect("mkdir sub");
    std::fs::write(
        main.join("sub/Cargo.toml"),
        "[package]\nname = \"sub\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(main.join("sub/src/lib.rs"), "pub fn sub() {}\n").expect("write sub lib");
    git(main, &["add", "-A"]);
    git(main, &["commit", "-q", "-m", "chore: sources"]);
    trust_repo(main);

    write_tree(&main.join("node_modules"), NODE_DIRS, NODE_PER_DIR, "nm");
    let target = main.join("sub/target/debug");
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join(".cargo-lock"), "").expect("write cargo lock");
    write_tree(&target.join("deps"), 30, TARGET_FILES / 30, "tg");

    let devmap = std::env::var("GITPULSE_DEVMAP_BIN").unwrap_or_else(|_| "devmap".to_string());
    let built = std::process::Command::new(&devmap)
        .args(["build", "--manifest"])
        .current_dir(main)
        .output_locked()
        .unwrap_or_else(|e| panic!("cannot run `{devmap} build --manifest`: {e}"));
    assert!(
        built.status.success(),
        "`{devmap} build --manifest` failed ({}): {}",
        built.status,
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(
        devmap_query::paths::store_path(main).is_file(),
        "devmap build left no store, so the create would skip seeding"
    );
    repo
}

/// Phases watched in the new worktree, in the order they are reported.
fn phases(wt: &Path, shape: &Shape) -> Vec<(&'static str, PathBuf)> {
    let store = devmap_query::paths::store_path(wt);
    let codeintel = store.parent().expect("store has a parent").to_path_buf();
    vec![
        ("checkout", wt.join(&shape.last_tracked)),
        ("store", store.clone()),
        ("csr", PathBuf::from(format!("{}.csr", store.display()))),
        ("cas", codeintel.join("cas")),
        ("overlay", codeintel.join("overlay.json")),
        ("node_modules", wt.join("node_modules")),
        ("nested_target", wt.join(&shape.nested_target)),
    ]
}

/// What the poller and the assertions need to know about the repository.
#[derive(Clone)]
struct Shape {
    /// The last tracked path: present once the checkout is complete.
    last_tracked: String,
    /// The nested Cargo `target/` beside a `Cargo.toml`.
    nested_target: String,
    /// Whether the anchor has a `cas` directory to seed.
    has_cas: bool,
}

struct Timeline {
    /// Milliseconds after `.git` appeared, per phase seen.
    seen: Vec<(&'static str, f64)>,
}

/// Polls the worktree until `stop`, recording when each phase first exists.
fn poll(wt: PathBuf, shape: Shape, stop: Arc<AtomicBool>, out: Arc<Mutex<Timeline>>) {
    let mut watched = phases(&wt, &shape);
    if !shape.has_cas {
        watched.retain(|(name, _)| *name != "cas");
    }
    let git_dir = wt.join(".git");
    let mut origin: Option<Instant> = None;
    let mut seen = vec![false; watched.len()];
    loop {
        // One sweep after `stop`, so a phase finishing in the last 2 ms counts.
        let stopping = stop.load(Ordering::Acquire);
        let now = Instant::now();
        if origin.is_none() && git_dir.symlink_metadata().is_ok() {
            origin = Some(now);
        }
        if let Some(origin) = origin {
            for (i, (name, path)) in watched.iter().enumerate() {
                if !seen[i] && path.symlink_metadata().is_ok() {
                    seen[i] = true;
                    out.lock()
                        .expect("timeline lock")
                        .seen
                        .push((name, now.duration_since(origin).as_secs_f64() * 1000.0));
                }
            }
        }
        if stopping {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn remove_lane(repo: &Repo, lane: &Path, branch: &str) {
    git(&repo.main, &["worktree", "remove", "--force", text(lane)]);
    let _ = std::fs::remove_dir_all(lane);
    git(&repo.main, &["worktree", "prune"]);
    git(&repo.main, &["branch", "-q", "-D", branch]);
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("worktree_create");
    let temp = tempfile::tempdir().expect("temp dir");
    // `GITPULSE_BENCH_REPO=<path>` times the same call on a real checkout (its
    // own store, node_modules and target); the default is the synthetic fixture.
    let real = std::env::var_os("GITPULSE_BENCH_REPO").map(PathBuf::from);
    let (repo, shape) = match &real {
        Some(path) => {
            let repo = Repo {
                main: path.clone(),
                linked: Vec::new(),
            };
            trust_repo(&repo.main);
            assert!(
                devmap_query::paths::store_path(&repo.main).is_file(),
                "GITPULSE_BENCH_REPO has no DevMap store, so the create would skip seeding"
            );
            let listed = git(&repo.main, &["ls-files"]);
            let last_tracked = listed.lines().last().expect("tracked files").to_string();
            let nested_target = ["src-tauri/target", "sub/target"]
                .into_iter()
                .find(|rel| repo.main.join(rel).is_dir())
                .expect("a nested target beside a Cargo.toml")
                .to_string();
            let has_cas = repo.main.join(".devcouncil/codeintel/cas").is_dir();
            report.note(format!(
                "real repository {}: nested target {nested_target}, store present, cas {has_cas}; hermetic HOME (trust store), production spawn rate lifted",
                repo.main.display()
            ));
            (
                repo,
                Shape {
                    last_tracked,
                    nested_target,
                    has_cas,
                },
            )
        }
        None => {
            let root = temp.path().join("fixture");
            let repo = build_fixture(&root);
            report.note(format!(
                "fixture: {} tracked files + {RUST_FILES} rust files, ignored node_modules of {NODE_FILES} small files, \
                 nested sub/Cargo.toml with an ignored sub/target of {TARGET_FILES} files; DevMap store built by the devmap CLI; \
                 hermetic HOME, production spawn rate lifted",
                SPEC.files
            ));
            let has_cas = repo.main.join(".devcouncil/codeintel/cas").is_dir();
            let last_tracked = git(&repo.main, &["ls-files"])
                .lines()
                .last()
                .expect("tracked files")
                .to_string();
            (
                repo,
                Shape {
                    last_tracked,
                    nested_target: "sub/target".to_string(),
                    has_cas,
                },
            )
        }
    };
    let root = temp.path().join("fixture");
    std::fs::create_dir_all(&root).expect("mkdir root");

    let serial = std::cell::Cell::new(0usize);
    let next = |prefix: &str| {
        serial.set(serial.get() + 1);
        format!("{prefix}{}", serial.get())
    };
    let lanes = root.join("lanes");
    std::fs::create_dir_all(&lanes).expect("mkdir lanes");
    let selection = CacheSelection::default();

    let create_e2e = || {
        let name = next("e2e");
        let lane = lanes.join(&name);
        let branch = format!("bench/{name}");
        let stop = Arc::new(AtomicBool::new(false));
        let timeline = Arc::new(Mutex::new(Timeline { seen: Vec::new() }));
        let poller = {
            let (lane, shape, stop, timeline) = (
                lane.clone(),
                shape.clone(),
                Arc::clone(&stop),
                Arc::clone(&timeline),
            );
            std::thread::spawn(move || poll(lane, shape, stop, timeline))
        };
        let started = Instant::now();
        let created = add_worktree_with_devmap(
            repo.path(),
            text(&lane),
            Some(&branch),
            None,
            false,
            Some(&selection),
            true,
            DevmapMode::Copy,
        )
        .expect("add_worktree_with_devmap");
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Release);
        poller.join().expect("poller");
        // A degraded create is not timed as if it were whole.
        assert_eq!(created.cache_error, None, "cache clone degraded");
        assert!(created.low_disk.is_none(), "cache clone skipped for disk");
        assert!(
            lane.join("node_modules").is_dir(),
            "node_modules was not cloned"
        );
        assert!(
            lane.join(&shape.nested_target).is_dir(),
            "nested {} was not cloned, and the result did not say so",
            shape.nested_target
        );
        let seen = timeline.lock().expect("timeline lock");
        let mut sample = Sample::default()
            .counter("phases_seen", seen.seen.len() as u64)
            .elapsed(elapsed);
        for (phase, ms) in &seen.seen {
            let key: &'static str = match *phase {
                "checkout" => "t_checkout_ms",
                "store" => "t_store_ms",
                "csr" => "t_csr_ms",
                "cas" => "t_cas_ms",
                "overlay" => "t_overlay_ms",
                "node_modules" => "t_node_modules_ms",
                _ => "t_nested_target_ms",
            };
            sample = sample.metric(key, *ms);
        }
        drop(seen);
        remove_lane(&repo, &lane, &branch);
        sample
    };

    let cp_baseline = || {
        let dst = lanes.join(next("cp"));
        std::fs::create_dir_all(&dst).expect("mkdir cp dst");
        let started = Instant::now();
        let status = std::process::Command::new("/bin/cp")
            .args(["-c", "-R"])
            .arg(repo.main.join("node_modules"))
            .arg(dst.join("node_modules"))
            .status_locked()
            .expect("spawn cp");
        let elapsed = started.elapsed();
        assert!(status.success(), "cp -c -R failed");
        std::fs::remove_dir_all(&dst).expect("remove cp dst");
        Sample::default().elapsed(elapsed)
    };

    let cache_only = || {
        let name = next("cc");
        let lane = lanes.join(&name);
        let branch = format!("bench/{name}");
        add_worktree(repo.path(), text(&lane), Some(&branch), None, false).expect("add_worktree");
        let started = Instant::now();
        let report = clone_ignored_caches(&repo.main, &lane, &selection).expect("clone caches");
        let elapsed = started.elapsed();
        assert!(report.error().is_none(), "cache clone failed");
        let copied = report.copied.len() as u64;
        remove_lane(&repo, &lane, &branch);
        Sample::default().counter("caches", copied).elapsed(elapsed)
    };

    let overlay_only = || {
        let name = next("ov");
        let lane = lanes.join(&name);
        let branch = format!("bench/{name}");
        add_worktree_with_devmap(
            repo.path(),
            text(&lane),
            Some(&branch),
            None,
            false,
            Some(&selection),
            false,
            DevmapMode::Copy,
        )
        .expect("add_worktree_with_devmap");
        let started = Instant::now();
        gitpulse_lib::codeintel::sync_worktree_overlay(text(&lane)).expect("overlay sync");
        let elapsed = started.elapsed();
        remove_lane(&repo, &lane, &branch);
        Sample::default().elapsed(elapsed)
    };

    let cases = vec![
        Case::new("create e2e (share_cache)", create_e2e).ungated(),
        Case::new("cp -c -R node_modules (baseline)", cp_baseline).ungated(),
        Case::new("clone_ignored_caches (all caches)", cache_only).ungated(),
        Case::new("overlay sync (provisioned worktree)", overlay_only).ungated(),
    ];
    let rows = interleave(cases, report.opts.samples(5), report.opts.warmup(1));
    report.extend(rows);
    report.finish();
}
