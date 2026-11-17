//! Adversary round 2 for ft-38ec498d: how a pass attributes a path to a root
//! (`transcript::RootSet`, reached through `deepest_root`) and
//! `repository_trust::directory_key`. Each test that stands for a pass builds
//! the root set it uses, as a pass does once, so nothing here outlives a pass.

use super::transcript::{deepest_root, RootSet};
use crate::procguard::LockedSpawn;
use crate::test_support::coverage_relaxed;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One pass over one path: the root prepared, then asked.
fn is_under(path: &str, root: &str) -> bool {
    deepest_root(path, &RootSet::new(&[root])).is_some()
}

fn canon_tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn real(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap()
}

// ---- the pre-change `under` from main 7b48ffdb, copied as the oracle --------

fn old_has_dot_components(path: &str) -> bool {
    let body = path.trim_prefix('/');
    let body = body.trim_suffix('/');
    !body.is_empty() && body.split('/').any(|c| matches!(c, "" | "." | ".."))
}

fn old_lexical(path: &str) -> std::borrow::Cow<'_, str> {
    if !old_has_dot_components(path) {
        return std::borrow::Cow::Borrowed(path);
    }
    let absolute = path.starts_with('/');
    let mut kept: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => match kept.last() {
                Some(last) if *last != ".." => {
                    kept.pop();
                }
                _ if absolute => {}
                _ => kept.push(".."),
            },
            other => kept.push(other),
        }
    }
    let joined = kept.join("/");
    std::borrow::Cow::Owned(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

fn old_under(path: &str, root: &str) -> bool {
    if root.is_empty() || path.is_empty() {
        return false;
    }
    let (path, root) = (old_lexical(path), old_lexical(root));
    let (path, root) = (path.as_ref(), root.as_ref());
    if !root.starts_with('/') {
        return false;
    }
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return path.starts_with('/');
    }
    path == root || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

// ---- benchmark ---------------------------------------------------------------

/// Old `under` against the prepared set, per path over every root, as the pass
/// uses them. `prepare` is the one-time cost of building the set for the pass.
fn time_rounds(label: &str, paths: &[String], roots: &[String]) {
    let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    let mut old_ms = Vec::new();
    let mut new_ms = Vec::new();
    let mut prep_ms = Vec::new();
    let mut hits = (0usize, 0usize);
    for _ in 0..5 {
        let t = Instant::now();
        let mut n = 0;
        for p in paths {
            for r in &root_refs {
                n += usize::from(old_under(std::hint::black_box(p), std::hint::black_box(r)));
            }
        }
        old_ms.push(t.elapsed().as_secs_f64() * 1e3);
        hits.0 = n;
        let t = Instant::now();
        let set = RootSet::new(&root_refs);
        prep_ms.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let mut n = 0;
        for p in paths {
            n += usize::from(deepest_root(std::hint::black_box(p), &set).is_some());
        }
        new_ms.push(t.elapsed().as_secs_f64() * 1e3);
        hits.1 = n;
    }
    let min = |v: &[f64]| v.iter().cloned().fold(f64::MAX, f64::min);
    let max = |v: &[f64]| v.iter().cloned().fold(0.0, f64::max);
    println!(
        "BENCH {label}: paths={} roots={} old min {:.3} ms max {:.3} | new match min {:.3} ms max {:.3} | new/old {:.2}x | prepare once min {:.3} ms | paths matched old {} new {}",
        paths.len(),
        roots.len(),
        min(&old_ms),
        max(&old_ms),
        min(&new_ms),
        max(&new_ms),
        min(&new_ms) / min(&old_ms),
        min(&prep_ms),
        hits.0,
        hits.1
    );
}

/// 200 paths against 50 existing roots, ~2% matching.
#[test]
#[ignore = "benchmark: run explicitly"]
fn adv2b_bench_under_existing_roots() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let roots: Vec<String> = (0..50)
        .map(|i| {
            let d = base.join(format!("w{i}"));
            std::fs::create_dir_all(d.join("src")).unwrap();
            d.to_string_lossy().into_owned()
        })
        .collect();
    let paths: Vec<String> = (0..200)
        .map(|p| format!("{}/src/f{p}.rs", roots[p % 50]))
        .collect();
    time_rounds("existing roots", &paths, &roots);
}

/// Same, but the roots are gone (a removed worktree is still in a transcript).
#[test]
#[ignore = "benchmark: run explicitly"]
fn adv2b_bench_under_nonexistent_roots() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let roots: Vec<String> = (0..50)
        .map(|i| base.join(format!("gone{i}")).to_string_lossy().into_owned())
        .collect();
    let paths: Vec<String> = (0..200)
        .map(|p| format!("{}/src/f{p}.rs", roots[p % 50]))
        .collect();
    time_rounds("nonexistent roots", &paths, &roots);
}

