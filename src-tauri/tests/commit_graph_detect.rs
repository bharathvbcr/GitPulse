//! The read-only commit-graph detector, run against graphs real git wrote.
//!
//! Each state the Health hint distinguishes is built with git itself, then read
//! back through `scan_storage` (the path the UI takes) or `inspect` (the
//! detector alone). Writing a graph here is test setup; GitPulse's production
//! code holds no such command, which the last test pins over the source.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::storage::commit_graph::{
    inspect, CommitGraphLayout, CommitGraphReport, CommitGraphState,
};
use gitpulse_lib::storage::scan_storage;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Tests here share the process-wide git config the spawn counter edits, so
/// they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_with_commits(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "gc.auto", "0"]);
    for i in 0..n {
        std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-q", "-m", &format!("c{i}")]);
    }
    dir
}

fn add_commit(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), name).unwrap();
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", name]);
}

fn detect(dir: &Path) -> CommitGraphReport {
    scan_storage(dir.to_str().unwrap())
        .expect("scan")
        .commit_graph
}

fn assert_state(report: &CommitGraphReport, state: CommitGraphState) {
    assert_eq!(report.state, state, "{report:?}");
    if state == CommitGraphState::Unknown {
        assert!(report.note.is_some(), "unknown must say why: {report:?}");
    }
}

#[test]
fn a_repository_without_a_graph_is_missing() {
    let _g = serial();
    let dir = repo_with_commits(3);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Missing);
    assert_eq!(report.layout, CommitGraphLayout::None);
    assert_eq!(report.commits, None);
}

#[test]
fn a_repository_with_no_commits_has_nothing_to_accelerate() {
    let _g = serial();
    let dir = repo_with_commits(0);
    assert_state(&detect(dir.path()), CommitGraphState::NoHistory);
}

#[test]
fn a_graph_without_bloom_filters_is_reported_as_such() {
    let _g = serial();
    let dir = repo_with_commits(4);
    run_git(dir.path(), &["commit-graph", "write", "--reachable"]);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::NoBloom);
    assert_eq!(report.bloom, Some(false));
    assert_eq!(report.head_covered, Some(true));
    assert_eq!(report.commits, Some(4));
    assert_eq!(report.layout, CommitGraphLayout::Single);
}

#[test]
fn a_graph_with_changed_paths_covering_head_is_present() {
    let _g = serial();
    let dir = repo_with_commits(4);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Present);
    assert_eq!(report.bloom, Some(true));
    assert_eq!(report.head_covered, Some(true));
}

#[test]
fn a_commit_newer_than_the_graph_makes_it_stale() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    add_commit(dir.path(), "newer.txt");
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Stale);
    assert_eq!(report.head_covered, Some(false));
}

#[test]
fn refs_in_packed_refs_and_a_detached_head_resolve() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    run_git(dir.path(), &["pack-refs", "--all", "--prune"]);
    assert!(!dir.path().join(".git/refs/heads/main").exists());
    assert_state(&detect(dir.path()), CommitGraphState::Present);
    run_git(dir.path(), &["checkout", "-q", "--detach"]);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_split_chain_is_read_layer_by_layer() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ],
    );
    let first = detect(dir.path());
    assert_state(&first, CommitGraphState::Present);
    assert_eq!(first.layout, CommitGraphLayout::Split);
    assert_eq!(first.layers, 1);

    add_commit(dir.path(), "second.txt");
    assert_state(&detect(dir.path()), CommitGraphState::Stale);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split=no-merge",
        ],
    );
    let second = detect(dir.path());
    assert_state(&second, CommitGraphState::Present);
    assert_eq!(second.layers, 2, "{second:?}");
    assert_eq!(second.commits, Some(4));
}

#[test]
fn a_chain_naming_a_missing_layer_could_not_be_checked_not_missing() {
    let _g = serial();
    let dir = repo_with_commits(2);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ],
    );
    let chain_dir = dir.path().join(".git/objects/info/commit-graphs");
    for entry in std::fs::read_dir(&chain_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "graph") {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert_state(&detect(dir.path()), CommitGraphState::Unknown);
}

