//! Plan and apply against a temp HOME and a recording runner; the real `~` and
//! the real agent CLIs are never touched.

use super::*;
use std::cell::RefCell;

type Call = (String, Vec<String>);

struct FakeRunner {
    calls: RefCell<Vec<Call>>,
    /// Answers `get` calls: (agent program, success, output).
    get: RefCell<Vec<(String, bool, String)>>,
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
            add_ok: true,
        }
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