/// 5000 distinct directories against 2 roots.
#[test]
#[ignore = "benchmark: run explicitly"]
fn adv2b_bench_under_many_distinct_dirs() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let roots: Vec<String> = (0..2)
        .map(|i| {
            let d = base.join(format!("w{i}"));
            std::fs::create_dir_all(&d).unwrap();
            d.to_string_lossy().into_owned()
        })
        .collect();
    let paths: Vec<String> = (0..5000)
        .map(|p| format!("{}/d{p}/f.rs", base.display()))
        .collect();
    time_rounds("5000 distinct dirs x 2 roots", &paths, &roots);
}

// ---- performance bounds that run in the suite -------------------------------

/// A path of 100k components is unattributable, not read: nothing walks it. A
/// path that would be under the root but is longer than the cap is refused too,
/// and one at the cap is not.
#[test]
fn a_very_long_path_is_unattributable_and_costs_nothing() {
    let root = "/r";
    let under_root = |len: usize| format!("{root}/{}", "a".repeat(len - root.len() - 1));
    assert!(is_under(&under_root(4096), root));
    assert!(!is_under(&under_root(4097), root));
    let tmp = canon_tmp();
    let root = real(&tmp);
    let path = format!("/zz-adv2-long{}", "/a".repeat(100_000));
    let started = Instant::now();
    let got = is_under(&path, root.to_str().unwrap());
    let took = started.elapsed();
    assert!(!got);
    assert!(
        took < coverage_relaxed(Duration::from_millis(250)),
        "under() on a 200 KB path took {took:?}"
    );
}

// ---- cache staleness ---------------------------------------------------------

/// A root link that is re-pointed between passes names its new target in the
/// next pass: the set is built for the pass and nothing outlives it. Each
/// `is_under` is one pass.
#[cfg(unix)]
#[test]
fn a_retargeted_root_link_is_read_again_by_the_next_pass() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let link = base.join("root-link");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    let root = link.to_str().unwrap();
    let in_a = a.join("f.rs");
    let in_b = b.join("f.rs");
    assert!(is_under(in_a.to_str().unwrap(), root));
    assert!(!is_under(in_b.to_str().unwrap(), root));
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&b, &link).unwrap();
    assert!(
        is_under(in_b.to_str().unwrap(), root),
        "the root now names b; b's file is not attributed to it"
    );
    assert!(
        !is_under(in_a.to_str().unwrap(), root),
        "the root no longer names a; a's file is still attributed to it"
    );
}

/// A directory link inside a transcript's path is taken as written, before and
/// after it is re-pointed: the path is not resolved, so there is nothing to go
/// stale, and `dir-link/f.rs` is not under the link's target either way.
#[cfg(unix)]
#[test]
fn a_directory_link_inside_a_path_is_taken_as_written() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let link = base.join("dir-link");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    let path = link.join("f.rs");
    assert!(!is_under(path.to_str().unwrap(), a.to_str().unwrap()));
    assert!(is_under(path.to_str().unwrap(), link.to_str().unwrap()));
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&b, &link).unwrap();
    assert!(!is_under(path.to_str().unwrap(), a.to_str().unwrap()));
    assert!(!is_under(path.to_str().unwrap(), b.to_str().unwrap()));
}

// ---- correctness: links, absence, permissions --------------------------------

/// Held: a link that points OUT of the root is still lexically inside it (the
/// pre-change answer, unchanged), and the file it points at is not inside.
#[cfg(unix)]
#[test]
fn adv2_symlink_out_of_root_does_not_pull_the_outside_in() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let (root, outside) = (base.join("root"), base.join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    let via = root.join("link").join("f.rs");
    let direct = outside.join("f.rs");
    assert!(is_under(via.to_str().unwrap(), root.to_str().unwrap()));
    assert!(!is_under(direct.to_str().unwrap(), root.to_str().unwrap()));
}

/// Held: the same answer for a sibling that only shares a name prefix with the
/// resolved root.
#[cfg(unix)]
#[test]
fn adv2_resolved_prefix_sibling_is_not_under() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let target = base.join("repo");
    let sibling = base.join("repo-other");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let link = base.join("alias");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let f = sibling.join("f.rs");
    assert!(!is_under(f.to_str().unwrap(), link.to_str().unwrap()));
}

