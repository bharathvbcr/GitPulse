//! App Hosting backends, rollouts and the commit each one deployed.
//!
//! Every call here builds its exact argv — program name included — through one
//! `*_argv` builder, so the command gate judges the same line the executor
//! runs. That is the rule `super::super::github::actions` states for GitHub
//! actions, and it applies with more force here: App Hosting has no read-only
//! OAuth scope, so the gate is the only thing between a misclick and the
//! user's production traffic.
//!
//! ## Why the listings are gated like actions
//!
//! `apphosting:backends:list` and `apphosting:rollouts:list` both declare
//! `.before(apphosting.ensureApiEnabled)` upstream, which enables the App
//! Hosting API on the project when it is off. A read that can change the
//! user's Cloud project is a mutation, so it is judged like one.
//!
//! ## `apphosting:rollouts:list` is not merely undocumented — it is usually absent
//!
//! Verified against firebase-tools 15.25.1 rather than inferred: upstream
//! registers the subcommand inside `if (experiments.isEnabled("internaltesting"))`,
//! and that experiment is off by default, with a description saying its
//! commands "are not meant for public consumption and may break or disappear
//! without a notice". So on a stock install the subcommand *does not exist*,
//! and firebase-tools answers an unregistered subcommand by exiting non-zero
//! having written nothing to either stream.
//!
//! Two consequences shape this module. [`super::probe_rollout_listing`] asks
//! the CLI what it has before anything tries to use it, so an absent feature
//! reads as an absent feature. And [`failure_reason`] never lets an
//! unparseable stdout hide the real message, because "no output at all" was
//! previously reported as a JSON parse error.
//!
//! `apphosting:backends:list` carries no such gate and is the surface this
//! panel can rely on everywhere.
//!
//! Everything here parses strictly and fails loudly: a shape we do not
//! recognise is an error, never an empty rollout list. An empty list is a
//! claim — "this backend has never deployed" — and only a real answer may
//! make it.

use super::{
    firebase_program, validate_project_id, FIREBASE_CALL_TIMEOUT, MAX_FIREBASE_ERROR_BYTES,
};
use crate::engine::git_cli::{byte_tail, capture_command, validate_repo};
use serde::{Deserialize, Serialize};

/// Upper bound on rollouts shown. One extra row is fetched so a capped list
/// reports itself instead of looking complete.
pub const ROLLOUT_DISPLAY_LIMIT: usize = 50;
/// Backends per project are few; the cap exists so a crafted or runaway
/// response cannot flood the panel.
pub const BACKEND_DISPLAY_LIMIT: usize = 50;

/// Bound on region names folded into a report, so `unreachable` cannot grow
/// without limit.
const MAX_UNREACHABLE_ENTRIES: usize = 20;

/// Bound on each free-text field carried out of a rollout.
///
/// Nothing upstream bounds a commit message, an author or a branch name, and
/// the subprocess output cap is 64 MiB — a backstop against a runaway process,
/// not a size a panel can render or a cache should hold. A commit subject is
/// tens of bytes; a kilobyte is already generous.
const MAX_COMMIT_TEXT_BYTES: usize = 1024;

/// Clips text to the cap on a character boundary, marking that it was cut.
///
/// The ellipsis is not decoration. A silently shortened commit message sends a
/// reader looking for text that is not missing, only cut — and this codebase
/// does not let a partial answer wear a complete one's clothes, at any scale.
fn clip(text: &str) -> String {
    if text.len() <= MAX_COMMIT_TEXT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_COMMIT_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Upper bound for a backend id reaching argv.
const MAX_BACKEND_ID_LEN: usize = 63;

/// The subcommand that lists rollouts.
///
/// Named once because three things must agree on it: the argv builder, the
/// capability probe in [`super`] that decides whether the CLI exposes it at
/// all, and the failure message that tells a reader which subcommand went
/// missing. Two of those are strings a reader compares by eye, which is how
/// they drift.
pub const ROLLOUTS_LIST_SUBCOMMAND: &str = "apphosting:rollouts:list";

/// App Hosting's rollout lifecycle, as the API's discovery document defines it.
///
/// `Unrecognised` is not a catch-all that behaves like a success. The rule this
/// codebase already applies to verdict vocabularies holds here: treating
/// everything that is not a known failure as a pass means the first writer to
/// invent a new state gets a green badge for it. An unrecognised state is
/// rendered as unknown and never counted as a deploy.
/// Internally tagged so every variant crosses the wire the same shape —
/// `{"kind":"succeeded"}` and `{"kind":"unrecognised","raw":"…"}` — rather than
/// a bare string for some variants and an object for one. A union that changes
/// shape per variant is the kind the UI gets wrong exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RolloutState {
    Unspecified,
    Queued,
    PendingBuild,
    Progressing,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    /// A state this build does not know. Carries the raw string so the UI can
    /// show what it actually was rather than inventing a verdict for it.
    Unrecognised {
        raw: String,
    },
}

impl RolloutState {
    pub fn from_api(raw: &str) -> Self {
        match raw {
            "STATE_UNSPECIFIED" => RolloutState::Unspecified,
            "QUEUED" => RolloutState::Queued,
            "PENDING_BUILD" => RolloutState::PendingBuild,
            "PROGRESSING" => RolloutState::Progressing,
            "PAUSED" => RolloutState::Paused,
            "SUCCEEDED" => RolloutState::Succeeded,
            "FAILED" => RolloutState::Failed,
            "CANCELLED" => RolloutState::Cancelled,
            "SKIPPED" => RolloutState::Skipped,
            other => RolloutState::Unrecognised {
                raw: other.to_string(),
            },
        }
    }

    /// True only for a rollout that actually reached production. Deliberately
    /// a closed set: a new upstream state must be added here on purpose.
    pub fn is_live(&self) -> bool {
        matches!(self, RolloutState::Succeeded)
    }
}

