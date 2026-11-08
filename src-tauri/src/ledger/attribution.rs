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
//! The one outcome this must never give is a confident *wrong* link, and
//! reading a shell command line cannot be made complete, so the rule is to
//! **fail closed**: a command is a candidate for a link only when everything
//! about it is determined with certainty, and anything the reader does not
//! fully understand is not a candidate. At most it makes the answer ambiguous
//! or earns a named [`basis`]:
//!
//! * a command is credited only if it is known to have **succeeded**, and its
//!   target worktree is **proved** (a plain `git <verb>` in the worktree it
//!   ran in, or a `-C` / `cd` through plain, existing paths to it). Env
//!   prefixes, wrappers, nested shells, quoted or escaped or `$`-expanded
//!   paths, missing paths, `-c` settings that can move the tree, and groupings
//!   are all "target unknown";
//! * only a form that can create a commit counts (`--dry-run`, `--abort`, `-h`
//!   do not);
//! * only a reflog entry that *created* the commit is its origin: a
//!   fast-forward, reset or checkout moves a ref onto someone else's commit;
//! * a read cut at its row bound says so (`scan_capped`) unless it can show,
//!   from the unread rows, that none could have changed the answer;
//! * two sessions that could each have made it give no session and
//!   `ambiguous: true`.
//!
//! What survives is still an *inference from time*: a command that could have
//! made the commit, in the same worktree, shortly before it.
//!
//! **What remains is irreducible, and it is ambiguous, not a link.** Two actors
//! who commit in one worktree inside one command's *timed* execution window
//! cannot be told apart by timing: the transcript times when a command was
//! issued and when its result came back (`occurred_at` and `duration_ms`), git
//! times each entry to the second, and two entries of the command's verb inside
//! that span are reported as `ambiguous_sessions`, never as a link. A command
//! whose end was never timed is never credited at all: it may have made nothing
//! (`Already up to date`), or made its commit after a slow hook that a human's
//! own commit beat it through, and only its end tells either from the entry. If
//! it is the only command that fits it gives `command_outcome_unknown`, and next
//! to another session's credited command it makes the answer ambiguous. And
//! `no_ingested_session_command` means no *ingested* transcript shows a git
//! command: a session GitPulse does not read leaves none, so it is not proof
//! that no agent was involved.

use super::{ids, with_access, Access, LedgerError};
use rusqlite::types::ValueRef;
use rusqlite::{params_from_iter, types::Value, Connection};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

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

/// The newest rows (by id) one call reads for commands and reflog entries. The
/// ledger retains 10 000 events by default, so this reads all of a default
/// ledger and bounds a larger one to a single indexed range.
pub const LINK_SCAN_ROWS: i64 = 20_000;

/// How many rows past [`LINK_SCAN_ROWS`] the unread region is examined for
/// proof that nothing in it matters. A ledger larger than both is not provable
/// and reads as [`basis::SCAN_CAPPED`].
pub const HIDDEN_PROBE_ROWS: i64 = 200_000;

/// Most commits one call links; the rest are reported as not examined.
pub const LINK_MAX_COMMITS: usize = 256;

/// Slack on the proof that a row past the scan bound cannot matter.
///
/// An event cannot be recorded before it happened, so a row appended before
/// the window opened holds nothing in the window. That is read off the append
/// clock (`ts_utc`), which can disagree with the clock that dated the event
/// by an NTP step or a laptop that slept; ten minutes covers those. A row whose
/// append stamp is malformed, or older than its own event time by more than
/// this, is not evidence of anything and defeats the proof.
pub const HIDDEN_ROW_SKEW_MS: u64 = 600_000;

