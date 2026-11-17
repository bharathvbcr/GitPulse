//! Build leases at the real executable boundary: `gitpulse-hook lease --
//! cargo build`, several at once, in processes of their own, against a fake
//! `cargo` that logs when it starts and stops.
//!
//! The unit tests in `src/sysload/lease_tests.rs` and
//! `src/hooks/lease_wrapper_tests.rs` prove the order, the counts and the
//! messages over facts handed to them. This proves what they cannot: that a
//! lock taken in one process is seen by the next, that it outlives the
//! wrapper (which becomes the build) and not the build, that a killed waiter
//! frees its place, and that a lease that could not be taken says so.
//!
//! Nothing here times a build. Where a test must show that something did *not*
//! happen (a third build starting) it waits a fixed settle and says so; every
//! positive wait is a poll to a generous deadline.

#![cfg(unix)]

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::sysload::{
    Binding, Clock, PublishedVerdict, Reading, Snapshot, SnapshotWriter, Tier, WriterIdentity,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");

/// A cargo that says when it started and stopped, and holds its slot until the
/// test releases it.
const FAKE_CARGO: &str = r#"#!/bin/sh
# A test may start the wrapper under a zero soft file-size limit (a disk that
# takes no more bytes); the build itself writes its log, so it lifts that.
ulimit -S -f unlimited 2>/dev/null
echo "start $TAG verb=$1 held=$GITPULSE_LEASE_HELD shim=$CARGO_SLOT_HELD jobs=$CARGO_BUILD_JOBS inc=$CARGO_INCREMENTAL" >> "$LOG"
if [ -n "$SIGPIPE_FILE" ]; then
  perl -e 'print defined($SIG{PIPE}) ? $SIG{PIPE} : "DEFAULT"' > "$SIGPIPE_FILE"
fi
if [ -n "$DAEMON_PID_FILE" ]; then
  sleep 120 &
  echo $! > "$DAEMON_PID_FILE"
fi
if [ -z "$NO_WAIT" ]; then
  while [ ! -e "$RELEASE" ]; do sleep 0.1; done
fi
echo "stop $TAG" >> "$LOG"
"#;

fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/GitPulse")
    } else {
        home.join(".config").join("gitpulse")
    }
}

struct World {
    _root: tempfile::TempDir,
    home: PathBuf,
    tmp: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    release: PathBuf,
    tools: PathBuf,
    next: usize,
    running: Vec<Child>,
}

