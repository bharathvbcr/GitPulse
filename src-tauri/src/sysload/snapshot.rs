//! `host-load.v1`: the snapshot the sampler publishes for every GitPulse
//! process to read without asking the app.
//!
//! A fixed-size record, rewritten in place under a sequence lock, in the
//! per-user config directory beside `spawn-budget.v1` — the config directory
//! rather than `temp_dir()` for the same reason: agent hosts give the
//! processes they start a private `TMPDIR`. It is opened through the same
//! refusals (`fs_entry::open_private_record`): no symlink, a regular file,
//! owned by this user, private to them.
//!
//! # The sequence lock
//!
//! The writer makes the head sequence odd, writes the body and a tail copy of
//! the next even sequence, then makes the head even. A reader reads the whole
//! record, then the head again, and accepts it only when the head it read, the
//! tail and the re-read head are one even number, and the record's checksum
//! matches its bytes. Anything else is a write in progress and is retried a
//! bounded number of times, then reported as [`SnapshotError::Torn`] — never
//! decoded.
//!
//! The checksum is insurance, not a measured fix. The sequence lock is sound
//! only if a reader that sees a new body byte also sees the odd head written
//! before it, and that ordering is the kernel's to give across two processes'
//! `pread` and `pwrite`; no documented guarantee for it on APFS was found. The
//! racing-reader test accepted no mixed record with the checksum disabled
//! (0 of 12 runs), so this is a guard against a failure not observed. It
//! covers every version-1 byte before it (`0..OFF_CHECKSUM`); a later version
//! that appends fields covers those with a checksum of its own.
//!
//! # Compatibility
//!
//! Fields are only ever added, at the end, before nothing that an older
//! reader knows. A reader accepts any `VERSION` at or above its minimum whose
//! `length` covers every field it knows; anything else is a typed error
//! ([`SnapshotError::UnsupportedVersion`], [`SnapshotError::Short`]), never a
//! guess. The tail sequence sits at a fixed offset, so a longer record from a
//! newer writer still carries it where a v1 reader looks.
//!
//! # One writer
//!
//! Writing requires an exclusive advisory lock on [`LOCK_FILE_NAME`], held
//! for the writer's whole life: a dev build or a second profile beside the
//! installed app loses the election and reads instead.

use super::governor::{Binding, GovernorVerdict, Tier};
use super::procscan::SessionCount;
use super::sensor::{
    Compressor, CompressorDelta, CompressorSegments, Cores, CpuUsage, GpuSample, HostSample, Level,
    MemorySample, Power, Reading, Thermal, Why,
};
use super::{age_ms, Clock, Platform, SensorReport, SensorState};
use serde::{Deserialize, Serialize};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

pub const SNAPSHOT_FILE_NAME: &str = "host-load.v1";
pub const LOCK_FILE_NAME: &str = "host-load.v1.lock";
const MAGIC: [u8; 4] = *b"GPHL";
/// The version this build writes. Version 2 appends the extension block
/// ([`EXTENDED_LEN`]); everything version 1 held is where it was.
pub const VERSION: u32 = 2;
/// The oldest version this build reads.
const MIN_READ_VERSION: u32 = 1;
/// Bytes in a version-1 record; also the length this build needs to read. A
/// record shorter than this is refused; one this long is a version-1 record
/// and reads with the extension's fields unsupported.
pub const RECORD_LEN: usize = 664;
/// Bytes through the version-2 `GPX3` block, kept fixed for installed readers.
const EXT_V2_LEN: usize = 712;
/// Bytes in the record this build writes: version 1's, the `GPX3` block, then
/// the `GPXM` host-metrics trailer (`712..816`).
pub const EXTENDED_LEN: usize = 816;
/// Reads retried while a write is in progress before giving up as torn.
const READ_ATTEMPTS: usize = 64;

const VERSION_FIELD_LEN: usize = 32;
const PATH_FIELD_LEN: usize = 256;

// Offsets. Every field after the header is little-endian.
const OFF_VERSION: usize = 4;
const OFF_LENGTH: usize = 8;
const OFF_FLAGS: usize = 12;
const OFF_SEQ: usize = 16;
const OFF_WALL: usize = 24;
const OFF_MONO: usize = 32;
const OFF_BOOT: usize = 40;
const OFF_WRITER_PID: usize = 48;
const OFF_CADENCE: usize = 52;
const OFF_WRITER_START: usize = 56;
const OFF_WRITER_VERSION: usize = 64;
const OFF_WRITER_PATH: usize = 96;
const OFF_STATUS: usize = 352;
const OFF_PRESSURE: usize = 384;
const OFF_THERMAL: usize = 388;
const OFF_POWER: usize = 389;
const OFF_CORE_LEVELS: usize = 390;
const OFF_CORE_LOGICAL: usize = 392;
const OFF_CORE_P0: usize = 394;
const OFF_CORE_P1: usize = 396;
const OFF_SWAP_USED: usize = 400;
const OFF_SWAP_TOTAL: usize = 408;
const OFF_COMPRESSOR: usize = 416;
const OFF_COMPRESSOR_DELTA: usize = 448;
const OFF_COMPRESSED: usize = 480;
const OFF_MEMSIZE: usize = 488;
const OFF_FREE_DISK: usize = 496;
const OFF_OPEN_FILES: usize = 504;
const OFF_MAX_FILES: usize = 512;
const OFF_LOAD: usize = 520;
const OFF_PROCESSES: usize = 532;
const OFF_UNREADABLE: usize = 536;
const OFF_ORPHANS: usize = 540;
const OFF_CLAUDE: usize = 544;
const OFF_CODEX: usize = 552;
const OFF_LAUNCHED_FP: usize = 560;
const OFF_OBSERVED_FP: usize = 568;
const OFF_LARGEST_FOREIGN: usize = 576;
const OFF_LAUNCHED_WRATE: usize = 584;
const OFF_MAX_TREE_WRATE: usize = 592;
const OFF_MEMGUARD_PID: usize = 600;
const OFF_TICK_LAST: usize = 604;
const OFF_TICKS: usize = 608;
const OFF_OVERRUNS: usize = 616;
const OFF_MISSED: usize = 624;
const OFF_TICK_MAX: usize = 632;
const OFF_SAMPLER_CPU: usize = 640;
const OFF_CHECKSUM: usize = 648;
const OFF_SEQ_TAIL: usize = 656;

// The version-2 extension, appended after the version-1 record. It opens with
// a magic of its own, so a longer record from some other newer writer is not
// mistaken for it, and ends with its own checksum, since the version-1
// checksum stops at `OFF_CHECKSUM`.
const OFF_EXT: usize = RECORD_LEN;
// The block's marker. `GPX2` was the marker of the block before the two disk
// fill rates changed what bytes 694 and 696..704 mean; a branch build that
// reads `GPX2` ignores this block (no extension) instead of misreading it.
const EXT_MAGIC: [u8; 4] = *b"GPX3";
const OFF_X_SEGMENTS_STATUS: usize = 668;
const OFF_X_VERDICT_STATUS: usize = 669;
const OFF_X_TIER: usize = 670;
const OFF_X_BINDING: usize = 671;
const OFF_X_ADMIT: usize = 672;
const OFF_X_SHED_BG: usize = 673;
const OFF_X_SHED_REACTIVE: usize = 674;
const OFF_X_SEG_TOTAL: usize = 676;
const OFF_X_SEG_LIMIT: usize = 684;
// Spare bytes of the version-2 extension, used without a version change: an
// older reader hashes them with the rest of the block and reads nothing from
// them. Zero (what every earlier writer left) means "not published".
//
// The exact binding when the byte at `OFF_X_BINDING` is the nearest one older
// readers know (`Binding::wire`); 0 when that byte is the whole truth.
const OFF_X_BINDING_EXACT: usize = 692;
// How the two disk fill rates read, each: 0 not published (a writer from before
// it), 1 a value, 2 not yet, 3 the disk could not be read, 4 this platform has
// no reader, 5 measurement is switched off.
const OFF_X_FILL_STATUS: usize = 693;
const OFF_X_FILL_SUSTAINED_STATUS: usize = 694;
// MiB of free disk lost per minute, as little endian u32, when the matching
// status is 1: the quick rate, then the sustained one.
const OFF_X_FILL_RATE: usize = 696;
const OFF_X_FILL_SUSTAINED_RATE: usize = 700;
const OFF_X_CHECKSUM: usize = 704;

// The metrics extension (`GPXM`), appended after the 712-byte `GPX3` block
// without changing any offset `gitpulse-hook` or `gitpulse-mcp` reads.
const OFF_EXT_M: usize = EXT_V2_LEN;
const EXT_M_MAGIC: [u8; 4] = *b"GPXM";
const OFF_M_CPU_STATUS: usize = 716;
const OFF_M_GPU_STATUS: usize = 717;
const OFF_M_MEM_STATUS: usize = 718;
const OFF_M_DISK_STATUS: usize = 719;
const OFF_M_THERMAL_STATUS: usize = 720;
const OFF_M_GPU_OPTS: usize = 721;
const OFF_M_MEM_OPTS: usize = 722;
const OFF_M_CPU_TOTAL: usize = 724;
const OFF_M_CPU_USER: usize = 726;
const OFF_M_CPU_SYS: usize = 728;
const OFF_M_GPU_UTIL: usize = 730;
const OFF_M_GPU_RENDERER: usize = 732;
const OFF_M_GPU_TILER: usize = 734;
const OFF_M_THERMAL_TEMP: usize = 736;
const OFF_M_GPU_MEM_IN_USE: usize = 744;
const OFF_M_GPU_MEM_ALLOC: usize = 752;
const OFF_M_MEM_USED: usize = 760;
const OFF_M_MEM_APP: usize = 768;
const OFF_M_MEM_WIRED: usize = 776;
const OFF_M_MEM_COMP: usize = 784;
const OFF_M_MEM_CACHED: usize = 792;
const OFF_M_TOTAL_DISK: usize = 800;
const OFF_M_CHECKSUM: usize = 808;

// One status byte per reading: 0 is a value, else a `Why` code.
const S_PRESSURE: usize = 0;
const S_SWAP_USED: usize = 1;
const S_SWAP_TOTAL: usize = 2;
const S_COMPRESSOR: usize = 3;
const S_COMPRESSED: usize = 4;
const S_LOAD: usize = 5;
const S_CORES: usize = 6;
const S_MEMSIZE: usize = 7;
const S_OPEN_FILES: usize = 8;
const S_MAX_FILES: usize = 9;
const S_FREE_DISK: usize = 10;
const S_THERMAL: usize = 11;
const S_POWER: usize = 12;
const S_COMPRESSOR_DELTA: usize = 13;
const S_PROCESSES: usize = 14;
const S_CLAUDE: usize = 15;
const S_CODEX: usize = 16;
const S_LAUNCHED_FP: usize = 17;
const S_OBSERVED_FP: usize = 18;
const S_LARGEST_FOREIGN: usize = 19;
const S_LAUNCHED_WRATE: usize = 20;
const S_MAX_TREE_WRATE: usize = 21;

// The layout's invariants, checked when this compiles: the tail sequence
// ends the record, the text fields end where the status bytes begin, and
// every status slot fits before the first value.
const _: () = assert!(OFF_SEQ_TAIL + 8 == RECORD_LEN);
const _: () = assert!(OFF_WRITER_VERSION + VERSION_FIELD_LEN == OFF_WRITER_PATH);
const _: () = assert!(OFF_WRITER_PATH + PATH_FIELD_LEN == OFF_STATUS);
const _: () = assert!(S_MAX_TREE_WRATE < OFF_PRESSURE - OFF_STATUS);
const _: () = assert!(OFF_SAMPLER_CPU + 8 == OFF_CHECKSUM);
const _: () = assert!(OFF_X_SEG_LIMIT + 8 <= OFF_X_BINDING_EXACT);
const _: () = assert!(OFF_X_FILL_SUSTAINED_RATE + 4 == OFF_X_CHECKSUM);
const _: () = assert!(OFF_X_CHECKSUM + 8 == EXT_V2_LEN);
const _: () = assert!(OFF_EXT_M == EXT_V2_LEN);
const _: () = assert!(OFF_M_THERMAL_TEMP + 2 <= OFF_M_GPU_MEM_IN_USE);
const _: () = assert!(OFF_M_TOTAL_DISK + 8 == OFF_M_CHECKSUM);
const _: () = assert!(OFF_M_CHECKSUM + 8 == EXTENDED_LEN);

