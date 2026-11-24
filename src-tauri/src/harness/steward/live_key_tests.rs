//! Live: a key Anthropic refuses, then one it accepts, through GitPulse's own
//! route to Manvi's `work.assist.complete`: the installed `manvi`, the account
//! `~/.anthropic-accounts` chooses, and the cloud model `manvi use` names.
//!
//! Opt-in (`GITPULSE_LIVE_MANVI_CLOUD=1`, run with `--ignored`): it spends a
//! few tokens on that account. Once opted in, a missing prerequisite fails the
//! test rather than skipping it, so "could not run" never reads as a pass.
//!
//! The refused key is a well-formed key no account holds, set by a wrapper
//! around the real `manvi` while a file names it. Removing the file is the
//! person fixing their account: the same app must then recover without a
//! restart.
//!
//! Tests run in a hermetic `HOME`. The developer's real one is bound only
//! while `manvi use` is read and while a Manvi worker starts, because the
//! account registry and Manvi's choice live there. Every other step, the
//! fixture repository's trust grant included, stays hermetic.

use super::*;
use crate::harness::sidecar::{bind_test_binary, test_serial};
use crate::test_support::env::{bind_env, env_serial, hermetic};
use crate::workbench::WorkbenchState;
use std::os::unix::fs::PermissionsExt;

const OPT_IN: &str = "GITPULSE_LIVE_MANVI_CLOUD";
const SYSTEM: &str = "Reply with the single word ok.";
const USER: &str = "Say ok.";

fn on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    let found = std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{OPT_IN}=1, but `{name}` is not on PATH"));
    std::fs::canonicalize(found).expect("canonical path")
}

fn quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_str().expect("utf-8 path").replace('\'', "'\\''")
    )
}

/// Runs `f` with the developer's real `HOME`.
fn in_real_home<T>(f: impl FnOnce() -> T) -> T {
    let real = hermetic::real_home().expect("the test process kept no real HOME");
    let serial = env_serial();
    let _home = bind_env(&serial).set("HOME", &real);
    f()
}

/// Makes sure a Manvi worker for `selection` is running, starting one from the
/// real `HOME` when none is. A worker already running is kept as it is, so this
/// never replaces one that holds a refused key: that is the app's job.
fn worker_running(host: &WorkbenchState, selection: &ModelSelection) {
    let served = in_real_home(|| host.assist_served(selection))
        .unwrap_or_else(|e| panic!("the Manvi worker did not start ({}): {}", e.code, e.message));
    assert!(served, "this Manvi does not serve work.assist.complete");
}

/// Shaped like an Anthropic key so nothing refuses it before Anthropic does;
/// assembled here so no key-shaped literal sits in the source.
fn key_no_account_holds() -> String {
    ["sk", "ant", "api03", &"0".repeat(93)].join("-") + "AA"
}

/// The steward's recorded rejection, from the process-wide runtime every call
/// site writes. Only this test (run alone) reads it.
fn recorded_rejection() -> Option<String> {
    crate::supervisor::lock_or_recover(&runtime().inner)
        .credential_rejected
        .clone()
}

fn ask_ok() -> serde_json::Value {
    json!({ "system": SYSTEM, "user": USER, "max_output_tokens": 16 })
}

