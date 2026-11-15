//! The Claude route: GitPulse's own AI features through Manvi's
//! `work.assist.complete`, with no loopback anywhere on the path.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A recorded call to the fake Manvi.
type Calls = Arc<Mutex<Vec<Value>>>;

const DESTINATION: &str = "Claude API via Manvi";

/// A fake `work.assist.complete`: answers `dry_run` with a capability and a
/// real call with `reply`, or fails the real call with `failure`.
fn fake_manvi(reply: Value, failure: Option<ManviError>, window: i64) -> (ManviRoute, Calls) {
    let calls: Calls = Arc::default();
    let record = calls.clone();
    let complete: ManviComplete = Arc::new(move |params: Value| {
        record.lock().unwrap().push(params.clone());
        if params["dry_run"].as_bool().unwrap_or(false) {
            return Ok(serde_json::json!({
                "ok": true, "provider": "anthropic", "model": "claude-sonnet-5-5",
                "context_window": window, "max_output_tokens": 900,
            }));
        }
        match &failure {
            Some(error) => Err(error.clone()),
            None => Ok(reply.clone()),
        }
    });
    (
        ManviRoute {
            model: "claude-sonnet-5-5".into(),
            provider: "anthropic",
            destination: DESTINATION.into(),
            complete,
        },
        calls,
    )
}

fn reply(text: &str) -> Value {
    serde_json::json!({
        "ok": true, "provider": "anthropic", "model": "claude-sonnet-5-5-20260101",
        "context_window": 200_000, "max_output_tokens": 900, "text": text,
        "reasoning": "private thoughts", "stop_reason": "end_turn", "truncated": false,
        "input_tokens": 321, "output_tokens": 45,
    })
}

fn turn(diff: &str) -> impl Fn(usize) -> Result<Turn, String> + '_ {
    move |budget| {
        let budgeted = prompt::budget_diff(diff, budget);
        Ok(Turn {
            system: "system".into(),
            user: format!("user\n{}", budgeted.text),
            diff: budgeted,
        })
    }
}

fn no_sweep() -> Vec<discovery::DiscoveredEndpoint> {
    panic!("a Claude route must never sweep for local servers");
}

fn no_probe_installed() -> (Hits, ClearFakeProbe) {
    let hits = install_fake_probe(Err("a Claude route must never probe loopback".into()));
    (hits, ClearFakeProbe)
}

#[test]
fn a_claude_route_never_sweeps_probes_or_touches_loopback() {
    let _serial = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (probes, _clear) = no_probe_installed();
    let (cloud, calls) = fake_manvi(reply("It adds a readme."), None, 200_000);

    let generation = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn("diff --git a/x b/x\n+x\n"),
        no_sweep,
    )
    .expect("cloud generation");

    assert_eq!(probes.load(Ordering::SeqCst), 0, "loopback was probed");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "one capability call, one real call");
    assert_eq!(calls[0]["dry_run"], true);
    assert!(calls[0].get("system").is_none() && calls[0].get("user").is_none());
    assert_eq!(calls[1]["system"], "system");
    assert!(calls[1]["user"].as_str().unwrap().contains("+x"));
    assert!(calls[1].get("dry_run").is_none());
    // Manvi's applied cap (900) is under the feature's own cap and is what is asked for.
    assert_eq!(calls[1]["max_output_tokens"], 900);
    assert_eq!(generation.base_url, DESTINATION);
}

#[test]
fn a_cloud_success_maps_tokens_window_model_and_keeps_reasoning_out_of_the_text() {
    let (cloud, _calls) = fake_manvi(reply("<think>x</think>The answer."), None, 200_000);
    let generation = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn("diff --git a/x b/x\n+x\n"),
        no_sweep,
    )
    .expect("cloud generation");

    assert_eq!(generation.text, "The answer.");
    assert_eq!(generation.reasoning, "private thoughts");
    assert_eq!(generation.model, "claude-sonnet-5-5-20260101");
    assert_eq!(generation.context_window, 200_000);
    assert!(generation
        .context_source
        .contains("Manvi's anthropic adapter"));
    assert_eq!(generation.prompt_tokens, 321);
    assert_eq!(generation.completion_tokens, 45);
    assert!(!generation.truncated);
    assert!(generation.warnings.is_empty(), "{:?}", generation.warnings);
}

#[test]
fn truncation_and_a_cut_diff_each_produce_a_warning() {
    let mut answer = reply("Partial.");
    answer["truncated"] = true.into();
    // A 4k window forces the diff budget down to its floor.
    let (cloud, _calls) = fake_manvi(answer, None, 4_096);
    let big = format!("diff --git a/f b/f\n{}", "+line of change\n".repeat(20_000));

    let generation = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn(&big),
        no_sweep,
    )
    .expect("cloud generation");

    assert!(generation.truncated);
    assert!(generation.diff_truncated);
    let joined = generation.warnings.join("\n");
    assert!(joined.contains("output cap"), "{joined}");
    assert!(
        joined.contains("Attached context exceeded the window"),
        "{joined}"
    );
}

#[test]
fn a_cloud_failure_is_surfaced_and_never_replaced_by_loopback() {
    let _serial = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (probes, _clear) = no_probe_installed();
    let failure = ManviError {
        code: "timeout".into(),
        message: "the model call exceeded its deadline".into(),
    };
    let (cloud, _calls) = fake_manvi(Value::Null, Some(failure), 200_000);

    let error = run_with(
        Feature::BranchName,
        "/repo",
        Route::Manvi(cloud),
        turn("diff --git a/x b/x\n+x\n"),
        no_sweep,
    )
    .expect_err("the failure must reach the caller");

    assert!(
        error.message().contains("exceeded its deadline"),
        "{}",
        error.message()
    );
    assert_eq!(probes.load(Ordering::SeqCst), 0);
}

