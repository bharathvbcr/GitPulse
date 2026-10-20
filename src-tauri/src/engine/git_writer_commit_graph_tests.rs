//! The user-clicked commit-graph write (Owner Decision 3): what a click does,
//! what refuses it, what bounds it, and that nothing else can reach it.

use super::*;
use crate::storage::commit_graph::{inspect, CommitGraphState};
use crate::test_support::{git_in, git_repo};

fn states(repo: &Path) -> CommitGraphState {
    let (private, common) = crate::repository_trust::git_directories(repo).unwrap();
    inspect(repo, &private, &common).state
}

fn commits(repo: &Path, count: usize) {
    for index in 0..count {
        std::fs::write(repo.join("app.txt"), format!("{index}\n")).unwrap();
        git_in(repo, &["add", "app.txt"]);
        git_in(repo, &["commit", "-q", "-m", &format!("c{index}")]);
    }
}

/// A repository with no commit-graph and `count` commits touching 50 paths,
/// built by `fast-import` so a graph write has real work to do.
fn large_history(count: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = git_repo();
    let mut stream = String::new();
    for index in 0..count {
        let body = format!("{index}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@example.test> {} +0000\ndata 3\nc{}\n",
            1_700_000_000 + index,
            index % 10
        ));
        stream.push_str(&format!(
            "M 644 inline d{}/f{}.txt\ndata {}\n{body}\n",
            index % 7,
            index % 50,
            body.len()
        ));
    }
    git_with_stdin(dir.path(), &["fast-import", "--quiet"], stream.as_bytes())
        .expect("fast-import builds the history");
    git_in(dir.path(), &["reset", "-q", "--hard", "main"]);
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn graph_files(repo: &Path) -> Vec<String> {
    let info = repo.join(".git/objects/info");
    let mut found: Vec<String> = std::fs::read_dir(&info)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("commit-graph"))
        .collect();
    found.sort();
    found
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}

/// A click as the command makes it: the gate promotes the calling thread to a
/// user action before the writer spawns, so a busy host defers refresh traffic,
/// not the person's write. Marked per call because the mark is thread-local.
fn click(repo: &str, confirmed: &[String]) -> Result<CommitGraphWriteOutcome, String> {
    crate::engine::git_cli::mark_user_action(repo);
    GitWriter::commit_graph_write(repo, confirmed)
}

#[test]
fn the_argv_is_reachable_and_changed_paths_and_never_touches_config() {
    let argv = GitWriter::commit_graph_write_argv();
    assert_eq!(
        argv,
        [
            "git",
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths"
        ]
    );
    assert!(!argv
        .iter()
        .any(|part| part == "config" || part == "--split"));
}

#[test]
fn a_click_on_a_repository_without_a_graph_writes_one_and_the_detector_agrees() {
    let dir = git_repo();
    commits(dir.path(), 3);
    let repo = dir.path().canonicalize().unwrap();
    assert!(
        graph_files(&repo).is_empty(),
        "fixture starts without a graph"
    );
    assert_eq!(states(&repo), CommitGraphState::Missing);
    let config_before = std::fs::read(repo.join(".git/config")).unwrap();

    let plan = GitWriter::commit_graph_write_plan(&text(&repo)).expect("a write is offered");
    assert_eq!(plan.state, CommitGraphState::Missing);
    assert_eq!(plan.argv, GitWriter::commit_graph_write_argv());
    assert_eq!(
        PathBuf::from(&plan.common_dir),
        crate::repository_trust::git_directories(&repo).unwrap().1
    );

    let outcome = click(&text(&repo), &plan.argv).expect("write");
    assert!(!outcome.cancelled && !outcome.joined);
    assert_eq!(outcome.report.state, CommitGraphState::Present);
    assert_eq!(graph_files(&repo), ["commit-graph"]);
    assert_eq!(states(&repo), CommitGraphState::Present);
    assert_eq!(
        std::fs::read(repo.join(".git/config")).unwrap(),
        config_before,
        "a graph write must not write git config"
    );
    // git itself agrees the graph is sound.
    git_in(&repo, &["commit-graph", "verify"]);

    // Hidden when fresh: no plan, no second write.
    let refused = GitWriter::commit_graph_write_plan(&text(&repo)).expect_err("fresh graph");
    assert!(refused.contains("Present"), "{refused}");
}

