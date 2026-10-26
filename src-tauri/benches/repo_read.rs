//! `GitReader::get_status` on generated repositories of 1k and 10k files,
//! clean and dirty, and the transcript pass over a 100k-line corpus and a
//! 10k-distinct-cwd corpus, and `ledger::redact::text` on representative
//! `detail_json` rows.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench repo_read`.
//!
//! `get_status` is what every watcher-triggered refresh pays. The work counter
//! that matters is the number of `git` processes it starts (counted from git's
//! own trace2 in a separate untimed pass, see `common::fixture`): a
//! regression to one spawn per file reads as `git_processes` growing with the
//! file count long before the wall clock moves. `rows` is the number of status
//! rows returned, so a change that returns fewer cannot pass as a speedup.
//!
//! All four repositories are generated once, in a hermetic temp `HOME`; dirty
//! ones have 50 (1k) or 500 (10k) tracked files modified in the working tree.
//! The spawn rate is lifted as the stress tests lift it, so the rows time the
//! work and not the production gate's 8-spawns-per-second refill.

mod common;

use common::fixture::{build_repo, count_git_processes, dirty_files, Repo, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::git_cli::sandbox_write_edited_text;
use gitpulse_lib::engine::git_reader::GitReader;

struct Shape {
    label: &'static str,
    spec: Spec,
    dirty: usize,
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("repo_read");
    let temp = tempfile::tempdir().expect("temp dir");

    let shapes = [
        Shape {
            label: "1k files",
            spec: Spec {
                files: 1_000,
                dirs: 50,
                extra_commits: 3,
                touch: 5,
                linked_worktrees: 0,
            },
            dirty: 50,
        },
        Shape {
            label: "10k files",
            spec: Spec {
                files: 10_000,
                dirs: 200,
                extra_commits: 3,
                touch: 5,
                linked_worktrees: 0,
            },
            dirty: 500,
        },
    ];

    // (name, repository) pairs, built once.
    let mut repos: Vec<(String, Repo)> = Vec::new();
    for (index, shape) in shapes.iter().enumerate() {
        for state in ["clean", "dirty"] {
            let root = temp.path().join(format!("r{index}-{state}"));
            let repo = build_repo(&root, &shape.spec);
            if state == "dirty" {
                dirty_files(&repo, &shape.spec, shape.dirty);
            }
            repos.push((format!("get_status {} {state}", shape.label), repo));
        }
    }
    report.note(
        "generated repositories: 1k files (50 dirty) and 10k files (500 dirty), 4 commits each, \
         hermetic HOME, production spawn rate lifted"
            .to_string(),
    );

    let status = |repo: &Repo| {
        GitReader::get_status(repo.path()).unwrap_or_else(|error| {
            panic!(
                "get_status failed on {}: {error}",
                repo.path(),
                error = error.message()
            );
        })
    };

    // Work counters first, in their own untimed pass.
    let mut processes = Vec::new();
    for (_, repo) in &repos {
        let (_rows, spawned) = count_git_processes(|| status(repo));
        processes.push(spawned);
    }

    let mut cases: Vec<Case> = repos
        .iter()
        .zip(&processes)
        .map(|((name, repo), spawned)| {
            let spawned = *spawned;
            Case::new(name.clone(), move || {
                let rows = status(repo);
                let count = rows.len() as u64;
                std::hint::black_box(&rows);
                Sample::default()
                    .counter("rows", count)
                    .counter("git_processes", spawned)
            })
        })
        .collect();

    // Opening a file and saving it are the two reads of a file's bytes the
    // editor pays for, and both now also hash those bytes (the base token a
    // save is judged against). `get_file_blob` reads the working-tree file and
    // `sandbox_write_edited_text` re-reads it inside the save's entry
    // transaction; each sample of the save row alternates between two texts of
    // the same size so the publish is real, never the identical-bytes no-op.
    let blob_repo = build_repo(
        &temp.path().join("blobs"),
        &Spec {
            files: 10,
            dirs: 2,
            extra_commits: 0,
            touch: 0,
            linked_worktrees: 0,
        },
    );
    let blob_root = blob_repo.path().to_string();
    for (label, bytes) in [("1 KB", 1 << 10), ("1 MB", 1 << 20), ("8 MB", 8 << 20)] {
        let line = "the quick brown fox jumps over the lazy dog 0123456789\n";
        let text: String = line.repeat(bytes / line.len() + 1)[..bytes].to_string();
        let name = format!("blob-{bytes}.txt");
        std::fs::write(std::path::Path::new(&blob_root).join(&name), &text).expect("write blob");
        let read_root = blob_root.clone();
        let read_name = name.clone();
        cases.push(Case::new(format!("get_file_blob {label}"), move || {
            let blob = GitReader::get_file_blob(&read_root, &read_name, None).expect("blob");
            let length = blob.text.as_ref().map_or(0, |t| t.len()) as u64;
            std::hint::black_box(&blob);
            Sample::default().counter("bytes", length)
        }));
        // The diff viewer's read (`with_digest: false`): the same bytes with no
        // digest pass, so this row against `get_file_blob` is the digest's cost.
        let view_root = blob_root.clone();
        let view_name = name.clone();
        cases.push(Case::new(
            format!("get_file_blob_view {label}"),
            move || {
                let blob =
                    GitReader::get_file_blob_view(&view_root, &view_name, None).expect("blob");
                let length = blob.text.as_ref().map_or(0, |t| t.len()) as u64;
                std::hint::black_box(&blob);
                Sample::default().counter("bytes", length)
            },
        ));
        let save_root = blob_root.clone();
        let (even, odd) = (text.clone(), format!("X{}", &text[1..]));
        let mut turn = false;
        cases.push(Case::new(format!("save_edited_text {label}"), move || {
            turn = !turn;
            let content = if turn { &odd } else { &even };
            sandbox_write_edited_text(&save_root, &name, content).expect("save");
            Sample::default().counter("bytes", content.len() as u64)
        }));
    }
    report.note(
        "get_file_blob / save_edited_text: one text file of 1 KB, 1 MB and 8 MB in a clean 10-file \
         repository; the save alternates two same-size texts"
            .to_string(),
    );

    // The transcript pass reads agent history into the ledger, so it sits with
    // the other reads of what a repository already holds. Two corpora: many
    // calls from a few working directories (the steady shape), and one call
    // from each of many distinct ones (what attribution must not make slower).
    // Each sample is a fresh, empty ledger directory, and only the pass is
    // timed.
    let transcripts = temp.path().join("transcripts");
    let shapes = [
        ("transcript_pass 100k lines", TRANSCRIPT_LINES, 1, false),
        (
            "transcript_pass 10k cwds",
            TRANSCRIPT_CWDS,
            TRANSCRIPT_CWDS,
            false,
        ),
        (
            "transcript_pass 10k departed cwds",
            TRANSCRIPT_CWDS,
            TRANSCRIPT_CWDS,
            true,
        ),
    ];
    for (name, lines, cwds, departed) in shapes {
        let temp = temp.path().to_path_buf();
        let transcripts = transcripts.clone();
        cases.push(Case::new(name, move || {
            transcript_pass(&temp, &transcripts, lines, cwds, departed)
        }));
    }
    report.note(format!(
        "transcript_pass: one corpus file of {TRANSCRIPT_LINES} edit lines from 1 cwd, and one of \
         {TRANSCRIPT_CWDS} lines from {TRANSCRIPT_CWDS} distinct cwds, into a fresh ledger; the \
         departed row names a distinct removed `.claude/worktrees/<name>` in each line"
    ));

    // The ledger redacts every `object` and `detail_json` before the insert, so
    // `redact::text` is a per-row cost of everything that records. Three shapes
    // of what the writers actually store: the 4-field transcript detail, a
    // 7-field detail (event-time stamp and task added), and a transcript edit
    // as the writer stores it (the object path plus the 4-field detail). Each
    // sample redacts REDACT_ROUNDS copies, so the row is long enough to time.
    let transcript_detail = serde_json::json!({
        "source": "transcript",
        "tool": "Edit",
        "transcript_version": "2.1.241",
        "git_branch": "pf/1de1ff48-redact",
    })
    .to_string();
    let wide_detail = serde_json::json!({
        "source": "transcript",
        "tool": "Edit",
        "transcript_version": "2.1.241",
        "git_branch": "pf/1de1ff48-redact",
        "occurred_at": "2026-09-01T12:00:00.000Z",
        "occurred_at_basis": "transcript",
        "task_id": "ft-1de1ff48177ae7b96c27dd1635cc8a1e",
    })
    .to_string();
    let edit_object =
        "/Users/dev/Code/devtools/GitPulse/.claude/worktrees/pf-redact/src-tauri/src/ledger/redact.rs"
            .to_string();
    for (name, texts) in [
        ("redact::text 4 fields", vec![transcript_detail.clone()]),
        ("redact::text 7 fields", vec![wide_detail]),
        (
            "redact::text transcript edit",
            vec![edit_object, transcript_detail],
        ),
    ] {
        cases.push(Case::new(name, move || {
            let mut bytes = 0u64;
            for _ in 0..REDACT_ROUNDS {
                for text in &texts {
                    let out = gitpulse_lib::ledger::redact::text(std::hint::black_box(text));
                    bytes += out.len() as u64;
                    std::hint::black_box(&out);
                }
            }
            Sample::default().counter("bytes", bytes)
        }));
    }
    report.note(format!(
        "redact::text: {REDACT_ROUNDS} redactions per sample of a 4-field detail_json, a 7-field \
         detail_json, and a transcript edit (object path plus 4-field detail_json)"
    ));

    let rows = interleave(cases, report.opts.samples(15), report.opts.warmup(2));
    report.extend(rows);
    report.finish();
}

const REDACT_ROUNDS: usize = 2_000;
const TRANSCRIPT_LINES: usize = 100_000;
const TRANSCRIPT_CWDS: usize = 10_000;

/// One timed `ingest_transcripts` of a corpus of `lines` edit lines spread over
/// `cwds` working directories of a fresh repository directory.
fn transcript_pass(
    temp: &std::path::Path,
    corpus: &std::path::Path,
    lines: usize,
    cwds: usize,
    departed: bool,
) -> Sample {
    static FRESH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = FRESH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let repo = temp.join(format!("ledger-{id}"));
    std::fs::create_dir_all(&repo).expect("ledger dir");
    let repo = repo.to_string_lossy().into_owned();
    let slug = corpus.join("slug");
    std::fs::create_dir_all(&slug).expect("corpus dir");
    let mut text = String::new();
    for line in 0..lines {
        let cwd = if departed {
            format!("{repo}/.claude/worktrees/gone{}", line % cwds)
        } else {
            format!("{repo}/d{}", line % cwds)
        };
        let record = serde_json::json!({
            "type": "assistant",
            "sessionId": "S",
            "timestamp": format!("2026-09-01T12:{:02}:{:02}.000Z", (line / 60) % 60, line % 60),
            "version": "2.1.241",
            "cwd": cwd,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit",
                  "input": { "file_path": format!("{cwd}/f{}.rs", line % 500) } }
            ]}
        });
        text.push_str(&record.to_string());
        text.push('\n');
    }
    let file = slug.join("S.jsonl");
    std::fs::write(&file, text).expect("write corpus");
    std::env::set_var("GITPULSE_TRANSCRIPT_ROOT", corpus);
    let started = std::time::Instant::now();
    let out = gitpulse_lib::ingest::ingest_transcripts(&repo);
    let elapsed = started.elapsed();
    std::fs::remove_file(&file).expect("remove corpus");
    assert!(
        out.error.is_empty() && !out.truncated && out.skipped_lines == 0,
        "transcript pass degraded ({}), so this run would time a partial pass",
        out.error
    );
    Sample::default()
        .counter("recorded", out.recorded as u64)
        .counter("transcripts", out.transcripts as u64)
        .elapsed(elapsed)
}
