//! Concurrent callers on the one shared sidecar: what `guard_files` relies on
//! when it keeps several verdict requests in flight.

use super::sidecar::{bind_test_binary, call_policy, lend_test_serial, test_serial};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Answers every request after a delay, concurrently, except one for the path
/// `hang`, which it never answers. Each start of the child is logged.
const FAKE_SLOW_SIDECAR: &str = r#"#!/bin/sh
echo started >> "@STARTS@"
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  case "$line" in
    *'"path":"hang"'*) ;;
    *) ( sleep 3; reply "$line" '{"action":"allow","rule":"","severity":"","reason":"","target":"","task_id":"","demoted":"","degraded":[]}' ) &;;
  esac
done
"#;

/// One caller's timeout must not take the sidecar away from the callers that
/// are still waiting on it: with eight verdicts in flight, a single unlucky
/// request used to restart the child under the other seven.
#[test]
fn one_callers_timeout_does_not_restart_the_sidecar_under_the_others() {
    let dir = tempfile::TempDir::new().unwrap();
    let starts = dir.path().join("starts");
    let script = dir.path().join("slow-manvi");
    std::fs::write(
        &script,
        FAKE_SLOW_SIDECAR.replace("@STARTS@", &starts.display().to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let serial = test_serial();
    let _binary = bind_test_binary(&serial, script.to_string_lossy());

    let ask = |path: &str, timeout: Duration| {
        let _lent = lend_test_serial(true);
        call_policy::<serde_json::Value>(
            "policy.check.file",
            serde_json::json!({ "root": "/r", "path": path, "op": "modify" }),
            timeout,
        )
    };
    // Warm the child so every caller below finds one already running.
    ask("warm", Duration::from_secs(10)).expect("warm-up answers");

    let outcomes: Vec<Result<serde_json::Value, String>> = std::thread::scope(|scope| {
        let hung = scope.spawn(|| ask("hang", Duration::from_secs(1)));
        let others: Vec<_> = (0..7)
            .map(|i| {
                scope.spawn(move || {
                    // Let the hung request go out and time out first.
                    std::thread::sleep(Duration::from_millis(100));
                    ask(&format!("f{i}.md"), Duration::from_secs(10))
                })
            })
            .collect();
        assert!(hung.join().unwrap().is_err(), "an unanswered request fails");
        others
            .into_iter()
            .map(|handle| handle.join().unwrap().map_err(|e| e.to_string()))
            .collect()
    });
    for (index, outcome) in outcomes.iter().enumerate() {
        assert!(
            outcome.is_ok(),
            "caller {index} was answered by a healthy sidecar but got {outcome:?}"
        );
    }
    let started = std::fs::read_to_string(&starts).unwrap().lines().count();
    assert_eq!(started, 1, "the child was restarted {} times", started - 1);
}