#[test]
fn a_manvi_without_the_op_gets_the_one_clear_update_message() {
    let refusal = ManviError {
        code: "unsupported_operation".into(),
        message:
            "This Manvi binary does not advertise work.assist.complete. Update Manvi to use x."
                .into(),
    };
    let complete: ManviComplete = Arc::new(move |_| Err(refusal.clone()));
    let error = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(ManviRoute {
            model: "claude-sonnet-5-5".into(),
            provider: "anthropic",
            destination: DESTINATION.into(),
            complete,
        }),
        turn("d"),
        no_sweep,
    )
    .expect_err("an old Manvi cannot serve Claude");
    assert_eq!(
        error.message(),
        "This Manvi does not offer work.assist.complete. Update Manvi to use Claude for GitPulse's AI features."
    );
}

#[test]
fn an_answer_from_another_provider_is_not_used() {
    let mut answer = reply("from elsewhere");
    answer["provider"] = "local".into();
    let (cloud, _calls) = fake_manvi(answer, None, 200_000);
    let error = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn("d"),
        no_sweep,
    )
    .expect_err("was asked for");
    assert!(
        error.message().contains("was asked for"),
        "{}",
        error.message()
    );
}

#[test]
fn a_commit_message_falls_back_to_the_patch_and_names_the_cloud_failure() {
    let repo = repo_with_staged_change();
    let failure = ManviError {
        code: "unavailable".into(),
        message: "Manvi is not running".into(),
    };
    let (cloud, _calls) = fake_manvi(Value::Null, Some(failure), 200_000);
    let sweeps = Arc::new(AtomicUsize::new(0));
    let counted = sweeps.clone();

    let generation = with_isolated_tools_config(|| {
        generate_commit_message_with(
            &repo.path().to_string_lossy(),
            Route::Manvi(cloud),
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            },
        )
    })
    .expect("a commit message is always produced");

    assert_eq!(
        sweeps.load(Ordering::SeqCst),
        0,
        "a Claude route swept loopback"
    );
    assert_eq!(generation.model, "gitpulse");
    assert!(!generation.text.trim().is_empty());
    let joined = generation.warnings.join("\n");
    assert!(
        joined.contains("Claude API via Manvi could not be reached"),
        "{joined}"
    );
    assert!(joined.contains("Manvi is not running"), "{joined}");
    assert!(joined.contains("written from the patch"), "{joined}");
}

#[test]
fn a_commit_message_from_claude_is_guarded_like_a_local_one() {
    let repo = repo_with_staged_change();
    let (cloud, calls) = fake_manvi(reply("docs: add a readme"), None, 200_000);
    let generation = with_isolated_tools_config(|| {
        generate_commit_message_with(
            &repo.path().to_string_lossy(),
            Route::Manvi(cloud),
            no_sweep,
        )
    })
    .expect("commit message");
    assert_eq!(generation.text, "docs: add a readme");
    assert_eq!(generation.base_url, DESTINATION);
    assert!(calls.lock().unwrap()[1]["user"]
        .as_str()
        .unwrap()
        .contains("README.md"));
}

#[test]
fn a_local_mode_route_goes_through_manvi_and_expects_the_local_provider() {
    let mut answer = reply("A local answer.");
    answer["provider"] = "local".into();
    answer["model"] = "qwen3".into();
    let (mut manvi, _calls) = fake_manvi(answer, None, 32_768);
    manvi.provider = "local";
    manvi.destination = "Local model via Manvi (http://127.0.0.1:11434/v1)".into();
    manvi.model = "qwen3".into();
    let probes = {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (probes, _clear) = no_probe_installed();
        let generation = run_with(
            Feature::ExplainCommit,
            "/repo",
            Route::Manvi(manvi),
            turn("d"),
            no_sweep,
        )
        .expect("local mode through Manvi");
        assert_eq!(generation.text, "A local answer.");
        assert_eq!(generation.model, "qwen3");
        assert!(generation.base_url.starts_with("Local model via Manvi"));
        probes.load(Ordering::SeqCst)
    };
    assert_eq!(probes, 0, "GitPulse's own loopback client was not used");
}

#[test]
fn a_busy_manvi_gets_its_own_message() {
    let busy = ManviError {
        code: "E_BUSY".into(),
        message: "two assist calls are in flight".into(),
    };
    let (cloud, _calls) = fake_manvi(Value::Null, Some(busy), 200_000);
    let error = run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn("d"),
        no_sweep,
    )
    .expect_err("busy");
    assert_eq!(
        error.message(),
        "Manvi is busy with two other AI requests; try again."
    );
}

/// Manvi refuses when ceil((system+user) bytes / 3) + max_output exceeds the
/// context window.
#[test]
fn the_assembled_request_passes_manvis_bytes_over_three_check() {
    let (cloud, calls) = fake_manvi(reply("ok"), None, 8_192);
    let big = format!("diff --git a/f b/f\n{}", "+line of change\n".repeat(20_000));
    run_with(
        Feature::ExplainCommit,
        "/repo",
        Route::Manvi(cloud),
        turn(&big),
        no_sweep,
    )
    .expect("cloud generation");
    let calls = calls.lock().unwrap();
    let sent = &calls[1];
    let bytes = sent["system"].as_str().unwrap().len() + sent["user"].as_str().unwrap().len();
    let estimate = (bytes as i64 + 2) / 3;
    assert!(
        estimate + 900 <= 8_192,
        "{estimate} + 900 exceeds the window"
    );
}