/// Why a commit has, or has no, session. The text is the wire value of
/// `session_basis`.
pub mod basis {
    /// A command in the window, from one session.
    pub const INFERRED: &str = "inferred_from_command";
    /// No transcript GitPulse *ingested* shows a git command for this commit.
    /// That is not "no agent was involved": a session under another Claude
    /// config directory, or a tool whose transcripts GitPulse does not read,
    /// leaves no command row, so a commit made there reads exactly like a hand
    /// commit.
    pub const NO_INGESTED_SESSION_COMMAND: &str = "no_ingested_session_command";
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
    /// The only command that fits is one whose result or end was never timed
    /// (or whose result was neither `ok` nor `error`), so whether it made this
    /// commit is not known. Such a command is never credited.
    pub const COMMAND_OUTCOME_UNKNOWN: &str = "command_outcome_unknown";
    /// The only command that fits did not make this commit: it made an earlier
    /// entry of its verb (a single-commit run is credited with the first entry
    /// of its verb after it), its run ended, it had finished, or it is not a
    /// command that writes this kind of entry (a `git pull` that found nothing
    /// to pull cannot have made a hand commit).
    pub const COMMAND_SPENT: &str = "command_did_not_make_this_commit";
    /// What lies between the command and the commit cannot be ordered in time
    /// (a reflog entry with no event time), so which entry the command made is
    /// not certain.
    pub const COMMAND_ORDER_UNKNOWN: &str = "command_order_unknown";
    /// The only command that fits is one whose target worktree could not be
    /// determined (an env prefix, a nested shell, a quoted or missing path):
    /// it may have made the commit, and it may have aimed elsewhere.
    pub const COMMAND_TARGET_UNKNOWN: &str = "command_target_unknown";
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
    pub const ALL: [&str; 17] = [
        INFERRED,
        NO_INGESTED_SESSION_COMMAND,
        AMBIGUOUS,
        COMMAND_OUTSIDE_WINDOW,
        COMMAND_AFTER_REFLOG,
        COMMAND_UNTIMED,
        COMMAND_OUTCOME_UNKNOWN,
        COMMAND_TARGET_UNKNOWN,
        COMMAND_SPENT,
        COMMAND_ORDER_UNKNOWN,
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

/// One ledger row, with the columns the match reads. Every text field is read
/// as whatever SQLite holds: a column of the wrong type is that field's own
/// "unknown" (`None`), never a failure of the read, so one malformed row cannot
/// blind every commit in the batch.
struct Row {
    id: i64,
    /// `worktree_path` as stored; the anchor checkout's NULL is the empty string.
    worktree: String,
    is_command: bool,
    /// The `action` column: for a reflog row, `reflog.<verb>` (see [`parse_entry`]).
    action: String,
    session_id: Option<String>,
    object: Option<String>,
    outcome: String,
    /// `detail_json.result` of a transcript row: `ok` or `error`, absent when
    /// the call's result was never read or was not a string.
    result: Option<String>,
    /// How long the call ran, in milliseconds, from the call's record to its
    /// result's (the `duration_ms` column). Present only when both were timed:
    /// the call's completion is then `at_ms + duration_ms`.
    duration_ms: Option<i64>,
    after_ref: Option<String>,
    ts_utc: String,
    /// The event's own time, only when its basis is the one this kind of row's
    /// time can be trusted from: the transcript for a command, git for a reflog
    /// entry. A row stamped at ingest, with no stamp, or with a stamp that is
    /// not a text time has `None`.
    at_ms: Option<u64>,
}

/// A text column as it is, or `None` for NULL or any other type.
fn text(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    })
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
        let (rows, floor) = read_rows(conn)?;
        let hidden = floor.map(|floor| read_hidden(conn, floor, &examined));
        let (reflogs, commands): (Vec<Row>, Vec<Row>) =
            rows.into_iter().partition(|row| !row.is_command);
        let anchor = canonical(repo_path);
        let mut seen = HashMap::new();
        let reflogs: Vec<Placed> = reflogs
            .into_iter()
            .map(|row| place(row, &anchor, &mut seen))
            .collect();
        let commands = classify(commands, &anchor, &mut seen);
        let mut linked = Vec::with_capacity(examined.len());
        for sha in &examined {
            linked.push((
                (*sha).to_string(),
                link_one(sha, &reflogs, &commands, hidden.as_ref()),
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

/// Every reflog row and every git-looking `session.command` row among the
/// newest [`LINK_SCAN_ROWS`] events, and the id at the bound when older rows
/// exist (`None` when the read covered the whole ledger). The reflog rows are
/// all of them, not only the asked commits': what lies between a command and a
/// commit decides whether the command made it.
///
/// The range is on the primary key, so the read touches at most that many rows
/// however large the ledger is. Event time comes from `detail_json` rather than
/// the typed `occurred_at` column so a ledger a generation-1 writer last opened
/// reads the same as a migrated one.
fn read_rows(conn: &Connection) -> Result<(Vec<Row>, Option<i64>), LedgerError> {
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
    let mut stmt = conn
        .prepare(
            "SELECT id, worktree_path, action, session_id, object, outcome, after_ref, ts_utc,
                    CASE WHEN json_valid(detail_json)
                         THEN json_extract(detail_json, '$.occurred_at') END,
                    CASE WHEN json_valid(detail_json)
                         THEN json_extract(detail_json, '$.occurred_at_basis') END,
                    CASE WHEN json_valid(detail_json)
                         THEN json_extract(detail_json, '$.result') END,
                    duration_ms
             FROM events
             WHERE id > ?1
               AND ((action = 'session.command' AND object LIKE '%git%')
                    OR action LIKE 'reflog.%')",
        )
        .map_err(failed)?;
    let rows = stmt
        .query_map([floor], |row| {
            let is_command = text(row, 2)?.as_deref() == Some("session.command");
            let at = text(row, 8)?;
            let stamp_basis = text(row, 9)?;
            let trusted = if is_command { "transcript" } else { "reflog" };
            Ok(Row {
                id: row.get(0)?,
                worktree: text(row, 1)?.unwrap_or_default(),
                is_command,
                action: text(row, 2)?.unwrap_or_default(),
                session_id: text(row, 3)?,
                object: text(row, 4)?,
                outcome: text(row, 5)?.unwrap_or_default(),
                after_ref: text(row, 6)?,
                ts_utc: text(row, 7)?.unwrap_or_default(),
                result: text(row, 10)?,
                duration_ms: match row.get_ref(11)? {
                    ValueRef::Integer(ms) => Some(ms),
                    _ => None,
                },
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

/// The canonical spelling of a UTC timestamp, as a SQL `GLOB`.
const CANONICAL_TS: &str =
    "[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z";

/// What the rows past the scan bound (`id <= floor`) show, enough to say
/// whether any of them could change an answer.
struct Hidden {
    /// The unread region cannot be vouched for: it is larger than
    /// [`HIDDEN_PROBE_ROWS`], a row in it has a malformed append stamp or an
    /// event time later than its own append stamp allows, or it could not be
    /// read. Every capped answer is then [`basis::SCAN_CAPPED`].
    unprovable: bool,
    /// The newest well-formed append stamp among them, in milliseconds.
    newest_append_ms: Option<u64>,
    /// Commits that a reflog row in the region names.
    named: BTreeSet<String>,
}

impl Hidden {
    /// Whether a row in the region may hold an event at or after `since_ms`.
    fn may_hold_event_since(&self, since_ms: u64) -> bool {
        self.unprovable
            || self
                .newest_append_ms
                .is_some_and(|at| at >= since_ms.saturating_sub(HIDDEN_ROW_SKEW_MS))
    }
}

/// The first [`HIDDEN_PROBE_ROWS`] rows past the scan bound, summarised.
///
/// An event cannot be recorded before it happens, so a hidden row appended
/// before a window opened holds nothing in it. That reasoning needs the append
/// clock to be honest, so a row that shows it is not (a stamp that is empty or
/// not a timestamp, or an event time more than the skew allowance after it) is
/// not skipped, it defeats the proof. Whatever cannot be read answers
/// "unprovable", never "nothing hidden".
fn read_hidden(conn: &Connection, floor: i64, commits: &[&str]) -> Hidden {
    let unprovable = || Hidden {
        unprovable: true,
        newest_append_ms: None,
        named: BTreeSet::new(),
    };
    let beyond = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id <= ?1)",
        [floor.saturating_sub(HIDDEN_PROBE_ROWS)],
        |row| row.get::<_, bool>(0),
    );
    if beyond.unwrap_or(true) {
        return unprovable();
    }
    let skew_s = HIDDEN_ROW_SKEW_MS / 1000;
    let summary = format!(
        "SELECT MAX(CASE WHEN ts_utc GLOB '{CANONICAL_TS}' THEN ts_utc END),
                MAX(CASE WHEN NOT (ts_utc GLOB '{CANONICAL_TS}') THEN 1
                         WHEN occ IS NOT NULL
                          AND (typeof(occ) != 'text'
                               OR NOT (occ GLOB '{CANONICAL_TS}')
                               OR occ > strftime('%Y-%m-%dT%H:%M:%fZ', ts_utc, '-{skew_s} seconds'))
                         THEN 1 ELSE 0 END)
         FROM (SELECT ts_utc,
                      CASE WHEN detail_json IS NOT NULL
                                AND instr(detail_json, 'occurred_at') > 0
                                AND json_valid(detail_json)
                           THEN json_extract(detail_json, '$.occurred_at') END AS occ
               FROM events WHERE id <= ?1 ORDER BY id DESC LIMIT ?2)"
    );
    let Ok((newest, flagged)) = conn.query_row(&summary, [floor, HIDDEN_PROBE_ROWS], |row| {
        Ok((text(row, 0)?, row.get::<_, Option<i64>>(1)?))
    }) else {
        return unprovable();
    };
    if flagged.unwrap_or(0) != 0 {
        return unprovable();
    }
    let marks = vec!["?"; commits.len()].join(",");
    let named_sql = format!(
        "SELECT DISTINCT after_ref
         FROM (SELECT action, after_ref FROM events WHERE id <= ?1 ORDER BY id DESC LIMIT ?2)
         WHERE action LIKE 'reflog.%' AND after_ref IN ({marks})"
    );
    let mut values = vec![Value::Integer(floor), Value::Integer(HIDDEN_PROBE_ROWS)];
    values.extend(commits.iter().map(|sha| Value::Text((*sha).to_string())));
    let named = conn.prepare(&named_sql).and_then(|mut stmt| {
        stmt.query_map(params_from_iter(values), |row| text(row, 0))?
            .collect::<Result<Vec<_>, _>>()
    });
    let Ok(named) = named else {
        return unprovable();
    };
    Hidden {
        unprovable: false,
        newest_append_ms: newest.and_then(|stamp| ids::parse_event_time(&stamp)),
        named: named.into_iter().flatten().collect(),
    }
}

/// A path as the filesystem names it when it exists, and otherwise as written
/// (trailing slash trimmed): the ledger's own [`super::canonical_repo`], the
/// one owner of worktree identity.
///
/// A path that no longer exists is never resolved through its ancestors. Doing
/// so makes a removed lane's directory, replaced since by a link to another
/// worktree, look like that worktree, and a wrong match is worse than a missed
/// one. A vanished path matches only its own spelling.
fn canonical(path: &str) -> String {
    super::canonical_repo(path)
}

/// What a command row is to the commit it is being matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    /// Not a run that could make a commit in this worktree, with certainty.
    Elsewhere,
    /// May have made a commit here: a form this reader does not fully
    /// understand. It never links, and it makes another session's link
    /// ambiguous.
    Unknown,
    /// A commit-making git form, proved to run in this worktree.
    Here,
}

/// A ledger row with the worktree it belongs to, resolved.
struct Placed {
    row: Row,
    worktree: String,
}

/// `row`'s worktree: the anchor checkout stores none and is the repository
/// itself; any other is resolved once per call.
fn place(row: Row, anchor: &str, seen: &mut HashMap<String, String>) -> Placed {
    let worktree = if row.worktree.is_empty() {
        anchor.to_string()
    } else {
        seen.entry(row.worktree.clone())
            .or_insert_with(|| canonical(&row.worktree))
            .clone()
    };
    Placed { row, worktree }
}

/// A command row that is, or may be, a commit-making run in its own worktree.
struct Command {
    placed: Placed,
    target: Target,
    shape: Shape,
    /// The git run is not the whole command ([`is_whole_command`]): the
    /// transcript's `ok` is the exit status of something else, so it says
    /// nothing about whether the commit was made.
    masked: bool,
}

/// The command rows that are, or may be, a commit-making run in their own
/// worktree, with that worktree resolved. Rows that certainly are not are
/// dropped.
fn classify(rows: Vec<Row>, anchor: &str, seen: &mut HashMap<String, String>) -> Vec<Command> {
    rows.into_iter()
        .filter_map(|row| {
            let placed = place(row, anchor, seen);
            let object = placed.row.object.as_deref()?;
            let (target, shape) = read_command(object, Path::new(&placed.worktree));
            let masked = !is_whole_command(object);
            (target != Target::Elsewhere).then_some(Command {
                masked,
                placed,
                target,
                shape,
            })
        })
        .collect()
}

/// A reflog entry's message, split the way git writes it: `verb (step): tail`.
struct Entry<'a> {
    verb: &'a str,
    /// The text of a trailing `(...)`, as in `rebase (pick)`.
    step: Option<&'a str>,
    /// Whether the entry closes an operation: `(finish)` or `(abort)`.
    closes: bool,
    tail: &'a str,
}

/// A reflog row, parsed the way production writes it.
///
/// Ingest splits git's message (`%gs`) at its first colon: the head becomes the
/// row's `action` (`reflog.` + the head folded to lowercase ASCII tokens, every
/// other character `_`, see `ledger::action_segment`) and only what follows the
/// colon is kept, in `object` (`"<commit> <tail>"`). So the verb and the step
/// live in `action` alone: `commit (amend): m` is `reflog.commit__amend_` over
/// `<commit> m`, `pull --rebase (pick): m` is `reflog.pull___rebase__pick_`,
/// `cherry-pick: m` is `reflog.cherry_pick`, `merge feat: Merge made by ...` is
/// `reflog.merge_feat`. A message with no colon is `reflog.action`.
///
/// The verb is the head's first word (`cherry_pick` is the one hyphenated verb a
/// run can write); the step is the last parenthesised word, which the folding
/// leaves as a trailing `_` after a `__`.
fn parse_entry<'a>(action: &'a str, object: &'a str) -> Option<Entry<'a>> {
    let head = action.strip_prefix("reflog.")?;
    let (head, step) = match head
        .strip_suffix('_')
        .and_then(|inner| inner.rsplit_once("__"))
    {
        Some((head, step)) if !step.is_empty() => (head, Some(step)),
        _ => (head, None),
    };
    let verb = if head == "cherry_pick" || head.starts_with("cherry_pick_") {
        "cherry-pick"
    } else {
        head.split('_').next().unwrap_or("")
    };
    Some(Entry {
        verb,
        step,
        closes: matches!(step, Some("finish" | "abort")),
        tail: object.split_once(' ').map_or("", |(_, rest)| rest),
    })
}

/// [`parse_entry`] of a ledger row.
fn entry_of(row: &Row) -> Option<Entry<'_>> {
    parse_entry(&row.action, row.object.as_deref()?)
}

/// What a reflog entry did to the ref.
///
/// Only an entry that *created* the commit is its origin. The message is git's
/// own (`%gs`), classified on the whole of it: the verb, the parenthesised step,
/// and what follows the colon. The forms that create a commit are listed; every
/// other form (`merge x: Fast-forward`, `pull: Fast-forward`, `cherry-pick:
/// fast-forward`, `reset`, `checkout`, `clone`, `rebase (start)` / `(finish)`,
/// `branch`, and any form this list has not heard of) is a move, so an unknown
/// form can only cost a link, never invent one.
///
/// Forms verified against git's own reflog: `commit: m`, `commit (initial): m`,
/// `commit (amend): m`, `commit (merge): m`, `cherry-pick: m` (and
/// `cherry-pick: fast-forward`, which is a move), `revert: m`, `am: m`,
/// `rebase (pick|continue|fixup|squash|reword|edit): m`,
/// `pull --rebase (pick): m`, `merge b: Merge made by the 'ort' strategy.` and
/// `pull: Merge made by ...`.
fn creates_commit(entry: &Entry<'_>) -> bool {
    match entry.verb {
        "commit" | "revert" | "am" => true,
        // `git cherry-pick` of a commit whose parent is already HEAD moves the
        // ref instead of making a commit, and says so.
        "cherry-pick" => {
            !entry.tail.eq_ignore_ascii_case("fast-forward")
                && entry.step.is_none_or(|s| s == "continue")
        }
        "rebase" | "pull" => {
            matches!(
                entry.step,
                Some("pick" | "continue" | "fixup" | "squash" | "reword" | "edit")
            ) || (entry.verb == "pull" && entry.tail.starts_with("Merge made by"))
        }
        "merge" => entry.tail.starts_with("Merge made by"),
        _ => false,
    }
}

/// Whether a reflog row is an origin entry (it made its commit).
fn is_origin(row: &Row) -> bool {
    entry_of(row).is_some_and(|entry| creates_commit(&entry))
}

/// What came between a command and a commit's origin entry.
#[derive(Debug, PartialEq, Eq)]
enum Between {
    /// Nothing that takes the command from this commit: it may have made it.
    Clear,
    /// The command made an earlier entry (or a run that ended), or is not the
    /// kind of run that makes this one: it did not make this commit.
    Spent,
    /// An entry that cannot be placed in time could lie between them: the
    /// order is not known, so there is no certain answer.
    Unordered,
    /// The command was running when another entry of the same verb was
    /// written as well as this one: two actors committed inside one command's
    /// execution, and timing cannot say which entry the command made.
    Contested,
    /// The command's end was never timed: it may have made nothing, or been
    /// beaten to the commit by another actor, and only its end could tell. It is
    /// never credited.
    NoCompletion,
}

/// Whether `command` can be the maker of the origin entry `entry` (at `at`),
/// given every reflog row of the worktree.
///
/// A command is credited with the *next* entry, not with every entry in its
/// window. A single-commit run (`commit`, `merge`, `revert`, one `cherry-pick`)
/// makes exactly one: any other commit's origin entry between the command and
/// this one means the command made that, and not this. A run (a rebase's picks,
/// a cherry-pick range) is credited with the entries of its own verbs: this
/// entry must be one, and the run must not have ended before it, at its
/// `(finish)` / `(abort)` entry or at an origin entry of another verb.
///
/// "Between" is by reflog second, and ties count as between: two entries of one
/// second have no order, and a command's credit may not rest on one.
fn between(
    command: &Command,
    command_at: u64,
    entry: &Placed,
    at: u64,
    sha: &str,
    reflogs: &[Placed],
) -> Between {
    let started = command_at.saturating_sub(REFLOG_GRANULARITY_MS - 1);
    let shape = command.shape;
    // A command can only have made an entry of a verb it writes: a `git pull`
    // that found nothing to pull made no entry, and a hand commit after it is
    // not its commit.
    let this = entry_of(&entry.row);
    if !this.as_ref().is_some_and(|this| shape.writes(this.verb)) {
        return Between::Spent;
    }
    // When the command finished, as the transcript timed its result. A command
    // whose end was not timed is never credited: it may have made nothing
    // (`Already up to date`), or made its commit after a hook that a human's own
    // commit beat it through, and only its end can tell those from the entry.
    let Some(done) = command
        .placed
        .row
        .duration_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .map(|ms| command_at.saturating_add(ms))
    else {
        return Between::NoCompletion;
    };
    // The entry came after the command was done.
    if at > done {
        return Between::Spent;
    }
    // Entries that count against the command: those of a verb it writes inside
    // its execution; for a run, those up to this one, any origin entry of
    // another verb ending it.
    let upper = if shape.run { at } else { done };
    let (mut unordered, mut crowded) = (false, false);
    for other in reflogs {
        let row = &other.row;
        if other.worktree != entry.worktree
            || row.after_ref.as_deref() == Some(sha)
            || row.id == entry.row.id
        {
            continue;
        }
        let parsed = entry_of(row);
        let origin = parsed.as_ref().is_some_and(creates_commit);
        let ours = parsed
            .as_ref()
            .is_some_and(|parsed| shape.writes(parsed.verb));
        let Some(t) = row.at_ms else {
            // Appended before the window opened, it cannot be inside it.
            let possible = ids::parse_event_time(&row.ts_utc)
                .is_none_or(|ts| ts >= at.saturating_sub(LINK_WINDOW_MS));
            unordered |= origin && possible && (ours || shape.run);
            continue;
        };
        if t < started || t > upper {
            continue;
        }
        let Some(parsed) = &parsed else { continue };
        if shape.run {
            let ended_before = parsed.closes && ours && (t < at || row.id < entry.row.id);
            if ended_before || (origin && !ours) {
                return Between::Spent;
            }
        } else if origin && ours {
            // Another entry of its verb while it ran: every such entry inside
            // its execution is a candidate and none can be told from the others.
            crowded = true;
        }
    }
    if crowded {
        Between::Contested
    } else if unordered {
        Between::Unordered
    } else {
        Between::Clear
    }
}

/// The matching rule, over rows already read. `reflogs` are every reflog row
/// read; `hidden` is `Some` when the read stopped at the row bound.
fn link_one(
    sha: &str,
    reflogs: &[Placed],
    commands: &[Command],
    hidden: Option<&Hidden>,
) -> CommitLink {
    let mine: Vec<&Placed> = reflogs
        .iter()
        .filter(|placed| placed.row.after_ref.as_deref() == Some(sha))
        .collect();
    if mine.is_empty() {
        return CommitLink::none(if hidden.is_some() {
            basis::SCAN_CAPPED
        } else {
            basis::NO_REFLOG_ENTRY
        });
    }
    let origins: Vec<&&Placed> = mine
        .iter()
        .filter(|placed| is_origin(&placed.row))
        .collect();
    if origins.is_empty() {
        // The entry that made it may be one the read did not reach.
        return CommitLink::none(if hidden.is_some() {
            basis::SCAN_CAPPED
        } else {
            basis::NO_ORIGIN_ENTRY
        });
    }
    // The earliest entry that made it; a later one (the same commit made
    // again, an amend that changed nothing) is the same link.
    let Some(entry) = origins
        .iter()
        .filter(|placed| placed.row.at_ms.is_some())
        .min_by_key(|placed| (placed.row.at_ms, placed.row.id))
    else {
        return CommitLink::none(basis::REFLOG_UNTIMED);
    };
    let Some(at) = entry.row.at_ms else {
        return CommitLink::none(basis::REFLOG_UNTIMED);
    };
    let earliest = at.saturating_sub(LINK_WINDOW_MS);
    let latest = at.saturating_add(REFLOG_GRANULARITY_MS - 1);
    // An earlier origin entry for this very commit, or a command or entry in
    // the window, may sit in rows the read did not reach.
    if hidden.is_some_and(|h| h.named.contains(sha) || h.may_hold_event_since(earliest)) {
        return CommitLink::none(basis::SCAN_CAPPED);
    }

    // Commands of the commit's own worktree, by what is known of them.
    let mut ok: Vec<(&Row, u64)> = Vec::new();
    let mut unobserved: Vec<&Row> = Vec::new();
    let mut target_unknown: Vec<&Row> = Vec::new();
    let (mut untimed, mut before, mut after) = (false, false, false);
    let mut unordered: Vec<&Row> = Vec::new();
    let mut contested: Vec<&Row> = Vec::new();
    let mut spent = false;
    for command in commands.iter().filter(|c| {
        c.placed.worktree == entry.worktree
            && c.placed.row.outcome == "ok"
            && c.placed.row.result.as_deref() != Some("error")
            && c.placed
                .row
                .session_id
                .as_deref()
                .is_some_and(|s| !s.is_empty())
    }) {
        let row = &command.placed.row;
        match row.at_ms {
            Some(t) if (earliest..=latest).contains(&t) => {
                if command.target == Target::Unknown {
                    target_unknown.push(row);
                    continue;
                }
                // The call's recorded `ok` is the exit status of the whole
                // line, which is not git's: a hook-rejected commit behind a
                // pipe or `;` reads `ok`, and the commit in the window may be a
                // person's. Never evidence for a link; it stays a candidate.
                if command.masked {
                    unobserved.push(row);
                    continue;
                }
                match between(command, t, entry, at, sha, reflogs) {
                    Between::Spent => spent = true,
                    Between::Unordered => unordered.push(row),
                    Between::Contested => contested.push(row),
                    Between::NoCompletion => unobserved.push(row),
                    Between::Clear if row.result.as_deref() == Some("ok") => ok.push((row, t)),
                    Between::Clear => unobserved.push(row),
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

    // A command that may have made it but is not known to have (its result was
    // never read, its target is not known, or what lies between it and the
    // commit cannot be ordered) counts toward ambiguity and never toward a link.
    let sessions: BTreeSet<&str> = ok
        .iter()
        .map(|(row, _)| row)
        .chain(unobserved.iter())
        .chain(target_unknown.iter())
        .chain(unordered.iter())
        .chain(contested.iter())
        .filter_map(|row| row.session_id.as_deref())
        .collect();
    // Two actors committed while one command ran: whichever of them it was, the
    // other is a second candidate, and timing cannot choose.
    if !contested.is_empty() && ok.is_empty() {
        return CommitLink {
            ambiguous: true,
            ..CommitLink::none(basis::AMBIGUOUS)
        };
    }
    match (sessions.len(), ok.is_empty()) {
        (0, _) => CommitLink::none(if untimed {
            basis::COMMAND_UNTIMED
        } else if spent {
            basis::COMMAND_SPENT
        } else if after {
            basis::COMMAND_AFTER_REFLOG
        } else if before {
            basis::COMMAND_OUTSIDE_WINDOW
        } else {
            basis::NO_INGESTED_SESSION_COMMAND
        }),
        (1, true) => CommitLink::none(if !unordered.is_empty() {
            basis::COMMAND_ORDER_UNKNOWN
        } else if !target_unknown.is_empty() {
            basis::COMMAND_TARGET_UNKNOWN
        } else {
            basis::COMMAND_OUTCOME_UNKNOWN
        }),
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
const MUTATING_SUBCOMMANDS: [&str; 7] = [
    "commit",
    "merge",
    "rebase",
    "cherry-pick",
    "pull",
    "revert",
    "am",
];

/// Git subcommands known to make no commit on a branch: they read, stage,
/// move a ref or the tree onto an existing commit, or touch only remotes. The
/// list is closed. A verb that is on neither it nor [`MUTATING_SUBCOMMANDS`]
/// (an alias such as `ci`, a verb this list has not heard of) may be a
/// commit-making run of a shape not understood, so it never links and makes
/// another session's link ambiguous.
const READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "rev-list",
    "ls-files",
    "ls-tree",
    "ls-remote",
    "cat-file",
    "blame",
    "grep",
    "describe",
    "diff-tree",
    "diff-index",
    "diff-files",
    "show-ref",
    "show-branch",
    "shortlog",
    "whatchanged",
    "reflog",
    "remote",
    "config",
    "fetch",
    "add",
    "rm",
    "mv",
    "restore",
    "clean",
    "checkout",
    "switch",
    "reset",
    "branch",
    "tag",
    "push",
    "stash",
    "worktree",
    "merge-base",
    "symbolic-ref",
    "update-ref",
    "for-each-ref",
    "name-rev",
    "check-ignore",
    "check-attr",
    "count-objects",
    "fsck",
    "gc",
    "prune",
    "repack",
    "maintenance",
    "help",
    "version",
    "init",
    "clone",
    "apply",
    "format-patch",
    "archive",
    "bundle",
    "sparse-checkout",
    "var",
    "difftool",
    "range-diff",
    "cherry",
    "request-pull",
    "verify-commit",
    "verify-tag",
    "hash-object",
    "write-tree",
    "read-tree",
    "mktree",
    "pack-refs",
    "commit-tree",
    "merge-file",
    "submodule",
    "notes",
];

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

/// Global options with no bearing on where git runs.
const HARMLESS_GLOBAL_OPTIONS: [&str; 8] = [
    "--no-pager",
    "-p",
    "--paginate",
    "--no-optional-locks",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
];

/// `git -c key=value` settings that cannot move the work tree or the git
/// directory. The list is closed: any other key (`core.worktree`,
/// `include.path`, `includeIf.*`, an alias, a `safe.*` or `gc.*` setting...) is
/// not understood.
const HARMLESS_CONFIG_PREFIXES: [&str; 9] = [
    "user.", "commit.", "gpg.", "advice.", "color.", "init.", "pull.", "merge.", "rebase.",
];

/// One shell word, and whether it is a plain literal: no quoting, no escape,
/// no expansion (`$`, backtick, `~`, glob characters, braces).
struct Word {
    text: String,
    plain: bool,
}

/// A command line split into words and statement separators. `None` for an
/// unterminated quote. The flag says the line groups or substitutes
/// (`(...)`, `{ ...; }`, backticks, `$(`, here-documents), which this reader
/// does not follow.
fn tokenize(command: &str) -> Option<(Vec<Vec<Word>>, bool)> {
    let mut segments: Vec<Vec<Word>> = vec![Vec::new()];
    let (mut text, mut plain, mut open, mut grouped) = (String::new(), true, false, false);
    let mut chars = command.chars().peekable();
    macro_rules! end_word {
        () => {
            if open {
                let word = Word {
                    text: std::mem::take(&mut text),
                    plain: std::mem::replace(&mut plain, true),
                };
                open = false;
                if let Some(segment) = segments.last_mut() {
                    segment.push(word);
                }
            }
        };
    }
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() && c != '\n' => end_word!(),
            ';' | '&' | '|' | '\n' => {
                end_word!();
                segments.push(Vec::new());
            }
            '(' | ')' | '{' | '}' | '`' => {
                grouped = true;
                end_word!();
            }
            '\'' => {
                open = true;
                plain = false;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        other => text.push(other),
                    }
                }
            }
            '"' => {
                open = true;
                plain = false;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => text.push(chars.next()?),
                        '$' => {
                            grouped |= chars.peek() == Some(&'(');
                            text.push('$');
                        }
                        '`' => grouped = true,
                        other => text.push(other),
                    }
                }
            }
            '\\' => {
                open = true;
                plain = false;
                text.push(chars.next()?);
            }
            '<' if chars.peek() == Some(&'<') => {
                grouped = true;
                text.push(c);
                open = true;
            }
            '#' if !open => break,
            other => {
                open = true;
                if matches!(other, '$' | '~' | '*' | '?' | '[' | ']' | '!') {
                    plain = false;
                    grouped |= other == '$' && chars.peek() == Some(&'(');
                }
                text.push(other);
            }
        }
    }
    if open {
        if let Some(segment) = segments.last_mut() {
            segment.push(Word { text, plain });
        }
    }
    Some((segments, grouped))
}

/// Whether `command` is one simple command, nothing else, so that the Bash
/// call's exit status is the git run's own.
///
/// Fails closed: any unquoted pipe, `;`, `&` (so `&&`, `||` chains and
/// background runs), newline between commands, redirection, subshell, brace
/// group, backtick, or `$(` / `$((` anywhere outside single quotes makes it
/// false. A wrapper (`eval`, `sh -c`, `env`, `time`, `xargs`, `VAR=x git`,
/// `! git`) is false because the first word must be `git` itself.
fn is_whole_command(command: &str) -> bool {
    let command = command.trim();
    let Some(first) = command.split(is_boundary).next() else {
        return false;
    };
    if first != "git" && !(first.starts_with('/') && first.ends_with("/git")) {
        return false;
    }
    let mut chars = command.chars().peekable();
    let mut word_start = true;
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                if chars.next().is_none_or(|q| q == '\'') {
                    break;
                }
            },
            '"' => loop {
                match chars.next() {
                    None | Some('"') => break,
                    Some('\\') => {
                        chars.next();
                    }
                    Some('`') => return false,
                    Some('$') if chars.peek() == Some(&'(') => return false,
                    Some(_) => {}
                }
            },
            '\\' => {
                chars.next();
            }
            '$' if chars.peek() == Some(&'(') => return false,
            ';' | '&' | '|' | '\n' | '\r' | '<' | '>' | '(' | ')' | '{' | '}' | '`' => {
                return false;
            }
            '#' if word_start => break,
            _ => {}
        }
        word_start = c.is_whitespace();
    }
    true
}

