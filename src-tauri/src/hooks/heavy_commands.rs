//! Which shell commands are heavy, and which are known to have exhausted this
//! host's memory: the table the load gate (`load_gate`) reads.
//!
//! The table is data. A new heavy command is a new [`Row`] in [`ROWS`], not a
//! new branch. What reads the command line is `signal_guard::command_words`,
//! the same reading the signal guard judges, so a word inside a quoted string
//! (`echo "cargo test"`, `git commit -m "run cargo test"`) is one word and
//! never a program.
//!
//! What this does not do, on purpose: it does not read `package.json` to learn
//! what `npm run <script>` runs. A script is heavy here only when its name says
//! so (`test*`, `ci:local`), which is a recorded follow-up, not a claim that
//! every other script is cheap.

use super::signal_guard::command_words;

/// What a matching row means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A command the load gate may refuse while the host is overloaded.
    Heavy,
    /// A command that has taken this host down before. Advice only, shown only
    /// while the host is already under load; never refused.
    Advise,
}

/// A condition on the words after the program.
#[derive(Debug, Clone, Copy)]
pub(super) enum Cond {
    Always,
    /// Any of these flags is present (before a bare `--`). A one-letter name
    /// (`-r`) matches inside a cluster (`-rn`); a longer single-dash name
    /// (`-timeout`, `-uu`) and a `--long` name match exactly or as `name=…`.
    Flags(&'static [&'static str]),
    /// None of these flags is present.
    NoFlags(&'static [&'static str]),
    /// An operand equals one of these.
    Args(&'static [&'static str]),
    /// An operand contains one of these.
    ArgsContain(&'static [&'static str]),
    /// No operand begins with any of these (`!` is a negated `rg` glob).
    NoArgPrefix(&'static [&'static str]),
    All(&'static [Cond]),
    Any(&'static [Cond]),
}

/// One command GitPulse has a view on.
#[derive(Debug)]
pub(super) struct Row {
    /// What the row is called in a log line and a test.
    pub id: &'static str,
    /// Program names (the last path segment). A trailing `*` is a prefix.
    programs: &'static [&'static str],
    /// The first operand must be one of these. Empty with `script_verbs`
    /// empty: any invocation of the program.
    verbs: &'static [&'static str],
    /// Options before the verb that take a value (`--prefix dir test`).
    value_flags: &'static [&'static str],
    /// A verb after which the next operand names a package script.
    script_verbs: &'static [&'static str],
    /// The script names that are heavy. A trailing `*` is a prefix.
    scripts: &'static [&'static str],
    when: Cond,
    pub kind: Kind,
    /// How to run it smaller, when there is a documented way.
    pub cap: Option<&'static str>,
    /// What to tell the model about an [`Kind::Advise`] row.
    pub advice: &'static str,
}

const NONE: &[&str] = &[];

