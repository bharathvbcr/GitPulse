//! Command timings on a generated 18-worktree repository: the insights
//! snapshot, `list_worktrees` (the command's default `Listing` depth and the
//! Worktrees panel's `Full` depth), language statistics, docs refresh, `catch_up`
//! and commit details.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench command_timings`.
//!
//! These are the commands the 2026-10-12 lag audit names (insights, listing,
//! language stats, docs refresh, catch-up, commit details). Before this bench
//! the only evidence was the slow-command log from one developer's machine;
//! this gives each one a repeatable number on a fixed fixture, with the work it
//! did beside the time:
//!
//! * the fixture: 600 tracked files in 30 directories, 61 commits, and 17
//!   linked worktrees (18 in all), each linked worktree with one dirty file;
//!   markdown files for the docs vault; 60 transcript files for `catch_up`;
//! * counters: worktrees listed, files scanned, notes indexed, ledger events
//!   recorded, and (insights, both listings) the git processes started, counted from
//!   git's own trace2 in a separate untimed pass;
//! * `catch_up` has two rows. `steady` is the re-open cost: the watermark makes
//!   every transcript a skip, which is what a user pays on each launch. `cold`
//!   is the first pass over a fresh repository and transcript set, rebuilt for
//!   every sample outside the timed span.
//!
//! Every response is checked: an `ok: false` facet in the insights snapshot,
//! a truncated language scan or a deadline-expired snapshot aborts the run
//! instead of being timed as if it were the work.

mod common;

use common::fixture::{build_repo, count_git_processes, git_process_argvs, Repo, Spec};
use common::{interleave, require_no_failed_facets, Case, Report, Sample};
use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::engine::worktree::ScanDepth;
use gitpulse_lib::{docs, ingest, insights};
use serde_json::json;
use std::path::Path;

const SPEC: Spec = Spec {
    files: 600,
    dirs: 30,
    extra_commits: 60,
    touch: 5,
    linked_worktrees: 17,
};

/// The ref-listing fixture: few files, enough commits for every ref to sit at
/// a different one, and 7 linked worktrees (8 in all) whose branches the
/// divergence batch measures.
const REF_SPEC: Spec = Spec {
    files: 40,
    dirs: 4,
    extra_commits: 60,
    touch: 2,
    linked_worktrees: 7,
};

/// Branches and tags added to the ref-listing fixture, beside the worktrees'.
const REF_BRANCHES: usize = 220;
const REF_TAGS: usize = 60;

