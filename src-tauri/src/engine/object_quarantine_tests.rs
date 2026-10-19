//! The quarantine against real git: a merge that must write objects leaves the
//! repository's object database byte for byte as it was, the scratch directory
//! is gone on every way out, and the spawn gate and the deadline are honoured.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

struct Repo(tempfile::TempDir);

impl Repo {
    fn new() -> Self {
        let repo = Repo(tempfile::tempdir().expect("tempdir"));
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "T"]);
        repo.git(&["config", "user.email", "t@e.com"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(repo.path());
        repo
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn put(&self, file: &str, content: &str) -> String {
        std::fs::write(self.path().join(file), content).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", file]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn has_object(&self, oid: &str) -> bool {
        Command::new("git")
            .args(["cat-file", "-e", oid])
            .current_dir(self.path())
            .output_locked()
            .expect("git")
            .status
            .success()
    }
}

/// `main` has m.txt, `f` has f.txt, both off the same root: merging them
/// cleanly makes a tree that exists nowhere yet. Returns `(main, f)`.
fn diverged(repo: &Repo) -> (String, String) {
    repo.put("root.txt", "root\n");
    repo.git(&["checkout", "-q", "-b", "f"]);
    let f = repo.put("f.txt", "f\n");
    repo.git(&["checkout", "-q", "main"]);
    let main = repo.put("m.txt", "m\n");
    (main, f)
}

/// The same file edited differently on both sides.
fn conflicting(repo: &Repo) -> (String, String) {
    repo.put("shared.txt", "root\n");
    repo.git(&["checkout", "-q", "-b", "f"]);
    let f = repo.put("shared.txt", "from f\n");
    repo.git(&["checkout", "-q", "main"]);
    let main = repo.put("shared.txt", "from main\n");
    (main, f)
}

/// Every file under `<git dir>/objects` with its bytes, in path order: the
/// listing, the count and the content in one comparable value.
fn object_store(repo: &Repo) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, std::fs::read(&path).expect("read")));
            }
        }
    }
    let root = repo.path().join(".git").join("objects");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

fn soon() -> Instant {
    Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(60))
}

#[test]
fn a_clean_merge_leaves_the_object_store_byte_identical_and_the_tree_in_the_quarantine() {
    let repo = Repo::new();
    let (main, f) = diverged(&repo);
    repo.git(&["gc", "-q"]);
    let before = object_store(&repo);
    assert!(!before.is_empty(), "an empty store proves nothing");

    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    let outcome = quarantine
        .merge_tree(repo.path(), &main, &f, soon())
        .expect("merge-tree");

    let MergeOutcome::Clean { tree } = outcome else {
        panic!("expected a clean merge, got {outcome:?}");
    };
    // The merged tree was written, and written to the quarantine only.
    assert!(
        !repo.has_object(&tree),
        "the merged tree reached the repository"
    );
    assert!(
        std::fs::read_dir(quarantine.path())
            .unwrap()
            .next()
            .is_some(),
        "git wrote nothing into the quarantine, so the redirect did not take effect"
    );
    assert!(object_store(&repo) == before, "the object store changed");
    assert_eq!(object_store(&repo).len(), before.len());
}

#[test]
fn a_conflicted_merge_is_reported_and_leaves_the_object_store_byte_identical() {
    let repo = Repo::new();
    let (main, f) = conflicting(&repo);
    repo.git(&["gc", "-q"]);
    let before = object_store(&repo);

    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    let outcome = quarantine
        .merge_tree(repo.path(), &main, &f, soon())
        .expect("merge-tree");

    let MergeOutcome::Conflicted { tree } = outcome else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert!(!repo.has_object(&tree));
    assert!(object_store(&repo) == before, "the object store changed");
}

#[test]
fn loose_objects_are_untouched_too() {
    // No `gc`: the repository holds loose objects, the other place a write
    // would show.
    let repo = Repo::new();
    let (main, f) = diverged(&repo);
    let before = object_store(&repo);
    assert!(before.iter().any(|(name, _)| !name.starts_with("pack/")));

    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    quarantine
        .merge_tree(repo.path(), &main, &f, soon())
        .expect("merge-tree");
    drop(quarantine);

    assert!(object_store(&repo) == before, "the object store changed");
}

