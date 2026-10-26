//! The signal blast-radius guard: `command-gate`'s check on Bash commands that
//! send signals.
//!
//! # Why
//!
//! `pkill -f "sleep 60" -u bharath` was run by an agent on 2026-10-12. macOS
//! `pkill` parses options with BSD `getopt`, which stops at the first operand,
//! so `-u` and `bharath` were not a user filter: they were two more patterns,
//! OR'd with `sleep 60`, and with `-f` each is matched against the *full
//! argument list* of every process. Every process with the home directory in
//! its argv — twenty hosted agent sessions among them — took a SIGTERM. `pkill`
//! never signals its own ancestors, which is the only reason the sender's own
//! session survived.
//!
//! # What this does
//!
//! It reads a command line the way the shell would, finds every invocation that
//! sends a signal, works out *which processes the tool would pick* — option
//! parsing, ancestors and all, as `man pkill` and `man killall` describe and as
//! the real `pgrep` was run to confirm — and refuses when that set includes an
//! agent session, a GitPulse process, or more processes than any deliberate
//! pattern kill reaches. The process table is read, never written: nothing here
//! signals anything.
//!
//! The shape is the module's: [`plan`] and [`decide`] are pure functions over a
//! command line and over a [`Facts`] someone already gathered, so every row of
//! the decision table is a unit test with no process spawned. [`gather`] is the
//! only impure step, and [`run`] puts one deadline over it.
//!
//! # What it answers
//!
//! * **Deny**, with a reason written for the model: the parsed patterns, how
//!   many agent sessions, GitPulse processes and processes in all, and the
//!   corrected command when the cause is option order.
//! * **Ask**, with a `systemMessage`, for anything it could not evaluate: a
//!   command it could not parse, a process table it could not read, a pattern
//!   built from a variable, a deadline. Never an `allow`, and never silence.
//! * **No decision** — empty stdout — for everything else, so the user's own
//!   permission rules run. `kill 1234` is not this guard's business.
//!
//! # The bound
//!
//! A command that signals more than [`MAX_MATCHES`] processes is refused even
//! when none is an agent. The intended `pkill -f "sleep 60"` of the incident
//! reached eight processes; a deliberate pattern kill of a dev server and its
//! workers is a handful. Twenty-five is three times the one and well over the
//! other, and the incident's accident was two orders of magnitude past it. A
//! person who really means more than that can run it themselves.
//!
//! # Limits, said plainly
//!
//! * `-U` (real uid) is compared with the effective uid the table carries, which
//!   differ only for set-uid processes.
//! * `-G` (group) and `-t` (terminal) are not in the table: those commands are
//!   asked about, not guessed at.
//! * POSIX extended regular expressions are evaluated with the `regex` crate. A
//!   pattern it cannot compile is asked about; a few constructs read differently
//!   (`\d` is a digit here and a literal `d` to `regcomp`).
//! * A variable that is not `$HOME`, `$USER` or `$LOGNAME` is not expanded, so a
//!   pattern or user built from one is asked about. A pid list from `$PID` is
//!   one pid and is left to the user's rules.
//! * Only this user's processes can be signalled, so only they are counted.
//! * `ssh host pkill …`, `find -exec kill`, `parallel` and the like are not read.

use std::collections::HashMap;
use std::time::Duration;

use regex::{Regex, RegexBuilder};

use super::{decision, HookInput, HookOutput, PermissionDecision, PRE_TOOL_USE};
use crate::sysload::procscan::{signal_table, TableRow};

/// The most processes one signal command may reach before it is refused.
pub const MAX_MATCHES: usize = 25;

/// The deadline over reading the process table and deciding.
const GUARD_BUDGET: Duration = Duration::from_secs(2);
/// Longest command line read; past it the line is asked about if it mentions
/// `kill` and left alone if it does not.
const MAX_COMMAND_BYTES: usize = 256 * 1024;
/// Commands one line may contain, nested ones included.
const MAX_COMMANDS: usize = 16_384;
/// Nesting of `$(…)`, `bash -c` and `eval` read before giving up.
const MAX_DEPTH: usize = 12;
/// Findings kept from one line.
const MAX_FINDINGS: usize = 64;
/// Patterns one invocation may carry, and the length of each.
const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_CHARS: usize = 4096;
/// Compiled size allowed to one pattern.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Processes named in a reason.
const NAMED_HITS: usize = 5;
/// Denials spelled out in one answer.
const MAX_DENIALS_SHOWN: usize = 3;
/// Longest invocation echoed back.
const ECHO_CHARS: usize = 200;
/// Ancestor hops followed from this process.
const MAX_ANCESTORS: usize = 128;
/// Environment variables a word may expand: the ones whose value is the same
/// in the hook as in the shell it is judging.
const EXPANDABLE: [&str; 3] = ["HOME", "USER", "LOGNAME"];

/* ── 1. Reading the command line ──────────────────────────────────────────── */

/// One shell word, after quoting.
#[derive(Debug, Clone, Default)]
pub(crate) struct Word {
    /// The word as written, for echoing a corrected command.
    src: String,
    /// The word after quoting and the expansions that can be evaluated.
    text: String,
    /// Part of it is only known when the shell runs.
    dynamic: bool,
    /// `$(…)`, backtick and `<(…)` bodies inside it.
    subst: Vec<Vec<Pipeline>>,
}

type Pipeline = Vec<Cmd>;

#[derive(Debug, Clone, Default)]
struct Cmd {
    words: Vec<Word>,
    /// Redirection targets, kept for the substitutions in them.
    redirects: Vec<Word>,
    heredocs: Vec<Heredoc>,
}

impl Cmd {
    fn is_empty(&self) -> bool {
        self.words.is_empty() && self.redirects.is_empty() && self.heredocs.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
struct Heredoc {
    body: String,
    /// Substitutions an unquoted body runs.
    nested: Vec<Vec<Pipeline>>,
}

struct Budget {
    commands: usize,
}

type EnvFn = fn(&str) -> Option<String>;

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

struct Pending {
    pipeline: usize,
    command: usize,
    heredoc: usize,
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Lexer<'a> {
    chars: &'a [char],
    i: usize,
    depth: usize,
    budget: &'a mut Budget,
    env: EnvFn,
    pending: Vec<Pending>,
}

fn parse_script(
    src: &str,
    depth: usize,
    budget: &mut Budget,
    env: EnvFn,
) -> Result<Vec<Pipeline>, String> {
    if depth > MAX_DEPTH {
        return Err(format!("commands nested more than {MAX_DEPTH} deep"));
    }
    let chars: Vec<char> = src.chars().collect();
    Lexer {
        chars: &chars,
        i: 0,
        depth,
        budget,
        env,
        pending: Vec::new(),
    }
    .script()
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    fn slice(&self, from: usize, to: usize) -> String {
        self.chars[from.min(self.chars.len())..to.min(self.chars.len())]
            .iter()
            .collect()
    }

    fn skip_blanks(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.i += 1;
            } else if c == '\\' && self.at(1) == Some('\n') {
                self.i += 2;
            } else {
                break;
            }
        }
    }

    fn count_command(&mut self) -> Result<(), String> {
        self.budget.commands += 1;
        if self.budget.commands > MAX_COMMANDS {
            return Err(format!("more than {MAX_COMMANDS} commands"));
        }
        Ok(())
    }

    fn flush_command(&mut self, cmd: &mut Cmd, pipe: &mut Pipeline) -> Result<(), String> {
        if !cmd.is_empty() {
            self.count_command()?;
            pipe.push(std::mem::take(cmd));
        }
        Ok(())
    }

    fn flush_pipeline(
        &mut self,
        cmd: &mut Cmd,
        pipe: &mut Pipeline,
        done: &mut Vec<Pipeline>,
    ) -> Result<(), String> {
        self.flush_command(cmd, pipe)?;
        if !pipe.is_empty() {
            done.push(std::mem::take(pipe));
        }
        Ok(())
    }

