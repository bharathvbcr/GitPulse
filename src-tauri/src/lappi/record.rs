//! Caller records: `lappi.caller_record` version 1, written locally and never sent.
//!
//! `docs/caller-contract.md` §5 in Lappi-decision is the contract, and its
//! checker is `qd_runtime::caller_record::validate_line`; nothing here is
//! trusted to have matched it except by the tests that run that checker. The
//! human's rule for this data is "Synthetic only": these records are not
//! training or eval data, so every line says `admission: "not_admitted"`, the
//! store sits under a `heldout` path segment that `qd-train` refuses by path,
//! and nothing here copies, uploads or sends a record anywhere.
//!
//! What goes in is structure, not content: the classifier's counts, the type
//! GitPulse chose, what Lappi said, and later the final message's parsed type,
//! scope and breaking flag. No patch text, no file names, no message text.
//! Every string still passes [`crate::ledger::redact::text`] before it is
//! written.

use std::collections::VecDeque;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value};

use super::client::Outcome;
use super::request::{APP, COMMIT_TYPE_TASK};
use crate::ai::commit_brief::{self, CommitDraft};

pub const RECORD: &str = "lappi.caller_record";
pub const RECORD_VERSION: u64 = 1;
/// The only legal value under version 1.
pub const ADMISSION_NOT_ADMITTED: &str = "not_admitted";
/// The redactor every string passes, named in each line's `redaction.policy`.
pub const REDACTION_POLICY: &str = "gitpulse.ledger.redact";
/// The decision point, which for GitPulse is also the task it asks.
pub const DECISION_POINT: &str = COMMIT_TYPE_TASK;
/// The store, relative to `$HOME`; mirrors `STORE_RELATIVE_TO_HOME` in
/// Lappi-decision `crates/qd-runtime/src/caller_record.rs`. The `heldout`
/// segment is load-bearing: it is what makes `qd-train` refuse the store.
pub const STORE_RELATIVE_TO_HOME: &str = "Library/Application Support/Lappi/heldout/caller-records";
/// Path segments that mark a held-out tree, case-folded (the checker's own).
const HELD_OUT_MARKERS: [&str; 3] = ["heldout", "held_out", "held-out"];
/// One line's cap, newline excluded; mirrors `MAX_RECORD_BYTES` in
/// `caller_record.rs`. A line over it is not written.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
/// A day's file over this stops recording until the user clears the store.
pub const MAX_DAY_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The app's store over this stops recording until the user clears it.
pub const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
/// More files than this in the store is treated as over the cap rather than
/// walked: the scan is bounded like everything else.
#[cfg(unix)]
const MAX_STORE_FILES: usize = 4096;
/// `LAPPI_COLLECT=0` forces recording off, whatever the setting says.
pub const COLLECT_ENV: &str = "LAPPI_COLLECT";

/// Whether the environment forces recording off.
pub fn collect_forced_off() -> bool {
    std::env::var_os(COLLECT_ENV).is_some_and(|value| value == "0")
}

/// 32 lowercase hex characters from the OS's random source.
#[cfg(unix)]
pub fn new_record_id() -> io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(not(unix))]
pub fn new_record_id() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "caller records draw their ids from /dev/urandom, which this platform does not have",
    ))
}

/// Why a record line was not built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// Over [`MAX_RECORD_BYTES`].
    TooLarge(usize),
    /// The redactor rewrote the record id, so the line could not be paired.
    IdRewritten,
    Serialize(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(got) => write!(
                f,
                "the record is {got} bytes, over the {MAX_RECORD_BYTES}-byte line cap"
            ),
            Self::IdRewritten => f.write_str("the redactor rewrote the record id"),
            Self::Serialize(error) => write!(f, "the record could not be serialised: {error}"),
        }
    }
}

#[derive(Serialize)]
struct Envelope<'a, B: Serialize> {
    record: &'static str,
    record_version: u64,
    kind: &'static str,
    record_id: &'a str,
    app: &'static str,
    app_version: &'static str,
    decision_point: &'static str,
    created_at: &'a str,
    admission: &'static str,
    #[serde(flatten)]
    body: B,
}

