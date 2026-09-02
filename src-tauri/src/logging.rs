use log::{Level, LevelFilter, Log, Metadata, Record};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

pub const MAX_LOG_ENTRIES: usize = 1000;
const TAIL_MAX_LINES: usize = 500;
const DEFAULT_TAIL_LINES: usize = 200;

/// Directory override for the durable log.
///
/// Also the only way a binary outside [`LOGGED_BINARIES`] gets a file at all,
/// which is what keeps a test run from writing into the real log directory.
pub const LOG_DIR_ENV: &str = "GITPULSE_LOG_DIR";

/// Size at which the live log rotates to `<stem>.log.1`. Two generations are
/// kept, so the durable record is bounded at twice this on disk.
const LOG_FILE_MAX_BYTES: u64 = 1_048_576;

/// The shipped binaries, each of which writes its own durable log.
///
/// The file is named from `current_exe()`, so without this gate every `cargo
/// test` binary would drop a log into the user's real log directory. Unknown
/// stems fail closed and get no file; [`LOG_DIR_ENV`] opts anything in.
const LOGGED_BINARIES: [&str; 3] = ["gitpulse", "gitpulsed", "gitpulse-mcp"];

/// Lines of panic backtrace kept.
///
/// A capture can run to a hundred frames of runtime scaffolding, and the bug
/// is at the top. Bounded so one panic cannot flush the ring it is being
/// written into — and the elision is announced, because a stack that stops
/// early looks exactly like a stack that ended.
const PANIC_BACKTRACE_LINES: usize = 40;

type Entries = Arc<Mutex<VecDeque<(Level, String)>>>;

/// The durable log's own account of itself.
///
/// `lines` being empty is not the same fact as there being no log, and neither
/// is the same as a log that could not be written — a reader given only the
/// lines would present all three as a quiet session. `path` and `degraded`
/// are what let the report say which of the three it is.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PersistedLog {
    /// The live log file, or empty when this binary keeps none.
    pub path: String,
    /// Tail of the durable record, oldest first, spanning both generations.
    pub lines: Vec<String>,
    /// Why the record is incomplete, when it is. `None` means the sink has
    /// accepted every line it was given.
    pub degraded: Option<String>,
}

impl PersistedLog {
    /// The answer for a binary that keeps no durable log. Stated, not implied
    /// by an empty `lines`.
    fn unavailable(reason: &str) -> Self {
        Self {
            path: String::new(),
            lines: Vec::new(),
            degraded: Some(reason.to_string()),
        }
    }
}

/// Mutable half of the file sink.
struct SinkState {
    /// `None` once opening or writing has failed; `degraded` says why.
    file: Option<File>,
    /// Bytes written to the live generation, for the rotation bound.
    bytes: u64,
    degraded: Option<String>,
}

/// Append-only mirror of the ring, on disk.
///
/// The ring alone answers "what happened" only for a process that is still
/// alive to be asked. The panic hook's own entry was the clearest casualty:
/// it was recorded into memory and then lost with the process that recorded
/// it. Every line therefore reaches the file synchronously, before the
/// process has a chance to die, and the file outlives it.
struct FileSink {
    current: PathBuf,
    previous: PathBuf,
    state: Mutex<SinkState>,
}

impl FileSink {
    /// Opens (or creates) `<dir>/<stem>.log` in append mode and marks the
    /// session. Appending rather than truncating is deliberate: the lines
    /// above the marker are the previous session's, which is exactly the
    /// context wanted after a crash and a relaunch.
    fn open_in(dir: &Path, stem: &str) -> Self {
        let sink = Self {
            current: dir.join(format!("{stem}.log")),
            previous: dir.join(format!("{stem}.log.1")),
            state: Mutex::new(SinkState {
                file: None,
                bytes: 0,
                degraded: None,
            }),
        };
        {
            let mut state = sink.state.lock().unwrap_or_else(PoisonError::into_inner);
            match Self::open_append(dir, &sink.current) {
                Ok((file, bytes)) => {
                    state.file = Some(file);
                    state.bytes = bytes;
                }
                Err(e) => state.degraded = Some(e),
            }
            Self::write_locked(&mut state, &session_marker(stem, "start"));
        }
        sink
    }

