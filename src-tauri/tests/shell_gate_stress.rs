//! Adversarial stress for the two things this audit changed: the shell-open
//! containment gate, and the char-boundary class that crashed the app.
//!
//! Deterministic by construction, matching `lane_solver_fuzz.rs`: every random
//! case comes from an inline LCG seeded from a constant, so a failure prints
//! the seed that reproduces it. No external property-testing crates.
//!
//! Two invariants are under test, and both are absolute — no input of any
//! shape may violate them:
//!
//! 1. `resolve_worktree_path` either returns an error, or returns a path that
//!    really is inside the canonicalized repository root. There is no third
//!    outcome, and in particular no panic.
//! 2. Reading a commit whose message carries multi-byte text never panics,
//!    at any byte offset. The crash this audit began from was
//!    `parse_co_authors` splitting `&str` at a fixed byte index that landed
//!    inside an em dash; the fix must hold for every offset, not the one in
//!    the log.
//!
//! Set `DEEP_FUZZ_SCALE=20` to widen the random sweeps for a longer run.

use gitpulse_lib::desktop::shell::resolve_worktree_path;
use gitpulse_lib::engine::GitReader;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

/// The same LCG the lane fuzzer uses, so seeds behave the same way here.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() >> 33) as usize % items.len()]
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 33) as usize % n.max(1)
    }
}

