//! Link-preserving rename: what each rewritten link says, where, and to whom.

use super::*;
use crate::engine::git_writer::MovePlan;
use crate::test_support::{git_in, git_repo, write};

/// A committed repository holding exactly `files`.
fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = git_repo();
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "fixture"]);
    dir
}

fn read(dir: &tempfile::TempDir, rel: &str) -> String {
    std::fs::read_to_string(dir.path().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Notes in folders, with a markdown link to the moved note from each place.
fn nested_fixture_repo() -> tempfile::TempDir {
    repo_with(&[
        (
            "docs/Guide.md",
            "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n",
        ),
        (
            "docs/Intro.md",
            "# Intro\n\nBack to [Guide](Guide.md).\n\n```md\n[[Roadmap]] [f](Roadmap.md)\n```\n",
        ),
        (
            "docs/Roadmap.md",
            "# Roadmap\n\nSee [guide](Guide.md#top).\n",
        ),
        ("README.md", "# Repo\n\n[R](docs/Roadmap.md)\n"),
    ])
}

#[test]
fn a_sibling_link_keeps_its_directory_when_the_target_is_renamed_in_place() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "docs/Plan.md", |_| Ok(())).expect("rename");

    assert!(
        read(&dir, "docs/Guide.md").contains("[x](Plan.md)"),
        "{}",
        read(&dir, "docs/Guide.md")
    );
    let built = load_vault(path, true).unwrap();
    let resolved = built
        .vault
        .resolve_from("docs/Guide.md", "Plan.md", None)
        .expect("the rewritten link resolves");
    assert_eq!(resolved.path, "docs/Plan.md");
    assert!(
        built.vault.broken_links().is_empty(),
        "{:?}",
        built.vault.broken_links()
    );
    // The fenced sample still says what it said.
    assert!(read(&dir, "docs/Intro.md").contains("```md\n[[Roadmap]] [f](Roadmap.md)\n```"));
}

#[test]
fn a_link_follows_the_target_into_another_directory_relative_to_the_linking_note() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");

    assert!(
        read(&dir, "docs/Guide.md").contains("[x](../plans/Roadmap.md)"),
        "{}",
        read(&dir, "docs/Guide.md")
    );
    assert!(
        read(&dir, "README.md").contains("[R](plans/Roadmap.md)"),
        "{}",
        read(&dir, "README.md")
    );
    let built = load_vault(path, true).unwrap();
    assert!(built.vault.broken_links().is_empty());
    assert_eq!(
        built
            .vault
            .resolve_from("docs/Guide.md", "../plans/Roadmap.md", None)
            .unwrap()
            .path,
        "plans/Roadmap.md"
    );
}

/// Every markdown link before the rename, followed to its note, ends at the
/// same note after it (the moved note having changed its path), and the set of
/// broken links is the one it was.
fn assert_rename_preserves_every_link(
    dir: &tempfile::TempDir,
    from: &str,
    to: &str,
) -> (Vec<String>, String) {
    let path = dir.path().to_str().unwrap();
    let before = load_vault(path, true).unwrap();
    let mut expected: Vec<(String, usize, Option<String>)> = Vec::new();
    for note in before.vault.notes() {
        for (position, link) in note.links.iter().enumerate() {
            let landed = before.vault.resolve_link(&note.path, link).map(|r| {
                if r.path == from {
                    to.to_string()
                } else {
                    r.path
                }
            });
            let source = if note.path == from { to } else { &note.path };
            expected.push((source.to_string(), position, landed));
        }
    }
    let mut broken_before: Vec<(String, String)> = before
        .vault
        .broken_links()
        .into_iter()
        .map(|(s, t)| (if s == from { to.to_string() } else { s }, t))
        .collect();
    broken_before.sort();

    let outcome = rename_doc(path, from, to, |_| Ok(())).expect("rename");

    let after = refresh_and_load(path);
    for (source, position, landed) in expected {
        let note = after
            .vault
            .note(&source)
            .unwrap_or_else(|| panic!("{source} missing after the rename"));
        let link = note
            .links
            .get(position)
            .unwrap_or_else(|| panic!("{source} lost link {position}"));
        let now = after.vault.resolve_link(&source, link).map(|r| r.path);
        assert_eq!(
            now, landed,
            "{source} link {position} ({:?}) moved to a different note",
            link.target
        );
    }
    let mut broken_after = after.vault.broken_links();
    broken_after.sort();
    assert_eq!(
        broken_after, broken_before,
        "the set of broken links changed"
    );
    (outcome.rewritten_paths, read(dir, to))
}

fn refresh_and_load(path: &str) -> Arc<CachedVault> {
    invalidate(path);
    load_vault(path, true).unwrap()
}

#[test]
fn every_link_spelling_still_reaches_its_note_after_a_move_between_directories() {
    let dir = repo_with(&[
        (
            "docs/deep/Target.md",
            "# Target\n\n## intro\n\n[self](Target.md#intro) [sib](Sib.md) [up](../Up.md#h) \
             [root](/README.md) [t](Sib.md \"title\") [n](<Sib.md>)\n",
        ),
        (
            "docs/deep/Sib.md",
            "[a](Target.md \"t\") [b](<Target.md>) [c](./Target.md#intro) [d](../deep/Target.md) \
             [e](/docs/deep/Target.md) [f](Target.md#intro) [g](%54arget.md) [h](Target.md 'single')\n",
        ),
        (
            "docs/Up.md",
            "[a](deep/Target.md) [b](./deep/Target.md \"title\") [c](<deep/Target.md#intro>)\n",
        ),
        ("Page.mdx", "[a](docs/deep/Target.md#intro)\n"),
        ("README.md", "[a](docs/deep/Target.md) [b](docs/Up.md)\n"),
        ("docs/Wiki.md", "[[Target]] and [[docs/deep/Target]]\n"),
    ]);
    let (rewritten, moved) =
        assert_rename_preserves_every_link(&dir, "docs/deep/Target.md", "archive/Moved Target.md");
    assert!(
        rewritten.contains(&"docs/deep/Sib.md".to_string()),
        "{rewritten:?}"
    );
    assert!(moved.contains("[self](Moved%20Target.md#intro)"), "{moved}");
    assert!(moved.contains("[sib](../docs/deep/Sib.md)"), "{moved}");
    assert!(
        moved.contains("[t](../docs/deep/Sib.md \"title\")"),
        "{moved}"
    );
    assert!(moved.contains("[n](<../docs/deep/Sib.md>)"), "{moved}");
    assert!(moved.contains("[root](/README.md)"), "{moved}");
    // Titles and the bracketed form are carried through byte for byte.
    let sib = read(&dir, "docs/deep/Sib.md");
    assert!(sib.contains("\"t\")"), "{sib}");
    assert!(sib.contains("'single')"), "{sib}");
}

#[test]
fn a_move_that_changes_the_extension_still_resolves() {
    let dir = repo_with(&[
        ("a/Note.md", "# Note\n"),
        ("b/Link.md", "[n](../a/Note.md#top) [m](../a/Note.md)\n"),
    ]);
    assert_rename_preserves_every_link(&dir, "a/Note.md", "c/Note.mdx");
    assert!(read(&dir, "b/Link.md").contains("../c/Note.mdx#top"));
}

#[test]
fn the_moved_notes_own_relative_links_are_re_expressed_from_its_new_folder() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n[b](B.md)\n"), ("docs/B.md", "# B\n")]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| Ok(())).expect("rename");

    assert_eq!(read(&dir, "notes/A.md"), "# A\n\n[b](../docs/B.md)\n");
    assert!(
        outcome.rewritten_paths.contains(&"notes/A.md".to_string()),
        "{outcome:?}"
    );
    let built = load_vault(path, true).unwrap();
    assert_eq!(
        built
            .vault
            .resolve_from("notes/A.md", "../docs/B.md", None)
            .unwrap()
            .path,
        "docs/B.md"
    );
    assert!(built.vault.broken_links().is_empty());
}

#[test]
fn a_same_folder_rename_leaves_the_moved_note_byte_identical() {
    let text = "# A\n\n[b](B.md) [w](./B.md#x \"t\") [[B]] [r](/docs/B.md)\r\nend\r\n";
    let dir = repo_with(&[("docs/A.md", text), ("docs/B.md", "# B\n\n## x\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "docs/Z.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "docs/Z.md"), text);
}

#[test]
fn a_self_link_in_the_moved_note_follows_it() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n## intro\n\n[top](A.md#intro)\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "notes/Renamed.md", |_| Ok(())).expect("rename");

    assert_eq!(
        read(&dir, "notes/Renamed.md"),
        "# A\n\n## intro\n\n[top](Renamed.md#intro)\n"
    );
    let built = load_vault(path, true).unwrap();
    assert_eq!(
        built
            .vault
            .resolve_from("notes/Renamed.md", "Renamed.md", None)
            .unwrap()
            .path,
        "notes/Renamed.md"
    );
}

