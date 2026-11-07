//! Which agent session made a commit, inferred from rows the ledger already
//! holds.
//!
//! Git records nothing about who moved a ref, and a reflog row says so
//! (`actor_id: reflog`, no `session_id`). What the ledger *does* hold is the
//! transcript's `session.command` rows, each dated by the transcript, and the
//! reflog rows, each dated by git. A `git commit` run by an agent's Bash tool
//! is a command row followed, a moment later, by the reflog row of the commit
//! it made; this module matches the two and says how sure it is.
//!
//! Nothing is written. The link is *computed* at read time from existing
//! columns, so it needs no schema change and no new writer, a row recorded by
//! an older `gitpulsed` is read exactly like one recorded by this build, and a
//! transcript row that is ingested after its reflog row (the two sources catch
//! up on their own clocks) still links. The cost of that choice is that the
//! answer is recomputed per read, so every scan here is bounded.
//!
//! The one outcome this must never give is a confident *wrong* link, so every
//! rule errs toward null with a named [`basis`]:
//!
//! * only a command that is known to have **succeeded** can be credited; one
//!   that failed, was refused, or whose result was never read cannot (a
//!   hook-rejected `git commit` made nothing);
//! * only a command aimed at the commit's own worktree counts (`git -C other`
//!   and `cd other &&` do not), and only a form that can create a commit
//!   (`--dry-run`, `--abort`, `-h` do not);
//! * only a reflog entry that *created* the commit is its origin: a
//!   fast-forward, reset or checkout moves a ref onto a commit someone else
//!   made, and crediting the mover would be crediting the wrong session;
//! * a read that was cut at its row bound says so (`scan_capped`) whenever a
//!   hidden row could have changed the answer;
//! * two sessions that could each have made it give no session and
//!   `ambiguous: true`.
//!
//! What survives is still an *inference from time*: a command that could have
//! made the commit, in the same worktree, shortly before it.

use super::{ids, with_access, Access, LedgerError};
use rusqlite::{params_from_iter, types::Value, Connection};
use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

/// How long before a reflog entry a command may have started and still be
/// credited with it: two minutes.
///
/// A command row is dated when the agent *issued* the Bash call; the reflog
/// entry is written when `git` finishes, after any `pre-commit` /
/// `commit-msg` hook. Hooks that run a type check or a test suite take tens of
/// seconds, so a window of a few seconds would drop the commits that matter
/// most (the ones with a slow gate). A rebase replays every pick inside one
/// command, so its entries land well after it started too. Longer widens the
/// chance that a second session's command also fits, which the matcher
/// reports as ambiguity rather than resolving, so the cost of a generous
/// window is a null answer, not a wrong one.
pub const LINK_WINDOW_MS: u64 = 120_000;

/// Git writes a reflog entry's time to the whole second, so the move happened
/// somewhere in `[t, t + 999 ms]`. A command that started inside that second is
/// not after its own commit; one that started later than it is clock skew (the
/// transcript's clock against git's) and does not link.
pub const REFLOG_GRANULARITY_MS: u64 = 1_000;

/// The newest rows (by id) one call reads: a row older than that is not looked
/// at. The ledger retains 10 000 events by default, so this reads all of a
/// default ledger and bounds a larger one to a single indexed range. A
/// capped read answers only when it can show the rows it did not read cannot
/// matter ([`HIDDEN_ROW_SKEW_MS`]); otherwise [`basis::SCAN_CAPPED`].
pub const LINK_SCAN_ROWS: i64 = 20_000;

/// Most commits one call links; the rest are reported as not examined.
pub const LINK_MAX_COMMITS: usize = 256;

/// Slack on the proof that a row past the scan bound cannot matter.
///
/// An event cannot be recorded before it happened, so a row appended before
/// the window opened holds nothing in the window. That is read off the append
/// clock (`ts_utc`), which can disagree with the clock that dated the event
/// by an NTP step or a laptop that slept; ten minutes covers those, and an
/// append time inside the slack is treated as possibly in the window.
pub const HIDDEN_ROW_SKEW_MS: u64 = 600_000;

