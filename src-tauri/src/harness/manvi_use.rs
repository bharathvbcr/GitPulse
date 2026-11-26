//! Manvi's own choice of what it runs on: `manvi use`.
//!
//! Whether Manvi runs on the cloud (Claude through the Anthropic API) or on a
//! local model server is one setting in Manvi's user config, shared by every
//! program that starts Manvi. GitPulse keeps no copy of it: it reads the answer
//! (`manvi use --json`) whenever it needs it, maps it to the [`ModelSelection`]
//! the profile worker is spawned with, and writes it only through the explicit
//! writer ([`write`], behind `cmd_manvi_use`), which a person triggers.
//!
//! The status carries no credential state. Whether a Claude key or an
//! Anthropic account exists is GitPulse's own check
//! (`cmd_claude_key_status`), made where the key is read.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::sidecar::{manvi_probe_command, resolve_binary, ModelSelection};

/// How long `manvi use` may take: it reads one config file.
const USE_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest status read. The real answer is under 1 KiB.
const USE_OUTPUT_CAP: usize = 64 * 1024;
/// Longest local model id GitPulse accepts (Manvi allows more).
const MAX_LOCAL_MODEL: usize = 128;
/// Longest base URL GitPulse accepts.
const MAX_BASE_URL: usize = 512;

/// `manvi use --json`, as GitPulse reads it. A host contract that only grows:
/// unknown fields are ignored, the fields below are required.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UseStatus {
    pub mode: String,
    pub provider: String,
    pub provider_from: String,
    #[serde(default)]
    pub user_config: String,
    #[serde(default)]
    pub shadowed_by: Vec<String>,
    pub model: String,
    #[serde(default)]
    pub model_from: String,
    pub local: UseLocal,
    pub cloud: UseCloud,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UseLocal {
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UseCloud {
    pub model: String,
}

/// What the frontend is told. Flat on purpose: one object, no variants to
/// narrow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManviStatus {
    /// `ready` (the fields below are Manvi's answer), `not_chosen` (Manvi
    /// answered, but its mode is the built-in default nobody chose, so no
    /// selection is offered), `not_installed`, `needs_update` (a Manvi without
    /// `manvi use`) or `failed`.
    pub state: String,
    /// Why the state is not `ready`; empty when it is.
    pub detail: String,
    /// `cloud`, `local` or `other`.
    pub mode: String,
    pub provider: String,
    pub provider_from: String,
    /// The model Manvi would run in this mode, and the layer that set it.
    pub model: String,
    pub model_from: String,
    /// Environment variables that override the saved choice for this process.
    pub shadowed_by: Vec<String>,
    pub user_config: String,
    pub local_base_url: String,
    pub local_model: String,
    pub cloud_model: String,
    pub warnings: Vec<String>,
    /// Why GitPulse cannot use this mode (a provider it does not support, a
    /// local server that is not on this machine, no model chosen); empty when
    /// `selection` is set.
    pub unsupported: String,
    /// The model selection GitPulse spawns the worker with, in the wire shape
    /// the task enhancement methods take. `None` when `unsupported`.
    pub selection: Option<Value>,
    /// The same selection, typed, for the host's own use; never serialized.
    #[serde(skip)]
    pub(crate) typed: Option<ModelSelection>,
}

impl ManviStatus {
    fn unavailable(state: &str, detail: impl Into<String>) -> Self {
        Self {
            state: state.into(),
            detail: detail.into(),
            ..Self::default()
        }
    }
}

/// The wire shape of a selection (what `selectionWire` sends for it).
pub(crate) fn selection_wire(selection: &ModelSelection) -> Value {
    match selection {
        ModelSelection::Claude { model } => json!({"provider": "anthropic", "model": model}),
        ModelSelection::Local { base_url, model } => {
            json!({"provider": "local", "base_url": base_url, "model": model})
        }
    }
}

