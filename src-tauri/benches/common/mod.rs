//! The one harness every bench in this directory shares.
//!
//! Declared with `mod common;` by each bench, which is also what makes the
//! bench hermetic: the module below includes `src/test_support/env.rs`, whose
//! pre-`main` constructor points `HOME`, the XDG roots, the git config and the
//! trust store at a per-process temp directory. A bench that read the
//! developer's `~/.gitconfig`, `~/.claude` sessions or real trust store would
//! produce numbers nobody else can reproduce, and its read paths that write
//! (the ledger, the board) would touch the real store hundreds of times.
//!
//! # What a run records
//!
//! * Every row: `n`, `min`, `p50`, `p95`, `max` in nanoseconds. **`min` is the
//!   figure to compare between builds** (min-of-N is the least load-sensitive
//!   statistic on a shared host); the tail is reported because it is what a
//!   user feels.
//! * Work counters per row (rows, ops, git processes, ...). A counter is
//!   deterministic for a fixed fixture, so it is *asserted equal across every
//!   sample* of the row: a counter that varies is a metric, not a counter, and
//!   goes in `metrics` where nothing gates on it.
//! * The host-load snapshot, taken before and after the run, through the
//!   repo's own `sysload` sensor (load average, cores, memory pressure,
//!   thermal, power), plus the CPU count. A loaded host hides fixes; the JSON
//!   says how loaded this one was rather than leaving the reader to guess.
//! * Commit, whether the tree was dirty, OS, architecture, runner class.
//!
//! # Interleaving
//!
//! [`interleave`] runs one sample of every case per round, in rotating order,
//! so load that drifts during the run lands on every case alike instead of on
//! whichever ran last. An A/B between two *builds* is two JSON files from two
//! binaries run back to back (A B A B) and compared by `scripts/bench-compare.mjs`;
//! one process cannot hold two builds of the library.
//!
//! # Flags
//!
//! `cargo bench` appends `--bench` to a `harness = false` binary, so unknown
//! arguments are ignored rather than fatal. `--smoke` cuts every row to a
//! handful of samples (the fixtures keep their size, so counters are
//! identical to a full run's). `--json <path>` writes the machine-readable
//! report; `GITPULSE_BENCH_JSON_DIR` writes `<bench>.json` into that directory.

#![allow(dead_code)]

/// The hermetic home, git config and trust store (pre-`main` constructor).
#[path = "../../src/test_support/env.rs"]
pub mod env;

pub mod fixture;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Samples per row in `--smoke` mode.
pub const SMOKE_SAMPLES: usize = 3;
/// Warmup rounds in `--smoke` mode.
pub const SMOKE_WARMUP: usize = 1;

/// Parsed command line.
pub struct Opts {
    pub smoke: bool,
    pub json: Option<PathBuf>,
}

impl Opts {
    pub fn from_args(bench: &str) -> Self {
        let mut smoke = false;
        let mut json = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--smoke" => smoke = true,
                "--json" => json = args.next().map(PathBuf::from),
                // `--bench` (appended by `cargo bench`) and anything a wrapper
                // adds: none of it changes what is measured.
                _ => {}
            }
        }
        if json.is_none() {
            json = std::env::var_os("GITPULSE_BENCH_JSON_DIR")
                .map(|dir| PathBuf::from(dir).join(format!("{bench}.json")));
        }
        Self { smoke, json }
    }

    /// `full` samples, or [`SMOKE_SAMPLES`] under `--smoke`.
    pub fn samples(&self, full: usize) -> usize {
        if self.smoke {
            SMOKE_SAMPLES.min(full)
        } else {
            full
        }
    }

    pub fn warmup(&self, full: usize) -> usize {
        if self.smoke {
            SMOKE_WARMUP.min(full)
        } else {
            full
        }
    }
}

/// What one sample of a case reports besides its time.
#[derive(Default)]
pub struct Sample {
    /// Deterministic work: asserted equal across the row's samples.
    pub counters: BTreeMap<&'static str, u64>,
    /// Anything that legitimately varies (bytes with a duration in them, rates,
    /// message counts under a race). Reported, never gated on.
    pub metrics: BTreeMap<&'static str, f64>,
    /// When the case times a sub-span of its own closure (excluding fixture
    /// reset), it says so here; otherwise the harness times the closure.
    pub elapsed: Option<Duration>,
}