/// The commit a rollout's build came from.
///
/// This is the join key to local history: `hash` is a full SHA-1, the same key
/// `git cat-file` speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutCommit {
    pub hash: String,
    pub branch: Option<String>,
    pub message: Option<String>,
    pub author: Option<String>,
    pub commit_time: Option<String>,
    /// Whether this SHA resolves in the opened checkout.
    ///
    /// False is a real answer, not a failure: a force-push, a fork or a shallow
    /// clone all legitimately deploy commits this working copy does not hold.
    /// Such a rollout is kept and marked rather than dropped, and is never used
    /// in a lead-time figure.
    pub present_locally: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutInfo {
    /// Trailing id segment of the resource name, which is what a user sees.
    pub id: String,
    pub state: RolloutState,
    pub create_time: Option<String>,
    pub update_time: Option<String>,
    /// The rollout's own error, when the API reported one.
    pub error: Option<String>,
    pub commit: Option<RolloutCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: String,
    pub location: Option<String>,
    pub uri: Option<String>,
}

/// A listing plus everything that stops it from being the whole truth.
///
/// `truncated` and `walk_incomplete` are not synonyms and must not be merged:
/// `truncated` is *our* display cap, set by fetching one row past the limit;
/// `walk_incomplete` is *the producer* stopping early, which for App Hosting
/// means regions it could not reach. A caller that collapses them cannot tell
/// "we showed you 50 of 90" from "a whole region is missing".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseRolloutsReport {
    pub available: bool,
    pub checked: bool,
    pub cli_present: bool,
    pub project_id: String,
    pub backend_id: String,
    pub rollouts: Vec<RolloutInfo>,
    pub truncated: bool,
    pub unreachable: Vec<String>,
    pub walk_incomplete: Option<String>,
    pub error: Option<String>,
}

