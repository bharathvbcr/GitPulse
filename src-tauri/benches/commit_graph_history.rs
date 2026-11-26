//! History walks with the commit-graph off, on, and on with changed-path
//! Bloom filters, over a 5k-commit and a 50k-commit repository.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench commit_graph_history`
//! (`-- --smoke` for 3 samples, `-- --json <path>` for the report).
//!
//! The question behind GitPulse's commit-graph hint (`storage/commit_graph.rs`)
//! is how much a missing graph costs. The operations are the ones the app runs
//! constantly, with the argv of the production readers in `engine/git_reader.rs`
//! (the `log --topo-order` history page, the `--parents` path-limited log, the
//! `for-each-ref %(ahead-behind:)` branch listing, `rev-list --count`, and
//! `blame --porcelain`). They go through the gated `git_cli::git_text`
//! seam but not the readers' caches, so every sample really walks.
//!
//! # Fixture
//!
//! Built once per size with `git fast-import`, so 50k commits cost seconds and
//! not minutes: one linear `main` of N commits, each rewriting one of 200 small
//! files; `rare/target.txt` rewritten by 50 evenly spread commits (the path a
//! Bloom filter can rule out of nearly every commit); 20 branches forked from
//! points along `main`, each three commits ahead. The repository is cloned into
//! three bare copies so the three states are measured *interleaved* in one run,
//! rounds rotating through every case, instead of in three runs that drifted
//! apart:
//!
//! * `graph-off`: no commit-graph file anywhere (`gc.auto=0`, so none appears).
//! * `graph-on`: `commit-graph write --reachable --no-changed-paths`.
//! * `graph+bloom`: `commit-graph write --reachable --changed-paths`.
//!
//! Each copy's state is asserted with the detector under test, and each
//! operation's output is asserted identical across the three, so a row cannot
//! get faster by returning something different. `lines` and `bytes` are the
//! work counters. Every sample is warm: the files are in the page cache.
//!
//! The graph writes here are fixture setup. GitPulse itself never writes a
//! commit-graph (Owner Decision 3).

mod common;

use std::path::{Path, PathBuf};

use common::fixture::{git, trust_repo};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::engine::git_cli::{git_text, git_with_stdin};
use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::graph::{history_rev_args, RefScope};
use gitpulse_lib::storage::commit_graph::{inspect, CommitGraphState};

/// The cold-window arms (AC1 of the history-graph card): a 100,000-commit
/// window read the shipped way (one walk) and the old way (one `--skip` walk
/// per chunk), with and without a commit-graph file.
const WINDOW_COMMITS: usize = 100_000;
/// The rows of one chunk of the old paging (and of the reader's chunk size).
const WINDOW_CHUNK: usize = 5_000;
/// Distinct branches at the tip for the window arms: each warmup and sample
/// reads one no earlier sample read, so the reader's ref cache is cold.
const WINDOW_BRANCHES: usize = 48;
const SIZES: [(&str, usize); 2] = [("5k", 5_000), ("50k", 50_000)];
const FILES: usize = 200;
const DIRS: usize = 20;
const BRANCHES: usize = 20;
const RARE_PATH: &str = "rare/target.txt";
/// Commits per `git fast-import` run.
const IMPORT_CHUNK: usize = 10_000;
const HISTORY_FORMAT: &str = "--format=format:%H%x01%P%x01%ct%x01%s%x01%an%x01%ae%x00";

#[derive(Clone, Copy)]
enum Mode {
    Off,
    Graph,
    Bloom,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Off, Mode::Graph, Mode::Bloom];

    fn label(self) -> &'static str {
        match self {
            Mode::Off => "graph-off",
            Mode::Graph => "graph-on",
            Mode::Bloom => "graph+bloom",
        }
    }

    fn expect(self) -> CommitGraphState {
        match self {
            Mode::Off => CommitGraphState::Missing,
            Mode::Graph => CommitGraphState::NoBloom,
            Mode::Bloom => CommitGraphState::Present,
        }
    }
}

struct Commit {
    index: usize,
    files: Vec<(String, String)>,
}

/// One `git fast-import` stream. `from` continues an existing ref and applies
/// to the first commit only.
fn stream(reference: &str, from: Option<&str>, commits: &[Commit]) -> Vec<u8> {
    let mut out = String::new();
    for (position, commit) in commits.iter().enumerate() {
        let message = format!("commit {}\n", commit.index);
        out.push_str(&format!(
            "commit {reference}\ncommitter Bench <bench@example.com> {} +0000\ndata {}\n{message}",
            1_700_000_000 + commit.index * 60,
            message.len()
        ));
        if let (0, Some(from)) = (position, from) {
            out.push_str(&format!("from {from}\n"));
        }
        for (path, content) in &commit.files {
            out.push_str(&format!(
                "M 100644 inline {path}\ndata {}\n{content}\n",
                content.len()
            ));
        }
        out.push('\n');
    }
    out.into_bytes()
}

