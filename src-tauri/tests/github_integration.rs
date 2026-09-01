//! Integration coverage for the GitHub module's command construction.
//!
//! Every mutating GitHub operation is an argv handed to `gh`. That argv is the
//! injection surface: a selector or ref that slips a `--flag` past validation
//! changes the command the user's credentials execute. These tests exercise
//! the argv builders directly, so they run without `gh` installed and without
//! network access, and they assert what would actually reach the process.

use gitpulse_lib::github::actions::{
    cancel_run_argv, parse_workflow_list, rerun_run_argv, trigger_workflow_argv,
    validate_workflow_selector,
};
use gitpulse_lib::github::{parse_github_remote_url, pick_github_remote, GitHubRepoRef};

fn remote() -> GitHubRepoRef {
    parse_github_remote_url("https://github.com/bharathvbcr/GitPulse.git").expect("a github remote")
}

#[test]
fn parses_the_remote_forms_git_actually_writes() {
    for url in [
        "https://github.com/owner/repo.git",
        "https://github.com/owner/repo",
        "git@github.com:owner/repo.git",
        "ssh://git@github.com/owner/repo.git",
    ] {
        let parsed = parse_github_remote_url(url).unwrap_or_else(|| panic!("{url} must parse"));
        assert_eq!(parsed.owner, "owner", "{url}");
        assert_eq!(parsed.name, "repo", "{url}");
    }
}

#[test]
fn keeps_the_host_rather_than_assuming_github_com() {
    // The parser is host-agnostic on purpose: GitHub Enterprise lives on a
    // private hostname, and gh is pointed at it by `--repo host/owner/name`.
    // So a non-github host parses; what matters is that the host survives
    // rather than being silently rewritten to github.com.
    let ghe = parse_github_remote_url("https://github.example.com/owner/repo.git")
        .expect("an enterprise remote parses");
    assert_eq!(ghe.host, "github.example.com");
    assert_eq!(ghe.slug(), "owner/repo");
    assert!(ghe.html_url().starts_with("https://github.example.com/"));

    let gitlab = parse_github_remote_url("https://gitlab.com/owner/repo.git")
        .expect("parsing is by URL shape, not by host allowlist");
    assert_eq!(gitlab.host, "gitlab.com");
}

#[test]
fn rejects_input_that_is_not_a_remote_url() {
    for url in [
        "not-a-url",
        "",
        "   ",
        "file:///tmp/repo",
        "https://",
        "https://host/owner",
    ] {
        assert!(
            parse_github_remote_url(url).is_none(),
            "{url:?} must not parse"
        );
    }
}

#[test]
fn strips_the_port_from_ssh_style_remotes_but_not_from_the_owner() {
    let parsed = parse_github_remote_url("ssh://git@github.com:22/owner/repo.git")
        .expect("an ssh remote with a port parses");
    assert_eq!(parsed.host, "github.com");
    assert_eq!(parsed.owner, "owner");
    assert_eq!(parsed.name, "repo");
}

#[test]
fn a_flag_shaped_selector_never_becomes_an_argument() {
    // The boundary the selector actually defends: a value that gh would read
    // as a flag rather than as a workflow, and control characters.
    for hostile in [
        "--repo",
        "-R",
        "--json=evil",
        "-",
        "",
        "   ",
        "ci.yml\nrun: evil",
    ] {
        assert!(
            validate_workflow_selector(hostile).is_err(),
            "selector {hostile:?} must be rejected"
        );
        assert!(
            trigger_workflow_argv(&remote(), hostile, "main").is_err(),
            "selector {hostile:?} must not reach an argv"
        );
    }
}

#[test]
fn shell_metacharacters_in_a_selector_are_inert_rather_than_rejected() {
    // Commands are executed as a direct argv with no shell, so `;` and `|`
    // carry no meaning: they travel as one literal argument that gh fails to
    // resolve as a workflow. Documented here because the permissiveness looks
    // alarming out of context, and because the guarantee it rests on — never
    // building a shell string — is the thing that must not regress.
    for inert in ["ci.yml; rm -rf /", "../../etc/passwd", "a|b", "$(whoami)"] {
        let selector = validate_workflow_selector(inert).expect("accepted as a literal");
        let argv = trigger_workflow_argv(&remote(), inert, "main").expect("reaches an argv");
        assert!(
            argv.iter().filter(|arg| *arg == &selector).count() == 1,
            "the selector must travel as exactly one argument: {argv:?}"
        );
    }
}

#[test]
fn a_flag_shaped_ref_never_becomes_an_argument() {
    for hostile in ["--ref", "-R", "..", "refs/heads/x y", "", "@{now}"] {
        assert!(
            trigger_workflow_argv(&remote(), "ci.yml", hostile).is_err(),
            "ref {hostile:?} must not reach an argv"
        );
    }
}

#[test]
fn a_valid_trigger_targets_the_repository_explicitly() {
    let argv = trigger_workflow_argv(&remote(), "ci.yml", "main").expect("valid input");
    assert_eq!(argv[0], "gh");
    assert_eq!(&argv[1..4], &["workflow", "run", "ci.yml"]);
    assert!(argv.contains(&"--ref".to_string()));
    assert!(argv.contains(&"main".to_string()));
    // Without an explicit repo, gh would infer one from the working directory.
    assert!(
        argv.windows(2).any(|w| w[0] == "--repo" || w[0] == "-R"),
        "argv must name the repository: {argv:?}"
    );
}

