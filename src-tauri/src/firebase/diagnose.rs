//! Why a Firebase call failed, said in terms a reader can act on, and a record
//! of what was actually run.
//!
//! ## The failure this module was written for
//!
//! firebase-tools 15.34 opens `firebase-debug.log` in its *working directory*
//! before it parses a single argument, and throws `Unable to obtain permissions
//! for firebase-debug.log` when it cannot. A Dock-launched GitPulse has `/` as
//! its working directory, and the CLI probe used to inherit it — so every
//! repository's panel said, in effect, "permission denied", and the reader went
//! looking for a missing IAM role. Nothing about the account, the project or
//! Google Cloud was involved.
//!
//! Two things follow, and both live here so no call site can forget them:
//!
//! * every `firebase` child is built by [`FirebaseSpawn::new`], which points
//!   `FIREBASE_DEBUG_PATH` at a directory GitPulse owns — so the CLI never
//!   needs to write where it was started, and never litters the user's
//!   repository with a debug log either;
//! * [`classify_failure`] recognises that startup signature *before* it looks
//!   for the word "permission", so a local file-system fault can never be
//!   reported as a Google Cloud one.
//!
//! ## Nothing here is a credential
//!
//! Every byte that leaves this module for the UI — argv, output tails, error
//! text — passes through [`crate::ledger::redact::text`], the app's canonical
//! redactor. The signed-in account is read from `firebase login:list` *without*
//! `--json`: that command's JSON result is the CLI's full account list, refresh
//! tokens included, so only the human line naming the email is ever parsed.

use super::MAX_FIREBASE_ERROR_BYTES;
use crate::engine::process::{byte_tail, capture_command, CapturedOutput};
use crate::ipc_error::CommandError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The firebase-tools variable that moves `firebase-debug.log` out of the
/// working directory (`lib/logger.js` `resolveLogTarget`).
pub(crate) const FIREBASE_DEBUG_PATH_ENV: &str = "FIREBASE_DEBUG_PATH";

/// Bound on each output tail kept in an attempt record.
const MAX_TAIL_BYTES: usize = MAX_FIREBASE_ERROR_BYTES;

/// Bound on an argv element in a record. Project and backend ids are short;
/// anything longer is not something a reader needs in full.
const MAX_ARG_CHARS: usize = 256;

/// Bound on argv elements in a record.
const MAX_ARGS: usize = 32;

/// Where the CLI's debug logs go: a directory GitPulse owns, never the
/// repository and never the app's own working directory.
pub(crate) fn firebase_log_dir() -> PathBuf {
    std::env::temp_dir().join("gitpulse-firebase")
}

/// How long a run directory may outlive its run before a later spawn removes
/// it. A run removes its own directory when it ends; this only catches the runs
/// that never ended (GitPulse killed mid-call).
const STALE_RUN_DIR: Duration = Duration::from_secs(24 * 60 * 60);

/// Bound on how many entries one sweep looks at.
const MAX_SWEEP_ENTRIES: usize = 256;

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

/// A directory of its own for one run's debug log.
///
/// Per run, not shared, for two reasons. firebase-tools deletes its log only
/// when it exits below 2, and *appends* to an existing one — so a shared file
/// grows with every exit-2 failure ("Failed to get Firebase project…") for as
/// long as the machine lives. And that log records request detail nobody here
/// reads; the shortest life it can have is the run that wrote it.
fn run_log_dir() -> PathBuf {
    let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    firebase_log_dir().join(format!("run-{}-{}-{run}", std::process::id(), now_ms()))
}

/// Removes run directories older than [`STALE_RUN_DIR`]. Bounded, and silent
/// on failure: a directory that cannot be removed now is tried again later,
/// and nothing a user asked for depends on it.
fn sweep_stale_runs() {
    let Ok(entries) = std::fs::read_dir(firebase_log_dir()) else {
        return;
    };
    for entry in entries.flatten().take(MAX_SWEEP_ENTRIES) {
        if !entry.file_name().to_string_lossy().starts_with("run-") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_RUN_DIR);
        if stale {
            remove_run_dir(&entry.path());
        }
    }
}

fn remove_run_dir(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "could not remove the Firebase CLI debug-log directory {}: {error}",
            dir.display()
        ),
    }
}

/// One `firebase` invocation, fully described before it runs.
///
/// The only way this module builds a child. Constructing one always carries
/// `FIREBASE_DEBUG_PATH`, which is what makes "a call site forgot the
/// environment" unrepresentable rather than merely unlikely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirebaseSpawn {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

impl FirebaseSpawn {
    pub(crate) fn new(program: String, args: Vec<String>, cwd: PathBuf, timeout: Duration) -> Self {
        FirebaseSpawn {
            program,
            args,
            cwd,
            timeout,
            env: vec![(
                FIREBASE_DEBUG_PATH_ENV.to_string(),
                run_log_dir().to_string_lossy().into_owned(),
            )],
        }
    }