impl Sample {
    pub fn counter(mut self, name: &'static str, value: u64) -> Self {
        self.counters.insert(name, value);
        self
    }

    pub fn metric(mut self, name: &'static str, value: f64) -> Self {
        self.metrics.insert(name, value);
        self
    }

    pub fn elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed = Some(elapsed);
        self
    }
}

/// One thing to measure. `time_gated: false` keeps the row in the report and
/// its counters in the gate but exempts its wall clock (a row whose time is a
/// property of the OS scheduler, not of the code).
pub struct Case<'a> {
    pub name: String,
    pub time_gated: bool,
    pub run: Box<dyn FnMut() -> Sample + 'a>,
}

impl<'a> Case<'a> {
    pub fn new(name: impl Into<String>, run: impl FnMut() -> Sample + 'a) -> Self {
        Self {
            name: name.into(),
            time_gated: true,
            run: Box::new(run),
        }
    }

    pub fn ungated(mut self) -> Self {
        self.time_gated = false;
        self
    }
}

/// A finished row.
pub struct Row {
    pub name: String,
    pub time_gated: bool,
    /// Sorted ascending, nanoseconds.
    pub ns: Vec<u64>,
    pub counters: BTreeMap<&'static str, u64>,
    /// Median of each metric across samples.
    pub metrics: BTreeMap<&'static str, f64>,
}

impl Row {
    pub fn min(&self) -> Duration {
        Duration::from_nanos(self.ns[0])
    }

    pub fn p50(&self) -> Duration {
        Duration::from_nanos(percentile(&self.ns, 0.50))
    }

    pub fn p95(&self) -> Duration {
        Duration::from_nanos(percentile(&self.ns, 0.95))
    }

    pub fn max(&self) -> Duration {
        Duration::from_nanos(*self.ns.last().expect("a row has samples"))
    }

    fn to_json(&self) -> Value {
        json!({
            "n": self.ns.len(),
            "time_gated": self.time_gated,
            "min_ns": self.ns[0],
            "p50_ns": percentile(&self.ns, 0.50),
            "p95_ns": percentile(&self.ns, 0.95),
            "max_ns": self.ns.last(),
            "counters": self.counters,
            "metrics": self.metrics,
        })
    }
}