#[derive(Serialize)]
struct DecisionBody<'a> {
    facts: Facts,
    app_choice: AppChoice,
    lappi: LappiBlock<'a>,
}

#[derive(Serialize)]
struct OutcomeBody {
    observed: Observed,
}

#[derive(Serialize)]
struct Facts {
    files: usize,
    roles: Roles,
    kinds: Kinds,
    unparsed: usize,
    binary: usize,
    mode_only: usize,
    style_only: usize,
    additions: u64,
    deletions: u64,
    patch_truncated: bool,
    repo_conventional: bool,
}

#[derive(Serialize)]
struct Roles {
    source: usize,
    test: usize,
    ci: usize,
    deps: usize,
    build: usize,
    docs: usize,
}

#[derive(Serialize)]
struct Kinds {
    added: usize,
    modified: usize,
    deleted: usize,
    renamed: usize,
    copied: usize,
}

#[derive(Serialize)]
struct AppChoice {
    /// The classifier's type: what GitPulse chose on its own.
    #[serde(rename = "type")]
    kind: Option<&'static str>,
    high_confidence: bool,
    /// The type Lappi's answer pre-selected in the draft, if one was admitted.
    prefilled: Option<&'static str>,
}

#[derive(Serialize)]
struct LappiBlock<'a> {
    asked: bool,
    task: Option<&'static str>,
    reading: &'static str,
    kind: Option<&'a str>,
    backend: Option<&'a str>,
    slots: Option<&'a Map<String, Value>>,
    latency_ms: Option<u64>,
}

#[derive(Serialize)]
struct Observed {
    committed: bool,
    #[serde(rename = "type")]
    kind: Option<String>,
    scope: Option<String>,
    breaking: bool,
    conventional: bool,
}

/// What GitPulse saw, chose and heard at one low-confidence draft.
pub struct DecisionInput<'a> {
    pub record_id: &'a str,
    pub created_at: &'a str,
    pub draft: &'a CommitDraft,
    pub prefilled: Option<&'static str>,
    pub outcome: &'a Outcome,
    pub latency_ms: Option<u64>,
}

/// The decision line, newline excluded.
pub fn decision_line(input: &DecisionInput<'_>) -> Result<Vec<u8>, BuildError> {
    let facts = &input.draft.facts;
    let answer = input.outcome.answer();
    let body = DecisionBody {
        facts: Facts {
            files: facts.files,
            roles: Roles {
                source: facts.source,
                test: facts.tests,
                ci: facts.ci,
                deps: facts.deps,
                build: facts.build,
                docs: facts.docs,
            },
            kinds: Kinds {
                added: facts.added,
                modified: facts.modified,
                deleted: facts.deleted,
                renamed: facts.renamed,
                copied: facts.copied,
            },
            unparsed: facts.unparsed,
            binary: facts.binary,
            mode_only: facts.mode_only,
            style_only: facts.style_only,
            additions: facts.additions,
            deletions: facts.deletions,
            patch_truncated: input.draft.patch_truncated,
            repo_conventional: input.draft.conventional,
        },
        app_choice: AppChoice {
            kind: input.draft.change_type,
            high_confidence: input.draft.high_confidence,
            prefilled: input.prefilled,
        },
        lappi: LappiBlock {
            asked: input.outcome.asked(),
            task: input.outcome.asked().then_some(COMMIT_TYPE_TASK),
            reading: input.outcome.reading(),
            kind: input.outcome.kind(),
            backend: answer.map(|(backend, _)| backend),
            slots: answer.map(|(_, slots)| slots),
            latency_ms: input.outcome.asked().then_some(input.latency_ms).flatten(),
        },
    };
    finish("decision", input.record_id, input.created_at, body)
}

