//! ADVERSARY (W5a, ft-67f95e2d item 7): `git_in` must give a fixture command the
//! hermetic git environment whatever a neighbouring test has rebound.
//!
//! The lane pinned four variables (`GIT_CONFIG_GLOBAL`, `XDG_CONFIG_HOME`,
//! `USERPROFILE`, `HOME`) by naming them on the child. A neighbour holds the
//! environment lock, which `git_in` does not take, and can rebind *any*
//! variable git reads. Each test here holds the lock, rebinds one such
//! variable the way a neighbour would, and runs a fixture command; a test that
//! fails names a variable that still reaches the fixture.
//!
//! Variables the startup constructor scrubs (`SCRUBBED` in `env.rs`) are the
//! ones a neighbour can put back; variables it does not scrub are named here
//! too (`GIT_COMMITTER_*`, `GIT_CEILING_DIRECTORIES`).

use super::env::{bind_env, env_serial, hermetic, EnvBinding};
use super::git_in;
use std::path::Path;

/// `git_in … init` under a neighbour's rebind; the new repository's `HEAD`.
fn init_head(rebind: impl for<'a> FnOnce(EnvBinding<'a>, &Path) -> EnvBinding<'a>) -> String {
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    {
        let _env = rebind(bind_env(&serial), neighbour.path());
        git_in(repo.path(), &["init", "-q"]);
    }
    std::fs::read_to_string(repo.path().join(".git/HEAD")).unwrap()
}

fn leaking_config(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    super::env::write_git_config(&path, "[init]\n\tdefaultBranch = leaked\n").unwrap();
    path
}

#[test]
fn a_neighbours_system_config_does_not_reach_a_fixture() {
    let head = init_head(|env, dir| {
        env.remove("GIT_CONFIG_NOSYSTEM")
            .set("GIT_CONFIG_SYSTEM", leaking_config(dir, "system.gitconfig"))
    });
    assert!(
        !head.contains("leaked"),
        "git_in read a rebound system config: {head}"
    );
}

#[test]
fn a_neighbours_config_parameters_do_not_reach_a_fixture() {
    let head = init_head(|env, _| env.set("GIT_CONFIG_PARAMETERS", "'init.defaultBranch=leaked'"));
    assert!(
        !head.contains("leaked"),
        "git_in read GIT_CONFIG_PARAMETERS: {head}"
    );
}

#[test]
fn a_neighbours_config_count_does_not_reach_a_fixture() {
    let head = init_head(|env, _| {
        env.set("GIT_CONFIG_COUNT", "1")
            .set("GIT_CONFIG_KEY_0", "init.defaultBranch")
            .set("GIT_CONFIG_VALUE_0", "leaked")
    });
    assert!(
        !head.contains("leaked"),
        "git_in read GIT_CONFIG_COUNT: {head}"
    );
}

#[test]
fn a_neighbours_git_dir_does_not_redirect_a_fixture() {
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let elsewhere = neighbour.path().join("elsewhere.git");
    {
        let _env = bind_env(&serial).set("GIT_DIR", &elsewhere);
        // The helper panics when git's own answer is wrong; the assertion below
        // is the one that names the cause.
        let _ = std::panic::catch_unwind(|| git_in(repo.path(), &["init", "-q"]));
    }
    assert!(
        repo.path().join(".git").is_dir() && !elsewhere.exists(),
        "git_in followed GIT_DIR: repo has .git = {}, the neighbour's dir exists = {}",
        repo.path().join(".git").is_dir(),
        elsewhere.exists()
    );
}

#[test]
fn a_neighbours_work_tree_does_not_reach_a_fixture() {
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    {
        let _env = bind_env(&serial).set("GIT_WORK_TREE", neighbour.path());
        let _ = std::panic::catch_unwind(|| git_in(repo.path(), &["init", "-q"]));
    }
    let config = std::fs::read_to_string(repo.path().join(".git/config")).unwrap_or_default();
    assert!(
        !config.contains("worktree"),
        "git_in followed GIT_WORK_TREE: {config}"
    );
}

