use super::*;

/// A stalled git must be killed at the deadline, not waited on forever.
///
/// `current_git_head` used `.output()`, which waits however long the child
/// feels like taking; a hung git (network mount, wedged hook) stalled every
/// drain batch behind it. The bounded runner kills at
/// [`GIT_HEAD_DEADLINE`]; this test proves the error arrives near the
/// deadline rather than after the sleeper's own 30s exit.
#[cfg(unix)]
#[test]
fn a_stalled_git_is_killed_at_the_deadline() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("devmap-gitdeadline-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("stalledgit");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let started = std::time::Instant::now();
    let result =
        run_git_head_with_deadline(&script.to_string_lossy(), std::path::Path::new("/tmp"));
    let elapsed = started.elapsed();

    let error = result.expect_err("a stalled git must produce an error");
    assert!(
        error.to_string().contains("killed"),
        "the error must say the child was killed: {error}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(GIT_HEAD_DEADLINE.as_secs() + 2),
        "kill must land near the deadline, took {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