    /// The run directory this spawn's debug log goes to.
    pub(crate) fn debug_log_dir(&self) -> Option<PathBuf> {
        self.env
            .iter()
            .find(|(key, _)| key == FIREBASE_DEBUG_PATH_ENV)
            .map(|(_, value)| PathBuf::from(value))
    }
}

/// Runs a [`FirebaseSpawn`] through the app's bounded spawn funnel.
pub(crate) fn spawn_firebase(spawn: &FirebaseSpawn) -> Result<CapturedOutput, CommandError> {
    sweep_stale_runs();
    let log_dir = spawn.debug_log_dir();
    if let Some(dir) = &log_dir {
        // Not an early return on failure: firebase-tools creates this directory
        // itself too, and when neither of us can, the CLI's own "Unable to
        // obtain permissions" reaches the record and is classified as the local
        // fault it is. Refusing here would replace that precise message with a
        // vaguer one of ours.
        if let Err(error) = std::fs::create_dir_all(dir) {
            log::warn!(
                "could not create the Firebase CLI debug-log directory {}: {error}",
                dir.display()
            );
        }
    }
    let args: Vec<&str> = spawn.args.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = spawn
        .env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let result = capture_command(&spawn.program, &args, Some(&spawn.cwd), spawn.timeout, &env);
    if let Some(dir) = &log_dir {
        remove_run_dir(dir);
    }
    result
}

/// What one `firebase` invocation did, for the panel's debug view and its
/// "Copy diagnostics" action.
///
/// `exit_code` is `None` exactly when the process produced no exit status — it
/// could not be started, or it was killed at the deadline — and `spawn_error`
/// then says which. A record never claims an exit it did not observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseAttempt {
    /// The command line, redacted, program first.
    pub argv: Vec<String>,
    pub cwd: String,
    /// Milliseconds since the Unix epoch when the child was started.
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    /// The last few KiB of each stream, redacted.
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub spawn_error: Option<String>,
}

fn clip_arg(arg: &str) -> String {
    let clean = crate::ledger::redact::text(arg);
    if clean.chars().count() <= MAX_ARG_CHARS {
        return clean;
    }
    let mut cut: String = clean.chars().take(MAX_ARG_CHARS).collect();
    cut.push('…');
    cut
}

fn redacted_tail(bytes: &[u8]) -> String {
    crate::ledger::redact::text(byte_tail(bytes, MAX_TAIL_BYTES).trim())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Runs `spawn` with `run` and records what happened.
///
/// `run` is injectable so the record — and the spawn description it was given
/// — can be asserted without a child process; production passes
/// [`spawn_firebase`].
pub(crate) fn run_recorded(
    spawn: &FirebaseSpawn,
    run: impl FnOnce(&FirebaseSpawn) -> Result<CapturedOutput, CommandError>,
) -> (Result<CapturedOutput, CommandError>, FirebaseAttempt) {
    let started_at_ms = now_ms();
    let clock = Instant::now();
    let result = run(spawn);
    let duration_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut argv: Vec<String> = std::iter::once(spawn.program.as_str())
        .chain(spawn.args.iter().map(String::as_str))
        .take(MAX_ARGS)
        .map(clip_arg)
        .collect();
    if spawn.args.len() + 1 > MAX_ARGS {
        argv.push(format!("… {} more", spawn.args.len() + 1 - MAX_ARGS));
    }
    let mut attempt = FirebaseAttempt {
        argv,
        cwd: spawn.cwd.to_string_lossy().into_owned(),
        started_at_ms,
        duration_ms,
        exit_code: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        spawn_error: None,
    };
    match &result {
        Ok(output) => {
            attempt.exit_code = Some(output.status_code);
            attempt.stdout_tail = redacted_tail(&output.stdout);
            attempt.stderr_tail = redacted_tail(&output.stderr);
        }
        Err(error) => {
            attempt.spawn_error = Some(crate::ledger::redact::text(error.message()));
        }
    }
    (result, attempt)
}

/// The distinct ways a Firebase call fails, each with its own next step.
///
/// `CliStartup` exists because the beta's "unable to obtain permissions" was
/// none of the others: the CLI is installed and was found, and then crashed
/// before it reached Google. Folding that into `MissingPermission` sends the
/// reader to an IAM console; folding it into `CliMissing` sends them to
/// reinstall a tool they have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirebaseFailureKind {
    CliMissing,
    CliStartup,
    /// A Node file-system error (`EACCES`, `EPERM`, `EROFS`) — "permission
    /// denied" on this machine, never on Google Cloud.
    LocalFiles,
    NotSignedIn,
    WrongAccount,
    MissingPermission,
    ApiDisabled,
    Network,
    Unknown,
}

/// A classified failure: what kind, one line saying so, and what to do next.
///
/// `permission`, `role` and `api` are filled only when the CLI's own message
/// names them — never guessed from the command that was run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseFailure {
    pub kind: FirebaseFailureKind,
    pub title: String,
    pub next_step: String,
    pub permission: Option<String>,
    pub role: Option<String>,
    pub api: Option<String>,
}