/// Why a commit has, or has no, session. The text is the wire value of
/// `session_basis`.
pub mod basis {
    /// A command in the window, from one session.
    pub const INFERRED: &str = "inferred_from_command";
    /// A commit nobody's transcript shows a git command for: made by hand, or
    /// by a tool whose transcript GitPulse does not read.
    pub const NO_SESSION_COMMAND: &str = "no_session_command";
    /// Commands from two or more sessions fit the window.
    pub const AMBIGUOUS: &str = "ambiguous_sessions";
    /// A git command in the worktree came before the window, so it cannot have
    /// made this commit.
    pub const COMMAND_OUTSIDE_WINDOW: &str = "command_outside_window";
    /// A git command in the worktree is dated after the commit: clock skew or
    /// a different commit, and not a link either way.
    pub const COMMAND_AFTER_REFLOG: &str = "command_after_reflog";
    /// A git command row with no usable event time could have made the commit
    /// (an old row, or one dated by ingest rather than the transcript).
    pub const COMMAND_UNTIMED: &str = "command_untimed";
    /// The only command that fits is one whose result was never read, so
    /// whether it made anything is not known.
    pub const COMMAND_OUTCOME_UNKNOWN: &str = "command_outcome_unknown";
    /// The ledger has no reflog row for the commit: it was not made in a
    /// replayed worktree (another machine, a clone) or has not been replayed.
    pub const NO_REFLOG_ENTRY: &str = "no_reflog_entry";
    /// The ledger names the commit only in reflog entries that moved a ref
    /// onto it (a fast-forward, reset, checkout): none of them made it.
    pub const NO_ORIGIN_ENTRY: &str = "no_origin_entry";
    /// The commit's origin entry has no event time from git.
    pub const REFLOG_UNTIMED: &str = "reflog_entry_untimed";
    /// The read stopped at [`super::LINK_SCAN_ROWS`] and a row it did not read
    /// could change the answer.
    pub const SCAN_CAPPED: &str = "scan_capped";
    /// This repository has no ledger yet.
    pub const LEDGER_ABSENT: &str = "ledger_not_initialised";
    /// The ledger could not be read.
    pub const LEDGER_UNREADABLE: &str = "ledger_unreadable";
    /// Past [`super::LINK_MAX_COMMITS`] in one call.
    pub const NOT_EXAMINED: &str = "not_examined_batch_cap";

    /// Every value `session_basis` can take, for the schema test.
    pub const ALL: [&str; 14] = [
        INFERRED,
        NO_SESSION_COMMAND,
        AMBIGUOUS,
        COMMAND_OUTSIDE_WINDOW,
        COMMAND_AFTER_REFLOG,
        COMMAND_UNTIMED,
        COMMAND_OUTCOME_UNKNOWN,
        NO_REFLOG_ENTRY,
        NO_ORIGIN_ENTRY,
        REFLOG_UNTIMED,
        SCAN_CAPPED,
        LEDGER_ABSENT,
        LEDGER_UNREADABLE,
        NOT_EXAMINED,
    ];
}

/// What the ledger says about one commit's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitLink {
    /// The session, only when exactly one fits.
    pub session_id: Option<String>,
    /// One of [`basis`]. Always set, whether or not a session was found.
    pub basis: &'static str,
    /// More than one session fits; `session_id` is then `None`.
    pub ambiguous: bool,
    /// When the credited command started, as the transcript dated it.
    pub command_at_ms: Option<u64>,
    /// The command line credited, so a reader can see what was matched.
    pub command: Option<String>,
    /// The window the match was made in: [`LINK_WINDOW_MS`].
    pub window_ms: u64,
}

impl CommitLink {
    fn none(basis: &'static str) -> Self {
        Self {
            session_id: None,
            basis,
            ambiguous: false,
            command_at_ms: None,
            command: None,
            window_ms: LINK_WINDOW_MS,
        }
    }
}

