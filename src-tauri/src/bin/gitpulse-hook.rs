//! GitPulse hook executable.
//!
//! One binary, dispatched by its first argument, reading the host's hook JSON
//! on stdin and writing hook JSON on stdout. Every decision lives in
//! `gitpulse_lib::hooks` so the contract is unit-tested rather than only
//! exercised by spawning this process.
//!
//! Two rules shape this file:
//!
//! * **stdout is the protocol channel.** On a hook invocation nothing but the
//!   hook JSON goes down it. Diagnostics go to stderr, where a hook that exits
//!   0 has them recorded in the host's debug log and shown to nobody else. The
//!   git and harness subprocesses underneath already pipe their own stdout
//!   (`engine::git_cli::git_command`, `harness::sidecar`), so none of them can
//!   leak into ours. The one argument that is not a hook invocation —
//!   `--version`, which no host sends — prints plain text there instead, and
//!   deliberately prints nothing a host could parse as a decision.
//!   The two worktree hooks (`worktree-create`, `worktree-remove`) speak
//!   Claude Code's `WorktreeCreate`/`WorktreeRemove` protocol instead: stdout
//!   carries the created worktree's path and nothing else, and every note goes
//!   to stderr (`hooks::worktree`).
//! * **Exit 0, except for one wake and the worktree hooks.** `deliver`, the
//!   `asyncRewake` `Stop` hook, exits 2 with the message it claimed on stderr:
//!   the documented way to wake the agent
//!   (<https://code.claude.com/docs/en/hooks.md>), on an event with no tool
//!   call to block. The worktree hooks exit with `hooks::worktree`'s codes,
//!   because for those events a non-zero exit is how Claude Code learns that no
//!   worktree was made or removed, and 75 tells the plugin's launcher to fall
//!   back to plain git. Every other path, `deliver`'s included, exits 0.
//!   Exit 2 blocks the tool call whatever the JSON says. A
//!   hook that crashed, timed out or could not read its input must never be the
//!   reason a user's edit is refused, so every failure path here degrades to
//!   "no decision" plus a stderr line. The single exception is a hook the host
//!   *itself* killed: `procguard::exit` reports that signal, which is not exit
//!   2 and so still blocks nothing, and which the host already knows about
//!   because it sent it.

use std::io;

use gitpulse_lib::hooks::{self, IDENTITY_FLAGS, SUBCOMMANDS};

fn main() {
    // A hook runs git for the user's own agent, so it spends the per-user
    // spawn budget the app and the agent servers spend. First, before any
    // spawn builds the gate; this only records the choice and blocks on nothing.
    gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget();
    // The one deadline for the whole process starts here, before anything that
    // can block, so every later wait is cut to what the host's timeout leaves.
    hooks::start_deadline();
    let code = run_guarded(decide);
    // The decision is already on stdout. Anything a check abandoned at its
    // deadline is still running: a `git status` or the harness sidecar, in its
    // own process group, able to hold `.git/index.lock` after the hook is gone.
    // `process::exit` runs no destructor, so they are taken down here, as the
    // servers do. Free when nothing is registered; otherwise one `CHILD_GRACE`.
    let sweep = gitpulse_lib::procguard::reap_all(gitpulse_lib::procguard::CHILD_GRACE);
    if !sweep.is_complete() {
        log::warn!(target: "hook", "{}", sweep.describe());
    }
    // Not a bare return: a hook that finished while a caught signal was still
    // being swept would exit out from under it, leaving the git or harness
    // subprocess the sweep was killing.
    gitpulse_lib::procguard::exit(code);
}

/// Runs the hook and returns its exit code, which is 0 when a judging hook
/// panicked. A panic otherwise exits 101: not the blocking 2, but not the
/// promised 0 either, and a host is free to treat any non-zero hook as a
/// failure the user has to look at. The installed panic hook has already logged
/// the fault. A worktree hook that panicked exits non-zero instead
/// ([`hooks::worktree::exit_code_after_panic`]): for those events exit 0 means
/// "made" or "removed", which a panic never did.
fn run_guarded(decide: impl FnOnce() -> i32 + std::panic::UnwindSafe) -> i32 {
    std::panic::catch_unwind(decide).unwrap_or_else(|_| {
        log::error!(target: "hook", "gitpulse-hook panicked; no decision");
        hooks::worktree::exit_code_after_panic()
    })
}