/// Whether any of `words` is `git` and a commit-making subcommand follows it
/// anywhere after: how a command this reader does not understand is told from
/// one that has nothing to do with git. Words are split on whitespace, shell
/// separators and quotes, so the text of a quoted `sh -c '...'` is looked into
/// and `git status;git commit` is not missed.
fn mentions_git_mutation<'a>(words: impl IntoIterator<Item = &'a str>) -> bool {
    let (mut seen_git, mut verb_seen) = (false, false);
    for word in words
        .into_iter()
        .flat_map(|text| text.split(is_boundary))
        .filter(|word| !word.is_empty())
    {
        if word == "git" || word.ends_with("/git") {
            (seen_git, verb_seen) = (true, false);
        } else if seen_git {
            if MUTATING_SUBCOMMANDS.contains(&word) {
                return true;
            }
            // The first word that is not an option is the verb, or the value
            // of an option: either way, one not known to be harmless counts.
            if !verb_seen && !word.starts_with('-') {
                verb_seen = true;
                if !READ_ONLY_SUBCOMMANDS.contains(&word) {
                    return true;
                }
            }
        }
    }
    false
}

/// Where a command line's words end: whitespace, shell separators, quotes.
fn is_boundary(c: char) -> bool {
    c.is_whitespace() || ";&|(){}`\"'".contains(c)
}

