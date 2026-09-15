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

/// Whether one subcommand exists in the installed CLI, and why not when it does
/// not.
///
/// `checked` is separate from `available` for the reason it always is here: an
/// uninstalled CLI cannot be asked what it supports, and "we could not look"
/// must not render as "your CLI does not have it". The first sends a reader to
/// install Firebase; the second sends them to enable an experiment they do not
/// need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseCapability {
    pub available: bool,
    pub checked: bool,
    pub reason: Option<String>,
}

impl FirebaseCapability {
    fn unchecked(reason: impl Into<String>) -> Self {
        FirebaseCapability {
            available: false,
            checked: false,
            reason: Some(reason.into()),
        }
    }
}

/// What `firebase experiments:enable` call unlocks rollout listing, and the
/// caveat that comes with it.
///
/// Stated in full rather than reduced to the command, because the command on
/// its own reads like a supported feature flag. It is not: upstream describes
/// this experiment as exposing commands "intended for internal testing
/// purposes … not meant for public consumption", which may "break or disappear
/// without a notice". A reader deciding whether to turn it on needs that
/// sentence, not just the incantation.
const ROLLOUT_LISTING_REMEDY: &str = "Rollout listing is not in this build of the Firebase CLI. \
     Upstream ships `apphosting:rollouts:list` only behind the `internaltesting` experiment, \
     which is off by default. `firebase experiments:enable internaltesting` turns it on, but \
     Firebase documents those commands as internal and says they may change or disappear without \
     notice. Backends below are listed with a supported command and are unaffected.";

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
    /// True when more aliases exist than the picker cap allowed through.
    ///
    /// `.firebaserc` is repository content, so the row count is not ours to
    /// assume. A capped list that renders like a complete one is exactly the
    /// substitution every other field here exists to prevent.
    pub projects_truncated: bool,
    /// True when `firebase.json` carries an `apphosting` key.
    pub has_apphosting_config: bool,
    pub cli: FirebaseCliProbe,
    /// Whether the installed CLI exposes `apphosting:rollouts:list`.
    ///
    /// Asked, never assumed. That subcommand is gated behind an experiment that
    /// is off by default, so on a stock install it does not exist — and an
    /// unregistered subcommand exits non-zero having printed nothing at all,
    /// which reaches a reader as a JSON parse error rather than as the missing
    /// feature it is.
    pub rollout_listing: FirebaseCapability,
    /// Per-file, so an unreadable `firebase.json` never renders as "App
    /// Hosting is not configured".
    pub firebaserc_error: Option<String>,
    pub firebasejson_error: Option<String>,
}