    fn script(&mut self) -> Result<Vec<Pipeline>, String> {
        let mut done: Vec<Pipeline> = Vec::new();
        let mut pipe: Pipeline = Vec::new();
        let mut cmd = Cmd::default();
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { break };
            match c {
                '\n' => {
                    self.i += 1;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                    self.take_bodies(&mut done)?;
                }
                ';' => {
                    self.i += 1;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                '&' if self.at(1) == Some('>') => {
                    self.redirect(&mut cmd, done.len(), pipe.len())?;
                }
                '&' => {
                    self.i += if self.at(1) == Some('&') { 2 } else { 1 };
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                '|' if self.at(1) == Some('|') => {
                    self.i += 2;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                '|' => {
                    self.i += if self.at(1) == Some('&') { 2 } else { 1 };
                    self.flush_command(&mut cmd, &mut pipe)?;
                }
                '(' | ')' => {
                    self.i += 1;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '<' | '>' if self.at(1) != Some('(') => {
                    self.redirect(&mut cmd, done.len(), pipe.len())?;
                }
                c if c.is_ascii_digit() && self.digits_then_redirect() => {
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.i += 1;
                    }
                    self.redirect(&mut cmd, done.len(), pipe.len())?;
                }
                _ => {
                    let word = self.word()?;
                    cmd.words.push(word);
                }
            }
        }
        self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
        Ok(done)
    }

    fn digits_then_redirect(&self) -> bool {
        let mut k = 0;
        while self.at(k).is_some_and(|c| c.is_ascii_digit()) {
            k += 1;
        }
        matches!(self.at(k), Some('<' | '>'))
    }

    /// A redirection: the operator, then its target word, which is data and
    /// not an argument — except `<<` and `<<<`, which carry a script a shell
    /// can be handed.
    fn redirect(&mut self, cmd: &mut Cmd, pipeline: usize, command: usize) -> Result<(), String> {
        if self.peek() == Some('<') && self.at(1) == Some('<') && self.at(2) == Some('<') {
            self.i += 3;
            self.skip_blanks();
            if self.starts_word() {
                let word = self.word()?;
                cmd.heredocs.push(Heredoc {
                    body: word.text.clone(),
                    nested: Vec::new(),
                });
                cmd.redirects.push(word);
            }
            return Ok(());
        }
        if self.peek() == Some('<') && self.at(1) == Some('<') {
            self.i += 2;
            let strip_tabs = self.peek() == Some('-');
            if strip_tabs {
                self.i += 1;
            }
            self.skip_blanks();
            if !self.starts_word() {
                return Ok(());
            }
            let word = self.word()?;
            let quoted = word.src.contains(['\'', '"', '\\']);
            self.pending.push(Pending {
                pipeline,
                command,
                heredoc: cmd.heredocs.len(),
                delimiter: word.text.clone(),
                strip_tabs,
                quoted,
            });
            cmd.heredocs.push(Heredoc::default());
            return Ok(());
        }
        // `>`, `>>`, `>|`, `<`, `<&`, `>&`, `&>`, `&>>`, `<&-`.
        let mut taken = 0;
        while taken < 3 && matches!(self.peek(), Some('<' | '>' | '&' | '|')) {
            self.i += 1;
            taken += 1;
        }
        if self.peek() == Some('-') && matches!(self.chars.get(self.i.wrapping_sub(1)), Some('&')) {
            self.i += 1;
        }
        self.skip_blanks();
        if self.starts_word() {
            let word = self.word()?;
            cmd.redirects.push(word);
        }
        Ok(())
    }

    fn starts_word(&self) -> bool {
        match self.peek() {
            None => false,
            Some(c) => !matches!(c, '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'),
        }
    }

    /// Reads the bodies of the heredocs the line just ended asked for.
    fn take_bodies(&mut self, done: &mut [Pipeline]) -> Result<(), String> {
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let mut body = String::new();
            while self.i < self.chars.len() {
                let start = self.i;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line = self.slice(start, self.i);
                if self.peek() == Some('\n') {
                    self.i += 1;
                }
                let compared = if p.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if compared == p.delimiter {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
                if body.len() > MAX_COMMAND_BYTES {
                    return Err("a here-document longer than the line limit".into());
                }
            }
            let nested = if p.quoted {
                Vec::new()
            } else {
                substitutions(&body, self.depth + 1, self.budget, self.env)?
            };
            if let Some(h) = done
                .get_mut(p.pipeline)
                .and_then(|pl| pl.get_mut(p.command))
                .and_then(|c| c.heredocs.get_mut(p.heredoc))
            {
                h.body = body;
                h.nested = nested;
            }
        }
        Ok(())
    }

    /// The index of the `)` closing a `(` whose body starts at `from`.
    fn matching_paren(&self, from: usize) -> Result<usize, String> {
        let mut depth = 1usize;
        let mut k = from;
        while k < self.chars.len() {
            match self.chars[k] {
                '\\' => k += 1,
                '\'' => {
                    k += 1;
                    while k < self.chars.len() && self.chars[k] != '\'' {
                        k += 1;
                    }
                }
                '"' => {
                    k += 1;
                    while k < self.chars.len() && self.chars[k] != '"' {
                        if self.chars[k] == '\\' {
                            k += 1;
                        }
                        k += 1;
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(k);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        Err("an unterminated `$(`".into())
    }

    fn nested(&mut self, from: usize, to: usize) -> Result<Vec<Pipeline>, String> {
        let inner = self.slice(from, to);
        parse_script(&inner, self.depth + 1, self.budget, self.env)
    }

    fn expand(&self, name: &str) -> Option<String> {
        EXPANDABLE
            .contains(&name)
            .then(|| (self.env)(name))
            .flatten()
    }

    /// One word: quoting, escapes and the expansions that can be read.
    fn word(&mut self) -> Result<Word, String> {
        let start = self.i;
        let mut w = Word::default();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' => break,
                '<' | '>' => {
                    if self.i == start && self.at(1) == Some('(') {
                        let end = self.matching_paren(self.i + 2)?;
                        let body = self.nested(self.i + 2, end)?;
                        w.subst.push(body);
                        w.dynamic = true;
                        w.text.push_str(&self.slice(self.i, end + 1));
                        self.i = end + 1;
                    } else {
                        break;
                    }
                }
                '\\' => {
                    match self.at(1) {
                        Some('\n') => {}
                        Some(next) => w.text.push(next),
                        None => w.text.push('\\'),
                    }
                    self.i += 2;
                }
                '\'' => {
                    self.i += 1;
                    let from = self.i;
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.i += 1;
                    }
                    if self.peek().is_none() {
                        return Err("an unterminated single quote".into());
                    }
                    w.text.push_str(&self.slice(from, self.i));
                    self.i += 1;
                }
                '"' => {
                    self.i += 1;
                    self.double_quoted(&mut w)?;
                }
                '`' => self.backtick(&mut w)?,
                '$' => self.dollar(&mut w, false)?,
                '~' if self.i == start && matches!(self.at(1), None | Some('/' | ' ' | '\t')) => {
                    match (self.env)("HOME") {
                        Some(home) => w.text.push_str(&home),
                        None => {
                            w.dynamic = true;
                            w.text.push('~');
                        }
                    }
                    self.i += 1;
                }
                other => {
                    w.text.push(other);
                    self.i += 1;
                }
            }
        }
        w.src = self.slice(start, self.i);
        Ok(w)
    }

    fn double_quoted(&mut self, w: &mut Word) -> Result<(), String> {
        loop {
            let Some(c) = self.peek() else {
                return Err("an unterminated double quote".into());
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(());
                }
                '\\' => {
                    match self.at(1) {
                        Some(n @ ('"' | '\\' | '$' | '`')) => w.text.push(n),
                        Some('\n') => {}
                        Some(n) => {
                            w.text.push('\\');
                            w.text.push(n);
                        }
                        None => return Err("an unterminated double quote".into()),
                    }
                    self.i += 2;
                }
                '$' => self.dollar(w, true)?,
                '`' => self.backtick(w)?,
                other => {
                    w.text.push(other);
                    self.i += 1;
                }
            }
        }
    }

    fn backtick(&mut self, w: &mut Word) -> Result<(), String> {
        let from = self.i + 1;
        let mut k = from;
        while k < self.chars.len() && self.chars[k] != '`' {
            if self.chars[k] == '\\' {
                k += 1;
            }
            k += 1;
        }
        if k >= self.chars.len() {
            return Err("an unterminated backtick".into());
        }
        let body = self.nested(from, k)?;
        w.subst.push(body);
        w.dynamic = true;
        w.text.push_str(&self.slice(self.i, k + 1));
        self.i = k + 1;
        Ok(())
    }

    fn dollar(&mut self, w: &mut Word, in_quotes: bool) -> Result<(), String> {
        match self.at(1) {
            Some('(') if self.at(2) == Some('(') => {
                let end = self.matching_paren(self.i + 2)?;
                w.dynamic = true;
                w.text.push_str(&self.slice(self.i, end + 1));
                self.i = end + 1;
                if self.peek() == Some(')') {
                    w.text.push(')');
                    self.i += 1;
                }
            }
            Some('(') => {
                let end = self.matching_paren(self.i + 2)?;
                let body = self.nested(self.i + 2, end)?;
                w.subst.push(body);
                w.dynamic = true;
                w.text.push_str(&self.slice(self.i, end + 1));
                self.i = end + 1;
            }
            Some('{') => {
                let mut depth = 1usize;
                let mut k = self.i + 2;
                while k < self.chars.len() && depth > 0 {
                    match self.chars[k] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    k += 1;
                }
                if depth > 0 {
                    return Err("an unterminated `${`".into());
                }
                let name = self.slice(self.i + 2, k - 1);
                match self.expand(&name) {
                    Some(value) => w.text.push_str(&value),
                    None => {
                        w.dynamic = true;
                        w.text.push_str(&self.slice(self.i, k));
                    }
                }
                self.i = k;
            }
            Some('\'') if !in_quotes => {
                // `$'…'`: the text between the quotes, escapes left as written.
                let from = self.i + 2;
                let mut k = from;
                while k < self.chars.len() && self.chars[k] != '\'' {
                    if self.chars[k] == '\\' {
                        k += 1;
                    }
                    k += 1;
                }
                if k >= self.chars.len() {
                    return Err("an unterminated `$'`".into());
                }
                w.text.push_str(&self.slice(from, k));
                self.i = k + 1;
            }
            Some('"') if !in_quotes => {
                self.i += 1;
            }
            Some(n) if n.is_ascii_alphabetic() || n == '_' => {
                let mut k = self.i + 1;
                while self
                    .chars
                    .get(k)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    k += 1;
                }
                let name = self.slice(self.i + 1, k);
                match self.expand(&name) {
                    Some(value) => w.text.push_str(&value),
                    None => {
                        w.dynamic = true;
                        w.text.push_str(&self.slice(self.i, k));
                    }
                }
                self.i = k;
            }
            Some(n)
                if n.is_ascii_digit() || matches!(n, '@' | '*' | '#' | '?' | '$' | '!' | '-') =>
            {
                w.dynamic = true;
                w.text.push('$');
                w.text.push(n);
                self.i += 2;
            }
            _ => {
                w.text.push('$');
                self.i += 1;
            }
        }
        Ok(())
    }
}

/// The `$(…)` and backtick bodies in free text, which an unquoted heredoc runs.
fn substitutions(
    text: &str,
    depth: usize,
    budget: &mut Budget,
    env: EnvFn,
) -> Result<Vec<Vec<Pipeline>>, String> {
    if !text.contains("$(") && !text.contains('`') {
        return Ok(Vec::new());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut lexer = Lexer {
        chars: &chars,
        i: 0,
        depth,
        budget,
        env,
        pending: Vec::new(),
    };
    while let Some(c) = lexer.peek() {
        match c {
            '\\' => lexer.i += 2,
            '$' if lexer.at(1) == Some('(') && lexer.at(2) != Some('(') => {
                let end = lexer.matching_paren(lexer.i + 2)?;
                found.push(lexer.nested(lexer.i + 2, end)?);
                lexer.i = end + 1;
            }
            '`' => {
                let mut w = Word::default();
                lexer.backtick(&mut w)?;
                found.extend(w.subst);
            }
            _ => lexer.i += 1,
        }
    }
    Ok(found)
}

/* ── 2. Finding the invocations that send signals ─────────────────────────── */

/// What a command line was found to do to other processes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Plan {
    findings: Vec<Finding>,
    /// Set when the line could not be read and mentions `kill`.
    unparsed: Option<String>,
}

impl Plan {
    /// Nothing here sends a signal, as far as the line could be read.
    pub(crate) fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.unparsed.is_none()
    }

