//! Firebase App Hosting deployment state for the opened repository.
//!
//! Answers one question — "which commits of this repository are deployed, and
//! what happened to them?" — and answers it the way [`crate::github`] answers
//! its own: by shelling out to the CLI the user has already authenticated,
//! never by holding a credential. App Hosting's only OAuth scope is
//! `cloud-platform` (full read/write on the whole Google Cloud project), so a
//! direct REST client would have to store a credential that can delete the
//! user's data. Delegating to `firebase` keeps the property `docs/SECURITY.md`
//! publishes for GitHub true for Firebase too: GitPulse holds no token.
//!
//! ## Discovery costs nothing
//!
//! Whether a checkout deploys to Firebase is answered by two files in the
//! working tree — `.firebaserc` and `firebase.json`. That read needs no
//! network, no credential and no CLI, so [`FirebaseStatus`] is free to compute
//! on panel mount. Only the listings below reach the network.
//!
//! ## Reads here can mutate, and that is why they are gated
//!
//! `firebase apphosting:backends:list` and `apphosting:rollouts:list` both
//! declare `.before(apphosting.ensureApiEnabled)` upstream, which *enables*
//! the App Hosting API on the project when it is off. That is a change to the
//! user's Cloud project hiding inside a read, and REST would not avoid it —
//! there is no read-only scope for App Hosting. So the listing commands are
//! policy-gated like an action, and every invocation passes `--json`, which
//! upstream treats as implying non-interactive: a prompt to enable an API
//! becomes a loud error instead of a silent enablement.
//!
//! ## Nothing here reports an absence it did not verify
//!
//! Every report carries `checked` separately from its rows. A missing CLI, an
//! unauthenticated user, a disabled API and a parse failure are all reasons,
//! never an empty list — an empty rollout list means "this backend has never
//! deployed", and only a real answer may say that.

pub mod apphosting;

use crate::engine::git_cli::{capture_command, sandbox_join_canonical, validate_repo};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Bound on `firebase` stderr folded into a report. Matches the `gh` cap: a
/// chatty failure must not flood a panel or the ledger.
pub(crate) const MAX_FIREBASE_ERROR_BYTES: usize = 4 * 1024;

/// `firebase` is Node and cold-starts slowly; `gh`'s 60s is the right order.
pub(crate) const FIREBASE_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on the probe. A hung CLI must not freeze a panel mount.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `.firebaserc` and `firebase.json` are small config files. A larger one is a
/// fault worth naming, not a stream to consume — the same call
/// `analyzer::deps` makes for package manifests.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Absolute path override for the `firebase` binary.
///
/// `gui_launch_fallback_dirs` covers Homebrew, `/usr/local/bin`, `~/.local/bin`
/// and the Go and Cargo roots — and nothing else. `firebase-tools` is normally
/// `npm i -g` under nvm, fnm or volta, whose bin directories are version-scoped
/// (`~/.nvm/versions/node/v22.x/bin`) and appear in none of them. A GUI launch
/// therefore cannot see it even though a terminal can, which is exactly the
/// failure already recorded for `gh`. This is the escape hatch.
const FIREBASE_BIN_ENV: &str = "GITPULSE_FIREBASE_BIN";

/// Whether the `firebase` CLI answered, and why not when it did not.
///
/// Deliberately not a bare bool. "Not installed" and "installed but not on a
/// GUI launch's PATH" have different remedies, and a panel that renders both
/// as "not installed" sends the user to reinstall a tool they already have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseCliProbe {
    pub present: bool,
    /// Resolved program name or the overridden absolute path.
    pub program: String,
    pub version: Option<String>,
    pub reason: Option<String>,
}

/// One `.firebaserc` alias and the project id it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseProjectAlias {
    pub alias: String,
    pub project_id: String,
}

/// What the working tree says about this repository's Firebase setup.
///
/// Computed entirely from files, so it is free and always safe to call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseStatus {
    /// True when `.firebaserc` or `firebase.json` exists at the repo root.
    pub configured: bool,
    pub projects: Vec<FirebaseProjectAlias>,
    /// The `default` alias when `.firebaserc` names one. Never auto-selected
    /// on the user's behalf — see [`discover_firebase_projects`].
    pub default_alias: Option<String>,
    /// True when `firebase.json` carries an `apphosting` key.
    pub has_apphosting_config: bool,
    pub cli: FirebaseCliProbe,
    /// Per-file, so an unreadable `firebase.json` never renders as "App
    /// Hosting is not configured".
    pub firebaserc_error: Option<String>,
    pub firebasejson_error: Option<String>,
}

impl FirebaseStatus {
    fn empty(cli: FirebaseCliProbe) -> Self {
        FirebaseStatus {
            configured: false,
            projects: Vec::new(),
            default_alias: None,
            has_apphosting_config: false,
            cli,
            firebaserc_error: None,
            firebasejson_error: None,
        }
    }