impl FirebaseRolloutsReport {
    /// A report that answers nothing, and says so.
    ///
    /// `checked: false` is the load-bearing field. Without it an unavailable
    /// listing is indistinguishable from a backend that has never deployed.
    pub fn unavailable(
        cli_present: bool,
        project_id: String,
        backend_id: String,
        error: Option<String>,
    ) -> Self {
        FirebaseRolloutsReport {
            available: false,
            checked: false,
            cli_present,
            project_id,
            backend_id,
            rollouts: Vec::new(),
            truncated: false,
            unreachable: Vec::new(),
            walk_incomplete: None,
            error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirebaseBackendsReport {
    pub available: bool,
    pub checked: bool,
    pub cli_present: bool,
    pub project_id: String,
    pub backends: Vec<BackendInfo>,
    pub truncated: bool,
    pub unreachable: Vec<String>,
    pub walk_incomplete: Option<String>,
    pub error: Option<String>,
}

impl FirebaseBackendsReport {
    pub fn unavailable(cli_present: bool, project_id: String, error: Option<String>) -> Self {
        FirebaseBackendsReport {
            available: false,
            checked: false,
            cli_present,
            project_id,
            backends: Vec::new(),
            truncated: false,
            unreachable: Vec::new(),
            walk_incomplete: None,
            error,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validates an argv-bound identifier.
///
/// What must never pass: flag-shaped values the CLI would re-parse as options,
/// control characters that corrupt argv and logs, and unbounded payloads.
fn validate_identifier(value: &str, label: &str, max_len: usize) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if trimmed.len() > max_len {
        return Err(format!("{label} exceeds the {max_len} character limit"));
    }
    if trimmed.starts_with('-') {
        return Err(format!("{label} must not start with '-'"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!("{label} contains control characters"));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(format!("{label} must not contain whitespace"));
    }
    Ok(trimmed.to_string())
}

pub fn validate_backend_id(backend_id: &str) -> Result<String, String> {
    validate_identifier(backend_id, "Backend id", MAX_BACKEND_ID_LEN)
}

/// Validates a full 40-character hex commit SHA.
///
/// Abbreviations are refused on purpose: the value names which commit reaches
/// production, and an ambiguous prefix is a fabricated target.
pub fn validate_commit_sha(sha: &str) -> Result<String, String> {
    let trimmed = sha.trim();
    if trimmed.len() != 40 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Commit must be a full 40-character hexadecimal SHA".into());
    }
    Ok(trimmed.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// argv builders — the single source both the gate and the executor read
// ---------------------------------------------------------------------------

/// Flags every invocation carries.
///
/// `--json` is not only for parsing: upstream treats it as implying
/// non-interactive, so a prompt — including the one that would enable the App
/// Hosting API — becomes a loud error rather than a silent enablement.
/// `--project` is always the resolved id and never an alias, so the string the
/// gate judges names the project the call actually hits; an alias could be
/// re-pointed by `.firebaserc` between the judgment and the run.
///
/// There is deliberately no `--location`. Neither App Hosting subcommand this
/// module calls accepts one: `apphosting:backends:list` declares no options at
/// all, `apphosting:rollouts:create` declares only `--git-branch`,
/// `--git-commit` and `--force`, and `apphosting:rollouts:list`'s `--location`
/// is documented upstream as "being removed in the next major release" — its
/// default of `-` already means every region, which is the answer this panel
/// wants. Sending a flag a subcommand does not declare is not ignored: the CLI
/// exits with `unknown option`, so an unused parameter here would be a
/// guaranteed failure rather than a harmless one.
fn push_common_flags(args: &mut Vec<String>, project_id: &str) {
    args.push("--project".to_string());
    args.push(project_id.to_string());
    args.push("--non-interactive".to_string());
    args.push("--json".to_string());
}

pub fn backends_list_argv(project_id: &str) -> Result<Vec<String>, String> {
    let project_id = validate_project_id(project_id)?;
    let mut args = vec![firebase_program(), "apphosting:backends:list".to_string()];
    push_common_flags(&mut args, &project_id);
    Ok(args)
}

pub fn rollouts_list_argv(project_id: &str, backend_id: &str) -> Result<Vec<String>, String> {
    let project_id = validate_project_id(project_id)?;
    let backend_id = validate_backend_id(backend_id)?;
    let mut args = vec![
        firebase_program(),
        ROLLOUTS_LIST_SUBCOMMAND.to_string(),
        backend_id,
    ];
    push_common_flags(&mut args, &project_id);
    Ok(args)
}

/// The argv for creating a rollout pinned to one commit.
///
/// There is no rollback verb in App Hosting, and inventing one here would be a
/// lie the gate then renders to the user. A rollback *is* this call with an
/// earlier SHA — same builder, same flags — so what the user approves is what
/// actually happens.
///
/// The flag is `--git-commit`, spelled exactly as upstream declares it. It was
/// `--git_commit` here until the CLI was asked: commander does not fold
/// underscores into hyphens, so that spelling exited 1 with
/// `error: unknown option '--git_commit'` and no rollout could ever have been
/// created. `--force` is deliberately absent — upstream only prompts when
/// *neither* a branch nor a commit is given, and this builder always gives a
/// commit, so nothing here suppresses a confirmation the user would otherwise
/// have seen.
pub fn rollout_create_argv(
    project_id: &str,
    backend_id: &str,
    git_commit: &str,
) -> Result<Vec<String>, String> {
    let project_id = validate_project_id(project_id)?;
    let backend_id = validate_backend_id(backend_id)?;
    let git_commit = validate_commit_sha(git_commit)?;
    let mut args = vec![
        firebase_program(),
        "apphosting:rollouts:create".to_string(),
        backend_id,
        "--git-commit".to_string(),
        git_commit,
    ];
    push_common_flags(&mut args, &project_id);
    Ok(args)
}

// ---------------------------------------------------------------------------
// Envelope + payload parsing
// ---------------------------------------------------------------------------

/// Unwraps `firebase --json`'s `{status, result}` / `{status, error}` envelope.
///
/// A non-success status is the CLI telling us the call failed — an
/// unauthenticated user, a disabled API, a missing backend. That is an error
/// with a reason, never an empty result.
fn unwrap_envelope(stdout: &[u8]) -> Result<serde_json::Value, String> {
    let root: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|error| format!("could not parse firebase --json output: {error}"))?;
    let status = root
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "firebase --json output carried no status field".to_string())?;
    if status != "success" {
        let message = root
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("firebase reported an error with no message");
        return Err(message.trim().to_string());
    }
    root.get("result")
        .cloned()
        .ok_or_else(|| "firebase reported success with no result".to_string())
}

/// Reads the `unreachable` list, distinguishing all three states it can be in.
///
/// Returns `None` when the key is **absent**, which is not the same as an empty
/// list. `apphosting:rollouts:list` is undocumented, so "the CLI did not tell
/// us about region reachability" is a real possibility, and it must not be
/// laundered into "every region answered". This is `PolicyVerdict.checked`'s
/// rule one level out: a check that could not run never looks like one that ran
/// and found nothing.
fn read_unreachable(value: &serde_json::Value) -> Option<Vec<String>> {
    let entries = value.get("unreachable")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .take(MAX_UNREACHABLE_ENTRIES)
            .collect(),
    )
}

/// Composes the one sentence that says why a listing is not the whole truth.
fn incompleteness(unreachable: &Option<Vec<String>>) -> (Vec<String>, Option<String>) {
    match unreachable {
        None => (
            Vec::new(),
            Some(
                "The Firebase CLI did not report region reachability, so this list may be missing \
                 whole regions."
                    .to_string(),
            ),
        ),
        Some(regions) if regions.is_empty() => (Vec::new(), None),
        Some(regions) => (
            regions.clone(),
            Some(format!(
                "Firebase could not reach {}: {}. Rollouts deployed there are missing from this \
                 list.",
                if regions.len() == 1 {
                    "one region".to_string()
                } else {
                    format!("{} regions", regions.len())
                },
                regions.join(", ")
            )),
        ),
    }
}

/// The trailing segment of a `projects/…/backends/…/rollouts/<id>` name.
fn resource_id(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).trim().to_string()
}

/// Pulls an array out of either a bare array result or an object carrying it
/// under `key`.
///
/// Both shapes are accepted because the CLI unwraps some list results and not
/// others, and the undocumented subcommand could do either. What is *not*
/// accepted is a shape carrying neither — that is a parse error.
fn array_field<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    if let Some(items) = value.as_array() {
        return Ok(items);
    }
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("firebase --json result carried no {key} array"))
}

fn parse_commit(rollout: &serde_json::Value) -> Option<RolloutCommit> {
    let codebase = rollout
        .get("build")
        .and_then(|b| b.get("source"))
        .and_then(|s| s.get("codebase"))?;
    let hash = codebase
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|h| !h.is_empty())?;
    let text = |key: &str| {
        codebase
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(clip)
    };
    Some(RolloutCommit {
        hash: hash.to_string(),
        branch: text("branch"),
        message: text("commit"),
        author: text("author"),
        commit_time: text("commitTime"),
        // Filled in by the caller, which is the only place that knows the repo.
        present_locally: false,
    })
}

