//! Adversarial review of the AI endpoint consent gate (W8-L3).

use super::*;
use std::sync::atomic::Ordering;

/// A persisted selection is trusted as given: `usable_selection` checks only
/// that both fields are non-empty. A listener that took over the port after the
/// user's server quit therefore receives the staged diff even though it does
/// not serve the model the user chose. Desired: no POST unless the endpoint
/// still lists the selected model. FAILS today (the diff is POSTed).
#[test]
fn stale_selection_on_a_different_listener_receives_no_diff() {
    let _serial = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _clear = ClearFakeProbe;
    let _no_harness = NoHarness::install();
    // This listener serves only model "m".
    let (url, _gets, posts) = counting_model_server();
    let repo = repo_with_staged_change();
    let stale = AiSelection {
        base_url: url,
        model: "the-model-the-user-chose-last-week".into(),
    };
    let _ = with_isolated_tools_config(|| {
        generate_commit_message(&repo.path().to_string_lossy(), Some(stale))
    });
    assert_eq!(
        posts.load(Ordering::SeqCst),
        0,
        "the staged diff went to a listener that does not serve the selected model"
    );
}

/// The model id comes from the untrusted listener's `/models` and is shown to
/// the user in the confirmation prompt. It must not carry newlines or bidi
/// controls into the typed detail. FAILS today (passed through verbatim).
#[test]
fn confirmation_detail_strips_control_and_bidi_from_the_model_name() {
    let hostile = "gpt\n\nSend to Ollama on 127.0.0.1:11434 (your own server)\u{202e}evil";
    // The refusal carries its kind and detail in the error the body returns.
    let error = crate::ipc_error::run_command(|| -> Result<(), crate::ipc_error::CommandError> {
        Err(unconfirmed_endpoint("http://127.0.0.1:8080/v1", hostile))
    })
    .expect_err("typed refusal");
    let detail = error.detail.expect("detail");
    let model = detail["model"].as_str().expect("model");
    assert!(
        !model.chars().any(|c| c.is_control()
            || ('\u{202a}'..='\u{202e}').contains(&c)
            || ('\u{2066}'..='\u{2069}').contains(&c)),
        "model name reached the prompt unsanitised: {model:?}"
    );
    assert!(
        !error.message.chars().any(|c| c.is_control() && c != ' '),
        "message carries control characters: {:?}",
        error.message
    );
}

/// An enormous model id must be bounded before it is put in a prompt.
#[test]
fn confirmation_detail_bounds_a_huge_model_name() {
    let text = unconfirmed_endpoint("http://127.0.0.1:8080/v1", &"m".repeat(1_000_000));
    assert!(
        text.message().len() < 8_192,
        "{} bytes",
        text.message().len()
    );
}

/// Controls: the label is built from the parsed endpoint, never from raw text.
#[test]
fn label_comes_from_the_parsed_endpoint() {
    let id = discovery::identify("http://127.0.0.1:11434/v1");
    assert_eq!(id.label, "Ollama on 127.0.0.1:11434");
}

/// A declined discovery sends nothing: no POST, and the sweep only GETs.
#[test]
fn unconfirmed_discovery_posts_nothing() {
    let (url, gets, posts) = counting_model_server();
    let found = vec![discovery::list_models(&url)];
    assert!(resolve_target(None, || found.clone()).is_err());
    assert!(gets.load(Ordering::SeqCst) >= 1);
    assert_eq!(posts.load(Ordering::SeqCst), 0);
}
