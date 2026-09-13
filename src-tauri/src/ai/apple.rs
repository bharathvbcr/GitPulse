//! On-device task drafting through Apple's Foundation Models.
//!
//! Wraps the Swift bridge in `apple/GitPulseAppleIntelligence.swift`, which
//! `build.rs` compiles into a static archive and which sets
//! `cfg(apple_intelligence)` when it succeeds.
//!
//! # Why there are three ways to be unavailable
//!
//! "Not compiled into this binary", "this is not macOS", and "the framework is
//! here and reports a reason" are three different facts, and collapsing them
//! misinforms whoever reads the result. A user whose Mac is perfectly capable
//! but whose build lacks the bridge must not be told their hardware is
//! ineligible; a Windows user must not be told to enable Apple Intelligence.
//! [`AppleAvailability`] keeps them apart all the way to the UI.

use std::ffi::{CStr, CString};

/// How long a single on-device generation may take.
///
/// Must stay comfortably under the store's pending-enhancement lease
/// (`ENHANCEMENT_LEASE_SECONDS`, 180s in dc-store): a generation that finishes
/// after the lease expires is refused on arrival as `expired`, so the model call
/// would be spent for nothing. Measured cost of a task draft on an M-series Mac
/// is a few seconds, so this is a safety bound, not a target.
pub const GENERATION_TIMEOUT_MS: i32 = 90_000;

/// The store's pending-enhancement lease, mirrored from
/// `dc-store/src/workbench/enhancements.rs`.
const ENHANCEMENT_LEASE_SECONDS: i32 = 180;

// Checked at compile time rather than by a test: this is a relationship between
// two constants, so the build is the right place to refuse a bad one. A timeout
// at or past the lease means a slow generation is refused on arrival as
// `expired` and the model call is spent for nothing. Half the lease leaves room
// for the time between `create` (when the lease starts) and the call itself.
const _: () = assert!(GENERATION_TIMEOUT_MS <= ENHANCEMENT_LEASE_SECONDS * 1000 / 2);

/// Why on-device generation cannot run, or that it can.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AppleAvailability {
    /// Ready to generate now.
    Available,
    /// This build has no Swift bridge linked in — a fact about the binary, not
    /// about the machine. Says nothing about whether the host could support it.
    NotCompiled,
    /// Not a macOS host. Nothing the reader can change.
    UnsupportedOs {
        /// The compiled target, so the message can name it.
        os: &'static str,
    },
    /// The framework answered and gave a reason. Reported verbatim because the
    /// reasons call for different actions: an ineligible device cannot be fixed,
    /// Apple Intelligence being off is fixed in Settings, and a model still
    /// downloading fixes itself.
    Unavailable { reason: String },
}

impl AppleAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// A sentence for the reader, distinct per cause.
    pub fn explain(&self) -> String {
        match self {
            Self::Available => "On-device drafting is ready.".into(),
            Self::NotCompiled => {
                "This build of GitPulse was compiled without Apple Intelligence support.".into()
            }
            Self::UnsupportedOs { os } => {
                format!("Apple Intelligence is a macOS feature and is not available on {os}.")
            }
            Self::Unavailable { reason } => match reason.as_str() {
                "device_not_eligible" => "This Mac does not support Apple Intelligence.".into(),
                "apple_intelligence_not_enabled" => {
                    "Apple Intelligence is turned off. Enable it in System Settings.".into()
                }
                "model_not_ready" => {
                    "The on-device model is still downloading. Try again shortly.".into()
                }
                "os_too_old" => "On-device drafting needs macOS 26 or later.".into(),
                "framework_missing" => {
                    "The Foundation Models framework is unavailable on this Mac.".into()
                }
                other => format!("Apple Intelligence is unavailable ({other})."),
            },
        }
    }
}

/// One drafted task, as the model returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub rationale: String,
}

impl Draft {
    /// The store's hard limit on a proposed title.
    ///
    /// The prompt asks for 80 characters, but a guide is a preference and not a
    /// contract: a model that overshoots must not produce a completion the store
    /// refuses, because a refused completion leaves the proposal stuck in
    /// `pending` and the reader with an error instead of an outcome.
    pub const MAX_TITLE_CHARS: usize = 300;