/// Parses the rollouts payload into rows plus the display-cap flag.
pub fn parse_rollouts(
    result: &serde_json::Value,
    display_limit: usize,
) -> Result<(Vec<RolloutInfo>, bool), String> {
    let items = array_field(result, "rollouts")?;
    let truncated = items.len() > display_limit;
    let rollouts = items
        .iter()
        .take(display_limit)
        .map(|rollout| {
            let name = rollout
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let state = rollout
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(RolloutState::from_api)
                // An absent state is not "unspecified" — that is a value the
                // API can send deliberately. Absence is a shape we did not
                // recognise, and it says so.
                .unwrap_or(RolloutState::Unrecognised {
                    raw: "<missing>".to_string(),
                });
            let error = rollout
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(clip);
            let text = |key: &str| {
                rollout
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            RolloutInfo {
                id: resource_id(name),
                state,
                create_time: text("createTime"),
                update_time: text("updateTime"),
                error,
                commit: parse_commit(rollout),
            }
        })
        .collect();
    Ok((rollouts, truncated))
}

pub fn parse_backends(
    result: &serde_json::Value,
    display_limit: usize,
) -> Result<(Vec<BackendInfo>, bool), String> {
    let items = array_field(result, "backends")?;
    let truncated = items.len() > display_limit;
    let backends = items
        .iter()
        .take(display_limit)
        .map(|backend| {
            let name = backend
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            BackendInfo {
                id: resource_id(name),
                // `projects/p/locations/us-central1/backends/b` — the segment
                // after `locations`.
                location: name
                    .split('/')
                    .skip_while(|segment| *segment != "locations")
                    .nth(1)
                    .map(str::to_string),
                uri: backend
                    .get("uri")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }
        })
        .collect();
    Ok((backends, truncated))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Runs one `firebase` invocation inside the repository.
///
/// `args` is a full argv line starting with the program name; everything after
/// it is passed verbatim, so the line the gate judged is the line that runs.
pub(crate) fn run_firebase_in(repo_path: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let program = firebase_program();
    debug_assert!(
        args.first() == Some(&program),
        "argv builders must include the program name"
    );
    let repo = validate_repo(repo_path)?;
    let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let output = capture_command(&program, &refs, Some(&repo), FIREBASE_CALL_TIMEOUT, &[])?;
    if !output.success {
        return Err(failure_reason(
            &output.stdout,
            &output.stderr,
            output.status_code,
            args.get(1).map(String::as_str).unwrap_or("firebase"),
        ));
    }
    Ok(output.stdout)
}

/// The best reason available for a non-zero `firebase` exit.
///
/// The three sources are tried in the order a reader would want them, and
/// crucially none of them can swallow the next. An earlier version ran
/// `unwrap_envelope(&stdout)?`, which meant an *unparseable* stdout — the empty
/// one every argument-level failure produces — short-circuited with
/// "could not parse firebase --json output: EOF while parsing a value", and the
/// stderr line that actually said `unknown option '--git_commit'` was never
/// reached. A diagnostic that hides the diagnosis is worse than none: it sends
/// the reader after a JSON bug that does not exist.
fn failure_reason(stdout: &[u8], stderr: &[u8], status_code: i32, subcommand: &str) -> String {
    // 1. The CLI's own `{status:"error"}` envelope, which is written for a human.
    if let Err(message) = unwrap_envelope(stdout) {
        if !message.trim().is_empty() && !message.starts_with("could not parse") {
            return message;
        }
    }
    // 2. Whatever it wrote to stderr — commander's option and argument errors
    //    land here and nowhere else.
    let tail = byte_tail(stderr, MAX_FIREBASE_ERROR_BYTES);
    if !tail.trim().is_empty() {
        return tail.trim().to_string();
    }
    // 3. Silence. firebase-tools exits non-zero and prints nothing at all when
    //    the subcommand is not registered in this build, so name the
    //    subcommand rather than reporting a bare exit code the reader cannot
    //    act on. Hedged deliberately: this is the shape of that failure, not
    //    proof of it, and a cause stated as certain would be a guess wearing a
    //    fact's clothes.
    format!(
        "firebase exited {status_code} without writing any output. This usually means \
         `{subcommand}` is not available in this build of the Firebase CLI."
    )
}

/// What a rollout-create attempt actually did.
///
/// `created` is the exit status and nothing else, deliberately. Creating a
/// rollout is **not idempotent** — upstream allocates the next rollout id per
/// call — so a run that succeeded and is reported as failed costs the user a
/// second deployment when they retry. The envelope is treated as corroboration
/// only: see [`create_rollout`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutCreateOutcome {
    pub project_id: String,
    pub backend_id: String,
    pub git_commit: String,
    pub created: bool,
    /// Set when the CLI exited zero but did not confirm it in its output.
    ///
    /// The rollout was still started. This field exists so the panel can say
    /// "started, but the CLI did not confirm it" rather than pick one of the
    /// two clean answers it does not have.
    pub unconfirmed: Option<String>,
}

/// Confirms a zero-exit envelope, without ever turning a success into an error.
///
/// `apphosting:rollouts:create` returns nothing from its action, so `--json`
/// emits `{"status":"success"}` with **no** `result` — which is why
/// [`unwrap_envelope`] cannot be reused here: it requires a result and would
/// report a completed deployment as a failure.
fn confirm_created(stdout: &[u8]) -> Option<String> {
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return Some(
            "The Firebase CLI exited successfully but its output could not be parsed, so the \
             rollout could not be confirmed here. Check the Firebase console before retrying — \
             creating a rollout twice deploys twice."
                .to_string(),
        );
    };
    match root.get("status").and_then(serde_json::Value::as_str) {
        Some("success") => None,
        _ => Some(
            "The Firebase CLI exited successfully but did not report success in its output, so \
             the rollout could not be confirmed here. Check the Firebase console before retrying \
             — creating a rollout twice deploys twice."
                .to_string(),
        ),
    }
}