/// The table. Order does not matter; the first heavy row that matches names
/// the command.
pub(super) const ROWS: &[Row] = &[
    Row {
        id: "cargo",
        programs: &["cargo"],
        verbs: &[
            "build", "b", "check", "c", "clippy", "test", "t", "bench", "nextest", "llvm-cov",
            "doc",
        ],
        value_flags: &["-Z", "-C", "--config", "--color"],
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::Always,
        kind: Kind::Heavy,
        cap: Some("prefix it with CARGO_BUILD_JOBS=2, and for tests add -- --test-threads=2"),
        advice: "",
    },
    Row {
        id: "go",
        programs: &["go"],
        verbs: &["build", "test"],
        value_flags: &["-C"],
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::Always,
        kind: Kind::Heavy,
        cap: Some("prefix it with GOMAXPROCS=4 and pass -p 2"),
        advice: "",
    },
    Row {
        id: "package-script",
        programs: &["npm", "bun", "pnpm"],
        verbs: &["test"],
        value_flags: &["--prefix", "-C", "--filter", "-F", "--cwd", "--dir"],
        script_verbs: &["run", "run-script"],
        scripts: &["test*", "ci:local"],
        when: Cond::Always,
        kind: Kind::Heavy,
        cap: None,
        advice: "",
    },
    Row {
        id: "vitest-jest",
        programs: &["vitest", "jest"],
        verbs: NONE,
        value_flags: NONE,
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::Always,
        kind: Kind::Heavy,
        cap: Some("pass --maxWorkers=2 to jest; vitest has no cap GitPulse has confirmed"),
        advice: "",
    },
    Row {
        id: "node-vitest-jest",
        programs: &["node"],
        verbs: NONE,
        value_flags: NONE,
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::ArgsContain(&["/vitest/", "/jest/", "/jest-cli/"]),
        kind: Kind::Heavy,
        cap: None,
        advice: "",
    },
    Row {
        id: "grep-recursive",
        programs: &["grep", "egrep", "fgrep"],
        verbs: NONE,
        value_flags: NONE,
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::All(&[
            Cond::Flags(&["-r", "-R", "--recursive", "--dereference-recursive"]),
            Cond::Any(&[
                Cond::Flags(&["-z", "-a", "--null-data", "--text"]),
                Cond::NoFlags(&["--exclude-dir"]),
            ]),
        ]),
        kind: Kind::Advise,
        cap: None,
        advice: "a recursive grep without --exclude-dir, or with -z/-a/--null-data, once grew to \
                 21 GiB on this host. Use rg with -g '!target*' and --max-filesize, or add \
                 --exclude-dir for build and dependency directories, and pipe into a bounded \
                 reader (head).",
    },
    Row {
        id: "rg-unrestricted",
        programs: &["rg"],
        verbs: NONE,
        value_flags: NONE,
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::All(&[
            Cond::Flags(&["-uu", "-uuu", "--no-ignore"]),
            Cond::NoArgPrefix(&["!", "--glob=!", "--iglob=!", "-g!"]),
        ]),
        kind: Kind::Advise,
        cap: None,
        advice: "rg with -uu or --no-ignore and no exclusion reads build output and was killed \
                 at 40.8 GiB on this host. Add -g '!target*' (and the other build directories) \
                 and --max-filesize 1M.",
    },
    Row {
        id: "go-test-unbounded",
        programs: &["go"],
        verbs: &["test"],
        value_flags: &["-C"],
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::All(&[
            Cond::Any(&[Cond::Args(&["./..."]), Cond::Flags(&["-fuzz"])]),
            Cond::NoFlags(&["-timeout"]),
        ]),
        kind: Kind::Advise,
        cap: None,
        advice: "go test ./... or -fuzz with no -timeout reached 120 GiB on this host. Pass \
                 -timeout (for example -timeout 10m) and set GOMEMLIMIT.",
    },
    Row {
        id: "python-train-bench",
        programs: &["python*"],
        verbs: NONE,
        value_flags: NONE,
        script_verbs: NONE,
        scripts: NONE,
        when: Cond::ArgsContain(&["train", "bench"]),
        kind: Kind::Advise,
        cap: None,
        advice: "a Python training or benchmark run once reached 74.8 GiB on this host. Bound \
                 its memory (a smaller batch or sample, or a ulimit) before running it.",
    },
];

/// A program that makes the rest of its line exempt: the heavy-lane holder
/// runs its command through it, so the lane is already booked.
const HOLDERS: &[&str] = &["mac_heavy.sh"];

/// Shells whose `-c` operand is a command line of its own.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// Programs that run the next command: skipped to reach it.
struct Wrapper {
    name: &'static str,
    /// Options that take a value, so the value is not mistaken for the program.
    value_flags: &'static [&'static str],
    /// When set, the program only wraps if its first operand is one of these
    /// (`pnpm exec vitest`, not `pnpm test`).
    verbs: &'static [&'static str],
}

const WRAPPERS: &[Wrapper] = &[
    Wrapper {
        name: "env",
        value_flags: &["-u", "-C", "-S", "-P"],
        verbs: &[],
    },
    Wrapper {
        name: "time",
        value_flags: &["-f", "-o"],
        verbs: &[],
    },
    Wrapper {
        name: "nice",
        value_flags: &["-n"],
        verbs: &[],
    },
    Wrapper {
        name: "nohup",
        value_flags: &[],
        verbs: &[],
    },
    Wrapper {
        name: "exec",
        value_flags: &["-a"],
        verbs: &[],
    },
    Wrapper {
        name: "command",
        value_flags: &[],
        verbs: &[],
    },
    Wrapper {
        name: "builtin",
        value_flags: &[],
        verbs: &[],
    },
    Wrapper {
        name: "npx",
        value_flags: &["-p", "--package", "-c"],
        verbs: &[],
    },
    Wrapper {
        name: "bunx",
        value_flags: &[],
        verbs: &[],
    },
    Wrapper {
        name: "pnpx",
        value_flags: &[],
        verbs: &[],
    },
    Wrapper {
        name: "pnpm",
        value_flags: &["--filter", "-F", "--dir", "-C"],
        verbs: &["exec", "dlx"],
    },
    Wrapper {
        name: "npm",
        value_flags: &["--prefix", "-C"],
        verbs: &["exec"],
    },
    Wrapper {
        name: "bun",
        value_flags: &["--cwd"],
        verbs: &["x"],
    },
];