    fn needs_table(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.sel, Sel::Pgrep(_) | Sel::Killall(_) | Sel::Group(_)))
    }

    fn user_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for f in &self.findings {
            match &f.sel {
                Sel::Pgrep(s) => names.extend(s.euids.iter().chain(&s.ruids).cloned()),
                Sel::Killall(s) => names.extend(s.user.clone()),
                _ => {}
            }
        }
        names
    }

    fn needs_pgids(&self) -> bool {
        self.findings.iter().any(|f| match &f.sel {
            Sel::Group(_) => true,
            Sel::Pgrep(s) => !s.pgrps.is_empty(),
            _ => false,
        })
    }

    fn pidfiles(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter_map(|f| match &f.sel {
                Sel::Pgrep(s) => s.pidfile.clone(),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Finding {
    /// The invocation as written, for the reason.
    text: String,
    /// Run through `sudo`: it can reach processes this user cannot.
    elevated: bool,
    /// The set is the whole answer. False when something downstream of a
    /// `pgrep` (`| head -1`) may narrow it, so only what it *could* reach
    /// counts, and its size does not.
    bounded: bool,
    sel: Sel,
}

#[derive(Debug, Clone)]
enum Sel {
    Pgrep(PgrepSpec),
    Killall(KillallSpec),
    /// `kill 0` or `kill -1`: no pattern, a scope of the shell's group or of
    /// everything.
    Special(&'static str),
    /// `kill -- -N`: the members of process group N.
    Group(i32),
    Unevaluable(String),
}

#[derive(Debug, Clone, Default)]
struct PgrepSpec {
    prog: &'static str,
    patterns: Vec<String>,
    full: bool,
    icase: bool,
    exact: bool,
    inverse: bool,
    newest: bool,
    oldest: bool,
    ancestors: bool,
    euids: Vec<String>,
    ruids: Vec<String>,
    ppids: Vec<String>,
    pgrps: Vec<String>,
    pidfile: Option<String>,
    /// The options that came after the first pattern, which BSD `getopt`
    /// reads as more patterns.
    late: Vec<String>,
    /// The same arguments with the options first.
    corrected: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct KillallSpec {
    names: Vec<String>,
    regex: bool,
    user: Option<String>,
    only: Option<String>,
    late: Vec<String>,
    corrected: Option<String>,
}

/// What reading one invocation's arguments came to.
enum Parsed<T> {
    Spec(T),
    /// The tool refuses these arguments or sends nothing: no signal goes out.
    NoEffect(String),
    Unevaluable(String),
}

const KEYWORDS: [&str; 11] = [
    "if", "then", "else", "elif", "while", "until", "do", "!", "time", "{", "}",
];
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];
/// First commands whose output is a handful of pids at most.
const NARROW_PRODUCERS: [&str; 7] = ["lsof", "cat", "echo", "printf", "jobs", "pbpaste", "head"];

const SIGNALS: [&str; 41] = [
    "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "IOT", "EMT", "FPE", "KILL", "BUS", "SEGV", "SYS",
    "PIPE", "ALRM", "TERM", "URG", "STOP", "TSTP", "CONT", "CHLD", "CLD", "TTIN", "TTOU", "IO",
    "POLL", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "INFO", "USR1", "USR2", "PWR", "STKFLT",
    "RTMIN", "RTMAX", "EXIT", "LOST", "UNUSED",
];

/// A signal given as a number or a name, with or without `SIG`. `Some(true)`
/// when it is signal 0, which only tests that a process exists.
fn signal_spec(spec: &str) -> Option<bool> {
    if !spec.is_empty() && spec.len() <= 3 && spec.bytes().all(|b| b.is_ascii_digit()) {
        return Some(spec.bytes().all(|b| b == b'0'));
    }
    let upper = spec.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    (SIGNALS.contains(&name) && name != "EXIT").then_some(false)
}

fn contains_kill(command: &str) -> bool {
    command
        .as_bytes()
        .windows(4)
        .any(|w| w.eq_ignore_ascii_case(b"kill"))
}

/// Reads `command` and finds every invocation that sends a signal.
pub(crate) fn plan(command: &str) -> Plan {
    plan_with(command, process_env)
}

fn plan_with(command: &str, env: EnvFn) -> Plan {
    let mut plan = Plan::default();
    if command.len() > MAX_COMMAND_BYTES {
        if contains_kill(command) {
            plan.unparsed = Some(format!(
                "the command is longer than the {MAX_COMMAND_BYTES} bytes GitPulse reads"
            ));
        }
        return plan;
    }
    let mut budget = Budget { commands: 0 };
    match parse_script(command, 0, &mut budget, env) {
        Ok(script) => {
            let mut cx = Cx {
                plan: &mut plan,
                budget: &mut budget,
                env,
            };
            cx.walk(&script, 0);
        }
        Err(why) => {
            if contains_kill(command) {
                plan.unparsed = Some(why);
            }
        }
    }
    plan.findings.truncate(MAX_FINDINGS);
    plan
}

struct Cx<'a> {
    plan: &'a mut Plan,
    budget: &'a mut Budget,
    env: EnvFn,
}

/// The command a wrapper (`sudo`, `env`, `nohup` …) runs.
struct Resolved<'a> {
    prog: String,
    prog_at: usize,
    args: &'a [Word],
    elevated: bool,
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_assignment(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn dashed(word: Option<&Word>) -> bool {
    word.is_some_and(|w| w.text.len() > 1 && w.text.starts_with('-'))
}

/// Skips the options (and their arguments, for those named in `with_arg`) of a
/// wrapper, returning the index of what follows.
fn skip_options(words: &[Word], mut i: usize, with_arg: &[&str]) -> usize {
    while dashed(words.get(i)) {
        let t = words[i].text.as_str();
        i += 1;
        if t == "--" {
            break;
        }
        if with_arg.contains(&t) {
            i += 1;
        }
    }
    i
}

/// Looks through `env`, `sudo`, `command`, `nohup` and the other wrappers to
/// the program that runs. `None` when nothing runs (a builtin that only
/// queries, a program named by a variable).
fn resolve(words: &[Word]) -> Option<Resolved<'_>> {
    let mut i = 0usize;
    let mut elevated = false;
    loop {
        let w = words.get(i)?;
        if is_assignment(&w.text) || KEYWORDS.contains(&w.text.as_str()) {
            i += 1;
            continue;
        }
        if w.dynamic {
            return None;
        }
        let base = base_name(w.text.trim_start_matches('\\'));
        i = match base {
            "env" => {
                let mut k = skip_options(words, i + 1, &["-u", "-C", "-P", "-S"]);
                while words.get(k).is_some_and(|w| is_assignment(&w.text)) {
                    k += 1;
                }
                k
            }
            "sudo" | "doas" => {
                let other_user = words[i + 1..]
                    .iter()
                    .position(|w| w.text == "-u")
                    .and_then(|p| words.get(i + 2 + p))
                    .is_some_and(|w| w.text != "root");
                elevated = elevated || !other_user;
                skip_options(
                    words,
                    i + 1,
                    &["-u", "-g", "-p", "-C", "-D", "-h", "-r", "-t", "-T", "-U"],
                )
            }
            "command" => {
                if words
                    .get(i + 1)
                    .is_some_and(|w| w.text == "-v" || w.text == "-V")
                {
                    return None;
                }
                skip_options(words, i + 1, &[])
            }
            "builtin" | "nohup" | "setsid" | "time" | "unbuffer" | "stdbuf" => {
                skip_options(words, i + 1, &["-i", "-o", "-e"])
            }
            "exec" => skip_options(words, i + 1, &["-a"]),
            "nice" => skip_options(words, i + 1, &["-n"]),
            "arch" => skip_options(words, i + 1, &["-e"]),
            "caffeinate" => skip_options(words, i + 1, &["-t", "-w"]),
            "timeout" => skip_options(words, i + 1, &["-s", "-k"]) + 1,
            _ => {
                return Some(Resolved {
                    prog: base.to_string(),
                    prog_at: i,
                    args: &words[i + 1..],
                    elevated,
                })
            }
        };
    }
}

fn echo(words: &[Word]) -> String {
    let mut text = words
        .iter()
        .map(|w| w.src.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > ECHO_CHARS {
        text = text.chars().take(ECHO_CHARS).collect::<String>() + "…";
    }
    text
}

/// The first non-option word after `xargs`' own options.
fn xargs_command(args: &[Word]) -> &[Word] {
    let mut i = 0;
    while dashed(args.get(i)) {
        let t = args[i].text.clone();
        i += 1;
        if t == "--" {
            break;
        }
        if t.starts_with("--") {
            continue;
        }
        let body: Vec<char> = t.chars().skip(1).collect();
        for (k, c) in body.iter().enumerate() {
            if "IJLnPRSsEda".contains(*c) {
                if k + 1 == body.len() {
                    i += 1;
                }
                break;
            }
        }
    }
    args.get(i..).unwrap_or(&[])
}

/// The script a shell was handed with `-c`.
fn shell_script(args: &[Word]) -> Option<&Word> {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        let t = w.text.as_str();
        if !t.starts_with('-') || t == "--" {
            return None;
        }
        if !t.starts_with("--") && t.contains('c') {
            return args.get(i + 1);
        }
        i += 1;
    }
    None
}

impl Cx<'_> {
    fn push(&mut self, finding: Finding) {
        if self.plan.findings.len() < MAX_FINDINGS {
            self.plan.findings.push(finding);
        }
    }

    fn unevaluable(&mut self, text: String, elevated: bool, why: String) {
        self.push(Finding {
            text,
            elevated,
            bounded: true,
            sel: Sel::Unevaluable(why),
        });
    }

    fn walk(&mut self, script: &[Pipeline], depth: usize) {
        for pipeline in script {
            for (at, cmd) in pipeline.iter().enumerate() {
                self.command(pipeline, at, cmd, depth);
            }
        }
    }

    fn script_text(&mut self, text: &str, depth: usize) {
        if depth >= MAX_DEPTH {
            if contains_kill(text) {
                self.plan.unparsed = Some(format!("commands nested more than {MAX_DEPTH} deep"));
            }
            return;
        }
        match parse_script(text, depth + 1, self.budget, self.env) {
            Ok(script) => self.walk(&script, depth + 1),
            Err(why) => {
                if contains_kill(text) {
                    self.plan.unparsed = Some(why);
                }
            }
        }
    }

    fn command(&mut self, pipeline: &Pipeline, at: usize, cmd: &Cmd, depth: usize) {
        for w in cmd.words.iter().chain(&cmd.redirects) {
            for body in &w.subst {
                self.walk(body, depth + 1);
            }
        }
        for h in &cmd.heredocs {
            for body in &h.nested {
                self.walk(body, depth + 1);
            }
        }
        let Some(r) = resolve(&cmd.words) else { return };
        let text = echo(&cmd.words[r.prog_at..]);
        match r.prog.as_str() {
            "pkill" => self.pgrep(&r, text, true, true),
            "killall" => match parse_killall(r.args) {
                Parsed::Spec(spec) => self.push(Finding {
                    text,
                    elevated: r.elevated,
                    bounded: true,
                    sel: Sel::Killall(spec),
                }),
                Parsed::NoEffect(_) => {}
                Parsed::Unevaluable(why) => self.unevaluable(text, r.elevated, why),
            },
            "kill" => self.kill(r.args, &text, r.elevated, None),
            "xargs" => {
                let inner = xargs_command(r.args);
                let Some(i) = resolve(inner) else { return };
                let upstream = (at > 0).then(|| (&pipeline[0], at == 1));
                match i.prog.as_str() {
                    "kill" => self.kill(i.args, &text, r.elevated || i.elevated, upstream),
                    "pkill" | "killall" => self.unevaluable(
                        text,
                        r.elevated,
                        format!("the arguments of `{}` come from xargs", i.prog),
                    ),
                    p if SHELLS.contains(&p) => {
                        if let Some(script) = shell_script(i.args) {
                            self.shell_script(script, depth);
                        }
                    }
                    _ => {}
                }
            }
            p if SHELLS.contains(&p) => {
                if let Some(script) = shell_script(r.args) {
                    self.shell_script(script, depth);
                }
                for h in &cmd.heredocs {
                    self.script_text(&h.body, depth);
                }
            }
            "eval" if r.args.iter().all(|w| !w.dynamic) => {
                let joined = r
                    .args
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                self.script_text(&joined, depth);
            }
            _ => {}
        }
    }

    fn shell_script(&mut self, script: &Word, depth: usize) {
        if script.dynamic {
            return;
        }
        let text = script.text.clone();
        self.script_text(&text, depth);
    }

    fn pgrep(&mut self, r: &Resolved<'_>, text: String, pkill: bool, bounded: bool) {
        match parse_pgrep(r.args, pkill) {
            Parsed::Spec(spec) => self.push(Finding {
                text,
                elevated: r.elevated,
                bounded,
                sel: Sel::Pgrep(spec),
            }),
            Parsed::NoEffect(_) => {}
            Parsed::Unevaluable(why) => self.unevaluable(text, r.elevated, why),
        }
    }

    /// Where the pids a `kill` is given come from: the first command of the
    /// pipeline that produces them.
    fn producer(&mut self, first: &Cmd, single: bool, text: &str, elevated: bool) {
        let bounded = single;
        let Some(r) = resolve(&first.words) else {
            self.unevaluable(
                text.to_string(),
                elevated,
                "the pids come from a command GitPulse could not read".into(),
            );
            return;
        };
        let source = echo(&first.words[r.prog_at..]);
        match r.prog.as_str() {
            "pgrep" => self.pgrep(&r, format!("{text} (pids from `{source}`)"), false, bounded),
            "pidof" => {
                let names: Vec<String> = r
                    .args
                    .iter()
                    .filter(|w| !w.text.starts_with('-'))
                    .map(|w| w.text.clone())
                    .collect();
                if r.args.iter().any(|w| w.dynamic) {
                    self.unevaluable(
                        format!("{text} (pids from `{source}`)"),
                        elevated,
                        "a process name built from a variable".into(),
                    );
                } else if !names.is_empty() {
                    self.push(Finding {
                        text: format!("{text} (pids from `{source}`)"),
                        elevated,
                        bounded,
                        sel: Sel::Killall(KillallSpec {
                            names,
                            ..KillallSpec::default()
                        }),
                    });
                }
            }
            p if NARROW_PRODUCERS.contains(&p) => {}
            other => self.unevaluable(
                format!("{text} (pids from `{source}`)"),
                elevated,
                format!("the pids come from `{other}`, which GitPulse cannot evaluate"),
            ),
        }
    }

    /// `kill`: signal, then targets. `upstream` is the pipeline an `xargs`
    /// feeds it from.
    fn kill(&mut self, args: &[Word], text: &str, elevated: bool, upstream: Option<(&Cmd, bool)>) {
        let mut i = 0;
        let mut signal_only = None;
        while let Some(w) = args.get(i) {
            let t = w.text.as_str();
            if w.dynamic && t.starts_with('-') {
                self.unevaluable(
                    text.to_string(),
                    elevated,
                    "a signal or process group built from a variable".into(),
                );
                return;
            }
            if t == "--" {
                i += 1;
                break;
            }
            if t == "-l" || t == "-L" {
                return;
            }
            if t == "-s" || t == "-n" {
                if args.get(i + 1).and_then(|w| signal_spec(&w.text)) == Some(true) {
                    return;
                }
                i += 2;
                continue;
            }
            match t.strip_prefix('-').and_then(signal_spec) {
                Some(true) => return,
                Some(false) if i == 0 => {
                    signal_only = Some(t == "-1");
                    i += 1;
                }
                _ => break,
            }
        }
        let targets = &args[i.min(args.len())..];
        if targets.is_empty() && upstream.is_none() {
            if signal_only == Some(true) {
                self.push(Finding {
                    text: text.to_string(),
                    elevated,
                    bounded: true,
                    sel: Sel::Special("bare -1"),
                });
            }
            return;
        }
        for w in targets {
            let t = w.text.as_str();
            if let Some(body) = w.subst.last() {
                if let Some(first) = body
                    .last()
                    .and_then(|pl| pl.first().map(|c| (c, pl.len() == 1)))
                {
                    self.producer(first.0, first.1, text, elevated);
                }
                continue;
            }
            if w.dynamic {
                if t.starts_with('-') {
                    self.unevaluable(
                        text.to_string(),
                        elevated,
                        "a process group built from a variable".into(),
                    );
                }
                continue;
            }
            let Ok(pid) = t.parse::<i64>() else { continue };
            let sel = match pid {
                0 => Sel::Special("0"),
                -1 => Sel::Special("-1"),
                p if p < -1 => match i32::try_from(-p) {
                    Ok(group) => Sel::Group(group),
                    Err(_) => continue,
                },
                _ => continue,
            };
            self.push(Finding {
                text: text.to_string(),
                elevated,
                bounded: true,
                sel,
            });
        }
        if let Some((first, adjacent)) = upstream {
            self.producer(first, adjacent, text, elevated);
        }
    }
}

/// The result of reading an argument list the way BSD `getopt` does, or the way
/// a permuting parser would.
struct Classified {
    opts: Vec<(char, Option<String>)>,
    /// Indexes of the words that are options or option arguments.
    option_words: Vec<usize>,
    operands: Vec<usize>,
    /// Operands that look like options: what `getopt` stopped before.
    late: Vec<usize>,
}

enum Reject {
    Usage(String),
    Unevaluable(String),
}

/// `getopt(3)` over `args`. BSD stops at the first operand, so everything after
/// it is an operand however it is spelled; `permute` reads on past operands for
/// the corrected command. `signals` accepts `-TERM` and `-15` as an option
/// (`S`, with the signal as its argument), as `killall` does.
fn classify(
    args: &[Word],
    flags: &str,
    with_arg: &str,
    permute: bool,
    signals: bool,
) -> Result<Classified, Reject> {
    let mut out = Classified {
        opts: Vec::new(),
        option_words: Vec::new(),
        operands: Vec::new(),
        late: Vec::new(),
    };
    let mut i = 0;
    let mut seen_operand = false;
    while i < args.len() {
        let w = &args[i];
        let t = w.text.as_str();
        let looks_like_option = t.len() > 1 && t.starts_with('-');
        if seen_operand && !permute {
            if looks_like_option && t != "--" {
                out.late.push(i);
            }
            out.operands.push(i);
            i += 1;
            continue;
        }
        if t == "--" {
            out.option_words.push(i);
            out.operands.extend(i + 1..args.len());
            return Ok(out);
        }
        if !looks_like_option {
            out.operands.push(i);
            seen_operand = true;
            i += 1;
            continue;
        }
        if w.dynamic {
            return Err(Reject::Unevaluable(
                "an option built from a variable".into(),
            ));
        }
        let body = &t[1..];
        if signals && signal_spec(body).is_some() {
            out.opts.push(('S', Some(body.to_string())));
            out.option_words.push(i);
            i += 1;
            continue;
        }
        out.option_words.push(i);
        let letters: Vec<char> = body.chars().collect();
        for (k, c) in letters.iter().enumerate() {
            if with_arg.contains(*c) {
                let rest: String = letters[k + 1..].iter().collect();
                if rest.is_empty() {
                    let Some(next) = args.get(i + 1) else {
                        return Err(Reject::Usage(format!("option requires an argument -- {c}")));
                    };
                    if next.dynamic {
                        return Err(Reject::Unevaluable(format!(
                            "the argument of -{c} is built from a variable"
                        )));
                    }
                    out.opts.push((*c, Some(next.text.clone())));
                    out.option_words.push(i + 1);
                    i += 1;
                } else {
                    out.opts.push((*c, Some(rest)));
                }
                break;
            } else if flags.contains(*c) {
                out.opts.push((*c, None));
            } else {
                return Err(Reject::Usage(format!("illegal option -- {c}")));
            }
        }
        i += 1;
    }
    Ok(out)
}

/// The arguments with every option ahead of every operand, as one command.
fn corrected_command(prog: &str, args: &[Word], head: &[Word], c: &Classified) -> String {
    let mut parts: Vec<&str> = vec![prog];
    parts.extend(head.iter().map(|w| w.src.as_str()));
    parts.extend(c.option_words.iter().filter_map(|&i| {
        let w = &args[i];
        (w.text != "--").then_some(w.src.as_str())
    }));
    let dashes = c.operands.iter().any(|&i| args[i].text.starts_with('-'));
    if dashes {
        parts.push("--");
    }
    parts.extend(c.operands.iter().map(|&i| args[i].src.as_str()));
    parts.join(" ")
}

fn list(arg: &str) -> Vec<String> {
    arg.split(',').map(str::to_string).collect()
}

fn parse_pgrep(all: &[Word], pkill: bool) -> Parsed<PgrepSpec> {
    // `-signal` only counts as the first argument, and only to pkill.
    let mut head: &[Word] = &[];
    let mut args = all;
    if pkill {
        if let Some(first) = all.first() {
            if let Some(body) = first.text.strip_prefix('-') {
                if !first.dynamic {
                    if let Some(zero) = signal_spec(body) {
                        if zero {
                            return Parsed::NoEffect("signal 0 only tests for existence".into());
                        }
                        head = &all[..1];
                        args = &all[1..];
                    }
                }
            }
        }
    }
    let flags = if pkill { "ILafilnoqvxD" } else { "Lafilnoqvx" };
    let classified = match classify(args, flags, "FGPUdgtu", false, false) {
        Ok(c) => c,
        Err(Reject::Usage(why)) => return Parsed::NoEffect(why),
        Err(Reject::Unevaluable(why)) => return Parsed::Unevaluable(why),
    };
    let prog = if pkill { "pkill" } else { "pgrep" };
    let mut spec = PgrepSpec {
        prog,
        ..PgrepSpec::default()
    };
    let mut criteria = false;
    for (flag, arg) in &classified.opts {
        let arg = arg.clone().unwrap_or_default();
        match flag {
            'f' => spec.full = true,
            'i' => spec.icase = true,
            'x' => spec.exact = true,
            'v' => spec.inverse = true,
            'n' => spec.newest = true,
            'o' => spec.oldest = true,
            'a' => spec.ancestors = true,
            'u' => {
                criteria = true;
                spec.euids.extend(list(&arg));
            }
            'U' => {
                criteria = true;
                spec.ruids.extend(list(&arg));
            }
            'P' => {
                criteria = true;
                spec.ppids.extend(list(&arg));
            }
            'g' => {
                criteria = true;
                if list(&arg).iter().any(|g| g == "0") {
                    return Parsed::Unevaluable(
                        "`-g 0` is the process group of the shell the command runs in".into(),
                    );
                }
                spec.pgrps.extend(list(&arg));
            }
            'F' => {
                criteria = true;
                spec.pidfile = Some(arg);
            }
            'G' | 't' => {
                return Parsed::Unevaluable(format!(
                    "`-{flag}` selects by {}, which is not in the process table GitPulse reads",
                    if *flag == 'G' { "group id" } else { "terminal" }
                ));
            }
            _ => {}
        }
    }
    if classified.opts.iter().any(|(f, _)| *f == 'L') && spec.pidfile.is_none() {
        return Parsed::NoEffect("-L without -F is a usage error".into());
    }
    if classified.operands.is_empty() && !criteria {
        return Parsed::NoEffect("no pattern and no criteria is a usage error".into());
    }
    if classified.operands.len() > MAX_PATTERNS {
        return Parsed::Unevaluable(format!("more than {MAX_PATTERNS} patterns"));
    }
    for &i in &classified.operands {
        let w = &args[i];
        if w.dynamic {
            return Parsed::Unevaluable(format!(
                "the pattern `{}` is built from a variable GitPulse cannot expand",
                w.src
            ));
        }
        if w.text.is_empty() {
            return Parsed::NoEffect("an empty pattern is an error".into());
        }
        if w.text.chars().count() > MAX_PATTERN_CHARS {
            return Parsed::Unevaluable(format!("a pattern longer than {MAX_PATTERN_CHARS}"));
        }
        spec.patterns.push(w.text.clone());
    }
    spec.late = classified
        .late
        .iter()
        .map(|&i| args[i].text.clone())
        .collect();
    if !spec.late.is_empty() {
        if let Ok(permuted) = classify(args, flags, "FGPUdgtu", true, false) {
            spec.corrected = Some(corrected_command(prog, args, head, &permuted));
        }
    }
    Parsed::Spec(spec)
}

fn parse_killall(args: &[Word]) -> Parsed<KillallSpec> {
    let classified = match classify(args, "dehlmsvqzI", "utc", false, true) {
        Ok(c) => c,
        Err(Reject::Usage(why)) => return Parsed::NoEffect(why),
        Err(Reject::Unevaluable(why)) => return Parsed::Unevaluable(why),
    };
    let mut spec = KillallSpec::default();
    let mut sends = true;
    for (flag, arg) in &classified.opts {
        match flag {
            'd' | 's' | 'l' => sends = false,
            'm' => spec.regex = true,
            'u' => spec.user = arg.clone(),
            'c' => spec.only = arg.clone(),
            't' => {
                return Parsed::Unevaluable(
                    "`-t` selects by terminal, which is not in the process table GitPulse reads"
                        .into(),
                )
            }
            'S' if arg.as_deref().and_then(signal_spec) == Some(true) => {
                return Parsed::NoEffect("signal 0 only tests for existence".into());
            }
            _ => {}
        }
    }
    if !sends {
        return Parsed::NoEffect("-d, -s and -l send no signal".into());
    }
    if classified.operands.is_empty() && spec.user.is_none() && spec.only.is_none() {
        return Parsed::NoEffect("no process to select is a usage error".into());
    }
    for &i in &classified.operands {
        let w = &args[i];
        if w.dynamic {
            return Parsed::Unevaluable(format!(
                "the name `{}` is built from a variable GitPulse cannot expand",
                w.src
            ));
        }
        if w.text.chars().count() > MAX_PATTERN_CHARS {
            return Parsed::Unevaluable(format!("a name longer than {MAX_PATTERN_CHARS}"));
        }
        spec.names.push(w.text.clone());
    }
    spec.late = classified
        .late
        .iter()
        .map(|&i| args[i].text.clone())
        .collect();
    if !spec.late.is_empty() {
        if let Ok(permuted) = classify(args, "dehlmsvqzI", "utc", true, true) {
            spec.corrected = Some(corrected_command("killall", args, &[], &permuted));
        }
    }
    Parsed::Spec(spec)
}

/* ── 3. The process table, as facts ───────────────────────────────────────── */

/// Everything [`decide`] reads, gathered up front so it can be a pure function.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub(crate) rows: Vec<TableRow>,
    /// The user the table was read as, and so the user whose processes a
    /// signal can reach.
    pub(crate) me_uid: u32,
    /// This process: never a match, as `pkill` never matches itself.
    pub(crate) self_pid: i32,
    pub(crate) users: HashMap<String, u32>,
    pub(crate) pgids: HashMap<i32, i32>,
    pub(crate) pidfiles: HashMap<String, Result<i32, String>>,
}

#[cfg(unix)]
fn lookup_user(name: &str) -> Option<u32> {
    if let Ok(uid) = name.parse::<u32>() {
        return Some(uid);
    }
    let c_name = std::ffi::CString::new(name).ok()?;
    // SAFETY: `passwd` is plain data; an all-zero one is a valid value to fill.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to a live local, `buffer` is the length given,
    // and `getpwnam_r` is the reentrant form.
    let rc = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    (rc == 0 && !found.is_null()).then_some(entry.pw_uid)
}

#[cfg(not(unix))]
fn lookup_user(name: &str) -> Option<u32> {
    name.parse().ok()
}

#[cfg(unix)]
fn group_of(pid: i32) -> Option<i32> {
    // SAFETY: `getpgid` takes a pid and has no other precondition.
    let group = unsafe { libc::getpgid(pid) };
    (group > 0).then_some(group)
}

#[cfg(not(unix))]
fn group_of(_pid: i32) -> Option<i32> {
    None
}

fn read_pidfile(path: &str) -> Result<i32, String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(64).read_to_string(&mut text))
        .map_err(|e| format!("cannot read pidfile {path}: {e}"))?;
    text.trim()
        .parse()
        .map_err(|_| format!("invalid pid in pidfile {path}"))
}