#[test]
fn the_directory_is_private_and_removed_on_drop() {
    let repo = Repo::new();
    diverged(&repo);
    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    let dir = quarantine.path().to_path_buf();
    assert!(dir.is_dir());
    assert!(dir.starts_with(std::env::temp_dir()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }
    drop(quarantine);
    assert!(!dir.exists(), "{} survived the drop", dir.display());
}

#[test]
fn the_directory_is_removed_when_a_panic_unwinds_through_it() {
    let repo = Repo::new();
    let (main, f) = diverged(&repo);
    let seen = std::sync::Mutex::new(None::<PathBuf>);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
        quarantine
            .merge_tree(repo.path(), &main, &f, soon())
            .expect("merge-tree");
        *seen.lock().unwrap() = Some(quarantine.path().to_path_buf());
        assert!(quarantine.path().is_dir());
        panic!("the caller failed while objects were in the quarantine");
    }));
    assert!(unwound.is_err());
    let dir = seen.lock().unwrap().clone().expect("the closure ran");
    assert!(!dir.exists(), "{} survived the panic", dir.display());
}

#[test]
fn an_expired_deadline_runs_nothing_and_leaves_nothing() {
    let repo = Repo::new();
    let (main, f) = diverged(&repo);
    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    let dir = quarantine.path().to_path_buf();

    let expired = quarantine.merge_tree(repo.path(), &main, &f, Instant::now());

    assert_eq!(expired, Err(QuarantineError::DeadlineExceeded));
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "git ran past a deadline that had already passed"
    );
    drop(quarantine);
    assert!(!dir.exists());
}

#[test]
fn a_refused_spawn_is_an_error_and_leaves_nothing() {
    let repo = Repo::new();
    let (main, f) = diverged(&repo);
    let before = object_store(&repo);
    for gate_refusal in ["spawn failure", "deferral under load"] {
        let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
        let dir = quarantine.path().to_path_buf();
        let run = || quarantine.merge_tree(repo.path(), &main, &f, soon());
        let answer = if gate_refusal == "spawn failure" {
            crate::engine::git_cli::with_forced_spawn_failure(run)
        } else {
            crate::engine::git_cli::with_forced_deferral(run)
        };
        assert!(
            matches!(answer, Err(QuarantineError::Git(_))),
            "a refused spawn must not read as an answer: {answer:?}"
        );
        drop(quarantine);
        assert!(!dir.exists());
    }
    assert!(object_store(&repo) == before);
}

