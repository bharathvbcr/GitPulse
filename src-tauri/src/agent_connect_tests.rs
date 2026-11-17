//! Plan and apply against a temp HOME and a recording runner; the real `~` and
//! the real agent CLIs are never touched.

use super::*;
use std::cell::RefCell;

type Call = (String, Vec<String>);

struct FakeRunner {
    calls: RefCell<Vec<Call>>,
    /// Answers `get` calls: (agent program, success, output).
    get: RefCell<Vec<(String, bool, String)>>,
    /// Answers `claude plugin list --json`: (success, output). No plugins by default.
    plugins: RefCell<(bool, String)>,
    add_ok: bool,
}

impl FakeRunner {
    fn new(get: Vec<(&str, bool, &str)>) -> Self {
        FakeRunner {
            calls: RefCell::new(Vec::new()),
            get: RefCell::new(
                get.into_iter()
                    .map(|(p, s, o)| (p.to_string(), s, o.to_string()))
                    .collect(),
            ),
            plugins: RefCell::new((true, "[]".to_string())),
            add_ok: true,
        }
    }
    fn with_plugins(self, success: bool, output: &str) -> Self {
        *self.plugins.borrow_mut() = (success, output.to_string());
        self
    }
    fn adds(&self) -> Vec<Call> {
        self.calls
            .borrow()
            .iter()
            .filter(|(_, a)| a.get(1).map(String::as_str) == Some("add"))
            .cloned()
            .collect()
    }
}

impl CliRunner for FakeRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CliRun, String> {
        self.calls
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        if args.first().map(String::as_str) == Some("plugin") {
            let (success, output) = self.plugins.borrow().clone();
            return Ok(CliRun { success, output });
        }
        if args.get(1).map(String::as_str) == Some("get") {
            let answers = self.get.borrow();
            return match answers.iter().find(|(p, _, _)| p == program) {
                Some((_, success, output)) => Ok(CliRun {
                    success: *success,
                    output: output.clone(),
                }),
                None => Err("no answer scripted".into()),
            };
        }
        Ok(CliRun {
            success: self.add_ok,
            output: if self.add_ok {
                String::new()
            } else {
                "boom".into()
            },
        })
    }
}

const NOT_FOUND: &str = "No MCP server named \"gitpulse\"";

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    bundle: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let bundle = dir.path().join("GitPulse.app/Contents/MacOS");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bundle).unwrap();
    for name in ["gitpulse-mcp", "gitpulse-hook"] {
        std::fs::write(bundle.join(name), b"#!/bin/sh\n").unwrap();
    }
    Fixture {
        _dir: dir,
        home,
        bundle,
    }
}

fn env<'a>(
    fx: &Fixture,
    runner: &'a dyn CliRunner,
    find: &'a dyn Fn(&str) -> Option<String>,
) -> ConnectEnv<'a> {
    ConnectEnv {
        home: fx.home.clone(),
        cargo_bin: fx.home.join(".cargo/bin"),
        mcp: Ok(fx.bundle.join("gitpulse-mcp")),
        hook: Ok(fx.bundle.join("gitpulse-hook")),
        find_tool: find,
        runner,
    }
}

fn both(name: &str) -> Option<String> {
    Some(format!("/fake/bin/{name}"))
}

fn none(_: &str) -> Option<String> {
    None
}

fn action<'a>(plan: &'a ConnectPlan, id: &str) -> &'a ConnectAction {
    plan.actions.iter().find(|a| a.id == id).unwrap()
}

#[test]
fn plan_is_side_effect_free_and_lists_links_and_registrations() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![
        ("/fake/bin/claude", false, NOT_FOUND),
        ("/fake/bin/codex", false, NOT_FOUND),
    ]);
    let plan = build_plan(&env(&fx, &runner, &both));
    assert!(plan.available && plan.has_work());
    assert_eq!(plan.agents_found, ["claude", "codex"]);
    assert_eq!(plan.actions.len(), 4);
    assert!(plan
        .actions
        .iter()
        .all(|a| a.state == ConnectActionState::Todo));
    assert!(!fx.home.join(".local").exists(), "a plan must not write");
    assert!(runner.adds().is_empty(), "a plan must not register");
}

