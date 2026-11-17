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
