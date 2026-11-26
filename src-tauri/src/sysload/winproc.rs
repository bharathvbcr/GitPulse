//! Win32 process access, in one place.
//!
//! Two callers share it: the Windows process scan (`procscan::os`) and the
//! process-identity reader (`workbench::process_birth`). Every `extern`
//! declaration lives in [`sys`], hand-declared: GitPulse has no Win32 binding
//! crate, and the surface here is a dozen functions.
//!
//! What the scan reads from another process is its *parameter block*: the
//! working directory, command line and environment the loader copied there.
//! That is process memory, reached through the PEB:
//!
//! ```text
//! NtQueryInformationProcess(ProcessBasicInformation) -> PebBaseAddress
//! PEB.ProcessParameters (+0x20)         -> RTL_USER_PROCESS_PARAMETERS*
//! RTL_USER_PROCESS_PARAMETERS
//!   .Flags                  (+0x08)     bit 0 = pointers are absolute
//!   .CurrentDirectory.DosPath (+0x38)   UNICODE_STRING
//!   .CommandLine            (+0x70)     UNICODE_STRING
//!   .Environment            (+0x80)     PVOID
//!   .EnvironmentSize        (+0x3F0)    SIZE_T, bytes
//! ```
//!
//! Sources, checked against the headers themselves rather than recalled:
//! `PEB` and `PROCESS_BASIC_INFORMATION` in the Windows SDK `winternl.h`
//! (Microsoft's documented layout, mirrored by mingw-w64's `winternl.h`) give
//! `ProcessParameters` at 0x20 and `PebBaseAddress` at 0x08 on a 64-bit
//! target. `RTL_USER_PROCESS_PARAMETERS`, `CURDIR`, `RTL_DRIVE_LETTER_CURDIR`
//! and `RTL_USER_PROC_PARAMS_NORMALIZED` are not in the public SDK header; they
//! are taken from System Informer's `phnt` (`ntrtl.h`), whose field order,
//! with `RTL_MAX_DRIVE_LETTERS` = 32, puts `CurrentDirectory` at 0x38,
//! `CommandLine` at 0x70, `Environment` at 0x80 and `EnvironmentSize` at 0x3F0.
//! The mirror structs below carry `offset_of!` assertions for exactly those
//! numbers, so a transcription error fails the build rather than reading the
//! wrong bytes. The layout is a function of pointer width, not of the CPU
//! family, so aarch64 Windows uses the same offsets; that is inferred from the
//! definitions, not observed.
//!
//! None of the `windows` code here has been run by its author: it is written
//! on macOS and compiled by a cross `clippy`. The Windows-only tests in
//! `procscan` are what exercise it.

// The pure helpers below are what the Windows reader and the tests call; on
// other platforms only the tests do.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;

/// Most bytes read for one `UNICODE_STRING` (a working directory or a command
/// line). Windows bounds a command line at 32,767 UTF-16 units, 65,534 bytes,
/// so a smaller cap would refuse legitimate lines; a larger length is a
/// corrupt or hostile block and is refused, never truncated.
pub(crate) const MAX_STRING_BYTES: usize = 64 * 1024;

/// Most bytes read for an environment block. The same bound Linux's
/// `/proc/<pid>/environ` read uses.
pub(crate) const MAX_ENV_BYTES: usize = 1 << 20;

/// `.exe`, case-insensitively, off the end of an executable's file name: the
/// classifiers compare `claude`, not `claude.exe`. A bare `.exe` is a name, not
/// a suffix, and stays.
pub(crate) fn exe_stem(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(at)
            if at > 0 && name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".exe") =>
        {
            &name[..at]
        }
        _ => name,
    }
}

/// A working directory as the PEB spells it, as a path. The loader keeps the
/// trailing backslash (`C:\work\lane\`); a directory is not spelled that way
/// anywhere else, so it goes, except from a drive or share root where it is
/// part of the name.
pub(crate) fn cwd_path(text: String) -> PathBuf {
    let trimmed = text
        .strip_suffix('\\')
        .filter(|rest| !rest.is_empty() && !rest.ends_with(':') && !rest.ends_with('\\'));
    PathBuf::from(trimmed.unwrap_or(&text))
}