#[test]
fn corrupt_graph_files_could_not_be_checked() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let graph = dir.path().join(".git/objects/info/commit-graph");
    let good = std::fs::read(&graph).unwrap();

    // Truncated mid-table, wrong signature, an offset past the end, an empty file.
    let mut bad_offset = good.clone();
    bad_offset[8 + 4..8 + 12].copy_from_slice(&u64::MAX.to_be_bytes());
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    let mut bad_fanout_total = good.clone();
    // The fanout chunk follows the chunk table; bump its last slot so the OID
    // lookup size no longer matches.
    let table_end = 8 + (good[6] as usize + 1) * 12;
    bad_fanout_total[table_end + 1020..table_end + 1024].copy_from_slice(&1000u32.to_be_bytes());
    for (label, bytes) in [
        ("truncated", good[..20].to_vec()),
        ("empty", Vec::new()),
        ("bad magic", bad_magic),
        ("bad offset", bad_offset),
        ("bad fanout", bad_fanout_total),
    ] {
        // Git writes the graph read-only.
        std::fs::remove_file(&graph).unwrap();
        std::fs::write(&graph, bytes).unwrap();
        let report = detect(dir.path());
        assert_eq!(
            report.state,
            CommitGraphState::Unknown,
            "{label}: {report:?}"
        );
        assert!(report.note.is_some(), "{label}");
    }
}

#[test]
fn a_linked_worktree_reads_the_graph_in_the_common_dir() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let parent = tempfile::tempdir().unwrap();
    let wt: PathBuf = parent.path().join("wt");
    run_git(
        dir.path(),
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    trust_repo(&wt);
    let report = detect(&wt);
    assert_state(&report, CommitGraphState::Present);
    assert!(
        wt.join(".git").is_file(),
        "a linked worktree has a .git file"
    );

    // A commit made only in the linked worktree is newer than the graph.
    add_commit(&wt, "only-here.txt");
    assert_state(&detect(&wt), CommitGraphState::Stale);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_shallow_clone_is_not_judged() {
    let _g = serial();
    let source = repo_with_commits(4);
    let parent = tempfile::tempdir().unwrap();
    let clone = parent.path().join("shallow");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", source.path().display()),
            clone.to_str().unwrap(),
        ],
    );
    trust_repo(&clone);
    assert_state(&detect(&clone), CommitGraphState::Unknown);
}

#[test]
fn replace_refs_turn_the_graph_off_so_nothing_is_judged() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(dir.path(), &["replace", "HEAD", "HEAD~1"]);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Unknown);
    assert!(
        report.note.as_deref().unwrap_or("").contains("replace"),
        "{report:?}"
    );
}

#[test]
fn a_repository_with_alternates_is_not_called_missing() {
    let _g = serial();
    let source = repo_with_commits(3);
    run_git(
        source.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let parent = tempfile::tempdir().unwrap();
    let clone = parent.path().join("shared");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--shared",
            source.path().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    trust_repo(&clone);
    assert!(clone.join(".git/objects/info/alternates").exists());
    let report = detect(&clone);
    assert_state(&report, CommitGraphState::Unknown);
    assert!(
        report.note.as_deref().unwrap_or("").contains("alternates"),
        "{report:?}"
    );
}

#[test]
fn a_bare_repository_is_read_in_place() {
    let _g = serial();
    let source = repo_with_commits(3);
    let parent = tempfile::tempdir().unwrap();
    let bare = parent.path().join("bare.git");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--bare",
            source.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    trust_repo(&bare);
    assert_state(&detect(&bare), CommitGraphState::Missing);
    run_git(
        &bare,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    assert_state(&detect(&bare), CommitGraphState::Present);
}

#[test]
fn a_sha256_graph_is_read() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "--object-format=sha256"])
        .current_dir(dir.path())
        .output_locked()
        .expect("spawn git");
    if !init.status.success() {
        eprintln!("git lacks sha256 support; case skipped");
        return;
    }
    trust_repo(dir.path());
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "user.name", "T"]);
    run_git(dir.path(), &["config", "user.email", "t@example.com"]);
    add_commit(dir.path(), "a.txt");
    add_commit(dir.path(), "b.txt");
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_hostile_head_is_unresolved_not_followed() {
    let _g = serial();
    let dir = repo_with_commits(2);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let git_dir = dir.path().join(".git");
    std::fs::write(git_dir.join("HEAD"), "ref: ../../../../etc/hosts\n").unwrap();
    let report = inspect(dir.path(), &git_dir, &git_dir);
    assert_state(&report, CommitGraphState::Unknown);
    assert_eq!(report.head_covered, None);
}