#[test]
fn register_argv_is_exact_for_each_agent() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![
        ("/fake/bin/claude", false, NOT_FOUND),
        ("/fake/bin/codex", false, NOT_FOUND),
    ]);
    apply(&env(&fx, &runner, &both));
    let mcp = fx
        .bundle
        .join("gitpulse-mcp")
        .to_string_lossy()
        .into_owned();
    let to_strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        runner.adds(),
        vec![
            (
                "/fake/bin/claude".to_string(),
                to_strings(&["mcp", "add", "--scope", "user", "gitpulse", "--", &mcp])
            ),
            (
                "/fake/bin/codex".to_string(),
                to_strings(&["mcp", "add", "gitpulse", "--", &mcp])
            ),
        ]
    );
    let gets: Vec<Call> = runner
        .calls
        .borrow()
        .iter()
        .filter(|(_, a)| a[1] == "get")
        .cloned()
        .collect();
    assert_eq!(gets[0].1, to_strings(&["mcp", "get", "gitpulse"]));
    assert_eq!(gets[1].1, to_strings(&["mcp", "get", "gitpulse", "--json"]));
}

#[cfg(unix)]
#[test]
fn apply_creates_links_and_a_second_apply_changes_nothing() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]);
    let only_claude = |n: &str| (n == "claude").then(|| "/fake/bin/claude".to_string());
    let first = apply(&env(&fx, &runner, &only_claude));
    assert!(first.results.iter().all(|r| r.ok), "{:?}", first.results);
    for name in ["gitpulse-mcp", "gitpulse-hook"] {
        let link = fx.home.join(".local/bin").join(name);
        assert_eq!(std::fs::read_link(&link).unwrap(), fx.bundle.join(name));
    }

    // Claude now reports the same command registered.
    let mcp = fx.bundle.join("gitpulse-mcp");
    let registered = format!("gitpulse:\n  Scope: User\n  Command: {}\n", mcp.display());
    let runner = FakeRunner::new(vec![("/fake/bin/claude", true, registered.as_str())]);
    let second = apply(&env(&fx, &runner, &only_claude));
    assert!(!second.plan.has_work());
    assert!(runner.adds().is_empty(), "idempotent: no second add");
    assert!(second.results.iter().all(|r| r.ok));
}

#[cfg(unix)]
#[test]
fn never_overwrites_a_file_or_a_link_pointing_elsewhere() {
    let fx = fixture();
    let bin = fx.home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("gitpulse-mcp"), b"mine").unwrap();
    let elsewhere = fx.home.join("other-hook");
    std::fs::write(&elsewhere, b"other").unwrap();
    std::os::unix::fs::symlink(&elsewhere, bin.join("gitpulse-hook")).unwrap();

    let runner = FakeRunner::new(vec![]);
    let out = apply(&env(&fx, &runner, &none));
    let mcp = action(&out.plan, "link:gitpulse-mcp");
    let hook = action(&out.plan, "link:gitpulse-hook");
    assert_eq!(mcp.state, ConnectActionState::Blocked);
    assert!(mcp.detail.contains("not a link"));
    assert_eq!(hook.state, ConnectActionState::Blocked);
    assert!(hook.detail.contains("not to this app"));
    assert!(out.results.iter().all(|r| !r.ok));
    assert_eq!(std::fs::read(bin.join("gitpulse-mcp")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read_link(bin.join("gitpulse-hook")).unwrap(),
        elsewhere
    );
}

#[test]
fn an_existing_registration_with_another_command_is_left_alone() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![(
        "/fake/bin/claude",
        true,
        "gitpulse:\n  Command: gitpulse-mcp\n",
    )]);
    let only_claude = |n: &str| (n == "claude").then(|| "/fake/bin/claude".to_string());
    let out = apply(&env(&fx, &runner, &only_claude));
    let reg = action(&out.plan, "register:claude");
    assert_eq!(reg.state, ConnectActionState::Blocked);
    assert!(reg.detail.contains("`gitpulse-mcp`"));
    assert!(runner.adds().is_empty());
}

#[test]
fn codex_registration_is_read_from_json() {
    let fx = fixture();
    let mcp = fx.bundle.join("gitpulse-mcp");
    let json = format!(
        r#"{{"name":"gitpulse","transport":{{"type":"stdio","command":"{}","args":[]}}}}"#,
        mcp.display()
    );
    let runner = FakeRunner::new(vec![("/fake/bin/codex", true, json.as_str())]);
    let only_codex = |n: &str| (n == "codex").then(|| "/fake/bin/codex".to_string());
    let plan = build_plan(&env(&fx, &runner, &only_codex));
    assert_eq!(
        action(&plan, "register:codex").state,
        ConnectActionState::Done
    );
}