fn ansi_free(text: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    ANSI.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("static ANSI regex must compile")
    })
    .replace_all(text, "")
    .into_owned()
}

fn any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// A permission name the message states, e.g.
/// `Permission 'firebaseapphosting.backends.list' denied` or
/// `the iam.serviceAccounts.actAs permission`.
fn named_permission(text: &str) -> Option<String> {
    static BEFORE: OnceLock<Regex> = OnceLock::new();
    static AFTER: OnceLock<Regex> = OnceLock::new();
    let before = BEFORE.get_or_init(|| {
        Regex::new(r#"(?i)permissions?\s*[:'"`\[(]?\s*([a-z][a-z0-9]*(?:\.[A-Za-z0-9]+){2,})"#)
            .expect("static permission regex must compile")
    });
    let after = AFTER.get_or_init(|| {
        Regex::new(r#"([a-z][a-z0-9]*(?:\.[A-Za-z0-9]+){2,})['"`\])]?\s+permission"#)
            .expect("static permission regex must compile")
    });
    before
        .captures_iter(text)
        .chain(after.captures_iter(text))
        .filter_map(|captures| captures.get(1).map(|m| m.as_str().to_string()))
        // A service host (`firebaseapphosting.googleapis.com`) has the same
        // dotted shape as a permission and is not one.
        .find(|candidate| !candidate.ends_with(".com") && !candidate.contains("googleapis"))
}

/// Node's file-system error code in `text`, when there is one.
fn local_file_error(text: &str) -> Option<&'static str> {
    static CODE: OnceLock<Regex> = OnceLock::new();
    let found = CODE
        .get_or_init(|| {
            Regex::new(r"\b(EACCES|EPERM|EROFS)\b").expect("static errno regex must compile")
        })
        .find(text)?;
    ["EACCES", "EPERM", "EROFS"]
        .into_iter()
        .find(|code| *code == found.as_str())
}

/// The first single-quoted absolute path, as Node quotes the file it failed on.
fn quoted_path(text: &str) -> Option<String> {
    static PATH: OnceLock<Regex> = OnceLock::new();
    PATH.get_or_init(|| Regex::new(r"'(/[^'\n]{0,512})'").expect("static path regex must compile"))
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|m| m.as_str().to_string())
}

fn named_role(text: &str) -> Option<String> {
    static ROLE: OnceLock<Regex> = OnceLock::new();
    ROLE.get_or_init(|| {
        Regex::new(r"roles/[A-Za-z0-9_.]+").expect("static role regex must compile")
    })
    .find(text)
    .map(|m| m.as_str().trim_end_matches('.').to_string())
}

fn named_api(text: &str) -> Option<String> {
    static API: OnceLock<Regex> = OnceLock::new();
    API.get_or_init(|| {
        Regex::new(r"[a-z0-9][a-z0-9-]*\.googleapis\.com").expect("static API regex must compile")
    })
    .find(text)
    .map(|m| m.as_str().to_string())
}

fn on_project(project_id: Option<&str>) -> String {
    project_id
        .filter(|id| !id.is_empty())
        .map(|id| format!(" on {id}"))
        .unwrap_or_default()
}