/// The detector reads files, plus the shared config snapshot. Git's own trace2
/// log is the witness: a positive control proves the log would have recorded a
/// spawn, then the detector runs with the log armed. Its only spawn may be the
/// snapshot's `config --list` on a cold cache, and a warm call adds none.
#[test]
fn detection_spawns_no_git_process() {
    let _g = serial();
    let dir = repo_with_commits(3);
    let git_dir = dir.path().join(".git");
    let log = dir.path().join("trace2.jsonl");
    // trace2 is only honoured from user-level config. The test's git reads
    // GIT_CONFIG_GLOBAL and the app's (which strips it) reads `$HOME/.gitconfig`,
    // so both files arm the log.
    let arm = format!("\n[trace2]\n\teventTarget = {}\n", log.display());
    let files = [
        PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").expect("hermetic config")),
        PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME")).join(".gitconfig"),
    ];
    let originals: Vec<Option<String>> = files
        .iter()
        .map(|f| std::fs::read_to_string(f).ok())
        .collect();
    for (file, original) in files.iter().zip(&originals) {
        std::fs::write(
            file,
            format!("{}{arm}", original.clone().unwrap_or_default()),
        )
        .unwrap();
    }

    let starts = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"event\":\"start\""))
            .map(str::to_string)
            .collect()
    };
    run_git(dir.path(), &["rev-parse", "HEAD"]);
    let control = starts(&log).len();
    // ref_cache keeps nothing stamped within its 3 s racy window, so a repository
    // written a moment ago would re-run `config --list` on every call.
    std::thread::sleep(std::time::Duration::from_millis(3300));
    let missing = inspect(dir.path(), &git_dir, &git_dir);
    let cold = starts(&log);
    let missing_again = inspect(dir.path(), &git_dir, &git_dir);
    let warm = starts(&log);
    let scanned = scan_storage(dir.path().to_str().unwrap());
    for (file, original) in files.iter().zip(originals) {
        match original {
            Some(text) => std::fs::write(file, text).unwrap(),
            None => std::fs::remove_file(file).unwrap(),
        }
    }

    assert!(
        control >= 1,
        "the trace2 log recorded no spawn: control broken"
    );
    assert_eq!(missing.state, CommitGraphState::Missing);
    assert_eq!(missing_again.state, CommitGraphState::Missing);
    // The one process the detector may start is the shared `config --list`
    // behind `config_snapshot`, on a cold cache. Nothing that reads the graph
    // or HEAD (`commit-graph`, `rev-parse`, ...) may run.
    for event in &cold[control..] {
        assert!(
            event.contains("\"config\"") && event.contains("\"--list\""),
            "inspect spawned something other than `config --list`: {event}"
        );
    }
    assert_eq!(warm.len(), cold.len(), "a warm inspect spawned a process");
    assert!(scanned.is_ok());
}

/// No production file may build a commit-graph write argv: writing one is
/// Owner Decision 3, and `git_config_posture.rs` guards the config half of the
/// same rule. The Health hint prints the command as display text for the
/// person to run, so the literal appears only in a TypeScript template string.
#[test]
fn no_production_rust_builds_a_commit_graph_write_argv() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap();
        if name == "tests.rs" || name.ends_with("_tests.rs") || rel.starts_with("test_support/") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        // Production text only: drop an inline `#[cfg(test)] mod tests`.
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        // Whitespace-free, so `"commit-graph",\n "write"` and a spelled-out
        // `commit-graph write` both match; the bare path component
        // `join("commit-graph")` (reading the file) does not.
        let squashed: String = production.split_whitespace().collect();
        if squashed.contains("\"commit-graph\",\"write\"")
            || production.contains("commit-graph write")
        {
            offenders.push(rel);
        }
    }
    assert!(offenders.is_empty(), "commit-graph argv in {offenders:?}");
}

// ---------------------------------------------------------------------------
// Adversarial coverage: hostile bytes, oracle-checked against `git commit-graph
// verify`. Every graph git rejects must read as `unknown`, never as a verdict.
// ---------------------------------------------------------------------------