/// The outcome line for a commit whose final message is `message`.
///
/// Only the message's parsed subject reaches the record: its conventional
/// type when it is one GitPulse knows, its scope, and whether it is marked
/// breaking. The message text itself never does.
pub fn outcome_line(
    record_id: &str,
    created_at: &str,
    message: &str,
) -> Result<Vec<u8>, BuildError> {
    let subject = message.lines().next().unwrap_or("").trim();
    let parsed = commit_brief::split_conventional(subject);
    let observed = match parsed {
        Some(parsed) => {
            let kind = parsed.kind.to_ascii_lowercase();
            Observed {
                committed: true,
                kind: commit_brief::KNOWN_TYPES
                    .contains(&kind.as_str())
                    .then_some(kind),
                scope: parsed.scope.map(|scope| scope.chars().take(64).collect()),
                breaking: parsed.breaking || has_breaking_footer(message),
                conventional: true,
            }
        }
        None => Observed {
            committed: true,
            kind: None,
            scope: None,
            breaking: has_breaking_footer(message),
            conventional: false,
        },
    };
    finish("outcome", record_id, created_at, OutcomeBody { observed })
}

/// A `BREAKING CHANGE:` / `BREAKING-CHANGE:` footer, per Conventional Commits.
fn has_breaking_footer(message: &str) -> bool {
    message.lines().skip(1).any(|line| {
        let line = line.trim_start();
        line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:")
    })
}

fn finish<B: Serialize>(
    kind: &'static str,
    record_id: &str,
    created_at: &str,
    body: B,
) -> Result<Vec<u8>, BuildError> {
    let envelope = Envelope {
        record: RECORD,
        record_version: RECORD_VERSION,
        kind,
        record_id,
        app: APP,
        app_version: env!("CARGO_PKG_VERSION"),
        decision_point: DECISION_POINT,
        created_at,
        admission: ADMISSION_NOT_ADMITTED,
        body,
    };
    let mut value = serde_json::to_value(&envelope)
        .map_err(|error| BuildError::Serialize(error.to_string()))?;
    let mut redacted = 0u64;
    redact_strings(&mut value, &mut redacted);
    let Value::Object(object) = &mut value else {
        return Err(BuildError::Serialize("the record is not an object".into()));
    };
    if object.get("record_id").and_then(Value::as_str) != Some(record_id) {
        return Err(BuildError::IdRewritten);
    }
    object.insert(
        "redaction".into(),
        serde_json::json!({ "policy": REDACTION_POLICY, "fields_redacted": redacted }),
    );
    let line =
        serde_json::to_vec(&value).map_err(|error| BuildError::Serialize(error.to_string()))?;
    if line.len() > MAX_RECORD_BYTES {
        return Err(BuildError::TooLarge(line.len()));
    }
    Ok(line)
}

