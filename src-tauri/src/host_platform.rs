//! What the host operating system is, and which natively-gated desktop
//! features this binary actually compiled in.
//!
//! The frontend previously inferred the platform from `navigator.platform`,
//! which answers only "Mac or not Mac" and says nothing about whether a
//! feature's implementation is present. That produced settings a reader could
//! toggle on a host where the code backing them does not exist.
//!
//! This module reports only capabilities that no other component already owns.
//! Native notification availability stays owned by the notification
//! coordinator (it is a runtime probe, not a compile-time fact) and closed-app
//! cleaner scheduling stays owned by the cleaner; duplicating either here
//! would create a second answer that can disagree with the first.

/// The host platform and its compile-time desktop capabilities.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostPlatform {
    /// `std::env::consts::OS` verbatim — "macos", "windows", "linux", …
    ///
    /// The compiled target, not a user-agent guess: a webview UA can be
    /// spoofed or reported oddly, and it cannot distinguish Windows from Linux
    /// in a way the backing code agrees with.
    pub os: &'static str,
    /// Whether GitPulse can hide its Dock/taskbar icon for menu-bar-only mode.
    ///
    /// See [`crate::desktop::DOCK_HIDING`] — the activation-policy path exists
    /// only on macOS, so every other host must not be offered the toggle.
    pub dock_hiding: bool,
}

impl HostPlatform {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS,
            dock_hiding: crate::desktop::DOCK_HIDING,
        }
    }
}

#[tauri::command]
pub fn cmd_host_platform() -> HostPlatform {
    HostPlatform::detect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported OS must be the compiled target. A hard-coded or inferred
    /// value here would defeat the entire point of asking the backend.
    #[test]
    fn reports_the_compiled_target_os() {
        let platform = HostPlatform::detect();
        assert_eq!(platform.os, std::env::consts::OS);
        assert!(
            !platform.os.is_empty(),
            "std::env::consts::OS is never empty"
        );
    }

    /// Dock hiding is advertised exactly where the activation-policy path is
    /// compiled. If `desktop::apply_dock_policy` is ever made portable, or its
    /// macOS gate is removed, this pins the capability report to follow.
    #[test]
    fn dock_hiding_tracks_the_macos_activation_policy_path() {
        assert_eq!(
            HostPlatform::detect().dock_hiding,
            cfg!(target_os = "macos")
        );
    }

    /// The wire shape the frontend parses. Renaming a field silently turns the
    /// gate it feeds into `undefined`, which is falsy — a feature would vanish
    /// on every platform rather than fail loudly.
    #[test]
    fn serializes_the_fields_the_frontend_reads() {
        let json = serde_json::to_value(HostPlatform::detect()).unwrap();
        let object = json.as_object().expect("a JSON object");
        assert!(object.contains_key("os"), "missing os: {json}");
        assert!(
            object.contains_key("dock_hiding"),
            "missing dock_hiding: {json}"
        );
        assert_eq!(object.len(), 2, "unexpected extra fields: {json}");
    }
}