/// Whether any word of `command` is `git` at all.
fn mentions_git(command: &str) -> bool {
    command
        .split(is_boundary)
        .any(|word| word == "git" || word.ends_with("/git"))
}

/// Where a command line aims its commit-making git runs, relative to
/// `worktree`, the checkout it ran in.
///
/// Fails closed. [`Target::Here`] needs proof: a plain `git <verb>`, or `-C`,
/// `cd` or `--work-tree` / `--git-dir` through plain paths that exist and lead
/// back to `worktree`. Everything else that mentions a commit-making git is
/// [`Target::Unknown`], not a guess in either direction: an env prefix
/// (`GIT_DIR=x git ...`, `env ...`), a wrapper or nested shell (`sh -c`,
/// `eval`, `xargs`, `time`), a quoted, escaped, `~`/`$`-expanded or globbed
/// path, a path that does not exist, `..` (resolved by the filesystem, never
/// lexically), a `-c` that can move the tree, `--config-env`, groupings and
/// substitutions. Only a run that is provably elsewhere, or not a commit-making
/// form, is [`Target::Elsewhere`].
#[cfg(test)]
pub(crate) fn command_target(command: &str, worktree: &Path) -> Target {
    read_command(command, worktree).0
}

/// The reflog verbs a commit-making run can write, as a bit set.
const VERBS: [&str; 7] = [
    "commit",
    "merge",
    "revert",
    "cherry-pick",
    "pull",
    "am",
    "rebase",
];

