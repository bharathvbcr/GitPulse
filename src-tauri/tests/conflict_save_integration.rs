use gitpulse_lib::diff::{
    conflict_session::{self, ConflictFileChoice, ConflictSaveRequest, ConflictSnapshot},
    ConflictResolutionChoice,
};
use gitpulse_lib::engine::git_cli::{git, git_with_stdin};
use std::fs;
use tempfile::TempDir;
mod common;
use common::run_git;

fn fixture() -> TempDir {
    fixture_with_format("sha1")
}
fn fixture_with_format(format: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let path = dir.path();
    run_git(
        path,
        &["init", &format!("--object-format={format}"), "-b", "main"],
    );
    run_git(path, &["config", "commit.gpgsign", "false"]);
    run_git(path, &["config", "core.autocrlf", "false"]);
    fs::write(path.join("file"), "base\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "base"]);
    run_git(path, &["checkout", "-b", "incoming"]);
    fs::write(path.join("file"), "theirs\n").unwrap();
    run_git(path, &["commit", "-am", "theirs"]);
    run_git(path, &["checkout", "main"]);
    fs::write(path.join("file"), "ours\n").unwrap();
    run_git(path, &["commit", "-am", "ours"]);
    assert!(git(path, &["merge", "incoming"]).is_err());
    dir
}

fn load(dir: &TempDir) -> ConflictSnapshot {
    conflict_session::snapshot(dir.path().to_str().unwrap(), "file").unwrap()
}
fn save(dir: &TempDir, captured: &ConflictSnapshot) -> Result<(), String> {
    let repo = dir.path().to_str().unwrap();
    let result = conflict_session::save(
        repo,
        &ConflictSaveRequest {
            file_path: "file".into(),
            revision: captured.revision.clone(),
            choice: ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptOurs]),
        },
    )?;
    if result.staged {
        Ok(())
    } else {
        Err(result.message)
    }
}

#[test]
fn stale_working_file_is_not_overwritten() {
    let dir = fixture();
    let captured = load(&dir);
    fs::write(dir.path().join("file"), "external edit\n").unwrap();
    assert!(save(&dir, &captured).is_err());
    assert_eq!(
        fs::read(dir.path().join("file")).unwrap(),
        b"external edit\n"
    );
    assert!(!git(dir.path(), &["ls-files", "--unmerged"])
        .unwrap()
        .is_empty());
}

#[test]
fn changed_operation_is_not_resolved() {
    let dir = fixture();
    let captured = load(&dir);
    run_git(dir.path(), &["merge", "--abort"]);
    assert!(save(&dir, &captured).is_err());
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"ours\n");
}

#[test]
fn restarting_an_identical_merge_invalidates_the_old_operation() {
    let dir = fixture();
    let captured = load(&dir);
    run_git(dir.path(), &["merge", "--abort"]);
    assert!(git(dir.path(), &["merge", "incoming"]).is_err());
    assert_ne!(captured.revision, load(&dir).revision);
    assert!(save(&dir, &captured).is_err());
}

#[test]
fn operation_continue_rejects_markers_staged_by_an_external_editor() {
    use gitpulse_lib::engine::repo_op::{run_action_with, OperationAction};
    let dir = fixture();
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    run_git(dir.path(), &["add", "file"]);
    assert!(run_action_with(
        dir.path().to_str().unwrap(),
        OperationAction::Continue,
        |_| Ok(())
    )
    .is_err());
    assert!(dir.path().join(".git/MERGE_HEAD").exists());
}

#[test]
fn an_index_lock_failure_does_not_write_the_working_file() {
    let dir = fixture();
    let captured = load(&dir);
    let original = fs::read(dir.path().join("file")).unwrap();
    fs::write(dir.path().join(".git/index.lock"), "another owner").unwrap();
    assert!(save(&dir, &captured).is_err());
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), original);
    assert_eq!(
        fs::read_to_string(dir.path().join(".git/index.lock")).unwrap(),
        "another owner"
    );
}

#[test]
fn successful_resolution_stages_the_exact_result_and_cleans_temporary_files() {
    let dir = fixture();
    let captured = load(&dir);
    save(&dir, &captured).unwrap();
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"ours\n");
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"ours\n");
    assert!(git(dir.path(), &["ls-files", "--unmerged"])
        .unwrap()
        .is_empty());
    for parent in [dir.path().to_path_buf(), dir.path().join(".git")] {
        assert!(!fs::read_dir(parent).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".gitpulse-conflict-")));
    }
    assert!(!dir.path().join(".git/index.lock").exists());
    assert!(
        save(&dir, &captured).is_err(),
        "a duplicate stale request cannot write twice"
    );
}