fn scale() -> u64 {
    std::env::var("DEEP_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

/// Everything worth knowing about a fixture repository whose git command has
/// just failed.
///
/// A commit loop that dies reporting only `fatal: could not parse HEAD` is a
/// failure nobody can act on: it does not say whether the object store is
/// genuinely corrupt, whether HEAD names a ref that is missing, or how far
/// the loop had got. Read-only, and built only on the failure path.
fn repo_state(dir: &Path) -> String {
    let read = |args: &[&str]| -> String {
        match Command::new("git").args(args).current_dir(dir).output() {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !out.status.success() {
                    text.push_str(&format!(
                        "<failed: {}>",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                text
            }
            Err(err) => format!("<could not run: {err}>"),
        }
    };
    let file = |path: &str| -> String {
        fs::read_to_string(dir.join(path))
            .map(|text| text.trim().to_string())
            .unwrap_or_else(|err| format!("<unreadable: {err}>"))
    };
    let count = |path: &str| -> usize {
        fs::read_dir(dir.join(path))
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0)
    };
    format!(
        "\n    loop marker (f.txt) = {}\
         \n    .git/HEAD           = {}\
         \n    rev-parse HEAD      = {}\
         \n    cat-file -t HEAD    = {}\
         \n    fsck                = {}\
         \n    object fanouts      = {}, pack dir entries = {}\
         \n    index.lock held     = {}\
         \n    .git/gc.log         = {}",
        file("f.txt"),
        file(".git/HEAD"),
        read(&["rev-parse", "HEAD"]),
        read(&["cat-file", "-t", "HEAD"]),
        read(&["fsck", "--no-progress"]),
        count(".git/objects"),
        count(".git/objects/pack"),
        dir.join(".git/index.lock").exists(),
        file(".git/gc.log"),
    )
}

/// Writes one commit per message in a single `git fast-import` stream and
/// returns their object ids, oldest first.
///
/// The obvious way to build a fixture with a few hundred commits is a loop
/// around `git commit`, which is what these tests used to do — and what made
/// them flaky. Several hundred rapid commit processes against one repository
/// drive git's own auto-maintenance hard enough that the loop intermittently
/// ended up with `refs/heads/main` pointing at a commit object that no longer
/// existed (`git fsck`: `invalid sha1 pointer`), failing roughly one run in
/// four with `fatal: could not parse HEAD`. One import process writes the same
/// history with no index churn, no ref hammering and nothing to repack.
///
/// Messages are stored byte for byte. `git commit -m` applies its default
/// cleanup and strips trailing blank lines, which is precisely what a test
/// about exact byte offsets inside a message does not want.
fn import_commits(repo: &Path, messages: &[String]) -> Vec<String> {
    let marks_path = repo.join(".git").join("import-marks");
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet", "--date-format=raw"])
        .arg(format!("--export-marks={}", marks_path.display()))
        .current_dir(repo)
        .stdin(Stdio::piped())
        .spawn()
        .expect("git fast-import must run");

    // Recent and strictly increasing, so a reader that only looks at a window
    // of recent history still sees every commit this fixture wrote.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(1_700_000_000);
    let base = now.saturating_sub(messages.len() as u64);

    let mut stdin = child.stdin.take().expect("fast-import stdin");
    for (index, message) in messages.iter().enumerate() {
        // Git stores a message with a trailing newline; keep that, so these
        // commits are byte-identical to what `git commit` would have written.
        let body = format!("{message}\n");
        let content = format!("{index}\n");
        let header = format!(
            "commit refs/heads/main\nmark :{mark}\ncommitter GitPulse <gitpulse@example.com> {when} +0000\ndata {length}\n",
            mark = index + 1,
            when = base + index as u64,
            length = body.len(),
        );
        let file_header = format!("M 100644 inline f.txt\ndata {}\n", content.len());
        for part in [
            header.as_bytes(),
            body.as_bytes(),
            file_header.as_bytes(),
            content.as_bytes(),
            b"\n",
        ] {
            stdin
                .write_all(part)
                .expect("fast-import stream must accept");
        }
    }
    stdin
        .write_all(b"done\n")
        .expect("fast-import must accept done");
    drop(stdin);

    let status = child.wait().expect("fast-import must finish");
    assert!(
        status.success(),
        "git fast-import failed: {status}{}",
        repo_state(repo)
    );

    // `:mark <oid>` per line, in no guaranteed order, so index by the mark
    // rather than trusting the file's sequence.
    let exported = fs::read_to_string(&marks_path).expect("fast-import must export marks");
    let mut ids = vec![String::new(); messages.len()];
    for line in exported.lines() {
        let (mark, oid) = line.split_once(' ').expect("a mark line is `:n <oid>`");
        let position: usize = mark
            .trim_start_matches(':')
            .parse()
            .expect("a mark is numbered");
        ids[position - 1] = oid.trim().to_string();
    }
    assert!(
        ids.iter().all(|id| !id.is_empty()),
        "every imported commit must come back with an id, or this fixture is \
         proving less than it looks like it is"
    );

    // Leave a normal checkout behind: the import touches neither index nor
    // worktree, and a reader handed a repository whose HEAD disagrees with
    // both is being asked a different question than the one under test.
    git_in(repo, &["reset", "--hard", "main"]);
    ids
}

fn git_in(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must run");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stderr).trim(),
        repo_state(dir)
    );
    if args.first() == Some(&"init") {
        common::trust_repo(dir);
    }
}

// ---------------------------------------------------------------------------
// Invariant 1: containment
// ---------------------------------------------------------------------------

/// Fragments chosen to attack every rule in the gate at once: traversal in
/// both separator styles, absolute and drive-qualified roots, empty and dot
/// segments, multi-byte text, and characters that are legal in a filename on
/// one platform and a separator on another.
const HOSTILE: &[&str] = &[
    "..",
    ".",
    "",
    "/",
    "\\",
    "//",
    "\\\\",
    "src",
    "a b",
    "a\tb",
    "...",
    "....",
    "C:",
    "c:",
    "~",
    "$HOME",
    "%2e%2e",
    "..%2f",
    "\u{2014}",
    "日本語",
    "ファイル—名",
    "e\u{301}",
    "\u{1f600}",
    "\u{202e}txt.exe",
    ".git",
    "HEAD",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "con",
    "nul",
    "file ",
    "file.",
];

/// The one thing that must always be true, whatever the gate decides.
fn assert_contained_or_refused(root: &Path, repo_arg: &str, relative: &str) {
    match resolve_worktree_path(repo_arg, relative) {
        Err(message) => {
            assert!(
                !message.trim().is_empty(),
                "a refusal of {relative:?} must say why"
            );
        }
        Ok(resolved) => {
            let canonical_root = root.canonicalize().expect("root canonicalizes");
            assert!(
                resolved.starts_with(&canonical_root),
                "resolve_worktree_path accepted {relative:?} and returned {resolved:?}, \
                 which is outside {canonical_root:?}"
            );
            assert!(
                resolved.exists(),
                "accepted {relative:?} but {resolved:?} does not exist"
            );
        }
    }
}

fn hostile_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for file in [
        "src/main.rs",
        "src/nested/deep/file.txt",
        "docs/日本語/ファイル—名.md",
        "docs/a b/c d.txt",
        "docs/dots...txt",
        ".hidden/secret.txt",
    ] {
        let path = dir.path().join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"x").expect("write");
    }
    dir
}