fn main_commit(index: usize, rare_every: usize) -> Commit {
    let file = index % FILES;
    let mut files = vec![(
        format!("src/d{:02}/f{file:03}.txt", file % DIRS),
        format!("rev {index}\n{}", "filler line\n".repeat(8)),
    )];
    if index.is_multiple_of(rare_every) {
        let hot = (index / rare_every) % 40;
        let body: String = (0..40)
            .map(|line| {
                let rev = if line == hot { index } else { 0 };
                format!("line {line} rev {rev}\n")
            })
            .collect();
        files.push((RARE_PATH.to_string(), body));
    }
    Commit { index, files }
}

fn import(repo: &Path, reference_stream: &[u8]) {
    git_with_stdin(repo, &["fast-import", "--quiet"], reference_stream).expect("fast-import");
}

/// The source repository: `commits` on `main` plus the forked branches.
fn build_source(root: &Path, commits: usize) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create source");
    git(&src, &["init", "-q", "--bare", "-b", "main"]);
    git(&src, &["config", "gc.auto", "0"]);
    trust_repo(&src);
    let rare_every = commits / 50;
    let mut next = 0;
    while next < commits {
        let end = (next + IMPORT_CHUNK).min(commits);
        let batch: Vec<Commit> = (next..end).map(|i| main_commit(i, rare_every)).collect();
        let from = (next > 0).then_some("refs/heads/main^0");
        import(&src, &stream("refs/heads/main", from, &batch));
        next = end;
    }
    let mut branches = Vec::new();
    for branch in 0..BRANCHES {
        let back = (branch + 1) * (commits / (BRANCHES * 2));
        let base = git_text(&src, &["rev-parse", &format!("main~{back}")])
            .expect("rev-parse fork point")
            .trim()
            .to_string();
        let ahead: Vec<Commit> = (0..3)
            .map(|k| Commit {
                index: commits + branch * 3 + k,
                files: vec![(
                    format!("branch/b{branch:02}.txt"),
                    format!("b{branch} {k}\n"),
                )],
            })
            .collect();
        branches.extend(stream(
            &format!("refs/heads/b{branch:02}"),
            Some(&base),
            &ahead,
        ));
    }
    import(&src, &branches);
    src
}

fn clone_in_mode(root: &Path, source: &Path, size: &str, mode: Mode) -> PathBuf {
    let dest = root.join(format!("{size}-{}.git", mode.label()));
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            "--no-local",
            source.to_str().expect("utf-8"),
            dest.to_str().expect("utf-8"),
        ],
    );
    git(&dest, &["config", "gc.auto", "0"]);
    let info = dest.join("objects/info");
    let _ = std::fs::remove_file(info.join("commit-graph"));
    let _ = std::fs::remove_dir_all(info.join("commit-graphs"));
    let write = match mode {
        Mode::Off => None,
        Mode::Graph => Some("--no-changed-paths"),
        Mode::Bloom => Some("--changed-paths"),
    };
    if let Some(flag) = write {
        git(&dest, &["commit-graph", "write", "--reachable", flag]);
    }
    trust_repo(&dest);
    let report = inspect(&dest, &dest, &dest);
    assert_eq!(
        report.state,
        mode.expect(),
        "{} fixture is not in the state it claims: {report:?}",
        mode.label()
    );
    dest
}

fn operations(size: usize, main_oid: &str) -> Vec<(&'static str, Vec<String>)> {
    let refs: Vec<String> = history_rev_args(RefScope::Named)
        .iter()
        .map(|a| a.to_string())
        .collect();
    let log = |skip: usize, parents: bool, path: Option<&str>| {
        let mut argv: Vec<String> = Vec::new();
        if path.is_some() {
            argv.extend(["-c".into(), "core.quotepath=off".into()]);
        }
        argv.extend([
            "log".into(),
            "-n101".into(),
            format!("--skip={skip}"),
            "--topo-order".into(),
        ]);
        if parents {
            argv.push("--parents".into());
        }
        argv.push(HISTORY_FORMAT.into());
        argv.extend(refs.iter().cloned());
        if let Some(path) = path {
            argv.extend(["--".into(), path.to_string()]);
        }
        argv
    };
    vec![
        ("history_page_first", log(0, false, None)),
        ("history_page_deep", log(size / 2, false, None)),
        ("path_log", log(0, true, Some(RARE_PATH))),
        (
            "ahead_behind",
            vec![
                "for-each-ref".into(),
                format!("--format=%(objectname)%00%(ahead-behind:{main_oid})"),
                "refs/heads/".into(),
                "refs/remotes/".into(),
            ],
        ),
        (
            "rev_list_count",
            vec![
                "rev-list".into(),
                "--count".into(),
                "refs/heads/b10".into(),
                "^refs/heads/main".into(),
            ],
        ),
        (
            "blame",
            vec![
                "blame".into(),
                "--porcelain".into(),
                "--".into(),
                RARE_PATH.into(),
            ],
        ),
    ]
}

