//! What a live PTY session is doing right now: the program in the terminal's
//! foreground and the directory it is working in.
//!
//! Read on demand, never polled. Every answer names its own gaps: a process
//! the OS would not describe is `None`, not the repository root, because a
//! guessed directory would open the next tab somewhere the user never went.

use std::path::PathBuf;

/// Longest program name this module reports. A process can rename itself to
/// anything; a tab title is not the place to discover how long.
pub(super) const MAX_PROCESS_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessView {
    pub pid: i32,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Keeps printable characters only and clips to [`MAX_PROCESS_NAME_CHARS`].
/// An empty result is `None`: a blank name is not a name.
pub(super) fn clean_process_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PROCESS_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(unix)]
pub(super) fn describe(pid: i32) -> ProcessView {
    ProcessView {
        pid,
        name: read_name(pid).as_deref().and_then(clean_process_name),
        cwd: read_cwd(pid),
    }
}

#[cfg(target_os = "macos")]
fn read_cwd(pid: i32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    // SAFETY: the buffer is exactly one `proc_vnodepathinfo`, and it is read
    // only after the kernel reports that it filled every byte of it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(size).ok()?,
        )
    };
    if usize::try_from(written).ok() != Some(size) {
        return None;
    }
    // SAFETY: fully initialised, established above.
    let info = unsafe { info.assume_init() };
    // `vip_path` is MAXPATHLEN bytes split into rows for old rustc; it is one
    // contiguous NUL-terminated C string.
    let raw: &[libc::c_char] = info.pvi_cdir.vip_path.as_flattened();
    // SAFETY: c_char and u8 share size and alignment.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast(), raw.len()) };
    let path = CStr::from_bytes_until_nul(bytes).ok()?.to_bytes();
    if path.first() != Some(&b'/') {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

#[cfg(target_os = "macos")]
fn read_name(pid: i32) -> Option<String> {
    // MAXCOMLEN is 16; proc_name can return the longer p_name (2 * MAXCOMLEN).
    let mut buf = [0u8; 64];
    // SAFETY: the length passed is the buffer's own length.
    let written = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(written)
        .ok()
        .filter(|n| *n > 0 && *n <= buf.len())?;
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

#[cfg(target_os = "linux")]
fn read_cwd(pid: i32) -> Option<PathBuf> {
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    // A deleted directory reads back as "/old/path (deleted)"; that is not a
    // place a new shell can start.
    path.is_absolute().then_some(path).filter(|p| p.is_dir())
}

#[cfg(target_os = "linux")]
fn read_name(pid: i32) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(format!("/proc/{pid}/comm"))
        .ok()?
        .take(256)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn read_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn read_name(_pid: i32) -> Option<String> {
    None
}

/// Upper bound on the processes one [`working_in`] scan inspects. A process
/// table larger than this is reported as a scan that could not finish, not
/// judged on a prefix of it.
const MAX_SCANNED_PROCESSES: usize = 65_536;

/// Every process, other than this one, whose working directory is `dir` or
/// inside it — the processes still *in* a checkout, whoever started them.
///
/// Used to prove an agent attempt over before its worktree is handed to the
/// next one: the agent's recorded process being gone says nothing about a
/// descendant that left its session (`setsid`, a double fork, `nohup`), but
/// such a process keeps the directory it was started in. A process whose
/// directory the OS will not describe — another user's — is skipped: it cannot
/// be the agent's, which runs as this user. `Err` when the process table
/// itself could not be read; that is never "nobody is there".
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn working_in(dir: &std::path::Path) -> Result<Vec<i32>, String> {
    let me = i32::try_from(std::process::id()).unwrap_or(-1);
    let mut found = Vec::new();
    for pid in all_pids()? {
        if pid <= 0 || pid == me {
            continue;
        }
        if read_cwd(pid).is_some_and(|cwd| cwd.starts_with(dir)) {
            found.push(pid);
        }
    }
    Ok(found)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn working_in(_dir: &std::path::Path) -> Result<Vec<i32>, String> {
    Err("listing the processes working in a directory is not supported on this platform".into())
}

#[cfg(target_os = "macos")]
fn all_pids() -> Result<Vec<i32>, String> {
    // Asked twice: once for the count, once with room to spare, because the
    // table can grow between the calls.
    // SAFETY: a null buffer asks only for the number of pids.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let count =
        usize::try_from(count).map_err(|_| "the process table could not be read".to_string())?;
    if count > MAX_SCANNED_PROCESSES {
        return Err(format!(
            "{count} processes are running, more than the {MAX_SCANNED_PROCESSES} this scan reads"
        ));
    }
    let mut pids = vec![0i32; count + 64];
    let bytes = i32::try_from(pids.len() * std::mem::size_of::<i32>())
        .map_err(|_| "the process table is too large to read".to_string())?;
    // SAFETY: `bytes` is the buffer's own size in bytes.
    let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let written =
        usize::try_from(written).map_err(|_| "the process table could not be read".to_string())?;
    if written >= pids.len() {
        return Err("the process table grew while it was being read".into());
    }
    pids.truncate(written);
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn all_pids() -> Result<Vec<i32>, String> {
    let entries = std::fs::read_dir("/proc")
        .map_err(|e| format!("the process table could not be read: {e}"))?;
    let mut pids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("the process table could not be read: {e}"))?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        {
            pids.push(pid);
            if pids.len() > MAX_SCANNED_PROCESSES {
                return Err(format!(
                    "more than {MAX_SCANNED_PROCESSES} processes are running"
                ));
            }
        }
    }
    Ok(pids)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child started in a directory is found there — and still found after
    /// it has left its parent's session, which is what an escaped descendant
    /// does — while a directory nobody works in reports nobody.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_process_working_in_a_directory_is_found_even_outside_its_parents_group() {
        use crate::procguard::LockedSpawn;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().expect("dir");
        let dir = dir.path().canonicalize().expect("canonical");
        let empty = tempfile::tempdir().expect("empty");
        let empty = empty.path().canonicalize().expect("canonical");
        let nested = dir.join("sub");
        std::fs::create_dir(&nested).expect("nested");
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .current_dir(&nested)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn_locked()
            .expect("spawn");
        let pid = i32::try_from(child.id()).expect("pid");
        let mut seen = Vec::new();
        for _ in 0..50 {
            seen = working_in(&dir).expect("scan");
            if seen.contains(&pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen.contains(&pid), "{pid} not found in {seen:?}");
        assert!(working_in(&empty).expect("scan").is_empty());
    }

    #[test]
    fn process_names_are_printable_bounded_and_never_blank() {
        assert_eq!(clean_process_name("vim"), Some("vim".into()));
        assert_eq!(clean_process_name("  zsh\n"), Some("zsh".into()));
        assert_eq!(
            clean_process_name("\x1b]0;evil\x07"),
            Some("]0;evil".into())
        );
        assert_eq!(clean_process_name(""), None);
        assert_eq!(clean_process_name(" \t\r\n"), None);
        let long = "x".repeat(500);
        assert_eq!(
            clean_process_name(&long).map(|s| s.chars().count()),
            Some(MAX_PROCESS_NAME_CHARS)
        );
        let wide = "é".repeat(500);
        assert_eq!(
            clean_process_name(&wide).map(|s| s.chars().count()),
            Some(MAX_PROCESS_NAME_CHARS)
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn this_process_describes_its_own_directory_and_name() {
        let me = i32::try_from(std::process::id()).unwrap();
        let view = describe(me);
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        let got = view
            .cwd
            .expect("own cwd is readable")
            .canonicalize()
            .unwrap();
        assert_eq!(got, expected);
        assert!(view.name.is_some(), "own process name is readable");
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_does_not_exist_describes_nothing() {
        // pid_max on macOS is 99999 and on Linux at most 4194304.
        let view = describe(i32::MAX - 7);
        assert_eq!(view.cwd, None);
        assert_eq!(view.name, None);
    }
}