/// Every string in the record, keys included, through the ledger's redactor.
fn redact_strings(value: &mut Value, count: &mut u64) {
    match value {
        Value::String(text) => {
            let clean = crate::ledger::redact::text(text);
            if clean != *text {
                *count += 1;
                *text = clean;
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_strings(item, count);
            }
        }
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut item) in entries {
                let clean_key = crate::ledger::redact::text(&key);
                if clean_key != key {
                    *count += 1;
                }
                redact_strings(&mut item, count);
                map.insert(clean_key, item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// What one append did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    Written,
    /// `LAPPI_COLLECT=0`: nothing was attempted, so nothing was dropped.
    ForcedOff,
    /// Not written, and counted.
    Dropped(&'static str),
}

/// What the store has done this process, for the settings panel.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct StoreStatus {
    pub dir: String,
    pub written: u64,
    pub dropped: u64,
    /// Why recording stopped, once a cap was reached. Stays set until the
    /// user clears the store and GitPulse restarts.
    pub stopped: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct StoreState {
    written: u64,
    dropped: u64,
    stopped: Option<&'static str>,
    #[cfg_attr(not(unix), allow(dead_code))]
    store_bytes: Option<u64>,
    last_error: Option<String>,
    failure_logged: bool,
}

/// One app's record directory and the caps that guard it.
pub struct Store {
    dir: PathBuf,
    state: Mutex<StoreState>,
}

impl Store {
    /// A store at `dir`, which must lie under a held-out path segment: a
    /// record stored anywhere else is exactly how it would leak into training.
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        let held = dir.components().any(|component| match component {
            Component::Normal(segment) => segment.to_str().is_some_and(|segment| {
                HELD_OUT_MARKERS
                    .iter()
                    .any(|marker| segment.eq_ignore_ascii_case(marker))
            }),
            _ => false,
        });
        if !held {
            return Err(format!(
                "{} is not under a held-out path segment ({HELD_OUT_MARKERS:?})",
                dir.display()
            ));
        }
        Ok(Self {
            dir,
            state: Mutex::new(StoreState::default()),
        })
    }

    /// `$HOME/` + [`STORE_RELATIVE_TO_HOME`] + `/gitpulse`, or `None` with no `$HOME`.
    pub fn default_dir() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        if home.is_empty() {
            return None;
        }
        Some(PathBuf::from(home).join(STORE_RELATIVE_TO_HOME).join(APP))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn status(&self) -> StoreStatus {
        let state = self.lock();
        StoreStatus {
            dir: self.dir.display().to_string(),
            written: state.written,
            dropped: state.dropped,
            stopped: state.stopped.map(str::to_string),
            last_error: state.last_error.clone(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append one complete line to `<dir>/<day>.jsonl`.
    ///
    /// `O_APPEND | O_CREAT`, file `0600`, directories `0700`, one `write(2)` of
    /// the line and its newline, then `fsync`. Never a read-modify-write. A
    /// failure is returned for the caller to log once; it never changes what
    /// GitPulse decided.
    pub fn append(&self, day: &str, line: &[u8]) -> io::Result<Appended> {
        if collect_forced_off() {
            return Ok(Appended::ForcedOff);
        }
        let mut state = self.lock();
        if state.stopped.is_some() {
            state.dropped += 1;
            return Ok(Appended::Dropped("stopped"));
        }
        if line.len() > MAX_RECORD_BYTES || line.contains(&b'\n') {
            state.dropped += 1;
            return Ok(Appended::Dropped("line_over_cap"));
        }
        if !is_day(day) {
            state.dropped += 1;
            return Ok(Appended::Dropped("bad_day"));
        }
        match self.append_locked(&mut state, day, line) {
            Ok(appended) => {
                if let Appended::Dropped(_) = appended {
                    state.dropped += 1;
                } else {
                    state.written += 1;
                }
                Ok(appended)
            }
            Err(error) => {
                state.dropped += 1;
                state.last_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Whether this is the first failure, so the caller logs exactly once.
    pub fn first_failure(&self) -> bool {
        let mut state = self.lock();
        !std::mem::replace(&mut state.failure_logged, true)
    }

    #[cfg(unix)]
    fn append_locked(
        &self,
        state: &mut StoreState,
        day: &str,
        line: &[u8],
    ) -> io::Result<Appended> {
        use std::fs::{DirBuilder, OpenOptions, Permissions};
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let framed_len = line.len() as u64 + 1;
        let store_bytes = match state.store_bytes {
            Some(bytes) => bytes,
            None => {
                let bytes = scan_store(&self.dir)?;
                state.store_bytes = Some(bytes);
                bytes
            }
        };
        if store_bytes.saturating_add(framed_len) > MAX_STORE_BYTES {
            state.stopped = Some("store_over_cap");
            return Ok(Appended::Dropped("store_over_cap"));
        }
        let path = self.dir.join(format!("{day}.jsonl"));
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&path)?;
        let metadata = file.metadata()?;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(Permissions::from_mode(0o600))?;
        }
        if metadata.len().saturating_add(framed_len) > MAX_DAY_FILE_BYTES {
            state.stopped = Some("day_file_over_cap");
            return Ok(Appended::Dropped("day_file_over_cap"));
        }
        let mut framed = Vec::with_capacity(line.len() + 1);
        framed.extend_from_slice(line);
        framed.push(b'\n');
        let wrote = file.write(&framed)?;
        if wrote != framed.len() {
            // A torn line is on disk now; stop rather than add to it.
            state.stopped = Some("short_write");
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!(
                    "{} took {wrote} of {} bytes; recording stopped",
                    path.display(),
                    framed.len()
                ),
            ));
        }
        file.sync_all()?;
        state.store_bytes = Some(store_bytes.saturating_add(framed_len));
        Ok(Appended::Written)
    }

    #[cfg(not(unix))]
    fn append_locked(
        &self,
        _state: &mut StoreState,
        _day: &str,
        _line: &[u8],
    ) -> io::Result<Appended> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "caller records need Unix file modes, which this platform does not offer",
        ))
    }
}

/// `YYYY-MM-DD`, digits and dashes only, so a day can never name a path.
fn is_day(day: &str) -> bool {
    let bytes = day.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, byte)| match i {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// Bytes held by the store's `.jsonl` files. More files than
/// [`MAX_STORE_FILES`] counts as over the cap rather than being walked.
#[cfg(unix)]
fn scan_store(dir: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for (seen, entry) in std::fs::read_dir(dir)?.enumerate() {
        if seen >= MAX_STORE_FILES {
            return Ok(u64::MAX);
        }
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

/// Decisions waiting for their commit, keyed by repository.
///
/// Bounded twice: at most [`Pending::CAP`] repositories, and an entry older
/// than [`Pending::TTL`] is gone — a commit hours after a draft is not
/// evidence about that draft. A newer draft in the same repository replaces
/// the older one, which then simply stays unpaired.
pub struct Pending {
    entries: VecDeque<(String, String, Instant)>,
}

impl Pending {
    pub const CAP: usize = 64;
    pub const TTL: Duration = Duration::from_secs(6 * 60 * 60);

    pub const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    pub fn remember(&mut self, repo: &str, record_id: String, now: Instant) {
        self.expire(now);
        self.entries.retain(|(key, _, _)| key != repo);
        while self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back((repo.to_string(), record_id, now));
    }

    pub fn take(&mut self, repo: &str, now: Instant) -> Option<String> {
        self.expire(now);
        let at = self.entries.iter().position(|(key, _, _)| key == repo)?;
        self.entries.remove(at).map(|(_, id, _)| id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|(_, _, at)| now.saturating_duration_since(*at) < Self::TTL);
    }
}

impl Default for Pending {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn record_ids_are_32_lowercase_hex_and_survive_the_redactor() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..256 {
            let id = new_record_id().expect("urandom is readable");
            assert_eq!(id.len(), 32);
            assert!(id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            assert_eq!(
                crate::ledger::redact::text(&id),
                id,
                "redactor rewrote {id}"
            );
            seen.insert(id);
        }
        assert_eq!(seen.len(), 256, "random ids repeated");
    }

    #[test]
    fn pending_pairs_once_bounds_its_size_and_expires() {
        let start = Instant::now();
        let mut pending = Pending::new();
        pending.remember("/repo", "a".repeat(32), start);
        pending.remember("/repo", "b".repeat(32), start);
        assert_eq!(pending.len(), 1, "a newer draft replaces the older one");
        assert_eq!(pending.take("/repo", start), Some("b".repeat(32)));
        assert_eq!(pending.take("/repo", start), None, "paired at most once");

        for i in 0..(Pending::CAP + 10) {
            pending.remember(&format!("/r{i}"), "c".repeat(32), start);
        }
        assert_eq!(pending.len(), Pending::CAP);
        assert_eq!(pending.take("/r0", start), None, "oldest evicted first");

        pending.remember("/late", "d".repeat(32), start);
        assert_eq!(
            pending.take("/late", start + Pending::TTL),
            None,
            "stale expires"
        );
    }

    #[test]
    fn days_cannot_name_a_path() {
        assert!(is_day("2026-10-06"));
        for bad in ["2026-10-6", "../../etc", "2026/10/06", "2026-10-06x", ""] {
            assert!(!is_day(bad), "{bad}");
        }
    }

    #[test]
    fn a_store_outside_a_held_out_segment_is_refused() {
        assert!(Store::new(PathBuf::from("/tmp/caller-records/gitpulse")).is_err());
        assert!(Store::new(PathBuf::from("/tmp/Lappi/heldout/caller-records/gitpulse")).is_ok());
        assert!(Store::default_dir().is_none_or(|dir| Store::new(dir).is_ok()));
    }
}
