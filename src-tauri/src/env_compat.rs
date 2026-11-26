//! Dual-read environment variable helpers for the GitPulse -> Waylode transition.
//!
//! New `WAYLODE_*` variables always take precedence over legacy `GITPULSE_*`
//! variables when set and non-empty. An unset or empty primary variable falls
//! back to the legacy variable.

use std::ffi::OsString;

/// Read `primary` if set and non-empty; otherwise fall back to `legacy` if set
/// and non-empty.
pub fn var_compat(primary: &str, legacy: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var(legacy).ok().filter(|value| !value.is_empty()))
}

/// Read `primary` as an [`OsString`] if set and non-empty; otherwise fall back
/// to `legacy` if set and non-empty.
pub fn var_os_compat(primary: &str, legacy: &str) -> Option<OsString> {
    std::env::var_os(primary)
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os(legacy).filter(|value| !value.is_empty()))
}

/// Return `true` when the effective value from `(primary, legacy)` is a truthy
/// flag (`1`, `true`, `yes`, or `on`, ASCII case-insensitive, trimmed).
///
/// A non-empty falsy `primary` value (for example `"0"` or `"false"`) overrides
/// a truthy `legacy` value and returns `false`.
pub fn is_truthy_compat(primary: &str, legacy: &str) -> bool {
    let Some(value) = var_os_compat(primary, legacy).and_then(|raw| raw.into_string().ok()) else {
        return false;
    };
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::env::{bind_env, env_serial};

    #[test]
    fn var_compat_prefers_non_empty_primary_over_legacy() {
        const P: &str = "WAYLODE_ENV_COMPAT_TEST_1_PRIMARY";
        const L: &str = "GITPULSE_ENV_COMPAT_TEST_1_LEGACY";
        let serial = env_serial();
        let _env = bind_env(&serial).set(P, "new-value").set(L, "old-value");

        assert_eq!(var_compat(P, L).as_deref(), Some("new-value"));
    }

    #[test]
    fn var_compat_falls_back_to_legacy_when_primary_unset_or_empty() {
        const P: &str = "WAYLODE_ENV_COMPAT_TEST_2_PRIMARY";
        const L: &str = "GITPULSE_ENV_COMPAT_TEST_2_LEGACY";
        let serial = env_serial();

        {
            let _env = bind_env(&serial).remove(P).remove(L);
            assert_eq!(var_compat(P, L), None);
        }
        {
            let _env = bind_env(&serial).remove(P).set(L, "legacy-value");
            assert_eq!(var_compat(P, L).as_deref(), Some("legacy-value"));
        }
        {
            let _env = bind_env(&serial).set(P, "").set(L, "legacy-value");
            assert_eq!(var_compat(P, L).as_deref(), Some("legacy-value"));
        }
        {
            let _env = bind_env(&serial).set(P, "").set(L, "");
            assert_eq!(var_compat(P, L), None);
        }
    }

    #[test]
    fn var_os_compat_prefers_non_empty_primary_and_falls_back() {
        const P: &str = "WAYLODE_ENV_COMPAT_TEST_3_PRIMARY";
        const L: &str = "GITPULSE_ENV_COMPAT_TEST_3_LEGACY";
        let serial = env_serial();

        {
            let _env = bind_env(&serial).remove(P).remove(L);
            assert_eq!(var_os_compat(P, L), None);
        }
        {
            let _env = bind_env(&serial).remove(P).set(L, "legacy-os");
            assert_eq!(var_os_compat(P, L), Some(OsString::from("legacy-os")));
        }
        {
            let _env = bind_env(&serial).set(P, "").set(L, "legacy-os");
            assert_eq!(var_os_compat(P, L), Some(OsString::from("legacy-os")));
        }
        {
            let _env = bind_env(&serial).set(P, "primary-os").set(L, "legacy-os");
            assert_eq!(var_os_compat(P, L), Some(OsString::from("primary-os")));
        }
    }

    #[test]
    fn is_truthy_compat_recognizes_truthy_values_and_respects_primary_override() {
        const P: &str = "WAYLODE_ENV_COMPAT_TEST_4_PRIMARY";
        const L: &str = "GITPULSE_ENV_COMPAT_TEST_4_LEGACY";
        let serial = env_serial();

        {
            let _env = bind_env(&serial).remove(P).remove(L);
            assert!(!is_truthy_compat(P, L));
        }

        for truthy in ["1", "true", "TRUE", " yes ", "On"] {
            let _env = bind_env(&serial).remove(P).set(L, truthy);
            assert!(is_truthy_compat(P, L), "expected {truthy:?} to be truthy");
        }

        for falsy in ["", "   ", "0", "false", "no", "off", "random"] {
            let _env = bind_env(&serial).remove(P).set(L, falsy);
            assert!(!is_truthy_compat(P, L), "expected {falsy:?} to be falsy");
        }

        // Non-empty falsy primary overrides truthy legacy.
        {
            let _env = bind_env(&serial).set(L, "1").set(P, "0");
            assert!(!is_truthy_compat(P, L));
        }

        // Empty primary falls back to truthy legacy.
        {
            let _env = bind_env(&serial).set(L, "1").set(P, "");
            assert!(is_truthy_compat(P, L));
        }
    }
}