/// Maps Manvi's mode to the selection a worker is spawned with: cloud to
/// Claude, local to a loopback server, anything else is not supported yet.
pub(crate) fn selection_for(status: &UseStatus) -> Result<ModelSelection, String> {
    match status.mode.as_str() {
        "cloud" => {
            let model = status.model.trim();
            if !is_claude_model(model) {
                return Err(format!(
                    "Manvi's cloud model `{model}` is not a Claude model id GitPulse can use."
                ));
            }
            Ok(ModelSelection::Claude {
                model: model.to_string(),
            })
        }
        "local" => {
            let model = status.model.trim();
            if model.is_empty() {
                return Err(
                    "Manvi is on a local model but none is chosen. Pick one in the Manvi pane."
                        .into(),
                );
            }
            check_local_model(model)?;
            let endpoint =
                crate::ai::http::parse_base_url(&status.local.base_url).map_err(|e| {
                    format!(
                        "Manvi's local server {} cannot be used by GitPulse: {e}",
                        status.local.base_url
                    )
                })?;
            Ok(ModelSelection::Local {
                base_url: endpoint.base_url(),
                model: model.to_string(),
            })
        }
        other => Err(format!(
            "Manvi is set to {} ({other}), which GitPulse does not support yet. \
             Run `manvi use cloud` or `manvi use local`.",
            status.provider
        )),
    }
}

pub(crate) fn is_claude_model(model: &str) -> bool {
    model.strip_prefix("claude-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 120
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    })
}

fn check_local_model(model: &str) -> Result<(), String> {
    if model.is_empty() || model.len() > MAX_LOCAL_MODEL {
        return Err(format!("A local model id is 1 to {MAX_LOCAL_MODEL} bytes."));
    }
    if model
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '#' | '"' | '\''))
    {
        return Err("A local model id has no spaces, control characters, quotes or #.".into());
    }
    Ok(())
}

/// Parses `manvi use --json` strictly: complete, within the cap, a known mode
/// and every required field present with the right type.
pub(crate) fn parse(stdout: &[u8]) -> Result<UseStatus, String> {
    if stdout.len() > USE_OUTPUT_CAP {
        return Err(format!("the answer exceeds {USE_OUTPUT_CAP} bytes"));
    }
    let status: UseStatus =
        serde_json::from_slice(stdout).map_err(|e| format!("the answer is not a status: {e}"))?;
    if !matches!(status.mode.as_str(), "cloud" | "local" | "other") {
        return Err(format!("unknown mode `{}`", status.mode));
    }
    Ok(status)
}

/// The layer `provider_from` names when nothing chose the mode: Manvi's
/// built-in default (cloud), with no saved config, environment or flag.
const PROVIDER_FROM_DEFAULT: &str = "default";

/// What a person reads while Manvi's mode is its built-in default.
pub(crate) const NOT_CHOSEN_DETAIL: &str = "Manvi's model has not been chosen yet. Choose Local model or Claude API in the Manvi pane (or run `manvi use local` or `manvi use cloud`) to send GitPulse's AI requests through Manvi.";

/// The frontend's view of a parsed status.
///
/// A mode nobody chose (`provider_from: default`) is `not_chosen`, never
/// `ready`: Manvi's built-in default is cloud, and following it would send
/// diffs to the Claude API for a person who never picked it. The fields still
/// say what Manvi would run, but no selection is offered, so every reader
/// falls back to GitPulse's own loopback client and its consent dialog.
pub(crate) fn present(status: &UseStatus) -> ManviStatus {
    if status.provider_from == PROVIDER_FROM_DEFAULT {
        return ManviStatus {
            state: "not_chosen".into(),
            detail: NOT_CHOSEN_DETAIL.into(),
            ..fields(status)
        };
    }
    let (typed, selection, unsupported) = match selection_for(status) {
        Ok(selection) => (
            Some(selection.clone()),
            Some(selection_wire(&selection)),
            String::new(),
        ),
        Err(why) => (None, None, why),
    };
    ManviStatus {
        state: "ready".into(),
        unsupported,
        selection,
        typed,
        ..fields(status)
    }
}

