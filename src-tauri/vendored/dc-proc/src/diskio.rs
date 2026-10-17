//! What a process has written to storage, from the kernel's own counters.
//!
//! A `devmap` process was reported by macOS for dirtying 549.76 GB of
//! file-backed memory in 4,281 s (128 MB/s sustained for 71 minutes), and five
//! more `devmap` processes crossed the 2–34 GB rungs of the same report. No
//! gate noticed, because nothing in the workspace measured writes: wall time,
//! peak RSS and database size were all inside their bounds while the disk was
//! being rewritten over and over. This module is the measurement; the soak's
//! write budget (`rust/tools/soak/src/diskio.rs`) is the policy built on it.
//!
//! It reads another process, not only this one, because the process that
//! writes is the daemon the soak drives, not the soak.
//!
//! * **macOS**: `proc_pid_rusage(pid, RUSAGE_INFO_V4)`. Two counters, because
//!   they answer different questions and the incident was reported in only one
//!   of them: `ri_diskio_byteswritten` is what this process sent to the block
//!   layer, and `ri_logical_writes` counts file-backed pages dirtied — writes
//!   through the page cache and mmap, including rewrites the cache absorbed
//!   before they reached the disk. The resource monitor's "file backed memory
//!   dirtied" figure is the second kind, so a sampler that read only the first
//!   could report a calm process for exactly the loop that was reported.
//! * **Linux**: `/proc/<pid>/io`, `write_bytes` (block layer) and `wchar`
//!   (every byte passed to `write(2)` and friends, cached or not), the closest
//!   equivalents.
//! * Elsewhere: `Unsupported`. A platform that cannot be sampled is reported
//!   as such, never as a process that wrote nothing.

use std::io;

/// One reading of a process's write counters, in bytes since it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteCounters {
    /// Bytes this process sent to storage (`ri_diskio_byteswritten` on macOS,
    /// `write_bytes` on Linux).
    pub storage_bytes: u64,
    /// Bytes this process dirtied or wrote through the page cache, absorbed
    /// or not (`ri_logical_writes` on macOS, `wchar` on Linux). At least as
    /// large as `storage_bytes` for a process that does not truncate, and the
    /// quantity the macOS disk-writes report is stated in.
    pub logical_bytes: u64,
}

/// The write counters of process `pid` (which may be this process).
///
/// A process that has exited, one this user may not inspect, and a platform
/// with no counter are all errors: the caller must never confuse a reading
/// that could not be taken with a process that wrote nothing.
pub fn write_counters(pid: u32) -> io::Result<WriteCounters> {
    platform::write_counters(pid)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::WriteCounters;
    use std::io;

    #[allow(
        unsafe_code,
        reason = "one audited proc_pid_rusage into an owned rusage_info_v4, the only way macOS \
                  exposes another process's disk-write counters"
    )]
    pub(super) fn write_counters(pid: u32) -> io::Result<WriteCounters> {
        let pid = libc::c_int::try_from(pid).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {pid} is out of range"),
            )
        })?;
        // SAFETY: `rusage_info_v4` is plain old data — a uuid and u64
        // counters — so the all-zero bit pattern is a valid value.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is an owned, writable `rusage_info_v4` that outlives
        // the call, and `RUSAGE_INFO_V4` tells the kernel to fill exactly that
        // layout. The `rusage_info_t` out-parameter is `*mut *mut c_void` in
        // libc's signature but the kernel writes the struct through it, which
        // is how `<libproc.h>` documents the call; the cast passes the struct's
        // address and nothing else is read or written.
        let status = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>(),
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(WriteCounters {
            storage_bytes: info.ri_diskio_byteswritten,
            logical_bytes: info.ri_logical_writes,
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::WriteCounters;
    use std::io;

    pub(super) fn write_counters(pid: u32) -> io::Result<WriteCounters> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/io"))?;
        let field = |name: &str| -> io::Result<u64> {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|rest| rest.strip_prefix(':'))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("/proc/{pid}/io has no {name} line"),
                    )
                })?
                .trim()
                .parse::<u64>()
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("/proc/{pid}/io {name} is not a number: {error}"),
                    )
                })
        };
        Ok(WriteCounters {
            storage_bytes: field("write_bytes")?,
            logical_bytes: field("wchar")?,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::WriteCounters;
    use std::io;

    pub(super) fn write_counters(_pid: u32) -> io::Result<WriteCounters> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform exposes no per-process disk-write counter to sample",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counters move when this process writes, and by at least what it
    /// wrote. Synced, so the storage counter is charged before it is read
    /// rather than whenever the page cache gets round to it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_synced_write_is_counted_for_the_writing_process() {
        use std::io::Write;
        const BYTES: usize = 4 * 1024 * 1024;
        let pid = std::process::id();
        let before = write_counters(pid).expect("this process can read its own counters");
        let path = std::env::temp_dir().join(format!(
            "dc-proc-diskio-{pid}-{:?}",
            std::thread::current().id()
        ));
        {
            let mut file = std::fs::File::create(&path).expect("create");
            file.write_all(&vec![0x5a; BYTES]).expect("write");
            file.sync_all().expect("sync");
        }
        let after = write_counters(pid).expect("second reading");
        let _ = std::fs::remove_file(&path);
        let logical = after.logical_bytes - before.logical_bytes;
        let storage = after.storage_bytes - before.storage_bytes;
        assert!(
            logical >= BYTES as u64,
            "logical writes moved {logical} bytes for a {BYTES}-byte write"
        );
        assert!(
            storage >= BYTES as u64,
            "storage writes moved {storage} bytes for a synced {BYTES}-byte write"
        );
    }

    /// A pid that names no process is an error, not a reading of zero.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_process_that_does_not_exist_is_no_reading() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", ":"])
            .spawn()
            .expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        assert!(
            write_counters(pid).is_err(),
            "a reaped pid must not read as a process"
        );
    }
}