fn choose(
    dir: &TempDir,
    file: &str,
    choice: ConflictFileChoice,
) -> Result<conflict_session::ConflictSaveOutcome, String> {
    let repo = dir.path().to_str().unwrap();
    let snap = conflict_session::snapshot(repo, file)?;
    conflict_session::save(
        repo,
        &ConflictSaveRequest {
            file_path: file.into(),
            revision: snap.revision,
            choice,
        },
    )
}
fn object(dir: &TempDir, bytes: &[u8]) -> String {
    String::from_utf8(git_with_stdin(dir.path(), &["hash-object", "-w", "--stdin"], bytes).unwrap())
        .unwrap()
        .trim()
        .into()
}
fn set_stages(
    dir: &TempDir,
    file: &str,
    ours: Option<(&str, &[u8])>,
    theirs: Option<(&str, &[u8])>,
) {
    let mut input = format!("0 {}\t{file}\0", "0".repeat(40));
    for (stage, value) in [(2, ours), (3, theirs)] {
        if let Some((mode, bytes)) = value {
            input.push_str(&format!("{mode} {} {stage}\t{file}\0", object(dir, bytes)));
        }
    }
    git_with_stdin(
        dir.path(),
        &["update-index", "-z", "--index-info"],
        input.as_bytes(),
    )
    .unwrap();
}

#[test]
fn changing_the_index_stages_invalidates_the_loaded_choices() {
    let dir = fixture();
    let captured = load(&dir);
    let original = fs::read(dir.path().join("file")).unwrap();
    set_stages(
        &dir,
        "file",
        Some(("100644", b"different stage")),
        Some(("100644", b"theirs")),
    );
    assert!(save(&dir, &captured).is_err());
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), original);
}

#[test]
fn unrelated_staging_is_preserved_and_does_not_invalidate_the_target() {
    let dir = fixture();
    let captured = load(&dir);
    fs::write(dir.path().join("unrelated"), b"keep staged").unwrap();
    run_git(dir.path(), &["add", "unrelated"]);
    save(&dir, &captured).unwrap();
    assert_eq!(
        git(dir.path(), &["show", ":unrelated"]).unwrap(),
        b"keep staged"
    );
}

#[test]
fn binary_and_non_utf8_whole_file_choices_preserve_exact_bytes() {
    for bytes in [&b"\0\xffours"[..], &b"\xff\xfeours"[..]] {
        let dir = fixture();
        set_stages(
            &dir,
            "file",
            Some(("100644", bytes)),
            Some(("100644", b"\0theirs")),
        );
        fs::write(dir.path().join("file"), bytes).unwrap();
        assert!(load(&dir).document.is_none());
        assert!(
            choose(&dir, "file", ConflictFileChoice::Theirs)
                .unwrap()
                .staged
        );
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"\0theirs");
        assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"\0theirs");
    }
}