#[test]
fn a_stale_graph_is_offered_a_refresh() {
    let dir = git_repo();
    commits(dir.path(), 2);
    let repo = dir.path().canonicalize().unwrap();
    click(&text(&repo), &GitWriter::commit_graph_write_argv()).unwrap();
    commits(&repo, 1);
    assert_eq!(states(&repo), CommitGraphState::Stale);
    let plan = GitWriter::commit_graph_write_plan(&text(&repo)).expect("stale offers a write");
    click(&text(&repo), &plan.argv).unwrap();
    assert_eq!(states(&repo), CommitGraphState::Present);
}

#[test]
fn an_untrusted_repository_is_refused_before_anything_runs() {
    let dir = git_repo();
    commits(dir.path(), 1);
    let repo = dir.path().canonicalize().unwrap();
    crate::repository_trust::revoke(&text(&repo)).expect("revoke the fixture's trust");
    let argv = GitWriter::commit_graph_write_argv();
    for refused in [
        GitWriter::commit_graph_write_plan(&text(&repo)).map(|_| ()),
        click(&text(&repo), &argv).map(|_| ()),
        GitWriter::commit_graph_write_cancel(&text(&repo)).map(|_| ()),
    ] {
        let error = refused.expect_err("untrusted");
        assert!(error.contains(crate::repository_trust::REQUIRED), "{error}");
    }
    assert!(graph_files(&repo).is_empty());
}

#[test]
fn a_command_other_than_the_previewed_one_does_not_run() {
    let dir = git_repo();
    commits(dir.path(), 1);
    let repo = dir.path().canonicalize().unwrap();
    for confirmed in [
        vec![],
        vec!["git".to_string(), "gc".to_string()],
        ["git", "commit-graph", "write", "--reachable"]
            .map(String::from)
            .to_vec(),
        [
            "git",
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ]
        .map(String::from)
        .to_vec(),
    ] {
        let error = click(&text(&repo), &confirmed).expect_err("mismatch");
        assert!(error.contains("nothing ran"), "{error}");
    }
    assert!(graph_files(&repo).is_empty());
}

#[test]
fn a_repository_with_no_history_is_not_offered_a_write() {
    let dir = git_repo();
    let error = GitWriter::commit_graph_write_plan(&text(&dir.path().canonicalize().unwrap()))
        .expect_err("nothing to accelerate");
    assert!(error.contains("NoHistory"), "{error}");
}

#[test]
fn concurrent_clicks_in_one_common_dir_are_one_write() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    let leader = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    // The leader is in flight once its cancel flag is registered; the second
    // click must then wait for it rather than start a second git.
    let deadline = std::time::Instant::now()
        + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(20));
    while !commit_graph_cancels().lock().unwrap().contains_key(&common) {
        assert!(
            std::time::Instant::now() < deadline,
            "the write never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let follower = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    let (leader, follower) = (leader.join().unwrap(), follower.join().unwrap());
    let (leader, follower) = (leader.expect("leader"), follower.expect("follower"));
    assert!(!leader.joined, "the first click runs the write");
    assert!(follower.joined, "the second click joins it");
    assert_eq!(leader.report.state, CommitGraphState::Present);
    assert_eq!(follower.report.state, CommitGraphState::Present);
    assert_eq!(graph_files(&repo), ["commit-graph"]);
}

#[test]
fn a_timeout_kills_the_write_and_leaves_no_lock_or_graph() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    crate::engine::git_cli::mark_user_action(&text(&repo));
    let error = GitWriter::commit_graph_write_within(
        &text(&repo),
        &argv,
        std::time::Duration::from_millis(5),
    )
    .expect_err("a 5 ms budget cannot write 30k commits");
    assert!(crate::engine::git_cli::is_timed_out(&error), "{error}");
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    assert!(commit_graph_locks(&common).is_empty(), "a lock was left");
    assert!(graph_files(&repo).is_empty(), "a partial graph was kept");
    assert!(!commit_graph_cancels().lock().unwrap().contains_key(&common));
    // And it is retryable: the next click, with a real budget, writes.
    click(&text(&repo), &argv).expect("retry");
    assert_eq!(states(&repo), CommitGraphState::Present);
}

