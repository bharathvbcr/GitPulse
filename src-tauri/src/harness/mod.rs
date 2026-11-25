//! Integration with the MANVI coding-agent harness.
//!
//! GitPulse embeds MANVI the way its host plane is meant to be embedded: as a
//! `manvi serve` child speaking NDJSON over stdio. Three of its planes are
//! used — the command gate and the write gate guard every mutating Git action,
//! and the local-LLM plane (capability probing, token budgeting, completion
//! parsing) backs the AI features in `crate::ai`.
//!
//! Everything here degrades: with no harness installed GitPulse still commits,
//! pushes and rebases, and every unguarded action is reported as unguarded.

pub(crate) mod anthropic_account;
pub(crate) mod claude_key;
pub mod hosted;
pub(crate) mod login_keychain;
pub mod manvi_use;
pub mod policy;
pub mod protocol;
pub(crate) mod review;
mod scratch;
#[cfg(all(test, unix))]
mod scratch_attack_tests;
pub mod sidecar;
pub mod steward;

#[cfg(all(test, unix))]
mod concurrent_tests;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use policy::{
    check_command, check_command_allowing, check_file, render_command, HostScope, PolicyStatus,
    PolicyVerdict,
};
pub use protocol::{
    HelloResult, PrepareResult, ProbeResult, ScanModel, ScanResult, ScanServer, SettleResult,
};
pub(crate) use sidecar::Lane;
pub use sidecar::{HarnessError, DEFAULT_CALL_TIMEOUT};

/// What the UI shows about the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub available: bool,
    /// Absolute path of the binary in use, empty when none was found.
    pub binary: String,
    pub protocol: i64,
    pub posture: String,
    pub ops: Vec<String>,
    /// Empty when available; otherwise why not.
    pub error: String,
    pub error_code: String,
}

impl HarnessStatus {
    pub fn probe() -> Self {
        match sidecar::handshake() {
            Ok((binary, hello)) => HarnessStatus {
                available: true,
                binary,
                protocol: hello.protocol,
                posture: hello.posture,
                ops: hello.ops,
                error: String::new(),
                error_code: String::new(),
            },
            Err(e) => HarnessStatus {
                available: false,
                binary: sidecar::resolve_binary().unwrap_or_default(),
                protocol: 0,
                posture: String::new(),
                ops: Vec::new(),
                error: e.message(),
                error_code: e.code().to_string(),
            },
        }
    }
}

/// The single gate seam for guarded Git actions.
///
/// Every mutating command goes through here and nowhere else: `argv` is
/// rendered the way a shell would show it ([`render_command`]) so the
/// command gate judges the command the client actually runs, a blocking
/// verdict becomes the rendered refusal (`Err`), and otherwise the
/// [`PolicyVerdict`] is handed back so the caller can report an action that
/// ran unchecked when no harness is installed — which is never rendered as
/// an approval.
///
/// This used to exist twice (a generic closure-taking variant here, and a
/// private reimplementation in `commands`); this signature is the canonical
/// one both now share.
pub fn guard_command(repo_path: &str, argv: &[&str]) -> Result<PolicyVerdict, String> {
    guard_command_allowing(repo_path, argv, &[])
}

/// Guards a command while declaring an allowlist for it.
///
/// The local CI runner is the caller this exists for. It used to bypass the
/// gate entirely — every `npm test` and `cargo clippy` it spawned ran
/// unjudged and unrecorded, which is a privileged unlogged execution path in a
/// product whose claim is to be the trust boundary between agents and the
/// repository. Declaring the step it is about to run lets the harness answer
/// cleanly instead of demoting a `command.not_allowed` on every step.
pub(crate) fn guard_command_allowing(
    repo_path: &str,
    argv: &[&str],
    allowed: &[String],
) -> Result<PolicyVerdict, String> {
    judge_command_allowing(repo_path, argv, allowed).and_then(gated)
}

/// [`guard_command`] that hands back the recorded verdict instead of turning a
/// refusal or a gate that could not run into an `Err`.
///
/// For the one caller that must degrade differently on the two: the
/// `WorktreeCreate` hook (`hooks::worktree`), which replaces Claude Code's own
/// `git worktree add`. A rule that fired is honoured there exactly as here; a
/// gate that could not run must not cost the agent a worktree plain git would
/// have made. The ledger row is written either way, before this returns, so
/// the caller cannot act on a verdict nobody recorded. The caller owns the
/// rest of [`gated`]'s contract: act on nothing that [`PolicyVerdict::blocks`],
/// and say so when [`PolicyVerdict::gate_failed`].
///
/// `Err` only when there is no checkout to judge the command under.
pub fn judge_command(repo_path: &str, argv: &[&str]) -> Result<PolicyVerdict, String> {
    judge_command_allowing(repo_path, argv, &[])
}

fn judge_command_allowing(
    repo_path: &str,
    argv: &[&str],
    allowed: &[String],
) -> Result<PolicyVerdict, String> {
    let command = render_command(argv);
    // Resolved once: the scope this action is judged under and the ledger row
    // that records the judgement are addressed by the same answer.
    let address = crate::ledger::bindings::repository_address(repo_path);
    if let Err(refused) = review::gate(repo_path, argv) {
        let action = crate::ledger::action_for_argv(argv);
        record_gate(
            repo_path,
            address.as_ref().ok(),
            &action,
            &command,
            serde_json::to_string(argv).ok(),
            &refused,
        );
        return Ok(*refused);
    }
    let verdict = match scope_at(&address) {
        Ok(scope) => check_command_allowing(repo_path, &command, scope.as_ref(), allowed),
        Err(failure) if failure.kind == ScopeFailureKind::Missing => {
            return Err(failure.missing_checkout(&command));
        }
        Err(failure) => failure.verdict(&command),
    };
    let action = crate::ledger::action_for_argv(argv);
    let argv_json = serde_json::to_string(argv).ok();
    record_gate(
        repo_path,
        address.as_ref().ok(),
        &action,
        &command,
        argv_json,
        &verdict,
    );
    Ok(verdict)
}

/// Judges a command line against this checkout's bound task scope, recording
/// nothing.
///
/// The agent hook's entry to the gate (`hooks::run_command_gate`). It turns a
/// binding into a scope through the same [`scope_from_binding`] that
/// [`guard_command`] uses, so an agent's Bash call and GitPulse's own guarded
/// action are measured against one declaration — including the fail-closed
/// answer when a binding names a task whose scope cannot be read. The binding
/// itself is read with [`resolve_binding_readonly`], since the hook is a fresh
/// process per tool call and must not create, secure or migrate the ledger. It
/// does not write a ledger row: the hook runs on every Bash call an agent
/// makes, and the gate rows record GitPulse's own actions, which an agent's
/// shell is not.
///
/// [`resolve_binding_readonly`]: crate::ledger::bindings::resolve_binding_readonly
///
/// One case is not failed closed here, where [`guard_command`] would: a
/// repository GitPulse is not trusted in. Resolving a binding runs Git, which
/// the trust gate refuses there, and a binding can only have been made through
/// a trusted path — so an untrusted checkout has no scope GitPulse may read,
/// and failing closed would refuse every command an agent runs in every
/// repository the person never opened in GitPulse. It is judged without a
/// scope, exactly as before scopes reached the hook — but not silently: the
/// answer carries [`UNSCOPED_UNTRUSTED`], so a reader can tell "no task is
/// bound here" from "GitPulse could not look". `guard_command` keeps failing
/// closed because the app never acts in an untrusted repository.
///
/// A checkout that vanished between finding its root and resolving its binding
/// is judged without a scope too: there is no binding to read, and the command
/// will meet a missing directory on its own.
///
/// Whether a scope was declared comes back beside the verdict, because the
/// verdict cannot say: a command decision carries either the declared task's
/// id or the harness's `host-scope` placeholder (always the placeholder from a
/// `manvi` older than the scope allowlist fix), so its task id is no evidence of
/// a binding.
///
/// `lane` is the sidecar that answers: the hook process's own one-shot child
/// ([`Lane::Shared`]) or the app's long-lived hook child ([`Lane::Hook`], via
/// `hooks::serve_gate`). It is the one body both run, so they cannot disagree
/// about what a scope is.
pub(crate) fn check_command_in_scope(lane: Lane, repo_path: &str, command: &str) -> ScopedVerdict {
    let ask =
        |scope: Option<&HostScope>| policy::check_command_on(lane, repo_path, command, scope, &[]);
    let caller = match hook_scope(repo_path) {
        HookScope::Declared(scope) => ScopedVerdict {
            bound: true,
            unscoped: None,
            verdict: ask(Some(&scope)),
        },
        HookScope::Unbound => ScopedVerdict {
            bound: false,
            unscoped: None,
            verdict: ask(None),
        },
        HookScope::Untrusted => ScopedVerdict {
            bound: false,
            unscoped: Some(UNSCOPED_UNTRUSTED),
            verdict: ask(None),
        },
        HookScope::Unresolved(failure) => ScopedVerdict {
            bound: true,
            unscoped: None,
            verdict: failure.verdict(command),
        },
    };
    if caller.verdict.gate_failed()
        || (caller.verdict.blocks() && !caller.verdict.is_unreadable_construct())
    {
        return caller;
    }
    if caller.bound {
        if let Some(clause) = unresolved_git_c(command) {
            return ScopedVerdict {
                bound: true,
                unscoped: None,
                verdict: unresolved_git_c_verdict(command, clause, &caller.verdict.task_id),
            };
        }
    }
    for (sibling_root, rewritten_cmd) in sibling_git_clauses(Path::new(repo_path), command) {
        let Some(sib_str) = sibling_root.to_str() else {
            continue;
        };
        match hook_scope(sib_str) {
            HookScope::Declared(sib_scope) => {
                let sib_verdict =
                    policy::check_command_on(lane, sib_str, &rewritten_cmd, Some(&sib_scope), &[]);
                let real_sibling_block = sib_verdict.blocks()
                    && (!caller.verdict.blocks() || !sib_verdict.is_unreadable_construct());
                if real_sibling_block || sib_verdict.status == PolicyStatus::Unchecked {
                    return ScopedVerdict {
                        bound: true,
                        unscoped: None,
                        verdict: sib_verdict,
                    };
                }
            }
            HookScope::Unresolved(failure) => {
                return ScopedVerdict {
                    bound: true,
                    unscoped: None,
                    verdict: failure.verdict(command),
                };
            }
            HookScope::Unbound | HookScope::Untrusted => {}
        }
    }
    caller
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
    AnsiC,
}

struct ShellToken<'a> {
    raw: &'a str,
    text: String,
    quoted: bool,
    expands: bool,
    is_redirect: bool,
}

struct GitInvocation<'a> {
    tokens: Vec<ShellToken<'a>>,
    c_args: Vec<(usize, Option<usize>, String, bool)>,
}

