//! Adversarial tests for the on-disk worktree family cache (ft-da85cd55 AC2).
//!
//! Every test asserts what a fresh `git worktree list` would decide; a test
//! that fails shows the cache believing something Git would not. Tests marked
//! BREAK in their doc comment fail against the lane's code on purpose.

use super::worktree::{
    admit_worktree_pair, family_disk_dir, forget_in_process, read_worktree_entries,
};
use super::worktree_family_disk as disk;
use crate::test_support::{git_in, repo_with_worktrees, trust_repo};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Fx {
    main: tempfile::TempDir,
    _parent: tempfile::TempDir,
    linked: Vec<PathBuf>,
    memory: tempfile::TempDir,
    _remembering: family_disk_dir::Reset,
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn fx(linked: usize) -> Fx {
    let (main, parent, linked) = repo_with_worktrees(linked);
    trust_repo(main.path());
    let memory = private_dir();
    let remembering = family_disk_dir::set(memory.path());
    Fx {
        main,
        _parent: parent,
        linked,
        memory,
        _remembering: remembering,
    }
}

impl Fx {
    fn repo(&self) -> String {
        self.main
            .path()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }
    fn common(&self) -> PathBuf {
        self.main.path().canonicalize().unwrap().join(".git")
    }
    /// What a brand new hook process answers for the pair, disk memory included.
    fn fresh(&self, repo: &str, worktree: &str) -> Result<super::worktree::WorktreeFamily, String> {
        forget_in_process(&self.common());
        admit_worktree_pair(repo, worktree)?.authenticate()
    }
    /// What the pair gets with nothing remembered anywhere: Git decides.
    #[allow(clippy::wrong_self_convention)]
    fn from_git(
        &self,
        repo: &str,
        worktree: &str,
    ) -> Result<super::worktree::WorktreeFamily, String> {
        forget_in_process(&self.common());
        let empty = private_dir();
        let _scope = family_disk_dir::set(empty.path());
        let answer = admit_worktree_pair(repo, worktree)?.authenticate();
        forget_in_process(&self.common());
        answer
    }
    fn json_files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.memory.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }
    fn live(&self) -> String {
        self.linked[0].to_str().unwrap().to_owned()
    }
    /// The registry entry directory of linked worktree `index`.
    fn entry(&self, index: usize) -> PathBuf {
        self.common()
            .join("worktrees")
            .join(format!("wt-{index:02}"))
    }
}

fn git_lists(repo: &str, path: &str) -> bool {
    read_worktree_entries(Path::new(repo))
        .unwrap()
        .iter()
        .filter_map(|e| std::fs::canonicalize(&e.path).ok())
        .any(|p| p == Path::new(path))
}

/// BREAK (security): the fingerprint always includes the admitted pair itself,
/// so a worktree whose own registration disappears while its directory still
/// resolves to the common directory yields the SAME fingerprint, and the
/// cached family still lists it. Git no longer does.
#[test]
fn a_worktree_whose_registry_pointer_vanished_is_not_believed() {
    let fx = fx(2);
    let (repo, live) = (fx.repo(), fx.live());
    fx.fresh(&repo, &live).expect("registered");
    assert!(!fx.json_files().is_empty(), "the family was stored");
    std::fs::remove_file(fx.entry(0).join("gitdir")).unwrap();
    assert!(!git_lists(&repo, &live), "Git no longer lists it");
    let cold = fx.from_git(&repo, &live);
    assert!(cold.is_err(), "Git denies it: {cold:?}");
    let warm = fx.fresh(&repo, &live);
    assert!(
        warm.is_err(),
        "the cache believed a deregistered worktree: {warm:?}"
    );
}

/// BREAK (security): as above, but the registry pointer is re-pointed at a path
/// that does not exist (Git reports the entry prunable and drops it).
#[test]
fn a_worktree_repointed_at_nothing_is_not_believed() {
    let fx = fx(2);
    let (repo, live) = (fx.repo(), fx.live());
    fx.fresh(&repo, &live).expect("registered");
    std::fs::write(fx.entry(0).join("gitdir"), "/nonexistent/elsewhere/.git\n").unwrap();
    assert!(!git_lists(&repo, &live));
    assert!(fx.from_git(&repo, &live).is_err());
    let warm = fx.fresh(&repo, &live);
    assert!(warm.is_err(), "believed a repointed worktree: {warm:?}");
}