/// Classifies one failure message from the Firebase CLI (or from the spawn
/// around it).
///
/// The arms are ordered, and the order is the point:
///
/// 1. **startup** first — `Unable to obtain permissions for firebase-debug.log`
///    contains the word "permission" and is not about Google Cloud at all;
/// 2. **sign-in** before anything that mentions access;
/// 3. **API disabled** before permission — "Permissions denied enabling
///    firebaseapphosting.googleapis.com" is an API that is off, which this
///    account also cannot turn on;
/// 4. **wrong account** before permission — "Failed to get Firebase project …
///    your account has permission to access it" is the shape of signing in as
///    someone who is not a member;
/// 5. **missing permission**, then **network**, then **unknown**.
///
/// The strings come from firebase-tools 15.34.2's own sources
/// (`logger.js`, `requireAuth.js`, `auth.js`, `ensureApiEnabled.js`,
/// `management/projects.js`, `responseToError.js`, `apiv2.js`) and Google's
/// API error bodies.
pub fn classify_failure(message: &str, project_id: Option<&str>) -> FirebaseFailure {
    let text = ansi_free(message);
    let lower = text.to_lowercase();
    let on = on_project(project_id);
    let failure = |kind, title: String, next_step: String| FirebaseFailure {
        kind,
        title,
        next_step,
        permission: None,
        role: None,
        api: None,
    };

    if any(
        &lower,
        &["firebase-debug.log", "unable to obtain permissions for"],
    ) {
        return failure(
            FirebaseFailureKind::CliStartup,
            "The Firebase CLI could not start".into(),
            format!(
                "It could not create its debug log (firebase-debug.log) in the folder it was \
                 started from. This is a local file-system problem, not a Google Cloud \
                 permission. GitPulse points the log at {} — check that folder is writable.",
                firebase_log_dir().display()
            ),
        );
    }
    // The class the debug-log case belongs to: Node's own file-system errors.
    // They read "permission denied" too, and a reader sent to an IAM console
    // for `EACCES … configstore/firebase-tools.json` is sent to the wrong
    // continent. Matched on Node's error codes, which no Google response uses.
    if let Some(code) = local_file_error(&text) {
        let path = quoted_path(&text);
        return failure(
            FirebaseFailureKind::LocalFiles,
            match &path {
                Some(path) => {
                    format!("The Firebase CLI couldn't access a local file ({code}): {path}")
                }
                None => format!("The Firebase CLI couldn't access a local file ({code})"),
            },
            "This is a file-system permission on this Mac, not a Google Cloud permission. Check \
             that your user owns the file or folder named (for the CLI's own settings, \
             ~/.config/configstore), then try again."
                .into(),
        );
    }
    if any(
        &lower,
        &["env: node", "node: no such file", "node: command not found"],
    ) {
        return failure(
            FirebaseFailureKind::CliStartup,
            "The Firebase CLI could not find Node".into(),
            "The `firebase` script runs on Node, which a windowed launch could not find. If Node \
             is installed under nvm, fnm or volta, set GITPULSE_FIREBASE_BIN to the absolute path \
             of `firebase` and relaunch GitPulse."
                .into(),
        );
    }
    if any(
        &lower,
        &[
            "cannot find module",
            "is not installed",
            "not on path",
            "command not found",
        ],
    ) {
        return failure(
            FirebaseFailureKind::CliMissing,
            "The Firebase CLI is not installed where GitPulse can run it".into(),
            "Install it with `npm install -g firebase-tools`, or set GITPULSE_FIREBASE_BIN to its \
             absolute path, then relaunch GitPulse."
                .into(),
        );
    }

    if any(
        &lower,
        &[
            "credentials are no longer valid",
            "invalid_grant",
            "invalid_rapt",
            "login --reauth",
        ],
    ) {
        return failure(
            FirebaseFailureKind::NotSignedIn,
            "The Firebase CLI's sign-in has expired".into(),
            "Run `firebase login --reauth` in a terminal, then try again.".into(),
        );
    }
    if any(
        &lower,
        &[
            "have you run firebase login",
            "command requires authentication",
            "not yet authenticated",
            "no authorized accounts",
            "not logged in",
            "authentication error",
            "try running firebase login",
            "try running `firebase login`",
        ],
    ) {
        return failure(
            FirebaseFailureKind::NotSignedIn,
            "Not signed in to the Firebase CLI".into(),
            "Run `firebase login` in a terminal, then try again.".into(),
        );
    }

    if any(
        &lower,
        &[
            "has not been used in project",
            "it is disabled",
            "service_disabled",
            "permissions denied enabling",
            "permission denied to enable service",
        ],
    ) {
        let api = named_api(&text);
        let what = api
            .clone()
            .unwrap_or_else(|| "A required Google Cloud API".into());
        let url = match (&api, project_id.filter(|id| !id.is_empty())) {
            (Some(api), Some(project)) => {
                format!(" https://console.cloud.google.com/apis/library/{api}?project={project}")
            }
            _ => String::new(),
        };
        let cannot_enable = any(
            &lower,
            &[
                "permissions denied enabling",
                "permission denied to enable service",
            ],
        );
        let next_step = if cannot_enable {
            format!("Your account cannot turn it on. Ask a project owner to enable it:{url}")
        } else if url.is_empty() {
            "Enable it in the Google Cloud console for this project, then try again.".into()
        } else {
            format!("Enable it, then try again:{url}")
        };
        let mut classified = failure(
            FirebaseFailureKind::ApiDisabled,
            format!("{what} is not enabled{on}"),
            next_step,
        );
        classified.api = api;
        return classified;
    }

    if any(
        &lower,
        &[
            "failed to get firebase project",
            "make sure the project exists",
        ],
    ) {
        return failure(
            FirebaseFailureKind::WrongAccount,
            format!("The signed-in account cannot see this project{on}"),
            "The account the Firebase CLI uses may not be a member of the project, or the project \
             id is wrong. Check the account with `firebase login:list`; switch with \
             `firebase login:use <email>` or add one with `firebase login:add`."
                .into(),
        );
    }

    let permission = named_permission(&text);
    if permission.is_some()
        || any(
            &lower,
            &[
                "permission_denied",
                "does not have permission",
                "permission denied",
                "http error: 403",
                "insufficient permission",
                "missing permission",
                "iam permission",
                "permission '",
            ],
        )
    {
        let role = named_role(&text);
        let grant = match (&role, &permission) {
            (Some(role), _) => format!("the role {role}"),
            (None, Some(permission)) => format!("a role that includes {permission}"),
            (None, None) => "a role with access to Firebase App Hosting".to_string(),
        };
        let mut classified = failure(
            FirebaseFailureKind::MissingPermission,
            match &permission {
                Some(permission) => format!("Missing permission {permission}{on}"),
                None => format!("The signed-in account lacks a permission{on}"),
            },
            format!(
                "Ask a project owner to grant your account {grant}. If you expected access, check \
                 the CLI is signed in as the right account with `firebase login:list`."
            ),
        );
        classified.permission = permission;
        classified.role = role;
        return classified;
    }

    if any(
        &lower,
        &[
            "enotfound",
            "econnrefused",
            "econnreset",
            "etimedout",
            "eai_again",
            "getaddrinfo",
            "socket hang up",
            "failed to make request",
            "unable to connect",
            "network",
            "timed out after",
        ],
    ) {
        return failure(
            FirebaseFailureKind::Network,
            "Could not reach Google".into(),
            "Check the network connection, VPN or proxy, then try again.".into(),
        );
    }

    failure(
        FirebaseFailureKind::Unknown,
        "The Firebase CLI reported an error GitPulse does not recognise".into(),
        "Copy the diagnostics below — they include the CLI's own output.".into(),
    )
}

