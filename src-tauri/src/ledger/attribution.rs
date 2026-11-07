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
//! The answer is never stronger than the evidence. A link is `inferred` from
//! time alone: a command that *could* have made the commit. Two distinct
//! sessions that could both have made it give no session and
//! `ambiguous: true`, never a guess, and a commit with no such command gives
//! no session and a [`basis`] saying which kind of nothing it is.

use super::{ids, with_access, Access, LedgerError};
use rusqlite::{params_from_iter, types::Value, Connection};
use std::collections::{BTreeSet, HashMap};

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

/// The newest rows (by id) one call reads: a `session.command` or reflog row
/// older than that is not looked at, and a commit whose reflog row is not found
/// in a capped read says [`basis::SCAN_CAPPED`], never "no session". The
/// ledger retains 10 000 events by default, so this reads all of a default
/// ledger and bounds a larger one to a single indexed range.
pub const LINK_SCAN_ROWS: i64 = 20_000;

/// Most commits one call links; the rest are reported as not examined.
pub const LINK_MAX_COMMITS: usize = 256;

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
    /// The ledger has no reflog row for the commit: it was not made in a
    /// replayed worktree (another machine, a clone) or has not been replayed.
    pub const NO_REFLOG_ENTRY: &str = "no_reflog_entry";
    /// The commit's reflog row has no event time from git.
    pub const REFLOG_UNTIMED: &str = "reflog_entry_untimed";
    /// The read stopped at [`super::LINK_SCAN_ROWS`] before finding it.
    pub const SCAN_CAPPED: &str = "scan_capped";
    /// This repository has no ledger yet.
    pub const LEDGER_ABSENT: &str = "ledger_not_initialised";
    /// The ledger could not be read.
    pub const LEDGER_UNREADABLE: &str = "ledger_unreadable";
    /// Past [`super::LINK_MAX_COMMITS`] in one call.
    pub const NOT_EXAMINED: &str = "not_examined_batch_cap";
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
    /// `worktree_path`, the anchor checkout's NULL spelled as the empty string.
    worktree: String,
    is_command: bool,
    session_id: Option<String>,
    object: Option<String>,
    outcome: String,
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
    match with_access(repo_path, Access::ReadOnly, |conn| {
        read_rows(conn, &examined)
    }) {
        Ok((rows, capped)) => {
            let (reflogs, commands): (Vec<Row>, Vec<Row>) =
                rows.into_iter().partition(|row| !row.is_command);
            for sha in examined {
                let mine: Vec<&Row> = reflogs
                    .iter()
                    .filter(|row| row.after_ref.as_deref() == Some(sha))
                    .collect();
                answers.insert(sha.to_string(), link_one(&mine, &commands, capped));
            }
        }
        Err(_) => {
            for sha in examined {
                answers.insert(sha.to_string(), CommitLink::none(basis::LEDGER_UNREADABLE));
            }
        }
    }
    answers
}

/// The reflog rows of `commits` and every git-looking `session.command` row,
/// among the newest [`LINK_SCAN_ROWS`] events, plus whether older ones exist.
///
/// The range is on the primary key, so the read touches at most that many rows
/// however large the ledger is. Event time comes from `detail_json` rather than
/// the typed `occurred_at` column so a ledger a generation-1 writer last opened
/// reads the same as a migrated one.
fn read_rows(conn: &Connection, commits: &[&str]) -> Result<(Vec<Row>, bool), LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let newest: Option<i64> = conn
        .query_row("SELECT MAX(id) FROM events", [], |row| row.get(0))
        .map_err(failed)?;
    let Some(newest) = newest else {
        return Ok((Vec::new(), false));
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
                     THEN json_extract(detail_json, '$.occurred_at_basis') END
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
                at_ms: at
                    .filter(|_| stamp_basis.as_deref() == Some(trusted))
                    .and_then(|text| ids::parse_event_time(&text)),
            })
        })
        .map_err(failed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed)?;
    Ok((rows, capped))
}

/// The matching rule, over rows already read. `reflogs` are the commit's own.
fn link_one(reflogs: &[&Row], commands: &[Row], capped: bool) -> CommitLink {
    if reflogs.is_empty() {
        return CommitLink::none(if capped {
            basis::SCAN_CAPPED
        } else {
            basis::NO_REFLOG_ENTRY
        });
    }
    // The entry that made the commit is the earliest to name it; a later one
    // (a fast-forward in another worktree) only moved a ref onto it.
    let Some(entry) = reflogs
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

    let mut candidates: Vec<(&Row, u64)> = Vec::new();
    let (mut untimed, mut before, mut after) = (false, false, false);
    for command in commands.iter().filter(|c| {
        c.worktree == entry.worktree
            && c.outcome != "blocked"
            && c.session_id.as_deref().is_some_and(|s| !s.is_empty())
            && c.object.as_deref().is_some_and(runs_git_mutation)
    }) {
        match command.at_ms {
            Some(t) if (earliest..=latest).contains(&t) => candidates.push((command, t)),
            Some(t) if t > latest => after |= t <= latest.saturating_add(LINK_WINDOW_MS),
            Some(t) => before |= t >= earliest.saturating_sub(LINK_WINDOW_MS),
            // It cannot have been recorded before it happened, so a row
            // appended before the window opened is ruled out by its append
            // time; one appended since cannot be.
            None => {
                untimed |= ids::parse_event_time(&command.ts_utc).is_none_or(|ts| ts >= earliest)
            }
        }
    }

    let sessions: BTreeSet<&str> = candidates
        .iter()
        .filter_map(|(c, _)| c.session_id.as_deref())
        .collect();
    match sessions.len() {
        0 => CommitLink::none(if untimed {
            basis::COMMAND_UNTIMED
        } else if after {
            basis::COMMAND_AFTER_REFLOG
        } else if before {
            basis::COMMAND_OUTSIDE_WINDOW
        } else {
            basis::NO_SESSION_COMMAND
        }),
        1 => {
            // Several commands of one session (a commit, then its amend):
            // the nearest one is the one named.
            let (nearest, t) = candidates
                .iter()
                .max_by_key(|(c, t)| (*t, c.id))
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

/// Global options that take their value as the next word.
const GIT_OPTIONS_WITH_VALUE: [&str; 6] = [
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];

/// Whether `command` runs `git commit`, `merge`, `rebase`, `cherry-pick` or
/// `pull` anywhere in it: `cd x && git -C y commit -m z` does. Words are split on
/// whitespace and shell separators only, so a quoted message that happens to
/// say "git commit" counts too: the match is an inference from the text, and a
/// false hit costs one extra candidate, which can only make a link ambiguous.
pub(crate) fn runs_git_mutation(command: &str) -> bool {
    let words: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`'))
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().enumerate().any(|(at, word)| {
        if *word != "git" && !word.ends_with("/git") {
            return false;
        }
        let mut rest = words[at + 1..].iter();
        while let Some(next) = rest.next() {
            if GIT_OPTIONS_WITH_VALUE.contains(next) {
                rest.next();
            } else if !next.starts_with('-') {
                return MUTATING_SUBCOMMANDS.contains(next);
            }
        }
        false
    })
}