/// Runs one hook invocation and returns its exit code, which
/// [`hooks::diagnostic`] fixes at 0 for every outcome it reports.
fn decide() -> i32 {
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // A host that decides a hook has taken too long kills it, and the git or
    // harness subprocess a check was in the middle of would otherwise carry
    // on unattached. The cost is one pipe and one thread, well under the
    // startup this file is written to protect. Not logged at startup like the
    // servers do: stderr is the host's debug channel and a line on every
    // matching tool call would drown the ones that matter.
    let _ = gitpulse_lib::procguard::install_signal_handlers();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let (flag_is_agy, args): (bool, &[String]) = match raw_args.first().map(String::as_str) {
        Some("--host=agy") => (true, &raw_args[1..]),
        _ => (false, &raw_args[..]),
    };

    let Some(subcommand) = args.first().cloned() else {
        // Exit 2, the host's "block", not 0: a host that does not know the
        // exec form's `args` field (the Windows settings copy uses it) would
        // start this binary with no arguments, and exit 0 there would read as
        // "allowed" for a collision guard or command gate that never ran.
        let reason = format!(
            "gitpulse-hook: no subcommand given; expected one of {}. Blocking: no safety check ran.",
            SUBCOMMANDS.join(", ")
        );
        log::warn!(target: "hook", "{reason}");
        emit_stderr(format!("{reason}\n"));
        return 2;
    };

    // Asked who we are rather than to judge anything. No host sends this — it
    // is what `npm run mcp:doctor` uses to tell an absent or stale hook binary
    // from a current one, which no hook invocation can reveal because silence
    // is a legitimate answer to every one of them. Handled before the
    // subcommand check so it is never reported as an unknown subcommand, and
    // it reads no stdin: there is none to read.
    if IDENTITY_FLAGS.contains(&subcommand.as_str()) {
        emit(hooks::identity());
        return 0;
    }

    if subcommand == "term" {
        return run_term_cli(&args[1..]);
    }

    if !SUBCOMMANDS.contains(&subcommand.as_str()) {
        log::warn!(target: "hook", "unknown hook subcommand {subcommand:?}; no decision");
        if flag_is_agy {
            emit("{}\n".to_string());
        }
        return 0;
    }

    // An unparseable payload is reported on stderr and dropped. Guessing at a
    // decision from input we could not read would be the worst of both worlds:
    // neither a check that ran nor an admission that one did not. Exiting 0
    // with no stdout leaves the host's own permission flow exactly as it was.
    //
    // `notify` takes the matcher's name as a second argument, because a
    // `Notification` payload does not say which matcher routed it. It is taken
    // first because it also decides whether stdin is read at all.
    let argument = args.get(1).cloned();
    let worktree_hook =
        [hooks::worktree::CREATE, hooks::worktree::REMOVE].contains(&subcommand.as_str());
    if worktree_hook {
        hooks::worktree::begin();
    }
    let mut input = match hooks::read_for(&subcommand, argument.as_deref(), io::stdin()) {
        Ok(input) => input,
        Err(error) => {
            log::warn!(target: "hook", "gitpulse-hook {subcommand}: {error}");
            if flag_is_agy && !worktree_hook {
                emit("{}\n".to_string());
            }
            // Nothing was done; the launcher can still have plain git do it.
            return if worktree_hook {
                hooks::worktree::EXIT_UNAVAILABLE
            } else {
                0
            };
        }
    };
    if flag_is_agy {
        input.is_agy = true;
    }

    if worktree_hook {
        let finished = hooks::worktree::run(&subcommand, &input);
        for note in &finished.notes {
            emit_stderr(format!("gitpulse-hook {subcommand}: {note}\n"));
        }
        if let Some(path) = &finished.path {
            emit(format!("{path}\n"));
        }
        return finished.exit_code;
    }

    match hooks::dispatch(&subcommand, argument.as_deref(), &input) {
        Ok(output) => {
            // GitPulse injects the PreToolUse gates into every session it
            // launches, so a session that also enables the plugin runs them
            // twice. Decisions are left alone; an identical warning is said once.
            let output = if hooks::INJECTED_SUBCOMMANDS.contains(&subcommand.as_str()) {
                hooks::advisory_once::dedupe(&subcommand, &input, output)
            } else {
                output
            };
            if let Some(json) = output.render_for_invocation(&subcommand, &input) {
                emit(format!("{json}\n"));
            }
            0
        }
        // `deliver` claimed a message. The agent reads stderr (the hook is
        // registered `asyncRewake`, and exit 2 is what wakes it), so the text
        // goes there as it is, with no log prefix, and the exit code is the
        // one `diagnostic` fixes for a wake.
        Err(error @ hooks::HookError::Wake(_)) => {
            emit_stderr(format!("{error}"));
            hooks::diagnostic(&error).exit_code
        }
        // One line per invocation, at the level the outcome calls for: a
        // transport this platform does not have is information, not a failure.
        Err(error) => {
            let diagnostic = hooks::diagnostic(&error);
            log::log!(target: "hook", diagnostic.level, "{}", diagnostic.line);
            if input.is_agy && diagnostic.exit_code == 0 {
                emit("{}\n".to_string());
            }
            diagnostic.exit_code
        }
    }
}

