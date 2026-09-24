//! Apple Intelligence: the on-device model, for task titles and descriptions.
//!
//! GitPulse already had one way to write a task from notes — a local model
//! server discovered over loopback, driven by the Manvi worker. That works,
//! but it asks the reader to install and run a model first, and on a Mac that
//! can run Apple Intelligence the answer is already on the machine, is faster,
//! and never touches a socket at all.
//!
//! ## Where this sits in the lifecycle
//!
//! It does **not** replace the enhancement lifecycle; it replaces exactly one
//! step of it. A proposal is still created in the local store
//! (`enhancements.create`), so it has an id, a revision, a source revision and
//! a history. Instead of routing `enhancements.generate` to the Manvi sidecar,
//! the frontend calls [`generate`] here and then publishes the result with
//! `enhancements.complete`. Accept, undo, dismiss, the history drawer and the
//! field locks are all untouched, because none of them ever knew which engine
//! wrote the text.
//!
//! ## What this module will not do
//!
//! It will not claim a capability it cannot observe. There are three distinct
//! negative answers and they are kept distinct:
//!
//! * **not compiled in** — this binary has no bridge, because the SDK that
//!   built it had no `FoundationModels.framework`. A fact about the build.
//! * **unsupported OS** — the bridge exists but this Mac is below macOS 26.
//! * **unavailable** — the framework answered, with a reason of its own
//!   (Apple Intelligence switched off, device not eligible, model not ready).
//!
//! Collapsing those into one "unavailable" would tell a reader whose Mac is
//! perfectly capable that their hardware is the problem.

#[cfg(apple_intelligence)]
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Longest note text handed to the model in one request.
///
/// The framework has its own context window and raises
/// `exceededContextWindowSize` past it, but that error costs a round trip and
/// arrives as a failed proposal. This bound is the cheap refusal in front of
/// it, and it is generous: a task brief that needs more than this is not a
/// task brief.
pub const MAX_INPUT_CHARS: usize = 12_000;

/// Published context window of Apple's on-device system model, in tokens.
///
/// This is the limit documented for `SystemLanguageModel`, not a measurement
/// from the Mac this process is running on. Commit briefs stay far below it
/// because the instructions and the reply share the same window.
pub const ON_DEVICE_CONTEXT_TOKENS: i64 = 4_096;
/// Wall-clock budget for one generation, including model load on first use.
///
/// Only the bridged build has anything to time; without it there is no call to
/// bound. The store's own lease on a pending proposal is 180 seconds, so this
/// has to stay comfortably under that or a slow generation would be refused on
/// arrival with "expired" instead of being reported as slow.
#[cfg(apple_intelligence)]
const GENERATION_TIMEOUT: Duration = Duration::from_secs(90);

/// Whether the on-device model can be used, and if not, exactly why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppleIntelligenceStatus {
    /// False when this binary was built without the framework.
    pub compiled: bool,
    /// `available` | `unavailable` | `unsupported_os`
    pub state: String,
    /// Machine-readable cause when `state` is not `available`.
    pub reason: Option<String>,
    /// One sentence a reader can act on.
    pub detail: String,
}

impl AppleIntelligenceStatus {
    pub fn ready(&self) -> bool {
        self.state == "available"
    }

    /// Only the bridge-less build constructs this; with the bridge linked,
    /// every status comes from the framework itself.
    #[cfg(not(apple_intelligence))]
    fn not_compiled(detail: &str) -> Self {
        Self {
            compiled: false,
            state: "unsupported_os".to_string(),
            reason: Some("not_compiled".to_string()),
            detail: detail.to_string(),
        }
    }
}

/// What the caller wants written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleIntelligenceRequest {
    /// `draft` (from notes), `improve` (rewrite what exists), `extract`.
    pub kind: String,
    /// Which of `title` / `description` to return.
    pub fields: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Repository, task type and labels, as one line.
    #[serde(default)]
    pub context: String,
}

/// One proposal from the on-device model.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppleIntelligenceDraft {
    pub title: Option<String>,
    pub description: Option<String>,
    /// Why this can be trusted, in the reader's terms. Stored on the proposal.
    pub rationale: String,
}

/// Everything the bridge can return, kept as one shape so a failure carries a
/// code the frontend can branch on rather than a sentence it has to match.
///
/// Gated on the bridge plus `test`: with no bridge there are no replies to
/// read, but the rules for reading one are the same either way and the tests
/// assert them on every platform.
#[cfg(any(apple_intelligence, test))]
#[derive(Debug, Deserialize)]
struct BridgeReply {
    ok: bool,
    code: Option<String>,
    message: Option<String>,
    title: Option<String>,
    description: Option<String>,
    rationale: Option<String>,
}