fn run(repo: &Path, argv: &[String]) -> String {
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    git_text(repo, &args)
        .unwrap_or_else(|e| panic!("git {argv:?} failed in {}: {}", repo.display(), e.message()))
}

/// The old paging: one `git log -n5001 --skip=k*5000` per chunk, the argv the
/// chunk loop used (the shape is the production one: `history_log_args`), read
/// alone, no parse. Returns the commit ids it saw, in order, with each chunk's
/// overlap row dropped.
fn old_paged_ids(repo: &Path, revision: &str, rows: usize) -> Vec<String> {
    let mut ids: Vec<String> = Vec::with_capacity(rows);
    let mut start = 0;
    while start + 1 < rows {
        let argv: Vec<String> = vec![
            "log".into(),
            format!("-n{}", WINDOW_CHUNK + 1),
            format!("--skip={start}"),
            "--topo-order".into(),
            HISTORY_FORMAT.into(),
            revision.into(),
        ];
        let text = run(repo, &argv);
        // Row `k` of this chunk is row `start + k` of the window; the chunk's
        // last row is the next chunk's first, so it is taken once.
        for (k, record) in text
            .split('\0')
            .map(|r| r.trim_matches('\n'))
            .filter(|r| !r.is_empty())
            .enumerate()
        {
            if start + k == ids.len() {
                ids.push(record.split('\u{1}').next().unwrap_or("").to_string());
            }
        }
        start += WINDOW_CHUNK;
    }
    ids.truncate(rows);
    ids
}

/// Cold 100k-row window, shipped single walk against the old `--skip` paging,
/// on a clone with no commit-graph file and one with. Parity is asserted
/// (the two reads return the same ids) before any case is built.
fn window_cases<'a>(root: &Path, reads: usize, report: &mut Report) -> Vec<Case<'a>> {
    assert!(
        reads + 2 < WINDOW_BRANCHES / 2,
        "{reads} reads need {reads} cold branches per arm, the fixture has {} per arm after the parity reads",
        WINDOW_BRANCHES / 2 - 2
    );
    let rows = WINDOW_COMMITS + 1;
    let started = std::time::Instant::now();
    let dir = root.join("window");
    std::fs::create_dir_all(&dir).expect("window root");
    let source = build_window_source(&dir);
    let built = started.elapsed();
    let mut cases: Vec<Case> = Vec::new();
    let mut parity: Option<Vec<String>> = None;
    for mode in [Mode::Off, Mode::Graph] {
        let repo = clone_in_mode(&dir, &source, "window", mode);
        for n in 0..WINDOW_BRANCHES {
            git(&repo, &["branch", &format!("cold{n}"), "main"]);
        }
        let path = repo.to_str().expect("utf-8").to_string();
        let ids = old_paged_ids(&repo, "cold0", rows);
        let shipped: Vec<String> = GitReader::read_commit_history_paged(
            &path,
            0,
            rows,
            Some("cold1"),
            None,
            RefScope::Named,
        )
        .expect("shipped window")
        .into_iter()
        .map(|c| c.id)
        .collect();
        assert_eq!(ids.len(), rows, "the old paging read {} rows", ids.len());
        assert_eq!(ids, shipped, "the two reads of the window disagree");
        match &parity {
            None => parity = Some(ids),
            Some(first) => assert_eq!(first, &ids, "the window differs between graph states"),
        }
        let old_repo = repo.clone();
        let next = std::sync::atomic::AtomicUsize::new(2);
        cases.push(Case::new(
            format!(
                "window 100k {} old: 20 git log --skip walks (read only)",
                mode.label()
            ),
            move || {
                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let revision = format!("cold{}", n % (WINDOW_BRANCHES / 2) * 2);
                let ids = old_paged_ids(&old_repo, &revision, rows);
                std::hint::black_box(&ids);
                Sample::default().counter("rows", ids.len() as u64)
            },
        ));
        let next = std::sync::atomic::AtomicUsize::new(0);
        cases.push(Case::new(
            format!(
                "window 100k {} shipped: one cold walk (read and parse)",
                mode.label()
            ),
            move || {
                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let revision = format!("cold{}", n % (WINDOW_BRANCHES / 2) * 2 + 1);
                let commits = GitReader::read_commit_history_paged(
                    &path,
                    0,
                    rows,
                    Some(&revision),
                    None,
                    RefScope::Named,
                )
                .expect("shipped window");
                std::hint::black_box(&commits);
                Sample::default().counter("rows", commits.len() as u64)
            },
        ));
    }
    report.note(format!(
        "window arms (synthetic): {} linear commits on main written with fast-import \
         (fixture built in {built:.1?}), a cold {rows}-row window of a branch no earlier sample \
         read; clones graph-off (no commit-graph file) and graph-on, asserted by the detector; \
         old arm = the {} `git log -n{} --skip` walks of the old chunk loop, read alone (git \
         time only, no parse, which understates the old read); shipped arm = \
         read_commit_history_paged, one walk, parsed. The ids of both reads are asserted equal \
         before any sample; every row is warm in the page cache",
        WINDOW_COMMITS + 1,
        WINDOW_COMMITS / WINDOW_CHUNK,
        WINDOW_CHUNK + 1,
    ));
    cases
}

