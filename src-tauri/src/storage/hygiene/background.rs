//! Optional per-user launchd registration. No shell, privileged job or keep-alive.
//! Registration is a host concern; DevCouncil's policy stays portable.
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "com.gitpulse.hygiene";
#[cfg(any(target_os = "macos", test))]
const OWNER: &str = "<!-- GitPulse repository hygiene v1 -->";

#[cfg(any(target_os = "macos", test))]
fn app_executable(path: &Path) -> Result<&str, String> {
    let text = path.to_str().ok_or("Application path is not UTF-8")?;
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || !path.parent().is_some_and(|p| p.ends_with("Contents/MacOS"))
        || !path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .is_some_and(|p| p.extension().is_some_and(|e| e == "app"))
    {
        return Err("Closed-app scheduling requires an installed macOS application bundle".into());
    }
    super::tree::no_symlinks(path)?;
    if !std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("Application executable is not a regular file".into());
    }
    Ok(text)
}
#[cfg(any(target_os = "macos", test))]
fn plist(executable: &str) -> String {
    let escaped = executable
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{OWNER}\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array><string>{escaped}</string><string>--cleaner-due</string></array>\n<key>StartInterval</key><integer>60</integer>\n<key>RunAtLoad</key><true/>\n<key>ProcessType</key><string>Background</string>\n</dict></plist>\n")
}
#[cfg(any(target_os = "macos", test))]
fn owned(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
        Ok(meta) => {
            if !meta.is_file() || meta.len() > 16384 {
                return Err("LaunchAgent is not a bounded regular file".into());
            }
            super::tree::no_symlinks(path)?;
            use std::io::Read;
            let mut text = String::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(16385)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            if text.len() > 16384
                || !text.contains(OWNER)
                || !text.contains(&format!("<string>{LABEL}</string>"))
            {
                return Err("Existing LaunchAgent is not owned by GitPulse; preserved it".into());
            }
            Ok(true)
        }
    }
}
#[cfg(target_os = "macos")]
fn location() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("Home directory unavailable")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("Home directory must be absolute".into());
    }
    super::tree::no_symlinks(&home)?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}
pub fn supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::env::current_exe().is_ok_and(|p| app_executable(&p).is_ok())
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn configure(enable: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let path = location()?;
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let document = if enable {
            Some(plist(app_executable(&executable)?))
        } else {
            None
        };
        // SAFETY: geteuid has no pointers or side effects.
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        configure_at(&path, document.as_deref(), &domain, |args| {
            let output = crate::engine::git_cli::capture_command(
                "/bin/launchctl",
                args,
                None,
                std::time::Duration::from_secs(5),
                &[],
            )?;
            if output.success {
                return Ok(true);
            }
            // launchctl identifies an absent service explicitly. Other failures
            // (permissions, invalid GUI domain, timeout) remain errors.
            if args.first() == Some(&"print")
                && output.stderr_text().contains("Could not find service")
            {
                return Ok(false);
            }
            Err(format!(
                "launchctl failed: {}",
                output.stderr_text().chars().take(1024).collect::<String>()
            ))
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        if enable {
            Err("Closed-app scheduling is supported only on macOS".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn configure_at(
    path: &Path,
    document: Option<&str>,
    domain: &str,
    mut launch: impl FnMut(&[&str]) -> Result<bool, String>,
) -> Result<(), String> {
    let exists = owned(path)?;
    if !exists && document.is_none() {
        return Ok(());
    }
    let parent = path.parent().ok_or("Missing LaunchAgent directory")?;
    for ancestor in parent.ancestors() {
        if ancestor.try_exists().map_err(|e| e.to_string())? {
            super::tree::no_symlinks(ancestor)?;
            break;
        }
    }
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    super::tree::no_symlinks(parent)?;
    let target = format!("{domain}/{LABEL}");
    if launch(&["print", &target])? {
        if !exists {
            return Err("A foreign service uses the cleaner label; preserved it".into());
        }
        if !launch(&["bootout", &target])? {
            return Err("LaunchAgent could not be stopped".into());
        }
    }
    if let Some(document) = document {
        devmap_query::write_atomic(path, document.as_bytes()).map_err(|e| e.to_string())?;
        if !launch(&[
            "bootstrap",
            domain,
            path.to_str().ok_or("Invalid LaunchAgent path")?,
        ])? {
            return Err("LaunchAgent could not be loaded".into());
        }
    } else {
        // Authority is already disabled in the saved policy before reaching here.
        if !owned(path)? {
            return Err("LaunchAgent changed during removal".into());
        }
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_escapes_paths_and_uses_only_explicit_headless_arguments() {
        let xml = plist("/Applications/A & <B> \"C\" 'D'.app/Contents/MacOS/gitpulse");
        assert!(xml.contains("A &amp; &lt;B&gt; &quot;C&quot; &apos;D&apos;"));
        assert!(xml.contains("<string>--cleaner-due</string>"));
        assert!(!xml.contains("/bin/sh"));
        assert!(!xml.contains("KeepAlive"));
        #[cfg(target_os = "macos")]
        {
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), xml).unwrap();
            assert!(std::process::Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(file.path())
                .status()
                .unwrap()
                .success());
        }
    }
    #[test]
    fn foreign_symlink_and_oversized_jobs_are_preserved_without_launchctl() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
        let path = root.join("job.plist");
        for text in ["foreign".to_string(), "x".repeat(16385)] {
            std::fs::write(&path, &text).unwrap();
            assert!(
                configure_at(&path, Some(&plist("app")), "gui/1", |_| panic!(
                    "foreign job"
                ))
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(root.join("precious"), &path).unwrap();
            assert!(owned(&path).is_err());
        }
    }
    #[test]
    fn registration_and_disable_use_owned_path_and_propagate_failures() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
        let path = root.join("jobs/job.plist");
        let mut calls = vec![];
        configure_at(&path, Some(&plist("app")), "gui/1", |args| {
            calls.push(args[0].to_string());
            Ok(args[0] != "print")
        })
        .unwrap();
        assert_eq!(calls, ["print", "bootstrap"]);
        assert!(owned(&path).unwrap());
        assert!(configure_at(&path, Some(&plist("new")), "gui/1", |_| Err(
            "launchctl unavailable".into()
        ))
        .is_err());
        calls.clear();
        configure_at(&path, None, "gui/1", |args| {
            calls.push(args[0].to_string());
            Ok(true)
        })
        .unwrap();
        assert_eq!(calls, ["print", "bootout"]);
        assert!(!path.exists());
        configure_at(&path, None, "gui/1", |_| panic!("already absent")).unwrap();
    }
    #[test]
    fn debug_executables_and_foreign_loaded_labels_cannot_be_scheduled() {
        assert!(app_executable(Path::new("/tmp/gitpulse")).is_err());
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
        let path = root.join("job.plist");
        assert!(
            configure_at(&path, Some(&plist("app")), "gui/1", |_| Ok(true))
                .unwrap_err()
                .contains("foreign")
        );
        assert!(!path.exists());
    }
}