/// UTF-16LE bytes as code units. An odd trailing byte is dropped; callers
/// have already refused an odd length.
pub(crate) fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect()
}

/// UTF-16LE bytes as text; unpaired surrogates become U+FFFD.
pub(crate) fn decode_utf16(bytes: &[u8]) -> String {
    String::from_utf16_lossy(&utf16_units(bytes))
}

/// The `NAME=value` entries of an environment block: NUL-terminated strings,
/// ended by an empty one. Entries such as `=C:=C:\x` (a per-drive directory)
/// start with `=` and are kept.
pub(crate) fn split_environment(bytes: &[u8]) -> Vec<String> {
    utf16_units(bytes)
        .split(|unit| *unit == 0)
        .take_while(|entry| !entry.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// The SID inside a `TOKEN_USER` buffer. `TOKEN_USER` is one
/// `SID_AND_ATTRIBUTES`, whose first field points at the SID, stored in the
/// same buffer after it. `None` for a pointer that does not land wholly inside
/// the buffer, or a SID whose header is not one: a SID is a revision byte, a
/// sub-authority count (at most 15), a six-byte authority and that many
/// four-byte sub-authorities.
pub(crate) fn sid_in_token_user(buffer: &[u8]) -> Option<Vec<u8>> {
    let pointer = u64::from_ne_bytes(buffer.get(..8)?.try_into().ok()?);
    let start = usize::try_from(pointer.checked_sub(buffer.as_ptr() as u64)?).ok()?;
    let header = buffer.get(start..start.checked_add(8)?)?;
    let count = usize::from(header[1]);
    if header[0] != 1 || count == 0 || count > 15 {
        return None;
    }
    let length = 8 + 4 * count;
    buffer
        .get(start..start.checked_add(length)?)
        .map(<[u8]>::to_vec)
}

/// 1601-01-01 to 1970-01-01, in 100 ns `FILETIME` ticks.
const UNIX_EPOCH_TICKS: u128 = 116_444_736_000_000_000;

/// A process creation time (100 ns ticks since 1601) as nanoseconds since the
/// Unix epoch. The one conversion: `process_birth` records start times through
/// it and the scan reports them through it, so the two cannot disagree.
pub(crate) fn unix_nanos(creation_ticks: u64) -> u128 {
    (u128::from(creation_ticks) * 100).saturating_sub(UNIX_EPOCH_TICKS * 100)
}

/// Why one `UNICODE_STRING` was not read.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StringRefusal {
    /// A UTF-16 string has an even byte length.
    OddLength,
    /// `Length` past `MaximumLength`: the field is not a string's.
    PastMaximum,
    /// Longer than the read bound.
    TooLong,
    /// A length with no buffer.
    NoBuffer,
}

/// The `UNICODE_STRING` of a 64-bit target: `Length` and `MaximumLength` in
/// bytes, then the pointer at +8.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    _padding: u32,
    pub buffer: u64,
}

#[cfg(any(all(windows, target_pointer_width = "64"), test))]
impl UnicodeString {
    #[cfg(test)]
    pub(crate) fn new(length: u16, maximum_length: u16, buffer: u64) -> Self {
        Self {
            length,
            maximum_length,
            _padding: 0,
            buffer,
        }
    }

    /// Where and how much to read, `None` for the empty string, or the reason
    /// the field cannot be trusted. Checked before any byte of it is read.
    pub(crate) fn extent(&self, cap: usize) -> Result<Option<(u64, usize)>, StringRefusal> {
        let length = usize::from(self.length);
        if length == 0 {
            return Ok(None);
        }
        if length % 2 != 0 {
            return Err(StringRefusal::OddLength);
        }
        if self.length > self.maximum_length {
            return Err(StringRefusal::PastMaximum);
        }
        if length > cap {
            return Err(StringRefusal::TooLong);
        }
        if self.buffer == 0 {
            return Err(StringRefusal::NoBuffer);
        }
        Ok(Some((self.buffer, length)))
    }
}