/// Reads the process table and what `plan` needs beside it.
pub(crate) fn gather(plan: &Plan) -> Result<Facts, String> {
    let table = signal_table()?;
    let mut facts = Facts {
        me_uid: table.me_uid,
        self_pid: i32::try_from(std::process::id()).unwrap_or(-1),
        rows: table.rows,
        ..Facts::default()
    };
    for name in plan.user_names() {
        if let Some(uid) = lookup_user(&name) {
            facts.users.insert(name, uid);
        }
    }
    if plan.needs_pgids() {
        for row in &facts.rows {
            if reachable(&facts, row) {
                if let Some(group) = group_of(row.pid) {
                    facts.pgids.insert(row.pid, group);
                }
            }
        }
    }
    for path in plan.pidfiles() {
        let read = read_pidfile(&path);
        facts.pidfiles.insert(path, read);
    }
    Ok(facts)
}

/// A signal from this user reaches the process.
fn reachable(facts: &Facts, row: &TableRow) -> bool {
    facts.me_uid == 0 || row.uid == facts.me_uid
}

/// This process and the processes it descends from.
fn lineage(facts: &Facts) -> Vec<i32> {
    let parents: HashMap<i32, i32> = facts.rows.iter().map(|r| (r.pid, r.ppid)).collect();
    let mut chain = vec![facts.self_pid];
    let mut at = facts.self_pid;
    for _ in 0..MAX_ANCESTORS {
        match parents.get(&at) {
            Some(&parent) if parent > 0 && !chain.contains(&parent) => {
                chain.push(parent);
                at = parent;
            }
            _ => break,
        }
    }
    chain
}

/* ── 4. Which processes would the tool pick ───────────────────────────────── */

/// What `pgrep` matches a name pattern against: `argv[0]`'s last component,
/// which is what the real tool was observed to use (it is not the kernel's
/// 16-character name).
fn match_name(row: &TableRow) -> String {
    match row.argv.as_ref().and_then(|a| a.first()) {
        Some(first) if !first.is_empty() => base_name(first).to_string(),
        _ => row.name.clone(),
    }
}

fn match_full(row: &TableRow) -> String {
    match &row.argv {
        Some(argv) if !argv.is_empty() => argv.join(" "),
        _ => row.name.clone(),
    }
}

fn compile(pattern: &str, icase: bool, exact: bool) -> Result<Regex, String> {
    let source = if exact {
        format!("^(?:{pattern})$")
    } else {
        pattern.to_string()
    };
    RegexBuilder::new(&source)
        .case_insensitive(icase)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("the pattern `{pattern}` is not one GitPulse can evaluate ({e})"))
}

fn uids(facts: &Facts, names: &[String]) -> Result<Vec<u32>, String> {
    names
        .iter()
        .map(|n| {
            n.parse::<u32>()
                .ok()
                .or_else(|| facts.users.get(n).copied())
                .ok_or_else(|| format!("GitPulse could not look up the user `{n}`"))
        })
        .collect()
}

fn numbers(names: &[String]) -> Result<Vec<i32>, String> {
    names
        .iter()
        .map(|n| {
            n.parse::<i32>()
                .map_err(|_| format!("`{n}` is not a number"))
        })
        .collect()
}

/// What a selection reached, and what it could not read.
struct Reach<'a> {
    hits: Vec<&'a TableRow>,
    /// This user's processes whose arguments could not be read, which a
    /// `-f` pattern could not be tested against.
    unread: usize,
}

fn eval_pgrep<'a>(spec: &PgrepSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let patterns: Vec<Regex> = spec
        .patterns
        .iter()
        .map(|p| compile(p, spec.icase, spec.exact))
        .collect::<Result<_, _>>()?;
    let euids = uids(facts, &spec.euids)?;
    let ruids = uids(facts, &spec.ruids)?;
    let ppids = numbers(&spec.ppids)?;
    let pgrps = numbers(&spec.pgrps)?;
    let pidfile = match &spec.pidfile {
        Some(path) => match facts.pidfiles.get(path) {
            Some(Ok(pid)) => Some(*pid),
            Some(Err(why)) => return Err(why.clone()),
            None => return Err(format!("the pidfile {path} was not read")),
        },
        None => None,
    };
    // `pkill` never matches itself or its ancestors; with `-v` the ancestors
    // are in the list again. Here "itself" is the hook, which is a sibling of
    // the shell the tool would run in and shares every ancestor above it.
    let excluded: Vec<i32> = if spec.ancestors || spec.inverse {
        vec![facts.self_pid]
    } else {
        lineage(facts)
    };
    let mut hits = Vec::new();
    let mut unread = 0;
    for row in &facts.rows {
        if !reachable(facts, row) || excluded.contains(&row.pid) {
            continue;
        }
        let subject_unread = spec.full && row.argv.is_none();
        let matched = patterns.is_empty() || {
            let subject = if spec.full {
                match_full(row)
            } else {
                match_name(row)
            };
            patterns.iter().any(|p| p.is_match(&subject))
        };
        let filtered = matched
            && (euids.is_empty() || euids.contains(&row.uid))
            && (ruids.is_empty() || ruids.contains(&row.uid))
            && (ppids.is_empty() || ppids.contains(&row.ppid))
            && (pgrps.is_empty() || facts.pgids.get(&row.pid).is_some_and(|g| pgrps.contains(g)))
            && pidfile.is_none_or(|p| p == row.pid);
        if filtered != spec.inverse {
            hits.push(row);
        } else if subject_unread {
            unread += 1;
        }
    }
    if spec.newest {
        hits = hits
            .into_iter()
            .max_by_key(|r| (r.start_us, r.pid))
            .into_iter()
            .collect();
    } else if spec.oldest {
        hits = hits
            .into_iter()
            .min_by_key(|r| (r.start_us, r.pid))
            .into_iter()
            .collect();
    }
    Ok(Reach { hits, unread })
}

fn eval_killall<'a>(spec: &KillallSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let patterns: Vec<Regex> = if spec.regex {
        spec.names
            .iter()
            .map(|n| compile(n, false, false))
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    let user = match &spec.user {
        Some(name) => uids(facts, std::slice::from_ref(name))?.first().copied(),
        None => None,
    };
    let mut hits = Vec::new();
    for row in &facts.rows {
        if row.pid == facts.self_pid || !reachable(facts, row) {
            continue;
        }
        if user.is_some_and(|u| u != row.uid) {
            continue;
        }
        let name = match_name(row);
        if spec.only.as_ref().is_some_and(|c| *c != name) {
            continue;
        }
        let named = spec.names.is_empty()
            || if spec.regex {
                patterns.iter().any(|p| p.is_match(&name))
            } else {
                spec.names.contains(&name)
            };
        if named {
            hits.push(row);
        }
    }
    Ok(Reach { hits, unread: 0 })
}

fn eval_group<'a>(group: i32, facts: &'a Facts) -> Reach<'a> {
    Reach {
        hits: facts
            .rows
            .iter()
            .filter(|r| reachable(facts, r) && facts.pgids.get(&r.pid) == Some(&group))
            .collect(),
        unread: 0,
    }
}