#[test]
#[ignore = "live: spends Anthropic tokens through the installed manvi; set GITPULSE_LIVE_MANVI_CLOUD=1"]
fn a_refused_key_reads_rejected_everywhere_and_a_fixed_account_recovers_live() {
    assert_eq!(
        std::env::var(OPT_IN).as_deref(),
        Ok("1"),
        "set {OPT_IN}=1 to run this live test"
    );
    let manvi = in_real_home(|| read_steward_manvi(&mut false)).expect("read `manvi use`");
    assert_eq!(
        manvi.state, "ready",
        "Manvi is not usable: {}",
        manvi.detail
    );
    assert_eq!(manvi.mode, "cloud", "run `manvi use cloud` first");
    let selection = manvi.typed.clone().expect("a cloud model selection");
    let ModelSelection::Claude { model } = &selection else {
        panic!("`manvi use` cloud did not name a Claude model: {selection:?}");
    };
    let model = model.clone();

    let manvi_bin = on_path("manvi");
    let dcstore = on_path("dcstore");
    let serial = test_serial();
    let dir = tempfile::tempdir().expect("tempdir");
    let refused_key_file = dir.path().join("refused-key");
    let wrapper = dir.path().join("manvi");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             unset ANTHROPIC_API_KEY_FALLBACK\n\
             if [ -s {file} ]; then\n\
             ANTHROPIC_API_KEY=$(cat {file})\n\
             MANVI_LLM_ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY\n\
             export ANTHROPIC_API_KEY MANVI_LLM_ANTHROPIC_API_KEY\n\
             fi\n\
             export MANVI_STORE_BINARY={store}\n\
             exec {manvi} \"$@\"\n",
            file = quote(&refused_key_file),
            store = quote(&dcstore),
            manvi = quote(&manvi_bin),
        ),
    )
    .expect("write wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
        .expect("chmod wrapper");
    let _binary = bind_test_binary(&serial, wrapper.to_str().expect("utf-8 path"));
    let host = WorkbenchState::for_profile(&dir.path().join("profile.sqlite"));

    let repo = crate::test_support::git_repo();
    crate::test_support::write(repo.path(), "notes.txt", "first\n");
    crate::test_support::git_in(repo.path(), &["add", "notes.txt"]);
    crate::test_support::commit_at(repo.path(), "seed", 1_800_000_000, "live@example.com");
    crate::test_support::write(repo.path(), "notes.txt", "first\nsecond: retry uploads\n");
    let repo_path = repo.path().to_str().expect("utf-8 path").to_string();
    let deps = LiveDeps {
        workbench: &host,
        watched_roots: Vec::new(),
    };
    let status = || build_status(runtime(), ManviStewardSettings::default(), &manvi, None);
    let evaluate = cleanup_evaluator(&model, |system, user, max_tokens| {
        deps.call_manvi_assist_checked(&selection, system, user, max_tokens, true)
    });

    // --- A key Anthropic refuses ---
    std::fs::write(&refused_key_file, key_no_account_holds()).expect("write refused key");

    // Manvi's own answer: the code, read as data.
    worker_running(&host, &selection);
    let failure = (host.assist_route(selection.clone()).complete)(ask_ok())
        .expect_err("Anthropic accepted a key no account holds");
    assert_eq!(
        failure.code,
        crate::ai::MANVI_PROVIDER_AUTH,
        "Manvi's message: {}",
        failure.message
    );

    // An AI feature: the person reads the replace-the-key sentence, and the
    // steward's status says rejected.
    worker_running(&host, &selection);
    let error = crate::ai::suggest_branch_name(&repo_path, host.assist_route(selection.clone()))
        .expect_err("a refused key named a branch");
    assert_eq!(error.message(), crate::ai::REJECTED_KEY_MESSAGE);
    assert_eq!(recorded_rejection().as_deref(), Some(model.as_str()));
    let shown = status();
    assert_eq!(shown.manvi_state, "rejected", "{}", shown.manvi_detail);
    assert_eq!(shown.manvi_detail, crate::ai::REJECTED_KEY_MESSAGE);

    // The steward's directive call reads the same code.
    worker_running(&host, &selection);
    let directive = deps
        .manvi_complete(&selection, SYSTEM, USER, 16)
        .expect_err("a refused key wrote a directive");
    assert!(directive.credential_rejected, "{directive}");

    // The cleanup selection's evaluator, wired as `clean_critical_storage`
    // wires it, reports the refusal too.
    worker_running(&host, &selection);
    take_key_answers_noted_here();
    evaluate(SYSTEM, USER, 16).expect_err("a refused key ranked cleanup candidates");
    assert_eq!(
        take_key_answers_noted_here(),
        vec![(model.clone(), Some(true))]
    );

    // --- The person fixes their account; the same app recovers ---
    std::fs::remove_file(&refused_key_file).expect("remove refused key");

    worker_running(&host, &selection);
    let reply =
        (host.assist_route(selection.clone()).complete)(ask_ok()).unwrap_or_else(|failure| {
            panic!(
                "the fixed account was not used ({}): {}",
                failure.code, failure.message
            )
        });
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["provider"], "anthropic", "{reply}");
    let answered_model = reply["model"].as_str().unwrap_or_default();
    assert!(
        answered_model.starts_with(&model),
        "asked for {model}, answered by {answered_model}"
    );
    assert!(
        !reply["text"].as_str().unwrap_or_default().trim().is_empty(),
        "{reply}"
    );

    worker_running(&host, &selection);
    let generation =
        crate::ai::suggest_branch_name(&repo_path, host.assist_route(selection.clone()))
            .expect("a working key named a branch");
    assert!(!generation.text.is_empty());
    assert_eq!(
        recorded_rejection(),
        None,
        "a working call did not clear the rejection"
    );
    let shown = status();
    assert_eq!(shown.manvi_state, "ready", "{}", shown.manvi_detail);

    worker_running(&host, &selection);
    deps.manvi_complete(&selection, SYSTEM, USER, 16)
        .expect("a working key wrote a directive");
    take_key_answers_noted_here();
    evaluate(SYSTEM, USER, 16).expect("a working key answered the cleanup selection");
    assert_eq!(
        take_key_answers_noted_here(),
        vec![(model.clone(), Some(false))]
    );
    eprintln!(
        "live: {model} answered as {answered_model}; branch name {:?}",
        generation.text
    );

    host.shutdown();
}