/// A refusal from this module or from the framework.
///
/// Crosses IPC as a struct rather than a string on purpose: "Apple
/// Intelligence is busy" and "Apple Intelligence declined to write this" want
/// different affordances, and a frontend matching on sentences would break the
/// first time one is reworded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppleIntelligenceError {
    pub code: String,
    pub message: String,
}

impl AppleIntelligenceError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AppleIntelligenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppleIntelligenceError {}

/// Rejects a request this module should not send, before any model runs.
///
/// Separate from the bridge so it is testable on every platform: the rules are
/// about what GitPulse is willing to ask for, not about what Apple's model can
/// do. Returns the request's total input size on success.
pub fn validate(request: &AppleIntelligenceRequest) -> Result<usize, AppleIntelligenceError> {
    if !matches!(
        request.kind.as_str(),
        "draft" | "improve" | "extract" | "commit_subject"
    ) {
        return Err(AppleIntelligenceError::new(
            "invalid_input",
            "Unknown generation kind.",
        ));
    }
    let fields: Vec<&str> = request.fields.iter().map(String::as_str).collect();
    if fields.is_empty()
        || fields
            .iter()
            .any(|f| !matches!(*f, "title" | "description"))
    {
        return Err(AppleIntelligenceError::new(
            "invalid_input",
            "Only the title and description can be written.",
        ));
    }
    if fields.len()
        != fields
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    {
        return Err(AppleIntelligenceError::new(
            "invalid_input",
            "A field was requested twice.",
        ));
    }
    let size = request.notes.chars().count()
        + request.title.chars().count()
        + request.description.chars().count()
        + request.context.chars().count();
    if size > MAX_INPUT_CHARS {
        return Err(AppleIntelligenceError::new(
            "too_large",
            format!("Keep the task below {MAX_INPUT_CHARS} characters for the on-device model."),
        ));
    }
    if request.notes.trim().is_empty()
        && request.title.trim().is_empty()
        && request.description.trim().is_empty()
    {
        return Err(AppleIntelligenceError::new(
            "invalid_input",
            "Write some notes first; the on-device model is not given the repository.",
        ));
    }
    Ok(size)
}

/// Turns a bridge reply into a draft, refusing one that contradicts the ask.
///
/// The framework fills every property of the generated type, so a reply always
/// carries both fields; the bridge blanks the ones that were not requested.
/// This is where that is *checked* rather than assumed, because
/// `enhancements.complete` refuses a proposal that changes a field nobody
/// asked for — and it would refuse it after the model had already run.
#[cfg(any(apple_intelligence, test))]
fn interpret(
    reply: BridgeReply,
    fields: &[String],
) -> Result<AppleIntelligenceDraft, AppleIntelligenceError> {
    if !reply.ok {
        return Err(AppleIntelligenceError::new(
            reply.code.as_deref().unwrap_or("worker_error"),
            reply
                .message
                .unwrap_or_else(|| "The on-device model could not finish.".to_string()),
        ));
    }
    let wants = |name: &str| fields.iter().any(|field| field == name);
    let mut draft = AppleIntelligenceDraft {
        rationale: reply.rationale.unwrap_or_default(),
        ..Default::default()
    };
    for (name, value) in [("title", reply.title), ("description", reply.description)] {
        let trimmed = value
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if wants(name) {
            let Some(text) = trimmed else {
                return Err(AppleIntelligenceError::new(
                    "worker_error",
                    format!("The on-device model returned no {name}."),
                ));
            };
            if name == "title" && text.chars().count() > 300 {
                return Err(AppleIntelligenceError::new(
                    "worker_error",
                    "The on-device model returned a title that is too long.",
                ));
            }
            match name {
                "title" => draft.title = Some(text),
                _ => draft.description = Some(text),
            }
        } else if trimmed.is_some() {
            return Err(AppleIntelligenceError::new(
                "worker_error",
                format!("The on-device model returned a {name} that was not requested."),
            ));
        }
    }
    Ok(draft)
}

#[cfg(apple_intelligence)]
mod bridge {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        fn gitpulse_apple_intelligence_status() -> *mut c_char;
        fn gitpulse_apple_intelligence_generate(
            request: *const c_char,
            timeout_ms: c_int,
        ) -> *mut c_char;
        fn gitpulse_apple_intelligence_free(pointer: *mut c_char);
    }

    /// Copies a bridge string out and frees the original.
    ///
    /// The Swift side allocates with `strdup`, so it must be released through
    /// the matching free — never Rust's allocator, and never left to leak on
    /// the error path, which is why the copy happens before any parsing.
    fn take(pointer: *mut c_char) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: the bridge returns either null or a NUL-terminated string it
        // allocated with `strdup`, and this is the only place that frees one.
        let owned = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { gitpulse_apple_intelligence_free(pointer) };
        Some(owned)
    }

    pub fn status() -> Option<String> {
        take(unsafe { gitpulse_apple_intelligence_status() })
    }

    pub fn generate(request: &str, timeout_ms: i32) -> Option<String> {
        let encoded = CString::new(request).ok()?;
        take(unsafe { gitpulse_apple_intelligence_generate(encoded.as_ptr(), timeout_ms) })
    }
}