/// The processes one invocation would signal, as a tool reading this table
/// would pick them. `pkill` and `pgrep` are the same selection; `killall`
/// picks by name.
pub fn live_match_pids(program: &str, args: &[&str]) -> Result<Vec<i32>, String> {
    let words: Vec<Word> = args
        .iter()
        .map(|a| Word {
            src: (*a).to_string(),
            text: (*a).to_string(),
            ..Word::default()
        })
        .collect();
    let sel = match program {
        "pkill" | "pgrep" => match parse_pgrep(&words, program == "pkill") {
            Parsed::Spec(spec) => Sel::Pgrep(spec),
            Parsed::NoEffect(why) | Parsed::Unevaluable(why) => return Err(why),
        },
        "killall" => match parse_killall(&words) {
            Parsed::Spec(spec) => Sel::Killall(spec),
            Parsed::NoEffect(why) | Parsed::Unevaluable(why) => return Err(why),
        },
        other => return Err(format!("{other} is not a program this guard reads")),
    };
    let plan = Plan {
        findings: vec![Finding {
            text: String::new(),
            elevated: false,
            bounded: true,
            sel,
        }],
        unparsed: None,
    };
    let facts = gather(&plan)?;
    let mut pids: Vec<i32> = match &plan.findings[0].sel {
        Sel::Pgrep(spec) => eval_pgrep(spec, &facts)?.hits,
        Sel::Killall(spec) => eval_killall(spec, &facts)?.hits,
        _ => Vec::new(),
    }
    .into_iter()
    .map(|r| r.pid)
    .collect();
    pids.sort_unstable();
    Ok(pids)
}

/* ── 5. The decision ──────────────────────────────────────────────────────── */

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Nothing to say, or only a notice for the user.
    Pass(Vec<String>),
    Ask(String),
    Deny(String),
}

/// What a set of processes is made of.
#[derive(Debug, Default)]
struct Tally {
    total: usize,
    agents: usize,
    hosted_agents: usize,
    gitpulse: usize,
    hosted: usize,
    /// Terminal sessions GitPulse hosts: a process GitPulse launched whose
    /// parent is GitPulse itself (the app, or the terminal host
    /// `gitpulse-ptyd`). A shell is one; a job running under that shell is
    /// not, so killing a dev server you started in a GitPulse terminal is not
    /// this guard's business.
    sessions: usize,
    examples: Vec<String>,
}

fn is_gitpulse(row: &TableRow) -> bool {
    let program = row
        .exec
        .as_deref()
        .or_else(|| {
            row.argv
                .as_ref()
                .and_then(|a| a.first())
                .map(String::as_str)
        })
        .unwrap_or(&row.name);
    let name = base_name(program).to_ascii_lowercase();
    let in_app = row
        .exec
        .as_deref()
        .is_some_and(|e| e.contains("/GitPulse.app/"));
    (name.starts_with("gitpulse") && name != "gitpulse-hook") || in_app
}

/// Whether `row` is a terminal session GitPulse hosts: launched by GitPulse
/// (it carries a GitPulse launch id) and a child of a GitPulse process.
fn is_hosted_session(row: &TableRow, rows: &[TableRow]) -> bool {
    row.hosted
        && rows
            .iter()
            .find(|parent| parent.pid == row.ppid)
            .is_some_and(is_gitpulse)
}

