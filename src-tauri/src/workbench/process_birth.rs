//! OS-observed process creation identity, captured while the PTY still owns the
//! unreaped child. A saved PID alone is never termination or ownership proof.
//!
//! Two questions are answered here, and each has three answers rather than
//! two. "Is the process I recorded still running?" and "was this PID's current
//! process already running at that instant?" can both come back *could not
//! check* — a permission error is not an absence — and run reconciliation
//! releases a slot only on a definite `Gone`. Folding `Unknown` into `Gone`
//! would hand a live agent's checkout to a second agent; folding it into
//! `Alive` would strand the slot, which is the defect this exists to end.

/// What the operating system reports about one PID right now.
pub(super) struct Birth {
    /// The creation identity recorded with a run (`process_start`).
    pub identity: String,
    /// When that process started, in nanoseconds since the Unix epoch.
    pub started_unix_nanos: u128,
}

/// Whether a recorded process still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Liveness {
    Alive,
    /// No process holds the PID, or a different (later) process does.
    Gone,
    /// The question could not be answered; never evidence of either.
    Unknown(String),
}

/// The creation identity of `pid`, for recording at launch.
pub(super) fn read(pid: u32) -> Result<String, String> {
    match observe(pid)? {
        Some(birth) => Ok(birth.identity),
        None => Err(format!(
            "Cannot read process creation identity: process {pid} does not exist"
        )),
    }
}

/// Is the process recorded as `pid` with creation identity `expected` still
/// running? A PID now held by a different process means ours is gone.
pub(super) fn probe(pid: u32, expected: &str) -> Liveness {
    match observe(pid) {
        Ok(Some(birth)) if birth.identity == expected => Liveness::Alive,
        Ok(_) => Liveness::Gone,
        Err(reason) => Liveness::Unknown(reason),
    }
}

/// Is `pid` held by a process that was already running at `instant`
/// (nanoseconds since the Unix epoch)?
///
/// This is how a host that only recorded a PID and a moment can be judged
/// later: a process that started *after* that moment is a reuse of the PID,
/// so the one that was there is gone.
pub(super) fn running_since(pid: u32, instant: u128) -> Liveness {
    match observe(pid) {
        Ok(Some(birth)) if birth.started_unix_nanos <= instant => Liveness::Alive,
        Ok(_) => Liveness::Gone,
        Err(reason) => Liveness::Unknown(reason),
    }
}

/// `Ok(None)` only when the OS says no such process exists.
#[cfg(target_os = "macos")]
fn observe(pid: u32) -> Result<Option<Birth>, String> {
    let native_pid = i32::try_from(pid).map_err(|_| "Invalid process ID")?;
    if native_pid <= 0 {
        return Err("Invalid process ID".into());
    }
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: the output allocation matches the requested BSDINFO layout. It
    // is read only after proc_pidinfo reports that it filled the entire struct.
    let written = unsafe {
        libc::proc_pidinfo(
            native_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(size).map_err(|_| "Invalid process info size")?,
        )
    };
    if usize::try_from(written).ok() != Some(size) {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(format!("Cannot read process creation identity: {error}"));
    }
    // SAFETY: the full initialization was established above.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_start_tvsec == 0 {
        return Err("Invalid process creation identity".into());
    }
    Ok(Some(Birth {
        identity: format!(
            "macos:{}:{}:{}",
            pid, info.pbi_start_tvsec, info.pbi_start_tvusec
        ),
        started_unix_nanos: u128::from(info.pbi_start_tvsec) * 1_000_000_000
            + u128::from(info.pbi_start_tvusec) * 1_000,
    }))
}

#[cfg(target_os = "linux")]
fn observe(pid: u32) -> Result<Option<Birth>, String> {
    use std::io::Read;
    if pid == 0 {
        return Err("Invalid process ID".into());
    }
    let bounded = |path: &str, limit| -> Result<Option<String>, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut text = String::new();
        match file.take(limit + 1).read_to_string(&mut text) {
            Ok(_) => {}
            // The process exited between open and read.
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        }
        if u64::try_from(text.len()).map_err(|e| e.to_string())? > limit {
            return Err("Process identity exceeds its limit".into());
        }
        Ok(Some(text))
    };
    let Some(stat) = bounded(&format!("/proc/{pid}/stat"), 8192)? else {
        return Ok(None);
    };
    let fields = stat.rsplit_once(')').ok_or("Malformed process identity")?.1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or("Missing process start time")?;
    let ticks = start
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or("Invalid process start time")?;
    let boot = bounded("/proc/sys/kernel/random/boot_id", 128)?.ok_or("Missing boot identity")?;
    let boot = boot.trim();
    if boot.len() != 36 || !boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err("Invalid boot identity".into());
    }
    let system = bounded("/proc/stat", 1 << 20)?.ok_or("Missing system statistics")?;
    let booted = system
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or("Missing boot time")?;
    // SAFETY: sysconf reads a constant; it has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = u128::try_from(hz)
        .ok()
        .filter(|hz| *hz > 0)
        .ok_or("Invalid clock tick rate")?;
    Ok(Some(Birth {
        identity: format!("linux:{boot}:{pid}:{start}"),
        started_unix_nanos: u128::from(booted) * 1_000_000_000
            + u128::from(ticks) * 1_000_000_000 / hz,
    }))
}

