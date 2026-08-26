//! Live end-to-end test of the local AI pipeline.
//!
//! It runs the real thing: loopback discovery, `capability.probe` and
//! `chat.prepare` through the MANVI sidecar, an actual completion against a
//! local model server, and `chat.settle` to read the reply back. It is
//! therefore opt-in — `GITPULSE_LIVE_AI=1`, with an optional
//! `GITPULSE_LIVE_AI_MODEL` and `GITPULSE_LIVE_AI_BASE_URL` — and reports a
//! skip on stderr when it is not enabled, so a run without a model server never
//! looks like a run that passed.

use std::process::Command;

use gitpulse_lib::ai::{self, AiSelection};

fn live_ai_enabled(test_name: &str) -> bool {
    if std::env::var("GITPULSE_LIVE_AI").as_deref() == Ok("1") {
        return true;
    }
    eprintln!(
        "SKIPPED {test_name}: set GITPULSE_LIVE_AI=1 to run it against a local model server. \
         This check did not run."
    );
    false
}

fn selection_from_env() -> Option<AiSelection> {
    match (
        std::env::var("GITPULSE_LIVE_AI_BASE_URL"),
        std::env::var("GITPULSE_LIVE_AI_MODEL"),
    ) {
        (Ok(base_url), Ok(model)) => Some(AiSelection { base_url, model }),
        _ => None,
    }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn writes_a_commit_message_for_a_real_staged_diff() {
    if !live_ai_enabled("writes_a_commit_message_for_a_real_staged_diff") {
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    std::fs::write(
        path.join("auth.rs"),
        "pub fn login(user: &str) -> bool {\n    !user.is_empty()\n}\n",
    )
    .unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "feat(auth): add login"]);

    // The change to describe: a token refresh path with an expiry guard.
    std::fs::write(
        path.join("auth.rs"),
        "use std::time::{Duration, SystemTime};\n\n\
         pub fn login(user: &str) -> bool {\n    !user.is_empty()\n}\n\n\
         /// Refreshes an access token when it is within a minute of expiring.\n\
         pub fn refresh_token(issued: SystemTime, ttl: Duration) -> bool {\n\
         \x20   match issued.elapsed() {\n\
         \x20       Ok(age) => age + Duration::from_secs(60) >= ttl,\n\
         \x20       Err(_) => true,\n\
         \x20   }\n}\n",
    )
    .unwrap();
    git(path, &["add", "."]);

    let repo = path.canonicalize().unwrap().to_string_lossy().into_owned();
    let selection = selection_from_env();

    let generation = ai::generate_commit_message(&repo, selection).expect("a commit message");

    eprintln!("--- generated commit message ---\n{}\n---", generation.text);
    eprintln!(
        "model={} endpoint={} context={} ({}) prompt_tokens={} completion_tokens={} in {}ms",
        generation.model,
        generation.base_url,
        generation.context_window,
        generation.context_source,
        generation.prompt_tokens,
        generation.completion_tokens,
        generation.elapsed_ms
    );
    for warning in &generation.warnings {
        eprintln!("warning: {}", warning);
    }

    assert!(!generation.text.is_empty(), "the message is empty");
    assert!(
        !generation.text.contains("<think>"),
        "thinking leaked into the message"
    );
    assert!(
        !generation.text.starts_with("```"),
        "a code fence survived cleaning"
    );
    assert!(!generation.model.is_empty());
    assert!(generation.context_window > 0);
    // A subject line, not an essay: the first line must be usable as one.
    let subject = generation.text.lines().next().unwrap_or_default();
    assert!(!subject.trim().is_empty(), "the subject line is blank");
    assert!(
        subject.len() < 200,
        "the subject line is {} characters: {}",
        subject.len(),
        subject
    );
}

#[test]
fn analyzes_a_real_coverage_report() {
    if !live_ai_enabled("analyzes_a_real_coverage_report") {
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(
        path.join("src/lib.rs"),
        "pub fn covered() -> bool { true }\n",
    )
    .unwrap();
    std::fs::write(
        path.join("src/cache.rs"),
        "pub fn stale() -> bool { false }\n",
    )
    .unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "feat: add coverage fixture"]);

    let repo = path.canonicalize().unwrap().to_string_lossy().into_owned();
    let report = "Coverage report — <repository>\n\n\
                  OVERALL\n82.0% (410/500 lines)\n\n\
                  PER-LANGUAGE\nRust: 82.0% (410/500 lines, 2 files)\n\n\
                  LOWEST-COVERED FILES (worst first, showing 2 of 2)\n\
                  - src/cache.rs: 15.0% (15/100 lines)\n\
                  - src/lib.rs: 98.8% (395/400 lines)";

    let generation =
        ai::coverage_report(&repo, report, selection_from_env()).expect("a coverage analysis");

    eprintln!(
        "--- generated coverage analysis ---\n{}\n---",
        generation.text
    );
    eprintln!(
        "model={} endpoint={} context={} ({}) prompt_tokens={} completion_tokens={} in {}ms",
        generation.model,
        generation.base_url,
        generation.context_window,
        generation.context_source,
        generation.prompt_tokens,
        generation.completion_tokens,
        generation.elapsed_ms
    );
    for warning in &generation.warnings {
        eprintln!("warning: {}", warning);
    }

    assert!(!generation.text.trim().is_empty(), "the analysis is empty");
    assert!(
        !generation.text.contains("<think>"),
        "thinking leaked into the analysis"
    );
    assert!(!generation.model.is_empty());
    assert!(generation.context_window > 0);
}