/// BREAK (security, same-user adversary): the file is believed when its
/// registry matches, and its `members` are never checked against that
/// registry. `authenticate_with_git` refuses a primary that belongs to another
/// repository; the disk path does not run that check. The agent a hook guards
/// runs as this user, so the owner/mode checks do not keep it from writing the
/// file. The family's anchor is where the ledger and task store are read.
#[test]
fn a_file_with_widened_members_cannot_move_the_anchor() {
    let fx = fx(1);
    let (repo, live) = (fx.repo(), fx.live());
    let honest = fx.fresh(&repo, &live).expect("registered");
    let stored = disk::load(&fx.common()).expect("stored");
    let foreign = PathBuf::from("/tmp/some-other-repository-with-a-permissive-task");
    let mut members = vec![foreign.clone()];
    members.extend(stored.members.clone());
    disk::store(&fx.common(), &stored.registry, &members);
    let forged = fx.fresh(&repo, &live).expect("still answers");
    assert_eq!(
        forged.anchor, honest.anchor,
        "anchor moved to {:?}",
        forged.anchor
    );
    assert!(
        !forged.members.contains(&foreign),
        "family widened: {:?}",
        forged.members
    );
}

/// BREAK (correctness): a record whose registry equals the current one but
/// whose members are a subset or reordering (primary no longer first) is
/// believed; the anchor is just `members[0]`.
#[test]
fn a_reordered_member_list_cannot_change_the_anchor() {
    let fx = fx(2);
    let (repo, live) = (fx.repo(), fx.live());
    let honest = fx.fresh(&repo, &live).expect("registered");
    let stored = disk::load(&fx.common()).expect("stored");
    let mut members = stored.members.clone();
    members.reverse();
    disk::store(&fx.common(), &stored.registry, &members);
    let forged = fx.fresh(&repo, &live).expect("answers");
    assert_eq!(forged.anchor, honest.anchor);
}

/// HELD: add, remove, move, prune, and a repository recreated at the same path
/// are all seen, whatever the cache holds.
#[test]
fn registry_changes_are_seen_by_the_next_process() {
    let fx = fx(2);
    let repo = fx.repo();
    let (a, b) = (fx.linked[0].clone(), fx.linked[1].clone());
    let (a_s, b_s) = (
        a.to_str().unwrap().to_owned(),
        b.to_str().unwrap().to_owned(),
    );
    fx.fresh(&repo, &a_s).unwrap();
    fx.fresh(&repo, &b_s).unwrap();

    // move
    let moved = a.parent().unwrap().join("moved-a");
    git_in(
        fx.main.path(),
        &["worktree", "move", &a_s, moved.to_str().unwrap()],
    );
    let moved_c = std::fs::canonicalize(&moved).unwrap();
    trust_repo(&moved_c);
    assert!(fx.fresh(&repo, moved_c.to_str().unwrap()).is_ok());
    assert!(
        fx.fresh(&repo, &a_s).is_err(),
        "old path of a moved worktree"
    );

    // remove
    git_in(fx.main.path(), &["worktree", "remove", "--force", &b_s]);
    assert!(fx.fresh(&repo, &b_s).is_err(), "removed");

    // a repository created at the removed path
    std::fs::create_dir_all(&b).unwrap();
    git_in(&b, &["init", "-b", "main"]);
    trust_repo(&b);
    assert!(fx.fresh(&repo, &b_s).is_err(), "new repo at the same path");

    // prune: delete the directory behind Git's back
    std::fs::remove_dir_all(&moved_c).unwrap();
    assert!(
        fx.fresh(&repo, moved_c.to_str().unwrap()).is_err(),
        "pruned"
    );
}

/// HELD: nothing in the fingerprint is a timestamp or inode, so a registry
/// rewritten inside one mtime tick, or an entry directory replaced by one with
/// a recycled inode, cannot be mistaken for the old one: the content differs
/// or the content is the same, and then the family is the same.
#[test]
fn a_registry_rewritten_in_place_is_compared_by_content() {
    let fx = fx(2);
    let repo = fx.repo();
    let (a, b) = (
        fx.linked[0].to_str().unwrap().to_owned(),
        fx.linked[1].to_str().unwrap().to_owned(),
    );
    fx.fresh(&repo, &a).unwrap();
    // Swap the two pointers in one instant, then put worktree b elsewhere.
    let (ga, gb) = (fx.entry(0).join("gitdir"), fx.entry(1).join("gitdir"));
    let (ta, tb) = (std::fs::read(&ga).unwrap(), std::fs::read(&gb).unwrap());
    std::fs::write(&ga, &tb).unwrap();
    std::fs::write(&gb, &ta).unwrap();
    for path in [&a, &b] {
        assert_eq!(fx.fresh(&repo, path), fx.from_git(&repo, path), "{path}");
    }
}