/// The one place this binary writes to stdout.
///
/// Bounded and deadlined for the same reason every other stdout in this
/// repository is: a host that has gone away leaves a pipe nobody drains, and
/// Rust ignores SIGPIPE, so an unchecked write is a hang rather than an error.
/// A failed write is logged and not retried — there is no second channel to
/// deliver it on, and exiting non-zero would block the user's tool call over a
/// message we could not send.
fn emit(payload: String) {
    let result = gitpulse_lib::output::BoundedOutput::new(
        io::stdout(),
        "gitpulse-hook-stdout",
        hooks::MAX_INPUT_BYTES,
        std::time::Duration::from_secs(1),
    )
    .and_then(|output| output.write(payload.as_bytes()));
    if let Err(error) = result {
        log::warn!(target: "hook", "could not deliver hook stdout: {error}; no retry");
    }
}

/// What an `asyncRewake` hook says to the agent it wakes: stderr, bounded and
/// deadlined like stdout, for the same reason (a host that has gone away leaves
/// a pipe nobody drains). A message that could not be written is logged; there
/// is no second channel, and the exit code stays the wake's, because the claim
/// has already happened and it is the host that decides what an unread
/// reminder is worth.
fn emit_stderr(payload: String) {
    let result = gitpulse_lib::output::BoundedOutput::new(
        io::stderr(),
        "gitpulse-hook-stderr",
        hooks::MAX_INPUT_BYTES,
        std::time::Duration::from_secs(1),
    )
    .and_then(|output| output.write(payload.as_bytes()));
    if let Err(error) = result {
        log::warn!(target: "hook", "could not deliver the wake message: {error}; no retry");
    }
}

const TERM_USAGE: &str = "\
Usage: gitpulse-hook term <command> [options]

Option A scoped child terminal orchestration for coding agents running inside a
GitPulse PTY (`GITPULSE_TERMINAL_SESSION` + `GITPULSE_SESSION_TOKEN`).

Commands:
  spawn       Spawn a scoped child PTY (`shell`, `claude`, `agy`, `codex`, `manvi`, `grok`)
  open-file   Open a repository file in a right-hand terminal split pane
  list        List child sessions spawned by this caller session
  wait        Wait on a child session (`OSC 133`, `OSC 7501`, pattern, or loopback port)
  read        Read secret-scrubbed `vt100` screen lines from a child session
  send        Send policy-gated plain-text input to a child session
  close       Terminate and close a child session spawned by this caller session