#[test]
fn a_self_wikilink_follows_a_stem_change() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n[[A]] and [[A#x]]\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "docs/Alpha.md", |_| Ok(())).expect("rename");
    assert_eq!(
        read(&dir, "docs/Alpha.md"),
        "# A\n\n[[Alpha]] and [[Alpha#x]]\n"
    );
}

#[test]
fn crlf_notes_change_only_inside_the_link() {
    let original = "# Guide\r\n\r\nSee [x](Roadmap.md) now.\r\nLast line\r\n";
    let dir = repo_with(&[("docs/Guide.md", original), ("docs/Roadmap.md", "# R\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        std::fs::read(dir.path().join("docs/Guide.md")).unwrap(),
        original
            .replace("(Roadmap.md)", "(../plans/Roadmap.md)")
            .into_bytes()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_case_only_rename_on_a_case_insensitive_volume_is_refused() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let probe = dir.path().join("docs/GUIDE.md");
    if !probe.exists() {
        return; // a case-sensitive volume: the situation does not arise
    }
    let refused = rename_doc(path, "docs/Roadmap.md", "docs/ROADMAP.md", |_| Ok(()));
    assert!(refused.is_err(), "{refused:?}");
    assert!(dir.path().join("docs/Roadmap.md").exists());
}

/// What a counting policy gate saw during one rename.
#[derive(Debug, Clone, Default)]
struct GateTally {
    /// Every policy call: the `git mv` line, the source delete, the destination
    /// write and one per rewritten note.
    calls: usize,
    /// Distinct file paths those calls judged (the `git mv` line is a command,
    /// not a file).
    files_judged: usize,
    /// How many of those files were rewritten notes.
    rewrites_judged: usize,
    /// Wall time spent inside the gate, start to finish.
    gate_wall: Duration,
    /// The sum of each call's own time. Above `gate_wall` when calls overlap.
    call_sum: Duration,
    slowest_call: Duration,
}

/// One rename of `from` to `to` through a gate that counts and times what it is
/// asked, with `latency` slept inside every call to stand in for the sidecar
/// round trip. The gate makes the same calls, in the same shape, as
/// `commands::knowledge::docs_rename_guarded`: the move, the source delete, the
/// destination write, then every other rewritten note through the fan-out
/// `harness::guard_files` uses (`judge_in_order`), `workers` at a time. Returns
/// the outcome, what the gate saw, and the whole rename's wall time.
fn counting_gate_rename(
    path: &str,
    from: &str,
    to: &str,
    latency: Duration,
    workers: usize,
) -> (DocRenameOutcome, GateTally, Duration) {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    let calls = AtomicUsize::new(0);
    let call_nanos = AtomicU64::new(0);
    let slowest_nanos = AtomicU64::new(0);
    let files = Mutex::new(Vec::<String>::new());
    let judge = |file: Option<&str>| {
        let started = Instant::now();
        if !latency.is_zero() {
            std::thread::sleep(latency);
        }
        calls.fetch_add(1, Ordering::AcqRel);
        if let Some(file) = file {
            files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(file.to_string());
        }
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        call_nanos.fetch_add(nanos, Ordering::AcqRel);
        slowest_nanos.fetch_max(nanos, Ordering::AcqRel);
    };

    let mut gate_wall = Duration::ZERO;
    let mut rewrites_judged = 0;
    let started = Instant::now();
    let outcome = rename_doc(path, from, to, |plan| {
        let gate_started = Instant::now();
        judge(None);
        judge(Some(plan.from));
        judge(Some(plan.to));
        let rewrites: Vec<&str> = plan
            .rewrites
            .iter()
            .map(String::as_str)
            .filter(|rewrite| *rewrite != plan.to)
            .collect();
        let verdicts =
            crate::harness::judge_files_for_bench(&rewrites, workers, |file| judge(Some(file)));
        assert!(
            verdicts.iter().all(Option::is_some),
            "a rewrite was never judged"
        );
        rewrites_judged = rewrites.len();
        gate_wall = gate_started.elapsed();
        Ok(())
    })
    .expect("rename");
    let wall = started.elapsed();

    let mut judged = files
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let files_judged = judged.len();
    judged.sort();
    judged.dedup();
    assert_eq!(judged.len(), files_judged, "a file was judged twice");
    let tally = GateTally {
        calls: calls.load(Ordering::Acquire),
        files_judged,
        rewrites_judged,
        gate_wall,
        call_sum: Duration::from_nanos(call_nanos.load(Ordering::Acquire)),
        slowest_call: Duration::from_nanos(slowest_nanos.load(Ordering::Acquire)),
    };
    // The move is one call and is not a file; every other call judged one file.
    assert_eq!(tally.calls, tally.files_judged + 1, "{tally:?}");
    assert_eq!(tally.files_judged, tally.rewrites_judged + 2, "{tally:?}");
    (outcome, tally, wall)
}

#[test]
fn the_counting_gate_sees_one_call_per_path_a_hub_rename_writes() {
    let dir = hub_repo(40);
    let path = dir.path().to_str().unwrap();
    let (outcome, one, _) = counting_gate_rename(
        path,
        "docs/Roadmap.md",
        "plans/Roadmap.md",
        Duration::ZERO,
        1,
    );
    assert_eq!(outcome.rewritten_paths.len(), 40, "{outcome:?}");
    // The move, the source, the destination, and each of the 40 linking notes.
    assert_eq!(one.calls, 43, "{one:?}");
    assert_eq!(one.files_judged, 42, "{one:?}");
    assert_eq!(one.rewrites_judged, 40, "{one:?}");

    let workers = crate::harness::production_guard_workers();
    let (_, many, _) = counting_gate_rename(
        path,
        "plans/Roadmap.md",
        "docs/Roadmap.md",
        Duration::ZERO,
        workers,
    );
    assert_eq!(
        (many.calls, many.files_judged),
        (one.calls, one.files_judged),
        "the worker count changes how long judging takes, not what is judged"
    );
}

/// What the per-file policy cost of a hub rename is, with the gate counting and
/// timing instead of waving every path through, on the synthetic 5,000-note
/// corpus the refresh benchmark uses. Four arms, run interleaved and reported as
/// the min of `RUNS`: a simulated per-call latency of 0 ms and 2 ms, each with 1
/// worker and with the production worker count (`GUARD_FILES_CONCURRENCY`).
/// The latency is slept inside each call; it stands in for the sidecar round
/// trip and is not a measurement of it. The in-memory pre-check is timed on its
/// own as before.
///
/// `cargo test --release --manifest-path src-tauri/Cargo.toml --lib
/// docs::rename_tests::docs_rename_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "manual timing workload"]
fn docs_rename_benchmark() {
    use std::time::Instant;
    const RUNS: usize = 5;
    fn min_ms(runs: usize, mut run: impl FnMut()) -> f64 {
        (0..runs)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min)
    }
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let dir = git_repo();
    for n in 0..MAX_DOC_FILES {
        write(
            dir.path(),
            &format!("notes/{:02}/note-{n:05}.md", n % 50),
            &format!("# Note {n}\n[[note-00000]] [m](../00/note-00000.md) #corpus body-{n}\n"),
        );
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "benchmark corpus"]);
    let path = dir.path().to_str().unwrap();
    probe::set_clock_ahead(Duration::from_secs(5));
    let repo = validate_repo(path).unwrap();
    let built = load_vault(path, true).unwrap();
    load_vault(path, true).unwrap();

    let production = crate::harness::production_guard_workers();
    // (simulated latency per call, workers)
    let arms = [
        (Duration::ZERO, 1),
        (Duration::ZERO, production),
        (Duration::from_millis(2), 1),
        (Duration::from_millis(2), production),
    ];

    // A note nothing links at, and the one every note links at.
    for (label, from, to) in [
        ("leaf", "notes/07/note-00007.md", "moved/note-00007.md"),
        ("hub", "notes/00/note-00000.md", "moved/note-00000.md"),
    ] {
        let mut edits = Vec::new();
        let plan_ms = min_ms(RUNS, || {
            edits = super::rename::plan_edits(&built.vault, from, to).unwrap();
        });
        let precheck_ms = min_ms(RUNS, || {
            super::rename::links_the_rename_breaks(&repo, &built.vault, from, to, &edits);
        });
        println!(
            "docs_rename_bench target={label} notes={} rewrites={} plan_ms={plan_ms:.2} \
             precheck_ms={precheck_ms:.2}",
            built.status.note_count,
            edits.len(),
        );

        // Interleaved: each round runs every arm once, so a quiet or busy
        // stretch of the host lands on all of them.
        let (mut a, mut b) = (from.to_string(), to.to_string());
        let mut best: Vec<Option<(Duration, GateTally)>> = vec![None; arms.len()];
        for _ in 0..RUNS {
            for (slot, (latency, workers)) in best.iter_mut().zip(arms) {
                let (_, tally, wall) = counting_gate_rename(path, &a, &b, latency, workers);
                std::mem::swap(&mut a, &mut b);
                assert!(
                    wall < crate::test_support::coverage_relaxed(Duration::from_secs(900)),
                    "{label} rename took {wall:?}"
                );
                if slot.as_ref().is_none_or(|(best_wall, _)| wall < *best_wall) {
                    *slot = Some((wall, tally));
                }
            }
        }
        let best: Vec<(Duration, GateTally)> = best.into_iter().map(Option::unwrap).collect();
        for ((latency, workers), (wall, tally)) in arms.iter().zip(&best) {
            println!(
                "docs_rename_policy_bench target={label} latency_ms={} workers={workers} \
                 policy_calls={} files_judged={} rewrites_judged={} gate_wall_ms={:.2} \
                 gate_call_sum_ms={:.2} slowest_call_ms={:.3} rename_ms={:.2} \
                 gate_share={:.1}% runs={RUNS}",
                latency.as_millis(),
                tally.calls,
                tally.files_judged,
                tally.rewrites_judged,
                ms(tally.gate_wall),
                ms(tally.call_sum),
                ms(tally.slowest_call),
                ms(*wall),
                100.0 * ms(tally.gate_wall) / ms(*wall),
            );
        }
        // Every arm makes the same calls; only how long they take differs.
        for (_, tally) in &best {
            assert_eq!(
                (tally.calls, tally.files_judged),
                (best[0].1.calls, best[0].1.files_judged)
            );
        }
        // With a real per-call cost, the production fan-out must beat one
        // worker; without one there is nothing to overlap and no claim is made.
        let (serial, parallel) = (&best[2].1, &best[3].1);
        if parallel.rewrites_judged > production {
            assert!(
                parallel.gate_wall < crate::test_support::coverage_relaxed(serial.gate_wall),
                "{production} workers took {:?} against {:?} for one",
                parallel.gate_wall,
                serial.gate_wall
            );
        }
        println!(
            "docs_rename_policy_bench target={label} workers_speedup_at_2ms={:.2}x",
            ms(serial.gate_wall) / ms(parallel.gate_wall).max(f64::MIN_POSITIVE)
        );
    }
}

/// One hub rename through the REAL policy path against a live sidecar: the
/// source delete, the destination write, then every other rewritten note
/// through `harness::guard_files`' walk (scope once, judge, record in the
/// ledger, gate), `workers` at a time. The `git mv` command line is not judged
/// here (it is one call of about 5,000). Returns the gate's wall time, the
/// whole rename's, and how many rewrites were judged.
fn live_gate_rename(
    path: &str,
    from: &str,
    to: &str,
    workers: usize,
) -> (Duration, Duration, usize) {
    use std::time::Instant;
    let mut gate_wall = Duration::ZERO;
    let mut verdicts = 0;
    let started = Instant::now();
    rename_doc(path, from, to, |plan| {
        let gate_started = Instant::now();
        crate::harness::guard_file(path, plan.from, "delete")?;
        crate::harness::guard_file(path, plan.to, "modify")?;
        let rewrites: Vec<&str> = plan
            .rewrites
            .iter()
            .map(String::as_str)
            .filter(|rewrite| *rewrite != plan.to)
            .collect();
        verdicts =
            crate::harness::guard_files_with_workers_for_bench(path, &rewrites, "modify", workers)?
                .len();
        gate_wall = gate_started.elapsed();
        Ok(())
    })
    .expect("rename through the live sidecar");
    (gate_wall, started.elapsed(), verdicts)
}

/// What the REAL Manvi sidecar round trip costs a hub rename, against the
/// simulated arms of `docs_rename_benchmark`, on the same 5,000-note hub
/// corpus. Needs a real `manvi`: `GITPULSE_MANVI_BIN` (or
/// `GITPULSE_BENCH_MANVI`) names it. The sidecar runs under the test build's
/// hermetic HOME and profile, never the user's.
///
/// Per round, in rotating order: a no-op gate (0 ms, production workers: the
/// harness's own overhead), the simulated 2 ms gate (the control), the live
/// gate at production workers and at 1 worker (each a full rename), and two
/// rename-free passes that time every sidecar call (`check_file` alone, no
/// ledger record) at 1 and at production workers. Reports the min of `RUNS`.
///
/// `GITPULSE_MANVI_BIN=$(command -v manvi) cargo test --release
/// --manifest-path src-tauri/Cargo.toml --lib
/// docs::rename_tests::docs_rename_live_sidecar_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "needs a real manvi (GITPULSE_MANVI_BIN); a measurement, not a check"]
fn docs_rename_live_sidecar_benchmark() {
    use std::time::Instant;
    const RUNS: usize = 5;
    let manvi = std::env::var("GITPULSE_BENCH_MANVI")
        .or_else(|_| std::env::var("GITPULSE_MANVI_BIN"))
        .expect("GITPULSE_MANVI_BIN names the manvi to measure");
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let serial = crate::harness::sidecar::test_serial();
    let _binary = crate::harness::sidecar::bind_test_binary(&serial, manvi.clone());

    let dir = git_repo();
    for n in 0..MAX_DOC_FILES {
        write(
            dir.path(),
            &format!("notes/{:02}/note-{n:05}.md", n % 50),
            &format!("# Note {n}\n[[note-00000]] [m](../00/note-00000.md) #corpus body-{n}\n"),
        );
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "benchmark corpus"]);
    let path = dir.path().to_str().unwrap();
    probe::set_clock_ahead(Duration::from_secs(5));
    load_vault(path, true).unwrap();
    load_vault(path, true).unwrap();
    let production = crate::harness::production_guard_workers();
    println!(
        "docs_rename_live_bench manvi={manvi} load_before={:?}",
        crate::test_support::env::host_load()
    );

    // The first call spawns the sidecar; keep that out of the figures.
    let cold = Instant::now();
    crate::harness::guard_file(path, "notes/00/note-00000.md", "modify").expect("first call");
    println!(
        "docs_rename_live_bench cold_first_call_ms={:.2}",
        ms(cold.elapsed())
    );

    let (from, to) = ("notes/00/note-00000.md", "moved/note-00000.md");
    let files: Vec<String> = (1..MAX_DOC_FILES)
        .map(|n| format!("notes/{:02}/note-{n:05}.md", n % 50))
        .collect();
    let files: Vec<&str> = files.iter().map(String::as_str).collect();

    #[derive(Clone, Copy)]
    enum Arm {
        Noop,
        Sim2ms,
        Live(usize),
        Calls(usize),
    }
    let arms = [
        Arm::Noop,
        Arm::Sim2ms,
        Arm::Live(production),
        Arm::Live(1),
        Arm::Calls(1),
        Arm::Calls(production),
    ];
    let label = |arm: Arm| match arm {
        Arm::Noop => "noop_gate_0ms".to_string(),
        Arm::Sim2ms => "simulated_2ms".to_string(),
        Arm::Live(w) => format!("live_rename_w{w}"),
        Arm::Calls(w) => format!("live_calls_only_w{w}"),
    };
    // (gate wall, rename wall) of the best-by-rename run per rename arm;
    // (p50, p95, max, wall) minima per calls arm.
    let mut renames: Vec<Option<(Duration, Duration)>> = vec![None; arms.len()];
    let mut min_gate = vec![Duration::MAX; arms.len()];
    let mut judged = vec![0_usize; arms.len()];
    let mut calls: Vec<Option<[Duration; 4]>> = vec![None; arms.len()];
    let (mut a, mut b) = (from.to_string(), to.to_string());
    for round in 0..RUNS {
        for step in 0..arms.len() {
            let index = (step + round) % arms.len();
            let arm = arms[index];
            match arm {
                Arm::Noop | Arm::Sim2ms => {
                    let latency = match arm {
                        Arm::Noop => Duration::ZERO,
                        _ => Duration::from_millis(2),
                    };
                    let (_, tally, wall) = counting_gate_rename(path, &a, &b, latency, production);
                    std::mem::swap(&mut a, &mut b);
                    judged[index] = tally.rewrites_judged;
                    min_gate[index] = min_gate[index].min(tally.gate_wall);
                    if renames[index].is_none_or(|(_, best)| wall < best) {
                        renames[index] = Some((tally.gate_wall, wall));
                    }
                }
                Arm::Live(workers) => {
                    let (gate, wall, verdicts) = live_gate_rename(path, &a, &b, workers);
                    std::mem::swap(&mut a, &mut b);
                    assert!(verdicts > 0, "the live gate judged nothing");
                    judged[index] = verdicts;
                    min_gate[index] = min_gate[index].min(gate);
                    if renames[index].is_none_or(|(_, best)| wall < best) {
                        renames[index] = Some((gate, wall));
                    }
                }
                Arm::Calls(workers) => {
                    let started = Instant::now();
                    let (checked, _) =
                        crate::harness::time_file_checks_for_bench(path, &files, "modify", workers)
                            .expect("time the sidecar calls");
                    let mut each: Vec<Duration> = checked.into_iter().map(|(d, _)| d).collect();
                    let wall = started.elapsed();
                    judged[index] = each.len();
                    each.sort();
                    let pct = |q: f64| each[((each.len() as f64 * q) as usize).min(each.len() - 1)];
                    let sample = [pct(0.50), pct(0.95), *each.last().unwrap(), wall];
                    println!(
                        "docs_rename_live_bench round={round} arm={} calls={} p50_ms={:.3} \
                         p95_ms={:.3} max_ms={:.3} wall_ms={:.1}",
                        label(arm),
                        each.len(),
                        ms(sample[0]),
                        ms(sample[1]),
                        ms(sample[2]),
                        ms(wall),
                    );
                    calls[index] = Some(match calls[index] {
                        None => sample,
                        Some(best) => std::array::from_fn(|i| best[i].min(sample[i])),
                    });
                }
            }
        }
    }
    for (index, arm) in arms.iter().enumerate() {
        match (renames[index], calls[index]) {
            (Some((gate, wall)), _) => println!(
                "docs_rename_live_bench arm={} judged={} min_gate_ms={:.2} \
                 best_run_gate_ms={:.2} best_run_rename_ms={:.2} gate_share={:.1}% runs={RUNS}",
                label(*arm),
                judged[index],
                ms(min_gate[index]),
                ms(gate),
                ms(wall),
                100.0 * ms(gate) / ms(wall),
            ),
            (_, Some(s)) => println!(
                "docs_rename_live_bench arm={} calls={} p50_ms={:.3} p95_ms={:.3} \
                 max_ms={:.3} min_wall_ms={:.1} runs={RUNS}",
                label(*arm),
                judged[index],
                ms(s[0]),
                ms(s[1]),
                ms(s[2]),
                ms(s[3]),
            ),
            _ => unreachable!(),
        }
    }
    println!(
        "docs_rename_live_bench load_after={:?}",
        crate::test_support::env::host_load()
    );
}

/// One repository of the realistic-state benchmark: the 5,000-note hub corpus
/// in a given state of task binding and ledger history.
struct StateRepo {
    name: &'static str,
    _dir: tempfile::TempDir,
    path: String,
    from: String,
    to: String,
}

/// Builds the hub corpus in a fresh repository, then puts it in the state
/// asked for: `planned` binds it to a task whose declared scope lists those
/// paths, `ledger_events` appends that many prior events to its ledger. The
/// repository is canonicalised first so the binding and the checks agree on
/// its address.
fn state_repo(name: &'static str, planned: Option<&[String]>, ledger_events: usize) -> StateRepo {
    use std::time::Instant;
    let dir = git_repo();
    for n in 0..MAX_DOC_FILES {
        write(
            dir.path(),
            &format!("notes/{:02}/note-{n:05}.md", n % 50),
            &format!("# Note {n}\n[[note-00000]] [m](../00/note-00000.md) #corpus body-{n}\n"),
        );
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "benchmark corpus"]);
    let path = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    if let Some(planned) = planned {
        let db = crate::tasks::store_path(&path);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        let files: Vec<serde_json::Value> = planned
            .iter()
            .map(|p| serde_json::json!({"path": p, "allowed_change": "modify"}))
            .collect();
        crate::tasks::plant_task::plant(
            &store,
            "TASK-B",
            "in_progress",
            &serde_json::json!({ "planned_files": files }).to_string(),
        );
        crate::ledger::bindings::bind(&path, &path, "TASK-B").expect("bind the task");
    }
    let started = Instant::now();
    for i in 0..ledger_events {
        crate::ledger::append(crate::ledger::Draft {
            repo_path: path.clone(),
            action: "git.commit".to_string(),
            object: Some(format!("history-{i}")),
            actor_kind: Some(crate::ledger::ActorKind::Human),
            outcome: Some(crate::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("prior ledger event");
    }
    if ledger_events > 0 {
        println!(
            "docs_rename_state_bench state={name} ledger_events={ledger_events} seeded_ms={:.0}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    probe::set_clock_ahead(Duration::from_secs(5));
    load_vault(&path, true).unwrap();
    load_vault(&path, true).unwrap();
    StateRepo {
        name,
        _dir: dir,
        path,
        from: "notes/00/note-00000.md".to_string(),
        to: "moved/note-00000.md".to_string(),
    }
}

/// The real sidecar's cost under realistic state: a bound task scope (a
/// declared glob, and every note listed explicitly), a ledger with thousands of
/// prior events, and both. Per round, for every state in rotating order, three
/// arms in rotating order: the sidecar calls alone at 1 and at production
/// workers (no ledger record, no rename), and the full gate inside a hub rename
/// at production workers. Min of `RUNS`. A counting wrapper around `manvi`
/// logs every sidecar start, so a respawn during any run is counted rather
/// than inferred, and every verdict is classified so a sidecar that
/// short-circuited would show as one rule rather than as speed.
///
/// `GITPULSE_MANVI_BIN=$(command -v manvi) cargo test --release
/// --manifest-path src-tauri/Cargo.toml --lib
/// docs::rename_tests::docs_rename_realistic_state_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "needs a real manvi (GITPULSE_MANVI_BIN); a measurement, not a check"]
fn docs_rename_realistic_state_benchmark() {
    use std::collections::BTreeMap;
    use std::time::Instant;
    const RUNS: usize = 5;
    const LEDGER_EVENTS: usize = 20_000;
    let manvi = std::env::var("GITPULSE_BENCH_MANVI")
        .or_else(|_| std::env::var("GITPULSE_MANVI_BIN"))
        .expect("GITPULSE_MANVI_BIN names the manvi to measure");
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let serial = crate::harness::sidecar::test_serial();
    let scratch = tempfile::tempdir().unwrap();
    let starts = scratch.path().join("starts.log");
    let wrapper = crate::harness::counting_manvi_wrapper(scratch.path(), &manvi, &starts);
    let _binary = crate::harness::sidecar::bind_test_binary(&serial, wrapper);
    let started_count = || {
        std::fs::read_to_string(&starts)
            .map(|log| log.lines().count())
            .unwrap_or(0)
    };
    let production = crate::harness::production_guard_workers();
    println!(
        "docs_rename_state_bench manvi={manvi} load_before={:?}",
        crate::test_support::env::host_load()
    );

    let every_note: Vec<String> = (0..MAX_DOC_FILES)
        .map(|n| format!("notes/{:02}/note-{n:05}.md", n % 50))
        .chain(["moved/note-00000.md".to_string()])
        .collect();
    let glob = ["notes/**".to_string(), "moved/**".to_string()];
    let states = [
        state_repo("unbound_empty_ledger", None, 0),
        state_repo("bound_glob_scope", Some(&glob), 0),
        state_repo("bound_5000_listed", Some(&every_note), 0),
        state_repo("unbound_20k_ledger", None, LEDGER_EVENTS),
        state_repo("bound_glob_20k_ledger", Some(&glob), LEDGER_EVENTS),
    ];

    // Fast-path check: classify every verdict of one pass per state.
    for state in &states {
        let files: Vec<String> = (1..MAX_DOC_FILES)
            .map(|n| format!("notes/{:02}/note-{n:05}.md", n % 50))
            .collect();
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let (checked, scope_bytes) =
            crate::harness::time_file_checks_for_bench(&state.path, &files, "modify", 1)
                .expect("classify the verdicts");
        let mut classes: BTreeMap<String, usize> = BTreeMap::new();
        for (_, verdict) in &checked {
            *classes
                .entry(format!(
                    "status={:?} rule={:?} checked={} gate_failed={} degraded={:?} task_id={:?}",
                    verdict.status,
                    verdict.rule,
                    verdict.checked,
                    verdict.gate_failed(),
                    verdict.degraded,
                    verdict.task_id,
                ))
                .or_default() += 1;
        }
        assert!(checked.iter().all(|(_, v)| v.checked && !v.gate_failed()));
        println!(
            "docs_rename_state_bench state={} scope_bytes_per_request={scope_bytes} verdicts={classes:?}",
            state.name
        );
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Arm {
        Calls(usize),
        Full,
    }
    let arms = [Arm::Calls(1), Arm::Calls(production), Arm::Full];
    let mut calls: BTreeMap<(usize, usize), [Duration; 4]> = BTreeMap::new();
    let mut full: BTreeMap<usize, (Duration, Duration, Duration)> = BTreeMap::new();
    let mut swaps: Vec<(String, String)> = states
        .iter()
        .map(|s| (s.from.clone(), s.to.clone()))
        .collect();
    let starts_at_beginning = started_count();
    for round in 0..RUNS {
        for step in 0..states.len() {
            let si = (step + round) % states.len();
            let state = &states[si];
            for astep in 0..arms.len() {
                let arm = arms[(astep + round) % arms.len()];
                match arm {
                    Arm::Calls(workers) => {
                        let files: Vec<String> = (1..MAX_DOC_FILES)
                            .map(|n| format!("notes/{:02}/note-{n:05}.md", n % 50))
                            .collect();
                        let files: Vec<&str> = files.iter().map(String::as_str).collect();
                        let before = Instant::now();
                        let (checked, _) = crate::harness::time_file_checks_for_bench(
                            &state.path,
                            &files,
                            "modify",
                            workers,
                        )
                        .expect("time the sidecar calls");
                        let wall = before.elapsed();
                        let mut each: Vec<Duration> = checked.into_iter().map(|(d, _)| d).collect();
                        each.sort();
                        let pct =
                            |q: f64| each[((each.len() as f64 * q) as usize).min(each.len() - 1)];
                        let sample = [pct(0.50), pct(0.95), *each.last().unwrap(), wall];
                        println!(
                            "docs_rename_state_bench round={round} state={} arm=calls_w{workers} \
                             p50_ms={:.3} p95_ms={:.3} max_ms={:.3} wall_ms={:.1}",
                            state.name,
                            ms(sample[0]),
                            ms(sample[1]),
                            ms(sample[2]),
                            ms(wall),
                        );
                        let slot = calls.entry((si, workers)).or_insert(sample);
                        *slot = std::array::from_fn(|i| slot[i].min(sample[i]));
                    }
                    Arm::Full => {
                        let before = started_count();
                        let (a, b) = swaps[si].clone();
                        let (gate, wall, verdicts) =
                            live_gate_rename(&state.path, &a, &b, production);
                        swaps[si] = (b, a);
                        assert!(verdicts > 0, "the live gate judged nothing");
                        let respawns = started_count() - before;
                        println!(
                            "docs_rename_state_bench round={round} state={} arm=full_w{production} \
                             gate_ms={:.1} rename_ms={:.1} sidecar_starts_during={respawns}",
                            state.name,
                            ms(gate),
                            ms(wall),
                        );
                        let best = full.entry(si).or_insert((gate, wall, gate));
                        best.2 = best.2.min(gate);
                        if wall < best.1 {
                            best.0 = gate;
                            best.1 = wall;
                        }
                    }
                }
            }
        }
    }
    for (si, state) in states.iter().enumerate() {
        for workers in [1, production] {
            let s = calls[&(si, workers)];
            println!(
                "docs_rename_state_bench SUMMARY state={} arm=calls_w{workers} p50_ms={:.3} \
                 p95_ms={:.3} max_ms={:.3} min_wall_ms={:.1} runs={RUNS}",
                state.name,
                ms(s[0]),
                ms(s[1]),
                ms(s[2]),
                ms(s[3]),
            );
        }
        let (gate, wall, min_gate) = full[&si];
        println!(
            "docs_rename_state_bench SUMMARY state={} arm=full_w{production} min_gate_ms={:.1} \
             best_run_gate_ms={:.1} best_run_rename_ms={:.1} gate_share={:.1}% runs={RUNS}",
            state.name,
            ms(min_gate),
            ms(gate),
            ms(wall),
            100.0 * ms(gate) / ms(wall),
        );
    }
    println!(
        "docs_rename_state_bench sidecar_starts_total={} starts_before_timed_section={} \
         load_after={:?}",
        started_count(),
        starts_at_beginning,
        crate::test_support::env::host_load()
    );
    drop(states);
}

/// A repository where `count` notes link at `docs/Roadmap.md`.
fn hub_repo(count: usize) -> tempfile::TempDir {
    let dir = git_repo();
    write(dir.path(), "docs/Roadmap.md", "# Roadmap\n");
    for i in 0..count {
        write(dir.path(), &format!("docs/n{i:04}.md"), "[r](Roadmap.md)\n");
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "hub"]);
    dir
}

/// The git processes a rename starts do not grow with the notes it rewrites.
#[test]
fn a_hub_rename_stages_in_a_bounded_number_of_git_processes() {
    let mut counts = Vec::new();
    for notes in [3, 60, 300] {
        let dir = hub_repo(notes);
        let path = dir.path().to_str().unwrap();
        let (outcome, labels) = crate::engine::git_cli::spawn_labels_during(|| {
            rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(()))
        });
        let outcome = outcome.expect("rename");
        assert!(outcome.complete, "{outcome:?}");
        assert_eq!(outcome.rewritten_paths.len(), notes);
        let adds = labels.iter().filter(|l| l.as_str() == "git add").count();
        assert_eq!(adds, 1, "{notes} notes: {labels:?}");
        // Every rewritten note is in the index with its new content.
        let index = staged(&dir);
        assert_eq!(index.lines().count(), notes + 1, "{notes}: {index}");
        counts.push(labels.len());
    }
    assert!(
        counts.windows(2).all(|pair| pair[0] == pair[1]),
        "{counts:?}"
    );
}

/// Four notes that all link at `Roadmap.md`, so a rename rewrites four files.
fn crowded_repo() -> tempfile::TempDir {
    repo_with(&[
        ("README.md", "[r](docs/Roadmap.md)\n"),
        ("docs/A.md", "[r](Roadmap.md)\n"),
        ("docs/B.md", "[r](Roadmap.md)\n"),
        ("docs/C.md", "[r](Roadmap.md)\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ])
}

fn staged(dir: &tempfile::TempDir) -> String {
    git_text(dir.path(), &["diff", "--cached", "--name-status"]).unwrap()
}

#[test]
fn a_note_edited_after_the_plan_is_left_as_the_user_wrote_it() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::write(&guide, "# Guide\n\nI was typing [x](Roadmap.md).\n").unwrap();
        Ok(())
    })
    .expect("rename");

    assert_eq!(outcome.skipped_conflict, ["docs/Guide.md"], "{outcome:?}");
    assert!(!outcome.complete);
    assert_eq!(
        std::fs::read_to_string(&guide).unwrap(),
        "# Guide\n\nI was typing [x](Roadmap.md).\n"
    );
    // The rest of the plan still lands.
    assert_eq!(
        outcome.rewritten_paths,
        ["README.md", "plans/Roadmap.md"],
        "{outcome:?}"
    );
    assert!(read(&dir, "README.md").contains("(plans/Roadmap.md)"));
}

#[test]
fn an_expecting_write_replaces_only_the_bytes_it_expected() {
    use crate::engine::git_cli::sandbox_write_expecting;
    let dir = repo_with(&[("a.md", "one\n")]);
    let path = dir.path().to_str().unwrap();
    assert!(sandbox_write_expecting(path, "a.md", Some("one\n"), "two\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // Stale expectation: nothing is written.
    assert!(!sandbox_write_expecting(path, "a.md", Some("one\n"), "three\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // A file that is gone is not "the bytes I expected" either.
    assert!(!sandbox_write_expecting(path, "gone.md", Some("one\n"), "three\n").unwrap());
    assert!(!dir.path().join("gone.md").exists());
    // No expectation is `sandbox_write`: written whatever is there.
    assert!(sandbox_write_expecting(path, "a.md", None, "four\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "four\n");
}

#[test]
fn a_write_fault_on_one_note_is_reported_and_the_others_are_still_written() {
    let dir = crowded_repo();
    let path = dir.path().to_str().unwrap();
    probe::set_write_fault_at(Some(1));
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(()));
    probe::set_write_fault_at(None);
    let outcome = outcome.expect("a partial rename is reported, not an error");

    assert!(!outcome.complete, "{outcome:?}");
    assert_eq!(outcome.failed.len(), 1, "{outcome:?}");
    assert_eq!(outcome.failed[0].path, "docs/A.md");
    assert!(outcome.failed[0].error.contains("injected"), "{outcome:?}");
    assert_eq!(
        outcome.rewritten_paths,
        ["README.md", "docs/B.md", "docs/C.md"],
        "{outcome:?}"
    );
    assert_eq!(outcome.links_rewritten, 3);
    assert_eq!(read(&dir, "docs/A.md"), "[r](Roadmap.md)\n");
    let staged = staged(&dir);
    for rewritten in ["README.md", "docs/B.md", "docs/C.md"] {
        assert!(
            staged.contains(rewritten),
            "{rewritten} not staged: {staged}"
        );
    }
    assert!(!staged.contains("docs/A.md"), "{staged}");
}

#[test]
fn a_refused_rewrite_path_refuses_the_whole_rename_before_anything_moves() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let before = read(&dir, "docs/Guide.md");
    let refused = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |plan| {
        if plan.rewrites.iter().any(|p| p == "docs/Guide.md") {
            Err("docs/Guide.md is outside the task".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(
        refused.unwrap_err(),
        "docs/Guide.md is outside the task".to_string()
    );
    assert!(dir.path().join("docs/Roadmap.md").exists());
    assert!(!dir.path().join("plans").exists());
    assert_eq!(read(&dir, "docs/Guide.md"), before);
    assert_eq!(
        git_text(dir.path(), &["status", "--porcelain"]).unwrap(),
        ""
    );
}

#[test]
fn the_gate_is_shown_the_move_and_every_path_the_rename_will_write() {
    let dir = repo_with(&[
        ("docs/A.md", "[b](B.md)\n"),
        ("docs/B.md", "# B\n"),
        ("docs/C.md", "[a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let mut seen = Vec::new();
    rename_doc(path, "docs/A.md", "notes/A.md", |plan| {
        assert!(matches!(plan.mv, MovePlan::Git { .. }));
        seen = plan.rewrites.to_vec();
        Ok(())
    })
    .expect("rename");
    // The moved note under its new path, then the note that links at it.
    assert_eq!(seen, ["notes/A.md", "docs/C.md"]);
}

#[test]
fn a_file_cap_that_hides_notes_is_reported_not_hidden() {
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("c.md", "[b](b.md)\n"),
        ("d.md", "[b](b.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    let outcome = outcome.expect("rename");

    assert!(outcome.scan.truncated, "{outcome:?}");
    assert_eq!(outcome.scan.note_count, 2);
    assert_eq!(outcome.scan.not_scanned, 2);
    assert_eq!(outcome.rewritten_paths, ["a.md"]);
    // The notes it never saw still point at the old name: that is what the
    // count is for.
    assert_eq!(read(&dir, "c.md"), "[b](b.md)\n");
}

#[test]
fn a_note_that_is_not_utf8_is_counted_and_never_written() {
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n")]);
    std::fs::write(dir.path().join("bin.md"), [0xff, 0xfe, b'[', b']']).unwrap();
    git_in(dir.path(), &["add", "bin.md"]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.skipped_unreadable, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(
        std::fs::read(dir.path().join("bin.md")).unwrap(),
        [0xff, 0xfe, b'[', b']']
    );
}

#[test]
fn a_reference_definition_is_reported_because_it_is_not_rewritten() {
    let dir = repo_with(&[
        ("docs/Ref.md", "[x][ref]\n\n[ref]: Roadmap.md\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        outcome.left_broken,
        [BrokenLink {
            source: "docs/Ref.md".into(),
            target: "Roadmap.md".into(),
        }],
        "{outcome:?}"
    );
    assert_eq!(read(&dir, "docs/Ref.md"), "[x][ref]\n\n[ref]: Roadmap.md\n");
}

#[test]
fn a_clean_rename_reports_nothing_left_broken_and_is_complete() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert!(outcome.complete, "{outcome:?}");
    assert!(outcome.left_broken.is_empty(), "{outcome:?}");
    assert!(outcome.skipped_conflict.is_empty() && outcome.failed.is_empty());
    assert_eq!(outcome.scan.not_scanned, 0);
}

#[test]
fn a_link_that_cannot_survive_a_title_with_a_paren_still_round_trips() {
    let dir = repo_with(&[
        ("docs/A.md", "[r](Roadmap.md \"a)b\") end\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        read(&dir, "docs/A.md"),
        "[r](../plans/Roadmap.md \"a)b\") end\n"
    );
}

// ---------------------------------------------------------------------------
// Adversarial cases (second-pass tester). Each test states what it attacks.
// ---------------------------------------------------------------------------

/// xorshift64*: a deterministic generator, so a failing seed replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const ADV_DIRS: &[&str] = &[
    "docs",
    "a b",
    "(d)",
    "é",
    "..x",
    "x..",
    "a..b",
    ".dot",
    "emoji😀",
    "dir'q",
];
// Not generated here, each for a reason of its own:
//   `#` in a name: the vault cannot resolve such a link at all (it reads the
//     `#` as an anchor even when percent-escaped), so there is nothing to compare;
//     see `adversarial_a_destination_with_a_hash_...`.
//   NFD names (`e\u{301}`): git precomposes them on macOS, so the vault's name is
//     not the generated one; see `adversarial_a_decomposed_spelling_...`.
const ADV_NAMES: &[&str] = &[
    "a.md",
    "b c.md",
    "(p).md",
    "50%.md",
    "q?.md",
    "é.md",
    "😀.md",
    ".hid.md",
    "a..b.md",
    "x.mdx",
    "100%41.md",
    "UP.md",
    "[br].md",
    "a'b.md",
    "a\"b.md",
];

fn adv_key(path: &str) -> String {
    let stem = path
        .strip_suffix(".mdx")
        .or_else(|| path.strip_suffix(".md"))
        .unwrap_or(path);
    stem.to_lowercase()
}

fn adv_path(rng: &mut Rng) -> String {
    let depth = rng.below(5);
    let mut parts: Vec<&str> = (0..depth).map(|_| rng.pick(ADV_DIRS)).collect();
    parts.push(rng.pick(ADV_NAMES));
    parts.join("/")
}

/// `target` seen from the folder `dir`, built independently of the code under test.
fn adv_relative(dir: &str, target: &str) -> String {
    let from: Vec<&str> = dir.split('/').filter(|c| !c.is_empty()).collect();
    let parts: Vec<&str> = target.split('/').collect();
    let (file, down) = parts.split_last().unwrap();
    let mut common = 0;
    while common < from.len() && common < down.len() && from[common] == down[common] {
        common += 1;
    }
    let mut out = "../".repeat(from.len() - common);
    for d in &down[common..] {
        out.push_str(d);
        out.push('/');
    }
    out.push_str(file);
    out
}

/// What a careful author would type: percent-escape (or bracket) what CommonMark
/// would otherwise misread.
fn adv_spell(rng: &mut Rng, linking: &str, target: &str) -> String {
    let dir = linking.rsplit_once('/').map_or("", |(d, _)| d);
    let rooted = rng.chance(15);
    let mut path = if rooted {
        format!("/{target}")
    } else {
        adv_relative(dir, target)
    };
    if !rooted && !path.starts_with("../") && rng.chance(25) {
        path = format!("./{path}");
    }
    if target.ends_with(".md") && rng.chance(30) {
        path.truncate(path.len() - 3);
    }
    let bracketed = rng.chance(30);
    let mut out = String::new();
    for c in path.chars() {
        let always = matches!(c, '(' | ')' | '<' | '>' | '#' | '%');
        let space = c == ' ' && !bracketed;
        if always || space || (c == '?' && rng.chance(50)) {
            out.push_str(&format!("%{:02X}", c as u8));
        } else {
            out.push(c);
        }
    }
    let mut link = if bracketed { format!("<{out}>") } else { out };
    if rng.chance(25) {
        link.push_str(rng.pick(&["#sec", "#", "#h1"]));
    }
    if rng.chance(20) {
        link.push_str(rng.pick(&[" \"t\"", " 't'", " \"a(b\""]));
    }
    link
}

/// Many random vaults; for each a random note moves to a random place.
/// Property: every markdown link resolves to the note it resolved to before
/// (the moved note under its new path), or the outcome reports it in `left_broken`.
#[test]
fn adversarial_random_vaults_keep_every_link_or_report_it() {
    let mut failures: Vec<String> = Vec::new();
    let (mut resolved_links, mut total_links, mut silent, mut reported) = (0, 0, 0, 0);
    let mut skipped_by_git = 0usize;
    for seed in 1..=120u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut paths: Vec<String> = Vec::new();
        let mut keys = std::collections::HashSet::new();
        while paths.len() < 6 {
            let p = adv_path(&mut rng);
            if keys.insert(adv_key(&p)) {
                paths.push(p);
            }
        }
        let mut to = adv_path(&mut rng);
        while keys.contains(&adv_key(&to)) {
            to = adv_path(&mut rng);
        }
        let from = paths[rng.below(paths.len())].clone();
        let mut files: Vec<(String, String)> = Vec::new();
        for p in &paths {
            let mut text = String::from("# H\n\n## sec\n\n");
            for _ in 0..(3 + rng.below(4)) {
                let target = paths[rng.below(paths.len())].clone();
                let spelled = adv_spell(&mut rng, p, &target);
                text.push_str(&format!("- [l]({spelled})\n"));
            }
            files.push((p.clone(), text));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let dir = repo_with(&refs);
        let path = dir.path().to_str().unwrap();
        let before = load_vault(path, true).unwrap();
        let mut expected: Vec<(String, usize, Option<String>)> = Vec::new();
        for note in before.vault.notes() {
            for (position, link) in note.links.iter().enumerate() {
                let landed = before.vault.resolve_link(&note.path, link).map(|r| {
                    if r.path == from {
                        to.clone()
                    } else {
                        r.path
                    }
                });
                total_links += 1;
                resolved_links += landed.is_some() as usize;
                let source = if note.path == from {
                    to.clone()
                } else {
                    note.path.clone()
                };
                expected.push((source, position, landed));
            }
        }
        let held: std::collections::HashSet<&str> = before
            .vault
            .notes()
            .iter()
            .map(|n| n.path.as_str())
            .collect();
        if !paths.iter().all(|p| held.contains(p.as_str())) {
            // git precomposes Unicode on macOS, so an NFD spelling is stored as
            // NFC and the generated name is not the vault's name: not a rename case.
            skipped_by_git += 1;
            total_links -= expected.len();
            resolved_links -= expected.iter().filter(|e| e.2.is_some()).count();
            continue;
        }
        let outcome = match rename_doc(path, &from, &to, |_| Ok(())) {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!("seed {seed}: {from} -> {to}: rename refused: {e}"));
                continue;
            }
        };
        let after = refresh_and_load(path);
        let mut bad = Vec::new();
        for (source, position, landed) in &expected {
            let Some(note) = after.vault.note(source) else {
                bad.push(format!("{source} missing"));
                continue;
            };
            let Some(link) = note.links.get(*position) else {
                bad.push(format!("{source} lost link {position}"));
                continue;
            };
            let now = after.vault.resolve_link(source, link).map(|r| r.path);
            // A link the vault could not follow before (a literal `%41` in a note
            // name is decoded twice by its index) is not one the rename can break.
            if landed.is_some() && now != *landed {
                bad.push(format!(
                    "{source}#{position} written {:?}: {landed:?} became {now:?}",
                    link.target
                ));
            }
        }
        if !bad.is_empty() {
            if !outcome.left_broken.is_empty() {
                // Reported to the caller: the outcome did its job. (Every such
                // case here is a new name holding a literal `%41`, which the
                // vault's index decodes twice and so cannot follow.)
                reported += 1;
                continue;
            }
            silent += 1;
            failures.push(format!(
                "seed {seed}: {from} -> {to}\n    left_broken={:?}\n    {}\n    texts (path, before, after): {:?}",
                outcome.left_broken,
                bad.join("\n    "),
                files
                    .iter()
                    .map(|(p, t)| {
                        let now = if *p == from { &to } else { p };
                        (p.clone(), t.clone(), std::fs::read_to_string(dir.path().join(now)).ok())
                    })
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        skipped_by_git < 80,
        "git renamed too many generated paths: {skipped_by_git}"
    );
    assert!(
        resolved_links * 2 > total_links,
        "generator is not producing resolvable links: {resolved_links}/{total_links}"
    );
    assert!(
        failures.is_empty(),
        "{} of 120 vaults broke a link ({silent} with NOTHING in left_broken, {reported} reported); first 6:\n{}",
        failures.len(),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn adversarial_a_destination_with_a_hash_is_escaped_so_it_is_not_read_as_an_anchor() {
    let dir = repo_with(&[
        ("docs/A.md", "[r](Roadmap.md)\n"),
        ("docs/Roadmap.md", "# R\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/Roadmap.md", "docs/C#1.md", |_| Ok(())).expect("rename");
    // The vault does not see this link afterwards (it parses no link for it, and
    // reports none broken), so the outcome stays silent. Any markdown reader
    // follows `C#1.md` as the page `C` at anchor `1.md`.
    assert_eq!(read(&dir, "docs/A.md"), "[r](C%231.md)\n", "{outcome:?}");
}

#[test]
fn adversarial_a_decomposed_spelling_of_a_composed_note_name_resolves_the_same() {
    // The file is stored precomposed; the author typed the decomposed form.
    let dir = repo_with(&[
        ("docs/\u{e9}.md", "# e acute\n"),
        ("docs/L.md", "[x](e\u{301}.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let before = load_vault(path, true).unwrap();
    let note = before.vault.note("docs/L.md").unwrap();
    let resolves = before
        .vault
        .resolve_link("docs/L.md", &note.links[0])
        .is_some();
    let outcome =
        rename_doc(path, "docs/\u{e9}.md", "notes/\u{e9}.md", |_| Ok(())).expect("rename");
    let after = refresh_and_load(path);
    let note = after.vault.note("docs/L.md").unwrap();
    let now = after
        .vault
        .resolve_link("docs/L.md", &note.links[0])
        .is_some();
    assert_eq!(resolves, now, "{outcome:?} {:?}", read(&dir, "docs/L.md"));
}

#[test]
fn adversarial_a_wikilink_to_a_target_with_a_hash_or_pipe_is_not_left_silently_broken() {
    for to in ["docs/Plan#1.md", "docs/Plan|x.md", "docs/Plan]].md"] {
        let dir = repo_with(&[
            ("docs/A.md", "[[docs/Roadmap]] and [[Roadmap]]\n"),
            ("docs/Roadmap.md", "# R\n"),
        ]);
        let path = dir.path().to_str().unwrap();
        let outcome = rename_doc(path, "docs/Roadmap.md", to, |_| Ok(())).expect("rename");
        let after = refresh_and_load(path);
        let broken = after.vault.broken_links();
        assert!(
            broken.is_empty() || !outcome.left_broken.is_empty(),
            "{to}: wikilinks broke silently: broken={broken:?} outcome={outcome:?} text={:?}",
            read(&dir, "docs/A.md")
        );
    }
}

fn adv_unchanged(files: &[(&str, &str)], from: &str, to: &str, untouched: &[&str]) {
    let dir = repo_with(files);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, from, to, |_| Ok(())).expect("rename");
    for name in untouched {
        let original = files.iter().find(|(p, _)| p == name).unwrap().1;
        assert_eq!(read(&dir, name), original, "{name} must be byte-identical");
    }
}

#[test]
fn adversarial_links_that_are_not_links_are_left_alone() {
    let guide = "# G\n\n\
        ```\n[f](Roadmap.md)\n```\n\n\
        ~~~md\n[t](Roadmap.md)\n~~~\n\n\
        Inline `[i](Roadmap.md)` code and ``[j](Roadmap.md)`` too.\n\n\
        <a href=\"Roadmap.md\">html</a> <https://x.test/Roadmap.md> <Roadmap.md>\n\n\
        \x20   [indented](Roadmap.md)\n";
    adv_unchanged(
        &[("docs/G.md", guide), ("docs/Roadmap.md", "# R\n")],
        "docs/Roadmap.md",
        "plans/Roadmap.md",
        &["docs/G.md"],
    );
}

#[test]
fn adversarial_front_matter_links_are_not_rewritten_in_place_of_data() {
    // Front matter is YAML: a `](` in a string is not a markdown link.
    let note = "---\ntitle: \"see [x](Roadmap.md)\"\nrelated: Roadmap.md\n---\n# G\n";
    adv_unchanged(
        &[("docs/G.md", note), ("docs/Roadmap.md", "# R\n")],
        "docs/Roadmap.md",
        "plans/Roadmap.md",
        &["docs/G.md"],
    );
}

#[test]
fn adversarial_an_image_whose_source_is_a_note_follows_the_note_or_is_left_alone_consistently() {
    let dir = repo_with(&[
        ("docs/G.md", "![x](Roadmap.md) ![y](pic.png)\n"),
        ("docs/pic.png", "png"),
        ("docs/Roadmap.md", "# R\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    let g = read(&dir, "docs/G.md");
    // A non-note image is never touched.
    assert!(g.contains("![y](pic.png)"), "{g}");
    // The image-of-a-note is either rewritten to resolve, or left exactly.
    assert!(
        g.contains("![x](../plans/Roadmap.md)") || g.contains("![x](Roadmap.md)"),
        "{g}"
    );
}

#[test]
fn adversarial_the_moved_notes_non_note_assets_are_a_silent_residual() {
    // `pic.png` next to the moved note stops resolving after the move. The brief
    // lists this as out of scope; this pins what the outcome says about it.
    let dir = repo_with(&[
        ("docs/A.md", "![y](pic.png) [d](data.csv)\n"),
        ("docs/pic.png", "png"),
        ("docs/data.csv", "1"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "notes/A.md"), "![y](pic.png) [d](data.csv)\n");
    assert!(
        outcome.left_broken.is_empty(),
        "asset links are invisible to the vault, so the outcome cannot warn: {outcome:?}"
    );
}

#[test]
fn adversarial_moves_into_and_out_of_the_notes_own_subfolder_keep_links() {
    let files = [
        (
            "docs/A.md",
            "[b](B.md) [s](sub/S.md) [self](A.md) [u](../Top.md) [r](/docs/B.md)\n",
        ),
        ("docs/B.md", "[a](A.md) [a2](./A.md#h) <A.md>\n"),
        ("docs/sub/S.md", "[a](../A.md) [b](../B.md)\n"),
        ("Top.md", "[a](docs/A.md)\n"),
    ];
    for (from, to) in [
        ("docs/A.md", "docs/sub/A.md"),
        ("docs/sub/S.md", "docs/S.md"),
        ("docs/A.md", "A.md"),
        ("Top.md", "docs/Top.md"),
    ] {
        let dir = repo_with(&files);
        assert_rename_preserves_every_link(&dir, from, to);
    }
}

#[test]
fn adversarial_there_and_back_again_restores_every_link_byte() {
    let files = [
        (
            "docs/A.md",
            "[b](B.md) [s](sub/S.md) [self](A.md#h) [u](../Top.md)\n",
        ),
        ("docs/B.md", "[a](A.md) [a2](A.md#h)\n"),
        ("docs/sub/S.md", "[a](../A.md)\n"),
        ("Top.md", "[a](docs/A.md)\n"),
    ];
    let dir = repo_with(&files);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "x/y/A.md", |_| Ok(())).expect("there");
    rename_doc(path, "x/y/A.md", "docs/A.md", |_| Ok(())).expect("back");
    for (name, text) in files {
        assert_eq!(read(&dir, name), text, "{name} did not come back");
    }
}

#[test]
fn adversarial_two_notes_linking_each_other_when_one_moves() {
    let dir = repo_with(&[
        ("a/A.md", "[b](../b/B.md) [self](A.md)\n"),
        ("b/B.md", "[a](../a/A.md#x) [b](B.md)\n"),
    ]);
    assert_rename_preserves_every_link(&dir, "a/A.md", "b/deeper/A2.md");
}

#[test]
fn adversarial_a_same_named_sibling_in_another_folder_is_the_one_each_link_reached() {
    let files = [
        ("docs/A.md", "[s](Sib.md) [t](../notes/Sib.md) [u](Sib)\n"),
        ("docs/Sib.md", "# docs sib\n"),
        ("notes/Sib.md", "# notes sib\n"),
    ];
    for to in ["notes/A.md", "other/A.md", "docs/sub/A.md"] {
        let dir = repo_with(&files);
        assert_rename_preserves_every_link(&dir, "docs/A.md", to);
    }
}

#[test]
fn adversarial_moving_onto_a_name_that_already_has_a_sibling_stem_keeps_links() {
    // `docs/B.mdx` exists; the moved note becomes `docs/B.md`: both read as `docs/b`.
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/B.mdx", "# B mdx\n"),
        ("docs/L.md", "[a](A.md) [b](B.mdx) [b2](B)\n"),
    ]);
    // The vault reads `B.md` and `B.mdx` as one name, so the move would retarget
    // every link written for the other note: it is refused, and nothing moves.
    let path = dir.path().to_str().unwrap();
    let refused = rename_doc(path, "docs/A.md", "docs/B.md", |_| Ok(())).unwrap_err();
    assert!(
        refused.contains("shares its link name") && refused.contains("docs/B.mdx"),
        "{refused}"
    );
    assert!(dir.path().join("docs/A.md").exists());
    assert!(!dir.path().join("docs/B.md").exists());
    assert_eq!(read(&dir, "docs/L.md"), "[a](A.md) [b](B.mdx) [b2](B)\n");
    // A different stem, or the same note changing its own extension, is fine.
    rename_doc(path, "docs/A.md", "docs/C.mdx", |_| Ok(())).expect("a free name");
}

#[test]
fn a_sibling_whose_own_name_holds_a_percent_escape_still_blocks_the_move() {
    // `docs/100%41.mdx` is filed under `docs/100%41`; as a link destination
    // `100%41` would decode to `100a` and miss it, so the check compares the
    // vault's own key.
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/100%41.mdx", "# sibling\n"),
        ("docs/L.md", "[[docs/100%41]] and [[A]]\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let refused = rename_doc(path, "docs/A.md", "docs/100%41.md", |_| Ok(())).unwrap_err();
    assert!(
        refused.contains("shares its link name") && refused.contains("docs/100%41.mdx"),
        "{refused}"
    );
    assert!(dir.path().join("docs/A.md").exists());
    assert_eq!(read(&dir, "docs/L.md"), "[[docs/100%41]] and [[A]]\n");
}

#[test]
fn adversarial_a_previously_broken_link_is_not_blamed_on_the_rename() {
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/L.md", "[gone](Gone.md) [a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "docs/Gone.md", |_| Ok(())).expect("rename");
    assert!(outcome.left_broken.is_empty(), "{outcome:?}");
}

#[test]
fn adversarial_an_untracked_note_that_links_at_the_moved_note_is_reported() {
    let dir = repo_with(&[
        ("docs/Roadmap.md", "# R\n"),
        ("docs/Tracked.md", "[r](Roadmap.md)\n"),
    ]);
    write(dir.path(), "docs/Untracked.md", "[r](Roadmap.md)\n");
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "docs/Untracked.md"), "[r](Roadmap.md)\n");
    // The stale link in the note nobody scanned must show up somewhere: either
    // it is counted as not scanned, or it is in left_broken.
    assert!(
        outcome.scan.not_scanned > 0 || !outcome.left_broken.is_empty(),
        "an untracked note was silently left pointing at the old path: {outcome:?}"
    );
}

// ---- concurrency ---------------------------------------------------------

#[test]
fn adversarial_the_moved_note_edited_while_the_gate_runs_keeps_the_users_bytes() {
    let dir = repo_with(&[
        ("docs/A.md", "[b](B.md)\n"),
        ("docs/B.md", "# B\n"),
        ("docs/C.md", "[a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let a = dir.path().join("docs/A.md");
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| {
        std::fs::write(&a, "# typed meanwhile [b](B.md)\n").unwrap();
        Ok(())
    })
    .expect("rename");
    assert_eq!(read(&dir, "notes/A.md"), "# typed meanwhile [b](B.md)\n");
    assert_eq!(outcome.skipped_conflict, ["notes/A.md"], "{outcome:?}");
    assert!(!outcome.complete);
}

#[test]
fn adversarial_a_note_deleted_while_the_gate_runs_is_not_resurrected() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::remove_file(&guide).unwrap();
        Ok(())
    })
    .expect("rename");
    assert!(!guide.exists(), "the rewrite recreated a deleted note");
    assert_eq!(outcome.skipped_conflict, ["docs/Guide.md"], "{outcome:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_a_note_swapped_for_a_symlink_is_never_written_through() {
    let dir = nested_fixture_repo();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n").unwrap();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::remove_file(&guide).unwrap();
        std::os::unix::fs::symlink(&secret, &guide).unwrap();
        Ok(())
    })
    .expect("rename");
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n",
        "wrote through a symlink: {outcome:?}"
    );
    assert!(!outcome.complete, "{outcome:?}");
}

#[test]
fn adversarial_a_held_index_lock_refuses_the_rename_before_anything_moves() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let lock = dir.path().join(".git/index.lock");
    let result = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::write(&lock, "").unwrap();
        Ok(())
    });
    std::fs::remove_file(&lock).ok();
    assert!(result.is_err(), "{result:?}");
    assert!(dir.path().join("docs/Roadmap.md").exists());
    // Known limit: `GitWriter::move_path` creates the destination's parent
    // directory before it runs `git mv`, so a refused move leaves an empty
    // `plans/` behind. Nothing else is left: no file moved or written there.
    // Follow-up (orchestrator): create the parent only after the move succeeds.
    let leftover: Vec<_> = std::fs::read_dir(dir.path().join("plans"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "files left behind: {leftover:?}");
    assert_eq!(
        read(&dir, "docs/Guide.md"),
        "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n"
    );
}

#[test]
fn adversarial_a_lock_taken_after_the_move_is_reported_per_file_not_as_an_error() {
    // Racy by construction: a watcher thread takes the index lock the moment
    // the destination appears. Whatever interleaving happens, the outcome
    // must tell the truth about what is on disk and in the index.
    let dir = crowded_repo();
    let path = dir.path().to_str().unwrap().to_string();
    let to = dir.path().join("plans/Roadmap.md");
    let lock = dir.path().join(".git/index.lock");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let (lock2, to2) = (lock.clone(), to.clone());
    let watcher = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            if to2.exists() {
                let _ = std::fs::write(&lock2, "");
                return;
            }
        }
    });
    let outcome = rename_doc(&path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(()));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
    std::fs::remove_file(&lock).ok();
    let outcome = outcome.expect("the move happened, so this is a reported outcome");
    for edit in &outcome.failed {
        assert!(edit.error.contains("not staged"), "{outcome:?}");
        assert!(
            read(&dir, &edit.path).contains("plans/Roadmap.md"),
            "{}",
            edit.path
        );
    }
    assert_eq!(
        outcome.complete,
        outcome.failed.is_empty() && outcome.skipped_conflict.is_empty()
    );
}

#[test]
fn adversarial_destinations_outside_the_repository_are_refused() {
    let mut accepted = Vec::new();
    for to in [
        "../Out.md",
        ".git/hooks/x.md",
        ".git/x.md",
        ".GIT/x.md",
        "docs/Guide.md/x.md",
        "/abs/x.md",
        "a/../../x.md",
        "a/./b/../../../x.md",
    ] {
        let fresh = nested_fixture_repo();
        let path = fresh.path().to_str().unwrap();
        if rename_doc(path, "docs/Roadmap.md", to, |_| Ok(())).is_ok() {
            accepted.push(to);
        }
    }
    assert!(accepted.is_empty(), "destinations accepted: {accepted:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_a_destination_through_a_symlinked_folder_does_not_leave_the_repository() {
    let dir = nested_fixture_repo();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let path = dir.path().to_str().unwrap();
    let r = rename_doc(path, "docs/Roadmap.md", "escape/Roadmap.md", |_| Ok(()));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(dir.path().join("docs/Roadmap.md").exists());
}

// ---- truncation ----------------------------------------------------------

fn numbered_repo(count: usize) -> tempfile::TempDir {
    let dir = git_repo();
    for n in 0..count {
        write(dir.path(), &format!("n{n:02}.md"), "[t](n00.md)\n");
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "n"]);
    dir
}

#[test]
fn adversarial_the_cap_is_exact_not_off_by_one() {
    for (count, truncated, hidden) in [(3usize, false, 0u32), (4, true, 1)] {
        let dir = numbered_repo(count);
        let path = dir.path().to_str().unwrap();
        probe::set_max_doc_files(Some(3));
        let outcome = rename_doc(path, "n00.md", "z.md", |_| Ok(()));
        probe::set_max_doc_files(None);
        let outcome = outcome.expect("rename");
        assert_eq!(
            outcome.scan.truncated, truncated,
            "count {count}: {outcome:?}"
        );
        assert_eq!(
            outcome.scan.not_scanned, hidden,
            "count {count}: {outcome:?}"
        );
        assert_eq!(outcome.scan.note_count as usize + hidden as usize, count);
    }
}

#[test]
fn adversarial_a_note_past_the_cap_cannot_be_the_one_renamed() {
    let dir = numbered_repo(5);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let r = rename_doc(path, "n04.md", "z.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    assert!(r.is_err(), "{r:?}");
    assert!(dir.path().join("n04.md").exists());
}

#[test]
fn adversarial_an_unread_destination_hidden_by_the_cap_is_still_refused() {
    let dir = numbered_repo(5);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let r = rename_doc(path, "n00.md", "n04.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    assert!(r.is_err(), "{r:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_an_unreadable_note_is_counted_and_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("locked.md", "[b](b.md)\n"),
    ]);
    let locked = dir.path().join("locked.md");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(()));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    let outcome = outcome.expect("rename");
    assert_eq!(outcome.scan.skipped_unreadable, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(read(&dir, "locked.md"), "[b](b.md)\n");
}

#[test]
fn adversarial_a_tracked_note_missing_from_disk_is_counted_not_fatal() {
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("gone.md", "[b](b.md)\n"),
    ]);
    std::fs::remove_file(dir.path().join("gone.md")).unwrap();
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
}

#[test]
fn adversarial_an_oversized_note_is_counted_and_untouched() {
    let big = format!("[b](b.md)\n{}", "x".repeat(DEFAULT_MAX_NOTE_BYTES + 1));
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n"), ("big.md", &big)]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.skipped_oversized, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(read(&dir, "big.md"), big);
}

#[test]
fn adversarial_the_byte_budget_overflow_is_counted_as_not_scanned() {
    let body = format!("[b](b.md)\n{}", "x".repeat(12 * 1024 * 1024));
    let dir = repo_with(&[
        ("b.md", "# B\n"),
        ("c1.md", &body),
        ("c2.md", &body),
        ("c3.md", &body),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert!(outcome.scan.truncated, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(outcome.scan.note_count, 3, "{outcome:?}");
}

#[test]
fn adversarial_a_non_utf8_note_with_a_link_shaped_byte_run_is_never_rewritten() {
    let raw: Vec<u8> = b"\xff[b](b.md)\n".to_vec();
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n")]);
    std::fs::write(dir.path().join("latin.md"), &raw).unwrap();
    git_in(dir.path(), &["add", "latin.md"]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(std::fs::read(dir.path().join("latin.md")).unwrap(), raw);
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
}

// ---- case-insensitive filesystems ------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn adversarial_a_rename_that_differs_from_its_folder_only_in_case_keeps_links() {
    let dir = repo_with(&[
        ("Docs/A.md", "[o](Other.md) [s](A.md)\n"),
        ("Docs/Other.md", "[a](A.md) [a2](./A.md)\n"),
        ("README.md", "[a](Docs/A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let insensitive = dir.path().join("docs/A.md").exists();
    let outcome = rename_doc(path, "Docs/A.md", "docs/B.md", |_| Ok(()));
    if insensitive {
        // The two spellings are one folder: a plain rename inside it must keep
        // every link landing, or refuse cleanly.
        match outcome {
            Ok(outcome) => {
                let after = refresh_and_load(path);
                assert!(
                    after.vault.broken_links().is_empty(),
                    "{:?} {outcome:?}",
                    after.vault.broken_links()
                );
            }
            Err(_) => assert!(dir.path().join("Docs/A.md").exists()),
        }
    } else {
        outcome.expect("rename");
    }
}
