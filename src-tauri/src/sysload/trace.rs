//! `host-load-trace.v1`: a bounded ring of the last three hours of host load,
//! written beside `host-load.v1` so that what the machine looked like before a
//! crash survives the reboot.
//!
//! # Why
//!
//! On 2026-11-14 the owner's Mac hung and hard-reset. `host-load.v1` is one
//! record, overwritten every 2 s, so nothing on disk said what the last
//! minutes looked like. This ring keeps one small record every 10 s.
//!
//! # The file
//!
//! A 64-byte header (magic `GPHT`, version, record length, capacity, the boot
//! it was started in, a checksum) followed by [`CAPACITY`] fixed 128-byte
//! records, 138,304 bytes in all, sized once when the file is created. A
//! record carries its own sequence number and checksum, so a record torn by a
//! crash is simply a record that fails its checksum and is dropped: nothing
//! half-written is ever decoded, and the header never changes after creation
//! (one write and one `fsync` per record, not two). Record `n` goes in slot
//! `(n - 1) % capacity`, so the ring overwrites its oldest record.
//!
//! Every reading is stored with an in-band "unreadable" value that no real
//! reading can take (`u64::MAX`, `u32::MAX`, 255): an unreadable reading round
//! trips as unreadable and is never stored as zero.
//!
//! # Who writes
//!
//! Only the sampler that holds the `host-load.v1` writer election, in the app
//! process, appends (`sampler.rs`; `tests/host_load_sampler_scope.rs` pins
//! that nothing else names [`TraceWriter`]). One record every
//! [`EVERY_NTH_TICK`] ticks, which at the 2 s cadence is every 10 s. The file
//! opens through `fs_entry::open_private_record`: no symlink, a regular file,
//! owned by this user, mode 0600. One that fails those checks is not touched:
//! tracing stays off and the sampler reports why.
//!
//! # Across a reboot
//!
//! When the sampler starts and finds a ring written under a different boot id,
//! it renames it to `host-load-trace.prev.v1` (replacing an older one) before
//! starting a new ring, so the readings from before a crash are kept for one
//! boot. A file that is short, has the wrong magic or version, or fails its
//! header checksum is a typed [`TraceError`], is never decoded, and is
//! replaced.
//!
//! # What a hard reset can lose, and what it costs
//!
//! `File::sync_all` follows every record. On Apple targets std implements it
//! as `fcntl(F_FULLFSYNC)` (a whole-device cache flush), so a hard reset loses
//! at most the record being appended (up to 10 s). A power cut mid-write loses
//! only that one record, which fails its checksum. The cost is one 128-byte
//! write and one full flush per 10 s: about 5 ms median and 23 ms worst of 300
//! on the owner's Mac, against 0.07 ms for plain `fsync(2)`.

use super::governor::{Binding, Tier};
use super::sensor::{Level, Power, Reading, Thermal};
use super::snapshot::Snapshot;
use serde::Serialize;
use std::fs::File;
use std::path::{Path, PathBuf};

pub const TRACE_FILE_NAME: &str = "host-load-trace.v1";
pub const PREV_TRACE_FILE_NAME: &str = "host-load-trace.prev.v1";
const MAGIC: [u8; 4] = *b"GPHT";
const VERSION: u32 = 1;
/// Records the ring holds: three hours at one every 10 s.
pub const CAPACITY: u32 = 1_080;
/// One record every this many sampler ticks (10 s at the 2 s cadence).
pub const EVERY_NTH_TICK: u64 = 5;
/// The last records of the previous boot Health shows.
pub const PREVIOUS_TAIL: usize = 30;

const HEADER_LEN: usize = 64;
const RECORD_LEN: usize = 128;

// Header offsets.
const H_VERSION: usize = 4;
const H_RECORD_LEN: usize = 8;
const H_CAPACITY: usize = 12;
const H_BOOT: usize = 16;
const H_CREATED: usize = 24;
const H_CHECKSUM: usize = 56;

