//! Connect an installed agent (Claude Code, Codex) to the GitPulse MCP server.
//!
//! A DMG install ships `gitpulse-mcp` and `gitpulse-hook` inside the app bundle
//! and puts neither on `PATH`, so an agent cannot start them. This module builds
//! a PLAN (no side effects) and applies it only when the caller asks:
//!
//! 1. symlink both binaries into `~/.local/bin` (never overwriting a regular
//!    file or a link that points somewhere else; idempotent when correct);
//! 2. register the server with each agent CLI that is installed, through the
//!    agent's own `mcp add` — never by editing its configuration by hand.
//!
//! The flags used come from the CLIs' own help on this machine:
//! `claude mcp add [options] <name> <commandOrUrl> [args...]` with
//! `-s, --scope <scope>` (local, user, project), and
//! `codex mcp add [OPTIONS] <NAME> (--url <URL> | -- <COMMAND>...)`.
//!
//! App-level, not repo-level: no repository is involved, so nothing here goes
//! through the repository-trust guard. Every child is bounded with
//! [`process::run_bounded_capped`], as the tool installer's probes are.

use crate::engine::process;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The name the server is registered under, matching what existing setups use.
pub const SERVER_NAME: &str = "gitpulse";

const CLI_DEADLINE: Duration = Duration::from_secs(30);
const CLI_OUTPUT_CAP: usize = 64 * 1024;
const MESSAGE_CAP: usize = 600;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectActionKind {
    Link,
    Register,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectActionState {
    /// Applying would do this.
    Todo,
    /// Already in place; applying leaves it alone.
    Done,
    /// Cannot be done safely; applying leaves it alone and says why.
    Blocked,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ConnectAction {
    pub id: String,
    pub kind: ConnectActionKind,
    /// `claude` / `codex` for a registration; `None` for a link.
    pub agent: Option<String>,
    pub title: String,
    /// The path this touches, or the command line it would run.
    pub target: String,
    pub state: ConnectActionState,
    pub detail: String,
}

/// The agent's own CLI lines, for the paste-into-your-agent prompt. Built here
/// so the prompt and the Connect button can never disagree on the argv.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AgentCommand {
    pub agent: String,
    pub label: String,
    pub add: String,
    pub verify: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ConnectPlan {
    /// False when nothing can be offered (no bundled binary was found).
    pub available: bool,
    pub reason: String,
    pub mcp_path: String,
    pub hook_path: String,
    /// Agent CLIs found on this machine, by key.
    pub agents_found: Vec<String>,
    /// Registration and verification lines for every supported agent, found
    /// on this machine or not. Empty when the server binary was not found.
    pub agent_commands: Vec<AgentCommand>,
    pub actions: Vec<ConnectAction>,
}

impl ConnectPlan {
    /// Whether applying would change anything.
    pub fn has_work(&self) -> bool {
        self.actions
            .iter()
            .any(|a| a.state == ConnectActionState::Todo)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ConnectOutcome {
    pub results: Vec<ActionResult>,
    /// The plan re-read after applying, so the UI shows what is left.
    pub plan: ConnectPlan,
}

/// Combined stdout and stderr of one finished agent-CLI call.
pub struct CliRun {
    pub success: bool,
    pub output: String,
}

/// Runs an agent CLI. A trait so tests assert the exact argv without a binary.
pub trait CliRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CliRun, String>;
}

/// The real runner: bounded by a deadline and an output cap.
pub struct BoundedCliRunner;

impl CliRunner for BoundedCliRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CliRun, String> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        let run = process::run_bounded_capped(cmd, program, CLI_DEADLINE, None, CLI_OUTPUT_CAP)
            .map_err(|e| e.message().to_string())?
            .require_complete(program)
            .map_err(|e| e.message().to_string())?;
        let mut output = String::from_utf8_lossy(&run.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&run.stderr));
        Ok(CliRun {
            success: run.success,
            output,
        })
    }
}