const FLAG_ENABLED: u32 = 1;
const FLAG_PLATFORM_SHIFT: u32 = 1;
const FLAG_PLATFORM_MASK: u32 = 0b11 << FLAG_PLATFORM_SHIFT;
const FLAG_THRASH: u32 = 1 << 3;
const FLAG_MEMGUARD_PRESENT: u32 = 1 << 4;
const FLAG_MEMGUARD_WARN: u32 = 1 << 5;

/// Who wrote a snapshot. Readers and Health show it, so "which GitPulse is
/// sampling" has an answer when a dev build and the installed app coexist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterIdentity {
    pub pid: u32,
    /// When the writer process started, in microseconds since the Unix
    /// epoch; with `pid`, what tells a live writer from a recycled pid.
    pub start_us: u64,
    pub version: String,
    pub path: String,
}

/// The sampler's own cost and punctuality, published so a sampler that falls
/// behind is visible rather than inferred.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickStats {
    pub ticks: u64,
    /// Ticks whose own work took longer than the cadence.
    pub overruns: u64,
    /// Scheduled ticks that never ran because the thread woke too late.
    pub missed: u64,
    pub last_us: u32,
    pub max_us: u32,
    /// CPU time the sampler thread has used, in nanoseconds.
    pub cpu_ns: u64,
}

/// The governor's verdict as the app's sampler held it when it published, so a
/// reader with no history (the hook, `gitpulse-mcp`) can use it without
/// reading settings. It is the hysteresis-held verdict of the tick **before**
/// the one that wrote this record: the sampler publishes, then counts the
/// sample, so the record can only carry what the previous count produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedVerdict {
    pub tier: Tier,
    pub binding: Option<Binding>,
    /// Whether agent launches were admitted.
    pub admit: bool,
    pub shed_background: bool,
    pub shed_reactive: bool,
}

impl PublishedVerdict {
    pub fn of(verdict: &GovernorVerdict) -> Self {
        Self {
            tier: verdict.tier,
            binding: verdict.binding,
            admit: verdict.admit,
            shed_background: verdict.shed.background,
            shed_reactive: verdict.shed.reactive,
        }
    }
}

/// Everything one snapshot carries. The detailed lists (sessions, orphans,
/// footprints) reach Health over IPC from the writer; the snapshot carries the
/// counts and sums any process can read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub length: u32,
    pub seq: u64,
    pub written: Clock,
    /// `host_load.enabled`, carried here so hooks never read settings and a
    /// flip propagates in one tick.
    pub enabled: bool,
    pub platform: Platform,
    pub cadence_ms: u32,
    pub writer: WriterIdentity,
    pub host: HostSample,
    pub compressor_delta: Reading<CompressorDelta>,
    pub compressor_thrash: bool,
    pub processes: Reading<u32>,
    /// Processes of this user the OS would not describe.
    pub unreadable_processes: u32,
    pub claude: Reading<SessionCount>,
    pub codex: Reading<SessionCount>,
    pub launched_footprint: Reading<u64>,
    pub observed_footprint: Reading<u64>,
    pub largest_foreign: Reading<u64>,
    /// Bytes per second written by every GitPulse-launched tree together.
    pub launched_write_rate: Reading<u64>,
    /// The fastest-writing single launched tree.
    pub max_tree_write_rate: Reading<u64>,
    pub memguard_present: bool,
    pub memguard_warn: bool,
    pub memguard_pid: u32,
    pub orphans: u32,
    pub tick: TickStats,
    /// Bytes of free disk lost per minute (`diskrate`), measured by the
    /// writer, which alone holds the history: a reader with none (the hook, a
    /// second GitPulse) still has the rate. Each unavailable reading says why:
    /// `Older` in a record from before it, `Disabled` while the sensor is
    /// switched off, `Unsupported` on a platform with no disk reader, `Failed`
    /// for a disk that cannot be read, `NotYet` while history builds. This is
    /// the quick rate, which Elevated decides on.
    pub disk_fill_rate: Reading<u64>,
    /// The sustained rate, which Severe decides on (`diskrate`): the one a
    /// single burst cannot fake and a pause cannot hide.
    pub disk_fill_sustained: Reading<u64>,
    /// The app's held governor verdict (see [`PublishedVerdict`]). Pending on
    /// a writer's first tick, unsupported in a version-1 record, and disabled
    /// while the sensor is switched off.
    pub verdict: Reading<PublishedVerdict>,
}

impl Snapshot {
    /// A record carrying no readings: what a writer publishes while the kill
    /// switch is off, so readers can tell "switched off" from "no sampler".
    pub fn empty(written: Clock, writer: WriterIdentity, enabled: bool) -> Self {
        let why = if enabled {
            Why::Unsupported
        } else {
            Why::Disabled
        };
        Self {
            version: VERSION,
            length: EXTENDED_LEN as u32,
            seq: 0,
            written,
            enabled,
            platform: Platform::current(),
            cadence_ms: super::CADENCE_MS as u32,
            writer,
            host: HostSample::unavailable(why),
            compressor_delta: Reading::Unavailable(why),
            compressor_thrash: false,
            processes: Reading::Unavailable(why),
            unreadable_processes: 0,
            claude: Reading::Unavailable(why),
            codex: Reading::Unavailable(why),
            launched_footprint: Reading::Unavailable(why),
            observed_footprint: Reading::Unavailable(why),
            largest_foreign: Reading::Unavailable(why),
            launched_write_rate: Reading::Unavailable(why),
            max_tree_write_rate: Reading::Unavailable(why),
            memguard_present: false,
            memguard_warn: false,
            memguard_pid: 0,
            orphans: 0,
            tick: TickStats::default(),
            disk_fill_rate: Reading::Unavailable(why),
            disk_fill_sustained: Reading::Unavailable(why),
            verdict: Reading::Unavailable(why),
        }
    }
}

/// Why a snapshot could not be read. Each is its own variant so a consumer
/// can tell "no sampler yet" from "a newer GitPulse wrote something this
/// build cannot read".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// No record exists: no sampler has ever written one here.
    Missing,
    /// The record exists but is not one this user may trust (a symlink,
    /// another owner, open to other users).
    Unsafe(String),
    Io(String),
    BadMagic,
    UnsupportedVersion {
        found: u32,
        min: u32,
    },
    /// The record, or its declared length, is shorter than every field this
    /// build reads.
    Short {
        length: usize,
        need: usize,
    },
    /// Every attempt overlapped a write.
    Torn,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no sampler has written a snapshot"),
            Self::Unsafe(why) => write!(f, "the snapshot is not safe to read: {why}"),
            Self::Io(why) => write!(f, "the snapshot could not be read: {why}"),
            Self::BadMagic => write!(f, "the snapshot is not a host-load record"),
            Self::UnsupportedVersion { found, min } => write!(
                f,
                "the snapshot is version {found}; this build reads version {min} and later"
            ),
            Self::Short { length, need } => write!(
                f,
                "the snapshot holds {length} bytes; this build needs {need}"
            ),
            Self::Torn => write!(f, "every read of the snapshot overlapped a write"),
        }
    }
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_i16(buf: &mut [u8], at: usize, v: i16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}
fn get_i16(buf: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([buf[at], buf[at + 1]])
}
fn put_pct(buf: &mut [u8], at: usize, pct: f64) {
    let tenths = (pct.max(0.0) * 10.0).round().min(f64::from(u16::MAX)) as u16;
    put_u16(buf, at, tenths);
}
fn get_pct(buf: &[u8], at: usize) -> f64 {
    f64::from(get_u16(buf, at)) / 10.0
}
fn get_u32(buf: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[at..at + 4]);
    u32::from_le_bytes(b)
}
fn get_u64(buf: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(b)
}