    fn open_append(dir: &Path, path: &Path) -> Result<(File, u64), String> {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok((file, bytes))
    }

    /// Writes one line, rotating first if the live generation is full.
    ///
    /// Every failure path here is silent by design: this runs inside the
    /// panic hook, and a panic raised while a panic is unwinding aborts the
    /// process immediately — turning the one moment the log matters most into
    /// the one moment it destroys the evidence. The failure is recorded in
    /// `degraded` instead, where [`PersistedLog`] reports it.
    fn write_line(&self, line: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.bytes >= LOG_FILE_MAX_BYTES {
            self.rotate(&mut state);
        }
        Self::write_locked(&mut state, line);
    }

    fn write_locked(state: &mut SinkState, line: &str) {
        let Some(file) = state.file.as_mut() else {
            return;
        };
        match writeln!(file, "{line}") {
            // `File` is unbuffered, so a successful write is already on the
            // way to disk; there is no flush left to miss on the way out.
            Ok(()) => state.bytes += line.len() as u64 + 1,
            Err(e) => {
                state.degraded = Some(format!("write failed: {e}"));
                state.file = None;
            }
        }
    }

    /// Moves the live generation aside and starts a fresh one.
    fn rotate(&self, state: &mut SinkState) {
        // Closed before the rename: Windows will not move an open file.
        state.file = None;
        let reopened = match fs::rename(&self.current, &self.previous) {
            Ok(()) => Self::open_append(parent_of(&self.current), &self.current),
            Err(e) => {
                // Bounded beats complete. Losing the older generation is a
                // real loss; a log that cannot rotate and grows without end
                // on a user's disk is a worse one, and this says which
                // happened rather than leaving the gap to be inferred.
                state.degraded = Some(format!("rotate failed, truncated instead: {e}"));
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&self.current)
                    .map(|file| (file, 0))
                    .map_err(|e| format!("truncate {}: {e}", self.current.display()))
            }
        };
        state.bytes = 0;
        match reopened {
            Ok((file, bytes)) => {
                state.file = Some(file);
                state.bytes = bytes;
                Self::write_locked(state, &session_marker(&stem_of(&self.current), "rotated"));
            }
            Err(e) => state.degraded = Some(e),
        }
    }

    /// The durable tail, oldest line first, spanning both generations.
    fn tail(&self, max_lines: usize) -> PersistedLog {
        let max_lines = max_lines.clamp(1, TAIL_MAX_LINES);
        let degraded = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.degraded.clone()
        };
        let mut lines = read_lines(&self.previous);
        lines.extend(read_lines(&self.current));
        let start = lines.len().saturating_sub(max_lines);
        PersistedLog {
            path: self.current.display().to_string(),
            lines: lines.split_off(start),
            degraded,
        }
    }
}

/// Best-effort whole-file read; an unreadable generation contributes nothing
/// rather than failing the tail that the other generation can still answer.
fn read_lines(path: &Path) -> Vec<String> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file).lines().map_while(Result::ok).collect()
}

fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gitpulse".to_string())
}

/// Delimits one process's lines from the ones already in the file.
fn session_marker(stem: &str, kind: &str) -> String {
    format!(
        "--- {stem} {kind} pid {} at {} ---",
        std::process::id(),
        format_utc(now_epoch_secs())
    )
}

/// The running binary's file stem, or `None` when it cannot be determined.
fn exe_stem() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
}

/// Where the platform keeps a user's application logs.
fn default_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Logs")
                .join("GitPulse")
        })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("GitPulse").join("logs"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .map(|base| base.join("gitpulse"))
    }
}