#[test]
fn a_check_that_could_not_run_is_not_read_as_not_registered() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, "network timeout")]);
    let only_claude = |n: &str| (n == "claude").then(|| "/fake/bin/claude".to_string());
    let out = apply(&env(&fx, &runner, &only_claude));
    let reg = action(&out.plan, "register:claude");
    assert_eq!(reg.state, ConnectActionState::Blocked);
    assert!(reg.detail.contains("Could not check"));
    assert!(
        runner.adds().is_empty(),
        "unknown must never trigger an add"
    );
}

#[test]
fn a_refused_add_is_reported_per_action() {
    let fx = fixture();
    let mut runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]);
    runner.add_ok = false;
    let only_claude = |n: &str| (n == "claude").then(|| "/fake/bin/claude".to_string());
    let out = apply(&env(&fx, &runner, &only_claude));
    let result = out
        .results
        .iter()
        .find(|r| r.id == "register:claude")
        .unwrap();
    assert!(!result.ok);
    assert!(result.message.contains("boom"));
}

#[test]
fn without_agent_clis_only_links_are_offered() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![]);
    let plan = build_plan(&env(&fx, &runner, &none));
    assert!(plan.agents_found.is_empty());
    assert!(plan
        .actions
        .iter()
        .all(|a| a.kind == ConnectActionKind::Link));
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn nothing_is_offered_when_no_bundled_binary_was_found() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![]);
    let mut e = env(&fx, &runner, &both);
    e.mcp = Err("gitpulse-mcp is not next to this app.".into());
    e.hook = Err("gitpulse-hook is not next to this app.".into());
    let plan = build_plan(&e);
    assert!(!plan.available);
    assert!(plan.actions.is_empty());
    assert!(plan.reason.contains("not next to this app"));
    assert!(apply(&e).results.is_empty());
    assert!(!fx.home.join(".local").exists());
}

#[cfg(unix)]
#[test]
fn the_real_runner_passes_the_argv_through_a_bounded_child() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let script = dir.path().join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho ok\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args: Vec<String> = [
        "mcp",
        "add",
        "--scope",
        "user",
        "gitpulse",
        "--",
        "/x y/gitpulse-mcp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let run = BoundedCliRunner
        .run(&script.to_string_lossy(), &args)
        .unwrap();
    assert!(run.success && run.output.contains("ok"));
    let logged = std::fs::read_to_string(&log).unwrap();
    assert_eq!(logged.lines().collect::<Vec<_>>(), args);
}

#[test]
fn relative_or_empty_home_offers_nothing_and_writes_nothing() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]);
    for home in [PathBuf::new(), PathBuf::from("relative/home")] {
        let mut e = env(&fx, &runner, &both);
        e.home = home;
        let plan = build_plan(&e);
        assert!(!plan.available, "no absolute home must not be offered");
        assert!(plan.actions.is_empty());
        assert!(plan.reason.contains("home directory"), "{}", plan.reason);
        let outcome = apply(&e);
        assert!(outcome.results.is_empty(), "apply must do nothing");
    }
    assert!(!std::path::Path::new(".local/bin/gitpulse-mcp").exists());
    assert!(runner.adds().is_empty());
}

#[test]
fn translocated_bundle_is_refused_with_a_move_instruction() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]);
    let mut e = env(&fx, &runner, &both);
    let moved = PathBuf::from(
        "/private/var/folders/xy/T/AppTranslocation/ABCD/d/GitPulse.app/Contents/MacOS",
    );
    e.mcp = Ok(moved.join("gitpulse-mcp"));
    e.hook = Ok(moved.join("gitpulse-hook"));
    let plan = build_plan(&e);
    assert!(!plan.available);
    assert!(plan.actions.is_empty());
    assert!(plan.reason.contains("/Applications"), "{}", plan.reason);
    assert!(apply(&e).results.is_empty());
    assert!(!fx.home.join(".local").exists());
    assert!(runner.adds().is_empty());
}

#[test]
fn link_details_say_what_depends_on_path() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![]);
    let e = env(&fx, &runner, &none);
    let before = build_plan(&e);
    assert!(action(&before, "link:gitpulse-hook")
        .detail
        .contains("must be on your shell's PATH"));
    let after = apply(&e).plan;
    let done = action(&after, "link:gitpulse-hook");
    assert_eq!(done.state, ConnectActionState::Done);
    assert!(
        done.detail.contains("must be on your shell's PATH"),
        "{}",
        done.detail
    );
}