#[test]
fn adv2_absent_file_and_unreadable_roots_answer_without_panic_or_hang() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let file = base.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let missing = base.join("missing");
    let started = Instant::now();
    assert!(!is_under(
        missing.join("x").to_str().unwrap(),
        file.to_str().unwrap()
    ));
    assert!(!is_under(file.to_str().unwrap(), missing.to_str().unwrap()));
    assert!(is_under(
        missing.join("x").to_str().unwrap(),
        missing.to_str().unwrap()
    ));
    assert!(!is_under("/", missing.to_str().unwrap()));
    assert!(!is_under("/nul\0byte/x", "/nul\0byte2"));
    assert!(started.elapsed() < coverage_relaxed(Duration::from_secs(2)));
    // A symlink loop is not a hang.
    #[cfg(unix)]
    {
        let (x, y) = (base.join("x"), base.join("y"));
        std::os::unix::fs::symlink(&y, &x).unwrap();
        std::os::unix::fs::symlink(&x, &y).unwrap();
        assert!(!is_under(
            x.join("f").to_str().unwrap(),
            base.join("root").to_str().unwrap()
        ));
        assert!(!is_under(
            base.join("root").join("f").to_str().unwrap(),
            x.to_str().unwrap()
        ));
    }
}

#[cfg(unix)]
#[test]
fn adv2_permission_denied_root_keeps_the_lexical_answer() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = canon_tmp();
    let base = real(&tmp);
    let locked = base.join("locked");
    std::fs::create_dir_all(locked.join("sub")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let inside = locked.join("sub").join("f.rs");
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&locked, &alias).unwrap();
    let lexical = is_under(inside.to_str().unwrap(), locked.to_str().unwrap());
    let through_link = is_under(inside.to_str().unwrap(), alias.to_str().unwrap());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(lexical);
    // Not asserted either way beyond "answers": a denied directory cannot be
    // resolved, and false is the safe reading.
    let _ = through_link;
}

/// Case folding is probed on the root only when the root exists.
#[test]
fn adv2_case_fold_on_a_nonexistent_root_is_exact() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let root = base.join("Gone");
    let path = base.join("gone").join("f.rs");
    assert!(!is_under(path.to_str().unwrap(), root.to_str().unwrap()));
}

// ---- Windows spellings -------------------------------------------------------

#[test]
fn adv2_windows_matrix_that_holds() {
    for (path, root, want) in [
        ("C:\\repo\\a", "C:\\repo", true),
        ("c:\\repo\\a", "C:\\repo\\", true),
        ("C:/repo\\a/b", "c:\\REPO", true),
        ("C:\\repo\\\\a", "C:\\repo", true),
        ("C:\\repo\\..\\..\\x", "C:\\repo", false),
        ("C:\\..\\repo\\a", "C:\\repo", true),
        ("D:\\repo\\a", "C:\\repo", false),
        // drive-relative: no root to compare against; answer is no.
        ("C:repo\\a", "C:\\repo", false),
        ("C:\\repo\\a", "C:repo", false),
        ("C:\\repo", "C:", false),
        ("C:\\x", "C:/", true),
        ("\\\\srv\\share\\a", "\\\\SRV\\Share", true),
        ("\\\\srv\\share2\\a", "\\\\srv\\share", false),
        ("\\\\srv2\\share\\a", "\\\\srv\\share", false),
        ("/c:/repo/a", "C:\\repo", false),
        ("C:\\repo\\a", "/repo", false),
    ] {
        assert_eq!(is_under(path, root), want, "under({path:?}, {root:?})");
    }
}

/// Git and Node on Windows print UNC with forward slashes (`//server/share/x`):
/// the same place as the backslash spelling. A POSIX root that starts with `//`
/// still collapses (`//w1/f` is under `/w1`).
#[test]
fn a_forward_slash_unc_path_is_the_same_place_as_the_backslash_one() {
    assert!(is_under("//srv/share/dir/f", "\\\\srv\\share"));
    assert!(is_under("\\\\srv\\share\\dir\\f", "//srv/share"));
}

/// Extended-length `\\?\C:\repo\a.rs` (what Rust's own `fs::canonicalize`
/// returns on Windows, so what GitPulse itself records for roots) is
/// `C:\repo\a.rs`, and `\\?\UNC\srv\share` is `\\srv\share`.
#[test]
fn a_verbatim_prefix_is_the_same_place() {
    assert!(is_under("\\\\?\\UNC\\srv\\share\\d\\f", "\\\\srv\\share"));
    assert!(is_under("C:\\repo\\a.rs", "\\\\?\\C:\\repo"));
    assert!(is_under("\\\\?\\C:\\repo\\a.rs", "C:\\repo"));
}