/// SHA-1, only to re-seal a mutated graph: git checks the trailing checksum
/// first, so without re-sealing every mutation is rejected for that one reason
/// and the structural checks behind it are never reached.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn reseal(bytes: &mut [u8]) {
    if bytes.len() > 20 {
        let n = bytes.len() - 20;
        let sum = sha1(&bytes[..n]);
        bytes[n..].copy_from_slice(&sum);
    }
}

fn git_accepts_graph(dir: &Path) -> bool {
    std::process::Command::new("git")
        .args(["commit-graph", "verify"])
        .current_dir(dir)
        .output_locked()
        .expect("spawn git")
        .status
        .success()
}

/// The detector alone, without `scan_storage`'s git spawns, for loops.
fn inspect_only(dir: &Path) -> CommitGraphReport {
    let git_dir = dir.join(".git");
    inspect(dir, &git_dir, &git_dir)
}

fn graph_repo(commits: usize) -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let dir = repo_with_commits(commits);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let graph = dir.path().join(".git/objects/info/commit-graph");
    let good = std::fs::read(&graph).unwrap();
    (dir, graph, good)
}

fn put_graph(graph: &Path, bytes: &[u8]) {
    let _ = std::fs::remove_file(graph);
    std::fs::write(graph, bytes).unwrap();
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn a_resealed_graph_is_accepted_by_git_and_read_as_present() {
    let _g = serial();
    let (dir, graph, mut good) = graph_repo(6);
    reseal(&mut good);
    put_graph(&graph, &good);
    assert!(
        git_accepts_graph(dir.path()),
        "the re-sealing helper is broken"
    );
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn every_truncation_of_a_graph_is_unknown() {
    let _g = serial();
    let (dir, graph, good) = graph_repo(6);
    for n in 0..good.len() {
        put_graph(&graph, &good[..n]);
        let report = inspect_only(dir.path());
        assert_eq!(
            report.state,
            CommitGraphState::Unknown,
            "prefix {n}: {report:?}"
        );
    }
}

/// Every bucket edge moved down by one: git rejects each, and none reads as
/// a verdict. The fuzz below finds these only when the clock-derived OIDs put
/// a mutated byte on an edge; this tries every edge on every run.
#[test]
fn a_fanout_shifted_at_any_bucket_edge_is_unknown() {
    let _g = serial();
    let (dir, graph, good) = graph_repo(40);
    let table = 8 + (good[6] as usize + 1) * 12;
    let at = (8..table)
        .step_by(12)
        .find(|&row| &good[row..row + 4] == b"OIDF")
        .unwrap();
    let oidf = u64::from_be_bytes(good[at + 4..at + 12].try_into().unwrap()) as usize;
    let fan =
        |i: usize| u32::from_be_bytes(good[oidf + 4 * i..oidf + 4 * i + 4].try_into().unwrap());
    let mut tried = 0;
    for i in 0..255 {
        if fan(i) == if i == 0 { 0 } else { fan(i - 1) } {
            continue;
        }
        let mut bytes = good.clone();
        bytes[oidf + 4 * i..oidf + 4 * i + 4].copy_from_slice(&(fan(i) - 1).to_be_bytes());
        reseal(&mut bytes);
        put_graph(&graph, &bytes);
        assert!(
            !git_accepts_graph(dir.path()),
            "git accepted fanout[{i}] - 1"
        );
        assert_eq!(
            inspect_only(dir.path()).state,
            CommitGraphState::Unknown,
            "fanout[{i}] - 1"
        );
        tried += 1;
    }
    assert!(tried > 2, "only {tried} bucket edges in the fixture");
}

#[test]
fn structural_mutations_git_rejects_never_read_as_a_verdict() {
    let _g = serial();
    let (dir, graph, good) = graph_repo(40);
    let table_end = 8 + (good[6] as usize + 1) * 12;
    let structural = table_end + 1024;
    let rows = good[6] as usize + 1;
    let mut rng = Rng(0x9cfa_4874_dead_beef);
    let mut offenders = Vec::new();
    let mut rejected = 0;
    for round in 0..250 {
        let mut bytes = good.clone();
        for _ in 0..=rng.below(3) {
            match rng.below(4) {
                0 => {
                    let i = rng.below(structural);
                    bytes[i] = rng.next() as u8;
                }
                1 => {
                    let i = rng.below(structural);
                    bytes[i] ^= 1 << rng.below(8);
                }
                2 => {
                    // Rewrite one chunk-table offset with an edge value.
                    let row = rng.below(rows);
                    let v: u64 = [0, 8, 1, u64::MAX, bytes.len() as u64, 1 << 32, 12][rng.below(7)];
                    bytes[8 + row * 12 + 4..8 + row * 12 + 12].copy_from_slice(&v.to_be_bytes());
                }
                _ => {
                    // Swap two chunk ids.
                    let a = rng.below(rows);
                    let b = rng.below(rows);
                    for k in 0..4 {
                        bytes.swap(8 + a * 12 + k, 8 + b * 12 + k);
                    }
                }
            }
        }
        reseal(&mut bytes);
        put_graph(&graph, &bytes);
        if git_accepts_graph(dir.path()) {
            continue;
        }
        rejected += 1;
        let report = inspect_only(dir.path());
        if report.state != CommitGraphState::Unknown {
            let changed: Vec<String> = (0..structural)
                .filter(|&i| bytes[i] != good[i])
                .map(|i| format!("{i}:{:02x}->{:02x}", good[i], bytes[i]))
                .collect();
            offenders.push(format!(
                "round {round}: {:?} with bytes changed {changed:?}",
                report.state
            ));
        }
    }
    assert!(
        rejected > 60,
        "the fuzz barely produced rejected graphs ({rejected})"
    );
    assert!(
        offenders.is_empty(),
        "{} graphs git rejects read as a verdict, first: {:?}",
        offenders.len(),
        offenders.first()
    );
}

fn sha256_repo_with_graph() -> Option<(tempfile::TempDir, Vec<u8>)> {
    let dir = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "--object-format=sha256"])
        .current_dir(dir.path())
        .output_locked()
        .expect("spawn git");
    if !init.status.success() {
        return None;
    }
    trust_repo(dir.path());
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "user.name", "T"]);
    run_git(dir.path(), &["config", "user.email", "t@example.com"]);
    add_commit(dir.path(), "a.txt");
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let bytes = std::fs::read(dir.path().join(".git/objects/info/commit-graph")).unwrap();
    Some((dir, bytes))
}