/// Who the Firebase CLI is signed in as, for this repository's folder.
///
/// Three states, for the honesty rule every report here follows: `checked`
/// with an email (signed in), `checked` with none (nobody is signed in), and
/// not `checked` (the CLI could not be asked — which says nothing about
/// sign-in).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseAccount {
    pub checked: bool,
    pub email: Option<String>,
    pub reason: Option<String>,
}

/// Reads `Logged in as <email>` from `firebase login:list`'s human output.
///
/// Never `--json`: that result is `auth.getAllAccounts()`, which carries each
/// account's tokens.
pub(crate) fn parse_login_list(stdout: &str) -> Option<String> {
    ansi_free(stdout).lines().find_map(|line| {
        line.trim()
            .strip_prefix("Logged in as ")
            .map(str::trim)
            .filter(|email| email.contains('@') && !email.contains(char::is_whitespace))
            .map(str::to_string)
    })
}

/// The argv for asking who is signed in. Never carries `--json`.
pub(crate) const LOGIN_LIST_ARGS: &[&str] = &["login:list"];

/// Bound on the account question: it reads a local file and should be quick.
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(20);

/// Asks the CLI who it is signed in as, from `repo` — the CLI keeps a
/// per-folder account (`firebase login:use`), so asking from anywhere else can
/// name the wrong one.
pub(crate) fn lookup_account_with(
    repo: &Path,
    run: impl FnOnce(&FirebaseSpawn) -> Result<CapturedOutput, CommandError>,
) -> (FirebaseAccount, FirebaseAttempt) {
    let spawn = FirebaseSpawn::new(
        super::firebase_program(),
        LOGIN_LIST_ARGS
            .iter()
            .map(|arg| (*arg).to_string())
            .collect(),
        repo.to_path_buf(),
        ACCOUNT_TIMEOUT,
    );
    let (result, mut attempt) = run_recorded(&spawn, run);
    // The output lists every account this CLI has ever been signed in as.
    // Diagnostics are made to be pasted into bug reports, so only the active
    // one — the answer to the question asked — is kept.
    attempt.stdout_tail = String::new();
    let account = match result {
        Ok(output) if output.success => {
            let email = parse_login_list(&output.stdout_text());
            attempt.stdout_tail = match &email {
                Some(email) => format!("Logged in as {email} (other accounts withheld)"),
                None => "(no account line; output withheld)".to_string(),
            };
            FirebaseAccount {
                checked: true,
                email,
                reason: None,
            }
        }
        Ok(output) => FirebaseAccount {
            checked: false,
            email: None,
            reason: Some(format!("firebase login:list exited {}", output.status_code)),
        },
        Err(error) => FirebaseAccount {
            checked: false,
            email: None,
            reason: Some(crate::ledger::redact::text(error.message())),
        },
    };
    (account, attempt)
}