#[test]
fn containment_holds_for_every_generated_relative_path() {
    let repo = hostile_repo();
    let root = repo.path().to_path_buf();
    let repo_arg = root.to_string_lossy().into_owned();

    let seed = 0x5eed_1234_u64;
    let mut rng = Lcg(seed);
    let cases = 20_000 * scale();

    for i in 0..cases {
        let depth = 1 + rng.below(6);
        let mut parts: Vec<String> = Vec::with_capacity(depth);
        for _ in 0..depth {
            parts.push((*rng.pick(HOSTILE)).to_string());
        }
        let separator = *rng.pick(&["/", "\\", "//", "/./", "/../"]);
        let candidate = parts.join(separator);
        // Any failure here reproduces with this seed and iteration.
        assert_contained_or_refused(&root, &repo_arg, &candidate);
        if i == 0 {
            // A trivially-true sweep would be worthless; prove the corpus
            // contains at least one path the gate accepts and one it refuses.
            assert!(resolve_worktree_path(&repo_arg, "src/main.rs").is_ok());
            assert!(resolve_worktree_path(&repo_arg, "../escape").is_err());
        }
    }
    eprintln!("containment sweep: {cases} cases from seed {seed:#x}");
}

/// Percent-encoding, URL escapes and lookalike separators must not be decoded
/// into anything that walks out. The gate never decodes, which is what makes
/// this safe — pinned so a future "helpful" normalization step fails here.
#[test]
fn encoded_traversal_is_never_decoded_into_an_escape() {
    let repo = hostile_repo();
    let repo_arg = repo.path().to_string_lossy().into_owned();
    for probe in [
        "%2e%2e/outside",
        "..%2foutside",
        "%2E%2E%2Foutside",
        "src/%2e%2e/%2e%2e/outside",
        "\u{ff0e}\u{ff0e}/outside",
        "\u{2024}\u{2024}/outside",
        "src/\u{0000}../outside",
    ] {
        match resolve_worktree_path(&repo_arg, probe) {
            Ok(resolved) => {
                let root = repo.path().canonicalize().expect("root");
                assert!(
                    resolved.starts_with(&root),
                    "{probe:?} resolved outside the repo to {resolved:?}"
                );
            }
            Err(_) => { /* refusing is the expected outcome */ }
        }
    }
}

/// A symlink maze: chains, loops and links that leave the repo. Canonicalize
/// resolves the chain, so containment is judged on the real destination; a
/// loop must produce an error rather than hanging or panicking.
#[cfg(unix)]
#[test]
fn symlink_mazes_never_escape_and_never_hang() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().expect("temp dir");
    let repo = parent.path().join("repo");
    fs::create_dir_all(repo.join("inside")).expect("mkdir");
    fs::write(repo.join("inside/real.txt"), b"x").expect("write");
    fs::write(parent.path().join("outside.txt"), b"secret").expect("write");

    symlink(parent.path().join("outside.txt"), repo.join("escape")).expect("escape link");
    symlink(repo.join("inside/real.txt"), repo.join("hop1")).expect("hop1");
    symlink(repo.join("hop1"), repo.join("hop2")).expect("hop2");
    symlink(repo.join("hop2"), repo.join("hop3")).expect("hop3");
    symlink(repo.join("loop_b"), repo.join("loop_a")).expect("loop a");
    symlink(repo.join("loop_a"), repo.join("loop_b")).expect("loop b");
    symlink(parent.path(), repo.join("up")).expect("up link");

    let repo_arg = repo.to_string_lossy().into_owned();

    assert!(
        resolve_worktree_path(&repo_arg, "escape").is_err(),
        "a link out of the repo is not inside it"
    );
    assert!(
        resolve_worktree_path(&repo_arg, "up/outside.txt").is_err(),
        "a link to the parent directory must not become a way out"
    );
    assert!(
        resolve_worktree_path(&repo_arg, "loop_a").is_err(),
        "a symlink loop must error, not hang"
    );
    let hopped = resolve_worktree_path(&repo_arg, "hop3").expect("a chain that stays inside");
    assert!(hopped.ends_with("real.txt"), "{hopped:?}");
}