Common options:
  --repo <abs-path>           Repository root (defaults to git root of current working directory)
  --caller-session <id>       Override GITPULSE_TERMINAL_SESSION
  --caller-token <token>      Override GITPULSE_SESSION_TOKEN

Spawn options:
  --launcher <kind>           shell (default) | claude | agy | codex | manvi | grok
  --cwd <dir>                 Subdirectory inside the repository
  --command <cmd>             Initial command when --launcher shell
  --prompt <text>             Initial prompt for an agent launcher
  --title <title>             Tab / pane title
  --split | --no-split        Split right beside parent pane (default: true)
  --rows <1..500>             Initial PTY rows
  --cols <2..1000>            Initial PTY columns

Open-file options:
  --path <rel-path>           Repository-relative path (or pass as positional arg)
  --line <1-based>            Optional line number
  --column <1-based>          Optional column number

Wait options:
  --child <session-id>        Child session id (or pass as positional arg)
  --timeout-ms <1..25000>     Wait ceiling in milliseconds (default: 10000)
  --pattern <substring>       Wake as soon as screen contains substring
  --wait-for-idle             Wait until prompt/agent idle (default: true)
  --no-wait-for-idle          Return immediately if pattern/exit already satisfied

Read options:
  --child <session-id>        Child session id (or pass as positional arg)
  --max-lines <1..200>        Maximum trailing screen lines (default: 80)

Send options:
  --child <session-id>        Child session id (or pass as positional arg)
  --input <text>              Plain-text input (ANSI escapes and NUL refused)
  --newline | --no-newline    Append trailing newline (default: true)

Close options:
  --child <session-id>        Child session id (or pass as positional arg)
";

#[derive(Debug, PartialEq, Eq)]
enum TermCliOutcome {
    Help(String),
    Call(gitpulse_lib::terminal::orchestrate::OrchestrateCall),
}

fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((flag, val)) => (flag, Some(val)),
        None => (arg, None),
    }
}

fn next_flag_value<'a>(
    flag: &str,
    inline: Option<&'a str>,
    iter: &mut std::slice::Iter<'a, String>,
) -> Result<String, String> {
    if let Some(val) = inline {
        return Ok(val.to_owned());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| format!("flag {flag:?} requires a value"))
}

fn parse_u16_flag(flag: &str, raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .map_err(|_| format!("flag {flag:?} expects an integer in 0..=65535, got {raw:?}"))
}

fn parse_u32_flag(flag: &str, raw: &str) -> Result<u32, String> {
    raw.parse::<u32>()
        .map_err(|_| format!("flag {flag:?} expects a positive integer, got {raw:?}"))
}

fn parse_u64_flag(flag: &str, raw: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("flag {flag:?} expects a non-negative integer, got {raw:?}"))
}

fn parse_usize_flag(flag: &str, raw: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|_| format!("flag {flag:?} expects a positive integer, got {raw:?}"))
}

