//! Host-wide readings from the operating system, and the clocks and thread
//! controls the sampler needs.
//!
//! macOS reads sysctls, `host_statistics64`, `host_statistics` (`HOST_CPU_LOAD_INFO`),
//! `getloadavg`, `statfs`, `IOAccelerator` `PerformanceStatistics`,
//! `NSProcessInfo.thermalState`, `IOHIDEventSystemClient` SoC die temperatures,
//! and the IOKit power-source estimate. Linux reads PSI and `/proc`
//! (observe-and-advise; the PSI level mapping has not been checked on a real
//! Linux host). Everything else reports every reading as unsupported.

use super::sensor::{CpuUsage, Reading, Why};

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn cpu_usage_from_deltas(user_plus_nice: u64, system: u64, idle: u64) -> Reading<CpuUsage> {
    let total = user_plus_nice.saturating_add(system).saturating_add(idle);
    if total == 0 {
        return Reading::Unavailable(Why::NotYet);
    }
    let total_f = total as f64;
    let busy = user_plus_nice.saturating_add(system) as f64;
    Reading::Value(CpuUsage {
        total_pct: round1((busy / total_f * 100.0).clamp(0.0, 100.0)),
        user_pct: round1((user_plus_nice as f64 / total_f * 100.0).clamp(0.0, 100.0)),
        system_pct: round1((system as f64 / total_f * 100.0).clamp(0.0, 100.0)),
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_soc_die_temp_sensor(product: &str, temp_c: f64) -> bool {
    if !temp_c.is_finite() || !(1.0..=150.0).contains(&temp_c) {
        return false;
    }
    product.starts_with("PMU tdie")
        || product.starts_with("eACC")
        || product.starts_with("pACC")
        || product.starts_with("gACC")
        || product.contains("MTR Temp Sensor")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy)]
struct GpuEntryReading {
    utilization_pct: Option<f64>,
    renderer_pct: Option<f64>,
    tiler_pct: Option<f64>,
    memory_in_use: Option<u64>,
    memory_allocated: Option<u64>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn select_gpu_sample(entries: &[GpuEntryReading]) -> Reading<super::sensor::GpuSample> {
    if entries.is_empty() {
        return Reading::Unavailable(Why::Unsupported);
    }
    let mut best_util: Option<f64> = None;
    let mut best_renderer: Option<f64> = None;
    let mut best_tiler: Option<f64> = None;
    let mut sum_in_use: Option<u64> = None;
    let mut sum_alloc: Option<u64> = None;

    for entry in entries {
        let Some(util) = entry.utilization_pct else {
            continue;
        };
        if best_util.is_none_or(|prev| util > prev) {
            best_util = Some(util);
            best_renderer = entry.renderer_pct;
            best_tiler = entry.tiler_pct;
        }
        if let Some(mem) = entry.memory_in_use {
            sum_in_use = Some(sum_in_use.unwrap_or(0).saturating_add(mem));
        }
        if let Some(alloc) = entry.memory_allocated {
            sum_alloc = Some(sum_alloc.unwrap_or(0).saturating_add(alloc));
        }
    }

    match best_util {
        Some(utilization_pct) => Reading::Value(super::sensor::GpuSample {
            utilization_pct,
            renderer_pct: best_renderer,
            tiler_pct: best_tiler,
            memory_in_use: sum_in_use,
            memory_allocated: sum_alloc,
        }),
        None => Reading::Unavailable(Why::Unreadable),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_linux_thermal_temp_c(text: &str) -> Option<f64> {
    let milli: i64 = text.trim().parse().ok()?;
    let temp_c = milli as f64 / 1000.0;
    if !temp_c.is_finite() || !(1.0..=150.0).contains(&temp_c) {
        return None;
    }
    Some(round1(temp_c))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_thermal_temp_in(dir: &std::path::Path) -> Reading<f64> {
    use std::io::Read;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return Reading::Unavailable(Why::of_errno(e.raw_os_error())),
    };
    let mut max_temp: Option<f64> = None;
    let mut saw_unreadable = false;
    let mut saw_failed = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        let temp_path = entry.path().join("temp");
        let mut text = String::new();
        match std::fs::File::open(&temp_path).and_then(|f| f.take(128).read_to_string(&mut text)) {
            Ok(_) => {
                if let Some(t) = parse_linux_thermal_temp_c(&text) {
                    max_temp = Some(max_temp.map_or(t, |prev| prev.max(t)));
                }
            }
            Err(e) => match Why::of_errno(e.raw_os_error()) {
                Why::Unreadable => saw_unreadable = true,
                Why::Unsupported => {}
                Why::Failed | Why::NotYet | Why::Disabled | Why::Older => saw_failed = true,
            },
        }
    }
    max_temp.map_or_else(
        || {
            Reading::Unavailable(if saw_unreadable {
                Why::Unreadable
            } else if saw_failed {
                Why::Failed
            } else {
                Why::Unsupported
            })
        },
        Reading::Value,
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_linux_loadavg(text: &str) -> Option<[f64; 3]> {
    let mut it = text
        .split_whitespace()
        .map(|v| v.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0));
    match (it.next(), it.next(), it.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c))) => Some([a, b, c]),
        _ => None,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat_cpu(text: &str) -> Option<(u64, u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let mut nums = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse::<u64>().ok());
    let user = nums.next()?;
    let nice = nums.next()?;
    let system = nums.next()?;
    let idle = nums.next()?;
    let iowait = nums.next().unwrap_or(0);
    let irq = nums.next().unwrap_or(0);
    let softirq = nums.next().unwrap_or(0);
    let steal = nums.next().unwrap_or(0);
    Some((
        user.saturating_add(nice),
        system
            .saturating_add(irq)
            .saturating_add(softirq)
            .saturating_add(steal),
        idle.saturating_add(iowait),
    ))
}

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
        Compressor, CompressorSegments, Cores, CpuUsage, GpuSample, HostSample, Level,
        MemorySample, Power, Reading, Thermal, Why,
    };
    use super::{
        cpu_usage_from_deltas, is_soc_die_temp_sensor, round1, select_gpu_sample, GpuEntryReading,
    };
    use std::ffi::{CStr, CString};
    use std::sync::OnceLock;

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
        fn IOServiceMatching(name: *const libc::c_char) -> *mut libc::c_void;
        fn IOServiceGetMatchingServices(
            main_port: libc::mach_port_t,
            matching: *const libc::c_void,
            existing: *mut u32,
        ) -> libc::kern_return_t;
        fn IOIteratorNext(iterator: u32) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: *const libc::c_void,
            allocator: *const libc::c_void,
            options: u32,
        ) -> *const libc::c_void;
        fn IOObjectRelease(object: u32) -> libc::kern_return_t;
    }

    type CFTypeRef = *const libc::c_void;
    type CFIndex = libc::c_long;
    type CFTypeID = libc::c_ulong;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;
    const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;
    const K_CF_NUMBER_FLOAT64_TYPE: CFIndex = 6;
    const K_IOHID_EVENT_TYPE_TEMPERATURE: i64 = 15;
    const K_IOHID_EVENT_FIELD_TEMP_LEVEL: i32 = 15 << 16;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFDictionaryGetTypeID() -> CFTypeID;
        fn CFDictionaryGetValue(the_dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            num_values: CFIndex,
            key_callbacks: *const libc::c_void,
            value_callbacks: *const libc::c_void,
        ) -> CFTypeRef;
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFTypeRef;
        fn CFStringGetTypeID() -> CFTypeID;
        fn CFStringGetLength(the_string: CFTypeRef) -> CFIndex;
        fn CFStringGetCString(
            the_string: CFTypeRef,
            buffer: *mut libc::c_char,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFNumberGetTypeID() -> CFTypeID;
        fn CFNumberGetValue(
            number: CFTypeRef,
            the_type: CFIndex,
            value_ptr: *mut libc::c_void,
        ) -> u8;
        fn CFNumberCreate(
            allocator: CFTypeRef,
            the_type: CFIndex,
            value_ptr: *const libc::c_void,
        ) -> CFTypeRef;
        fn CFArrayGetTypeID() -> CFTypeID;
        fn CFArrayGetCount(the_array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(the_array: CFTypeRef, idx: CFIndex) -> CFTypeRef;
    }

    struct OwnedCf(CFTypeRef);

    impl Drop for OwnedCf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is an owned CoreFoundation object with +1 retain.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    struct OwnedIo(u32);

    impl Drop for OwnedIo {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: `self.0` is a live `io_object_t` handle.
                unsafe { IOObjectRelease(self.0) };
            }
        }
    }

    fn cf_str(s: &str) -> OwnedCf {
        // SAFETY: `s.as_ptr()` is valid for `s.len()` UTF-8 bytes.
        let ptr = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                s.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
                0,
            )
        };
        OwnedCf(ptr)
    }

    fn cf_dict_f64(dict: CFTypeRef, key: &str) -> Option<f64> {
        let cf_key = cf_str(key);
        if cf_key.0.is_null() {
            return None;
        }
        // SAFETY: `dict` is a verified CFDictionaryRef and `cf_key.0` is a CFStringRef.
        let val = unsafe { CFDictionaryGetValue(dict, cf_key.0) };
        if val.is_null() || unsafe { CFGetTypeID(val) != CFNumberGetTypeID() } {
            return None;
        }
        let mut out = 0.0f64;
        // SAFETY: `val` is a verified CFNumberRef and `out` is a valid f64 slot.
        if unsafe { CFNumberGetValue(val, K_CF_NUMBER_FLOAT64_TYPE, (&raw mut out).cast()) } == 0 {
            return None;
        }
        out.is_finite().then_some(out)
    }

    fn cf_dict_u64(dict: CFTypeRef, key: &str) -> Option<u64> {
        let cf_key = cf_str(key);
        if cf_key.0.is_null() {
            return None;
        }
        // SAFETY: `dict` is a verified CFDictionaryRef and `cf_key.0` is a CFStringRef.
        let val = unsafe { CFDictionaryGetValue(dict, cf_key.0) };
        if val.is_null() || unsafe { CFGetTypeID(val) != CFNumberGetTypeID() } {
            return None;
        }
        let mut out = 0i64;
        // SAFETY: `val` is a verified CFNumberRef and `out` is a valid i64 slot.
        if unsafe { CFNumberGetValue(val, K_CF_NUMBER_SINT64_TYPE, (&raw mut out).cast()) } == 0 {
            return None;
        }
        u64::try_from(out).ok()
    }

    fn cf_string_to_rust(s: CFTypeRef) -> Option<String> {
        if s.is_null() || unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
            return None;
        }
        let len = unsafe { CFStringGetLength(s) };
        if !(0..=1024).contains(&len) {
            return None;
        }
        let mut buf = vec![0u8; (len as usize) * 4 + 1];
        // SAFETY: `s` is a CFStringRef and `buf` is sized for UTF-8 + NUL.
        let ok = unsafe {
            CFStringGetCString(
                s,
                buf.as_mut_ptr().cast(),
                buf.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return None;
        }
        let text = CStr::from_bytes_until_nul(&buf).ok()?;
        Some(text.to_string_lossy().into_owned())
    }

    fn gpu() -> Reading<GpuSample> {
        // SAFETY: returns a CFMutableDictionaryRef (+1) consumed by IOServiceGetMatchingServices.
        let matching = unsafe { IOServiceMatching(c"IOAccelerator".as_ptr()) };
        if matching.is_null() {
            return Reading::Unavailable(Why::Unsupported);
        }
        let mut raw_iter: u32 = 0;
        // SAFETY: `IOServiceGetMatchingServices` always consumes one retain on `matching`.
        let kr = unsafe { IOServiceGetMatchingServices(0, matching.cast(), &mut raw_iter) };
        if kr != libc::KERN_SUCCESS {
            return Reading::Unavailable(Why::Unsupported);
        }
        let iter = OwnedIo(raw_iter);
        let perf_key = cf_str("PerformanceStatistics");
        if perf_key.0.is_null() {
            return Reading::Unavailable(Why::Failed);
        }

        let mut entries = Vec::new();
        loop {
            // SAFETY: `iter.0` is a live `io_iterator_t`.
            let raw_entry = unsafe { IOIteratorNext(iter.0) };
            if raw_entry == 0 {
                break;
            }
            let entry = OwnedIo(raw_entry);
            // SAFETY: returns a CFTypeRef (+1) owned by `prop`.
            let prop = OwnedCf(unsafe {
                IORegistryEntryCreateCFProperty(entry.0, perf_key.0, std::ptr::null(), 0)
            });
            if prop.0.is_null() || unsafe { CFGetTypeID(prop.0) != CFDictionaryGetTypeID() } {
                entries.push(GpuEntryReading {
                    utilization_pct: None,
                    renderer_pct: None,
                    tiler_pct: None,
                    memory_in_use: None,
                    memory_allocated: None,
                });
                continue;
            }
            let dict = prop.0;
            let utilization_pct = cf_dict_f64(dict, "Device Utilization %")
                .or_else(|| cf_dict_f64(dict, "GPU Activity(%)"))
                .or_else(|| {
                    cf_dict_f64(dict, "GPU Core Utilization").map(|raw| {
                        if raw > 100.0 {
                            raw / 10_000_000.0
                        } else {
                            raw
                        }
                    })
                })
                .map(|v| round1(v.clamp(0.0, 100.0)));
            let renderer_pct =
                cf_dict_f64(dict, "Renderer Utilization %").map(|v| round1(v.clamp(0.0, 100.0)));
            let tiler_pct =
                cf_dict_f64(dict, "Tiler Utilization %").map(|v| round1(v.clamp(0.0, 100.0)));
            let memory_in_use = cf_dict_u64(dict, "In use system memory")
                .or_else(|| cf_dict_u64(dict, "vramUsedBytes"));
            let memory_allocated = cf_dict_u64(dict, "Alloc system memory");
            entries.push(GpuEntryReading {
                utilization_pct,
                renderer_pct,
                tiler_pct,
                memory_in_use,
                memory_allocated,
            });
        }

        select_gpu_sample(&entries)
    }

    type IoHidCreateFn = unsafe extern "C" fn(CFTypeRef) -> CFTypeRef;
    type IoHidSetMatchingFn = unsafe extern "C" fn(CFTypeRef, CFTypeRef);
    type IoHidCopyServicesFn = unsafe extern "C" fn(CFTypeRef) -> CFTypeRef;
    type IoHidCopyPropertyFn = unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> CFTypeRef;
    type IoHidCopyEventFn = unsafe extern "C" fn(CFTypeRef, i64, CFTypeRef, u32) -> CFTypeRef;
    type IoHidGetFloatFn = unsafe extern "C" fn(CFTypeRef, i32) -> f64;

    struct IoHidSyms {
        create: IoHidCreateFn,
        set_matching: IoHidSetMatchingFn,
        copy_services: IoHidCopyServicesFn,
        copy_property: IoHidCopyPropertyFn,
        copy_event: IoHidCopyEventFn,
        get_float: IoHidGetFloatFn,
    }

    fn iohid_syms() -> Option<&'static IoHidSyms> {
        static SYMS: OnceLock<Option<IoHidSyms>> = OnceLock::new();
        SYMS.get_or_init(|| {
            let resolve = |name: &CStr| -> Option<*mut libc::c_void> {
                // SAFETY: `name` is a static C string and `RTLD_DEFAULT` searches linked frameworks.
                let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
                (!ptr.is_null()).then_some(ptr)
            };
            // SAFETY: symbol signatures match CoreFoundation/IOKit `IOHIDEventSystemClient` APIs.
            unsafe {
                Some(IoHidSyms {
                    create: std::mem::transmute::<*mut libc::c_void, IoHidCreateFn>(resolve(
                        c"IOHIDEventSystemClientCreate",
                    )?),
                    set_matching: std::mem::transmute::<*mut libc::c_void, IoHidSetMatchingFn>(
                        resolve(c"IOHIDEventSystemClientSetMatching")?,
                    ),
                    copy_services: std::mem::transmute::<*mut libc::c_void, IoHidCopyServicesFn>(
                        resolve(c"IOHIDEventSystemClientCopyServices")?,
                    ),
                    copy_property: std::mem::transmute::<*mut libc::c_void, IoHidCopyPropertyFn>(
                        resolve(c"IOHIDServiceClientCopyProperty")?,
                    ),
                    copy_event: std::mem::transmute::<*mut libc::c_void, IoHidCopyEventFn>(
                        resolve(c"IOHIDServiceClientCopyEvent")?,
                    ),
                    get_float: std::mem::transmute::<*mut libc::c_void, IoHidGetFloatFn>(resolve(
                        c"IOHIDEventGetFloatValue",
                    )?),
                })
            }
        })
        .as_ref()
    }

    struct HidClient(OwnedCf);

    // SAFETY: Created synchronously without scheduling on a run loop, owned
    // exclusively by `HostProbe`, and accessed only through `&mut self`.
    unsafe impl Send for HidClient {}

    fn create_hid_client(syms: &IoHidSyms) -> Result<HidClient, Why> {
        // SAFETY: `IOHIDEventSystemClientCreate(NULL)` returns a +1 CFTypeRef.
        let client = OwnedCf(unsafe { (syms.create)(std::ptr::null()) });
        if client.0.is_null() {
            return Err(Why::Unsupported);
        }
        let k_page = cf_str("PrimaryUsagePage");
        let k_usage = cf_str("PrimaryUsage");
        let page_val: i32 = 0xff00;
        let usage_val: i32 = 5;
        let v_page = OwnedCf(unsafe {
            CFNumberCreate(
                std::ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&raw const page_val).cast(),
            )
        });
        let v_usage = OwnedCf(unsafe {
            CFNumberCreate(
                std::ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&raw const usage_val).cast(),
            )
        });
        if k_page.0.is_null() || k_usage.0.is_null() || v_page.0.is_null() || v_usage.0.is_null() {
            return Err(Why::Failed);
        }
        let keys = [k_page.0, k_usage.0];
        let vals = [v_page.0, v_usage.0];
        let match_dict = OwnedCf(unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                vals.as_ptr(),
                2,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            )
        });
        if match_dict.0.is_null() {
            return Err(Why::Failed);
        }
        // SAFETY: `client.0` and `match_dict.0` are valid CF objects.
        unsafe { (syms.set_matching)(client.0, match_dict.0) };
        Ok(HidClient(client))
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
        /// The host port, asked for once and deallocated in `Drop`: every
        /// `mach_host_self` call hands out another send right.
        host: libc::mach_port_t,
        page_size: u64,
        cores: Reading<Cores>,
        memsize: Reading<u64>,
        disk_path: Option<CString>,
        prev_cpu_ticks: Option<[u32; 4]>,
        hid_client: Option<HidClient>,
        hid_had_services: bool,
    }

    impl Drop for HostProbe {
        fn drop(&mut self) {
            if self.host != 0 {
                extern "C" {
                    fn mach_port_deallocate(
                        task: libc::mach_port_t,
                        name: libc::mach_port_t,
                    ) -> libc::kern_return_t;
                }
                // SAFETY: `self.host` is the send right owned by this `HostProbe`.
                #[allow(deprecated)]
                unsafe {
                    mach_port_deallocate(libc::mach_task_self(), self.host);
                }
            }
        }
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
                prev_cpu_ticks: None,
                hid_client: None,
                hid_had_services: false,
            }
        }

        pub(crate) fn reset(&mut self) {
            self.prev_cpu_ticks = None;
            self.hid_client = None;
            self.hid_had_services = false;
        }

        #[cfg(test)]
        pub(super) fn hid_client_state(&self) -> (Option<usize>, bool) {
            (
                self.hid_client.as_ref().map(|c| c.0 .0 as usize),
                self.hid_had_services,
            )
        }

        pub(crate) fn sample(&mut self) -> HostSample {
            let (compressed_bytes, memory) = self.vm_memory();
            let (free_disk, total_disk) = self.disk();
            let swap = self.swap();
            HostSample {
                pressure: match sysctl_uint(c"kern.memorystatus_vm_pressure_level") {
                    Reading::Value(raw) => u32::try_from(raw)
                        .ok()
                        .and_then(Level::from_kernel)
                        .map_or(Reading::Unavailable(Why::Failed), Reading::Value),
                    Reading::Unavailable(why) => Reading::Unavailable(why),
                },
                swap_used: swap.map(|s| s.0),
                swap_total: swap.map(|s| s.1),
                compressor: compressor(),
                compressed_bytes,
                compressor_segments: compressor_segments(),
                load: load(),
                cores: self.cores,
                memsize: self.memsize,
                open_files: sysctl_uint(c"kern.num_files"),
                max_files: sysctl_uint(c"kern.maxfiles"),
                free_disk,
                thermal: thermal(),
                power: power(),
                cpu_usage: self.cpu_usage(),
                gpu: gpu(),
                memory,
                total_disk,
                thermal_temp_c: self.thermal_temp_c(),
            }
        }

        fn thermal_temp_c(&mut self) -> Reading<f64> {
            let Some(syms) = iohid_syms() else {
                return Reading::Unavailable(Why::Unsupported);
            };
            let client_ref = match self.hid_client.as_ref() {
                Some(client) => client.0 .0,
                None => match create_hid_client(syms) {
                    Ok(client) => self.hid_client.insert(client).0 .0,
                    Err(why) => return Reading::Unavailable(why),
                },
            };
            // SAFETY: returns a +1 CFArrayRef of HID service clients, or NULL.
            let services = OwnedCf(unsafe { (syms.copy_services)(client_ref) });
            if services.0.is_null() || unsafe { CFGetTypeID(services.0) != CFArrayGetTypeID() } {
                self.hid_client = None;
                self.hid_had_services = false;
                return Reading::Unavailable(Why::Unsupported);
            }
            let product_key = cf_str("Product");
            if product_key.0.is_null() {
                return Reading::Unavailable(Why::Failed);
            }
            let count = unsafe { CFArrayGetCount(services.0) };
            if count > 0 {
                self.hid_had_services = true;
            } else if self.hid_had_services {
                // If the HID daemon restarted after previously publishing services,
                // drop the cached client so the next tick creates a fresh one.
                self.hid_client = None;
                self.hid_had_services = false;
            }
            let mut max_temp: Option<f64> = None;
            for i in 0..count {
                let service = unsafe { CFArrayGetValueAtIndex(services.0, i) };
                if service.is_null() {
                    continue;
                }
                let prop = OwnedCf(unsafe { (syms.copy_property)(service, product_key.0) });
                let Some(name) = cf_string_to_rust(prop.0) else {
                    continue;
                };
                let event = OwnedCf(unsafe {
                    (syms.copy_event)(service, K_IOHID_EVENT_TYPE_TEMPERATURE, std::ptr::null(), 0)
                });
                if event.0.is_null() {
                    continue;
                }
                let temp = unsafe { (syms.get_float)(event.0, K_IOHID_EVENT_FIELD_TEMP_LEVEL) };
                if is_soc_die_temp_sensor(&name, temp) {
                    max_temp = Some(max_temp.map_or(temp, |prev| prev.max(temp)));
                }
            }
            max_temp.map_or(Reading::Unavailable(Why::Unsupported), |t| {
                Reading::Value(round1(t))
            })
        }

        fn cpu_usage(&mut self) -> Reading<CpuUsage> {
            if self.host == 0 {
                return Reading::Unavailable(Why::Failed);
            }
            let mut ticks = [0u32; 4];
            let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
            // SAFETY: `ticks` holds 4 `u32`s (`cpu_ticks`), matching `HOST_CPU_LOAD_INFO_COUNT`.
            let kr = unsafe {
                libc::host_statistics(
                    self.host,
                    libc::HOST_CPU_LOAD_INFO,
                    ticks.as_mut_ptr().cast(),
                    &mut count,
                )
            };
            if kr != libc::KERN_SUCCESS || count != libc::HOST_CPU_LOAD_INFO_COUNT {
                return Reading::Unavailable(Why::Failed);
            }
            let Some(prev) = self.prev_cpu_ticks else {
                self.prev_cpu_ticks = Some(ticks);
                return Reading::Unavailable(Why::NotYet);
            };
            let delta = |idx: usize| u64::from(ticks[idx].wrapping_sub(prev[idx]));
            let d_user = delta(libc::CPU_STATE_USER as usize)
                .saturating_add(delta(libc::CPU_STATE_NICE as usize));
            let d_sys = delta(libc::CPU_STATE_SYSTEM as usize);
            let d_idle = delta(libc::CPU_STATE_IDLE as usize);
            let reading = cpu_usage_from_deltas(d_user, d_sys, d_idle);
            if matches!(reading, Reading::Value(_)) {
                self.prev_cpu_ticks = Some(ticks);
            }
            reading
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

        fn vm_memory(&self) -> (Reading<u64>, Reading<MemorySample>) {
            if self.host == 0 {
                return (
                    Reading::Unavailable(Why::Failed),
                    Reading::Unavailable(Why::Failed),
                );
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
                return (
                    Reading::Unavailable(Why::Failed),
                    Reading::Unavailable(Why::Failed),
                );
            }
            // SAFETY: zero-initialised, then filled by the kernel.
            let stats = unsafe { stats.assume_init() };
            let compressed = u64::from(stats.compressor_page_count).saturating_mul(self.page_size);
            let app = u64::from(
                stats
                    .internal_page_count
                    .saturating_sub(stats.purgeable_count),
            )
            .saturating_mul(self.page_size);
            let wired = u64::from(stats.wire_count).saturating_mul(self.page_size);
            let cached = u64::from(
                stats
                    .external_page_count
                    .saturating_add(stats.purgeable_count),
            )
            .saturating_mul(self.page_size);
            let used = app.saturating_add(wired).saturating_add(compressed);
            (
                Reading::Value(compressed),
                Reading::Value(MemorySample {
                    used_bytes: used,
                    app_bytes: Some(app),
                    wired_bytes: Some(wired),
                    compressed_bytes: Some(compressed),
                    cached_bytes: Some(cached),
                }),
            )
        }

        fn disk(&self) -> (Reading<u64>, Reading<u64>) {
            let Some(path) = &self.disk_path else {
                return (
                    Reading::Unavailable(Why::Failed),
                    Reading::Unavailable(Why::Failed),
                );
            };
            super::super::disk::volume_bytes_c(path)
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

    /// `vm.compressor.segment.total` against `vm.compressor.segment.limit`.
    /// Both must read: one missing makes the pair unusable, with that
    /// reading's reason, never a zero.
    fn compressor_segments() -> Reading<CompressorSegments> {
        match (
            sysctl_uint(c"vm.compressor.segment.total"),
            sysctl_uint(c"vm.compressor.segment.limit"),
        ) {
            (Reading::Value(total), Reading::Value(limit)) => {
                Reading::Value(CompressorSegments { total, limit })
            }
            (Reading::Unavailable(why), _) | (_, Reading::Unavailable(why)) => {
                Reading::Unavailable(why)
            }
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
    use super::super::sensor::{Cores, CpuUsage, HostSample, Level, MemorySample, Reading, Why};
    use super::{cpu_usage_from_deltas, parse_linux_loadavg, parse_proc_stat_cpu};
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
        prev_cpu: Option<(u64, u64, u64)>,
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
                prev_cpu: None,
            }
        }

        pub(crate) fn reset(&mut self) {
            self.prev_cpu = None;
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
            let memsize = kib("MemTotal");
            let memory = match (memsize, kib("MemAvailable")) {
                (Reading::Value(total), Reading::Value(avail)) => Reading::Value(MemorySample {
                    used_bytes: total.saturating_sub(avail),
                    app_bytes: None,
                    wired_bytes: None,
                    compressed_bytes: None,
                    cached_bytes: kib("Cached").value(),
                }),
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
            let (free_disk, total_disk) = self.disk();
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
                compressor_segments: Reading::Unavailable(Why::Unsupported),
                load: match read_small("/proc/loadavg") {
                    Ok(text) => parse_linux_loadavg(&text)
                        .map_or(Reading::Unavailable(Why::Failed), Reading::Value),
                    Err(why) => Reading::Unavailable(why),
                },
                cores: self.cores,
                memsize,
                open_files: file_field(0),
                max_files: file_field(2),
                free_disk,
                thermal: Reading::Unavailable(Why::Unsupported),
                power: Reading::Unavailable(Why::Unsupported),
                cpu_usage: self.cpu_usage(),
                gpu: Reading::Unavailable(Why::Unsupported),
                memory,
                total_disk,
                thermal_temp_c: super::linux_thermal_temp_in(std::path::Path::new(
                    "/sys/class/thermal",
                )),
            }
        }

        fn cpu_usage(&mut self) -> Reading<CpuUsage> {
            let now = match read_small("/proc/stat") {
                Ok(text) => match parse_proc_stat_cpu(&text) {
                    Some(v) => v,
                    None => return Reading::Unavailable(Why::Failed),
                },
                Err(why) => return Reading::Unavailable(why),
            };
            let Some(prev) = self.prev_cpu else {
                self.prev_cpu = Some(now);
                return Reading::Unavailable(Why::NotYet);
            };
            let d_user = now.0.wrapping_sub(prev.0);
            let d_sys = now.1.wrapping_sub(prev.1);
            let d_idle = now.2.wrapping_sub(prev.2);
            let reading = cpu_usage_from_deltas(d_user, d_sys, d_idle);
            if matches!(reading, Reading::Value(_)) {
                self.prev_cpu = Some(now);
            }
            reading
        }

        fn disk(&self) -> (Reading<u64>, Reading<u64>) {
            let Some(path) = &self.disk_path else {
                return (
                    Reading::Unavailable(Why::Failed),
                    Reading::Unavailable(Why::Failed),
                );
            };
            super::super::disk::volume_bytes_c(path)
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

        pub(crate) fn reset(&mut self) {}

        /// Every reading unsupported: the default sample.
        pub(crate) fn sample(&mut self) -> HostSample {
            HostSample::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::sensor::{Reading, Why};

    #[test]
    fn boot_ids_hash_stably_and_never_to_the_unknown_value() {
        assert_eq!(hash_boot_id("abc"), hash_boot_id(" abc\n"));
        assert_ne!(hash_boot_id("abc"), hash_boot_id("abd"));
        assert_ne!(hash_boot_id(""), 0);
    }

    #[test]
    fn cpu_usage_from_deltas_handles_zero_wraparound_and_normal() {
        assert_eq!(
            cpu_usage_from_deltas(0, 0, 0),
            Reading::Unavailable(Why::NotYet)
        );
        // 30 user + 10 sys + 60 idle = 100 ticks -> 40.0% total, 30.0% user, 10.0% system.
        assert_eq!(
            cpu_usage_from_deltas(30, 10, 60),
            Reading::Value(CpuUsage {
                total_pct: 40.0,
                user_pct: 30.0,
                system_pct: 10.0,
            })
        );
        // 32-bit u32 wraparound: prev = u32::MAX - 9, now = 10 -> wrapping_sub = 20.
        let prev = u32::MAX - 9;
        let now = 10u32;
        assert_eq!(u64::from(now.wrapping_sub(prev)), 20);
        // Linux /proc/stat parser extracts (user+nice, system+irq+softirq+steal, idle+iowait).
        assert_eq!(
            parse_proc_stat_cpu("cpu  100 20 30 400 50 5 15 10\ncpu0 50 10 15 200 25 2 8 5\n"),
            Some((120, 60, 450))
        );
    }

    #[test]
    fn soc_die_sensor_filter_accepts_die_and_rejects_noise() {
        assert!(is_soc_die_temp_sensor("PMU tdie1", 97.3));
        assert!(is_soc_die_temp_sensor("eACC MTR Temp Sensor0", 54.2));
        assert!(is_soc_die_temp_sensor("pACC MTR Temp Sensor3", 61.0));
        assert!(is_soc_die_temp_sensor("gACC MTR Temp Sensor0", 58.0));
        assert!(is_soc_die_temp_sensor("gACC0", 55.5));
        assert!(!is_soc_die_temp_sensor("PMU tdev3", -9201.1));
        assert!(!is_soc_die_temp_sensor("PMU tdev3", 45.0));
        assert!(!is_soc_die_temp_sensor("gas gauge battery", 30.5));
        assert!(!is_soc_die_temp_sensor("NAND CH0 temp", 43.0));
        assert!(!is_soc_die_temp_sensor("PMU tcal", 51.8));
        assert!(!is_soc_die_temp_sensor("PMU tdie1", f64::NAN));
        assert!(!is_soc_die_temp_sensor("PMU tdie1", 0.0));
        assert!(!is_soc_die_temp_sensor("PMU tdie1", 200.0));
    }

    #[test]
    fn gpu_selection_never_splices_counters_across_gpus_and_reports_unreadable() {
        assert_eq!(
            select_gpu_sample(&[]),
            Reading::Unavailable(Why::Unsupported)
        );
        assert_eq!(
            select_gpu_sample(&[GpuEntryReading {
                utilization_pct: None,
                renderer_pct: Some(12.0),
                tiler_pct: None,
                memory_in_use: Some(1024),
                memory_allocated: None,
            }]),
            Reading::Unavailable(Why::Unreadable)
        );
        // Discrete GPU (util 75%, no tiler) beats integrated GPU (util 10%, tiler 8%):
        // tiler must come strictly from the winning entry (None), never spliced from GPU 0.
        let picked = select_gpu_sample(&[
            GpuEntryReading {
                utilization_pct: Some(10.0),
                renderer_pct: Some(9.0),
                tiler_pct: Some(8.0),
                memory_in_use: Some(100),
                memory_allocated: Some(200),
            },
            GpuEntryReading {
                utilization_pct: Some(75.0),
                renderer_pct: Some(70.0),
                tiler_pct: None,
                memory_in_use: Some(400),
                memory_allocated: Some(800),
            },
        ]);
        assert_eq!(
            picked,
            Reading::Value(crate::sysload::sensor::GpuSample {
                utilization_pct: 75.0,
                renderer_pct: Some(70.0),
                tiler_pct: None,
                memory_in_use: Some(500),
                memory_allocated: Some(1000),
            })
        );
    }

    #[test]
    fn linux_thermal_parser_and_directory_walk_handle_millidegrees_and_noise() {
        assert_eq!(parse_linux_thermal_temp_c("45230\n"), Some(45.2));
        assert_eq!(parse_linux_thermal_temp_c("0\n"), None);
        assert_eq!(parse_linux_thermal_temp_c("-1000\n"), None);
        assert_eq!(parse_linux_thermal_temp_c("200000\n"), None);
        assert_eq!(parse_linux_thermal_temp_c("not-a-number"), None);

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            linux_thermal_temp_in(&dir.path().join("missing")),
            Reading::Unavailable(Why::Unsupported)
        );
        std::fs::create_dir(dir.path().join("thermal_zone0")).unwrap();
        std::fs::write(dir.path().join("thermal_zone0/temp"), "42340\n").unwrap();
        std::fs::create_dir(dir.path().join("thermal_zone1")).unwrap();
        std::fs::write(dir.path().join("thermal_zone1/temp"), "68760\n").unwrap();
        std::fs::create_dir(dir.path().join("thermal_zone2")).unwrap();
        std::fs::write(dir.path().join("thermal_zone2/temp"), "-1000\n").unwrap();
        std::fs::create_dir(dir.path().join("cooling_device0")).unwrap();
        std::fs::write(dir.path().join("cooling_device0/temp"), "99000\n").unwrap();
        assert_eq!(linux_thermal_temp_in(dir.path()), Reading::Value(68.8));

        // A directory where the reading file should be: opening it fails
        // as a read error on Unix (EISDIR) and as access denied on Windows.
        let err_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(err_dir.path().join("thermal_zone0/temp")).unwrap();
        assert_eq!(
            linux_thermal_temp_in(err_dir.path()),
            Reading::Unavailable(if cfg!(windows) {
                Why::Unreadable
            } else {
                Why::Failed
            })
        );

        assert_eq!(
            parse_linux_loadavg("1.25 0.50 0.12 2/400 12345\n"),
            Some([1.25, 0.50, 0.12])
        );
        assert_eq!(parse_linux_loadavg("NaN 0.50 0.12"), None);
        assert_eq!(parse_linux_loadavg("inf 0.50 0.12"), None);
        assert_eq!(parse_linux_loadavg("-0.1 0.50 0.12"), None);
    }

    #[test]
    fn host_probe_is_send_and_reset_clears_cpu_baseline() {
        fn assert_send<T: Send>() {}
        assert_send::<HostProbe>();

        let dir = tempfile::tempdir().unwrap();
        let mut probe = HostProbe::new(dir.path());
        // What a probe with no baseline yet reads: not-yet where there is a
        // sensor, and (Windows) unsupported, which has no baseline to clear.
        let unprimed =
            Reading::Unavailable(if cfg!(any(target_os = "macos", target_os = "linux")) {
                Why::NotYet
            } else {
                Why::Unsupported
            });
        let first = probe.sample();
        assert_eq!(first.cpu_usage, unprimed);
        std::thread::sleep(std::time::Duration::from_millis(25));
        let _second = probe.sample();
        probe.reset();
        let after_reset = probe.sample();
        assert_eq!(after_reset.cpu_usage, unprimed);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn host_probe_reuses_hid_client_across_repeated_samples() {
        use crate::procguard::LockedSpawn;
        let virtualised = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "kern.hv_vmm_present"])
            .output_locked()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1");

        let dir = tempfile::tempdir().unwrap();
        let mut probe = HostProbe::new(dir.path());
        let a = probe.sample();
        let (ptr_a, _) = probe.hid_client_state();
        let b = probe.sample();
        let (ptr_b, _) = probe.hid_client_state();
        let c = probe.sample();
        let (ptr_c, _) = probe.hid_client_state();
        if !virtualised {
            assert!(
                matches!(a.thermal_temp_c, Reading::Value(t) if (10.0..=130.0).contains(&t)),
                "bare-metal macOS must read die temp: {a:?}"
            );
            assert!(ptr_a.is_some(), "bare-metal macOS must cache HID client");
        }
        assert_eq!(ptr_a, ptr_b);
        assert_eq!(ptr_b, ptr_c);
        assert_eq!(
            matches!(a.thermal_temp_c, Reading::Value(_)),
            matches!(b.thermal_temp_c, Reading::Value(_))
        );
        assert_eq!(
            matches!(b.thermal_temp_c, Reading::Value(_)),
            matches!(c.thermal_temp_c, Reading::Value(_))
        );
        probe.reset();
        let (after_ptr, after_had) = probe.hid_client_state();
        assert!(after_ptr.is_none());
        assert!(!after_had);
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
            // A virtual machine — GitHub's macOS runners — need not publish
            // the compressor, thermal or power readings: there `compressor`
            // reads `Unavailable(Unsupported)`, which is the named reason this
            // test allows. On bare metal they must be readable.
            use crate::procguard::LockedSpawn;
            let virtualised = std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.hv_vmm_present"])
                .output_locked()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1");
            assert!(matches!(sample.pressure, Reading::Value(_)), "{sample:?}");
            assert_eq!(sample.cpu_usage, Reading::Unavailable(Why::NotYet));
            assert!(
                matches!(sample.memory, Reading::Value(m) if m.used_bytes > 0),
                "{sample:?}"
            );
            assert!(
                matches!(
                    (sample.free_disk, sample.total_disk),
                    (Reading::Value(f), Reading::Value(t)) if t >= f && t > 0
                ),
                "{sample:?}"
            );
            if !virtualised {
                assert!(matches!(sample.compressor, Reading::Value(_)), "{sample:?}");
                // The segment sysctls read as plain integers in-process.
                assert!(
                    matches!(sample.compressor_segments, Reading::Value(s) if s.limit > 0 && s.total <= s.limit),
                    "{sample:?}"
                );
                assert!(matches!(sample.thermal, Reading::Value(_)), "{sample:?}");
                assert!(matches!(sample.power, Reading::Value(_)), "{sample:?}");
                assert!(
                    matches!(sample.gpu, Reading::Value(g) if (0.0..=100.0).contains(&g.utilization_pct)),
                    "{sample:?}"
                );
            }
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
            assert_ne!(boot_id(), 0);
            assert!(shared_monotonic_ns() > 0);
            assert!(thread_cpu_ns().is_some());

            // Second sample after a short interval produces a valid CPU delta.
            std::thread::sleep(std::time::Duration::from_millis(60));
            let second = probe.sample();
            assert!(
                matches!(
                    second.cpu_usage,
                    Reading::Value(c) if (0.0..=100.0).contains(&c.total_pct)
                ) || second.cpu_usage == Reading::Unavailable(Why::NotYet),
                "{second:?}"
            );
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
            assert!(
                matches!(sample.memory, Reading::Value(m) if m.used_bytes > 0),
                "{sample:?}"
            );
            assert!(
                matches!(
                    (sample.free_disk, sample.total_disk),
                    (Reading::Value(f), Reading::Value(t)) if t >= f && t > 0
                ),
                "{sample:?}"
            );
            assert!(matches!(sample.open_files, Reading::Value(_)), "{sample:?}");
            assert_eq!(sample.compressor, Reading::Unavailable(Why::Unsupported));
            assert_eq!(sample.cpu_usage, Reading::Unavailable(Why::NotYet));
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
            let activity = Activity::begin("Waylode host-load sampler test").unwrap();
            drop(activity);
        })
        .join()
        .unwrap();
    }
}