fn verb_bits(verbs: &[&str]) -> u8 {
    verbs
        .iter()
        .filter_map(|verb| VERBS.iter().position(|known| known == verb))
        .fold(0, |bits, at| bits | (1 << at))
}

/// What a commit-making command can have written to the reflog, which decides
/// which entries it can be credited with.
///
/// A run is credited only with an entry **of its own verb** (`git pull` with a
/// `pull...` entry, `git commit` with a `commit...` entry), so a command that
/// made nothing cannot be credited with a commit of another kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    /// The entry verbs this run writes ([`VERBS`] bits).
    verbs: u8,
    /// A run of commits written by one operation (a rebase's picks, a
    /// cherry-pick range): credited with the origin entries of its verbs and no
    /// further, ending at its `(finish)` / `(abort)` entry or at the next origin
    /// entry of another verb. Otherwise one commit, and one entry: the first of
    /// its verb after it.
    run: bool,
}

impl Shape {
    const NONE: Shape = Shape {
        verbs: 0,
        run: false,
    };

    /// Whether the command is a run of several commits.
    #[cfg(test)]
    pub(crate) fn is_run(self) -> bool {
        self.run
    }

    /// Whether the command can have written an entry of `verb`.
    fn writes(self, verb: &str) -> bool {
        self.verbs & verb_bits(&[verb]) != 0
    }

