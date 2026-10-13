//! The watcher's per-event classifier over a 10k-event burst.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench watcher`.
//!
//! `record_event_signal` decides, for every filesystem event the debounce loop
//! receives, whether it names a change worth a refresh and of what class. It
//! runs on the watch thread, so its cost per event is the ceiling on how fast a
//! storm (a `git checkout`, a build, an editor's save-all) can be drained
//! before events queue behind it.
//!
//! The burst is generated and deterministic: 10,000 modify events spread over
//! source files, build output, ignored directories, git internals and
//! generated state, classified against `IgnoreRules` populated with 600 ignored
//! directory prefixes (the shape `git ls-files -o -i --directory` returns for a
//! repository with a few dozen build trees). Work counters (admitted, dropped,
//! paths recorded) sit next to the time so a change that classifies differently
//! cannot pass as a speedup. `events_per_s` is the headline figure.
//!
//! The classifier is reached through `watcher::bench_seam`, which calls the
//! same function the watch loop calls.

mod common;

use common::{interleave, Case, Report, Sample};
use gitpulse_lib::watcher::bench_seam::StormClassifier;
use notify::event::{DataChange, ModifyKind};
use notify::{Event, EventKind};
use std::path::{Path, PathBuf};

const EVENTS: usize = 10_000;
const IGNORED_PREFIXES: usize = 600;

fn modify(path: PathBuf) -> Event {
    Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(path)
}

/// 10k events whose mix is fixed by index, not by a random generator.
fn burst(worktree: &Path) -> Vec<Event> {
    (0..EVENTS)
        .map(|i| {
            let path = match i % 20 {
                // 60% ordinary source edits under 200 directories.
                0..=11 => worktree.join(format!("src/mod{:03}/file{i}.rs", i % 200)),
                // 15% build output.
                12..=14 => worktree.join(format!("target/debug/deps/lib{i}.rlib")),
                // 10% inside a directory the ignore rules cover.
                15 | 16 => worktree.join(format!("scratch{:03}/out/{i}.log", i % IGNORED_PREFIXES)),
                // 5% git internals, 5% generated state a refresh itself writes.
                17 => worktree
                    .join(".git")
                    .join(format!("objects/{:02x}/{i:038x}", i % 256)),
                18 => worktree.join(format!(".devcouncil/codeintel/part{}.sqlite-wal", i % 7)),
                // 5% documents, which add a second change class.
                _ => worktree.join(format!("docs/page{i}.md")),
            };
            modify(path)
        })
        .collect()
}

fn main() {
    let mut report = Report::new("watcher");
    let worktree = tempfile::tempdir().expect("temp dir");
    let root = worktree.path();
    std::fs::create_dir_all(root.join(".git")).expect("git dir");

    // `git ls-files -o -i --directory -z` output: NUL-separated, trailing '/'.
    let ignored: String = (0..IGNORED_PREFIXES)
        .map(|n| format!("scratch{n:03}/\0"))
        .collect();
    let (classifier, kept) = StormClassifier::new(root, &root.join(".git"), &ignored);
    assert_eq!(
        kept, IGNORED_PREFIXES,
        "the fixture's ignore list was not fully parsed"
    );
    let events = burst(root);
    report.note(format!(
        "{EVENTS} generated events against {kept} ignored-directory prefixes (IgnoreRules populated)"
    ));

    let case = Case::new(format!("record_event_signal {EVENTS}-event burst"), || {
        let started = std::time::Instant::now();
        let outcome = classifier.classify(&events);
        let elapsed = started.elapsed();
        std::hint::black_box(&outcome);
        Sample::default()
            .counter("events", EVENTS as u64)
            .counter("admitted", outcome.admitted)
            .counter("dropped", outcome.dropped)
            .counter("paths_recorded", outcome.paths_recorded)
            .counter("kinds", outcome.kinds)
            .counter("truncated", u64::from(outcome.truncated))
            .metric("events_per_s", EVENTS as f64 / elapsed.as_secs_f64())
            .elapsed(elapsed)
    });
    let rows = interleave(vec![case], report.opts.samples(60), report.opts.warmup(5));
    report.extend(rows);
    report.finish();
}