#[test]
fn choosing_a_missing_side_stages_deletion_and_keeps_other_paths() {
    let dir = fixture();
    set_stages(&dir, "file", Some(("100644", b"ours")), None);
    assert!(
        choose(&dir, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert!(!dir.path().join("file").exists());
    assert!(git(dir.path(), &["ls-files", "--", "file"])
        .unwrap()
        .is_empty());
}

#[test]
fn a_missing_working_file_can_be_restored_from_an_index_side() {
    let dir = fixture();
    fs::remove_file(dir.path().join("file")).unwrap();
    assert_eq!(load(&dir).worktree_mode, "missing");
    assert!(
        choose(&dir, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"theirs\n");
}

#[test]
fn literal_pathspecs_do_not_expand_globs_or_break_on_newlines() {
    let dir = fixture();
    let name = "[a]*\n-é.txt";
    fs::write(dir.path().join(name), b"ours").unwrap();
    set_stages(
        &dir,
        name,
        Some(("100644", b"ours")),
        Some(("100644", b"chosen")),
    );
    assert!(
        choose(&dir, name, ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_eq!(fs::read(dir.path().join(name)).unwrap(), b"chosen");
    assert!(!git(dir.path(), &["ls-files", "--unmerged", "--", "file"])
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn whole_file_choices_preserve_executable_modes_and_symbolic_links() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    set_stages(
        &dir,
        "file",
        Some(("100644", b"ours")),
        Some(("100755", b"#!/bin/sh\nexit 0\n")),
    );
    assert!(
        choose(&dir, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_ne!(
        fs::metadata(dir.path().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    set_stages(
        &dir,
        "file",
        Some(("100755", b"original")),
        Some(("120000", b"../../outside-target")),
    );
    assert!(
        choose(&dir, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_eq!(
        fs::read_link(dir.path().join("file")).unwrap(),
        std::path::Path::new("../../outside-target")
    );
    assert!(
        String::from_utf8(git(dir.path(), &["ls-files", "--stage", "--", "file"]).unwrap())
            .unwrap()
            .starts_with("120000 ")
    );
}

#[cfg(unix)]
#[test]
fn symlink_ancestors_and_metadata_paths_are_rejected_without_writes() {
    let dir = fixture();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("file"), b"protected").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    for file in [
        "escape/file",
        "../file",
        ".git/config",
        "/absolute",
        "./file",
    ] {
        assert!(
            conflict_session::snapshot(dir.path().to_str().unwrap(), file).is_err(),
            "accepted {file}"
        );
    }
    assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"protected");
}

#[test]
fn staged_filters_and_crlf_are_respected_without_changing_preview_source_bytes() {
    let dir = fixture();
    fs::write(dir.path().join(".gitattributes"), "file text eol=crlf\n").unwrap();
    let original = fs::read_to_string(dir.path().join("file"))
        .unwrap()
        .replace('\n', "\r\n");
    fs::write(dir.path().join("file"), &original).unwrap();
    save(&dir, &load(&dir)).unwrap();
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"ours\r\n");
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"ours\n");
}

#[cfg(unix)]
#[test]
fn atomic_resolution_does_not_widen_private_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    fs::set_permissions(dir.path().join("file"), fs::Permissions::from_mode(0o600)).unwrap();
    save(&dir, &load(&dir)).unwrap();
    assert_eq!(
        fs::metadata(dir.path().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(target_os = "macos")]
#[test]
fn atomic_resolution_preserves_extended_attributes() {
    let dir = fixture();
    let path = dir.path().join("file");
    assert!(std::process::Command::new("/usr/bin/xattr")
        .args(["-w", "com.gitpulse.conflict-test", "keep-me"])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    save(&dir, &load(&dir)).unwrap();
    let result = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", "com.gitpulse.conflict-test"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"keep-me\n");
}

#[test]
fn repository_marker_width_is_used_by_the_text_editor() {
    let dir = fixture();
    fs::write(
        dir.path().join(".gitattributes"),
        "file conflict-marker-size=3\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("file"),
        "<<< HEAD\nours\n===\ntheirs\n>>> incoming\n",
    )
    .unwrap();
    let captured = load(&dir);
    assert_eq!(captured.document.as_ref().unwrap().total_conflicts, 1);
    save(&dir, &captured).unwrap();
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"ours\n");
}

#[test]
fn simultaneous_saves_have_one_winner_without_mixed_outputs() {
    use std::sync::{Arc, Barrier};
    let dir = fixture();
    let snap = load(&dir);
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let repo = dir.path().to_str().unwrap().to_owned();
            let revision = snap.revision.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                conflict_session::save(
                    &repo,
                    &ConflictSaveRequest {
                        file_path: "file".into(),
                        revision,
                        choice: ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::Custom(
                            format!("writer {index}"),
                        )]),
                    },
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().is_ok_and(|value| value.staged))
            .count(),
        1
    );
    let working = fs::read(dir.path().join("file")).unwrap();
    assert_eq!(working, git(dir.path(), &["show", ":file"]).unwrap());
    assert!(std::str::from_utf8(&working)
        .unwrap()
        .starts_with("writer "));
}

#[test]
fn staging_retry_preserves_binary_bytes_without_rewriting() {
    let dir = fixture();
    let bytes = b"resolved\0binary\xff";
    fs::write(dir.path().join("file"), bytes).unwrap();
    let result = choose(&dir, "file", ConflictFileChoice::StageOnly).unwrap();
    assert!(result.staged);
    assert!(!result.written);
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), bytes);
}

#[test]
fn choosing_an_already_present_side_reports_no_working_file_write() {
    let dir = fixture();
    fs::write(dir.path().join("file"), b"ours\n").unwrap();
    let before = fs::metadata(dir.path().join("file"))
        .unwrap()
        .modified()
        .unwrap();
    let outcome = choose(&dir, "file", ConflictFileChoice::Ours).unwrap();
    assert!(outcome.staged);
    assert!(!outcome.written);
    assert_eq!(
        fs::metadata(dir.path().join("file"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn externally_resolved_text_can_be_staged_beyond_the_interactive_editor_limit() {
    let dir = fixture();
    let content = "x".repeat(4 * 1024 * 1024 + 1);
    fs::write(dir.path().join("file"), &content).unwrap();
    assert!(load(&dir).document.is_none());
    let outcome = choose(&dir, "file", ConflictFileChoice::WorkingTree).unwrap();
    assert!(outcome.staged);
    assert!(!outcome.written);
    assert_eq!(
        git(dir.path(), &["show", ":file"]).unwrap(),
        content.as_bytes()
    );
}

#[test]
fn clean_filters_cannot_introduce_markers_into_the_staged_resolution() {
    let dir = fixture();
    let original = fs::read(dir.path().join("file")).unwrap();
    fs::write(dir.path().join(".gitattributes"), "file filter=markers\n").unwrap();
    run_git(
        dir.path(),
        &[
            "config",
            "filter.markers.clean",
            "printf '<<<<<<< filter\\na\\n=======\\nb\\n>>>>>>> filter\\n'",
        ],
    );
    let result = save(&dir, &load(&dir));
    assert!(result.is_err(), "filter-generated markers were staged");
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), original);
    assert!(!git(dir.path(), &["ls-files", "--unmerged"])
        .unwrap()
        .is_empty());
}

#[test]
fn a_clean_filter_that_changes_the_source_cannot_overwrite_the_external_edit() {
    let dir = fixture();
    fs::write(dir.path().join(".gitattributes"), "file filter=race\n").unwrap();
    run_git(
        dir.path(),
        &["config", "filter.race.clean", "printf external > file; cat"],
    );
    assert!(save(&dir, &load(&dir))
        .unwrap_err()
        .contains("Source changed"));
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"external");
    assert!(!git(dir.path(), &["ls-files", "--unmerged"])
        .unwrap()
        .is_empty());
}

#[test]
fn sha256_repositories_stage_the_exact_selected_object() {
    let dir = fixture_with_format("sha256");
    let snap = load(&dir);
    assert_eq!(snap.stages[0].oid.len(), 64);
    assert!(
        choose(&dir, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"theirs\n");
}

#[test]
fn linked_worktrees_use_their_own_operation_and_index() {
    let main = fixture();
    run_git(main.path(), &["merge", "--abort"]);
    let linked = TempDir::new().unwrap();
    run_git(
        main.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked.path().to_str().unwrap(),
            "main",
        ],
    );
    let main_index = fs::read(main.path().join(".git/index")).unwrap();
    assert!(git(linked.path(), &["merge", "incoming"]).is_err());
    assert!(
        choose(&linked, "file", ConflictFileChoice::Theirs)
            .unwrap()
            .staged
    );
    assert_eq!(git(linked.path(), &["show", ":file"]).unwrap(), b"theirs\n");
    assert_eq!(
        fs::read(main.path().join(".git/index")).unwrap(),
        main_index
    );
    assert_eq!(fs::read(main.path().join("file")).unwrap(), b"ours\n");
}

#[test]
fn submodule_choices_only_update_the_gitlink_and_preserve_working_content() {
    let dir = fixture();
    let ours = String::from_utf8(git(dir.path(), &["rev-parse", "HEAD"]).unwrap()).unwrap();
    let theirs = String::from_utf8(git(dir.path(), &["rev-parse", "incoming"]).unwrap()).unwrap();
    let input = format!(
        "0 {}\tfile\0{} {} 2\tfile\0{} {} 3\tfile\0",
        "0".repeat(40),
        "160000",
        ours.trim(),
        "160000",
        theirs.trim()
    );
    git_with_stdin(
        dir.path(),
        &["update-index", "-z", "--index-info"],
        input.as_bytes(),
    )
    .unwrap();
    fs::remove_file(dir.path().join("file")).unwrap();
    fs::create_dir(dir.path().join("file")).unwrap();
    fs::write(dir.path().join("file/local-edit"), "keep me").unwrap();
    let outcome = choose(&dir, "file", ConflictFileChoice::Theirs).unwrap();
    assert!(outcome.staged);
    assert!(!outcome.written);
    assert_eq!(
        fs::read(dir.path().join("file/local-edit")).unwrap(),
        b"keep me"
    );
    let entry =
        String::from_utf8(git(dir.path(), &["ls-files", "--stage", "file"]).unwrap()).unwrap();
    assert!(entry.starts_with(&format!("160000 {} 0", theirs.trim())));
}

#[cfg(unix)]
#[test]
fn staging_retry_preserves_symlink_identity_and_private_index_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    fs::remove_file(dir.path().join("file")).unwrap();
    std::os::unix::fs::symlink("../outside", dir.path().join("file")).unwrap();
    fs::set_permissions(
        dir.path().join(".git/index"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        choose(&dir, "file", ConflictFileChoice::StageOnly)
            .unwrap()
            .staged
    );
    assert_eq!(git(dir.path(), &["show", ":file"]).unwrap(), b"../outside");
    assert_eq!(
        fs::read_link(dir.path().join("file")).unwrap(),
        std::path::Path::new("../outside")
    );
    assert_eq!(
        fs::metadata(dir.path().join(".git/index"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn special_files_are_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let dir = fixture();
    fs::remove_file(dir.path().join("file")).unwrap();
    let name = std::ffi::CString::new(dir.path().join("file").as_os_str().as_bytes()).unwrap();
    // SAFETY: NUL-terminated path to a new FIFO in this test's temporary repo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(conflict_session::snapshot(dir.path().to_str().unwrap(), "file").is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn every_mutating_conflict_command_is_judged_before_execution() {
    for denied in ["hash-object", "update-index"] {
        let dir = fixture();
        let captured = load(&dir);
        let source = fs::read(dir.path().join("file")).unwrap();
        let index = fs::read(dir.path().join(".git/index")).unwrap();
        let mut judged = Vec::new();
        let result = conflict_session::save_with_gate(
            dir.path().to_str().unwrap(),
            &ConflictSaveRequest {
                file_path: "file".into(),
                revision: captured.revision,
                choice: ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptOurs]),
            },
            |file, op| {
                assert_eq!((file, op), ("file", "modify"));
                Ok(())
            },
            |argv| {
                judged.push(argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
                if argv[1] == denied {
                    Err("fixture policy denied".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.unwrap_err(), "fixture policy denied");
        assert_eq!(
            judged[0],
            ["git", "hash-object", "-w", "--path=file", "--stdin"]
        );
        if denied == "update-index" {
            assert_eq!(judged[1], ["git", "update-index", "-z", "--index-info"]);
        }
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), source);
        assert_eq!(fs::read(dir.path().join(".git/index")).unwrap(), index);
        assert!(!dir.path().join(".git/index.lock").exists());
    }
}

#[test]
fn file_authorization_distinguishes_creation_modification_and_deletion() {
    for operation in ["create", "modify", "delete", "stage-deletion"] {
        let dir = fixture();
        let choice = match operation {
            "stage-deletion" => {
                fs::remove_file(dir.path().join("file")).unwrap();
                ConflictFileChoice::StageOnly
            }
            "create" => {
                fs::remove_file(dir.path().join("file")).unwrap();
                ConflictFileChoice::Theirs
            }
            "delete" => {
                set_stages(&dir, "file", Some(("100644", b"ours")), None);
                ConflictFileChoice::Theirs
            }
            _ => ConflictFileChoice::Chunks(vec![ConflictResolutionChoice::AcceptOurs]),
        };
        let captured = load(&dir);
        let source = fs::read(dir.path().join("file")).ok();
        let index = fs::read(dir.path().join(".git/index")).unwrap();
        let mut checked = false;
        let result = conflict_session::save_with_gate(
            dir.path().to_str().unwrap(),
            &ConflictSaveRequest {
                file_path: "file".into(),
                revision: captured.revision,
                choice,
            },
            |file, op| {
                checked = true;
                assert_eq!(
                    (file, op),
                    (
                        "file",
                        if operation == "stage-deletion" {
                            "delete"
                        } else {
                            operation
                        }
                    )
                );
                Err("file capability denied".into())
            },
            |_| panic!("file denial must stop before any mutating Git command"),
        );
        assert!(checked);
        assert_eq!(result.unwrap_err(), "file capability denied");
        assert_eq!(fs::read(dir.path().join("file")).ok(), source);
        assert_eq!(fs::read(dir.path().join(".git/index")).unwrap(), index);
        assert!(!dir.path().join(".git/index.lock").exists());
    }
}