    /// What two commit-making runs in one command line, taken together, are:
    /// a run only when both are the same run, a single commit otherwise (which
    /// credits fewer entries, never more), able to write either verb.
    fn and(self, other: Shape) -> Shape {
        Shape {
            verbs: self.verbs | other.verbs,
            run: self.run && other.run && self.verbs == other.verbs,
        }
    }
}

/// The entries a rebase writes: `rebase (pick)`, or `pull --rebase (pick)`.
const REBASE_VERBS: &[&str] = &["rebase", "pull"];

/// [`command_target`], and the shape of the commit-making runs proved to be
/// here ([`Shape::NONE`] when there are none).
pub(crate) fn read_command(command: &str, worktree: &Path) -> (Target, Shape) {
    let mut shape: Option<Shape> = None;
    let target = read_command_into(command, worktree, &mut shape);
    (target, shape.unwrap_or(Shape::NONE))
}

fn read_command_into(command: &str, worktree: &Path, shape: &mut Option<Shape>) -> Target {
    if !mentions_git(command) {
        return Target::Elsewhere;
    }
    let Some((segments, grouped)) = tokenize(command) else {
        return Target::Unknown;
    };
    if grouped {
        return Target::Unknown;
    }
    let mut dir: Option<PathBuf> = Some(worktree.to_path_buf());
    let mut best = Target::Elsewhere;
    for words in segments.iter().filter(|words| !words.is_empty()) {
        let leading = words[0].text.as_str();
        let target = match (leading, words[0].plain) {
            ("cd" | "pushd", true) => {
                dir = words
                    .iter()
                    .skip(1)
                    .find(|word| !word.text.starts_with('-') || word.text == "-")
                    .and_then(|word| enter(dir.as_deref(), word));
                continue;
            }
            ("popd", true) => {
                dir = None;
                continue;
            }
            (leading, true) if leading == "git" || leading.ends_with("/git") => {
                let mut run = None;
                let target = git_target(&words[1..], dir.as_deref(), worktree, &mut run);
                if let (Target::Here, Some(run)) = (target, run) {
                    *shape = Some(shape.map_or(run, |seen| seen.and(run)));
                }
                target
            }
            _ if mentions_git_mutation(words.iter().map(|word| word.text.as_str())) => {
                Target::Unknown
            }
            _ => Target::Elsewhere,
        };
        best = best.max(target);
    }
    best
}