/// The gate is called from async command handlers, so it runs concurrently.
/// It holds no state, and this proves the claim rather than assuming it.
#[test]
fn containment_holds_under_concurrent_hammering() {
    let repo = Arc::new(hostile_repo());
    let repo_arg = Arc::new(repo.path().to_string_lossy().into_owned());
    let root = Arc::new(repo.path().to_path_buf());

    let threads = 16;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::with_capacity(threads);

    for t in 0..threads {
        let repo_arg = Arc::clone(&repo_arg);
        let root = Arc::clone(&root);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut rng = Lcg(0xc0ffee ^ t as u64);
            barrier.wait();
            for _ in 0..2_000 * scale() {
                let depth = 1 + rng.below(4);
                let parts: Vec<String> = (0..depth)
                    .map(|_| (*rng.pick(HOSTILE)).to_string())
                    .collect();
                let candidate = parts.join(*rng.pick(&["/", "\\", "/../"]));
                assert_contained_or_refused(&root, &repo_arg, &candidate);
            }
        }));
    }
    for handle in handles {
        handle.join().expect("no thread may panic");
    }
}

/// A repository root that is itself hostile — a symlink, a relative path, a
/// path with a trailing separator, a file rather than a directory.
#[test]
fn hostile_repository_roots_are_refused_or_handled() {
    let repo = hostile_repo();
    let root = repo.path().to_string_lossy().into_owned();

    // A trailing separator is ordinary and must still work.
    let with_slash = format!("{root}/");
    assert!(
        resolve_worktree_path(&with_slash, "src/main.rs").is_ok(),
        "a trailing separator on the root is not an attack"
    );

    // A file as the root must be refused, not treated as a directory.
    let file_root = repo
        .path()
        .join("src/main.rs")
        .to_string_lossy()
        .into_owned();
    assert!(
        resolve_worktree_path(&file_root, "main.rs").is_err(),
        "a file cannot be a repository root"
    );

    for bogus in ["", "   ", "\u{0000}", "/nonexistent/repo/path"] {
        assert!(
            resolve_worktree_path(bogus, "src/main.rs").is_err(),
            "{bogus:?} is not a usable repository root"
        );
    }
}

// ---------------------------------------------------------------------------
// Invariant 2: the char-boundary class, end to end
// ---------------------------------------------------------------------------

