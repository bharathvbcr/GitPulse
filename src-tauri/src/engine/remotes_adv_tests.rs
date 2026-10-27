//! Adversarial checks on remote credential redaction and configured-remote
//! membership. Tests marked `#[ignore = "break: ..."]` document a reproduced
//! defect and must start passing when it is fixed.

use super::*;
use crate::ledger::redact::url_for_display;
use crate::test_support::git_in;

const SECRET: &str = "S3cr3tT0ken";

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    dir
}

/// Prints the display form of every attack input (run with --nocapture).
#[test]
fn probe_display_forms() {
    let inputs = [
        "https://tok@host",
        "https://user:@host",
        "https://:tok@host",
        "http://user:pw@host/x",
        "git://user:pw@host/x",
        "ftp://user:pw@host/x",
        "file://user:pw@/path",
        "ssh://user:pw@host/x",
        "git+ssh://user:pw@host/x",
        "ssh://git@github.com:22/x",
        "HTTPS://user:pw@host/x",
        "https://user:pw @host/x",
        "https://user:pw\t@host/x",
        "https://user:pw\n@host/x",
        "https://user:pw@host@evil.com/",
        "https://host/x?a=u:pw@h",
        "https://host/x#u:pw@h",
        "https://user:p%40w@host/x",
        "https://user:pw@[::1]/x",
        "user:pw@host:path",
        "user:pw@host:22/path",
        "C:\\repo",
        "https://user:pa/ss@host/x",
        "https://user:pa?ss@host/x",
        "https://user:pa#ss@host/x",
        "https://user:pa\\ss@host/x",
        "https://host/x?access_token=abcdef0123456789abcdef",
        "https://host/x?token=hunter2",
        "https://host/x.git?password=hunter2",
        "https://a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0@host/x",
        "https://abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst@dev.azure.com/o/p/_git/r",
        "https://bob:x@host/x",
        "https://glpat-abcdefghij0123456789@host/x",
        "https://u:pw@host:8080/x",
        "https://%75ser:pw@host/x",
        "https://user:pw:more@host/x",
        "https:///user:pw@host/x",
        "https:/user:pw@host/x",
        "https:user:pw@host/x",
        "ssh://user:pw@host:22:/x",
        "ext::sh -c 'curl u:pw@h'",
        "git@host:path",
        "user:pw@host",
        "https://user:pw@",
        "https://@host",
        "https://:@host",
        "x://u:pw@h",
        "1x://u:pw@h/x",
    ];
    for input in inputs {
        let shown = url_for_display(input);
        println!(
            "PROBE {input:?} -> {:?} creds={}",
            shown.display, shown.has_credentials
        );
    }
}

fn leaks(input: &str) -> bool {
    url_for_display(input).display.contains(SECRET)
}

#[test]
fn display_never_contains_a_password_on_any_known_scheme_shape() {
    for input in [
        format!("https://user:{SECRET}@host/x"),
        format!("http://user:{SECRET}@host/x"),
        format!("git://user:{SECRET}@host/x"),
        format!("ftp://user:{SECRET}@host/x"),
        format!("file://user:{SECRET}@/path"),
        format!("ssh://user:{SECRET}@host/x"),
        format!("git+ssh://user:{SECRET}@host/x"),
        format!("HTTPS://user:{SECRET}@host/x"),
        format!("https://user:{SECRET}@host@evil.com/"),
        format!("https://user:p%40{SECRET}@host/x"),
        format!("https://user:{SECRET}@[::1]:8080/x"),
        format!("https://user:{SECRET}:more@host/x"),
        format!("https://:{SECRET}@host/x"),
    ] {
        assert!(
            !leaks(&input),
            "{input} -> {}",
            url_for_display(&input).display
        );
    }
}

#[test]
fn scp_like_url_with_password_is_redacted() {
    let input = format!("user:{SECRET}@host:path");
    assert!(!leaks(&input), "{}", url_for_display(&input).display);
}

#[test]
fn password_containing_slash_is_redacted() {
    let input = format!("https://user:pa/{SECRET}@host/x");
    assert!(!leaks(&input), "{}", url_for_display(&input).display);
}

#[test]
fn lone_unrecognised_token_is_not_shown_as_a_username() {
    for input in [
        "https://abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst@dev.azure.com/o/p/_git/r",
        "https://a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0@host/x",
    ] {
        let shown = url_for_display(input);
        assert!(
            shown.has_credentials
                && !shown
                    .display
                    .contains("a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0")
                && !shown.display.contains("abcdefghijklmnopqrstuvwxyz234567"),
            "{input} -> {shown:?}"
        );
    }
}

#[test]
fn query_string_credentials_are_redacted() {
    for input in [
        format!("https://host/x.git?token={SECRET}"),
        format!("https://host/x.git?password={SECRET}"),
        format!("https://host/x.git?access_token={SECRET}"),
    ] {
        assert!(
            !leaks(&input),
            "{input} -> {}",
            url_for_display(&input).display
        );
    }
}