/// The directory `word` names from `base`, through the filesystem: a plain
/// path that exists and is a directory, with symlinks and `..` resolved by the
/// kernel. `None` for anything else.
fn enter(base: Option<&Path>, word: &Word) -> Option<PathBuf> {
    if !word.plain || word.text.is_empty() || word.text == "-" {
        return None;
    }
    let path = Path::new(&word.text);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base?.join(path)
    };
    std::fs::canonicalize(joined)
        .ok()
        .filter(|dir| dir.is_dir())
}

/// Whether a git run at the existing directory `dir` takes effect in
/// `worktree`: the same checkout, or one whose nearest enclosing checkout (the
/// closest ancestor holding a `.git`) is it. The second keeps a subdirectory in
/// and a lane nested under the worktree out.
fn lands_in(dir: &Path, worktree: &Path) -> bool {
    let there = PathBuf::from(canonical(&worktree.to_string_lossy()));
    dir == there
        || dir
            .ancestors()
            .find(|candidate| candidate.join(".git").exists())
            .is_some_and(|root| root == there)
}

/// One git invocation's words after `git`, run from `dir`.
fn git_target(
    words: &[Word],
    dir: Option<&Path>,
    worktree: &Path,
    run: &mut Option<Shape>,
) -> Target {
    let mut here = dir.map(Path::to_path_buf);
    let mut work_tree: Option<Option<PathBuf>> = None;
    let mut git_dir: Option<Option<PathBuf>> = None;
    let mut rest = words.iter();
    let subcommand = loop {
        let Some(word) = rest.next() else {
            return Target::Elsewhere;
        };
        let option = word.text.as_str();
        if !word.plain {
            return Target::Unknown;
        }
        if option == "-C" {
            here = rest.next().and_then(|path| enter(here.as_deref(), path));
        } else if option == "-c" {
            let Some(setting) = rest.next() else {
                return Target::Unknown;
            };
            if !setting.plain
                || !HARMLESS_CONFIG_PREFIXES
                    .iter()
                    .any(|prefix| setting.text.to_ascii_lowercase().starts_with(prefix))
            {
                return Target::Unknown;
            }
        } else if let Some(path) = option.strip_prefix("--work-tree=") {
            work_tree = Some(enter(
                here.as_deref(),
                &Word {
                    text: path.to_string(),
                    plain: true,
                },
            ));
        } else if option == "--work-tree" {
            work_tree = Some(rest.next().and_then(|path| enter(here.as_deref(), path)));
        } else if let Some(path) = option.strip_prefix("--git-dir=") {
            git_dir = Some(enter(
                here.as_deref(),
                &Word {
                    text: path.to_string(),
                    plain: true,
                },
            ));
        } else if option == "--git-dir" {
            git_dir = Some(rest.next().and_then(|path| enter(here.as_deref(), path)));
        } else if HARMLESS_GLOBAL_OPTIONS.contains(&option) {
        } else if option.starts_with('-') {
            return Target::Unknown;
        } else {
            break option;
        }
    };
    if !MUTATING_SUBCOMMANDS.contains(&subcommand) {
        // Known not to make a commit, or not known at all: an alias, a verb
        // this reader has not heard of.
        return if READ_ONLY_SUBCOMMANDS.contains(&subcommand) {
            Target::Elsewhere
        } else {
            Target::Unknown
        };
    }
    let all_args: Vec<&str> = rest.map(|word| word.text.as_str()).collect();
    // The words that are the command's own, not a message's text.
    let mut args = Vec::new();
    let mut remaining = all_args.iter().copied();
    while let Some(arg) = remaining.next() {
        if NO_OP_ARGUMENTS.contains(&arg) {
            return Target::Elsewhere;
        }
        if matches!(arg, "-m" | "--message" | "-F" | "--file") {
            remaining.next();
        } else {
            args.push(arg);
        }
    }
    let there = PathBuf::from(canonical(&worktree.to_string_lossy()));
    let target = aimed_at(work_tree, git_dir, here, worktree, &there);
    if target == Target::Here {
        *run = Some(shape_of(subcommand, &args));
    }
    target
}