/// Spellings of one worktree path give what Git gives, warm or cold, and a
/// subdirectory of a member is never a member.
#[test]
fn path_spellings_agree_with_git_warm_and_cold() {
    let fx = fx(1);
    let repo = fx.repo();
    let live = fx.live();
    fx.fresh(&repo, &live).unwrap();
    let link = fx.memory.path().join("link-to-live");
    std::os::unix::fs::symlink(&live, &link).unwrap();
    let sub = fx.linked[0].join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let spellings = [
        format!("{live}/"),
        format!("{live}//"),
        format!("{live}/sub/.."),
        link.to_str().unwrap().to_owned(),
        live.to_uppercase(),
        live.to_lowercase(),
        sub.to_str().unwrap().to_owned(),
        format!("{repo}/sub-does-not-exist/.."),
    ];
    for spelling in &spellings {
        let cold = fx.from_git(&repo, spelling);
        let warm = fx.fresh(&repo, spelling);
        assert_eq!(
            cold.is_ok(),
            warm.is_ok(),
            "{spelling}: git said {cold:?}, cache said {warm:?}"
        );
        if let (Ok(c), Ok(w)) = (&cold, &warm) {
            assert_eq!(c, w, "{spelling}");
        }
        // The repository side, too.
        let cold = fx.from_git(spelling, &live);
        let warm = fx.fresh(spelling, &live);
        assert_eq!(cold.is_ok(), warm.is_ok(), "repo spelled {spelling}");
    }
}

/// BREAK (availability, same-user): `load` opens the file BEFORE it checks it is
/// a regular file. A FIFO planted at the cache's name makes `open(O_RDONLY)`
/// block until someone writes, hanging every hook that authenticates that
/// repository. (Needs `O_NONBLOCK`, or an lstat before the open.)
#[test]
fn a_fifo_in_place_of_the_file_is_a_miss_not_a_hang() {
    let fx = fx(1);
    let (repo, live) = (fx.repo(), fx.live());
    fx.fresh(&repo, &live).unwrap();
    let file = fx.json_files().pop().expect("stored file");
    std::fs::remove_file(&file).unwrap();
    let c = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let common = fx.common();
    let memory = fx.memory.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _scope = family_disk_dir::set(&memory);
        let _ = tx.send(disk::load(&common).is_none());
    });
    let verdict = rx.recv_timeout(std::time::Duration::from_secs(3));
    // Release the blocked reader whatever happened so the test can end.
    // O_NONBLOCK: with no reader a plain write-open of a FIFO blocks forever,
    // which is exactly what happens once `load` correctly returned a miss.
    let _ = std::os::unix::fs::OpenOptionsExt::custom_flags(
        std::fs::OpenOptions::new().write(true),
        libc::O_NONBLOCK,
    )
    .open(&file);
    reader.join().unwrap();
    assert_eq!(verdict, Ok(true), "load did not return a miss within 3s");
}

