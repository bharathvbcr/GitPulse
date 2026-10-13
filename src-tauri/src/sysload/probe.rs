//! Host-wide readings from the operating system, and the clocks and thread
//! controls the sampler needs.
//!
//! macOS reads sysctls, `host_statistics64`, `getloadavg`, `statfs`,
//! `NSProcessInfo.thermalState` and the IOKit power-source estimate. Linux
//! reads PSI and `/proc` (observe-and-advise; the PSI level mapping has not
//! been checked on a real Linux host). Everything else reports every reading
//! as unsupported.

/// A hash of a boot identity string, never 0 (0 means unknown).
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn hash_boot_id(text: &str) -> u64 {
    // FNV-1a: stable across builds, which `DefaultHasher` does not promise.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.trim().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

pub(super) use imp::{boot_id, shared_monotonic_ns, thread_cpu_ns, Activity, HostProbe};

/// Puts the calling thread in the user-initiated QoS class (macOS), so the
/// sampler is neither throttled as background work nor coalesced away while
/// the window is hidden. Elsewhere there is no equivalent and it says so.
pub(super) fn set_user_initiated_qos() -> Result<(), String> {
    imp::set_user_initiated_qos()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::super::sensor::{
        Compressor, Cores, HostSample, Level, Power, Reading, Thermal, Why,
    };
    use std::ffi::{CStr, CString};

    fn last_errno() -> Option<i32> {
        std::io::Error::last_os_error().raw_os_error()
    }

    /// Reads `name` into `out`, returning the bytes the kernel wrote.
    fn sysctl_into(name: &CStr, out: &mut [u8]) -> Result<usize, Why> {
        let mut size = out.len();
        // SAFETY: `out` is a live buffer of `size` bytes; the kernel writes at
        // most `size` and reports how many through `size`.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                out.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            Ok(size)
        } else {
            Err(Why::of_errno(last_errno()))
        }
    }

    /// An unsigned integer sysctl, whether the kernel declares it 32 or 64
    /// bits wide.
    fn sysctl_uint(name: &CStr) -> Reading<u64> {
        let mut buf = [0u8; 8];
        match sysctl_into(name, &mut buf) {
            Ok(4) => Reading::Value(u64::from(u32::from_ne_bytes([
                buf[0], buf[1], buf[2], buf[3],
            ]))),
            Ok(8) => Reading::Value(u64::from_ne_bytes(buf)),
            Ok(_) => Reading::Unavailable(Why::Failed),
            Err(why) => Reading::Unavailable(why),
        }
    }

    fn sysctl_string(name: &CStr) -> Option<String> {
        let mut buf = [0u8; 128];
        let n = sysctl_into(name, &mut buf).ok()?;
        let text = CStr::from_bytes_until_nul(&buf[..n.min(buf.len())]).ok()?;
        Some(text.to_string_lossy().into_owned())
    }

    pub(crate) fn shared_monotonic_ns() -> u64 {
        clock_ns(libc::CLOCK_MONOTONIC).unwrap_or(0)
    }

    pub(crate) fn thread_cpu_ns() -> Option<u64> {
        clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
    }

    fn clock_ns(clock: libc::clockid_t) -> Option<u64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a live timespec.
        if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
            return None;
        }
        let secs = u64::try_from(ts.tv_sec).ok()?;
        let nanos = u64::try_from(ts.tv_nsec).ok()?;
        secs.checked_mul(1_000_000_000)?.checked_add(nanos)
    }

    pub(crate) fn boot_id() -> u64 {
        static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *ID.get_or_init(|| {
            sysctl_string(c"kern.bootsessionuuid").map_or(0, |id| super::hash_boot_id(&id))
        })
    }

    pub(super) fn set_user_initiated_qos() -> Result<(), String> {
        // SAFETY: affects only the calling thread; no pointers.
        let rc = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!(
                "pthread_set_qos_class_self_np failed: {}",
                std::io::Error::from_raw_os_error(rc)
            ))
        }
    }

    // NSActivityOptions, from <Foundation/NSProcessInfo.h>.
    const NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED: u64 = 1 << 20;
    const NS_ACTIVITY_SUDDEN_TERMINATION_DISABLED: u64 = 1 << 14;
    const NS_ACTIVITY_AUTOMATIC_TERMINATION_DISABLED: u64 = 1 << 15;
    const NS_ACTIVITY_USER_INITIATED: u64 = 0x00FF_FFFF | NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED;
    /// User-initiated, which is what keeps App Nap off, minus everything that
    /// would hold the machine awake or stand in the way of logout: the
    /// sampler must not be why a Mac cannot sleep or quit.
    const SAMPLER_ACTIVITY: u64 = NS_ACTIVITY_USER_INITIATED
        & !NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED
        & !NS_ACTIVITY_SUDDEN_TERMINATION_DISABLED
        & !NS_ACTIVITY_AUTOMATIC_TERMINATION_DISABLED;

    /// An `NSProcessInfo` activity assertion: while it lives, App Nap does
    /// not coalesce the sampler's timers when the window is hidden.
    pub(crate) struct Activity {
        token: objc2::rc::Retained<objc2::runtime::AnyObject>,
    }

    // SAFETY: the token is an opaque NSObject that is only ever passed back
    // to `-[NSProcessInfo endActivity:]`, which is documented thread-safe.
    unsafe impl Send for Activity {}

    impl Activity {
        pub(crate) fn begin(reason: &str) -> Result<Self, String> {
            use objc2::rc::Retained;
            use objc2::runtime::AnyObject;
            use objc2::{class, msg_send};
            use objc2_foundation::NSString;
            objc2::rc::autoreleasepool(|_| {
                let reason = NSString::from_str(reason);
                // SAFETY: `processInfo` is a class method returning the shared
                // instance; `beginActivityWithOptions:reason:` takes an
                // NSActivityOptions (uint64_t) and an NSString and returns an
                // object the caller keeps to end the activity.
                let token: Option<Retained<AnyObject>> = unsafe {
                    let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                    if info.is_null() {
                        return Err("NSProcessInfo is unavailable".to_string());
                    }
                    msg_send![info, beginActivityWithOptions: SAMPLER_ACTIVITY, reason: &*reason]
                };
                token
                    .map(|token| Self { token })
                    .ok_or_else(|| "beginActivityWithOptions returned no activity".to_string())
            })
        }
    }

    impl Drop for Activity {
        fn drop(&mut self) {
            use objc2::runtime::AnyObject;
            use objc2::{class, msg_send};
            objc2::rc::autoreleasepool(|_| {
                // SAFETY: ends the activity this token began.
                unsafe {
                    let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                    if !info.is_null() {
                        let _: () = msg_send![info, endActivity: &*self.token];
                    }
                }
            });
        }
    }

    fn thermal() -> Reading<Thermal> {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: `thermalState` is a read-only NSInteger property of the
            // shared process info.
            let state: isize = unsafe {
                let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                if info.is_null() {
                    return Reading::Unavailable(Why::Failed);
                }
                msg_send![info, thermalState]
            };
            u8::try_from(state)
                .ok()
                .and_then(Thermal::from_code)
                .map_or(Reading::Unavailable(Why::Failed), Reading::Value)
        })
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        /// `CFTimeInterval IOPSGetTimeRemainingEstimate(void)`:
        /// `kIOPSTimeRemainingUnlimited` (-2.0) on external power,
        /// `kIOPSTimeRemainingUnknown` (-1.0) or seconds left on battery.
        fn IOPSGetTimeRemainingEstimate() -> f64;
    }

    fn power() -> Reading<Power> {
        // SAFETY: no arguments; returns a plain double.
        let estimate = unsafe { IOPSGetTimeRemainingEstimate() };
        if estimate == -2.0 {
            Reading::Value(Power::Ac)
        } else if estimate == -1.0 || estimate >= 0.0 {
            Reading::Value(Power::Battery)
        } else {
            Reading::Unavailable(Why::Failed)
        }
    }

    pub(crate) struct HostProbe {
        /// The host port, asked for once: every `mach_host_self` call hands
        /// out another send right.
        host: libc::mach_port_t,
        page_size: u64,
        cores: Reading<Cores>,
        memsize: Reading<u64>,
        disk_path: Option<CString>,
    }

    impl HostProbe {
        pub(crate) fn new(disk_path: &std::path::Path) -> Self {
            use std::os::unix::ffi::OsStrExt;
            // SAFETY: no preconditions. Deprecated in `libc` in favour of the
            // `mach2` crate, which this build does not depend on.
            #[allow(deprecated)]
            let host = unsafe { libc::mach_host_self() };
            // SAFETY: sysconf has no memory preconditions.
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            Self {
                host,
                page_size: u64::try_from(page).unwrap_or(16_384),
                cores: read_cores(),
                memsize: sysctl_uint(c"hw.memsize"),
                disk_path: CString::new(disk_path.as_os_str().as_bytes()).ok(),
            }
        }

        pub(crate) fn sample(&mut self) -> HostSample {
            HostSample {
                pressure: match sysctl_uint(c"kern.memorystatus_vm_pressure_level") {
                    Reading::Value(raw) => u32::try_from(raw)
                        .ok()
                        .and_then(Level::from_kernel)
                        .map_or(Reading::Unavailable(Why::Failed), Reading::Value),
                    Reading::Unavailable(why) => Reading::Unavailable(why),
                },
                swap_used: self.swap().map(|s| s.0),
                swap_total: self.swap().map(|s| s.1),
                compressor: compressor(),
                compressed_bytes: self.compressed_bytes(),
                load: load(),
                cores: self.cores,
                memsize: self.memsize,
                open_files: sysctl_uint(c"kern.num_files"),
                max_files: sysctl_uint(c"kern.maxfiles"),
                free_disk: self.free_disk(),
                thermal: thermal(),
                power: power(),
            }
        }

        fn swap(&self) -> Reading<(u64, u64)> {
            let mut usage = std::mem::MaybeUninit::<libc::xsw_usage>::zeroed();
            let mut size = std::mem::size_of::<libc::xsw_usage>();
            // SAFETY: the buffer is one xsw_usage, sized in `size`.
            let rc = unsafe {
                libc::sysctlbyname(
                    c"vm.swapusage".as_ptr(),
                    usage.as_mut_ptr().cast(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 {
                return Reading::Unavailable(Why::of_errno(last_errno()));
            }
            if size != std::mem::size_of::<libc::xsw_usage>() {
                return Reading::Unavailable(Why::Failed);
            }
            // SAFETY: zero-initialised, then filled by the kernel.
            let usage = unsafe { usage.assume_init() };
            Reading::Value((usage.xsu_used, usage.xsu_total))
        }

        fn compressed_bytes(&self) -> Reading<u64> {
            if self.host == 0 {
                return Reading::Unavailable(Why::Failed);
            }
            let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
            let mut count = libc::HOST_VM_INFO64_COUNT;
            // SAFETY: `stats` is a vm_statistics64 and `count` its size in
            // natural_t words, as HOST_VM_INFO64 requires.
            let kr = unsafe {
                libc::host_statistics64(
                    self.host,
                    libc::HOST_VM_INFO64,
                    stats.as_mut_ptr().cast(),
                    &mut count,
                )
            };
            if kr != libc::KERN_SUCCESS {
                return Reading::Unavailable(Why::Failed);
            }
            // SAFETY: zero-initialised, then filled by the kernel.
            let stats = unsafe { stats.assume_init() };
            Reading::Value(u64::from(stats.compressor_page_count).saturating_mul(self.page_size))
        }

        fn free_disk(&self) -> Reading<u64> {
            let Some(path) = &self.disk_path else {
                return Reading::Unavailable(Why::Failed);
            };
            let mut fs = std::mem::MaybeUninit::<libc::statfs>::zeroed();
            // SAFETY: a NUL-terminated path and one statfs buffer.
            if unsafe { libc::statfs(path.as_ptr(), fs.as_mut_ptr()) } != 0 {
                return Reading::Unavailable(Why::of_errno(last_errno()));
            }
            // SAFETY: filled by the kernel.
            let fs = unsafe { fs.assume_init() };
            Reading::Value(fs.f_bavail.saturating_mul(u64::from(fs.f_bsize)))
        }
    }

    fn compressor() -> Reading<Compressor> {
        let read = |name: &CStr| sysctl_uint(name);
        let counters = [
            read(c"vm.compressor.swapper.wakeups.thrashing_detected"),
            read(c"vm.compressor.swapper.wakeups.free_below_reserved"),
            read(c"vm.compressor.swapper.swapouts_pressure"),
            read(c"vm.compressor.compactor.major_compaction_segments_freed"),
        ];
        match counters {
            [Reading::Value(a), Reading::Value(b), Reading::Value(c), Reading::Value(d)] => {
                Reading::Value(Compressor {
                    thrashing_detected: a,
                    free_below_reserved: b,
                    swapouts_pressure: c,
                    major_compaction_segments_freed: d,
                })
            }
            // One counter missing makes the set unusable for a delta.
            _ => Reading::Unavailable(
                counters
                    .iter()
                    .find_map(|r| match r {
                        Reading::Unavailable(why) => Some(*why),
                        Reading::Value(_) => None,
                    })
                    .unwrap_or(Why::Failed),
            ),
        }
    }

    fn load() -> Reading<[f64; 3]> {
        let mut avg = [0f64; 3];
        // SAFETY: three doubles, as requested.
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
        if n == 3 {
            Reading::Value(avg)
        } else {
            Reading::Unavailable(Why::Failed)
        }
    }

    /// Performance levels where the CPU has them; the logical count where it
    /// does not (Intel, or a Rosetta process, which reports no levels).
    fn read_cores() -> Reading<Cores> {
        let Reading::Value(logical) = sysctl_uint(c"hw.logicalcpu") else {
            return Reading::Unavailable(Why::Failed);
        };
        let logical = u16::try_from(logical).unwrap_or(u16::MAX);
        let levels = sysctl_uint(c"hw.nperflevels").value().unwrap_or(0);
        let level = |name: &CStr| {
            sysctl_uint(name)
                .value()
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(0)
        };
        Reading::Value(Cores {
            logical,
            levels: u8::try_from(levels).unwrap_or(u8::MAX),
            perf0: if levels >= 1 {
                level(c"hw.perflevel0.logicalcpu")
            } else {
                logical
            },
            perf1: if levels >= 2 {
                level(c"hw.perflevel1.logicalcpu")
            } else {
                0
            },
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::super::sensor::{Cores, HostSample, Level, Reading, Why};
    use std::ffi::CString;

    /// PSI `avg10` percentages at which the level steps up. Provisional: no
    /// real Linux host has calibrated these against the macOS levels.
    const PSI_WARN_SOME_AVG10: f64 = 10.0;
    const PSI_CRITICAL_FULL_AVG10: f64 = 10.0;

    fn read_small(path: &str) -> Result<String, Why> {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(64 * 1024).read_to_string(&mut text))
            .map_err(|e| Why::of_errno(e.raw_os_error()))?;
        Ok(text)
    }

    pub(crate) fn shared_monotonic_ns() -> u64 {
        clock_ns(libc::CLOCK_BOOTTIME).unwrap_or(0)
    }

    pub(crate) fn thread_cpu_ns() -> Option<u64> {
        clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
    }

    fn clock_ns(clock: libc::clockid_t) -> Option<u64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a live timespec.
        if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
            return None;
        }
        let secs = u64::try_from(ts.tv_sec).ok()?;
        let nanos = u64::try_from(ts.tv_nsec).ok()?;
        secs.checked_mul(1_000_000_000)?.checked_add(nanos)
    }

    pub(crate) fn boot_id() -> u64 {
        static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *ID.get_or_init(|| {
            read_small("/proc/sys/kernel/random/boot_id").map_or(0, |id| super::hash_boot_id(&id))
        })
    }

    pub(super) fn set_user_initiated_qos() -> Result<(), String> {
        Err("this platform has no QoS classes; the sampler runs at the default priority".into())
    }

    /// Linux has no App Nap; there is nothing to assert.
    pub(crate) struct Activity;

    impl Activity {
        pub(crate) fn begin(_reason: &str) -> Result<Self, String> {
            Ok(Self)
        }
    }

    /// The `avg10` value of the `some` or `full` line of a PSI file.
    pub(crate) fn psi_avg10(text: &str, kind: &str) -> Option<f64> {
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(kind))?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("avg10="))?
            .parse()
            .ok()
    }

    pub(crate) fn psi_level(text: &str) -> Option<Level> {
        let some = psi_avg10(text, "some")?;
        let full = psi_avg10(text, "full").unwrap_or(0.0);
        Some(if full >= PSI_CRITICAL_FULL_AVG10 {
            Level::Critical
        } else if some >= PSI_WARN_SOME_AVG10 {
            Level::Warn
        } else {
            Level::Normal
        })
    }

    fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    pub(crate) struct HostProbe {
        disk_path: Option<CString>,
        cores: Reading<Cores>,
    }

    impl HostProbe {
        pub(crate) fn new(disk_path: &std::path::Path) -> Self {
            use std::os::unix::ffi::OsStrExt;
            let logical = std::thread::available_parallelism()
                .map(|n| u16::try_from(n.get()).unwrap_or(u16::MAX))
                .map_or(Reading::Unavailable(Why::Failed), |logical| {
                    Reading::Value(Cores {
                        logical,
                        levels: 0,
                        perf0: logical,
                        perf1: 0,
                    })
                });
            Self {
                disk_path: CString::new(disk_path.as_os_str().as_bytes()).ok(),
                cores: logical,
            }
        }

        pub(crate) fn sample(&mut self) -> HostSample {
            let meminfo = read_small("/proc/meminfo");
            let kib = |key: &str| match &meminfo {
                Ok(text) => meminfo_kib(text, key).map_or(Reading::Unavailable(Why::Failed), |v| {
                    Reading::Value(v.saturating_mul(1024))
                }),
                Err(why) => Reading::Unavailable(*why),
            };
            let swap_total = kib("SwapTotal");
            let swap_used = match (swap_total, kib("SwapFree")) {
                (Reading::Value(t), Reading::Value(f)) => Reading::Value(t.saturating_sub(f)),
                (Reading::Unavailable(why), _) | (_, Reading::Unavailable(why)) => {
                    Reading::Unavailable(why)
                }
            };
            let file_nr = read_small("/proc/sys/fs/file-nr");
            let file_field = |i: usize| match &file_nr {
                Ok(text) => text
                    .split_whitespace()
                    .nth(i)
                    .and_then(|v| v.parse().ok())
                    .map_or(Reading::Unavailable(Why::Failed), Reading::Value),
                Err(why) => Reading::Unavailable(*why),
            };
            HostSample {
                pressure: match read_small("/proc/pressure/memory") {
                    Ok(text) => {
                        psi_level(&text).map_or(Reading::Unavailable(Why::Failed), Reading::Value)
                    }
                    Err(why) => Reading::Unavailable(why),
                },
                swap_used,
                swap_total,
                compressor: Reading::Unavailable(Why::Unsupported),
                compressed_bytes: Reading::Unavailable(Why::Unsupported),
                load: match read_small("/proc/loadavg") {
                    Ok(text) => {
                        let mut it = text.split_whitespace().map(|v| v.parse::<f64>().ok());
                        match (it.next(), it.next(), it.next()) {
                            (Some(Some(a)), Some(Some(b)), Some(Some(c))) => {
                                Reading::Value([a, b, c])
                            }
                            _ => Reading::Unavailable(Why::Failed),
                        }
                    }
                    Err(why) => Reading::Unavailable(why),
                },
                cores: self.cores,
                memsize: kib("MemTotal"),
                open_files: file_field(0),
                max_files: file_field(2),
                free_disk: self.free_disk(),
                thermal: Reading::Unavailable(Why::Unsupported),
                power: Reading::Unavailable(Why::Unsupported),
            }
        }

        fn free_disk(&self) -> Reading<u64> {
            let Some(path) = &self.disk_path else {
                return Reading::Unavailable(Why::Failed);
            };
            let mut fs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
            // SAFETY: a NUL-terminated path and one statvfs buffer.
            if unsafe { libc::statvfs(path.as_ptr(), fs.as_mut_ptr()) } != 0 {
                return Reading::Unavailable(Why::of_errno(
                    std::io::Error::last_os_error().raw_os_error(),
                ));
            }
            // SAFETY: filled by the kernel.
            let fs = unsafe { fs.assume_init() };
            #[allow(clippy::useless_conversion)]
            let bytes = u64::from(fs.f_bavail).saturating_mul(u64::from(fs.f_frsize));
            Reading::Value(bytes)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn psi_lines_map_to_levels() {
            let calm = "some avg10=0.50 avg60=0.10 avg300=0.00 total=1\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
            let warm = "some avg10=12.00 avg60=3.00 avg300=1.00 total=9\nfull avg10=1.00 avg60=0.00 avg300=0.00 total=2\n";
            let hot = "some avg10=40.00 avg60=3.00 avg300=1.00 total=9\nfull avg10=15.00 avg60=0.00 avg300=0.00 total=2\n";
            assert_eq!(psi_level(calm), Some(Level::Normal));
            assert_eq!(psi_level(warm), Some(Level::Warn));
            assert_eq!(psi_level(hot), Some(Level::Critical));
            assert_eq!(psi_level("garbage"), None);
            assert_eq!(
                meminfo_kib("SwapTotal:  2048 kB\n", "SwapTotal"),
                Some(2048)
            );
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::super::sensor::HostSample;

    pub(crate) fn shared_monotonic_ns() -> u64 {
        0
    }

    pub(crate) fn thread_cpu_ns() -> Option<u64> {
        None
    }

    pub(crate) fn boot_id() -> u64 {
        0
    }

    pub(super) fn set_user_initiated_qos() -> Result<(), String> {
        Err("no host-load sampler on this platform".into())
    }

    pub(crate) struct Activity;

    impl Activity {
        pub(crate) fn begin(_reason: &str) -> Result<Self, String> {
            Ok(Self)
        }
    }

    pub(crate) struct HostProbe;

    impl HostProbe {
        pub(crate) fn new(_disk_path: &std::path::Path) -> Self {
            Self
        }

        /// Every reading unsupported: the default sample.
        pub(crate) fn sample(&mut self) -> HostSample {
            HostSample::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::sensor::Reading;
    #[cfg(not(target_os = "macos"))]
    use crate::sysload::sensor::Why;

    #[test]
    fn boot_ids_hash_stably_and_never_to_the_unknown_value() {
        assert_eq!(hash_boot_id("abc"), hash_boot_id(" abc\n"));
        assert_ne!(hash_boot_id("abc"), hash_boot_id("abd"));
        assert_ne!(hash_boot_id(""), 0);
    }

    /// Real: this host's own readings, whatever they are. Each must be a
    /// value or a named reason, and on macOS the ones this card depends on
    /// must actually be readable.
    #[test]
    fn real_host_readings_are_values_or_named_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let mut probe = HostProbe::new(dir.path());
        let sample = probe.sample();
        #[cfg(target_os = "macos")]
        {
            assert!(matches!(sample.pressure, Reading::Value(_)), "{sample:?}");
            assert!(matches!(sample.compressor, Reading::Value(_)), "{sample:?}");
            // Part 0: host_statistics64 is reachable through the existing
            // libc and reports the compressor's page count.
            assert!(
                matches!(sample.compressed_bytes, Reading::Value(_)),
                "{sample:?}"
            );
            assert!(
                matches!(sample.free_disk, Reading::Value(v) if v > 0),
                "{sample:?}"
            );
            assert!(matches!(sample.cores, Reading::Value(c) if c.logical > 0));
            assert!(matches!(sample.thermal, Reading::Value(_)), "{sample:?}");
            assert!(matches!(sample.power, Reading::Value(_)), "{sample:?}");
            assert_ne!(boot_id(), 0);
            assert!(shared_monotonic_ns() > 0);
            assert!(thread_cpu_ns().is_some());
        }
        #[cfg(target_os = "linux")]
        {
            // PSI may be absent (an old kernel, a container), so pressure is
            // a value or a named reason; these always exist under /proc.
            assert!(matches!(sample.load, Reading::Value(_)), "{sample:?}");
            assert!(
                matches!(sample.memsize, Reading::Value(v) if v > 0),
                "{sample:?}"
            );
            assert!(matches!(sample.open_files, Reading::Value(_)), "{sample:?}");
            assert_eq!(sample.compressor, Reading::Unavailable(Why::Unsupported));
            assert_ne!(boot_id(), 0);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert_eq!(sample.pressure, Reading::Unavailable(Why::Unsupported));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_sampler_thread_can_take_its_qos_and_activity() {
        std::thread::spawn(|| {
            set_user_initiated_qos().unwrap();
            let activity = Activity::begin("GitPulse host-load sampler test").unwrap();
            drop(activity);
        })
        .join()
        .unwrap();
    }
}