#[test]
fn run_ids_are_numeric_so_they_cannot_carry_a_flag() {
    let argv = rerun_run_argv(&remote(), 42).expect("valid run id");
    assert!(argv.contains(&"42".to_string()));
    assert_eq!(argv[0], "gh");

    let cancel = cancel_run_argv(&remote(), 42).expect("valid run id");
    assert_eq!(cancel[0], "gh");
    assert!(cancel.contains(&"42".to_string()));

    // Zero is not a run id GitHub issues; it should be refused rather than sent.
    assert!(rerun_run_argv(&remote(), 0).is_err());
    assert!(cancel_run_argv(&remote(), 0).is_err());
}

#[test]
fn workflow_list_parsing_survives_malformed_payloads() {
    // gh returning something unexpected must be an error, never a panic.
    for payload in [
        &b""[..],
        &b"not json"[..],
        &b"{}"[..],
        &b"null"[..],
        &br#"[{"name":1}]"#[..],
        &[0xff, 0xfe, 0x00][..],
    ] {
        assert!(
            parse_workflow_list(payload, 20).is_err(),
            "payload {payload:?} should be reported as unparseable"
        );
    }
    let (empty, truncated) = parse_workflow_list(b"[]", 20).expect("an empty list is valid");
    assert!(empty.is_empty() && !truncated);
}

#[test]
fn workflow_list_reports_truncation_rather_than_silently_dropping_rows() {
    let many = (0..50)
        .map(|i| {
            format!(
                r#"{{"id":{i},"name":"w{i}","path":".github/workflows/w{i}.yml","state":"active"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let (shown, truncated) =
        parse_workflow_list(format!("[{many}]").as_bytes(), 10).expect("valid payload");
    assert_eq!(shown.len(), 10);
    assert!(truncated, "a capped list must say it was capped");
}

#[test]
fn a_credential_embedded_in_a_remote_never_survives_parsing() {
    // A remote can carry a token: `https://ghp_TOKEN@github.com/owner/repo` is
    // a normal way to configure one. Keeping it in the host put the token into
    // `html_url()` — shown in the UI and opened in a browser — and into the
    // `--repo host/owner/name` argument handed to gh, where it would appear in
    // the process list.
    for url in [
        "https://ghp_SECRETTOKEN@github.com/owner/repo.git",
        "https://user:ghp_SECRETTOKEN@github.com/owner/repo.git",
        "https://x-access-token:ghs_SECRETTOKEN@github.com/owner/repo.git",
        "http://token@github.com/owner/repo",
        "ssh://git@github.com/owner/repo.git",
    ] {
        let parsed = parse_github_remote_url(url).unwrap_or_else(|| panic!("{url} should parse"));
        assert_eq!(parsed.host, "github.com", "{url} kept userinfo in the host");
        assert_eq!(parsed.slug(), "owner/repo", "{url}");
        for leaky in ["SECRETTOKEN", "ghp_", "ghs_", "x-access-token", "@"] {
            assert!(
                !parsed.html_url().contains(leaky),
                "{url} leaked {leaky:?} into {}",
                parsed.html_url()
            );
        }
        // The argv handed to gh is built from the host, so check it too.
        let argv = trigger_workflow_argv(&parsed, "ci.yml", "main").expect("valid input");
        assert!(
            !argv.iter().any(|arg| arg.contains("SECRETTOKEN")),
            "{url} leaked a credential into {argv:?}"
        );
    }
}

#[test]
fn stripping_userinfo_does_not_disturb_ordinary_remotes() {
    for (url, host) in [
        ("https://github.com/owner/repo.git", "github.com"),
        ("git@github.com:owner/repo.git", "github.com"),
        ("ssh://git@github.com:22/owner/repo.git", "github.com"),
        (
            "https://github.example.com/owner/repo.git",
            "github.example.com",
        ),
    ] {
        let parsed = parse_github_remote_url(url).unwrap_or_else(|| panic!("{url} should parse"));
        assert_eq!(parsed.host, host, "{url}");
        assert_eq!(parsed.slug(), "owner/repo", "{url}");
    }
}

#[test]
fn a_credential_in_real_remote_output_never_reaches_the_parsed_ref() {
    // The realistic entry point: this is what `git remote -v` prints for a
    // repository configured with a token, tab-separated exactly as git emits
    // it, with the fetch and push lines git always writes as a pair.
    let output = concat!(
        "origin\thttps://ghp_SECRETTOKEN@github.com/owner/repo.git (fetch)\n",
        "origin\thttps://ghp_SECRETTOKEN@github.com/owner/repo.git (push)\n",
    );
    let picked = pick_github_remote(output).expect("a github remote");
    assert_eq!(picked.host, "github.com");
    assert_eq!(picked.slug(), "owner/repo");
    assert!(
        !picked.html_url().contains("SECRETTOKEN"),
        "{}",
        picked.html_url()
    );

    // A push-only credentialed remote alongside a clean fetch remote must not
    // contribute its userinfo either.
    let mixed = concat!(
        "origin\thttps://github.com/owner/repo.git (fetch)\n",
        "origin\thttps://ghp_SECRETTOKEN@github.com/owner/repo.git (push)\n",
    );
    let picked = pick_github_remote(mixed).expect("a github remote");
    assert!(!picked.html_url().contains("SECRETTOKEN"));
}