#[test]
fn a_graph_of_the_other_hash_version_is_unknown() {
    let _g = serial();
    let Some((_sha256_dir, sha256_graph)) = sha256_repo_with_graph() else {
        eprintln!("git lacks sha256 support; case skipped");
        return;
    };
    let (dir, graph, _good) = graph_repo(3);
    put_graph(&graph, &sha256_graph);
    assert!(
        !git_accepts_graph(dir.path()),
        "oracle: git must reject a sha256 graph in a sha1 repository"
    );
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Unknown);
}

#[test]
fn a_sha1_graph_in_a_sha256_repository_is_unknown() {
    let _g = serial();
    let Some((sha256_dir, _)) = sha256_repo_with_graph() else {
        eprintln!("git lacks sha256 support; case skipped");
        return;
    };
    let (_dir, _graph, sha1_graph) = graph_repo(3);
    put_graph(
        &sha256_dir.path().join(".git/objects/info/commit-graph"),
        &sha1_graph,
    );
    assert!(
        !git_accepts_graph(sha256_dir.path()),
        "oracle: git must reject a sha1 graph in a sha256 repository"
    );
    assert_state(&detect(sha256_dir.path()), CommitGraphState::Unknown);
}

#[test]
fn core_commit_graph_false_means_git_ignores_the_graph() {
    let _g = serial();
    // Control: the same fixture without the setting is a plain "present". The
    // config snapshot is cached per repository, so the setting goes in before
    // the repository under test is first scanned.
    let (control, _graph, _good) = graph_repo(3);
    assert_state(&detect(control.path()), CommitGraphState::Present);
    let (dir, _graph, _good) = graph_repo(3);
    run_git(dir.path(), &["config", "core.commitGraph", "false"]);
    let report = detect(dir.path());
    assert_eq!(report.state, CommitGraphState::Unknown, "{report:?}");
    assert!(
        report
            .note
            .as_deref()
            .unwrap_or("")
            .contains("core.commitGraph"),
        "{report:?}"
    );
}