/// One ledger row, with the columns the match reads.
struct Row {
    id: i64,
    /// `worktree_path` as stored; the anchor checkout's NULL is the empty string.
    worktree: String,
    is_command: bool,
    session_id: Option<String>,
    object: Option<String>,
    outcome: String,
    /// `detail_json.result` of a transcript row: `ok` or `error`, absent when
    /// the call's result was never read.
    result: Option<String>,
    after_ref: Option<String>,
    ts_utc: String,
    /// The event's own time, only when its basis is the one this kind of row's
    /// time can be trusted from: the transcript for a command, git for a reflog
    /// entry. A row stamped at ingest, or with no stamp, has `None`.
    at_ms: Option<u64>,
}

/// Links each of `commits` to the session whose command most plausibly made it.
///
/// One bounded read answers the whole batch. Every commit in `commits` (up to
/// [`LINK_MAX_COMMITS`]) has an entry in the answer.
pub fn link_commits(repo_path: &str, commits: &[String]) -> HashMap<String, CommitLink> {
    let wanted: BTreeSet<&str> = commits
        .iter()
        .map(String::as_str)
        .filter(|sha| !sha.is_empty())
        .collect();
    let mut answers = HashMap::with_capacity(wanted.len());
    if wanted.is_empty() {
        return answers;
    }
    let (examined, skipped): (Vec<&str>, Vec<&str>) = {
        let all: Vec<&str> = wanted.into_iter().collect();
        let (head, tail) = all.split_at(all.len().min(LINK_MAX_COMMITS));
        (head.to_vec(), tail.to_vec())
    };
    for sha in skipped {
        answers.insert(sha.to_string(), CommitLink::none(basis::NOT_EXAMINED));
    }
    if !super::is_initialised(repo_path) {
        for sha in examined {
            answers.insert(sha.to_string(), CommitLink::none(basis::LEDGER_ABSENT));
        }
        return answers;
    }
    let read = with_access(repo_path, Access::ReadOnly, |conn| {
        let (rows, floor) = read_rows(conn, &examined)?;
        let (reflogs, commands): (Vec<Row>, Vec<Row>) =
            rows.into_iter().partition(|row| !row.is_command);
        let mut keys = Keys::new(repo_path);
        let commands = classify(commands, &mut keys);
        let mut linked = Vec::with_capacity(examined.len());
        for sha in &examined {
            let mine: Vec<&Row> = reflogs
                .iter()
                .filter(|row| row.after_ref.as_deref() == Some(*sha))
                .collect();
            let hidden_since = |since_ms: u64| match floor {
                None => false,
                Some(floor) => hidden_rows_since(conn, floor, since_ms),
            };
            linked.push((
                (*sha).to_string(),
                link_one(&mine, &commands, &mut keys, floor.is_some(), &hidden_since),
            ));
        }
        Ok(linked)
    });
    match read {
        Ok(linked) => answers.extend(linked),
        Err(_) => {
            for sha in examined {
                answers.insert(sha.to_string(), CommitLink::none(basis::LEDGER_UNREADABLE));
            }
        }
    }
    answers
}