/// Everything the plan reads from the machine, injectable for tests.
pub struct ConnectEnv<'a> {
    pub home: PathBuf,
    pub mcp: Result<PathBuf, String>,
    pub hook: Result<PathBuf, String>,
    pub find_tool: &'a dyn Fn(&str) -> Option<String>,
    pub runner: &'a dyn CliRunner,
}

#[derive(Clone, Copy)]
enum Agent {
    Claude,
    Codex,
}

const AGENTS: [Agent; 2] = [Agent::Claude, Agent::Codex];

impl Agent {
    fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
    /// `claude mcp get <name>` / `codex mcp get <name> --json`.
    fn get_args(self) -> Vec<String> {
        let mut args = vec!["mcp".into(), "get".into(), SERVER_NAME.into()];
        if matches!(self, Agent::Codex) {
            args.push("--json".into());
        }
        args
    }
    /// Claude is registered at user scope so it applies in every project;
    /// Codex has no scope flag, its registry is the one `~/.codex/config.toml`.
    fn add_args(self, command: &str) -> Vec<String> {
        let mut args: Vec<String> = vec!["mcp".into(), "add".into()];
        if matches!(self, Agent::Claude) {
            args.extend(["--scope".into(), "user".into()]);
        }
        args.extend([SERVER_NAME.into(), "--".into(), command.into()]);
        args
    }
}

fn shell_words(program: &str, args: &[String]) -> String {
    let mut line = program.to_string();
    for arg in args {
        line.push(' ');
        if arg
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"')
        {
            line.push('\'');
            line.push_str(&arg.replace('\'', "'\\''"));
            line.push('\'');
        } else {
            line.push_str(arg);
        }
    }
    line
}

fn cap(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MESSAGE_CAP {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MESSAGE_CAP).collect();
    cut.push('…');
    cut
}

/// Whether `path` lies inside a macOS App Translocation mount.
fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("AppTranslocation"))
}

fn link_path(home: &Path, name: &str) -> PathBuf {
    home.join(".local").join("bin").join(name)
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// What a link at `dest` to `source` is now, without touching anything.
fn inspect_link(dest: &Path, source: &Path) -> (ConnectActionState, String) {
    if let Some(dir) = dest.parent() {
        if dir.exists() && !dir.is_dir() {
            return (
                ConnectActionState::Blocked,
                format!("{} exists and is not a directory", dir.display()),
            );
        }
    }
    match std::fs::symlink_metadata(dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            ConnectActionState::Todo,
            format!("Creates a link to {}", source.display()),
        ),
        Err(e) => (
            ConnectActionState::Blocked,
            format!("could not inspect {}: {e}", dest.display()),
        ),
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(dest) {
            Ok(target) if same_file(&target, source) => (
                ConnectActionState::Done,
                "Already linked to this app.".into(),
            ),
            Ok(target) => (
                ConnectActionState::Blocked,
                format!(
                    "Left alone: it is a link to {}, not to this app.",
                    target.display()
                ),
            ),
            Err(e) => (
                ConnectActionState::Blocked,
                format!("could not read the link {}: {e}", dest.display()),
            ),
        },
        Ok(_) => (
            ConnectActionState::Blocked,
            "Left alone: a file you put there is not a link.".into(),
        ),
    }
}

enum Registration {
    Absent,
    Current,
    Different(String),
    Unknown(String),
}

/// The registered command out of `claude mcp get`'s text (`Command: <path>`).
fn claude_command(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Command:"))
        .map(|c| c.trim().to_string())
}

/// The registered command out of `codex mcp get --json`.
fn codex_command(output: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
    value
        .get("transport")?
        .get("command")?
        .as_str()
        .map(str::to_string)
}

fn registration(
    agent: Agent,
    program: &str,
    runner: &dyn CliRunner,
    command: &str,
) -> Registration {
    let run = match runner.run(program, &agent.get_args()) {
        Ok(run) => run,
        Err(e) => return Registration::Unknown(e),
    };
    if !run.success {
        // Both CLIs say `No MCP server named …` for a name they do not have;
        // any other failure is "could not check", never "not registered".
        return if run.output.contains("No MCP server named") {
            Registration::Absent
        } else {
            Registration::Unknown(cap(&run.output))
        };
    }
    let registered = match agent {
        Agent::Claude => claude_command(&run.output),
        Agent::Codex => codex_command(&run.output),
    };
    match registered {
        Some(found) if same_file(Path::new(&found), Path::new(command)) => Registration::Current,
        Some(found) => Registration::Different(found),
        None => Registration::Different("a command GitPulse could not read".into()),
    }
}