// Record offsets.
const R_SEQ: usize = 0;
const R_WALL_S: usize = 8;
const R_MONO_MS: usize = 16;
const R_BOOT: usize = 24;
const R_PRESSURE: usize = 32;
const R_TIER: usize = 33;
const R_BINDING: usize = 34;
const R_ADMIT: usize = 35;
const R_THERMAL: usize = 36;
const R_POWER: usize = 37;
const R_SWAP: usize = 40;
const R_COMPRESSED: usize = 48;
const R_LOAD: usize = 56;
const R_DISK: usize = 64;
const R_LAUNCHED_N: usize = 72;
const R_OBSERVED_N: usize = 76;
const R_FOREIGN: usize = 80;
const R_LAUNCHED_FP: usize = 88;
const R_SEGMENTS: usize = 96;
const R_CHECKSUM: usize = 120;

/// The stored value of a reading that could not be taken.
const NONE64: u64 = u64::MAX;
const NONE32: u32 = u32::MAX;
const NONE8: u8 = 255;

/// One sample of the ring. A reading is a value or the reason there is none;
/// the reason is not kept on disk, only that there was none.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceRecord {
    pub wall_s: u64,
    pub mono_ms: u64,
    pub boot_id: u64,
    pub pressure: Option<Level>,
    /// The tier the app's hysteresis held when this was taken.
    pub tier: Option<Tier>,
    pub binding: Option<Binding>,
    /// Whether launches were admitted.
    pub admit: Option<bool>,
    pub swap_used: Option<u64>,
    pub compressed_bytes: Option<u64>,
    pub load_1m: Option<f64>,
    pub free_disk: Option<u64>,
    pub launched_sessions: Option<u32>,
    pub observed_sessions: Option<u32>,
    /// The heaviest process GitPulse did not start.
    pub largest_foreign: Option<u64>,
    /// Summed footprint of every GitPulse-launched tree.
    pub launched_footprint: Option<u64>,
    pub thermal: Option<Thermal>,
    pub power: Option<Power>,
    /// Compressor segments in use over its segment limit.
    pub segment_fraction: Option<f64>,
}

fn value<T: Copy>(reading: Reading<T>) -> Option<T> {
    reading.value()
}

impl TraceRecord {
    /// The record for one tick: the snapshot just published and the verdict
    /// the governor holds after counting it.
    pub fn of(
        snapshot: &Snapshot,
        tier: Tier,
        binding: Option<Binding>,
        admit: bool,
    ) -> Self {
        let sessions = |launched: Option<u32>, observed: Option<u32>| match (launched, observed) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            _ => None,
        };
        let claude = value(snapshot.claude);
        let codex = value(snapshot.codex);
        let launched = sessions(
            claude.map(|c| c.launched),
            codex.map(|c| c.launched),
        );
        let observed = sessions(
            claude.map(|c| c.observed),
            codex.map(|c| c.observed),
        );
        let host = &snapshot.host;
        Self {
            wall_s: snapshot.written.wall_ms / 1000,
            mono_ms: snapshot.written.mono_ns / 1_000_000,
            boot_id: snapshot.written.boot_id,
            pressure: value(host.pressure),
            tier: Some(tier),
            binding,
            admit: Some(admit),
            swap_used: value(host.swap_used),
            compressed_bytes: value(host.compressed_bytes),
            load_1m: value(host.load).map(|l| l[0]),
            free_disk: value(host.free_disk),
            launched_sessions: launched,
            observed_sessions: observed,
            largest_foreign: value(snapshot.largest_foreign),
            launched_footprint: value(snapshot.launched_footprint),
            thermal: value(host.thermal),
            power: value(host.power),
            segment_fraction: value(host.compressor_segments)
                .and_then(|s| (s.limit > 0).then(|| s.total as f64 / s.limit as f64)),
        }
    }
}

/// Why a trace file could not be used. Each is its own variant, as for the
/// snapshot: a refusal to trust the file is not "no file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceError {
    Missing,
    /// A symlink, another owner, open to other users. Never touched.
    Unsafe(String),
    Io(String),
    BadMagic,
    UnsupportedVersion(u32),
    /// Shorter than its header says it must be.
    Short {
        length: usize,
        need: usize,
    },
    /// The header fails its checksum or disagrees with this build's layout.
    BadHeader,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no trace has been written"),
            Self::Unsafe(why) => write!(f, "the trace is not safe to read: {why}"),
            Self::Io(why) => write!(f, "the trace could not be read: {why}"),
            Self::BadMagic => write!(f, "the file is not a host-load trace"),
            Self::UnsupportedVersion(v) => write!(f, "the trace is version {v}; this build reads 1"),
            Self::Short { length, need } => {
                write!(f, "the trace holds {length} bytes; it needs {need}")
            }
            Self::BadHeader => write!(f, "the trace header is damaged"),
        }
    }
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
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

