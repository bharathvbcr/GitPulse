use super::*;
use std::fs;

fn temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
    (temp, root)
}

fn file(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
}

fn spend() -> Spend {
    Spend {
        entries: 0,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

/// Restores a directory's mode when dropped, so a failing assertion cannot
/// leave a fixture the temp directory cannot remove.
#[cfg(unix)]
struct Denied(PathBuf);
#[cfg(unix)]
impl Denied {
    fn new(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        Self(path.to_path_buf())
    }
}
#[cfg(unix)]
impl Drop for Denied {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn a_size_counts_files_once_per_hard_link_and_never_follows_a_symlink() {
    let (_temp, root) = temp();
    let data = root.join("data");
    file(&data.join("a.bin"), 1000);
    file(&data.join("sub/b.bin"), 500);
    fs::hard_link(data.join("a.bin"), data.join("sub/a-again.bin")).unwrap();
    // A symlink to something big elsewhere is neither followed nor counted.
    let elsewhere = root.join("elsewhere");
    file(&elsewhere.join("huge.bin"), 100_000);
    std::os::unix::fs::symlink(&elsewhere, data.join("link")).unwrap();
    let reading = size_of(&data, &mut spend(), &AtomicBool::new(false)).unwrap();
    assert_eq!(reading.bytes, 1500, "the hard link counts once");
    assert!(!reading.floor);
    // The root itself being a symlink is refused, not followed.
    let via = root.join("via");
    std::os::unix::fs::symlink(&data, &via).unwrap();
    let refused = size_of(&via, &mut spend(), &AtomicBool::new(false)).unwrap_err();
    assert!(refused.contains("symlink"), "{refused}");
}

#[test]
fn a_walk_cut_short_reports_a_floor_not_a_total() {
    let (_temp, root) = temp();
    let data = root.join("data");
    for n in 0..(CHECK_STRIDE * 2) {
        file(&data.join(format!("f{n}")), 10);
    }
    let whole = size_of(&data, &mut spend(), &AtomicBool::new(false)).unwrap();
    assert_eq!(whole.bytes, (CHECK_STRIDE * 2 * 10) as u64);
    assert!(!whole.floor);
    // The total entry budget spent: a floor.
    let mut nearly = spend();
    nearly.entries = TOTAL_ENTRIES - 5;
    let cut = size_of(&data, &mut nearly, &AtomicBool::new(false)).unwrap();
    assert!(cut.floor && cut.bytes < whole.bytes, "{}", cut.bytes);
    // The clock spent: a floor.
    let mut late = spend();
    late.deadline = Instant::now();
    let cut = size_of(&data, &mut late, &AtomicBool::new(false)).unwrap();
    assert!(cut.floor && cut.bytes < whole.bytes);
    // A cancellation: a floor.
    let cut = size_of(&data, &mut spend(), &AtomicBool::new(true)).unwrap();
    assert!(cut.floor && cut.bytes < whole.bytes);
}

#[cfg(unix)]
#[test]
fn an_unreadable_part_is_a_floor_and_an_unreadable_root_is_not_zero_bytes() {
    let (_temp, root) = temp();
    let data = root.join("data");
    file(&data.join("a.bin"), 100);
    file(&data.join("sealed/b.bin"), 900);
    {
        let _sealed = Denied::new(&data.join("sealed"));
        let reading = size_of(&data, &mut spend(), &AtomicBool::new(false)).unwrap();
        assert_eq!(reading.bytes, 100);
        assert!(reading.floor, "part of it could not be read");
    }
    let closed = root.join("closed");
    file(&closed.join("c.bin"), 5);
    let _closed = Denied::new(&closed);
    let rows = report(
        &[closed.to_string_lossy().into_owned()],
        &[],
        &root.join("home"),
        &[],
        &AtomicBool::new(false),
    );
    let row = rows
        .iter()
        .find(|r| r.path == closed.to_string_lossy())
        .unwrap();
    assert_eq!(row.bytes, None, "unreadable is not 0 bytes: {row:?}");
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("could not be read")),
        "{row:?}"
    );
}

#[test]
fn the_defaults_list_what_exists_and_leave_out_what_does_not() {
    let (_temp, root) = temp();
    let home = root.join("home");
    file(&home.join(".cache/huggingface/hub/model.bin"), 4000);
    file(&home.join(".rustup/toolchains/stable/bin/rustc"), 700);
    let claude = home.join(".claude");
    file(&claude.join("projects/p/t.jsonl"), 300);
    let second = home.join(".claude-work");
    file(&second.join("projects/p/t.jsonl"), 200);
    fs::create_dir_all(&home).unwrap();
    let rows = report(
        &[],
        &[],
        &home,
        &[claude.clone(), second.clone(), home.join(".claude-gone")],
        &AtomicBool::new(false),
    );
    let by = |label: &str| rows.iter().filter(|r| r.label == label).collect::<Vec<_>>();
    assert_eq!(by("Hugging Face cache")[0].bytes, Some(4000));
    assert_eq!(by("Rust toolchains (rustup)")[0].bytes, Some(700));
    let configs = by("Claude configuration directory");
    assert_eq!(
        configs.len(),
        2,
        "every directory given, the missing one absent: {rows:?}"
    );
    assert!(configs.iter().any(|r| r.bytes == Some(200)));
    for absent in [
        "Ollama models",
        "LM Studio",
        "PyTorch hub cache",
        "Claude desktop app data",
    ] {
        assert!(
            by(absent).is_empty(),
            "{absent} does not exist, so it is not a zero row"
        );
    }
    // Paths the cleaner already manages are never in the defaults.
    for managed in [".cargo/registry", ".npm", ".cache/uv", ".bun", "pnpm"] {
        assert!(
            defaults(&home, &[])
                .iter()
                .all(|(_, p)| !p.to_string_lossy().contains(managed)),
            "{managed} is offered through the shared-cache inventory"
        );
    }
}

#[test]
fn a_user_path_is_validated_when_saved_and_again_when_reported() {
    let (_temp, root) = temp();
    let home = root.join("home");
    let repo_root = root.join("home/code");
    fs::create_dir_all(&repo_root).unwrap();
    let models = home.join("models");
    file(&models.join("m.bin"), 10);
    let plain_file = home.join("a.txt");
    file(&plain_file, 1);
    let roots = vec![repo_root.to_string_lossy().into_owned()];
    let check = |p: &str| validate_user_path(p, &roots, &home);
    assert!(check(models.to_str().unwrap()).is_ok());
    // Inside a reclaim root is allowed: it is only reported, never listed as a candidate.
    let inside = repo_root.join("data");
    fs::create_dir_all(&inside).unwrap();
    assert!(check(inside.to_str().unwrap()).is_ok());
    let refused = |p: &str, why: &str| {
        let message = check(p).expect_err(p);
        assert!(message.contains(why), "{p}: {message}");
    };
    refused("relative/dir", "not an absolute path");
    refused(&format!("{}/../models", home.display()), "components");
    refused(&format!("{}/./models", home.display()), "components");
    // This platform's own root: `/`, or the drive's root (`C:\`) on Windows.
    refused(
        home.ancestors().last().unwrap().to_str().unwrap(),
        "filesystem root",
    );
    refused(home.to_str().unwrap(), "home directory");
    refused(root.to_str().unwrap(), "ancestors");
    refused(&format!("{}/missing", home.display()), "cannot be read");
    refused(plain_file.to_str().unwrap(), "not a directory");
    refused(&"a".repeat(5000), "too long");
    refused("/tmp/with\ncontrol", "control character");
    // An ancestor of a reclaim root would list what the cleaner manages.
    let containing = home.join("code");
    refused(containing.to_str().unwrap(), "contains the repository root");
    #[cfg(unix)]
    {
        let link = home.join("link");
        std::os::unix::fs::symlink(&models, &link).unwrap();
        refused(link.to_str().unwrap(), "symlink");
        let through = link.join("sub");
        fs::create_dir_all(models.join("sub")).unwrap();
        refused(through.to_str().unwrap(), "symlink");
    }
}

#[test]
fn a_user_path_that_no_longer_validates_is_a_row_with_the_reason_not_a_walk() {
    let (_temp, root) = temp();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let gone = home.join("gone");
    let rows = report(
        &[gone.to_string_lossy().into_owned()],
        &[],
        &home,
        &[],
        &AtomicBool::new(false),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].bytes, None);
    assert!(rows[0]
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("not measured:")));
}