/// Builds the plan. Reads the filesystem and runs the agents' read-only
/// `mcp get`; changes nothing.
pub fn build_plan(env: &ConnectEnv) -> ConnectPlan {
    let mut plan = ConnectPlan {
        available: false,
        reason: String::new(),
        mcp_path: String::new(),
        hook_path: String::new(),
        agents_found: Vec::new(),
        agent_commands: Vec::new(),
        actions: Vec::new(),
    };
    // Without an absolute home every link path would resolve against the
    // process's working directory, so refuse rather than write there.
    if !env.home.is_absolute() {
        plan.reason = "GitPulse could not find your home directory (HOME is unset or not an \
                       absolute path), so it will not create links or register anything."
            .into();
        return plan;
    }
    // A quarantined app opened from Downloads runs from a randomized
    // App Translocation mount that disappears after quit; links and
    // registrations pointing there would break on the next launch.
    if let Some(moved) = [&env.mcp, &env.hook]
        .into_iter()
        .flatten()
        .find(|path| is_translocated(path))
    {
        plan.reason = format!(
            "GitPulse is running from a temporary macOS App Translocation location ({}). Move \
             GitPulse.app into /Applications, open it from there, then connect your agent.",
            moved.display()
        );
        return plan;
    }
    let mut notes: Vec<String> = Vec::new();
    for (name, found, slot) in [
        ("gitpulse-mcp", &env.mcp, &mut plan.mcp_path),
        ("gitpulse-hook", &env.hook, &mut plan.hook_path),
    ] {
        let dest = link_path(&env.home, name);
        match found {
            Ok(source) => {
                plan.available = true;
                *slot = source.to_string_lossy().into_owned();
                let (state, detail) = inspect_link(&dest, source);
                plan.actions.push(ConnectAction {
                    id: format!("link:{name}"),
                    kind: ConnectActionKind::Link,
                    agent: None,
                    title: format!("Link {name} into ~/.local/bin"),
                    target: dest.to_string_lossy().into_owned(),
                    state,
                    detail,
                });
            }
            Err(e) => notes.push(format!("{name}: {e}")),
        }
    }
    if !plan.available {
        plan.reason = format!(
            "GitPulse could not find its bundled agent binaries. {}",
            notes.join(" ")
        );
        return plan;
    }
    if let Ok(mcp) = &env.mcp {
        let command = mcp.to_string_lossy().into_owned();
        for agent in AGENTS {
            plan.agent_commands.push(AgentCommand {
                agent: agent.key().to_string(),
                label: agent.label().to_string(),
                add: shell_words(agent.key(), &agent.add_args(&command)),
                verify: format!("{} mcp list", agent.key()),
            });
            let Some(program) = (env.find_tool)(agent.key()) else {
                continue;
            };
            plan.agents_found.push(agent.key().to_string());
            let (state, detail) = match registration(agent, &program, env.runner, &command) {
                Registration::Absent => (
                    ConnectActionState::Todo,
                    format!(
                        "Registers {SERVER_NAME} through {}'s own CLI.",
                        agent.label()
                    ),
                ),
                Registration::Current => (
                    ConnectActionState::Done,
                    format!("Already registered with {}.", agent.label()),
                ),
                Registration::Different(found) => (
                    ConnectActionState::Blocked,
                    format!(
                        "Left alone: {SERVER_NAME} is already registered with {} as `{found}`.",
                        agent.label()
                    ),
                ),
                Registration::Unknown(why) => (
                    ConnectActionState::Blocked,
                    format!("Could not check {}'s registry: {why}", agent.label()),
                ),
            };
            plan.actions.push(ConnectAction {
                id: format!("register:{}", agent.key()),
                kind: ConnectActionKind::Register,
                agent: Some(agent.key().to_string()),
                title: format!("Register with {}", agent.label()),
                target: shell_words(agent.key(), &agent.add_args(&command)),
                state,
                detail,
            });
        }
    }
    for note in notes {
        plan.reason.push_str(&note);
        plan.reason.push(' ');
    }
    plan.reason = plan.reason.trim().to_string();
    plan
}