/// `..` stays inside its share: `\\srv\other\..\share\f` is a path in `other`.
#[test]
fn a_unc_dotdot_cannot_leave_its_share() {
    assert!(!is_under("\\\\srv\\other\\..\\share\\f", "\\\\srv\\share"));
    assert!(!is_under("\\\\srv\\share\\..\\..\\..\\x\\f", "\\\\srv\\x"));
}

/// Held: a backslash in a POSIX name stays a byte match.
#[test]
fn adv2_posix_backslash_name_is_a_byte_match() {
    assert!(!is_under("/a\\b/f", "/a"));
    assert!(is_under("/a\\b/f", "/a\\b"));
    assert!(!is_under("/a/b\\c", "/a/b"));
    // A relative POSIX path that merely starts like a drive is Windows-shaped
    // by the rule, and is not under a POSIX root.
    assert!(!is_under("C:\\Users\\x", "/Users/x"));
}

/// `deepest_root` ranks by length after trimming `/` and `\`, so a Windows root
/// spelled with trailing backslashes does not outrank a deeper one.
#[test]
fn deepest_root_ranks_windows_roots_by_depth() {
    let got = deepest_root(
        "C:\\repo\\wt\\a.rs",
        &RootSet::new(&["C:\\repo\\\\\\\\\\", "C:\\repo\\wt"]),
    );
    assert_eq!(got, Some("C:\\repo\\wt"));
}

// ---- directory_key -----------------------------------------------------------

#[cfg(unix)]
#[test]
fn adv2_directory_key_collapses_links_and_separates_siblings() {
    use crate::repository_trust::directory_key;
    let tmp = canon_tmp();
    let base = real(&tmp);
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    let detour = base.join("b").join("..").join("a");
    assert_eq!(directory_key(&a).unwrap(), directory_key(&link).unwrap());
    assert_eq!(directory_key(&a).unwrap(), directory_key(&detour).unwrap());
    assert_ne!(directory_key(&a).unwrap(), directory_key(&b).unwrap());
    // A file, a missing path, and an unreadable parent are errors, not keys.
    let file = base.join("f");
    std::fs::write(&file, b"x").unwrap();
    assert!(directory_key(&file).is_err());
    assert!(directory_key(&base.join("missing")).is_err());
}

/// An unreadable identity for one member must not merge it into another: the
/// fallback key is the exact path, so two different unreadable repositories
/// stay two. (Direct on the key type: a directory whose parent denies search.)
#[cfg(unix)]
#[test]
fn adv2_unreadable_directory_key_errors_instead_of_colliding() {
    use crate::repository_trust::directory_key;
    use std::os::unix::fs::PermissionsExt;
    let tmp = canon_tmp();
    let base = real(&tmp);
    let hidden = base.join("hidden");
    std::fs::create_dir_all(hidden.join("r1")).unwrap();
    std::fs::create_dir_all(hidden.join("r2")).unwrap();
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (k1, k2) = (
        directory_key(&hidden.join("r1")),
        directory_key(&hidden.join("r2")),
    );
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(k1.is_err() && k2.is_err(), "{k1:?} {k2:?}");
}

fn git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            crate::test_support::env::hermetic::pinned_git_config(),
        )
        .status_locked()
        .unwrap();
    assert!(status.success());
    crate::test_support::trust_repo(dir);
}

/// The same repository reached through a symlink is one member, and two
/// repositories under spellings that differ are two. The registry labels stay
/// distinct.
#[cfg(unix)]
#[test]
fn adv2_sync_open_tabs_collapses_a_symlinked_repo_and_labels_stay_unique() {
    let tmp = canon_tmp();
    let base = real(&tmp);
    let host = base.join("host");
    let (one, two) = (base.join("p").join("repo"), base.join("q").join("repo"));
    for d in [&host, &one, &two] {
        git_repo(d);
    }
    let link = base.join("repo-link");
    std::os::unix::fs::symlink(&one, &link).unwrap();
    let snap = crate::workspace_registry::sync_open_tabs(
        host.to_str().unwrap(),
        &[
            one.to_string_lossy().into_owned(),
            link.to_string_lossy().into_owned(),
            two.to_string_lossy().into_owned(),
        ],
    )
    .unwrap();
    assert_eq!(snap.repos.len(), 3, "{:?}", snap.repos);
    let names: std::collections::HashSet<_> = snap.repos.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names.len(), 3, "labels collide: {:?}", snap.repos);
}