fn split_shell_clauses(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = QuoteState::None;
    let mut iter = command.char_indices().peekable();

    while let Some((i, ch)) = iter.next() {
        match quote {
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::AnsiC => {
                if ch == '\\' {
                    let _ = iter.next();
                } else if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::Double => {
                if ch == '\\' {
                    let _ = iter.next();
                } else if ch == '"' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::None => match ch {
                '\\' => {
                    let _ = iter.next();
                }
                '$' if bytes.get(i + 1) == Some(&b'\'') => {
                    let _ = iter.next();
                    quote = QuoteState::AnsiC;
                }
                '\'' => quote = QuoteState::Single,
                '"' => quote = QuoteState::Double,
                ';' | '\n' => {
                    let slice = command[start..i].trim();
                    if !slice.is_empty() {
                        parts.push(slice);
                    }
                    start = i + ch.len_utf8();
                }
                '|' => {
                    if bytes.get(i + 1) == Some(&b'|') {
                        let _ = iter.next();
                        let slice = command[start..i].trim();
                        if !slice.is_empty() {
                            parts.push(slice);
                        }
                        start = i + 2;
                    } else if i > 0 && bytes[i - 1] == b'>' {
                        // `>|` clobber redirection, not a pipe boundary.
                    } else {
                        let slice = command[start..i].trim();
                        if !slice.is_empty() {
                            parts.push(slice);
                        }
                        start = i + 1;
                    }
                }
                '&' => {
                    if bytes.get(i + 1) == Some(&b'&') {
                        let _ = iter.next();
                        let slice = command[start..i].trim();
                        if !slice.is_empty() {
                            parts.push(slice);
                        }
                        start = i + 2;
                    } else if i > 0 && (bytes[i - 1] == b'>' || bytes[i - 1] == b'<') {
                        // `>&` / `<&` descriptor duplication.
                    } else if bytes.get(i + 1) == Some(&b'>') {
                        let _ = iter.next();
                        if bytes.get(i + 2) == Some(&b'>') {
                            let _ = iter.next();
                        }
                    } else {
                        let slice = command[start..i].trim();
                        if !slice.is_empty() {
                            parts.push(slice);
                        }
                        start = i + 1;
                    }
                }
                _ => {}
            },
        }
    }
    let tail = command[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn scan_shell_tokens(clause: &str) -> Vec<ShellToken<'_>> {
    let bytes = clause.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let n = bytes.len();

    while i < n {
        while i < n && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i >= n {
            break;
        }
        // Check standalone redirection operators starting at an unquoted boundary.
        if bytes[i] == b'&' && i + 1 < n && bytes[i + 1] == b'>' {
            let start = i;
            i += if i + 2 < n && bytes[i + 2] == b'>' {
                3
            } else {
                2
            };
            let raw = &clause[start..i];
            tokens.push(ShellToken {
                raw,
                text: raw.to_string(),
                quoted: false,
                expands: false,
                is_redirect: true,
            });
            continue;
        }
        if bytes[i] == b'<' || bytes[i] == b'>' {
            let start = i;
            i = advance_redirection_op(bytes, i);
            let raw = &clause[start..i];
            tokens.push(ShellToken {
                raw,
                text: raw.to_string(),
                quoted: false,
                expands: false,
                is_redirect: true,
            });
            continue;
        }

        let start = i;
        let mut text = String::new();
        let mut quote = QuoteState::None;
        let mut quoted = false;
        let mut expands = false;
        let mut all_unquoted_digits = true;

        while i < n {
            let b = bytes[i];
            match quote {
                QuoteState::Single => {
                    all_unquoted_digits = false;
                    if b == b'\'' {
                        quote = QuoteState::None;
                    } else {
                        let ch = clause[i..].chars().next().unwrap();
                        text.push(ch);
                        i += ch.len_utf8();
                        continue;
                    }
                    i += 1;
                }
                QuoteState::AnsiC => {
                    all_unquoted_digits = false;
                    if b == b'\\' && i + 1 < n {
                        let next_ch = clause[i + 1..].chars().next().unwrap();
                        text.push(next_ch);
                        i += 1 + next_ch.len_utf8();
                        continue;
                    } else if b == b'\'' {
                        quote = QuoteState::None;
                    } else {
                        let ch = clause[i..].chars().next().unwrap();
                        text.push(ch);
                        i += ch.len_utf8();
                        continue;
                    }
                    i += 1;
                }
                QuoteState::Double => {
                    all_unquoted_digits = false;
                    if b == b'\\' && i + 1 < n {
                        let next_b = bytes[i + 1];
                        if matches!(next_b, b'"' | b'\\' | b'$' | b'`') {
                            text.push(next_b as char);
                            i += 2;
                            continue;
                        }
                    }
                    if b == b'"' {
                        quote = QuoteState::None;
                        i += 1;
                    } else {
                        if b == b'$' || b == b'`' {
                            expands = true;
                        }
                        let ch = clause[i..].chars().next().unwrap();
                        text.push(ch);
                        i += ch.len_utf8();
                    }
                }
                QuoteState::None => {
                    if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    if b == b'<' || b == b'>' {
                        if all_unquoted_digits {
                            i = advance_redirection_op(bytes, i);
                            let raw = &clause[start..i];
                            tokens.push(ShellToken {
                                raw,
                                text: raw.to_string(),
                                quoted: false,
                                expands: false,
                                is_redirect: true,
                            });
                            break;
                        }
                        break;
                    }
                    if b == b'\\' && i + 1 < n {
                        all_unquoted_digits = false;
                        quoted = true;
                        let next_ch = clause[i + 1..].chars().next().unwrap();
                        text.push(next_ch);
                        i += 1 + next_ch.len_utf8();
                    } else if b == b'$' && i + 1 < n && bytes[i + 1] == b'\'' {
                        all_unquoted_digits = false;
                        quoted = true;
                        quote = QuoteState::AnsiC;
                        i += 2;
                    } else if b == b'\'' {
                        all_unquoted_digits = false;
                        quoted = true;
                        quote = QuoteState::Single;
                        i += 1;
                    } else if b == b'"' {
                        all_unquoted_digits = false;
                        quoted = true;
                        quote = QuoteState::Double;
                        i += 1;
                    } else {
                        if !b.is_ascii_digit() {
                            all_unquoted_digits = false;
                        }
                        if matches!(b, b'$' | b'`' | b'*' | b'?' | b'[')
                            || (i == start && b == b'~')
                        {
                            expands = true;
                        }
                        let ch = clause[i..].chars().next().unwrap();
                        text.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
        }
        // Only push if loop ended on a normal word (fd-prefixed redirect already pushed above).
        if i > start
            && !tokens.last().is_some_and(|t| {
                t.is_redirect && std::ptr::eq(t.raw.as_ptr(), bytes[start..].as_ptr())
            })
        {
            tokens.push(ShellToken {
                raw: &clause[start..i],
                text,
                quoted,
                expands,
                is_redirect: false,
            });
        }
    }
    tokens
}

fn advance_redirection_op(bytes: &[u8], i: usize) -> usize {
    let n = bytes.len();
    match bytes[i] {
        b'<' => {
            if i + 2 < n && bytes[i + 1] == b'<' && (bytes[i + 2] == b'<' || bytes[i + 2] == b'-') {
                i + 3
            } else if i + 1 < n && matches!(bytes[i + 1], b'<' | b'&' | b'>') {
                i + 2
            } else {
                i + 1
            }
        }
        b'>' => {
            if i + 1 < n && matches!(bytes[i + 1], b'>' | b'|' | b'&') {
                i + 2
            } else {
                i + 1
            }
        }
        _ => i + 1,
    }
}

fn is_assignment_word(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    if eq == 0 {
        return false;
    }
    word[..eq]
        .chars()
        .enumerate()
        .all(|(i, c)| c == '_' || c.is_alphabetic() || (i > 0 && c.is_ascii_digit()))
}

fn is_git_command_word(tok: &ShellToken<'_>) -> bool {
    !tok.expands && (tok.text == "git" || tok.text.ends_with("/git"))
}

fn parse_git_invocation(clause: &str) -> Option<GitInvocation<'_>> {
    let tokens = scan_shell_tokens(clause);
    let at = locate_git(&tokens)?;

    let mut c_args = Vec::new();
    let mut i = at + 1;
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok.is_redirect {
            i += 2;
            continue;
        }
        if tok.text == "--" || tok.text == "-" || !tok.text.starts_with('-') {
            break;
        }
        let next = match tok.text.as_str() {
            "-C" => {
                if i + 1 < tokens.len() {
                    c_args.push((
                        i,
                        Some(i + 1),
                        tokens[i + 1].text.clone(),
                        tokens[i + 1].expands,
                    ));
                    i + 2
                } else {
                    i + 1
                }
            }
            opt if opt.starts_with("-C") && opt.len() > 2 => {
                c_args.push((i, None, opt[2..].to_string(), tok.expands));
                i + 1
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
            | "--config-env" | "--attr-source" => {
                if i + 1 < tokens.len() {
                    i + 2
                } else {
                    i + 1
                }
            }
            _ => i + 1,
        };
        i = next;
    }
    if c_args.is_empty() {
        return None;
    }
    Some(GitInvocation { tokens, c_args })
}

/// The rule a `-C` that cannot be attributed to a git invocation is refused under.
pub(crate) const GIT_C_UNRESOLVED: &str = "git.c_unresolved";

/// Commands that run the command following them, after options of their own
/// (so `git` after one is in command position). Mirrors DevCouncil's
/// `commandWrappers` (backend/go_orchestrator/policy/command_harnessonly.go,
/// read at 63ca82ea6fe7), whose `argumentWrappers` (command_argwrites.go) is a
/// subset. A wrapper's options take arguments of an arity this reader cannot
/// know (`sudo -u x`, `timeout -s KILL 5`, `nice -n 5`), so like DevCouncil it
/// does not guess which word is the command: after a wrapper, the first later
/// word that is git is taken as the command.
const COMMAND_WRAPPERS: &[&str] = &[
    "env",
    "nice",
    "ionice",
    "time",
    "command",
    "exec",
    "builtin",
    "nohup",
    "setsid",
    "sudo",
    "doas",
    "xargs",
    "timeout",
    "stdbuf",
    "flock",
    "chronic",
    "caffeinate",
    "watch",
    "parallel",
    "-exec",
    "-execdir",
    "-ok",
    "-okdir",
    "chrt",
    "taskset",
    "unbuffer",
];

fn is_command_wrapper(tok: &ShellToken<'_>) -> bool {
    if tok.expands {
        return false;
    }
    let name = tok.text.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = name.to_lowercase();
    COMMAND_WRAPPERS.contains(&name.strip_suffix(".exe").unwrap_or(&name))
}

/// The index of the first word of the command the shell would run: past
/// assignments, redirections and reserved words.
fn command_position(tokens: &[ShellToken<'_>]) -> usize {
    let mut at = 0usize;
    while at < tokens.len() {
        let tok = &tokens[at];
        if tok.is_redirect {
            at += 2;
            continue;
        }
        if !tok.quoted
            && (is_assignment_word(&tok.text)
                || matches!(
                    tok.text.as_str(),
                    "if" | "elif"
                        | "while"
                        | "until"
                        | "do"
                        | "then"
                        | "else"
                        | "!"
                        | "time"
                        | "{"
                        | "("
                        | "command"
                        | "builtin"
                        | "noglob"
                ))
        {
            at += 1;
            continue;
        }
        break;
    }
    at
}

/// The index of the word the shell would run as git in one clause, or None.
///
/// Assignments, redirections, reserved words and [`COMMAND_WRAPPERS`] come
/// first; a git word anywhere else (an argument of `echo`, a quoted command
/// line) is not found here on purpose — the caller decides what an unreadable
/// `-C` means (see [`unresolved_git_c`]).
fn locate_git(tokens: &[ShellToken<'_>]) -> Option<usize> {
    let at = command_position(tokens);

    let first = tokens.get(at)?;
    if is_git_command_word(first) {
        return Some(at);
    }
    if !is_command_wrapper(first) {
        return None;
    }
    let mut j = at + 1;
    while j < tokens.len() {
        if tokens[j].is_redirect {
            j += 2;
            continue;
        }
        if is_git_command_word(&tokens[j]) {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn is_git_text(word: &str) -> bool {
    word == "git" || word.ends_with("/git")
}

fn is_dash_c_text(word: &str) -> bool {
    word.starts_with("-C")
}

/// How far into nested shell scripts (`bash -c "bash -c '…'"`) the -C reader
/// follows. A script nested deeper that contains an evaluator is refused: a
/// check that could not finish must not read as one that passed.
const MAX_SCRIPT_DEPTH: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Evaluator {
    /// `eval`: its arguments are joined into one script.
    Eval,
    /// A shell, `su` or `script`: the argument of its `-c` is a script.
    Shell,
}

fn evaluator_of(tok: &ShellToken<'_>) -> Option<Evaluator> {
    let name = tok.text.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = name.to_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "eval" => Some(Evaluator::Eval),
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "su" | "script" => Some(Evaluator::Shell),
        _ => None,
    }
}

/// The first shell evaluator in command position: the command word itself, or
/// one behind a [`COMMAND_WRAPPERS`] word (`env bash -c …`, `xargs sh -c …`,
/// `find -exec sh -c …`). A word like `sh` that is only an argument
/// (`echo sh -c …`) is not one.
fn locate_evaluator(tokens: &[ShellToken<'_>]) -> Option<(usize, Evaluator)> {
    let start = command_position(tokens);
    let mut armed = false;
    let mut j = start;
    while j < tokens.len() {
        let tok = &tokens[j];
        if tok.is_redirect {
            j += 2;
            continue;
        }
        if j == start {
            armed = true;
        }
        if armed {
            if let Some(kind) = evaluator_of(tok) {
                return Some((j, kind));
            }
        }
        armed = is_command_wrapper(tok) || (armed && j != start);
        j += 1;
    }
    None
}

/// The script string an evaluator runs: the argument of its `-c` (also inside
/// a short-option cluster such as `-lc`) for a shell, `su` or `script`, or
/// everything after `eval`. None when it runs a script file or reads stdin.
fn script_of(tokens: &[ShellToken<'_>], at: usize, kind: Evaluator) -> Option<String> {
    match kind {
        Evaluator::Eval => {
            let words: Vec<&str> = tokens[at + 1..]
                .iter()
                .filter(|tok| !tok.is_redirect)
                .map(|tok| tok.text.as_str())
                .collect();
            (!words.is_empty()).then(|| words.join(" "))
        }
        Evaluator::Shell => (at + 1..tokens.len()).find_map(|k| {
            let flag = tokens[k].text.as_str();
            let cluster = flag.starts_with('-')
                && !flag.starts_with("--")
                && flag.len() > 1
                && flag[1..].chars().all(char::is_alphabetic)
                && flag.contains('c');
            (cluster || flag == "--command")
                .then(|| tokens.get(k + 1).map(|tok| tok.text.clone()))
                .flatten()
        }),
    }
}

/// Whether one clause carries a `-C` that no located git invocation accounts
/// for. `in_script` is true for a clause read out of a string an evaluator
/// runs, where even a located `git -C` is refused: [`sibling_git_clauses`]
/// resolves the target of a clause it can see, not one inside a string.
fn clause_has_unresolved_git_c(clause: &str, in_script: bool, depth: usize) -> bool {
    let tokens = scan_shell_tokens(clause);
    if locate_git(&tokens).is_some() {
        return in_script && parse_git_invocation(clause).is_some();
    }
    // Unquoted words only: a quoted "git -C" is text being searched for or
    // printed, not a command.
    let mut seen_git = false;
    for tok in tokens.iter().filter(|tok| !tok.is_redirect && !tok.quoted) {
        for word in tok.text.split_whitespace() {
            if seen_git && is_dash_c_text(word) {
                return true;
            }
            if is_git_text(word) {
                seen_git = true;
            }
        }
    }
    let Some((at, kind)) = locate_evaluator(&tokens) else {
        return false;
    };
    let Some(script) = script_of(&tokens, at, kind) else {
        return false;
    };
    depth >= MAX_SCRIPT_DEPTH
        || split_shell_clauses(&script)
            .into_iter()
            .any(|inner| clause_has_unresolved_git_c(inner, true, depth + 1))
}

/// A clause with a `-C` that no located git invocation accounts for:
///
/// - unquoted `git` then `-C…` where git is not the command (`echo git -C ../x`,
///   a wrapper not in [`COMMAND_WRAPPERS`]), and
/// - a script string run by a shell evaluator (`bash -c '…'`, `sh -lc "…"`,
///   `eval "…"`, also behind a wrapper) that holds a git `-C`, located or not.
///
/// Such a `-C` may be aimed at another worktree whose scope was never
/// consulted, so a bound caller is refused rather than judged against its own
/// scope. A clause git is located in is left to [`sibling_git_clauses`]. A quoted
/// string that no evaluator runs (`rg -n "git -C" src`, `echo "git -C <dir>"`), a
/// `-C` before any git word (`grep -C 3 git f`) and one after git's subcommand
/// (`git log -C`) are not this case. Over-reports on purpose in one place:
/// an unquoted `rg git -C2 f`.
fn unresolved_git_c(command: &str) -> Option<&str> {
    split_shell_clauses(command)
        .into_iter()
        .find(|clause| clause_has_unresolved_git_c(clause, false, 0))
}

fn unresolved_git_c_verdict(command: &str, clause: &str, task_id: &str) -> PolicyVerdict {
    let shown: String = clause.trim().chars().take(120).collect();
    PolicyVerdict {
        status: PolicyStatus::Blocked,
        checked: false,
        target: command.to_string(),
        rule: GIT_C_UNRESOLVED.to_string(),
        severity: "hard".to_string(),
        reason: format!(
            "`-C` appears in `{shown}` but the git invocation it belongs to could not be \
             located, so the checkout it points at cannot be judged against its own task scope."
        ),
        demoted: String::new(),
        grant_id: String::new(),
        granted_by: String::new(),
        widened: String::new(),
        degraded: Vec::new(),
        task_id: task_id.to_string(),
        detail:
            "GitPulse refused before asking the harness: run git directly, or as `env git -C …`, \
                 so the target worktree can be resolved."
                .to_string(),
        detail_code: "git_c_unresolved".to_string(),
    }
}

pub(crate) fn has_divergent_relative_git_c(cwd: &Path, repo_root: &Path, command: &str) -> bool {
    let (Ok(canon_cwd), Ok(canon_root)) = (cwd.canonicalize(), repo_root.canonicalize()) else {
        return false;
    };
    if canon_cwd == canon_root {
        return false;
    }
    for clause in split_shell_clauses(command) {
        let Some(inv) = parse_git_invocation(clause) else {
            continue;
        };
        let mut cur_root = canon_root.clone();
        let mut cur_cwd = canon_cwd.clone();
        for (_, _, val, expands) in inv.c_args {
            if expands {
                break;
            }
            if Path::new(&val).is_absolute() {
                if let Some(next) =
                    crate::repository_trust::resolve_registered_worktree(&cur_root, &val)
                {
                    cur_root = next.clone();
                    cur_cwd = next;
                    continue;
                }
                break;
            }
            let via_root = crate::repository_trust::resolve_registered_worktree(&cur_root, &val);
            let via_cwd = cur_cwd
                .join(&val)
                .to_str()
                .and_then(|p| crate::repository_trust::resolve_registered_worktree(&cur_root, p));
            if via_root != via_cwd {
                return true;
            }
            match via_root {
                Some(next) => {
                    cur_root = next.clone();
                    cur_cwd = next;
                }
                None => break,
            }
        }
    }
    false
}

fn sibling_git_clauses(caller_root: &Path, command: &str) -> Vec<(PathBuf, String)> {
    let Ok(canon_caller) = caller_root.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for clause in split_shell_clauses(command) {
        let Some(inv) = parse_git_invocation(clause) else {
            continue;
        };
        let mut cur = canon_caller.clone();
        let mut omit = vec![false; inv.tokens.len()];
        let mut resolved_all = true;
        for (flag_idx, val_idx, val, expands) in &inv.c_args {
            if *expands {
                resolved_all = false;
                break;
            }
            let Some(next) = crate::repository_trust::resolve_registered_worktree(&cur, val) else {
                resolved_all = false;
                break;
            };
            omit[*flag_idx] = true;
            if let Some(vi) = val_idx {
                omit[*vi] = true;
            }
            cur = next;
        }
        if !resolved_all || cur == canon_caller {
            continue;
        }
        let mut idx = 0usize;
        while idx < inv.tokens.len() {
            if inv.tokens[idx].is_redirect {
                omit[idx] = true;
                if idx + 1 < inv.tokens.len() {
                    omit[idx + 1] = true;
                }
                idx += 2;
                continue;
            }
            idx += 1;
        }
        let rewritten: Vec<&str> = inv
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(i, tok)| if omit[i] { None } else { Some(tok.raw) })
            .collect();
        if !rewritten.is_empty() {
            out.push((cur, rewritten.join(" ")));
        }
    }
    out
}

/// Judges one write by an agent's file tool against this checkout's bound task
/// scope, recording nothing.
///
/// The file-tool hook's entry to the gate, resolved through the same
/// [`hook_scope`] as [`check_command_in_scope`] — so `echo x > f`, `sed -i … f`
/// and an Edit of `f` are measured against one declaration. Before this, the
/// Edit/Write hook checked collisions only, and the most common way an agent
/// writes a file was fenced by nothing.
///
/// An unbound checkout is not sent to the harness at all. There is no scope to
/// fence, and the file tools run far more often than anything else an agent
/// does; asking would add a sidecar round trip to every edit for an answer
/// (`task.absent`, demoted) that cannot change what the hook says.
pub(crate) fn check_file_in_scope(repo_path: &str, file_path: &str, op: &str) -> FileScopeCheck {
    match hook_scope(repo_path) {
        HookScope::Declared(scope) => FileScopeCheck::Judged(Box::new(ScopedVerdict {
            bound: true,
            unscoped: None,
            verdict: check_file(repo_path, file_path, op, Some(&scope)),
        })),
        HookScope::Unbound => FileScopeCheck::Unbound { unscoped: None },
        HookScope::Untrusted => FileScopeCheck::Unbound {
            unscoped: Some(UNSCOPED_UNTRUSTED),
        },
        HookScope::Unresolved(failure) => FileScopeCheck::Judged(Box::new(ScopedVerdict {
            bound: true,
            unscoped: None,
            verdict: failure.verdict(file_path),
        })),
    }
}

/// The rule a task-bound session's write into another checkout is refused under.
pub(crate) const SCOPE_OTHER_CHECKOUT: &str = "scope.other_checkout";

/// Judges one write by a session rooted at `session_root` into a file that
/// belongs to a different checkout, `owner_root`, where `owner_target` is the
/// file's path inside it.
///
/// A task scope is a list of paths inside the checkout the task is bound to, so
/// it never authorises a file in another checkout, and the same repo-relative
/// spelling in the plan must not be read as if it did. With `same_family` (the
/// two checkouts share one repository: the main checkout and its linked
/// worktrees) a session bound to a declared scope is therefore refused outright,
/// and one whose binding could not be resolved is refused as
/// [`TASK_SCOPE_UNAVAILABLE`] (it fails closed, as the in-checkout path does).
/// Otherwise, and for a file in an unrelated repository, the write is judged
/// against the owning checkout's own binding by [`check_file_in_scope`]; a
/// session binding says nothing about another repository's files.
pub(crate) fn check_file_in_other_checkout(
    session_root: &str,
    owner_root: &str,
    owner_target: &str,
    same_family: bool,
    nested_in_session: Option<&str>,
    op: &str,
) -> FileScopeCheck {
    // A repository nested under the session's checkout (a submodule or an
    // independent clone) used to be judged by the session's own plan at its
    // path there. It still is, first, so owning it separately can only add a
    // refusal; the owner's own binding is then asked as well.
    if let (false, Some(nested)) = (same_family, nested_in_session) {
        if let FileScopeCheck::Judged(judged) = check_file_in_scope(session_root, nested, op) {
            if judged.verdict.blocks() || judged.verdict.gate_failed() {
                return FileScopeCheck::Judged(judged);
            }
        }
    }
    if same_family {
        match hook_scope(session_root) {
            HookScope::Declared(scope) => {
                let verdict = PolicyVerdict {
                    status: PolicyStatus::Blocked,
                    checked: false,
                    target: owner_target.to_string(),
                    rule: SCOPE_OTHER_CHECKOUT.to_string(),
                    severity: "hard".to_string(),
                    reason: format!(
                        "Task {} authorizes changes inside {session_root} only; {owner_target} \
                         belongs to the checkout at {owner_root}, which is outside its scope",
                        scope.task_id
                    ),
                    demoted: String::new(),
                    grant_id: String::new(),
                    granted_by: String::new(),
                    widened: String::new(),
                    degraded: Vec::new(),
                    task_id: scope.task_id,
                    detail: "GitPulse refused before asking the harness: the write targets \
                             another checkout of this repository."
                        .to_string(),
                    detail_code: "other_checkout".to_string(),
                };
                return FileScopeCheck::Judged(Box::new(ScopedVerdict {
                    bound: true,
                    unscoped: None,
                    verdict,
                }));
            }
            HookScope::Unresolved(failure) => {
                return FileScopeCheck::Judged(Box::new(ScopedVerdict {
                    bound: true,
                    unscoped: None,
                    verdict: failure.verdict(owner_target),
                }));
            }
            HookScope::Unbound | HookScope::Untrusted => {}
        }
    }
    check_file_in_scope(owner_root, owner_target, op)
}

/// What [`check_file_in_scope`] established.
#[derive(Debug, Clone)]
pub(crate) enum FileScopeCheck {
    /// No task scope applies, so the harness was not asked. `unscoped` says
    /// why when that is not simply "no task is bound here".
    Unbound { unscoped: Option<&'static str> },
    /// The write was measured against a declared scope, or refused because the
    /// declared scope could not be read.
    Judged(Box<ScopedVerdict>),
}

/// Why the hook judged a checkout without a scope it could not rule out.
pub(crate) const UNSCOPED_UNTRUSTED: &str = "unscoped: repository not trusted";

/// The scope the agent hook measures against, resolved by [`hook_scope`] and
/// sorted by how each outcome degrades on the hook's side of the gate.
enum HookScope {
    Declared(HostScope),
    /// No binding, or no checkout left on disk to hold one.
    Unbound,
    /// GitPulse is not trusted here, so no binding could be looked up.
    Untrusted,
    /// A binding exists, or could not be ruled out, and its scope could not be
    /// produced.
    Unresolved(ScopeFailure),
}

fn hook_scope(repo_path: &str) -> HookScope {
    let binding = crate::ledger::bindings::resolve_binding_readonly(repo_path, repo_path);
    match scope_from_binding(binding) {
        Ok(Some(scope)) => HookScope::Declared(scope),
        Ok(None) => HookScope::Unbound,
        Err(failure) => match failure.kind {
            ScopeFailureKind::Untrusted => HookScope::Untrusted,
            ScopeFailureKind::Missing => HookScope::Unbound,
            ScopeFailureKind::Broken => HookScope::Unresolved(failure),
        },
    }
}

/// A verdict and whether it was measured against a declared task scope.
#[derive(Debug, Clone)]
pub(crate) struct ScopedVerdict {
    pub verdict: PolicyVerdict,
    /// True when the checkout is bound to a task, or a binding could not be
    /// ruled out. Never inferred from `verdict.task_id`.
    pub bound: bool,
    /// Set when the verdict was reached without a scope that might exist —
    /// [`UNSCOPED_UNTRUSTED`] — so the answer can say so rather than read as
    /// an unbound checkout's.
    pub unscoped: Option<&'static str>,
}

/// Evaluates one file write, on the same terms as [`guard_command`].
pub(crate) fn guard_file(
    repo_path: &str,
    file_path: &str,
    op: &str,
) -> Result<PolicyVerdict, String> {
    let action = format!("file.{}", if op.is_empty() { "write" } else { op });
    let address = crate::ledger::bindings::repository_address(repo_path);
    let scope = match scope_at(&address) {
        Ok(scope) => scope,
        Err(failure) if failure.kind == ScopeFailureKind::Missing => {
            return Err(failure.missing_checkout(file_path));
        }
        Err(failure) => {
            let verdict = failure.verdict(file_path);
            record_gate(
                repo_path,
                address.as_ref().ok(),
                &action,
                file_path,
                None,
                &verdict,
            );
            return gated(verdict);
        }
    };
    let verdict = check_file(repo_path, file_path, op, scope.as_ref());
    record_gate(
        repo_path,
        address.as_ref().ok(),
        &action,
        file_path,
        None,
        &verdict,
    );
    gated(verdict)
}

/// How many file checks [`guard_files`] keeps in flight at once. The sidecar
/// correlates replies by id, so overlapping calls proceed side by side; the cap
/// keeps one rename from queueing thousands of requests behind each other.
const GUARD_FILES_CONCURRENCY: usize = 8;

/// Evaluates `op` on each of `files`, one verdict per file, in order — exactly
/// what calling [`guard_file`] on each in turn answers, in less time.
///
/// The scope is resolved once instead of once per file, and the checks go to
/// the sidecar [`GUARD_FILES_CONCURRENCY`] at a time. The verdicts are then
/// recorded and gated in order: the first file whose verdict refuses (or that
/// could not be checked) ends the walk with the same `Err` `guard_file` would
/// have returned for it, and nothing after it is recorded. Workers stop taking
/// new files once one refuses, so a dead or refusing sidecar costs a handful of
/// timeouts, not one per file.
///
/// A scope that cannot be resolved blocks every file alike; that case hands
/// the list to [`guard_file`] unchanged so its classification is the one used.
pub(crate) fn guard_files(
    repo_path: &str,
    files: &[&str],
    op: &str,
) -> Result<Vec<PolicyVerdict>, String> {
    guard_files_with(repo_path, files, op, GUARD_FILES_CONCURRENCY)
}

/// [`guard_files`] with `workers` checks in flight. Production always runs
/// [`GUARD_FILES_CONCURRENCY`]; the count is a parameter so a test drives the
/// same concurrent path, not a one-worker stand-in.
fn guard_files_with(
    repo_path: &str,
    files: &[&str],
    op: &str,
    workers: usize,
) -> Result<Vec<PolicyVerdict>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let address = crate::ledger::bindings::repository_address(repo_path);
    let scope = match scope_at(&address) {
        Ok(scope) => scope,
        Err(_) => return files.iter().map(|f| guard_file(repo_path, f, op)).collect(),
    };
    let action = format!("file.{}", if op.is_empty() { "write" } else { op });

    // The test build's one fake sidecar is guarded by a lock that only the
    // thread that took it may re-enter (see `sidecar::test_serial`). This
    // thread blocks until every worker is done, so it lends what it holds to
    // them; without a lend a worker would wait on its own spawner.
    #[cfg(test)]
    let lent = sidecar::test_serial_held_here();
    let judged = judge_in_order(
        files,
        workers,
        |file| {
            #[cfg(test)]
            let _lent = sidecar::lend_test_serial(lent);
            check_file(repo_path, file, op, scope.as_ref())
        },
        |verdict| verdict.blocks() || verdict.gate_failed(),
    );

    let mut verdicts = Vec::with_capacity(files.len());
    for (file, verdict) in files.iter().zip(judged) {
        let verdict =
            verdict.ok_or_else(|| format!("No policy verdict was produced for {file}"))?;
        record_gate(
            repo_path,
            address.as_ref().ok(),
            &action,
            file,
            None,
            &verdict,
        );
        verdicts.push(gated(verdict)?);
    }
    Ok(verdicts)
}

/// Runs `check` on every file with up to `workers` in flight, claiming files in
/// order, and stops claiming once a result satisfies `refuses`.
///
/// Slot `i` holds the result for `files[i]`, or `None` if it was never
/// claimed. Because files are claimed in order and every worker finishes
/// before this returns, every slot before the first refusing one is `Some`.
fn judge_in_order<V: Send>(
    files: &[&str],
    workers: usize,
    check: impl Fn(&str) -> V + Sync,
    refuses: impl Fn(&V) -> bool + Sync,
) -> Vec<Option<V>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<V>>> = files.iter().map(|_| Mutex::new(None)).collect();
    let work = || loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let index = next.fetch_add(1, Ordering::AcqRel);
        let Some(file) = files.get(index) else {
            return;
        };
        let result = check(file);
        if refuses(&result) {
            stop.store(true, Ordering::Release);
        }
        *slots[index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
    };
    let workers = workers.clamp(1, files.len().max(1));
    if workers == 1 {
        work();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(work);
            }
        });
    }
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .collect()
}

/// Test-only door to [`judge_in_order`] and [`GUARD_FILES_CONCURRENCY`], so the
/// hub-rename benchmark drives the fan-out production uses for rewrites with
/// its own counting check instead of a copy of it. Never refuses early.
#[cfg(test)]
pub(crate) fn judge_files_for_bench<V: Send>(
    files: &[&str],
    workers: usize,
    check: impl Fn(&str) -> V + Sync,
) -> Vec<Option<V>> {
    judge_in_order(files, workers, check, |_| false)
}

/// The worker count [`guard_files`] runs in production.
#[cfg(test)]
pub(crate) const fn production_guard_workers() -> usize {
    GUARD_FILES_CONCURRENCY
}

/// Test-only door to [`guard_files_with`], so the live-sidecar benchmark runs
/// the production walk (scope once, judge, record, gate) at a chosen worker
/// count.
#[cfg(test)]
pub(crate) fn guard_files_with_workers_for_bench(
    repo_path: &str,
    files: &[&str],
    op: &str,
    workers: usize,
) -> Result<Vec<PolicyVerdict>, String> {
    guard_files_with(repo_path, files, op, workers)
}

/// Test-only: the sidecar round trip alone. Resolves the scope once, then
/// times every [`check_file`] call (no ledger record, no gating) on `workers`
/// threads, returning each file's duration and verdict in order, and the byte
/// size of the declared scope every request carries (0 when unbound). A check
/// that could not run is an `Err`, never a duration.
#[cfg(test)]
pub(crate) fn time_file_checks_for_bench(
    repo_path: &str,
    files: &[&str],
    op: &str,
    workers: usize,
) -> Result<(Vec<(std::time::Duration, PolicyVerdict)>, usize), String> {
    let address = crate::ledger::bindings::repository_address(repo_path);
    let scope = scope_at(&address).map_err(|failure| failure.reason)?;
    let scope_bytes = scope.as_ref().map_or(0, |scope| {
        serde_json::to_vec(scope).map_or(0, |bytes| bytes.len())
    });
    let lent = sidecar::test_serial_held_here();
    let timed = judge_in_order(
        files,
        workers,
        |file| {
            let _lent = sidecar::lend_test_serial(lent);
            let started = std::time::Instant::now();
            let verdict = check_file(repo_path, file, op, scope.as_ref());
            (started.elapsed(), verdict)
        },
        |_| false,
    );
    let each = timed
        .into_iter()
        .zip(files)
        .map(|(slot, file)| match slot {
            Some((_, verdict)) if verdict.gate_failed() => {
                Err(format!("the sidecar could not judge {file}"))
            }
            Some(judged) => Ok(judged),
            None => Err(format!("{file} was never judged")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((each, scope_bytes))
}

/// Test-only: a sidecar counted from outside. A `manvi` wrapper that appends
/// one line to `log` every time the sidecar process starts, then becomes the
/// real binary (`exec`, so the pid is the sidecar's). Returns the script path.
#[cfg(all(test, unix))]
pub(crate) fn counting_manvi_wrapper(
    dir: &std::path::Path,
    real: &str,
    log: &std::path::Path,
) -> String {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("counting-manvi");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\nexec '{}' \"$@\"\n",
        log.display(),
        real
    );
    std::fs::write(&script, body).expect("write the counting wrapper");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make the counting wrapper executable");
    script.to_string_lossy().into_owned()
}

/// The task scope this checkout is working under, if it is bound to one.
///
/// Two lookups, both cheap and both allowed to come back empty:
///
/// 1. the ledger, for the binding recorded when the worktree was bound;
/// 2. DevCouncil's store, for that task's declared scope.
///
/// Returning `None` is the ordinary case — most checkouts are bound to no
/// task — and it reproduces exactly the behaviour that existed before scopes:
/// the ladder stops at `task.absent` and the host posture demotes it.
///
/// A binding that resolves to a task the store cannot produce is a hard local
/// failure. Treating it as `None` would silently turn a scoped checkout into an
/// unbound one; inventing an empty scope would misreport the dependency outage
/// as a valid plan. The explicit error blocks before either ambiguity reaches
/// the sidecar.
///
/// That block is GitPulse's, not the harness's, and the verdict says so with
/// `checked: false`: the harness was never asked. It used to claim
/// `checked: true` and render as "Blocked by the MANVI harness", which blamed
/// MANVI for a lookup GitPulse could not complete — on a worktree that had
/// merely been deleted, among others.
#[derive(Debug)]
struct ScopeFailure {
    task_id: String,
    reason: String,
    kind: ScopeFailureKind,
}

/// Why a scope could not be resolved. The kinds degrade differently, so they are
/// told apart where the failure is first seen rather than re-parsed later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeFailureKind {
    /// GitPulse is not trusted in this repository, so the binding was never
    /// looked up — not because a binding is broken.
    Untrusted,
    /// The checkout is not on disk any more (removed or moved). There is no
    /// binding to enforce and no directory for the action to run in.
    Missing,
    /// A binding exists, or could not be ruled out, and its scope could not be
    /// produced. The case the fail-closed rule exists for.
    Broken,
}

/// The rule a scope failure is reported under.
pub(crate) const TASK_SCOPE_UNAVAILABLE: &str = "task.scope_unavailable";

impl ScopeFailure {
    fn verdict(self, target: &str) -> PolicyVerdict {
        PolicyVerdict {
            status: PolicyStatus::Blocked,
            checked: false,
            target: target.to_string(),
            rule: TASK_SCOPE_UNAVAILABLE.to_string(),
            severity: "hard".to_string(),
            reason: self.reason,
            demoted: String::new(),
            grant_id: String::new(),
            granted_by: String::new(),
            widened: String::new(),
            degraded: Vec::new(),
            task_id: self.task_id,
            detail: "GitPulse refused before asking the harness: the task scope this \
                     checkout is bound to could not be resolved."
                .to_string(),
            detail_code: "scope_unavailable".to_string(),
        }
    }

    /// The error for an action aimed at a checkout that no longer exists.
    ///
    /// Not a policy verdict and not recorded: nothing was judged, the action
    /// cannot run in a directory that is gone, and a ledger row would have to
    /// be written under that same missing path — which is how a deleted
    /// worktree used to add a `mkdir_failed` warning to every refused action.
    fn missing_checkout(self, target: &str) -> String {
        log::info!(target: "harness", "refused an action on a missing checkout: {}", self.reason);
        format!(
            "This checkout no longer exists on disk — it was removed or moved — so GitPulse \
             did not run this.\n  target: {target}\n  detail: {}",
            self.reason
        )
    }
}

/// The scope at a family address the app's gate resolved for this action.
///
/// A failed resolution is classified exactly as a failed binding lookup, which
/// is what it was: the address is the first half of every binding lookup.
fn scope_at(
    address: &Result<crate::ledger::bindings::BindingAddress, crate::ledger::LedgerError>,
) -> Result<Option<HostScope>, ScopeFailure> {
    scope_from_binding(
        address
            .clone()
            .and_then(|address| crate::ledger::bindings::binding_at(&address)),
    )
}

/// Turns a binding lookup into the scope it declares.
///
/// The one owner of that step for both callers: the app's gate, which looks
/// the binding up at the address it resolved ([`scope_at`]), and the agent
/// hook, which looks it up read-only ([`hook_scope`]).
fn scope_from_binding(
    binding: Result<Option<crate::ledger::bindings::ResolvedBinding>, crate::ledger::LedgerError>,
) -> Result<Option<HostScope>, ScopeFailure> {
    let binding = binding.map_err(|error| {
        let kind = match error.code {
            "untrusted_worktree" => ScopeFailureKind::Untrusted,
            crate::ledger::bindings::MISSING_WORKTREE => ScopeFailureKind::Missing,
            _ => ScopeFailureKind::Broken,
        };
        ScopeFailure {
            task_id: String::new(),
            kind,
            reason: format!("could not resolve this worktree's task binding: {error}"),
        }
    })?;
    let Some(binding) = binding else {
        return Ok(None);
    };
    let scope = crate::tasks::scope(&binding.anchor, &binding.task_id)
        .map_err(|error| ScopeFailure {
            task_id: binding.task_id.clone(),
            kind: ScopeFailureKind::Broken,
            reason: format!("could not read the bound task's scope: {error}"),
        })?
        .ok_or_else(|| ScopeFailure {
            task_id: binding.task_id.clone(),
            kind: ScopeFailureKind::Broken,
            reason: "the bound task's scope is no longer available".to_string(),
        })?;
    Ok(Some(HostScope {
        task_id: scope.id,
        // The union the gate measures against: what the planner authorised
        // plus what the executor argued into its own scope. They stay
        // distinguishable in the store, and the harness reports a write
        // authorised by the second half as `widened` rather than clean.
        planned_files: scope
            .planned_files
            .into_iter()
            .chain(scope.agent_appended_files)
            .collect(),
        forbidden_changes: scope.forbidden_changes,
        worktree: binding.worktree,
        allowed_commands: scope.allowed_commands,
    }))
}

/// Writes one gate decision to the ledger.
///
/// This is the seam every guarded mutation already passes through, which is
/// what makes the record complete without touching the thirty-six call sites
/// above it. `repoStore.runMutating()` on the frontend covers only about four
/// fifths of the writes — worktree operations, `cmd_write_file_content`,
/// `cmd_restack`, interactive rebase and the PTY all reach Git without it — so
/// hooking the frontend seam instead would have recorded a history that looked
/// complete and was not.
///
/// # What `outcome` means on these rows
///
/// A gate row records what became of the *request*, at the moment the gate
/// answered:
///
/// * `blocked` — the gate refused, or could not run when it should have.
///   GitPulse did not perform the action. This is a complete and final fact.
/// * `ok` — the gate permitted it, and the action was about to run.
///
/// An `ok` gate row is therefore **not** a claim that the operation succeeded;
/// it is a claim that it was authorised. Rows carrying `{"phase":"gate"}` in
/// `detail_json` say so explicitly, so a consumer counting successful commits
/// cannot mistake an authorisation for a result. Completion rows, which do
/// carry the operation's own outcome, are written by the callers that observe
/// it and are absent from this build — the distinction is recorded here rather
/// than left for a reader to infer.
///
/// `address` is the family address the caller resolved for this action, or
/// `None` when it could not be resolved; the row then goes to `repo_path`.
fn record_gate(
    repo_path: &str,
    address: Option<&crate::ledger::bindings::BindingAddress>,
    action: &str,
    target: &str,
    argv_json: Option<String>,
    verdict: &PolicyVerdict,
) {
    use crate::ledger::{ActorKind, Draft, Outcome};

    // A refusal and a gate that could not run are both "this did not happen".
    // `gated` turns each into an `Err`, and the ledger must agree with it —
    // otherwise the history would show an authorised action the app refused.
    let blocked = verdict.blocks() || verdict.gate_failed();

    let ledger_repo = address
        .map(|address| address.anchor.clone())
        .unwrap_or_else(|| repo_path.to_string());
    let worktree_path = address
        .and_then(|address| (address.worktree != address.anchor).then(|| address.worktree.clone()));

    crate::ledger::record(Draft {
        repo_path: ledger_repo,
        worktree_path,
        // Taken from the verdict rather than looked up again: the ledger must
        // record the task the gate actually measured against, not the one a
        // second lookup would find now.
        task_id: Some(verdict.task_id.clone()).filter(|t| !t.is_empty()),
        action: action.to_string(),
        object: Some(target.to_string()),
        argv_json,
        // The human at the keyboard is the actor for everything that reaches
        // this seam today. Agent attribution arrives with transcript ingest;
        // until then, claiming `Agent` here would be a guess written to disk.
        actor_kind: Some(ActorKind::Human),
        outcome: Some(if blocked {
            Outcome::Blocked
        } else {
            Outcome::Ok
        }),
        verdict_json: serde_json::to_string(verdict).ok(),
        detail_json: Some(r#"{"phase":"gate"}"#.to_string()),
        ..Default::default()
    });
}

/// The single place a verdict becomes permission to act.
///
/// Two things refuse here, and the second is the one that is easy to miss. A
/// rule firing is obvious. A gate that *could not run* is not: an unchecked
/// verdict does not block, so before this seam refused it, a busy or wedged
/// sidecar let `git push --force` straight through while the UI recorded it as
/// merely "unguarded" — the check that could not run reporting exactly what a
/// check that ran and passed reports. See [`PolicyVerdict::gate_failed`] for
/// why a missing harness is the only unchecked verdict that may proceed.
fn gated(verdict: PolicyVerdict) -> Result<PolicyVerdict, String> {
    if verdict.blocks() {
        return Err(verdict.refusal());
    }
    if verdict.gate_failed() {
        return Err(verdict.gate_failure());
    }
    Ok(verdict)
}

#[cfg(test)]
mod ledger_seam_tests {
    use super::*;

    /// Every guarded mutation reaches the ledger, whatever the gate said.
    ///
    /// This drives the real seam rather than the ledger API, because the claim
    /// being tested is about coverage: that a mutation cannot reach Git without
    /// leaving a durable record. Testing `ledger::append` directly would prove
    /// the ledger works and nothing about whether it is wired to anything.
    ///
    /// It passes with or without a MANVI harness installed. With one, the row
    /// carries the harness's verdict; without one, it carries an `unchecked`
    /// verdict — and recording *that* is the point, since an action that ran
    /// with no gate is exactly what a history must not omit.
    #[test]
    fn a_guarded_command_leaves_a_durable_row() {
        // Serialized with every other sidecar-touching test: this reaches the
        // one process-global slot, so a fake installed elsewhere would answer
        // these requests instead of the real resolution path.
        let _serial = sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_buf = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let repo = repo_buf.to_str().expect("utf8 path");

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_command(repo, &["git", "commit", "-m", "hello"]);
        let after = crate::ledger::latest_cursor(repo).expect("cursor");
        assert!(after > before, "the guard seam recorded nothing");

        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        assert_eq!(events.len(), 1, "exactly one row per guarded command");
        let row = &events[0];

        assert_eq!(row.action, "git.commit", "the verb comes from argv");
        assert_eq!(row.repo_path, repo);
        assert_eq!(
            row.argv_json.as_deref(),
            Some(r#"["git","commit","-m","hello"]"#),
            "the argv is recorded as sent"
        );
        assert!(
            row.verdict_json.is_some(),
            "a row with no verdict at all would read as an unjudged action"
        );
        assert_eq!(
            row.detail_json.as_deref(),
            Some(r#"{"phase":"gate"}"#),
            "a gate row must say so, or an `ok` on it reads as a completed commit"
        );
        // Whatever the harness said, the row's outcome must agree with whether
        // the action was allowed to proceed.
        assert!(
            matches!(row.outcome.as_str(), "ok" | "blocked"),
            "unexpected outcome {:?}",
            row.outcome
        );
    }

    /// The verdict-returning entry the worktree hook uses records exactly as
    /// the guard does, before it answers: a caller that degrades differently
    /// on a failed gate still cannot act on an unrecorded judgement.
    #[test]
    fn a_judged_command_leaves_a_durable_row_before_it_answers() {
        let _serial = sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_buf = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let repo = repo_buf.to_str().expect("utf8 path");
        let argv = ["git", "worktree", "add", "-b", "worktree-x", "/tmp/x"];

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let verdict = judge_command(repo, &argv).expect("a checkout to judge under");
        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        assert_eq!(events.len(), 1, "exactly one row per judged command");
        let row = &events[0];
        assert_eq!(row.action, "git.worktree");
        assert_eq!(
            row.argv_json.as_deref(),
            Some(serde_json::to_string(&argv).unwrap().as_str())
        );
        // The row and the answer agree on whether the command may run.
        assert_eq!(row.outcome == "blocked", verdict.blocks(), "{row:?}");
        // And `guard_command` is still this plus `gated`.
        assert_eq!(guard_command(repo, &argv).is_ok(), gated(verdict).is_ok());
    }

    /// A file write is recorded under its own operation, not a git verb.
    #[test]
    fn a_guarded_file_write_records_its_operation() {
        // Serialized with every other sidecar-touching test: this reaches the
        // one process-global slot, so a fake installed elsewhere would answer
        // these requests instead of the real resolution path.
        let _serial = sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8 path");

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_file(repo, "src/lib.rs", "modify");
        let events = crate::ledger::tail(repo, before, 10).expect("tail");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "file.modify");
        assert_eq!(events[0].object.as_deref(), Some("src/lib.rs"));
    }

    /// An unspecified operation is recorded as `write`, matching the harness's
    /// own reading of an empty op — not dropped, and not guessed at.
    #[test]
    fn an_unspecified_file_operation_is_recorded_as_a_write() {
        // Serialized with every other sidecar-touching test: this reaches the
        // one process-global slot, so a fake installed elsewhere would answer
        // these requests instead of the real resolution path.
        let _serial = sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_str().expect("utf8 path");
        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let _ = guard_file(repo, "x.txt", "");
        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        assert_eq!(events[0].action, "file.write");
    }

    #[test]
    fn a_non_git_command_is_not_filed_under_a_git_verb() {
        // Serialized with every other sidecar-touching test: this reaches the
        // one process-global slot, so a fake installed elsewhere would answer
        // these requests instead of the real resolution path.
        let _serial = sidecar::test_serial();
        // `command.run` rather than a fabricated `git.*`: a mutation this build
        // does not recognise must still be recorded, and must not claim to be
        // something it is not.
        assert_eq!(
            crate::ledger::action_for_argv(&["cargo", "test"]),
            "command.run"
        );
        assert_eq!(crate::ledger::action_for_argv(&["git"]), "command.run");
        assert_eq!(crate::ledger::action_for_argv(&[]), "command.run");
        assert_eq!(
            crate::ledger::action_for_argv(&["git", "commit"]),
            "git.commit"
        );
        // A subcommand carrying separators still yields a legal action name,
        // because the event schema constrains the shape.
        assert_eq!(
            crate::ledger::action_for_argv(&["git", "cherry-pick"]),
            "git.cherry_pick"
        );
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    use crate::test_support::git_in;

    fn git_repo() -> tempfile::TempDir {
        let main = tempfile::tempdir().expect("main checkout");
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed repository");
        git_in(main.path(), &["add", "seed.txt"]);
        git_in(main.path(), &["commit", "-m", "seed"]);
        main
    }

    fn linked_repo() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let main = git_repo();
        let parent = tempfile::tempdir().expect("worktree parent");
        let worktree = parent.path().join("task-worktree");
        git_in(
            main.path(),
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().expect("utf8 worktree path"),
            ],
        );
        crate::test_support::trust_repo(&worktree);
        let worktree = worktree
            .canonicalize()
            .expect("canonical worktree")
            .to_string_lossy()
            .into_owned();
        (main, parent, worktree)
    }

    /// The scope the app's gate computes for `repo`: one family resolution,
    /// then the binding at that address.
    fn scope_for(repo: &str) -> Result<Option<HostScope>, ScopeFailure> {
        scope_at(&crate::ledger::bindings::repository_address(repo))
    }

    /// `git worktree list` children spawned in `cwd` while `f` ran.
    #[cfg(unix)]
    fn family_resolutions<T>(cwd: &str, f: impl FnOnce() -> T) -> (T, usize) {
        let cwd = std::path::Path::new(cwd);
        let before = crate::engine::process::spawn_log::spawns_in(cwd).len();
        let value = f();
        let count = crate::engine::process::spawn_log::spawns_in(cwd)
            .into_iter()
            .skip(before)
            .filter(|argv| {
                argv.windows(2)
                    .any(|w| w[0] == "worktree" && w[1] == "list")
            })
            .count();
        (value, count)
    }

    /// One gated action resolves its worktree family once. The scope lookup
    /// and the ledger row each used to resolve it — a `git worktree list` and
    /// a consolidation pass apiece — so every guarded command or write paid
    /// two.
    #[cfg(unix)]
    #[test]
    fn a_gated_action_resolves_its_family_once() {
        use std::os::unix::fs::PermissionsExt;

        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");
        let script = main.path().join("allowing-manvi");
        std::fs::write(&script, FAKE_ALLOW).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        let before = crate::ledger::latest_cursor(main_path).expect("family cursor");
        let (written, resolutions) = family_resolutions(&worktree, || {
            guard_file(&worktree, "src/planned.rs", "modify")
        });
        written.expect("allowed write");
        assert_eq!(
            resolutions, 1,
            "a guarded write resolved its family {resolutions} times"
        );

        // The first action authenticated this family with Git and it is
        // remembered while the registry is unchanged, so the second asks Git
        // no more: "at most once", never twice.
        let (ran, resolutions) =
            family_resolutions(&worktree, || guard_command(&worktree, &["git", "status"]));
        ran.expect("allowed command");
        assert!(
            resolutions <= 1,
            "a guarded command resolved its family {resolutions} times"
        );

        // Still recorded once each, in the family ledger, under the checkout.
        let events = crate::ledger::tail(main_path, before, 10).expect("family tail");
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events
            .iter()
            .all(|event| event.worktree_path.as_deref() == Some(worktree.as_str())));
    }

    /// The hook's scope comes from the read-only binding lookup: a bound
    /// checkout still declares its scope, and no read-write ledger open — the
    /// chmod pass, WAL pragmas and schema DDL — happens to find it.
    #[test]
    fn the_hook_scope_is_read_without_opening_the_ledger_for_writing() {
        let dir = git_repo();
        let repo_path = dir.path().canonicalize().expect("canonical repository");
        let repo = repo_path.to_str().expect("utf8");
        let db = crate::tasks::store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        crate::tasks::plant_task::plant(
            &store,
            "TASK-HOOK",
            "in_progress",
            r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}]}"#,
        );
        crate::ledger::bindings::bind(repo, repo, "TASK-HOOK").expect("bind");
        // A hook is a fresh process: nothing cached.
        crate::ledger::tests_support::reset_registry_for(repo);
        let opens = crate::ledger::tests_support::write_opens();

        let HookScope::Declared(scope) = hook_scope(repo) else {
            panic!("a bound checkout must declare its scope to the hook");
        };
        assert_eq!(scope.task_id, "TASK-HOOK");
        assert_eq!(scope.planned_files, ["src/planned.rs"]);
        assert_eq!(
            crate::ledger::tests_support::write_opens(),
            opens,
            "the hook opened the ledger read-write to read a binding"
        );
    }

    /// The hook sorts the read-only lookup's failures exactly as before:
    /// untrusted is judged unscoped (and says so), a missing checkout has no
    /// binding, and history it cannot rule out fails closed.
    #[test]
    fn the_hook_scope_sorts_each_failure_as_before() {
        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");

        // Legacy sibling history the anchor has not imported, holding a row
        // that could still set a binding: Broken. (A row that cannot — a
        // commit — no longer refuses; see `ledger::bindings::readonly_tests`.)
        crate::ledger::append(crate::ledger::Draft {
            repo_path: worktree.clone(),
            worktree_path: Some(worktree.clone()),
            object: Some(worktree.clone()),
            action: crate::ledger::bindings::UNBIND.to_string(),
            actor_kind: Some(crate::ledger::ActorKind::Human),
            outcome: Some(crate::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy sibling row");
        assert!(matches!(hook_scope(&worktree), HookScope::Unresolved(_)));

        // Consolidated by GitPulse: unbound again.
        crate::ledger::bindings::repository_address(main_path).expect("consolidate");
        assert!(matches!(hook_scope(&worktree), HookScope::Unbound));

        // Untrusted.
        crate::repository_trust::revoke(main_path).expect("revoke");
        assert!(matches!(hook_scope(&worktree), HookScope::Untrusted));
        crate::test_support::trust_repo(main.path());

        // Missing. The legacy row above opened the worktree's own ledger, and
        // this process keeps that connection. Unix deletes a directory under an
        // open file and drops the connection later; Windows refuses the delete
        // while the handle is open (os error 32), so it is released first.
        crate::ledger::tests_support::reset_registry_for(&worktree);
        crate::test_support::remove_dir_all_retrying(Path::new(&worktree))
            .expect("delete worktree");
        assert!(matches!(hook_scope(&worktree), HookScope::Unbound));
    }

    /// A `manvi serve` stand-in that answers the handshake, records every
    /// later request, and refuses with a scope verdict.
    #[cfg(unix)]
    const FAKE_RECORDER: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  reply "$line" '{"action":"deny","rule":"scope.unplanned","severity":"soft","reason":"outside the plan","target":"docs/elsewhere.md","task_id":"TASK-7","demoted":"","degraded":["repo_map.unavailable"]}'
done
"#;

    #[cfg(unix)]
    const FAKE_ALLOW: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  reply "$line" '{"action":"allow","rule":"","severity":"","reason":"","target":"","task_id":"","demoted":"","degraded":[]}'
done
"#;

    /// GitPulse sends the declared scope on the wire, and keeps the task the
    /// harness answers with.
    ///
    /// Hermetic on purpose: it drives a recording fake sidecar rather than
    /// whatever `manvi` happens to be installed. An earlier version of this
    /// test used the real binary and passed against a build with no scope
    /// support at all — the verdict came back `task.absent`, which is not
    /// `Allowed`, so a weak assertion called it a success. The bar here is that
    /// the scope is provably on the wire.
    #[cfg(unix)]
    #[test]
    fn a_bound_worktree_sends_its_scope_and_keeps_the_task() {
        use std::os::unix::fs::PermissionsExt;

        let dir = git_repo();
        let repo_path = dir.path().canonicalize().expect("canonical repository");
        let repo = repo_path.to_str().expect("utf8");

        let db = crate::tasks::store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        crate::tasks::plant_task::plant(
            &store,
            "TASK-7",
            "in_progress",
            r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}]}"#,
        );

        // Unbound, there is nothing to declare — the behaviour every existing
        // checkout keeps.
        assert!(
            scope_for(repo).expect("binding lookup").is_none(),
            "an unbound checkout declares nothing"
        );

        crate::ledger::bindings::bind(repo, repo, "TASK-7").expect("bind");
        let scope = scope_for(repo)
            .expect("binding lookup")
            .expect("a bound checkout declares its scope");
        assert_eq!(scope.task_id, "TASK-7");
        assert_eq!(scope.planned_files, vec!["src/planned.rs"]);
        assert_eq!(scope.worktree, repo);

        // A sidecar that records every request and refuses, so both halves of
        // the round trip are observable.
        let requests = dir.path().join("requests.ndjson");
        let script = dir.path().join("recording-manvi");
        let body = FAKE_RECORDER.replace("@REQUESTS@", &requests.display().to_string());
        std::fs::write(&script, body).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        // The hook learns the binding from here, never from the verdict's
        // task id, which may be the harness's `host-scope` placeholder.
        assert!(
            check_command_in_scope(Lane::Shared, repo, "ls").bound,
            "a bound checkout must report that it is bound"
        );
        std::fs::remove_file(&requests).ok();

        let before = crate::ledger::latest_cursor(repo).expect("cursor");
        let refused = guard_file(repo, "docs/elsewhere.md", "modify");
        assert!(
            refused.is_err(),
            "a denied write must not become permission to act"
        );

        // What went onto the wire.
        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(!sent.is_empty(), "no policy request reached the sidecar");
        let request: serde_json::Value =
            serde_json::from_str(sent.lines().next().unwrap()).expect("the request is JSON");
        let params = &request["params"];
        assert_eq!(
            params["scope"]["task_id"], "TASK-7",
            "the declared scope did not reach the harness: {params}"
        );
        assert_eq!(params["scope"]["planned_files"][0], "src/planned.rs");
        assert_eq!(params["scope"]["worktree"], repo);

        // And what came back was kept rather than dropped.
        let events = crate::ledger::tail(repo, before, 10).expect("tail");
        let row = events.last().expect("a row");
        assert_eq!(
            row.task_id.as_deref(),
            Some("TASK-7"),
            "the ledger row must name the task the gate measured against"
        );
        assert_eq!(row.outcome, "blocked", "a refused write is blocked, not ok");
        let verdict: PolicyVerdict =
            serde_json::from_str(row.verdict_json.as_deref().unwrap()).expect("verdict");
        assert_eq!(
            verdict.task_id, "TASK-7",
            "this end used to drop the task id"
        );
        assert_eq!(verdict.rule, "scope.unplanned");
        assert_eq!(verdict.status, PolicyStatus::Blocked);
    }

    /// A binding whose task store is absent is a broken security precondition,
    /// not the same state as an unbound checkout.
    #[test]
    fn a_binding_without_a_task_scope_is_an_error() {
        let dir = git_repo();
        let repo = dir.path().to_str().expect("utf8");
        crate::ledger::bindings::bind(repo, repo, "TASK-DOES-NOT-EXIST").expect("bind");
        assert!(
            scope_for(repo).is_err(),
            "a durable binding with no enforceable scope must fail closed"
        );
    }

    /// Agent-appended scope reaches the gate, so a write it authorises is
    /// permitted — and the harness reports it as `widened`, not clean.
    #[test]
    fn agent_appended_scope_is_part_of_what_the_gate_measures() {
        let dir = git_repo();
        let repo = dir.path().to_str().expect("utf8");
        let db = crate::tasks::store_path(repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = dc_store::Store::open(&db).unwrap();
        crate::tasks::plant_task::plant(
            &store,
            "TASK-8",
            "in_progress",
            r#"{"planned_files":[{"path":"src/a.rs","allowed_change":"modify"}]}"#,
        );
        store
            .connection()
            .execute(
                "INSERT INTO task_scope_appends (task_id, planned_files_json) VALUES ('TASK-8', ?1)",
                [r#"[{"path":"src/b.rs","allowed_change":"modify"}]"#],
            )
            .unwrap();
        crate::ledger::bindings::bind(repo, repo, "TASK-8").expect("bind");

        let scope = scope_for(repo).expect("binding lookup").expect("scope");
        assert!(
            scope.planned_files.contains(&"src/b.rs".to_string()),
            "appended scope must reach the gate: {:?}",
            scope.planned_files
        );
        assert!(scope.planned_files.contains(&"src/a.rs".to_string()));
    }

    /// A binding made by WorktreesPanel is written while the main checkout is
    /// open. The actual agent mutation is guarded with the linked worktree as
    /// `repo_path`; the latter must still load the former's task scope.
    #[test]
    fn a_main_checkout_binding_is_enforced_from_the_linked_worktree() {
        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");
        let db = crate::tasks::store_path(main_path);
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store directory");
        let store = dc_store::Store::open(&db).expect("task store");
        crate::tasks::plant_task::plant(
            &store,
            "TASK-LINKED",
            "in_progress",
            r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}]}"#,
        );

        crate::ledger::bindings::bind(main_path, &worktree, "TASK-LINKED")
            .expect("bind linked worktree");

        let scope = scope_for(&worktree)
            .expect("binding lookup")
            .expect("linked worktree must inherit its binding");
        assert_eq!(scope.task_id, "TASK-LINKED");
        assert_eq!(scope.planned_files, ["src/planned.rs"]);
        assert_eq!(scope.worktree, worktree);
    }

    /// A linked-worktree gate decision belongs to the repository-wide ledger,
    /// while retaining the exact checkout it judged. Writing it only into the
    /// sibling directory makes Work View in the main checkout miss the action.
    #[cfg(unix)]
    #[test]
    fn a_linked_worktree_gate_row_is_visible_once_from_the_family_ledger() {
        use std::os::unix::fs::PermissionsExt;

        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");
        let script = main.path().join("allowing-manvi");
        std::fs::write(&script, FAKE_ALLOW).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        let before = crate::ledger::latest_cursor(main_path).expect("family cursor");
        guard_file(&worktree, "src/planned.rs", "modify").expect("allowed write");

        let events = crate::ledger::tail(main_path, before, 10).expect("family tail");
        assert_eq!(events.len(), 1, "the gate row must be written exactly once");
        assert_eq!(events[0].repo_path, main_path);
        assert_eq!(events[0].worktree_path.as_deref(), Some(worktree.as_str()));
        assert_eq!(events[0].action, "file.modify");
    }

    /// A binding is durable, while its task database can be deleted or become
    /// unreadable later. That lifecycle failure must block locally before an
    /// otherwise-permissive sidecar can treat the checkout as simply unbound.
    #[cfg(unix)]
    #[test]
    fn a_bound_worktree_fails_closed_when_its_task_scope_disappears() {
        use std::os::unix::fs::PermissionsExt;

        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");
        let db = crate::tasks::store_path(main_path);
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store directory");
        {
            let store = dc_store::Store::open(&db).expect("task store");
            crate::tasks::plant_task::plant(
                &store,
                "TASK-VANISHED",
                "in_progress",
                r#"{"planned_files":[{"path":"src/planned.rs","allowed_change":"modify"}]}"#,
            );
        }
        crate::ledger::bindings::bind(main_path, &worktree, "TASK-VANISHED")
            .expect("bind linked worktree");
        std::fs::remove_file(&db).expect("remove task store");

        let script = main.path().join("allowing-manvi");
        std::fs::write(&script, FAKE_ALLOW).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        let result = guard_file(&worktree, "src/planned.rs", "modify");
        let refusal = result.expect_err(
            "a durable binding whose scope vanished must not degrade to unbound permission",
        );
        // Still refused, and now attributed to the one that refused: the
        // harness was never asked.
        assert!(
            refusal.starts_with("Blocked by GitPulse") && !refusal.contains("by the MANVI harness"),
            "a local refusal must not be blamed on MANVI: {refusal}"
        );
        assert!(refusal.contains(TASK_SCOPE_UNAVAILABLE), "{refusal}");
        let failure = scope_for(&worktree).expect_err("still a scope failure");
        let verdict = failure.verdict("src/planned.rs");
        assert_eq!(verdict.status, PolicyStatus::Blocked);
        assert!(
            !verdict.checked,
            "the harness never ran, so it did not check"
        );
        assert!(!verdict.gate_failed(), "a refusal, not a gate outage");
        // And the agent hook, which judges through the same owner, refuses too.
        let judged = check_command_in_scope(Lane::Shared, &worktree, "echo x > src/planned.rs");
        assert!(judged.verdict.blocks() && judged.bound, "{judged:?}");
    }

    /// The reported incident: a worktree deleted from disk while GitPulse still
    /// had it open. `git switch --guess main` against it came back "Blocked by
    /// the MANVI harness [task.scope_unavailable/hard]", and the gate's ledger
    /// row then failed to create `.devcouncil` under the missing path.
    #[cfg(unix)]
    #[test]
    fn a_deleted_worktree_is_named_as_missing_not_blamed_on_the_harness() {
        use std::os::unix::fs::PermissionsExt;

        let (main, _parent, worktree) = linked_repo();
        let main_path_buf = main.path().canonicalize().expect("canonical repository");
        let main_path = main_path_buf.to_str().expect("utf8 repository");
        std::fs::remove_dir_all(&worktree).expect("delete the worktree from disk");

        let requests = main.path().join("requests.ndjson");
        let script = main.path().join("recording-manvi");
        let body = FAKE_RECORDER.replace("@REQUESTS@", &requests.display().to_string());
        std::fs::write(&script, body).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        let before = crate::ledger::latest_cursor(main_path).expect("family cursor");
        for refusal in [
            guard_command(&worktree, &["git", "switch", "--guess", "main"])
                .expect_err("nothing can run in a directory that is gone"),
            guard_file(&worktree, "src/lib.rs", "modify").expect_err("nor be written there"),
        ] {
            assert!(
                refusal.contains("no longer exists"),
                "the cause must be named: {refusal}"
            );
            assert!(
                !refusal.contains("MANVI") && !refusal.contains(TASK_SCOPE_UNAVAILABLE),
                "a deleted checkout is not a policy decision: {refusal}"
            );
        }
        assert!(
            !std::path::Path::new(&worktree).exists(),
            "refusing must not recreate the deleted worktree (or its .devcouncil)"
        );
        assert!(
            std::fs::read_to_string(&requests)
                .unwrap_or_default()
                .is_empty(),
            "nothing was judged, so nothing may be sent to the harness"
        );
        assert_eq!(
            crate::ledger::latest_cursor(main_path).expect("family cursor"),
            before,
            "no gate row for an action that was never judged"
        );

        // The agent hook path: no binding to read, so judged unscoped rather
        // than refused for want of one.
        let ScopedVerdict { verdict, bound, .. } =
            check_command_in_scope(Lane::Shared, &worktree, "ls");
        assert!(!bound, "a missing checkout declares no scope");
        assert_ne!(verdict.rule, TASK_SCOPE_UNAVAILABLE, "{verdict:?}");
    }

    /// A bound checkout `main`, a bound sibling worktree `wt_b` beside it, and a
    /// fake sidecar script that denies `unplanned.txt` under TASK-WTB. The
    /// sidecar logs every request it judged to `requests`. The caller binds the
    /// script with `sidecar::bind_test_binary`.
    #[cfg(unix)]
    struct SiblingFixture {
        main: tempfile::TempDir,
        _wt_b: tempfile::TempDir,
        main_path: String,
        rel_wt_b: String,
        requests: PathBuf,
        script: PathBuf,
    }

    #[cfg(unix)]
    fn sibling_fixture() -> SiblingFixture {
        use std::os::unix::fs::PermissionsExt;

        let main = git_repo();
        let sibling_parent = main.path().parent().unwrap();
        let wt_b = tempfile::Builder::new()
            .prefix("wt-b-")
            .tempdir_in(sibling_parent)
            .expect("sibling dir");
        std::fs::remove_dir(wt_b.path()).expect("vacate target for git worktree add");
        git_in(
            main.path(),
            &[
                "worktree",
                "add",
                "-b",
                "feat-b",
                wt_b.path().to_str().expect("utf8 wt_b"),
            ],
        );
        crate::test_support::trust_repo(main.path());
        crate::test_support::trust_repo(wt_b.path());
        let main_path = main
            .path()
            .canonicalize()
            .expect("canonical main")
            .to_string_lossy()
            .into_owned();
        let wt_b_path = wt_b
            .path()
            .canonicalize()
            .expect("canonical wt_b")
            .to_string_lossy()
            .into_owned();
        let rel_wt_b = wt_b
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let db = crate::tasks::store_path(&main_path);
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store dir");
        {
            let store = dc_store::Store::open(&db).expect("task store");
            crate::tasks::plant_task::plant(
                &store,
                "TASK-MAIN",
                "in_progress",
                r#"{"planned_files":[{"path":"src/main.rs","allowed_change":"modify"}]}"#,
            );
            crate::tasks::plant_task::plant(
                &store,
                "TASK-WTB",
                "in_progress",
                r#"{"planned_files":[{"path":"src/sibling.rs","allowed_change":"modify"}]}"#,
            );
        }
        crate::ledger::bindings::bind(&main_path, &main_path, "TASK-MAIN").expect("bind main");
        crate::ledger::bindings::bind(&main_path, &wt_b_path, "TASK-WTB").expect("bind wt_b");

        let requests = main.path().join("requests.ndjson");
        let script = main.path().join("scope-aware-manvi");
        let body = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  case "$line" in
    *unplanned.txt*'"TASK-WTB"'*) reply "$line" '{"action":"deny","rule":"scope.unplanned","severity":"soft","reason":"Task TASK-WTB does not authorize changes to unplanned.txt","target":"unplanned.txt","task_id":"TASK-WTB","demoted":"","degraded":[]}' ;;
    *) reply "$line" '{"action":"allow","rule":"","severity":"","reason":"allowed","target":"","task_id":"","demoted":"","degraded":[]}' ;;
  esac
done
"#
        .replace("@REQUESTS@", &requests.display().to_string());
        std::fs::write(&script, body).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        SiblingFixture {
            main,
            _wt_b: wt_b,
            main_path,
            rel_wt_b,
            requests,
            script,
        }
    }

    #[cfg(unix)]
    #[test]
    fn sibling_git_c_command_checks_sibling_worktree_scope_and_forwards_rewritten_command() {
        let fixture = sibling_fixture();
        let (main_path, rel_wt_b, requests) = (
            fixture.main_path.clone(),
            fixture.rel_wt_b.clone(),
            fixture.requests.clone(),
        );
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, fixture.script.to_string_lossy());

        let cmd = format!("git -C ../{rel_wt_b} checkout -- unplanned.txt");
        let judged = check_command_in_scope(Lane::Shared, &main_path, &cmd);
        assert!(
            judged.verdict.blocks() && judged.verdict.rule == "scope.unplanned",
            "expected sibling scope denial for {cmd:?}, got {judged:?}"
        );
        let logged = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(
            logged.contains("TASK-WTB") && logged.contains("git checkout -- unplanned.txt"),
            "expected rewritten sibling command evaluated against TASK-WTB, got: {logged}"
        );

        let allowed_cmd = format!("git -C ../{rel_wt_b} checkout -- src/sibling.rs > out.txt");
        let allowed_judged = check_command_in_scope(Lane::Shared, &main_path, &allowed_cmd);
        assert!(
            !allowed_judged.verdict.blocks(),
            "expected sibling planned file + caller redirect stripped for sibling check, got {allowed_judged:?}"
        );
        let logged_after = std::fs::read_to_string(&requests).unwrap_or_default();
        let sibling_lines: Vec<&str> = logged_after
            .lines()
            .filter(|l| l.contains("TASK-WTB") && l.contains("src/sibling.rs"))
            .collect();
        assert!(
            sibling_lines
                .iter()
                .any(|l| l.contains("git checkout -- src/sibling.rs") && !l.contains("out.txt")),
            "expected rewritten sibling command without -C or caller redirect, got: {sibling_lines:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_behind_a_wrapper_is_judged_against_the_sibling_it_names() {
        let fixture = sibling_fixture();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, fixture.script.to_string_lossy());
        let rel = &fixture.rel_wt_b;
        let wrappers = [
            "env",
            "env -i",
            "env FOO=1",
            "env -u HOME",
            "FOO=1 env",
            "/usr/bin/env",
            "nice -n 5",
            "nohup",
            "sudo",
            "sudo -u root",
            "exec",
            "xargs",
            "timeout 5",
            "timeout -s KILL 5",
            "command",
            "time",
            "env nice -n 5",
            "setsid",
        ];
        for wrapper in wrappers {
            for target in [
                format!("../{rel}"),
                format!("'../{rel}'"),
                format!("\"../{rel}\""),
            ] {
                let cmd = format!("{wrapper} git -C {target} checkout -- unplanned.txt");
                let judged = check_command_in_scope(Lane::Shared, &fixture.main_path, &cmd);
                assert!(
                    judged.verdict.blocks() && judged.verdict.rule == "scope.unplanned",
                    "expected the sibling's scope to judge {cmd:?}, got {judged:?}"
                );
                assert_eq!(judged.verdict.task_id, "TASK-WTB", "{cmd:?}");
            }
        }
        let logged = std::fs::read_to_string(&fixture.requests).unwrap_or_default();
        assert!(
            logged.contains("env git checkout -- unplanned.txt"),
            "the wrapper stays in the command the sibling is asked about, got: {logged}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dash_c_that_names_no_git_invocation_is_refused_for_a_bound_caller() {
        let fixture = sibling_fixture();
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, fixture.script.to_string_lossy());
        let rel = &fixture.rel_wt_b;
        let unreadable = [
            format!("bash -c 'git -C ../{rel} checkout -- unplanned.txt'"),
            format!("echo git -C ../{rel} checkout -- unplanned.txt"),
            format!("somewrapper git -C ../{rel} checkout -- unplanned.txt"),
            format!("true && eval \"git -C ../{rel} checkout -- unplanned.txt\""),
            format!("sh -lc \"git -C ../{rel} checkout -- unplanned.txt\""),
            format!("env bash -c 'git -C ../{rel} checkout -- unplanned.txt'"),
            "xargs sh -c 'git -C \"$0\" status'".to_string(),
            "find . -exec sh -c 'git -C \"$1\" status' _ {} \\;".to_string(),
            "bash -c \"bash -c 'git -C ../x status'\"".to_string(),
        ];
        for cmd in &unreadable {
            let judged = check_command_in_scope(Lane::Shared, &fixture.main_path, cmd);
            assert!(judged.bound, "{cmd:?}");
            assert!(
                judged.verdict.blocks() && judged.verdict.rule == GIT_C_UNRESOLVED,
                "expected {cmd:?} refused as an unresolved -C, got {judged:?}"
            );
            assert!(!judged.verdict.checked, "GitPulse refused, not the harness");
            assert!(judged.verdict.reason.contains("-C"), "{judged:?}");
        }

        // Neighbours that carry a -C but are not git's, or are readable git,
        // are not caught by the new refusal.
        for cmd in [
            "ls -C".to_string(),
            "grep -C 3 git seed.txt".to_string(),
            "rg -C2 git seed.txt".to_string(),
            "git log -C --stat".to_string(),
            "git status".to_string(),
            format!("env git -C ../{rel} status"),
            // Searches for the text "git -C" are routine and must not be refused.
            "rg -n \"git -C\" src".to_string(),
            "grep -rn 'git -C' .".to_string(),
            "rg \"git\" -C 3 src".to_string(),
            "echo \"use git -C <dir>\"".to_string(),
            "bash -c 'rg \"git -C\" src'".to_string(),
            "bash -c 'git status'".to_string(),
            "bash -c 'git log -C --stat'".to_string(),
            "bash script.sh".to_string(),
        ] {
            let judged = check_command_in_scope(Lane::Shared, &fixture.main_path, &cmd);
            assert_ne!(judged.verdict.rule, GIT_C_UNRESOLVED, "{cmd:?}: {judged:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unbound_caller_is_not_refused_for_an_unresolved_dash_c() {
        let main = git_repo();
        crate::test_support::trust_repo(main.path());
        let main_path = main
            .path()
            .canonicalize()
            .expect("canonical main")
            .to_string_lossy()
            .into_owned();
        let script = main.path().join("allow-manvi");
        std::fs::write(
            &script,
            r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  reply "$line" '{"action":"allow","rule":"","severity":"","reason":"allowed","target":"","task_id":"","demoted":"","degraded":[]}'
done
"#,
        )
        .expect("write fake sidecar");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let serial = sidecar::test_serial();
        let _binary = sidecar::bind_test_binary(&serial, script.to_string_lossy());
        let judged = check_command_in_scope(
            Lane::Shared,
            &main_path,
            "bash -c 'git -C ../elsewhere status'",
        );
        assert!(!judged.bound);
        assert_ne!(judged.verdict.rule, GIT_C_UNRESOLVED, "{judged:?}");
        assert!(!judged.verdict.blocks(), "{judged:?}");
    }
}

#[cfg(test)]
mod git_invocation_tests {
    use super::*;

    fn c_values(clause: &str) -> Option<Vec<String>> {
        parse_git_invocation(clause)
            .map(|inv| inv.c_args.into_iter().map(|(_, _, val, _)| val).collect())
    }

    fn some(values: &[&str]) -> Option<Vec<String>> {
        Some(values.iter().map(|v| v.to_string()).collect())
    }

    #[test]
    fn git_is_found_behind_each_wrapper_and_its_options() {
        for prefix in [
            "",
            "env",
            "env -i",
            "env -u HOME",
            "env FOO=1 BAR=2",
            "/usr/bin/env",
            "nice -n 5",
            "nice -n5",
            "ionice -c 3",
            "nohup",
            "setsid",
            "sudo",
            "sudo -u root",
            "doas",
            "exec",
            "xargs",
            "xargs -n 1",
            "timeout 5",
            "timeout -s KILL 5",
            "stdbuf -oL",
            "command",
            "builtin",
            "time",
            "FOO=1 env",
            "env nice -n 5 timeout 9",
            "if",
            "! env",
        ] {
            let clause = format!("{prefix} git -C ../wt-b checkout -- x");
            assert_eq!(c_values(&clause), some(&["../wt-b"]), "{clause:?}");
        }
    }

    #[test]
    fn quoted_and_unicode_paths_survive_behind_a_wrapper() {
        assert_eq!(
            c_values("env git -C 'wt é/日本' checkout"),
            some(&["wt é/日本"])
        );
        assert_eq!(c_values("env git -C \"wt-ü\" status"), some(&["wt-ü"]));
        assert_eq!(c_values("sudo git -C../wt-b status"), some(&["../wt-b"]));
        assert_eq!(
            c_values("env git -C a -C b status"),
            some(&["a", "b"]),
            "chained -C stays in order"
        );
    }

    #[test]
    fn a_word_git_that_is_not_in_command_position_is_not_an_invocation() {
        for clause in [
            "echo git -C ../wt-b",
            "grep git -C 3 file",
            "somewrapper git -C ../wt-b status",
            "bash -c 'git -C ../wt-b status'",
            "ls git",
            "env",
            "env -i",
            "env git",
            "git status",
            "git log -C --stat",
            "env $VAR -C x",
        ] {
            assert_eq!(c_values(clause), None, "{clause:?}");
        }
        // An expanding git word is never trusted to be git.
        assert_eq!(c_values("env $G -C ../wt-b status"), None);
    }

    #[test]
    fn the_unresolved_dash_c_reader_names_only_clauses_git_cannot_be_found_in() {
        for (command, refused) in [
            ("echo git -C ../x", true),
            ("bash -c 'git -C ../x status'", true),
            ("ls && somewrapper git -C ../x", true),
            ("sh -lc \"git -C ../x y\"", true),
            ("sh -ec 'git -C ../x y'", true),
            ("/bin/bash.exe -c 'git -C ../x y'", true),
            ("env bash -c 'git -C ../x y'", true),
            ("xargs sh -c 'git -C \"$0\" status'", true),
            ("eval \"git -C ../x y\"", true),
            ("su root -c 'git -C ../x y'", true),
            ("script -q f -c 'git -C ../x y'", true),
            ("bash -c 'x; git -C ../x y'", true),
            ("bash -c \"sh -c 'git -C ../x y'\"", true),
            ("rg -n \"git -C\" src", false),
            ("grep -rn 'git -C' .", false),
            ("rg \"git\" -C 3 src", false),
            ("echo \"use git -C <dir>\"", false),
            ("echo sh -c 'git -C x'", false),
            ("bash -c 'git status'", false),
            ("bash -c 'rg \"git -C\" src'", false),
            ("bash -c \"$CMD\"", false),
            ("bash script.sh", false),
            ("env git -C ../x status", false),
            ("git -C ../x status", false),
            ("git log -C", false),
            ("grep -C 3 git f", false),
            ("ls -C", false),
            ("echo -C git", false),
            ("", false),
        ] {
            assert_eq!(unresolved_git_c(command).is_some(), refused, "{command:?}");
        }
    }
}

#[cfg(test)]
mod judge_in_order_tests {
    use super::judge_in_order;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("f{i}.md")).collect()
    }

    #[test]
    fn every_file_is_checked_once_with_a_bounded_number_in_flight() {
        let names = names(64);
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let (in_flight, peak, checks) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        let judged = judge_in_order(
            &files,
            8,
            |file| {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                checks.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                file.to_string()
            },
            |_| false,
        );
        assert_eq!(checks.load(Ordering::SeqCst), 64);
        assert!(peak.load(Ordering::SeqCst) <= 8, "{peak:?}");
        assert!(peak.load(Ordering::SeqCst) >= 2, "no overlap: {peak:?}");
        let answered: Vec<_> = judged.into_iter().collect::<Option<Vec<_>>>().unwrap();
        assert_eq!(answered, names, "results stay in input order");
    }

    #[test]
    fn a_refusal_stops_new_checks_and_leaves_every_earlier_file_judged() {
        let names = names(2_000);
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let checks = AtomicUsize::new(0);
        for workers in [1, 8] {
            checks.store(0, Ordering::SeqCst);
            let judged = judge_in_order(
                &files,
                workers,
                |file| {
                    checks.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_micros(200));
                    file == "f40.md"
                },
                |refused| *refused,
            );
            let first = judged
                .iter()
                .position(|r| *r == Some(true))
                .expect("the refusal is recorded");
            assert_eq!(first, 40);
            assert!(judged[..first].iter().all(Option::is_some), "{workers}");
            assert!(
                checks.load(Ordering::SeqCst) < 40 + 1 + 2 * workers + 8,
                "{workers} workers checked {} of 2000",
                checks.load(Ordering::SeqCst)
            );
        }
    }

    #[test]
    fn no_files_and_one_file() {
        assert!(judge_in_order(&[], 8, |_| 0u8, |_| false).is_empty());
        let one = judge_in_order(&["a.md"], 8, |_| 7u8, |_| false);
        assert_eq!(one, vec![Some(7)]);
    }
}