fn parse_term_cli(
    args: &[String],
    default_repo: Option<&str>,
    env_session_id: Option<&str>,
    env_session_token: Option<&str>,
) -> Result<TermCliOutcome, String> {
    use gitpulse_lib::terminal::orchestrate::OrchestrateCall;

    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(TermCliOutcome::Help(TERM_USAGE.to_owned()));
    };
    if matches!(verb, "help" | "--help" | "-h") {
        return Ok(TermCliOutcome::Help(TERM_USAGE.to_owned()));
    }
    if args
        .iter()
        .skip(1)
        .any(|a| matches!(a.as_str(), "--help" | "-h"))
    {
        return Ok(TermCliOutcome::Help(TERM_USAGE.to_owned()));
    }

    let mut repo_override: Option<String> = None;
    let mut sid_override: Option<String> = None;
    let mut tok_override: Option<String> = None;

    let resolve_common = |repo_opt: Option<String>,
                          sid_opt: Option<String>,
                          tok_opt: Option<String>|
     -> Result<(String, String, String), String> {
        let repo = repo_opt
            .or_else(|| default_repo.map(str::to_owned))
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "missing repository root: run inside a Git worktree or pass --repo <abs-path>"
                    .to_string()
            })?;
        let sid = sid_opt
            .or_else(|| env_session_id.map(str::to_owned))
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "missing caller terminal session id: {} is not set (pass --caller-session inside tests)",
                    gitpulse_lib::alerts::bridge::SESSION_ENV
                )
            })?;
        let tok = tok_opt
            .or_else(|| env_session_token.map(str::to_owned))
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "missing caller session capability token: {} is not set",
                    gitpulse_lib::terminal::orchestrate::SESSION_TOKEN_ENV
                )
            })?;
        Ok((repo, sid, tok))
    };

    let mut iter = args[1..].iter();
    let call = match verb {
        "spawn" => {
            let mut launcher = None;
            let mut cwd = None;
            let mut command = None;
            let mut prompt = None;
            let mut title = None;
            let mut split = None;
            let mut rows = None;
            let mut cols = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--launcher" => launcher = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--cwd" => cwd = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--command" => command = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--prompt" => prompt = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--title" => title = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--split" => {
                        if inline.is_some() {
                            return Err("--split takes no value (use --split or --no-split)".into());
                        }
                        split = Some(true);
                    }
                    "--no-split" => {
                        if inline.is_some() {
                            return Err("--no-split takes no value".into());
                        }
                        split = Some(false);
                    }
                    "--rows" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        rows = Some(parse_u16_flag(flag, &v)?);
                    }
                    "--cols" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        cols = Some(parse_u16_flag(flag, &v)?);
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term spawn`"
                        ))
                    }
                }
            }
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::Spawn {
                repo_path,
                caller_session_id,
                caller_token,
                launcher,
                cwd,
                command,
                prompt,
                title,
                split,
                rows,
                cols,
            }
        }
        "open-file" | "open_file" => {
            let mut path: Option<String> = None;
            let mut line = None;
            let mut column = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--path" => path = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--line" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        line = Some(parse_u32_flag(flag, &v)?);
                    }
                    "--column" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        column = Some(parse_u32_flag(flag, &v)?);
                    }
                    _ if !raw.starts_with('-') && path.is_none() => {
                        path = Some(raw.clone());
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term open-file`"
                        ))
                    }
                }
            }
            let path = path.ok_or_else(|| {
                "`gitpulse-hook term open-file` requires --path <repo-relative-path>".to_string()
            })?;
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::OpenFile {
                repo_path,
                caller_session_id,
                caller_token,
                path,
                line,
                column,
            }
        }
        "list" => {
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term list`"
                        ))
                    }
                }
            }
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::List {
                repo_path,
                caller_session_id,
                caller_token,
            }
        }
        "wait" => {
            let mut child_session_id: Option<String> = None;
            let mut timeout_ms = None;
            let mut pattern = None;
            let mut wait_for_idle = None;
            let mut since_seq = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--child" => child_session_id = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--timeout-ms" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        timeout_ms = Some(parse_u64_flag(flag, &v)?);
                    }
                    "--since-seq" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        since_seq = Some(parse_u64_flag(flag, &v)?);
                    }
                    "--pattern" => pattern = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--wait-for-idle" => {
                        if inline.is_some() {
                            return Err("--wait-for-idle takes no value".into());
                        }
                        wait_for_idle = Some(true);
                    }
                    "--no-wait-for-idle" => {
                        if inline.is_some() {
                            return Err("--no-wait-for-idle takes no value".into());
                        }
                        wait_for_idle = Some(false);
                    }
                    _ if !raw.starts_with('-') && child_session_id.is_none() => {
                        child_session_id = Some(raw.clone());
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term wait`"
                        ))
                    }
                }
            }
            let child_session_id = child_session_id.ok_or_else(|| {
                "`gitpulse-hook term wait` requires --child <session-id>".to_string()
            })?;
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::Wait {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                timeout_ms,
                pattern,
                wait_for_idle,
                since_seq,
            }
        }
        "read" => {
            let mut child_session_id: Option<String> = None;
            let mut max_lines = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--child" => child_session_id = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--max-lines" => {
                        let v = next_flag_value(flag, inline, &mut iter)?;
                        max_lines = Some(parse_usize_flag(flag, &v)?);
                    }
                    _ if !raw.starts_with('-') && child_session_id.is_none() => {
                        child_session_id = Some(raw.clone());
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term read`"
                        ))
                    }
                }
            }
            let child_session_id = child_session_id.ok_or_else(|| {
                "`gitpulse-hook term read` requires --child <session-id>".to_string()
            })?;
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::Read {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                max_lines,
            }
        }
        "send" => {
            let mut child_session_id: Option<String> = None;
            let mut input: Option<String> = None;
            let mut append_newline = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--child" => child_session_id = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--input" => input = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--newline" => {
                        if inline.is_some() {
                            return Err("--newline takes no value".into());
                        }
                        append_newline = Some(true);
                    }
                    "--no-newline" => {
                        if inline.is_some() {
                            return Err("--no-newline takes no value".into());
                        }
                        append_newline = Some(false);
                    }
                    _ if !raw.starts_with('-') && child_session_id.is_none() => {
                        child_session_id = Some(raw.clone());
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term send`"
                        ))
                    }
                }
            }
            let child_session_id = child_session_id.ok_or_else(|| {
                "`gitpulse-hook term send` requires --child <session-id>".to_string()
            })?;
            let input = input
                .ok_or_else(|| "`gitpulse-hook term send` requires --input <text>".to_string())?;
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::Send {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
                input,
                append_newline,
            }
        }
        "close" => {
            let mut child_session_id: Option<String> = None;
            while let Some(raw) = iter.next() {
                let (flag, inline) = split_flag(raw);
                match flag {
                    "--repo" => repo_override = Some(next_flag_value(flag, inline, &mut iter)?),
                    "--caller-session" => {
                        sid_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--caller-token" => {
                        tok_override = Some(next_flag_value(flag, inline, &mut iter)?)
                    }
                    "--child" => child_session_id = Some(next_flag_value(flag, inline, &mut iter)?),
                    _ if !raw.starts_with('-') && child_session_id.is_none() => {
                        child_session_id = Some(raw.clone());
                    }
                    _ => {
                        return Err(format!(
                            "unknown option {raw:?} for `gitpulse-hook term close`"
                        ))
                    }
                }
            }
            let child_session_id = child_session_id.ok_or_else(|| {
                "`gitpulse-hook term close` requires --child <session-id>".to_string()
            })?;
            let (repo_path, caller_session_id, caller_token) =
                resolve_common(repo_override, sid_override, tok_override)?;
            OrchestrateCall::Close {
                repo_path,
                caller_session_id,
                caller_token,
                child_session_id,
            }
        }
        other => {
            return Err(format!(
                "unknown `gitpulse-hook term` command {other:?}; expected spawn, open-file, list, wait, read, send, or close"
            ));
        }
    };

    call.validate()?;
    Ok(TermCliOutcome::Call(call))
}

