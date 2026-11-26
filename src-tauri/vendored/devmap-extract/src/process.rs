//! Whether a process id names a running process, on Windows.
//!
//! Unix callers ask `kill(pid, 0)`; Windows has no such call, and a caller that
//! never asked treated every pid as live, so a staging file or lock left by a
//! killed process was never reclaimed. One owner here, so the hook's build lock
//! and the store's seed sweep agree on what "running" means.

/// Whether `pid` names a running process. Unknown is held (reads as running),
/// like every other unknown owner: a process this user may not query still
/// exists, and a failed exit-code read proves nothing about it.
pub fn windows_is_running(pid: u32) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(process: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    // SAFETY: plain Win32 calls on a handle this function opens and closes.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // A process this user may not query still exists; anything else
            // (invalid parameter) is no process with that pid.
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        !ok || code == STILL_ACTIVE
    }
}