/// Nearest-rank percentile of an ascending slice (`q` in `0..=1`).
pub fn percentile(sorted: &[u64], q: f64) -> u64 {
    assert!(!sorted.is_empty(), "percentile of no samples");
    let rank = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

/// Runs every case `warmup` times (untimed), then `rounds` times with one
/// timed sample of each case per round, rotating which case goes first so no
/// case is always the one that follows the heaviest neighbour.
pub fn interleave(cases: Vec<Case<'_>>, rounds: usize, warmup: usize) -> Vec<Row> {
    assert!(rounds > 0, "a bench with zero samples measures nothing");
    let mut cases = cases;
    for _ in 0..warmup {
        for case in cases.iter_mut() {
            std::hint::black_box((case.run)());
        }
    }
    let count = cases.len();
    let mut ns: Vec<Vec<u64>> = vec![Vec::with_capacity(rounds); count];
    let mut counters: Vec<Option<BTreeMap<&'static str, u64>>> = vec![None; count];
    let mut metrics: Vec<BTreeMap<&'static str, Vec<f64>>> = vec![BTreeMap::new(); count];
    for round in 0..rounds {
        for step in 0..count {
            let at = (round + step) % count;
            let case = &mut cases[at];
            let started = Instant::now();
            let sample = (case.run)();
            let elapsed = sample.elapsed.unwrap_or_else(|| started.elapsed());
            ns[at].push(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
            match &counters[at] {
                None => counters[at] = Some(sample.counters),
                Some(first) => assert_eq!(
                    first, &sample.counters,
                    "row `{}`: a work counter changed between samples of one run. A counter \
                     that varies is a metric: report it with `.metric(..)`",
                    case.name
                ),
            }
            for (name, value) in sample.metrics {
                metrics[at].entry(name).or_default().push(value);
            }
        }
    }
    cases
        .into_iter()
        .zip(ns)
        .zip(counters)
        .zip(metrics)
        .map(|(((case, mut ns), counters), metrics)| {
            ns.sort_unstable();
            Row {
                name: case.name,
                time_gated: case.time_gated,
                ns,
                counters: counters.unwrap_or_default(),
                metrics: metrics
                    .into_iter()
                    .map(|(name, mut values)| (name, median(&mut values)))
                    .collect(),
            }
        })
        .collect()
}

/// One case on its own (`interleave` of one).
pub fn measure(case: Case<'_>, rounds: usize, warmup: usize) -> Row {
    interleave(vec![case], rounds, warmup)
        .pop()
        .expect("one case, one row")
}

// ---------------------------------------------------------------------------
// Degraded answers

/// Walks `value` for objects carrying `"ok": false` (a facet that did not
/// run). Returns the dotted key path of each, and the number of `"ok": true`
/// objects seen. A bench times the work, so a degraded answer aborts it via
/// [`require_no_failed_facets`] instead of being timed.
pub fn facet_walk(value: &Value) -> (Vec<String>, u64) {
    fn walk(value: &Value, path: &mut Vec<String>, failed: &mut Vec<String>, passed: &mut u64) {
        match value {
            Value::Object(map) => {
                match map.get("ok") {
                    Some(Value::Bool(false)) => failed.push(path.join(".")),
                    Some(Value::Bool(true)) => *passed += 1,
                    _ => {}
                }
                for (key, child) in map {
                    path.push(key.clone());
                    walk(child, path, failed, passed);
                    path.pop();
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    path.push(index.to_string());
                    walk(child, path, failed, passed);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let (mut failed, mut passed) = (Vec::new(), 0);
    walk(value, &mut Vec::new(), &mut failed, &mut passed);
    (failed, passed)
}

/// Aborts the run, naming `what` and every failing facet, when `payload` has
/// an `ok: false` facet. Returns the number of `ok: true` facets otherwise.
pub fn require_no_failed_facets(what: &str, payload: &Value) -> u64 {
    let (failed, passed) = facet_walk(payload);
    if !failed.is_empty() {
        panic!(
            "{what}: facet(s) reported ok:false [{}], so this run would time a degraded answer, \
             not the work. Payload: {}",
            failed.join(", "),
            serde_json::to_string(payload).unwrap_or_default()
        );
    }
    passed
}

// ---------------------------------------------------------------------------
// Host snapshot

/// The host-wide readings the repo's own sampler takes, as JSON. `null` on a
/// platform with no sampler, which is stated rather than dressed up as calm.
fn host_snapshot() -> Value {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use gitpulse_lib::sysload::{OsSensor, Sensor};
        let dir = std::env::temp_dir();
        let sample = OsSensor::new(&dir).sample();
        serde_json::to_value(sample).unwrap_or(Value::Null)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Value::Null
    }
}

pub fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// 1-minute load average from a [`host_snapshot`], when it was readable.
fn load1(host: &Value) -> Option<f64> {
    host.pointer("/load/value/0").and_then(Value::as_f64)
}

/// The runner class a baseline is keyed on: `GITPULSE_BENCH_RUNNER_CLASS`
/// (the workflow sets it to the runner label) or `local-<os>-<arch>`.
pub fn runner_class() -> String {
    std::env::var("GITPULSE_BENCH_RUNNER_CLASS")
        .unwrap_or_else(|_| format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

fn git_output(args: &[&str]) -> Option<String> {
    use gitpulse_lib::procguard::LockedSpawn;
    let out = std::process::Command::new("git")
        .args(args)
        .output_locked()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `GITHUB_SHA` on CI, else the checkout's `HEAD`. `None` is recorded as
/// `null`; it is never invented.
pub fn commit() -> (Option<String>, Option<bool>) {
    if let Ok(sha) = std::env::var("GITHUB_SHA") {
        return (Some(sha), Some(false));
    }
    let sha = git_output(&["rev-parse", "HEAD"]);
    let dirty =
        git_output(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    (sha, dirty)
}

// ---------------------------------------------------------------------------
// Report

/// Collects rows, prints a table, writes the JSON.
pub struct Report {
    bench: &'static str,
    pub opts: Opts,
    host_start: Value,
    rows: Vec<Row>,
    /// Rows measured and printed but never written to the JSON report; see
    /// [`Report::extend_unrecorded`].
    unrecorded: Vec<Row>,
    notes: Vec<String>,
    started: Instant,
}

impl Report {
    pub fn new(bench: &'static str) -> Self {
        let opts = Opts::from_args(bench);
        let host_start = host_snapshot();
        println!(
            "bench    : {bench}{}",
            if opts.smoke { " (smoke)" } else { "" }
        );
        println!("cpus     : {}", cpu_count());
        if let Some(load) = load1(&host_start) {
            let cpus = cpu_count() as f64;
            println!(
                "load avg : {load:.1} on {cpus} cpus ({:.1}x){}",
                load / cpus,
                if load / cpus > 1.0 {
                    "  <- LOADED HOST: absolute times are inflated; compare min and counters"
                } else {
                    ""
                }
            );
        }
        Self {
            bench,
            opts,
            host_start,
            rows: Vec::new(),
            unrecorded: Vec::new(),
            notes: Vec::new(),
            started: Instant::now(),
        }
    }

    pub fn extend(&mut self, rows: Vec<Row>) {
        self.rows.extend(rows);
    }

    /// Rows that are printed beside the others but never written to the JSON,
    /// so they can never be in a baseline written from that report: a row that
    /// exists only in some runs (a comparison against a binary built before a
    /// change) would otherwise make every later run fail with "baselined row is
    /// missing from this run".
    pub fn extend_unrecorded(&mut self, rows: Vec<Row>) {
        self.unrecorded.extend(rows);
    }

    pub fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    /// A line of context that travels with the JSON (fixture shape, a gap).
    pub fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        println!("note     : {note}");
        self.notes.push(note);
    }

    /// Prints the table and writes the JSON. Consumes the report.
    pub fn finish(self) {
        let width = self
            .rows
            .iter()
            .chain(&self.unrecorded)
            .map(|r| r.name.len())
            .max()
            .unwrap_or(0)
            .max(9);
        println!(
            "\n{:<width$}  {:>4}  {:>10}  {:>10}  {:>10}  {:>10}  counters / metrics",
            "row", "n", "min", "p50", "p95", "max"
        );
        println!("{}", "-".repeat(width + 60));
        for row in self.rows.iter().chain(&self.unrecorded) {
            let counters: Vec<String> = row
                .counters
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let metrics: Vec<String> = row
                .metrics
                .iter()
                .map(|(k, v)| format!("{k}~{v:.2}"))
                .collect();
            println!(
                "{:<width$}  {:>4}  {:>10}  {:>10}  {:>10}  {:>10}  {} {}",
                row.name,
                row.ns.len(),
                format!("{:.2?}", row.min()),
                format!("{:.2?}", row.p50()),
                format!("{:.2?}", row.p95()),
                format!("{:.2?}", row.max()),
                counters.join(" "),
                metrics.join(" "),
            );
        }
        let host_end = host_snapshot();
        let (commit, dirty) = commit();
        let rows: BTreeMap<&str, Value> = self
            .rows
            .iter()
            .map(|row| (row.name.as_str(), row.to_json()))
            .collect();
        let report = json!({
            "schema": 1,
            "bench": self.bench,
            "commit": commit,
            "dirty": dirty,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpu_count": cpu_count(),
            "runner_class": runner_class(),
            "smoke": self.opts.smoke,
            "wall_seconds": self.started.elapsed().as_secs_f64(),
            "host": { "start": self.host_start, "end": host_end },
            "notes": self.notes,
            "rows": rows,
        });
        if let Some(path) = &self.opts.json {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create the JSON output directory");
            }
            std::fs::write(
                path,
                serde_json::to_string_pretty(&report).expect("report serializes") + "\n",
            )
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
            println!("\njson     : {}", path.display());
        }
    }
}