fn git_lines(repo: &Path, args: &[&str]) -> Vec<String> {
    common::fixture::git(repo, args)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Applies `commands` (`git update-ref --stdin` lines) in one process.
#[cfg(unix)]
fn update_refs(repo: &Path, commands: &str) {
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git update-ref");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(commands.as_bytes())
        .expect("write the ref commands");
    let out = child.wait_with_output().expect("wait for update-ref");
    assert!(
        out.status.success(),
        "update-ref failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(not(unix))]
fn update_refs(repo: &Path, commands: &str) {
    for line in commands.lines() {
        let words: Vec<&str> = line.split(' ').skip(1).collect();
        common::fixture::git(repo, &[&["update-ref"], words.as_slice()].concat());
    }
}

/// `count` transcripts, each with `edits` assistant tool_use lines naming a
/// file inside `repo`, in the shape `tests/catch_up_integration.rs` uses.
fn write_transcripts(root: &Path, repo: &str, count: usize, edits: usize) {
    let slug = root.join("project-slug");
    std::fs::create_dir_all(&slug).expect("transcript dir");
    for session in 0..count {
        let mut text = String::new();
        for edit in 0..edits {
            let line = json!({
                "type": "assistant",
                "sessionId": format!("S{session}"),
                "timestamp": format!("2026-09-01T12:{:02}:{:02}.000Z", session % 60, edit % 60),
                "version": "2.1.241",
                "cwd": repo,
                "gitBranch": "main",
                "message": { "content": [
                    { "type": "tool_use", "name": "Edit",
                      "input": { "file_path": format!("{repo}/d{:03}/f{:06}.rs", edit % 30, edit * 8) } }
                ]}
            });
            text.push_str(&line.to_string());
            text.push('\n');
        }
        std::fs::write(slug.join(format!("S{session}.jsonl")), text).expect("write transcript");
    }
}

fn catch_up_sample(root: &Path, repo: &str) -> (Sample, std::time::Duration) {
    std::env::set_var("GITPULSE_TRANSCRIPT_ROOT", root);
    let started = std::time::Instant::now();
    let out = ingest::catch_up(repo);
    let elapsed = started.elapsed();
    assert!(
        !out.truncated && out.error.is_empty(),
        "catch_up degraded ({}), so this run would time a partial pass",
        out.error
    );
    (
        Sample::default()
            .counter("recorded", out.recorded as u64)
            .counter("transcripts", out.transcripts as u64)
            .counter("reflog_entries", out.reflog_entries as u64)
            .counter("skipped_lines", out.skipped_lines as u64),
        elapsed,
    )
}

/// Commits in each `pulse signed` chain.
const PULSE_COMMITS: usize = 500;

/// How long the stand-in signing program sleeps per call, the cost of a real
/// verifier (a PGP commit measured ten seconds) scaled down to bench size.
const VERIFIER_DELAY_MS: u32 = 20;

/// Set to time a build that still verifies in the bulk walk. Without it a
/// walk that starts the signing program at all aborts the run: the row would
/// otherwise time the verifier instead of the walk.
const ALLOW_VERIFIER_ENV: &str = "GITPULSE_BENCH_ALLOW_VERIFIER";

/// Writes one object of `kind` and returns its id.
#[cfg(unix)]
fn hash_object(repo: &Path, kind: &str, content: &[u8]) -> String {
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["hash-object", "-t", kind, "-w", "--stdin"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git hash-object");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(content)
        .expect("write object");
    let out = child.wait_with_output().expect("wait for hash-object");
    assert!(
        out.status.success(),
        "hash-object failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("object id")
        .trim()
        .to_string()
}

/// A chain of [`PULSE_COMMITS`] commits of which `signed` carry a signature
/// header (evenly spread), with every signing program set to `verifier`.
/// Commits are written by hand, so no key and no signing program is involved
/// in building it.
#[cfg(unix)]
fn signed_chain(root: &Path, signed: usize, verifier: &Path) -> String {
    use common::fixture::{git, trust_repo};
    let dir = root.join(format!("signed-{signed}"));
    std::fs::create_dir_all(&dir).expect("create chain dir");
    git(&dir, &["init", "-q", "-b", "main"]);
    for key in ["gpg.program", "gpg.ssh.program", "gpg.x509.program"] {
        git(&dir, &["config", key, verifier.to_str().expect("utf-8")]);
    }
    let tree = hash_object(&dir, "tree", b"");
    let every = (signed > 0).then(|| PULSE_COMMITS / signed);
    let mut parent: Option<String> = None;
    for n in 0..PULSE_COMMITS {
        let mut body = format!("tree {tree}\n");
        if let Some(parent) = &parent {
            body.push_str(&format!("parent {parent}\n"));
        }
        let who = format!("Bench <bench@example.com> {} +0000", 1_700_000_000 + n);
        body.push_str(&format!("author {who}\ncommitter {who}\n"));
        if every.is_some_and(|every| n % every == 0) {
            body.push_str(
                "gpgsig -----BEGIN PGP SIGNATURE-----\n \n bm90IGEgcmVhbCBzaWduYXR1cmU=\n -----END PGP SIGNATURE-----\n",
            );
        }
        body.push_str(&format!("\nfeat: change {n}\n"));
        parent = Some(hash_object(&dir, "commit", body.as_bytes()));
    }
    git(
        &dir,
        &["update-ref", "refs/heads/main", &parent.expect("a chain")],
    );
    trust_repo(&dir);
    let raw = git(&dir, &["log", "--format=raw"]);
    let carrying = raw
        .lines()
        .filter(|line| line.starts_with("gpgsig "))
        .count();
    assert_eq!(carrying, signed, "the fixture carries {signed} signatures");
    dir.to_str().expect("utf-8").to_string()
}

/// The `pulse signed 0|50|500` rows. Empty off unix, where the stand-in
/// signing program is a shell script.
#[cfg(unix)]
fn pulse_signed_cases(root: &Path, only: Option<&str>) -> Vec<Case<'static>> {
    use std::os::unix::fs::PermissionsExt;
    let stub_dir = root.join("verifier");
    std::fs::create_dir_all(&stub_dir).expect("create stub dir");
    let verifier = stub_dir.join("verifier");
    std::fs::write(
        &verifier,
        format!(
            "#!/bin/sh\necho x >> \"$(dirname \"$0\")/count\"\nsleep {:.3}\ncat >/dev/null\nexit 2\n",
            f64::from(VERIFIER_DELAY_MS) / 1000.0
        ),
    )
    .expect("write verifier stub");
    std::fs::set_permissions(&verifier, std::fs::Permissions::from_mode(0o755))
        .expect("stub executable");
    let allow_verifier = std::env::var_os(ALLOW_VERIFIER_ENV).is_some();
    let count_file = stub_dir.join("count");

    [0usize, 50, PULSE_COMMITS]
        .into_iter()
        .filter(|signed| wanted(only, &format!("pulse signed {signed}")))
        .map(|signed| {
            let path = signed_chain(root, signed, &verifier);
            let count_file = count_file.clone();
            let walk = {
                let path = path.clone();
                move || {
                    GitReader::pulse_report(&path, None)
                        .unwrap_or_else(|error| panic!("pulse report failed: {error}"))
                }
            };
            let _ = std::fs::remove_file(&count_file);
            let (report, git_processes) = count_git_processes(&walk);
            assert_eq!(
                report.commits.len(),
                PULSE_COMMITS,
                "the pulse walk did not read the whole chain, so this run would time a partial walk"
            );
            Case::new(format!("pulse signed {signed}"), move || {
                let _ = std::fs::remove_file(&count_file);
                let started = std::time::Instant::now();
                let report = walk();
                let elapsed = started.elapsed();
                let verifier_spawns = std::fs::read_to_string(&count_file)
                    .map_or(0, |text| text.lines().count() as u64);
                assert!(
                    allow_verifier || verifier_spawns == 0,
                    "the pulse walk started the signing program {verifier_spawns} times; \
                     set {ALLOW_VERIFIER_ENV} to time a build that does"
                );
                std::hint::black_box(&report);
                Sample::default()
                    .counter("commits", report.commits.len() as u64)
                    .counter("verifier_spawns", verifier_spawns)
                    .counter("git_processes", git_processes)
                    .metric("peak_rss_mb", peak_rss_mb())
                    .elapsed(elapsed)
            })
        })
        .collect()
}

#[cfg(not(unix))]
fn pulse_signed_cases(_root: &Path, _only: Option<&str>) -> Vec<Case<'static>> {
    Vec::new()
}

/// Whether the row `name` is selected by `GITPULSE_BENCH_ONLY` (all of them
/// when it is unset).
fn wanted(only: Option<&str>, name: &str) -> bool {
    only.is_none_or(|only| only == name)
}

/// This process's peak resident set so far, in MiB. A high-water mark for the
/// whole process, so it says something about one row only when that row is the
/// only work the process did: run it with `GITPULSE_BENCH_ONLY=<row>`.
#[cfg(unix)]
fn peak_rss_mb() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is a valid, writable rusage and RUSAGE_SELF needs no
    // other argument; it is read only after getrusage reports success.
    let peak = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return 0.0;
        }
        usage.assume_init().ru_maxrss as f64
    };
    // macOS reports bytes, Linux kilobytes.
    let bytes = if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024.0
    };
    bytes / (1024.0 * 1024.0)
}