fn tally(hits: &[&TableRow], rows: &[TableRow]) -> Tally {
    let mut t = Tally {
        total: hits.len(),
        ..Tally::default()
    };
    let mut ranked: Vec<(u8, &TableRow)> = Vec::new();
    for row in hits {
        let agent = row.agent.is_some();
        let gitpulse = is_gitpulse(row);
        let session = is_hosted_session(row, rows);
        t.agents += usize::from(agent);
        t.hosted_agents += usize::from(agent && row.hosted);
        t.gitpulse += usize::from(gitpulse);
        t.hosted += usize::from(row.hosted);
        t.sessions += usize::from(session);
        let rank = match (agent && row.hosted, agent, gitpulse || session) {
            (true, _, _) => 0,
            (_, true, _) => 1,
            (_, _, true) => 2,
            _ => 3,
        };
        ranked.push((rank, row));
    }
    ranked.sort_by_key(|(rank, row)| (*rank, row.pid));
    t.examples = ranked
        .iter()
        .take(NAMED_HITS)
        .map(|(_, row)| match row.agent {
            // A native Claude's own name is its version string.
            Some(kind) => format!("{} {} session", row.pid, kind.name()),
            None => format!("{} {}", row.pid, match_name(row)),
        })
        .collect();
    t
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn describe_pgrep(spec: &PgrepSpec) -> String {
    let mut text = format!(
        "{} read {} {:?}, each matched against the {}",
        spec.prog,
        plural(spec.patterns.len(), "pattern", "patterns"),
        spec.patterns,
        if spec.full {
            "full command line (-f)"
        } else {
            "process name"
        }
    );
    if spec.icase {
        text.push_str(", ignoring case");
    }
    if spec.exact {
        text.push_str(", exactly (-x)");
    }
    if spec.inverse {
        text.push_str(", inverted (-v: every process that does NOT match)");
    }
    let filters: Vec<String> = [
        ("euid", &spec.euids),
        ("ruid", &spec.ruids),
        ("ppid", &spec.ppids),
        ("pgrp", &spec.pgrps),
    ]
    .iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| format!("{k} in {v:?}"))
    .collect();
    if !filters.is_empty() {
        text.push_str(&format!("; filters: {}", filters.join(", ")));
    }
    text.push('.');
    if !spec.late.is_empty() {
        text.push_str(&format!(
            " {} came after the first pattern, and macOS option parsing stops at the first \
             operand: everything after it is one more pattern OR'd with the first, not an \
             option. That is how `pkill -f \"sleep 60\" -u user` matches every process with \
             the user's name in its arguments.",
            spec.late
                .iter()
                .map(|l| format!("`{l}`"))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    text
}

fn describe_killall(spec: &KillallSpec) -> String {
    let mut text = format!(
        "killall read {} {:?}{}",
        plural(spec.names.len(), "name", "names"),
        spec.names,
        if spec.regex {
            ", as regular expressions (-m)"
        } else {
            ", each matched exactly against the process name"
        }
    );
    if let Some(user) = &spec.user {
        text.push_str(&format!("; limited to user {user}"));
    } else {
        text.push_str("; with no -u, every process of yours with that name");
    }
    text.push('.');
    if !spec.late.is_empty() {
        text.push_str(&format!(
            " {} came after the first name, and option parsing stops at the first operand, so \
             {} taken as more names.",
            spec.late
                .iter()
                .map(|l| format!("`{l}`"))
                .collect::<Vec<_>>()
                .join(", "),
            if spec.late.len() == 1 {
                "it was"
            } else {
                "they were"
            },
        ));
    }
    text
}

fn deny_reason(finding: &Finding, why: &str, tally: Option<&Tally>, advice: &str) -> String {
    let mut reason = format!("GitPulse's signal guard refused `{}`: {why}", finding.text);
    if let Some(t) = tally {
        reason.push_str(&format!(
            "\nIt would signal {} of your processes: {} agent session{} ({} hosted by GitPulse), \
             {} GitPulse process{} (the app, its terminal host gitpulse-ptyd, gitpulsed, \
             gitpulse-mcp), {} terminal session{} GitPulse hosts, and {} in a GitPulse \
             terminal tree. The bound for a pattern kill is {MAX_MATCHES}.",
            t.total,
            t.agents,
            if t.agents == 1 { "" } else { "s" },
            t.hosted_agents,
            t.gitpulse,
            if t.gitpulse == 1 { "" } else { "es" },
            t.sessions,
            if t.sessions == 1 { "" } else { "s" },
            t.hosted,
        ));
        if !t.examples.is_empty() {
            reason.push_str(&format!("\nIncluding: {}.", t.examples.join(", ")));
        }
    }
    reason.push('\n');
    reason.push_str(advice);
    reason
}

const NARROWER: &str =
    "Signal explicit pids you started yourself (`kill <pid>`), or use a pattern \
                        that names only your own processes.";

fn judge(finding: &Finding, facts: &Facts) -> Verdict {
    let reach = match &finding.sel {
        Sel::Unevaluable(why) => {
            return Verdict::Ask(format!(
                "GitPulse could not tell which processes `{}` would signal: {why}.",
                finding.text
            ))
        }
        Sel::Special(which) => {
            let scope = match *which {
                "0" => {
                    "every process in the shell's process group, which includes the agent \
                     session that started the command"
                }
                "-1" => {
                    "every process this user can signal: all agent sessions, GitPulse and every \
                     terminal"
                }
                _ => {
                    "no process as written (`kill -1` is signal 1 with no target), but it is \
                     one character from `kill -- -1`, which signals every process this user \
                     can"
                }
            };
            return Verdict::Deny(deny_reason(
                finding,
                &format!("it signals {scope}, and its scope cannot be narrowed."),
                None,
                NARROWER,
            ));
        }
        Sel::Pgrep(spec) => eval_pgrep(spec, facts),
        Sel::Killall(spec) => eval_killall(spec, facts),
        Sel::Group(group) => Ok(eval_group(*group, facts)),
    };
    let reach = match reach {
        Ok(reach) => reach,
        Err(why) => {
            return Verdict::Ask(format!(
                "GitPulse could not tell which processes `{}` would signal: {why}.",
                finding.text
            ))
        }
    };
    let t = tally(&reach.hits, &facts.rows);
    let over = finding.bounded && t.total > MAX_MATCHES;
    if t.agents > 0 || t.gitpulse > 0 || t.sessions > 0 || over {
        let cause = match &finding.sel {
            Sel::Pgrep(spec) => describe_pgrep(spec),
            Sel::Killall(spec) => describe_killall(spec),
            Sel::Group(g) => format!("process group {g} has these members."),
            _ => String::new(),
        };
        let advice = match &finding.sel {
            Sel::Pgrep(PgrepSpec {
                corrected: Some(fixed),
                ..
            })
            | Sel::Killall(KillallSpec {
                corrected: Some(fixed),
                ..
            }) => format!(
                "Put the options before the pattern, which is what was meant: `{fixed}`. Check \
                 what that matches first with `pgrep` and the same arguments."
            ),
            _ => NARROWER.to_string(),
        };
        return Verdict::Deny(deny_reason(finding, &cause, Some(&t), &advice));
    }
    if finding.elevated {
        return Verdict::Ask(format!(
            "`{}` runs as root, which can signal processes GitPulse cannot see; it found {} of \
             your own and none of them an agent session or GitPulse.",
            finding.text,
            plural(t.total, "process", "processes")
        ));
    }
    let mut notes = Vec::new();
    if reach.unread > 0 {
        notes.push(format!(
            "GitPulse could not read the arguments of {} of your processes, so `{}` was not \
             tested against them.",
            reach.unread, finding.text
        ));
    }
    Verdict::Pass(notes)
}

fn refusal(reason: String, system: String) -> HookOutput {
    HookOutput {
        hook_specific_output: Some(decision(PRE_TOOL_USE, PermissionDecision::Deny, reason)),
        system_message: Some(system),
    }
}

fn question(reason: String) -> HookOutput {
    HookOutput {
        hook_specific_output: Some(decision(
            PRE_TOOL_USE,
            PermissionDecision::Ask,
            reason.clone(),
        )),
        system_message: Some(reason),
    }
}

/// The answer for `plan` over `facts`. Pure.
pub(crate) fn decide(plan: &Plan, facts: &Facts) -> HookOutput {
    let verdicts: Vec<Verdict> = plan.findings.iter().map(|f| judge(f, facts)).collect();
    finish(plan, verdicts)
}

fn finish(plan: &Plan, verdicts: Vec<Verdict>) -> HookOutput {
    let denials: Vec<&String> = verdicts
        .iter()
        .filter_map(|v| match v {
            Verdict::Deny(r) => Some(r),
            _ => None,
        })
        .collect();
    if !denials.is_empty() {
        let mut reason = denials
            .iter()
            .take(MAX_DENIALS_SHOWN)
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if denials.len() > MAX_DENIALS_SHOWN {
            reason.push_str(&format!(
                "\n\n… and {} more refused invocations.",
                denials.len() - MAX_DENIALS_SHOWN
            ));
        }
        let system = format!(
            "GitPulse refused a command whose signals would reach too many of your processes: {}",
            reason.lines().next().unwrap_or_default()
        );
        return refusal(reason, system);
    }
    let mut questions: Vec<String> = verdicts
        .iter()
        .filter_map(|v| match v {
            Verdict::Ask(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    if let Some(why) = &plan.unparsed {
        questions.push(format!(
            "GitPulse could not read this command well enough to tell what it signals ({why}), \
             and it mentions `kill`."
        ));
    }
    if !questions.is_empty() {
        questions.truncate(MAX_DENIALS_SHOWN);
        return question(format!("{} Confirm before it runs.", questions.join(" ")));
    }
    let notes: Vec<String> = verdicts
        .into_iter()
        .flat_map(|v| match v {
            Verdict::Pass(n) => n,
            _ => Vec::new(),
        })
        .collect();
    if notes.is_empty() {
        HookOutput::silent()
    } else {
        HookOutput::notice(notes.join("\n"))
    }
}

/// The answer for a plan: gathers what it needs, then decides.
fn answer(plan: &Plan) -> HookOutput {
    if !plan.needs_table() {
        return decide(plan, &Facts::default());
    }
    match gather(plan) {
        Ok(facts) => decide(plan, &facts),
        Err(why) => unread(plan, &why),
    }
}

/// Plans, gathers and decides one command line, with no deadline of its own.
pub fn check_command(command: &str) -> HookOutput {
    let plan = plan(command);
    if plan.is_empty() {
        return HookOutput::silent();
    }
    answer(&plan)
}

/// The answer when the table could not be read: a question, because a signal
/// command that could not be checked is not one that checked clean. An
/// invocation that needs no table is still judged.
fn unread(plan: &Plan, why: &str) -> HookOutput {
    let mut verdicts: Vec<Verdict> = plan
        .findings
        .iter()
        .filter(|f| matches!(f.sel, Sel::Special(_) | Sel::Unevaluable(_)))
        .map(|f| judge(f, &Facts::default()))
        .collect();
    verdicts.push(Verdict::Ask(format!(
        "GitPulse could not read the process table ({why}), so it could not tell which \
         processes this command would signal."
    )));
    finish(plan, verdicts)
}

/// The signal guard for one hook call, under one deadline.
pub(crate) fn run(input: &HookInput) -> HookOutput {
    if input.command.is_empty() {
        return HookOutput::silent();
    }
    let plan = plan(&input.command);
    if plan.is_empty() {
        return HookOutput::silent();
    }
    if !plan.needs_table() {
        return answer(&plan);
    }
    let job_plan = plan.clone();
    super::within_budget(GUARD_BUDGET, move || answer(&job_plan)).unwrap_or_else(|| {
        unread(
            &plan,
            &format!("it did not finish within {}s", GUARD_BUDGET.as_secs()),
        )
    })
}

/// Whether `output` refuses the call.
pub(crate) fn refuses(output: &HookOutput) -> bool {
    output
        .hook_specific_output
        .as_ref()
        .is_some_and(|s| s.permission_decision == Some(PermissionDecision::Deny))
}

/// One answer from the guard and the harness gate. A refusal from either wins,
/// then the guard's question; both notices are kept, and so is what each told
/// the model — two waived refusals in bypass mode are both context, and the
/// decision chosen here must not drop the other side's.
pub(crate) fn combine(guard: HookOutput, gate: HookOutput) -> HookOutput {
    if guard.is_silent() {
        return gate;
    }
    let joined = |a: Option<String>, b: Option<String>| match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
        (a, b) => a.or(b),
    };
    let message = joined(guard.system_message.clone(), gate.system_message.clone());
    let context = |output: &HookOutput| {
        output
            .hook_specific_output
            .as_ref()
            .and_then(|specific| specific.additional_context.clone())
    };
    let context = joined(context(&guard), context(&gate));
    let mut chosen = if refuses(&gate) || guard.hook_specific_output.is_none() {
        gate
    } else {
        guard
    };
    chosen.system_message = message;
    if let Some(specific) = chosen.hook_specific_output.as_mut() {
        specific.additional_context = context;
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::procscan::AgentKind;

    const HOME: &str = "/Users/bharath";
    const ME: u32 = 501;

    fn fake_env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some(HOME.to_string()),
            "USER" | "LOGNAME" => Some("bharath".to_string()),
            _ => None,
        }
    }

    fn row(pid: i32, ppid: i32, argv: &[&str]) -> TableRow {
        TableRow {
            pid,
            ppid,
            uid: ME,
            start_us: u64::try_from(pid).unwrap_or(0) * 1000,
            name: base_name(argv[0]).to_string(),
            exec: Some(argv[0].to_string()),
            argv: Some(argv.iter().map(|a| (*a).to_string()).collect()),
            agent: None,
            hosted: false,
        }
    }

    fn agent(pid: i32, ppid: i32, hosted: bool) -> TableRow {
        let mut r = row(
            pid,
            ppid,
            &[
                "/Users/bharath/.local/bin/claude",
                "--settings",
                "/Users/bharath/.claude/settings.json",
            ],
        );
        r.agent = Some(AgentKind::Claude);
        r.hosted = hosted;
        r
    }

    /// The machine of the incident, small: the sender's own session and shell
    /// (this hook's ancestors), five hosted sessions whose argv holds the home
    /// directory, eight `sleep 60`s, thirty node servers, the GitPulse
    /// processes, and root's.
    fn world() -> Facts {
        let mut rows = vec![
            row(1, 0, &["/sbin/launchd"]),
            agent(700, 1, true),
            row(800, 700, &["/bin/zsh", "-c", "run"]),
            row(900, 800, &["/usr/local/bin/gitpulse-hook", "command-gate"]),
        ];
        rows[0].uid = 0;
        for n in 0..5 {
            rows.push(agent(1000 + n, 1, true));
        }
        for n in 0..8 {
            rows.push(row(2000 + n, 800, &["sleep", "60"]));
        }
        for n in 0..30 {
            rows.push(row(
                3000 + n,
                1,
                &[
                    "node",
                    "/Users/bharath/Code/app/server.js",
                    "--port",
                    "8080",
                ],
            ));
        }
        let data = "/Users/bharath/Library/Application Support/GitPulse";
        rows.push(row(4000, 1, &["/usr/local/bin/gitpulsed", "--data", data]));
        rows.push(row(
            4001,
            1,
            &["/Applications/GitPulse.app/Contents/MacOS/gitpulse", data],
        ));
        rows.push(row(
            4002,
            1,
            &["/usr/local/bin/gitpulse-mcp", "--data", data],
        ));
        let mut logd = row(5000, 1, &["/usr/libexec/logd"]);
        logd.uid = 0;
        rows.push(logd);
        let mut root_home = row(5001, 1, &["/usr/sbin/mdworker", "/Users/bharath/Library"]);
        root_home.uid = 0;
        rows.push(root_home);
        let mut facts = Facts {
            rows,
            me_uid: ME,
            self_pid: 900,
            ..Facts::default()
        };
        facts.users.insert("bharath".into(), ME);
        facts.users.insert("root".into(), 0);
        facts
    }

    /// A machine with nothing but the sender's own session and a few sleeps.
    fn quiet() -> Facts {
        let mut facts = world();
        facts
            .rows
            .retain(|r| r.pid < 1000 || (2000..2008).contains(&r.pid));
        facts
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Out {
        Deny,
        Ask,
        Notice,
        Silent,
    }

    fn classify_output(output: &HookOutput) -> Out {
        match output
            .hook_specific_output
            .as_ref()
            .and_then(|s| s.permission_decision.as_ref())
        {
            Some(PermissionDecision::Deny) => Out::Deny,
            Some(PermissionDecision::Ask) => Out::Ask,
            None if output.system_message.is_some() => Out::Notice,
            None => Out::Silent,
        }
    }

    fn run_in(command: &str, facts: &Facts) -> HookOutput {
        decide(&plan_with(command, fake_env), facts)
    }

    fn reason_of(output: &HookOutput) -> String {
        output
            .hook_specific_output
            .as_ref()
            .and_then(|s| s.permission_decision_reason.clone())
            .unwrap_or_default()
    }

    fn pids_of(command: &str, facts: &Facts) -> Vec<i32> {
        let plan = plan_with(command, fake_env);
        let Sel::Pgrep(spec) = &plan.findings[0].sel else {
            panic!("not a pattern kill: {plan:?}");
        };
        let mut pids: Vec<i32> = eval_pgrep(spec, facts)
            .expect("evaluates")
            .hits
            .iter()
            .map(|r| r.pid)
            .collect();
        pids.sort_unstable();
        pids
    }

    fn words(args: &[&str]) -> Vec<Word> {
        args.iter()
            .map(|a| Word {
                src: (*a).into(),
                text: (*a).into(),
                ..Word::default()
            })
            .collect()
    }

    /* ── the incident, and its corrected form ── */

    #[test]
    fn the_incident_command_is_denied_with_the_patterns_the_counts_and_the_fix() {
        let output = run_in(r#"pkill -f "sleep 60" -u bharath"#, &world());
        assert_eq!(classify_output(&output), Out::Deny);
        let reason = reason_of(&output);
        assert!(
            reason.contains(r#"["sleep 60", "-u", "bharath"]"#),
            "names the patterns pkill parsed: {reason}"
        );
        assert!(reason.contains("full command line (-f)"), "{reason}");
        assert!(
            reason.contains("5 hosted by GitPulse"),
            "counts the hosted sessions it would hit: {reason}"
        );
        assert!(reason.contains("3 GitPulse processes"), "{reason}");
        assert!(
            reason.contains(r#"pkill -f -u bharath "sleep 60""#),
            "carries the corrected command: {reason}"
        );
        assert!(
            output.system_message.is_some(),
            "the person is told too, not only the model"
        );
    }

    #[test]
    fn the_corrected_form_matching_only_sleeps_is_left_to_the_users_rules() {
        let output = run_in(r#"pkill -u bharath -f "sleep 60""#, &world());
        assert_eq!(classify_output(&output), Out::Silent, "{output:?}");
    }

    #[test]
    fn the_session_that_ran_it_is_not_a_victim_but_the_others_are() {
        let facts = world();
        let pids = pids_of("pkill -f claude", &facts);
        assert!(!pids.contains(&700), "the sender's own session is excluded");
        assert_eq!(pids.len(), 5, "the five hosted ones are not: {pids:?}");
        let alone = quiet();
        assert_eq!(
            classify_output(&run_in("pkill -f claude", &alone)),
            Out::Silent,
            "only an ancestor matches, and pkill never signals its ancestors"
        );
        assert_eq!(
            classify_output(&run_in("pkill -a -f claude", &alone)),
            Out::Deny,
            "-a includes ancestors"
        );
    }

    /// The terminal host and every session it (or the app) hosts are in the
    /// protected set: a pattern kill that reaches a hosted shell is refused,
    /// as one that reaches a hosted agent always was. A job under that shell
    /// is not a session, and stays the user's to kill.
    #[test]
    fn a_terminal_session_gitpulse_hosts_is_protected_but_the_jobs_under_it_are_not() {
        let mut facts = world();
        facts.rows.push(row(
            4100,
            1,
            &[
                "/Applications/GitPulse.app/Contents/MacOS/gitpulse-ptyd",
                "--dir",
                "/x",
            ],
        ));
        let mut hosted_shell = row(4101, 4100, &["/bin/zsh", "-l"]);
        hosted_shell.hosted = true;
        facts.rows.push(hosted_shell);
        let mut job = row(
            4102,
            4101,
            &["node", "/Users/bharath/Code/web/dev-server.js"],
        );
        job.hosted = true;
        facts.rows.push(job);
        // A session the app holds itself, with no terminal host.
        let mut in_app = row(4103, 4001, &["/opt/homebrew/bin/fish"]);
        in_app.hosted = true;
        facts.rows.push(in_app);
        for (command, want) in [
            ("pkill -f gitpulse-ptyd", Out::Deny),
            ("pkill -x zsh", Out::Deny),
            ("pkill fish", Out::Deny),
            ("pkill -f dev-server", Out::Silent),
        ] {
            assert_eq!(classify_output(&run_in(command, &facts)), want, "{command}");
        }
        let reason = reason_of(&run_in("pkill -x zsh", &facts));
        assert!(
            reason.contains("1 terminal session GitPulse hosts"),
            "{reason}"
        );
    }

    /* ── the decision table ── */

    #[test]
    fn the_decision_table() {
        use Out::{Ask, Deny, Silent};
        let table: &[(&str, Out)] = &[
            // The home directory as a pattern, however it is spelled.
            ("pkill -f /Users/bharath", Deny),
            ("pkill -f $HOME", Deny),
            ("pkill -f \"$HOME/Code\"", Deny),
            ("pkill -f ~/Code", Deny),
            ("pkill -f ${HOME}", Deny),
            ("pkill -f bharath", Deny),
            // Options after operands become patterns.
            // `-f` after the operand is a pattern against names, which no
            // process here has in its name: harmless in this table, and the
            // same shape as the incident where it is not.
            ("pkill sleep -f", Silent),
            ("pkill -x claude -f", Deny),
            ("pkill -f sleep -u bharath", Deny),
            ("pkill -f sleep -u $USER", Deny),
            ("pkill -9 -f \"sleep 60\" -u bharath", Deny),
            ("pkill -TERM -f \"sleep 60\" -u bharath", Deny),
            ("pkill -SIGKILL -f \"sleep 60\" -u bharath", Deny),
            ("pkill -15 sleep -x", Silent),
            ("pkill -15 -f sleep -u $HOME", Deny),
            // The same arguments in the right order.
            ("pkill -f \"sleep 60\"", Silent),
            ("pkill -x sleep", Silent),
            ("pkill sleep", Silent),
            ("pkill -9 -f \"sleep 60\"", Silent),
            ("pkill -U 501 -f \"sleep 60\"", Silent),
            ("pkill -i SLEEP", Silent),
            ("pkill -f -- \"sleep 60\"", Silent),
            ("pkill -fx \"sleep 60\"", Silent),
            // Agents, GitPulse and the bound.
            ("pkill -x claude", Deny),
            ("pkill claude", Deny),
            ("pkill gitpulsed", Deny),
            ("pkill -f gitpulse-mcp", Deny),
            ("pkill -f GitPulse.app", Deny),
            ("pkill -x node", Deny),
            ("pkill -u bharath", Deny),
            ("pkill -v -f xyzzy", Deny),
            ("pkill -P 1 -f server.js", Deny),
            // Only this user's processes can be signalled.
            ("pkill -f logd", Silent),
            ("pkill -f mdworker", Silent),
            ("pkill -u root -f \"sleep 60\"", Silent),
            // -n and -o select one.
            ("pkill -n -f \"sleep 60\"", Silent),
            ("pkill -o -f server.js", Silent),
            ("pkill -n -f claude", Deny),
            // Signal 0 and usage errors send nothing.
            ("pkill -0 -f bharath", Silent),
            ("pkill", Silent),
            ("pkill -f", Silent),
            ("pkill -f ''", Silent),
            ("pkill -Z foo", Silent),
            ("pgrep -f bharath", Silent),
            // Whole-machine kills.
            ("kill -- -1", Deny),
            ("kill -TERM 0", Deny),
            ("kill -9 -1", Deny),
            ("kill -1", Deny),
            ("kill 0", Deny),
            ("kill -s KILL -1", Deny),
            ("kill -KILL -- -1", Deny),
            ("/bin/kill -9 -1", Deny),
            ("kill -0 -1", Silent),
            ("kill -s 0 -1", Silent),
            ("kill -l", Silent),
            ("kill -l 9", Silent),
            // Explicit pids are the user's rules to judge.
            ("kill 1234", Silent),
            ("kill -9 1234 1235", Silent),
            ("kill -TERM 1234", Silent),
            ("kill %1", Silent),
            ("kill $pid", Silent),
            ("kill -9 $!", Silent),
            ("kill", Silent),
            // killall.
            ("killall node", Deny),
            ("killall claude", Deny),
            ("killall -u bharath", Deny),
            ("killall -m .", Deny),
            ("killall -m 'sl.*'", Silent),
            ("killall sleep", Silent),
            ("killall -TERM sleep", Silent),
            ("killall -9 sleep", Silent),
            ("killall -s node", Silent),
            ("killall -d node", Silent),
            ("killall -l", Silent),
            ("killall -0 node", Silent),
            ("killall", Silent),
            ("killall sleep -u bharath", Silent),
            ("killall -m sl -u bharath", Silent),
            ("killall -m . -u bharath", Deny),
            ("killall -u nobodyatall sleep", Ask),
            ("killall -t ttys001 sleep", Ask),
            // Command substitution, xargs, pipes.
            ("kill $(pgrep -f \"sleep 60\")", Silent),
            ("kill $(pgrep -f bharath)", Deny),
            ("kill $(pgrep -f \"sleep 60\" -u bharath)", Deny),
            ("kill -9 `pgrep -f bharath`", Deny),
            ("kill \"$(pgrep -f bharath)\"", Deny),
            ("kill $(pgrep node)", Deny),
            ("kill $(pgrep sleep)", Silent),
            ("kill $(pidof node)", Deny),
            ("pgrep -f \"sleep 60\" | xargs kill", Silent),
            ("pgrep -f bharath | xargs kill", Deny),
            ("pgrep -f bharath | xargs kill -9", Deny),
            ("pgrep -f bharath | xargs -n1 kill", Deny),
            ("pgrep -f bharath | xargs -I{} kill {}", Deny),
            ("pgrep -f bharath | xargs -0 kill", Deny),
            ("pgrep -f bharath | xargs -r -n 1 kill -TERM", Deny),
            ("pgrep node | grep -v 3000 | xargs kill", Silent),
            ("pgrep -f claude | grep -v 700 | xargs kill", Deny),
            ("pgrep -f \"sleep 60\" | head -1 | xargs kill", Silent),
            ("lsof -ti :3000 | xargs kill", Silent),
            ("kill $(lsof -ti :3000)", Silent),
            ("cat pids.txt | xargs kill", Silent),
            ("xargs kill < pids.txt", Silent),
            ("ps aux | grep foo | awk '{print $2}' | xargs kill", Ask),
            ("kill $(ps -ax | awk '{print $1}')", Ask),
            ("kill $(some-tool --list)", Ask),
            ("kill -- -$pgid", Ask),
            ("echo 1234 | xargs kill", Silent),
            ("echo foo | xargs pkill -f", Ask),
            // Prefixes and wrappers.
            ("sudo pkill -f bharath", Deny),
            ("sudo -n pkill -f bharath", Deny),
            ("sudo -u bharath pkill -f bharath", Deny),
            ("sudo pkill -f \"sleep 60\"", Ask),
            ("env FOO=1 pkill -f bharath", Deny),
            ("env -i pkill -f bharath", Deny),
            ("command pkill -f bharath", Deny),
            ("exec pkill -f bharath", Deny),
            ("nohup pkill -f bharath", Deny),
            ("nice -n 5 pkill -f bharath", Deny),
            ("timeout 5 pkill -f bharath", Deny),
            ("FOO=1 pkill -f bharath", Deny),
            ("/usr/bin/pkill -f bharath", Deny),
            ("\\pkill -f bharath", Deny),
            ("time pkill -f bharath", Deny),
            ("arch -arm64 pkill -f bharath", Deny),
            ("command -v pkill", Silent),
            // Separators, grouping, nesting.
            ("echo hi; pkill -f bharath", Deny),
            ("true && pkill -f bharath", Deny),
            ("false || pkill -f bharath", Deny),
            ("echo a | pkill -f bharath", Deny),
            ("sleep 1 & pkill -f bharath", Deny),
            ("(pkill -f bharath)", Deny),
            ("{ pkill -f bharath; }", Deny),
            ("if true; then pkill -f bharath; fi", Deny),
            ("for i in 1 2; do pkill -f bharath; done", Deny),
            ("echo $(pkill -f bharath)", Deny),
            ("echo `pkill -f bharath`", Deny),
            ("echo \"$(pkill -f bharath)\"", Deny),
            ("bash -c 'pkill -f bharath'", Deny),
            ("sh -c \"pkill -f bharath\"", Deny),
            ("zsh -lc 'pkill -f bharath'", Deny),
            ("bash -c 'bash -c \"pkill -f bharath\"'", Deny),
            ("eval \"pkill -f bharath\"", Deny),
            ("eval pkill -f bharath", Deny),
            ("xargs sh -c 'pkill -f bharath'", Deny),
            ("bash <<EOF\npkill -f bharath\nEOF", Deny),
            ("bash <<'EOF'\npkill -f bharath\nEOF", Deny),
            ("sh <<< 'pkill -f bharath'", Deny),
            ("cat <<EOF\n$(pkill -f bharath)\nEOF", Deny),
            ("pkill -f \"sleep 60\" 2>/dev/null", Silent),
            ("pkill -f bharath 2>&1 | tee log", Deny),
            ("pkill -f bharath >/dev/null 2>&1 || true", Deny),
            ("pkill -f bharath &>/dev/null", Deny),
            ("diff <(pkill -f bharath) x", Deny),
            // Text that mentions signals is not a signal.
            ("echo \"pkill -f bharath\"", Silent),
            ("cat <<'EOF'\npkill -f bharath\nEOF", Silent),
            ("grep -r pkill src", Silent),
            ("git commit -m 'fix: pkill -f bharath'", Silent),
            ("echo kill -9 -1", Silent),
            ("ls", Silent),
            ("", Silent),
            ("   \n  ", Silent),
            ("# pkill -f bharath", Silent),
            ("echo hi # pkill -f bharath", Silent),
            // Could not be evaluated: asked, never silent and never allowed.
            ("pkill -f \"$PATTERN\"", Ask),
            ("pkill -f \"foo$(date)\"", Ask),
            ("pkill -u $WHO -f x", Ask),
            ("pkill -t ttys001 foo", Ask),
            ("pkill -G staff foo", Ask),
            ("pkill -g 0 foo", Ask),
            ("pkill -f '('", Ask),
            ("pkill -u nobodyatall foo", Ask),
            ("pkill -F /nonexistent/pidfile", Ask),
            ("pkill -f \"unterminated", Ask),
            ("kill $(pgrep -f \"unterminated", Ask),
            ("pkill -f 'a' $(", Ask),
            ("sudo pkill -x claude", Deny),
        ];
        let facts = world();
        let mut wrong = Vec::new();
        for (command, want) in table {
            let got = classify_output(&decide(&plan_with(command, fake_env), &facts));
            if got != *want {
                wrong.push(format!("{command:?}: wanted {want:?}, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    #[test]
    fn a_group_kill_is_judged_by_the_groups_members() {
        let mut facts = world();
        for r in &facts.rows {
            // Group 4242 holds the hosted sessions; group 4343 the sleeps.
            if (1000..1005).contains(&r.pid) {
                facts.pgids.insert(r.pid, 4242);
            }
            if (2000..2008).contains(&r.pid) {
                facts.pgids.insert(r.pid, 4343);
            }
        }
        assert_eq!(classify_output(&run_in("kill -- -4242", &facts)), Out::Deny);
        assert_eq!(
            classify_output(&run_in("kill -TERM -4343", &facts)),
            Out::Silent
        );
        assert_eq!(
            classify_output(&run_in("kill -9 -999", &facts)),
            Out::Silent,
            "an empty group reaches nothing"
        );
    }

    #[test]
    fn a_pidfile_restricts_the_match_to_its_pid() {
        let mut facts = world();
        facts.pidfiles.insert("/tmp/x.pid".into(), Ok(2003));
        assert_eq!(
            classify_output(&run_in("pkill -F /tmp/x.pid", &facts)),
            Out::Silent
        );
        facts.pidfiles.insert("/tmp/x.pid".into(), Ok(1000));
        assert_eq!(
            classify_output(&run_in("pkill -F /tmp/x.pid", &facts)),
            Out::Deny,
            "a pidfile naming a hosted agent session"
        );
        facts
            .pidfiles
            .insert("/tmp/x.pid".into(), Err("cannot read".into()));
        assert_eq!(
            classify_output(&run_in("pkill -F /tmp/x.pid", &facts)),
            Out::Ask
        );
    }

    /* ── the reasons ── */

    #[test]
    fn a_killall_with_a_late_option_gets_its_corrected_form() {
        let output = run_in("killall -m . -u bharath", &world());
        assert_eq!(classify_output(&output), Out::Deny);
        let reason = reason_of(&output);
        assert!(reason.contains("killall -m -u bharath ."), "{reason}");
    }

    #[test]
    fn a_deny_for_the_bound_names_the_bound() {
        let reason = reason_of(&run_in("pkill -x node", &world()));
        assert!(reason.contains("30 of your processes"), "{reason}");
        assert!(reason.contains(&MAX_MATCHES.to_string()), "{reason}");
        assert!(reason.contains("Including:"), "{reason}");
    }

    #[test]
    fn a_question_says_it_could_not_evaluate_and_a_deny_is_not_a_question() {
        let output = run_in("pkill -f \"$PATTERN\"", &world());
        assert_eq!(classify_output(&output), Out::Ask);
        let reason = reason_of(&output);
        assert!(
            reason.contains("could not tell which processes"),
            "{reason}"
        );
        assert_eq!(output.system_message.as_deref(), Some(reason.as_str()));
    }

    #[test]
    fn a_process_table_that_could_not_be_read_is_asked_about_never_passed() {
        let plan = plan_with("pkill -f anything", fake_env);
        let output = unread(&plan, "the kernel refused");
        assert_eq!(classify_output(&output), Out::Ask);
        assert!(reason_of(&output).contains("the kernel refused"));
        assert!(output.system_message.is_some());
    }

    #[test]
    fn a_table_that_could_not_be_read_still_refuses_what_needs_no_table() {
        let plan = plan_with("kill -- -1; pkill -f anything", fake_env);
        let output = unread(&plan, "boom");
        assert_eq!(classify_output(&output), Out::Deny);
    }

    #[test]
    fn a_command_that_could_not_be_parsed_and_mentions_kill_is_asked_about() {
        let output = run_in("pkill -f 'oops", &world());
        assert_eq!(classify_output(&output), Out::Ask);
        assert!(reason_of(&output).contains("unterminated single quote"));
        assert_eq!(
            classify_output(&run_in("echo 'oops", &world())),
            Out::Silent,
            "no signal word, nothing to ask about"
        );
    }

    #[test]
    fn arguments_that_could_not_be_read_are_a_notice_not_a_clean_pass() {
        let mut facts = quiet();
        facts.rows.push({
            let mut r = row(2100, 1, &["sleep", "99"]);
            r.argv = None;
            r
        });
        let output = run_in("pkill -f \"sleep 60\"", &facts);
        assert_eq!(classify_output(&output), Out::Notice, "{output:?}");
        assert!(output
            .system_message
            .unwrap_or_default()
            .contains("could not read the arguments of 1"));
    }

    #[test]
    fn nothing_the_guard_says_is_ever_an_allow() {
        let facts = world();
        for command in [
            "pkill -f bharath",
            "pkill -f \"sleep 60\"",
            "kill -- -1",
            "kill 1",
            "pkill -f \"$X\"",
            "pkill -f 'oops",
            "killall node",
            "ls",
        ] {
            let output = run_in(command, &facts);
            let json = output.render().unwrap_or_default();
            assert!(!json.contains("\"allow\""), "{command}: {json}");
            assert!(!json.contains("\"defer\""), "{command}: {json}");
        }
    }

    #[test]
    fn root_through_sudo_is_asked_about_when_nothing_visible_matches() {
        let output = run_in("sudo pkill -f \"sleep 60\"", &world());
        assert_eq!(classify_output(&output), Out::Ask);
        assert!(reason_of(&output).contains("runs as root"));
    }

    #[test]
    fn combine_keeps_a_refusal_then_a_question_and_both_notices() {
        let deny = refusal("no".into(), "guard says no".into());
        let ask = question("sure?".into());
        let gate_deny = HookOutput {
            hook_specific_output: Some(decision(
                PRE_TOOL_USE,
                PermissionDecision::Deny,
                "scope".into(),
            )),
            system_message: Some("gate says no".into()),
        };
        let gate_notice = HookOutput::notice("ungated");
        assert!(refuses(&combine(deny, gate_notice.clone())));
        assert_eq!(
            classify_output(&combine(ask.clone(), gate_notice.clone())),
            Out::Ask
        );
        let both = combine(ask, gate_deny);
        assert!(refuses(&both));
        assert!(both.system_message.unwrap_or_default().contains("sure?"));
        assert_eq!(
            combine(HookOutput::silent(), gate_notice.clone()).system_message,
            gate_notice.system_message,
            "a quiet guard changes nothing"
        );
        assert!(combine(HookOutput::silent(), HookOutput::silent()).is_silent());
        let notice_only = combine(HookOutput::notice("note"), gate_notice);
        assert_eq!(classify_output(&notice_only), Out::Notice);
        assert!(notice_only
            .system_message
            .unwrap_or_default()
            .contains("ungated"));
    }

    /* ── reading the command line ── */

    fn words_of(command: &str) -> Vec<String> {
        let mut budget = Budget { commands: 0 };
        let script = parse_script(command, 0, &mut budget, fake_env).expect("parses");
        script[0][0].words.iter().map(|w| w.text.clone()).collect()
    }

    #[test]
    fn quoting_resolves_the_way_the_shell_does() {
        for (command, want) in [
            (r#"pkill -f "sleep 60""#, vec!["pkill", "-f", "sleep 60"]),
            ("pkill -f 'sleep 60'", vec!["pkill", "-f", "sleep 60"]),
            (r"pkill -f sleep\ 60", vec!["pkill", "-f", "sleep 60"]),
            (r#"pkill -f "a \"b\" c""#, vec!["pkill", "-f", r#"a "b" c"#]),
            ("pkill -f 'a'\"b\"c", vec!["pkill", "-f", "abc"]),
            ("pkill -f $'a b'", vec!["pkill", "-f", "a b"]),
            (
                r#"pkill -f "$HOME/x""#,
                vec!["pkill", "-f", "/Users/bharath/x"],
            ),
            ("pkill -f ~/x", vec!["pkill", "-f", "/Users/bharath/x"]),
            ("pkill -f '$HOME'", vec!["pkill", "-f", "$HOME"]),
            (r"pkill -f \$HOME", vec!["pkill", "-f", "$HOME"]),
            ("pkill -f 'é ü 日本'", vec!["pkill", "-f", "é ü 日本"]),
            ("a  \t b", vec!["a", "b"]),
            ("a \\\n b", vec!["a", "b"]),
        ] {
            assert_eq!(words_of(command), want, "{command:?}");
        }
    }

    #[test]
    fn a_variable_that_cannot_be_expanded_makes_the_word_dynamic() {
        let mut budget = Budget { commands: 0 };
        let script =
            parse_script("pkill -f \"$X/a\" '$Y' ${Z}", 0, &mut budget, fake_env).expect("parses");
        let dynamic: Vec<bool> = script[0][0].words.iter().map(|w| w.dynamic).collect();
        assert_eq!(dynamic, vec![false, false, true, false, true]);
    }

    #[test]
    fn heredoc_bodies_are_read_not_mistaken_for_commands() {
        let plan = plan_with("cat <<EOF\npkill -f bharath\nEOF\nls", fake_env);
        assert!(plan.is_empty(), "{plan:?}");
        let plan = plan_with("cat <<-EOF\n\tx\n\tEOF\npkill -f bharath", fake_env);
        assert_eq!(
            plan.findings.len(),
            1,
            "a command after the heredoc is read"
        );
        let plan = plan_with("cat <<A <<B\none\nA\ntwo\nB\npkill -f bharath", fake_env);
        assert_eq!(plan.findings.len(), 1);
    }

    #[test]
    fn a_quoted_heredoc_delimiter_runs_no_substitution() {
        assert!(plan_with("cat <<'EOF'\n$(pkill -f bharath)\nEOF", fake_env).is_empty());
        assert_eq!(
            plan_with("cat <<EOF\n$(pkill -f bharath)\nEOF", fake_env)
                .findings
                .len(),
            1
        );
    }

    /* ── adversarial input ── */

    #[test]
    fn adversarial_lines_are_answered_without_panicking_and_in_time() {
        let huge_word = "a".repeat(300_000);
        let deep = format!("{}pkill -f bharath{}", "$(".repeat(40), ")".repeat(40));
        let many_quotes = "'\"".repeat(5_000);
        let long_pipeline = vec!["echo"; 10_000].join(" | ");
        let long_chain = vec!["pkill -f \"sleep 60\""; 10_000].join("; ");
        let big_pipeline_xargs = format!("{} | xargs kill", vec!["echo 1"; 10_000].join(" | "));
        let cases: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("blank", " \t\n ".into()),
            ("nul", "pkill\0 -f bharath".into()),
            ("only nul", "\0\0\0".into()),
            ("lone quote", "'".into()),
            ("lone dquote", "\"".into()),
            ("lone dollar", "$".into()),
            ("lone paren", "$(".into()),
            ("lone backtick", "`".into()),
            ("lone backslash", "\\".into()),
            ("lone pipe", "|".into()),
            ("operators", ";&|&&||();;&".into()),
            ("redirect only", ">".into()),
            ("heredoc only", "<<".into()),
            ("heredoc no delim", "cat <<\n".into()),
            ("unicode", "pkill -f 'héllo 日本語 \u{1f600}'".into()),
            ("rtl", "pkill -f '\u{202e}evil'".into()),
            ("huge word", huge_word),
            ("deep nesting", deep),
            ("many quotes", many_quotes),
            ("10k pipeline", long_pipeline),
            ("10k chain", long_chain),
            ("10k pipeline into xargs", big_pipeline_xargs),
            ("kill and unterminated", "kill $(pgrep".into()),
            ("number overflow", "kill -- -99999999999999999999".into()),
            ("negative min", "kill -2147483648".into()),
            ("control chars", "pkill \u{1}\u{2} -f \u{7f}".into()),
        ];
        let facts = world();
        for (label, command) in cases {
            let started = std::time::Instant::now();
            let output = decide(&plan_with(&command, fake_env), &facts);
            let took = started.elapsed();
            assert!(
                took < Duration::from_secs(2),
                "{label}: took {took:?} for {} bytes",
                command.len()
            );
            let json = output.render().unwrap_or_default();
            assert!(!json.contains("\"allow\""), "{label}: {json}");
        }
    }

    #[test]
    fn a_line_too_long_to_read_is_asked_about_only_when_it_says_kill() {
        let long = "a ".repeat(MAX_COMMAND_BYTES);
        assert_eq!(classify_output(&run_in(&long, &world())), Out::Silent);
        let long_kill = format!("{long} pkill");
        assert_eq!(classify_output(&run_in(&long_kill, &world())), Out::Ask);
    }

    #[test]
    fn nesting_past_the_limit_is_asked_about_when_it_says_kill() {
        let deep = format!("{}pkill -f bharath{}", "$(".repeat(40), ")".repeat(40));
        assert_eq!(classify_output(&run_in(&deep, &world())), Out::Ask);
        let within = format!("{}pkill -f bharath{}", "$(".repeat(5), ")".repeat(5));
        assert_eq!(classify_output(&run_in(&within, &world())), Out::Deny);
    }

    #[test]
    fn the_command_count_is_bounded() {
        let many = vec!["kill 1"; MAX_COMMANDS + 10].join("; ");
        assert_eq!(
            classify_output(&run_in(&many, &world())),
            Out::Ask,
            "more commands than the limit, and every one says kill"
        );
    }

    /* ── the pattern engine ── */

    #[test]
    fn name_matching_uses_the_last_component_of_argv0_not_the_kernels_short_name() {
        let mut r = row(
            1,
            0,
            &["/very/long/path/averyveryverylongprocessname_zq", "x"],
        );
        r.name = "averyverylongpr".into();
        assert_eq!(match_name(&r), "averyveryverylongprocessname_zq");
        let mut spaced = row(2, 0, &["sp ace", "x"]);
        spaced.name = "sp ace".into();
        assert_eq!(match_name(&spaced), "sp ace");
        let mut empty = row(3, 0, &["", "x"]);
        empty.name = "fallback".into();
        assert_eq!(match_name(&empty), "fallback");
        let mut unread = row(4, 0, &["x"]);
        unread.argv = None;
        unread.name = "zqshort".into();
        assert_eq!(match_name(&unread), "zqshort");
        assert_eq!(match_full(&unread), "zqshort");
    }

    #[test]
    fn exact_matches_must_cover_the_whole_subject_and_alternation_is_anchored_as_a_whole() {
        let mut facts = quiet();
        facts.rows.push(row(2200, 1, &["foo", "bar"]));
        facts.rows.push(row(2201, 1, &["foobar"]));
        let hit = |c: &str| pids_of(c, &facts);
        assert_eq!(hit("pkill -x foo"), vec![2200]);
        assert_eq!(hit("pkill -x 'foo|foobar'"), vec![2200, 2201]);
        assert_eq!(hit("pkill -fx 'foo bar'"), vec![2200]);
        assert_eq!(hit("pkill -fx foo"), Vec::<i32>::new());
        assert_eq!(hit("pkill -f 'foo bar'"), vec![2200]);
        assert_eq!(hit("pkill -i FOOBAR"), vec![2201]);
        assert_eq!(hit("pkill FOOBAR"), Vec::<i32>::new());
    }

    #[test]
    fn extra_patterns_after_the_first_operand_are_or_ed() {
        let mut facts = quiet();
        facts.rows.push(row(2200, 1, &["tool", "-u", "x"]));
        facts.rows.push(row(2201, 1, &["other", "bharath"]));
        facts.rows.push(row(2202, 1, &["unrelated"]));
        assert_eq!(
            pids_of("pkill -f tool -u bharath", &facts),
            vec![2200, 2201]
        );
        assert_eq!(pids_of("pkill -u bharath -f tool", &facts), vec![2200]);
    }

    #[test]
    fn inverse_and_ancestors_follow_the_manual() {
        let facts = quiet();
        let inverse = pids_of("pkill -v -f zzzz", &facts);
        assert!(inverse.contains(&700), "-v includes ancestors");
        assert!(!inverse.contains(&900), "never itself");
        let normal = pids_of("pkill -f .", &facts);
        assert!(!normal.contains(&700) && !normal.contains(&800) && !normal.contains(&900));
    }

    #[test]
    fn newest_and_oldest_pick_by_start_time() {
        let facts = world();
        assert_eq!(pids_of("pkill -n -f \"sleep 60\"", &facts), vec![2007]);
        assert_eq!(pids_of("pkill -o -f \"sleep 60\"", &facts), vec![2000]);
    }

    #[test]
    fn a_pattern_the_engine_cannot_compile_is_a_question() {
        assert_eq!(
            classify_output(&run_in("pkill -f '[a-'", &world())),
            Out::Ask
        );
        let giant = format!("pkill -f '{}'", "(a|b)".repeat(20_000));
        // Refused for its size, or evaluated: either way an answer, in time.
        let started = std::time::Instant::now();
        let _ = run_in(&giant, &world());
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    }

    #[test]
    fn option_parsing_is_getopt_not_a_permuting_parser() {
        for (args, patterns, late) in [
            (vec!["-f", "a", "-u", "b"], vec!["a", "-u", "b"], vec!["-u"]),
            (vec!["a", "-f"], vec!["a", "-f"], vec!["-f"]),
            (vec!["-fx", "a"], vec!["a"], vec![]),
            (vec!["-ubharath", "a"], vec!["a"], vec![]),
            (vec!["-u", "bharath", "a"], vec!["a"], vec![]),
            (vec!["--", "-a"], vec!["-a"], vec![]),
            (vec!["a", "--", "b"], vec!["a", "--", "b"], vec![]),
            (vec!["-", "a"], vec!["-", "a"], vec![]),
            (vec!["-f", "-", "a"], vec!["-", "a"], vec![]),
        ] {
            let Parsed::Spec(spec) = parse_pgrep(&words(&args), false) else {
                panic!("{args:?} does not parse");
            };
            assert_eq!(spec.patterns, patterns, "{args:?}");
            assert_eq!(spec.late, late, "{args:?}");
        }
    }

    #[test]
    fn pkill_reads_a_signal_only_as_its_first_argument() {
        let Parsed::Spec(first) = parse_pgrep(&words(&["-HUP", "-f", "x"]), true) else {
            panic!("signal first");
        };
        assert_eq!(first.patterns, vec!["x"]);
        assert!(first.full);
        // Later, `-HUP` is an illegal option: pkill refuses and sends nothing.
        assert!(matches!(
            parse_pgrep(&words(&["-f", "-HUP", "x"]), true),
            Parsed::NoEffect(_)
        ));
        // pgrep has no signal argument at all.
        assert!(matches!(
            parse_pgrep(&words(&["-HUP", "x"]), false),
            Parsed::NoEffect(_)
        ));
    }

    #[test]
    fn signal_specs_are_numbers_and_names() {
        for (spec, want) in [
            ("9", Some(false)),
            ("0", Some(true)),
            ("00", Some(true)),
            ("TERM", Some(false)),
            ("term", Some(false)),
            ("SIGKILL", Some(false)),
            ("sigint", Some(false)),
            ("USR1", Some(false)),
            ("f", None),
            ("u", None),
            ("", None),
            ("1234", None),
            ("SIG", None),
            ("EXIT", None),
        ] {
            assert_eq!(signal_spec(spec), want, "{spec:?}");
        }
    }

    #[test]
    fn contains_kill_is_case_blind_and_bounded() {
        assert!(contains_kill("PKILL"));
        assert!(contains_kill("xKiLlx"));
        assert!(!contains_kill("kil"));
        assert!(!contains_kill(""));
    }

    #[test]
    fn gitpulse_processes_are_recognised_by_name_and_by_bundle() {
        for (argv, want) in [
            (vec!["/usr/local/bin/gitpulsed"], true),
            (vec!["/usr/local/bin/gitpulse-mcp"], true),
            (
                vec!["/Applications/GitPulse.app/Contents/MacOS/gitpulse"],
                true,
            ),
            (
                vec!["/Applications/GitPulse.app/Contents/Frameworks/Helper"],
                true,
            ),
            (vec!["GitPulse"], true),
            (vec!["/usr/local/bin/gitpulse-hook"], false),
            (vec!["/usr/bin/git"], false),
            (vec!["sleep"], false),
        ] {
            assert_eq!(is_gitpulse(&row(1, 0, &argv)), want, "{argv:?}");
        }
    }

    /* ── property: nothing panics, everything is bounded ── */

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        const PIECES: [&str; 40] = [
            "pkill", "killall", "kill", "xargs", "pgrep", "sudo", "env", "bash -c", "-f", "-u",
            "-9", "-1", "--", "0", "sleep 60", "bharath", "$(", ")", "`", "'", "\"", "\\", "|",
            ";", "&&", "||", "&", "<<", "EOF", "\n", " ", "$HOME", "$X", "(", "{", "}", ">",
            "2>&1", "#", "é",
        ];

        fn line() -> impl Strategy<Value = String> {
            proptest::collection::vec(0..PIECES.len(), 0..60).prop_map(|ix| {
                ix.into_iter()
                    .map(|i| PIECES[i])
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 3000,
                failure_persistence: None,
                ..ProptestConfig::default()
            })]

            #[test]
            fn random_command_lines_never_panic_and_stay_in_budget(command in line()) {
                let facts = world();
                let started = std::time::Instant::now();
                let output = decide(&plan_with(&command, fake_env), &facts);
                prop_assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
                let json = output.render().unwrap_or_default();
                prop_assert!(!json.contains("\"allow\""));
            }

            #[test]
            fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
                let command = String::from_utf8_lossy(&bytes).into_owned();
                let _ = decide(&plan_with(&command, fake_env), &world());
            }

            #[test]
            fn a_plan_never_holds_more_findings_than_its_bound(command in line()) {
                let plan = plan_with(&command, fake_env);
                prop_assert!(plan.findings.len() <= MAX_FINDINGS);
            }
        }
    }
}