/// The reflog rows of `commits` and every git-looking `session.command` row,
/// among the newest [`LINK_SCAN_ROWS`] events, and the id at the bound when
/// older rows exist (`None` when the read covered the whole ledger).
///
/// The range is on the primary key, so the read touches at most that many rows
/// however large the ledger is. Event time comes from `detail_json` rather than
/// the typed `occurred_at` column so a ledger a generation-1 writer last opened
/// reads the same as a migrated one.
fn read_rows(conn: &Connection, commits: &[&str]) -> Result<(Vec<Row>, Option<i64>), LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let newest: Option<i64> = conn
        .query_row("SELECT MAX(id) FROM events", [], |row| row.get(0))
        .map_err(failed)?;
    let Some(newest) = newest else {
        return Ok((Vec::new(), None));
    };
    let floor = newest.saturating_sub(LINK_SCAN_ROWS);
    let capped = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE id <= ?1)",
            [floor],
            |row| row.get::<_, bool>(0),
        )
        .map_err(failed)?;
    let marks = vec!["?"; commits.len()].join(",");
    let sql = format!(
        "SELECT id, worktree_path, action, session_id, object, outcome, after_ref, ts_utc,
                CASE WHEN json_valid(detail_json)
                     THEN json_extract(detail_json, '$.occurred_at') END,
                CASE WHEN json_valid(detail_json)
                     THEN json_extract(detail_json, '$.occurred_at_basis') END,
                CASE WHEN json_valid(detail_json)
                     THEN json_extract(detail_json, '$.result') END
         FROM events
         WHERE id > ?1
           AND ((action = 'session.command' AND object LIKE '%git%')
                OR (action LIKE 'reflog.%' AND after_ref IN ({marks})))"
    );
    let mut values = vec![Value::Integer(floor)];
    values.extend(commits.iter().map(|sha| Value::Text((*sha).to_string())));
    let mut stmt = conn.prepare(&sql).map_err(failed)?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            let action: String = row.get(2)?;
            let is_command = action == "session.command";
            let at: Option<String> = row.get(8)?;
            let stamp_basis: Option<String> = row.get(9)?;
            let trusted = if is_command { "transcript" } else { "reflog" };
            Ok(Row {
                id: row.get(0)?,
                worktree: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                is_command,
                session_id: row.get(3)?,
                object: row.get(4)?,
                outcome: row.get(5)?,
                after_ref: row.get(6)?,
                ts_utc: row.get(7)?,
                result: row.get(10)?,
                at_ms: at
                    .filter(|_| stamp_basis.as_deref() == Some(trusted))
                    .and_then(|text| ids::parse_event_time(&text)),
            })
        })
        .map_err(failed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed)?;
    Ok((rows, capped.then_some(floor)))
}

/// Whether any row past the scan bound (`id <= floor`) was appended at or after
/// `since_ms` less [`HIDDEN_ROW_SKEW_MS`]: such a row could hold an event in a
/// window that opens at `since_ms`, because an event is recorded after it
/// happens. A probe that cannot run answers yes, never no.
///
/// Reads through `idx_events_ts`, from the window start forward, and stops at
/// the first hidden row, so it costs the rows appended since the window and no
/// more.
fn hidden_rows_since(conn: &Connection, floor: i64, since_ms: u64) -> bool {
    let since = ids::iso8601_utc(since_ms.saturating_sub(HIDDEN_ROW_SKEW_MS));
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events INDEXED BY idx_events_ts
                       WHERE ts_utc >= ?1 AND id <= ?2)",
        rusqlite::params![since, floor],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(true)
}

/// Worktree identities, resolved once per call.
///
/// Two spellings of one checkout (`/var` against `/private/var`, a symlink, a
/// trailing slash) are one worktree. The ledger's own [`super::canonical_repo`]
/// is the owner of that rule for paths that exist; this adds the one case it
/// leaves open, a path that no longer exists (a removed lane), by resolving its
/// nearest existing ancestor and re-attaching the rest, so a symlinked parent
/// does not make one worktree look like two.
struct Keys {
    anchor: String,
    seen: HashMap<String, String>,
}

impl Keys {
    fn new(repo_path: &str) -> Self {
        Self {
            anchor: resolve_path(repo_path),
            seen: HashMap::new(),
        }
    }

    /// The identity of a stored `worktree_path`; the anchor checkout stores
    /// none and is the repository itself.
    fn of(&mut self, stored: &str) -> String {
        if stored.is_empty() {
            return self.anchor.clone();
        }
        self.seen
            .entry(stored.to_string())
            .or_insert_with(|| resolve_path(stored))
            .clone()
    }
}

fn resolve_path(raw: &str) -> String {
    if let Ok(real) = std::fs::canonicalize(raw) {
        return real.to_string_lossy().into_owned();
    }
    let mut tail = PathBuf::new();
    let mut at = Path::new(raw);
    while let (Some(parent), Some(name)) = (at.parent(), at.file_name()) {
        tail = Path::new(name).join(&tail);
        if let Ok(real) = std::fs::canonicalize(parent) {
            return real.join(tail).to_string_lossy().into_owned();
        }
        at = parent;
    }
    super::canonical_repo(raw)
}

/// A command row that may have made a commit, with where it ran.
struct Command {
    row: Row,
    worktree: String,
}