fn call_repo_path(call: &gitpulse_lib::terminal::orchestrate::OrchestrateCall) -> &str {
    use gitpulse_lib::terminal::orchestrate::OrchestrateCall;
    match call {
        OrchestrateCall::Spawn { repo_path, .. }
        | OrchestrateCall::OpenFile { repo_path, .. }
        | OrchestrateCall::List { repo_path, .. }
        | OrchestrateCall::Wait { repo_path, .. }
        | OrchestrateCall::Read { repo_path, .. }
        | OrchestrateCall::Send { repo_path, .. }
        | OrchestrateCall::Close { repo_path, .. } => repo_path,
    }
}

fn dispatch_term_call(
    call: gitpulse_lib::terminal::orchestrate::OrchestrateCall,
) -> Result<serde_json::Value, String> {
    gitpulse_lib::repository_trust::require(std::path::Path::new(call_repo_path(&call)))
        .map_err(|e| e.into_message())?;
    #[cfg(target_os = "macos")]
    {
        static NEXT_TERM_REQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let profile = gitpulse_lib::tool_config::default_config_dir()
            .ok_or_else(|| "GitPulse's profile directory is unknown (no HOME)".to_string())?;
        let owner = gitpulse_lib::desktop::instance::reach_owner(
            &profile,
            std::time::Duration::from_secs(2),
        )
        .ok_or_else(|| {
            "GitPulse is not running, so there is no terminal orchestrator to reach".to_string()
        })?;
        let seq = NEXT_TERM_REQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req_id = format!("hook-term-{}-{seq}", std::process::id());
        owner.orchestrate(&req_id, call)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = call;
        Err(
            "Terminal orchestration over the desktop mailbox is available on macOS only for now"
                .to_string(),
        )
    }
}