#[test]
fn core_commit_graph_false_in_the_user_config_is_seen_too() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(3);
    // The app's git runs with GIT_CONFIG_GLOBAL stripped, so the user's config
    // is `$HOME/.gitconfig` (the hermetic HOME here).
    let config = PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME")).join(".gitconfig");
    let original = std::fs::read_to_string(&config).ok();
    std::fs::write(
        &config,
        format!(
            "{}\n[core]\n\tcommitGraph = no\n",
            original.clone().unwrap_or_default()
        ),
    )
    .unwrap();
    let report = detect(dir.path());
    match original {
        Some(text) => std::fs::write(&config, text).unwrap(),
        None => std::fs::remove_file(&config).unwrap(),
    }
    assert_eq!(report.state, CommitGraphState::Unknown, "{report:?}");
}

#[test]
fn an_unreadable_config_is_unknown_never_a_verdict() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(3);
    let git_dir = dir.path().join(".git");
    // A repository path git cannot run in makes `config_snapshot` fail.
    let report = inspect(&dir.path().join("gone"), &git_dir, &git_dir);
    assert_state(&report, CommitGraphState::Unknown);
    assert!(
        report.note.as_deref().unwrap_or("").contains("config"),
        "{report:?}"
    );
}

#[test]
fn fetch_write_commit_graph_does_not_change_what_a_present_graph_means() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(3);
    run_git(dir.path(), &["config", "fetch.writeCommitGraph", "false"]);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

fn chain_repo() -> (tempfile::TempDir, PathBuf, Vec<String>) {
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ],
    );
    add_commit(dir.path(), "x.txt");
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split=no-merge",
        ],
    );
    let chain = dir
        .path()
        .join(".git/objects/info/commit-graphs/commit-graph-chain");
    let lines = std::fs::read_to_string(&chain)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    (dir, chain, lines)
}

fn write_chain(chain: &Path, text: &str) {
    let _ = std::fs::remove_file(chain);
    std::fs::write(chain, text).unwrap();
}

#[test]
fn hostile_chain_files_are_unknown_whenever_git_rejects_them() {
    let _g = serial();
    let (dir, chain, lines) = chain_repo();
    assert_eq!(lines.len(), 2, "fixture: two layers");
    assert_state(&detect(dir.path()), CommitGraphState::Present);
    let (base, tip) = (&lines[0], &lines[1]);
    let cases: Vec<(&str, String)> = vec![
        ("layers swapped", format!("{tip}\n{base}\n")),
        ("duplicate layer", format!("{base}\n{base}\n")),
        ("tip repeated", format!("{base}\n{tip}\n{tip}\n")),
        ("tip alone", format!("{tip}\n")),
        ("crlf", format!("{base}\r\n{tip}\r\n")),
        ("blank lines", format!("\n{base}\n\n{tip}\n\n")),
        ("non hex", format!("{base}\nzzzz{}\n", &tip[4..])),
        (
            "uppercase",
            format!("{}\n{}\n", base.to_uppercase(), tip.to_uppercase()),
        ),
        ("trailing garbage", format!("{base}\n{tip}\nnot-a-hash\n")),
    ];
    for (label, text) in cases {
        write_chain(&chain, &text);
        let accepted = git_accepts_graph(dir.path());
        let report = detect(dir.path());
        if !accepted {
            assert_eq!(
                report.state,
                CommitGraphState::Unknown,
                "{label}: git rejects it but the detector said {report:?}"
            );
        }
    }
}

#[test]
fn a_chain_of_sixty_five_layers_is_unknown_and_sixty_four_is_bounded() {
    let _g = serial();
    let (dir, chain, lines) = chain_repo();
    let tip = &lines[1];
    write_chain(&chain, &format!("{tip}\n").repeat(65));
    let report = detect(dir.path());
    assert_eq!(report.state, CommitGraphState::Unknown, "{report:?}");
    write_chain(&chain, &format!("{tip}\n").repeat(64));
    let report = detect(dir.path());
    assert!(report.layers <= 64, "{report:?}");
}

