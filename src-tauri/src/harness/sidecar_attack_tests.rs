//! Adversarial tests for the scratch-directory lifecycle and the bounded keep of
//! a silent child that hosts runs (child module of `sidecar`).

use super::*;

const FAKE_SH: &str = r#"#!/bin/sh
# Holds a file open in its own state directory (its cwd) for its whole life.
exec 3>"$PWD/held-open"
reply() { printf '{"id":"%s","ok":true,"result":%s}\n' "$1" "$2"; }
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  case "$line" in
    *'"op":"hello"'*) reply "$id" '{"protocol":1,"ops":["hello","work.runs.managed.prepare","work.runs.managed.stop"],"posture":"host"}' ;;
    *) reply "$id" '{}' ;;
  esac
done
"#;

struct Fixture {
    _bin: tempfile::TempDir,
    root: tempfile::TempDir,
    _binary: TestBinaryBinding,
    _scratch: TestScratchRoot,
}

fn fixture(serial: &SidecarTestGuard) -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::TempDir::new().unwrap();
    let root = tempfile::TempDir::new().unwrap();
    let binary = bin.path().join("holding-sidecar");
    std::fs::write(&binary, FAKE_SH).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bound = bind_test_binary(serial, binary.to_str().unwrap());
    let scratch = bind_test_scratch_root(serial, root.path());
    Fixture {
        _bin: bin,
        root,
        _binary: bound,
        _scratch: scratch,
    }
}

fn dirs(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

#[test]
fn a_child_holding_a_file_open_in_its_directory_does_not_keep_the_directory() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let sidecar = spawn().expect("spawn");
    let live = dirs(fixture.root.path());
    assert_eq!(live.len(), 1);
    assert!(live[0].join("held-open").exists(), "the child holds a file");
    sidecar.terminate();
    assert_eq!(dirs(fixture.root.path()), Vec::<PathBuf>::new());
}

#[test]
fn terminate_twice_then_drop_is_quiet_and_leaves_nothing() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let sidecar = spawn().expect("spawn");
    sidecar.terminate();
    sidecar.terminate();
    drop(sidecar);
    assert_eq!(dirs(fixture.root.path()), Vec::<PathBuf>::new());
}

#[test]
fn a_dropped_sidecar_that_was_never_terminated_leaves_nothing() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let sidecar = spawn().expect("spawn");
    drop(sidecar);
    assert_eq!(dirs(fixture.root.path()), Vec::<PathBuf>::new());
}

#[test]
fn a_scratch_root_that_disappeared_does_not_fail_terminate() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let sidecar = spawn().expect("spawn");
    for dir in dirs(fixture.root.path()) {
        std::fs::remove_dir_all(dir).unwrap();
    }
    sidecar.terminate();
    drop(sidecar);
    assert_eq!(dirs(fixture.root.path()), Vec::<PathBuf>::new());
}

#[test]
fn a_directory_swapped_for_a_symlink_before_terminate_is_not_followed() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let victim = tempfile::TempDir::new().unwrap();
    std::fs::write(victim.path().join("precious"), b"x").unwrap();
    let sidecar = spawn().expect("spawn");
    let dir = dirs(fixture.root.path()).remove(0);
    // The child is still running and holds its cwd handle; swap the path.
    std::fs::rename(&dir, fixture.root.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(victim.path(), &dir).unwrap();
    sidecar.terminate();
    assert!(victim.path().join("precious").is_file());
}

#[test]
fn concurrent_spawn_and_terminate_on_many_sidecars_leave_nothing() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    let sidecar = spawn().expect("spawn");
                    sidecar.terminate();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert_eq!(dirs(fixture.root.path()), Vec::<PathBuf>::new());
}

#[test]
fn concurrent_calls_and_shutdown_on_one_connection_leave_nothing() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let connection =
        ProfileConnection::new(fixture.root.path().join("profile.sqlite"), None).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..10 {
                let _ =
                    connection.call("work.runs.managed.prepare", serde_json::json!({"id": "a"}));
            }
        });
        scope.spawn(|| {
            for _ in 0..10 {
                let _ = connection.call("work.runs.managed.stop", serde_json::json!({"id": "a"}));
                connection.shutdown();
            }
        });
    });
    connection.shutdown();
    let left: Vec<_> = dirs(fixture.root.path())
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("gitpulse-harness-"))
        })
        .collect();
    assert_eq!(left, Vec::<PathBuf>::new());
}