/// The command rows that run a commit-making git form against their own
/// worktree, with that worktree resolved.
fn classify(rows: Vec<Row>, keys: &mut Keys) -> Vec<Command> {
    rows.into_iter()
        .filter_map(|row| {
            let worktree = keys.of(&row.worktree);
            let object = row.object.as_deref()?;
            runs_git_mutation(object, Path::new(&worktree)).then_some(Command { row, worktree })
        })
        .collect()
}

/// What a reflog entry did to the ref.
///
/// Only an entry that *created* the commit is its origin. The message is git's
/// own (`%gs`), the part of the `object` after the commit id, and the forms
/// that create a commit are listed; every other form (`merge x: Fast-forward`,
/// `pull: Fast-forward`, `reset`, `checkout`, `clone`, `rebase (start)` /
/// `(finish)`, `branch`, and any form this list has not heard of) is a move, so
/// an unknown form can only cost a link, never invent one.
///
/// Forms verified against git's own reflog: `commit: m`, `commit (initial): m`,
/// `commit (amend): m`, `commit (merge): m`, `cherry-pick: m`, `revert: m`,
/// `am: m`, `rebase (pick|continue|fixup|squash|reword|edit): m`,
/// `pull --rebase (pick): m`, `merge b: Merge made by the 'ort' strategy.` and
/// `pull: Merge made by ...`.
fn creates_commit(object: &str) -> bool {
    let message = object.split_once(' ').map_or("", |(_, rest)| rest);
    let Some((head, tail)) = message.split_once(": ") else {
        return false;
    };
    let verb = head.split([' ', '(']).next().unwrap_or("");
    let step = head
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'));
    match verb {
        "commit" | "revert" | "am" => true,
        "cherry-pick" => step.is_none_or(|s| s == "continue"),
        "rebase" | "pull" => {
            matches!(
                step,
                Some("pick" | "continue" | "fixup" | "squash" | "reword" | "edit")
            ) || (verb == "pull" && tail.starts_with("Merge made by"))
        }
        "merge" => tail.starts_with("Merge made by"),
        _ => false,
    }
}