    /// The fields to publish, or why this draft cannot be published.
    ///
    /// Mirrors the store's completion rules exactly, because breaking any of
    /// them turns a finished generation into a refused write:
    ///
    ///   - every requested field must be present and non-blank. A blank value
    ///     is the dangerous case rather than the harmless one: accepting a
    ///     proposal applies it to the task, so an empty description would erase
    ///     prose the reader wrote.
    ///   - a field that was NOT requested must be absent, or the completion is
    ///     refused outright. That is how a field lock stays a lock.
    pub fn proposal_for(
        &self,
        requested: &[String],
    ) -> Result<Vec<(&'static str, String)>, String> {
        let mut fields = Vec::new();
        for (name, value) in [("title", &self.title), ("description", &self.description)] {
            if !requested.iter().any(|field| field == name) {
                continue;
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(format!(
                    "On-device generation returned an empty {name}, which would erase the current one."
                ));
            }
            if name == "title" && value.chars().count() > Self::MAX_TITLE_CHARS {
                return Err(format!(
                    "On-device generation returned a title of {} characters; the limit is {}.",
                    value.chars().count(),
                    Self::MAX_TITLE_CHARS
                ));
            }
            fields.push((name, value.to_owned()));
        }
        if fields.is_empty() {
            return Err("The enhancement requested no fields this model can fill.".into());
        }
        Ok(fields)
    }
}

#[cfg(apple_intelligence)]
unsafe extern "C" {
    fn gitpulse_apple_availability() -> *mut std::ffi::c_char;
    fn gitpulse_apple_generate(
        instructions: *const std::ffi::c_char,
        prompt: *const std::ffi::c_char,
        timeout_ms: i32,
    ) -> *mut std::ffi::c_char;
    fn gitpulse_apple_string_free(pointer: *mut std::ffi::c_char);
}

/// Takes ownership of a string the Swift side allocated, freeing it through the
/// matching export. Returns `None` for a null pointer or invalid UTF-8.
#[cfg(apple_intelligence)]
fn take_swift_string(pointer: *mut std::ffi::c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: `pointer` came from one of the bridge's `strdup` allocations, so
    // it is NUL-terminated and must be released through the bridge's own `free`
    // export. The copy is made before freeing.
    let owned = unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .ok()
        .map(str::to_owned);
    unsafe { gitpulse_apple_string_free(pointer) };
    owned
}

/// Whether on-device generation can run right now.
pub fn availability() -> AppleAvailability {
    #[cfg(not(apple_intelligence))]
    {
        // Order matters: a non-macOS host is unsupported regardless of the
        // build, but on macOS the only thing we can truthfully say is that this
        // binary has no bridge.
        if cfg!(target_os = "macos") {
            return AppleAvailability::NotCompiled;
        }
        AppleAvailability::UnsupportedOs {
            os: std::env::consts::OS,
        }
    }
    #[cfg(apple_intelligence)]
    {
        // SAFETY: the bridge returns either null or a `strdup` string we own.
        let raw = unsafe { gitpulse_apple_availability() };
        let Some(text) = take_swift_string(raw) else {
            return AppleAvailability::Unavailable {
                reason: "bridge_returned_nothing".into(),
            };
        };
        parse_availability(&text)
    }
}

/// Parses the bridge's availability envelope.
///
/// Separate from the FFI call so it can be tested on any host, including the
/// malformed and hostile shapes a native test cannot easily produce.
pub fn parse_availability(text: &str) -> AppleAvailability {
    #[derive(serde::Deserialize)]
    struct Wire {
        available: bool,
        reason: Option<String>,
    }
    match serde_json::from_str::<Wire>(text) {
        Ok(wire) if wire.available => AppleAvailability::Available,
        Ok(wire) => AppleAvailability::Unavailable {
            reason: wire.reason.unwrap_or_else(|| "unknown".into()),
        },
        // Unparseable is not available. Failing open here would offer a feature
        // that then errors on every use.
        Err(_) => AppleAvailability::Unavailable {
            reason: "bridge_reply_unreadable".into(),
        },
    }
}