/// FNV-1a over `bytes`.
fn checksum(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn encode_record(seq: u64, r: &TraceRecord) -> [u8; RECORD_LEN] {
    let mut b = [0u8; RECORD_LEN];
    put_u64(&mut b, R_SEQ, seq);
    put_u64(&mut b, R_WALL_S, r.wall_s);
    put_u64(&mut b, R_MONO_MS, r.mono_ms);
    put_u64(&mut b, R_BOOT, r.boot_id);
    b[R_PRESSURE] = r.pressure.map_or(NONE8, |l| l.kernel() as u8);
    b[R_TIER] = r.tier.map_or(NONE8, Tier::code);
    // 0 is "no binding", a real value; an unknowable binding is not stored.
    b[R_BINDING] = Binding::code_of(r.binding);
    b[R_ADMIT] = r.admit.map_or(NONE8, u8::from);
    b[R_THERMAL] = r.thermal.map_or(NONE8, Thermal::code);
    b[R_POWER] = r.power.map_or(NONE8, |p| match p {
        Power::Ac => 0,
        Power::Battery => 1,
    });
    put_u64(&mut b, R_SWAP, r.swap_used.unwrap_or(NONE64));
    put_u64(&mut b, R_COMPRESSED, r.compressed_bytes.unwrap_or(NONE64));
    put_u64(
        &mut b,
        R_LOAD,
        r.load_1m
            .filter(|l| l.is_finite() && *l >= 0.0)
            .map_or(NONE64, |l| (l * 1000.0).round().min(1e15) as u64),
    );
    put_u64(&mut b, R_DISK, r.free_disk.unwrap_or(NONE64));
    put_u32(&mut b, R_LAUNCHED_N, r.launched_sessions.unwrap_or(NONE32));
    put_u32(&mut b, R_OBSERVED_N, r.observed_sessions.unwrap_or(NONE32));
    put_u64(&mut b, R_FOREIGN, r.largest_foreign.unwrap_or(NONE64));
    put_u64(&mut b, R_LAUNCHED_FP, r.launched_footprint.unwrap_or(NONE64));
    put_u32(
        &mut b,
        R_SEGMENTS,
        r.segment_fraction
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map_or(NONE32, |f| ((f * 1_000_000.0).round() as u64).min(u64::from(NONE32 - 1)) as u32),
    );
    let sum = checksum(&b[..R_CHECKSUM]);
    put_u64(&mut b, R_CHECKSUM, sum);
    b
}

/// A record and its sequence number, or `None` for an empty slot, a torn one
/// (checksum mismatch) or one with a code this build cannot place.
fn decode_record(b: &[u8]) -> Option<(u64, TraceRecord)> {
    let seq = get_u64(b, R_SEQ);
    if seq == 0 || get_u64(b, R_CHECKSUM) != checksum(&b[..R_CHECKSUM]) {
        return None;
    }
    let opt64 = |at: usize| Some(get_u64(b, at)).filter(|v| *v != NONE64);
    let opt32 = |at: usize| Some(get_u32(b, at)).filter(|v| *v != NONE32);
    Some((
        seq,
        TraceRecord {
            wall_s: get_u64(b, R_WALL_S),
            mono_ms: get_u64(b, R_MONO_MS),
            boot_id: get_u64(b, R_BOOT),
            pressure: match b[R_PRESSURE] {
                NONE8 => None,
                raw => Level::from_kernel(u32::from(raw)),
            },
            tier: Tier::of_code(b[R_TIER]),
            binding: Binding::of_code(b[R_BINDING])?,
            admit: match b[R_ADMIT] {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
            swap_used: opt64(R_SWAP),
            compressed_bytes: opt64(R_COMPRESSED),
            load_1m: opt64(R_LOAD).map(|milli| milli as f64 / 1000.0),
            free_disk: opt64(R_DISK),
            launched_sessions: opt32(R_LAUNCHED_N),
            observed_sessions: opt32(R_OBSERVED_N),
            largest_foreign: opt64(R_FOREIGN),
            launched_footprint: opt64(R_LAUNCHED_FP),
            thermal: Thermal::from_code(b[R_THERMAL]),
            power: match b[R_POWER] {
                0 => Some(Power::Ac),
                1 => Some(Power::Battery),
                _ => None,
            },
            segment_fraction: opt32(R_SEGMENTS).map(|ppm| f64::from(ppm) / 1_000_000.0),
        },
    ))
}

struct Header {
    capacity: u32,
    boot_id: u64,
}

fn encode_header(capacity: u32, boot_id: u64, created_wall_s: u64) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..4].copy_from_slice(&MAGIC);
    put_u32(&mut b, H_VERSION, VERSION);
    put_u32(&mut b, H_RECORD_LEN, RECORD_LEN as u32);
    put_u32(&mut b, H_CAPACITY, capacity);
    put_u64(&mut b, H_BOOT, boot_id);
    put_u64(&mut b, H_CREATED, created_wall_s);
    let sum = checksum(&b[..H_CHECKSUM]);
    put_u64(&mut b, H_CHECKSUM, sum);
    b
}

