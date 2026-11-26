//! Whether a per-user LaunchAgent or login autostart entry still points at
//! the executable that is running, plus cross-platform migration of legacy
//! `"GitPulse"` login autostart registrations.
//!
//! Waylode registers two agents on macOS: the hygiene cleaner (`StartInterval`
//! 60, `RunAtLoad`) and the launch-at-login entry (`~/Library/LaunchAgents/Waylode.plist`).
//! Windows uses `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Waylode`,
//! and Linux uses `~/.config/autostart/Waylode.desktop`.

use std::path::Path;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;

/// The most a plist or desktop entry is read for: the entries are a few hundred bytes.
const MAX_PLIST: u64 = 16 * 1024;
const LEGACY_AUTOSTART_NAME: &str = "GitPulse";
#[cfg(any(target_os = "windows", test))]
const WIN_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(any(target_os = "windows", test))]
const WIN_STARTUP_APPROVED_KEY: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// No agent is installed.
    Absent,
    /// The agent starts the running executable.
    Current,
    /// The agent starts something else: a moved, removed or older app.
    Stale { registered: String },
}

/// The executable a LaunchAgent plist starts: the first `<string>` of its
/// `ProgramArguments` array, with XML entities undone.
pub fn registered_executable(document: &str) -> Option<String> {
    let after_key = &document[document.find("<key>ProgramArguments</key>")?..];
    let array = &after_key[after_key.find("<array>")? + "<array>".len()..];
    let array = &array[..array.find("</array>")?];
    let start = array.find("<string>")? + "<string>".len();
    let end = array[start..].find("</string>")? + start;
    Some(
        array[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Whether `path` lies under Gatekeeper's randomised read-only copy of a
/// quarantined app. Registering it would point an agent at a path that
/// disappears when the app quits.
pub fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str() == "AppTranslocation")
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Classifies the plist at `plist` (`None` when it does not exist) against the
/// executable that is running.
pub fn classify(document: Option<&str>, current: &Path) -> Registration {
    let Some(document) = document else {
        return Registration::Absent;
    };
    match registered_executable(document) {
        Some(registered) if same_file(Path::new(&registered), current) => Registration::Current,
        Some(registered) => Registration::Stale { registered },
        // A plist this cannot read an executable from starts nothing known.
        None => Registration::Stale {
            registered: String::new(),
        },
    }
}

// Read by the macOS and XDG migrations; Windows migrates through the registry.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn read_bounded_regular_file(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > MAX_PLIST {
        return Ok(Some(String::new()));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    file.take(MAX_PLIST + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(text))
}

/// [`classify`] for the file at `plist`. A file larger than [`MAX_PLIST`] is
/// reported as stale rather than read.
pub fn inspect(plist: &Path, current: &Path) -> Result<Registration, String> {
    use std::io::Read;
    let file = match std::fs::File::open(plist) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registration::Absent)
        }
        Err(error) => return Err(format!("{}: {error}", plist.display())),
    };
    let mut text = String::new();
    file.take(MAX_PLIST + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", plist.display()))?;
    if text.len() as u64 > MAX_PLIST {
        return Ok(Registration::Stale {
            registered: String::new(),
        });
    }
    Ok(classify(Some(&text), current))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartMigration {
    /// No legacy GitPulse autostart entry was found.
    Absent,
    /// Migrated enabled state (when target was absent) and removed legacy entry.
    MigratedAndRemoved,
    /// Target already existed; removed leftover legacy entry.
    RemovedLeftover,
    /// Legacy entry belonged to something unrelated; left untouched.
    IgnoredForeign,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn looks_like_legacy_plist(doc: &str) -> bool {
    doc.contains("<string>GitPulse</string>") || doc.to_ascii_lowercase().contains("gitpulse")
}

#[cfg(any(not(any(target_os = "macos", target_os = "windows")), test))]
fn looks_like_legacy_desktop(doc: &str) -> bool {
    doc.contains("Name=GitPulse") || doc.to_ascii_lowercase().contains("gitpulse")
}

/// Migrates macOS `~/Library/LaunchAgents/GitPulse.plist` -> `{name}.plist`.
#[cfg(any(target_os = "macos", test))]
pub fn migrate_macos_autostart_in(
    launch_agents_dir: &Path,
    new_name: &str,
    current_exe: &Path,
    mut enable: impl FnMut() -> Result<(), String>,
    mut bootout: impl FnMut(&Path) -> Result<(), String>,
) -> Result<AutostartMigration, String> {
    let legacy_plist = launch_agents_dir.join(format!("{LEGACY_AUTOSTART_NAME}.plist"));
    let Some(doc) = read_bounded_regular_file(&legacy_plist)? else {
        return Ok(AutostartMigration::Absent);
    };
    if doc.is_empty() || !looks_like_legacy_plist(&doc) {
        return Ok(AutostartMigration::IgnoredForeign);
    }
    let target_plist = launch_agents_dir.join(format!("{new_name}.plist"));
    let outcome = if !target_plist.exists() {
        if is_translocated(current_exe) {
            return Err("the legacy launch-at-login agent exists, but this copy runs from a translocated path".into());
        }
        enable()?;
        AutostartMigration::MigratedAndRemoved
    } else {
        AutostartMigration::RemovedLeftover
    };
    let _ = bootout(&legacy_plist);
    std::fs::remove_file(&legacy_plist)
        .map_err(|e| format!("failed to remove {}: {e}", legacy_plist.display()))?;
    Ok(outcome)
}

/// Migrates Linux `~/.config/autostart/GitPulse.desktop` -> `{name}.desktop`.
#[cfg(any(not(any(target_os = "macos", target_os = "windows")), test))]
pub fn migrate_linux_autostart_in(
    autostart_dir: &Path,
    new_name: &str,
    mut enable: impl FnMut() -> Result<(), String>,
) -> Result<AutostartMigration, String> {
    let legacy_desktop = autostart_dir.join(format!("{LEGACY_AUTOSTART_NAME}.desktop"));
    let Some(doc) = read_bounded_regular_file(&legacy_desktop)? else {
        return Ok(AutostartMigration::Absent);
    };
    if doc.is_empty() || !looks_like_legacy_desktop(&doc) {
        return Ok(AutostartMigration::IgnoredForeign);
    }
    let target_desktop = autostart_dir.join(format!("{new_name}.desktop"));
    let outcome = if !target_desktop.exists() {
        enable()?;
        AutostartMigration::MigratedAndRemoved
    } else {
        AutostartMigration::RemovedLeftover
    };
    std::fs::remove_file(&legacy_desktop)
        .map_err(|e| format!("failed to remove {}: {e}", legacy_desktop.display()))?;
    Ok(outcome)
}

/// Migrates Windows `HKCU\...\Run\GitPulse` -> `{new_name}` via `reg.exe` runner.
#[cfg(any(target_os = "windows", test))]
pub fn migrate_windows_autostart_with(
    new_name: &str,
    mut enable: impl FnMut() -> Result<(), String>,
    mut reg: impl FnMut(&[&str]) -> Result<bool, String>,
) -> Result<AutostartMigration, String> {
    let legacy_present = reg(&["query", WIN_RUN_KEY, "/v", LEGACY_AUTOSTART_NAME])?;
    if !legacy_present {
        return Ok(AutostartMigration::Absent);
    }
    let target_present = reg(&["query", WIN_RUN_KEY, "/v", new_name])?;
    let outcome = if !target_present {
        enable()?;
        AutostartMigration::MigratedAndRemoved
    } else {
        AutostartMigration::RemovedLeftover
    };
    if !reg(&["delete", WIN_RUN_KEY, "/v", LEGACY_AUTOSTART_NAME, "/f"])? {
        return Err(format!(
            "could not delete legacy {WIN_RUN_KEY}\\{LEGACY_AUTOSTART_NAME}"
        ));
    }
    let _ = reg(&[
        "delete",
        WIN_STARTUP_APPROVED_KEY,
        "/v",
        LEGACY_AUTOSTART_NAME,
        "/f",
    ]);
    Ok(outcome)
}

#[cfg(target_os = "macos")]
fn bootout_legacy_plist(plist: &Path) -> Result<(), String> {
    // SAFETY: geteuid has no pointers or side effects.
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let Some(path_str) = plist.to_str() else {
        return Ok(());
    };
    let _ = crate::engine::process::capture_command(
        "/bin/launchctl",
        &["bootout", &domain, path_str],
        None,
        std::time::Duration::from_secs(5),
        &[],
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_reg(args: &[&str]) -> Result<bool, String> {
    let output = crate::engine::process::capture_command(
        "reg.exe",
        args,
        None,
        std::time::Duration::from_secs(5),
        &[],
    )
    .map_err(|e| e.message().to_string())?;
    Ok(output.success)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn xdg_autostart_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|base| base.join("autostart"))
}

/// Migrates any legacy `"GitPulse"` login autostart registration on the current
/// OS and (on macOS) refreshes a stale LaunchAgent path.
pub fn migrate_and_refresh_autostart(name: &str, mut enable: impl FnMut() -> Result<(), String>) {
    #[cfg(target_os = "macos")]
    {
        if let (Some(home), Ok(current)) = (
            std::env::var_os("HOME").map(PathBuf::from),
            std::env::current_exe(),
        ) {
            let dir = home.join("Library/LaunchAgents");
            match migrate_macos_autostart_in(
                &dir,
                name,
                &current,
                &mut enable,
                bootout_legacy_plist,
            ) {
                Ok(AutostartMigration::MigratedAndRemoved) => {
                    log::info!(target: "setup", "migrated legacy GitPulse login autostart to {name}");
                }
                Ok(AutostartMigration::RemovedLeftover) => {
                    log::info!(target: "setup", "removed leftover GitPulse login autostart entry");
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!(target: "setup", "legacy login autostart migration: {error}");
                }
            }
        }
        refresh_autostart(name, enable);
    }
    #[cfg(target_os = "windows")]
    {
        match migrate_windows_autostart_with(name, &mut enable, run_reg) {
            Ok(AutostartMigration::MigratedAndRemoved) => {
                log::info!(target: "setup", "migrated legacy GitPulse Windows Run autostart to {name}");
            }
            Ok(AutostartMigration::RemovedLeftover) => {
                log::info!(target: "setup", "removed leftover GitPulse Windows Run autostart entry");
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!(target: "setup", "legacy Windows autostart migration: {error}");
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(dir) = xdg_autostart_dir() {
            match migrate_linux_autostart_in(&dir, name, &mut enable) {
                Ok(AutostartMigration::MigratedAndRemoved) => {
                    log::info!(target: "setup", "migrated legacy GitPulse XDG autostart to {name}");
                }
                Ok(AutostartMigration::RemovedLeftover) => {
                    log::info!(target: "setup", "removed leftover GitPulse XDG autostart entry");
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!(target: "setup", "legacy XDG autostart migration: {error}");
                }
            }
        }
    }
}

/// Re-registers the launch-at-login agent `~/Library/LaunchAgents/{name}.plist`
/// when it starts something other than the running app. `enable` is the
/// plugin's own registration, which rewrites the plist from `current_exe`.
/// Never registers a translocated path, and never creates an agent the user
/// did not ask for.
#[cfg(target_os = "macos")]
pub fn refresh_autostart(name: &str, enable: impl FnOnce() -> Result<(), String>) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{name}.plist"));
    let current = match std::env::current_exe() {
        Ok(current) => current,
        Err(error) => {
            log::warn!(target: "setup", "launch-at-login check: {error}");
            return;
        }
    };
    match inspect(&plist, &current) {
        Ok(Registration::Stale { registered }) if is_translocated(&current) => {
            log::warn!(target: "setup", "the launch-at-login agent starts {registered:?}, but this copy runs from a translocated path; move Waylode to Applications and reopen it");
        }
        Ok(Registration::Stale { registered }) => match enable() {
            Ok(()) => {
                log::warn!(target: "setup", "the launch-at-login agent started {registered:?}; it now starts this app")
            }
            Err(error) => {
                log::warn!(target: "setup", "the launch-at-login agent starts {registered:?} and could not be re-registered: {error}")
            }
        },
        Ok(_) => {}
        Err(error) => log::warn!(target: "setup", "launch-at-login check: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plist(executable: &str) -> String {
        format!("<plist><dict><key>Label</key><string>x</string>\n<key>ProgramArguments</key><array><string>{executable}</string><string>--background</string></array></dict></plist>")
    }

    #[test]
    fn the_registered_executable_is_the_first_program_argument() {
        assert_eq!(
            registered_executable(&plist(
                "/Applications/Git &amp; Pulse.app/Contents/MacOS/gitpulse"
            ))
            .as_deref(),
            Some("/Applications/Git & Pulse.app/Contents/MacOS/gitpulse")
        );
        assert_eq!(registered_executable("<plist/>"), None);
    }

    #[test]
    fn a_stale_path_is_detected_and_a_current_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("waylode");
        std::fs::write(&current, b"x").unwrap();
        let agent = dir.path().join("agent.plist");

        assert_eq!(inspect(&agent, &current).unwrap(), Registration::Absent);

        std::fs::write(&agent, plist(current.to_str().unwrap())).unwrap();
        assert_eq!(inspect(&agent, &current).unwrap(), Registration::Current);

        // The app moved: the agent still names the old location.
        let old = dir.path().join("Old.app/Contents/MacOS/waylode");
        std::fs::write(&agent, plist(old.to_str().unwrap())).unwrap();
        assert_eq!(
            inspect(&agent, &current).unwrap(),
            Registration::Stale {
                registered: old.display().to_string()
            }
        );

        // Another install that does exist is still not the running one.
        let other = dir.path().join("other");
        std::fs::write(&other, b"y").unwrap();
        std::fs::write(&agent, plist(other.to_str().unwrap())).unwrap();
        assert!(matches!(
            inspect(&agent, &current).unwrap(),
            Registration::Stale { .. }
        ));

        std::fs::write(&agent, "not a plist").unwrap();
        assert!(matches!(
            inspect(&agent, &current).unwrap(),
            Registration::Stale { .. }
        ));
        std::fs::write(&agent, "x".repeat(MAX_PLIST as usize + 1)).unwrap();
        assert!(matches!(
            inspect(&agent, &current).unwrap(),
            Registration::Stale { .. }
        ));
    }

    #[test]
    fn a_translocated_path_is_never_registered() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Waylode.app/Contents/MacOS/waylode"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/Waylode.app/Contents/MacOS/waylode"
        )));
    }

    #[test]
    fn macos_legacy_autostart_plist_migrates_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("waylode");
        std::fs::write(&current, b"exe").unwrap();
        let legacy = dir.path().join("GitPulse.plist");
        std::fs::write(
            &legacy,
            "<plist><dict><key>Label</key><string>GitPulse</string><key>ProgramArguments</key><array><string>/Applications/GitPulse.app/Contents/MacOS/gitpulse</string></array></dict></plist>",
        )
        .unwrap();

        let mut enabled = false;
        let mut booted_out = false;
        let outcome = migrate_macos_autostart_in(
            dir.path(),
            "Waylode",
            &current,
            || {
                enabled = true;
                std::fs::write(dir.path().join("Waylode.plist"), "<plist/>").unwrap();
                Ok(())
            },
            |_| {
                booted_out = true;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, AutostartMigration::MigratedAndRemoved);
        assert!(enabled);
        assert!(booted_out);
        assert!(!legacy.exists());
        assert!(dir.path().join("Waylode.plist").exists());

        // Second run is a no-op.
        assert_eq!(
            migrate_macos_autostart_in(
                dir.path(),
                "Waylode",
                &current,
                || panic!("already migrated"),
                |_| panic!("already removed"),
            )
            .unwrap(),
            AutostartMigration::Absent
        );
    }

    #[test]
    fn linux_legacy_autostart_desktop_migrates_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("GitPulse.desktop");
        std::fs::write(
            &legacy,
            "[Desktop Entry]\nType=Application\nName=GitPulse\nExec=/usr/bin/gitpulse --background\n",
        )
        .unwrap();

        let mut enabled = false;
        let outcome = migrate_linux_autostart_in(dir.path(), "Waylode", || {
            enabled = true;
            std::fs::write(
                dir.path().join("Waylode.desktop"),
                "[Desktop Entry]\nName=Waylode\n",
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

        assert_eq!(outcome, AutostartMigration::MigratedAndRemoved);
        assert!(enabled);
        assert!(!legacy.exists());
        assert!(dir.path().join("Waylode.desktop").exists());
    }

    #[test]
    fn windows_legacy_run_value_migrates_and_deletes_old_key() {
        use std::collections::BTreeSet;

        let mut run_values = BTreeSet::from(["GitPulse".to_string()]);
        let mut approved = BTreeSet::from(["GitPulse".to_string()]);
        let mut enabled = false;

        let outcome = migrate_windows_autostart_with(
            "Waylode",
            || {
                enabled = true;
                Ok(())
            },
            |args| match args {
                ["query", key, "/v", val] if *key == WIN_RUN_KEY => Ok(run_values.contains(*val)),
                ["delete", key, "/v", val, "/f"] if *key == WIN_RUN_KEY => {
                    Ok(run_values.remove(*val))
                }
                ["delete", key, "/v", val, "/f"] if *key == WIN_STARTUP_APPROVED_KEY => {
                    Ok(approved.remove(*val))
                }
                other => panic!("unexpected reg args: {other:?}"),
            },
        )
        .unwrap();

        assert_eq!(outcome, AutostartMigration::MigratedAndRemoved);
        assert!(enabled);
        assert!(!run_values.contains("GitPulse"));
        assert!(!approved.contains("GitPulse"));
    }
}