#[test]
fn an_operand_that_is_not_an_object_name_is_refused_before_git_runs() {
    let repo = Repo::new();
    let (main, _) = diverged(&repo);
    let quarantine = ObjectQuarantine::new(repo.path()).expect("quarantine");
    for bad in ["--output=/tmp/x", "main", "", "abc", &"g".repeat(40)] {
        let answer = quarantine.merge_tree(repo.path(), &main, bad, soon());
        assert!(
            matches!(&answer, Err(QuarantineError::Git(reason)) if reason.contains("object name")),
            "{bad:?}: {answer:?}"
        );
    }
    assert!(std::fs::read_dir(quarantine.path())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn objects_reached_only_through_the_repositorys_own_alternates_are_readable() {
    // `clone --shared` leaves the origin's objects out of the clone: the clone
    // reads them through `objects/info/alternates`, which the quarantine must
    // reach by following the clone's object directory.
    let origin = Repo::new();
    origin.put("root.txt", "root\n");
    let root = origin.git(&["rev-parse", "HEAD"]);
    let origin_tip = origin.put("m.txt", "m\n");
    let clone = tempfile::tempdir().expect("tempdir");
    let url = origin.path().to_str().unwrap();
    let out = Command::new("git")
        .args(["clone", "-q", "--shared", url])
        .arg(clone.path())
        .output_locked()
        .expect("clone");
    assert!(out.status.success(), "{out:?}");
    crate::test_support::trust_repo(clone.path());
    let clone = Repo(clone);
    clone.git(&["config", "user.name", "T"]);
    clone.git(&["config", "user.email", "t@e.com"]);
    clone.git(&["config", "commit.gpgsign", "false"]);
    clone.git(&["checkout", "-q", "-b", "g", &root]);
    let g = clone.put("g.txt", "g\n");
    assert!(
        !std::fs::read_to_string(clone.path().join(".git/objects/info/alternates"))
            .unwrap_or_default()
            .is_empty(),
        "the fixture is not an alternates repository"
    );
    let before = object_store(&clone);

    let quarantine = ObjectQuarantine::new(clone.path()).expect("quarantine");
    let outcome = quarantine
        .merge_tree(clone.path(), &origin_tip, &g, soon())
        .expect("merge-tree through alternates");

    assert!(matches!(outcome, MergeOutcome::Clean { .. }), "{outcome:?}");
    assert!(object_store(&clone) == before);
}

#[test]
fn a_directory_that_is_not_a_repository_is_a_setup_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let answer = ObjectQuarantine::new(dir.path());
    assert!(
        matches!(answer, Err(QuarantineError::Setup(_))),
        "{answer:?}"
    );
}

#[test]
fn names_are_unique_across_quarantines() {
    let repo = Repo::new();
    diverged(&repo);
    let first = ObjectQuarantine::new(repo.path()).expect("quarantine");
    let second = ObjectQuarantine::new(repo.path()).expect("quarantine");
    assert_ne!(first.path(), second.path());
}

/// A directory under `base` with a file in it, last modified `age` ago.
#[cfg(unix)]
fn aged_dir(base: &Path, name: &str, age: Duration) -> PathBuf {
    let path = base.join(name);
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("obj"), b"x").unwrap();
    let handle = std::fs::File::open(&path).unwrap();
    handle
        .set_modified(std::time::SystemTime::now() - age)
        .unwrap();
    path
}

#[cfg(unix)]
#[test]
fn a_sweep_removes_only_old_directories_named_as_a_quarantine_names_them() {
    let base = tempfile::tempdir().expect("tempdir");
    let two_hours = Duration::from_secs(2 * 60 * 60);
    let stale = aged_dir(base.path(), "gitpulse-quarantine-1-1-1", two_hours);
    let fresh = aged_dir(base.path(), "gitpulse-quarantine-2-2-2", Duration::ZERO);
    let foreign = aged_dir(base.path(), "not-gitpulse-quarantine-1-1-1", two_hours);
    let odd = aged_dir(base.path(), "gitpulse-quarantine-abc", two_hours);
    let bare = aged_dir(base.path(), "gitpulse-quarantine-", two_hours);
    let file = base.path().join("gitpulse-quarantine-3-3-3");
    std::fs::write(&file, b"not a directory").unwrap();
    // A link under a matching name is not followed to what it points at.
    let target = aged_dir(base.path(), "elsewhere", two_hours);
    let link = base.path().join("gitpulse-quarantine-4-4-4");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let removed = sweep_stale(base.path(), STALE_AFTER);

    assert_eq!(removed, 1);
    assert!(!stale.exists(), "an old quarantine was left behind");
    for kept in [&fresh, &foreign, &odd, &bare, &file, &target] {
        assert!(kept.exists(), "{} was removed", kept.display());
    }
    assert!(target.join("obj").exists(), "the sweep followed a symlink");
    assert!(link.symlink_metadata().is_ok());
}

#[cfg(unix)]
#[test]
fn one_sweep_removes_a_bounded_number_and_the_next_continues() {
    let base = tempfile::tempdir().expect("tempdir");
    let two_hours = Duration::from_secs(2 * 60 * 60);
    let total = SWEEP_REMOVALS + 5;
    for n in 0..total {
        aged_dir(
            base.path(),
            &format!("gitpulse-quarantine-9-{n}-1"),
            two_hours,
        );
    }
    assert_eq!(sweep_stale(base.path(), STALE_AFTER), SWEEP_REMOVALS);
    assert_eq!(sweep_stale(base.path(), STALE_AFTER), 5);
    assert_eq!(sweep_stale(base.path(), STALE_AFTER), 0);
}

#[test]
fn a_missing_base_sweeps_nothing() {
    let base = tempfile::tempdir().expect("tempdir");
    assert_eq!(sweep_stale(&base.path().join("absent"), STALE_AFTER), 0);
}