impl World {
    fn new(leases: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("bl")
            .tempdir_in("/tmp")
            .unwrap();
        let base = root.path().canonicalize().unwrap();
        let (home, tmp, bin) = (base.join("home"), base.join("tmp"), base.join("bin"));
        for dir in [&home, &tmp, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(config_dir(&home)).unwrap();
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, FAKE_CARGO).unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tools = base.join("tools.json");
        std::fs::write(
            &tools,
            format!(r#"{{"version":1,"host_load_leases":{leases}}}"#),
        )
        .unwrap();
        Self {
            log: base.join("log"),
            release: base.join("release"),
            _root: root,
            home,
            tmp,
            bin,
            tools,
            next: 0,
            running: Vec::new(),
        }
    }

    fn lease_dir(&self) -> PathBuf {
        config_dir(&self.home).join("leases")
    }

    fn queued(&self) -> usize {
        std::fs::read_dir(self.lease_dir().join("queue"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    /// The wrapper, started as `gitpulse-hook lease -- cargo <args>`, with its
    /// own session, tagged `tag` in the log.
    fn command(&mut self, tag: &str, args: &[&str]) -> (Command, PathBuf) {
        self.next += 1;
        let stderr_path = self.log.with_extension(format!("err-{}", self.next));
        let mut command = Command::new(HOOK);
        command
            .args(["lease", "--", "cargo"])
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("GITPULSE_TOOL_CONFIG", &self.tools)
            .env(gitpulse_lib::logging::LOG_DIR_ENV, self.home.join("logs"))
            .env("GITPULSE_LEASE_POLL_MS", "50")
            .env("GITPULSE_LEASE_ANNOUNCE_MS", "200")
            .env("GITPULSE_TERMINAL_SESSION", format!("term-{tag}"))
            .env("LOG", &self.log)
            .env("RELEASE", &self.release)
            .env("TAG", tag)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()));
        (command, stderr_path)
    }

    fn start(&mut self, tag: &str) -> PathBuf {
        let (mut command, stderr) = self.command(tag, &["build"]);
        self.running
            .push(command.spawn_locked().expect("spawn gitpulse-hook"));
        stderr
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn started(&self) -> Vec<String> {
        self.lines()
            .iter()
            .filter_map(|l| l.strip_prefix("start "))
            .map(|l| l.split(' ').next().unwrap().to_string())
            .collect()
    }

    fn release(&self) {
        std::fs::write(&self.release, "go").unwrap();
    }

    fn finish(&mut self) -> Vec<Option<i32>> {
        self.running
            .drain(..)
            .map(|mut child| child.wait().unwrap().code())
            .collect()
    }

    /// The most builds that were running at once, read off the log.
    fn peak(&self) -> usize {
        let (mut now, mut peak) = (0usize, 0usize);
        for line in self.lines() {
            if line.starts_with("start ") {
                now += 1;
                peak = peak.max(now);
            } else if line.starts_with("stop ") {
                now -= 1;
            }
        }
        peak
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Children this test started, by pid: nothing else is touched.
        for child in &mut self.running {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(1500));
}

#[test]
fn at_most_n_run_at_once() {
    let mut world = World::new(r#"{"slots":2}"#);
    for tag in ["a", "b", "c", "d", "e"] {
        world.start(tag);
    }
    wait_for("two builds to start", || world.started().len() == 2);
    settle();
    assert_eq!(
        world.started().len(),
        2,
        "a third build started while two held the slots: {:?}",
        world.lines()
    );
    assert_eq!(world.queued(), 3, "three wait their turn");
    world.release();
    assert!(world.finish().iter().all(|code| *code == Some(0)));
    assert_eq!(
        world.started().len(),
        5,
        "every build ran: {:?}",
        world.lines()
    );
    assert_eq!(world.peak(), 2, "{:?}", world.lines());
    assert_eq!(world.queued(), 0);
}

#[test]
fn the_build_runs_with_its_slot_marked_and_its_jobs_split() {
    let mut world = World::new(r#"{"slots":2,"incremental_off":true}"#);
    world.start("a");
    wait_for("the build to start", || !world.started().is_empty());
    let line = world.lines().remove(0);
    assert!(line.contains("verb=build"), "{line}");
    assert!(
        line.contains("held=1"),
        "the slot number is passed down: {line}"
    );
    assert!(
        line.contains("shim=gitpulse-lease"),
        "the shell shim passes through: {line}"
    );
    assert!(line.contains("inc=0"), "incremental_off is applied: {line}");
    let jobs: usize = line
        .split("jobs=")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|jobs| jobs.parse().ok())
        .expect("CARGO_BUILD_JOBS is set");
    assert!(jobs >= 2, "{line}");
    world.release();
    world.finish();

    let mut world = World::new(r#"{"slots":2}"#);
    world.start("a");
    wait_for("the build to start", || !world.started().is_empty());
    assert!(
        world.lines()[0].ends_with("inc="),
        "incremental stays on unless asked: {}",
        world.lines()[0]
    );
    world.release();
    world.finish();
}

#[test]
fn fifo_within_a_tier() {
    let mut world = World::new(r#"{"slots":1}"#);
    world.start("a");
    wait_for("a to hold the slot", || world.started() == ["a"]);
    for (n, tag) in ["b", "c", "d"].into_iter().enumerate() {
        world.start(tag);
        // One at a time, so the enqueue order is the spawn order.
        wait_for(&format!("{tag} to queue"), || world.queued() == n + 1);
    }
    world.release();
    assert!(world.finish().iter().all(|code| *code == Some(0)));
    assert_eq!(world.started(), ["a", "b", "c", "d"], "{:?}", world.lines());
}

#[test]
fn a_killed_waiter_frees_its_place() {
    let mut world = World::new(r#"{"slots":1}"#);
    world.start("a");
    wait_for("a to hold the slot", || world.started() == ["a"]);
    world.start("b");
    wait_for("b to queue", || world.queued() == 1);
    world.start("c");
    wait_for("c to queue", || world.queued() == 2);
    // b is killed, not asked: it cannot clean up its ticket.
    let b = world.running.remove(1);
    let mut b = b;
    b.kill().unwrap();
    b.wait().unwrap();
    world.release();
    assert!(world.finish().iter().all(|code| *code == Some(0)));
    assert_eq!(world.started(), ["a", "c"], "{:?}", world.lines());
    assert_eq!(world.queued(), 0, "the dead ticket was removed by a reader");
}

#[test]
fn the_lock_dies_with_cargo_not_its_daemon_child() {
    let mut world = World::new(r#"{"slots":1}"#);
    let pidfile = world.tmp.join("daemon.pid");
    let (mut first, _) = world.command("a", &["build"]);
    first.env("DAEMON_PID_FILE", &pidfile).env("NO_WAIT", "1");
    let mut first = first.spawn_locked().unwrap();
    // The daemon holds an inherited descriptor to the slot file after cargo
    // has exited. A flock would be held by it; a record lock is not.
    assert_eq!(first.wait().unwrap().code(), Some(0));
    wait_for("the daemon's pid", || pidfile.exists());
    let daemon: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(daemon > 1);
    let (mut second, _) = world.command("b", &["build"]);
    second.env("NO_WAIT", "1");
    let mut second = second.spawn_locked().unwrap();
    wait_for("the second build to start behind the daemon", || {
        world.started() == ["a", "b"]
    });
    assert_eq!(second.wait().unwrap().code(), Some(0));
    // The daemon was started by this test and is stopped by its pid.
    // SAFETY: a pid this test recorded from its own fake build.
    unsafe {
        libc::kill(daemon, libc::SIGKILL);
    }
}

#[test]
fn an_unusable_dir_runs_and_says_not_leased() {
    use std::os::unix::fs::PermissionsExt;
    let mut world = World::new(r#"{"slots":1}"#);
    // A lease directory other users could write: refused, not used.
    std::fs::create_dir_all(world.lease_dir()).unwrap();
    std::fs::set_permissions(world.lease_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let (mut command, stderr) = world.command("a", &["build"]);
    command.env("NO_WAIT", "1");
    let status = command.spawn_locked().unwrap().wait().unwrap();
    assert_eq!(status.code(), Some(0), "the build ran");
    assert_eq!(world.started(), ["a"]);
    let said = std::fs::read_to_string(stderr).unwrap();
    assert!(said.contains("did NOT lease this build"), "{said}");
    assert!(said.contains("not a private directory"), "{said}");
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let markers = world.tmp.join(format!("gitpulse-hook-advisories-{uid}"));
    let counted = std::fs::read_dir(&markers)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("leasemiss-"))
        .count();
    assert_eq!(counted, 1, "Health counts the unleased build");
}

#[test]
fn a_build_that_waits_past_the_limit_starts_and_says_it_has_no_slot() {
    let mut world = World::new(r#"{"slots":1,"wait_max_s":1,"wait_hard_s":30}"#);
    world.start("a");
    wait_for("a to hold the slot", || world.started() == ["a"]);
    let stderr = world.start("b");
    wait_for("b to start without a slot", || {
        world.started() == ["a", "b"]
    });
    let said = std::fs::read_to_string(stderr).unwrap();
    assert!(said.contains("WITHOUT one"), "{said}");
    assert!(
        said.contains("waiting for a build slot"),
        "it said where it was: {said}"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(world.lease_dir().join("slot-2.json")).unwrap())
            .unwrap();
    assert_eq!(record["overflow"], true, "{record}");
    world.release();
    world.finish();
}

#[test]
fn a_severe_verdict_holds_the_build_back_and_it_gives_up_without_building() {
    let mut world = World::new(r#"{"slots":2,"wait_max_s":1,"wait_hard_s":2}"#);
    let mut snapshot = Snapshot::empty(Clock::now(), WriterIdentity::default(), true);
    snapshot.verdict = Reading::Value(PublishedVerdict {
        tier: Tier::Severe,
        binding: Some(Binding::Swap),
        admit: false,
        shed_background: true,
        shed_reactive: true,
    });
    let mut writer = SnapshotWriter::elect(&config_dir(&world.home))
        .unwrap()
        .expect("the election is free in a fresh home");
    writer.publish(&snapshot).unwrap();
    let stderr = world.start("a");
    assert_eq!(world.finish(), [Some(75)]);
    assert!(world.started().is_empty(), "nothing was built");
    let said = std::fs::read_to_string(stderr).unwrap();
    assert!(said.contains("gave up"), "{said}");
    assert!(said.contains("not admitting"), "{said}");
}

#[test]
fn a_nested_call_and_a_verb_that_builds_nothing_never_wait() {
    let mut world = World::new(r#"{"slots":1}"#);
    world.start("a");
    wait_for("a to hold the only slot", || world.started() == ["a"]);
    for (tag, args, nested) in [
        ("run", vec!["run"], false),
        ("clean", vec!["clean"], false),
        ("version", vec!["--version"], false),
        ("fmt", vec!["fmt", "--check"], false),
        ("nested", vec!["build"], true),
    ] {
        let (mut command, _) = world.command(tag, &args);
        command.env("NO_WAIT", "1");
        if nested {
            command.env("GITPULSE_LEASE_HELD", "1");
        }
        let status = command.spawn_locked().unwrap().wait().unwrap();
        assert_eq!(status.code(), Some(0), "{tag}");
    }
    assert_eq!(
        world.started(),
        ["a", "run", "clean", "version", "fmt", "nested"],
        "each ran at once while the only slot was held: {:?}",
        world.lines()
    );
    assert_eq!(world.queued(), 0);
    world.release();
    world.finish();
}

#[test]
fn leases_switched_off_in_settings_run_every_build_at_once() {
    let mut world = World::new(r#"{"mode":"off","slots":1}"#);
    world.start("a");
    world.start("b");
    wait_for("both to start", || world.started().len() == 2);
    assert!(!world.lease_dir().exists(), "off creates no lease state");
    world.release();
    world.finish();
}

/// Rust's runtime ignores SIGPIPE, and an ignored signal survives `exec`: a
/// build the wrapper starts must have the default back, or `cargo build | head`
/// never ends the way it does from a shell.
#[test]
fn a_leased_build_does_not_inherit_an_ignored_sigpipe() {
    let mut world = World::new(r#"{"slots":2}"#);
    let seen = world.tmp.join("sigpipe");
    let (mut command, _stderr) = world.command("a", &["build"]);
    command.env("NO_WAIT", "1").env("SIGPIPE_FILE", &seen);
    let status = command.spawn_locked().unwrap().wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "DEFAULT",
        "the build must start with SIGPIPE at its default"
    );
}

/// Writes an executable at `path` by rename, so nothing runs it half written.
fn install_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension("tmp-script");
    std::fs::write(&tmp, body).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// `gitpulse-hook` as it was before build leases: `--version` names no
/// `lease`, and an argument it does not know (`build`) is answered with exit 0
/// and nothing done. Run as `cargo`, every build "passes" without running.
const STALE_HOOK: &str = "#!/bin/sh\n\
if [ \"$1\" = --version ]; then\n\
  printf 'gitpulse-hook 0.4.0\\nsubcommands: collision-guard, command-gate, session-brief\\n'\n\
fi\n\
exit 0\n";

/// The PATH value in `env`, from `launch_env`.
fn path_of(env: &[(&'static str, Option<std::ffi::OsString>)]) -> Option<std::ffi::OsString> {
    env.iter()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value.clone())
}

/// Runs `cargo build` the way an agent's shell does: by PATH lookup.
fn shell_build(world: &World, path: &std::ffi::OsStr, tag: &str) -> std::process::Output {
    Command::new("/bin/sh")
        .args(["-c", "cargo build"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &world.home)
        .env("TMPDIR", &world.tmp)
        .env("XDG_CONFIG_HOME", world.home.join(".config"))
        .env("GITPULSE_TOOL_CONFIG", &world.tools)
        .env(gitpulse_lib::logging::LOG_DIR_ENV, world.home.join("logs"))
        .env("LOG", &world.log)
        .env("RELEASE", &world.release)
        .env("TAG", tag)
        .env("NO_WAIT", "1")
        .stdin(Stdio::null())
        .output_locked()
        .expect("run a shell")
}

#[test]
fn a_stale_hook_behind_the_links_never_passes_a_build_it_did_not_run() {
    use gitpulse_lib::sysload::lease;
    use std::ffi::OsStr;
    let world = World::new(r#"{"slots":2}"#);
    let config = config_dir(&world.home);
    let app = world.home.join("App");
    std::fs::create_dir_all(&app).unwrap();
    let hook = app.join("gitpulse-hook");
    std::os::unix::fs::symlink(HOOK, &hook).unwrap();
    lease::ensure_links(&config, &hook).expect("a current hook is linked");
    let bin = lease::bin_dir(&config);
    let base = format!("{}:/usr/bin:/bin", world.bin.display());
    // The PATH an agent GitPulse launches is given, from the product.
    let routed = path_of(&lease::launch_env(
        Some(&config),
        Some(OsStr::new(&base)),
        true,
    ))
    .expect("the links work, so a launched session is routed through them");
    assert_eq!(std::env::split_paths(&routed).next().unwrap(), bin);

    // The app's gitpulse-hook is replaced by one from before leases (a
    // downgrade, or a tree that rebuilt the app and not the hook), and the app
    // starts again, which is when it makes its links.
    std::fs::remove_file(&hook).unwrap();
    install_script(&hook, STALE_HOOK);
    let relinked = lease::ensure_links(&config, &hook);

    // A session routed before the swap runs a build. Either the real cargo
    // runs, or the call fails loudly: never exit 0 with nothing built.
    let out = shell_build(&world, &routed, "stale");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        world.started() == ["stale"] || (!out.status.success() && !said.is_empty()),
        "a build through a stale gitpulse-hook exited {:?} having run nothing (stderr: {said:?}); ensure_links said {relinked:?}",
        out.status.code()
    );
    assert_eq!(out.status.code(), Some(0), "the real cargo ran: {said}");

    let refused = relinked.expect_err("a hook without lease mode is never linked");
    assert!(refused.contains("lease"), "{refused}");
    for name in ["cargo", "go"] {
        assert!(
            bin.join(name).symlink_metadata().is_err(),
            "the {name} link to a binary that is not a lease wrapper is removed"
        );
    }
    let problem = lease::links_problem(&bin, None).expect("the links do not work");
    assert!(problem.contains("does not exist"), "{problem}");
    assert!(
        path_of(&lease::launch_env(
            Some(&config),
            Some(OsStr::new(&base)),
            true
        ))
        .is_none(),
        "a new session is not routed through a folder with no wrapper in it"
    );
}

#[test]
fn a_link_whose_target_went_stale_is_reported_and_not_routed() {
    use gitpulse_lib::sysload::lease;
    use std::ffi::OsStr;
    let world = World::new(r#"{"slots":2}"#);
    let config = config_dir(&world.home);
    let bin = lease::bin_dir(&config);
    std::fs::create_dir_all(&bin).unwrap();
    // Links the app made earlier, to a binary that is now a stale hook, a
    // binary that prints something else, or one that never answers.
    let bodies = [
        ("stale", STALE_HOOK.to_string()),
        (
            "garbage",
            "#!/bin/sh\necho 'cargo 1.91.0 (not gitpulse)'\nexit 0\n".to_string(),
        ),
        ("hangs", "#!/bin/sh\nexec sleep 30\n".to_string()),
    ];
    for (kind, body) in bodies {
        let target = world.home.join(format!("hook-{kind}"));
        install_script(&target, &body);
        for name in ["cargo", "go"] {
            let link = bin.join(name);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link).unwrap();
        }
        let started = Instant::now();
        let problem = lease::links_problem(&bin, None);
        assert!(
            started.elapsed() < hermetic_env::coverage_relaxed(Duration::from_secs(15)),
            "{kind}: the probe is bounded"
        );
        let problem = problem.unwrap_or_else(|| panic!("{kind}: reported as working"));
        let expected = if kind == "hangs" {
            // Never answered: not verified, which is not a verdict.
            "not verified as a build-lease wrapper"
        } else {
            "is not a build-lease wrapper"
        };
        assert!(problem.contains(expected), "{kind}: {problem}");
        assert!(
            path_of(&lease::launch_env(
                Some(&config),
                Some(OsStr::new("/usr/bin:/bin")),
                true
            ))
            .is_none(),
            "{kind}: not routed"
        );
    }
}

/// Starts `world`'s wrapper for `tag` as if on a disk that takes no more
/// bytes: a zero soft file-size limit, so every write past the first byte
/// fails (SIGXFSZ ignored, as for EFBIG), while creating an empty file works.
fn start_on_a_full_disk(world: &mut World, tag: &str) -> Child {
    use std::os::unix::process::CommandExt;
    let (mut command, _) = world.command(tag, &["build"]);
    command.stderr(Stdio::piped());
    // SAFETY: between fork and exec this calls only getrlimit, setrlimit and
    // signal, which are async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = 0;
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            Ok(())
        });
    }
    command.spawn_locked().expect("spawn gitpulse-hook")
}

#[test]
fn a_disk_that_takes_no_more_bytes_still_leases_every_build() {
    let mut world = World::new(r#"{"slots":1}"#);
    // The lease directory exists already, private, as the first build left it
    // (one other users could write is refused, which is another test).
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(world.lease_dir().join("queue"))
            .unwrap();
    }
    let a = start_on_a_full_disk(&mut world, "a");
    wait_for("a to start", || world.started() == ["a"]);
    let b = start_on_a_full_disk(&mut world, "b");
    settle();
    assert_eq!(
        world.started(),
        ["a"],
        "b started beside a with one slot: a full disk let a build run unleased"
    );
    world.release();
    let mut said = Vec::new();
    for child in [a, b] {
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        said.push(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    assert_eq!(world.started(), ["a", "b"], "{:?}", world.lines());
    assert_eq!(world.peak(), 1, "{:?}", world.lines());
    for text in &said {
        assert!(!text.contains("did NOT lease"), "{text}");
    }
    assert!(
        said.iter()
            .any(|text| text.contains("could not be written")),
        "the slot it holds without a record is said: {said:?}"
    );
}

#[test]
fn an_unleased_build_that_cannot_be_counted_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new(r#"{"slots":1}"#);
    std::fs::create_dir_all(world.lease_dir()).unwrap();
    std::fs::set_permissions(world.lease_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    // Where Health counts unleased builds is not a directory.
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let markers = world.tmp.join(format!("gitpulse-hook-advisories-{uid}"));
    std::fs::write(&markers, b"not a directory").unwrap();
    let mut world = world;
    let (mut command, stderr) = world.command("a", &["build"]);
    command.env("NO_WAIT", "1");
    let status = command.spawn_locked().unwrap().wait().unwrap();
    assert_eq!(status.code(), Some(0), "the build ran");
    assert_eq!(world.started(), ["a"]);
    let said = std::fs::read_to_string(stderr).unwrap();
    assert!(said.contains("did NOT lease this build"), "{said}");
    assert!(
        said.contains("could NOT be counted"),
        "a miss Health cannot count is said, not dropped: {said}"
    );
}