/// Which directory and file stem this process's durable log uses, if any.
///
/// Split out from [`durable_sink`] so the decision is testable without
/// mutating process-global environment state from a parallel test run.
fn sink_target(stem: Option<&str>, explicit: Option<&Path>) -> Option<(PathBuf, String)> {
    match explicit {
        Some(dir) => Some((dir.to_path_buf(), stem.unwrap_or("gitpulse").to_string())),
        None => {
            let stem = stem.filter(|s| LOGGED_BINARIES.contains(s))?;
            Some((default_log_dir()?, stem.to_string()))
        }
    }
}

fn durable_sink() -> Option<FileSink> {
    let explicit = std::env::var_os(LOG_DIR_ENV).filter(|dir| !dir.is_empty());
    let (dir, stem) = sink_target(exe_stem().as_deref(), explicit.as_ref().map(Path::new))?;
    Some(FileSink::open_in(&dir, &stem))
}

#[derive(Clone)]
pub(crate) struct RingLogger {
    max_level: LevelFilter,
    entries: Entries,
    /// The durable mirror. `None` for a binary that keeps no file, and for
    /// every logger a test builds by hand.
    sink: Option<Arc<FileSink>>,
}

static LOGGER: OnceLock<RingLogger> = OnceLock::new();

impl RingLogger {
    pub(crate) fn new(max_level: LevelFilter) -> Self {
        Self {
            max_level,
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
            sink: None,
        }
    }

    fn push(&self, level: Level, line: String) {
        let mut queue = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.len() >= MAX_LOG_ENTRIES {
            queue.pop_front();
        }
        queue.push_back((level, line));
    }

    fn write_entry(&self, level: Level, target: &str, message: &str) {
        let line = format_entry(now_epoch_secs(), level, target, message);
        self.push(level, line.clone());
        // Disk before stderr: stderr is discarded for a bundled app launched
        // from a desktop shell, and is the half that cannot be read back.
        if let Some(sink) = self.sink.as_ref() {
            sink.write_line(&line);
        }
        eprintln!("{line}");
    }

    fn snapshot_tail(&self, max_lines: usize) -> Vec<String> {
        let max_lines = max_lines.clamp(1, TAIL_MAX_LINES);
        let queue = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<String> = queue
            .iter()
            .rev()
            .take(max_lines)
            .map(|(_, line)| line.clone())
            .collect();
        out.reverse();
        out
    }
}

impl Log for RingLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.max_level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.write_entry(record.level(), record.target(), &record.args().to_string());
    }

    fn flush(&self) {}
}

fn configured_level() -> LevelFilter {
    if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    }
}

/// The process-wide logger, ring and durable sink together.
fn build_logger() -> RingLogger {
    let mut logger = RingLogger::new(configured_level());
    logger.sink = durable_sink().map(Arc::new);
    logger
}

pub fn init() {
    #[cfg(not(test))]
    {
        let logger = LOGGER.get_or_init(build_logger);
        if log::set_logger(logger).is_ok() {
            log::set_max_level(logger.max_level);
        }
    }
    #[cfg(test)]
    {
        LOGGER.get_or_init(build_logger);
    }
}

pub fn diagnostic_tail(max_lines: usize) -> Vec<String> {
    match LOGGER.get() {
        Some(logger) => logger.snapshot_tail(max_lines),
        None => Vec::new(),
    }
}

/// The durable record, which unlike [`diagnostic_tail`] survives the process
/// that wrote it — so the lines it returns after a relaunch include the ones
/// the previous session died producing.
pub fn persisted_log(max_lines: usize) -> PersistedLog {
    let Some(logger) = LOGGER.get() else {
        return PersistedLog::unavailable("logging not initialised");
    };
    match logger.sink.as_ref() {
        Some(sink) => sink.tail(max_lines),
        None => PersistedLog::unavailable(&format!(
            "no durable log for this binary; set {LOG_DIR_ENV} to enable one"
        )),
    }
}