/// Words that begin a command without being a program.
const KEYWORDS: &[&str] = &[
    "!", "if", "then", "else", "elif", "do", "while", "until", "{", "(",
];

/// How deep a `bash -c '…'` is read.
const MAX_SHELL_DEPTH: usize = 3;

/// The longest program label put in a message.
const LABEL_CHARS: usize = 48;

/// A row that matched, and what the command was called.
#[derive(Debug, Clone)]
pub(super) struct Hit {
    pub row: &'static Row,
    /// The program and its verb, as in `cargo test`: never the whole line.
    pub label: String,
}

#[derive(Debug, Default)]
pub(super) struct Classified {
    /// The first heavy row a command in the line matched.
    pub heavy: Option<Hit>,
    /// Every advise-only row it matched, once each.
    pub advise: Vec<Hit>,
    /// Some command in the line is a `cargo llvm-cov` that builds, whichever
    /// heavy command came first: it writes instrumented output many times the
    /// size of an ordinary build's, so the load gate asks for disk headroom
    /// before the line starts (`load_gate`).
    pub coverage_build: bool,
    /// A word of the table was in the line but the line could not be read, so
    /// nothing above says anything about it. Never a refusal.
    pub unreadable: bool,
}

impl Classified {
    pub fn is_empty(&self) -> bool {
        self.heavy.is_none() && self.advise.is_empty() && !self.unreadable
    }
}

/// Reads `command` against the table.
pub(super) fn classify(command: &str) -> Classified {
    let mut out = Classified::default();
    if !names_a_program(command) {
        return out;
    }
    scan(command, 0, &mut out);
    out
}

/// A cheap read of the raw line: whether any program the table knows appears at
/// a word boundary. A line that cannot hold one never reaches the lexer.
fn names_a_program(command: &str) -> bool {
    let bytes = command.as_bytes();
    let boundary = |b: u8| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.');
    let known = ROWS
        .iter()
        .flat_map(|row| row.programs.iter().copied())
        .chain(HOLDERS.iter().copied());
    known.into_iter().any(|name| {
        command
            .match_indices(name.trim_end_matches('*'))
            .any(|(at, _)| at.checked_sub(1).is_none_or(|i| boundary(bytes[i])))
    })
}