#[test]
fn a_neighbours_index_file_does_not_reach_a_fixture() {
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_in(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "x").unwrap();
    {
        let _env = bind_env(&serial).set("GIT_INDEX_FILE", neighbour.path().join("index"));
        let _ = std::panic::catch_unwind(|| git_in(repo.path(), &["add", "f.txt"]));
    }
    assert!(
        repo.path().join(".git/index").is_file(),
        "git_in wrote the neighbour's GIT_INDEX_FILE, not the repository's index"
    );
}

#[test]
fn a_neighbours_committer_identity_does_not_reach_a_fixture() {
    let serial = env_serial();
    let repo = tempfile::tempdir().unwrap();
    git_in(repo.path(), &["init", "-q"]);
    {
        let _env = bind_env(&serial)
            .set("GIT_COMMITTER_NAME", "Neighbour")
            .set("GIT_COMMITTER_EMAIL", "neighbour@elsewhere.test")
            .set("GIT_AUTHOR_NAME", "Neighbour");
        git_in(repo.path(), &["commit", "-q", "--allow-empty", "-m", "x"]);
    }
    let reflog = std::fs::read_to_string(repo.path().join(".git/logs/HEAD")).unwrap();
    assert!(
        !reflog.contains("Neighbour") && reflog.contains("gitpulse@test.local"),
        "git_in committed as the neighbour's identity: {reflog}"
    );
}

#[test]
fn a_neighbours_ceiling_directories_do_not_hide_the_repository_from_a_fixture() {
    let serial = env_serial();
    let repo = tempfile::tempdir().unwrap();
    git_in(repo.path(), &["init", "-q"]);
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let ceiling = std::fs::canonicalize(repo.path()).unwrap();
    let _env = bind_env(&serial).set("GIT_CEILING_DIRECTORIES", &ceiling);
    // Fails with "not a git repository" when the ceiling reaches the child.
    git_in(&sub, &["status", "--short"]);
}

/// The `XDG_CONFIG_HOME` pin, proved through something git reads from it that
/// `GIT_CONFIG_GLOBAL` does not replace: the default `core.excludesFile` is
/// `$XDG_CONFIG_HOME/git/ignore`.
#[test]
fn a_neighbours_xdg_ignore_file_does_not_hide_a_fixtures_files() {
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(neighbour.path().join("git")).unwrap();
    std::fs::write(neighbour.path().join("git/ignore"), "secret.txt\n").unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_in(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("secret.txt"), "x").unwrap();
    {
        let _env = bind_env(&serial).set("XDG_CONFIG_HOME", neighbour.path());
        git_in(repo.path(), &["add", "."]);
    }
    // No index at all means `add .` staged nothing: the ignore file applied.
    let index = std::fs::read(repo.path().join(".git/index")).unwrap_or_default();
    assert!(
        index.windows(10).any(|window| window == b"secret.txt"),
        "git_in honoured the neighbour's XDG ignore file"
    );
}

/// The `HOME` pin, proved through a hook: git runs it with the environment it
/// was given, and nothing about a `HOME` rebind changes git's own config reads
/// once `GIT_CONFIG_GLOBAL` is named, so the lane's own test cannot see this
/// half of the pin.
#[cfg(unix)]
#[test]
fn a_neighbours_home_does_not_reach_a_fixtures_hook() {
    use std::os::unix::fs::PermissionsExt;
    let serial = env_serial();
    let neighbour = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git_in(repo.path(), &["init", "-q"]);
    let seen = repo.path().join("seen-home");
    let hook = repo.path().join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nprintf %s \"$HOME\" > '{}'\n", seen.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    {
        let _env = bind_env(&serial).set("HOME", neighbour.path());
        git_in(repo.path(), &["commit", "-q", "--allow-empty", "-m", "x"]);
    }
    assert_eq!(
        std::fs::read_to_string(&seen).expect("the pre-commit hook did not run"),
        hermetic::home().to_string_lossy(),
        "the fixture's hook saw a rebound HOME"
    );
}