/// Whether `status` represents Manvi's built-in default cloud mode
/// (`provider_from == "default"`, `mode == "cloud"`).
pub(crate) fn is_default_cloud(status: &ManviStatus) -> bool {
    status.state == "not_chosen"
        && status.mode == "cloud"
        && status.provider_from == PROVIDER_FROM_DEFAULT
}

/// Promotes Manvi's built-in default cloud status into a `ready` Claude
/// selection for callers (such as `harness::steward`) that verified a valid
/// `~/.anthropic-accounts` account or GitPulse Keychain Claude key.
pub(crate) fn promote_default_cloud(
    mut status: ManviStatus,
    credential_detail: String,
) -> ManviStatus {
    let model = status.model.trim().to_string();
    if !is_claude_model(&model) {
        status.state = "ready".into();
        status.detail.clear();
        status.unsupported =
            format!("Manvi's cloud model `{model}` is not a Claude model id GitPulse can use.");
        status.selection = None;
        status.typed = None;
        return status;
    }
    let sel = ModelSelection::Claude { model };
    status.selection = Some(selection_wire(&sel));
    status.typed = Some(sel);
    status.state = "ready".into();
    status.unsupported.clear();
    status.detail = credential_detail;
    status
}

/// What Manvi reported, as shown; no state, selection or verdict.
fn fields(status: &UseStatus) -> ManviStatus {
    ManviStatus {
        state: String::new(),
        detail: String::new(),
        mode: status.mode.clone(),
        provider: status.provider.clone(),
        provider_from: status.provider_from.clone(),
        model: status.model.clone(),
        model_from: status.model_from.clone(),
        shadowed_by: status.shadowed_by.clone(),
        user_config: status.user_config.clone(),
        local_base_url: status.local.base_url.clone(),
        local_model: status.local.model.clone(),
        cloud_model: status.cloud.model.clone(),
        warnings: status.warnings.clone(),
        unsupported: String::new(),
        selection: None,
        typed: None,
    }
}

const UPDATE_HINT: &str = "Update Manvi to choose its model from GitPulse.";