#[tauri::command]
pub fn cmd_diagnostic_log_tail(max_lines: Option<usize>) -> Vec<String> {
    diagnostic_tail(max_lines.unwrap_or(DEFAULT_TAIL_LINES))
}

#[tauri::command]
pub fn cmd_diagnostic_persisted_log(max_lines: Option<usize>) -> PersistedLog {
    persisted_log(max_lines.unwrap_or(DEFAULT_TAIL_LINES))
}

pub fn install_panic_hook() {
    // Capture the handle at install time rather than looking up the static
    // per-panic: a hook installed before init() used to silently record
    // nothing forever, because `LOGGER.get()` stayed empty on every fire.
    let logger = LOGGER.get_or_init(build_logger);
    install_panic_hook_for(Arc::new(logger.clone()));
}

/// Core of [`install_panic_hook`], parameterized over the destination so a
/// test can point it at its own ring without polluting the global one. Still
/// global process state — callers must serialize and restore the prior hook.
pub(crate) fn install_panic_hook_for(logger: Arc<RingLogger>) {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let location = info
            .location()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        logger.write_entry(
            Level::Error,
            "panic",
            &format!("panic at {location}: {payload}"),
        );
        // One entry per line, never one entry holding a multi-line string:
        // every reader of this log — the ring, the file, the report — treats a
        // line as an entry, and an embedded newline would forge entries that
        // no longer parse.
        for line in backtrace_lines(PANIC_BACKTRACE_LINES) {
            logger.write_entry(Level::Error, "panic", &line);
        }
        original(info);
    }));
}