/// `RTL_DRIVE_LETTER_CURDIR`; its `STRING` has the shape of a
/// [`UnicodeString`].
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct DriveLetterCurdir {
    flags: u16,
    length: u16,
    timestamp: u32,
    dos_path: UnicodeString,
}

/// `RTL_USER_PROCESS_PARAMETERS` of a 64-bit target, as far as
/// `EnvironmentSize`. Pointers are `u64`, so the layout is the same on every
/// 64-bit host, which lets the assertions below run wherever this compiles.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
pub(crate) struct ProcessParameters {
    maximum_length: u32,
    length: u32,
    pub flags: u32,
    debug_flags: u32,
    console_handle: u64,
    console_flags: u32,
    standard_input: u64,
    standard_output: u64,
    standard_error: u64,
    pub current_directory: UnicodeString,
    current_directory_handle: u64,
    dll_path: UnicodeString,
    image_path_name: UnicodeString,
    pub command_line: UnicodeString,
    pub environment: u64,
    starting_x: u32,
    starting_y: u32,
    count_x: u32,
    count_y: u32,
    count_chars_x: u32,
    count_chars_y: u32,
    fill_attribute: u32,
    window_flags: u32,
    show_window_flags: u32,
    window_title: UnicodeString,
    desktop_info: UnicodeString,
    shell_info: UnicodeString,
    runtime_data: UnicodeString,
    current_directories: [DriveLetterCurdir; 32],
    pub environment_size: u64,
}

/// `PROCESS_BASIC_INFORMATION` of a 64-bit target.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: u64,
    affinity_mask: u64,
    base_priority: i32,
    unique_process_id: u64,
    inherited_from_unique_process_id: u64,
}

/// `PEB.ProcessParameters`.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
const PEB_PROCESS_PARAMETERS: u64 = 0x20;

/// `RTL_USER_PROC_PARAMS_NORMALIZED`: the string pointers are absolute. Clear,
/// they are offsets from the block, and reading them as addresses would read
/// unrelated memory.
#[cfg(any(all(windows, target_pointer_width = "64"), test))]
const PARAMS_NORMALIZED: u32 = 0x1;

#[cfg(all(target_pointer_width = "64", any(windows, test)))]
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<UnicodeString>() == 16);
    assert!(size_of::<ProcessBasicInformation>() == 48);
    assert!(offset_of!(ProcessBasicInformation, peb_base_address) == 0x08);
    assert!(offset_of!(ProcessParameters, flags) == 0x08);
    assert!(offset_of!(ProcessParameters, current_directory) == 0x38);
    assert!(offset_of!(ProcessParameters, image_path_name) == 0x60);
    assert!(offset_of!(ProcessParameters, command_line) == 0x70);
    assert!(offset_of!(ProcessParameters, environment) == 0x80);
    assert!(offset_of!(ProcessParameters, current_directories) == 0xF0);
    assert!(offset_of!(ProcessParameters, environment_size) == 0x3F0);
};

#[cfg(windows)]
pub(crate) mod sys {
    use super::{cwd_path, decode_utf16, sid_in_token_user, split_environment, utf16_units};
    #[cfg(target_pointer_width = "64")]
    use super::{
        ProcessBasicInformation, ProcessParameters, UnicodeString, MAX_ENV_BYTES, MAX_STRING_BYTES,
        PARAMS_NORMALIZED, PEB_PROCESS_PARAMETERS,
    };
    use std::ffi::c_void;
    use std::io;
    #[cfg(target_pointer_width = "64")]
    use std::path::PathBuf;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        fn ticks(&self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }

    /// `PROCESSENTRY32W`.
    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        base_priority: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn QueryFullProcessImageNameW(
            process: *mut c_void,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn ReadProcessMemory(
            process: *mut c_void,
            base: *const c_void,
            buffer: *mut c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(command_line: *const u16, count: *mut i32) -> *mut *mut u16;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process: *mut c_void,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    /// `PROCESS_QUERY_LIMITED_INFORMATION`: enough for times, image path,
    /// the token, and `NtQueryInformationProcess`'s basic and WoW64 classes.
    pub(crate) const QUERY_LIMITED: u32 = 0x1000;
    /// `PROCESS_VM_READ`.
    pub(crate) const VM_READ: u32 = 0x0010;
    /// `TOKEN_QUERY`.
    const TOKEN_QUERY: u32 = 0x0008;
    /// `TOKEN_INFORMATION_CLASS::TokenUser`.
    const TOKEN_USER: u32 = 1;
    /// `ProcessBasicInformation` and `ProcessWow64Information` of
    /// `PROCESSINFOCLASS` (`ntpsapi.h`).
    const PROCESS_BASIC_INFORMATION: u32 = 0;
    const PROCESS_WOW64_INFORMATION: u32 = 26;
    /// `TH32CS_SNAPPROCESS`.
    const SNAP_PROCESS: u32 = 0x2;
    /// What `OpenProcess` reports for a pid no process holds.
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// What `Process32NextW` reports at the end of the snapshot.
    const ERROR_NO_MORE_FILES: i32 = 18;

    /// A kernel handle, closed exactly once when it drops.
    pub(crate) struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: this value owns the handle and nothing else closes it.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Why a process could not be opened.
    #[derive(Debug)]
    pub(crate) enum OpenFail {
        /// No process holds the pid.
        Gone,
        /// Access control refused it: another user's, protected, or
        /// outranking this process.
        Denied(String),
        Other(String),
    }

    impl OpenFail {
        pub(crate) fn message(self) -> String {
            match self {
                Self::Gone => "no such process".into(),
                Self::Denied(text) | Self::Other(text) => text,
            }
        }
    }

    /// A handle to `pid` with `access`.
    pub(crate) fn open(pid: u32, access: u32) -> Result<Handle, OpenFail> {
        // SAFETY: a query-only open names no caller memory and inherits nothing.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if !handle.is_null() {
            return Ok(Handle(handle));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(ERROR_INVALID_PARAMETER) => Err(OpenFail::Gone),
            Some(ERROR_ACCESS_DENIED) => Err(OpenFail::Denied(error.to_string())),
            _ => Err(OpenFail::Other(error.to_string())),
        }
    }

    /// What `GetProcessTimes` says.
    pub(crate) struct Times {
        /// Creation, in 100 ns ticks since 1601.
        pub creation_ticks: u64,
        /// An exit time is set: the process has ended, though another handle
        /// keeps it answering.
        pub exited: bool,
        /// Kernel plus user time, in 100 ns ticks.
        pub cpu_ticks: u64,
    }

    pub(crate) fn times(handle: &Handle) -> Result<Times, String> {
        let (mut creation, mut exit, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        // SAFETY: the handle is live and each pointer targets a writable FILETIME.
        let ok =
            unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) };
        if ok == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        Ok(Times {
            creation_ticks: creation.ticks(),
            exited: exit.ticks() != 0,
            cpu_ticks: kernel.ticks().saturating_add(user.ticks()),
        })
    }

    /// One row of the process snapshot.
    pub(crate) struct ProcEntry {
        pub pid: u32,
        /// The parent as of creation; Windows reuses pids, so this is a claim
        /// to check against start times, not an identity.
        pub parent: u32,
        /// The image's file name (`claude.exe`).
        pub name: String,
    }

