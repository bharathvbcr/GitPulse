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
    /// Lines the user can run themselves for what Connect leaves alone (switch
    /// a registration or a link, remove an older copy). Never run by Connect.
    pub commands: Vec<String>,
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
        // Run outside any repository, so a project-scoped `.mcp.json` in the
        // app's working directory cannot change what `mcp get` reports.
        cmd.current_dir(std::env::temp_dir());
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
    /// Where `cargo install` (so `bun run mcp:install`) puts binaries, to name
    /// an older copy that can shadow the link on PATH.
    pub cargo_bin: PathBuf,
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

impl Agent {
    /// The agent's own line that removes the registration, for the user to run.
    fn remove_line(self, scope: Option<&str>) -> String {
        let mut line = format!("{} mcp remove {SERVER_NAME}", self.key());
        if let (Agent::Claude, Some(scope)) = (self, scope) {
            line.push_str(" -s ");
            line.push_str(scope);
        }
        line
    }
}

/// Connect writes to the profile `claude` reads with no `CLAUDE_CONFIG_DIR`
/// set (a GUI app inherits none), which may not be the one a session uses.
const CLAUDE_PROFILE_NOTE: &str = "Connect runs `claude` with its default profile; if you start \
     Claude Code with CLAUDE_CONFIG_DIR, use Copy setup prompt from that session instead.";

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        format!("'{}'", arg.replace('\'', "'\\''"))
    } else {
        arg.to_string()
    }
}