fn run_term_cli(args: &[String]) -> i32 {
    let default_repo = std::env::current_dir()
        .ok()
        .and_then(|cwd| gitpulse_lib::engine::git_cli::find_git_root(&cwd))
        .map(|p| p.to_string_lossy().into_owned());
    let env_sid = std::env::var(gitpulse_lib::terminal::TERMINAL_SESSION_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var(gitpulse_lib::alerts::bridge::SESSION_ENV).ok());
    let env_tok = std::env::var(gitpulse_lib::terminal::orchestrate::SESSION_TOKEN_ENV).ok();

    match parse_term_cli(
        args,
        default_repo.as_deref(),
        env_sid.as_deref(),
        env_tok.as_deref(),
    ) {
        Ok(TermCliOutcome::Help(help)) => {
            emit(help);
            0
        }
        Ok(TermCliOutcome::Call(call)) => match dispatch_term_call(call) {
            Ok(value) => {
                match serde_json::to_string(&value) {
                    Ok(serialized) => emit(format!("{serialized}\n")),
                    Err(error) => {
                        emit_stderr(format!(
                            "gitpulse-hook term: failed to serialize response: {error}\n"
                        ));
                        return 1;
                    }
                }
                0
            }
            Err(error) => {
                emit_stderr(format!("gitpulse-hook term: {error}\n"));
                1
            }
        },
        Err(error) => {
            emit_stderr(format!("gitpulse-hook term: {error}\n"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitpulse_lib::terminal::orchestrate::OrchestrateCall;

    fn vec_args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    const SAMPLE_TOKEN: &str =
        "gpst_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_panicking_hook_still_exits_zero_with_no_decision() {
        // Outside a worktree hook, which this test process never starts.
        assert_eq!(run_guarded(|| panic!("hook fault")), 0);
        assert_eq!(run_guarded(|| 0), 0);
    }

    #[test]
    fn term_cli_renders_help_when_asked_or_empty() {
        for argv in [
            vec_args(&[]),
            vec_args(&["--help"]),
            vec_args(&["help"]),
            vec_args(&["spawn", "-h"]),
        ] {
            let parsed = parse_term_cli(&argv, None, None, None).expect("help");
            assert!(matches!(parsed, TermCliOutcome::Help(_)));
        }
    }

    #[test]
    fn term_cli_parses_all_six_verbs_with_env_defaults_and_overrides() {
        let spawn = parse_term_cli(
            &vec_args(&[
                "spawn",
                "--launcher=shell",
                "--cwd",
                "src",
                "--command",
                "cargo test",
                "--title",
                "Unit tests",
                "--no-split",
                "--rows=36",
                "--cols",
                "120",
            ]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("spawn parse");
        assert_eq!(
            spawn,
            TermCliOutcome::Call(OrchestrateCall::Spawn {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                launcher: Some("shell".into()),
                cwd: Some("src".into()),
                command: Some("cargo test".into()),
                prompt: None,
                title: Some("Unit tests".into()),
                split: Some(false),
                rows: Some(36),
                cols: Some(120),
            })
        );

        let open_file = parse_term_cli(
            &vec_args(&["open-file", "src/main.rs", "--line", "42", "--column=7"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("open-file parse");
        assert_eq!(
            open_file,
            TermCliOutcome::Call(OrchestrateCall::OpenFile {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                path: "src/main.rs".into(),
                line: Some(42),
                column: Some(7),
            })
        );

        let list = parse_term_cli(
            &vec_args(&[
                "list",
                "--repo",
                "/explicit/repo",
                "--caller-session=term-200-9",
            ]),
            Some("/ignored/default"),
            Some("ignored-sid"),
            Some(SAMPLE_TOKEN),
        )
        .expect("list parse");
        assert_eq!(
            list,
            TermCliOutcome::Call(OrchestrateCall::List {
                repo_path: "/explicit/repo".into(),
                caller_session_id: "term-200-9".into(),
                caller_token: SAMPLE_TOKEN.into(),
            })
        );

        let wait = parse_term_cli(
            &vec_args(&[
                "wait",
                "term-100-2",
                "--timeout-ms",
                "5000",
                "--pattern=Listening on",
                "--no-wait-for-idle",
                "--since-seq=7",
            ]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("wait parse");
        assert_eq!(
            wait,
            TermCliOutcome::Call(OrchestrateCall::Wait {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                child_session_id: "term-100-2".into(),
                timeout_ms: Some(5000),
                pattern: Some("Listening on".into()),
                wait_for_idle: Some(false),
                since_seq: Some(7),
            })
        );

        let read = parse_term_cli(
            &vec_args(&["read", "--child", "term-100-2", "--max-lines=50"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("read parse");
        assert_eq!(
            read,
            TermCliOutcome::Call(OrchestrateCall::Read {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                child_session_id: "term-100-2".into(),
                max_lines: Some(50),
            })
        );

        let send = parse_term_cli(
            &vec_args(&["send", "term-100-2", "--input", "echo ok", "--no-newline"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("send parse");
        assert_eq!(
            send,
            TermCliOutcome::Call(OrchestrateCall::Send {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                child_session_id: "term-100-2".into(),
                input: "echo ok".into(),
                append_newline: Some(false),
            })
        );

        let close = parse_term_cli(
            &vec_args(&["close", "term-100-2"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect("close parse");
        assert_eq!(
            close,
            TermCliOutcome::Call(OrchestrateCall::Close {
                repo_path: "/repo/root".into(),
                caller_session_id: "term-100-1".into(),
                caller_token: SAMPLE_TOKEN.into(),
                child_session_id: "term-100-2".into(),
            })
        );
    }

    #[test]
    fn term_cli_refuses_unknown_flags_missing_credentials_and_bad_paths() {
        let missing_tok = parse_term_cli(
            &vec_args(&["list"]),
            Some("/repo/root"),
            Some("term-100-1"),
            None,
        )
        .expect_err("missing token");
        assert!(
            missing_tok.contains("GITPULSE_SESSION_TOKEN"),
            "{missing_tok}"
        );

        let bad_flag = parse_term_cli(
            &vec_args(&["list", "--bogus"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect_err("unknown flag");
        assert!(bad_flag.contains("unknown option"), "{bad_flag}");

        let rel_repo = parse_term_cli(
            &vec_args(&["list", "--repo", "relative/repo"]),
            None,
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect_err("relative repo");
        assert!(rel_repo.contains("absolute path"), "{rel_repo}");

        let traversal = parse_term_cli(
            &vec_args(&["open-file", "../secret.txt"]),
            Some("/repo/root"),
            Some("term-100-1"),
            Some(SAMPLE_TOKEN),
        )
        .expect_err("parent traversal");
        assert!(!traversal.is_empty());
    }
}