#[cfg(target_os = "windows")]
fn observe(pid: u32) -> Result<Option<Birth>, String> {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    /// What `OpenProcess` reports for a PID no process holds.
    const ERROR_INVALID_PARAMETER: i32 = 87;
    /// 1601-01-01 to 1970-01-01, in 100 ns FILETIME ticks.
    const UNIX_EPOCH_TICKS: u128 = 116_444_736_000_000_000;
    if pid == 0 {
        return Err("Invalid process ID".into());
    }
    // SAFETY: query-only handle; no inherited access or mutation of the child.
    let handle = unsafe { OpenProcess(0x1000, 0, pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            return Ok(None);
        }
        return Err(error.to_string());
    }
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: the handle is live and each pointer targets a writable FILETIME.
    let result =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let failure = (result == 0).then(std::io::Error::last_os_error);
    // SAFETY: this function owns the handle and closes it exactly once.
    let closed = unsafe { CloseHandle(handle) };
    if let Some(error) = failure {
        return Err(error.to_string());
    }
    if closed == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // A process another handle keeps open still answers, with an exit time.
    if exit.low != 0 || exit.high != 0 {
        return Ok(None);
    }
    let ticks = (u64::from(creation.high) << 32) | u64::from(creation.low);
    if ticks == 0 {
        return Err("Invalid process creation identity".into());
    }
    Ok(Some(Birth {
        identity: format!("windows:{pid}:{ticks}"),
        started_unix_nanos: (u128::from(ticks) * 100).saturating_sub(UNIX_EPOCH_TICKS * 100),
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
compile_error!("Task terminal process identity requires a platform adapter");

#[cfg(test)]
mod tests {
    use super::{probe, read, running_since, Liveness};
    use crate::procguard::LockedSpawn;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn own_process_creation_identity_is_stable_and_not_just_a_pid() {
        let id = read(std::process::id()).unwrap();
        assert_eq!(read(std::process::id()).unwrap(), id);
        assert!(id.split(':').count() >= 3);
        assert!(read(u32::MAX).is_err());
    }

    #[test]
    fn a_running_process_is_alive_only_under_its_own_identity() {
        let me = std::process::id();
        let id = read(me).unwrap();
        assert_eq!(probe(me, &id), Liveness::Alive);
        // Same PID, another birth: the recorded process is not this one.
        assert_eq!(probe(me, &format!("{id}0")), Liveness::Gone);
    }

    #[test]
    fn a_reaped_child_is_gone_not_unknown() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit"][..]
            } else {
                &[][..]
            })
            .spawn_locked()
            .unwrap();
        let pid = child.id();
        let id = read(pid).unwrap_or_default();
        child.wait().unwrap();
        // Reaped: the PID names nothing, or something that started later.
        assert_eq!(probe(pid, &id), Liveness::Gone);
    }

    #[test]
    fn a_pid_the_platform_cannot_express_proves_nothing() {
        // Out of range for pid_t / never issued: not evidence of an exit.
        assert!(matches!(probe(0, "x"), Liveness::Unknown(_)));
        if cfg!(unix) {
            assert!(matches!(probe(u32::MAX - 7, "x"), Liveness::Unknown(_)));
        }
    }

    #[test]
    fn a_pid_is_running_since_an_instant_only_if_it_started_before_it() {
        let me = std::process::id();
        assert_eq!(running_since(me, now()), Liveness::Alive);
        // A moment before this process existed: whatever holds the PID now is
        // not what held it then.
        assert_eq!(running_since(me, 1), Liveness::Gone);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_may_not_be_inspected_is_never_reported_gone() {
        // PID 1 started before now and always exists. Unprivileged, it may or
        // may not be readable: Alive or Unknown are both honest, Gone is not.
        match running_since(1, now()) {
            Liveness::Alive => {}
            Liveness::Unknown(reason) => assert!(!reason.is_empty()),
            Liveness::Gone => panic!("an existing process was reported gone"),
        }
    }
}