fn decode_header(buf: &[u8]) -> Result<Header, TraceError> {
    if buf.len() < HEADER_LEN {
        return Err(TraceError::Short {
            length: buf.len(),
            need: HEADER_LEN,
        });
    }
    if buf[0..4] != MAGIC {
        return Err(TraceError::BadMagic);
    }
    let version = get_u32(buf, H_VERSION);
    if version != VERSION {
        return Err(TraceError::UnsupportedVersion(version));
    }
    let capacity = get_u32(buf, H_CAPACITY);
    if get_u32(buf, H_RECORD_LEN) as usize != RECORD_LEN
        || capacity == 0
        || get_u64(buf, H_CHECKSUM) != checksum(&buf[..H_CHECKSUM])
    {
        return Err(TraceError::BadHeader);
    }
    Ok(Header {
        capacity,
        boot_id: get_u64(buf, H_BOOT),
    })
}

fn file_len(capacity: u32) -> usize {
    HEADER_LEN + capacity as usize * RECORD_LEN
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

/// Reads the whole ring from an open file: the header, then every record that
/// passes its checksum, oldest first. A file shorter than its header says is
/// [`TraceError::Short`] and nothing in it is decoded.
fn read_ring_from(file: &File) -> Result<(Header, Vec<TraceRecord>), TraceError> {
    let io = |e: std::io::Error| TraceError::Io(e.to_string());
    let length = usize::try_from(file.metadata().map_err(io)?.len()).unwrap_or(usize::MAX);
    let mut head = [0u8; HEADER_LEN];
    let got = read_at(file, &mut head, 0).map_err(io)?;
    let header = decode_header(&head[..got])?;
    // A capacity a hostile file could claim must not size an allocation.
    if header.capacity > CAPACITY * 4 {
        return Err(TraceError::BadHeader);
    }
    let need = file_len(header.capacity);
    if length < need {
        return Err(TraceError::Short { length, need });
    }
    let mut body = vec![0u8; header.capacity as usize * RECORD_LEN];
    let mut filled = 0;
    while filled < body.len() {
        let n = read_at(file, &mut body[filled..], (HEADER_LEN + filled) as u64).map_err(io)?;
        if n == 0 {
            return Err(TraceError::Short {
                length: HEADER_LEN + filled,
                need,
            });
        }
        filled += n;
    }
    let mut records: Vec<(u64, TraceRecord)> =
        body.as_chunks::<RECORD_LEN>().0.iter().filter_map(|c| decode_record(c)).collect();
    records.sort_by_key(|(seq, _)| *seq);
    Ok((header, records.into_iter().map(|(_, r)| r).collect()))
}

/// Reads the ring at `path` (the previous boot's, say) without ever creating
/// it, oldest record first.
pub fn read_ring(path: &Path) -> Result<Vec<TraceRecord>, TraceError> {
    let file = match crate::fs_entry::open_private_record_readonly(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Err(TraceError::Missing),
        Err(why) => return Err(TraceError::Unsafe(why)),
    };
    read_ring_from(&file).map(|(_, records)| records)
}

/// The appender: held by the one sampler that holds the snapshot election.
pub struct TraceWriter {
    file: File,
    path: PathBuf,
    capacity: u32,
    next_seq: u64,
}

impl TraceWriter {
    /// Opens (creating if absent) the ring in `dir` for the boot `boot_id`,
    /// first moving a ring from another boot to `host-load-trace.prev.v1`.
    pub fn open(dir: &Path, boot_id: u64) -> Result<Self, String> {
        Self::open_with_capacity(dir, boot_id, CAPACITY)
    }

    /// [`TraceWriter::open`] with a chosen capacity, so a test can wrap a
    /// small ring.
    pub(super) fn open_with_capacity(dir: &Path, boot_id: u64, capacity: u32) -> Result<Self, String> {
        let path = dir.join(TRACE_FILE_NAME);
        match crate::fs_entry::open_private_record_readonly(&path) {
            Ok(None) => {}
            // Never touch a file this user does not alone own.
            Err(why) => return Err(why),
            Ok(Some(existing)) => match read_ring_from(&existing) {
                Ok((header, _)) if header.capacity == capacity && header.boot_id == boot_id => {
                    // The same boot (the app restarted): carry the ring on.
                    drop(existing);
                    let next_seq = Self::resume_seq(&path, header.capacity)?;
                    let file = crate::fs_entry::open_private_record(&path)?;
                    return Ok(Self {
                        file,
                        path,
                        capacity,
                        next_seq,
                    });
                }
                Ok((header, _)) if header.boot_id != boot_id => {
                    // Another boot: keep it as the one before this.
                    drop(existing);
                    std::fs::rename(&path, dir.join(PREV_TRACE_FILE_NAME))
                        .map_err(|e| format!("could not keep the previous trace: {e}"))?;
                }
                // A different capacity in this boot, or a file that is torn,
                // short or foreign: never decoded, replaced.
                Ok(_) | Err(_) => {
                    drop(existing);
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("could not replace the trace: {e}"))?;
                }
            },
        }
        let file = crate::fs_entry::open_private_record(&path)?;
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        file.set_len(file_len(capacity) as u64)
            .map_err(|e| format!("could not size {}: {e}", path.display()))?;
        write_at(&file, &encode_header(capacity, boot_id, created), 0)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("could not sync {}: {e}", path.display()))?;
        Ok(Self {
            file,
            path,
            capacity,
            next_seq: 1,
        })
    }

    /// The sequence a resumed ring continues from: after the newest record it
    /// holds.
    fn resume_seq(path: &Path, capacity: u32) -> Result<u64, String> {
        let file = crate::fs_entry::open_private_record_readonly(path)?
            .ok_or_else(|| "the trace vanished".to_string())?;
        let mut body = vec![0u8; capacity as usize * RECORD_LEN];
        let mut filled = 0;
        while filled < body.len() {
            let n = read_at(&file, &mut body[filled..], (HEADER_LEN + filled) as u64)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        let newest = body
            .as_chunks::<RECORD_LEN>()
            .0
            .iter()
            .filter_map(|c| decode_record(c))
            .map(|(seq, _)| seq)
            .max()
            .unwrap_or(0);
        Ok(newest + 1)
    }

    /// The sequence number the next record will take.
    #[cfg(test)]
    pub(super) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Writes `record` into its slot and `fsync`s. The record is whole or its
    /// checksum fails; there is no second write to keep in step with it.
    pub fn append(&mut self, record: &TraceRecord) -> Result<(), String> {
        let seq = self.next_seq;
        let slot = (seq - 1) % u64::from(self.capacity);
        let at = HEADER_LEN as u64 + slot * RECORD_LEN as u64;
        write_at(&self.file, &encode_record(seq, record), at)
            .map_err(|e| format!("could not write {}: {e}", self.path.display()))?;
        self.file
            .sync_all()
            .map_err(|e| format!("could not sync {}: {e}", self.path.display()))?;
        self.next_seq = seq + 1;
        Ok(())
    }
}

