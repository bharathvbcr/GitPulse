//! Timestamps and event identity for the ledger.
//!
//! Both are written by hand rather than pulled from a crate, because both are
//! small, and because the ledger's correctness rests on properties a general
//! library would not promise: that ids sort in creation order, and that a
//! timestamp is UTC ISO-8601 with a `Z`, which is the only form the schema's
//! string comparisons order correctly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Crockford base32, the ULID alphabet: no I, L, O or U.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// FNV-1a 64 over raw bytes: a digest that is the same in every build.
///
/// For keys that are persisted. `std::collections::hash_map::DefaultHasher`
/// promises no stable algorithm, so a Rust release could re-key everything
/// written under it. Not a security boundary: callers either re-check the
/// identity they looked up, or accept a 2^-64 collision.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Milliseconds since the Unix epoch, saturating at 0 for clocks before it.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Formats epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ`.
///
/// Always UTC, always with the milliseconds and the `Z`. The schema's
/// `idx_events_repo_ts` index and every range query over it compare these as
/// strings, and that only matches chronological order when the shape is fixed:
/// a local-time or offset-bearing timestamp would sort into the wrong place
/// and quietly break "events since the last cursor".
pub fn iso8601_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        millis
    )
}

/// Epoch milliseconds of a strict ISO-8601 instant, the inverse of
/// [`iso8601_utc`] for the forms an agent or a log writes: `YYYY-MM-DDTHH:MM:SS`,
/// then an optional `.` and 1 to 9 fraction digits (truncated to milliseconds),
/// then `Z` or a `±HH:MM` offset, which is converted to UTC.
///
/// `None` for anything else: a lowercase `t` or `z`, a missing offset (a local
/// time names no instant), an impossible date or a leap second, an offset past
/// 23:59, or an instant before the epoch. A caller that must keep going treats
/// `None` as "this record's time is not known", never as "now".
pub fn parse_event_time(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let part = bytes.get(from..to)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if !separators
        .iter()
        .all(|(at, sep)| bytes.get(*at) == Some(sep))
    {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let mut at = 19;
    let mut millis = 0i64;
    if bytes.get(at) == Some(&b'.') {
        let fraction = bytes[at + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if !(1..=9).contains(&fraction) {
            return None;
        }
        for place in 0..3 {
            let digit = bytes.get(at + 1 + place).filter(|_| place < fraction);
            millis = millis * 10 + digit.map_or(0, |d| i64::from(d - b'0'));
        }
        at += 1 + fraction;
    }
    let offset_secs = match &bytes[at..] {
        b"Z" => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (hours, minutes) = (number(at + 1, at + 3)?, number(at + 4, at + 6)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                magnitude
            } else {
                -magnitude
            }
        }
        _ => return None,
    };

    let local_secs =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    let utc_ms = (local_secs - offset_secs) * 1000 + millis;
    u64::try_from(utc_ms).ok()
}

/// A civil (year, month, day) to days since 1970-01-01: the inverse of
/// [`civil_from_days`], from the same algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 to a civil (year, month, day).
///
/// Howard Hinnant's algorithm. It is exact for the whole representable range,
/// including leap years and the 100/400-year rules, which a hand-rolled
/// approximation gets wrong roughly once a century and then only in February.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Per-process entropy for the random half of a ULID.
///
/// `RandomState` is seeded by the OS once per process; hashing a fixed value
/// through it yields a stable per-process number that differs between runs.
/// Combined with the monotonic counter below this makes ids unique within a
/// process and, in practice, across processes on one machine.
///
/// This is deliberately weaker than a CSPRNG, and the ledger does not need one:
/// event ids are identity, never capability. Nothing is authorised by knowing
/// or guessing a ulid.
fn process_seed() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SEED.get_or_init(|| {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(0x9E37_79B9_7F4A_7C15);
        h.finish()
    })
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A 26-character ULID: 48 bits of timestamp, then 80 bits of per-process
/// entropy and a monotonic counter.
///
/// Lexicographic order matches creation order, which is what lets a consumer
/// merge two ledgers — a repo's and the global one, or a live stream and an
/// importer's backfill — without a second sort key. The integer `id` column is
/// the paging cursor; this is the identity that survives being copied between
/// databases, where the integer does not.
pub fn ulid(ms: u64) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // 80 bits of "randomness": the process seed in the high half, the counter
    // in the low half so ids from one process never collide and always ascend.
    //
    // The seed must NOT be mixed with the counter. Folding `n` into the high
    // half made each id's leading characters jump around, so ids from a single
    // millisecond sorted arbitrarily — which defeats the one property the ulid
    // exists for, since the integer `id` is already the paging cursor.
    let hi = process_seed();
    let lo = n;

    let mut out = [0u8; 26];
    // 48-bit timestamp -> 10 characters.
    let mut t = ms & 0xFFFF_FFFF_FFFF;
    for slot in out[..10].iter_mut().rev() {
        *slot = CROCKFORD[(t & 0x1F) as usize];
        t >>= 5;
    }
    // 80-bit body -> 16 characters, low 40 bits from each of two u64s.
    let mut a = hi & 0xFF_FFFF_FFFF;
    for slot in out[10..18].iter_mut().rev() {
        *slot = CROCKFORD[(a & 0x1F) as usize];
        a >>= 5;
    }
    let mut b = lo & 0xFF_FFFF_FFFF;
    for slot in out[18..26].iter_mut().rev() {
        *slot = CROCKFORD[(b & 0x1F) as usize];
        b >>= 5;
    }
    // Every byte written above came from CROCKFORD, which is ASCII.
    String::from_utf8(out.to_vec()).expect("crockford alphabet is ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_epoch_and_a_known_instant() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_788_957_296_789), "2026-09-09T12:34:56.789Z");
    }

    #[test]
    fn handles_leap_days() {
        // 2024-02-29 is a leap day; 2100-02-28 is not (the 100-year rule).
        assert!(iso8601_utc(1_709_164_800_000).starts_with("2024-02-29"));
        assert!(iso8601_utc(4_107_456_000_000).starts_with("2100-02-28"));
        // ...and the day after it is 1 March, not 29 February.
        assert!(iso8601_utc(4_107_542_400_000).starts_with("2100-03-01"));
    }

    #[test]
    fn timestamps_sort_as_strings_in_chronological_order() {
        // The property every range query over idx_events_repo_ts depends on.
        let mut stamps: Vec<String> = [0u64, 1, 1_000, 1_788_957_296_789, 4_107_456_000_000]
            .iter()
            .map(|ms| iso8601_utc(*ms))
            .collect();
        let sorted = {
            let mut s = stamps.clone();
            s.sort();
            s
        };
        assert_eq!(stamps, sorted);
        stamps.dedup();
        assert_eq!(stamps.len(), 5, "distinct instants must render distinctly");
    }

    #[test]
    fn ulids_are_26_crockford_characters() {
        let id = ulid(now_millis());
        assert_eq!(id.len(), 26, "{id}");
        for c in id.chars() {
            assert!(
                CROCKFORD.contains(&(c as u8)),
                "{c:?} is not in the ULID alphabet ({id})"
            );
        }
        // The pattern the event schema enforces.
        assert!(id
            .chars()
            .all(|c| c.is_ascii_digit() || "ABCDEFGHJKMNPQRSTVWXYZ".contains(c)));
    }

    #[test]
    fn ulids_from_one_millisecond_are_unique_and_ascending() {
        // Two events appended in the same millisecond must still order, or a
        // consumer merging ledgers has no stable sequence.
        let ms = now_millis();
        let ids: Vec<String> = (0..1000).map(|_| ulid(ms)).collect();
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "ulid collision within one millisecond"
        );

        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "ulids from one process must ascend");
    }

    #[test]
    fn later_milliseconds_sort_after_earlier_ones() {
        let early = ulid(1_000_000_000_000);
        let late = ulid(1_000_000_000_001);
        assert!(early < late, "{early} should sort before {late}");
    }
}