#[cfg(not(unix))]
fn peak_rss_mb() -> f64 {
    0.0
}

/// A chain of `commits` unsigned empty-tree commits, written by one
/// `git fast-import`, so a 25,000-commit history costs seconds to build.
#[cfg(unix)]
fn unsigned_chain(root: &Path, commits: usize) -> String {
    use common::fixture::{git, trust_repo};
    use gitpulse_lib::procguard::LockedSpawn;
    use std::io::Write;
    let dir = root.join(format!("unsigned-{commits}"));
    std::fs::create_dir_all(&dir).expect("create chain dir");
    git(&dir, &["init", "-q", "-b", "main"]);
    let mut stream = String::new();
    for n in 0..commits {
        let message = format!("feat: change {n}");
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{}\ncommitter Bench <bench@example.com> {} +0000\ndata {}\n{message}\n",
            n + 1,
            1_700_000_000 + n,
            message.len()
        ));
        if n > 0 {
            stream.push_str(&format!("from :{n}\n"));
        }
        stream.push('\n');
    }
    let mut child = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git fast-import");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stream.as_bytes())
        .expect("write the import stream");
    let out = child.wait_with_output().expect("wait for fast-import");
    assert!(
        out.status.success(),
        "fast-import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    trust_repo(&dir);
    let counted = git(&dir, &["rev-list", "--count", "HEAD"]);
    assert_eq!(counted.trim(), commits.to_string(), "the chain is whole");
    dir.to_str().expect("utf-8").to_string()
}

