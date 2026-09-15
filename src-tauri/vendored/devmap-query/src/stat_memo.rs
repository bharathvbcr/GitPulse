//! The stat key every file-digest memo in this workspace is keyed on, and the
//! bounded read they load through.
//!
//! **One derivation of the key.** Two memos of one computation is how the two
//! come to disagree about which digest belongs to which file state, and the key
//! is the half that goes wrong silently: a memo that hashes correctly but keys
//! carelessly returns a *stale digest as a fresh one*, which no caller can
//! detect. `freshness.rs` memoises repository source digests here and the CLI
//! memoises `devmap` binary digests in its own file; both ask this module what
//! "unchanged" means.

use std::io::Read;
use std::path::Path;

/// A stat key identical to `indexing.graph.build._stat_key`:
/// `"{size}:{mtime_ns}:{ctime_ns}"`.
///
/// `ctime_ns` is what makes the key safe — unlike `mtime_ns` it cannot be
/// back-dated by `utime`, `cp -p`, `rsync --times` or tar extraction, so a
/// rewrite that restores the old mtime still moves ctime and can never present
/// the key of the content it replaced. Size alone is weaker still: a rebuild of
/// the same source routinely lands on the same byte count.
#[cfg(unix)]
pub fn stat_key(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    let mtime_ns = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
    let ctime_ns = meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128;
    format!("{}:{}:{}", meta.len(), mtime_ns, ctime_ns)
}

/// Off unix there is no `st_ctime`, so the third field is absent and the key is
/// not the one Python writes.
///
/// mtime still carries. The Python transcription this replaced emitted
/// `"{size}:-:-"` off unix and then read its own key back, which made every
/// same-size rewrite within the memo's lifetime a silent hit on a *size-only*
/// key. Keeping mtime costs nothing and removes that. What is genuinely lost
/// off unix is the back-dating guard, so a key written on one platform is
/// deliberately not accepted on the other: that costs a rehash on each
/// crossing and never a wrong digest, because the digest is recomputed from
/// the bytes.
///
/// A file whose mtime the platform cannot report yields `"{size}:-:-"`, which
/// callers are expected to refuse rather than serve — see
/// `digest_cache::is_stat_key` in the CLI.
#[cfg(not(unix))]
pub fn stat_key(meta: &std::fs::Metadata) -> String {
    match meta.modified() {
        Ok(time) => format!("{}:{}:-", meta.len(), nanos_since_epoch(time)),
        Err(_) => format!("{}:-:-", meta.len()),
    }
}

/// Signed nanoseconds since the Unix epoch; negative before it.
///
/// `pub` and tested on every platform on purpose: it is only *used* by the
/// non-unix key, and a branch that compiles nowhere the tests run is a branch
/// nobody has ever executed.
pub fn nanos_since_epoch(time: std::time::SystemTime) -> i128 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => since.as_nanos() as i128,
        Err(before) => -(before.duration().as_nanos() as i128),
    }
}

/// Read a memo file, refusing anything larger than `cap` bytes.
///
/// A memo is state this process wrote and will read back without a schema
/// check on every field, so its size is the one thing that must not be taken on
/// trust: `fs::read_to_string` on a memo that something else has grown to
/// gigabytes turns an optimisation into an OOM. The cap is applied twice — once
/// against the stat, once as a hard ceiling on the read — because the file can
/// grow between the two.
///
/// **The open itself must not block.** `File::open` on a fifo with no writer
/// blocks inside `open(2)`, so a guard that inspects the handle afterwards
/// never runs: a fifo left where a memo belongs hangs the caller forever. The
/// shape is checked *before* the open, and on unix the open also carries
/// `O_NONBLOCK`, which closes the window where the path is swapped between the
/// two. `O_NONBLOCK` is a no-op for reads on a regular file, which is the only
/// thing this ever ends up reading.
///
/// `None` for absent, oversized, non-UTF-8, or not-a-regular-file, all of which
/// mean the same thing to a caller: no memo, hash the bytes.
pub fn read_bounded(path: &Path, cap: u64) -> Option<String> {
    // Follows symlinks deliberately — a memo reached through one is still a
    // memo — but resolves to something that must be a regular file.
    let stat = std::fs::metadata(path).ok()?;
    if !stat.is_file() || stat.len() > cap {
        return None;
    }
    let mut options = std::fs::File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    // Rechecked on the handle: between the stat and the open the path can be
    // replaced, and the cap is applied again below because the file can also
    // grow in that window.
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > cap {
        return None;
    }
    let mut text = String::new();
    file.take(cap).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "devmap-statmemo-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn the_epoch_offset_is_signed_in_both_directions() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(nanos_since_epoch(UNIX_EPOCH), 0);
        assert_eq!(
            nanos_since_epoch(UNIX_EPOCH + Duration::from_nanos(1_500_000_000)),
            1_500_000_000
        );
        // Before the epoch is not an error to swallow: two distinct pre-epoch
        // timestamps must not collapse onto one key.
        assert_eq!(
            nanos_since_epoch(UNIX_EPOCH - Duration::from_nanos(1_500_000_000)),
            -1_500_000_000
        );
        assert_ne!(
            nanos_since_epoch(UNIX_EPOCH - Duration::from_secs(1)),
            nanos_since_epoch(UNIX_EPOCH - Duration::from_secs(2))
        );
    }

    #[test]
    fn a_memo_larger_than_its_cap_reads_as_absent() {
        let dir = scratch("cap");
        let path = dir.join("memo.json");
        std::fs::write(&path, vec![b'x'; 4096]).expect("write");
        assert!(read_bounded(&path, 4096).is_some(), "exactly at the cap");
        assert!(read_bounded(&path, 4095).is_none(), "one byte over");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_and_a_missing_file_both_read_as_absent() {
        let dir = scratch("shapes");
        assert!(read_bounded(&dir, 1 << 20).is_none(), "a directory");
        assert!(
            read_bounded(&dir.join("nope.json"), 1 << 20).is_none(),
            "a missing file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_memo_that_is_not_utf8_reads_as_absent() {
        let dir = scratch("utf8");
        let path = dir.join("memo.json");
        std::fs::write(&path, [0xffu8, 0xfe, 0x00, 0x01]).expect("write");
        assert!(read_bounded(&path, 1 << 20).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