/// The fix is the class only if no other library test helper spawns git on the
/// inherited environment. The lane named eight `git_in` copies and left them; a
/// scan of the library's own sources (this crate runs one process, so every
/// helper here shares the environment a neighbour rebinds) finds every helper
/// of that shape. Production git wrappers are excluded by name.
#[test]
fn no_library_test_helper_spawns_git_on_the_inherited_environment() {
    const HELPERS: &[&str] = &[
        "git",
        "git_in",
        "git_ok",
        "git_out",
        "git_text",
        "run_git",
        "git_output",
    ];
    const PRODUCTION: &[&str] = &[
        "engine/git_cli.rs",
        "engine/ref_cache.rs",
        "test_support.rs",
    ];
    fn visit(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    visit(&src, &mut files);
    let mut unpinned = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if PRODUCTION.contains(&relative.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let defines = text.lines().any(|line| {
            let line = line
                .trim_start()
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            line.strip_prefix("fn ").is_some_and(|rest| {
                HELPERS.iter().any(|name| {
                    rest.strip_prefix(name)
                        .is_some_and(|after| after.starts_with('(') || after.starts_with('<'))
                })
            })
        });
        // `engine/git_reader.rs` pins by hand with `pinned_git_config`; that
        // counts as pinned here, though it is the copy `pin_git` should replace.
        let pinned = text.contains("pin_git") || text.contains("pinned_git_config");
        if defines && text.contains("Command::new(\"git\")") && !pinned {
            unpinned.push(relative);
        }
    }
    assert!(
        unpinned.is_empty(),
        "{} library files define a git helper that spawns git without `hermetic::pin_git`: {unpinned:#?}",
        unpinned.len()
    );
}

/// What `engine/deadbranch.rs`'s test fixture `BackupStore { dir, _env }` does
/// on drop, with a probe in the directory's place: Rust drops fields in
/// declaration order, so the directory is gone while the variable still names
/// it. Anything that reads the variable without the environment lock sees a
/// path that does not exist.
mod drop_order {
    use super::*;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "GITPULSE_BACKUPS_DIR";
    type Window = Arc<Mutex<Option<(bool, bool)>>>;

    /// A directory that records, once it is deleted, whether the variable still
    /// names it.
    struct ProbedDir {
        dir: Option<tempfile::TempDir>,
        window: Window,
    }

    impl Drop for ProbedDir {
        fn drop(&mut self) {
            let dir = self.dir.take().unwrap();
            let path = dir.path().to_path_buf();
            drop(dir);
            let names_it = std::env::var_os(KEY).is_some_and(|value| value == path.as_os_str());
            *self.window.lock().unwrap() = Some((names_it, path.exists()));
        }
    }

    fn probed(window: &Window) -> (ProbedDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (
            ProbedDir {
                dir: Some(dir),
                window: window.clone(),
            },
            path,
        )
    }

    /// Field order as deadbranch.rs has it.
    struct DirFirst {
        _dir: ProbedDir,
        _env: EnvBinding<'static>,
    }

    /// The order that restores the variable before the directory goes.
    struct EnvFirst {
        _env: EnvBinding<'static>,
        _dir: ProbedDir,
    }

    #[test]
    fn a_fixture_that_declares_its_directory_first_leaves_the_variable_naming_a_deleted_directory()
    {
        let window: Window = Arc::default();
        {
            let (dir, path) = probed(&window);
            let env = super::super::env::bind_env_owned(env_serial()).set(KEY, &path);
            let _store = DirFirst {
                _dir: dir,
                _env: env,
            };
        }
        assert_eq!(
            *window.lock().unwrap(),
            Some((true, false)),
            "expected the variable to still name the deleted directory (the footgun)"
        );
    }

    #[test]
    fn declaring_the_binding_first_restores_the_variable_before_the_directory_goes() {
        let window: Window = Arc::default();
        {
            let (dir, path) = probed(&window);
            let env = super::super::env::bind_env_owned(env_serial()).set(KEY, &path);
            let _store = EnvFirst {
                _env: env,
                _dir: dir,
            };
        }
        assert_eq!(*window.lock().unwrap(), Some((false, false)));
    }
}