fn apply_link(dest: &Path, source: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, dest)
            .map_err(|e| format!("could not link {}: {e}", dest.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (dest, source);
        Err("links are not created on this platform".into())
    }
}

/// Applies the plan the machine would produce right now. The plan is rebuilt
/// here rather than trusted from the caller, so what is written is what was
/// checked a moment ago, and a state that changed since the preview is
/// respected.
pub fn apply(env: &ConnectEnv) -> ConnectOutcome {
    let plan = build_plan(env);
    let mut results = Vec::new();
    for action in &plan.actions {
        let result = match action.state {
            ConnectActionState::Done => ActionResult {
                id: action.id.clone(),
                ok: true,
                message: action.detail.clone(),
            },
            ConnectActionState::Blocked => ActionResult {
                id: action.id.clone(),
                ok: false,
                message: action.detail.clone(),
            },
            ConnectActionState::Todo => {
                let outcome = match action.kind {
                    ConnectActionKind::Link => {
                        let name = action.id.trim_start_matches("link:");
                        let source = if name == "gitpulse-mcp" {
                            &env.mcp
                        } else {
                            &env.hook
                        };
                        match source {
                            Ok(source) => apply_link(Path::new(&action.target), source),
                            Err(e) => Err(e.clone()),
                        }
                    }
                    ConnectActionKind::Register => apply_register(env, action),
                };
                match outcome {
                    Ok(()) => ActionResult {
                        id: action.id.clone(),
                        ok: true,
                        message: "Done.".into(),
                    },
                    Err(e) => ActionResult {
                        id: action.id.clone(),
                        ok: false,
                        message: cap(&e),
                    },
                }
            }
        };
        results.push(result);
    }
    ConnectOutcome {
        results,
        plan: build_plan(env),
    }
}

fn apply_register(env: &ConnectEnv, action: &ConnectAction) -> Result<(), String> {
    let key = action.agent.as_deref().unwrap_or_default();
    let agent = AGENTS
        .into_iter()
        .find(|a| a.key() == key)
        .ok_or_else(|| format!("unknown agent {key}"))?;
    let program =
        (env.find_tool)(key).ok_or_else(|| format!("{} is no longer installed", agent.label()))?;
    let mcp = env.mcp.as_ref().map_err(Clone::clone)?;
    let run = env
        .runner
        .run(&program, &agent.add_args(&mcp.to_string_lossy()))?;
    if run.success {
        Ok(())
    } else {
        Err(format!("{} refused: {}", agent.label(), run.output.trim()))
    }
}

/// The plan for this machine and this running app.
pub fn plan_for_this_app() -> ConnectPlan {
    with_real_env(build_plan)
}

/// Applies the plan for this machine and this running app.
pub fn apply_for_this_app() -> ConnectOutcome {
    with_real_env(apply)
}

fn with_real_env<T>(f: impl FnOnce(&ConnectEnv) -> T) -> T {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    #[cfg(unix)]
    let (mcp, hook) = (
        crate::insights::bundled_binary("GITPULSE_MCP_PATH", "gitpulse-mcp"),
        crate::insights::bundled_binary("GITPULSE_HOOK_PATH", "gitpulse-hook"),
    );
    #[cfg(not(unix))]
    let (mcp, hook) = (
        Err::<PathBuf, String>("connecting an agent is not available on this platform yet".into()),
        Err::<PathBuf, String>("connecting an agent is not available on this platform yet".into()),
    );
    let find = |name: &str| process::find_external_tool(name);
    let env = ConnectEnv {
        home,
        mcp,
        hook,
        find_tool: &find,
        runner: &BoundedCliRunner,
    };
    f(&env)
}

#[cfg(test)]
#[path = "agent_connect_tests.rs"]
mod tests;