/// The reported crash, reproduced as a repository rather than a unit test.
///
/// `parse_co_authors` split every trimmed body line at `"co-authored-by:".len()`
/// (15 bytes). The log shows the panic firing at "byte index 15 ... inside
/// '—'", so this walks a multi-byte character across every offset that split
/// can land on, and reads each commit back through the public API that
/// crashed.
#[test]
fn commit_bodies_with_multibyte_text_at_every_offset_are_read_without_panicking() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path();
    git_in(repo, &["init", "-q", "-b", "main"]);

    // Multi-byte characters of every UTF-8 width, so a split at a fixed byte
    // index can land on any continuation byte.
    let wide = ["\u{2014}", "\u{00e9}", "\u{4e2d}", "\u{1f600}"];
    let mut messages = Vec::new();
    for ch in wide {
        for offset in 0..24usize {
            let pad = "a".repeat(offset);
            // The character straddles the 15-byte split point for some offset
            // in this range, whichever width it has.
            messages.push(format!("subject {ch}\n\n{pad}{ch} trailing body text"));
            messages.push(format!(
                "subject\n\nCo-authored-by: {pad}{ch} Name <n@example.com>"
            ));
            // A line that is a *prefix* of the trailer and shorter than the
            // split point, with the character at the cut.
            messages.push(format!("subject\n\nCo-authored-b{ch}"));
            messages.push(format!("subject\n\n{pad}{ch}"));
        }
    }

    let ids = import_commits(repo, &messages);

    let repo_arg = repo.to_string_lossy().into_owned();
    for (message, id) in messages.iter().zip(&ids) {
        // The assertion is that this returns at all: before the fix the same
        // call aborted the blocking thread with a char-boundary panic.
        let details = GitReader::get_commit_details(&repo_arg, id)
            .unwrap_or_else(|e| panic!("commit {id} must be readable, got {e}"));
        assert_eq!(
            &details.id, id,
            "the reader must return the commit asked for"
        );
        // And that it read *these* bytes. The fixture streams messages through
        // `fast-import`, where a mis-counted `data <n>` would silently truncate
        // a body — leaving the sweep reading offsets that are no longer there
        // and passing because nothing was left to panic on.
        let (subject, body) = message
            .split_once("\n\n")
            .expect("every swept message has a subject and a body");
        assert_eq!(
            details.summary, subject,
            "the commit must carry the subject the sweep built"
        );
        assert_eq!(
            details.body.trim_end(),
            body.trim_end(),
            "the commit must carry the body the sweep built, to its last byte"
        );
    }

    // The same bodies through the other caller in the crash log.
    let report = GitReader::pulse_report(&repo_arg, Some(ids.len()))
        .expect("pulse_report must survive the same bodies");
    assert!(
        report.total_commits_scanned > 0,
        "the report must actually see the commits it parsed"
    );
    eprintln!(
        "read {} commits with multi-byte bodies at every offset",
        ids.len()
    );
}

/// Randomized bodies: arbitrary mixtures of trailer-shaped lines, multi-byte
/// text and whitespace, to catch offsets the systematic sweep does not name.
#[test]
fn randomized_multibyte_commit_bodies_never_panic() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path();
    git_in(repo, &["init", "-q", "-b", "main"]);

    let pieces = [
        "Co-authored-by:",
        "co-authored-by:",
        "CO-AUTHORED-BY:",
        "Co-authored-b",
        "\u{2014}",
        "\u{1f600}",
        "\u{4e2d}\u{6587}",
        "e\u{301}",
        " ",
        "\t",
        "a",
        "<n@example.com>",
        "\u{202e}",
        "\u{feff}",
    ];

    let seed = 0xfeed_beef_u64;
    let mut rng = Lcg(seed);
    let repo_arg = repo.to_string_lossy().into_owned();
    let rounds = 120 * scale();

    let mut bodies = Vec::new();
    for _ in 0..rounds {
        let mut body = String::from("subject\n\n");
        for _ in 0..(1 + rng.below(12)) {
            for _ in 0..(1 + rng.below(6)) {
                body.push_str(rng.pick(&pieces));
            }
            body.push('\n');
        }
        bodies.push(body);
    }

    let ids = import_commits(repo, &bodies);
    for (round, (id, body)) in ids.iter().zip(&bodies).enumerate() {
        let details = GitReader::get_commit_details(&repo_arg, id).unwrap_or_else(|e| {
            panic!("round {round} (seed {seed:#x}) body {body:?} must be readable, got {e}")
        });
        // Every generated body opens with this subject, so a fixture that
        // imported nothing readable cannot look like a passing sweep.
        assert_eq!(
            details.summary, "subject",
            "round {round} (seed {seed:#x}) must carry the message it was given"
        );
    }
    eprintln!("randomized bodies: {rounds} rounds from seed {seed:#x}");
}

mod common;