fn scan(command: &str, depth: usize, out: &mut Classified) {
    let Some(lines) = command_words(command) else {
        out.unreadable = true;
        return;
    };
    for words in &lines {
        classify_words(words, depth, out);
    }
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn glob_eq(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Skips options (and the values of the ones that take one) from `at`.
fn skip_options(words: &[String], mut at: usize, value_flags: &[&str]) -> usize {
    while let Some(word) = words.get(at) {
        if !word.starts_with('-') || word == "-" {
            break;
        }
        at += 1 + usize::from(value_flags.contains(&word.as_str()));
    }
    at
}

fn classify_words(words: &[String], depth: usize, out: &mut Classified) {
    let mut at = 0;
    let program = loop {
        let Some(word) = words.get(at) else { return };
        if is_assignment(word) || KEYWORDS.contains(&word.as_str()) {
            at += 1;
            continue;
        }
        let base = base_name(word);
        if HOLDERS.contains(&base) {
            return;
        }
        let wrapper = WRAPPERS.iter().find(|w| {
            w.name == base
                && (w.verbs.is_empty()
                    || words
                        .get(skip_options(words, at + 1, w.value_flags))
                        .is_some_and(|verb| w.verbs.contains(&verb.as_str())))
        });
        match wrapper {
            Some(w) => {
                at = skip_options(words, at + 1, w.value_flags);
                if !w.verbs.is_empty() {
                    at += 1;
                }
            }
            None => break base,
        }
    };
    let args = &words[at + 1..];
    if SHELLS.contains(&program) {
        if args
            .first()
            .is_some_and(|first| HOLDERS.contains(&base_name(first)))
        {
            return;
        }
        if let Some(script) = shell_script(args) {
            if depth < MAX_SHELL_DEPTH {
                scan(script, depth + 1, out);
            } else {
                out.unreadable = true;
            }
        }
    }
    for row in ROWS {
        if !row.programs.iter().any(|p| glob_eq(p, program)) || !row_matches(row, args) {
            continue;
        }
        let coverage = llvm_cov(program, args, row);
        if coverage == Some(false) {
            // `cargo llvm-cov clean|report|show-env` builds nothing: not heavy.
            // `clean` is the command that gives the disk back, and refusing it
            // for a full disk would be the failure the gate exists to prevent.
            continue;
        }
        let hit = Hit {
            row,
            label: label_of(program, args, row),
        };
        out.coverage_build |= coverage == Some(true);
        match row.kind {
            Kind::Heavy if out.heavy.is_none() => out.heavy = Some(hit),
            Kind::Heavy => {}
            Kind::Advise if !out.advise.iter().any(|seen| seen.row.id == row.id) => {
                out.advise.push(hit);
            }
            Kind::Advise => {}
        }
    }
}

/// `cargo llvm-cov`'s subcommands that read or remove its output and compile
/// nothing.
const LLVM_COV_NON_BUILDING: &[&str] = &["clean", "report", "show-env"];

/// Options of `cargo llvm-cov` that take a value, so the value is not mistaken
/// for the subcommand. This is the `<ARG>` options of `cargo llvm-cov --help`
/// (cargo-llvm-cov 0.8.x), not a recollection of them.
const LLVM_COV_VALUE_FLAGS: &[&str] = &[
    "--output-path",
    "--output-dir",
    "--failure-mode",
    "--ignore-filename-regex",
    "--fail-under-functions",
    "--fail-under-lines",
    "--fail-under-file-lines",
    "--fail-under-regions",
    "--fail-uncovered-lines",
    "--fail-uncovered-regions",
    "--fail-uncovered-functions",
    "--dep-coverage",
    "--bin",
    "--example",
    "--test",
    "--bench",
    "-p",
    "--package",
    "--exclude",
    "--exclude-from-test",
    "--exclude-from-report",
    "-j",
    "--jobs",
    "--profile",
    "-F",
    "--features",
    "--target",
    "--color",
    "--manifest-path",
    "-Z",
];

/// The rest of its options, which take no value.
const LLVM_COV_PLAIN_FLAGS: &[&str] = &[
    "--json",
    "--lcov",
    "--cobertura",
    "--codecov",
    "--text",
    "--html",
    "--open",
    "--summary-only",
    "--show-instantiations",
    "--no-cfg-coverage",
    "--no-cfg-coverage-nightly",
    "--no-report",
    "--no-clean",
    "--no-rustc-wrapper",
    "--show-missing-lines",
    "--include-build-script",
    "--skip-functions",
    "--branch",
    "--mcdc",
    "--doctests",
    "--no-run",
    "--no-fail-fast",
    "--ignore-run-fail",
    "-q",
    "--quiet",
    "--lib",
    "--bins",
    "--examples",
    "--tests",
    "--benches",
    "--all-targets",
    "--doc",
    "--workspace",
    "--all",
    "-r",
    "--release",
    "--all-features",
    "--no-default-features",
    "--coverage-target-only",
    "--coverage-host-only",
    "-v",
    "--verbose",
    "--remap-path-prefix",
    "--include-ffi",
    "--keep-going",
    "--ignore-rust-version",
    "--frozen",
    "--locked",
    "--offline",
];

/// What `cargo llvm-cov` is asked to do, read from the words after it.
enum LlvmCov {
    /// A subcommand that reads or removes output (`clean`, `report`,
    /// `show-env`), or `--help` / `--version`: it compiles nothing.
    Reads,
    /// Anything else, including every line this cannot read with certainty.
    Builds,
}

/// For `cargo llvm-cov`: whether the invocation builds (`Some(true)`) or only
/// reads or removes output (`Some(false)`); `None` for anything else.
///
/// Reads only when the subcommand is certainly one of the non-building ones.
/// The subcommand is the first word that is not an option or an option's
/// value; the scan stops at `--` (what follows is for the test binary), and at
/// an option it does not know (it may take a value that is then mistaken for
/// the subcommand), and both mean a build: a wrong "reads" lets a 60 GiB
/// build start on a full disk, a wrong "builds" only asks for the headroom.
fn llvm_cov(program: &str, args: &[String], row: &Row) -> Option<bool> {
    if row.id != "cargo" || program != "cargo" {
        return None;
    }
    let at = first_operand(args, row.value_flags)?;
    if args[at] != "llvm-cov" {
        return None;
    }
    Some(matches!(llvm_cov_reading(&args[at + 1..]), LlvmCov::Builds))
}

fn llvm_cov_reading(words: &[String]) -> LlvmCov {
    let mut i = 0;
    while let Some(word) = words.get(i) {
        i += 1;
        if word == "--" {
            return LlvmCov::Builds;
        }
        if matches!(word.as_str(), "-h" | "--help" | "-V" | "--version") {
            return LlvmCov::Reads;
        }
        if !word.starts_with('-') || word == "-" {
            return if LLVM_COV_NON_BUILDING.contains(&word.as_str()) {
                LlvmCov::Reads
            } else {
                LlvmCov::Builds
            };
        }
        // `--name=value` carries its value; a short option may too (`-j4`).
        let (name, inline) = match word.split_once('=') {
            Some((name, _)) => (name, true),
            None => (word.as_str(), false),
        };
        if LLVM_COV_VALUE_FLAGS.contains(&name) {
            i += usize::from(!inline);
        } else if LLVM_COV_PLAIN_FLAGS.contains(&name)
            || (!word.starts_with("--")
                && word
                    .get(..2)
                    .is_some_and(|short| LLVM_COV_VALUE_FLAGS.contains(&short)))
        {
            continue;
        } else {
            return LlvmCov::Builds;
        }
    }
    LlvmCov::Builds
}

/// The operand after `-c` in a shell's own options, when there is one.
fn shell_script(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while let Some(word) = args.get(i) {
        i += 1;
        if word == "--" || !word.starts_with('-') || word.len() < 2 {
            return None;
        }
        if !word.starts_with("--") && word.contains('c') {
            return args.get(i).map(String::as_str);
        }
    }
    None
}

fn first_operand(args: &[String], value_flags: &[&str]) -> Option<usize> {
    let mut at = skip_options(args, 0, value_flags);
    // `cargo +nightly test`: the toolchain is not the verb.
    if args.get(at).is_some_and(|word| word.starts_with('+')) {
        at = skip_options(args, at + 1, value_flags);
    }
    (at < args.len()).then_some(at)
}

fn row_matches(row: &Row, args: &[String]) -> bool {
    let verbed = !(row.verbs.is_empty() && row.script_verbs.is_empty());
    if verbed {
        let Some(at) = first_operand(args, row.value_flags) else {
            return false;
        };
        let verb = args[at].as_str();
        let named = row.verbs.contains(&verb);
        let script = row.script_verbs.contains(&verb)
            && args
                .get(at + 1)
                .is_some_and(|name| row.scripts.iter().any(|s| glob_eq(s, name)));
        if !(named || script) {
            return false;
        }
    }
    holds(&row.when, args)
}

fn flag_names(arg: &str, name: &str) -> bool {
    let is_cluster_letter = name.len() == 2 && !name.starts_with("--");
    if is_cluster_letter {
        return arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(&name[1..]);
    }
    arg == name
        || arg
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('='))
}

fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| names.iter().any(|name| flag_names(arg, name)))
}

