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
//! * `-U` (real uid) is compared with the real uid where the table has it
//!   (Linux) and with the effective one, always: a set-uid process is counted
//!   when either matches, never fewer than before.
//! * `-G` (group) and `-t` (terminal) are not in the table: those commands are
//!   asked about, not guessed at.
//! * Patterns are evaluated with the `regex` crate and the tools use glibc's
//!   `regcomp` (POSIX extended). A pattern it cannot compile is asked about. On
//!   Linux the guard also asks, after refusing if what it could evaluate already
//!   reaches too much, about a pattern whose syntax the two read differently
//!   (`ere_uncertainty`): a backslash escape that is not of a metacharacter or
//!   a backslash (`\d`, `\w`, `\b`, `\<`, `\n`, `\1`), a backslash in a bracket,
//!   any POSIX class, equivalence class or collating symbol (`[[:alpha:]]`,
//!   `[[=a=]]`, `[[.x.]]`: glibc's are the locale's, so `é`, `α` and `—` are
//!   `[:alpha:]` or `[:punct:]` to it and not to the crate), an invalid `{`.
//!   It also asks when `-i` meets a non-ASCII pattern, or a non-ASCII name or
//!   command line it has not matched (glibc folds `ı` to `I`; the crate does
//!   not), and when a command line holds bytes that are not UTF-8 and the
//!   pattern can match a character it does not spell (`.`, a bracket): procps
//!   counts those bytes one at a time (a surrogate as one) and the guard holds
//!   them replaced, so it cannot reproduce the count. The guard matches `.`
//!   across a newline, and tries every subject in the readings procps-ng 4.0.4
//!   was observed to give it: control characters replaced (newline by a space;
//!   in a UTF-8 locale the rest by `?`, in the C locale by `.`, and every
//!   non-ASCII byte there by `?`), an empty argument between two others kept, a
//!   blanked command line a single `?`, and the first 131071 bytes of a long
//!   one (`Pattern`, `full_subjects`). Not modelled: bracket ranges under a
//!   locale's collation order, locales other than `C` and UTF-8, and procps or
//!   glibc releases other than the ones the differential ran against. macOS
//!   reads patterns as it always did, so there `\d` is a digit here and a
//!   literal `d` to `regcomp`.
//! * A variable that is not `$HOME`, `$USER` or `$LOGNAME` is not expanded in an
//!   argument, so a pattern or user built from one is asked about. A program, an
//!   `eval` argument or a `-c` script is different: where it is built from
//!   variables the line itself assigned (`p=pk''ill; $p`, `${p}ill`,
//!   `$a$b$c`, `export p=…`, an alias, `$'\x70kill'`) it is read as what they
//!   come to (`Binds`, `bound_text`), and a variable assigned twice, from a
//!   command, a loop or a glob is not known. What a subshell `( … )`, a
//!   command substitution, a stage of a pipeline, a command run with `&` or a
//!   new shell (`bash -c`) binds is dropped when it ends, as the shell that ran
//!   it never sees it (`Cx::walk`; the lexer numbers the groups); a pipeline
//!   stage that opens a loop keeps its bindings to the loop's `done`. A
//!   variable set by `read`, `printf -v`, `mapfile`, `getopts`, `let`, `(( ))`
//!   or `declare -n` is not known, and `source FILE` makes everything bound
//!   before it unknown; each of those marks the line when it spells a signal
//!   tool, and none does otherwise. A program that is not known is
//!   asked about only when a signal tool (`kill`, `pkill`, `killall`,
//!   `killall5`, `pgrep`, `pidof`) is a whole word of its command, of a command
//!   on the line that bound something, or (for a script piped into a shell, or
//!   `{}` put in place by `xargs` or `find`) of its pipeline; a path, `skill`,
//!   `--kill-after` and `KILL_AFTER=5` are not (`spelled_tools`). A shell given
//!   `-c SCRIPT` through a variable has the script read. Pids in a variable or
//!   a file a `pgrep` or `pidof` on the line filled (`for p in $(pgrep …)`,
//!   `pgrep … | while read p`, `pids=$(…)`, `mapfile`, `> FILE` then
//!   `$(cat FILE)`) are that command's set; a pid variable the line did not
//!   fill, beside a `pgrep` or `pidof`, is asked about; otherwise `kill $PID`
//!   is left to the user's rules.
//! * Only this user's processes can be signalled, so only they are counted. A
//!   sender that is root reaches every process, so the table reads every
//!   process's arguments (up to 64 MiB of them) and counts them all; one it
//!   could not read is asked about, not passed with a notice.
//! * macOS `pkill` leaves ancestors out; procps `pkill` does not. The guard
//!   follows the tool on the host, so a Linux `pkill` that would reach an
//!   ancestor is counted and can be refused.
//! * The tools differ by host and the guard reads the host's ([`Dialect`]). On
//!   Linux (procps `pgrep`/`pkill`, psmisc `killall`): a name is `comm` (15
//!   bytes) and the guard also tries `argv[0]`'s last component; options are
//!   read anywhere, with long options, abbreviations and a `pkill` signal
//!   anywhere in the line; one pattern only; an option the guard does not model
//!   (`-s`, `-r`, `-g`, a path to `killall`, …) is asked about rather than read
//!   with its macOS meaning. When options follow the first operand the macOS
//!   reading is counted too, so the set is the union of the two, which can
//!   refuse a command Linux would have run narrower. A command line is silent
//!   on Linux where the macOS reading would have counted it only when the real
//!   Linux tool refuses or ignores it and signals nothing: more than one
//!   pattern, no pattern and no criterion, `-h`/`-V` and `killall -l`/`-V`,
//!   `-L` without `-F`, a missing option argument, signal 0. Those classes do
//!   not consult the macOS reading. They were checked against the real
//!   tools in a differential over 3849 command lines (`tests/adv_w4a_signal_guard.rs`,
//!   adversary report w4a-adv): of the 160 lines the Linux model passes where
//!   the macOS reading did not, none reaches a decoy process. Everything else
//!   is the union of the Linux and macOS readings.
//! * A process is the user's by the `kill(2)` rule over the real, effective and
//!   saved uids in `/proc/<pid>/status` (Linux), not by the owner of
//!   `/proc/<pid>`, which the kernel makes root's for a process that is not
//!   dumpable. A process that runs as the user is counted whatever its other
//!   ids say.
//! * A process is a GitPulse process or an agent session by the executable the
//!   kernel runs it from (`/proc/<pid>/exe` on Linux, with ` (deleted)` taken
//!   off: a binary replaced by an update), by the path it was exec'd with, by
//!   the kernel's name for it (`comm`, which `argv[0]` does not change), and,
//!   for a program run as `ld.so PROGRAM`, by `PROGRAM`. A process reached by
//!   the command whose executable could not be read (not dumpable) or is the
//!   loader is of unknown identity: if nothing refuses the command it is asked
//!   about, never passed by the spelling the process chose. macOS does not
//!   report an executable (its exec path is the kernel's), so none is unknown
//!   there.
//! * A word given to Linux `pidof` with a `/` in it is an executable: the guard
//!   counts the processes running that file (the path as written and as it
//!   resolves, symbolic links followed) and, as for a name, those whose name is
//!   its last component. A glob or brace list there is asked about (the shell
//!   expands it first). The real `pidof` does not follow a hard link or a path
//!   relative to the shell's directory, so neither is modelled; `pidof -x NAME`
//!   for a shell running a script named NAME (it answers with the shell's pid)
//!   is not.
//! * The shell reader looks through the wrappers in `resolve` (`sudo`, `env`,
//!   `command`, `nohup`, `timeout`, `nice`, `ionice`, `taskset`, `chrt`,
//!   `flock`, `setpriv`, `unshare`, `setsid`, `stdbuf`, `exec`, …), and
//!   through `busybox`/`toybox`, whose `pkill`, `killall` and `killall5` read
//!   their own options and are asked about. It reads the script of a shell
//!   given options before `-c` (`-o pipefail`, `-ec`, `-lc`). It asks about: a
//!   script piped into a shell, or `source <(…)`, on a pipeline that spells a
//!   signal tool; a program that is `{}` (what `find -exec` or `xargs -I{}` puts
//!   in place); a program that is a glob or brace list one of whose expansions
//!   is a signal tool (`/usr/bin/pk[i]ll`, `{pkill,-f,x}`); `find -exec` of a
//!   signal command; `ssh` to this machine (`localhost`, a loopback address, or
//!   a host built from a variable) with a signal command in it.
//! * Stated limits, silent today and pinned by `the_stated_limits_of_the_shell_
//!   reader_are_pinned`: `ssh` to another host (its processes are not on this
//!   machine); `parallel`, `watch`, `nsenter`, `su -c`, `env -S`, `script -c`,
//!   `docker exec` and the like, whose arguments are a command for another
//!   program; a program built by a command substitution where no signal tool is
//!   a word of the command (`$(printf 'pk%s' ill)`); a link or script file the
//!   line makes and then runs (`ln -s /usr/bin/pkill xpkill; ./xpkill`,
//!   `printf … > s.sh; bash s.sh`), and a file it sources (`p=ls; . ./env.sh;
//!   $p …`: the file may set `p`); a pid file the line did not write; an
//!   alias, function or variable defined outside the line; other programs that
//!   signal (`skill`, `snice`, `fuser -k`, `systemctl kill`, `killall5` outside
//!   busybox, `python3`/`perl` doing `kill`).

use std::collections::HashMap;
use std::time::Duration;

use regex::{Regex, RegexBuilder};

use super::{decision, HookInput, HookOutput, PermissionDecision, PRE_TOOL_USE};
use crate::sysload::procscan::{is_dynamic_loader, signal_table, TableRow};

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
    /// The `( … )` groups the command is inside, outermost first, each by an
    /// id that is unique within its script: a subshell, whose assignments the
    /// shell that ran it never sees.
    scope: Vec<u32>,
    /// Run with `&`: a subshell of its own.
    background: bool,
    /// An arithmetic command `(( … ))`; its words are those of the expression.
    arith: bool,
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
    /// The `( … )` groups open at this point, and the next id to hand out.
    groups: Vec<u32>,
    next_group: u32,
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
        groups: Vec::new(),
        next_group: 0,
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
            let mut done = std::mem::take(cmd);
            done.scope.clone_from(&self.groups);
            pipe.push(done);
        }
        Ok(())
    }

    /// The `))` that closes the arithmetic command opened by `((` at
    /// `self.i`, with the parentheses inside it balanced.
    fn arith_end(&self) -> Option<usize> {
        let mut depth = 0usize;
        let mut k = self.i + 2;
        while k < self.chars.len() {
            match self.chars[k] {
                '\\' => k += 1,
                '(' => depth += 1,
                ')' if depth > 0 => depth -= 1,
                ')' if self.chars.get(k + 1) == Some(&')') => return Some(k),
                ')' => return None,
                _ => {}
            }
            k += 1;
        }
        None
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
                    let background = self.at(1) != Some('&');
                    self.i += if background { 1 } else { 2 };
                    let before = done.len();
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                    if background && done.len() > before {
                        if let Some(last) = done.last_mut() {
                            for c in last {
                                c.background = true;
                            }
                        }
                    }
                }
                '|' if self.at(1) == Some('|') => {
                    self.i += 2;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                '|' => {
                    self.i += if self.at(1) == Some('&') { 2 } else { 1 };
                    self.flush_command(&mut cmd, &mut pipe)?;
                }
                // `(( … ))`: an arithmetic command. Its expression is read as
                // the words of one command (substitutions in it still run).
                '(' if self.at(1) == Some('(') && cmd.is_empty() && self.arith_end().is_some() => {
                    let end = self.arith_end().unwrap_or(self.i + 2);
                    let inner = self.nested(self.i + 2, end)?;
                    let mut arith = Cmd {
                        arith: true,
                        ..Cmd::default()
                    };
                    for c in inner.into_iter().flatten() {
                        arith.words.extend(c.words);
                        arith.redirects.extend(c.redirects);
                        arith.heredocs.extend(c.heredocs);
                    }
                    self.i = end + 2;
                    cmd = arith;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                }
                // `( … )`: a subshell.
                '(' => {
                    self.i += 1;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                    if self.groups.len() >= MAX_DEPTH {
                        return Err(format!("subshells nested more than {MAX_DEPTH} deep"));
                    }
                    self.groups.push(self.next_group);
                    self.next_group += 1;
                }
                ')' => {
                    self.i += 1;
                    self.flush_pipeline(&mut cmd, &mut pipe, &mut done)?;
                    self.groups.pop();
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
                // `$'…'`: the text between the quotes, escapes decoded (`\x70`,
                // `\153`, `\n`), so `$'\x70kill'` is the program `pkill`.
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
                w.text.push_str(&ansi_c_decode(&self.slice(from, k)));
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
        groups: Vec::new(),
        next_group: 0,
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

    fn exe_paths(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter_map(|f| match &f.sel {
                Sel::Killall(s) => Some(s.exe_paths.clone()),
                _ => None,
            })
            .flatten()
            .collect()
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

/// Which family of `pkill`, `pgrep` and `killall` a command line is read as.
/// The tools differ in what a name is matched against, in how options are
/// read, in whose ancestors they leave out and in which options they have, so
/// the answer to "which processes would this signal" depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// macOS: BSD `getopt` stops at the first operand, a name is `argv[0]`'s
    /// last component, and `pkill` never matches its own ancestors.
    Bsd,
    /// Linux: procps `pgrep`/`pkill` and psmisc `killall`. GNU `getopt_long`
    /// reads options anywhere, a name is the kernel's `comm`, a signal can be
    /// named anywhere in the arguments, and only `pkill` itself is left out.
    Procps,
}

impl Dialect {
    fn of(os: &str) -> Self {
        if os == "linux" {
            Self::Procps
        } else {
            Self::Bsd
        }
    }

    /// The tools on the host the hook runs on.
    fn host() -> Self {
        Self::of(std::env::consts::OS)
    }

    /// Whether `pkill` leaves its ancestors out of the match unasked.
    fn skips_ancestors(self) -> bool {
        self == Self::Bsd
    }
}

impl Default for Dialect {
    fn default() -> Self {
        Self::host()
    }
}

#[derive(Debug, Clone, Default)]
struct PgrepSpec {
    prog: &'static str,
    dialect: Dialect,
    patterns: Vec<String>,
    full: bool,
    icase: bool,
    exact: bool,
    inverse: bool,
    newest: bool,
    oldest: bool,
    /// macOS `-a`: ancestors are matched again.
    ancestors: bool,
    /// procps `-A`: ancestors are left out.
    ignore_ancestors: bool,
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
    /// On Linux, when options follow the first operand: the same words read
    /// the macOS way. procps reads those options as options (a narrower or a
    /// wider set, depending on which), so the guard counts what either
    /// reading would signal and never fewer than the macOS reading did.
    alt: Option<Box<PgrepSpec>>,
}

#[derive(Debug, Clone, Default)]
struct KillallSpec {
    dialect: Dialect,
    names: Vec<String>,
    /// Paths to executables, from a `pidof` word with a `/` in it: a process
    /// that runs one is picked whatever name it goes by.
    exe_paths: Vec<String>,
    regex: bool,
    /// psmisc `-I`.
    icase: bool,
    user: Option<String>,
    only: Option<String>,
    late: Vec<String>,
    corrected: Option<String>,
    /// As [`PgrepSpec::alt`].
    alt: Option<Box<KillallSpec>>,
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
/// How many compound commands `cmd` opens (`while`, `until`, `for`, `select`,
/// `if`, `case`, `{`) and closes (`done`, `fi`, `esac`, `}`): by its first word,
/// which is where a shell reads a reserved word.
fn compound_opens(cmd: &Cmd) -> usize {
    usize::from(cmd.words.first().is_some_and(|w| {
        matches!(
            w.text.as_str(),
            "while" | "until" | "for" | "select" | "if" | "case" | "{"
        )
    }))
}

fn compound_closes(cmd: &Cmd) -> usize {
    usize::from(
        cmd.words
            .first()
            .is_some_and(|w| matches!(w.text.as_str(), "done" | "fi" | "esac" | "}")),
    )
}

/// Programs that send a signal to whatever they are given or select.
const SIGNALLERS: [&str; 4] = ["kill", "pkill", "killall", "killall5"];

/// Whether an `ssh` destination (`host`, `user@host`) is this machine.
fn is_loopback_destination(word: &str) -> bool {
    let host = word.rsplit('@').next().unwrap_or(word);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host == "0.0.0.0"
        || host.starts_with("127.")
}
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
    plan_with(command, process_env, Dialect::host())
}

fn plan_with(command: &str, env: EnvFn, dialect: Dialect) -> Plan {
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
                dialect,
                spelled: spelled_tools(command),
                near: String::new(),
                pipeline_tool: false,
                group_path: Vec::new(),
                group_frames: Vec::new(),
                stage_frames: Vec::new(),
                compound_depth: 0,
                bound_tool: false,
                binds: Binds::default(),
                alias_stack: Vec::new(),
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
    dialect: Dialect,
    /// What the line spells out as words, for the pid lists a `kill` is given.
    spelled: Spelled,
    /// What the command being read spells out (see [`Cx::command`]).
    near: String,
    /// The pipeline being read is more than one command and spells a signal tool.
    pipeline_tool: bool,
    /// The subshell groups the last command read was in, and what was bound
    /// when each was entered (see [`Cx::walk`]).
    group_path: Vec<u32>,
    group_frames: Vec<Binds>,
    /// What was bound before a pipeline stage that opened a compound command,
    /// and the compound depth to return to for it to end.
    stage_frames: Vec<(Binds, usize)>,
    compound_depth: usize,
    /// A command that binds something (an assignment, an alias, a loop
    /// variable) spelled a signal tool: a program the line named later may be
    /// it.
    bound_tool: bool,
    /// What the line has bound so far.
    binds: Binds,
    /// The aliases being expanded, so one that names itself ends.
    alias_stack: Vec<String>,
}

/// The command a wrapper (`sudo`, `env`, `nohup` …) runs.
struct Resolved<'a> {
    prog: String,
    prog_at: usize,
    args: &'a [Word],
    elevated: bool,
    /// Run as an applet of `busybox` or `toybox`, whose `pkill`, `killall` and
    /// `kill` read their own options, not procps's or psmisc's.
    applet: bool,
}

/// Why a command line names no program the guard can read.
#[derive(Debug, PartialEq, Eq)]
enum Unresolved {
    /// Nothing runs: a builtin that only queries, or no command at all.
    Nothing,
    /// The program word is built from a variable or a substitution, so which
    /// program runs is known only when the shell runs it.
    Dynamic,
    /// The program word is a glob or a brace list the shell expands, and an
    /// expansion of it is the name of a program that signals.
    MaySignal,
}

/// What a line spells out as words, for deciding whether a program the guard
/// cannot read is worth a question: a signal tool as a whole shell word (not
/// inside a path, `skill`, `--kill-after` or `KILL_AFTER=5`). Quotes and
/// backslashes are removed first, so `pk''ill` is `pkill`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Spelled {
    /// `kill`, `pkill`, `killall`, `killall5`, `pgrep` or `pidof` is a word.
    tool: bool,
    /// `pgrep` or `pidof` is a word: pids that reach a `kill` may come from it.
    pids: bool,
}

/// The tools [`Spelled`] looks for.
const GATING_TOOLS: [&str; 6] = ["kill", "pkill", "killall", "killall5", "pgrep", "pidof"];

fn spelled_tools(command: &str) -> Spelled {
    let flat: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let mut spelled = Spelled::default();
    for raw in flat.split(|c: char| c.is_whitespace() || ";&|()<>{}$`,".contains(c)) {
        let token = match raw.split_once('=') {
            Some((_, value)) if is_assignment(raw) => value,
            _ => raw,
        };
        let name = base_name(token);
        if GATING_TOOLS.contains(&name) {
            spelled.tool = true;
            spelled.pids |= matches!(name, "pgrep" | "pidof");
        }
    }
    spelled
}

/// What this line has bound so far, in the order the shell runs it. Used only
/// where a word is a program or a script (`resolve`, `eval`, `-c`) and where a
/// `kill` is given pids: never to expand an argument, which stays "built from
/// a variable" and is asked about as before.
#[derive(Debug, Clone, Default)]
struct Binds {
    /// `NAME=value` with a value the line spells out; `None` when it was
    /// assigned twice differently or from something unknown.
    values: HashMap<String, Option<String>>,
    /// `alias NAME=value`; `None` for a value that is not plain text.
    aliases: HashMap<String, Option<String>>,
    /// Variables holding pids from a `pgrep`/`pidof` on the line: the first
    /// command of the pipeline that made them, and whether it was alone.
    pid_vars: HashMap<String, Vec<(Cmd, bool)>>,
    /// Files a `pgrep`/`pidof` on the line wrote.
    pid_files: HashMap<String, Cmd>,
}