/// The matching rule, over rows already read. `reflogs` are the commit's own;
/// `capped` says the read stopped at the row bound and `hidden_since` whether a
/// row past it was appended after a given time.
fn link_one(
    reflogs: &[&Row],
    commands: &[Command],
    keys: &mut Keys,
    capped: bool,
    hidden_since: &dyn Fn(u64) -> bool,
) -> CommitLink {
    if reflogs.is_empty() {
        return CommitLink::none(if capped {
            basis::SCAN_CAPPED
        } else {
            basis::NO_REFLOG_ENTRY
        });
    }
    let origins: Vec<&&Row> = reflogs
        .iter()
        .filter(|row| row.object.as_deref().is_some_and(creates_commit))
        .collect();
    if origins.is_empty() {
        // The entry that made it may be one the read did not reach.
        return CommitLink::none(if capped {
            basis::SCAN_CAPPED
        } else {
            basis::NO_ORIGIN_ENTRY
        });
    }
    // The earliest entry that made it; a later one (the same commit made
    // again, an amend that changed nothing) is the same link.
    let Some(entry) = origins
        .iter()
        .filter(|row| row.at_ms.is_some())
        .min_by_key(|row| (row.at_ms, row.id))
    else {
        return CommitLink::none(basis::REFLOG_UNTIMED);
    };
    let Some(at) = entry.at_ms else {
        return CommitLink::none(basis::REFLOG_UNTIMED);
    };
    let earliest = at.saturating_sub(LINK_WINDOW_MS);
    let latest = at.saturating_add(REFLOG_GRANULARITY_MS - 1);
    if capped && hidden_since(earliest) {
        return CommitLink::none(basis::SCAN_CAPPED);
    }
    let worktree = keys.of(&entry.worktree);

    // Commands of the commit's own worktree, by what is known of them.
    let mut ok: Vec<(&Row, u64)> = Vec::new();
    let mut unobserved: Vec<&Row> = Vec::new();
    let (mut untimed, mut before, mut after) = (false, false, false);
    for command in commands.iter().filter(|c| {
        c.worktree == worktree
            && c.row.outcome == "ok"
            && c.row.result.as_deref() != Some("error")
            && c.row.session_id.as_deref().is_some_and(|s| !s.is_empty())
    }) {
        let row = &command.row;
        match row.at_ms {
            Some(t) if (earliest..=latest).contains(&t) => {
                if row.result.as_deref() == Some("ok") {
                    ok.push((row, t));
                } else {
                    unobserved.push(row);
                }
            }
            Some(t) if t > latest => after |= t <= latest.saturating_add(LINK_WINDOW_MS),
            Some(t) => before |= t >= earliest.saturating_sub(LINK_WINDOW_MS),
            // It cannot have been recorded before it happened, so a row
            // appended before the window opened is ruled out by its append
            // time; one appended since cannot be.
            None => untimed |= ids::parse_event_time(&row.ts_utc).is_none_or(|ts| ts >= earliest),
        }
    }

    // A command whose result was never read may have made it, so it counts
    // toward ambiguity but never toward a link.
    let sessions: BTreeSet<&str> = ok
        .iter()
        .map(|(row, _)| row)
        .chain(unobserved.iter())
        .filter_map(|row| row.session_id.as_deref())
        .collect();
    match (sessions.len(), ok.is_empty()) {
        (0, _) => CommitLink::none(if untimed {
            basis::COMMAND_UNTIMED
        } else if after {
            basis::COMMAND_AFTER_REFLOG
        } else if before {
            basis::COMMAND_OUTSIDE_WINDOW
        } else {
            basis::NO_SESSION_COMMAND
        }),
        (1, true) => CommitLink::none(basis::COMMAND_OUTCOME_UNKNOWN),
        (1, false) => {
            // Several commands of one session (a commit, then its amend):
            // the nearest one is the one named.
            let (nearest, t) = ok
                .iter()
                .max_by_key(|(row, t)| (*t, row.id))
                .expect("one session has a command");
            CommitLink {
                session_id: nearest.session_id.clone(),
                basis: basis::INFERRED,
                ambiguous: false,
                command_at_ms: Some(*t),
                command: nearest.object.clone(),
                window_ms: LINK_WINDOW_MS,
            }
        }
        _ => CommitLink {
            ambiguous: true,
            ..CommitLink::none(basis::AMBIGUOUS)
        },
    }
}

/// The git subcommands whose run moves HEAD onto a commit.
const MUTATING_SUBCOMMANDS: [&str; 5] = ["commit", "merge", "rebase", "cherry-pick", "pull"];

/// Arguments that make a run create nothing: a dry run, a help page, or the
/// end of an operation (`rebase --abort`). `--continue` and `--skip` stay out:
/// they go on to create commits.
const NO_OP_ARGUMENTS: [&str; 7] = [
    "--dry-run",
    "-h",
    "--help",
    "--abort",
    "--quit",
    "--show-current-patch",
    "--edit-todo",
];

/// Global options that take their value as the next word.
const GIT_OPTIONS_WITH_VALUE: [&str; 3] = ["-c", "--exec-path", "--namespace"];

/// Whether `command`, run in `worktree`, runs a git form that can create a
/// commit *in that worktree*: `commit`, `merge`, `rebase`, `cherry-pick` or
/// `pull`, anywhere in a compound line, and not a dry run, a help page or an
/// abort. `git -C other`, `--work-tree`, `--git-dir` and an earlier
/// `cd other &&` move the target, and a run aimed at another checkout is not
/// this one's. A target that cannot be resolved (`~`, `$VAR`, a subshell)
/// counts as elsewhere.
///
/// Words are split on whitespace and shell separators, so a quoted message
/// that says `--dry-run` hides a real commit (a missed link), and one that says
/// "git commit" counts (one more candidate, which can only make a link
/// ambiguous). Both are the safe direction.
pub(crate) fn runs_git_mutation(command: &str, worktree: &Path) -> bool {
    let mut dir: Option<PathBuf> = Some(worktree.to_path_buf());
    for segment in command.split([';', '&', '|', '(', ')', '`', '\n']) {
        let words: Vec<&str> = segment
            .split_whitespace()
            .map(|word| word.trim_matches(|c| c == '"' || c == '\''))
            .filter(|word| !word.is_empty())
            .collect();
        if let Some("cd" | "pushd") = words.first().copied() {
            dir = words
                .iter()
                .skip(1)
                .find(|word| !word.starts_with('-') || **word == "-")
                .and_then(|target| step_into(dir.as_deref(), target));
            continue;
        }
        let Some(at) = words
            .iter()
            .position(|word| *word == "git" || word.ends_with("/git"))
        else {
            continue;
        };
        if git_run_creates_commit(&words[at + 1..], dir.as_deref(), worktree) {
            return true;
        }
    }
    false
}

