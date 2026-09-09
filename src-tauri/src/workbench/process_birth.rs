//! OS-observed process creation identity, captured while the PTY still owns the
//! unreaped child. A saved PID alone is never termination or ownership proof.

#[cfg(target_os = "macos")]
pub(super) fn read(pid: u32) -> Result<String, String> {
    let native_pid = i32::try_from(pid).map_err(|_| "Invalid process ID")?;
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
        return Err(format!(
            "Cannot read process creation identity: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the full initialization was established above.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_start_tvsec == 0 {
        return Err("Invalid process creation identity".into());
    }
    Ok(format!(
        "macos:{}:{}:{}",
        pid, info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(target_os = "linux")]
pub(super) fn read(pid: u32) -> Result<String, String> {
    use std::io::Read;
    let bounded = |path: &str, limit| -> Result<String, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut text = String::new();
        file.take(limit + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if u64::try_from(text.len()).map_err(|e| e.to_string())? > limit {
            return Err("Process identity exceeds its limit".into());
        }
        Ok(text)
    };
    let stat = bounded(&format!("/proc/{pid}/stat"), 8192)?;
    let fields = stat.rsplit_once(')').ok_or("Malformed process identity")?.1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or("Missing process start time")?;
    if start.parse::<u64>().ok().filter(|v| *v > 0).is_none() {
        return Err("Invalid process start time".into());
    }
    let boot = bounded("/proc/sys/kernel/random/boot_id", 128)?;
    let boot = boot.trim();
    if boot.len() != 36 || !boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err("Invalid boot identity".into());
    }
    Ok(format!("linux:{boot}:{pid}:{start}"))
}

#[cfg(target_os = "windows")]
pub(super) fn read(pid: u32) -> Result<String, String> {
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
    // SAFETY: query-only handle; no inherited access or mutation of the child.
    let handle = unsafe { OpenProcess(0x1000, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
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
    let ticks = (u64::from(creation.high) << 32) | u64::from(creation.low);
    if ticks == 0 {
        return Err("Invalid process creation identity".into());
    }
    Ok(format!("windows:{pid}:{ticks}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
compile_error!("Task terminal process identity requires a platform adapter");

#[cfg(test)]
mod tests {
    #[test]
    fn own_process_creation_identity_is_stable_and_not_just_a_pid() {
        let id = super::read(std::process::id()).unwrap();
        assert_eq!(super::read(std::process::id()).unwrap(), id);
        assert!(id.split(':').count() >= 3);
        assert!(super::read(u32::MAX).is_err());
    }
}
