//! What a live PTY session is doing right now: the program in the terminal's
//! foreground and the directory it is working in.
//!
//! Read on demand, never polled. Every answer names its own gaps: a process
//! the OS would not describe is `None`, not the repository root, because a
//! guessed directory would open the next tab somewhere the user never went.

#[cfg(unix)]
use std::path::PathBuf;

/// Longest program name this module reports. A process can rename itself to
/// anything; a tab title is not the place to discover how long.
#[cfg(unix)]
pub(super) const MAX_PROCESS_NAME_CHARS: usize = 64;

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessView {
    pub pid: i32,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Keeps printable characters only and clips to [`MAX_PROCESS_NAME_CHARS`].
/// An empty result is `None`: a blank name is not a name.
#[cfg(unix)]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_cwd(pid: i32) -> Option<PathBuf> {
    crate::sysload::procscan::cwd_of(pid)
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

/// Every process, other than this one, whose working directory is `dir` or
/// inside it — the processes still *in* a checkout, whoever started them.
///
/// Used to prove an agent attempt over before its worktree is handed to the
/// next one: the agent's recorded process being gone says nothing about a
/// descendant that left its session (`setsid`, a double fork, `nohup`), but
/// such a process keeps the directory it was started in. A process whose
/// directory the OS will not describe — another user's — is skipped: it cannot
/// be the agent's, which runs as this user. `Err` when the process table
/// itself could not be read; that is never "nobody is there". The table walk
/// is the host-load scan's (`sysload::procscan`), the one owner of it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn working_in(dir: &std::path::Path) -> Result<Vec<i32>, String> {
    crate::sysload::procscan::working_in(dir)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn working_in(_dir: &std::path::Path) -> Result<Vec<i32>, String> {
    Err("listing the processes working in a directory is not supported on this platform".into())
}

/// "name (pid N)" for up to five of `pids`, then a count of the rest: enough
/// to find a process by, never a listing of the machine.
#[cfg(unix)]
pub(crate) fn describe_pids(pids: &[i32]) -> String {
    const SHOWN: usize = 5;
    let mut parts: Vec<String> = pids
        .iter()
        .take(SHOWN)
        .map(|pid| match describe(*pid).name {
            Some(name) => format!("{name}, pid {pid}"),
            None => format!("pid {pid}"),
        })
        .collect();
    if pids.len() > SHOWN {
        parts.push(format!("and {} more", pids.len() - SHOWN));
    }
    parts.join("; ")
}

#[cfg(not(unix))]
pub(crate) fn describe_pids(pids: &[i32]) -> String {
    pids.iter()
        .map(|pid| format!("pid {pid}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every other process in the session `sid`, with its process group.
///
/// A terminal's shell leads a session of its own, and every job it starts
/// stays in it: a job-control shell puts each job in a group of its own
/// (`set -m`, any interactive shell), so the tty's foreground group names at
/// most one of them. A process that leaves the session (`setsid`, a daemon)
/// is not here and is not this terminal's to stop. `Err` when the process
/// table could not be read; that is never "nobody is there".
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn session_members(sid: i32) -> Result<Vec<(i32, i32)>, String> {
    let mut found = Vec::new();
    for pid in crate::sysload::procscan::all_pids()? {
        if pid <= 0 || pid == sid {
            continue;
        }
        // SAFETY: getsid/getpgid take numeric ids and read nothing else; a
        // process that exited between the listing and here answers -1.
        let (session, group) = unsafe { (libc::getsid(pid), libc::getpgid(pid)) };
        if session == sid && group > 1 {
            found.push((pid, group));
        }
    }
    Ok(found)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn session_members(_sid: i32) -> Result<Vec<(i32, i32)>, String> {
    Err("listing a terminal session's processes is not supported on this platform".into())
}

#[cfg(all(test, unix))]
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

    #[cfg(unix)]
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