impl FirebaseStatus {
    fn empty(cli: FirebaseCliProbe, rollout_listing: FirebaseCapability) -> Self {
        FirebaseStatus {
            configured: false,
            projects: Vec::new(),
            default_alias: None,
            projects_truncated: false,
            has_apphosting_config: false,
            cli,
            rollout_listing,
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
        let mut status = FirebaseStatus::empty(
            FirebaseCliProbe {
                present: false,
                program: firebase_program(),
                version: None,
                reason: Some(reason.clone()),
            },
            FirebaseCapability::unchecked("The Firebase status could not be read at all."),
        );
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

/// Asks the installed CLI whether it exposes `apphosting:rollouts:list`.
///
/// ## Why this exists
///
/// `apphosting:rollouts:list` is registered upstream only when the
/// `internaltesting` experiment is enabled, and that experiment is off by
/// default. On a stock install the subcommand therefore does not exist — and an
/// unregistered subcommand is the worst-behaved failure in this CLI: it exits
/// non-zero having written nothing to either stream, so a caller that expects a
/// `--json` envelope reports a parse error and sends the reader hunting a bug
/// in their own output handling.
///
/// ## Why the help text, and why this particular help text
///
/// Listing a command *group* (`firebase apphosting:rollouts --help`) prints the
/// subcommands registered under it and exits, with no auth, no network and no
/// project — so unlike every other question about App Hosting, this one is free
/// and cannot mutate anything. It is matched on content, not exit status:
/// commander exits 0 for an unknown group too, and answers it by printing the
/// root help, which does not contain the string.
///
/// A false negative here is the safe direction — the panel degrades to the
/// supported backends listing and says why — which is the right way round for a
/// probe whose input is human-readable text.
/// Whether a `--help` listing offers `apphosting:rollouts:list` as a command.
///
/// Split out from the spawn so the decision is testable against the CLI's real
/// output rather than only reachable through a process. Matched under the
/// `Commands:` heading, not anywhere in the text: commander's group help ends
/// with the hint line `firebase apphosting:rollouts:<command> --help`, and a
/// future one that spelled an example out would otherwise read as the command
/// being present.
fn help_lists_rollout_listing(help: &str) -> bool {
    help.lines()
        .skip_while(|line| !line.trim_start().starts_with("Commands:"))
        .any(|line| {
            line.trim_start()
                .starts_with(apphosting::ROLLOUTS_LIST_SUBCOMMAND)
        })
}

fn probe_rollout_listing(cli_present: bool) -> FirebaseCapability {
    if !cli_present {
        return FirebaseCapability::unchecked(
            "The Firebase CLI could not be run, so what it supports is unknown.",
        );
    }
    let program = firebase_program();
    match capture_command(
        &program,
        &["apphosting:rollouts", "--help"],
        None,
        PROBE_TIMEOUT,
        &[],
    ) {
        Ok(output) if output.success => {
            let available = help_lists_rollout_listing(&output.stdout_text());
            FirebaseCapability {
                available,
                checked: true,
                reason: if available {
                    None
                } else {
                    Some(ROLLOUT_LISTING_REMEDY.to_string())
                },
            }
        }
        Ok(output) => FirebaseCapability::unchecked(format!(
            "Could not ask the Firebase CLI which App Hosting commands it has: \
             `firebase apphosting:rollouts --help` exited {}.",
            output.status_code
        )),
        Err(error) => FirebaseCapability::unchecked(format!(
            "Could not ask the Firebase CLI which App Hosting commands it has: {error}"
        )),
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

/// Upper bound on aliases offered from one `.firebaserc`.
///
/// `.firebaserc` is repository content, so its size is chosen by whoever wrote
/// the repository, and [`MAX_CONFIG_BYTES`] bounds the *file* rather than the
/// rows: at roughly twenty bytes an entry, a file well inside that limit names
/// tens of thousands of aliases, every one of which would become an option in a
/// picker. Two hundred is far past any real project and still a bound.
const MAX_PROJECT_ALIASES: usize = 200;

/// What `.firebaserc` names, and whether that is all of it.
pub struct FirebasercProjects {
    pub aliases: Vec<FirebaseProjectAlias>,
    /// The `default` alias when the file names one — marked, never chosen.
    pub default_alias: Option<String>,
    /// True when more aliases exist than [`MAX_PROJECT_ALIASES`] allowed
    /// through. Carried rather than swallowed: a capped list that renders like
    /// a complete one is the failure this whole module is built around.
    pub truncated: bool,
}

/// Parses `.firebaserc`'s `projects` map into aliases, sorted by alias so the
/// UI order does not depend on JSON key order.
///
/// Returns every alias up to the cap. Picking one is deliberately not done
/// here: `pick_github_remote` may default to `origin` because that convention
/// is near-universal, but `.firebaserc` has no equivalent and `default` is very
/// often production. Auto-selecting it would aim a deploy at prod without the
/// user ever naming the target.
pub fn parse_firebaserc(source: &str) -> Result<FirebasercProjects, String> {
    let root: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| format!("could not parse .firebaserc: {error}"))?;
    let Some(projects) = root.get("projects").and_then(serde_json::Value::as_object) else {
        return Ok(FirebasercProjects {
            aliases: Vec::new(),
            default_alias: None,
            truncated: false,
        });
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
    // Sorted before the cap, so which aliases survive it is stable rather than
    // whatever order the JSON happened to use.
    aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
    let truncated = aliases.len() > MAX_PROJECT_ALIASES;
    aliases.truncate(MAX_PROJECT_ALIASES);
    let default_alias = aliases
        .iter()
        .find(|a| a.alias == "default")
        .map(|a| a.alias.clone());
    Ok(FirebasercProjects {
        aliases,
        default_alias,
        truncated,
    })
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
    let rollout_listing = probe_rollout_listing(cli.present);
    let mut status = FirebaseStatus::empty(cli, rollout_listing);

    match read_repo_config(repo_path, ".firebaserc") {
        Ok(Some(source)) => {
            status.configured = true;
            match parse_firebaserc(&source) {
                Ok(parsed) => {
                    status.projects = parsed.aliases;
                    status.default_alias = parsed.default_alias;
                    status.projects_truncated = parsed.truncated;
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

    /// Exactly what firebase-tools 15.25.1 printed for
    /// `firebase apphosting:rollouts --help` on a stock install — captured,
    /// not imagined. The subcommand this panel wants is absent, and the
    /// trailing hint line is the near-miss the matcher has to survive.
    const GROUP_HELP_WITHOUT_LISTING: &str = "\
Usage: firebase apphosting:rollouts [options] [command]

manage apphosting:rollouts resources

Options:
  -h, --help                                        display help for command

Commands:
  apphosting:rollouts:create [options] <backendId>  create a rollout using a build for an App Hosting backend

To see more about a specific command, run:
  firebase apphosting:rollouts:<command> --help
";

    /// The same listing as it reads once `internaltesting` is enabled.
    const GROUP_HELP_WITH_LISTING: &str = "\
Usage: firebase apphosting:rollouts [options] [command]

Commands:
  apphosting:rollouts:create [options] <backendId>  create a rollout using a build for an App Hosting backend
  apphosting:rollouts:list <backendId>              list rollouts of an App Hosting backend

To see more about a specific command, run:
  firebase apphosting:rollouts:<command> --help
";

    /// What commander prints for a group it does not know: the root help — and
    /// it still exits 0, which is why the status proves nothing here and only
    /// the content can answer the question.
    const ROOT_HELP: &str = "\
Usage: firebase [options] [command]

Options:
  -V, --version                        output the version number
  --token <token>                      DEPRECATED - supply an auth token

Commands:
  apphosting                           manage App Hosting resources
  deploy [options]                     deploy code and assets to your Firebase project
";

    #[test]
    fn a_stock_cli_is_correctly_read_as_not_offering_rollout_listing() {
        assert!(
            !help_lists_rollout_listing(GROUP_HELP_WITHOUT_LISTING),
            "the hint line `apphosting:rollouts:<command>` must not read as the command"
        );
        assert!(help_lists_rollout_listing(GROUP_HELP_WITH_LISTING));
        assert!(
            !help_lists_rollout_listing(ROOT_HELP),
            "an unknown group prints the root help and still exits 0"
        );
        assert!(!help_lists_rollout_listing(""));
    }

    #[test]
    fn a_mention_outside_the_command_list_is_not_a_capability() {
        // Prose naming the command — a deprecation notice, an error hint — must
        // not be read as the command being registered.
        let prose = "Commands:\n  apphosting:rollouts:create  create a rollout\n\n\
                     Note: apphosting:rollouts:list was removed.\n";
        assert!(!help_lists_rollout_listing(prose));
    }

    /// Writes a fake `firebase` whose `apphosting:rollouts --help` prints
    /// `help`, and answers anything else the way commander does for a command
    /// it does not know: exit non-zero, print nothing at all.
    ///
    /// Unix-only for the reason [`crate::devmap::cli`]'s recording stub is: a
    /// `#!/bin/sh` script is not a Win32 application. The decision this
    /// exercises is [`help_lists_rollout_listing`], which is pure and is
    /// covered on every platform above; what only a real process can prove is
    /// the plumbing between them — and plumbing that was never run is exactly
    /// the defect this probe exists to catch.
    #[cfg(unix)]
    fn write_fake_firebase(dir: &std::path::Path, help: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("firebase");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"apphosting:rollouts\" ]; then\n  cat <<'HELP_EOF'\n{help}\nHELP_EOF\n  exit 0\nfi\nexit 1\n"
        );
        std::fs::write(&path, script).expect("write fake firebase");
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_reads_a_real_process_and_not_just_a_string() {
        let _serial = crate::harness::sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var(FIREBASE_BIN_ENV).ok();

        for (help, expected_available) in [
            (GROUP_HELP_WITHOUT_LISTING, false),
            (GROUP_HELP_WITH_LISTING, true),
        ] {
            let bin = write_fake_firebase(dir.path(), help);
            // SAFETY: the serial guard above is the repository's contract for
            // installing a process-global override inside a test.
            unsafe { std::env::set_var(FIREBASE_BIN_ENV, &bin) };
            let capability = probe_rollout_listing(true);
            assert!(
                capability.checked,
                "a probe that ran must say so, whatever it found"
            );
            assert_eq!(
                capability.available, expected_available,
                "probe disagreed with the help text it was given"
            );
            assert_eq!(capability.reason.is_none(), expected_available);
        }

        // SAFETY: same serial guard; restores what the test replaced.
        unsafe {
            match previous {
                Some(value) => std::env::set_var(FIREBASE_BIN_ENV, value),
                None => std::env::remove_var(FIREBASE_BIN_ENV),
            }
        }
    }

    #[test]
    fn an_unrunnable_cli_leaves_the_capability_unchecked_not_absent() {
        // The honesty invariant one level out: "we could not ask" and "your CLI
        // does not have it" send a reader to two different remedies, so they
        // must not share a rendering.
        let capability = probe_rollout_listing(false);
        assert!(!capability.checked);
        assert!(!capability.available);
        assert!(capability.reason.is_some());
    }

    #[test]
    fn an_unreadable_status_reports_its_capabilities_as_unchecked() {
        let status = FirebaseStatus::unreadable("thread pool exhausted".into());
        assert!(!status.rollout_listing.checked);
        assert!(
            !status.configured,
            "a status that could not be read is not a repository without Firebase"
        );
        assert!(status.firebaserc_error.is_some());
    }

    #[test]
    fn a_crafted_firebaserc_cannot_flood_the_alias_picker() {
        // `.firebaserc` is repository content, so its size is the attacker's to
        // choose. The file cap alone is not a bound on *rows*: at roughly
        // twenty bytes an entry, a file inside the 1 MiB limit carries tens of
        // thousands of aliases, and every one of them becomes an option in a
        // picker. Bound the fan-out, and — because this codebase does not let a
        // capped sample wear a complete one's clothes — say that it was capped.
        let entries: Vec<String> = (0..MAX_PROJECT_ALIASES * 3)
            .map(|i| format!(r#""a{i:05}":"proj-{i:05}""#))
            .collect();
        let source = format!(r#"{{"projects":{{{}}}}}"#, entries.join(","));
        let parsed = parse_firebaserc(&source).expect("a large but valid file still parses");
        assert_eq!(parsed.aliases.len(), MAX_PROJECT_ALIASES);
        assert!(
            parsed.truncated,
            "a capped list must report that it was capped"
        );

        let small = parse_firebaserc(r#"{"projects":{"default":"acme-prod"}}"#).expect("parses");
        assert!(
            !small.truncated,
            "a list that fits must not claim to have been capped"
        );
    }

    #[test]
    fn firebaserc_aliases_are_sorted_and_default_is_named_not_chosen() {
        let FirebasercProjects {
            aliases,
            default_alias,
            ..
        } = parse_firebaserc(r#"{"projects":{"staging":"acme-staging","default":"acme-prod"}}"#)
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
        let FirebasercProjects {
            aliases,
            default_alias,
            ..
        } = parse_firebaserc("{}").expect("empty object parses");
        assert!(aliases.is_empty());
        assert!(default_alias.is_none());
    }

    #[test]
    fn malformed_firebaserc_is_an_error_not_an_empty_alias_list() {
        assert!(parse_firebaserc("not json").is_err());
        // A non-string target is skipped rather than offered as a broken
        // option the user can select.
        let aliases = parse_firebaserc(r#"{"projects":{"bad":42,"good":"acme-prod"}}"#)
            .expect("parses")
            .aliases;
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