/// The text of the ANSI-C quoted `$'…'` body `s`, escapes decoded.
fn ansi_c_decode(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut bytes: Vec<u8> = Vec::new();
    let push_char = |bytes: &mut Vec<u8>, c: char| {
        let mut buf = [0u8; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '\\' {
            push_char(&mut bytes, c);
            continue;
        }
        let Some(&e) = chars.get(i) else {
            bytes.push(b'\\');
            break;
        };
        i += 1;
        let hex = |from: usize, max: usize| -> (u32, usize) {
            let mut v = 0u32;
            let mut n = 0;
            while n < max {
                match chars.get(from + n).and_then(|c| c.to_digit(16)) {
                    Some(d) => {
                        v = v * 16 + d;
                        n += 1;
                    }
                    None => break,
                }
            }
            (v, n)
        };
        match e {
            'a' => bytes.push(7),
            'b' => bytes.push(8),
            'e' | 'E' => bytes.push(27),
            'f' => bytes.push(12),
            'n' => bytes.push(b'\n'),
            'r' => bytes.push(b'\r'),
            't' => bytes.push(b'\t'),
            'v' => bytes.push(11),
            '\\' | '\'' | '"' | '?' => push_char(&mut bytes, e),
            'x' => {
                let (v, n) = hex(i, 2);
                if n == 0 {
                    bytes.extend_from_slice(b"\\x");
                } else {
                    bytes.push(u8::try_from(v).unwrap_or(b'?'));
                    i += n;
                }
            }
            'u' | 'U' => {
                let (v, n) = hex(i, if e == 'u' { 4 } else { 8 });
                match char::from_u32(v).filter(|_| n > 0) {
                    Some(ch) => {
                        push_char(&mut bytes, ch);
                        i += n;
                    }
                    None => {
                        bytes.push(b'\\');
                        push_char(&mut bytes, e);
                    }
                }
            }
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap_or(0);
                let mut n = 1;
                while n < 3 {
                    match chars.get(i).and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                bytes.push(u8::try_from(v & 0xff).unwrap_or(b'?'));
            }
            other => {
                bytes.push(b'\\');
                push_char(&mut bytes, other);
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What the line `src` of one word comes to when the variables the line has
/// bound are filled in, or `None` when any part of it is not known: a command
/// or arithmetic substitution, a variable that is unbound, assigned twice, or
/// from something unknown, an unquoted glob or brace, or a value that would
/// split into several words.
fn bound_text(src: &str, binds: &Binds) -> Option<String> {
    bound_text_with(src, binds, false)
}

/// [`bound_text`], where `rejoined` says the words a value splits into are put
/// back together with spaces (what `eval` does with its arguments), so a value
/// with blanks in it is still known.
fn bound_text_with(src: &str, binds: &Binds, rejoined: bool) -> Option<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let lookup = |name: &str, quoted: bool| -> Option<String> {
        let value = binds.values.get(name)?.as_ref()?;
        let glob = value.chars().any(|c| "*?[]{}~".contains(c));
        let blanks = value.chars().any(char::is_whitespace);
        (quoted || (!glob && (rejoined || !blanks))).then(|| value.clone())
    };
    while i < chars.len() {
        match chars[i] {
            '\'' => {
                let end = chars[i + 1..].iter().position(|c| *c == '\'')? + i + 1;
                out.extend(&chars[i + 1..end]);
                i = end + 1;
            }
            '"' => {
                i += 1;
                loop {
                    match *chars.get(i)? {
                        '"' => {
                            i += 1;
                            break;
                        }
                        '\\' => {
                            let n = *chars.get(i + 1)?;
                            if !matches!(n, '"' | '\\' | '$' | '`') {
                                out.push('\\');
                            }
                            out.push(n);
                            i += 2;
                        }
                        '$' => {
                            let (name, next) = var_at(&chars, i)?;
                            out.push_str(&lookup(&name, true)?);
                            i = next;
                        }
                        '`' => return None,
                        c => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                out.push(*chars.get(i + 1)?);
                i += 2;
            }
            '$' if chars.get(i + 1) == Some(&'\'') => {
                let mut k = i + 2;
                while k < chars.len() && chars[k] != '\'' {
                    if chars[k] == '\\' {
                        k += 1;
                    }
                    k += 1;
                }
                if k >= chars.len() {
                    return None;
                }
                let body: String = chars[i + 2..k].iter().collect();
                out.push_str(&ansi_c_decode(&body));
                i = k + 1;
            }
            '$' => {
                let (name, next) = var_at(&chars, i)?;
                out.push_str(&lookup(&name, false)?);
                i = next;
            }
            '*' | '?' | '[' | ']' | '{' | '}' | '`' | '~' => return None,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

/// The variable named at `chars[at]` (`$name` or `${name}`) and the index after
/// it; `None` for anything else, `${name:-x}` and `$(…)` included.
fn var_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    let ident = |from: usize| {
        let len = chars[from..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
            .count();
        let name: String = chars[from..from + len].iter().collect();
        let valid = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        valid.then_some((name, from + len))
    };
    match chars.get(at + 1)? {
        '{' => {
            let (name, end) = ident(at + 2)?;
            (chars.get(end) == Some(&'}')).then_some((name, end + 1))
        }
        _ => ident(at + 1),
    }
}

/// Every variable `src` refers to (`$name`, `${name}`, `${name[@]}`), outside
/// single quotes.
fn var_refs(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut refs = Vec::new();
    let mut i = 0;
    let mut single = false;
    while i < chars.len() {
        match chars[i] {
            '\'' => {
                single = !single;
                i += 1;
            }
            '\\' => i += 2,
            '$' if !single => {
                let named = if chars.get(i + 1) == Some(&'{') {
                    let from = i + 2;
                    let len = chars[from.min(chars.len())..]
                        .iter()
                        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                        .count();
                    Some((
                        chars[from..from + len].iter().collect::<String>(),
                        from + len,
                    ))
                } else {
                    var_at(&chars, i)
                };
                match named {
                    Some((name, next)) if !name.is_empty() => {
                        refs.push(name);
                        i = next;
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    refs
}

/// The variables an arithmetic expression assigns: a name before `=`, `+=`,
/// `-=`, `*=`, `/=`, `%=`, `<<=`, `>>=`, `&=`, `|=`, `^=` (not `==`), or next to
/// `++` or `--`; an array subscript is skipped.
fn arith_targets(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut targets = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !(chars[i].is_ascii_alphabetic() || chars[i] == '_') {
            i += 1;
            continue;
        }
        let from = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let name: String = chars[from..i].iter().collect();
        let mut j = i;
        if chars.get(j) == Some(&'[') {
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            j += 1;
        }
        while chars.get(j) == Some(&' ') {
            j += 1;
        }
        let rest: String = chars[j.min(chars.len())..].iter().take(3).collect();
        let assigns = (rest.starts_with('=') && !rest.starts_with("=="))
            || [
                "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "++", "--",
            ]
            .iter()
            .any(|op| rest.starts_with(op));
        let preceded = from >= 2
            && matches!(
                chars[from - 2..from].iter().collect::<String>().as_str(),
                "++" | "--"
            );
        if assigns || preceded {
            targets.push(name);
        }
    }
    targets
}

/// Whether `src` has a glob or brace the shell would expand: one outside quotes.
fn has_unquoted_meta(src: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = src.chars();
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\\') | (Some('"'), '\\') => {
                chars.next();
            }
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            // `${name}` is a parameter, not a brace list.
            (None, '{') if previous == '$' => {}
            (None, '*' | '?' | '[' | '{') => return true,
            _ => {}
        }
        previous = c;
    }
    false
}

/// Whether a glob or brace word, whose text is `text`, can expand to the name
/// of a program that signals: each alternative of a one-level brace list, and
/// each file name `*`, `?` and `[…]` could be, against the tool names.
fn could_name_tool(text: &str) -> bool {
    let mut words = vec![text.to_string()];
    if let (Some(open), Some(close)) = (text.find('{'), text.rfind('}')) {
        if open < close {
            let (head, body, tail) = (&text[..open], &text[open + 1..close], &text[close + 1..]);
            words = body
                .split(',')
                .take(16)
                .map(|alt| format!("{head}{alt}{tail}"))
                .collect();
        }
    }
    words.iter().any(|w| {
        let last = base_name(w);
        GATING_TOOLS.iter().any(|tool| glob_matches(last, tool))
    })
}

/// A shell glob (`*`, `?`, `[…]`) against `name`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some('[') => {
                let Some(close) = p.iter().skip(2).position(|c| *c == ']').map(|k| k + 2) else {
                    return !n.is_empty() && n[0] == '[' && go(&p[1..], &n[1..]);
                };
                let class = &p[1..close];
                let (negate, class) = match class.first() {
                    Some('!' | '^') => (true, &class[1..]),
                    _ => (false, class),
                };
                let Some(&c) = n.first() else { return false };
                let mut hit = false;
                let mut k = 0;
                while k < class.len() {
                    if class.get(k + 1) == Some(&'-') && k + 2 < class.len() {
                        hit |= class[k] <= c && c <= class[k + 2];
                        k += 3;
                    } else {
                        hit |= class[k] == c;
                        k += 1;
                    }
                }
                hit != negate && go(&p[close + 1..], &n[1..])
            }
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
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
/// the program that runs. `Err` says why there is none: nothing runs (a
/// builtin that only queries), or the program is named by a variable.
fn resolve<'a>(words: &'a [Word], binds: &Binds) -> Result<Resolved<'a>, Unresolved> {
    let mut i = 0usize;
    let mut elevated = false;
    let mut applet = false;
    loop {
        let w = words.get(i).ok_or(Unresolved::Nothing)?;
        if is_assignment(&w.text) || KEYWORDS.contains(&w.text.as_str()) {
            i += 1;
            continue;
        }
        // A program built from variables this line bound is what they come to;
        // one built from anything else is not known.
        let bound;
        let text: &str = if w.dynamic {
            bound = bound_text(&w.src, binds).ok_or(Unresolved::Dynamic)?;
            &bound
        } else {
            &w.text
        };
        if !w.dynamic && has_unquoted_meta(&w.src) && could_name_tool(text) {
            return Err(Unresolved::MaySignal);
        }
        let base = base_name(text.trim_start_matches('\\'));
        i = match base {
            "ionice" => skip_options(
                words,
                i + 1,
                &[
                    "-c",
                    "-n",
                    "-p",
                    "-P",
                    "-u",
                    "--class",
                    "--classdata",
                    "--pid",
                    "--pgid",
                    "--uid",
                ],
            ),
            "taskset" => {
                let k = skip_options(words, i + 1, &[]);
                if words[i + 1..k.min(words.len())].iter().any(|o| {
                    o.text == "--pid" || (!o.text.starts_with("--") && o.text.contains('p'))
                }) {
                    return Err(Unresolved::Nothing);
                }
                k + 1
            }
            "chrt" => {
                let k = skip_options(
                    words,
                    i + 1,
                    &[
                        "-T",
                        "-P",
                        "-D",
                        "--sched-runtime",
                        "--sched-period",
                        "--sched-deadline",
                    ],
                );
                if words[i + 1..k.min(words.len())].iter().any(|o| {
                    o.text == "--pid"
                        || o.text == "--max"
                        || (!o.text.starts_with("--") && o.text.contains(['p', 'm']))
                }) {
                    return Err(Unresolved::Nothing);
                }
                k + 1
            }
            "flock" => {
                let k = skip_options(
                    words,
                    i + 1,
                    &["-w", "-E", "--timeout", "--wait", "--conflict-exit-code"],
                );
                // `flock FILE -c STRING` hands the string to a shell.
                if let Some(c) = words
                    .iter()
                    .enumerate()
                    .skip(k)
                    .find(|(_, o)| o.text == "-c" || o.text == "--command")
                    .map(|(at, _)| at)
                {
                    return Ok(Resolved {
                        prog: "sh".to_string(),
                        prog_at: c,
                        args: &words[c..],
                        elevated,
                        applet,
                    });
                }
                k + 1
            }
            "setpriv" => skip_options(
                words,
                i + 1,
                &[
                    "--reuid",
                    "--regid",
                    "--ruid",
                    "--euid",
                    "--rgid",
                    "--egid",
                    "--groups",
                    "--inh-caps",
                    "--ambient-caps",
                    "--bounding-set",
                    "--securebits",
                    "--pdeathsig",
                    "--selinux-label",
                    "--apparmor-profile",
                    "--landlock-access",
                    "--seccomp-filter",
                ],
            ),
            "unshare" => skip_options(
                words,
                i + 1,
                &[
                    "--propagation",
                    "--setgroups",
                    "--map-user",
                    "--map-group",
                    "--map-users",
                    "--map-groups",
                    "-S",
                    "-G",
                    "--wd",
                    "-w",
                    "--root",
                    "-R",
                ],
            ),
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
                    return Err(Unresolved::Nothing);
                }
                skip_options(words, i + 1, &[])
            }
            "busybox" | "toybox" => {
                applet = true;
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
                return Ok(Resolved {
                    prog: base.to_string(),
                    prog_at: i,
                    args: &words[i + 1..],
                    elevated,
                    applet,
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

/// The script a shell was handed with `-c`: the first operand once a cluster
/// with `c` has been read, past options that take an argument (`-o pipefail`,
/// `-O extglob`, `+o`, `--rcfile F`, a cluster ending in `o` or `O`) and long
/// options that do not. `None` for a shell given a file, or nothing.
fn shell_script(args: &[Word]) -> Option<&Word> {
    let mut i = 0;
    let mut command = false;
    while let Some(w) = args.get(i) {
        let t = w.text.as_str();
        i += 1;
        if t == "--" {
            return command.then(|| args.get(i)).flatten();
        }
        if t.len() < 2 || !(t.starts_with('-') || t.starts_with('+')) {
            return command.then_some(w);
        }
        if matches!(t, "-o" | "-O" | "+o" | "+O" | "--rcfile" | "--init-file") {
            i += 1;
        } else if !t.starts_with("--") {
            command |= t.starts_with('-') && t.contains('c');
            if t.ends_with(['o', 'O']) {
                i += 1;
            }
        }
    }
    None
}

/// Whether a shell is given no script at all: no `-c`, no file, so it reads
/// its standard input.
fn shell_reads_stdin(args: &[Word]) -> bool {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        let t = w.text.as_str();
        i += 1;
        if t == "--" || t.len() < 2 || !(t.starts_with('-') || t.starts_with('+')) {
            return false;
        }
        if matches!(t, "-o" | "-O" | "+o" | "+O" | "--rcfile" | "--init-file") {
            i += 1;
        } else if !t.starts_with("--") && t.starts_with('-') && t.contains('c') {
            return false;
        }
    }
    true
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

    /// Reads a script. What it binds stays bound after it, except what a
    /// subshell binds (`( … )`, a stage of a pipeline, a command run with `&`):
    /// the shell that ran it never sees that, so the bindings are put back as
    /// they were when the subshell ends. A pipeline stage that opens a compound
    /// command (`… | while read p; do …; done`) runs until the compound
    /// closes, so what it binds (the loop variable) is visible to the body.
    fn walk(&mut self, script: &[Pipeline], depth: usize) {
        let outer = (
            std::mem::take(&mut self.group_path),
            std::mem::take(&mut self.group_frames),
            std::mem::take(&mut self.stage_frames),
            std::mem::replace(&mut self.compound_depth, 0),
        );
        for pipeline in script {
            // Whether the pipeline spells a signal tool, once: a script piped
            // into a shell is asked about when it does.
            let tool = self.pipeline_tool
                || (pipeline.len() > 1
                    && pipeline
                        .iter()
                        .any(|c| c.words.iter().any(|w| spelled_tools(&w.src).tool)));
            let saved = std::mem::replace(&mut self.pipeline_tool, tool);
            let subshell = pipeline.len() > 1 || pipeline.iter().any(|c| c.background);
            let before = subshell.then(|| self.binds.clone());
            let depth_before = self.compound_depth;
            for (at, cmd) in pipeline.iter().enumerate() {
                self.enter_groups(&cmd.scope);
                self.command(pipeline, at, cmd, depth);
            }
            self.pipeline_tool = saved;
            for cmd in pipeline {
                self.compound_depth = (self.compound_depth + compound_opens(cmd))
                    .saturating_sub(compound_closes(cmd));
            }
            if let Some(before) = before {
                let runs_on = self.compound_depth > depth_before
                    && pipeline.last().is_some_and(|c| compound_opens(c) > 0);
                if runs_on {
                    self.stage_frames.push((before, depth_before));
                } else {
                    self.binds = before;
                }
            }
            while self
                .stage_frames
                .last()
                .is_some_and(|(_, d)| self.compound_depth <= *d)
            {
                if let Some((before, _)) = self.stage_frames.pop() {
                    self.binds = before;
                }
            }
        }
        self.enter_groups(&[]);
        while let Some((before, _)) = self.stage_frames.pop() {
            self.binds = before;
        }
        self.group_path = outer.0;
        self.group_frames = outer.1;
        self.stage_frames = outer.2;
        self.compound_depth = outer.3;
    }

    /// Moves from the subshell groups the last command was in to those of the
    /// next: each one left puts back what was bound when it was entered, each
    /// one entered remembers what is bound now.
    fn enter_groups(&mut self, path: &[u32]) {
        let common = self
            .group_path
            .iter()
            .zip(path)
            .take_while(|(a, b)| a == b)
            .count();
        while self.group_path.len() > common {
            self.group_path.pop();
            if let Some(before) = self.group_frames.pop() {
                self.binds = before;
            }
        }
        for id in &path[common..] {
            self.group_frames.push(self.binds.clone());
            self.group_path.push(*id);
        }
    }

    /// [`Cx::walk`] for what a new shell or a substitution runs: nothing it
    /// binds is visible to the shell that started it.
    fn walk_scoped(&mut self, script: &[Pipeline], depth: usize) {
        let before = self.binds.clone();
        self.walk(script, depth);
        self.binds = before;
    }

    /// [`Cx::script_text`] for a script a new shell runs.
    fn shell_text(&mut self, text: &str, depth: usize) {
        let before = self.binds.clone();
        self.script_text(text, depth);
        self.binds = before;
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

    /// One command. `near` is what it spells out, for deciding whether a
    /// program the guard cannot read is worth a question: the words of the
    /// command, and, for a script piped into a shell, of its whole pipeline.
    fn command(&mut self, pipeline: &Pipeline, at: usize, cmd: &Cmd, depth: usize) {
        let near = cmd
            .words
            .iter()
            .map(|w| w.src.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let saved = std::mem::replace(&mut self.near, near);
        self.command_in(pipeline, at, cmd, depth);
        self.near = saved;
    }

    fn command_in(&mut self, pipeline: &Pipeline, at: usize, cmd: &Cmd, depth: usize) {
        // A substitution runs in a subshell: what it binds is not seen here.
        for w in cmd.words.iter().chain(&cmd.redirects) {
            for body in &w.subst {
                self.walk_scoped(body, depth + 1);
            }
        }
        for h in &cmd.heredocs {
            for body in &h.nested {
                self.walk_scoped(body, depth + 1);
            }
        }
        // An alias is read as what it stands for. One named for a signal tool
        // (`alias pkill=echo`) is read both ways: the command as written was
        // always judged, and an alias must never make it quieter.
        let shadows_tool = cmd
            .words
            .first()
            .is_some_and(|w| GATING_TOOLS.contains(&base_name(&w.text)));
        if self.expand_alias(cmd, depth) && !shadows_tool {
            self.record(pipeline, at, cmd);
            return;
        }
        let resolved = resolve(&cmd.words, &self.binds);
        self.record(pipeline, at, cmd);
        let r = match resolved {
            Ok(r) => r,
            Err(Unresolved::Dynamic) => {
                // A program named by a variable that is handed `-c SCRIPT` is
                // a shell as far as this goes: its script is read.
                let program = cmd
                    .words
                    .iter()
                    .position(|w| !is_assignment(&w.text) && !KEYWORDS.contains(&w.text.as_str()));
                if let Some(script) = program.and_then(|p| shell_script(&cmd.words[p + 1..])) {
                    self.shell_script(script, depth);
                } else {
                    self.dynamic_program(echo(&cmd.words));
                }
                return;
            }
            Err(Unresolved::MaySignal) => {
                self.unevaluable(
                    echo(&cmd.words),
                    false,
                    "the program is a glob or a brace list the shell expands, and one of its \
                     expansions is a program that signals"
                        .into(),
                );
                return;
            }
            Err(Unresolved::Nothing) => return,
        };
        let text = echo(&cmd.words[r.prog_at..]);
        match r.prog.as_str() {
            "pkill" | "killall" | "killall5" if r.applet => self.unevaluable(
                text,
                r.elevated,
                format!(
                    "`{}` run as a busybox or toybox applet reads its options its own way, not \
                     procps's or psmisc's",
                    r.prog
                ),
            ),
            "pkill" => self.pgrep(&r, text, true, true),
            // The alias is recorded by `record`; a word that runs it is read as
            // what it stands for (`expand_alias`).
            "alias" => {}
            "find" => self.find_exec(&r, &text, depth),
            "{}" if self.pipeline_tool => self.unevaluable(
                text,
                r.elevated,
                "the program is what a pipeline's xargs or find puts in place of `{}`, and the \
                 pipeline names a signal tool"
                    .into(),
            ),
            "{}" => self.dynamic_program(text),
            "ssh" => self.ssh_remote(&r, text),
            "killall" => match parse_killall(r.args, self.dialect) {
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
                let i = match resolve(inner, &self.binds) {
                    Ok(i) => i,
                    Err(Unresolved::Dynamic | Unresolved::MaySignal) => {
                        self.dynamic_program(text);
                        return;
                    }
                    Err(Unresolved::Nothing) => return,
                };
                // The pids `xargs kill` is fed: the pipeline before it, or a
                // `< <(pgrep …)` it reads.
                let fed = cmd
                    .redirects
                    .iter()
                    .find_map(|w| w.subst.last())
                    .and_then(|body| body.last())
                    .and_then(|pl| pl.first().map(|c| (c.clone(), pl.len() == 1)));
                let upstream = if at > 0 {
                    Some((&pipeline[0], at == 1))
                } else {
                    fed.as_ref().map(|(c, single)| (c, *single))
                };
                match i.prog.as_str() {
                    "kill" => self.kill(i.args, &text, r.elevated || i.elevated, upstream),
                    "pkill" | "killall" => self.unevaluable(
                        text,
                        r.elevated,
                        format!("the arguments of `{}` come from xargs", i.prog),
                    ),
                    // The program is what xargs puts in place of `{}`: asked
                    // about when the command or its pipeline spells a tool.
                    "{}" if self.pipeline_tool => self.unevaluable(
                        text,
                        r.elevated,
                        "the program xargs runs is what it puts in place of `{}`, and the \
                         pipeline names a signal tool"
                            .into(),
                    ),
                    "{}" => self.dynamic_program(text),
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
                } else if at > 0 && cmd.heredocs.is_empty() && shell_reads_stdin(r.args) {
                    // A script piped in: what the guard cannot read is asked
                    // about when the pipeline spells a signal tool.
                    if self.pipeline_tool {
                        self.unevaluable(
                            text,
                            r.elevated,
                            "the shell runs a script piped into it, which GitPulse does not \
                             read, and the pipeline names a signal tool"
                                .into(),
                        );
                    }
                }
                for h in &cmd.heredocs {
                    self.shell_text(&h.body, depth);
                }
            }
            "source" | "." => {
                if r.args.first().is_some_and(|w| !w.subst.is_empty()) {
                    self.dynamic_program(text);
                }
            }
            "eval" => {
                // Read as written, a variable the line did not bind staying a
                // variable: `eval $c` runs a program the line does not name.
                let joined = r
                    .args
                    .iter()
                    .map(|w| {
                        if w.dynamic {
                            bound_text_with(&w.src, &self.binds, true)
                                .unwrap_or_else(|| w.text.clone())
                        } else {
                            w.text.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                self.script_text(&joined, depth);
            }
            _ => {}
        }
    }

    /// A program, or a script a program runs, that is known only when the
    /// shell expands it. Asked about when a signal tool is a word of the
    /// command (`kill`, `pkill`, `killall`, `killall5`, `pgrep`, `pidof`), or
    /// of a command that bound something earlier on the line: without one a
    /// variable is an ordinary program, with one it may stand for it.
    fn dynamic_program(&mut self, text: String) {
        if self.bound_tool || spelled_tools(&self.near).tool {
            self.unevaluable(
                text,
                false,
                "which program runs, or what script it is given, is built from something \
                 GitPulse cannot expand, on a line that names a signal tool"
                    .into(),
            );
        }
    }

    /// What the line has bound, from the command just read: a variable given
    /// a value, a variable that holds the pids of a `pgrep` or `pidof`, an
    /// alias, a file a `pgrep` or `pidof` wrote.
    fn record(&mut self, pipeline: &Pipeline, at: usize, cmd: &Cmd) {
        let first = cmd.words.first().map(|w| w.text.as_str()).unwrap_or("");
        let declares = matches!(
            first,
            "export" | "declare" | "typeset" | "local" | "readonly"
        );
        let assignments_only = !cmd.words.is_empty()
            && cmd.words.iter().enumerate().all(|(k, w)| {
                is_assignment(&w.text)
                    || (k == 0 && declares)
                    || (declares && w.text.starts_with('-'))
            });
        if (assignments_only || matches!(first, "alias" | "for" | "select"))
            && spelled_tools(&self.near).tool
        {
            self.bound_tool = true;
        }
        // A command that sets variables in ways this does not model: what it
        // names is no longer known, and it marks the line when it spells a tool
        // (here-strings and redirections included).
        let setter_text = || {
            let mut text = cmd
                .words
                .iter()
                .chain(&cmd.redirects)
                .map(|w| w.src.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            for h in &cmd.heredocs {
                text.push(' ');
                text.push_str(&h.body);
            }
            text
        };
        if cmd.arith {
            // The words are the expression cut at its blanks: read it whole.
            let expression = cmd
                .words
                .iter()
                .map(|w| w.src.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            for name in arith_targets(&expression) {
                self.unbind(&name);
            }
            if spelled_tools(&setter_text()).tool {
                self.bound_tool = true;
            }
            return;
        }
        // `declare -n NAME[=REF]`: NAME is a name for REF, so both are unknown.
        if matches!(first, "declare" | "typeset" | "local")
            && cmd.words.iter().skip(1).any(|w| {
                w.text.starts_with('-') && !w.text.starts_with("--") && w.text.contains('n')
            })
        {
            for w in cmd
                .words
                .iter()
                .skip(1)
                .filter(|w| !w.text.starts_with('-'))
            {
                for name in w.text.split('=') {
                    self.unbind(name);
                }
            }
            if spelled_tools(&setter_text()).tool {
                self.bound_tool = true;
            }
            return;
        }
        if assignments_only {
            for w in cmd.words.iter().filter(|w| is_assignment(&w.text)) {
                let Some(eq) = w.src.find('=') else { continue };
                let name = w.src[..eq].to_string();
                let value_src = &w.src[eq + 1..];
                let value = if w.dynamic {
                    bound_text(value_src, &self.binds)
                } else {
                    w.text.split_once('=').map(|(_, v)| v.to_string())
                };
                let producers: Vec<(Cmd, bool)> = w
                    .subst
                    .iter()
                    .filter_map(|body| body.last())
                    .filter_map(|pl| pl.first().map(|c| (c.clone(), pl.len() == 1)))
                    .collect();
                if value.is_none() && !producers.is_empty() {
                    self.binds.pid_vars.insert(name.clone(), producers);
                }
                let merged = match (self.binds.values.get(&name), value) {
                    (Some(None), _) | (_, None) => None,
                    (Some(Some(old)), Some(new)) if *old != new => None,
                    (_, Some(new)) => Some(new),
                };
                self.binds.values.insert(name, merged);
            }
            return;
        }
        match first {
            "alias" => {
                for w in cmd.words.iter().skip(1) {
                    if let Some((name, value)) = w.text.split_once('=') {
                        let plain = (!w.dynamic).then(|| value.to_string());
                        self.binds.aliases.insert(name.to_string(), plain);
                    }
                }
            }
            "for" | "select" => {
                if let Some(var) = cmd.words.get(1) {
                    let producers: Vec<(Cmd, bool)> = cmd
                        .words
                        .iter()
                        .skip(3)
                        .flat_map(|w| w.subst.iter())
                        .filter_map(|body| body.last())
                        .filter_map(|pl| pl.first().map(|c| (c.clone(), pl.len() == 1)))
                        .collect();
                    if !producers.is_empty() {
                        self.binds.pid_vars.insert(var.text.clone(), producers);
                    }
                    self.binds.values.insert(var.text.clone(), None);
                }
            }
            _ => {}
        }
        if let Ok(r) = resolve(&cmd.words, &self.binds) {
            if matches!(
                r.prog.as_str(),
                "read" | "printf" | "getopts" | "let" | "mapfile" | "readarray"
            ) && spelled_tools(&setter_text()).tool
            {
                self.bound_tool = true;
            }
            match r.prog.as_str() {
                // `read [-r] [-a ARRAY] [-d D] [-p P] … NAME…` (REPLY without one).
                "read" => {
                    let takes_arg = ["-d", "-i", "-n", "-N", "-p", "-t", "-u"];
                    let mut names: Vec<String> = Vec::new();
                    let mut k = 0;
                    while let Some(w) = r.args.get(k) {
                        k += 1;
                        let t = w.text.as_str();
                        if t == "-a" {
                            names.extend(r.args.get(k).map(|n| n.text.clone()));
                            k += 1;
                        } else if takes_arg.contains(&t) {
                            k += 1;
                        } else if !t.starts_with('-') {
                            names.push(t.to_string());
                        }
                    }
                    if names.is_empty() {
                        names.push("REPLY".to_string());
                    }
                    for name in &names {
                        self.unbind(name);
                        // `producer | while read p` runs the body in the stage
                        // that read the pids.
                        if at > 0 {
                            self.binds
                                .pid_vars
                                .insert(name.clone(), vec![(pipeline[0].clone(), at == 1)]);
                        }
                    }
                }
                // `printf -v NAME FORMAT …`.
                "printf" => {
                    if let Some(at_v) = r.args.iter().position(|w| w.text == "-v") {
                        if let Some(name) = r.args.get(at_v + 1) {
                            self.unbind(&name.text);
                        }
                    }
                }
                // `getopts OPTSTRING NAME [ARGS]` sets NAME, OPTARG and OPTIND.
                "getopts" => {
                    for name in r
                        .args
                        .get(1)
                        .map(|w| w.text.as_str())
                        .into_iter()
                        .chain(["OPTARG", "OPTIND"])
                    {
                        self.unbind(name);
                    }
                }
                // `let EXPR…`: each word is an arithmetic expression.
                "let" => {
                    for w in r.args {
                        for name in arith_targets(&w.src) {
                            self.unbind(&name);
                        }
                    }
                }
                // A sourced file may set, change or unset anything: nothing
                // bound before it is known after it.
                "source" | "." => {
                    for value in self.binds.values.values_mut() {
                        *value = None;
                    }
                    for value in self.binds.aliases.values_mut() {
                        *value = None;
                    }
                }
                "mapfile" | "readarray" => {
                    // `< <(…)` reads as a word of the command to this lexer.
                    let producers: Vec<(Cmd, bool)> = cmd
                        .redirects
                        .iter()
                        .chain(cmd.words.iter().filter(|w| w.src.starts_with("<(")))
                        .flat_map(|w| w.subst.iter())
                        .filter_map(|body| body.last())
                        .filter_map(|pl| pl.first().map(|c| (c.clone(), pl.len() == 1)))
                        .collect();
                    let skip_arg = ["-d", "-n", "-O", "-s", "-u", "-C", "-c"];
                    let mut k = 0;
                    let mut name = "MAPFILE".to_string();
                    while let Some(w) = r.args.get(k) {
                        k += 1;
                        if skip_arg.contains(&w.text.as_str()) {
                            k += 1;
                        } else if !w.text.starts_with('-') {
                            name = w.text.clone();
                            break;
                        }
                    }
                    self.unbind(&name);
                    if !producers.is_empty() {
                        self.binds.pid_vars.insert(name, producers);
                    }
                }
                "pgrep" | "pidof" => {
                    for w in &cmd.redirects {
                        self.binds.pid_files.insert(w.text.clone(), cmd.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// A variable something set that this does not model: unknown, and no
    /// longer the pids of anything.
    fn unbind(&mut self, name: &str) {
        self.binds.values.insert(name.to_string(), None);
        self.binds.pid_vars.remove(name);
    }

    /// A word that runs an alias the line defined is read as what the alias
    /// stands for, with the rest of the command after it. `true` when it was.
    fn expand_alias(&mut self, cmd: &Cmd, depth: usize) -> bool {
        let Some(first) = cmd.words.first().filter(|w| !w.dynamic) else {
            return false;
        };
        let Some(value) = self.binds.aliases.get(&first.text).cloned() else {
            return false;
        };
        if self.alias_stack.contains(&first.text) {
            return false;
        }
        let Some(value) = value else {
            self.dynamic_program(echo(&cmd.words));
            return true;
        };
        let rest: Vec<&str> = cmd.words.iter().skip(1).map(|w| w.src.as_str()).collect();
        let expanded = format!("{value} {}", rest.join(" "));
        self.alias_stack.push(first.text.clone());
        self.script_text(&expanded, depth);
        self.alias_stack.pop();
        true
    }

    /// `find … -exec CMD … ;` runs `CMD` once for each file, with the file
    /// named in place of `{}`, so the signal command on it is not the one that
    /// runs: asked about, not read. A shell it hands a script is read.
    fn find_exec(&mut self, r: &Resolved<'_>, text: &str, depth: usize) {
        let args = r.args;
        let mut i = 0;
        while i < args.len() {
            if !matches!(
                args[i].text.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir"
            ) {
                i += 1;
                continue;
            }
            let end = (i + 1..args.len())
                .find(|&j| args[j].text == ";" || (args[j].text == "+" && args[j - 1].text == "{}"))
                .unwrap_or(args.len());
            let inner = &args[i + 1..end];
            match resolve(inner, &self.binds) {
                // The file `find` found is the program: whatever it is, it
                // runs.
                Ok(p) if p.prog == "{}" => self.unevaluable(
                    text.to_string(),
                    r.elevated || p.elevated,
                    "`find -exec {}` runs each file it finds as a program, so GitPulse cannot \
                     tell what it would do"
                        .into(),
                ),
                Ok(p) if SIGNALLERS.contains(&p.prog.as_str()) => self.unevaluable(
                    text.to_string(),
                    r.elevated || p.elevated,
                    format!(
                        "`find -exec {}` runs it once for each file it finds, with the file \
                         named in the arguments, so GitPulse cannot tell which processes it \
                         would signal",
                        p.prog
                    ),
                ),
                Ok(p) if SHELLS.contains(&p.prog.as_str()) => {
                    if let Some(script) = shell_script(p.args) {
                        self.shell_script(script, depth);
                    }
                }
                Ok(_) | Err(Unresolved::Nothing) => {}
                Err(Unresolved::Dynamic | Unresolved::MaySignal) => {
                    self.dynamic_program(text.to_string());
                }
            }
            i = end + 1;
        }
    }

    /// `ssh HOST … pkill …` runs the signal command on `HOST`. On another
    /// machine that is not this machine's processes, and not the guard's
    /// business; on this one (`localhost`, a loopback address, a host named by
    /// a variable) it is, and the command is a string the remote shell reads.
    fn ssh_remote(&mut self, r: &Resolved<'_>, text: String) {
        let names_a_signaller = r.args.iter().any(|w| {
            w.text
                .split(|c: char| c.is_whitespace() || ";&|()`$".contains(c))
                .any(|token| SIGNALLERS.contains(&base_name(token)))
        });
        let reaches_this_machine = r
            .args
            .iter()
            .any(|w| w.dynamic || is_loopback_destination(&w.text));
        if names_a_signaller && reaches_this_machine {
            self.unevaluable(
                text,
                r.elevated,
                "`ssh` to this machine (or to a host GitPulse cannot name) runs the signal \
                 command in a shell GitPulse does not read"
                    .into(),
            );
        }
    }

    fn shell_script(&mut self, script: &Word, depth: usize) {
        // A script with a variable in it is read as written: `kill $PID` is a
        // `kill` with a pid the line does not give, `$c` is a program the
        // line does not name (asked about when the command spells a tool).
        let text = if script.dynamic {
            bound_text(&script.src, &self.binds).unwrap_or_else(|| script.text.clone())
        } else {
            script.text.clone()
        };
        self.shell_text(&text, depth);
    }

    fn pgrep(&mut self, r: &Resolved<'_>, text: String, pkill: bool, bounded: bool) {
        match parse_pgrep(r.args, pkill, self.dialect) {
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
        let Ok(r) = resolve(&first.words, &self.binds) else {
            self.unevaluable(
                text.to_string(),
                elevated,
                "the pids come from a command GitPulse could not read".into(),
            );
            return;
        };
        let source = echo(&first.words[r.prog_at..]);
        match r.prog.as_str() {
            // `kill $(cat FILE)` after a `pgrep`/`pidof` wrote FILE on this line.
            "cat"
                if r.args
                    .iter()
                    .any(|w| self.binds.pid_files.contains_key(&w.text)) =>
            {
                let stored: Vec<Cmd> = r
                    .args
                    .iter()
                    .filter_map(|w| self.binds.pid_files.get(&w.text).cloned())
                    .collect();
                for cmd in stored {
                    self.producer(&cmd, false, text, elevated);
                }
            }
            "pgrep" => self.pgrep(&r, format!("{text} (pids from `{source}`)"), false, bounded),
            "pidof" => {
                let words: Vec<String> = r
                    .args
                    .iter()
                    .filter(|w| !w.text.starts_with('-'))
                    .map(|w| w.text.clone())
                    .collect();
                // Linux `pidof` reads a word with a `/` as a path to an
                // executable and picks the processes running it, whatever
                // they call themselves. The guard counts the processes that
                // run that executable (the path as written and the one it
                // resolves to), and, as it always has, every process with the
                // last component of the path as its name.
                let exe_paths: Vec<String> = words
                    .iter()
                    .filter(|word| word.contains('/'))
                    .cloned()
                    .collect();
                let names: Vec<String> = words
                    .iter()
                    .flat_map(|word| {
                        let last = base_name(word);
                        if word.contains('/') && last != word {
                            vec![word.clone(), last.to_string()]
                        } else {
                            vec![word.clone()]
                        }
                    })
                    .collect();
                if r.args.iter().any(|w| w.dynamic) {
                    self.unevaluable(
                        format!("{text} (pids from `{source}`)"),
                        elevated,
                        "a process name built from a variable".into(),
                    );
                } else if r.args.iter().any(|w| has_unquoted_meta(&w.src)) {
                    // The shell expands a glob or a brace list before `pidof`
                    // sees it, against files the guard does not list.
                    self.unevaluable(
                        format!("{text} (pids from `{source}`)"),
                        elevated,
                        "a `pidof` argument is a glob or a brace list the shell expands first"
                            .into(),
                    );
                } else if !words.is_empty() {
                    self.push(Finding {
                        text: format!("{text} (pids from `{source}`)"),
                        elevated,
                        bounded,
                        sel: Sel::Killall(KillallSpec {
                            dialect: self.dialect,
                            names,
                            exe_paths,
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
                    continue;
                }
                // Pids held in a variable a `pgrep` or `pidof` on this line
                // filled are that command's; any other variable on a line
                // that runs one is a pid list GitPulse cannot follow.
                let linked: Vec<(Cmd, bool)> = var_refs(&w.src)
                    .iter()
                    .filter_map(|name| self.binds.pid_vars.get(name))
                    .flatten()
                    .cloned()
                    .collect();
                if !linked.is_empty() {
                    for (cmd, single) in &linked {
                        self.producer(cmd, *single, text, elevated);
                    }
                } else if self.spelled.pids {
                    self.unevaluable(
                        text.to_string(),
                        elevated,
                        "the pids come from a variable GitPulse could not follow, on a line \
                         that runs pgrep or pidof"
                            .into(),
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

fn parse_pgrep(all: &[Word], pkill: bool, dialect: Dialect) -> Parsed<PgrepSpec> {
    match dialect {
        Dialect::Bsd => parse_pgrep_bsd(all, pkill),
        Dialect::Procps => parse_pgrep_procps(all, pkill),
    }
}

fn parse_killall(args: &[Word], dialect: Dialect) -> Parsed<KillallSpec> {
    match dialect {
        Dialect::Bsd => parse_killall_bsd(args),
        Dialect::Procps => parse_killall_procps(args),
    }
}

fn parse_pgrep_bsd(all: &[Word], pkill: bool) -> Parsed<PgrepSpec> {
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
        dialect: Dialect::Bsd,
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

fn parse_killall_bsd(args: &[Word]) -> Parsed<KillallSpec> {
    let classified = match classify(args, "dehlmsvqzI", "utc", false, true) {
        Ok(c) => c,
        Err(Reject::Usage(why)) => return Parsed::NoEffect(why),
        Err(Reject::Unevaluable(why)) => return Parsed::Unevaluable(why),
    };
    let mut spec = KillallSpec {
        dialect: Dialect::Bsd,
        ..KillallSpec::default()
    };
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

/* ── 2b. Reading the Linux tools' arguments ────────────────────────────────── */

/// One `getopt_long` option: its long name, the letter it stands for, and
/// whether it takes an argument.
struct Long {
    name: &'static str,
    letter: char,
    arg: bool,
}

const fn long(name: &'static str, letter: char, arg: bool) -> Long {
    Long { name, letter, arg }
}

/// `longopts[]` of procps-ng 4.0.4 `pgrep.c`, which `pkill` shares: a long
/// option works in `pkill` even where its short letter does not (`--inverse`).
/// `signal`, `cgroup`, `ns` and `nslist` have no short letter; they are given
/// private ones that no short option string accepts.
const PGREP_LONGS: [Long; 32] = [
    long("signal", 'S', true),
    long("ignore-ancestors", 'A', false),
    long("require-handler", 'H', false),
    long("count", 'c', false),
    long("cgroup", 'C', true),
    long("delimiter", 'd', true),
    long("list-name", 'l', false),
    long("list-full", 'a', false),
    long("full", 'f', false),
    long("pgroup", 'g', true),
    long("group", 'G', true),
    long("ignore-case", 'i', false),
    long("newest", 'n', false),
    long("oldest", 'o', false),
    long("older", 'O', true),
    long("parent", 'P', true),
    long("session", 's', true),
    long("terminal", 't', true),
    long("euid", 'u', true),
    long("uid", 'U', true),
    long("inverse", 'v', false),
    long("lightweight", 'w', false),
    long("exact", 'x', false),
    long("pidfile", 'F', true),
    long("logpidfile", 'L', false),
    long("echo", 'e', false),
    long("ns", 'N', true),
    long("nslist", 'M', true),
    long("queue", 'q', true),
    long("runstates", 'r', true),
    long("help", 'h', false),
    long("version", 'V', false),
];

/// `options[]` of psmisc 23.7 `killall.c`. It reads them with
/// `getopt_long_only`, so a single dash also introduces a long name.
const KILLALL_LONGS: [Long; 16] = [
    long("exact", 'e', false),
    long("ignore-case", 'I', false),
    long("process-group", 'g', false),
    long("younger-than", 'y', true),
    long("older-than", 'o', true),
    long("interactive", 'i', false),
    long("list-signals", 'l', false),
    long("quiet", 'q', false),
    long("regexp", 'r', false),
    long("signal", 's', true),
    long("user", 'u', true),
    long("verbose", 'v', false),
    long("wait", 'w', false),
    long("ns", 'n', true),
    long("context", 'Z', true),
    long("version", 'V', false),
];

/// What a permuting `getopt_long` read: the options in order, and the
/// operands as indexes into the words it was given.
struct Gnu {
    opts: Vec<(char, Option<String>)>,
    operands: Vec<usize>,
}

/// The option `name` names: exactly, or as the one option it abbreviates.
fn resolve_long<'a>(name: &str, longs: &'a [Long]) -> Result<&'a Long, String> {
    if let Some(found) = longs.iter().find(|l| l.name == name) {
        return Ok(found);
    }
    let mut candidates = longs.iter().filter(|l| l.name.starts_with(name));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) if !name.is_empty() => Ok(only),
        (None, _) => Err(format!("`{name}` is not an option GitPulse reads")),
        _ => Err(format!("`{name}` abbreviates more than one option")),
    }
}

/// `getopt_long` over `words` as GNU reads it: options anywhere among the
/// operands, `--` ends them, clusters of short flags, an argument attached or
/// in the next word. `flags` and `with_arg` are the short letters, `longs` the
/// long names. With `long_only` a single dash that begins a long name is one
/// (`getopt_long_only`); with `signals` a single-dash word that is a signal is
/// the option `S`. A letter in `alone` is read only as a word of its own.
/// Anything this does not model is [`Reject::Unevaluable`], never a usage
/// error: whether a word is a signal or an unknown option is not worth a guess.
fn gnu_options(
    words: &[&Word],
    flags: &str,
    with_arg: &str,
    longs: &[Long],
    long_only: bool,
    signals: bool,
    alone: &str,
) -> Result<Gnu, Reject> {
    let mut out = Gnu {
        opts: Vec::new(),
        operands: Vec::new(),
    };
    let mut i = 0;
    while i < words.len() {
        let t = words[i].text.as_str();
        if t == "--" {
            out.operands.extend(i + 1..words.len());
            break;
        }
        if t.len() < 2 || !t.starts_with('-') {
            out.operands.push(i);
            i += 1;
            continue;
        }
        let body = &t[1..];
        if !t.starts_with("--") && signals && signal_spec(body).is_some() {
            out.opts.push(('S', Some(body.to_string())));
            i += 1;
            continue;
        }
        let long_text = if let Some(rest) = t.strip_prefix("--") {
            Some(rest)
        } else if long_only
            && body.chars().count() > 1
            && longs
                .iter()
                .any(|l| l.name.starts_with(body.split('=').next().unwrap_or(body)))
        {
            Some(body)
        } else {
            None
        };
        if let Some(text) = long_text {
            let (name, value) = match text.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (text, None),
            };
            let found = resolve_long(name, longs).map_err(Reject::Unevaluable)?;
            let arg = match (found.arg, value) {
                (true, Some(value)) => Some(value.to_string()),
                (true, None) => {
                    i += 1;
                    let next = words.get(i).ok_or_else(|| {
                        Reject::Usage(format!("option `--{}` requires an argument", found.name))
                    })?;
                    Some(next.text.clone())
                }
                (false, None) => None,
                (false, Some(_)) => {
                    return Err(Reject::Unevaluable(format!(
                        "`--{}` takes no argument but was given one",
                        found.name
                    )))
                }
            };
            out.opts.push((found.letter, arg));
            i += 1;
            continue;
        }
        let letters: Vec<char> = body.chars().collect();
        for (k, c) in letters.iter().enumerate() {
            if letters.len() > 1 && alone.contains(*c) {
                return Err(Reject::Unevaluable(format!(
                    "`{t}` puts `-{c}` in a cluster, which the tool reads by hand"
                )));
            }
            if with_arg.contains(*c) {
                let rest: String = letters[k + 1..].iter().collect();
                if rest.is_empty() {
                    i += 1;
                    let next = words.get(i).ok_or_else(|| {
                        Reject::Usage(format!("option requires an argument -- {c}"))
                    })?;
                    out.opts.push((*c, Some(next.text.clone())));
                } else {
                    out.opts.push((*c, Some(rest)));
                }
                break;
            } else if flags.contains(*c) {
                out.opts.push((*c, None));
            } else {
                return Err(Reject::Unevaluable(format!(
                    "`{t}` is not an option GitPulse reads, and the Linux tools also read a \
                     signal written like one"
                )));
            }
        }
        i += 1;
    }
    Ok(out)
}

/// A signal as procps' `signal_name_to_number` reads it: a name with or
/// without `SIG`, `RTMIN+n`, or a number up to `127 - SIGRTMIN`. `Some(true)`
/// is signal 0, which only tests that a process exists. Wider than the tool:
/// a word read as a signal here that the tool refuses sends nothing anyway.
fn procps_signal(spec: &str) -> Option<bool> {
    const SIGRTMIN: u64 = 34;
    let upper = spec.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    if matches!(name, "EXIT" | "NULL") {
        return Some(true);
    }
    if SIGNALS.contains(&name) {
        return Some(false);
    }
    let (offset, digits) = match name.strip_prefix("RTMIN+") {
        Some(rest) => (SIGRTMIN, rest),
        None => (0, name),
    };
    let digits = digits.trim_start();
    let (negative, digits) = match digits.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, digits.strip_prefix('+').unwrap_or(digits)),
    };
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    if (negative && value != 0) || value + SIGRTMIN > 127 {
        return None;
    }
    Some(value + offset == 0)
}

/// Why an option of the Linux tools cannot be evaluated, for the ones that
/// select by something the process table does not carry.
fn procps_unmodelled(letter: char) -> &'static str {
    match letter {
        'w' => "`-w` lists threads, which are not in the process table GitPulse reads",
        'H' => "`-H` selects by signal handler, which is not in the process table GitPulse reads",
        'O' => "`-O` selects by process age, which GitPulse does not evaluate",
        's' => "`-s` selects by session id, which is not in the process table GitPulse reads",
        'G' => "`-G` selects by group id, which is not in the process table GitPulse reads",
        't' => "`-t` selects by terminal, which is not in the process table GitPulse reads",
        'r' => "`-r` selects by run state, which is not in the process table GitPulse reads",
        'C' => {
            "`--cgroup` selects by control group, which is not in the process table GitPulse reads"
        }
        _ => {
            "`--ns` and `--nslist` select by namespace, which is not in the process table \
              GitPulse reads"
        }
    }
}

/// procps `pgrep` and `pkill`, as `pgrep.c` of procps-ng 4.0.4 reads them.
fn parse_pgrep_procps(all: &[Word], pkill: bool) -> Parsed<PgrepSpec> {
    // Any word could be an option or a signal wherever it stands, so a word
    // GitPulse cannot expand is not something to guess at.
    if all.iter().any(|w| w.dynamic) {
        return Parsed::Unevaluable(
            "an argument is built from a variable GitPulse cannot expand".into(),
        );
    }
    let mut words: Vec<&Word> = all.iter().collect();
    if pkill {
        // `signal_option`: the first word anywhere, not only the first, that is
        // a dash and a signal is the signal and is taken out before `getopt`.
        let found = words
            .iter()
            .position(|w| w.text.starts_with('-') && procps_signal(&w.text[1..]).is_some());
        if let Some(at) = found {
            if procps_signal(&words[at].text[1..]) == Some(true) {
                return Parsed::NoEffect("signal 0 only tests for existence".into());
            }
            words.remove(at);
        }
    }
    let (flags, with_arg) = if pkill {
        ("eLcfinoxAH?Vh", "qFPOgsuUGtr")
    } else {
        ("lavwLcfinoxAH?Vh", "dFPOgsuUGtr")
    };
    let read = match gnu_options(&words, flags, with_arg, &PGREP_LONGS, false, false, "") {
        Ok(read) => read,
        Err(Reject::Usage(why)) => return Parsed::NoEffect(why),
        Err(Reject::Unevaluable(why)) => return Parsed::Unevaluable(why),
    };
    let mut spec = PgrepSpec {
        prog: if pkill { "pkill" } else { "pgrep" },
        dialect: Dialect::Procps,
        ..PgrepSpec::default()
    };
    let mut criteria = false;
    let mut lock = false;
    for (flag, arg) in &read.opts {
        let arg = arg.clone().unwrap_or_default();
        match flag {
            'f' => spec.full = true,
            'i' => spec.icase = true,
            'x' => spec.exact = true,
            'v' => spec.inverse = true,
            'A' => spec.ignore_ancestors = true,
            'n' => {
                criteria = true;
                spec.newest = true;
            }
            'o' => {
                criteria = true;
                spec.oldest = true;
            }
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
            'L' => lock = true,
            // What is printed, or how the signal is sent, not which processes.
            'c' | 'l' | 'a' | 'e' | 'd' | 'q' => {}
            'S' => {
                if procps_signal(&arg) == Some(true) {
                    return Parsed::NoEffect("signal 0 only tests for existence".into());
                }
            }
            'h' | '?' | 'V' => {
                return Parsed::NoEffect("it prints usage or a version and signals nothing".into())
            }
            other => return Parsed::Unevaluable(procps_unmodelled(*other).into()),
        }
    }
    if lock && spec.pidfile.is_none() {
        return Parsed::NoEffect("-L without -F is a usage error".into());
    }
    if read.operands.len() > 1 {
        return Parsed::NoEffect("only one pattern can be provided".into());
    }
    if read.operands.is_empty() && !criteria {
        return Parsed::NoEffect("no matching criteria specified".into());
    }
    for &i in &read.operands {
        let text = &words[i].text;
        if text.chars().count() > MAX_PATTERN_CHARS {
            return Parsed::Unevaluable(format!("a pattern longer than {MAX_PATTERN_CHARS}"));
        }
        spec.patterns.push(text.clone());
    }
    match parse_pgrep_bsd(all, pkill) {
        Parsed::Spec(mut bsd) if !bsd.late.is_empty() => {
            bsd.dialect = Dialect::Procps;
            spec.late = bsd.late.clone();
            spec.corrected = bsd.corrected.clone();
            spec.alt = Some(Box::new(bsd));
        }
        Parsed::Unevaluable(why) => return Parsed::Unevaluable(why),
        _ => {}
    }
    Parsed::Spec(spec)
}

/// psmisc `killall`, as `killall.c` of psmisc 23.7 reads it.
fn parse_killall_procps(args: &[Word]) -> Parsed<KillallSpec> {
    if args.iter().any(|w| w.dynamic) {
        return Parsed::Unevaluable(
            "an argument is built from a variable GitPulse cannot expand".into(),
        );
    }
    let words: Vec<&Word> = args.iter().collect();
    let read = match gnu_options(
        &words,
        "egilqrvwVI",
        "yosuZn",
        &KILLALL_LONGS,
        true,
        true,
        "IV",
    ) {
        Ok(read) => read,
        Err(Reject::Usage(why)) => return Parsed::NoEffect(why),
        Err(Reject::Unevaluable(why)) => return Parsed::Unevaluable(why),
    };
    let mut spec = KillallSpec {
        dialect: Dialect::Procps,
        ..KillallSpec::default()
    };
    let mut sends = true;
    let mut zero = false;
    for (flag, arg) in &read.opts {
        match flag {
            // They narrow the match, or change how the signal is waited for.
            'e' | 'i' | 'q' | 'v' | 'w' => {}
            'r' => spec.regex = true,
            'I' => spec.icase = true,
            'u' => spec.user = arg.clone(),
            'l' | 'V' => sends = false,
            's' | 'S' => zero = arg.as_deref().and_then(procps_signal) == Some(true),
            'g' => {
                return Parsed::Unevaluable(
                    "`-g` signals the whole process group of each match, which reaches \
                     processes the name did not"
                        .into(),
                )
            }
            'y' | 'o' => {
                return Parsed::Unevaluable(
                    "`-y` and `-o` select by process age, which GitPulse does not evaluate".into(),
                )
            }
            'n' => {
                return Parsed::Unevaluable(
                    "`-n` selects by namespace, which is not in the process table GitPulse reads"
                        .into(),
                )
            }
            _ => {
                return Parsed::Unevaluable(
                    "`-Z` selects by security context, which is not in the process table \
                     GitPulse reads"
                        .into(),
                )
            }
        }
    }
    if !sends {
        return Parsed::NoEffect("-l and -V send no signal".into());
    }
    if zero {
        return Parsed::NoEffect("signal 0 only tests for existence".into());
    }
    if read.operands.is_empty() && spec.user.is_none() {
        return Parsed::NoEffect("no process to select is a usage error".into());
    }
    for &i in &read.operands {
        let text = &words[i].text;
        if text.chars().count() > MAX_PATTERN_CHARS {
            return Parsed::Unevaluable(format!("a name longer than {MAX_PATTERN_CHARS}"));
        }
        if !spec.regex && text.contains('/') {
            return Parsed::Unevaluable(format!(
                "the name `{text}` is a path, which killall matches against the executable's \
                 inode, not the table GitPulse reads"
            ));
        }
        spec.names.push(text.clone());
    }
    match parse_killall_bsd(args) {
        Parsed::Spec(mut bsd) if !bsd.late.is_empty() => {
            bsd.dialect = Dialect::Procps;
            spec.late = bsd.late.clone();
            spec.corrected = bsd.corrected.clone();
            spec.alt = Some(Box::new(bsd));
        }
        Parsed::Unevaluable(why) => return Parsed::Unevaluable(why),
        _ => {}
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
    /// The effective uid of the sender, when it is not the real one. `None`
    /// reads as the real uid.
    pub(crate) me_euid: Option<u32>,
    /// This process: never a match, as `pkill` never matches itself.
    pub(crate) self_pid: i32,
    pub(crate) users: HashMap<String, u32>,
    pub(crate) pgids: HashMap<i32, i32>,
    pub(crate) pidfiles: HashMap<String, Result<i32, String>>,
    /// Executable paths a `pidof` was given, as the file system resolves them
    /// (symbolic links followed): `pidof` compares the file, not the spelling.
    pub(crate) exes: HashMap<String, String>,
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
fn effective_uid() -> Option<u32> {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_uid() -> Option<u32> {
    None
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
        me_euid: effective_uid(),
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
    facts.exes = resolved_executables(&plan.exe_paths());
    Ok(facts)
}

/// Each executable path as the file system resolves it (symbolic links
/// followed, `.` and `..` folded); a path that does not resolve is left out.
fn resolved_executables(paths: &[String]) -> HashMap<String, String> {
    paths
        .iter()
        .filter_map(|path| {
            let resolved = std::fs::canonicalize(path).ok()?;
            Some((path.clone(), resolved.to_string_lossy().into_owned()))
        })
        .collect()
}

/// A signal from this user reaches the process: the sender is root, or the
/// `kill(2)` rule holds (the sender's real or effective uid is the target's
/// real or saved uid), or the process runs as this user, the one thing the
/// table has always counted. The last clause is why a process the kernel makes
/// root-owned in `/proc` (not dumpable) is still counted when it is this
/// user's, and why a set-uid process that runs as this user is too: this can
/// only count more processes, never fewer.
fn reachable(facts: &Facts, row: &TableRow) -> bool {
    let senders = [facts.me_uid, facts.me_euid.unwrap_or(facts.me_uid)];
    senders.contains(&0)
        || row.uid == facts.me_uid
        || senders
            .iter()
            .any(|s| *s == row.real_uid() || *s == row.saved_uid())
}

/// Whether the sender is root (by its real or effective uid), which can signal
/// every process.
fn sender_is_root(facts: &Facts) -> bool {
    [facts.me_uid, facts.me_euid.unwrap_or(facts.me_uid)].contains(&0)
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

/// `argv[0]` of a process whose arguments were read and are not empty.
fn argv0(row: &TableRow) -> Option<&str> {
    row.argv
        .as_ref()
        .and_then(|a| a.first())
        .map(String::as_str)
        .filter(|a| !a.is_empty())
}

/// The longest process name the Linux kernel keeps (`comm`, 15 bytes).
const KERNEL_COMM: usize = 15;

/// What a name pattern is tried against for one process, given the name the
/// kernel reports (`comm`) and `argv[0]` when its arguments could be read.
///
/// macOS `pgrep` was observed to use `argv[0]`'s last component, not the
/// kernel's name. Linux procps `pgrep`/`pkill` use `comm` (from
/// `/proc/<pid>/stat`, the executable's basename cut at 15 bytes, **not**
/// `argv[0]`), and psmisc `killall` uses `comm` too, falling back to the
/// command line only when `comm` is cut. So on Linux a login shell run as
/// `-zsh` is `zsh` to `pgrep -x zsh`, and a program that rewrites its title is
/// still its executable's name. The guard tries both: a pattern that matches
/// either is a hit, which is every process either tool would pick and can
/// only be more.
fn name_subjects(dialect: Dialect, comm: &str, argv0: Option<&str>) -> Vec<String> {
    let from_argv = argv0
        .map(base_name)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    match dialect {
        Dialect::Bsd => vec![from_argv.unwrap_or_else(|| comm.to_string())],
        Dialect::Procps => {
            let mut subjects = vec![comm.to_string()];
            subjects.extend(from_argv.filter(|name| name != comm));
            subjects
        }
    }
}

/// Whether `killall NAME` (no `-r`) picks a process. macOS compares the name
/// with the process name exactly. psmisc compares it with `comm` exactly,
/// and when `comm` is cut at 15 bytes and the name is that long or longer it
/// compares the first 15 bytes (without `-e`) or the name with the command
/// line's. The guard also accepts `argv[0]`'s last component, so it picks
/// every process either reading would.
fn killall_literal(
    dialect: Dialect,
    name: &str,
    icase: bool,
    comm: &str,
    argv0: Option<&str>,
) -> bool {
    let same = |a: &str, b: &str| {
        if icase {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    if name_subjects(dialect, comm, argv0)
        .iter()
        .any(|subject| same(name, subject))
    {
        return true;
    }
    dialect == Dialect::Procps
        && comm.len() >= KERNEL_COMM
        && name.len() >= KERNEL_COMM
        && name.get(..comm.len()).is_some_and(|head| same(head, comm))
}

/// The macOS name of a process, for the examples a refusal names.
fn match_name(row: &TableRow) -> String {
    name_subjects(Dialect::Bsd, &row.name, argv0(row)).remove(0)
}

fn match_full(row: &TableRow) -> String {
    match &row.argv {
        Some(argv) if !argv.is_empty() => argv.join(" "),
        _ => row.name.clone(),
    }
}

/// What a `-f` pattern is tried against for one process. macOS joins the
/// arguments with a space. The procps tools do too, but an empty argument
/// between two others is a double space to them, and a process that blanked
/// its argument area is a single `?`; the guard tries those readings as well as
/// the one it always has (the arguments that are not empty, or the name).
///
/// `foreign` is a process that is not the sender's own, whose arguments only a
/// root sender reads. Until it did, such a process was matched by its name
/// alone; the name stays one of its subjects, so no process that reading
/// picked is let go.
fn full_subjects(dialect: Dialect, row: &TableRow, foreign: bool) -> Vec<String> {
    let mut subjects = if dialect == Dialect::Procps {
        let present = |argv: &[String]| {
            argv.iter()
                .filter(|a| !a.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        };
        // The reading the guard has always had: the arguments that are not
        // empty, or the name when there are none.
        let mut subjects = vec![match &row.argv {
            Some(argv) if argv.iter().any(|a| !a.is_empty()) => present(argv),
            _ => row.name.clone(),
        }];
        match &row.argv {
            Some(argv) if argv.is_empty() => subjects.push("?".to_string()),
            Some(argv) => subjects.push(argv.join(" ")),
            None => {}
        }
        subjects
    } else {
        vec![match_full(row)]
    };
    if foreign {
        subjects.push(row.name.clone());
    }
    // procps shows `-f` the first 131071 bytes of a command line (probed:
    // `pgrep -a` prints that many, and an end anchor sees the cut), where the
    // guard reads a mebibyte. A cut copy is one more reading.
    if dialect == Dialect::Procps {
        let cut: Vec<String> = subjects
            .iter()
            .filter(|s| s.len() > PROCPS_CMDLINE_CUT)
            .map(|s| {
                let mut end = PROCPS_CMDLINE_CUT;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                s[..end].to_string()
            })
            .collect();
        subjects.extend(cut);
    }
    subjects.dedup();
    subjects
}

/// How much of a command line the procps tools match `-f` against.
const PROCPS_CMDLINE_CUT: usize = 131_071;

/// A pattern as the guard reads it.
///
/// The tools match with glibc's `regcomp` (POSIX extended) and the guard with
/// the `regex` crate, so on Linux the guard never answers from one reading of a
/// subject or a pattern when the tools may read it another way that picks a
/// process the first would not:
///
/// * `.` also matches a newline (`dot_matches_new_line`), as it does to glibc;
/// * the procps tools do not hand the pattern the raw command line but one
///   with its control characters replaced, and which replacement depends on
///   the locale: newline becomes a space everywhere; in a UTF-8 locale every
///   other control character and every invalid byte becomes `?`; in the C
///   locale a control character becomes `.` and every byte outside ASCII,
///   valid or not, a `?` of its own. A subject is tried as written and in both
///   readings ([`Pattern::readings`]);
/// * a pattern whose syntax means one thing to `regcomp` and another to the
///   `regex` crate carries [`Pattern::uncertain`], and the command is asked
///   about, never passed, when nothing else refuses it ([`ere_uncertainty`]).
///
/// Every reading only adds processes to the set the old reading picked.
struct Pattern {
    regex: Regex,
    /// Why `regcomp` may read this pattern another way than the `regex` crate.
    uncertain: Option<String>,
    /// The pattern can match a character it does not spell (`.`, a bracket).
    consumes_chars: bool,
}

fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

fn compile(dialect: Dialect, pattern: &str, icase: bool, exact: bool) -> Result<Pattern, String> {
    let source = if exact {
        format!("^(?:{pattern})$")
    } else {
        pattern.to_string()
    };
    let procps = dialect == Dialect::Procps;
    let regex = RegexBuilder::new(&source)
        .case_insensitive(icase)
        .dot_matches_new_line(procps)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("the pattern `{pattern}` is not one GitPulse can evaluate ({e})"))?;
    let mut uncertain = procps.then(|| ere_uncertainty(pattern)).flatten();
    if procps && icase && !pattern.is_ascii() && uncertain.is_none() {
        uncertain = Some(
            "`-i` folds case by the locale's rules for a pattern outside ASCII (`ı` is an `I` to \
             glibc), which GitPulse's engine does differently"
                .to_string(),
        );
    }
    Ok(Pattern {
        regex,
        uncertain,
        consumes_chars: pattern_consumes_chars(pattern),
    })
}

/// Whether a pattern can match a character it does not spell: a `.`, a bracket
/// expression, or a literal `?` (what the procps tools show for a byte that is
/// not valid UTF-8). Where the tools and the guard count the characters of a
/// subject differently, only such a pattern sees the difference.
fn pattern_consumes_chars(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if chars.next() == Some('?') {
                    return true;
                }
            }
            '.' | '[' => return true,
            _ => {}
        }
    }
    false
}

/// Why a process the guard did not pick (or left out of an inverted match) may
/// be one the procps tools pick: the subject is read in a way the guard cannot
/// reproduce. Asked about, not guessed at. `None` on macOS.
fn row_uncertainty(
    icase: bool,
    dialect: Dialect,
    patterns: &[Pattern],
    subjects: &[String],
) -> Option<String> {
    if dialect != Dialect::Procps {
        return None;
    }
    if icase && subjects.iter().any(|s| !s.is_ascii()) {
        return Some(
            "`-i` folds case by the locale's rules for a name or command line outside ASCII \
             (`ı` is an `I` to glibc), which GitPulse's engine does differently"
                .to_string(),
        );
    }
    if subjects
        .iter()
        .any(|s| s.contains(char::REPLACEMENT_CHARACTER))
        && patterns.iter().any(|p| p.consumes_chars)
    {
        return Some(
            "a name or command line holds bytes that are not valid UTF-8: the procps tools \
             count them one character at a time (or as one, for a surrogate), and GitPulse holds \
             them replaced, so a `.` or a bracket may count differently"
                .to_string(),
        );
    }
    None
}

impl Pattern {
    /// Each way the tools may see `subject`: as written, and as the procps
    /// tools show it in a UTF-8 locale and in the C locale. macOS has one.
    fn readings<'a>(subject: &'a str, dialect: Dialect) -> Vec<std::borrow::Cow<'a, str>> {
        let mut readings = vec![std::borrow::Cow::Borrowed(subject)];
        if dialect != Dialect::Procps || is_printable_ascii(subject) {
            return readings;
        }
        let utf8_locale: String = subject
            .chars()
            .map(|c| match c {
                '\n' => ' ',
                c if c.is_control() || c == char::REPLACEMENT_CHARACTER => '?',
                c => c,
            })
            .collect();
        readings.push(utf8_locale.into());
        let mut c_locale = String::with_capacity(subject.len());
        for c in subject.chars() {
            match c {
                '\n' => c_locale.push(' '),
                char::REPLACEMENT_CHARACTER => c_locale.push('?'),
                c if c.is_ascii_control() => c_locale.push('.'),
                c if c.is_ascii() => c_locale.push(c),
                c => c_locale.extend(std::iter::repeat_n('?', c.len_utf8())),
            }
        }
        readings.push(c_locale.into());
        readings
    }

    /// Whether the pattern picks `subject` under any reading of it: the most
    /// processes either tool could pick.
    fn is_match(&self, subject: &str, dialect: Dialect) -> bool {
        self.regex.is_match(subject)
            || (dialect == Dialect::Procps
                && !is_printable_ascii(subject)
                && Self::readings(subject, dialect)
                    .iter()
                    .any(|text| self.regex.is_match(text)))
    }

    /// Whether the pattern picks `subject` under every reading, which is when
    /// an inverted match (`-v`) is certain to leave the process out.
    fn is_match_in_every_reading(&self, subject: &str, dialect: Dialect) -> bool {
        Self::readings(subject, dialect)
            .iter()
            .all(|text| self.regex.is_match(text))
    }
}

/// Why glibc's `regcomp` (POSIX extended) may read `pattern` another way than
/// the `regex` crate, or `None` when the syntax is the common one: literal
/// characters, `.`, the repetitions, alternation, groups, anchors, valid
/// intervals, bracket expressions of characters, ranges and `[:class:]` names,
/// and a backslash before one of the characters that mean something. The rest
/// reads differently (`\d`, `\w`, `\b`, `\<`, `\n`, `\1` are GNU escapes,
/// back-references or plain letters to `regcomp` and classes, boundaries,
/// control characters or errors to the crate; a backslash in a bracket is
/// literal to one and an escape to the other; `[[.x.]]`, `[[=x=]]`, `&&`, `--`
/// and `~~` in a bracket are collation and class operations to one and
/// characters to the other). Over-reporting is the safe direction: it asks.
fn ere_uncertainty(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => match chars.get(i + 1) {
                Some(c) if ".[]()*+?{}|^$\\".contains(*c) => i += 2,
                Some(c) => {
                    return Some(format!(
                        "`\\{c}` is a different thing to glibc's regcomp, which the tools use, \
                         than to the regular-expression engine GitPulse evaluates with"
                    ))
                }
                None => return Some("the pattern ends in a backslash".to_string()),
            },
            '[' => match bracket_end(&chars, i) {
                Ok(next) => i = next,
                Err(why) => return Some(why),
            },
            '{' => match interval_end(&chars, i) {
                Some(next) => i = next,
                None => {
                    return Some(
                        "a `{` that does not open a valid repetition is a literal to some \
                         regular-expression engines and an error to others"
                            .to_string(),
                    )
                }
            },
            _ => i += 1,
        }
    }
    None
}

/// The index after a valid `{n}`, `{n,}`, `{n,m}` or `{,m}` that opens at
/// `start`.
fn interval_end(chars: &[char], start: usize) -> Option<usize> {
    let close = chars[start..].iter().position(|c| *c == '}')? + start;
    let body: String = chars[start + 1..close].iter().collect();
    let (low, high) = match body.split_once(',') {
        Some((low, high)) => (low, Some(high)),
        None => (body.as_str(), None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let valid = match high {
        None => !low.is_empty() && digits(low),
        Some(high) => digits(low) && digits(high) && !(low.is_empty() && high.is_empty()),
    };
    valid.then_some(close + 1)
}

/// The index after the bracket expression that opens at `start`, or why it
/// cannot be read the same way by both engines.
fn bracket_end(chars: &[char], start: usize) -> Result<usize, String> {
    let unlike = |what: &str| {
        Err(format!(
            "{what} in a bracket expression is read differently by glibc's regcomp, which the \
             tools use, and by the regular-expression engine GitPulse evaluates with"
        ))
    };
    let mut i = start + 1;
    if chars.get(i) == Some(&'^') {
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        i += 1;
    }
    while let Some(&c) = chars.get(i) {
        match c {
            ']' => return Ok(i + 1),
            '\\' => return unlike("a backslash"),
            // A class (`[:alpha:]`) is ASCII to the crate and the locale's to
            // glibc, which in a UTF-8 locale also takes `é`, `α` and `—`.
            '[' => match chars.get(i + 1) {
                Some(':') => return unlike("a POSIX character class"),
                Some('.' | '=') => return unlike("a collating symbol or an equivalence class"),
                _ => return unlike("a `[`"),
            },
            '&' | '-' | '~' if chars.get(i + 1) == Some(&c) => return unlike(&format!("`{c}{c}`")),
            c if !c.is_ascii() => return unlike("a character outside ASCII"),
            _ => i += 1,
        }
    }
    Err("an unterminated bracket expression".to_string())
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
    /// Processes the signal could reach whose arguments could not be read,
    /// which a `-f` pattern could not be tested against.
    unread: usize,
    /// Why the set may be short: a pattern the tools may read another way than
    /// the guard does ([`Pattern::uncertain`]). What it reached is still real.
    uncertain: Option<String>,
}

impl<'a> Reach<'a> {
    /// What either reading reaches.
    fn union(self, other: Reach<'a>) -> Reach<'a> {
        let mut seen: std::collections::HashSet<i32> = self.hits.iter().map(|r| r.pid).collect();
        let mut hits = self.hits;
        hits.extend(other.hits.into_iter().filter(|r| seen.insert(r.pid)));
        Reach {
            hits,
            unread: self.unread.max(other.unread),
            uncertain: self.uncertain.or(other.uncertain),
        }
    }
}

fn eval_pgrep<'a>(spec: &PgrepSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let reach = eval_pgrep_reading(spec, facts)?;
    match &spec.alt {
        Some(alt) => Ok(reach.union(eval_pgrep_reading(alt, facts)?)),
        None => Ok(reach),
    }
}

fn eval_pgrep_reading<'a>(spec: &PgrepSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let patterns: Vec<Pattern> = spec
        .patterns
        .iter()
        .map(|p| compile(spec.dialect, p, spec.icase, spec.exact))
        .collect::<Result<_, _>>()?;
    let mut uncertain = patterns.iter().find_map(|p| p.uncertain.clone());
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
    // macOS `pkill` never matches itself or its ancestors; with `-v` the
    // ancestors are in the list again. procps `pkill` (Linux) skips only its
    // own pid unless `-A` is given, so an ancestor is a process the command
    // would signal. Leaving those out made `pkill -u $USER` look smaller than
    // the set it reaches, and a command that would hit them was not asked
    // about. Counting them refuses more commands; it does not allow one the
    // guard already refused. "Itself" is the hook, a sibling of the shell the
    // tool would run in.
    let leave_out_ancestors = if spec.dialect.skips_ancestors() {
        !(spec.ancestors || spec.inverse)
    } else {
        spec.ignore_ancestors
    };
    let excluded: Vec<i32> = if leave_out_ancestors {
        lineage(facts)
    } else {
        vec![facts.self_pid]
    };
    let mut hits = Vec::new();
    let mut unread = 0;
    for row in &facts.rows {
        if !reachable(facts, row) || excluded.contains(&row.pid) {
            continue;
        }
        let subject_unread = spec.full && row.argv.is_none();
        let subjects = if patterns.is_empty() {
            Vec::new()
        } else if spec.full {
            let own = [row.uid, row.real_uid(), row.saved_uid()].contains(&facts.me_uid);
            full_subjects(spec.dialect, row, !own)
        } else {
            name_subjects(spec.dialect, &row.name, argv0(row))
        };
        let matched = patterns.is_empty() || {
            if spec.inverse {
                // Inverted, a row is a hit when the tool's one subject does not
                // match. Which of the readings of it the tool has is not known,
                // so the row is out only when the pattern picks every subject
                // in every reading: never fewer hits than any of them.
                subjects.iter().all(|s| {
                    patterns
                        .iter()
                        .any(|p| p.is_match_in_every_reading(s, spec.dialect))
                })
            } else {
                subjects
                    .iter()
                    .any(|s| patterns.iter().any(|p| p.is_match(s, spec.dialect)))
            }
        };
        // A process the guard did not pick (or, inverted, left out) that the
        // tools may pick by reading its name or command line another way.
        if uncertain.is_none() && (!matched || spec.inverse) {
            uncertain = row_uncertainty(spec.icase, spec.dialect, &patterns, &subjects);
        }
        let filtered = matched
            && (euids.is_empty() || euids.contains(&row.uid))
            && (ruids.is_empty() || ruids.contains(&row.real_uid()) || ruids.contains(&row.uid))
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
    Ok(Reach {
        hits,
        unread,
        uncertain,
    })
}

fn eval_killall<'a>(spec: &KillallSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let reach = eval_killall_reading(spec, facts)?;
    match &spec.alt {
        Some(alt) => Ok(reach.union(eval_killall_reading(alt, facts)?)),
        None => Ok(reach),
    }
}

fn eval_killall_reading<'a>(spec: &KillallSpec, facts: &'a Facts) -> Result<Reach<'a>, String> {
    let patterns: Vec<Pattern> = if spec.regex {
        spec.names
            .iter()
            .map(|n| compile(spec.dialect, n, spec.icase, false))
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    let mut uncertain = patterns.iter().find_map(|p| p.uncertain.clone());
    let user = match &spec.user {
        Some(name) => uids(facts, std::slice::from_ref(name))?.first().copied(),
        None => None,
    };
    let mut hits = Vec::new();
    for row in &facts.rows {
        if row.pid == facts.self_pid || !reachable(facts, row) {
            continue;
        }
        // psmisc `-u` compares the real uid; the effective one is kept too, as
        // the table always compared it.
        if user.is_some_and(|u| u != row.real_uid() && u != row.uid) {
            continue;
        }
        if spec.only.as_ref().is_some_and(|c| *c != match_name(row)) {
            continue;
        }
        let by_name = if spec.regex {
            name_subjects(spec.dialect, &row.name, argv0(row))
                .iter()
                .any(|s| patterns.iter().any(|p| p.is_match(s, spec.dialect)))
        } else {
            spec.names
                .iter()
                .any(|n| killall_literal(spec.dialect, n, spec.icase, &row.name, argv0(row)))
        };
        let by_path = spec
            .exe_paths
            .iter()
            .any(|word| runs_executable(row, word, facts.exes.get(word)));
        if spec.names.is_empty() || by_name || by_path {
            hits.push(row);
        } else if uncertain.is_none() && !spec.names.is_empty() {
            let subjects = name_subjects(spec.dialect, &row.name, argv0(row));
            uncertain = row_uncertainty(spec.icase, spec.dialect, &patterns, &subjects);
        }
    }
    Ok(Reach {
        hits,
        unread: 0,
        uncertain,
    })
}

/// Whether `row` runs the executable `word` names: by the path as written or
/// as the file system resolves it (symbolic links followed), against the path
/// the kernel runs it from, the one it was exec'd with and `argv[0]`.
fn runs_executable(row: &TableRow, word: &str, resolved: Option<&String>) -> bool {
    [row.real_exe.as_deref(), row.exec.as_deref(), argv0(row)]
        .into_iter()
        .flatten()
        .any(|path| path == word || resolved.is_some_and(|r| r == path))
}

fn eval_group<'a>(group: i32, facts: &'a Facts) -> Reach<'a> {
    Reach {
        hits: facts
            .rows
            .iter()
            .filter(|r| reachable(facts, r) && facts.pgids.get(&r.pid) == Some(&group))
            .collect(),
        unread: 0,
        uncertain: None,
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
        "pkill" | "pgrep" => match parse_pgrep(&words, program == "pkill", Dialect::host()) {
            Parsed::Spec(spec) => Sel::Pgrep(spec),
            Parsed::NoEffect(why) | Parsed::Unevaluable(why) => return Err(why),
        },
        "killall" => match parse_killall(&words, Dialect::host()) {
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
    let reach = match &plan.findings[0].sel {
        Sel::Pgrep(spec) => eval_pgrep(spec, &facts)?,
        Sel::Killall(spec) => eval_killall(spec, &facts)?,
        _ => Reach {
            hits: Vec::new(),
            unread: 0,
            uncertain: None,
        },
    };
    // A set that may be short is not an answer to "which processes".
    if let Some(why) = reach.uncertain {
        return Err(why);
    }
    let mut pids: Vec<i32> = reach.hits.into_iter().map(|r| r.pid).collect();
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
    /// Processes whose executable could not be read or is the dynamic loader:
    /// what they are is not known.
    unknown_identity: usize,
    examples: Vec<String>,
}

/// Whether `row` is a GitPulse process. By the executable the kernel runs it
/// from, which `argv[0]` cannot change, and by the path it was exec'd with:
/// either one is enough, so a GitPulse executable run under another `argv[0]`
/// is still GitPulse, and so is any process that says it is. The kernel's name
/// for the process (`comm`, which `argv[0]` does not change) and, for a
/// program run through the dynamic loader (`ld.so PROGRAM`), its first
/// argument are looked at too.
fn is_gitpulse(row: &TableRow) -> bool {
    let first_arg = row
        .argv
        .as_ref()
        .and_then(|a| a.first())
        .map(String::as_str);
    let spelled = row.exec.as_deref().or(first_arg).unwrap_or(&row.name);
    let through_loader = row
        .real_exe
        .as_deref()
        .is_some_and(is_dynamic_loader)
        .then(|| row.argv.as_ref().and_then(|a| a.get(1)).map(String::as_str))
        .flatten();
    [
        row.real_exe.as_deref(),
        Some(spelled),
        Some(row.name.as_str()),
        through_loader,
    ]
    .into_iter()
    .flatten()
    .any(|program| {
        let name = base_name(program).to_ascii_lowercase();
        (name.starts_with("gitpulse") && name != "gitpulse-hook")
            || program.contains("/GitPulse.app/")
    })
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
        t.unknown_identity += usize::from(row.exe_unknown);
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
        let late = spec
            .late
            .iter()
            .map(|l| format!("`{l}`"))
            .collect::<Vec<_>>()
            .join(", ");
        if spec.dialect == Dialect::Procps {
            let macos = spec
                .alt
                .as_ref()
                .map_or(String::new(), |alt| format!(" {:?}", alt.patterns));
            text.push_str(&format!(
                " {late} came after the first pattern. Linux reads them as options; macOS \
                 option parsing stops at the first operand and reads everything after it as one \
                 more pattern OR'd with the first, the patterns{macos}. This counts the \
                 processes either reading would signal, which is how `pkill -f \"sleep 60\" -u \
                 user` can match every process with the user's name in its arguments."
            ));
        } else {
            text.push_str(&format!(
                " {late} came after the first pattern, and macOS option parsing stops at the \
                 first operand: everything after it is one more pattern OR'd with the first, \
                 not an option. That is how `pkill -f \"sleep 60\" -u user` matches every \
                 process with the user's name in its arguments."
            ));
        }
    }
    text
}

fn describe_killall(spec: &KillallSpec) -> String {
    let mut text = format!(
        "killall read {} {:?}{}",
        plural(spec.names.len(), "name", "names"),
        spec.names,
        if spec.regex && spec.dialect == Dialect::Procps {
            ", as regular expressions (-r)"
        } else if spec.regex {
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
        let late = spec
            .late
            .iter()
            .map(|l| format!("`{l}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let taken = if spec.late.len() == 1 {
            "it was"
        } else {
            "they were"
        };
        if spec.dialect == Dialect::Procps {
            text.push_str(&format!(
                " {late} came after the first name. Linux reads them as options; macOS option \
                 parsing stops at the first operand, so {taken} taken as more names. This \
                 counts the processes either reading would signal."
            ));
        } else {
            text.push_str(&format!(
                " {late} came after the first name, and option parsing stops at the first \
                 operand, so {taken} taken as more names."
            ));
        }
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
    if let Some(why) = &reach.uncertain {
        return Verdict::Ask(format!(
            "GitPulse could not tell which processes `{}` would signal: {why}.",
            finding.text
        ));
    }
    // A process the command reaches whose executable could not be read (not
    // dumpable, or a program run through the loader) may be GitPulse or an
    // agent under a spelling it chose: asked about, never passed by that spelling.
    if t.unknown_identity > 0 {
        return Verdict::Ask(format!(
            "`{}` reaches {} whose executable GitPulse could not read, so it cannot tell \
             whether {} GitPulse or an agent session.",
            finding.text,
            plural(t.unknown_identity, "process", "processes"),
            if t.unknown_identity == 1 {
                "it is"
            } else {
                "any is"
            },
        ));
    }
    // A sender that can signal every process has no process of its own to
    // fall back on: one whose command line a `-f` pattern was not tested
    // against may be exactly the one it picks.
    if reach.unread > 0 && sender_is_root(facts) {
        return Verdict::Ask(format!(
            "`{}` runs as root, so it can signal the {} process{} whose command line GitPulse \
             could not read, and `-f` was not tested against {}.",
            finding.text,
            reach.unread,
            if reach.unread == 1 { "" } else { "es" },
            if reach.unread == 1 { "it" } else { "them" },
        ));
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

    /// The tests below that do not name a dialect read the macOS tools, so
    /// they mean the same on every host the suite runs on. The Linux tools
    /// are read with [`Dialect::Procps`] explicitly.
    fn plan_with(command: &str, env: EnvFn) -> Plan {
        super::plan_with(command, env, Dialect::Bsd)
    }

    fn parse_pgrep(args: &[Word], pkill: bool) -> Parsed<PgrepSpec> {
        super::parse_pgrep(args, pkill, Dialect::Bsd)
    }

    /// The program a command line runs, with nothing bound.
    fn resolve(words: &[Word]) -> Result<Resolved<'_>, Unresolved> {
        super::resolve(words, &Binds::default())
    }

    fn row(pid: i32, ppid: i32, argv: &[&str]) -> TableRow {
        TableRow {
            pid,
            ppid,
            uid: ME,
            ruid: None,
            suid: None,
            start_us: u64::try_from(pid).unwrap_or(0) * 1000,
            name: base_name(argv[0]).to_string(),
            exec: Some(argv[0].to_string()),
            real_exe: None,
            exe_unknown: false,
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
        run_on(Dialect::Bsd, command, facts)
    }

    fn run_on(dialect: Dialect, command: &str, facts: &Facts) -> HookOutput {
        decide(&super::plan_with(command, fake_env, dialect), facts)
    }

    /// What the first invocation in `command` reaches, read as `dialect` does.
    fn pids_in(dialect: Dialect, command: &str, facts: &Facts) -> Vec<i32> {
        let plan = super::plan_with(command, fake_env, dialect);
        let reach = match &plan.findings.first().map(|f| &f.sel) {
            Some(Sel::Pgrep(spec)) => eval_pgrep(spec, facts),
            Some(Sel::Killall(spec)) => eval_killall(spec, facts),
            _ => panic!("not a pattern kill: {plan:?}"),
        }
        .expect("evaluates");
        let mut pids: Vec<i32> = reach.hits.iter().map(|r| r.pid).collect();
        pids.sort_unstable();
        pids
    }

    /// The selection the first invocation in `command` was read as.
    fn sel_in(dialect: Dialect, command: &str) -> Option<Sel> {
        super::plan_with(command, fake_env, dialect)
            .findings
            .into_iter()
            .next()
            .map(|f| f.sel)
    }

    fn reason_of(output: &HookOutput) -> String {
        output
            .hook_specific_output
            .as_ref()
            .and_then(|s| s.permission_decision_reason.clone())
            .unwrap_or_default()
    }

    fn pids_of(command: &str, facts: &Facts) -> Vec<i32> {
        pids_in(Dialect::Bsd, command, facts)
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
            "macOS pkill never signals its ancestors"
        );
        assert_eq!(
            classify_output(&run_in("pkill -a -f claude", &alone)),
            Out::Deny,
            "-a includes ancestors"
        );
    }

    #[test]
    fn on_linux_the_session_that_ran_it_is_a_victim_unless_a_is_given() {
        let facts = world();
        let pids = pids_in(Dialect::Procps, "pkill -f claude", &facts);
        assert!(
            pids.contains(&700),
            "procps reaches the ancestor session: {pids:?}"
        );
        assert_eq!(pids.len(), 6, "the ancestor and the five: {pids:?}");
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill -f claude", &quiet())),
            Out::Deny,
            "the ancestor is a hosted session procps would signal"
        );
        let ignored = pids_in(Dialect::Procps, "pkill -A -f claude", &facts);
        assert!(
            !ignored.contains(&700),
            "-A leaves ancestors out: {ignored:?}"
        );
        assert_eq!(ignored.len(), 5, "{ignored:?}");
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill -A -f claude", &quiet())),
            Out::Silent
        );
        // procps `-a` is --list-full: it prints, it does not change the match.
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -f claude", &facts),
            pids_in(Dialect::Procps, "kill $(pgrep -a -f claude)", &facts)
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
        assert!(!normal.contains(&900), "never itself");
        assert!(!normal.contains(&700) && !normal.contains(&800));
        let procps = pids_in(Dialect::Procps, "pkill -f .", &facts);
        assert!(
            procps.contains(&700) && procps.contains(&800) && !procps.contains(&900),
            "procps counts ancestors: {procps:?}"
        );
    }

    #[test]
    fn procps_counts_ancestors_and_macos_pkill_does_not() {
        assert!(Dialect::of("macos").skips_ancestors());
        assert!(!Dialect::of("linux").skips_ancestors());
        assert_eq!(Dialect::of("linux"), Dialect::Procps);
        assert_eq!(Dialect::of("freebsd"), Dialect::Bsd);
        assert_eq!(Dialect::host(), Dialect::of(std::env::consts::OS));
    }

    /* ── the Linux tools: names ── */

    fn named(pid: i32, comm: &str, argv: &[&str]) -> TableRow {
        let mut r = row(pid, 1, argv);
        r.name = comm.to_string();
        r
    }

    #[test]
    fn a_name_is_tried_as_comm_and_as_argv0_on_linux_and_as_argv0_alone_on_macos() {
        let subjects = |d, comm: &str, argv0: Option<&str>| name_subjects(d, comm, argv0);
        // The login shell: its executable is zsh, its argv[0] is `-zsh`.
        assert_eq!(
            subjects(Dialect::Procps, "zsh", Some("-zsh")),
            vec!["zsh", "-zsh"]
        );
        assert_eq!(subjects(Dialect::Bsd, "zsh", Some("-zsh")), vec!["-zsh"]);
        // A program that rewrote its title: comm stays the executable's name.
        assert_eq!(
            subjects(Dialect::Procps, "sleep", Some("/x/gpsg1x2")),
            vec!["sleep", "gpsg1x2"]
        );
        // The same name twice is one subject; no argv[0] leaves comm alone.
        assert_eq!(
            subjects(Dialect::Procps, "zsh", Some("/bin/zsh")),
            vec!["zsh"]
        );
        assert_eq!(subjects(Dialect::Procps, "zsh", None), vec!["zsh"]);
        assert_eq!(subjects(Dialect::Procps, "zsh", Some("")), vec!["zsh"]);
        assert_eq!(subjects(Dialect::Procps, "zsh", Some("/")), vec!["zsh"]);
        assert_eq!(subjects(Dialect::Bsd, "zsh", None), vec!["zsh"]);
        // comm is cut at 15 bytes; argv[0] is not.
        assert_eq!(
            subjects(
                Dialect::Procps,
                "averyverylongpr",
                Some("averyverylongprocess")
            ),
            vec!["averyverylongpr", "averyverylongprocess"]
        );
    }

    #[test]
    fn a_login_shell_run_as_dash_zsh_is_picked_by_pkill_x_zsh_on_linux() {
        let mut facts = quiet();
        facts.rows.push(named(2200, "zsh", &["-zsh"]));
        facts.rows.push(named(2201, "zsh", &["/bin/zsh", "-l"]));
        facts.rows.push(named(2202, "bash", &["-bash"]));
        // procps `pgrep -x zsh` finds the login shell: comm is `zsh`. (800 is
        // the sender's own shell, an ancestor procps counts.)
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -x zsh", &facts),
            vec![800, 2200, 2201]
        );
        // macOS matched argv[0], where it is `-zsh`, and left the ancestor out.
        assert_eq!(pids_in(Dialect::Bsd, "pkill -x zsh", &facts), vec![2201]);
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -x bash", &facts),
            vec![2202]
        );
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -x -- -bash", &facts),
            vec![2202]
        );
    }

    #[test]
    fn a_name_longer_than_comm_is_still_counted_on_linux_where_pgrep_finds_nothing() {
        let mut facts = quiet();
        facts.rows.push(named(
            2200,
            "averyverylongpr",
            &["/b/averyverylongprocessname"],
        ));
        // procps matches comm, which is cut at 15, so this finds nothing there;
        // the guard also tries argv[0] and counts it: the safe direction.
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -x averyverylongprocessname", &facts),
            vec![2200]
        );
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -x averyverylongpr", &facts),
            vec![2200]
        );
    }

    #[test]
    fn an_inverted_name_match_is_never_smaller_than_either_tools() {
        let mut facts = quiet();
        // comm differs from the pattern, argv[0] matches it.
        facts.rows.push(named(2200, "sleep", &["zsh"]));
        // comm matches, argv[0] differs.
        facts.rows.push(named(2201, "zsh", &["-zsh"]));
        // both match.
        facts.rows.push(named(2202, "zsh", &["zsh"]));
        let hits = pids_in(Dialect::Procps, "pkill --inverse -x zsh", &facts);
        assert!(hits.contains(&2200), "{hits:?}");
        assert!(hits.contains(&2201), "{hits:?}");
        assert!(!hits.contains(&2202), "{hits:?}");
    }

    #[test]
    fn killall_names_follow_psmisc_on_linux_and_the_process_name_on_macos() {
        let lit = killall_literal;
        assert!(lit(Dialect::Procps, "zsh", false, "zsh", Some("-zsh")));
        assert!(lit(
            Dialect::Procps,
            "gpsg",
            false,
            "sleep",
            Some("/x/gpsg")
        ));
        assert!(!lit(Dialect::Procps, "zs", false, "zsh", Some("-zsh")));
        // comm is cut at 15 bytes: a name that long or longer is compared
        // with its first 15 bytes, or with the command line's.
        assert!(lit(
            Dialect::Procps,
            "averyverylongprocessname",
            false,
            "averyverylongpr",
            None
        ));
        assert!(!lit(
            Dialect::Procps,
            "averyverylongprocessname",
            false,
            "differentlongnm",
            None
        ));
        assert!(!lit(
            Dialect::Procps,
            "averyverylong",
            false,
            "averyverylongpr",
            None
        ));
        // -I is case-blind, on both names and comm.
        assert!(lit(Dialect::Procps, "ZSH", true, "zsh", None));
        assert!(!lit(Dialect::Procps, "ZSH", false, "zsh", None));
        assert!(lit(
            Dialect::Procps,
            "AVERYVERYLONGPROCESS",
            true,
            "averyverylongpr",
            None
        ));
        // macOS: argv[0]'s last component, exactly.
        assert!(lit(Dialect::Bsd, "-zsh", false, "zsh", Some("-zsh")));
        assert!(!lit(Dialect::Bsd, "zsh", false, "zsh", Some("-zsh")));
        assert!(!lit(Dialect::Bsd, "sleep", false, "sleep", Some("/x/gpsg")));
    }

    /* ── the Linux tools: whose processes ── */

    #[test]
    fn a_signal_reaches_a_process_by_the_kill_rule_not_only_by_its_owner() {
        let facts = Facts {
            me_uid: ME,
            ..Facts::default()
        };
        let with = |uid, ruid, suid| TableRow {
            uid,
            ruid,
            suid,
            ..row(1, 0, &["x"])
        };
        // The kernel reports a process that is not dumpable as root's in
        // /proc; its credentials say it is this user's.
        assert!(reachable(&facts, &with(ME, Some(ME), Some(ME))));
        // Running as this user whatever the other ids say: always counted.
        assert!(reachable(&facts, &with(ME, Some(9), Some(9))));
        // Real or saved uid this user's: `kill(2)` permits it.
        assert!(reachable(&facts, &with(0, Some(ME), Some(0))));
        assert!(reachable(&facts, &with(7, Some(7), Some(ME))));
        // Nothing of this user's.
        assert!(!reachable(&facts, &with(7, Some(7), Some(7))));
        assert!(!reachable(&facts, &with(0, Some(0), Some(0))));
        // No ids beyond the one: that one decides, as before.
        assert!(reachable(&facts, &with(ME, None, None)));
        assert!(!reachable(&facts, &with(0, None, None)));
        // Root reaches everything; so does a sender whose effective uid is root.
        let root = Facts {
            me_uid: 0,
            ..Facts::default()
        };
        assert!(reachable(&root, &with(7, Some(7), Some(7))));
        let setuid = Facts {
            me_uid: ME,
            me_euid: Some(0),
            ..Facts::default()
        };
        assert!(reachable(&setuid, &with(7, Some(7), Some(7))));
        // The effective uid of the sender counts as well as the real one.
        let euid = Facts {
            me_uid: ME,
            me_euid: Some(7),
            ..Facts::default()
        };
        assert!(reachable(&euid, &with(0, Some(7), Some(0))));
    }

    #[test]
    fn user_filters_read_the_effective_and_the_real_uid_the_way_each_tool_does() {
        let mut facts = quiet();
        let mut shared = named(2300, "svc", &["svc"]);
        shared.uid = ME;
        shared.ruid = Some(0);
        shared.suid = Some(ME);
        facts.rows.push(shared);
        let mut dropped = named(2301, "svc2", &["svc2"]);
        dropped.uid = 77;
        dropped.ruid = Some(ME);
        dropped.suid = Some(77);
        facts.rows.push(dropped);
        let hit = |c: &str| pids_in(Dialect::Procps, c, &facts);
        // -u is the effective uid.
        assert_eq!(hit("pkill -u 501 -x svc"), vec![2300]);
        assert!(hit("pkill -u 501 -x svc2").is_empty());
        // -U is the real uid; the effective one still counts, as it always did.
        assert_eq!(hit("pkill -U 0 -x svc"), vec![2300]);
        assert_eq!(hit("pkill -U 501 -x svc2"), vec![2301]);
        assert_eq!(hit("pkill -U 501 -x svc"), vec![2300]);
        // psmisc killall -u is the real uid.
        assert_eq!(hit("killall -u bharath svc2"), vec![2301]);
        assert_eq!(hit("killall -u root svc"), vec![2300]);
    }

    /* ── the Linux tools: how the arguments are read ── */

    fn pgrep_on_linux(args: &[&str], pkill: bool) -> Parsed<PgrepSpec> {
        super::parse_pgrep(&words(args), pkill, Dialect::Procps)
    }

    fn killall_on_linux(args: &[&str]) -> Parsed<KillallSpec> {
        super::parse_killall(&words(args), Dialect::Procps)
    }

    fn spec_of(parsed: Parsed<PgrepSpec>) -> PgrepSpec {
        match parsed {
            Parsed::Spec(spec) => spec,
            Parsed::NoEffect(why) => panic!("no effect: {why}"),
            Parsed::Unevaluable(why) => panic!("unevaluable: {why}"),
        }
    }

    fn killall_spec_of(parsed: Parsed<KillallSpec>) -> KillallSpec {
        match parsed {
            Parsed::Spec(spec) => spec,
            Parsed::NoEffect(why) => panic!("no effect: {why}"),
            Parsed::Unevaluable(why) => panic!("unevaluable: {why}"),
        }
    }

    fn unevaluable<T>(parsed: Parsed<T>) -> bool {
        matches!(parsed, Parsed::Unevaluable(_))
    }

    fn no_effect<T>(parsed: Parsed<T>) -> bool {
        matches!(parsed, Parsed::NoEffect(_))
    }

    #[test]
    fn procps_reads_options_anywhere_and_long_options_and_one_pattern() {
        // `pkill X -f` is `pkill -f X` there; the macOS reading is kept beside it.
        let late = spec_of(pgrep_on_linux(&["bharath", "-f"], true));
        assert!(late.full);
        assert_eq!(late.patterns, vec!["bharath"]);
        let alt = late.alt.as_ref().expect("the macOS reading is kept");
        assert_eq!(alt.patterns, vec!["bharath", "-f"]);
        assert_eq!(late.late, vec!["-f"]);
        // Options first: one reading, nothing kept beside it.
        let plain = spec_of(pgrep_on_linux(&["-f", "bharath"], true));
        assert!(plain.full && plain.alt.is_none() && plain.late.is_empty());
        // Long options work in pkill, `--inverse` and `--list-full` included.
        let long = spec_of(pgrep_on_linux(&["--full", "--inverse", "x"], true));
        assert!(long.full && long.inverse);
        let abbreviated = spec_of(pgrep_on_linux(&["--ful", "--euid=bharath", "x"], true));
        assert!(abbreviated.full);
        assert_eq!(abbreviated.euids, vec!["bharath"]);
        let separate = spec_of(pgrep_on_linux(&["--uid", "0,bharath", "x"], false));
        assert_eq!(separate.ruids, vec!["0", "bharath"]);
        // Short flags cluster, and an argument can be attached.
        let cluster = spec_of(pgrep_on_linux(&["-fxubharath", "x"], true));
        assert!(cluster.full && cluster.exact);
        assert_eq!(cluster.euids, vec!["bharath"]);
        // `--` ends the options.
        let dashed = spec_of(pgrep_on_linux(&["-f", "--", "-x"], true));
        assert_eq!(dashed.patterns, vec!["-x"]);
        // More than one pattern is a usage error there: nothing is signalled.
        assert!(no_effect(pgrep_on_linux(&["foo", "bar"], true)));
        assert!(no_effect(pgrep_on_linux(&["-f", "foo", "bar"], true)));
        // No pattern and no criterion, the same.
        assert!(no_effect(pgrep_on_linux(&["-f"], true)));
        assert!(!no_effect(pgrep_on_linux(&["-u", "bharath"], true)));
        // An empty pattern matches every process there.
        let empty = spec_of(pgrep_on_linux(&[""], true));
        assert_eq!(empty.patterns, vec![""]);
        // -L needs -F.
        assert!(no_effect(pgrep_on_linux(&["-L", "x"], false)));
        // Usage and version print and signal nothing.
        assert!(no_effect(pgrep_on_linux(&["--help"], true)));
        assert!(no_effect(pgrep_on_linux(&["-V"], true)));
        assert!(no_effect(pgrep_on_linux(&["--version"], false)));
    }

    #[test]
    fn procps_pkill_takes_a_signal_from_anywhere_in_the_arguments() {
        // `signal_option` scans every word, so a signal after the options (or
        // the pattern) is the signal, not an illegal option.
        let after = spec_of(pgrep_on_linux(&["-u", "bharath", "-9"], true));
        assert_eq!(after.euids, vec!["bharath"]);
        assert!(after.patterns.is_empty());
        let named_after = spec_of(pgrep_on_linux(&["-f", "x", "-TERM"], true));
        assert_eq!(named_after.patterns, vec!["x"]);
        let sig_prefix = spec_of(pgrep_on_linux(&["-SIGKILL", "-f", "x"], true));
        assert!(sig_prefix.full);
        // Signal 0 only tests: nothing is signalled, wherever it stands.
        for zero in ["-0", "-00", "-EXIT", "-NULL", "-SIGNULL"] {
            assert!(
                no_effect(pgrep_on_linux(&["-f", "x", zero], true)),
                "{zero}"
            );
        }
        assert!(no_effect(pgrep_on_linux(
            &["--signal", "0", "-f", "x"],
            true
        )));
        assert!(no_effect(pgrep_on_linux(&["--signal=NULL", "x"], true)));
        // pgrep has no signal: `-9` is an option it does not have.
        assert!(unevaluable(pgrep_on_linux(&["-9", "x"], false)));
        // The macOS reading refuses a signal after the options as an illegal
        // option, and reads one after the pattern as one more pattern.
        assert!(matches!(
            super::parse_pgrep(&words(&["-u", "bharath", "-9"]), true, Dialect::Bsd),
            Parsed::NoEffect(_)
        ));
        let bsd = spec_of(super::parse_pgrep(
            &words(&["-f", "x", "-9"]),
            true,
            Dialect::Bsd,
        ));
        assert_eq!(bsd.patterns, vec!["x", "-9"]);
    }

    #[test]
    fn procps_signal_specs_are_what_signal_name_to_number_reads() {
        for (spec, want) in [
            ("9", Some(false)),
            ("0", Some(true)),
            ("TERM", Some(false)),
            ("sigterm", Some(false)),
            ("SIGHUP", Some(false)),
            ("USR1", Some(false)),
            ("RTMIN", Some(false)),
            ("RTMIN+3", Some(false)),
            ("EXIT", Some(true)),
            ("null", Some(true)),
            ("93", Some(false)),
            ("093", Some(false)),
            ("-0", Some(true)),
            ("+5", Some(false)),
            ("94", None),
            ("094", None),
            ("-1", None),
            ("", None),
            ("SIG", None),
            ("f", None),
            ("9x", None),
            ("1234567", None),
        ] {
            assert_eq!(procps_signal(spec), want, "{spec:?}");
        }
    }

    #[test]
    fn procps_options_the_guard_does_not_model_are_asked_about_never_read_as_macos_does() {
        for args in [
            vec!["-s", "12", "x"],
            vec!["--session", "12", "x"],
            vec!["-r", "R", "x"],
            vec!["-H", "x"],
            vec!["-O", "5", "x"],
            vec!["-G", "100", "x"],
            vec!["-t", "pts/1", "x"],
            vec!["--cgroup", "c", "x"],
            vec!["--ns", "1", "x"],
            vec!["--nslist", "pid", "x"],
            vec!["-z", "x"],
            vec!["--bogus", "x"],
            vec!["--ig", "x"],
            vec!["-g", "0"],
        ] {
            assert!(unevaluable(pgrep_on_linux(&args, true)), "{args:?}");
        }
        // pgrep only: threads; pkill has neither `-w` nor short `-v`.
        assert!(unevaluable(pgrep_on_linux(&["-w", "x"], false)));
        assert!(unevaluable(pgrep_on_linux(&["-v", "x"], true)));
        assert!(unevaluable(pgrep_on_linux(&["-a", "x"], true)));
        assert!(!unevaluable(pgrep_on_linux(&["-v", "x"], false)));
        // A word built from a variable can be an option or a signal anywhere.
        let mut dynamic = words(&["-f", "x"]);
        dynamic.push(Word {
            src: "$FLAGS".into(),
            text: String::new(),
            dynamic: true,
            ..Word::default()
        });
        assert!(unevaluable(super::parse_pgrep(
            &dynamic,
            true,
            Dialect::Procps
        )));
        assert!(unevaluable(super::parse_killall(&dynamic, Dialect::Procps)));
        // Options that change nothing about the set are read as such.
        let quiet_flags = spec_of(pgrep_on_linux(&["-e", "-c", "-q", "7", "-f", "x"], true));
        assert!(quiet_flags.full);
        let listing = spec_of(pgrep_on_linux(&["-l", "-a", "-d", ",", "x"], false));
        assert!(
            !listing.ancestors,
            "procps -a lists, it does not add ancestors"
        );
    }

    #[test]
    fn psmisc_killall_options_are_parsed_on_linux() {
        let r = killall_spec_of(killall_on_linux(&["-r", "sle+p"]));
        assert!(r.regex && !r.icase && r.names == vec!["sle+p"]);
        let i = killall_spec_of(killall_on_linux(&["-I", "ZSH"]));
        assert!(i.icase && !i.regex);
        let both = killall_spec_of(killall_on_linux(&["-I", "-r", "ZS+H"]));
        assert!(both.icase && both.regex);
        let long = killall_spec_of(killall_on_linux(&["--regexp", "--ignore-case", "x"]));
        assert!(long.regex && long.icase);
        let single_dash_long = killall_spec_of(killall_on_linux(&["-regexp", "x"]));
        assert!(single_dash_long.regex);
        let prefix = killall_spec_of(killall_on_linux(&["-reg", "-ign", "x"]));
        assert!(prefix.regex && prefix.icase);
        // Options anywhere, a user alone, the signal as an option or by name.
        let late = killall_spec_of(killall_on_linux(&["x", "-u", "bharath"]));
        assert_eq!(late.user.as_deref(), Some("bharath"));
        assert_eq!(late.names, vec!["x"]);
        assert!(late.alt.is_some(), "the macOS reading is kept beside it");
        let user_only = killall_spec_of(killall_on_linux(&["-u", "bharath"]));
        assert!(user_only.names.is_empty() && user_only.user.is_some());
        let attached = killall_spec_of(killall_on_linux(&["-ubharath", "x"]));
        assert_eq!(attached.user.as_deref(), Some("bharath"));
        for args in [
            vec!["-HUP", "x"],
            vec!["-9", "x"],
            vec!["-s", "HUP", "x"],
            vec!["x", "-INT"],
        ] {
            let spec = killall_spec_of(killall_on_linux(&args));
            assert_eq!(spec.names, vec!["x"], "{args:?}");
        }
        // Flags that only change how it reports or waits.
        let calm = killall_spec_of(killall_on_linux(&["-e", "-i", "-q", "-v", "-w", "x"]));
        assert_eq!(calm.names, vec!["x"]);
        // A signal that only tests, listing and version: nothing is signalled.
        for args in [
            vec!["-0", "x"],
            vec!["-s", "0", "x"],
            vec!["--signal=0", "x"],
            vec!["-l"],
            vec!["--list-signals"],
            vec!["-V"],
            vec!["--version"],
            vec![],
        ] {
            assert!(no_effect(killall_on_linux(&args)), "{args:?}");
        }
    }

    #[test]
    fn psmisc_killall_options_that_widen_or_select_by_what_the_table_lacks_are_asked_about() {
        for args in [
            vec!["-g", "x"],
            vec!["--process-group", "x"],
            vec!["-y", "5m", "x"],
            vec!["-o", "5m", "x"],
            vec!["-n", "1", "x"],
            vec!["--ns", "1", "x"],
            vec!["-Z", "ctx", "x"],
            vec!["--context=ctx", "x"],
            vec!["-Q", "x"],
            vec!["--bogus", "x"],
            vec!["-ve", "x"],
            vec!["-Iq", "x"],
            vec!["/bin/sleep"],
            vec!["./sleep"],
        ] {
            assert!(unevaluable(killall_on_linux(&args)), "{args:?}");
        }
        // A path is only a path outside a regular expression.
        assert!(!unevaluable(killall_on_linux(&["-r", "/bin/sleep"])));
    }

    #[test]
    fn the_macos_killall_options_are_not_read_with_their_macos_meaning_on_linux() {
        // macOS `-m` is a regular expression and `-d` a dry run; psmisc has
        // neither, so they are options it does not have, and asked about.
        assert!(unevaluable(killall_on_linux(&["-m", "x"])));
        assert!(unevaluable(killall_on_linux(&["-d", "x"])));
        assert!(unevaluable(killall_on_linux(&["-c", "x", "y"])));
        // And macOS `-s` is a simulation, psmisc's a signal.
        assert!(matches!(
            super::parse_killall(&words(&["-s", "x"]), Dialect::Bsd),
            Parsed::NoEffect(_)
        ));
        let linux = killall_spec_of(killall_on_linux(&["-s", "HUP", "x"]));
        assert_eq!(linux.names, vec!["x"]);
    }

    /* ── the Linux tools: the decision ── */

    #[test]
    fn the_incident_command_is_still_refused_on_linux_where_the_options_are_options() {
        // procps reads `-u bharath` as the user filter, which is narrower; the
        // macOS reading is wider and is kept, so the set is not smaller.
        let output = run_on(
            Dialect::Procps,
            r#"pkill -f "sleep 60" -u bharath"#,
            &world(),
        );
        assert_eq!(classify_output(&output), Out::Deny);
        let reason = reason_of(&output);
        assert!(reason.contains("Linux reads them as options"), "{reason}");
        assert!(reason.contains("macOS"), "{reason}");
        assert!(
            reason.contains("`-u` came after the first pattern"),
            "{reason}"
        );
        assert!(
            reason.contains(r#"["sleep 60", "-u", "bharath"]"#),
            "the patterns the macOS reading takes: {reason}"
        );
        assert!(
            reason.contains("Put the options before the pattern"),
            "{reason}"
        );
    }

    #[test]
    fn a_late_option_that_widens_the_match_is_counted_on_linux() {
        let mut facts = quiet();
        facts
            .rows
            .push(row(2200, 1, &["node", "/srv/zzzmarker.js"]));
        // procps: `-f` is an option, so the pattern is tried against command
        // lines. The macOS reading tries `zzzmarker` and `-f` against names.
        assert_eq!(
            pids_in(Dialect::Procps, "pkill zzzmarker -f", &facts),
            vec![2200]
        );
        assert!(pids_in(Dialect::Bsd, "pkill zzzmarker -f", &facts).is_empty());
    }

    #[test]
    fn a_signal_written_after_the_options_is_not_a_silent_pass_on_linux() {
        // `pkill -u bharath -9` kills every process of the user on Linux. The
        // macOS reading calls `-9` an illegal option and signals nothing.
        let on_linux = run_on(Dialect::Procps, "pkill -u bharath -9", &world());
        assert_eq!(classify_output(&on_linux), Out::Deny, "{on_linux:?}");
        assert_eq!(
            classify_output(&run_on(Dialect::Bsd, "pkill -u bharath -9", &world())),
            Out::Silent
        );
        // The long form of an option, which macOS has no such thing as.
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill --full claude", &world())),
            Out::Deny
        );
        assert_eq!(
            classify_output(&run_on(
                Dialect::Procps,
                "pkill --inverse -f zzzz",
                &world()
            )),
            Out::Deny
        );
    }

    #[test]
    fn an_empty_pattern_matches_every_process_on_linux() {
        let facts = world();
        let hits = pids_in(Dialect::Procps, "pkill ''", &facts);
        assert!(hits.len() > 40, "{}", hits.len());
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill ''", &facts)),
            Out::Deny
        );
        assert_eq!(
            classify_output(&run_on(Dialect::Bsd, "pkill ''", &facts)),
            Out::Silent,
            "macOS refuses an empty pattern"
        );
    }

    #[test]
    fn an_option_the_guard_does_not_model_is_a_question_on_linux_not_a_pass() {
        for command in [
            "pkill -s 12 sleep",
            "pkill -r R sleep",
            "pkill --cgroup system.slice sleep",
            "killall -g sleep",
            "killall -y 1h sleep",
            "killall /bin/sleep",
            "pkill -z sleep",
        ] {
            assert_eq!(
                classify_output(&run_on(Dialect::Procps, command, &world())),
                Out::Ask,
                "{command}"
            );
        }
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "killall -l", &world())),
            Out::Silent
        );
    }

    #[test]
    fn a_killall_late_option_is_counted_both_ways_on_linux() {
        let facts = world();
        let hits = pids_in(Dialect::Procps, "killall sleep -u bharath", &facts);
        assert_eq!(hits.len(), 8, "{hits:?}");
        // The macOS reading takes `-u` and `bharath` as more names and finds
        // none; procps reads the option. The union is what is counted.
        assert!(pids_in(Dialect::Bsd, "killall sleep -u bharath", &facts).len() == 8);
        assert!(matches!(
            sel_in(Dialect::Procps, "killall sleep -u bharath"),
            Some(Sel::Killall(KillallSpec { alt: Some(_), .. }))
        ));
    }

    #[test]
    fn the_linux_readings_never_panic_on_hostile_arguments() {
        for args in [
            vec!["--"],
            vec!["--="],
            vec!["--=x"],
            vec!["-"],
            vec!["--", "--"],
            vec!["-u"],
            vec!["--euid"],
            vec!["--euid="],
            vec!["--full=1", "x"],
            vec!["-é", "x"],
            vec!["--é", "x"],
            vec!["-ü9"],
            vec!["-", "-", "-"],
        ] {
            let _ = pgrep_on_linux(&args, true);
            let _ = pgrep_on_linux(&args, false);
            let _ = killall_on_linux(&args);
        }
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

    /* ── ft-1d9f44d5: the five holes the W4a adversary found ── */

    /// What the guard says about `command` as the Linux tools would run it.
    fn procps_says(command: &str, facts: &Facts) -> Out {
        classify_output(&run_on(Dialect::Procps, command, facts))
    }

    /// A row as the procps tools see it, with the executable the kernel runs.
    fn linux_row(pid: i32, comm: &str, argv: &[&str], exe: &str) -> TableRow {
        let mut r = row(pid, 1, argv);
        r.name = comm.to_string();
        r.exec = argv
            .first()
            .filter(|a| a.starts_with('/'))
            .map(|a| (*a).to_string())
            .or_else(|| Some(exe.to_string()));
        r.real_exe = Some(exe.to_string());
        r
    }

    /// Class 1. The tools hand `regcomp` a command line with its control
    /// characters replaced, so `.` and a space both reach a newline.
    #[test]
    fn procps_reads_a_newline_in_an_argument_as_a_space_and_dot_crosses_it() {
        let mut facts = quiet();
        facts.rows.push(row(
            2100,
            1,
            &["gitpulse-newl", "first line\nsecond line", "tail"],
        ));
        for pattern in [
            "line.second",
            "line second",
            "first.*second",
            "first line\nsecond",
        ] {
            let command = format!("pkill -f '{pattern}'");
            assert_eq!(
                pids_in(Dialect::Procps, &command, &facts),
                vec![2100],
                "{command}"
            );
        }
        assert_eq!(
            pids_in(
                Dialect::Procps,
                "pkill -fx 'gitpulse-newl first line second line tail'",
                &facts
            ),
            vec![2100]
        );
        // macOS reads it as it always did: a newline is not a space there.
        assert!(pids_in(Dialect::Bsd, "pkill -f 'line second'", &facts).is_empty());
    }

    #[test]
    fn procps_reads_a_control_character_as_a_question_mark_or_a_dot_by_locale() {
        let mut facts = quiet();
        facts
            .rows
            .push(row(2100, 1, &["gitpulse-ctl", "a\u{1}b\tc"]));
        for pattern in [r"a\?b", r"a\.b", "a.b", r"b\?c", r"b\.c"] {
            let command = format!("pkill -f '{pattern}'");
            assert_eq!(
                pids_in(Dialect::Procps, &command, &facts),
                vec![2100],
                "{command}"
            );
        }
        assert!(pids_in(Dialect::Bsd, r"pkill -f 'a\?b'", &facts).is_empty());
    }

    #[test]
    fn procps_reads_an_empty_argument_as_a_double_space_and_a_blank_command_line_as_a_question_mark(
    ) {
        let mut facts = quiet();
        facts.rows.push(row(2100, 1, &["gitpulse-empty", "", "x"]));
        let mut blank = row(2101, 1, &["gitpulse-blank"]);
        blank.argv = Some(Vec::new());
        facts.rows.push(blank);
        for (pattern, want) in [
            ("gitpulse-empty  x", vec![2100]),
            // The reading the guard always had, kept: never fewer.
            ("gitpulse-empty x", vec![2100]),
            ("^gitpulse-empty x$", vec![2100]),
            (r"^\?$", vec![2101]),
            ("^.$", vec![2101]),
        ] {
            let command = format!("pkill -f '{pattern}'");
            assert_eq!(
                pids_in(Dialect::Procps, &command, &facts),
                want,
                "{command}"
            );
        }
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -fx '.'", &facts),
            vec![2101]
        );
        // The name stands for a blanked command line, as it always did.
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -f 'gitpulse-blank'", &facts),
            vec![2101]
        );
    }

    /// In the C locale a multi-byte character is as many `?` as it has bytes.
    #[test]
    fn procps_reads_a_non_ascii_character_as_one_character_or_as_a_question_mark_per_byte() {
        let mut facts = quiet();
        facts.rows.push(row(2100, 1, &["gitpulse-u8", "caf\u{e9}"]));
        for pattern in [
            "caf.$",
            "caf..$",
            "caf\u{e9}",
            "caf[^a]",
            r"caf\?\?$",
            r"caf\?",
        ] {
            let command = format!("pkill -f '{pattern}'");
            assert_eq!(
                pids_in(Dialect::Procps, &command, &facts),
                vec![2100],
                "{command}"
            );
        }
        assert!(pids_in(Dialect::Procps, "pkill -f 'caf...$'", &facts).is_empty());
    }

    /// An inverted match leaves a process out only when the pattern picks it
    /// in every reading: otherwise the tool, in the reading it has, may not.
    #[test]
    fn an_inverted_pattern_leaves_a_process_out_only_when_every_reading_picks_it() {
        let mut facts = quiet();
        facts.rows.push(row(2100, 1, &["gitpulse-newl", "x\ny"]));
        facts.rows.push(row(2101, 1, &["gitpulse-plain", "x y"]));
        let hits = pids_in(Dialect::Procps, "kill $(pgrep -fv 'x y')", &facts);
        assert!(
            hits.contains(&2100),
            "the raw reading does not pick it: {hits:?}"
        );
        assert!(!hits.contains(&2101), "every reading picks it: {hits:?}");
    }

    #[test]
    fn a_pattern_regcomp_reads_differently_is_asked_about_and_not_downgraded_from_a_refusal() {
        let quiet = quiet();
        for pattern in [
            r"sleep\d",
            r"sleep\w",
            r"\bsleep",
            r"\<sleep",
            r"sleep\1",
            r"sl[\d]ep",
            "sl[[.e.]]ep",
            "sl[[=e=]]ep",
            "sleep{",
            r"sleep\n",
        ] {
            let command = format!("pkill -f '{pattern}'");
            assert_eq!(procps_says(&command, &quiet), Out::Ask, "{command}");
        }
        // macOS evaluates the pattern with the regex crate, as it always did.
        assert_eq!(
            classify_output(&run_on(Dialect::Bsd, r"pkill -f 'sleep\d'", &quiet)),
            Out::Silent
        );
        // What the guard can evaluate and finds too much stays a refusal.
        assert_eq!(procps_says(r"pkill -f '\w'", &world()), Out::Deny);
        assert_eq!(procps_says(r"pkill -f 'sleep.*[\d]'", &world()), Out::Ask);
        assert_eq!(procps_says(r"pkill -f '/Users/\w'", &world()), Out::Deny);
    }

    #[test]
    fn the_syntax_regcomp_and_the_regex_crate_read_alike_is_not_asked_about() {
        for pattern in [
            "gitpulse-short",
            "a.b",
            "^(a|b)+$",
            "x{1,2}",
            "x{2}",
            "x{2,}",
            "x{,2}",
            "[a-z]+",
            "[_-]",
            "[^]a]",
            "[a-]",
            r"\.",
            r"\(",
            r"a\|b",
            r"\\",
            r"\$",
            "a|",
            "()",
            "caf\u{e9}",
        ] {
            assert_eq!(ere_uncertainty(pattern), None, "{pattern}");
        }
        for pattern in [
            r"\d",
            r"\w",
            r"\b",
            r"\<",
            r"\1",
            r"\a",
            r"\t",
            "ends\\",
            r"[\d]",
            "[[.a.]]",
            "[[=a=]]",
            "[a&&b]",
            "[a--b]",
            "[a~~b]",
            "[[:bogus:]]",
            // A class is the locale's to glibc (`é`, `α`, `—` are `[:alpha:]`
            // or `[:punct:]` in a UTF-8 locale) and ASCII to the crate.
            "[[:alpha:]]",
            "[[:alpha:]_-]",
            "[^[:space:]]",
            "[[:alpha",
            "[a[b]",
            "a{",
            "a{x}",
            "a{,}",
            "[caf\u{e9}]",
            "[abc",
        ] {
            assert!(ere_uncertainty(pattern).is_some(), "{pattern}");
        }
    }

    /// Class 2. Linux `pidof` takes a word with a `/` for the executable.
    #[test]
    fn a_pidof_path_picks_the_processes_that_run_that_executable_by_any_name() {
        let mut facts = quiet();
        let lab = "/tmp/lab";
        // Run as another path: argv[0] says otherprog, the file says GitPulse.
        facts.rows.push(linux_row(
            2100,
            "gitpulse-aliased",
            &["/opt/other/otherprog"],
            "/tmp/lab/gitpulse-aliased",
        ));
        // The same file under a different name and argv[0].
        facts.rows.push(linux_row(
            2101,
            "renamed",
            &["renamed"],
            "/tmp/lab/gitpulse-aliased",
        ));
        facts
            .rows
            .push(linux_row(2102, "other", &["other"], "/usr/bin/other"));
        let hits = |command: &str, facts: &Facts| pids_in(Dialect::Procps, command, facts);
        assert_eq!(
            hits(&format!("kill $(pidof {lab}/gitpulse-aliased)"), &facts),
            vec![2100, 2101]
        );
        // The path the file system resolves it to.
        facts
            .exes
            .insert("/tmp/lab/link".into(), "/tmp/lab/gitpulse-aliased".into());
        assert_eq!(
            hits("kill $(pidof /tmp/lab/link)", &facts),
            vec![2100, 2101]
        );
        // Any process with the last component as its name, as a name always was.
        facts
            .rows
            .push(linux_row(2103, "link", &["link"], "/usr/bin/link"));
        assert!(hits("kill $(pidof /tmp/lab/link)", &facts).contains(&2103));
        assert!(!hits("kill $(pidof /tmp/lab/link)", &facts).contains(&2102));
        // The decision: a GitPulse executable by its file is a refusal.
        assert_eq!(
            procps_says("kill $(pidof /tmp/lab/gitpulse-aliased)", &facts),
            Out::Deny
        );
        assert_eq!(
            procps_says("kill $(pidof /usr/bin/other)", &facts),
            Out::Silent
        );
    }

    #[test]
    fn a_pidof_name_without_a_slash_has_no_executable_to_compare() {
        let Some(Sel::Killall(spec)) = sel_in(Dialect::Procps, "kill $(pidof gitpulse-short)")
        else {
            panic!("a pidof is read as a name match");
        };
        assert_eq!(spec.names, vec!["gitpulse-short"]);
        assert!(spec.exe_paths.is_empty());
        let Some(Sel::Killall(spec)) = sel_in(Dialect::Procps, "kill $(pidof /usr/bin/claude)")
        else {
            panic!("a pidof is read as a name match");
        };
        // The word as written, which is what it always was, and its last
        // component.
        assert_eq!(spec.names, vec!["/usr/bin/claude", "claude"]);
        assert_eq!(spec.exe_paths, vec!["/usr/bin/claude"]);
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_path_resolves_through_a_symbolic_link() {
        let dir = tempfile::tempdir().expect("a directory");
        let real = dir.path().join("real-binary");
        std::fs::write(&real, b"x").expect("a file");
        let link = dir.path().join("innocuous");
        std::os::unix::fs::symlink(&real, &link).expect("a link");
        let words = vec![
            link.to_string_lossy().into_owned(),
            dir.path().join("missing").to_string_lossy().into_owned(),
        ];
        let resolved = resolved_executables(&words);
        let want = std::fs::canonicalize(&real)
            .expect("resolves")
            .to_string_lossy()
            .into_owned();
        assert_eq!(resolved.get(&words[0]), Some(&want));
        assert!(
            !resolved.contains_key(&words[1]),
            "an unresolved path is left out"
        );
    }

    /// Class 3. A program named by variables the line itself bound is what they
    /// come to, and is read as that program (Deny here, where it reaches
    /// GitPulse); one it cannot work out is asked about when the command spells
    /// a signal tool, and is an ordinary program otherwise.
    #[test]
    fn a_program_named_by_variables_the_line_bound_is_read_as_that_program() {
        let facts = world();
        for command in [
            "p=pkill; $p -f gitpulse",
            "p=pk''ill; $p -f gitpulse",
            "p=pk\"\"ill; $p -f gitpulse",
            "p=pk; ${p}ill -f gitpulse",
            "p=pkil; ${p}l -f gitpulse",
            "a=p; b=ki; c=ll; $a$b$c -f gitpulse",
            "P=/usr/bin/pk; ${P}ill -f gitpulse",
            "P=/usr/bin/pkill; $P -f gitpulse",
            "p=pkill; \"$p\" -f gitpulse",
            "export p=pkill; $p -f gitpulse",
            "p=pkill; sudo $p -f gitpulse",
            "p=pkill; nohup $p -f gitpulse",
            "$'\\x70kill' -f gitpulse",
            "$'p\\153ill' -f gitpulse",
            "c='pkill -f gitpulse'; eval $c",
            "c='pkill -f gitpulse'; bash -c \"$c\"",
            "c='pkill -f gitpulse'; sh -c \"$c\"",
            "p=killall; $p gitpulsed",
        ] {
            for dialect in [Dialect::Bsd, Dialect::Procps] {
                assert_eq!(
                    classify_output(&run_on(dialect, command, &facts)),
                    Out::Deny,
                    "{dialect:?} {command}"
                );
            }
        }
        // Read, not asked: a pattern that reaches nothing is silent.
        assert_eq!(
            procps_says("p=pkill; $p -f no-such-process-xyz", &quiet()),
            Out::Silent
        );
        // The bound value is for the program only: an argument built from a
        // variable is still one GitPulse cannot expand (asked, as it always was).
        assert_eq!(
            procps_says("pat=gitpulse; pkill -f $pat", &quiet()),
            Out::Ask
        );
        // Bound twice differently, or from something unknown: not known.
        for command in [
            "p=ls; p=pkill; $p -f x",
            "p=$(echo pkill); $p -f x",
            "for p in pkill killall; do $p -f x; done",
            "p='pkill -f'; $p x",
        ] {
            assert_eq!(procps_says(command, &quiet()), Out::Ask, "{command}");
        }
    }

    #[test]
    fn a_program_the_line_does_not_name_is_asked_about_only_beside_a_signal_tool() {
        let facts = quiet();
        // A signal tool is a word of the command, or of one that bound
        // something earlier.
        for command in [
            "$CMD kill",
            "$RUN pkill -f x",
            "echo x | xargs $p pkill -f gitpulse-short",
            "x=$(echo pkill); $x -f gitpulse-short",
            "eval \"$(echo pkill) -f x\"",
            "bash -c \"$(echo pkill) -f x\"",
            "echo 'pkill -f x' | sh",
            "echo pkill | bash -s",
            "source <(echo 'pkill -f x')",
            ". <(echo pkill)",
            "find . -exec {} -f x \\; -o -name pkill",
            "echo /usr/bin/pkill | xargs -I{} {} -f x",
            "/usr/bin/pk[i]ll -f x",
            "/usr/bin/pk?ll -f x",
            "/usr/bin/pk*ll -f x",
            "{pkill,-f,x}",
            "/usr/bin/p{k,k}ill -f x",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        // Anything else is an ordinary program: a path, `skill`, `--kill-after`,
        // `KILL_AFTER=5` and a tool in another command are not the tool.
        for command in [
            "$EDITOR notes.txt",
            "$EDITOR notes/skills.md",
            "$CC -o skill skill.c",
            "$TOOL --killswitch",
            "$RUNNER --kill-after 5 test",
            "export KILL_AFTER=5; $CMD",
            "git commit -m 'skill: x' && $PAGER log",
            "git log --grep=kill | $PAGER",
            "${EDITOR:-vi} killer.txt",
            "c=echo; $c kill",
            "pkill -f no-such-process-xyz; $EDITOR out.log",
            "for s in 1 2 3; do $RUNNER $s; done; echo \"kill list done\"",
            "$SHELL -c 'echo kill'",
            "sh -c \"kill $PID\"",
            "bash -c \"$script\"",
            "eval $setup",
            "exec \"$@\"",
            "FOO=1 $tool --flag",
            "cat notes | sh",
            "echo ok | bash",
            "source ~/.profile",
            "eval \"$(ssh-agent -s)\"",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    #[test]
    fn a_busybox_or_toybox_applet_of_a_signal_command_is_asked_about() {
        let facts = quiet();
        for command in [
            "busybox pkill -f gitpulse-short",
            "busybox killall gitpulse-short",
            "toybox pkill -f gitpulse-short",
            "sudo busybox pkill -f gitpulse-short",
            "busybox killall5 -9",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        for command in [
            "busybox ls -l",
            "busybox --list",
            "busybox kill 12345",
            "busybox",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
        let applet = words(&["busybox", "pkill", "-f", "x"]);
        let resolved = resolve(&applet).expect("resolves");
        assert_eq!(resolved.prog, "pkill");
        assert!(resolved.applet);
        let plain = words(&["pkill", "x"]);
        assert!(!resolve(&plain).expect("resolves").applet);
    }

    #[test]
    fn a_program_word_that_names_nothing_is_not_the_same_as_one_that_is_unknown() {
        assert!(matches!(
            resolve(&words(&["p=pkill"])),
            Err(Unresolved::Nothing)
        ));
        assert!(matches!(
            resolve(&words(&["command", "-v", "pkill"])),
            Err(Unresolved::Nothing)
        ));
        let mut dynamic = words(&["$p", "-f", "x"]);
        dynamic[0].dynamic = true;
        assert!(matches!(resolve(&dynamic), Err(Unresolved::Dynamic)));
    }

    #[test]
    fn an_alias_defined_on_the_line_is_read_as_what_it_stands_for() {
        let facts = world();
        for command in [
            "alias k=pkill; k -f gitpulse",
            "alias k='pkill -f'; k gitpulse",
            "alias k='pkill -f gitpulse'; k",
            "bash -c \"alias k=pkill; k -f gitpulse\"",
            "alias a=b b=pkill; b -f gitpulse",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Deny, "{command}");
        }
        // Read, so what it stands for decides: nothing signalled, nothing said.
        let facts = quiet();
        for command in [
            "alias ll='ls -l'; ll",
            "alias killp='echo kill'; killp",
            "alias k='kill -9'; k 99999",
            "alias killer=ls",
            "alias",
            "unalias k",
            "alias k=pkill; k -f no-such-process-xyz",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
        // An alias named for a signal tool does not hide the command as written.
        let facts_world = world();
        for command in [
            "alias pkill=echo; pkill -f gitpulse",
            "alias kill=true; kill -1",
            "alias killall='echo x'; killall gitpulsed",
        ] {
            assert_eq!(procps_says(command, &facts_world), Out::Deny, "{command}");
        }
        // An alias that names itself ends.
        assert_eq!(procps_says("alias ls='ls -l'; ls", &facts), Out::Silent);
        assert_eq!(procps_says("alias a=a; a -f x", &facts), Out::Silent);
        // An alias whose value is not plain text, on a line that spells a tool.
        assert_eq!(
            procps_says("alias k=\"$x pkill\"; k -f x", &facts),
            Out::Ask
        );
    }

    #[test]
    fn a_signal_command_under_find_exec_is_asked_about() {
        let facts = quiet();
        for command in [
            "find . -name x -exec pkill -f gitpulse-short {} +",
            "find . -name x -execdir pkill -f gitpulse-short {} \\;",
            "find . -exec killall gitpulse-short \\;",
            "find . -ok kill {} \\;",
            "find . -name x -exec sudo pkill -f gitpulse-short {} +",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        for command in [
            "find . -name '*.rs' -exec grep -l todo {} +",
            "find . -type f -exec ls -l {} \\;",
            "find . -name killme.txt",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
        // A shell it hands a script is read like any other.
        assert!(!plan_with("find . -exec sh -c 'pkill -f x' \\;", fake_env).is_empty());
    }

    #[test]
    fn ssh_to_this_machine_is_asked_about_and_ssh_to_another_is_a_stated_limit() {
        let facts = quiet();
        for command in [
            "ssh localhost pkill -f gitpulse-short",
            "ssh user@127.0.0.1 'pkill -f gitpulse-short'",
            "ssh -p 22 localhost \"killall gitpulse-short\"",
            "ssh $HOST pkill -f gitpulse-short",
            "ssh [::1] 'sleep 1; killall x'",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        for command in [
            "ssh localhost ls",
            "ssh build-host ls",
            "ssh build-host pkill -f gitpulse-short",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    /// The limits the module header states, pinned: each is silent today, on
    /// a machine with agents on it, and a change here is a decision.
    #[test]
    fn the_stated_limits_of_the_shell_reader_are_pinned() {
        let facts = world();
        for command in [
            "ssh build-host pkill -f claude",
            "parallel pkill -f claude ::: a",
            "watch pkill -f claude",
            "nsenter -t 1 pkill -f claude",
            "su -c 'pkill -f claude' other",
            "env -S 'pkill -f claude'",
            "docker exec c pkill -f claude",
            "skill -9 -u bharath",
            "fuser -k 8080/tcp",
            "killall5 -9",
            "k -f claude",
            // A program the line builds with a command, where no tool is a
            // word of it; a link or a file the line makes and then runs; a
            // script file; a pid list the line only reads from a file it
            // did not write.
            "c=$(printf 'pk%s' ill); $c -f claude",
            "eval \"$(printf 'pk%s' ill) -f claude\"",
            "bash -c \"$(printf 'pk%s' ill) -f claude\"",
            "ln -s /usr/bin/pkill /tmp/xpkill; /tmp/xpkill -f claude",
            "printf 'pkill -f claude' > /tmp/s.sh; bash /tmp/s.sh",
            "kill $(cat /tmp/pids)",
            "script -qc 'pkill -f claude' /dev/null",
            "flock /tmp/lock -c 'echo hi'",
            // A sourced file may set anything: what was bound before it is
            // not known after it, and nothing on the line spells a tool.
            "p=ls; . ./env.sh; $p -f claude",
            "p=ls; source ./env.sh; $p -f claude",
            // A setter that does not spell a tool.
            "getopts p x; $p -f claude",
            "read p <<< ls; $p -f claude",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    /// Rework 2. Bindings made in a subshell, a command substitution, a stage of
    /// a pipeline, a command run with `&` or a new shell are not seen by the
    /// shell that ran them; the variables a `read`, `printf -v`, `mapfile`,
    /// `getopts`, `let`, `(( ))`, `declare -n` or `source` set are not known.
    #[test]
    fn what_a_subshell_binds_is_not_seen_outside_it_and_other_setters_are_binding_commands() {
        let facts = world();
        // The parent's value stands.
        for command in [
            "p=pkill; (p=ls); $p -f gitpulse",
            "p=pkill; p=ls | true; $p -f gitpulse",
            "p=pkill; true | p=ls; $p -f gitpulse",
            "p=pkill; p=ls & $p -f gitpulse",
            "p=pkill; (p=ls) & $p -f gitpulse",
            "p=pkill; x=$(p=ls); $p -f gitpulse",
            "p=pkill; echo $(p=ls) `p=ls`; $p -f gitpulse",
            "p=pkill; cat <(p=ls); $p -f gitpulse",
            "p=pkill; bash -c 'p=ls'; $p -f gitpulse",
            "p=pkill; (p=ls; (p=sleep)); $p -f gitpulse",
            "p=pkill; ( ( p=ls ) ); $p -f gitpulse",
            // A brace group is the same shell, and is not modelled as one:
            // the earlier value stands.
            "p=pkill; { p=ls; }; $p -f gitpulse",
            // Inside the subshell its own bindings hold.
            "(p=pkill; $p -f gitpulse)",
            "(p=pkill; (p=ls); $p -f gitpulse)",
            // The body of a loop fed by a pipeline runs in the stage that read
            // the pids, so it sees the variable.
            "pgrep -f gitpulse | while read p; do kill $p; done",
            "pgrep -f gitpulse | while read -r p; do echo $p; kill $p; done",
            // eval and an alias run in this shell.
            "eval 'p=pkill'; $p -f gitpulse",
            "alias setp='p=pkill'; setp; $p -f gitpulse",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Deny, "{command}");
        }
        // Not known, with a tool spelled by the command that bound it.
        for command in [
            "(p=pkill); $p -f gitpulse",
            "(p=pkill; p=ls) ; $p -f gitpulse",
            "true | p=pkill; $p -f gitpulse",
            "p=pkill & $p -f gitpulse",
            "p=pkill; false && p=ls; $p -f gitpulse",
            // Assigned twice in this shell, differently: not known.
            "p=ls; eval 'p=pkill'; $p -f gitpulse",
            "alias setp='p=pkill'; p=ls; setp; $p -f gitpulse",
            "read p <<< pkill; $p -f gitpulse",
            "read -r p <<< pkill; $p -f gitpulse",
            "read -a p <<< pkill; $p -f gitpulse",
            "read <<< pkill; $REPLY -f gitpulse",
            "printf -v p pkill; $p -f gitpulse",
            "printf -v p '%s' pkill; $p -f gitpulse",
            "read p <<< pkill; (p=ls); $p -f gitpulse",
            "mapfile -t p <<< pkill; $p -f gitpulse",
            "readarray -t p <<< pkill; $p -f gitpulse",
            "mapfile -t <<< pkill; $MAPFILE -f gitpulse",
            "getopts p pkill; $p -f gitpulse",
            "p=pkill; ((p=1)); $p -f gitpulse",
            "p=pkill; (( p += 1 )); $p -f gitpulse",
            "p=pkill; ((p++)); $p -f gitpulse",
            "p=pkill; let p=1; $p -f gitpulse",
            "p=pkill; let 'p+=1'; $p -f gitpulse",
            "p=pkill; read -a p <<< ls; $p -f gitpulse",
            "p=pkill; read -p prompt p <<< ls; $p -f gitpulse",
            "p=pkill; printf -v p ls; $p -f gitpulse",
            "p=pkill; declare -n p=q; $p -f gitpulse",
            "p=pkill; typeset -n p; $p -f gitpulse",
            "p=pkill; local -n p=q; $p -f gitpulse",
            "p=pkill; . ./env.sh; $p -f gitpulse",
            "p=pkill; source ./env.sh; $p -f gitpulse",
            "alias k=pkill; . ./env.sh; k -f gitpulse",
            "pgrep -f gitpulse | read p; kill $p",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        // A loop variable the body read, after the loop has ended, is not.
        assert_eq!(
            procps_says(
                "pgrep -f gitpulse | while read p; do :; done; kill $p",
                &facts
            ),
            Out::Ask
        );
    }

    #[test]
    fn the_lexer_numbers_the_subshell_groups_a_command_is_in() {
        let mut budget = Budget { commands: 0 };
        let script =
            parse_script("a; (b; (c); d) e & (f)", 0, &mut budget, fake_env).expect("parses");
        let scopes: Vec<(String, Vec<u32>, bool)> = script
            .iter()
            .flatten()
            .map(|c| (c.words[0].text.clone(), c.scope.clone(), c.background))
            .collect();
        assert_eq!(
            scopes,
            vec![
                ("a".to_string(), vec![], false),
                ("b".to_string(), vec![0], false),
                ("c".to_string(), vec![0, 1], false),
                ("d".to_string(), vec![0], false),
                ("e".to_string(), vec![], true),
                ("f".to_string(), vec![2], false),
            ]
        );
        // `(( ))` is an arithmetic command and not a group.
        let script =
            parse_script("((p=1)); ((x = $(echo 1)))", 0, &mut budget, fake_env).expect("parses");
        let cmds: Vec<&Cmd> = script.iter().flatten().collect();
        assert!(cmds.iter().all(|c| c.arith && c.scope.is_empty()));
        assert!(!cmds[1].words.iter().all(|w| w.subst.is_empty()));
        // A nesting no line needs is refused, not followed.
        let deep = format!(
            "{}x{}",
            "( ".repeat(MAX_DEPTH + 1),
            " )".repeat(MAX_DEPTH + 1)
        );
        assert!(parse_script(&deep, 0, &mut budget, fake_env).is_err());
    }

    #[test]
    fn the_variables_an_arithmetic_expression_assigns_are_found() {
        for (expr, want) in [
            ("p=1", vec!["p"]),
            ("p = 1", vec!["p"]),
            ("p+=1", vec!["p"]),
            ("p<<=2", vec!["p"]),
            ("a[1]=2", vec!["a"]),
            ("p++", vec!["p"]),
            ("--p", vec!["p"]),
            ("x=1, y=2", vec!["x", "y"]),
            ("p==1", vec![]),
            ("1+2", vec![]),
            ("a<b", vec![]),
        ] {
            assert_eq!(arith_targets(expr), want, "{expr}");
        }
    }

    /// Class 3, wrappers: each is read through to the program it runs, from the
    /// one list `resolve` has (nothing here is a second list).
    #[test]
    fn wrappers_with_options_and_a_positional_are_read_through() {
        let facts = world();
        for command in [
            "ionice -c3 pkill -f gitpulse",
            "ionice -c 2 -n 5 pkill -f gitpulse",
            "taskset -c 0 pkill -f gitpulse",
            "taskset 0x3 pkill -f gitpulse",
            "chrt -o 0 pkill -f gitpulse",
            "chrt -f 10 pkill -f gitpulse",
            "flock /tmp/lock pkill -f gitpulse",
            "flock -w 5 /tmp/lock pkill -f gitpulse",
            "flock /tmp/lock -c 'pkill -f gitpulse'",
            "setpriv --no-new-privs pkill -f gitpulse",
            "setpriv --reuid 1000 --regid 1000 pkill -f gitpulse",
            "unshare -r pkill -f gitpulse",
            "unshare --map-user=1000 -m pkill -f gitpulse",
            "nohup nice -n 5 timeout 5 stdbuf -o0 setsid pkill -f gitpulse",
            "env -i A=1 command exec pkill -f gitpulse",
            "sudo -n doas pkill -f gitpulse",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Deny, "{command}");
        }
        for command in [
            "ionice -p 1",
            "taskset -p 0x1 1234",
            "chrt -p 1234",
            "chrt -m",
            "ionice -c3 ls",
            "taskset -c 0 sleep 1",
            "flock /tmp/lock true",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    #[test]
    fn a_shell_with_options_before_dash_c_has_its_script_read() {
        let facts = world();
        for command in [
            "bash -o pipefail -c 'pkill -f gitpulse'",
            "bash -O extglob -c 'pkill -f gitpulse'",
            "bash +o history -c 'pkill -f gitpulse'",
            "bash --noprofile --norc -o errexit -c 'pkill -f gitpulse'",
            "bash -eu -o pipefail -c 'pkill -f gitpulse'",
            "bash -ec 'pkill -f gitpulse'",
            "bash -lc 'pkill -f gitpulse'",
            "zsh -l -c 'pkill -f gitpulse'",
            "sh -c 'pkill -f gitpulse' name arg",
            "bash --rcfile /dev/null -c 'pkill -f gitpulse'",
            "bash -c -- 'pkill -f gitpulse'",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Deny, "{command}");
        }
        // A shell given a file, or a script with no signal in it.
        for command in [
            "bash script.sh",
            "bash -o pipefail script.sh",
            "bash -c 'echo hi'",
            "bash",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    /// Class 3, pids: a loop, a variable or a file that carries the output of a
    /// `pgrep` or `pidof` on the line into `kill` is that command's set.
    #[test]
    fn pids_a_loop_a_variable_or_a_file_carries_into_kill_are_the_producers_set() {
        let facts = world();
        for command in [
            "for p in $(pgrep -f gitpulse); do kill $p; done",
            "for p in `pgrep -f gitpulse`; do kill -9 $p; done",
            "pgrep -f gitpulse | while read p; do kill $p; done",
            "pgrep -f gitpulse | while read -r p; do kill -TERM \"$p\"; done",
            "pids=$(pgrep -f gitpulse); kill $pids",
            "pids=$(pgrep -f gitpulse); kill \"$pids\"",
            "export pids=$(pgrep -f gitpulse); kill $pids",
            "mapfile -t p < <(pgrep -f gitpulse); kill \"${p[@]}\"",
            "readarray -t p < <(pgrep -f gitpulse); kill ${p[@]}",
            "xargs kill < <(pgrep -f gitpulse)",
            "pgrep -f gitpulse > /tmp/pids; kill $(cat /tmp/pids)",
            "pidof gitpulsed > /tmp/pids; kill $(cat /tmp/pids)",
            "pids=$(pidof gitpulsed); kill $pids",
            "for p in $(pidof gitpulsed); do kill $p; done",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Deny, "{command}");
        }
        // The producer decides: nothing it reaches, nothing said.
        for command in [
            "for p in $(pgrep -f no-such-process-xyz); do kill $p; done",
            "pids=$(pgrep -f no-such-process-xyz); kill $pids",
        ] {
            assert_eq!(procps_says(command, &quiet()), Out::Silent, "{command}");
        }
        // A pid variable the line did not fill, beside a pgrep or a pidof, is a
        // list GitPulse cannot follow; without one it is an ordinary variable.
        assert_eq!(
            procps_says("pgrep -f x > /dev/null; kill $PID", &quiet()),
            Out::Ask
        );
        for command in ["kill $PID", "kill -9 $pid", "echo hi; kill $!"] {
            assert_eq!(procps_says(command, &quiet()), Out::Silent, "{command}");
        }
    }

    #[test]
    fn a_pidof_argument_the_shell_expands_first_is_asked_about() {
        let facts = quiet();
        for command in [
            "kill $(pidof /d/gitpulse-w5b-e*)",
            "kill $(pidof /d/s[l])",
            "kill $(pidof /d/{sl,x})",
            "kill $(pidof /d/gitpulse-w5b-ex?)",
            "kill $(pidof gitpulse*)",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        for command in [
            "kill $(pidof nosuchprocessxyz)",
            "kill $(pidof /usr/bin/nosuchxyz)",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Silent, "{command}");
        }
    }

    /// Ruling 2, regular expressions the guard cannot answer for.
    #[test]
    fn a_posix_class_or_a_case_folded_non_ascii_name_is_asked_about() {
        let mut facts = quiet();
        facts.rows.push(row(2100, 1, &["caf\u{e9}-daemon"]));
        facts
            .rows
            .push(row(2101, 1, &["g\u{131}tpulse-x", "--flag"]));
        // A class is the locale's to glibc.
        for command in [
            "pkill -f 'caf[[:alpha:]]'",
            "pkill 'x[[:space:]]y'",
            "pkill -f 'a[[=e=]]b'",
            "pkill -f 'a[[.hyphen.]]b'",
            "killall -r 'x[[:alpha:]]'",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        // `-i` against a non-ASCII pattern or name: `ı` is an `I` to glibc.
        for command in [
            "pkill -fi 'GITPULSE-X'",
            "pkill -fi 'nomatch'",
            "pkill -i 'caf\u{c9}'",
            "killall -r -I 'nomatch'",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        // Without `-i` the non-ASCII subject is an ordinary one, and a row that
        // matched is simply a hit.
        assert_eq!(procps_says("pkill -f nomatchxyz", &facts), Out::Silent);
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -fi 'caf\u{e9}-daemon'", &facts),
            vec![2100]
        );
        // macOS is untouched.
        assert_eq!(
            classify_output(&run_on(Dialect::Bsd, "pkill -fi 'nomatch'", &facts)),
            Out::Silent
        );
    }

    /// procps matches `-f` against the first 131071 bytes of a command line.
    #[test]
    fn a_command_line_is_also_read_as_procps_cuts_it() {
        let mut facts = quiet();
        let filler = "a".repeat(100_000);
        facts
            .rows
            .push(row(2100, 1, &["gitpulse-trunc", &filler, &filler, "END"]));
        // The tail is past the cut: the tool does not see `END`, an anchor on
        // the cut sees `a`; the whole line still shows the guard `END`.
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -f 'a$'", &facts),
            vec![2100]
        );
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -f 'END$'", &facts),
            vec![2100]
        );
        assert!(
            full_subjects(Dialect::Procps, &facts.rows[facts.rows.len() - 1], false)
                .iter()
                .any(|s| s.len() == PROCPS_CMDLINE_CUT)
        );
        // A short line has no cut copy.
        let short = row(2101, 1, &["gitpulse-short", "x"]);
        assert_eq!(full_subjects(Dialect::Procps, &short, false).len(), 1);
    }

    /// A command line with bytes that are not UTF-8 reaches the guard replaced;
    /// a pattern that counts characters is asked about, one that does not is
    /// not.
    #[test]
    fn invalid_utf8_in_a_command_line_is_asked_about_for_a_pattern_that_counts_characters() {
        let mut facts = quiet();
        facts
            .rows
            .push(row(2100, 1, &["gitpulse-bad", "x\u{fffd}y"]));
        for command in [
            "pkill -f 'x..y$'",
            "pkill -f 'bad x.{3}y'",
            "pkill -f 'x[^a][^a]y'",
            r"pkill -f 'x\?\?y$'",
        ] {
            assert_eq!(procps_says(command, &facts), Out::Ask, "{command}");
        }
        // A literal pattern counts nothing.
        assert_eq!(
            procps_says("pkill -f 'nomatch-literal'", &facts),
            Out::Silent
        );
        assert_eq!(procps_says("pkill -f 'nomatch-lit*'", &facts), Out::Silent);
    }

    /// Class 4. A root sender can signal every process, so a command line it
    /// could not read is a process `-f` may pick.
    #[test]
    fn a_root_sender_that_could_not_read_a_command_line_is_asked_not_told() {
        let mut facts = quiet();
        facts.me_uid = 0;
        facts.me_euid = Some(0);
        let mut other = row(2100, 1, &["/usr/bin/worker", "marker-xyz"]);
        other.uid = 1001;
        other.argv = None;
        facts.rows.push(other);
        let output = run_on(Dialect::Procps, "pkill -f marker-xyz", &facts);
        assert_eq!(classify_output(&output), Out::Ask, "{output:?}");
        assert!(reason_of(&output).contains("could not read"), "{output:?}");
        // The same unread process of the sender's own, for a sender that is
        // not root: only a notice, as it always was.
        facts.me_uid = ME;
        facts.me_euid = Some(ME);
        facts.rows.last_mut().expect("the process").uid = ME;
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill -f marker-xyz", &facts)),
            Out::Notice
        );
    }

    #[test]
    fn a_root_sender_that_read_another_users_command_line_counts_the_process() {
        let mut facts = world();
        facts.me_uid = 0;
        facts.me_euid = Some(0);
        let mut other = row(2100, 1, &["/usr/bin/worker", "marker-xyz"]);
        other.uid = 1001;
        facts.rows.push(other);
        assert_eq!(
            pids_in(Dialect::Procps, "pkill -f marker-xyz", &facts),
            vec![2100]
        );
        // And a GitPulse process of another user is a refusal.
        let mut theirs = row(
            2101,
            1,
            &["/usr/local/bin/gitpulsed", "--data", "theirs-xyz"],
        );
        theirs.uid = 1001;
        facts.rows.push(theirs);
        assert_eq!(
            classify_output(&run_on(Dialect::Procps, "pkill -f theirs-xyz", &facts)),
            Out::Deny
        );
    }

    /// Before a root sender read another user's command line it matched that
    /// process by its name; the name stays a subject, so no process the old
    /// reading picked is let go.
    #[test]
    fn a_root_sender_still_matches_another_users_process_by_its_name_under_dash_f() {
        let mut facts = quiet();
        facts.me_uid = 0;
        facts.me_euid = Some(0);
        let mut other = row(2100, 1, &["/usr/bin/other", "--flag"]);
        other.name = "worker".to_string();
        other.uid = 1001;
        facts.rows.push(other);
        for dialect in [Dialect::Procps, Dialect::Bsd] {
            assert_eq!(
                pids_in(dialect, "pkill -f worker", &facts),
                vec![2100],
                "{dialect:?}"
            );
        }
        // The sender's own process is matched by its command line, as ever.
        facts.me_uid = ME;
        facts.me_euid = Some(ME);
        facts.rows.last_mut().expect("the process").uid = ME;
        assert!(pids_in(Dialect::Procps, "pkill -f worker", &facts).is_empty());
    }

    /// Class 5. `argv[0]` is the process's to choose; the executable is not.
    #[test]
    fn a_gitpulse_executable_run_under_another_argv0_is_still_gitpulse() {
        let spoofed = linux_row(
            2100,
            "gitpulse-aliased",
            &["/opt/other/otherprog", "--flag"],
            "/tmp/lab/gitpulse-aliased",
        );
        assert!(is_gitpulse(&spoofed));
        let mut without = spoofed.clone();
        without.real_exe = None;
        without.name = "renamed-comm".to_string();
        assert!(
            !is_gitpulse(&without),
            "by argv[0] alone it is not, which is how it was missed"
        );
        // The kernel's name for it, which `argv[0]` does not change, is one
        // more thing that says what it is.
        let mut by_comm = without.clone();
        by_comm.name = "gitpulse-aliased".to_string();
        assert!(is_gitpulse(&by_comm));
        let mut facts = quiet();
        facts.rows.push(spoofed);
        assert_eq!(procps_says("pkill gitpulse-aliased", &facts), Out::Deny);
        facts.rows.pop();
        facts.rows.push(without);
        assert_eq!(procps_says("pkill renamed-comm", &facts), Out::Silent);
        // The app bundle is recognised by the executable too.
        let bundled = linux_row(
            2101,
            "gitpulse",
            &["renamed"],
            "/Applications/GitPulse.app/Contents/MacOS/gitpulse",
        );
        assert!(is_gitpulse(&bundled));
        // A process run from an ordinary executable under a GitPulse path as
        // `argv[0]` still says it is GitPulse, as it did.
        let claims = linux_row(2102, "sleep", &["/opt/gitpulse-fake"], "/bin/sleep");
        assert!(is_gitpulse(&claims));
    }

    /// Fail closed on identity: a process in the match set whose executable
    /// could not be read, or is the dynamic loader, is not passed by the
    /// spelling it chose.
    #[test]
    fn a_process_whose_executable_is_unknown_is_asked_about_not_passed_by_its_spelling() {
        let mut unknown = linux_row(2100, "ndprog", &["/opt/other/ndprog"], "/unused");
        unknown.real_exe = None;
        unknown.exe_unknown = true;
        let mut facts = quiet();
        facts.rows.push(unknown.clone());
        let output = run_on(Dialect::Procps, "pkill ndprog", &facts);
        assert_eq!(classify_output(&output), Out::Ask, "{output:?}");
        assert!(reason_of(&output).contains("executable"), "{output:?}");
        // The same process with its executable known is an ordinary one.
        let mut known = unknown.clone();
        known.exe_unknown = false;
        known.real_exe = Some("/opt/other/ndprog".to_string());
        facts.rows.pop();
        facts.rows.push(known);
        assert_eq!(procps_says("pkill ndprog", &facts), Out::Silent);
        // By the kernel's name it is still GitPulse: a refusal, not a question.
        let mut by_name = unknown.clone();
        by_name.name = "gitpulse-nd".to_string();
        facts.rows.pop();
        facts.rows.push(by_name);
        assert_eq!(procps_says("pkill gitpulse-nd", &facts), Out::Deny);
        // Through the loader its program is the first argument.
        let through = linux_row(
            2101,
            "ld-linux-aarch64",
            &["/lib/ld-linux-aarch64.so.1", "/tmp/lab/gitpulse-ld"],
            "/lib/ld-linux-aarch64.so.1",
        );
        assert!(is_gitpulse(&through));
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