/// `WINDOW_COMMITS + 1` linear commits on `main`, nothing else.
fn build_window_source(root: &Path) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create source");
    git(&src, &["init", "-q", "--bare", "-b", "main"]);
    git(&src, &["config", "gc.auto", "0"]);
    trust_repo(&src);
    // The graph's 100,000-row cap plus its has_more probe row.
    let total = WINDOW_COMMITS + 1;
    let mut next = 0;
    while next < total {
        let end = (next + IMPORT_CHUNK).min(total);
        let batch: Vec<Commit> = (next..end).map(|i| main_commit(i, total)).collect();
        let from = (next > 0).then_some("refs/heads/main^0");
        import(&src, &stream("refs/heads/main", from, &batch));
        next = end;
    }
    src
}

fn main() {
    gitpulse_lib::engine::process::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("commit_graph_history");
    let temp = tempfile::tempdir().expect("temp dir");

    // (size label, operation, mode label, repo, argv, lines, bytes)
    struct Row {
        name: String,
        repo: PathBuf,
        argv: Vec<String>,
        lines: u64,
        bytes: u64,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (label, size) in SIZES {
        let root = temp.path().join(label);
        std::fs::create_dir_all(&root).expect("size root");
        let source = build_source(&root, size);
        let repos: Vec<(Mode, PathBuf)> = Mode::ALL
            .iter()
            .map(|mode| (*mode, clone_in_mode(&root, &source, label, *mode)))
            .collect();
        let main_oid = git_text(&source, &["rev-parse", "main"])
            .expect("rev-parse main")
            .trim()
            .to_string();
        for (op, argv) in operations(size, &main_oid) {
            let mut expected: Option<String> = None;
            for (mode, repo) in &repos {
                let output = run(repo, &argv);
                match &expected {
                    None => expected = Some(output.clone()),
                    Some(first) => assert_eq!(
                        first, &output,
                        "{op} {label}: output differs between graph states, so a row would time a different answer"
                    ),
                }
                rows.push(Row {
                    name: format!("{op} {label} {}", mode.label()),
                    repo: repo.clone(),
                    argv: argv.clone(),
                    lines: output.lines().count() as u64,
                    bytes: output.len() as u64,
                });
            }
        }
    }
    let window_rounds = std::env::var("GITPULSE_BENCH_WINDOW_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| report.opts.samples(7));
    let window_warmup = report.opts.warmup(1);
    let window_cases = window_cases(temp.path(), window_rounds + window_warmup, &mut report);
    report.note(
        "fixture: bare repositories built with git fast-import. 5k and 50k linear commits on main \
         (one of 200 small files each), 50 commits on rare/target.txt, 20 branches three commits \
         ahead of fork points along main; three clones per size (graph-off, graph-on, \
         graph+bloom) measured interleaved, warm page cache; operation output asserted identical \
         across the three"
            .to_string(),
    );

    let cases: Vec<Case> = rows
        .iter()
        .map(|row| {
            Case::new(row.name.clone(), move || {
                let output = run(&row.repo, &row.argv);
                std::hint::black_box(&output);
                Sample::default()
                    .counter("lines", row.lines)
                    .counter("bytes", row.bytes)
            })
        })
        .collect();
    let measured = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
    report.extend(measured);
    let window = interleave(window_cases, window_rounds, window_warmup);
    report.extend(window);
    report.finish();
}
