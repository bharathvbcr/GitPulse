//! Transcript sources: the agents whose session logs the ledger replays.
//!
//! The pass reads files; what makes a file a Claude Code transcript or a Codex
//! rollout is a source's. A source says where its files live ([`root`]), what
//! one of its lines holds ([`RecordParser`], one per file, since a Codex line
//! cannot be read without the `session_meta` line before it) and who the rows
//! are attributed to ([`actor_id`]). Everything after a line is parsed is
//! shared: [`ToolCall`] is the one record, and which worktree owns it is
//! [`transcript::attribution_root`] / [`transcript::departed_root`] for every
//! source, because a call's path or `cwd` says that whichever agent made it.
//!
//! [`root`]: TranscriptSource::root
//! [`actor_id`]: TranscriptSource::actor_id
//! [`ToolCall`]: super::transcript::ToolCall
//! [`transcript::attribution_root`]: super::transcript::attribution_root
//! [`transcript::departed_root`]: super::transcript::departed_root

use std::path::{Path, PathBuf};

use super::transcript::{self, LineOutcome, ToolCall};

/// Where a source's files live, and whether the person said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceRoot {
    pub path: PathBuf,
    /// Named by an environment override, rather than the agent's usual place
    /// under the home directory. A default that is absent is a machine that
    /// never ran the agent; an override that is absent is a mistake.
    pub explicit: bool,
}

/// What one line of a transcript turned out to hold.
#[derive(Debug, Default)]
pub(super) struct Parsed {
    /// The mutating tool calls the line holds.
    pub calls: Vec<ToolCall>,
    /// Things on the line this build could not read, counted into
    /// `CatchUp::skipped_lines`: a line that is not the format, or a call whose
    /// command or target could not be recovered. A line can hold readable calls
    /// and still skip one.
    pub skipped: i64,
}

impl Parsed {
    pub fn nothing() -> Self {
        Parsed::default()
    }

    pub fn skipped() -> Self {
        Parsed {
            calls: Vec::new(),
            skipped: 1,
        }
    }
}

/// Reads the lines of one transcript file, in order.
pub(super) trait RecordParser {
    fn parse(&mut self, line: &str) -> Parsed;
}

/// One agent's transcripts.
pub(super) trait TranscriptSource: Sync {
    /// For messages: which source a note is about.
    fn name(&self) -> &'static str;

    /// The `actor_id` of every row this source produces.
    fn actor_id(&self) -> &'static str;

    /// Where the files are; `None` when there is nowhere to look (no home
    /// directory and no override).
    fn root(&self) -> Option<SourceRoot>;

    /// Whether the files this source lists are covered by the legacy baseline
    /// floor: the line the per-worktree watermark drew before offsets existed,
    /// which only ever described Claude transcripts. A source that was never
    /// read that way must not be held to it, or its history before the newest
    /// Claude row would be dropped on its first pass.
    fn legacy_floor(&self) -> bool;

    /// Whether a root that cannot be walked is an error to report. Every source
    /// reports an override that points nowhere; a source that adopted this
    /// after shipping without it (Claude) keeps the silence it had.
    fn reports_unwalkable_root(&self) -> bool;

    /// Whether the agent may simply not be installed: its default directory
    /// being absent is then the ordinary case, and says nothing about the
    /// cursors it once wrote. For every other source an absent root stops the
    /// pass forgetting any cursor, as it always did for Claude's, because the
    /// cursors are keyed by a hash of the path and cannot be told apart by
    /// source: a cursor is forgotten only when every source that might own it
    /// was walked whole.
    fn optional(&self) -> bool;

    /// Whether the file at `path` is certainly this source's, by its first line.
    /// Asked only of a file two sources both list (their roots overlap), which
    /// has exactly one owner: the source that recognises it, else the one with
    /// the deeper root, else the first listed. A source with no such mark says no.
    fn recognises(&self, path: &Path) -> bool;

    /// A parser for the file at `path`, to be fed the lines from byte `start`.
    fn open(&self, path: &Path, start: u64) -> Box<dyn RecordParser>;
}

/// Claude Code: `~/.claude/projects/**/*.jsonl`.
pub(super) struct ClaudeSource;

struct ClaudeParser;

impl RecordParser for ClaudeParser {
    fn parse(&mut self, line: &str) -> Parsed {
        match transcript::parse_record(line) {
            LineOutcome::NoCalls => Parsed::nothing(),
            LineOutcome::Unreadable => Parsed::skipped(),
            LineOutcome::Calls(calls) => Parsed { calls, skipped: 0 },
        }
    }
}

impl TranscriptSource for ClaudeSource {
    fn name(&self) -> &'static str {
        "Claude"
    }

    fn actor_id(&self) -> &'static str {
        "claude-code"
    }

    fn root(&self) -> Option<SourceRoot> {
        match std::env::var_os("GITPULSE_TRANSCRIPT_ROOT") {
            Some(path) => Some(SourceRoot {
                path: PathBuf::from(path),
                explicit: true,
            }),
            None => super::dirs_home().map(|home| SourceRoot {
                path: home.join(".claude").join("projects"),
                explicit: false,
            }),
        }
    }

    fn legacy_floor(&self) -> bool {
        true
    }

    fn reports_unwalkable_root(&self) -> bool {
        false
    }

    fn optional(&self) -> bool {
        false
    }

    fn recognises(&self, _path: &Path) -> bool {
        false
    }

    fn open(&self, _path: &Path, _start: u64) -> Box<dyn RecordParser> {
        Box::new(ClaudeParser)
    }
}

/// Every source the pass reads, Claude first.
pub(super) fn sources() -> [&'static dyn TranscriptSource; 2] {
    [&ClaudeSource, &super::codex::CodexSource]
}