    /// A status that could not be read at all, carrying why.
    ///
    /// Distinct from the default empty status: "we could not look" and "we
    /// looked and this repository has no Firebase config" are different
    /// answers, and only the second may render as "not a Firebase project".
    pub fn unreadable(reason: String) -> Self {
        let mut status = FirebaseStatus::empty(FirebaseCliProbe {
            present: false,
            program: firebase_program(),
            version: None,
            reason: Some(reason.clone()),
        });
        status.firebaserc_error = Some(reason);
        status
    }
}

/// The program name to spawn: the override when set and non-empty, else the
/// bare name for PATH resolution.
pub(crate) fn firebase_program() -> String {
    match std::env::var(FIREBASE_BIN_ENV) {
        Ok(path) if !path.trim().is_empty() => path.trim().to_string(),
        _ => "firebase".to_string(),
    }
}

/// Bounded probe for the `firebase` CLI.
///
/// Routed through `capture_command` for the same reason `gh_cli_present` is: a
/// raw `Command::output()` would block forever on a hung CLI (a stale
/// credential helper, a stuck network mount) and freeze the panel that called
/// it. Success is treated as presence; the reason distinguishes the two ways
/// absence happens.
pub fn firebase_cli_probe() -> FirebaseCliProbe {
    let program = firebase_program();
    let overridden = program != "firebase";
    match capture_command(&program, &["--version"], None, PROBE_TIMEOUT, &[]) {
        Ok(output) if output.success => FirebaseCliProbe {
            present: true,
            program,
            version: Some(output.stdout_text().trim().to_string()).filter(|v| !v.is_empty()),
            reason: None,
        },
        Ok(output) => {
            let tail = crate::engine::git_cli::byte_tail(&output.stderr, MAX_FIREBASE_ERROR_BYTES);
            FirebaseCliProbe {
                present: false,
                program,
                version: None,
                reason: Some(if tail.trim().is_empty() {
                    format!("firebase --version exited {}", output.status_code)
                } else {
                    tail.trim().to_string()
                }),
            }
        }
        Err(error) => FirebaseCliProbe {
            present: false,
            program,
            version: None,
            reason: Some(if overridden {
                format!("{FIREBASE_BIN_ENV} is set but could not be run: {error}")
            } else {
                // Naming the nvm case here is the difference between a user
                // reinstalling a tool they already have and one setting an
                // override that takes ten seconds.
                format!(
                    "The Firebase CLI could not be run ({error}). If it is installed under nvm, \
                     fnm or volta, a windowed launch does not inherit that PATH — set \
                     {FIREBASE_BIN_ENV} to its absolute path."
                )
            }),
        },
    }
}

