//! Deterministic fixtures, built once per run in a temp directory.
//!
//! Every fixture is a function of its arguments alone: file names, contents,
//! commit messages and identities carry no clock or random value, so two runs
//! (and two machines) measure the same bytes. Git itself is the only source of
//! variation, and its commit ids are not read by any bench.

use std::path::{Path, PathBuf};

use gitpulse_lib::procguard::LockedSpawn;

/// Runs `git` in `cwd` with a fixed identity and asserts success. Returns
/// stdout.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Bench")
        .env("GIT_AUTHOR_EMAIL", "bench@example.com")
        .env("GIT_COMMITTER_NAME", "Bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output_locked()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Admits the fixture through the real trust boundary, into the hermetic
/// store. A session grant for a repository the bench itself just created.
pub fn trust_repo(repo: &Path) {
    let path = repo.to_str().expect("fixture path is UTF-8");
    let preview = gitpulse_lib::repository_trust::inspect(path).expect("inspect fixture");
    gitpulse_lib::repository_trust::grant(path, &preview.identity, false).expect("approve fixture");
}

/// The shape of a generated repository.
#[derive(Clone, Copy)]
pub struct Spec {
    /// Tracked files in the first commit, spread over `dirs` directories.
    pub files: usize,
    pub dirs: usize,
    /// Commits after the first, each touching `touch` files.
    pub extra_commits: usize,
    pub touch: usize,
    /// Linked worktrees besides the main one; each gets one dirty file.
    pub linked_worktrees: usize,
}

/// A built repository: the main checkout and its linked worktrees.
pub struct Repo {
    pub main: PathBuf,
    pub linked: Vec<PathBuf>,
}

impl Repo {
    pub fn path(&self) -> &str {
        self.main.to_str().expect("fixture path is UTF-8")
    }

    pub fn file(&self, index: usize, spec: &Spec) -> PathBuf {
        self.main.join(rel_path(index, spec.dirs))
    }
}

fn rel_path(index: usize, dirs: usize) -> String {
    let ext = ["rs", "ts", "go", "md", "json", "py", "css", "html"][index % 8];
    format!("d{:03}/f{index:06}.{ext}", index % dirs.max(1))
}

fn content(index: usize, revision: usize) -> String {
    // A few dozen short lines: realistic enough for numstat and language
    // statistics to have something to count, small enough for 10k files.
    let mut text = String::with_capacity(512);
    for line in 0..24 {
        text.push_str(&format!(
            "// file {index} rev {revision} line {line}: let value_{line} = {};\n",
            index * 31 + line * 7 + revision
        ));
    }
    text
}

/// Builds the repository described by `spec` under `root/main`.
pub fn build_repo(root: &Path, spec: &Spec) -> Repo {
    let main = root.join("main");
    std::fs::create_dir_all(&main).expect("create fixture root");
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.name", "Bench"]);
    git(&main, &["config", "user.email", "bench@example.com"]);
    git(&main, &["config", "commit.gpgsign", "false"]);
    // GitPulse's own state (ledger, code-graph store, WAL/SHM files that come and
    // go while a command runs) lives under these and would otherwise be listed
    // as untracked files by `git ls-files --others`, then vanish before they
    // are read: a language scan would report itself incomplete for a reason
    // that has nothing to do with the work being measured.
    std::fs::write(
        main.join(".git/info/exclude"),
        ".devcouncil/\n.devmap/\n.gitpulse/\n",
    )
    .expect("write info/exclude");
    for index in 0..spec.files {
        let path = main.join(rel_path(index, spec.dirs));
        std::fs::create_dir_all(path.parent().expect("file has a parent")).expect("mkdir");
        std::fs::write(&path, content(index, 0)).expect("write fixture file");
    }
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "chore: initial tree"]);
    for commit in 1..=spec.extra_commits {
        for k in 0..spec.touch {
            let index = (commit * 13 + k * 101) % spec.files.max(1);
            std::fs::write(
                main.join(rel_path(index, spec.dirs)),
                content(index, commit),
            )
            .expect("rewrite fixture file");
        }
        git(&main, &["add", "-A"]);
        git(
            &main,
            &["commit", "-q", "-m", &format!("feat: change {commit}")],
        );
    }
    let mut linked = Vec::new();
    for n in 0..spec.linked_worktrees {
        let path = root.join(format!("wt{n:02}"));
        let branch = format!("wt/{n:02}");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                path.to_str().expect("utf-8"),
            ],
        );
        let index = (n * 7) % spec.files.max(1);
        std::fs::write(
            path.join(rel_path(index, spec.dirs)),
            content(index, 9_000 + n),
        )
        .expect("dirty a linked worktree");
        linked.push(path);
    }
    trust_repo(&main);
    Repo { main, linked }
}

/// Leaves `count` tracked files modified in the main checkout.
pub fn dirty_files(repo: &Repo, spec: &Spec, count: usize) {
    for k in 0..count {
        let index = (k * 3) % spec.files.max(1);
        std::fs::write(repo.file(index, spec), content(index, 7_000 + k)).expect("dirty a file");
    }
}

/// Puts every file back as committed.
pub fn clean(repo: &Repo) {
    git(&repo.main, &["checkout", "-q", "--", "."]);
}

/// Runs `body` with every `git` process it starts logged by git's own trace2,
/// and returns how many there were beside its result.
///
/// `GIT_TRACE2*` is stripped from the environment of every child GitPulse
/// starts (a trace would flood the few bytes of stderr it keeps), but git also
/// reads `trace2.eventTarget` from the *global* config, which GitPulse does not
/// touch. The hermetic `HOME` makes that file ours alone. Counting happens in
/// its own pass: the log append adds a little to every process, so a timed
/// sample never runs with it on.
pub fn count_git_processes<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let home = PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME"));
    let config = home.join(".gitconfig");
    let log = home.join("trace2-events.json");
    let _ = std::fs::remove_file(&log);
    std::fs::write(
        &config,
        format!("[trace2]\n\teventTarget = {}\n", log.display()),
    )
    .expect("write the counting gitconfig");
    let result = body();
    let _ = std::fs::remove_file(&config);
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    let processes = text
        .lines()
        .filter(|l| l.contains("\"event\":\"start\""))
        .count() as u64;
    (result, processes)
}
