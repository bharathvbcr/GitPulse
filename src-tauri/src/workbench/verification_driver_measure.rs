//! A measurement, not a check: the real driver swept over a copy of a real
//! profile for a fixed window, with Manvi's start stubbed, so the children it
//! spawns can be counted from outside (a sampler rooted at this process) and
//! from inside (the spawn log).
//!
//! Everything is the production path except the one seam that would start a
//! model turn: [`Backend::start`] is a counter that answers as an accepted
//! start would, and the run asserts every start the driver made reached it.
//! `head_of` and `base_of` are the host backend's own functions, so git runs
//! for real, read-only, against whatever worktrees the copied cards name. The
//! ref-state cache keeps its production racy window (no `eager`).
//!
//! What differs from the app: a test build has no app governor and an
//! unpaced spawn gate, so nothing is shed or deferred under load. The numbers
//! are the driver's demand, not what admission let through.
//!
//! The profile must be a copy. A path inside the GitPulse config directory
//! (the live profile's home) is refused before anything opens it. Rerun from
//! the repository root, on a fresh copy each time (the driver writes to it):
//!
//! ```text
//! GITPULSE_DRIVER_MEASURE_PROFILE=<abs path to a copy of workbench.sqlite> \
//! GITPULSE_DRIVER_MEASURE_SECS=45 \
//! cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --exact \
//!   workbench::verification_driver::measure::measure_driver_children_over_a_profile_copy \
//!   --test-threads=1 --nocapture
//! ```

use super::{held_cards, target_of, verification_base, Backend, Driver, Report, SWEEP_PERIOD};
use crate::workbench::verification_gate as gate;
use crate::workbench::WorkbenchError;
use dc_store::Store;
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The host backend with Manvi's start replaced by a counter.
struct Measured {
    starts: Cell<usize>,
    heads: Cell<usize>,
    heads_failed: Cell<usize>,
}

impl Backend for Measured {
    fn start(&self, _params: Value) -> Result<Value, WorkbenchError> {
        self.starts.set(self.starts.get() + 1);
        Ok(json!({"ok": true, "stub": "measure"}))
    }
    fn head_of(&self, worktree: &str) -> Result<String, String> {
        self.heads.set(self.heads.get() + 1);
        let head = gate::current_head(worktree);
        if head.is_err() {
            self.heads_failed.set(self.heads_failed.get() + 1);
        }
        head
    }
    fn base_of(&self, worktree: &str, head: &str) -> Result<String, String> {
        verification_base(Path::new(worktree), head)
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn cancelled(&self) -> bool {
        false
    }
}

/// The copy to sweep, refused when it is the live profile or lives beside it.
fn profile_copy() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("GITPULSE_DRIVER_MEASURE_PROFILE")
            .expect("GITPULSE_DRIVER_MEASURE_PROFILE names the profile copy to sweep"),
    );
    assert!(path.is_absolute(), "the profile copy path must be absolute");
    let path = path.canonicalize().expect("the profile copy exists");
    if let Some(config) = crate::tool_config::default_config_dir() {
        let config = config.canonicalize().unwrap_or(config);
        assert!(
            !path.starts_with(&config),
            "{} is inside the live GitPulse config directory {}; measure a copy",
            path.display(),
            config.display()
        );
    }
    path
}

#[test]
#[ignore = "measurement over a copy of a real profile for a fixed window; needs GITPULSE_DRIVER_MEASURE_PROFILE (rerun command in this file's header)"]
fn measure_driver_children_over_a_profile_copy() {
    let profile = profile_copy();
    let window = Duration::from_secs(
        std::env::var("GITPULSE_DRIVER_MEASURE_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(45),
    );
    let store = Store::open(&profile).expect("open the profile copy");
    let backend = Measured {
        starts: Cell::new(0),
        heads: Cell::new(0),
        heads_failed: Cell::new(0),
    };
    let mut driver = Driver::default();
    let mut totals = Report::default();
    let mut sweeps = 0usize;
    let began = Instant::now();
    // The install loop's shape: sweep, then wait a period.
    while began.elapsed() < window {
        let report = driver.sweep(&store, &backend);
        totals.started += report.started;
        totals.promoted += report.promoted;
        totals.parked += report.parked;
        totals.could_not_run += report.could_not_run;
        sweeps += 1;
        let left = window.saturating_sub(began.elapsed());
        std::thread::sleep(SWEEP_PERIOD.min(left));
    }
    let elapsed = began.elapsed();

    let cards = held_cards(&store).expect("list held cards");
    let targets: Vec<_> = cards.iter().filter_map(target_of).collect();
    let worktrees: BTreeSet<&str> = targets.iter().map(|t| t.worktree.as_str()).collect();
    let present = worktrees.iter().filter(|w| Path::new(w).is_dir()).count();
    // The spawn log files a child under the directory it ran in; count each
    // spelling of a worktree once.
    let mut dirs = BTreeSet::new();
    for worktree in &worktrees {
        dirs.insert(PathBuf::from(worktree));
        if let Ok(canonical) = Path::new(worktree).canonicalize() {
            dirs.insert(canonical);
        }
    }
    let mut spawned = 0usize;
    let mut head_reads = 0usize;
    for dir in &dirs {
        for argv in crate::engine::git_cli::spawn_log::spawns_in(dir) {
            spawned += 1;
            if argv.iter().any(|arg| arg == "HEAD^{commit}") {
                head_reads += 1;
            }
        }
    }
    println!(
        "MEASURE {}",
        json!({
            "window_s": elapsed.as_secs_f64(),
            "sweeps": sweeps,
            "review_cards": cards.len(),
            "held_with_target": targets.len(),
            "target_worktrees": worktrees.len(),
            "target_worktrees_present": present,
            "head_of_calls": backend.heads.get(),
            "head_of_failed": backend.heads_failed.get(),
            "stub_starts": backend.starts.get(),
            "driver_report": {
                "started": totals.started, "promoted": totals.promoted,
                "parked": totals.parked, "could_not_run": totals.could_not_run,
            },
            "spawn_log_in_target_worktrees": spawned,
            "spawn_log_head_commit_reads": head_reads,
        })
    );
    assert!(sweeps > 0, "the window ran no sweep");
    assert_eq!(
        backend.starts.get(),
        totals.started,
        "every start the driver reported went to the stub"
    );
}