fn shell_words(program: &str, args: &[String]) -> String {
    let mut line = program.to_string();
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
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

/// Why a link can be in place and still not be found. The app cannot read the
/// PATH of the shell an agent starts from (a GUI app's own PATH differs), so
/// it says what depends on it instead of guessing.
const PATH_NOTE: &str = "The MCP registration uses the full path and works either way; \
     Claude Code hooks call gitpulse-hook by name, so ~/.local/bin must be on your shell's PATH.";

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

/// What a link at `dest` to `source` is now, without touching anything, plus
/// the line that would repoint a link Connect leaves alone.
fn inspect_link(dest: &Path, source: &Path) -> (ConnectActionState, String, Vec<String>) {
    let (state, detail) = inspect_link_state(dest, source);
    let repoint = state == ConnectActionState::Blocked
        && std::fs::symlink_metadata(dest).is_ok_and(|m| m.file_type().is_symlink());
    let commands = if repoint {
        vec![format!(
            "ln -sfn {} {}",
            shell_quote(&source.to_string_lossy()),
            shell_quote(&dest.to_string_lossy())
        )]
    } else {
        Vec::new()
    };
    (state, detail, commands)
}

fn inspect_link_state(dest: &Path, source: &Path) -> (ConnectActionState, String) {
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
            format!("Creates a link to {}. {PATH_NOTE}", source.display()),
        ),
        Err(e) => (
            ConnectActionState::Blocked,
            format!("could not inspect {}: {e}", dest.display()),
        ),
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(dest) {
            Ok(target) if same_file(&target, source) => (
                ConnectActionState::Done,
                format!("Already linked to this app. {PATH_NOTE}"),
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
    /// Registered with this app's command; `scope` as for `Different`.
    Current {
        scope: Option<&'static str>,
    },
    /// Registered with another command; `scope` is Claude Code's, when shown.
    Different {
        command: String,
        scope: Option<&'static str>,
    },
    Unknown(String),
}

/// The scope out of `claude mcp get`'s text (`Scope: User config (…)`), as
/// `claude mcp remove -s` spells it.
fn claude_scope(output: &str) -> Option<&'static str> {
    let word = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Scope:"))?
        .split_whitespace()
        .next()?
        .to_ascii_lowercase();
    match word.as_str() {
        "user" => Some("user"),
        "local" => Some("local"),
        "project" => Some("project"),
        _ => None,
    }
}

/// Whether an enabled Claude Code plugin already provides the server.
enum PluginServer {
    None,
    /// The providing plugin's id, e.g. `gitpulse@gitpulse`.
    Provided(String),
    Unknown(String),
}

/// Reads `claude plugin list --json` (no server is started, unlike
/// `claude mcp list`). A plugin counts when it is enabled and serves a server
/// named `gitpulse` or one whose command is `gitpulse-mcp`.
fn claude_plugin_server(program: &str, runner: &dyn CliRunner) -> PluginServer {
    let args: Vec<String> = vec!["plugin".into(), "list".into(), "--json".into()];
    let run = match runner.run(program, &args) {
        Ok(run) => run,
        Err(e) => return PluginServer::Unknown(cap(&e)),
    };
    if !run.success {
        return PluginServer::Unknown(cap(&run.output));
    }
    let Ok(serde_json::Value::Array(plugins)) = serde_json::from_str(run.output.trim()) else {
        return PluginServer::Unknown("its plugin list was not readable".into());
    };
    let serves_gitpulse = |servers: &serde_json::Map<String, serde_json::Value>| {
        servers.iter().any(|(name, server)| {
            name == SERVER_NAME
                || server
                    .get("command")
                    .and_then(|c| c.as_str())
                    .and_then(|c| Path::new(c).file_name())
                    .is_some_and(|f| f == "gitpulse-mcp")
        })
    };
    plugins
        .iter()
        .find(|p| {
            p.get("enabled").and_then(|e| e.as_bool()) == Some(true)
                && p.get("mcpServers")
                    .and_then(|m| m.as_object())
                    .is_some_and(serves_gitpulse)
        })
        .map(|p| {
            PluginServer::Provided(
                p.get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("a GitPulse plugin")
                    .to_string(),
            )
        })
        .unwrap_or(PluginServer::None)
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
    let scope = match agent {
        Agent::Claude => claude_scope(&run.output),
        Agent::Codex => None,
    };
    match registered {
        Some(found) if same_file(Path::new(&found), Path::new(command)) => {
            Registration::Current { scope }
        }
        Some(found) => Registration::Different {
            command: found,
            scope,
        },
        None => Registration::Different {
            command: "a command GitPulse could not read".into(),
            scope,
        },
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
                let (state, mut detail, mut commands) = inspect_link(&dest, source);
                let older = env.cargo_bin.join(name);
                if std::fs::symlink_metadata(&older).is_ok() && !same_file(&older, source) {
                    detail.push_str(&format!(
                        " Also found {}, an older copy from `bun run mcp:install`; whichever \
                         directory comes first on PATH wins. If you use this app's copy, remove \
                         it with `cargo uninstall gitpulse`.",
                        older.display()
                    ));
                    commands.push("cargo uninstall gitpulse".into());
                }
                plan.actions.push(ConnectAction {
                    id: format!("link:{name}"),
                    kind: ConnectActionKind::Link,
                    agent: None,
                    title: format!("Link {name} into ~/.local/bin"),
                    target: dest.to_string_lossy().into_owned(),
                    state,
                    detail,
                    commands,
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
            let add_line = shell_words(agent.key(), &agent.add_args(&command));
            let registered = registration(agent, &program, env.runner, &command);
            let plugin = match agent {
                Agent::Claude => claude_plugin_server(&program, env.runner),
                Agent::Codex => PluginServer::None,
            };
            let label = agent.label();
            let (state, mut detail, commands) = match (registered, plugin) {
                (Registration::Unknown(why), _) => (
                    ConnectActionState::Blocked,
                    format!("Could not check {label}'s registry: {why}"),
                    Vec::new(),
                ),
                // Registering anyway could add a second gitpulse server beside
                // the plugin's; not knowing is not "no plugin".
                (_, PluginServer::Unknown(why)) => (
                    ConnectActionState::Blocked,
                    format!(
                        "Could not check whether GitPulse's {label} plugin already provides the \
                         server ({why}), so nothing is registered, to avoid a second {SERVER_NAME} \
                         server. If the plugin is not installed, run the line below yourself."
                    ),
                    vec![add_line.clone()],
                ),
                (Registration::Absent, PluginServer::Provided(id)) => (
                    ConnectActionState::Done,
                    format!(
                        "Provided by the {id} plugin, which starts gitpulse-mcp by name, so it \
                         needs ~/.local/bin (the link above) on your shell's PATH. Not registered \
                         a second time."
                    ),
                    Vec::new(),
                ),
                (
                    Registration::Current { scope } | Registration::Different { scope, .. },
                    PluginServer::Provided(id),
                ) => (
                    ConnectActionState::Blocked,
                    format!(
                        "{label} has two {SERVER_NAME} servers: the {id} plugin's and a \
                             `{SERVER_NAME}` registration. Keep one; to keep the plugin's, remove \
                             the registration."
                    ),
                    vec![agent.remove_line(scope)],
                ),
                (Registration::Absent, PluginServer::None) => (
                    ConnectActionState::Todo,
                    format!("Registers {SERVER_NAME} through {label}'s own CLI."),
                    Vec::new(),
                ),
                (Registration::Current { .. }, PluginServer::None) => (
                    ConnectActionState::Done,
                    format!("Already registered with {label}."),
                    Vec::new(),
                ),
                (
                    Registration::Different {
                        command: found,
                        scope,
                    },
                    PluginServer::None,
                ) => (
                    ConnectActionState::Blocked,
                    format!(
                        "Left alone: {SERVER_NAME} is already registered with {label} as \
                         `{found}`. To use this app's copy instead, run the lines below."
                    ),
                    vec![agent.remove_line(scope), add_line.clone()],
                ),
            };
            if matches!(agent, Agent::Claude) {
                detail.push(' ');
                detail.push_str(CLAUDE_PROFILE_NOTE);
            }
            plan.actions.push(ConnectAction {
                id: format!("register:{}", agent.key()),
                kind: ConnectActionKind::Register,
                agent: Some(agent.key().to_string()),
                title: format!("Register with {}", agent.label()),
                target: add_line,
                state,
                detail,
                commands,
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
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".cargo"))
        .join("bin");
    let env = ConnectEnv {
        home,
        cargo_bin,
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
