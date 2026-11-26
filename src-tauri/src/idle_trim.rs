//! Idle memory trimming pass run when the main WebView hibernates.
//!
//! Releases cold in-process caches (`engine::ref_cache`, `docs::vault_cache`),
//! performs non-blocking `PRAGMA shrink_memory; PRAGMA wal_checkpoint(PASSIVE);`
//! on existing idle SQLite connections (`workbench`, `churn_store`, `ledger`)
//! without opening any new connections, and asks the OS allocator to release
//! unused heap pages back to the system.

use serde::Serialize;
use tauri::Manager as _;

/// Summary of work performed during an idle memory trim pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IdleTrimReport {
    pub ref_cache_evicted: usize,
    pub docs_vault_evicted: usize,
    pub sqlite_trimmed: usize,
    pub os_allocator_trimmed: bool,
}

/// Runs a best-effort, non-blocking idle memory trim pass across caches,
/// already-open idle SQLite connections, and the OS heap allocator.
pub fn run_idle_trim<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> IdleTrimReport {
    let ref_cache_evicted = crate::engine::ref_cache::trim_idle();
    let docs_vault_evicted = crate::docs::trim_idle_cache();
    let sqlite_trimmed = trim_idle_sqlite_stores(app);
    let os_allocator_trimmed = trim_os_allocator();
    IdleTrimReport {
        ref_cache_evicted,
        docs_vault_evicted,
        sqlite_trimmed,
        os_allocator_trimmed,
    }
}

fn trim_idle_sqlite_stores<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> usize {
    let mut trimmed = 0usize;

    if let Some(workbench) = app.try_state::<crate::workbench::WorkbenchState>() {
        if workbench.trim_idle() {
            trimmed += 1;
        }
    }

    trimmed += crate::engine::churn_store::trim_idle();
    trimmed += crate::ledger::trim_idle();
    trimmed
}

/// Asks the platform memory allocator to release idle heap pages back to the OS.
pub fn trim_os_allocator() -> bool {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: Passing `NULL` for `zone` and `0` for `goal` is documented by
        // Apple's `<malloc/malloc.h>` to relieve pressure across all zones
        // safely without touching live allocations.
        unsafe {
            let _ = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
        true
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: libc::size_t) -> std::os::raw::c_int;
        }
        // SAFETY: `malloc_trim(0)` is thread-safe in glibc and releases free
        // top-of-heap pages back to the kernel.
        unsafe {
            let _ = malloc_trim(0);
        }
        true
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_trim_report_serializes_expected_fields() {
        let report = IdleTrimReport {
            ref_cache_evicted: 3,
            docs_vault_evicted: 2,
            sqlite_trimmed: 1,
            os_allocator_trimmed: true,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["ref_cache_evicted"], 3);
        assert_eq!(json["docs_vault_evicted"], 2);
        assert_eq!(json["sqlite_trimmed"], 1);
        assert_eq!(json["os_allocator_trimmed"], true);
    }

    #[test]
    fn allocator_trim_is_safe_under_live_allocations() {
        let mut live = vec![vec![42u8; 4096]; 64];
        let trimmed = trim_os_allocator();
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
        assert!(trimmed);
        #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
        let _ = trimmed;

        assert_eq!(live.len(), 64);
        assert_eq!(live[0][0], 42);
        live.clear();
        let _ = trim_os_allocator();
    }

    #[test]
    fn workbench_trim_idle_is_nonblocking_when_unopened() {
        let wb = crate::workbench::WorkbenchState::default();
        assert!(!wb.trim_idle());
    }
}