/// Reads one repository-root config file, or `None` when it does not exist.
///
/// Goes through `sandbox_join_canonical` rather than `repo.join(..)`: these
/// are repository-controlled paths, and `.firebaserc` symlinked at
/// `~/.config/configstore/firebase-tools.json` would otherwise read the user's
/// own refresh token into a payload bound for the UI.
fn read_repo_config(repo_path: &str, name: &str) -> Result<Option<String>, String> {
    let repo = validate_repo(repo_path)?;
    let path = sandbox_join_canonical(&repo, name)?;
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read {name}: {error}")),
    };
    if !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(format!(
            "{name} is {} bytes, past the {MAX_CONFIG_BYTES} byte limit",
            meta.len()
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("Cannot read {name}: {error}"))?;
    if bytes.contains(&0) {
        return Err(format!("{name} contains NUL bytes and is not text"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{name} is not valid UTF-8"))
}

/// Parses `.firebaserc`'s `projects` map into aliases, sorted by alias so the
/// UI order does not depend on JSON key order.
///
/// Returns every alias. Picking one is deliberately not done here:
/// `pick_github_remote` may default to `origin` because that convention is
/// near-universal, but `.firebaserc` has no equivalent and `default` is very
/// often production. Auto-selecting it would aim a "Roll back" button at prod
/// without the user ever naming the target.
pub fn parse_firebaserc(source: &str) -> Result<(Vec<FirebaseProjectAlias>, Option<String>), String> {
    let root: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| format!("could not parse .firebaserc: {error}"))?;
    let Some(projects) = root.get("projects").and_then(serde_json::Value::as_object) else {
        return Ok((Vec::new(), None));
    };
    let mut aliases: Vec<FirebaseProjectAlias> = projects
        .iter()
        .filter_map(|(alias, value)| {
            let project_id = value.as_str()?.trim();
            // A malformed entry is skipped rather than surfaced as an alias
            // with an empty target, which would render as a selectable option
            // that cannot work.
            if alias.trim().is_empty() || project_id.is_empty() {
                return None;
            }
            Some(FirebaseProjectAlias {
                alias: alias.trim().to_string(),
                project_id: project_id.to_string(),
            })
        })
        .collect();
    aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
    let default_alias = aliases
        .iter()
        .find(|a| a.alias == "default")
        .map(|a| a.alias.clone());
    Ok((aliases, default_alias))
}

/// True when `firebase.json` carries a top-level `apphosting` key.
pub fn parse_firebase_json_apphosting(source: &str) -> Result<bool, String> {
    let root: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| format!("could not parse firebase.json: {error}"))?;
    Ok(root.get("apphosting").is_some())
}

/// Reads both config files and the CLI probe into one status.
///
/// Never returns `Err` for a missing or malformed file: each becomes its own
/// `*_error` so one unreadable file cannot blank the other's answer.
pub fn discover_firebase_projects(repo_path: &str) -> FirebaseStatus {
    let cli = firebase_cli_probe();
    let mut status = FirebaseStatus::empty(cli);

    match read_repo_config(repo_path, ".firebaserc") {
        Ok(Some(source)) => {
            status.configured = true;
            match parse_firebaserc(&source) {
                Ok((projects, default_alias)) => {
                    status.projects = projects;
                    status.default_alias = default_alias;
                }
                Err(error) => status.firebaserc_error = Some(error),
            }
        }
        Ok(None) => {}
        Err(error) => status.firebaserc_error = Some(error),
    }

    match read_repo_config(repo_path, "firebase.json") {
        Ok(Some(source)) => {
            status.configured = true;
            match parse_firebase_json_apphosting(&source) {
                Ok(has) => status.has_apphosting_config = has,
                Err(error) => status.firebasejson_error = Some(error),
            }
        }
        Ok(None) => {}
        Err(error) => status.firebasejson_error = Some(error),
    }

    status
}

/// Upper bound for a project id reaching argv.
///
/// Google's own rule is 6–30 characters, lowercase letter start, letters,
/// digits and hyphens. The value comes from a repository file, so it is
/// untrusted content and is validated before it can become a `--project` flag.
const MAX_PROJECT_ID_LEN: usize = 30;
const MIN_PROJECT_ID_LEN: usize = 6;

pub fn validate_project_id(project_id: &str) -> Result<String, String> {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        return Err("Firebase project id must not be empty".into());
    }
    if trimmed.len() < MIN_PROJECT_ID_LEN || trimmed.len() > MAX_PROJECT_ID_LEN {
        return Err(format!(
            "Firebase project id must be {MIN_PROJECT_ID_LEN}-{MAX_PROJECT_ID_LEN} characters"
        ));
    }
    if !trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase())
    {
        return Err("Firebase project id must start with a lowercase letter".into());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "Firebase project id may contain only lowercase letters, digits and hyphens".into(),
        );
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firebaserc_aliases_are_sorted_and_default_is_named_not_chosen() {
        let (aliases, default_alias) = parse_firebaserc(
            r#"{"projects":{"staging":"acme-staging","default":"acme-prod"}}"#,
        )
        .expect("valid .firebaserc parses");
        assert_eq!(
            aliases,
            vec![
                FirebaseProjectAlias {
                    alias: "default".into(),
                    project_id: "acme-prod".into()
                },
                FirebaseProjectAlias {
                    alias: "staging".into(),
                    project_id: "acme-staging".into()
                },
            ]
        );
        // Named, so the UI can mark it — but every alias is still returned, so
        // the user picks the target rather than inheriting production.
        assert_eq!(default_alias.as_deref(), Some("default"));
    }

    #[test]
    fn firebaserc_without_projects_is_empty_not_an_error() {
        let (aliases, default_alias) = parse_firebaserc("{}").expect("empty object parses");
        assert!(aliases.is_empty());
        assert!(default_alias.is_none());
    }

    #[test]
    fn malformed_firebaserc_is_an_error_not_an_empty_alias_list() {
        assert!(parse_firebaserc("not json").is_err());
        // A non-string target is skipped rather than offered as a broken
        // option the user can select.
        let (aliases, _) =
            parse_firebaserc(r#"{"projects":{"bad":42,"good":"acme-prod"}}"#).expect("parses");
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].alias, "good");
    }

    #[test]
    fn apphosting_key_presence_is_what_firebase_json_is_read_for() {
        assert!(parse_firebase_json_apphosting(r#"{"apphosting":{"backendId":"x"}}"#).unwrap());
        assert!(!parse_firebase_json_apphosting(r#"{"hosting":{}}"#).unwrap());
        assert!(parse_firebase_json_apphosting("nope").is_err());
    }

    #[test]
    fn project_ids_that_could_be_reparsed_as_flags_are_refused() {
        assert!(validate_project_id("acme-prod").is_ok());
        assert!(validate_project_id("-project").is_err());
        assert!(validate_project_id("Acme-Prod").is_err());
        assert!(validate_project_id("short").is_err());
        assert!(validate_project_id("acme prod x").is_err());
        assert!(validate_project_id("acme\u{0}prod").is_err());
        assert!(validate_project_id(&"a".repeat(MAX_PROJECT_ID_LEN + 1)).is_err());
    }
}