/// Parses the bridge's generation envelope into a draft or a failure reason.
pub fn parse_generation(text: &str) -> Result<Draft, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        ok: bool,
        title: Option<String>,
        description: Option<String>,
        rationale: Option<String>,
        failure: Option<String>,
    }
    let wire: Wire = serde_json::from_str(text)
        .map_err(|error| format!("The on-device bridge returned unreadable JSON: {error}"))?;
    if !wire.ok {
        return Err(wire
            .failure
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or_else(|| "On-device generation failed without a reason.".into()));
    }
    // A success carrying no title is a failure: the store refuses a proposal
    // whose fields are absent, so accepting it here would only move the error.
    let title = wire
        .title
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "On-device generation returned no title.".to_string())?;
    Ok(Draft {
        title,
        description: wire.description.unwrap_or_default(),
        rationale: wire.rationale.unwrap_or_default(),
    })
}

/// Runs one on-device generation, bounded by [`GENERATION_TIMEOUT_MS`].
///
/// Blocks the calling thread, so callers must be off the UI thread.
pub fn generate(instructions: &str, prompt: &str) -> Result<Draft, String> {
    #[cfg(not(apple_intelligence))]
    {
        let _ = (instructions, prompt);
        Err(availability().explain())
    }
    #[cfg(apple_intelligence)]
    {
        // Interior NULs cannot cross a C string boundary. Refusing here beats
        // silently truncating the prompt, which would spend a model call on
        // less context than the caller believes it sent.
        let instructions = CString::new(instructions)
            .map_err(|_| "Instructions contain an interior NUL byte.".to_string())?;
        let prompt = CString::new(prompt)
            .map_err(|_| "The prompt contains an interior NUL byte.".to_string())?;
        // SAFETY: both pointers stay alive for the call, and the reply is either
        // null or a `strdup` string this process owns.
        let raw = unsafe {
            gitpulse_apple_generate(
                instructions.as_ptr(),
                prompt.as_ptr(),
                GENERATION_TIMEOUT_MS,
            )
        };
        let text = take_swift_string(raw)
            .ok_or_else(|| "The on-device bridge returned no reply.".to_string())?;
        parse_generation(&text)
    }
}

/// Reports on-device generation availability to the frontend.
///
/// A command rather than a field on `cmd_host_platform`, because this answer
/// genuinely changes while the app runs: `model_not_ready` becomes available
/// once the download finishes, and Apple Intelligence can be switched off in
/// System Settings. A session-long cache would strand the reader on a stale no.
#[tauri::command]
pub fn cmd_apple_intelligence_status() -> AppleIntelligenceStatus {
    let state = availability();
    AppleIntelligenceStatus {
        explanation: state.explain(),
        available: state.is_available(),
        state,
    }
}