fn holds(cond: &Cond, args: &[String]) -> bool {
    match cond {
        Cond::Always => true,
        Cond::Flags(names) => has_flag(args, names),
        Cond::NoFlags(names) => !has_flag(args, names),
        Cond::Args(names) => args.iter().any(|a| names.contains(&a.as_str())),
        Cond::ArgsContain(parts) => args.iter().any(|a| parts.iter().any(|p| a.contains(p))),
        Cond::NoArgPrefix(prefixes) => !args
            .iter()
            .any(|a| prefixes.iter().any(|p| a.starts_with(p))),
        Cond::All(all) => all.iter().all(|c| holds(c, args)),
        Cond::Any(any) => any.iter().any(|c| holds(c, args)),
    }
}

/// `cargo test`, `npm run test:browser`, `vitest`: the program and the word that
/// names the work, cut to [`LABEL_CHARS`] and to characters that are safe to
/// repeat to a model.
fn label_of(program: &str, args: &[String], row: &Row) -> String {
    let mut label = program.to_string();
    if let Some(at) = first_operand(args, row.value_flags) {
        if !(row.verbs.is_empty() && row.script_verbs.is_empty()) {
            label.push(' ');
            label.push_str(&args[at]);
            if row.script_verbs.contains(&args[at].as_str()) {
                if let Some(name) = args.get(at + 1) {
                    label.push(' ');
                    label.push_str(name);
                }
            }
        }
    }
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " -_.:/+".contains(c) {
                c
            } else {
                '?'
            }
        })
        .take(LABEL_CHARS)
        .collect()
}

#[cfg(test)]
#[path = "heavy_commands_tests.rs"]
mod tests;
