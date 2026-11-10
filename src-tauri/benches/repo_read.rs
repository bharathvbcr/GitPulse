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

use common::fixture::{build_repo, count_git_processes, dirty_files, git, trust_repo, Repo, Spec};
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

    // The numstat limit (F-3): at 50k tracked files `get_status` used to run
    // `git diff --numstat -z` over every index entry to find churn for the few
    // paths `status` had just named. The two arms are the shipped read and the
    // whole-tree oracle on one repository, interleaved by `interleave`, so the
    // pair shares the host load of the moment. `rows` and `churn` must agree
    // between the arms or the row is not a comparison.
    let wide_spec = Spec {
        files: 50_000,
        dirs: 500,
        extra_commits: 1,
        touch: 5,
        linked_worktrees: 0,
    };
    for dirty in [1usize, 50] {
        let repo = build_repo(&temp.path().join(format!("wide-{dirty}")), &wide_spec);
        dirty_files(&repo, &wide_spec, dirty);
        let (limited, whole) = (
            GitReader::get_status(repo.path()).expect("limited status"),
            GitReader::get_status_whole_tree_numstat(repo.path()).expect("whole-tree status"),
        );
        let churn = |rows: &[gitpulse_lib::engine::git_reader::FileStatus]| {
            rows.iter()
                .map(|row| (row.additions + row.deletions) as u64)
                .sum::<u64>()
        };
        assert_eq!(
            format!("{limited:?}"),
            format!("{whole:?}"),
            "the arms disagree on {dirty} dirty files"
        );
        let total = churn(&limited);
        let repo = std::sync::Arc::new(repo);
        let shipped = std::sync::Arc::clone(&repo);
        cases.push(Case::new(
            format!("get_status 50k files {dirty} dirty, numstat limited to changed paths"),
            move || {
                let rows = status(&shipped);
                std::hint::black_box(&rows);
                Sample::default()
                    .counter("rows", rows.len() as u64)
                    .counter("churn", total)
            },
        ));
        cases.push(Case::new(
            format!("get_status 50k files {dirty} dirty, numstat over the whole tree"),
            move || {
                let rows = GitReader::get_status_whole_tree_numstat(repo.path())
                    .expect("whole-tree status");
                std::hint::black_box(&rows);
                Sample::default()
                    .counter("rows", rows.len() as u64)
                    .counter("churn", total)
            },
        ));
    }
    report.note(
        "50k-file repository (500 dirs, 2 commits), 1 and 50 tracked files modified: the same \
         get_status with the numstat limited to the reported paths against the whole-tree numstat \
         it replaced; the pair is interleaved and asserted row-for-row equal before timing"
            .to_string(),
    );

    // Blame (F-4): the old read was `git blame --line-porcelain`, which repeats a
    // commit's metadata for every line; the shipped one is `--porcelain` plus
    // the parse. The old arm is the old git read ALONE (bytes, no parse: the
    // old parser is test-only), so the shipped arm carries the parse on top and
    // a win for it is conservative. 20,000 lines edited by 40 commits.
    let blame_dir = temp.path().join("blame");
    std::fs::create_dir_all(&blame_dir).expect("blame dir");
    git(&blame_dir, &["init", "-q", "-b", "main"]);
    git(&blame_dir, &["config", "commit.gpgsign", "false"]);
    let mut blame_lines: Vec<String> = (0..20_000)
        .map(|n| format!("line {n:06}: let value_{n} = {};\n", n * 7))
        .collect();
    for commit in 0..=40usize {
        for n in (commit..blame_lines.len()).step_by(500) {
            blame_lines[n] = format!("line {n:06}: let value_{n} = {}; // rev {commit}\n", n * 7);
        }
        std::fs::write(blame_dir.join("big.txt"), blame_lines.concat()).expect("write blame file");
        git(&blame_dir, &["add", "big.txt"]);
        git(
            &blame_dir,
            &["commit", "-q", "-m", &format!("feat: revision {commit}")],
        );
    }
    trust_repo(&blame_dir);
    let blame_old = {
        let dir = blame_dir.clone();
        move || {
            gitpulse_lib::engine::git_cli::git_text_capped(
                &dir,
                &["blame", "--line-porcelain", "--", "big.txt"],
                64 << 20,
            )
            .expect("line-porcelain blame")
        }
    };
    let blame_new_bytes = gitpulse_lib::engine::git_cli::git_text_capped(
        &blame_dir,
        &["blame", "--porcelain", "--", "big.txt"],
        64 << 20,
    )
    .expect("porcelain blame")
    .0
    .len() as u64;
    let blame_old_bytes = blame_old().0.len() as u64;
    let blame_path = blame_dir.to_str().expect("utf-8").to_string();
    assert_eq!(
        GitReader::get_file_blame(&blame_path, "big.txt", None)
            .expect("blame")
            .len(),
        20_000
    );
    cases.push(Case::new(
        "blame 20k lines x 41 commits, git --line-porcelain read only (old format, no parse)",
        move || {
            let (text, _) = blame_old();
            std::hint::black_box(&text);
            Sample::default().counter("bytes", blame_old_bytes)
        },
    ));
    cases.push(Case::new(
        "blame 20k lines x 41 commits, get_file_blame (--porcelain read and parse)",
        move || {
            let lines = GitReader::get_file_blame(&blame_path, "big.txt", None).expect("blame");
            std::hint::black_box(&lines);
            Sample::default()
                .counter("lines", lines.len() as u64)
                .counter("bytes", blame_new_bytes)
        },
    ));
    report.note(
        "blame: one 20,000-line file rewritten at 40 lines in 500 by each of 41 commits; the old \
         arm is `git blame --line-porcelain` read alone, the shipped arm is `get_file_blame` \
         (`--porcelain`, read and parse); `bytes` is what git printed in each format"
            .to_string(),
    );

    // History window (F-1): a cold 20,001-row window is four 5,000-row chunks.
    // The old read was one `git log --skip=k*5000` walk per chunk (each walks
    // every commit before its skip); the shipped read is one walk cut into the
    // same chunks. Old arm: those four walks, bytes only. Shipped arm:
    // `read_commit_history_paged`, parsed, against a branch no earlier sample
    // has read, so the ref cache is cold for every sample.
    let history_dir = temp.path().join("history");
    std::fs::create_dir_all(&history_dir).expect("history dir");
    git(&history_dir, &["init", "-q", "-b", "main"]);
    trust_repo(&history_dir);
    let mut stream = String::new();
    for n in 1..=20_001usize {
        let message = format!("commit {n}");
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{n}\ncommitter Bench <bench@example.com> {} +0000\ndata {}\n{message}\n",
            1_767_225_600 + n,
            message.len()
        ));
        if n > 1 {
            stream.push_str(&format!("from :{}\n", n - 1));
        }
        stream.push_str("M 100644 inline f.txt\ndata 2\nx\n\n");
    }
    {
        use gitpulse_lib::procguard::LockedSpawn;
        use std::io::Write;
        let mut import = std::process::Command::new("git");
        import
            .args(["fast-import", "--quiet"])
            .current_dir(&history_dir)
            .stdin(std::process::Stdio::piped());
        let mut child = import.spawn_locked().expect("spawn fast-import");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stream.as_bytes())
            .expect("feed fast-import");
        assert!(child.wait().expect("fast-import").success());
    }
    // One cold revision per warmup and sample, all at the same tip.
    let revisions = 64usize;
    for n in 0..revisions {
        git(&history_dir, &["branch", &format!("cold{n}"), "main"]);
    }
    let history_path = history_dir.to_str().expect("utf-8").to_string();
    let next_revision = std::sync::atomic::AtomicUsize::new(0);
    let old_dir = history_dir.clone();
    cases.push(Case::new(
        "history window 20k, four git log --skip walks (old read, bytes only)",
        move || {
            let revision = format!(
                "cold{}",
                next_revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % revisions
            );
            let mut bytes = 0u64;
            for chunk in 0..4usize {
                let skip = format!("--skip={}", chunk * 5_000);
                let (text, _) = gitpulse_lib::engine::git_cli::git_text_capped(
                    &old_dir,
                    &[
                        "log",
                        "-n5001",
                        skip.as_str(),
                        "--topo-order",
                        "--format=format:%H%x01%P%x01%ct%x01%s%x01%an%x01%ae%x00",
                        revision.as_str(),
                    ],
                    64 << 20,
                )
                .expect("log chunk");
                bytes += text.len() as u64;
            }
            Sample::default().counter("bytes", bytes)
        },
    ));
    let next_revision = std::sync::atomic::AtomicUsize::new(0);
    cases.push(Case::new(
        "history window 20k, read_commit_history_paged cold (one walk, parsed)",
        move || {
            let revision = format!(
                "cold{}",
                next_revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % revisions
            );
            let rows = GitReader::read_commit_history_paged(
                &history_path,
                0,
                20_001,
                Some(&revision),
                None,
                gitpulse_lib::graph::RefScope::Named,
            )
            .expect("history window");
            std::hint::black_box(&rows);
            Sample::default().counter("rows", rows.len() as u64)
        },
    ));
    report.note(
        "history: 20,001 commits made with fast-import, a cold 20,001-row window; the shipped \
         arm reads a branch no earlier sample read (64 of them, warmup and samples included), so \
         the ref cache is cold; the old arm is the four `--skip` walks the old chunk loop spawned, \
         read alone"
            .to_string(),
    );

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
