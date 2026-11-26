//! Which owner key `gitpulse-mcp` orchestrates as when it has no terminal
//! credentials: `mcp-<pid>`, or the Mavi thread key the host put in
//! `GITPULSE_ORCHESTRATE_CALLER`.

use crate::terminal::orchestrate::{ORCHESTRATE_CALLER_ENV, SESSION_TOKEN_ENV};
use crate::test_support::env::{bind_env, env_serial};

const THREAD: &str = "0f8e2b9a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";

fn trusted_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::test_support::git_in(dir.path(), &["init", "-q"]);
    crate::test_support::trust_repo(dir.path());
    let repo = dir.path().canonicalize().expect("canonical");
    let repo = repo.to_string_lossy().into_owned();
    (dir, repo)
}

#[test]
fn a_mavi_agents_caller_variable_names_its_thread_as_the_external_owner() {
    let (_dir, repo) = trusted_repo();
    let serial = env_serial();
    let _env = bind_env(&serial)
        .remove(crate::terminal::TERMINAL_SESSION_ENV)
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(SESSION_TOKEN_ENV)
        .set(ORCHESTRATE_CALLER_ENV, format!("ext-mavi-{THREAD}"));

    let (_, sid, tok, external) =
        super::resolve_orchestrate_context(&repo, None, None).expect("resolve");
    assert_eq!((sid, tok), (None, None));
    assert_eq!(
        external.as_deref(),
        Some(format!("ext-mavi-{THREAD}").as_str())
    );
}

#[test]
fn an_invalid_caller_variable_is_refused_not_replaced_by_mcp_pid() {
    let (_dir, repo) = trusted_repo();
    let serial = env_serial();
    for bad in ["ext-1234", "mcp-9", "ext-mavi-not-a-uuid", "someone-else"] {
        let _env = bind_env(&serial)
            .remove(crate::terminal::TERMINAL_SESSION_ENV)
            .remove(crate::alerts::bridge::SESSION_ENV)
            .remove(SESSION_TOKEN_ENV)
            .set(ORCHESTRATE_CALLER_ENV, bad);
        let err = super::resolve_orchestrate_context(&repo, None, None)
            .expect_err("an invalid caller variable must fail loudly");
        assert!(err.contains(ORCHESTRATE_CALLER_ENV), "{bad}: {err}");
    }
}

#[test]
fn terminal_credentials_outrank_the_caller_variable() {
    let (_dir, repo) = trusted_repo();
    let serial = env_serial();
    let _env = bind_env(&serial)
        .remove(crate::terminal::TERMINAL_SESSION_ENV)
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(SESSION_TOKEN_ENV)
        .set(ORCHESTRATE_CALLER_ENV, format!("ext-mavi-{THREAD}"));

    let (_, sid, tok, external) =
        super::resolve_orchestrate_context(&repo, Some("term-1-abc".into()), Some("tok".into()))
            .expect("resolve");
    assert_eq!(sid.as_deref(), Some("term-1-abc"));
    assert_eq!(tok.as_deref(), Some("tok"));
    assert_eq!(external, None);
}

#[test]
fn without_the_variable_the_standalone_caller_is_still_mcp_pid() {
    let (_dir, repo) = trusted_repo();
    let serial = env_serial();
    let _env = bind_env(&serial)
        .remove(crate::terminal::TERMINAL_SESSION_ENV)
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(SESSION_TOKEN_ENV)
        .remove(ORCHESTRATE_CALLER_ENV);
    let (_, _, _, external) =
        super::resolve_orchestrate_context(&repo, None, None).expect("resolve");
    assert_eq!(external, Some(format!("mcp-{}", std::process::id())));
}