    /// Every process, from a Toolhelp snapshot. `Err` when the snapshot or its
    /// walk failed, or holds more than `limit`: never a short list standing in
    /// for the table.
    pub(crate) fn snapshot(limit: usize) -> Result<Vec<ProcEntry>, String> {
        // SAFETY: a snapshot request names no caller memory.
        let raw = unsafe { CreateToolhelp32Snapshot(SNAP_PROCESS, 0) };
        // INVALID_HANDLE_VALUE is -1.
        if raw.is_null() || raw as usize == usize::MAX {
            return Err(format!(
                "the process table could not be read: {}",
                io::Error::last_os_error()
            ));
        }
        let snapshot = Handle(raw);
        // SAFETY: an all-zero entry is valid for this plain-data struct.
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut out = Vec::new();
        // SAFETY: the snapshot is live and `entry` is a writable, sized
        // PROCESSENTRY32W, as both calls require.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        loop {
            if !more {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                    return Ok(out);
                }
                return Err(format!("the process table could not be read: {error}"));
            }
            if out.len() >= limit {
                return Err(format!("more than {limit} processes are running"));
            }
            let end = entry
                .exe_file
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(entry.exe_file.len());
            out.push(ProcEntry {
                pid: entry.process_id,
                parent: entry.parent_process_id,
                name: String::from_utf16_lossy(&entry.exe_file[..end]),
            });
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
    }

    /// The path of the image `handle` runs, as the kernel names it.
    pub(crate) fn image_path(handle: &Handle) -> Option<String> {
        // 32,767 units is the longest path Windows names.
        let mut buffer = vec![0u16; 32_768];
        let mut size = u32::try_from(buffer.len()).ok()?;
        // SAFETY: `buffer` holds `size` writable units, which the call updates
        // to the length it wrote.
        let ok = unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut size) };
        if ok == 0 {
            return None;
        }
        let written = usize::try_from(size).ok()?.min(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..written]))
    }

    /// Whose a process is, by its token's user SID against this process's.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Owner {
        Ours,
        /// A readable token that names another user.
        Other,
        /// The token could not be read (access control, or this process's own
        /// could not be): not "someone else's".
        Unknown,
    }

    fn token_user(process: *mut c_void) -> Option<Vec<u8>> {
        let mut token: *mut c_void = std::ptr::null_mut();
        // SAFETY: `process` is a live handle or the current-process pseudo
        // handle, and `token` is a writable out-pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = Handle(token);
        // Eight-byte aligned, because TOKEN_USER holds a pointer.
        let mut buffer = [0u64; 64];
        let mut returned = 0u32;
        // SAFETY: `buffer` is 512 writable bytes, the length passed.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TOKEN_USER,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&buffer) as u32,
                &mut returned,
            )
        };
        if ok == 0 {
            return None;
        }
        let used = usize::try_from(returned)
            .ok()?
            .min(std::mem::size_of_val(&buffer));
        // SAFETY: `used` bytes of `buffer` were written; u64 to u8 widens no
        // alignment requirement.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), used) };
        sid_in_token_user(bytes)
    }

    pub(crate) fn owner_of(handle: &Handle) -> Owner {
        static OWN: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
        // SAFETY: GetCurrentProcess has no preconditions.
        let own = OWN.get_or_init(|| token_user(unsafe { GetCurrentProcess() }));
        match (token_user(handle.0), own) {
            (Some(theirs), Some(own)) if theirs == *own => Owner::Ours,
            (Some(_), Some(_)) => Owner::Other,
            _ => Owner::Unknown,
        }
    }

    /// Another process's parameter block, read through its PEB. 64-bit
    /// targets only: a 32-bit build cannot walk a 64-bit PEB.
    #[cfg(target_pointer_width = "64")]
    pub(crate) struct Remote<'a> {
        handle: &'a Handle,
        block: ProcessParameters,
    }

    #[cfg(target_pointer_width = "64")]
    fn read_exact(handle: &Handle, address: u64, buffer: &mut [u8]) -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        let mut read = 0usize;
        // SAFETY: `buffer` is writable for its length and `read` is a live usize.
        let ok = unsafe {
            ReadProcessMemory(
                handle.0,
                address as usize as *const c_void,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut read,
            )
        };
        if ok == 0 {
            return Err(format!(
                "reading process memory: {}",
                io::Error::last_os_error()
            ));
        }
        if read != buffer.len() {
            return Err("reading process memory: partial read".into());
        }
        Ok(())
    }

    /// A plain-data struct read from another process. Only for the two
    /// integer-only mirror structs above, for which every bit pattern is valid.
    #[cfg(target_pointer_width = "64")]
    fn read_struct<T: Copy + Default>(handle: &Handle, address: u64) -> Result<T, String> {
        let mut value = T::default();
        // SAFETY: `value` is a live T, viewed as the bytes it occupies; T is
        // integers only, so whatever is written back is a valid T.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut value as *mut T).cast::<u8>(),
                std::mem::size_of::<T>(),
            )
        };
        read_exact(handle, address, bytes)?;
        Ok(value)
    }

    #[cfg(target_pointer_width = "64")]
    impl<'a> Remote<'a> {
        /// Locates `handle`'s parameter block. `handle` needs
        /// [`QUERY_LIMITED`] and [`VM_READ`]. A 32-bit (WoW64) process is
        /// refused: its 64-bit PEB is not where it keeps its working
        /// directory, and a wrong answer is worse than none.
        pub(crate) fn of(handle: &'a Handle) -> Result<Self, String> {
            let mut wow64 = 0usize;
            // SAFETY: `wow64` is a writable pointer-sized out-value, the
            // length of the ULONG_PTR this class returns.
            let status = unsafe {
                NtQueryInformationProcess(
                    handle.0,
                    PROCESS_WOW64_INFORMATION,
                    (&mut wow64 as *mut usize).cast(),
                    std::mem::size_of::<usize>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if status < 0 {
                return Err(format!("NtQueryInformationProcess failed: {status:#x}"));
            }
            if wow64 != 0 {
                return Err("a 32-bit process: its parameters are not read".into());
            }
            let mut basic = ProcessBasicInformation::default();
            // SAFETY: `basic` is a writable PROCESS_BASIC_INFORMATION of the
            // length passed.
            let status = unsafe {
                NtQueryInformationProcess(
                    handle.0,
                    PROCESS_BASIC_INFORMATION,
                    (&mut basic as *mut ProcessBasicInformation).cast(),
                    std::mem::size_of::<ProcessBasicInformation>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if status < 0 {
                return Err(format!("NtQueryInformationProcess failed: {status:#x}"));
            }
            if basic.peb_base_address == 0 {
                return Err("the process has no PEB".into());
            }
            let mut pointer = [0u8; 8];
            read_exact(
                handle,
                basic
                    .peb_base_address
                    .saturating_add(PEB_PROCESS_PARAMETERS),
                &mut pointer,
            )?;
            let block_at = u64::from_le_bytes(pointer);
            if block_at == 0 {
                return Err("the process has no parameter block".into());
            }
            let block: ProcessParameters = read_struct(handle, block_at)?;
            if block.flags & PARAMS_NORMALIZED == 0 {
                return Err("the parameter block is not normalized".into());
            }
            Ok(Self { handle, block })
        }

        fn string(&self, text: &UnicodeString, cap: usize) -> Result<Vec<u8>, String> {
            let Some((address, length)) = text.extent(cap).map_err(|why| format!("{why:?}"))?
            else {
                return Ok(Vec::new());
            };
            let mut bytes = vec![0u8; length];
            read_exact(self.handle, address, &mut bytes)?;
            Ok(bytes)
        }

        /// The working directory.
        pub(crate) fn cwd(&self) -> Result<PathBuf, String> {
            let bytes = self.string(&self.block.current_directory, MAX_STRING_BYTES)?;
            if bytes.is_empty() {
                return Err("the process has no working directory".into());
            }
            Ok(cwd_path(decode_utf16(&bytes)))
        }

        /// The command line split the way the C runtime does
        /// (`CommandLineToArgvW`).
        pub(crate) fn argv(&self) -> Result<Vec<String>, String> {
            let bytes = self.string(&self.block.command_line, MAX_STRING_BYTES)?;
            // `CommandLineToArgvW("")` answers with the program's own path, so
            // an empty line is answered here instead.
            if bytes.is_empty() {
                return Ok(Vec::new());
            }
            let mut line = utf16_units(&bytes);
            line.push(0);
            let mut count = 0i32;
            // SAFETY: `line` is NUL-terminated and outlives the call.
            let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut count) };
            if argv.is_null() {
                return Err(format!(
                    "CommandLineToArgvW: {}",
                    io::Error::last_os_error()
                ));
            }
            let mut out = Vec::new();
            for index in 0..usize::try_from(count).unwrap_or(0) {
                // SAFETY: the call returned `count` valid, NUL-terminated
                // argument pointers.
                let argument = unsafe { *argv.add(index) };
                let mut length = 0usize;
                // SAFETY: each argument is NUL-terminated within the command
                // line's length, which bounds the scan.
                while length <= MAX_STRING_BYTES / 2 && unsafe { *argument.add(length) } != 0 {
                    length += 1;
                }
                // SAFETY: `length` units were just read from this argument.
                let units = unsafe { std::slice::from_raw_parts(argument, length) };
                out.push(String::from_utf16_lossy(units));
            }
            // SAFETY: `argv` came from CommandLineToArgvW, which documents
            // LocalFree as the release.
            unsafe { LocalFree(argv.cast()) };
            Ok(out)
        }

        /// The environment, `NAME=value` entries. Refused (not read as empty)
        /// when the block's address or size is absent, odd or over
        /// [`MAX_ENV_BYTES`].
        pub(crate) fn env(&self) -> Result<Vec<String>, String> {
            let size = usize::try_from(self.block.environment_size)
                .map_err(|_| "the environment size is not a size")?;
            if self.block.environment == 0 || size == 0 {
                return Err("the process has no environment block".into());
            }
            if size % 2 != 0 || size > MAX_ENV_BYTES {
                return Err(format!("the environment block size {size} is not readable"));
            }
            let mut bytes = vec![0u8; size];
            read_exact(self.handle, self.block.environment, &mut bytes)?;
            Ok(split_environment(&bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn a_trailing_exe_is_dropped_whatever_its_case() {
        assert_eq!(exe_stem("claude.exe"), "claude");
        assert_eq!(exe_stem("Antigravity.EXE"), "Antigravity");
        assert_eq!(exe_stem("node.Exe"), "node");
        assert_eq!(exe_stem("claude"), "claude");
        assert_eq!(exe_stem("claude.exe.bak"), "claude.exe.bak");
        assert_eq!(exe_stem(".exe"), ".exe");
        assert_eq!(exe_stem("exe"), "exe");
        assert_eq!(exe_stem(""), "");
        // A multi-byte character that straddles the cut is not a suffix.
        assert_eq!(exe_stem("é.exé"), "é.exé");
    }

    #[test]
    fn the_loaders_trailing_backslash_goes_except_from_a_root() {
        assert_eq!(
            cwd_path(r"C:\work\lane\".into()),
            PathBuf::from(r"C:\work\lane")
        );
        assert_eq!(
            cwd_path(r"C:\work\lane".into()),
            PathBuf::from(r"C:\work\lane")
        );
        assert_eq!(cwd_path(r"C:\".into()), PathBuf::from(r"C:\"));
        assert_eq!(cwd_path(r"\\".into()), PathBuf::from(r"\\"));
        assert_eq!(
            cwd_path(r"\\host\share\dir\".into()),
            PathBuf::from(r"\\host\share\dir")
        );
    }

    #[test]
    fn a_string_is_read_only_when_its_header_is_a_strings() {
        let ok = UnicodeString::new(8, 10, 0x1000);
        assert_eq!(ok.extent(64), Ok(Some((0x1000, 8))));
        // Empty needs no buffer and no read.
        assert_eq!(UnicodeString::new(0, 0, 0).extent(64), Ok(None));
        assert_eq!(
            UnicodeString::new(7, 8, 0x1000).extent(64),
            Err(StringRefusal::OddLength)
        );
        assert_eq!(
            UnicodeString::new(12, 8, 0x1000).extent(64),
            Err(StringRefusal::PastMaximum)
        );
        assert_eq!(
            UnicodeString::new(66, 66, 0x1000).extent(64),
            Err(StringRefusal::TooLong)
        );
        assert_eq!(
            UnicodeString::new(8, 8, 0).extent(64),
            Err(StringRefusal::NoBuffer)
        );
        // The bound is inclusive: a string of exactly the cap is read.
        assert_eq!(UnicodeString::new(64, 64, 8).extent(64), Ok(Some((8, 64))));
    }

    #[test]
    fn utf16_decodes_and_an_environment_ends_at_the_empty_entry() {
        assert_eq!(decode_utf16(&wide("C:\\lane é")), "C:\\lane é");
        let mut block = wide("A=1");
        block.extend([0, 0]);
        block.extend(wide("=C:=C:\\x"));
        block.extend([0, 0]);
        block.extend(wide("GITPULSE_RUN_ID=abc"));
        block.extend([0, 0, 0, 0]);
        block.extend(wide("AFTER=the-terminator"));
        assert_eq!(
            split_environment(&block),
            ["A=1", "=C:=C:\\x", "GITPULSE_RUN_ID=abc"]
        );
        assert!(split_environment(&[0, 0, 0, 0]).is_empty());
        assert!(split_environment(&[]).is_empty());
    }

    /// A `TOKEN_USER` buffer whose pointer names a SID stored after it, as the
    /// kernel lays one out.
    fn token_user(sid: &[u8]) -> Vec<u8> {
        let mut buffer = vec![0u8; 16 + sid.len()];
        let pointer = buffer.as_ptr() as u64 + 16;
        buffer[..8].copy_from_slice(&pointer.to_ne_bytes());
        buffer[16..].copy_from_slice(sid);
        buffer
    }

    #[test]
    fn a_token_users_sid_is_copied_out_only_when_it_lies_inside_the_buffer() {
        // S-1-5-21-1-2-3-1001: revision 1, five sub-authorities.
        let mut sid = vec![1u8, 5, 0, 0, 0, 0, 0, 5];
        for part in [21u32, 1, 2, 3, 1001] {
            sid.extend(part.to_le_bytes());
        }
        assert_eq!(sid_in_token_user(&token_user(&sid)), Some(sid.clone()));
        // A sub-authority count that runs past the buffer.
        let mut long = sid.clone();
        long[1] = 15;
        assert_eq!(sid_in_token_user(&token_user(&long)), None);
        // An impossible count, and a bad revision.
        let mut many = sid.clone();
        many[1] = 16;
        assert_eq!(sid_in_token_user(&token_user(&many)), None);
        let mut revision = sid.clone();
        revision[0] = 2;
        assert_eq!(sid_in_token_user(&token_user(&revision)), None);
        // A pointer outside the buffer, and a buffer too short to hold one.
        let mut outside = token_user(&sid);
        outside[..8].copy_from_slice(&8u64.to_ne_bytes());
        assert_eq!(sid_in_token_user(&outside), None);
        assert_eq!(sid_in_token_user(&[0u8; 4]), None);
    }

    #[test]
    fn creation_ticks_convert_to_unix_nanoseconds_without_underflow() {
        // 1970-01-01 is 116,444,736,000,000,000 ticks after 1601.
        assert_eq!(unix_nanos(116_444_736_000_000_000), 0);
        assert_eq!(unix_nanos(116_444_736_000_000_001), 100);
        assert_eq!(unix_nanos(0), 0);
    }
}