/// `claude plugin list --json` with GitPulse's plugin installed, in the shape
/// Claude Code 2026 prints (only the fields read here matter).
fn gitpulse_plugin(enabled: bool) -> String {
    format!(
        r#"[{{"id":"gitpulse@gitpulse","version":"1.4.0","scope":"user","enabled":{enabled},
            "mcpServers":{{"gitpulse":{{"type":"stdio","command":"gitpulse-mcp","args":[]}}}}}},
           {{"id":"other@market","enabled":true,"mcpServers":{{"other":{{"command":"x"}}}}}}]"#
    )
}

fn only_claude(n: &str) -> Option<String> {
    (n == "claude").then(|| "/fake/bin/claude".to_string())
}

#[test]
fn a_server_the_gitpulse_plugin_provides_is_not_registered_a_second_time() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)])
        .with_plugins(true, &gitpulse_plugin(true));
    let out = apply(&env(&fx, &runner, &only_claude));
    let reg = action(&out.plan, "register:claude");
    assert_eq!(reg.state, ConnectActionState::Done, "{}", reg.detail);
    assert!(reg.detail.contains("gitpulse@gitpulse"), "{}", reg.detail);
    assert!(reg.detail.contains("PATH"), "{}", reg.detail);
    assert!(runner.adds().is_empty(), "no duplicate user-scope server");
}

#[test]
fn a_disabled_plugin_does_not_count_as_providing_the_server() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)])
        .with_plugins(true, &gitpulse_plugin(false));
    let plan = build_plan(&env(&fx, &runner, &only_claude));
    assert_eq!(
        action(&plan, "register:claude").state,
        ConnectActionState::Todo
    );
}

#[test]
fn a_plugin_check_that_could_not_run_blocks_the_registration() {
    let fx = fixture();
    for (success, output) in [
        (false, "error: unknown command 'plugin'"),
        (true, "not json"),
    ] {
        let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)])
            .with_plugins(success, output);
        let out = apply(&env(&fx, &runner, &only_claude));
        let reg = action(&out.plan, "register:claude");
        assert_eq!(reg.state, ConnectActionState::Blocked, "{output}");
        assert!(reg.detail.contains("plugin"), "{}", reg.detail);
        assert!(runner.adds().is_empty());
    }
}

#[test]
fn plugin_and_registration_together_are_reported_as_two_servers() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![(
        "/fake/bin/claude",
        true,
        "gitpulse:\n  Scope: User config (available in all your projects)\n  Command: /old/gitpulse-mcp\n",
    )])
    .with_plugins(true, &gitpulse_plugin(true));
    let plan = build_plan(&env(&fx, &runner, &only_claude));
    let reg = action(&plan, "register:claude");
    assert_eq!(reg.state, ConnectActionState::Blocked);
    assert!(reg.detail.contains("two"), "{}", reg.detail);
    assert_eq!(reg.commands, ["claude mcp remove gitpulse -s user"]);
}

#[test]
fn a_different_registration_shows_the_commands_that_switch_it() {
    let fx = fixture();
    let mcp = fx
        .bundle
        .join("gitpulse-mcp")
        .to_string_lossy()
        .into_owned();
    let codex_json = r#"{"name":"gitpulse","transport":{"type":"stdio","command":"/Users/me/.cargo/bin/gitpulse-mcp","args":[]}}"#;
    let runner = FakeRunner::new(vec![
        (
            "/fake/bin/claude",
            true,
            "gitpulse:\n  Scope: Local config (private to you in this project)\n  Command: gitpulse-mcp\n",
        ),
        ("/fake/bin/codex", true, codex_json),
    ]);
    let out = apply(&env(&fx, &runner, &both));
    let claude = action(&out.plan, "register:claude");
    assert_eq!(claude.state, ConnectActionState::Blocked);
    assert_eq!(
        claude.commands,
        [
            "claude mcp remove gitpulse -s local".to_string(),
            format!("claude mcp add --scope user gitpulse -- {mcp}"),
        ]
    );
    let codex = action(&out.plan, "register:codex");
    assert_eq!(codex.state, ConnectActionState::Blocked);
    assert!(codex.detail.contains("/Users/me/.cargo/bin/gitpulse-mcp"));
    assert_eq!(
        codex.commands,
        [
            "codex mcp remove gitpulse".to_string(),
            format!("codex mcp add gitpulse -- {mcp}"),
        ]
    );
    assert!(runner.adds().is_empty(), "switching is the user's to run");
    assert!(
        !runner
            .calls
            .borrow()
            .iter()
            .any(|(_, a)| a.get(1).map(String::as_str) == Some("remove")),
        "Connect never removes a registration"
    );
}