#[test]
fn a_cancel_stops_the_write_and_leaves_no_lock_or_graph() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    assert_eq!(
        GitWriter::commit_graph_write_cancel(&text(&repo)),
        Ok(false),
        "nothing is running yet"
    );
    let running = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    let deadline = std::time::Instant::now()
        + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(20));
    while !GitWriter::commit_graph_write_cancel(&text(&repo)).unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "the write never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let outcome = running.join().unwrap().expect("a cancel is an outcome");
    assert!(outcome.cancelled);
    assert!(commit_graph_locks(&common).is_empty(), "a lock was left");
    assert!(
        graph_files(&repo).is_empty(),
        "a cancelled write kept a graph"
    );
    assert!(!commit_graph_cancels().lock().unwrap().contains_key(&common));
    click(&text(&repo), &argv).expect("a click after a cancel writes");
    assert_eq!(states(&repo), CommitGraphState::Present);
}

#[test]
fn a_lock_held_by_another_git_is_never_removed_and_blocks_the_write() {
    let dir = git_repo();
    commits(dir.path(), 2);
    let repo = dir.path().canonicalize().unwrap();
    let lock = repo.join(".git/objects/info/commit-graph.lock");
    std::fs::write(&lock, b"").unwrap();
    let error =
        click(&text(&repo), &GitWriter::commit_graph_write_argv()).expect_err("a held lock");
    assert!(error.contains("Another git process holds"), "{error}");
    // The message names the lock, how old it is and who may delete it.
    assert!(error.contains(&lock.display().to_string()), "{error}");
    assert!(error.contains("last modified"), "{error}");
    assert!(error.contains("delete that file yourself"), "{error}");
    assert!(lock.exists(), "someone else's lock was removed");
}

#[test]
fn an_old_git_is_refused_by_the_plan_and_the_writer_and_nothing_runs() {
    let dir = git_repo();
    commits(dir.path(), 2);
    let repo = text(&dir.path().canonicalize().unwrap());
    let plan = GitWriter::commit_graph_write_plan_with(&repo, false).expect_err("old git");
    assert!(plan.contains("2.27"), "{plan}");
    let written =
        GitWriter::commit_graph_write_checked(&repo, &GitWriter::commit_graph_write_argv(), false)
            .expect_err("old git");
    assert!(written.contains("2.27"), "{written}");
    assert!(graph_files(&dir.path().canonicalize().unwrap()).is_empty());
    // The same repository is offered the write by a git that can run it.
    GitWriter::commit_graph_write_plan_with(&repo, true).expect("a capable git");
}

#[test]
fn the_panel_report_hides_the_write_for_an_old_git_but_not_for_a_new_one() {
    use crate::storage::commit_graph::{gated_on_git, CommitGraphReport};
    let missing = CommitGraphReport {
        state: CommitGraphState::Missing,
        ..CommitGraphReport::default()
    };
    let hidden = gated_on_git(missing.clone(), false, "2.27");
    assert!(!hidden.state.offers_write());
    assert!(hidden.note.unwrap().contains("2.27"));
    assert!(gated_on_git(missing, true, "2.27").state.offers_write());
    let present = CommitGraphReport {
        state: CommitGraphState::Present,
        ..CommitGraphReport::default()
    };
    assert_eq!(
        gated_on_git(present, false, "2.27").state,
        CommitGraphState::Present
    );
}

#[test]
fn a_stop_sent_before_the_write_registers_is_honoured_by_it() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    assert_eq!(
        GitWriter::commit_graph_write_cancel(&text(&repo)),
        Ok(false),
        "nothing is registered yet"
    );
    let outcome = click(&text(&repo), &argv).expect("a stopped write is an outcome");
    assert!(outcome.cancelled, "the stop that came first was dropped");
    assert!(graph_files(&repo).is_empty());
    // The intent was consumed: the next click writes.
    let again = click(&text(&repo), &argv).expect("write");
    assert!(!again.cancelled);
    assert_eq!(states(&repo), CommitGraphState::Present);
}