#[test]
fn layers_that_disagree_on_hash_version_are_unknown() {
    let _g = serial();
    let Some((_d, sha256_graph)) = sha256_repo_with_graph() else {
        eprintln!("git lacks sha256 support; case skipped");
        return;
    };
    let (dir, chain, lines) = chain_repo();
    // Replace the tip layer with a sha256 graph under the same name.
    let layer = dir.path().join(format!(
        ".git/objects/info/commit-graphs/graph-{}.graph",
        lines[1]
    ));
    put_graph(&layer, &sha256_graph);
    let _ = chain;
    assert!(!git_accepts_graph(dir.path()), "oracle");
    assert_state(&detect(dir.path()), CommitGraphState::Unknown);
}

#[test]
fn a_graph_missing_a_required_chunk_is_unknown() {
    let _g = serial();
    let (dir, graph, good) = graph_repo(6);
    let rows = good[6] as usize;
    for row in 0..rows {
        let id = &good[8 + row * 12..8 + row * 12 + 4];
        if id == b"CDAT" || id == b"OIDF" || id == b"OIDL" {
            let mut bytes = good.clone();
            bytes[8 + row * 12] = b'X';
            reseal(&mut bytes);
            put_graph(&graph, &bytes);
            assert!(
                !git_accepts_graph(dir.path()),
                "oracle: renaming {:?} must make git reject",
                std::str::from_utf8(id)
            );
            assert_eq!(
                detect(dir.path()).state,
                CommitGraphState::Unknown,
                "chunk {:?} renamed",
                std::str::from_utf8(id)
            );
        }
    }
}

#[test]
fn a_loose_ref_wins_over_packed_refs_and_packed_refs_is_not_opened_for_it() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(4);
    run_git(dir.path(), &["pack-refs", "--all"]);
    let git_dir = dir.path().join(".git");
    // A new commit makes a loose ref that disagrees with the packed one; the
    // graph predates it, so a loose-wins read is stale and a packed-wins read
    // is present.
    add_commit(dir.path(), "newer.txt");
    assert_state(&detect(dir.path()), CommitGraphState::Stale);
    // With the branch loose, an unreadable packed-refs must not be touched.
    use std::os::unix::fs::PermissionsExt;
    let packed = git_dir.join("packed-refs");
    std::fs::set_permissions(&packed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = detect(dir.path());
    std::fs::set_permissions(&packed, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_state(&report, CommitGraphState::Stale);
}

#[test]
fn symref_loops_and_packed_refs_oddities_do_not_hang_or_mislead() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(3);
    let git_dir = dir.path().join(".git");
    std::fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
    std::fs::write(git_dir.join("refs/heads/a"), "ref: refs/heads/b\n").unwrap();
    std::fs::write(git_dir.join("refs/heads/b"), "ref: refs/heads/a\n").unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/a\n").unwrap();
    let report = inspect(dir.path(), &git_dir, &git_dir);
    assert_state(&report, CommitGraphState::Unknown);

    // Header, peeled and annotated lines, an unrelated ref, then the real one.
    let head_oid = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    std::fs::remove_file(git_dir.join("refs/heads/a")).unwrap();
    std::fs::remove_file(git_dir.join("refs/heads/b")).unwrap();
    std::fs::remove_file(git_dir.join("refs/heads/main")).unwrap();
    std::fs::write(
        git_dir.join("packed-refs"),
        format!(
            "# pack-refs with: peeled fully-peeled sorted \n{z} refs/heads/other\n^{z}\n{head_oid} refs/heads/main\n",
            z = "0".repeat(40)
        ),
    )
    .unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    assert_state(
        &inspect(dir.path(), &git_dir, &git_dir),
        CommitGraphState::Present,
    );
}

#[test]
fn a_linked_worktree_with_its_own_head_reads_the_shared_objects() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(3);
    let wt = dir.path().join("wt");
    run_git(
        dir.path(),
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    add_commit(&wt, "side.txt");
    // The worktree's HEAD is newer than the shared graph: stale, not present.
    assert_state(&detect(&wt), CommitGraphState::Stale);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_fifty_thousand_commit_graph_is_read_correctly_and_cheaply() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    let mut stream = String::new();
    for i in 0..50_000u32 {
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{}\ncommitter T <t@example.com> {} +0000\ndata 2\nc\n",
            i + 1,
            1_000_000_000 + i
        ));
        if i > 0 {
            stream.push_str(&format!("from :{i}\n"));
        }
        stream.push('\n');
    }
    let input = dir.path().join("stream");
    std::fs::write(&input, stream).unwrap();
    let status = std::process::Command::new("sh")
        .args(["-c", "git fast-import --quiet < stream"])
        .current_dir(dir.path())
        .output_locked()
        .expect("spawn sh");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let report = detect(dir.path());
    assert_eq!(report.state, CommitGraphState::Present, "{report:?}");
    assert_eq!(report.commits, Some(50_000));
}