/// The `pulse unsigned 500|5000|25000` rows: the pulse walk over histories
/// with no signed commit at all, which is what almost every repository is and
/// where any per-commit cost of reading signatures shows up undiluted. Empty
/// off unix.
#[cfg(unix)]
fn pulse_unsigned_cases(root: &Path, only: Option<&str>) -> Vec<Case<'static>> {
    [500usize, 5_000, 25_000]
        .into_iter()
        .filter(|commits| wanted(only, &format!("pulse unsigned {commits}")))
        .map(|commits| {
            let path = unsigned_chain(root, commits);
            let walk = move || {
                GitReader::pulse_report(&path, Some(commits))
                    .unwrap_or_else(|error| panic!("pulse report failed: {error}"))
            };
            let (report, git_processes) = count_git_processes(&walk);
            assert_eq!(
                report.commits.len(),
                commits,
                "the pulse walk did not read the whole chain, so this run would time a partial walk"
            );
            Case::new(format!("pulse unsigned {commits}"), move || {
                let started = std::time::Instant::now();
                let report = walk();
                let elapsed = started.elapsed();
                std::hint::black_box(&report);
                Sample::default()
                    .counter("commits", report.commits.len() as u64)
                    .counter("git_processes", git_processes)
                    .metric("peak_rss_mb", peak_rss_mb())
                    .elapsed(elapsed)
            })
        })
        .collect()
}

#[cfg(not(unix))]
fn pulse_unsigned_cases(_root: &Path, _only: Option<&str>) -> Vec<Case<'static>> {
    Vec::new()
}