/// Whether Apple Intelligence can write a task on this Mac, right now.
#[cfg(apple_intelligence)]
pub fn status() -> AppleIntelligenceStatus {
    let Some(raw) = bridge::status() else {
        return AppleIntelligenceStatus {
            compiled: true,
            state: "unavailable".to_string(),
            reason: Some("worker_error".to_string()),
            detail: "The Apple Intelligence bridge returned nothing.".to_string(),
        };
    };
    serde_json::from_str(&raw).unwrap_or_else(|error| AppleIntelligenceStatus {
        compiled: true,
        state: "unavailable".to_string(),
        reason: Some("worker_error".to_string()),
        detail: format!("The Apple Intelligence bridge returned an unreadable status: {error}"),
    })
}

#[cfg(not(apple_intelligence))]
pub fn status() -> AppleIntelligenceStatus {
    AppleIntelligenceStatus::not_compiled(if cfg!(target_os = "macos") {
        "This build has no Apple Intelligence support: it was compiled against an SDK without the Foundation Models framework."
    } else {
        "Apple Intelligence is a macOS feature."
    })
}

/// Runs one on-device generation.
#[cfg(apple_intelligence)]
pub fn generate(
    request: &AppleIntelligenceRequest,
) -> Result<AppleIntelligenceDraft, AppleIntelligenceError> {
    validate(request)?;
    let current = status();
    if !current.ready() {
        return Err(AppleIntelligenceError::new(
            current.reason.as_deref().unwrap_or("unavailable"),
            current.detail,
        ));
    }
    let encoded = serde_json::to_string(request)
        .map_err(|error| AppleIntelligenceError::new("invalid_input", error.to_string()))?;
    let timeout = i32::try_from(GENERATION_TIMEOUT.as_millis()).unwrap_or(i32::MAX);
    let raw = bridge::generate(&encoded, timeout).ok_or_else(|| {
        AppleIntelligenceError::new(
            "worker_error",
            "The Apple Intelligence bridge returned nothing.",
        )
    })?;
    let reply: BridgeReply = serde_json::from_str(&raw).map_err(|error| {
        AppleIntelligenceError::new(
            "worker_error",
            format!("The Apple Intelligence bridge returned an unreadable reply: {error}"),
        )
    })?;
    interpret(reply, &request.fields)
}