/// Writes `text` NUL-padded into a fixed field, cut at a character boundary.
fn put_text(buf: &mut [u8], at: usize, len: usize, text: &str) {
    let mut end = text.len().min(len - 1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    buf[at..at + len].fill(0);
    buf[at..at + end].copy_from_slice(&text.as_bytes()[..end]);
}
fn get_text(buf: &[u8], at: usize, len: usize) -> String {
    let field = &buf[at..at + len];
    let end = field.iter().position(|b| *b == 0).unwrap_or(len);
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Encodes a reading's status byte and, for a value, its bytes.
fn put_reading<T: Copy>(
    buf: &mut [u8],
    slot: usize,
    reading: Reading<T>,
    write: impl FnOnce(&mut [u8], T),
) {
    match reading {
        Reading::Value(v) => {
            buf[OFF_STATUS + slot] = 0;
            write(buf, v);
        }
        Reading::Unavailable(why) => buf[OFF_STATUS + slot] = why.code(),
    }
}

fn get_reading<T>(buf: &[u8], slot: usize, read: impl FnOnce(&[u8]) -> Option<T>) -> Reading<T> {
    match buf[OFF_STATUS + slot] {
        // A value the decoder cannot interpret is a failed reading, not a
        // default one.
        0 => read(buf).map_or(Reading::Unavailable(Why::Failed), Reading::Value),
        code => Reading::Unavailable(Why::from_code(code).unwrap_or(Why::Failed)),
    }
}

/// The record for `snap`, with `seq` in both the head and the tail.
pub(super) fn encode(snap: &Snapshot, seq: u64) -> [u8; EXTENDED_LEN] {
    let mut b = [0u8; EXTENDED_LEN];
    encode_v1_fields(&mut b, snap, seq);
    b[0..4].copy_from_slice(&MAGIC);
    put_u32(&mut b, OFF_VERSION, VERSION);
    put_u32(&mut b, OFF_LENGTH, EXTENDED_LEN as u32);
    encode_extension(&mut b, snap, seq);
    encode_metrics_extension(&mut b, snap, seq);
    let sum = checksum(&b);
    put_u64(&mut b, OFF_CHECKSUM, sum);
    b
}

/// The record a version-1 writer (an older build) wrote: no extension, the
/// version and length of that record. For the compatibility tests.
#[cfg(test)]
pub(super) fn encode_v1(snap: &Snapshot, seq: u64) -> [u8; RECORD_LEN] {
    let mut b = [0u8; EXTENDED_LEN];
    encode_v1_fields(&mut b, snap, seq);
    b[0..4].copy_from_slice(&MAGIC);
    put_u32(&mut b, OFF_VERSION, 1);
    put_u32(&mut b, OFF_LENGTH, RECORD_LEN as u32);
    let sum = checksum(&b);
    put_u64(&mut b, OFF_CHECKSUM, sum);
    let mut out = [0u8; RECORD_LEN];
    out.copy_from_slice(&b[..RECORD_LEN]);
    out
}

/// The extension block: the compressor's segment use and the published
/// verdict, with a checksum of its own that also covers the sequence, so a
/// body mixed from two writes cannot match.
fn encode_extension(b: &mut [u8], snap: &Snapshot, seq: u64) {
    b[OFF_EXT..OFF_EXT + 4].copy_from_slice(&EXT_MAGIC);
    match snap.host.compressor_segments {
        Reading::Value(s) => {
            b[OFF_X_SEGMENTS_STATUS] = 0;
            put_u64(b, OFF_X_SEG_TOTAL, s.total);
            put_u64(b, OFF_X_SEG_LIMIT, s.limit);
        }
        Reading::Unavailable(why) => b[OFF_X_SEGMENTS_STATUS] = why.code(),
    }
    match snap.verdict {
        Reading::Value(v) => {
            b[OFF_X_VERDICT_STATUS] = 0;
            b[OFF_X_TIER] = v.tier.code();
            // Older readers refuse a code they do not know and so read no
            // verdict at all; they get the nearest binding they do know, and
            // this build reads the exact one beside it.
            let (base, exact) = Binding::wire_codes(v.binding);
            b[OFF_X_BINDING] = base;
            b[OFF_X_BINDING_EXACT] = exact;
            b[OFF_X_ADMIT] = u8::from(v.admit);
            b[OFF_X_SHED_BG] = u8::from(v.shed_background);
            b[OFF_X_SHED_REACTIVE] = u8::from(v.shed_reactive);
        }
        Reading::Unavailable(why) => b[OFF_X_VERDICT_STATUS] = why.code(),
    }
    put_fill(b, OFF_X_FILL_STATUS, OFF_X_FILL_RATE, snap.disk_fill_rate);
    put_fill(
        b,
        OFF_X_FILL_SUSTAINED_STATUS,
        OFF_X_FILL_SUSTAINED_RATE,
        snap.disk_fill_sustained,
    );
    let sum = ext_checksum(b, seq);
    put_u64(b, OFF_X_CHECKSUM, sum);
}

/// One fill rate: its status byte and, for a value, MiB a minute (the most
/// the field holds is 4 PiB a minute).
fn put_fill(b: &mut [u8], status_at: usize, rate_at: usize, reading: Reading<u64>) {
    match reading {
        Reading::Value(rate) => {
            b[status_at] = 1;
            let mib = u32::try_from(rate >> 20).unwrap_or(u32::MAX);
            b[rate_at..rate_at + 4].copy_from_slice(&mib.to_le_bytes());
        }
        Reading::Unavailable(Why::NotYet) => b[status_at] = 2,
        Reading::Unavailable(Why::Unreadable | Why::Failed) => b[status_at] = 3,
        Reading::Unavailable(Why::Unsupported) => b[status_at] = 4,
        Reading::Unavailable(Why::Disabled) => b[status_at] = 5,
        // A writer never says "older"; zero is what an older writer left.
        Reading::Unavailable(Why::Older) => b[status_at] = 0,
    }
}

/// The reading [`put_fill`] wrote.
fn get_fill(b: &[u8], status_at: usize, rate_at: usize) -> Reading<u64> {
    match b[status_at] {
        1 => Reading::Value(
            u64::from(u32::from_le_bytes([
                b[rate_at],
                b[rate_at + 1],
                b[rate_at + 2],
                b[rate_at + 3],
            ])) << 20,
        ),
        2 => Reading::Unavailable(Why::NotYet),
        3 => Reading::Unavailable(Why::Failed),
        4 => Reading::Unavailable(Why::Unsupported),
        5 => Reading::Unavailable(Why::Disabled),
        _ => Reading::Unavailable(Why::Older),
    }
}

fn put_m_reading<T>(
    b: &mut [u8],
    status_at: usize,
    reading: Reading<T>,
    write: impl FnOnce(&mut [u8], T),
) {
    match reading {
        Reading::Value(v) => {
            b[status_at] = 0;
            write(b, v);
        }
        Reading::Unavailable(why) => b[status_at] = why.code(),
    }
}

/// Appends the metrics extension (`GPXM`, bytes 712..816) without altering any
/// version-1 or `GPX3` offset read by older hook/MCP binaries.
fn encode_metrics_extension(b: &mut [u8], snap: &Snapshot, seq: u64) {
    b[OFF_EXT_M..OFF_EXT_M + 4].copy_from_slice(&EXT_M_MAGIC);
    let h = &snap.host;
    let valid_pct = |v: f64| v.is_finite() && v >= 0.0;
    let cpu_usage = match h.cpu_usage {
        Reading::Value(c)
            if !valid_pct(c.total_pct) || !valid_pct(c.user_pct) || !valid_pct(c.system_pct) =>
        {
            Reading::Unavailable(Why::Failed)
        }
        other => other,
    };
    put_m_reading(b, OFF_M_CPU_STATUS, cpu_usage, |b, c| {
        put_pct(b, OFF_M_CPU_TOTAL, c.total_pct);
        put_pct(b, OFF_M_CPU_USER, c.user_pct);
        put_pct(b, OFF_M_CPU_SYS, c.system_pct);
    });
    let gpu = match h.gpu {
        Reading::Value(g)
            if !valid_pct(g.utilization_pct)
                || g.renderer_pct.is_some_and(|v| !valid_pct(v))
                || g.tiler_pct.is_some_and(|v| !valid_pct(v)) =>
        {
            Reading::Unavailable(Why::Failed)
        }
        other => other,
    };
    put_m_reading(b, OFF_M_GPU_STATUS, gpu, |b, g| {
        put_pct(b, OFF_M_GPU_UTIL, g.utilization_pct);
        let mut opts = 0u8;
        if let Some(r) = g.renderer_pct {
            opts |= 1 << 0;
            put_pct(b, OFF_M_GPU_RENDERER, r);
        }
        if let Some(t) = g.tiler_pct {
            opts |= 1 << 1;
            put_pct(b, OFF_M_GPU_TILER, t);
        }
        if let Some(v) = g.memory_in_use {
            opts |= 1 << 2;
            put_u64(b, OFF_M_GPU_MEM_IN_USE, v);
        }
        if let Some(v) = g.memory_allocated {
            opts |= 1 << 3;
            put_u64(b, OFF_M_GPU_MEM_ALLOC, v);
        }
        b[OFF_M_GPU_OPTS] = opts;
    });
    put_m_reading(b, OFF_M_MEM_STATUS, h.memory, |b, m| {
        put_u64(b, OFF_M_MEM_USED, m.used_bytes);
        let mut opts = 0u8;
        if let Some(v) = m.app_bytes {
            opts |= 1 << 0;
            put_u64(b, OFF_M_MEM_APP, v);
        }
        if let Some(v) = m.wired_bytes {
            opts |= 1 << 1;
            put_u64(b, OFF_M_MEM_WIRED, v);
        }
        if let Some(v) = m.compressed_bytes {
            opts |= 1 << 2;
            put_u64(b, OFF_M_MEM_COMP, v);
        }
        if let Some(v) = m.cached_bytes {
            opts |= 1 << 3;
            put_u64(b, OFF_M_MEM_CACHED, v);
        }
        b[OFF_M_MEM_OPTS] = opts;
    });
    put_m_reading(b, OFF_M_DISK_STATUS, h.total_disk, |b, v| {
        put_u64(b, OFF_M_TOTAL_DISK, v)
    });
    let thermal_temp = match h.thermal_temp_c {
        Reading::Value(t) if !t.is_finite() => Reading::Unavailable(Why::Failed),
        other => other,
    };
    put_m_reading(b, OFF_M_THERMAL_STATUS, thermal_temp, |b, t| {
        let tenths = (t * 10.0)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        put_i16(b, OFF_M_THERMAL_TEMP, tenths);
    });
    let sum = metrics_checksum(b, seq);
    put_u64(b, OFF_M_CHECKSUM, sum);
}

fn fnv1a_seq(slice: &[u8], seq: u64) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in slice {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ seq
}

/// FNV-1a over the extension's bytes, with the sequence mixed in.
fn ext_checksum(record: &[u8], seq: u64) -> u64 {
    fnv1a_seq(&record[OFF_EXT..OFF_X_CHECKSUM], seq)
}

/// FNV-1a over the `GPXM` metrics block, with the sequence mixed in.
fn metrics_checksum(record: &[u8], seq: u64) -> u64 {
    fnv1a_seq(&record[OFF_EXT_M..OFF_M_CHECKSUM], seq)
}

/// Everything version 1 held, written where version 1 wrote it. The caller
/// stamps the magic, version, length and checksum, which depend on which
/// version is being written.
fn encode_v1_fields(b: &mut [u8], snap: &Snapshot, seq: u64) {
    let mut flags = (snap.platform.code() << FLAG_PLATFORM_SHIFT) & FLAG_PLATFORM_MASK;
    if snap.enabled {
        flags |= FLAG_ENABLED;
    }
    if snap.compressor_thrash {
        flags |= FLAG_THRASH;
    }
    if snap.memguard_present {
        flags |= FLAG_MEMGUARD_PRESENT;
    }
    if snap.memguard_warn {
        flags |= FLAG_MEMGUARD_WARN;
    }
    put_u32(b, OFF_FLAGS, flags);
    put_u64(b, OFF_SEQ, seq);
    put_u64(b, OFF_WALL, snap.written.wall_ms);
    put_u64(b, OFF_MONO, snap.written.mono_ns);
    put_u64(b, OFF_BOOT, snap.written.boot_id);
    put_u32(b, OFF_WRITER_PID, snap.writer.pid);
    put_u32(b, OFF_CADENCE, snap.cadence_ms);
    put_u64(b, OFF_WRITER_START, snap.writer.start_us);
    put_text(
        b,
        OFF_WRITER_VERSION,
        VERSION_FIELD_LEN,
        &snap.writer.version,
    );
    put_text(b, OFF_WRITER_PATH, PATH_FIELD_LEN, &snap.writer.path);

    let h = &snap.host;
    put_reading(b, S_PRESSURE, h.pressure, |b, v| {
        put_u32(b, OFF_PRESSURE, v.kernel())
    });
    put_reading(b, S_SWAP_USED, h.swap_used, |b, v| {
        put_u64(b, OFF_SWAP_USED, v)
    });
    put_reading(b, S_SWAP_TOTAL, h.swap_total, |b, v| {
        put_u64(b, OFF_SWAP_TOTAL, v)
    });
    put_reading(b, S_COMPRESSOR, h.compressor, |b, c| {
        put_u64(b, OFF_COMPRESSOR, c.thrashing_detected);
        put_u64(b, OFF_COMPRESSOR + 8, c.free_below_reserved);
        put_u64(b, OFF_COMPRESSOR + 16, c.swapouts_pressure);
        put_u64(b, OFF_COMPRESSOR + 24, c.major_compaction_segments_freed);
    });
    put_reading(b, S_COMPRESSOR_DELTA, snap.compressor_delta, |b, d| {
        put_u64(b, OFF_COMPRESSOR_DELTA, d.thrashing_detected);
        put_u64(b, OFF_COMPRESSOR_DELTA + 8, d.free_below_reserved);
        put_u64(b, OFF_COMPRESSOR_DELTA + 16, d.swapouts_pressure);
        put_u64(
            b,
            OFF_COMPRESSOR_DELTA + 24,
            d.major_compaction_segments_freed,
        );
    });
    put_reading(b, S_COMPRESSED, h.compressed_bytes, |b, v| {
        put_u64(b, OFF_COMPRESSED, v)
    });
    let load = match h.load {
        Reading::Value(l) if l.iter().any(|v| !v.is_finite() || *v < 0.0) => {
            Reading::Unavailable(Why::Failed)
        }
        other => other,
    };
    put_reading(b, S_LOAD, load, |b, l| {
        for (i, v) in l.iter().enumerate() {
            // Milli-load: a load of 4.2 B is far past anything real.
            let milli = (v * 1000.0).round().min(f64::from(u32::MAX)) as u32;
            put_u32(b, OFF_LOAD + 4 * i, milli);
        }
    });
    put_reading(b, S_CORES, h.cores, |b, c| {
        b[OFF_CORE_LEVELS] = c.levels;
        put_u16(b, OFF_CORE_LOGICAL, c.logical);
        put_u16(b, OFF_CORE_P0, c.perf0);
        put_u16(b, OFF_CORE_P1, c.perf1);
    });
    put_reading(b, S_MEMSIZE, h.memsize, |b, v| put_u64(b, OFF_MEMSIZE, v));
    put_reading(b, S_OPEN_FILES, h.open_files, |b, v| {
        put_u64(b, OFF_OPEN_FILES, v)
    });
    put_reading(b, S_MAX_FILES, h.max_files, |b, v| {
        put_u64(b, OFF_MAX_FILES, v)
    });
    put_reading(b, S_FREE_DISK, h.free_disk, |b, v| {
        put_u64(b, OFF_FREE_DISK, v)
    });
    put_reading(b, S_THERMAL, h.thermal, |b, t| b[OFF_THERMAL] = t.code());
    put_reading(b, S_POWER, h.power, |b, p| {
        b[OFF_POWER] = match p {
            Power::Ac => 0,
            Power::Battery => 1,
        }
    });
    put_reading(b, S_PROCESSES, snap.processes, |b, v| {
        put_u32(b, OFF_PROCESSES, v)
    });
    put_u32(b, OFF_UNREADABLE, snap.unreadable_processes);
    put_u32(b, OFF_ORPHANS, snap.orphans);
    put_reading(b, S_CLAUDE, snap.claude, |b, c| {
        put_u32(b, OFF_CLAUDE, c.launched);
        put_u32(b, OFF_CLAUDE + 4, c.observed);
    });
    put_reading(b, S_CODEX, snap.codex, |b, c| {
        put_u32(b, OFF_CODEX, c.launched);
        put_u32(b, OFF_CODEX + 4, c.observed);
    });
    put_reading(b, S_LAUNCHED_FP, snap.launched_footprint, |b, v| {
        put_u64(b, OFF_LAUNCHED_FP, v)
    });
    put_reading(b, S_OBSERVED_FP, snap.observed_footprint, |b, v| {
        put_u64(b, OFF_OBSERVED_FP, v)
    });
    put_reading(b, S_LARGEST_FOREIGN, snap.largest_foreign, |b, v| {
        put_u64(b, OFF_LARGEST_FOREIGN, v)
    });
    put_reading(b, S_LAUNCHED_WRATE, snap.launched_write_rate, |b, v| {
        put_u64(b, OFF_LAUNCHED_WRATE, v)
    });
    put_reading(b, S_MAX_TREE_WRATE, snap.max_tree_write_rate, |b, v| {
        put_u64(b, OFF_MAX_TREE_WRATE, v)
    });
    put_u32(b, OFF_MEMGUARD_PID, snap.memguard_pid);
    put_u32(b, OFF_TICK_LAST, snap.tick.last_us);
    put_u64(b, OFF_TICKS, snap.tick.ticks);
    put_u64(b, OFF_OVERRUNS, snap.tick.overruns);
    put_u64(b, OFF_MISSED, snap.tick.missed);
    put_u32(b, OFF_TICK_MAX, snap.tick.max_us);
    put_u64(b, OFF_SAMPLER_CPU, snap.tick.cpu_ns);
    put_u64(b, OFF_SEQ_TAIL, seq);
}

/// FNV-1a over the version-1 fields, with the head sequence included: a
/// record whose head and body come from different writes cannot match.
fn checksum(record: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in &record[..OFF_CHECKSUM] {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ get_u64(record, OFF_SEQ_TAIL)
}

/// Decodes a record as a reader whose minimum version is `min_version` and
/// who knows `known_len` bytes of fields. Production readers pass
/// [`MIN_READ_VERSION`] and [`RECORD_LEN`]; the compatibility tests pass the
/// numbers an older or a newer build would.
fn decode_as(buf: &[u8], min_version: u32, known_len: usize) -> Result<Snapshot, SnapshotError> {
    if buf.len() < OFF_LENGTH + 4 {
        return Err(SnapshotError::Short {
            length: buf.len(),
            need: known_len,
        });
    }
    if buf[0..4] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let version = get_u32(buf, OFF_VERSION);
    if version < min_version {
        return Err(SnapshotError::UnsupportedVersion {
            found: version,
            min: min_version,
        });
    }
    let length = get_u32(buf, OFF_LENGTH) as usize;
    if length < known_len || buf.len() < known_len {
        return Err(SnapshotError::Short {
            length: length.min(buf.len()),
            need: known_len,
        });
    }
    let seq = get_u64(buf, OFF_SEQ);
    if seq % 2 == 1
        || get_u64(buf, OFF_SEQ_TAIL) != seq
        || get_u64(buf, OFF_CHECKSUM) != checksum(buf)
    {
        return Err(SnapshotError::Torn);
    }
    let (segments, verdict, disk_fill_rate, disk_fill_sustained) =
        decode_extension(buf, version, length, seq)?;
    let (cpu_usage, gpu, memory, total_disk, thermal_temp_c) =
        decode_metrics_extension(buf, version, length, seq)?;
    let flags = get_u32(buf, OFF_FLAGS);
    let platform = Platform::from_code((flags & FLAG_PLATFORM_MASK) >> FLAG_PLATFORM_SHIFT)
        .unwrap_or(Platform::Unavailable);
    let host = HostSample {
        pressure: get_reading(buf, S_PRESSURE, |b| {
            Level::from_kernel(get_u32(b, OFF_PRESSURE))
        }),
        swap_used: get_reading(buf, S_SWAP_USED, |b| Some(get_u64(b, OFF_SWAP_USED))),
        swap_total: get_reading(buf, S_SWAP_TOTAL, |b| Some(get_u64(b, OFF_SWAP_TOTAL))),
        compressor: get_reading(buf, S_COMPRESSOR, |b| {
            Some(Compressor {
                thrashing_detected: get_u64(b, OFF_COMPRESSOR),
                free_below_reserved: get_u64(b, OFF_COMPRESSOR + 8),
                swapouts_pressure: get_u64(b, OFF_COMPRESSOR + 16),
                major_compaction_segments_freed: get_u64(b, OFF_COMPRESSOR + 24),
            })
        }),
        compressed_bytes: get_reading(buf, S_COMPRESSED, |b| Some(get_u64(b, OFF_COMPRESSED))),
        compressor_segments: segments,
        load: get_reading(buf, S_LOAD, |b| {
            let at = |i: usize| f64::from(get_u32(b, OFF_LOAD + 4 * i)) / 1000.0;
            Some([at(0), at(1), at(2)])
        }),
        cores: get_reading(buf, S_CORES, |b| {
            Some(Cores {
                levels: b[OFF_CORE_LEVELS],
                logical: get_u16(b, OFF_CORE_LOGICAL),
                perf0: get_u16(b, OFF_CORE_P0),
                perf1: get_u16(b, OFF_CORE_P1),
            })
        }),
        memsize: get_reading(buf, S_MEMSIZE, |b| Some(get_u64(b, OFF_MEMSIZE))),
        open_files: get_reading(buf, S_OPEN_FILES, |b| Some(get_u64(b, OFF_OPEN_FILES))),
        max_files: get_reading(buf, S_MAX_FILES, |b| Some(get_u64(b, OFF_MAX_FILES))),
        free_disk: get_reading(buf, S_FREE_DISK, |b| Some(get_u64(b, OFF_FREE_DISK))),
        thermal: get_reading(buf, S_THERMAL, |b| Thermal::from_code(b[OFF_THERMAL])),
        power: get_reading(buf, S_POWER, |b| match b[OFF_POWER] {
            0 => Some(Power::Ac),
            1 => Some(Power::Battery),
            _ => None,
        }),
        cpu_usage,
        gpu,
        memory,
        total_disk,
        thermal_temp_c,
    };
    let count = |b: &[u8], at: usize| {
        Some(SessionCount {
            launched: get_u32(b, at),
            observed: get_u32(b, at + 4),
        })
    };
    Ok(Snapshot {
        version,
        length: length as u32,
        seq,
        written: Clock {
            wall_ms: get_u64(buf, OFF_WALL),
            mono_ns: get_u64(buf, OFF_MONO),
            boot_id: get_u64(buf, OFF_BOOT),
        },
        enabled: flags & FLAG_ENABLED != 0,
        platform,
        cadence_ms: get_u32(buf, OFF_CADENCE),
        writer: WriterIdentity {
            pid: get_u32(buf, OFF_WRITER_PID),
            start_us: get_u64(buf, OFF_WRITER_START),
            version: get_text(buf, OFF_WRITER_VERSION, VERSION_FIELD_LEN),
            path: get_text(buf, OFF_WRITER_PATH, PATH_FIELD_LEN),
        },
        host,
        compressor_delta: get_reading(buf, S_COMPRESSOR_DELTA, |b| {
            Some(CompressorDelta {
                thrashing_detected: get_u64(b, OFF_COMPRESSOR_DELTA),
                free_below_reserved: get_u64(b, OFF_COMPRESSOR_DELTA + 8),
                swapouts_pressure: get_u64(b, OFF_COMPRESSOR_DELTA + 16),
                major_compaction_segments_freed: get_u64(b, OFF_COMPRESSOR_DELTA + 24),
            })
        }),
        compressor_thrash: flags & FLAG_THRASH != 0,
        processes: get_reading(buf, S_PROCESSES, |b| Some(get_u32(b, OFF_PROCESSES))),
        unreadable_processes: get_u32(buf, OFF_UNREADABLE),
        claude: get_reading(buf, S_CLAUDE, |b| count(b, OFF_CLAUDE)),
        codex: get_reading(buf, S_CODEX, |b| count(b, OFF_CODEX)),
        launched_footprint: get_reading(buf, S_LAUNCHED_FP, |b| Some(get_u64(b, OFF_LAUNCHED_FP))),
        observed_footprint: get_reading(buf, S_OBSERVED_FP, |b| Some(get_u64(b, OFF_OBSERVED_FP))),
        largest_foreign: get_reading(buf, S_LARGEST_FOREIGN, |b| {
            Some(get_u64(b, OFF_LARGEST_FOREIGN))
        }),
        launched_write_rate: get_reading(buf, S_LAUNCHED_WRATE, |b| {
            Some(get_u64(b, OFF_LAUNCHED_WRATE))
        }),
        max_tree_write_rate: get_reading(buf, S_MAX_TREE_WRATE, |b| {
            Some(get_u64(b, OFF_MAX_TREE_WRATE))
        }),
        memguard_present: flags & FLAG_MEMGUARD_PRESENT != 0,
        memguard_warn: flags & FLAG_MEMGUARD_WARN != 0,
        memguard_pid: get_u32(buf, OFF_MEMGUARD_PID),
        orphans: get_u32(buf, OFF_ORPHANS),
        tick: TickStats {
            ticks: get_u64(buf, OFF_TICKS),
            overruns: get_u64(buf, OFF_OVERRUNS),
            missed: get_u64(buf, OFF_MISSED),
            last_us: get_u32(buf, OFF_TICK_LAST),
            max_us: get_u32(buf, OFF_TICK_MAX),
            cpu_ns: get_u64(buf, OFF_SAMPLER_CPU),
        },
        disk_fill_rate,
        disk_fill_sustained,
        verdict,
    })
}

/// A reading's status byte and, for a value, how to decode it. A value this
/// build cannot interpret is a failed reading, not a default one.
fn ext_reading<T>(status: u8, read: impl FnOnce() -> Option<T>) -> Reading<T> {
    match status {
        0 => read().map_or(Reading::Unavailable(Why::Failed), Reading::Value),
        code => Reading::Unavailable(Why::from_code(code).unwrap_or(Why::Failed)),
    }
}

/// What the extension block carries: compressor segments, the held verdict and
/// the two disk fill rates.
type Extension = (
    Reading<CompressorSegments>,
    Reading<PublishedVerdict>,
    Reading<u64>,
    Reading<u64>,
);

/// The extension's readings, or unsupported for both when the record has no
/// extension: a version-1 record (an older writer), or a longer one from some
/// other newer writer that does not carry this build's block. A block that is
/// there and fails its checksum is torn, never decoded.
fn decode_extension(
    buf: &[u8],
    version: u32,
    length: usize,
    seq: u64,
) -> Result<Extension, SnapshotError> {
    if version < 2
        || length < EXT_V2_LEN
        || buf.len() < EXT_V2_LEN
        || buf[OFF_EXT..OFF_EXT + 4] != EXT_MAGIC
    {
        let older = Reading::Unavailable(Why::Older);
        return Ok((Reading::default(), Reading::default(), older, older));
    }
    if get_u64(buf, OFF_X_CHECKSUM) != ext_checksum(buf, seq) {
        return Err(SnapshotError::Torn);
    }
    let segments = ext_reading(buf[OFF_X_SEGMENTS_STATUS], || {
        Some(CompressorSegments {
            total: get_u64(buf, OFF_X_SEG_TOTAL),
            limit: get_u64(buf, OFF_X_SEG_LIMIT),
        })
    });
    let verdict = ext_reading(buf[OFF_X_VERDICT_STATUS], || {
        let flag = |at: usize| match buf[at] {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        Some(PublishedVerdict {
            tier: Tier::of_code(buf[OFF_X_TIER])?,
            binding: Binding::of_wire(buf[OFF_X_BINDING], buf[OFF_X_BINDING_EXACT])?,
            admit: flag(OFF_X_ADMIT)?,
            shed_background: flag(OFF_X_SHED_BG)?,
            shed_reactive: flag(OFF_X_SHED_REACTIVE)?,
        })
    });
    let fill = get_fill(buf, OFF_X_FILL_STATUS, OFF_X_FILL_RATE);
    let sustained = get_fill(buf, OFF_X_FILL_SUSTAINED_STATUS, OFF_X_FILL_SUSTAINED_RATE);
    Ok((segments, verdict, fill, sustained))
}

type MetricsExtension = (
    Reading<CpuUsage>,
    Reading<GpuSample>,
    Reading<MemorySample>,
    Reading<u64>,
    Reading<f64>,
);

/// Decodes the `GPXM` metrics block (`712..816`). Older 712-byte `GPX3`
/// records return [`Why::Older`] for these fields so readers know an older
/// writer published the record.
fn decode_metrics_extension(
    buf: &[u8],
    version: u32,
    length: usize,
    seq: u64,
) -> Result<MetricsExtension, SnapshotError> {
    if version < 2
        || length < EXTENDED_LEN
        || buf.len() < EXTENDED_LEN
        || buf[OFF_EXT_M..OFF_EXT_M + 4] != EXT_M_MAGIC
    {
        return Ok((
            Reading::Unavailable(Why::Older),
            Reading::Unavailable(Why::Older),
            Reading::Unavailable(Why::Older),
            Reading::Unavailable(Why::Older),
            Reading::Unavailable(Why::Older),
        ));
    }
    if get_u64(buf, OFF_M_CHECKSUM) != metrics_checksum(buf, seq) {
        return Err(SnapshotError::Torn);
    }
    let cpu_usage = ext_reading(buf[OFF_M_CPU_STATUS], || {
        Some(CpuUsage {
            total_pct: get_pct(buf, OFF_M_CPU_TOTAL),
            user_pct: get_pct(buf, OFF_M_CPU_USER),
            system_pct: get_pct(buf, OFF_M_CPU_SYS),
        })
    });
    let gpu = ext_reading(buf[OFF_M_GPU_STATUS], || {
        let opts = buf[OFF_M_GPU_OPTS];
        Some(GpuSample {
            utilization_pct: get_pct(buf, OFF_M_GPU_UTIL),
            renderer_pct: (opts & (1 << 0) != 0).then(|| get_pct(buf, OFF_M_GPU_RENDERER)),
            tiler_pct: (opts & (1 << 1) != 0).then(|| get_pct(buf, OFF_M_GPU_TILER)),
            memory_in_use: (opts & (1 << 2) != 0).then(|| get_u64(buf, OFF_M_GPU_MEM_IN_USE)),
            memory_allocated: (opts & (1 << 3) != 0).then(|| get_u64(buf, OFF_M_GPU_MEM_ALLOC)),
        })
    });
    let memory = ext_reading(buf[OFF_M_MEM_STATUS], || {
        let opts = buf[OFF_M_MEM_OPTS];
        Some(MemorySample {
            used_bytes: get_u64(buf, OFF_M_MEM_USED),
            app_bytes: (opts & (1 << 0) != 0).then(|| get_u64(buf, OFF_M_MEM_APP)),
            wired_bytes: (opts & (1 << 1) != 0).then(|| get_u64(buf, OFF_M_MEM_WIRED)),
            compressed_bytes: (opts & (1 << 2) != 0).then(|| get_u64(buf, OFF_M_MEM_COMP)),
            cached_bytes: (opts & (1 << 3) != 0).then(|| get_u64(buf, OFF_M_MEM_CACHED)),
        })
    });
    let total_disk = ext_reading(buf[OFF_M_DISK_STATUS], || {
        Some(get_u64(buf, OFF_M_TOTAL_DISK))
    });
    let thermal_temp_c = ext_reading(buf[OFF_M_THERMAL_STATUS], || {
        Some(f64::from(get_i16(buf, OFF_M_THERMAL_TEMP)) / 10.0)
    });
    Ok((cpu_usage, gpu, memory, total_disk, thermal_temp_c))
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, at)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, at)
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], at: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, at)
}