/// One git invocation's words after `git`, aimed from `dir`.
fn git_run_creates_commit(words: &[&str], dir: Option<&Path>, worktree: &Path) -> bool {
    let mut target = dir.map(Path::to_path_buf);
    let mut work_tree: Option<Option<PathBuf>> = None;
    let mut git_dir: Option<Option<PathBuf>> = None;
    let mut rest = words.iter();
    let subcommand = loop {
        let Some(&word) = rest.next() else {
            return false;
        };
        if word == "-C" {
            target = rest
                .next()
                .and_then(|path| step_into(target.as_deref(), path));
        } else if let Some(path) = word.strip_prefix("--work-tree=") {
            work_tree = Some(step_into(target.as_deref(), path));
        } else if word == "--work-tree" {
            work_tree = Some(
                rest.next()
                    .and_then(|path| step_into(target.as_deref(), path)),
            );
        } else if let Some(path) = word.strip_prefix("--git-dir=") {
            git_dir = Some(step_into(target.as_deref(), path));
        } else if word == "--git-dir" {
            git_dir = Some(
                rest.next()
                    .and_then(|path| step_into(target.as_deref(), path)),
            );
        } else if GIT_OPTIONS_WITH_VALUE.contains(&word) {
            rest.next();
        } else if !word.starts_with('-') {
            break word;
        }
    };
    if !MUTATING_SUBCOMMANDS.contains(&subcommand) {
        return false;
    }
    let mut args = rest.copied();
    while let Some(arg) = args.next() {
        if NO_OP_ARGUMENTS.contains(&arg) {
            return false;
        }
        // A message's text is not a flag of the command.
        if matches!(arg, "-m" | "--message" | "-F" | "--file") {
            args.next();
        }
    }
    // Where the run takes effect: the work tree when one is named, else the
    // directory the git dir sits in, else where -C and cd led.
    let aimed = match (work_tree, git_dir) {
        (Some(tree), _) => tree,
        (None, Some(Some(dir))) if dir.file_name().is_some_and(|name| name == ".git") => {
            dir.parent().map(Path::to_path_buf)
        }
        (None, Some(_)) => None,
        (None, None) => target,
    };
    aimed.is_some_and(|path| aims_at(&path, worktree))
}

/// `path` as seen from `base`: absolute as written, relative joined, cleaned of
/// `.` and `..`. `None` for a place the text cannot name (`~`, `$VAR`, a glob,
/// the previous directory, no base for a relative path).
fn step_into(base: Option<&Path>, path: &str) -> Option<PathBuf> {
    if path.is_empty()
        || path == "-"
        || path.starts_with(['~', '$'])
        || path.contains(['$', '*', '?', '`'])
    {
        return None;
    }
    let joined = match base {
        _ if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(base) => base.join(path),
        None => return None,
    };
    let mut clean = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    Some(clean)
}

/// Whether a git run at `path` takes effect in `worktree`: the same place, or
/// a place whose nearest enclosing checkout (the closest ancestor holding a
/// `.git`) is it. The second is what keeps a subdirectory of the worktree in
/// and a lane nested under it out.
fn aims_at(path: &Path, worktree: &Path) -> bool {
    let here = resolve_path(&path.to_string_lossy());
    let there = resolve_path(&worktree.to_string_lossy());
    if here == there {
        return true;
    }
    Path::new(&here)
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .is_some_and(|root| root.to_string_lossy() == there)
}