#[test]
fn an_oid_table_out_of_order_under_its_fanout_is_unknown() {
    let _g = serial();
    let (dir, graph, good) = graph_repo(40);
    let rows = good[6] as usize;
    let lookup = (0..rows)
        .find(|&r| &good[8 + r * 12..8 + r * 12 + 4] == b"OIDL")
        .map(|r| u64::from_be_bytes(good[8 + r * 12 + 4..8 + r * 12 + 12].try_into().unwrap()))
        .expect("OIDL row") as usize;
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output_locked()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap();
    let head: Vec<u8> = (0..20)
        .map(|i| u8::from_str_radix(&head[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let at = |i: usize| lookup + i * 20..lookup + (i + 1) * 20;
    let pos = (0..40)
        .find(|&i| good[at(i)] == head[..])
        .expect("HEAD is in the graph");
    // Swap HEAD's entry with one that starts with a different byte.
    let other = [0usize, 39, 20]
        .into_iter()
        .find(|&i| i != pos && good[lookup + i * 20] != head[0])
        .expect("an entry with another first byte");
    let mut bytes = good.clone();
    let (a, b) = (good[at(pos)].to_vec(), good[at(other)].to_vec());
    bytes[at(pos)].copy_from_slice(&b);
    bytes[at(other)].copy_from_slice(&a);
    reseal(&mut bytes);
    put_graph(&graph, &bytes);
    assert!(
        !git_accepts_graph(dir.path()),
        "oracle: git must reject an unsorted OID table"
    );
    assert_state(&inspect_only(dir.path()), CommitGraphState::Unknown);
}

#[test]
fn a_hostile_ref_name_cannot_flood_or_dress_up_the_note() {
    let _g = serial();
    let (dir, _graph, _good) = graph_repo(2);
    let git_dir = dir.path().join(".git");
    // `..` makes the name unusable, which is the path that quotes it.
    let name = format!("refs/heads/../{}\u{1b}[31m\tX", "a".repeat(60_000));
    std::fs::write(git_dir.join("HEAD"), format!("ref: {name}\n")).unwrap();
    std::fs::write(
        git_dir.join("refs").join("heads").join("x"),
        "ref: refs/heads/\u{1b}\n",
    )
    .unwrap();
    let report = inspect(dir.path(), &git_dir, &git_dir);
    let note = report.note.expect("a note");
    assert!(note.chars().count() <= 240, "note is {} chars", note.len());
    assert!(!note.chars().any(char::is_control), "{note:?}");
}

#[test]
fn structural_mutations_of_a_chain_layer_git_rejects_never_read_as_a_verdict() {
    let _g = serial();
    let (dir, _chain, lines) = chain_repo();
    let layer = dir.path().join(format!(
        ".git/objects/info/commit-graphs/graph-{}.graph",
        lines[1]
    ));
    let good = std::fs::read(&layer).unwrap();
    let table_end = 8 + (good[6] as usize + 1) * 12;
    let structural = table_end + 1024;
    let mut rng = Rng(0x00dd_c0ff_ee15_600d);
    let mut offenders = Vec::new();
    let mut rejected = 0;
    for round in 0..80 {
        let mut bytes = good.clone();
        for _ in 0..=rng.below(3) {
            let i = rng.below(structural.min(bytes.len()));
            bytes[i] = if rng.below(2) == 0 {
                rng.next() as u8
            } else {
                bytes[i] ^ (1 << rng.below(8))
            };
        }
        reseal(&mut bytes);
        put_graph(&layer, &bytes);
        if git_accepts_graph(dir.path()) {
            continue;
        }
        rejected += 1;
        let report = inspect_only(dir.path());
        if report.state != CommitGraphState::Unknown {
            offenders.push(format!("round {round}: {:?}", report.state));
        }
    }
    assert!(
        rejected > 20,
        "the fuzz barely produced rejected graphs ({rejected})"
    );
    assert!(offenders.is_empty(), "{offenders:?}");
}