/// How many commits `git <subcommand> <args>` makes, as far as the words say.
/// A rebase is a run; so is a `pull` that says it rebases and a cherry-pick of
/// a range or of several commits. Everything else is one commit, including the
/// verbs whose count the words do not give (`am`, `revert` of several): a
/// single commit credits fewer entries, never more.
fn shape_of(subcommand: &str, args: &[&str]) -> Shape {
    let flag = |name: &str| args.contains(&name);
    let one = |verbs: &[&str]| Shape {
        verbs: verb_bits(verbs),
        run: false,
    };
    let run = |verbs: &[&str]| Shape {
        run: true,
        ..one(verbs)
    };
    match subcommand {
        "rebase" => run(REBASE_VERBS),
        "pull"
            if flag("--rebase")
                || flag("-r")
                || args.iter().any(|a| a.starts_with("--rebase=")) =>
        {
            run(REBASE_VERBS)
        }
        "cherry-pick" => {
            let commits = args.iter().filter(|arg| !arg.starts_with('-')).count();
            if commits > 1 || args.iter().any(|arg| arg.contains("..")) {
                run(&["cherry-pick"])
            } else {
                one(&["cherry-pick"])
            }
        }
        // The count of an `am` or a multi-commit `revert` is not in the words:
        // one commit, which credits fewer entries, never more.
        other => one(&[other]),
    }
}

/// Where a run lands, given the work tree and git dir it named and where it ran.
fn aimed_at(
    work_tree: Option<Option<PathBuf>>,
    git_dir: Option<Option<PathBuf>>,
    here: Option<PathBuf>,
    worktree: &Path,
    there: &Path,
) -> Target {
    match (work_tree, git_dir) {
        (None, None) => match here {
            Some(dir) if lands_in(&dir, worktree) => Target::Here,
            Some(_) => Target::Elsewhere,
            None => Target::Unknown,
        },
        // A named git dir without a work tree is that repository's own: it is
        // this one only when it is this worktree's `.git`.
        (None, Some(Some(git))) => {
            if git == there.join(".git") {
                Target::Here
            } else {
                Target::Elsewhere
            }
        }
        (Some(Some(tree)), None) => {
            let (in_tree, in_dir) = (
                lands_in(&tree, worktree),
                here.as_deref().is_some_and(|dir| lands_in(dir, worktree)),
            );
            match (in_tree, in_dir) {
                (true, true) => Target::Here,
                (false, false) => Target::Elsewhere,
                _ => Target::Unknown,
            }
        }
        (Some(Some(tree)), Some(Some(git))) => {
            match (lands_in(&tree, worktree), git == there.join(".git")) {
                (true, true) => Target::Here,
                (false, false) => Target::Elsewhere,
                _ => Target::Unknown,
            }
        }
        // A path that does not resolve is not a place.
        _ => Target::Unknown,
    }
}

/// Whether `command`, run in `worktree`, is a commit-making git form proved to
/// run there: the only kind of command a link is made from. See
/// [`command_target`] for what is proved and what is not.
#[cfg(test)]
pub(crate) fn runs_git_mutation(command: &str, worktree: &Path) -> bool {
    command_target(command, worktree) == Target::Here
}