/// HELD: a symlinked directory, group-readable file or foreign mode is a miss.
#[test]
fn a_symlinked_cache_directory_and_loose_modes_are_misses() {
    let fx = fx(1);
    let (repo, live) = (fx.repo(), fx.live());
    fx.fresh(&repo, &live).unwrap();
    assert!(disk::load(&fx.common()).is_some());
    let file = fx.json_files().pop().unwrap();
    for mode in [0o640, 0o604, 0o660, 0o606] {
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(disk::load(&fx.common()).is_none(), "mode {mode:o}");
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    // Replace the directory by a symlink to a private copy.
    let real = private_dir();
    std::fs::copy(&file, real.path().join(file.file_name().unwrap())).unwrap();
    std::fs::set_permissions(
        real.path().join(file.file_name().unwrap()),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let holder = private_dir();
    let link = holder.path().join("via-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    let _scope = family_disk_dir::set(&link);
    assert!(
        disk::load(&fx.common()).is_none(),
        "followed a symlinked directory"
    );
}

/// HELD: truncated, empty, nested-deep and type-confused JSON are misses and a
/// partial record never produces a family.
#[test]
fn partial_and_malformed_records_are_misses() {
    let fx = fx(1);
    let (repo, live) = (fx.repo(), fx.live());
    fx.fresh(&repo, &live).unwrap();
    let file = fx.json_files().pop().unwrap();
    let good = std::fs::read(&file).unwrap();
    let mut bodies: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"{}".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        "[".repeat(100_000).into_bytes(),
        br#"{"version":1}"#.to_vec(),
    ];
    for cut in [1, good.len() / 3, good.len() / 2, good.len() - 1] {
        bodies.push(good[..cut].to_vec());
    }
    let text = String::from_utf8(good.clone()).unwrap();
    bodies.push(
        text.replace("\"members\":[", "\"members\":{\"x\":[")
            .into_bytes(),
    );
    bodies.push(
        text.replace("\"version\":1", "\"version\":\"1\"")
            .into_bytes(),
    );
    for body in bodies {
        std::fs::write(&file, &body).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            disk::load(&fx.common()).is_none(),
            "{:?}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// HELD: concurrent writers (distinct records, threads share one pid) and
/// concurrent readers never see a torn or mixed record.
#[test]
fn concurrent_writers_never_tear_a_record() {
    let memory = private_dir();
    let common = PathBuf::from("/r/.git");
    let variant = |n: usize| -> Vec<PathBuf> {
        (0..=n * 7)
            .map(|i| PathBuf::from(format!("/r/wt-{n}-{i}")))
            .collect()
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut writers = Vec::new();
    for n in 1..=6usize {
        let (dir, common) = (memory.path().to_path_buf(), common.clone());
        writers.push(std::thread::spawn(move || {
            let _scope = family_disk_dir::set(&dir);
            for _ in 0..150 {
                let family = variant(n);
                disk::store(&common, &family, &family);
            }
        }));
    }
    let reader = {
        let (dir, common, stop) = (memory.path().to_path_buf(), common.clone(), stop.clone());
        std::thread::spawn(move || {
            let _scope = family_disk_dir::set(&dir);
            let mut seen = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(stored) = disk::load(&common) {
                    assert_eq!(
                        stored.registry, stored.members,
                        "registry and members from different writers"
                    );
                    let n = (stored.members.len() - 1) / 7;
                    assert_eq!(stored.members, variant(n), "mixed record");
                    seen += 1;
                }
            }
            seen
        })
    };
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0, "reader never saw a record");
    let leaked: Vec<_> = std::fs::read_dir(memory.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leaked.is_empty(), "staging files left behind: {leaked:?}");
}

/// BREAK (minor, availability of the cache only): a staging file left by a
/// crashed process whose pid is recycled makes one store silently delete it and
/// write nothing, so that call and the next process pay Git again; it then
/// heals. Pins the lost write.
#[test]
fn a_stale_staging_file_with_this_pid_does_not_lose_the_write() {
    let memory = private_dir();
    let _scope = family_disk_dir::set(memory.path());
    let common = PathBuf::from("/r/.git");
    let family = vec![PathBuf::from("/r")];
    disk::store(&common, &family, &family);
    let json = std::fs::read_dir(memory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    std::fs::remove_file(memory.path().join(&json)).unwrap();
    let stale = memory.path().join(format!(
        ".{}.{}.tmp",
        json.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&stale, b"left by a crashed process").unwrap();
    disk::store(&common, &family, &family);
    assert!(
        disk::load(&common).is_some(),
        "the write was lost to a stale staging file"
    );
}

/// Quantifies growth: one file per repository ever authenticated, never
/// pruned. Reports bytes per file; asserts only that a bound exists, which it
/// does not (BREAK, minor).
#[test]
fn the_cache_directory_is_bounded() {
    let memory = private_dir();
    let _scope = family_disk_dir::set(memory.path());
    let family = vec![PathBuf::from("/Users/someone/Code/devtools/GitPulse")];
    for n in 0..1500 {
        disk::store(
            &PathBuf::from(format!("/Users/someone/repo-{n}/.git")),
            &family,
            &family,
        );
    }
    let files: Vec<_> = std::fs::read_dir(memory.path()).unwrap().collect();
    let bytes: u64 = files
        .iter()
        .map(|e| e.as_ref().unwrap().metadata().unwrap().len())
        .sum();
    eprintln!(
        "{} files, {} bytes of content ({} per file)",
        files.len(),
        bytes,
        bytes / files.len() as u64
    );
    assert!(files.len() <= 1024, "{} files and no pruning", files.len());
}