/// Creates a rollout pinned to one commit.
///
/// The argv is built by the caller and passed through whole, so the line the
/// policy gate judged is the line that runs — the property that matters most
/// for the one command here that changes what production serves.
pub fn create_rollout(
    repo_path: &str,
    project_id: &str,
    backend_id: &str,
    git_commit: &str,
    argv: &[String],
) -> Result<RolloutCreateOutcome, String> {
    let stdout = run_firebase_in(repo_path, argv)?;
    Ok(RolloutCreateOutcome {
        project_id: project_id.to_string(),
        backend_id: backend_id.to_string(),
        git_commit: git_commit.to_string(),
        created: true,
        unconfirmed: confirm_created(&stdout),
    })
}

/// Marks which rollout commits exist in the opened checkout.
///
/// A rollout whose SHA does not resolve locally is kept and flagged, never
/// dropped: it is a true statement about production that this working copy
/// simply cannot show a commit for.
/// Whether a hash is safe and specific enough to ask git about.
///
/// `hash` arrives from the App Hosting API — a field nothing in this process
/// wrote — and [`mark_local_presence`] turns it into an argument for
/// `git cat-file -e`. A value beginning with `-` would be re-parsed by git as
/// an *option* rather than an object, which is argument injection through a
/// payload field; a ref name like `HEAD` would resolve to something that is not
/// the deployed commit and quietly report the wrong answer. Only a full SHA is
/// both safe to pass and specific enough to mean anything, and the resulting
/// `present_locally: false` is true either way — this checkout cannot resolve
/// it.
fn resolvable_locally(hash: &str) -> bool {
    hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit())
}

fn mark_local_presence(repo_path: &str, rollouts: &mut [RolloutInfo]) {
    let Ok(repo) = validate_repo(repo_path) else {
        return;
    };
    for rollout in rollouts.iter_mut() {
        let Some(commit) = rollout.commit.as_mut() else {
            continue;
        };
        if !resolvable_locally(&commit.hash) {
            continue;
        }
        let spec = format!("{}^{{commit}}", commit.hash);
        // `git_captured` returns `Ok` for a non-zero exit — that is the whole
        // point of it, because "this object is absent" is an answer rather
        // than a failure. So presence is `success`, never `is_ok()`: reading
        // the result as `is_ok()` would mark every rollout present, including
        // the force-pushed ones this flag exists to distinguish.
        commit.present_locally =
            crate::engine::git_cli::git_captured(&repo, &["cat-file", "-e", &spec])
                .map(|run| run.success)
                .unwrap_or(false);
    }
}

/// Lists App Hosting backends for one project.
pub fn load_backends_report(
    repo_path: &str,
    project_id: &str,
    argv: &[String],
    cli_present: bool,
) -> FirebaseBackendsReport {
    let stdout = match run_firebase_in(repo_path, argv) {
        Ok(stdout) => stdout,
        Err(error) => {
            return FirebaseBackendsReport::unavailable(
                cli_present,
                project_id.to_string(),
                Some(error),
            )
        }
    };
    let result = match unwrap_envelope(&stdout) {
        Ok(result) => result,
        Err(error) => {
            return FirebaseBackendsReport::unavailable(
                cli_present,
                project_id.to_string(),
                Some(error),
            )
        }
    };
    let (backends, truncated) = match parse_backends(&result, BACKEND_DISPLAY_LIMIT) {
        Ok(parsed) => parsed,
        Err(error) => {
            return FirebaseBackendsReport::unavailable(
                cli_present,
                project_id.to_string(),
                Some(error),
            )
        }
    };
    let (unreachable, walk_incomplete) = incompleteness(&read_unreachable(&result));
    FirebaseBackendsReport {
        available: true,
        checked: true,
        cli_present,
        project_id: project_id.to_string(),
        backends,
        truncated,
        unreachable,
        walk_incomplete,
        error: None,
    }
}