/// The highest, lowest or worst of each reading over a window of records.
/// `None` where every record in the window was unreadable: not a zero.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TracePeaks {
    pub records: usize,
    pub max_tier: Option<Tier>,
    /// Whether launches were refused at any record.
    pub refused: bool,
    pub max_swap_used: Option<u64>,
    pub max_compressed_bytes: Option<u64>,
    pub max_load_1m: Option<f64>,
    pub min_free_disk: Option<u64>,
    pub max_launched_footprint: Option<u64>,
    pub max_largest_foreign: Option<u64>,
    pub max_segment_fraction: Option<f64>,
    pub worst_thermal: Option<Thermal>,
    /// The highest kernel pressure level seen.
    pub max_pressure: Option<Level>,
}

impl TracePeaks {
    pub fn of(records: &[TraceRecord]) -> Self {
        fn fold<T: Copy>(
            records: &[TraceRecord],
            get: impl Fn(&TraceRecord) -> Option<T>,
            better: impl Fn(&T, &T) -> bool,
        ) -> Option<T> {
            records.iter().filter_map(get).fold(None, |best, v| match best {
                Some(b) if !better(&v, &b) => Some(b),
                _ => Some(v),
            })
        }
        Self {
            records: records.len(),
            max_tier: fold(records, |r| r.tier, |a, b| a > b),
            refused: records.iter().any(|r| r.admit == Some(false)),
            max_swap_used: fold(records, |r| r.swap_used, |a, b| a > b),
            max_compressed_bytes: fold(records, |r| r.compressed_bytes, |a, b| a > b),
            max_load_1m: fold(records, |r| r.load_1m, |a, b| a > b),
            min_free_disk: fold(records, |r| r.free_disk, |a, b| a < b),
            max_launched_footprint: fold(records, |r| r.launched_footprint, |a, b| a > b),
            max_largest_foreign: fold(records, |r| r.largest_foreign, |a, b| a > b),
            max_segment_fraction: fold(records, |r| r.segment_fraction, |a, b| a > b),
            worst_thermal: fold(records, |r| r.thermal, |a, b| a.code() > b.code()),
            max_pressure: fold(records, |r| r.pressure, |a, b| a > b),
        }
    }
}

