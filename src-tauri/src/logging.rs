use log::{Level, LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

pub const MAX_LOG_ENTRIES: usize = 1000;
const TAIL_MAX_LINES: usize = 500;
const DEFAULT_TAIL_LINES: usize = 200;

type Entries = Arc<Mutex<VecDeque<(Level, String)>>>;

#[derive(Clone)]
pub(crate) struct RingLogger {
    max_level: LevelFilter,
    entries: Entries,
}

static LOGGER: OnceLock<RingLogger> = OnceLock::new();

impl RingLogger {
    pub(crate) fn new(max_level: LevelFilter) -> Self {
        Self {
            max_level,
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
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

pub fn init() {
    #[cfg(not(test))]
    {
        let logger = LOGGER.get_or_init(|| RingLogger::new(configured_level()));
        if log::set_logger(logger).is_ok() {
            log::set_max_level(logger.max_level);
        }
    }
    #[cfg(test)]
    {
        LOGGER.get_or_init(|| RingLogger::new(configured_level()));
    }
}

pub fn diagnostic_tail(max_lines: usize) -> Vec<String> {
    match LOGGER.get() {
        Some(logger) => logger.snapshot_tail(max_lines),
        None => Vec::new(),
    }
}

#[tauri::command]
pub fn cmd_diagnostic_log_tail(max_lines: Option<usize>) -> Vec<String> {
    diagnostic_tail(max_lines.unwrap_or(DEFAULT_TAIL_LINES))
}

pub fn install_panic_hook() {
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
        if let Some(logger) = LOGGER.get() {
            logger.write_entry(
                Level::Error,
                "panic",
                &format!("panic at {location}: {payload}"),
            );
        }
        original(info);
    }));
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
}