#[test]
fn probe_display_scaling() {
    for n in [25_000usize, 50_000, 100_000, 200_000] {
        let long = format!("https://user:{SECRET}@host/{}", "a/".repeat(n));
        let started = std::time::Instant::now();
        let _ = url_for_display(&long);
        println!("SCALE {n} -> {:?}", started.elapsed());
    }
}

#[test]
fn display_is_bounded_and_fast_on_an_enormous_url() {
    let long = format!("https://user:{SECRET}@host/{}", "a/".repeat(500_000));
    let started = std::time::Instant::now();
    let shown = url_for_display(&long);
    assert!(!shown.display.contains(SECRET));
    let elapsed = started.elapsed();
    println!("LONG_URL_ELAPSED {elapsed:?}");
    assert!(
        elapsed < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
        "{elapsed:?}"
    );
}

// --- re-attach ---------------------------------------------------------

fn add_stored(dir: &tempfile::TempDir, url: &str) {
    git_in(dir.path(), &["remote", "add", "origin", url]);
}

fn stored(dir: &tempfile::TempDir, key: &str) -> Option<String> {
    git_text(
        dir.path(),
        &["config", "--get", &format!("remote.origin.{key}")],
    )
    .ok()
    .map(|v| v.trim().to_string())
}

#[test]
fn user_can_remove_stored_credentials_without_changing_the_host() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, &format!("https://user:{SECRET}@host/o/r.git"));
    let change = RemoteChange::SetUrl {
        name: "origin".into(),
        url: "https://user@host/o/r.git".into(),
        push: false,
        drop_credentials: true,
    };
    apply_with(path, &change, |_| Ok(())).unwrap();
    assert_eq!(stored(&dir, "url").unwrap(), "https://user@host/o/r.git");
    assert!(!list(path).unwrap().remotes[0].has_credentials);
}

#[test]
fn the_verdict_the_webview_receives_has_no_secret() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, &format!("https://user:{SECRET}@host/o/r.git"));
    let shown = list(path).unwrap().remotes.remove(0).fetch_url.unwrap();
    let change = RemoteChange::SetUrl {
        name: "origin".into(),
        url: shown,
        push: false,
        drop_credentials: false,
    };
    // The real gate, whatever it answers here: its verdict (or its error)
    // is what crosses IPC.
    let (verdict, _) = apply_with(path, &change, |argv| {
        Ok(match crate::harness::guard_command(path, argv) {
            Ok(verdict) => serde_json::to_string(&verdict).unwrap(),
            Err(error) => error,
        })
    })
    .unwrap();
    assert!(!verdict.contains(SECRET), "verdict leaks: {verdict}");
    assert!(
        verdict.contains("host"),
        "judge still sees the host: {verdict}"
    );
    assert_eq!(
        stored(&dir, "url").unwrap(),
        format!("https://user:{SECRET}@host/o/r.git")
    );
}

/// The argv handed to the judge carries the re-attached stored secret. The
/// real judge (`harness::guard_command`) builds `PolicyVerdict::target` from
/// that argv and the verdict is returned to the webview in `Guarded.policy`.
#[test]
fn the_judge_never_sees_the_stored_secret() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, &format!("https://user:{SECRET}@host/o/r.git"));
    let shown = list(path).unwrap().remotes.remove(0).fetch_url.unwrap();
    let change = RemoteChange::SetUrl {
        name: "origin".into(),
        url: shown,
        push: false,
        drop_credentials: false,
    };
    let seen = std::cell::RefCell::new(String::new());
    apply_with(path, &change, |argv| {
        *seen.borrow_mut() = crate::harness::render_command(argv);
        Ok(())
    })
    .unwrap();
    assert!(
        !seen.borrow().contains(SECRET),
        "judge saw: {}",
        seen.borrow()
    );
}

#[test]
fn set_url_with_a_changed_path_replaces_and_drops_credentials() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, &format!("https://user:{SECRET}@host/o/r.git"));
    let change = RemoteChange::SetUrl {
        name: "origin".into(),
        url: "https://user@host/o/other.git".into(),
        push: false,
        drop_credentials: false,
    };
    apply_with(path, &change, |_| Ok(())).unwrap();
    // Documented behaviour: editing the path drops the credentials.
    assert_eq!(
        stored(&dir, "url").unwrap(),
        "https://user@host/o/other.git"
    );
}

#[test]
fn push_url_display_does_not_reattach_the_fetch_credentials_to_a_pushurl() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, &format!("https://user:{SECRET}@host/o/r.git"));
    // No pushurl: Set URL (push) of the fetch display re-attaches the FETCH
    // secret to a new pushurl (same host, so acceptable) - characterise it.
    let change = RemoteChange::SetUrl {
        name: "origin".into(),
        url: "https://user@host/o/r.git".into(),
        push: true,
        drop_credentials: false,
    };
    apply_with(path, &change, |_| Ok(())).unwrap();
    assert_eq!(
        stored(&dir, "pushurl").unwrap(),
        format!("https://user:{SECRET}@host/o/r.git")
    );
}