/// What Health shows under "Before the last restart".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviousBoot {
    /// Wall-clock seconds of the newest record: when the previous boot's trace
    /// ended. `None` when no record could be read.
    pub ended_wall_s: Option<u64>,
    /// The last [`PREVIOUS_TAIL`] records, oldest first.
    pub records: Vec<TraceRecord>,
    /// Peaks over exactly those records.
    pub peaks: TracePeaks,
    /// Why the previous trace could not be read, when it could not. A trace
    /// that is missing is `None` here and a `None` previous boot altogether.
    pub error: Option<String>,
}

/// The tail of the previous boot's ring in `dir`, or `None` when there is
/// none. A file that cannot be trusted is a `PreviousBoot` with an `error` and
/// no records: said, never read as an empty history.
pub fn previous_boot(dir: &Path) -> Option<PreviousBoot> {
    match read_ring(&dir.join(PREV_TRACE_FILE_NAME)) {
        Err(TraceError::Missing) => None,
        Err(error) => Some(PreviousBoot {
            ended_wall_s: None,
            records: Vec::new(),
            peaks: TracePeaks::of(&[]),
            error: Some(error.to_string()),
        }),
        Ok(mut records) => {
            let keep = records.len().saturating_sub(PREVIOUS_TAIL);
            let tail = records.split_off(keep);
            Some(PreviousBoot {
                ended_wall_s: tail.last().map(|r| r.wall_s),
                peaks: TracePeaks::of(&tail),
                records: tail,
                error: None,
            })
        }
    }
}

/// Whether tick number `tick` (counted from 1) appends a record: only the
/// elected writer, only while measuring, and only every [`EVERY_NTH_TICK`]th.
pub fn due(is_writer: bool, enabled: bool, tick: u64) -> bool {
    is_writer && enabled && tick.is_multiple_of(EVERY_NTH_TICK)
}

#[cfg(test)]
mod tests;