fn head(repo: &Repo) -> String {
    common::fixture::git(&repo.main, &["rev-parse", "HEAD"])
        .trim()
        .to_string()
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("command_timings");
    let temp = tempfile::tempdir().expect("temp dir");

    // `GITPULSE_BENCH_ONLY=<row>` runs one pulse row in a process that did
    // nothing else, which is what makes its `peak_rss_mb` the row's own.
    if let Ok(name) = std::env::var("GITPULSE_BENCH_ONLY") {
        let root = temp.path().join("pulse");
        let mut cases = pulse_unsigned_cases(&root, Some(&name));
        cases.extend(pulse_signed_cases(&root, Some(&name)));
        assert!(
            !cases.is_empty(),
            "GITPULSE_BENCH_ONLY={name} names no pulse row"
        );
        report.note(format!(
            "only `{name}`: its peak_rss_mb is this process's whole high-water mark"
        ));
        let rows = interleave(cases, report.opts.samples(5), report.opts.warmup(1));
        report.extend(rows);
        report.finish();
        return;
    }
    // `git_preflight` caches a passing answer: warm it so a counted pass below
    // is its own work and not charged for the one-time `git version` probe.
    let preflight = gitpulse_lib::engine::git_cli::git_preflight();
    assert_eq!(
        preflight.status,
        gitpulse_lib::engine::git_cli::GitPreflightStatus::Ok,
        "git did not pass its preflight ({:?})",
        preflight.message
    );
    // The ref-listing rows: a repository with 220 branches, 60 tags and 8
    // worktrees, every ref at a different commit so ahead-behind has real work.
    // A ref is moved before every timed call, outside the span: the ref cache
    // never keeps an answer read inside its racy window of the last ref write,
    // so each timed call is the cold listing a watcher event pays.
    let refs = build_repo(&temp.path().join("refs"), &REF_SPEC);
    let refs_path = refs.path().to_string();
    let tips = git_lines(&refs.main, &["rev-list", "HEAD"]);
    let mut stdin = String::new();
    for n in 0..REF_BRANCHES {
        stdin.push_str(&format!(
            "create refs/heads/bench/b{n:03} {}\n",
            tips[n % tips.len()]
        ));
    }
    for n in 0..REF_TAGS {
        stdin.push_str(&format!(
            "create refs/tags/t{n:03} {}\n",
            tips[(n * 3) % tips.len()]
        ));
    }
    update_refs(&refs.main, &stdin);
    let churn_tips = (tips[1].clone(), tips[2].clone());
    let churn_flip = std::cell::Cell::new(false);
    let churn = || {
        let flip = !churn_flip.get();
        churn_flip.set(flip);
        let tip = if flip { &churn_tips.0 } else { &churn_tips.1 };
        common::fixture::git(&refs.main, &["update-ref", "refs/heads/bench-churn", tip]);
    };
    let timed = |run: &dyn Fn() -> u64| {
        churn();
        let started = std::time::Instant::now();
        let rows = run();
        (rows, started.elapsed())
    };
    let branch_rows = || {
        let listed = GitReader::list_branches(&refs_path)
            .unwrap_or_else(|error| panic!("list_branches failed: {error}"));
        assert!(
            listed.len() >= REF_BRANCHES && listed.iter().all(|b| b.compared_to.is_some()),
            "a branch row was dropped or uncompared, so this run would time a degraded listing"
        );
        std::hint::black_box(&listed);
        listed.len() as u64
    };
    let tag_rows = || {
        let listed = GitReader::list_tags(&refs_path)
            .unwrap_or_else(|error| panic!("list_tags failed: {error}"));
        assert!(
            listed.tags.len() == REF_TAGS && listed.tags.iter().all(|t| t.compared_to.is_some()),
            "a tag row was dropped or uncompared, so this run would time a degraded listing"
        );
        std::hint::black_box(&listed);
        listed.tags.len() as u64
    };
    let divergence_rows = || {
        let listed = gitpulse_lib::engine::worktree::list_worktrees(&refs_path)
            .unwrap_or_else(|error| panic!("list_worktrees failed: {error}"));
        assert!(
            listed.len() == REF_SPEC.linked_worktrees + 1
                && listed
                    .iter()
                    .filter(|w| w.branch.as_deref().is_some_and(|b| b.starts_with("wt/")))
                    .all(|w| w.main_divergence.is_some()),
            "a worktree has no divergence, so this run would time the per-worktree walks"
        );
        std::hint::black_box(&listed);
        listed.len() as u64
    };
    // Untimed pass: git processes per call, and for-each-ref/tag among them.
    let listing_spawns = |run: &dyn Fn() -> u64| {
        churn();
        let (_, argvs) = git_process_argvs(run);
        let listings = argvs
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "for-each-ref" || a == "tag"))
            .count() as u64;
        (argvs.len() as u64, listings)
    };
    let (branch_procs, branch_listings) = listing_spawns(&branch_rows);
    let (tag_procs, tag_listings) = listing_spawns(&tag_rows);
    let (divergence_procs, divergence_listings) = listing_spawns(&divergence_rows);

    let ref_cases = || {
        vec![
            Case::new("list_branches cold", || {
                let (rows, elapsed) = timed(&branch_rows);
                Sample::default()
                    .counter("rows", rows)
                    .counter("git_processes", branch_procs)
                    .counter("listing_processes", branch_listings)
                    .elapsed(elapsed)
            }),
            Case::new("list_tags cold", || {
                let (rows, elapsed) = timed(&tag_rows);
                Sample::default()
                    .counter("rows", rows)
                    .counter("git_processes", tag_procs)
                    .counter("listing_processes", tag_listings)
                    .elapsed(elapsed)
            }),
            Case::new("worktree divergence cold", || {
                let (rows, elapsed) = timed(&divergence_rows);
                Sample::default()
                    .counter("worktrees", rows)
                    .counter("git_processes", divergence_procs)
                    .counter("listing_processes", divergence_listings)
                    .elapsed(elapsed)
            }),
        ]
    };
    // `GITPULSE_BENCH_REFS_ONLY=1` times just the ref-listing rows, which is
    // what an A/B of the listing code needs without the other rows' minutes.
    if std::env::var_os("GITPULSE_BENCH_REFS_ONLY").is_some() {
        let rows = interleave(ref_cases(), report.opts.samples(15), report.opts.warmup(2));
        report.extend(rows);
        report.finish();
        return;
    }

    let repo = build_repo(&temp.path().join("fixture"), &SPEC);
    let path = repo.path().to_string();
    let commit = head(&repo);
    report.note(format!(
        "fixture: {} files, {} commits, {} worktrees ({} dirty), hermetic HOME, production spawn rate lifted",
        SPEC.files,
        SPEC.extra_commits + 1,
        SPEC.linked_worktrees + 1,
        SPEC.linked_worktrees
    ));

    // Transcripts for the steady catch_up row; one cold pass settles the
    // watermark so the timed passes are the re-open cost.
    let steady_root = temp.path().join("transcripts-steady");
    write_transcripts(&steady_root, &path, 60, 8);

    let snapshot = |path: &str| {
        let snapshot = insights::snapshot(path);
        assert!(
            !snapshot.deadline_expired,
            "the insights snapshot hit its deadline: this run would time a partial snapshot"
        );
        let value = serde_json::to_value(&snapshot).expect("snapshot serializes");
        let facets_ok = require_no_failed_facets("insights::snapshot", &value);
        (snapshot, facets_ok)
    };
    // The Worktrees panel's explicit opt-in: `ScanDepth::Full`.
    let list_full = |path: &str| {
        gitpulse_lib::engine::worktree::list_worktrees(path)
            .unwrap_or_else(|error| panic!("list_worktrees full failed: {error}"))
    };
    // What `cmd_list_worktrees` runs by default
    // (`depth.unwrap_or(ScanDepth::Listing)`): one `git worktree list`. At this
    // depth a failure is the `Err`, and an entry cannot carry a partial scan,
    // so the only way to time a degraded answer is a short list.
    let list_listing = |path: &str| {
        let worktrees =
            gitpulse_lib::engine::worktree::list_worktrees_scanned(path, ScanDepth::Listing)
                .unwrap_or_else(|error| panic!("list_worktrees listing failed: {error}"));
        assert_eq!(
            worktrees.len(),
            SPEC.linked_worktrees + 1,
            "the listing did not return every worktree, so this run would time a partial answer"
        );
        worktrees
    };

    // `git_preflight` runs `git version` once per process and remembers a
    // passing answer; `git_supports` (commit details' merge rendering) is the
    // first caller to trigger it. Production pays that once, off the per-call
    // path, so warm it here: each row's counter is then its own work, and a
    // row that happens to run first is not charged for the probe.
    let preflight = gitpulse_lib::engine::git_cli::git_preflight();
    assert_eq!(
        preflight.status,
        gitpulse_lib::engine::git_cli::GitPreflightStatus::Ok,
        "git did not pass its preflight ({:?}), so the warm-up did not cache and a counted row would pay the probe",
        preflight.message
    );

    // Work counters (git processes) in their own untimed pass.
    let (_, snapshot_procs) = count_git_processes(|| snapshot(&path));
    let (_, list_full_procs) = count_git_processes(|| list_full(&path));
    let (_, list_listing_procs) = count_git_processes(|| list_listing(&path));
    // Unbounded: the production 10 s deadline is a property of the host's load,
    // and a scan it cut short would be timed as if it were the whole work.
    let (_, lang_procs) =
        count_git_processes(|| GitReader::get_repo_language_stats_bounded(&path, None));
    let (_, details_argvs) = git_process_argvs(|| GitReader::get_commit_details(&path, &commit));
    // Pins the warm-up: the probe is `git version`, and none may land in the
    // counted pass, or the row would charge commit details a one-time cost.
    // trace2's argv is the git binary, then the `-c key=value` pairs the app
    // puts first, then the subcommand, so `version` is matched as a whole word
    // anywhere after argv[0].
    assert!(
        !details_argvs
            .iter()
            .any(|argv| argv.iter().skip(1).any(|word| word == "version")),
        "commit details started `git version` inside the counted pass; the git_preflight \
         warm-up must run before it: {details_argvs:?}"
    );
    let details_procs = details_argvs.len() as u64;

    // Settle catch_up's watermark (first pass over the transcripts).
    let (first, _) = catch_up_sample(&steady_root, &path);
    report.note(format!(
        "catch_up first pass recorded {} events from {} transcripts (the timed `steady` rows start after it)",
        first.counters["recorded"], first.counters["transcripts"]
    ));

    // Ownership sample sizes: every `cold` call takes the next one up from 20,
    // and the primed `warm` size is the production default, which no cold call
    // reaches. The ref cache does not keep an answer read inside its racy
    // window of the repository's last write, so wait that out before priming.
    const WARM_OWNERSHIP_SAMPLE: usize = 128;
    let next_cold = std::cell::Cell::new(20usize);
    std::thread::sleep(std::time::Duration::from_secs(4));
    GitReader::knowledge_report(&path, Some(WARM_OWNERSHIP_SAMPLE)).expect("prime ownership");

    let mut cases = ref_cases();
    cases.extend(vec![
        Case::new("insights snapshot", || {
            let (snap, facets_ok) = snapshot(&path);
            std::hint::black_box(&snap);
            Sample::default()
                .counter("worktrees", snap.worktrees.count as u64)
                .counter("facets_ok", facets_ok)
                .counter("git_processes", snapshot_procs)
        }),
        Case::new("list_worktrees full", || {
            let worktrees = list_full(&path);
            std::hint::black_box(&worktrees);
            Sample::default()
                .counter("worktrees", worktrees.len() as u64)
                .counter("git_processes", list_full_procs)
        }),
        Case::new("list_worktrees listing", || {
            let worktrees = list_listing(&path);
            std::hint::black_box(&worktrees);
            Sample::default()
                .counter("worktrees", worktrees.len() as u64)
                .counter("git_processes", list_listing_procs)
        }),
        Case::new("language stats", || {
            let report = GitReader::get_repo_language_stats_bounded(&path, None)
                .unwrap_or_else(|error| panic!("language stats failed: {error}"));
            assert!(
                !report.truncated,
                "the language scan was truncated ({} of {} candidates scanned), so this run \
                 would time a partial scan",
                report.scanned_files, report.candidate_files
            );
            std::hint::black_box(&report);
            Sample::default()
                .counter("scanned_files", report.scanned_files as u64)
                .counter("candidate_files", report.candidate_files as u64)
                .counter("languages", report.stats.len() as u64)
                .counter("git_processes", lang_procs)
        }),
        Case::new("docs refresh", || {
            let status =
                docs::refresh(&path).unwrap_or_else(|error| panic!("docs refresh failed: {error}"));
            assert!(
                !status.truncated && status.skipped_unreadable == 0,
                "the docs vault was partial: {status:?}"
            );
            Sample::default().counter("notes", u64::from(status.note_count))
        }),
        Case::new("catch_up steady", || {
            let (sample, elapsed) = catch_up_sample(&steady_root, &path);
            sample.elapsed(elapsed)
        }),
        // The ownership report on the 600-file fixture. `cold` asks for a
        // sample size no earlier sample used, so no kept answer can serve it;
        // `warm` repeats one size primed above, which a build with no memo
        // measures again every time.
        Case::new("ownership cold", || {
            let size = next_cold.get();
            next_cold.set(size + 1);
            let started = std::time::Instant::now();
            let report = GitReader::knowledge_report(&path, Some(size))
                .unwrap_or_else(|error| panic!("ownership report failed: {error}"));
            let elapsed = started.elapsed();
            assert_eq!(
                report.scanned_files, size,
                "the sample was not fully blamed"
            );
            std::hint::black_box(&report);
            Sample::default()
                .metric("scanned_files", report.scanned_files as f64)
                .elapsed(elapsed)
        }),
        Case::new("ownership warm", || {
            let started = std::time::Instant::now();
            let report = GitReader::knowledge_report(&path, Some(WARM_OWNERSHIP_SAMPLE))
                .unwrap_or_else(|error| panic!("ownership report failed: {error}"));
            let elapsed = started.elapsed();
            std::hint::black_box(&report);
            Sample::default()
                .counter("scanned_files", report.scanned_files as u64)
                .elapsed(elapsed)
        }),
        Case::new("commit details", || {
            let details = GitReader::get_commit_details(&path, &commit)
                .unwrap_or_else(|error| panic!("commit details failed: {error}"));
            std::hint::black_box(&details);
            Sample::default()
                .counter("parents", details.parent_ids.len() as u64)
                .counter("git_processes", details_procs)
        }),
    ]);
    let mut rows = interleave(cases, report.opts.samples(15), report.opts.warmup(2));

    // The cold pass: a fresh repository and transcript set per sample, built
    // outside the timed span.
    let cold = Case::new("catch_up cold", || {
        let dir = tempfile::tempdir().expect("temp dir");
        let small = Spec {
            files: 50,
            dirs: 5,
            extra_commits: 4,
            touch: 2,
            linked_worktrees: 0,
        };
        let fresh = build_repo(&dir.path().join("cold"), &small);
        let root = dir.path().join("transcripts");
        write_transcripts(&root, fresh.path(), 60, 8);
        let (sample, elapsed) = catch_up_sample(&root, fresh.path());
        sample.elapsed(elapsed)
    });
    rows.extend(interleave(vec![cold], report.opts.samples(5), 0));

    // The pulse walk over histories that differ only in how many commits are
    // signed. The fixtures are built here, outside every timed span.
    let signed_root = temp.path().join("pulse-signed");
    let signed_cases = pulse_signed_cases(&signed_root, None);
    let unsigned_cases = pulse_unsigned_cases(&signed_root, None);
    if !signed_cases.is_empty() {
        report.note(format!(
            "pulse signed: {PULSE_COMMITS}-commit chains of hand-written commits carrying 0, 50 and \
             {PULSE_COMMITS} fake signatures, a signing program that counts its calls and sleeps \
             {} ms per call; `verifier_spawns` is how many times it ran during one walk",
            VERIFIER_DELAY_MS
        ));
        rows.extend(interleave(
            signed_cases,
            report.opts.samples(5),
            report.opts.warmup(1),
        ));
    }
    if !unsigned_cases.is_empty() {
        report.note(
            "pulse unsigned: 500, 5,000 and 25,000 unsigned commits (one fast-import each); \
             the walk's own cost with nothing to verify, which is where reading signatures \
             costs the most if it costs anything per commit",
        );
        rows.extend(interleave(
            unsigned_cases,
            report.opts.samples(5),
            report.opts.warmup(1),
        ));
    }

    report.extend(rows);
    report.finish();
}