#[test]
fn an_unclaimed_stop_expires_and_does_not_stop_a_later_write() {
    let dir = git_repo();
    commits(dir.path(), 2);
    let repo = dir.path().canonicalize().unwrap();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    commit_graph_stop_intents().lock().unwrap().insert(
        common,
        std::time::Instant::now() - COMMIT_GRAPH_STOP_INTENT_LIFETIME * 2,
    );
    let outcome = click(&text(&repo), &GitWriter::commit_graph_write_argv()).expect("write");
    assert!(!outcome.cancelled);
    assert_eq!(states(&repo), CommitGraphState::Present);
}

#[test]
fn a_running_write_is_reported_and_no_second_write_is_planned() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    assert!(!commit_graph_write_running(&common));
    let running = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    let deadline = std::time::Instant::now()
        + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(20));
    while !commit_graph_write_running(&common) {
        assert!(std::time::Instant::now() < deadline, "never started");
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    let refused = GitWriter::commit_graph_write_plan(&text(&repo)).expect_err("already running");
    assert!(refused.contains("already running"), "{refused}");
    assert_eq!(GitWriter::commit_graph_write_cancel(&text(&repo)), Ok(true));
    let _ = running.join().unwrap();
    assert!(!commit_graph_write_running(&common));
}

#[test]
fn a_partial_clone_is_not_offered_a_write_by_the_detector_or_the_plan() {
    let source = git_repo();
    commits(source.path(), 3);
    git_in(source.path(), &["config", "uploadpack.allowFilter", "true"]);
    let holder = tempfile::TempDir::new().unwrap();
    let clone = holder.path().join("clone");
    git_in(
        holder.path(),
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", source.path().display()),
            "clone",
        ],
    );
    crate::test_support::trust_repo(&clone);
    let clone = clone.canonicalize().unwrap();
    let (private, common) = crate::repository_trust::git_directories(&clone).unwrap();
    let report = inspect(&clone, &private, &common);
    assert_eq!(report.state, CommitGraphState::Unknown, "{report:?}");
    assert!(report.note.unwrap().contains("partial clone"));
    let refused = GitWriter::commit_graph_write_plan(&text(&clone)).expect_err("partial");
    assert!(refused.contains("partial clone"), "{refused}");
}

/// Every non-test source file that names the writer, the command or the
/// builder. A new caller (ingest, a timer, an MCP tool, a hook) fails here.
#[test]
fn no_path_but_the_storage_panel_command_reaches_the_writer() {
    fn visit(dir: &Path, found: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, found);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs") || name.ends_with("tests.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            let production = source
                .split("\n#[cfg(test)]")
                .next()
                .unwrap_or_default()
                .to_string();
            for needle in [
                "commit_graph_write(",
                "commit_graph_write_within(",
                "COMMIT_GRAPH_WRITE_ARGV",
                "cmd_commit_graph_write",
            ] {
                if production.contains(needle) {
                    found.push((path.display().to_string(), needle.to_string()));
                }
            }
        }
    }
    let mut found = Vec::new();
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    let mut who: Vec<(String, String)> = found
        .into_iter()
        .map(|(path, needle)| (path.rsplit("src/").next().unwrap().to_string(), needle))
        .collect();
    who.sort();
    who.dedup();
    let allowed = [
        // the writer, its argv, its builder and the docs that name the command
        ("engine/git_writer.rs", "COMMIT_GRAPH_WRITE_ARGV"),
        ("engine/git_writer.rs", "commit_graph_write("),
        ("engine/git_writer.rs", "commit_graph_write_within("),
        ("engine/git_writer.rs", "cmd_commit_graph_write"),
        // the one caller of the writer: the Storage panel's confirm click
        ("commands/analysis.rs", "cmd_commit_graph_write"),
        ("commands/analysis.rs", "commit_graph_write("),
        // registration of the command with the webview
        ("lib.rs", "cmd_commit_graph_write"),
    ];
    let unexpected: Vec<_> = who
        .iter()
        .filter(|entry| !allowed.contains(&(entry.0.as_str(), entry.1.as_str())))
        .collect();
    assert!(
        unexpected.is_empty(),
        "something other than cmd_commit_graph_write reaches the commit-graph writer: {unexpected:#?}"
    );
    // The writer has exactly one production caller outside its own file.
    assert!(who.contains(&(
        "commands/analysis.rs".to_string(),
        "commit_graph_write(".to_string()
    )));
}