/// The panicking stack, trimmed to `max_lines` and labelled.
///
/// `force_capture` rather than `capture`: RUST_BACKTRACE is not set for an app
/// launched from a desktop shell, which is every crash a user will ever
/// report. Symbols survive in shipped builds because the release profile
/// strips debuginfo only — see `strip` in Cargo.toml.
fn backtrace_lines(max_lines: usize) -> Vec<String> {
    let captured = std::backtrace::Backtrace::force_capture().to_string();
    let all: Vec<&str> = captured
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    if all.is_empty() {
        return Vec::new();
    }
    let kept = all.len().min(max_lines);
    let mut out = Vec::with_capacity(kept + 2);
    out.push(format!("backtrace ({kept} of {} lines):", all.len()));
    out.extend(all[..kept].iter().map(|line| format!("  {line}")));
    if kept < all.len() {
        // A stack that stops early must not read as a stack that ended.
        out.push(format!("  … {} further lines elided …", all.len() - kept));
    }
    out
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days-from-civil inverse (Howard Hinnant's algorithm): converts a day count
/// relative to 1970-01-01 into a proleptic Gregorian (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

pub(crate) fn format_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

fn format_entry(epoch_secs: u64, level: Level, target: &str, message: &str) -> String {
    format!(
        "{} {} [{}] {}",
        format_utc(epoch_secs),
        level,
        target,
        message
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// `std::panic::set_hook` is process-global, so every test that installs
    /// one serializes here. Declared once at module scope on purpose: a
    /// per-test `static HOOK_LOCK` looks like the same guard and is not — two
    /// such tests hold *different* mutexes and race each other for the hook.
    static HOOK_LOCK: Mutex<()> = Mutex::new(());

    fn assert_parses(line: &str) {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        assert_eq!(parts.len(), 3, "malformed entry: {line:?}");
        let ts = parts[0];
        assert_eq!(ts.len(), 20, "timestamp shape wrong in {ts:?}");
        assert!(ts.ends_with('Z'), "timestamp must be UTC-suffixed: {ts:?}");
        for (i, b) in ts.bytes().enumerate() {
            match i {
                4 | 7 => assert!(b == b'-', "expected '-' at {i} in {ts:?}"),
                10 => assert!(b == b'T', "expected 'T' at {i} in {ts:?}"),
                13 | 16 => assert!(b == b':', "expected ':' at {i} in {ts:?}"),
                19 => assert!(b == b'Z', "expected UTC 'Z' suffix in {ts:?}"),
                _ => assert!(b.is_ascii_digit(), "expected digit at {i} in {ts:?}"),
            }
        }
        assert!(
            matches!(parts[1], "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"),
            "unknown level token {:?}",
            parts[1]
        );
        assert!(
            parts[2].starts_with('['),
            "entry must carry [target]: {:?}",
            parts[2]
        );
    }

    fn message_of(line: &str) -> &str {
        line.split_once("] ")
            .map(|(_, message)| message)
            .expect("entry carries [target] prefix")
    }

    fn messages_of(lines: &[String]) -> Vec<&str> {
        lines.iter().map(|l| message_of(l)).collect()
    }

    #[test]
    fn timestamp_formatter_matches_known_epochs() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(59), "1970-01-01T00:00:59Z");
        assert_eq!(format_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_utc(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(format_utc(1_234_567_890), "2009-02-13T23:31:30Z");
        assert_eq!(format_utc(2_000_000_000), "2033-05-18T03:33:20Z");
        assert_eq!(format_utc(4_102_444_799), "2099-12-31T23:59:59Z");
        assert_eq!(format_utc(4_102_444_800), "2100-01-01T00:00:00Z");
    }

    #[test]
    fn ring_buffer_evicts_oldest_at_cap_in_fifo_order() {
        let logger = RingLogger::new(LevelFilter::Trace);
        let total = (MAX_LOG_ENTRIES + 25) as i32;
        for i in 0..total {
            logger.write_entry(Level::Info, "ring", &format!("m{i}"));
        }
        let queue = logger.entries.lock().unwrap();
        assert_eq!(queue.len(), MAX_LOG_ENTRIES, "cap enforced");
        assert_eq!(
            message_of(&queue.front().unwrap().1),
            "m25",
            "oldest entries evicted first"
        );
        assert_eq!(
            message_of(&queue.back().unwrap().1),
            format!("m{}", total - 1),
            "newest entry retained at back"
        );
    }

    #[test]
    fn tail_is_newest_last_and_clamped_to_bounds() {
        let logger = RingLogger::new(LevelFilter::Trace);
        for i in 0..5 {
            logger.write_entry(Level::Info, "tail", &format!("m{i}"));
        }
        assert_eq!(
            messages_of(&logger.snapshot_tail(3)),
            vec!["m2", "m3", "m4"]
        );
        assert_eq!(messages_of(&logger.snapshot_tail(1)), vec!["m4"]);
        assert_eq!(logger.snapshot_tail(TAIL_MAX_LINES).len(), 5);
        assert_eq!(
            logger.snapshot_tail(usize::MAX).len(),
            5,
            "tail must never exceed what exists"
        );
        assert_eq!(
            messages_of(&logger.snapshot_tail(0)),
            vec!["m4"],
            "clamp raises 0 to 1"
        );
        let empty = RingLogger::new(LevelFilter::Trace);
        assert!(empty.snapshot_tail(10).is_empty());
    }

    #[test]
    fn global_tail_clamps_requested_line_count() {
        init();
        let logger = LOGGER.get_or_init(|| RingLogger::new(configured_level()));
        for i in 0..5 {
            logger.write_entry(Level::Info, "global-tail", &format!("g{i}"));
        }
        assert_eq!(diagnostic_tail(0).len(), 1);
        assert_eq!(
            messages_of(&diagnostic_tail(2)),
            vec!["g3", "g4"],
            "tail is newest-last"
        );
        assert!(diagnostic_tail(usize::MAX).len() <= TAIL_MAX_LINES);
        assert_eq!(messages_of(&diagnostic_tail(1)), vec!["g4"]);
    }

    #[test]
    fn entries_below_max_level_are_dropped() {
        let logger = RingLogger::new(LevelFilter::Info);
        logger.log(
            &Record::builder()
                .level(Level::Debug)
                .target("filter")
                .args(format_args!("chatty"))
                .build(),
        );
        assert!(
            logger.entries.lock().unwrap().is_empty(),
            "debug must be filtered under Info"
        );
        logger.log(
            &Record::builder()
                .level(Level::Warn)
                .target("filter")
                .args(format_args!("careful"))
                .build(),
        );
        logger.log(
            &Record::builder()
                .level(Level::Error)
                .target("filter")
                .args(format_args!("broken"))
                .build(),
        );
        let queue = logger.entries.lock().unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].0, Level::Warn);
        assert_eq!(queue[1].0, Level::Error);
        assert!(logger.enabled(&Metadata::builder().level(Level::Info).build()));
        assert!(
            !logger.enabled(&Metadata::builder().level(Level::Trace).build()),
            "trace must not be enabled under Info filter"
        );
    }

    #[test]
    fn concurrent_writers_never_corrupt_the_ring() {
        let logger = Arc::new(RingLogger::new(LevelFilter::Trace));
        let mut joins = Vec::new();
        for t in 0..8u32 {
            let logger = Arc::clone(&logger);
            joins.push(
                std::thread::Builder::new()
                    .name(format!("logging-test-{t}"))
                    .spawn(move || {
                        for i in 0..100u32 {
                            logger.write_entry(
                                Level::Info,
                                "concurrent",
                                &format!("thread{t}-i{i}"),
                            );
                        }
                    })
                    .expect("spawn writer"),
            );
        }
        for join in joins {
            join.join().expect("writer thread panicked");
        }
        let queue = logger.entries.lock().unwrap();
        assert!(queue.len() <= MAX_LOG_ENTRIES);
        assert_eq!(queue.len(), 800, "800 entries fit below the cap");
        for (_, line) in queue.iter() {
            assert_parses(line);
        }
    }

    #[test]
    fn formatted_entries_round_trip_through_parser() {
        let logger = RingLogger::new(LevelFilter::Trace);
        for level in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            logger.write_entry(level, "round-trip", "hello");
        }
        let queue = logger.entries.lock().unwrap();
        assert_eq!(queue.len(), 5);
        for ((recorded_level, line), expected) in queue.iter().zip([
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ]) {
            assert_eq!(*recorded_level, expected);
            assert!(line.contains("[round-trip] hello"), "{line}");
            assert_parses(line);
        }
    }

    /// The panic hook must actually record what it observes. set_hook is
    /// global process state, so the test serializes on a mutex and restores
    /// the prior (test-harness) hook before any assertion can fail.
    #[test]
    fn panic_hook_records_payload_and_location_through_its_logger() {
        let _guard = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        let original = std::panic::take_hook();
        let logger = Arc::new(RingLogger::new(LevelFilter::Trace));
        install_panic_hook_for(logger.clone());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("boom-gap-probe");
        }));
        std::panic::set_hook(original);

        assert!(caught.is_err(), "the panic must still unwind");
        let lines = logger.snapshot_tail(50);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[panic]") && l.contains("boom-gap-probe")),
            "hook must record payload under the [panic] target: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("panic at ")),
            "hook must record a location: {lines:?}"
        );
    }

    // ---- durable sink -------------------------------------------------
    //
    // The ring answers "what happened" only for a process still alive to be
    // asked, which is never true of the process whose crash you are trying to
    // explain. These pin the half that outlives it.

    fn sunk(dir: &std::path::Path, stem: &str) -> RingLogger {
        let mut logger = RingLogger::new(LevelFilter::Trace);
        logger.sink = Some(Arc::new(FileSink::open_in(dir, stem)));
        logger
    }

    fn file_lines(path: &std::path::Path) -> Vec<String> {
        read_lines(path)
    }

    #[test]
    fn durable_log_outlives_the_logger_that_wrote_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        {
            let logger = sunk(dir.path(), "gitpulse");
            logger.write_entry(Level::Error, "boot", "engine failed to start");
        } // logger dropped: the process it stood for is gone.

        let lines = file_lines(&dir.path().join("gitpulse.log"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[boot] engine failed to start")),
            "the entry must survive its logger: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("gitpulse start pid")),
            "the session must be marked so a relaunch is distinguishable: {lines:?}"
        );
    }

    /// The gap this sink exists to close. The hook recorded the panic into a
    /// ring that died with the process, so the one entry that explained the
    /// crash was the one guaranteed to be lost.
    #[test]
    fn panic_hook_entry_reaches_disk_before_the_process_can_die() {
        let _guard = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("temp dir");

        let original = std::panic::take_hook();
        let logger = Arc::new(sunk(dir.path(), "gitpulse"));
        install_panic_hook_for(logger.clone());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("durable-boom");
        }));
        std::panic::set_hook(original);
        assert!(caught.is_err(), "the panic must still unwind");

        // Read the file, not the ring: the ring is what used to be lost.
        let lines = file_lines(&dir.path().join("gitpulse.log"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[panic]") && l.contains("durable-boom")),
            "the panic must be on disk, not only in memory: {lines:?}"
        );
    }

    #[test]
    fn a_relaunch_appends_below_the_previous_sessions_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        sunk(dir.path(), "gitpulse").write_entry(Level::Error, "run1", "died here");
        sunk(dir.path(), "gitpulse").write_entry(Level::Info, "run2", "back up");

        let lines = file_lines(&dir.path().join("gitpulse.log"));
        let first = lines
            .iter()
            .position(|l| l.contains("died here"))
            .expect("previous session retained");
        let second = lines
            .iter()
            .position(|l| l.contains("back up"))
            .expect("current session recorded");
        assert!(
            first < second,
            "the crash must still be readable above the relaunch: {lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.contains("start pid")).count(),
            2,
            "each session marks itself: {lines:?}"
        );
    }

    #[test]
    fn rotation_bounds_the_file_and_keeps_the_older_generation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let logger = sunk(dir.path(), "gitpulse");
        let sink = logger.sink.clone().expect("sink");

        sink.write_line(&"x".repeat(LOG_FILE_MAX_BYTES as usize + 1));
        sink.write_line("after the rotation");

        let current = dir.path().join("gitpulse.log");
        let previous = dir.path().join("gitpulse.log.1");
        assert!(previous.exists(), "the full generation must be kept aside");
        assert!(
            std::fs::metadata(&current).expect("live log").len() < LOG_FILE_MAX_BYTES,
            "the live generation restarts small"
        );
        let live = file_lines(&current);
        assert!(
            live.iter().any(|l| l.contains("after the rotation")),
            "writing continues past a rotation: {live:?}"
        );
        assert!(
            live.iter().any(|l| l.contains("rotated pid")),
            "the rotation announces itself so a gap is never silent: {live:?}"
        );
    }

    #[test]
    fn tail_spans_both_generations_oldest_first_and_clamps() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("gitpulse.log.1"), "older-a\nolder-b\n").expect("seed");
        let logger = sunk(dir.path(), "gitpulse");
        let sink = logger.sink.clone().expect("sink");
        sink.write_line("newer-a");

        let all = sink.tail(TAIL_MAX_LINES);
        assert_eq!(all.lines.first().map(String::as_str), Some("older-a"));
        assert_eq!(all.lines.last().map(String::as_str), Some("newer-a"));
        assert_eq!(
            all.degraded, None,
            "a healthy sink claims nothing is missing"
        );
        assert!(all.path.ends_with("gitpulse.log"), "{}", all.path);

        assert_eq!(sink.tail(1).lines, vec!["newer-a".to_string()]);
        assert_eq!(sink.tail(0).lines.len(), 1, "clamp raises 0 to 1");
        assert!(sink.tail(usize::MAX).lines.len() <= TAIL_MAX_LINES);
    }

    /// A sink that cannot write must not report the same thing as one that
    /// wrote everything: an empty tail would read as a quiet session.
    #[test]
    fn an_unwritable_directory_degrades_loudly_and_never_panics() {
        let dir = tempfile::tempdir().expect("temp dir");
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"occupied").expect("occupy the path");

        let sink = FileSink::open_in(&blocked, "gitpulse");
        // No panic, and the failure is still reported through the sink.
        sink.write_line("this cannot land anywhere");
        let reported = sink.tail(10);
        assert!(reported.lines.is_empty(), "nothing was written");
        let degraded = reported.degraded.expect("an unwritable sink must say so");
        assert!(
            degraded.contains("not-a-directory"),
            "the reason must name the path: {degraded}"
        );
    }

    #[test]
    fn only_shipped_binaries_get_a_log_directory_of_their_own() {
        // A test binary must never leave a file in the user's log directory,
        // which is exactly what naming the file after current_exe() invites.
        assert_eq!(sink_target(Some("gitpulse_lib-2a0bc691"), None), None);
        assert_eq!(sink_target(None, None), None);
        for shipped in LOGGED_BINARIES {
            let (dir, stem) = sink_target(Some(shipped), None)
                .unwrap_or_else(|| panic!("{shipped} must keep a durable log"));
            assert_eq!(stem, shipped);
            assert!(!dir.as_os_str().is_empty());
        }
    }

    #[test]
    fn an_explicit_directory_opts_any_binary_in() {
        let explicit = std::path::Path::new("/tmp/gitpulse-logs");
        assert_eq!(
            sink_target(Some("some-test-runner"), Some(explicit)),
            Some((explicit.to_path_buf(), "some-test-runner".to_string()))
        );
    }

    #[test]
    fn a_panic_records_the_stack_that_produced_it_not_only_its_location() {
        let _guard = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        let original = std::panic::take_hook();
        let logger = Arc::new(RingLogger::new(LevelFilter::Trace));
        install_panic_hook_for(logger.clone());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("stack-probe");
        }));
        std::panic::set_hook(original);
        assert!(caught.is_err());

        let lines = logger.snapshot_tail(TAIL_MAX_LINES);
        assert!(
            lines.iter().any(|l| l.contains("stack-probe")),
            "the payload is still recorded: {lines:?}"
        );
        let header = lines
            .iter()
            .find(|l| l.contains("backtrace ("))
            .expect("a panic must record the stack that produced it");
        assert!(
            header.contains("[panic]"),
            "backtrace belongs to the panic target: {header}"
        );
        // Every recorded line is one entry: a multi-line entry would forge
        // lines that no longer parse when the file is read back.
        for line in &lines {
            assert!(!line.contains('\n'), "entry spans lines: {line:?}");
        }
    }

    #[test]
    fn a_trimmed_backtrace_says_it_was_trimmed() {
        let trimmed = backtrace_lines(3);
        assert!(!trimmed.is_empty(), "a capture must produce something");
        assert!(trimmed[0].starts_with("backtrace ("), "{:?}", trimmed[0]);
        assert!(
            trimmed.len() <= 3 + 2,
            "the bound holds, plus header and elision note: {trimmed:?}"
        );
        // The whole point: a stack that stops early must not read as one that
        // ended. Only assert the notice when there was genuinely more.
        let full = backtrace_lines(usize::MAX);
        if full.len() > trimmed.len() {
            assert!(
                trimmed
                    .last()
                    .is_some_and(|l| l.contains("further lines elided")),
                "truncation must announce itself: {trimmed:?}"
            );
        }
    }

    #[test]
    fn persisted_log_states_absence_rather_than_implying_it() {
        // The global logger under `cargo test` resolves no sink, so this is
        // the real "no durable log" answer, not a fabricated one.
        init();
        let reported = persisted_log(10);
        assert!(reported.lines.is_empty());
        assert!(
            reported
                .degraded
                .as_deref()
                .is_some_and(|d| d.contains(LOG_DIR_ENV)),
            "absence must be stated, and say how to change it: {reported:?}"
        );
    }
}