/// Lists rollouts for one backend, joined to local commits.
pub fn load_rollouts_report(
    repo_path: &str,
    project_id: &str,
    backend_id: &str,
    argv: &[String],
    cli_present: bool,
) -> FirebaseRolloutsReport {
    let unavailable = |error: String| {
        FirebaseRolloutsReport::unavailable(
            cli_present,
            project_id.to_string(),
            backend_id.to_string(),
            Some(error),
        )
    };
    let stdout = match run_firebase_in(repo_path, argv) {
        Ok(stdout) => stdout,
        Err(error) => return unavailable(error),
    };
    let result = match unwrap_envelope(&stdout) {
        Ok(result) => result,
        Err(error) => return unavailable(error),
    };
    let (mut rollouts, truncated) = match parse_rollouts(&result, ROLLOUT_DISPLAY_LIMIT) {
        Ok(parsed) => parsed,
        Err(error) => return unavailable(error),
    };
    mark_local_presence(repo_path, &mut rollouts);
    let (unreachable, walk_incomplete) = incompleteness(&read_unreachable(&result));
    FirebaseRolloutsReport {
        available: true,
        checked: true,
        cli_present,
        project_id: project_id.to_string(),
        backend_id: backend_id.to_string(),
        rollouts,
        truncated,
        unreachable,
        walk_incomplete,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(source: &str) -> serde_json::Value {
        serde_json::from_str(source).expect("test fixture parses")
    }

    #[test]
    fn envelope_error_status_is_an_error_not_an_empty_result() {
        let err = unwrap_envelope(br#"{"status":"error","error":"Not logged in"}"#)
            .expect_err("error status must not unwrap");
        assert_eq!(err, "Not logged in");
    }

    #[test]
    fn garbage_is_a_parse_error_not_an_empty_success() {
        // The negative battery the gh parser already uses: nothing that is not
        // a recognised success envelope may deserialize into "no rollouts".
        for bad in [
            &b""[..],
            b"not json",
            br#"{"no":true}"#,
            br#"[]"#,
            br#"{"status":"success"}"#,
        ] {
            assert!(
                unwrap_envelope(bad).is_err(),
                "unrecognised payload must be an error: {}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_warning_preamble_before_the_envelope_is_a_parse_error() {
        assert!(
            unwrap_envelope(b"! Warning: preview\n{\"status\":\"success\",\"result\":[]}").is_err()
        );
    }

    #[test]
    fn unreachable_has_three_states_and_absence_is_not_emptiness() {
        // Present and non-empty: named regions, and every derived number is a
        // floor.
        let (regions, note) = incompleteness(&read_unreachable(&value(
            r#"{"unreachable":["us-central1"]}"#,
        )));
        assert_eq!(regions, vec!["us-central1".to_string()]);
        assert!(note
            .expect("a named region must be disclosed")
            .contains("us-central1"));

        // Present and empty: the producer told us it reached everything.
        let (regions, note) = incompleteness(&read_unreachable(&value(r#"{"unreachable":[]}"#)));
        assert!(regions.is_empty());
        assert!(
            note.is_none(),
            "an empty unreachable list is a complete answer"
        );

        // Absent: the CLI said nothing. This must NOT read as complete.
        let (regions, note) = incompleteness(&read_unreachable(&value(r#"{"rollouts":[]}"#)));
        assert!(regions.is_empty());
        assert!(
            note.expect("a missing key must be disclosed")
                .contains("did not report region reachability"),
            "a missing unreachable key must never render as a complete listing"
        );
    }

    #[test]
    fn display_cap_reports_itself() {
        let rollouts: Vec<serde_json::Value> = (0..5)
            .map(|i| value(&format!(r#"{{"name":"r/{i}","state":"SUCCEEDED"}}"#)))
            .collect();
        let payload = serde_json::json!({ "rollouts": rollouts });
        let (rows, truncated) = parse_rollouts(&payload, 3).expect("parses");
        assert_eq!(rows.len(), 3);
        assert!(truncated);

        let (rows, truncated) = parse_rollouts(&payload, 50).expect("parses");
        assert_eq!(rows.len(), 5);
        assert!(!truncated);
    }

    #[test]
    fn the_commit_join_key_is_read_from_build_source_codebase() {
        let payload = value(
            r#"{"rollouts":[{"name":"projects/p/locations/us-central1/backends/b/rollouts/r1",
                 "state":"SUCCEEDED","createTime":"2026-09-01T00:00:00Z",
                 "build":{"source":{"codebase":{
                   "hash":"0123456789abcdef0123456789abcdef01234567",
                   "branch":"main","commit":"ship it","author":"ada"}}}}]}"#,
        );
        let (rows, _) = parse_rollouts(&payload, 50).expect("parses");
        let commit = rows[0].commit.as_ref().expect("codebase carries a commit");
        assert_eq!(commit.hash, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(commit.branch.as_deref(), Some("main"));
        assert_eq!(rows[0].id, "r1");
        assert_eq!(rows[0].state, RolloutState::Succeeded);
        // Local presence is the caller's answer, not the payload's.
        assert!(!commit.present_locally);
    }

    #[test]
    fn an_unknown_state_is_unknown_rather_than_a_deploy() {
        let payload = value(r#"{"rollouts":[{"name":"r/1","state":"TELEPORTED"}]}"#);
        let (rows, _) = parse_rollouts(&payload, 50).expect("parses");
        assert_eq!(
            rows[0].state,
            RolloutState::Unrecognised {
                raw: "TELEPORTED".into()
            }
        );
        assert!(
            !rows[0].state.is_live(),
            "a state this build does not know must never count as live"
        );
        // An absent state is a shape we did not recognise, not "unspecified".
        let payload = value(r#"{"rollouts":[{"name":"r/1"}]}"#);
        let (rows, _) = parse_rollouts(&payload, 50).expect("parses");
        assert!(matches!(rows[0].state, RolloutState::Unrecognised { .. }));
    }

    #[test]
    fn a_result_carrying_neither_shape_is_an_error() {
        assert!(parse_rollouts(&value(r#"{"items":[]}"#), 50).is_err());
        // A bare array is accepted — the CLI unwraps some list results.
        assert!(parse_rollouts(&value("[]"), 50).is_ok());
    }

    /// Every argv this module can build, so no test below has to remember to
    /// add itself to a second list. A new builder that is not routed through
    /// here is the one thing this file cannot catch, which is what
    /// `every_subcommand_literal_in_this_module_is_on_the_allow_list` is for.
    fn every_argv() -> Vec<Vec<String>> {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        vec![
            backends_list_argv("acme-prod").expect("valid"),
            rollouts_list_argv("acme-prod", "web").expect("valid"),
            rollout_create_argv("acme-prod", "web", sha).expect("valid"),
        ]
    }

    #[test]
    fn argv_leads_with_the_program_and_pins_the_project() {
        let argv = rollouts_list_argv("acme-prod", "web").expect("valid");
        assert_eq!(
            argv,
            vec![
                firebase_program(),
                "apphosting:rollouts:list".to_string(),
                "web".to_string(),
                "--project".to_string(),
                "acme-prod".to_string(),
                "--non-interactive".to_string(),
                "--json".to_string(),
            ]
        );
    }

    #[test]
    fn a_rollback_is_the_create_builder_with_an_earlier_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let argv = rollout_create_argv("acme-prod", "web", sha).expect("valid");
        assert_eq!(argv[1], "apphosting:rollouts:create");
        assert!(argv.contains(&"--git-commit".to_string()));
        assert!(argv.contains(&sha.to_string()));
        // No separate rollback verb exists to drift from this one.
        assert!(!argv.iter().any(|a| a.contains("rollback")));
        // `--force` suppresses upstream's confirmation prompt. Upstream only
        // prompts when neither a branch nor a commit was named, and this
        // builder always names a commit — so the flag would buy nothing and
        // cost the one prompt a user might still see.
        assert!(!argv.iter().any(|a| a == "--force"));
    }

    #[test]
    fn no_flag_is_spelled_with_an_underscore() {
        // The class behind a real defect: this builder emitted `--git_commit`,
        // and commander does not fold underscores into hyphens, so the CLI
        // answered `error: unknown option '--git_commit'` and no rollout could
        // ever have been created. Asserting the one corrected spelling would
        // fix the case; refusing the shape fixes the class, and costs nothing
        // because no Firebase flag contains an underscore.
        for argv in every_argv() {
            for arg in argv.iter().filter(|a| a.starts_with("--")) {
                assert!(
                    !arg.contains('_'),
                    "`{arg}` would be rejected as an unknown option: {argv:?}"
                );
            }
        }
    }

    #[test]
    fn every_flag_is_one_its_own_subcommand_declares() {
        // Verified against firebase-tools 15.25.1's own command definitions.
        // A flag a subcommand does not declare is not ignored — commander
        // exits non-zero with `unknown option` — so an extra flag is a
        // guaranteed failure, which is exactly how `--location` survived on
        // two builders that cannot accept it.
        const GLOBAL: [&str; 3] = ["--project", "--non-interactive", "--json"];
        let declared = |subcommand: &str| -> Vec<&'static str> {
            match subcommand {
                // `.option()` appears nowhere in apphosting-backends-list.js.
                "apphosting:backends:list" => vec![],
                // `-l, --location` exists but upstream logs that it "is being
                // removed in the next major release", and its default of `-`
                // already means every region.
                "apphosting:rollouts:list" => vec!["--location"],
                "apphosting:rollouts:create" => {
                    vec!["--git-branch", "--git-commit", "--force"]
                }
                other => panic!("unknown subcommand {other} — add its declared flags"),
            }
        };
        for argv in every_argv() {
            let subcommand = argv[1].clone();
            let allowed = declared(&subcommand);
            for arg in argv.iter().filter(|a| a.starts_with("--")) {
                assert!(
                    GLOBAL.contains(&arg.as_str()) || allowed.contains(&arg.as_str()),
                    "`{subcommand}` does not declare `{arg}`, so the CLI would refuse it: {argv:?}"
                );
            }
        }
    }

    #[test]
    fn flag_shaped_and_malformed_inputs_never_reach_argv() {
        assert!(rollouts_list_argv("acme-prod", "-f").is_err());
        assert!(rollouts_list_argv("acme-prod", "web app").is_err());
        assert!(rollouts_list_argv("acme-prod", "web\u{7}").is_err());
        assert!(rollouts_list_argv("-evil", "web").is_err());
        assert!(rollouts_list_argv("acme-prod", "--project=evil").is_err());
        assert!(rollouts_list_argv("acme-prod", "").is_err());
        // Abbreviated SHAs are ambiguous targets for a production rollout.
        assert!(rollout_create_argv("acme-prod", "web", "0123456").is_err());
        assert!(rollout_create_argv("acme-prod", "web", &"z".repeat(40)).is_err());
        assert!(rollout_create_argv("acme-prod", "web", "").is_err());
    }

    #[test]
    fn listing_argv_never_carries_a_destructive_verb() {
        // The firebase CLI puts read and write verbs in one namespace:
        // `firestore:indexes` and `firestore:delete` are one word apart. These
        // builders are the only place argv is constructed, so this is where
        // that class is refused.
        const DENIED: [&str; 9] = [
            "deploy",
            "firestore:delete",
            "firestore:bulkdelete",
            "functions:delete",
            "hosting:disable",
            "apphosting:backends:delete",
            "apphosting:backends:create",
            "databases:delete",
            // A credential on argv, readable by any local process through
            // /proc/<pid>/cmdline or ps. Deprecated upstream as well.
            "--token",
        ];
        for argv in every_argv() {
            for denied in DENIED {
                assert!(
                    !argv.iter().any(|arg| arg == denied),
                    "argv must never carry {denied}: {argv:?}"
                );
            }
            assert!(argv.contains(&"--json".to_string()));
            assert!(argv.contains(&"--non-interactive".to_string()));
            assert_eq!(argv[0], firebase_program(), "the gate judges argv[0] too");
        }
    }

    #[test]
    fn every_subcommand_literal_in_this_module_is_on_the_allow_list() {
        // Derived, not hand-listed: it reads this file and finds every literal
        // shaped like a firebase subcommand, so a builder added tomorrow is
        // covered whether or not anyone remembers `every_argv`. The allow-list
        // holds only verbs that read, plus the one create verb the user
        // explicitly confirms.
        const ALLOWED: [&str; 4] = [
            "apphosting:backends:list",
            "apphosting:rollouts:list",
            "apphosting:rollouts:create",
            // The capability probe lists a command *group*; it runs no verb.
            "apphosting:rollouts",
        ];
        // Only the shipping half. The test module below deliberately spells
        // out verbs it exists to forbid, and a scanner that read those would
        // fail on its own deny-list — so the cut is load-bearing, and the
        // assertions that follow it prove it landed where it was meant to
        // rather than at the end of an empty string.
        const TEST_MODULE_MARKER: &str = "#[cfg(test)]";
        let whole = include_str!("apphosting.rs");
        let cut = whole
            .find(TEST_MODULE_MARKER)
            .expect("the test module marker must exist, or this scan covers the wrong text");
        let source = &whole[..cut];
        assert!(
            source.contains("apphosting:rollouts:create"),
            "the production half must still be inside the scanned region"
        );
        assert!(
            !source.contains("apphosting:backends:delete"),
            "the deny-list in the test module must be outside the scanned region"
        );
        let mut seen = Vec::new();
        for (index, _) in source.match_indices('"') {
            let rest = &source[index + 1..];
            let Some(end) = rest.find('"') else { continue };
            let literal = &rest[..end];
            if literal.contains(':')
                && !literal.contains(' ')
                && literal.starts_with("apphosting")
                && literal
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == ':' || c.is_ascii_digit())
            {
                seen.push(literal.to_string());
            }
        }
        assert!(
            !seen.is_empty(),
            "the scanner found no subcommand literals at all, so it proves nothing"
        );
        for literal in seen {
            assert!(
                ALLOWED.contains(&literal.as_str()),
                "`{literal}` is not an allowed firebase subcommand — add it deliberately, \
                 with a reason, or use a read verb"
            );
        }
    }

    #[test]
    fn only_a_real_sha_is_ever_handed_to_git() {
        // `hash` arrives from the App Hosting API, and `mark_local_presence`
        // turns it into an argument for `git cat-file -e`. A value beginning
        // with `-` would be re-parsed by git as an option rather than as an
        // object — argument injection from a field nothing here wrote. The
        // presence check must decline such a value instead of spawning it.
        for hostile in [
            "--batch",
            "-e",
            "--help",
            "HEAD",
            "master:../../etc/passwd",
            "",
            "   ",
            "0123456",
            &"f".repeat(41),
        ] {
            assert!(
                !resolvable_locally(hostile),
                "`{hostile}` must never reach git as an object spec"
            );
        }
        assert!(resolvable_locally(
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(resolvable_locally(
            "0123456789ABCDEF0123456789abcdef01234567"
        ));
    }

    #[test]
    fn a_runaway_commit_message_is_clipped_rather_than_carried_whole() {
        // Nothing upstream bounds these strings, and the subprocess cap is 64
        // MiB — a backstop, not a size a panel can render or a cache should
        // hold. Clipped visibly, because silently truncating a commit message
        // is the kind of edit that has a reader hunting for text that is not
        // missing, only cut.
        let long = "x".repeat(MAX_COMMIT_TEXT_BYTES * 3);
        let payload = value(&format!(
            r#"{{"rollouts":[{{"name":"p/r/1","state":"SUCCEEDED","build":{{"source":{{"codebase":{{"hash":"{}","commit":"{long}","author":"{long}","branch":"{long}"}}}}}}}}]}}"#,
            "a".repeat(40)
        ));
        let (rows, _) = parse_rollouts(&payload, 50).expect("parses");
        let commit = rows[0].commit.as_ref().expect("a commit is present");
        for field in [&commit.message, &commit.author, &commit.branch] {
            let text = field.as_ref().expect("field is present");
            assert!(
                text.len() <= MAX_COMMIT_TEXT_BYTES + 4,
                "field was {} bytes, past the cap",
                text.len()
            );
            assert!(
                text.ends_with('…'),
                "a clipped field must show that it was cut"
            );
        }
        // A short field is untouched — the cap must not mark everything.
        let ok = value(&format!(
            r#"{{"rollouts":[{{"name":"p/r/1","build":{{"source":{{"codebase":{{"hash":"{}","commit":"ship it"}}}}}}}}]}}"#,
            "a".repeat(40)
        ));
        let (rows, _) = parse_rollouts(&ok, 50).expect("parses");
        assert_eq!(
            rows[0].commit.as_ref().unwrap().message.as_deref(),
            Some("ship it")
        );
    }

    #[test]
    fn a_zero_exit_is_never_re_reported_as_a_failed_deployment() {
        // `apphosting:rollouts:create` returns nothing from its action, so
        // `--json` emits `{"status":"success"}` with no `result` — which is why
        // `unwrap_envelope` cannot be reused here: it requires a result and
        // would call a completed deployment a failure.
        assert_eq!(confirm_created(br#"{"status":"success"}"#), None);
        assert_eq!(
            confirm_created(br#"{"status":"success","result":null}"#),
            None
        );

        // Everything else is *unconfirmed*, never failed. Creating a rollout
        // allocates a new id per call, so a false failure costs the user a
        // second deployment when they retry — and the message has to say so.
        for ambiguous in [&b""[..], b"not json", br#"{"status":"error"}"#, br#"{}"#] {
            let note = confirm_created(ambiguous)
                .expect("an unconfirmable envelope must be reported, not swallowed");
            assert!(
                note.contains("twice deploys twice"),
                "the note must warn against a blind retry: {note}"
            );
        }
    }

    #[test]
    fn a_failure_never_hides_its_reason_behind_a_parse_error() {
        // The CLI's own envelope wins when it carries one.
        assert_eq!(
            failure_reason(
                br#"{"status":"error","error":"Not logged in"}"#,
                b"",
                1,
                "x"
            ),
            "Not logged in"
        );
        // An unparseable stdout must fall THROUGH to stderr rather than
        // short-circuit on it. This is the regression: commander writes
        // `unknown option` to stderr and nothing at all to stdout, and the
        // previous code answered "could not parse firebase --json output".
        assert_eq!(
            failure_reason(b"", b"error: unknown option '--git_commit'\n", 1, "x"),
            "error: unknown option '--git_commit'"
        );
        assert_eq!(
            failure_reason(b"not json", b"real reason", 1, "x"),
            "real reason"
        );
        // Silence on both streams is firebase-tools' unregistered-subcommand
        // signature, so the message names the subcommand instead of reporting
        // a bare exit code.
        let silent = failure_reason(b"", b"", 1, "apphosting:rollouts:list");
        assert!(silent.contains("apphosting:rollouts:list"), "{silent}");
        assert!(!silent.contains("could not parse"), "{silent}");
    }

    #[test]
    fn an_unavailable_report_is_distinguishable_from_a_backend_that_never_deployed() {
        let report = FirebaseRolloutsReport::unavailable(
            false,
            "acme-prod".into(),
            "web".into(),
            Some("Firebase CLI is not installed".into()),
        );
        assert!(
            !report.checked,
            "checked separates 'could not ask' from 'asked'"
        );
        assert!(report.rollouts.is_empty());
        assert!(report.error.is_some());
    }
}
