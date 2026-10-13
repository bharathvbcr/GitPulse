//! Whether a per-user LaunchAgent still points at the executable that is
//! running.
//!
//! GitPulse registers two agents: the hygiene cleaner (`StartInterval` 60,
//! `RunAtLoad`) and the launch-at-login entry. Each records one fixed
//! executable path and is rewritten only when Settings is saved. An app that
//! was moved, translocated or replaced by an older build leaves an agent
//! starting something else, every 60 seconds in the cleaner's case. Startup
//! compares what each agent records with `current_exe` and either registers
//! the running app again or says why it did not.

use std::path::Path;

/// The most a plist is read for: the agents are a few hundred bytes.
const MAX_PLIST: u64 = 16 * 1024;

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

/// Re-registers the launch-at-login agent `~/Library/LaunchAgents/{name}.plist`
/// when it starts something other than the running app. `enable` is the
/// plugin's own registration, which rewrites the plist from `current_exe`.
/// Never registers a translocated path, and never creates an agent the user
/// did not ask for.
#[cfg(target_os = "macos")]
pub fn refresh_autostart(name: &str, enable: impl FnOnce() -> Result<(), String>) {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
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
            log::warn!(target: "setup", "the launch-at-login agent starts {registered:?}, but this copy runs from a translocated path; move GitPulse to Applications and reopen it");
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
        let current = dir.path().join("gitpulse");
        std::fs::write(&current, b"x").unwrap();
        let agent = dir.path().join("agent.plist");

        assert_eq!(inspect(&agent, &current).unwrap(), Registration::Absent);

        std::fs::write(&agent, plist(current.to_str().unwrap())).unwrap();
        assert_eq!(inspect(&agent, &current).unwrap(), Registration::Current);

        // The app moved: the agent still names the old location.
        let old = dir.path().join("Old.app/Contents/MacOS/gitpulse");
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
            "/private/var/folders/x/AppTranslocation/ABC/d/GitPulse.app/Contents/MacOS/gitpulse"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/GitPulse.app/Contents/MacOS/gitpulse"
        )));
    }
}