/// Whether a failure is one where knowing the account changes the next step.
pub(crate) fn account_matters(kind: FirebaseFailureKind) -> bool {
    matches!(
        kind,
        FirebaseFailureKind::NotSignedIn
            | FirebaseFailureKind::WrongAccount
            | FirebaseFailureKind::MissingPermission
            | FirebaseFailureKind::ApiDisabled
            | FirebaseFailureKind::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What firebase-tools 15.34.2 wrote to stderr for `firebase --version`
    /// started from `/` — captured on 2026-11-14 from the reproduction of the
    /// beta report, not imagined.
    const STARTUP_FROM_ROOT: &str = "\
/opt/homebrew/lib/node_modules/firebase-tools/lib/logger.js:148
    throw new Error(`Unable to obtain permissions for ${baseName}${ext}`);
    ^

Error: Unable to obtain permissions for firebase-debug.log
    at findAvailableLogFile (/opt/homebrew/lib/node_modules/firebase-tools/lib/logger.js:148:11)";

    #[test]
    fn the_beta_report_is_a_cli_startup_fault_not_a_missing_permission() {
        let failure = classify_failure(STARTUP_FROM_ROOT, Some("scholarlm-vbc"));
        assert_eq!(failure.kind, FirebaseFailureKind::CliStartup, "{failure:?}");
        assert!(failure.next_step.contains("not a Google Cloud permission"));
        assert!(failure.permission.is_none() && failure.role.is_none());
    }

    #[test]
    fn each_firebase_tools_message_lands_in_its_own_state() {
        use FirebaseFailureKind::*;
        let cases: &[(&str, FirebaseFailureKind)] = &[
            // requireAuth.js
            ("Failed to authenticate, have you run firebase login?", NotSignedIn),
            ("Command requires authentication, please run firebase login", NotSignedIn),
            // auth.js invalidCredentialError
            (
                "Authentication Error: Your credentials are no longer valid. Please run firebase login --reauth",
                NotSignedIn,
            ),
            // management/projects.js getFirebaseProject
            (
                "Failed to get Firebase project acme-prod. Please make sure the project exists and your account has permission to access it.",
                WrongAccount,
            ),
            // ensureApiEnabled.js enable()
            (
                "Permissions denied enabling firebaseapphosting.googleapis.com.\n        Please ask a project owner to visit the following URL to enable this service:",
                ApiDisabled,
            ),
            // Google's SERVICE_DISABLED body through responseToError.js
            (
                "Request to https://firebaseapphosting.googleapis.com/v1beta/projects/acme-prod/locations/-/backends had HTTP Error: 403, Firebase App Hosting API has not been used in project 1234 before or it is disabled.",
                ApiDisabled,
            ),
            // Google's IAM_PERMISSION_DENIED body through responseToError.js
            (
                "Request to https://firebaseapphosting.googleapis.com/v1beta/projects/acme-prod/locations/-/backends had HTTP Error: 403, Permission 'firebaseapphosting.backends.list' denied on resource '//firebaseapphosting.googleapis.com/projects/acme-prod/locations/-' (or it may not exist).",
                MissingPermission,
            ),
            ("HTTP Error: 403, The caller does not have permission", MissingPermission),
            // apiv2.js
            ("Failed to make request: getaddrinfo ENOTFOUND firebase.googleapis.com", Network),
            ("firebase timed out after 60s", Network),
            ("Your project must be on the Blaze plan", Unknown),
            ("", Unknown),
        ];
        for (message, expected) in cases {
            let failure = classify_failure(message, Some("acme-prod"));
            assert_eq!(failure.kind, *expected, "{message:?} → {failure:?}");
            assert!(!failure.title.is_empty() && !failure.next_step.is_empty());
        }
    }

    /// The class, not the one string: any Node file-system refusal says
    /// "permission denied", and none of them is about Google Cloud.
    #[test]
    fn a_local_file_refusal_is_never_reported_as_a_cloud_permission() {
        let cases = [
            (
                "Error: EACCES: permission denied, open '/Users/me/.config/configstore/firebase-tools.json'",
                "EACCES",
                Some("/Users/me/.config/configstore/firebase-tools.json"),
            ),
            (
                "Error: EPERM: operation not permitted, mkdir '/Volumes/locked/cache'",
                "EPERM",
                Some("/Volumes/locked/cache"),
            ),
            ("Error: EROFS: read-only file system, open", "EROFS", None),
        ];
        for (message, code, path) in cases {
            let failure = classify_failure(message, Some("acme-prod"));
            assert_eq!(failure.kind, FirebaseFailureKind::LocalFiles, "{message}");
            assert!(failure.title.contains(code), "{failure:?}");
            if let Some(path) = path {
                assert!(failure.title.contains(path), "{failure:?}");
            }
            assert!(failure.next_step.contains("not a Google Cloud permission"));
            assert!(failure.permission.is_none());
        }
        // A Google refusal that happens to say "EPERM"-free "permission denied"
        // still lands where it belongs.
        assert_eq!(
            classify_failure("HTTP Error: 403, Permission denied on resource", None).kind,
            FirebaseFailureKind::MissingPermission
        );
    }

    /// The beta's failure against the real CLI, through the production spawn:
    /// started from `/` — where a Dock-launched GitPulse lives — `firebase
    /// --version` must answer. Ignored by default because CI has no Firebase
    /// CLI; run with `--ignored` on a machine that has one. Read-only: no
    /// account, no network, no project.
    #[test]
    #[ignore = "needs the firebase CLI on PATH"]
    fn the_installed_cli_answers_when_started_from_the_root_directory() {
        let spawn = FirebaseSpawn::new(
            super::super::firebase_program(),
            vec!["--version".into()],
            PathBuf::from("/"),
            Duration::from_secs(60),
        );
        let output = spawn_firebase(&spawn).expect("the firebase CLI must be runnable");
        assert!(
            output.success,
            "firebase --version from / failed (exit {}): {}",
            output.status_code,
            output.stderr_text()
        );
        assert!(!output.stdout_text().trim().is_empty());
        let probe = super::super::firebase_cli_probe();
        assert!(probe.present, "{:?}", probe.failure);
        let listing = super::super::probe_rollout_listing(true);
        assert!(listing.checked, "{:?}", listing.reason);
    }

    #[test]
    fn a_permission_the_message_names_is_carried_and_a_host_is_not_mistaken_for_one() {
        let failure = classify_failure(
            "HTTP Error: 403, Permission 'firebaseapphosting.backends.list' denied on resource '//firebaseapphosting.googleapis.com/projects/acme-prod'",
            Some("acme-prod"),
        );
        assert_eq!(
            failure.permission.as_deref(),
            Some("firebaseapphosting.backends.list")
        );
        assert!(failure
            .next_step
            .contains("firebaseapphosting.backends.list"));

        let delegated = classify_failure(
            "Failed to create backend due to missing delegation permissions for sa@x. Make sure you have the iam.serviceAccounts.actAs permission.",
            None,
        );
        assert_eq!(delegated.kind, FirebaseFailureKind::MissingPermission);
        assert_eq!(
            delegated.permission.as_deref(),
            Some("iam.serviceAccounts.actAs")
        );

        let role = classify_failure(
            "PERMISSION_DENIED: grant roles/firebaseapphosting.viewer to continue.",
            None,
        );
        assert_eq!(
            role.role.as_deref(),
            Some("roles/firebaseapphosting.viewer")
        );

        // No name in the message means no name in the answer.
        let bare = classify_failure("HTTP Error: 403, The caller does not have permission", None);
        assert!(bare.permission.is_none() && bare.role.is_none());
    }

    #[test]
    fn a_disabled_api_names_itself_and_links_only_when_both_parts_are_known() {
        let message = "Firebase App Hosting API has not been used in project 1234 before or it is \
                       disabled. Enable it by visiting https://console.developers.google.com/apis/api/firebaseapphosting.googleapis.com/overview?project=1234";
        let with_project = classify_failure(message, Some("acme-prod"));
        assert_eq!(
            with_project.api.as_deref(),
            Some("firebaseapphosting.googleapis.com")
        );
        assert!(with_project.next_step.contains(
            "https://console.cloud.google.com/apis/library/firebaseapphosting.googleapis.com?project=acme-prod"
        ));
        let without_project = classify_failure(message, None);
        assert!(!without_project
            .next_step
            .contains("console.cloud.google.com/apis/library"));
    }

    #[test]
    fn colour_codes_do_not_hide_a_known_message() {
        let coloured = "Failed to authenticate, have you run \u{1b}[1mfirebase login\u{1b}[22m?";
        assert_eq!(
            classify_failure(coloured, None).kind,
            FirebaseFailureKind::NotSignedIn
        );
    }

    #[test]
    fn every_spawn_moves_the_debug_log_out_of_its_working_directory() {
        let spawn = FirebaseSpawn::new(
            "firebase".into(),
            vec!["--version".into()],
            PathBuf::from("/"),
            Duration::from_secs(1),
        );
        let debug_path = spawn
            .debug_log_dir()
            .expect("every firebase child must carry FIREBASE_DEBUG_PATH");
        assert!(
            debug_path.starts_with(firebase_log_dir()) && debug_path != firebase_log_dir(),
            "each run gets a directory of its own under GitPulse's: {debug_path:?}"
        );
        assert!(
            debug_path.is_absolute() && debug_path != Path::new("/"),
            "the debug log must go to a directory GitPulse owns: {debug_path:?}"
        );
        let second = FirebaseSpawn::new(
            "firebase".into(),
            Vec::new(),
            PathBuf::from("/"),
            Duration::from_secs(1),
        );
        assert_ne!(
            second.debug_log_dir(),
            Some(debug_path),
            "two runs must never share (and append to) one debug log"
        );
    }

    /// A real child through the real funnel: the run directory exists while it
    /// runs (the CLI writes its log there) and is gone once it has finished.
    #[cfg(unix)]
    #[test]
    fn a_run_directory_lives_exactly_as_long_as_its_run() {
        let spawn = FirebaseSpawn::new(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                "test -d \"$FIREBASE_DEBUG_PATH\" && echo present > \"$FIREBASE_DEBUG_PATH/firebase-debug.log\" && echo ok".into(),
            ],
            std::env::temp_dir(),
            Duration::from_secs(30),
        );
        let dir = spawn.debug_log_dir().expect("run directory");
        let output = spawn_firebase(&spawn).expect("sh runs");
        assert!(
            output.success,
            "the directory must exist for the child: {:?}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_text().trim(), "ok");
        assert!(
            !dir.exists(),
            "the run directory must be removed after the run: {dir:?}"
        );
    }

    fn output(stdout: &str, stderr: &str, code: i32) -> CapturedOutput {
        CapturedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success: code == 0,
            status_code: code,
        }
    }

    /// A GitHub token shape, split so no source line is one (dc-verify's
    /// secret gate reads this file at commit).
    const PLANTED_TOKEN: &str = concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789");

    #[test]
    fn a_record_is_redacted_bounded_and_never_claims_an_exit_it_did_not_see() {
        let spawn = FirebaseSpawn::new(
            "firebase".into(),
            vec![
                "apphosting:backends:list".into(),
                "--token".into(),
                PLANTED_TOKEN.into(),
            ],
            PathBuf::from("/repo"),
            Duration::from_secs(1),
        );
        let long = "x".repeat(MAX_TAIL_BYTES * 3);
        let (_, attempt) = run_recorded(&spawn, |seen| {
            assert_eq!(seen, &spawn, "the runner must be handed the spawn as built");
            Ok(output(&long, &format!("{PLANTED_TOKEN} leaked"), 1))
        });
        assert_eq!(attempt.exit_code, Some(1));
        assert!(attempt.stdout_tail.len() <= MAX_TAIL_BYTES);
        let joined = format!("{} {}", attempt.argv.join(" "), attempt.stderr_tail);
        assert!(
            !joined.contains(PLANTED_TOKEN),
            "a token reached the record: {joined}"
        );
        assert_eq!(attempt.argv[0], "firebase");
        assert_eq!(attempt.cwd, "/repo");

        let (_, unstarted) = run_recorded(&spawn, |_| {
            Err(CommandError::from("firebase: no such file".to_string()))
        });
        assert_eq!(unstarted.exit_code, None, "no process, no exit code");
        assert!(unstarted
            .spawn_error
            .as_deref()
            .unwrap_or_default()
            .contains("no such file"));
    }

    #[test]
    fn the_account_is_read_from_the_human_line_and_asked_from_the_repository() {
        assert_eq!(
            parse_login_list(
                "Logged in as ada@example.com\n\nOther available accounts\n - b@example.com\n"
            ),
            Some("ada@example.com".to_string())
        );
        assert_eq!(
            parse_login_list("⚠  No authorized accounts, run \"firebase login\"\n"),
            None
        );
        assert!(
            !LOGIN_LIST_ARGS.contains(&"--json"),
            "login:list --json returns every account's tokens"
        );

        let repo = PathBuf::from("/repo");
        let (account, attempt) = lookup_account_with(&repo, |spawn| {
            assert_eq!(
                spawn.cwd, repo,
                "login:use is per folder, so ask from the repository"
            );
            assert!(!spawn.args.iter().any(|arg| arg == "--json"));
            Ok(output("Logged in as ada@example.com\n", "", 0))
        });
        assert!(account.checked);
        assert_eq!(account.email.as_deref(), Some("ada@example.com"));
        assert_eq!(attempt.exit_code, Some(0));

        // The record keeps the active account and nothing else from that
        // list: diagnostics are made to be pasted into bug reports.
        let (_, listed) = lookup_account_with(&repo, |_| {
            Ok(output(
                "Logged in as ada@example.com\n\nOther available accounts (switch with \"firebase login:use\")\n - private@example.com\n",
                "",
                0,
            ))
        });
        assert!(listed.stdout_tail.contains("ada@example.com"));
        assert!(
            !listed.stdout_tail.contains("private@example.com"),
            "another signed-in account leaked into the record: {}",
            listed.stdout_tail
        );

        let (nobody, _) =
            lookup_account_with(&repo, |_| Ok(output("No authorized accounts\n", "", 0)));
        assert!(
            nobody.checked && nobody.email.is_none(),
            "asked, and nobody is signed in"
        );

        let (unasked, _) = lookup_account_with(&repo, |_| {
            Err(CommandError::from("spawn failed".to_string()))
        });
        assert!(
            !unasked.checked,
            "a question that could not be asked is not 'nobody'"
        );
        assert!(unasked.reason.is_some());
    }

    /// The credentials the Google tooling behind `firebase` hands out, bare:
    /// no JSON key and no `Bearer` beside them, as a debug line or an error
    /// quoting a request prints them. Prefixes and lengths follow Google's
    /// documented shapes; the bodies are fake. Each literal is split so no
    /// scanner reads a whole token in this file.
    #[test]
    fn a_bare_google_oauth_credential_never_reaches_the_record() {
        let access = concat!("ya", "29.FAKE-access-token-body-for-the-firebase-test_0123");
        let refresh = concat!("1/", "/FAKE-refresh-token-body-for-the-firebase-test_42");
        let client_secret = concat!("GOC", "SPX-FAKE-client-secret-body_0123456");
        let spawn = FirebaseSpawn::new(
            "firebase".into(),
            vec![
                "apphosting:backends:list".into(),
                "--token".into(),
                refresh.into(),
            ],
            PathBuf::from("/repo"),
            Duration::from_secs(1),
        );
        let (_, attempt) = run_recorded(&spawn, |_| {
            Ok(output(
                &format!("[debug] using access token {access}"),
                &format!(
                    "Error: refresh {refresh} was rejected (token {access}, client {client_secret})."
                ),
                1,
            ))
        });
        let record = serde_json::to_string(&attempt).expect("the record serializes");
        for token in [access, refresh, client_secret] {
            assert!(
                !record.contains(token),
                "a credential reached the record: {record}"
            );
        }
        assert!(
            record.contains("chars)"),
            "the redaction says what it withheld: {record}"
        );
    }
}