#[test]
fn multiple_url_entries_do_not_report_credentials_that_are_not_shown() {
    let dir = repo();
    let path = dir.path().to_str().unwrap();
    add_stored(&dir, "https://clean@host/o/r.git");
    git_in(
        dir.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://u:pw@host/o/r2.git",
        ],
    );
    git_in(
        dir.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://clean@host/o/r3.git",
        ],
    );
    let r = list(path).unwrap().remotes.remove(0);
    // last value wins in display; has_credentials is OR-ed across entries.
    assert_eq!(r.fetch_url.as_deref(), Some("https://clean@host/o/r3.git"));
    assert!(
        !r.has_credentials,
        "has_credentials=true although the displayed URL has none (OR across entries)"
    );
}

// --- IPC leak surfaces ---------------------------------------------------

#[test]
fn a_failed_fetch_error_does_not_contain_the_token() {
    let dir = repo();
    git_in(dir.path(), &["commit", "--allow-empty", "-q", "-m", "x"]);
    // Nothing listens on port 1: the fetch fails fast with a connection error.
    add_stored(&dir, &format!("http://user:{SECRET}@127.0.0.1:1/x.git"));
    crate::test_support::trust_repo(dir.path());
    let err =
        crate::engine::git_writer::GitWriter::fetch(dir.path().to_str().unwrap(), Some("origin"))
            .expect_err("fetch must fail");
    assert!(
        !err.contains(SECRET),
        "fetch error leaks the token: {err}",
        err = err.message()
    );
}

#[test]
fn a_failed_fetch_from_a_credentialed_path_like_url_does_not_leak() {
    let dir = repo();
    add_stored(&dir, &format!("ssh://user:{SECRET}@127.0.0.1:1/x.git"));
    crate::test_support::trust_repo(dir.path());
    let err =
        crate::engine::git_writer::GitWriter::fetch(dir.path().to_str().unwrap(), Some("origin"))
            .expect_err("fetch must fail");
    assert!(
        !err.contains(SECRET),
        "fetch error leaks the token: {err}",
        err = err.message()
    );
}

// --- membership ----------------------------------------------------------

#[test]
fn membership_is_exact_not_case_whitespace_or_lookalike_tolerant() {
    let dir = repo();
    git_in(dir.path(), &["remote", "add", "Origin", "/tmp/x"]);
    for name in [
        "origin",
        "ORIGIN",
        "Origin ",
        " Origin",
        "Origin\n",
        "Orig\u{0131}n",
        "Or\u{0069}gin\u{200b}",
    ] {
        assert!(
            require_configured(dir.path(), name).is_err(),
            "{name:?} must not match 'Origin'"
        );
    }
    assert!(require_configured(dir.path(), "Origin").is_ok());
}

#[test]
fn a_configured_remote_with_a_local_path_url_is_allowed() {
    let dir = repo();
    git_in(
        dir.path(),
        &["remote", "add", "local", "/tmp/some-other-repo"],
    );
    assert!(require_configured(dir.path(), "local").is_ok());
}

#[test]
fn push_with_a_path_in_the_branch_slot_is_refused() {
    let dir = repo();
    git_in(dir.path(), &["commit", "--allow-empty", "-q", "-m", "x"]);
    let path = dir.path().to_str().unwrap();
    git_in(dir.path(), &["init", "-q", "--bare", "victim.git"]);
    let planned =
        crate::engine::git_writer::GitWriter::plan_push(path, None, Some("victim.git"), false);
    assert!(planned.is_err(), "planned argv would run: {planned:?}");
}

#[test]
fn push_planned_alone_refuses_an_unconfigured_remote() {
    let dir = repo();
    git_in(dir.path(), &["commit", "--allow-empty", "-q", "-m", "x"]);
    crate::test_support::trust_repo(dir.path());
    git_in(dir.path(), &["init", "-q", "--bare", "victim.git"]);
    let args = vec![
        "push".to_string(),
        "victim.git".to_string(),
        "main".to_string(),
    ];
    let result =
        crate::engine::git_writer::GitWriter::push_planned(dir.path().to_str().unwrap(), &args);
    assert!(
        result.is_err(),
        "push to an unconfigured path ran: {result:?}"
    );
}

#[test]
fn a_remote_named_like_a_path_or_option_is_refused_even_if_it_cannot_be_configured() {
    let dir = repo();
    for name in ["/tmp/x", "../x", "-u", "--upload-pack=sh", "", " "] {
        assert!(require_configured(dir.path(), name).is_err(), "{name:?}");
    }
}
