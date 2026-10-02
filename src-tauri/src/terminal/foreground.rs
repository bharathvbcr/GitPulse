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

#[cfg(test)]
mod tests {
    use super::*;

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