/// The availability answer plus its reader-facing sentence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppleIntelligenceStatus {
    /// True only when a generation can run right now.
    pub available: bool,
    /// Which of the distinct causes applies.
    pub state: AppleAvailability,
    /// The sentence to show, already specific to the cause.
    pub explanation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status envelope must agree with itself: `available` is derived from
    /// the same state the explanation describes, so the two cannot disagree.
    #[test]
    fn the_status_envelope_is_self_consistent() {
        let status = cmd_apple_intelligence_status();
        assert_eq!(status.available, status.state.is_available());
        assert_eq!(status.explanation, status.state.explain());
        assert!(!status.explanation.trim().is_empty());
    }

    #[test]
    fn parses_an_available_reply() {
        assert_eq!(
            parse_availability(r#"{"available":true,"reason":null}"#),
            AppleAvailability::Available
        );
    }

    /// Each framework reason must survive to the UI verbatim, because each one
    /// asks the reader to do something different.
    #[test]
    fn keeps_each_framework_reason_distinct() {
        for reason in [
            "device_not_eligible",
            "apple_intelligence_not_enabled",
            "model_not_ready",
            "os_too_old",
            "framework_missing",
        ] {
            let parsed =
                parse_availability(&format!(r#"{{"available":false,"reason":"{reason}"}}"#));
            assert_eq!(
                parsed,
                AppleAvailability::Unavailable {
                    reason: reason.into()
                }
            );
            assert!(!parsed.is_available());
        }
        // And the sentences differ, or the distinction is lost on the way out.
        let explanations: std::collections::HashSet<String> = [
            "device_not_eligible",
            "apple_intelligence_not_enabled",
            "model_not_ready",
            "os_too_old",
            "framework_missing",
        ]
        .into_iter()
        .map(|reason| {
            AppleAvailability::Unavailable {
                reason: reason.into(),
            }
            .explain()
        })
        .collect();
        assert_eq!(explanations.len(), 5, "reasons collapsed into one message");
    }

    /// The invariant this module exists to protect: a build without the bridge,
    /// a non-macOS host, and a framework refusal must never read alike.
    #[test]
    fn the_three_negatives_never_collapse() {
        let states = [
            AppleAvailability::NotCompiled,
            AppleAvailability::UnsupportedOs { os: "windows" },
            AppleAvailability::Unavailable {
                reason: "device_not_eligible".into(),
            },
        ];
        let messages: std::collections::HashSet<String> =
            states.iter().map(AppleAvailability::explain).collect();
        assert_eq!(messages.len(), 3, "distinct causes produced one message");
        for state in &states {
            assert!(!state.is_available());
        }
        // "Not compiled" is a statement about the build, so it must not blame
        // the machine or the OS.
        let not_compiled = AppleAvailability::NotCompiled.explain();
        assert!(not_compiled.contains("build"), "{not_compiled}");
        assert!(
            !not_compiled.contains("Mac does not support"),
            "{not_compiled}"
        );
    }

    #[test]
    fn unreadable_or_missing_replies_are_unavailable() {
        for text in ["", "not json", "{}", "[]", r#"{"available":"yes"}"#, "null"] {
            assert!(
                !parse_availability(text).is_available(),
                "{text} read as available"
            );
        }
    }

    /// An absent `reason` must still produce an unavailable answer rather than
    /// defaulting to available.
    #[test]
    fn an_unavailable_reply_without_a_reason_stays_unavailable() {
        assert_eq!(
            parse_availability(r#"{"available":false,"reason":null}"#),
            AppleAvailability::Unavailable {
                reason: "unknown".into()
            }
        );
    }

    fn draft(title: &str, description: &str) -> Draft {
        Draft {
            title: title.into(),
            description: description.into(),
            rationale: "because".into(),
        }
    }

    fn requested(fields: &[&str]) -> Vec<String> {
        fields.iter().map(|f| (*f).to_string()).collect()
    }

    #[test]
    fn publishes_exactly_the_requested_fields() {
        let value = draft("A title", "Some prose.");
        assert_eq!(
            value.proposal_for(&requested(&["title"])).unwrap(),
            vec![("title", "A title".to_string())]
        );
        assert_eq!(
            value.proposal_for(&requested(&["description"])).unwrap(),
            vec![("description", "Some prose.".to_string())]
        );
        assert_eq!(
            value
                .proposal_for(&requested(&["title", "description"]))
                .unwrap()
                .len(),
            2
        );
    }

    /// Accepting a proposal applies it to the task, so a blank value for a
    /// requested field would erase prose the reader wrote. It must fail loudly
    /// instead, and the store would refuse it anyway.
    #[test]
    fn refuses_a_blank_value_for_a_requested_field() {
        for (title, description, field) in [
            ("", "prose", "title"),
            ("   ", "prose", "title"),
            ("title", "", "description"),
            ("title", " \n ", "description"),
        ] {
            let error = draft(title, description)
                .proposal_for(&requested(&["title", "description"]))
                .expect_err("a blank requested field must not publish");
            assert!(error.contains(field), "{error} should name {field}");
            assert!(
                error.contains("erase"),
                "{error} should say what is at risk"
            );
        }
    }

    /// A blank value in a field nobody asked for is irrelevant — it is not sent.
    #[test]
    fn ignores_a_blank_value_in_an_unrequested_field() {
        assert_eq!(
            draft("A title", "")
                .proposal_for(&requested(&["title"]))
                .unwrap(),
            vec![("title", "A title".to_string())]
        );
    }

    /// The store refuses a title over 300 characters, and a refused completion
    /// strands the proposal in `pending`. Catch it here, where the cause can be
    /// named, and publish a failure instead.
    #[test]
    fn refuses_a_title_longer_than_the_store_allows() {
        let long = "x".repeat(Draft::MAX_TITLE_CHARS + 1);
        let error = draft(&long, "prose")
            .proposal_for(&requested(&["title"]))
            .expect_err("an over-long title must not publish");
        assert!(
            error.contains(&(Draft::MAX_TITLE_CHARS + 1).to_string()),
            "{error}"
        );

        // Exactly at the limit is allowed: the prompt asks for 80, but the guide
        // is a preference and the store's bound is the contract.
        let exact = "y".repeat(Draft::MAX_TITLE_CHARS);
        assert!(draft(&exact, "prose")
            .proposal_for(&requested(&["title"]))
            .is_ok());
    }

    /// Counted in characters, not bytes: a 300-emoji title is within the store's
    /// limit even though it is far more than 300 bytes.
    #[test]
    fn measures_the_title_in_characters_not_bytes() {
        let emoji = "🙂".repeat(Draft::MAX_TITLE_CHARS);
        assert!(emoji.len() > Draft::MAX_TITLE_CHARS);
        assert!(draft(&emoji, "prose")
            .proposal_for(&requested(&["title"]))
            .is_ok());
    }

    #[test]
    fn refuses_a_proposal_with_nothing_to_publish() {
        assert!(draft("t", "d").proposal_for(&[]).is_err());
        assert!(draft("t", "d")
            .proposal_for(&requested(&["owner"]))
            .is_err());
    }

    #[test]
    fn parses_a_generated_draft() {
        let draft = parse_generation(
            r#"{"ok":true,"title":"Gate the Dock toggle","description":"Hide it off macOS.","rationale":"The policy path is macOS-only."}"#,
        )
        .expect("a draft");
        assert_eq!(draft.title, "Gate the Dock toggle");
        assert_eq!(draft.description, "Hide it off macOS.");
        assert_eq!(draft.rationale, "The policy path is macOS-only.");
    }

    #[test]
    fn surfaces_a_failure_reason_verbatim() {
        let error = parse_generation(r#"{"ok":false,"failure":"exceeded its 90000 ms budget"}"#)
            .expect_err("a failure");
        assert!(error.contains("90000 ms"), "{error}");
    }

    #[test]
    fn a_failure_without_a_reason_still_fails() {
        for text in [r#"{"ok":false}"#, r#"{"ok":false,"failure":"  "}"#] {
            assert!(parse_generation(text).is_err(), "{text} parsed as success");
        }
    }

    /// A success with no usable title would be refused by the store anyway; the
    /// error belongs here, where it can name the cause.
    #[test]
    fn a_success_without_a_title_is_an_error() {
        for text in [
            r#"{"ok":true}"#,
            r#"{"ok":true,"title":""}"#,
            r#"{"ok":true,"title":"   "}"#,
        ] {
            assert!(parse_generation(text).is_err(), "{text} parsed as a draft");
        }
    }

    #[test]
    fn a_draft_may_omit_the_optional_prose() {
        let draft = parse_generation(r#"{"ok":true,"title":"Only a title"}"#).expect("a draft");
        assert_eq!(draft.description, "");
        assert_eq!(draft.rationale, "");
    }

    #[test]
    fn unreadable_generation_replies_are_errors() {
        for text in ["", "not json", "[]"] {
            assert!(parse_generation(text).is_err(), "{text} parsed as a draft");
        }
    }

    /// On a host with the bridge compiled in, availability must resolve to a
    /// real answer rather than a bridge-level fault.
    #[cfg(apple_intelligence)]
    #[test]
    fn the_compiled_bridge_answers() {
        let state = availability();
        assert!(
            !matches!(&state, AppleAvailability::Unavailable { reason }
                if reason == "bridge_returned_nothing" || reason == "bridge_reply_unreadable"),
            "the linked bridge failed to answer: {state:?}"
        );
        assert!(!matches!(state, AppleAvailability::NotCompiled));
    }

    #[cfg(not(apple_intelligence))]
    #[test]
    fn a_bridge_less_build_says_so_without_blaming_the_host() {
        let state = availability();
        if cfg!(target_os = "macos") {
            assert_eq!(state, AppleAvailability::NotCompiled);
        } else {
            assert!(matches!(state, AppleAvailability::UnsupportedOs { .. }));
        }
        assert!(generate("i", "p").is_err());
    }
}