#[cfg(not(apple_intelligence))]
pub fn generate(
    request: &AppleIntelligenceRequest,
) -> Result<AppleIntelligenceDraft, AppleIntelligenceError> {
    validate(request)?;
    let current = status();
    Err(AppleIntelligenceError::new(
        current.reason.as_deref().unwrap_or("not_compiled"),
        current.detail,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AppleIntelligenceRequest {
        AppleIntelligenceRequest {
            kind: "draft".to_string(),
            fields: vec!["title".to_string(), "description".to_string()],
            notes: "the board menu cannot set a due date".to_string(),
            title: String::new(),
            description: String::new(),
            context: "Repository: GitPulse".to_string(),
        }
    }

    #[test]
    fn status_always_says_which_of_the_three_negatives_it_means() {
        let current = status();
        assert!(
            matches!(
                current.state.as_str(),
                "available" | "unavailable" | "unsupported_os"
            ),
            "unexpected state {current:?}"
        );
        assert!(
            !current.detail.trim().is_empty(),
            "every state explains itself"
        );
        assert_eq!(current.ready(), current.state == "available");
        // A build without the bridge must never look like a Mac that said no.
        if !current.compiled {
            assert_eq!(current.reason.as_deref(), Some("not_compiled"));
            assert_ne!(current.state, "unavailable");
        }
    }

    #[test]
    fn refuses_a_request_it_should_not_send() {
        for (mutate, code) in [
            (
                Box::new(|r: &mut AppleIntelligenceRequest| r.kind = "summarize".into())
                    as Box<dyn Fn(&mut AppleIntelligenceRequest)>,
                "invalid_input",
            ),
            (
                Box::new(|r: &mut AppleIntelligenceRequest| r.fields.clear()),
                "invalid_input",
            ),
            (
                Box::new(|r: &mut AppleIntelligenceRequest| r.fields = vec!["owner".into()]),
                "invalid_input",
            ),
            (
                Box::new(|r: &mut AppleIntelligenceRequest| {
                    r.fields = vec!["title".into(), "title".into()]
                }),
                "invalid_input",
            ),
            (
                Box::new(|r: &mut AppleIntelligenceRequest| {
                    r.notes = String::new();
                    r.context = "x".into();
                }),
                "invalid_input",
            ),
            (
                Box::new(|r: &mut AppleIntelligenceRequest| {
                    r.notes = "x".repeat(MAX_INPUT_CHARS + 1)
                }),
                "too_large",
            ),
        ] {
            let mut input = request();
            mutate(&mut input);
            assert_eq!(validate(&input).unwrap_err().code, code, "{input:?}");
        }
        assert!(validate(&request()).is_ok());
    }

    #[test]
    fn a_commit_subject_asks_only_for_the_title() {
        let mut input = request();
        input.kind = "commit_subject".into();
        input.fields = vec!["title".into()];
        input.notes = "Classified staged change.".into();
        input.description.clear();
        assert!(validate(&input).is_ok());
        input.fields = vec!["owner".into()];
        assert_eq!(validate(&input).unwrap_err().code, "invalid_input");
    }

    #[test]
    fn counts_characters_rather_than_bytes_so_prose_is_not_penalised() {
        let mut input = request();
        // 11 999 astral characters is 48 KB of UTF-8 and still one under the cap.
        input.notes = "𝄞".repeat(MAX_INPUT_CHARS - 12);
        input.context = "x".repeat(12);
        assert_eq!(validate(&input).unwrap(), MAX_INPUT_CHARS);
        input.context.push('x');
        assert_eq!(validate(&input).unwrap_err().code, "too_large");
    }

    fn reply(title: Option<&str>, description: Option<&str>) -> BridgeReply {
        BridgeReply {
            ok: true,
            code: None,
            message: None,
            title: title.map(str::to_string),
            description: description.map(str::to_string),
            rationale: Some("on device".to_string()),
        }
    }

    #[test]
    fn keeps_a_proposal_to_the_fields_that_were_requested() {
        let only_title = vec!["title".to_string()];
        let draft = interpret(reply(Some("Fix the menu"), None), &only_title).unwrap();
        assert_eq!(draft.title.as_deref(), Some("Fix the menu"));
        assert_eq!(draft.description, None);
        // The store refuses a proposal that changes an unrequested field, and
        // it refuses it after the model has already run. Catch it here.
        let extra = interpret(reply(Some("Fix the menu"), Some("Also this")), &only_title);
        assert_eq!(extra.unwrap_err().code, "worker_error");
        // Blank is the same as missing: `enhancements.complete` rejects an
        // empty title, so an empty one must not travel as a success.
        assert_eq!(
            interpret(reply(Some("   "), None), &only_title)
                .unwrap_err()
                .code,
            "worker_error"
        );
        assert_eq!(
            interpret(reply(None, None), &only_title).unwrap_err().code,
            "worker_error"
        );
    }

    #[test]
    fn refuses_a_title_the_store_would_refuse() {
        let fields = vec!["title".to_string()];
        let long = "t".repeat(301);
        assert_eq!(
            interpret(reply(Some(&long), None), &fields)
                .unwrap_err()
                .code,
            "worker_error"
        );
        let exact = "t".repeat(300);
        assert_eq!(
            interpret(reply(Some(&exact), None), &fields)
                .unwrap()
                .title
                .unwrap()
                .chars()
                .count(),
            300
        );
    }

    #[test]
    fn carries_the_bridge_failure_code_instead_of_flattening_it() {
        let failed = BridgeReply {
            ok: false,
            code: Some("refused".to_string()),
            message: Some("Apple Intelligence declined.".to_string()),
            title: None,
            description: None,
            rationale: None,
        };
        let error = interpret(failed, &["title".to_string()]).unwrap_err();
        assert_eq!(error.code, "refused");
        assert_eq!(error.message, "Apple Intelligence declined.");
        let bare = BridgeReply {
            ok: false,
            code: None,
            message: None,
            title: None,
            description: None,
            rationale: None,
        };
        assert_eq!(
            interpret(bare, &["title".to_string()]).unwrap_err().code,
            "worker_error"
        );
    }

    #[test]
    fn generation_matches_what_status_reports() {
        // The one invariant that holds on every host: a module that says it is
        // not ready must not also produce a draft, and one that says it is
        // ready must not fail for a reason that means "not ready".
        let current = status();
        let result = generate(&request());
        if current.ready() {
            if let Err(error) = &result {
                assert!(
                    !matches!(error.code.as_str(), "not_compiled" | "unsupported_os"),
                    "a ready host failed with {error:?}"
                );
            }
        } else {
            let error = result.expect_err("an unready host must not produce a draft");
            assert_eq!(error.code, current.reason.unwrap_or_default());
        }
    }
}