#[cfg(windows)]
fn write_at(file: &File, mut buf: &[u8], mut at: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, at)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        at += n as u64;
    }
    Ok(())
}

/// Reads as much of the record as exists, up to `cap` bytes.
fn read_record(file: &File, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<()> {
    buf.resize(cap, 0);
    let mut filled = 0;
    while filled < cap {
        let n = read_at(file, &mut buf[filled..], filled as u64)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(())
}

/// Reads and decodes the snapshot at `path` under the sequence lock.
pub fn read(path: &Path) -> Result<Snapshot, SnapshotError> {
    read_as(path, MIN_READ_VERSION, RECORD_LEN)
}

fn read_as(path: &Path, min_version: u32, known_len: usize) -> Result<Snapshot, SnapshotError> {
    let file = match crate::fs_entry::open_private_record_readonly(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Err(SnapshotError::Missing),
        Err(why) => return Err(SnapshotError::Unsafe(why)),
    };
    // A newer writer's record may be longer; read what this build knows plus
    // room to see that it is longer, never an unbounded file.
    let cap = known_len.max(EXTENDED_LEN) * 2;
    let mut buf = Vec::with_capacity(cap);
    let mut head = [0u8; 8];
    for _ in 0..READ_ATTEMPTS {
        read_record(&file, &mut buf, cap).map_err(|e| SnapshotError::Io(e.to_string()))?;
        if buf.is_empty() || buf.iter().take(OFF_SEQ).all(|b| *b == 0) {
            // Created by an elected writer that has not published yet.
            return Err(SnapshotError::Missing);
        }
        let decoded = decode_as(&buf, min_version, known_len);
        match decoded {
            Err(SnapshotError::Torn) => {}
            Err(other) => return Err(other),
            Ok(snapshot) => {
                let n = read_at(&file, &mut head, OFF_SEQ as u64)
                    .map_err(|e| SnapshotError::Io(e.to_string()))?;
                if n == head.len() && u64::from_le_bytes(head) == snapshot.seq {
                    return Ok(snapshot);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(SnapshotError::Torn)
}

/// What a reader can say about the sampler from the record at `path`.
pub fn report(path: &Path, now: Clock) -> (SensorReport, Option<Snapshot>) {
    report_on(Platform::current(), path, now)
}

/// [`report`] for a reader on `platform`: a reader with no sensor has nothing
/// to say about the sampler whatever the record holds. Named so the replay
/// tests can read a record as a reader that has one on any host.
pub(crate) fn report_on(
    platform: Platform,
    path: &Path,
    now: Clock,
) -> (SensorReport, Option<Snapshot>) {
    let dead = |reason: String| SensorReport {
        state: SensorState::Dead,
        age_ms: None,
        level: None,
        enabled: None,
        reason: Some(reason),
    };
    if platform == Platform::Unavailable {
        return (dead("ungoverned: no sensor on this platform".into()), None);
    }
    let snapshot = match read(path) {
        Ok(snapshot) => snapshot,
        Err(SnapshotError::Missing) => return (dead("no sampler".into()), None),
        Err(error) => return (dead(error.to_string()), None),
    };
    let mut report = SensorReport {
        state: SensorState::Dead,
        age_ms: None,
        level: snapshot.host.pressure.value(),
        enabled: Some(snapshot.enabled),
        reason: None,
    };
    match age_ms(snapshot.written, now) {
        Ok(age) => {
            report.age_ms = Some(age);
            report.state = SensorState::from_age_ms(age);
            if report.state == SensorState::Dead {
                report.reason = Some(format!("no sampler: last snapshot {}s ago", age / 1000));
            }
        }
        Err(why) => report.reason = Some(why),
    }
    if snapshot.platform == Platform::Unavailable {
        report.state = SensorState::Dead;
        report.reason = Some("ungoverned: the writer has no sensor".into());
    } else if !snapshot.enabled && report.reason.is_none() {
        report.reason = Some("host_load.enabled is off".into());
    }
    (report, Some(snapshot))
}

/// The elected writer: holds the lock for as long as it lives.
pub struct Writer {
    file: File,
    /// Held, never read: dropping it is what releases the election.
    _lock: File,
    path: PathBuf,
    seq: u64,
}

impl Writer {
    /// Tries to become the one writer for `dir`. `Ok(None)` when another
    /// process holds the election.
    pub fn elect(dir: &Path) -> Result<Option<Self>, String> {
        let lock = crate::fs_entry::open_private_record(&dir.join(LOCK_FILE_NAME))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => {
                return Err(format!(
                    "could not lock {}: {e}",
                    dir.join(LOCK_FILE_NAME).display()
                ))
            }
        }
        let path = dir.join(SNAPSHOT_FILE_NAME);
        let file = crate::fs_entry::open_private_record(&path)?;
        // Carry the sequence on from whatever is there, so a reader never sees
        // it run backwards across a change of writer.
        let mut head = [0u8; 8];
        let seq = match read_at(&file, &mut head, OFF_SEQ as u64) {
            Ok(8) => (u64::from_le_bytes(head).saturating_add(1)) & !1,
            _ => 0,
        };
        // A newer writer's longer record would otherwise keep stale trailing
        // fields this build never rewrites; an older writer's shorter one is
        // grown to this build's length (zero-filled until the first publish).
        file.set_len(EXTENDED_LEN as u64)
            .map_err(|e| format!("could not size {}: {e}", path.display()))?;
        Ok(Some(Self {
            file,
            _lock: lock,
            path,
            seq,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Publishes `snap` under the sequence lock.
    pub fn publish(&mut self, snap: &Snapshot) -> Result<(), String> {
        let next = self.seq.wrapping_add(2) & !1;
        let record = encode(snap, next);
        let io = |e: std::io::Error| format!("could not write {}: {e}", self.path.display());
        // Odd head first: from here until the even head lands, every reader
        // sees a write in progress.
        write_at(
            &self.file,
            &(next.wrapping_sub(1)).to_le_bytes(),
            OFF_SEQ as u64,
        )
        .map_err(io)?;
        write_at(&self.file, &record[OFF_SEQ + 8..], (OFF_SEQ + 8) as u64).map_err(io)?;
        write_at(&self.file, &record[..OFF_SEQ], 0).map_err(io)?;
        write_at(&self.file, &next.to_le_bytes(), OFF_SEQ as u64).map_err(io)?;
        self.seq = next;
        Ok(())
    }

    /// Writes an odd head and stops: what a writer killed mid-publish leaves.
    #[cfg(test)]
    pub(super) fn tear(&self) {
        write_at(&self.file, &(self.seq + 1).to_le_bytes(), OFF_SEQ as u64).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn clock(mono_s: u64) -> Clock {
        Clock {
            wall_ms: 1_700_000_000_000 + mono_s * 1000,
            mono_ns: mono_s * 1_000_000_000,
            boot_id: 0x5eed_b007,
        }
    }

    fn writer_id() -> WriterIdentity {
        WriterIdentity {
            pid: 4242,
            start_us: 1_699_000_100_000_000,
            version: "1.5.0".into(),
            path: "/Applications/GitPulse.app/Contents/MacOS/gitpulse".into(),
        }
    }

    fn full(written: Clock) -> Snapshot {
        let mut s = Snapshot::empty(written, writer_id(), true);
        s.platform = Platform::Governed;
        s.host = HostSample {
            pressure: Reading::Value(Level::Warn),
            swap_used: Reading::Value(5 << 30),
            swap_total: Reading::Value(8 << 30),
            compressor: Reading::Value(Compressor {
                thrashing_detected: 3,
                free_below_reserved: 20,
                swapouts_pressure: 1336,
                major_compaction_segments_freed: 1_109_273,
            }),
            compressed_bytes: Reading::Value(10 << 30),
            compressor_segments: Reading::Value(CompressorSegments {
                total: 254_515,
                limit: 1_677_721,
            }),
            load: Reading::Value([12.5, 9.25, 4.0]),
            cores: Reading::Value(Cores {
                logical: 18,
                levels: 2,
                perf0: 6,
                perf1: 12,
            }),
            memsize: Reading::Value(64 << 30),
            open_files: Reading::Value(10_000),
            max_files: Reading::Value(491_520),
            free_disk: Reading::Value(199 << 30),
            thermal: Reading::Value(Thermal::Fair),
            power: Reading::Value(Power::Ac),
            cpu_usage: Reading::Value(CpuUsage {
                total_pct: 42.5,
                user_pct: 31.2,
                system_pct: 11.3,
            }),
            gpu: Reading::Value(GpuSample {
                utilization_pct: 28.0,
                renderer_pct: Some(24.5),
                tiler_pct: Some(12.0),
                memory_in_use: Some(2 << 30),
                memory_allocated: Some(6 << 30),
            }),
            memory: Reading::Value(MemorySample {
                used_bytes: 34 << 30,
                app_bytes: Some(18 << 30),
                wired_bytes: Some(6 << 30),
                compressed_bytes: Some(10 << 30),
                cached_bytes: Some(14 << 30),
            }),
            total_disk: Reading::Value(926 << 30),
            thermal_temp_c: Reading::Value(64.5),
        };
        s.compressor_delta = Reading::Value(CompressorDelta {
            thrashing_detected: 1,
            swapouts_pressure: 4,
            ..CompressorDelta::default()
        });
        s.compressor_thrash = true;
        s.processes = Reading::Value(1964);
        s.unreadable_processes = 7;
        s.claude = Reading::Value(SessionCount {
            launched: 3,
            observed: 4,
        });
        s.codex = Reading::Unavailable(Why::Unreadable);
        s.launched_footprint = Reading::Value(9 << 30);
        s.observed_footprint = Reading::Value(3 << 30);
        s.largest_foreign = Reading::Value(18 << 30);
        s.launched_write_rate = Reading::Unavailable(Why::NotYet);
        s.max_tree_write_rate = Reading::Value(52_000_000);
        s.memguard_present = true;
        s.memguard_warn = true;
        s.memguard_pid = 68214;
        s.orphans = 1;
        s.disk_fill_rate = Reading::Value(2_899_102_924 & !((1 << 20) - 1));
        s.disk_fill_sustained = Reading::Value(1 << 30);
        s.verdict = Reading::Value(PublishedVerdict {
            tier: Tier::Severe,
            binding: Some(Binding::Swap),
            admit: false,
            shed_background: true,
            shed_reactive: true,
        });
        s.tick = TickStats {
            ticks: 900,
            overruns: 2,
            missed: 1,
            last_us: 4_100,
            max_us: 2_500_000,
            cpu_ns: 7_000_000_000,
        };
        s
    }

    /// Elects a writer after a previous one was dropped. Retried briefly: a
    /// process another test forks holds a copy of the lock descriptor, and so
    /// the lock, until it execs.
    fn elect_eventually(dir: &Path) -> Writer {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(w) = Writer::elect(dir).unwrap() {
                return w;
            }
            assert!(std::time::Instant::now() < deadline, "never re-elected");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn every_field_round_trips_through_the_record() {
        let snap = full(clock(50));
        let record = encode(&snap, 10);
        let back = decode_as(&record, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back, Snapshot { seq: 10, ..snap });
        // Unavailable stays unavailable, with its reason: never a zero.
        assert_eq!(back.codex, Reading::Unavailable(Why::Unreadable));
        assert_eq!(back.launched_write_rate, Reading::Unavailable(Why::NotYet));
    }

    const EVERY_BINDING: [Binding; 9] = [
        Binding::MemoryPressure,
        Binding::CompressorThrash,
        Binding::CompressorSegments,
        Binding::Swap,
        Binding::LowDisk,
        Binding::Thermal,
        Binding::CpuLoad,
        Binding::Battery,
        Binding::DiskFilling,
    ];

    fn with_binding(binding: Option<Binding>) -> Snapshot {
        let mut snap = full(clock(50));
        // What a writer from before the rates left in those bytes: zero.
        snap.disk_fill_rate = Reading::Unavailable(Why::Older);
        snap.disk_fill_sustained = Reading::Unavailable(Why::Older);
        snap.verdict = Reading::Value(PublishedVerdict {
            tier: Tier::Elevated,
            binding,
            admit: true,
            shed_background: true,
            shed_reactive: false,
        });
        snap
    }

    /// What a build from before the disk fill rule does with the byte every
    /// version reads: it refuses a code it does not know, and a refused code
    /// makes the whole verdict unreadable (`ext_reading`), which the command
    /// gate answers with "not load-checked" and never a refusal.
    fn older_reader_binding(record: &[u8]) -> Option<Option<Binding>> {
        const KNOWN_BEFORE: [u8; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
        let code = record[OFF_X_BINDING];
        KNOWN_BEFORE
            .contains(&code)
            .then(|| Binding::of_code(code))?
    }

    /// An installed hook or MCP binary is older than this build. The byte it
    /// reads must be one it knows for every binding, so a disk filling is read
    /// as the low disk it is and refused for, not dropped as unreadable.
    #[test]
    fn every_binding_is_published_in_a_byte_an_older_reader_knows() {
        for binding in EVERY_BINDING.into_iter().map(Some).chain([None]) {
            let record = encode(&with_binding(binding), 10);
            let seen = older_reader_binding(&record)
                .unwrap_or_else(|| panic!("{binding:?} is published as an unknown byte"));
            assert_eq!(seen, binding.map(Binding::wire), "{binding:?}");
        }
        let filling = encode(&with_binding(Some(Binding::DiskFilling)), 10);
        assert_eq!(
            filling[OFF_X_BINDING], 5,
            "the byte every version reads is LowDisk"
        );
        assert_eq!(filling[OFF_X_BINDING_EXACT], 9);
        // This build reads the exact one back.
        let back = decode_as(&filling, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(
            back.verdict.value().map(|v| v.binding),
            Some(Some(Binding::DiskFilling))
        );
        // Codes are never reused: nine bindings, nine bytes.
        let mut codes: Vec<u8> = EVERY_BINDING
            .iter()
            .map(|b| Binding::code_of(Some(*b)))
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    /// Everything an older reader reads is byte for byte what an older writer
    /// wrote: a record that holds no new binding differs from the record of a
    /// writer that predates the rule only in the spare bytes it left zero, and
    /// the filling record differs from the LowDisk record only in the spare
    /// bytes. Its bytes 0..692 are the same.
    #[test]
    fn the_new_bytes_live_only_in_the_spare_ones_an_older_reader_never_reads() {
        for binding in [None, Some(Binding::Swap), Some(Binding::LowDisk)] {
            let record = encode(&with_binding(binding), 10);
            assert!(
                record[OFF_X_BINDING_EXACT..OFF_X_CHECKSUM]
                    .iter()
                    .all(|b| *b == 0),
                "{binding:?}: nothing is added for a binding every version knows"
            );
        }
        let low = encode(&with_binding(Some(Binding::LowDisk)), 10);
        let filling = encode(&with_binding(Some(Binding::DiskFilling)), 10);
        assert_eq!(low[..OFF_X_BINDING_EXACT], filling[..OFF_X_BINDING_EXACT]);
        assert_eq!(filling.len(), EXTENDED_LEN);
        assert_eq!(get_u32(&filling, OFF_LENGTH) as usize, EXTENDED_LEN);
        // The extension's checksum covers the spare bytes, and a reader that
        // does not know them hashes them the same way: the record still reads.
        assert!(decode_as(&filling, MIN_READ_VERSION, RECORD_LEN).is_ok());
    }

    /// The failure this design exists to avoid, on the code an older reader
    /// runs: an unknown binding byte makes the verdict unreadable, not refused.
    #[test]
    fn an_unknown_binding_byte_reads_as_no_verdict_which_is_why_it_is_never_written() {
        let mut record = encode(&with_binding(Some(Binding::DiskFilling)), 10);
        // What a build that does not know code 9 would have read had it been
        // written to the byte every version reads.
        record[OFF_X_BINDING] = 200;
        record[OFF_X_BINDING_EXACT] = 0;
        let sum = ext_checksum(&record, 10);
        put_u64(&mut record, OFF_X_CHECKSUM, sum);
        let back = decode_as(&record, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert!(
            matches!(back.verdict, Reading::Unavailable(Why::Failed)),
            "{:?}",
            back.verdict
        );
        // A binding this build does not know in the detail byte falls back to
        // the byte every version reads.
        let mut future = encode(&with_binding(Some(Binding::DiskFilling)), 10);
        future[OFF_X_BINDING_EXACT] = 200;
        let sum = ext_checksum(&future, 10);
        put_u64(&mut future, OFF_X_CHECKSUM, sum);
        let back = decode_as(&future, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(
            back.verdict.value().map(|v| v.binding),
            Some(Some(Binding::LowDisk))
        );
    }

    #[test]
    fn the_fill_rates_round_trip_with_their_reasons_and_older_records_say_older() {
        let mib = |n: u64| Reading::Value(n << 20);
        for rate in [
            Reading::Value(0),
            mib(2_765),
            Reading::Unavailable(Why::NotYet),
            Reading::Unavailable(Why::Failed),
            Reading::Unavailable(Why::Unsupported),
            Reading::Unavailable(Why::Disabled),
        ] {
            let mut snap = full(clock(50));
            snap.disk_fill_rate = rate;
            snap.disk_fill_sustained = rate;
            let back = decode_as(&encode(&snap, 10), MIN_READ_VERSION, RECORD_LEN).unwrap();
            assert_eq!(back.disk_fill_rate, rate);
            assert_eq!(back.disk_fill_sustained, rate);
        }
        // The two are independent.
        let mut snap = full(clock(50));
        snap.disk_fill_rate = mib(4_096);
        snap.disk_fill_sustained = Reading::Unavailable(Why::NotYet);
        let back = decode_as(&encode(&snap, 10), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.disk_fill_rate, mib(4_096));
        assert_eq!(back.disk_fill_sustained, Reading::Unavailable(Why::NotYet));
        // A version-1 record has no extension: nothing published, which is
        // not a zero and not an unsupported platform.
        let v1 = encode_v1(&full(clock(50)), 10);
        let back = decode_as(&v1, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.disk_fill_rate, Reading::Unavailable(Why::Older));
        assert_eq!(back.disk_fill_sustained, Reading::Unavailable(Why::Older));
    }

    #[test]
    fn an_extension_written_before_the_fill_rates_reads_older_not_unsupported() {
        // The status bytes an earlier writer left are zero.
        let mut b = encode(&full(clock(50)), 10);
        b[OFF_X_FILL_STATUS] = 0;
        b[OFF_X_FILL_SUSTAINED_STATUS] = 0;
        let sum = ext_checksum(&b, 10);
        put_u64(&mut b, OFF_X_CHECKSUM, sum);
        let back = decode_as(&b, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.disk_fill_rate, Reading::Unavailable(Why::Older));
        assert_eq!(back.disk_fill_sustained, Reading::Unavailable(Why::Older));
    }

    #[test]
    fn long_writer_text_is_cut_on_a_character_boundary() {
        let mut snap = full(clock(1));
        snap.writer.path = format!("/{}", "é".repeat(400));
        let back = decode_as(&encode(&snap, 2), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert!(back.writer.path.len() < PATH_FIELD_LEN);
        assert!(back.writer.path.chars().skip(1).all(|c| c == 'é'));
    }

    /// An older build reading a newer record, and a newer build reading an
    /// older one: additive fields are accepted, anything less is typed.
    #[test]
    fn readers_accept_newer_additive_records_and_refuse_short_or_old_ones() {
        let snap = full(clock(5));
        // A v2 writer: VERSION 2, 64 more bytes of fields after this build's.
        let mut v2 = encode(&snap, 8).to_vec();
        put_u32(&mut v2, OFF_VERSION, 2);
        put_u32(&mut v2, OFF_LENGTH, (EXTENDED_LEN + 64) as u32);
        reseal(&mut v2);
        v2.extend_from_slice(&[0xAB; 64]);
        let read_by_v1 = decode_as(&v2, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(read_by_v1.version, 2);
        assert_eq!(read_by_v1.host, snap.host);

        // A v2 reader (minimum 2, knows RECORD_LEN + 64) given a v1 record.
        let v1 = encode_v1(&snap, 8);
        assert_eq!(
            decode_as(&v1, 2, RECORD_LEN + 64),
            Err(SnapshotError::UnsupportedVersion { found: 1, min: 2 })
        );
        assert_eq!(
            decode_as(&v1, 1, RECORD_LEN + 64),
            Err(SnapshotError::Short {
                length: RECORD_LEN,
                need: RECORD_LEN + 64
            })
        );
        // A record that declares less than it holds, or holds less.
        let mut lying = v1;
        put_u32(&mut lying, OFF_LENGTH, 100);
        reseal(&mut lying);
        assert!(matches!(
            decode_as(&lying, 1, RECORD_LEN),
            Err(SnapshotError::Short { length: 100, .. })
        ));
        assert!(matches!(
            decode_as(&v1[..300], 1, RECORD_LEN),
            Err(SnapshotError::Short { length: 300, .. })
        ));
        assert!(matches!(
            decode_as(&v1[..6], 1, RECORD_LEN),
            Err(SnapshotError::Short { .. })
        ));
        let mut zero = encode(&snap, 8);
        put_u32(&mut zero, OFF_VERSION, 0);
        reseal(&mut zero);
        assert!(matches!(
            decode_as(&zero, 1, RECORD_LEN),
            Err(SnapshotError::UnsupportedVersion { found: 0, .. })
        ));
        let mut alien = encode(&snap, 8);
        alien[0] = b'X';
        assert_eq!(
            decode_as(&alien, 1, RECORD_LEN),
            Err(SnapshotError::BadMagic)
        );
    }

    #[test]
    fn an_odd_or_mismatched_sequence_is_torn_never_decoded() {
        let snap = full(clock(5));
        let mut odd = encode(&snap, 8);
        put_u64(&mut odd, OFF_SEQ, 9);
        assert_eq!(decode_as(&odd, 1, RECORD_LEN), Err(SnapshotError::Torn));
        let mut split = encode(&snap, 8);
        put_u64(&mut split, OFF_SEQ_TAIL, 6);
        assert_eq!(decode_as(&split, 1, RECORD_LEN), Err(SnapshotError::Torn));
        // Both sequence copies agree and are even, but the body is a mix of
        // two writes: the checksum is what refuses it.
        let older = encode(&full(clock(1)), 8);
        let mut mixed = encode(&full(clock(2)), 8);
        mixed[OFF_PRESSURE..OFF_SWAP_TOTAL].copy_from_slice(&older[OFF_PRESSURE..OFF_SWAP_TOTAL]);
        mixed[OFF_WALL..OFF_WALL + 8].copy_from_slice(&older[OFF_WALL..OFF_WALL + 8]);
        assert_eq!(decode_as(&mixed, 1, RECORD_LEN), Err(SnapshotError::Torn));
    }

    /// Recomputes the checksum after a test edits a sealed record.
    fn reseal(record: &mut [u8]) {
        let sum = checksum(record);
        put_u64(record, OFF_CHECKSUM, sum);
    }

    /// Version 2 appends after version 1 and leaves every version-1 byte where
    /// it was. An older build checks exactly these things, from these offsets,
    /// and reads the same host readings it always did.
    #[test]
    fn an_older_reader_accepts_this_builds_record_and_reads_what_it_always_read() {
        let snap = full(clock(5));
        let record = encode(&snap, 8);
        assert_eq!(record.len(), EXTENDED_LEN);
        assert_eq!(&record[0..4], &MAGIC);
        assert_eq!(
            get_u32(&record, OFF_VERSION),
            2,
            "at or above its minimum, 1"
        );
        assert!(get_u32(&record, OFF_LENGTH) as usize >= RECORD_LEN);
        assert_eq!(get_u64(&record, OFF_SEQ), 8);
        assert_eq!(
            get_u64(&record, OFF_SEQ_TAIL),
            8,
            "the tail is where v1 looks"
        );
        assert_eq!(get_u64(&record, OFF_CHECKSUM), checksum(&record));
        // Decoded as version 1 reads it (the first RECORD_LEN bytes known):
        let older = decode_as(&record[..RECORD_LEN], MIN_READ_VERSION, RECORD_LEN);
        let older = older.expect("an older reader decodes the first 664 bytes alone");
        assert_eq!(older.host.pressure, snap.host.pressure);
        assert_eq!(older.host.swap_used, snap.host.swap_used);
        assert_eq!(older.tick, snap.tick);
        assert_eq!(older.written, snap.written);
        assert_eq!(
            older.verdict,
            Reading::default(),
            "it never saw the extension"
        );
    }

    /// An older writer's 664-byte record reads in this build, with the new
    /// fields unsupported, never as zero or as a guess.
    #[test]
    fn this_build_reads_an_older_writers_record_with_the_new_fields_unsupported() {
        let snap = full(clock(5));
        let old = encode_v1(&snap, 8);
        let seen = decode_as(&old, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(seen.version, 1);
        assert_eq!(seen.host.swap_used, snap.host.swap_used);
        assert_eq!(seen.host.compressor_segments, Reading::default());
        assert_eq!(seen.verdict, Reading::default());
        assert_eq!(
            Reading::<u8>::default(),
            Reading::Unavailable(Why::Unsupported)
        );
    }

    #[test]
    fn the_extension_round_trips_including_pending_and_unreadable() {
        let mut snap = full(clock(5));
        let back = decode_as(&encode(&snap, 8), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.verdict, snap.verdict);
        assert_eq!(back.host.compressor_segments, snap.host.compressor_segments);
        snap.verdict = Reading::Unavailable(Why::NotYet);
        snap.host.compressor_segments = Reading::Unavailable(Why::Unreadable);
        let back = decode_as(&encode(&snap, 10), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.verdict, Reading::Unavailable(Why::NotYet));
        assert_eq!(
            back.host.compressor_segments,
            Reading::Unavailable(Why::Unreadable),
            "unreadable is not zero segments"
        );
        snap.verdict = Reading::Value(PublishedVerdict {
            tier: Tier::Clear,
            binding: None,
            admit: true,
            shed_background: false,
            shed_reactive: false,
        });
        let back = decode_as(&encode(&snap, 12), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.verdict, snap.verdict);
    }

    #[test]
    fn a_torn_extension_or_an_unplaceable_verdict_is_never_decoded() {
        let snap = full(clock(5));
        // The extension's bytes from one write, its checksum from another.
        let mut torn = encode(&snap, 8);
        torn[OFF_X_TIER] = 0;
        assert_eq!(
            decode_as(&torn, MIN_READ_VERSION, RECORD_LEN),
            Err(SnapshotError::Torn)
        );
        // A tier byte this build cannot place, sealed consistently: a failed
        // reading, not a default tier.
        let mut odd = encode(&snap, 8);
        odd[OFF_X_TIER] = 9;
        let sum = ext_checksum(&odd, 8);
        put_u64(&mut odd, OFF_X_CHECKSUM, sum);
        let seen = decode_as(&odd, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(seen.verdict, Reading::Unavailable(Why::Failed));
        // A longer record from some other newer writer, with no such block.
        let mut other = encode(&snap, 8);
        other[OFF_EXT] = b'Z';
        let seen = decode_as(&other, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(seen.verdict, Reading::default());
        assert_eq!(seen.host.compressor_segments, Reading::default());
    }

    #[test]
    fn a_writer_grows_an_older_writers_record_and_publishes_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SNAPSHOT_FILE_NAME);
        std::fs::write(&path, encode_v1(&full(clock(1)), 6)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read(&path).unwrap().verdict, Reading::default());
        let mut writer = Writer::elect(dir.path()).unwrap().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len() as usize,
            EXTENDED_LEN
        );
        writer.publish(&full(clock(2))).unwrap();
        let seen = read(&path).unwrap();
        assert_eq!(seen.version, 2);
        assert_eq!(seen.verdict, full(clock(2)).verdict);
        assert!(
            seen.seq > 6,
            "the sequence carries on from the older writer"
        );
    }

    #[test]
    fn one_writer_per_directory_and_the_loser_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut winner = Writer::elect(dir.path()).unwrap().expect("first elects");
        assert!(
            Writer::elect(dir.path()).unwrap().is_none(),
            "a second process loses while the first lives"
        );
        winner.publish(&full(clock(3))).unwrap();
        let (seen, snapshot) = report_on(
            Platform::Governed,
            &dir.path().join(SNAPSHOT_FILE_NAME),
            clock(4),
        );
        assert_eq!(seen.state, SensorState::Fresh, "{seen:?}");
        assert_eq!(seen.level, Some(Level::Warn));
        assert_eq!(snapshot.unwrap().writer, writer_id());
        let seq = winner.seq;
        drop(winner);
        // The lock goes with the writer; the next one carries the sequence on.
        let mut next = elect_eventually(dir.path());
        next.publish(&full(clock(6))).unwrap();
        assert!(next.seq > seq, "{} after {seq}", next.seq);
    }

    #[test]
    fn a_writer_killed_mid_publish_reads_as_torn_not_as_its_half_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = Writer::elect(dir.path()).unwrap().unwrap();
        writer.publish(&full(clock(3))).unwrap();
        writer.tear();
        assert_eq!(
            read(&dir.path().join(SNAPSHOT_FILE_NAME)),
            Err(SnapshotError::Torn)
        );
        // Its successor's first publish heals it.
        drop(writer);
        let mut next = elect_eventually(dir.path());
        next.publish(&full(clock(4))).unwrap();
        assert!(read(&dir.path().join(SNAPSHOT_FILE_NAME)).is_ok());
    }

    /// Simulated: two threads on two open-file descriptions, as two
    /// processes would hold them. A reader racing a writer that never stops
    /// sees only whole records — every one it accepts decodes to a snapshot
    /// the writer actually published.
    #[test]
    fn a_reader_racing_a_writer_sees_only_whole_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SNAPSHOT_FILE_NAME);
        let mut writer = Writer::elect(dir.path()).unwrap().unwrap();
        let mut first = full(clock(0));
        first.tick.ticks = 0;
        writer.publish(&first).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let writing = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    n += 1;
                    let mut snap = full(clock(n));
                    // Every field that varies carries the same n, so a mixed
                    // record shows up as a disagreement between them.
                    snap.tick.ticks = n;
                    snap.processes = Reading::Value(n as u32);
                    snap.host.swap_used = Reading::Value(n);
                    writer.publish(&snap).unwrap();
                }
                n
            })
        };
        let (mut whole, mut torn) = (0u32, 0u32);
        let started = std::time::Instant::now();
        while started.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(400))
        {
            match read(&path) {
                Ok(s) => {
                    let n = s.tick.ticks;
                    if n > 0 {
                        assert_eq!(s.processes, Reading::Value(n as u32), "mixed record");
                        assert_eq!(s.host.swap_used, Reading::Value(n), "mixed record");
                        assert_eq!(s.written, clock(n), "mixed record");
                    }
                    whole += 1;
                }
                Err(SnapshotError::Torn) => torn += 1,
                Err(other) => panic!("{other}"),
            }
        }
        stop.store(true, Ordering::Relaxed);
        let published = writing.join().unwrap();
        assert!(
            whole > 0,
            "no whole read in {published} publishes ({torn} torn)"
        );
    }

    #[test]
    fn report_names_each_way_there_is_nothing_to_go_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SNAPSHOT_FILE_NAME);
        let (missing, none) = report_on(Platform::Governed, &path, clock(1));
        assert_eq!(missing.state, SensorState::Dead);
        assert_eq!(missing.reason.as_deref(), Some("no sampler"));
        assert!(none.is_none());

        let mut writer = Writer::elect(dir.path()).unwrap().unwrap();
        let mut off = full(clock(10));
        off.enabled = false;
        writer.publish(&off).unwrap();
        let (switched_off, _) = report_on(Platform::Governed, &path, clock(11));
        assert_eq!(switched_off.state, SensorState::Fresh);
        assert_eq!(switched_off.enabled, Some(false));
        assert_eq!(
            switched_off.reason.as_deref(),
            Some("host_load.enabled is off")
        );

        let mut blind = full(clock(10));
        blind.platform = Platform::Unavailable;
        writer.publish(&blind).unwrap();
        let (ungoverned, _) = report_on(Platform::Governed, &path, clock(11));
        assert_eq!(ungoverned.state, SensorState::Dead);
        assert!(ungoverned.reason.unwrap().contains("ungoverned"));

        // A reader with no sensor says so whatever the record holds, and
        // never hands that record back as something it could act on. Windows
        // is such a reader.
        let (blind_reader, handed_back) = report_on(Platform::Unavailable, &path, clock(11));
        assert_eq!(blind_reader.state, SensorState::Dead);
        assert_eq!(
            blind_reader.reason.as_deref(),
            Some("ungoverned: no sensor on this platform")
        );
        assert!(handed_back.is_none());
        #[cfg(windows)]
        assert_eq!(Platform::current(), Platform::Unavailable);
    }

    /// Replayed: one snapshot read at the four ages that bound the states.
    #[test]
    fn replayed_ages_give_each_of_the_four_states_with_the_last_known_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SNAPSHOT_FILE_NAME);
        let mut writer = Writer::elect(dir.path()).unwrap().unwrap();
        writer.publish(&full(clock(100))).unwrap();
        for (later_s, state) in [
            (5, SensorState::Fresh),
            (60, SensorState::StaleRecent),
            (300, SensorState::StaleOld),
            (601, SensorState::Dead),
        ] {
            let (seen, _) = report_on(Platform::Governed, &path, clock(100 + later_s));
            assert_eq!(seen.state, state, "{later_s}s");
            assert_eq!(seen.age_ms, Some(later_s * 1000));
            // The last-known level travels with every state, so a consumer
            // can say "critical, 5 min ago" rather than nothing.
            assert_eq!(seen.level, Some(Level::Warn), "{later_s}s");
        }
        let (rebooted, _) = report_on(
            Platform::Governed,
            &path,
            Clock {
                boot_id: 0xdead_b007,
                ..clock(1)
            },
        );
        assert_eq!(rebooted.state, SensorState::Dead);
        assert!(rebooted.reason.unwrap().contains("before this boot"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unsafe_snapshot_is_refused_and_a_reader_never_creates_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SNAPSHOT_FILE_NAME);
        assert_eq!(read(&path), Err(SnapshotError::Missing));
        assert!(!path.exists(), "reading must not create the record");

        let target = dir.path().join("elsewhere");
        std::fs::write(&target, encode(&full(clock(1)), 2)).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(matches!(read(&path), Err(SnapshotError::Unsafe(_))));
        assert!(
            Writer::elect(dir.path()).is_err(),
            "a writer refuses a planted symlink too"
        );
        std::fs::remove_file(&path).unwrap();

        std::fs::write(&path, encode(&full(clock(1)), 2)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read(&path).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Unsafe(why) if why.contains("other users")),
            "{err}"
        );
    }

    #[test]
    fn metrics_extension_preserves_gpx3_reads_older_and_rejects_tamper() {
        let mut snap = full(clock(9));
        let full_rec = encode(&snap, 14);
        let decoded = decode_as(&full_rec, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(decoded.host.cpu_usage, snap.host.cpu_usage);
        assert_eq!(decoded.host.gpu, snap.host.gpu);
        assert_eq!(decoded.host.memory, snap.host.memory);
        assert_eq!(decoded.host.total_disk, snap.host.total_disk);
        assert_eq!(decoded.host.thermal_temp_c, snap.host.thermal_temp_c);

        // Optional fields absent + negative/warm temperature + non-finite guard.
        snap.host.gpu = Reading::Value(GpuSample {
            utilization_pct: 15.4,
            renderer_pct: None,
            tiler_pct: None,
            memory_in_use: None,
            memory_allocated: None,
        });
        snap.host.memory = Reading::Value(MemorySample {
            used_bytes: 12 << 30,
            app_bytes: None,
            wired_bytes: None,
            compressed_bytes: None,
            cached_bytes: None,
        });
        snap.host.thermal_temp_c = Reading::Unavailable(Why::Unsupported);
        let partial = decode_as(&encode(&snap, 16), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(partial.host.gpu, snap.host.gpu);
        assert_eq!(partial.host.memory, snap.host.memory);
        assert_eq!(
            partial.host.thermal_temp_c,
            Reading::Unavailable(Why::Unsupported)
        );

        // A 712-byte GPX3-only record keeps GPX3 fields valid and marks GPXM Older.
        let mut gpx3_only = full_rec[..EXT_V2_LEN].to_vec();
        put_u32(&mut gpx3_only, OFF_LENGTH, EXT_V2_LEN as u32);
        reseal(&mut gpx3_only);
        let from_gpx3 = decode_as(&gpx3_only, MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(from_gpx3.verdict, full(clock(9)).verdict);
        assert_eq!(from_gpx3.host.cpu_usage, Reading::Unavailable(Why::Older));
        assert_eq!(from_gpx3.host.gpu, Reading::Unavailable(Why::Older));
        assert_eq!(from_gpx3.host.memory, Reading::Unavailable(Why::Older));
        assert_eq!(from_gpx3.host.total_disk, Reading::Unavailable(Why::Older));
        assert_eq!(
            from_gpx3.host.thermal_temp_c,
            Reading::Unavailable(Why::Older)
        );

        // A corrupted GPXM byte without updating GPXM checksum is Torn.
        let mut torn = full_rec;
        torn[OFF_M_CPU_TOTAL] ^= 0xFF;
        assert_eq!(
            decode_as(&torn, MIN_READ_VERSION, RECORD_LEN),
            Err(SnapshotError::Torn)
        );
    }

    #[test]
    fn disabled_snapshot_populates_and_round_trips_why_disabled() {
        let off = Snapshot::empty(clock(12), writer_id(), false);
        assert!(!off.enabled);
        let decoded = decode_as(&encode(&off, 4), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert!(!decoded.enabled);
        assert_eq!(decoded.host.pressure, Reading::Unavailable(Why::Disabled));
        assert_eq!(decoded.host.cpu_usage, Reading::Unavailable(Why::Disabled));
        assert_eq!(decoded.host.gpu, Reading::Unavailable(Why::Disabled));
        assert_eq!(decoded.host.memory, Reading::Unavailable(Why::Disabled));
        assert_eq!(decoded.host.total_disk, Reading::Unavailable(Why::Disabled));
        assert_eq!(
            decoded.host.thermal_temp_c,
            Reading::Unavailable(Why::Disabled)
        );
        assert_eq!(decoded.processes, Reading::Unavailable(Why::Disabled));
        assert_eq!(decoded.disk_fill_rate, Reading::Unavailable(Why::Disabled));
        assert_eq!(
            decoded.disk_fill_sustained,
            Reading::Unavailable(Why::Disabled)
        );
        assert_eq!(decoded.verdict, Reading::Unavailable(Why::Disabled));
    }

    #[test]
    fn non_finite_or_negative_floats_in_cpu_and_gpu_degrade_to_failed_not_zero() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1] {
            for cpu in [
                CpuUsage {
                    total_pct: bad,
                    user_pct: 10.0,
                    system_pct: 5.0,
                },
                CpuUsage {
                    total_pct: 15.0,
                    user_pct: bad,
                    system_pct: 5.0,
                },
                CpuUsage {
                    total_pct: 15.0,
                    user_pct: 10.0,
                    system_pct: bad,
                },
            ] {
                let mut snap = full(clock(20));
                snap.host.cpu_usage = Reading::Value(cpu);
                let back = decode_as(&encode(&snap, 6), MIN_READ_VERSION, RECORD_LEN).unwrap();
                assert_eq!(
                    back.host.cpu_usage,
                    Reading::Unavailable(Why::Failed),
                    "cpu {cpu:?} must fail closed"
                );
            }

            for gpu in [
                GpuSample {
                    utilization_pct: bad,
                    renderer_pct: Some(5.0),
                    tiler_pct: Some(2.0),
                    memory_in_use: None,
                    memory_allocated: None,
                },
                GpuSample {
                    utilization_pct: 10.0,
                    renderer_pct: Some(bad),
                    tiler_pct: Some(2.0),
                    memory_in_use: None,
                    memory_allocated: None,
                },
                GpuSample {
                    utilization_pct: 10.0,
                    renderer_pct: Some(5.0),
                    tiler_pct: Some(bad),
                    memory_in_use: None,
                    memory_allocated: None,
                },
            ] {
                let mut snap = full(clock(20));
                snap.host.gpu = Reading::Value(gpu);
                let back = decode_as(&encode(&snap, 8), MIN_READ_VERSION, RECORD_LEN).unwrap();
                assert_eq!(
                    back.host.gpu,
                    Reading::Unavailable(Why::Failed),
                    "gpu {gpu:?} must fail closed"
                );
            }
        }

        let mut bad_temp = full(clock(20));
        bad_temp.host.thermal_temp_c = Reading::Value(f64::NAN);
        let back = decode_as(&encode(&bad_temp, 10), MIN_READ_VERSION, RECORD_LEN).unwrap();
        assert_eq!(back.host.thermal_temp_c, Reading::Unavailable(Why::Failed));

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            for load in [[bad, 1.0, 1.0], [1.0, bad, 1.0], [1.0, 1.0, bad]] {
                let mut bad_load = full(clock(20));
                bad_load.host.load = Reading::Value(load);
                let back = decode_as(&encode(&bad_load, 12), MIN_READ_VERSION, RECORD_LEN).unwrap();
                assert_eq!(
                    back.host.load,
                    Reading::Unavailable(Why::Failed),
                    "load {load:?} must fail closed"
                );
            }
        }
    }
}