#[cfg(unix)]
#[test]
fn a_link_pointing_elsewhere_shows_the_command_that_repoints_it() {
    let fx = fixture();
    let bin = fx.home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(
        "/Volumes/GitPulse/GitPulse.app/Contents/MacOS/gitpulse-hook",
        bin.join("gitpulse-hook"),
    )
    .unwrap();
    let runner = FakeRunner::new(vec![]);
    let plan = build_plan(&env(&fx, &runner, &none));
    let hook = action(&plan, "link:gitpulse-hook");
    assert_eq!(hook.state, ConnectActionState::Blocked);
    assert_eq!(
        hook.commands,
        [format!(
            "ln -sfn {} {}",
            fx.bundle.join("gitpulse-hook").display(),
            bin.join("gitpulse-hook").display()
        )]
    );
    assert!(action(&plan, "link:gitpulse-mcp").commands.is_empty());
}

#[test]
fn an_older_cargo_install_copy_is_named_beside_the_link() {
    let fx = fixture();
    let cargo_bin = fx.home.join(".cargo/bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    std::fs::write(cargo_bin.join("gitpulse-hook"), b"old").unwrap();
    let runner = FakeRunner::new(vec![]);
    let plan = build_plan(&env(&fx, &runner, &none));
    let hook = action(&plan, "link:gitpulse-hook");
    assert_eq!(
        hook.state,
        ConnectActionState::Todo,
        "the copy does not block the link"
    );
    assert!(
        hook.detail
            .contains(&cargo_bin.join("gitpulse-hook").display().to_string()),
        "{}",
        hook.detail
    );
    assert!(
        hook.detail.contains("bun run mcp:install"),
        "{}",
        hook.detail
    );
    assert_eq!(hook.commands, ["cargo uninstall gitpulse"]);
    let mcp = action(&plan, "link:gitpulse-mcp");
    assert!(mcp.commands.is_empty(), "no copy of gitpulse-mcp there");
}

#[test]
fn the_claude_registration_says_which_profile_it_writes() {
    let fx = fixture();
    let runner = FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]);
    let plan = build_plan(&env(&fx, &runner, &only_claude));
    let reg = action(&plan, "register:claude");
    assert!(reg.detail.contains("CLAUDE_CONFIG_DIR"), "{}", reg.detail);
    assert!(reg.detail.contains("Copy setup prompt"), "{}", reg.detail);
}

#[test]
fn json_answers_are_read_through_a_wrapper_notice_and_trailing_hook_noise() {
    // What a real `claude` prints here, stdout and stderr merged: a shell
    // wrapper's notice before the JSON and a SessionEnd hook line after it.
    let fx = fixture();
    let noisy = format!(
        "claude: ignoring CLAUDE_CONFIG_DIR from the shell; this directory has no seat pin\n{}\nSessionEnd hook [/x/hook.sh] failed: Hook cancelled\n",
        gitpulse_plugin(true)
    );
    let runner =
        FakeRunner::new(vec![("/fake/bin/claude", false, NOT_FOUND)]).with_plugins(true, &noisy);
    let plan = build_plan(&env(&fx, &runner, &only_claude));
    let reg = action(&plan, "register:claude");
    assert_eq!(reg.state, ConnectActionState::Done, "{}", reg.detail);

    let mcp = fx.bundle.join("gitpulse-mcp");
    let json = format!(
        "warning: something\n{{\"name\":\"gitpulse\",\"transport\":{{\"type\":\"stdio\",\"command\":\"{}\"}}}}\ntrailing\n",
        mcp.display()
    );
    let runner = FakeRunner::new(vec![("/fake/bin/codex", true, json.as_str())]);
    let only_codex = |n: &str| (n == "codex").then(|| "/fake/bin/codex".to_string());
    let plan = build_plan(&env(&fx, &runner, &only_codex));
    assert_eq!(
        action(&plan, "register:codex").state,
        ConnectActionState::Done
    );
}