#[test]
fn runs_ended_on_a_connection_with_no_worker_starts_none() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let connection =
        ProfileConnection::new(fixture.root.path().join("profile.sqlite"), None).unwrap();
    connection.runs_ended(&["x".to_string()]);
    assert_eq!(connection.hosted_runs(), Some(Vec::<String>::new()));
    assert_eq!(connection.child_pid(), None);
    assert_eq!(
        dirs(fixture.root.path()),
        Vec::<PathBuf>::new(),
        "no scratch directory means no child was started"
    );
}

#[test]
fn an_ended_run_that_is_prepared_again_counts_as_hosted_again() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let connection =
        ProfileConnection::new(fixture.root.path().join("profile.sqlite"), None).unwrap();
    connection
        .call("work.runs.managed.prepare", serde_json::json!({"id": "r"}))
        .unwrap();
    connection.runs_ended(&["r".to_string()]);
    assert_eq!(connection.hosted_runs(), Some(Vec::<String>::new()));
    connection
        .call("work.runs.managed.prepare", serde_json::json!({"id": "r"}))
        .unwrap();
    assert_eq!(connection.hosted_runs(), Some(vec!["r".to_string()]));
    // A late runs_ended for an id that is not hosted changes nothing.
    connection.runs_ended(&["other".to_string()]);
    assert_eq!(connection.hosted_runs(), Some(vec!["r".to_string()]));
    connection.shutdown();
}

#[test]
fn a_prepare_racing_runs_ended_for_the_same_id_never_leaves_a_phantom_or_loses_the_last_prepare() {
    let serial = test_serial();
    let fixture = fixture(&serial);
    let connection =
        ProfileConnection::new(fixture.root.path().join("profile.sqlite"), None).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..30 {
                connection
                    .call("work.runs.managed.prepare", serde_json::json!({"id": "r"}))
                    .unwrap();
            }
        });
        scope.spawn(|| {
            for _ in 0..30 {
                connection.runs_ended(&["r".to_string()]);
            }
        });
    });
    // Whatever interleaving happened, a final prepare is hosted.
    connection
        .call("work.runs.managed.prepare", serde_json::json!({"id": "r"}))
        .unwrap();
    assert_eq!(connection.hosted_runs(), Some(vec!["r".to_string()]));
    connection.shutdown();
}

#[test]
fn stall_counting_edges() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    // Two timeouts are not three.
    let mut stall = Stall::default();
    assert_eq!(stall.count_timeout(at(0), at(100)).0, 1);
    let (two, _) = stall.count_timeout(at(150), at(250));
    assert_eq!(two, 2);
    assert!(!keep_is_exhausted(
        two,
        Duration::from_millis(250),
        Duration::from_secs(60)
    ));
    let (three, _) = stall.count_timeout(at(300), at(400));
    assert!(keep_is_exhausted(
        three,
        Duration::from_millis(400),
        Duration::from_secs(60)
    ));
    // A call issued at exactly the instant of the last counted timeout is not
    // proof it was sent after it: it is one timeout.
    let mut same = Stall::default();
    same.count_timeout(at(0), at(100));
    assert_eq!(same.count_timeout(at(100), at(101)).0, 1);
    // A call issued before the first (a late straggler) never adds.
    assert_eq!(same.count_timeout(at(0), at(500)).0, 1);
    // An answer resets everything, including the quiet clock.
    let mut reset = Stall::default();
    reset.count_timeout(at(0), at(100));
    reset = Stall::default();
    let (count, quiet) = reset.count_timeout(at(10_000), at(10_100));
    assert_eq!((count, quiet), (1, Duration::ZERO));
}

#[test]
fn the_longest_managed_deadline_follows_what_the_child_advertised() {
    let table = MANAGED_OPS.iter().map(|op| op_deadline(op)).max().unwrap();
    // The bound with nothing advertised is the table's longest.
    let none = HelloResult::default();
    let bound = |hello: &HelloResult| {
        MANAGED_OPS
            .iter()
            .map(|op| advertised_timeout(hello, op).unwrap_or_else(|| op_deadline(op)))
            .max()
            .unwrap()
    };
    assert_eq!(bound(&none), table);
    let limit = |ms: u64| OpLimit {
        lane: "managed".into(),
        deadline_ms: ms,
    };
    let small = HelloResult {
        op_limits: Some(
            MANAGED_OPS
                .iter()
                .map(|op| ((*op).to_owned(), limit(1)))
                .collect(),
        ),
        ..HelloResult::default()
    };
    assert!(
        bound(&small) < table,
        "all four advertised small shrinks the bound"
    );
    let big = HelloResult {
        op_limits: Some(
            [(MANAGED_OPS[0].to_owned(), limit(10 * 60_000))]
                .into_iter()
                .collect(),
        ),
        ..HelloResult::default()
    };
    assert!(bound(&big) >= table);
    assert!(bound(&big) <= MAX_ADVERTISED_TIMEOUT);
}