/// The outcome of one `manvi use` run, before interpretation.
pub(crate) struct UseRun {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Interprets a run: a Manvi without `use` (it exits non-zero, or answers
/// something that is not a status) is `needs_update`, never a guess.
pub(crate) fn interpret(run: UseRun) -> ManviStatus {
    if !run.success {
        let first = run.stderr.lines().next().unwrap_or("").trim();
        return ManviStatus::unavailable(
            "needs_update",
            format!("This Manvi has no working `manvi use --json` ({first}). {UPDATE_HINT}"),
        );
    }
    match parse(&run.stdout) {
        Ok(status) => present(&status),
        Err(why) => ManviStatus::unavailable(
            "needs_update",
            format!("`manvi use --json` did not answer as expected ({why}). {UPDATE_HINT}"),
        ),
    }
}

fn run_use_checked(binary: &str, args: &[&str]) -> Result<UseRun, crate::ipc_error::CommandError> {
    let probe = manvi_probe_command(binary, args);
    let run = crate::engine::process::run_bounded_capped(
        probe,
        "manvi use",
        USE_TIMEOUT,
        None,
        USE_OUTPUT_CAP,
    )?;
    // Output past the cap is a prefix, never a status.
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    if run.incomplete.is_some() {
        return Ok(UseRun {
            success: false,
            stdout: Vec::new(),
            stderr: "its answer was cut short".into(),
        });
    }
    Ok(UseRun {
        success: run.success,
        stdout: run.stdout,
        stderr,
    })
}

fn run_use(binary: &str, args: &[&str]) -> Result<UseRun, String> {
    run_use_checked(binary, args).map_err(|e| e.message().to_string())
}

/// Reads Manvi's choice, preserving typed spawn-gate errors (such as
/// [`crate::ipc_error::IpcErrorKind::DeferredUnderLoad`]) so callers like the
/// Manvi steward never mistake a load-deferred probe for an unconfigured Manvi.
pub(crate) fn try_read() -> Result<ManviStatus, crate::ipc_error::CommandError> {
    match resolve_binary() {
        None => Ok(ManviStatus::unavailable(
            "not_installed",
            "Manvi is not installed, so its model choice cannot be read.",
        )),
        Some(binary) => run_use_checked(&binary, &["use", "--json"]).map(interpret),
    }
}

/// Reads Manvi's choice with the binary GitPulse resolves.
pub fn read() -> ManviStatus {
    match resolve_binary() {
        None => ManviStatus::unavailable(
            "not_installed",
            "Manvi is not installed, so its model choice cannot be read.",
        ),
        Some(binary) => read_from(&binary),
    }
}

pub(crate) fn read_from(binary: &str) -> ManviStatus {
    match run_use(binary, &["use", "--json"]) {
        Ok(run) => interpret(run),
        Err(why) => ManviStatus::unavailable("failed", format!("Could not ask Manvi: {why}")),
    }
}

/// A request to change Manvi's choice. Built only by [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UseChange {
    pub mode: &'static str,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

/// Checks a writer request before anything is spawned: a mode of `cloud` or
/// `local`; a cloud model that is a `claude-` id and no base URL; a local base
/// URL that is on this machine and a local model id of a sane shape.
pub(crate) fn validate(
    mode: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<UseChange, String> {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    match mode {
        "cloud" => {
            if base_url.is_some() {
                return Err("A cloud choice takes no base URL.".into());
            }
            if let Some(model) = model {
                if !is_claude_model(model) {
                    return Err(format!("`{model}` is not a Claude model id (claude-…)."));
                }
            }
            Ok(UseChange {
                mode: "cloud",
                model: model.map(str::to_string),
                base_url: None,
            })
        }
        "local" => {
            if let Some(model) = model {
                check_local_model(model)?;
            }
            let base_url = match base_url {
                None => None,
                Some(raw) => {
                    if raw.is_empty() || raw.len() > MAX_BASE_URL || raw != raw.trim() {
                        return Err("The local base URL is empty, padded or too long.".into());
                    }
                    if raw
                        .chars()
                        .any(|c| c.is_control() || c == '#' || c == '"' || c == '\'')
                    {
                        return Err("The local base URL holds a character it cannot.".into());
                    }
                    let endpoint = crate::ai::http::parse_base_url(raw)
                        .map_err(|e| format!("The local base URL is refused: {e}"))?;
                    Some(endpoint.base_url())
                }
            };
            Ok(UseChange {
                mode: "local",
                model: model.map(str::to_string),
                base_url,
            })
        }
        other => Err(format!("`{other}` is not a mode; use cloud or local.")),
    }
}

/// Writes Manvi's choice (`manvi use cloud|local …`) and returns what it now
/// reads. Human-initiated: the only caller is `cmd_manvi_use`.
pub fn write(
    mode: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<ManviStatus, String> {
    let change = validate(mode, model, base_url)?;
    let binary = resolve_binary().ok_or("Manvi is not installed.")?;
    let mut args: Vec<&str> = vec!["use", change.mode];
    if let Some(model) = &change.model {
        args.extend(["--model", model]);
    }
    if let Some(base_url) = &change.base_url {
        args.extend(["--base-url", base_url]);
    }
    args.push("--json");
    let run = run_use(&binary, &args)?;
    if !run.success {
        let why = run.stderr.trim();
        return Err(if why.is_empty() {
            "Manvi refused the change.".into()
        } else {
            why.to_string()
        });
    }
    match interpret(run) {
        status if status.state == "ready" => Ok(status),
        status => Err(status.detail),
    }
}

#[cfg(test)]
mod manvi_use_tests {
    use super::*;

    const CLOUD: &str = r#"{"mode":"cloud","provider":"anthropic","provider_from":"config","user_config":"/home/u/.config/manvi/config.toml","shadowed_by":[],"model":"claude-sonnet-5-5","model_from":"config","local":{"base_url":"http://127.0.0.1:11434/v1","base_url_from":"default","model":"qwen","model_from":"config"},"cloud":{"model":"claude-sonnet-5-5","model_from":"config"},"warnings":[],"future_field":1}"#;

    fn with(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut value: Value = serde_json::from_str(CLOUD).unwrap();
        edit(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn a_complete_status_parses_and_unknown_fields_are_ignored() {
        let status = parse(CLOUD.as_bytes()).unwrap();
        assert_eq!(status.mode, "cloud");
        assert_eq!(status.local.model, "qwen");
        assert_eq!(status.cloud.model, "claude-sonnet-5-5");
    }

    #[test]
    fn a_missing_required_field_or_wrong_type_is_refused() {
        for field in [
            "mode",
            "provider",
            "provider_from",
            "model",
            "local",
            "cloud",
        ] {
            let bytes = with(|v| {
                v.as_object_mut().unwrap().remove(field);
            });
            assert!(parse(&bytes).is_err(), "{field} is required");
        }
        assert!(parse(&with(|v| v["local"]["base_url"] = json!(3))).is_err());
        assert!(parse(&with(|v| v["shadowed_by"] = json!("MANVI_X"))).is_err());
        assert!(parse(&with(|v| v["mode"] = json!("hybrid"))).is_err());
        assert!(parse(b"").is_err());
        assert!(parse(b"manvi: unknown command \"use\"").is_err());
    }

    #[test]
    fn an_oversized_answer_is_refused() {
        let mut big = CLOUD.as_bytes().to_vec();
        big.extend(vec![b' '; USE_OUTPUT_CAP]);
        assert!(parse(&big).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_manvi_without_use_is_needs_update_never_a_guess() {
        let refused = interpret(UseRun {
            success: false,
            stdout: Vec::new(),
            stderr: "manvi: unknown command \"use\"\nusage: ...".into(),
        });
        assert_eq!(refused.state, "needs_update");
        assert!(refused.detail.contains("unknown command"));
        assert!(refused.detail.contains("Update Manvi"));
        assert_eq!(refused.selection, None);

        let junk = interpret(UseRun {
            success: true,
            stdout: b"hello".to_vec(),
            stderr: String::new(),
        });
        assert_eq!(junk.state, "needs_update");
    }

    #[test]
    fn mode_maps_to_the_selection_the_worker_spawns_with() {
        let cloud = parse(CLOUD.as_bytes()).unwrap();
        assert_eq!(
            selection_for(&cloud).unwrap(),
            ModelSelection::Claude {
                model: "claude-sonnet-5-5".into()
            }
        );
        let local = parse(&with(|v| {
            v["mode"] = json!("local");
            v["provider"] = json!("local");
            v["model"] = json!("qwen3");
        }))
        .unwrap();
        assert_eq!(
            selection_for(&local).unwrap(),
            ModelSelection::Local {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen3".into()
            }
        );
        let wire = present(&local).selection.unwrap();
        assert_eq!(wire["provider"], "local");
        assert_eq!(wire["base_url"], "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn a_mode_nobody_chose_offers_no_selection_but_still_shows_what_manvi_would_run() {
        let unchosen = parse(&with(|v| {
            v["provider_from"] = json!("default");
            v["model_from"] = json!("default");
        }))
        .unwrap();
        let shown = present(&unchosen);
        assert_eq!(shown.state, "not_chosen");
        assert_eq!(shown.detail, NOT_CHOSEN_DETAIL);
        assert_eq!(shown.selection, None);
        assert_eq!(shown.typed, None);
        assert!(shown.unsupported.is_empty());
        assert_eq!(
            (shown.mode.as_str(), shown.model.as_str()),
            ("cloud", "claude-sonnet-5-5")
        );
        assert_eq!(shown.provider_from, "default");

        // Every other layer is a choice: the saved config, the environment
        // and a flag each keep the selection.
        for from in ["config", "env", "flag"] {
            let chosen = parse(&with(|v| v["provider_from"] = json!(from))).unwrap();
            let shown = present(&chosen);
            assert_eq!(shown.state, "ready", "{from}");
            assert!(shown.typed.is_some(), "{from}");
        }
    }

    #[test]
    fn modes_gitpulse_cannot_use_say_so() {
        let other = parse(&with(|v| {
            v["mode"] = json!("other");
            v["provider"] = json!("gemini");
        }))
        .unwrap();
        let why = selection_for(&other).unwrap_err();
        assert!(
            why.contains("gemini") && why.contains("does not support"),
            "{why}"
        );
        let shown = present(&other);
        assert_eq!(shown.state, "ready");
        assert_eq!(shown.selection, None);
        assert!(!shown.unsupported.is_empty());

        // A cloud model that is not a Claude id, a remote local server and an
        // unset local model are each refused with their own reason.
        let not_claude = parse(&with(|v| v["model"] = json!("gpt-5"))).unwrap();
        assert!(selection_for(&not_claude).is_err());
        let remote = parse(&with(|v| {
            v["mode"] = json!("local");
            v["model"] = json!("m");
            v["local"]["base_url"] = json!("https://api.example.com/v1");
        }))
        .unwrap();
        assert!(selection_for(&remote)
            .unwrap_err()
            .contains("cannot be used"));
        let unset = parse(&with(|v| {
            v["mode"] = json!("local");
            v["model"] = json!("");
        }))
        .unwrap();
        assert!(selection_for(&unset)
            .unwrap_err()
            .contains("none is chosen"));
    }

    #[test]
    fn the_writer_accepts_only_well_shaped_requests() {
        let ok = validate("cloud", Some("claude-sonnet-5-5"), None).unwrap();
        assert_eq!(
            (ok.mode, ok.model.as_deref(), ok.base_url),
            ("cloud", Some("claude-sonnet-5-5"), None)
        );
        assert!(validate("cloud", None, None).is_ok());
        let local = validate("local", Some("qwen3:8b"), Some("http://localhost:11434/v1")).unwrap();
        assert_eq!(local.base_url.as_deref(), Some("http://localhost:11434/v1"));

        for bad in [
            validate("cloud", Some("gpt-5"), None),
            validate("cloud", Some("claude-"), None),
            validate("cloud", Some("claude-Opus"), None),
            validate("cloud", Some("claude-x y"), None),
            validate("cloud", None, Some("http://127.0.0.1:1/v1")),
            validate("local", Some("a b"), None),
            validate("local", Some("a#b"), None),
            validate("local", Some(&"m".repeat(129)), None),
            validate("local", None, Some("https://api.example.com/v1")),
            validate("local", None, Some("http://10.0.0.5:11434/v1")),
            validate("local", None, Some("http://127.0.0.1:1/v1\n--flag")),
            validate("local", None, Some(" http://127.0.0.1:1/v1")),
            validate("local", None, Some("")),
            validate("other", None, None),
            validate("", None, None),
            validate("--help", None, None),
        ] {
            assert!(bad.is_err(), "{bad:?}");
        }
    }

    /// The real process path: a stand-in `manvi` that answers `use --json`.
    #[cfg(unix)]
    #[test]
    fn read_from_runs_the_binary_and_reports_a_manvi_without_use() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        let good = script("good", &format!("cat <<'EOF'\n{CLOUD}\nEOF"));
        let shown = read_from(&good);
        assert_eq!(shown.state, "ready");
        assert_eq!(shown.mode, "cloud");

        let old = script("old", "echo 'manvi: unknown command \"use\"' >&2; exit 2");
        assert_eq!(read_from(&old).state, "needs_update");

        assert_eq!(read_from("/nonexistent/manvi").state, "failed");
    }
}
