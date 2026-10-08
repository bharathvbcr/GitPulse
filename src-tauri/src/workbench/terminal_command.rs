//! Provider arguments and one temporary source file for an explicitly requested
//! terminal attempt. User task text never becomes shell syntax or an argv blob.

use super::WorkbenchError;
use crate::engine::git_cli::{
    capture_command, extended_child_path, resolve_spawn_program_with, CapturedOutput,
};
use crate::tool_config::ModelChoice;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// The brief, alone in a private directory of its own.
///
/// A directory rather than a file in the shared temp root because that
/// directory is what the agent is granted (`--add-dir`): Claude Code confines
/// its file tools to the working directories, so a brief outside the checkout
/// was a read the agent had to ask for — or, under `dontAsk`, was refused —
/// before it knew what the task was. Granting the temp root itself would hand
/// it every other program's scratch files.
pub(super) struct BriefFile {
    pub dir: PathBuf,
    pub path: PathBuf,
    _file: File,
}

impl BriefFile {
    pub fn create(markdown: &str) -> Result<Self, WorkbenchError> {
        Self::under(&std::env::temp_dir(), markdown)
    }
    fn under(root: &Path, markdown: &str) -> Result<Self, WorkbenchError> {
        if markdown.is_empty() || markdown.len() > 2 * 1024 * 1024 {
            return Err(error(
                "invalid_input",
                "The task brief is empty or exceeds 2 MiB.",
            ));
        }
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| error("file_error", e.to_string()))?
            .as_nanos();
        let dir = root.join(format!(
            "{BRIEF_DIR_PREFIX}{}-{tick}-{}",
            std::process::id(),
            FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        // Not recursive: an existing directory at this name is refused rather
        // than adopted, so a planted one cannot become the agent's grant.
        builder
            .create(&dir)
            .map_err(|e| error("file_error", e.to_string()))?;
        let path = dir.join(BRIEF_FILE_NAME);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) => {
                let _ = std::fs::remove_dir(&dir);
                return Err(error("file_error", e.to_string()));
            }
        };
        // From here `Drop` owns the cleanup of both the file and the directory.
        let mut brief = Self {
            dir,
            path,
            _file: file,
        };
        brief
            ._file
            .write_all(markdown.as_bytes())
            .and_then(|()| brief._file.sync_all())
            .map_err(|e| error("file_error", e.to_string()))?;
        Ok(brief)
    }
}
/// The directory-name prefix [`BriefFile::under`] writes.
const BRIEF_DIR_PREFIX: &str = "gitpulse-task-";
const BRIEF_FILE_NAME: &str = "task-brief.md";
/// Bounds one sweep of a shared temp root.
const MAX_BRIEF_DIRS_PER_SWEEP: usize = 1024;

/// `gitpulse-task-{pid}-{nanos}-{seq}`: the GitPulse process that wrote a
/// brief directory, and when.
fn brief_owner(name: &str) -> Option<(u32, u128)> {
    let mut parts = name.strip_prefix(BRIEF_DIR_PREFIX)?.split('-');
    let pid = parts.next()?.parse::<u32>().ok().filter(|p| *p > 0)?;
    let stamp = parts.next()?.parse::<u128>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((pid, stamp))
}

/// Removes brief directories whose writer is provably gone.
///
/// [`BriefFile`]'s `Drop` is the normal cleanup, and a GitPulse that crashed,
/// was force-quit or SIGKILLed never runs it — every brief it had handed an
/// agent stays in the temp root, holding task text, until the OS clears it.
/// The directory's name records its writer's pid and the instant it was
/// written, which is exactly what `owner_since` judges: `Gone` means no
/// process with that pid had started by then, so the writer is dead (a pid
/// reused since is a later process and does not count). `Alive` and `Unknown`
/// keep the directory; a check that could not run never deletes.
///
/// Only the exact shape `BriefFile` makes is removed: a real directory (not a
/// symlink), owned by this user, holding nothing but the brief. Anything else
/// at a matching name is left, and logged, rather than recursed into. Returns
/// how many directories were removed.
pub(super) fn remove_abandoned_briefs(
    root: &Path,
    owner_since: impl Fn(u32, u128) -> super::process_birth::Liveness,
) -> usize {
    use super::process_birth::Liveness;
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            log::warn!(target: "workbench", "could not scan {} for abandoned task briefs: {error}", root.display());
            return 0;
        }
    };
    let mut removed = 0;
    let mut considered = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, stamp)) = name.to_str().and_then(brief_owner) else {
            continue;
        };
        considered += 1;
        if considered > MAX_BRIEF_DIRS_PER_SWEEP {
            log::warn!(target: "workbench", "more than {MAX_BRIEF_DIRS_PER_SWEEP} task brief directories in {}; the rest wait for the next sweep", root.display());
            break;
        }
        match owner_since(pid, stamp) {
            Liveness::Gone => {}
            Liveness::Alive => continue,
            Liveness::Unknown(reason) => {
                log::info!(target: "workbench", "kept task brief {}: could not check its writer (pid {pid}): {reason}", name.to_string_lossy());
                continue;
            }
        }
        let dir = entry.path();
        match remove_brief_dir(&dir) {
            Ok(()) => removed += 1,
            Err(why) => {
                log::warn!(target: "workbench", "kept abandoned task brief {}: {why}", dir.display());
            }
        }
    }
    removed
}

fn remove_brief_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if !meta.file_type().is_dir() {
        return Err("it is not a directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err("it belongs to another user".into());
        }
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if entry.file_name() != BRIEF_FILE_NAME || !kind.is_file() {
            return Err(format!(
                "it holds {:?}, which a task brief directory never does",
                entry.file_name()
            ));
        }
    }
    match std::fs::remove_file(dir.join(BRIEF_FILE_NAME)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    std::fs::remove_dir(dir).map_err(|e| e.to_string())
}

impl Drop for BriefFile {
    fn drop(&mut self) {
        for (what, result) in [
            ("brief", std::fs::remove_file(&self.path)),
            ("brief directory", std::fs::remove_dir(&self.dir)),
        ] {
            if let Err(error) = result {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(target: "workbench", "could not remove the temporary task {what}: {error}");
                }
            }
        }
    }
}

/// Each agent CLI's own documented, **session-scoped** notification override:
/// the flags that make it speak to a GitPulse terminal at all.
///
/// Neither Claude Code nor Codex says anything here by default. Claude Code
/// sends a desktop notification only in Ghostty, kitty and iTerm2 and is
/// otherwise silent unless `preferredNotifChannel` is set; Codex probes for a
/// terminal it recognises and falls back to nothing it can be sure of. So an
/// agent stopped on a permission prompt in a hidden tab with no sign at all.
///
/// * `claude --settings '<json>'` sits above the user's files and below
///   managed settings, merges key by key — a key not named here keeps its
///   value from wherever it was set — lasts one session and writes no file.
///   Exactly one key, so nothing the user set is overridden.
/// * `codex -c key=value` is parsed as TOML for that invocation. The inner
///   quotes are TOML, not shell: these are argv entries and nothing expands
///   them. `notification_condition = "always"` because Codex can only guess
///   at focus from escape sequences, while GitPulse knows which tab is on
///   screen and decides itself.
///
/// Launchers absent here get nothing: Grok, Antigravity and Manvi publish no
/// notification setting this code has read, and an invented flag would at
/// best be ignored and at worst refuse to start. Their own bells and OSC
/// notifications still reach the user, because detection does not depend on
/// this.
///
/// This is the only copy. Both launch paths — a plain agent tab
/// ([`with_notify_flags`], from `cmd_terminal_spawn`) and a task attempt
/// ([`arguments`]) — read it, so the task lane can no longer be the one that
/// forgot, which it was.
pub(crate) fn notify_flags(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["--settings", r#"{"preferredNotifChannel":"terminal_bell"}"#],
        "codex" => &[
            "-c",
            "tui.notifications=true",
            "-c",
            r#"tui.notification_method="osc9""#,
            "-c",
            r#"tui.notification_condition="always""#,
        ],
        _ => &[],
    }
}

/// Claude Code's `--setting-sources`, when the user narrowed which of its
/// settings files an agent loads (`tool_config::AgentDefaults`). Nothing
/// otherwise, and nothing for any other CLI: none of them has the flag.
pub(crate) fn setting_source_flags(provider: &str, sources: Option<&str>) -> Vec<String> {
    match (provider, sources) {
        ("claude", Some(sources)) => vec!["--setting-sources".to_owned(), sources.to_owned()],
        _ => Vec::new(),
    }
}

/// The model controls a [`ModelChoice`] can carry, in the order a settings
/// row shows them. Read by `agentDefaults.contract.test.ts`, which fails if
/// the frontend's list drifts from this one.
pub(crate) const MODEL_FIELDS: [&str; 4] = ["model", "effort", "fallback", "advisor"];

/// Reasoning-effort levels, least first. Both CLIs that take `--effort`
/// (Claude Code 2.1.289, Antigravity 1.2.17) list exactly these in their
/// `--help`, and [`advertises`] proves the chosen level against the build's
/// own list before a task launch — a build that offers fewer refuses rather
/// than reading an unknown level however it likes.
pub(crate) const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The most fallback models one launch may name. Claude Code tries them in
/// order on each overloaded turn; a longer list is a typo, not a plan.
pub(crate) const MAX_FALLBACK_MODELS: usize = 4;

/// The longest model id accepted. Real ids — aliases, `opus[1m]`, dated
/// full names, Bedrock inference-profile ARNs — are well under this.
pub(crate) const MAX_MODEL_ID_LEN: usize = 160;

/// Flags whose *value* a build must list in that flag's own `--help` block,
/// not merely the flag. Only flags with a closed, advertised value set
/// belong here: a model id is open-ended and no help screen lists them all.
const VALUE_PROVEN_FLAGS: [&str; 1] = ["--effort"];

/// How one model control reaches one CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelControl {
    /// An option followed by its value.
    Flag(&'static str),
    /// A key in Claude Code's session-scoped `--settings` JSON.
    ClaudeSetting(&'static str),
}

/// Which model control means what for which CLI. The only copy: the settings
/// panel's per-launcher fields ([`model_fields`]), the save-time validation
/// and the launch argv are all derived from it.
///
/// Every arm was read off the installed CLI rather than recalled:
///
/// * `claude` 2.1.289 — `--model`, `--effort`, `--fallback-model` (one
///   comma-separated value) are in `--help`. The advisor is the settings key
///   `advisorModel` ("Advisor model for the server-side advisor tool", in the
///   binary's settings schema). The CLI also has `--advisor <model>`, but it
///   is hidden from `--help`, and a task launch passes only flags the build
///   can be *shown* to advertise; `--settings` is advertised.
/// * `agy` 1.2.17 — `--model` (a slug from `agy models`) and `--effort`.
/// * `codex` 0.153.4 and `grok` 1.0.46 — `--model` only. Both have some
///   reasoning-effort control, but neither lists its accepted values, so a
///   level could not be proved before launch and is not offered.
fn model_control(provider: &str, field: &str) -> Option<ModelControl> {
    match (provider, field) {
        ("claude" | "agy" | "codex" | "grok", "model") => Some(ModelControl::Flag("--model")),
        ("claude" | "agy", "effort") => Some(ModelControl::Flag("--effort")),
        ("claude", "fallback") => Some(ModelControl::Flag("--fallback-model")),
        ("claude", "advisor") => Some(ModelControl::ClaudeSetting("advisorModel")),
        _ => None,
    }
}

/// The model fields `provider` takes, in [`MODEL_FIELDS`] order. Derived from
/// [`model_control`], so a field a CLI gains or loses changes the settings
/// panel without a second list to update.
pub(crate) fn model_fields(provider: &str) -> Vec<&'static str> {
    MODEL_FIELDS
        .iter()
        .copied()
        .filter(|field| model_control(provider, field).is_some())
        .collect()
}

/// Every launcher that takes at least one model control, with its fields.
pub(crate) fn model_launchers() -> std::collections::BTreeMap<String, Vec<String>> {
    crate::terminal::AGENT_LAUNCHERS
        .iter()
        .filter(|launcher| is_terminal_provider(launcher))
        .map(|launcher| {
            let fields: Vec<String> = model_fields(launcher)
                .into_iter()
                .map(str::to_owned)
                .collect();
            ((*launcher).to_owned(), fields)
        })
        .filter(|(_, fields)| !fields.is_empty())
        .collect()
}

/// Whether `id` has the shape of a model name, which is all GitPulse can know
/// about it: Claude Code accepts any full model name and Antigravity's list
/// is per account, so the *truth* of an id is the CLI's to decide at start.
///
/// The shape is what keeps the value an argument. It must not begin with `-`
/// (the CLI would parse it as an option) and must not contain a comma
/// (`--fallback-model` splits on it), whitespace, or a control character.
/// It allows what real ids use: `opus[1m]`, `claude-opus-4-1@20250805`,
/// `arn:aws:bedrock:…:inference-profile/…`.
pub(crate) fn validate_model_id(what: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("The {what} is empty."));
    }
    if id.len() > MAX_MODEL_ID_LEN {
        return Err(format!(
            "The {what} is longer than {MAX_MODEL_ID_LEN} characters."
        ));
    }
    if !id.as_bytes()[0].is_ascii_alphanumeric() {
        return Err(format!(
            "The {what} {id:?} must start with a letter or digit."
        ));
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "._:/@[]-".contains(*c)))
    {
        return Err(format!(
            "The {what} {id:?} contains {bad:?}; model names use letters, digits and . _ : / @ [ ] -"
        ));
    }
    Ok(())
}

/// Whether `choice` is one a launch of `provider` could apply in full.
///
/// A field the CLI does not take is refused rather than dropped: a model the
/// user picked that a launch silently left out would look applied and not be.
pub(crate) fn validate_model_choice(provider: &str, choice: &ModelChoice) -> Result<(), String> {
    if !is_terminal_provider(provider) {
        return Err(format!("{provider} does not take a model setting."));
    }
    for (field, present) in [
        ("model", choice.model.is_some()),
        ("effort", choice.effort.is_some()),
        ("fallback", choice.fallback.is_some()),
        ("advisor", choice.advisor.is_some()),
    ] {
        if present && model_control(provider, field).is_none() {
            return Err(format!("{provider} does not take a {field} setting."));
        }
    }
    if let Some(model) = &choice.model {
        validate_model_id("model", model)?;
    }
    if let Some(effort) = &choice.effort {
        if !EFFORT_LEVELS.contains(&effort.as_str()) {
            return Err(format!(
                "{effort:?} is not an effort level ({}).",
                EFFORT_LEVELS.join(", ")
            ));
        }
    }
    if let Some(fallback) = &choice.fallback {
        if fallback.is_empty() || fallback.len() > MAX_FALLBACK_MODELS {
            return Err(format!(
                "Name between 1 and {MAX_FALLBACK_MODELS} fallback models."
            ));
        }
        for (index, id) in fallback.iter().enumerate() {
            validate_model_id("fallback model", id)?;
            if fallback[..index].contains(id) {
                return Err(format!("The fallback model {id:?} is listed twice."));
            }
        }
    }
    if let Some(advisor) = &choice.advisor {
        validate_model_id("advisor model", advisor)?;
    }
    Ok(())
}

/// The choice one task launch applies: the saved default for `provider` with
/// this launch's override laid over it field by field. A field the override
/// names replaces the default's; one it leaves out keeps it. Validated as a
/// whole, so an override cannot combine with a default into something the
/// CLI would not take. `None` when neither chooses anything.
pub(crate) fn launch_model_choice(
    provider: &str,
    default: Option<&ModelChoice>,
    launch: Option<&ModelChoice>,
) -> Result<Option<ModelChoice>, String> {
    let mut choice = default.cloned().unwrap_or_default();
    if let Some(launch) = launch {
        let launch = launch.clone();
        choice.model = launch.model.or(choice.model);
        choice.effort = launch.effort.or(choice.effort);
        choice.fallback = launch.fallback.or(choice.fallback);
        choice.advisor = launch.advisor.or(choice.advisor);
    }
    if choice.is_empty() {
        return Ok(None);
    }
    validate_model_choice(provider, &choice)?;
    Ok(Some(choice))
}

/// A [`ModelChoice`] as the store records it on a run (`model_choice`): one
/// string per field, `fallback` joined by commas — a model name never holds
/// one ([`validate_model_id`]).
pub(crate) fn model_choice_record(choice: &ModelChoice) -> serde_json::Value {
    let mut record = serde_json::Map::new();
    for (field, value) in [
        ("model", choice.model.clone()),
        ("effort", choice.effort.clone()),
        (
            "fallback",
            choice.fallback.as_ref().map(|list| list.join(",")),
        ),
        ("advisor", choice.advisor.clone()),
    ] {
        if let Some(value) = value {
            record.insert(field.into(), serde_json::Value::String(value));
        }
    }
    serde_json::Value::Object(record)
}

/// The run's recorded choice back as a [`ModelChoice`]; `None` for a run that
/// recorded none. A record this build cannot read is refused rather than
/// launched without the model it names.
pub(crate) fn model_choice_from_record(
    record: &serde_json::Value,
) -> Result<Option<ModelChoice>, String> {
    if record.is_null() {
        return Ok(None);
    }
    let object = record
        .as_object()
        .ok_or("The run's model choice is not an object.")?;
    let mut choice = ModelChoice::default();
    for (field, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| format!("The run's model field {field} is not text."))?
            .to_owned();
        match field.as_str() {
            "model" => choice.model = Some(value),
            "effort" => choice.effort = Some(value),
            "fallback" => choice.fallback = Some(value.split(',').map(str::to_owned).collect()),
            "advisor" => choice.advisor = Some(value),
            other => {
                return Err(format!(
                    "The run records a model field this build does not know: {other}."
                ))
            }
        }
    }
    Ok((!choice.is_empty()).then_some(choice))
}

/// Keys for Claude Code's session-scoped `--settings` object, with their values.
type ClaudeSettings = Vec<(&'static str, String)>;

/// A [`ModelChoice`] as `provider`'s own flags, plus any keys that belong in
/// Claude Code's `--settings` object. Validated first, so nothing a launch
/// could not apply in full is ever expanded.
fn model_flags(
    provider: &str,
    choice: &ModelChoice,
) -> Result<(Vec<String>, ClaudeSettings), String> {
    validate_model_choice(provider, choice)?;
    let fallback = choice.fallback.as_ref().map(|list| list.join(","));
    let mut flags = Vec::new();
    let mut settings = Vec::new();
    for (field, value) in [
        ("model", choice.model.as_ref()),
        ("effort", choice.effort.as_ref()),
        ("fallback", fallback.as_ref()),
        ("advisor", choice.advisor.as_ref()),
    ] {
        let Some(value) = value else { continue };
        match model_control(provider, field) {
            Some(ModelControl::Flag(flag)) => flags.extend([flag.to_owned(), value.clone()]),
            Some(ModelControl::ClaudeSetting(key)) => settings.push((key, value.clone())),
            None => return Err(format!("{provider} does not take a {field} setting.")),
        }
    }
    Ok((flags, settings))
}

/// Adds `entries` to the one `--settings` object in `flags`, or appends one.
///
/// One object because `--settings` takes a single value: a second occurrence
/// would replace the first, and the notification channel would be lost the
/// moment someone chose an advisor.
fn merge_claude_settings(flags: &mut Vec<String>, entries: ClaudeSettings) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let (at, mut object) = match flags.iter().position(|flag| flag == "--settings") {
        Some(at) => {
            let raw = flags
                .get(at + 1)
                .ok_or("--settings is missing its value.")?;
            match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(serde_json::Value::Object(object)) => (Some(at), object),
                _ => return Err("--settings is not a JSON object.".into()),
            }
        }
        None => (None, serde_json::Map::new()),
    };
    for (key, value) in entries {
        object.insert(key.to_owned(), serde_json::Value::String(value));
    }
    let merged = serde_json::Value::Object(object).to_string();
    match at {
        Some(at) => flags[at + 1] = merged,
        None => flags.extend(["--settings".to_owned(), merged]),
    }
    Ok(())
}

/// What an agent launch adds for the user's settings rather than for its
/// permission mode. One value so a new setting is one field here, not a new
/// parameter threaded through every launch path.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LaunchOptions<'a> {
    /// Whether to add [`notify_flags`].
    pub notify: bool,
    /// The user's `--setting-sources` value, if they narrowed it.
    pub setting_sources: Option<&'a str>,
    /// The user's model choice for this launcher, if they made one.
    pub model: Option<&'a ModelChoice>,
}

/// Every flag an agent launch adds for the user's settings rather than for
/// its permission mode: the notification flags when `notify`, then the
/// setting sources, then the model choice. The one list both launch paths
/// pass and the task lane checks the build for, so the three cannot disagree.
///
/// Refuses a model choice the launcher cannot apply in full rather than
/// passing part of it.
pub(crate) fn launch_flags(
    provider: &str,
    options: &LaunchOptions<'_>,
) -> Result<Vec<String>, String> {
    let mut flags: Vec<String> = if options.notify {
        notify_flags(provider)
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect()
    } else {
        Vec::new()
    };
    flags.extend(setting_source_flags(provider, options.setting_sources));
    if let Some(choice) = options.model {
        let (model, settings) = model_flags(provider, choice)?;
        flags.extend(model);
        merge_claude_settings(&mut flags, settings)?;
    }
    Ok(flags)
}

/// A plain agent tab's arguments with the [`launch_flags`] in front.
///
/// In front because Claude Code's prompt form is `-- <text>`, after which
/// every argument is positional: a flag appended behind the prompt becomes
/// part of what the user asked for. [`apply_permission_mode`] then puts the
/// policy flags in front of these, so the order is policy, launch, caller.
pub(crate) fn with_launch_flags(
    program: Option<&str>,
    options: &LaunchOptions<'_>,
    args: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, String> {
    let flags = match program.map(str::trim) {
        Some(provider) => launch_flags(provider, options)?,
        None => Vec::new(),
    };
    if flags.is_empty() {
        return Ok(args);
    }
    let mut out = flags;
    out.extend(args.unwrap_or_default());
    Ok(Some(out))
}

/// Whether `id` is a UUID in the canonical 8-4-4-4-12 hex form, which is the
/// only form Claude Code's `--session-id` accepts.
pub(crate) fn is_canonical_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// What a task attempt adds to its provider's argv beyond policy and prompt.
#[derive(Default)]
pub(super) struct Extras<'a> {
    /// The attempt's id, passed to Claude Code as `--session-id` when it is a
    /// canonical UUID so the conversation can be found and resumed by the id
    /// GitPulse already holds. Any other shape is omitted, never reshaped: a
    /// derived id would be one nothing else could look up.
    pub run_id: Option<&'a str>,
    /// The brief's private directory, granted to Claude Code with `--add-dir`.
    pub brief_dir: Option<&'a Path>,
    /// The user's launch settings: notifications, setting sources, model.
    pub launch: LaunchOptions<'a>,
}

fn error(code: &str, message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new(code, message)
}

pub(super) fn is_terminal_provider(provider: &str) -> bool {
    matches!(provider, "codex" | "claude" | "grok" | "agy")
}

/// Providers GitPulse can run in the **managed** lane, where the harness hosts
/// the session over a protocol and GitPulse mediates every approval — rather
/// than handing the task to a terminal and stepping back.
///
/// Strictly smaller than [`is_terminal_provider`], and it is smaller for a
/// mechanical reason rather than a policy one: a managed run needs an adapter
/// in the harness that speaks that provider's own session protocol. Grok and
/// Antigravity have none, so admitting them here would hand their output to a
/// reader written for someone else's wire format. This list is the mirror of
/// the harness's own `codingagent.ManagedProviders`; the two are checked
/// against each other by `managed-provider-parity-contract`.
pub(super) fn is_managed_provider(provider: &str) -> bool {
    matches!(provider, "codex" | "claude")
}

pub(super) fn program(provider: &str) -> Result<String, WorkbenchError> {
    if !is_terminal_provider(provider) {
        return Err(error("invalid_input", "Unsupported terminal provider."));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let path = extended_child_path(std::env::var_os("PATH").as_deref(), home.as_deref());
    let resolved = resolve_spawn_program_with(provider, path.as_deref(), home.as_deref());
    if !Path::new(&resolved).is_absolute() || !Path::new(&resolved).is_file() {
        return Err(error(
            "not_installed",
            format!("{provider} is not available on the terminal PATH."),
        ));
    }
    Ok(resolved)
}

pub(super) fn check(
    program: &str,
    cwd: &str,
    provider: &str,
    mode: &str,
    options: &LaunchOptions<'_>,
) -> Result<(), WorkbenchError> {
    let version = capture_command(
        program,
        &["--version"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    let help = capture_command(
        program,
        &["--help"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    advertises(provider, mode, options, &version, &help)
}

/// Whether what a build *said* proves it can be launched the way `mode` asks.
///
/// Split from [`check`] because this is the whole decision, and it is a decision
/// about two byte strings: which stream they arrived on, what they bound, and
/// what the help advertises. Driving it through two real `--version`/`--help`
/// spawns made every case here wait on a child reaching `main` inside eight
/// seconds — a bound on host load, not on any of this. `capture_command` owns
/// the other half, that the spawn happens and its output comes back.
///
/// Every flag [`arguments`] will pass is proved here, not only the policy
/// ones: a build that does not know `--add-dir` or `--settings` would refuse
/// to start, or worse read the flag's value as the prompt. A flag in
/// [`VALUE_PROVEN_FLAGS`] must also list the chosen value in its own block.
///
/// What this cannot prove: that a model id exists (no help lists them all),
/// or that a `--settings` key such as `advisorModel` is honoured — `--help`
/// shows that `--settings` exists, not which keys this build reads.
fn advertises(
    provider: &str,
    mode: &str,
    options: &LaunchOptions<'_>,
    version: &CapturedOutput,
    help: &CapturedOutput,
) -> Result<(), WorkbenchError> {
    if !version.success
        || version.stdout.len() > 1024
        || version.stderr.len() > 1024
        || !help.success
        || help.stdout.len() > 128 * 1024
        || help.stderr.len() > 128 * 1024
    {
        return Err(error(
            "capability_error",
            "The provider did not return bounded version and help responses.",
        ));
    }
    let version = command_text(&version.stdout, &version.stderr)
        .map_err(|_| error("capability_error", "Invalid provider version response."))?;
    let help = command_text(&help.stdout, &help.stderr)
        .map_err(|_| error("capability_error", "Invalid provider help response."))?;
    let identity = match provider {
        "codex" => version.starts_with("codex-cli "),
        "claude" => version.contains("Claude Code"),
        "grok" => version.starts_with("grok "),
        // `agy --version` prints only a semver. Identity is the help banner;
        // Antigravity writes that banner to stderr, which `command_text` prefers
        // only when stdout is empty.
        "agy" => help.contains("Usage of agy:") && help.contains("--prompt-interactive"),
        _ => false,
    };
    let (flags, _) = policy(provider, mode)?;
    let supported = flags
        .iter()
        .enumerate()
        .filter(|(_, flag)| flag.starts_with("--"))
        .all(|(index, flag)| {
            let value = flags
                .get(index + 1)
                .copied()
                .filter(|next| !next.starts_with("--"));
            advertised_option(help, flag, value)
        });
    let required: &[&str] = match provider {
        "codex" => &["--cd"],
        "grok" => &["--cwd"],
        "claude" => &["--session-id", "--add-dir"],
        "agy" => &[
            "--mode",
            "--prompt-interactive",
            "--dangerously-skip-permissions",
        ],
        _ => &[],
    };
    let chosen =
        launch_flags(provider, options).map_err(|message| error("invalid_input", message))?;
    let chosen_supported = chosen
        .iter()
        .enumerate()
        .filter(|(_, flag)| flag.starts_with('-'))
        .all(|(index, flag)| {
            let value = VALUE_PROVEN_FLAGS
                .contains(&flag.as_str())
                .then(|| chosen.get(index + 1).map(String::as_str))
                .flatten();
            advertised_option(help, flag, value)
        });
    if !identity
        || !supported
        || !chosen_supported
        || required
            .iter()
            .any(|flag| !advertised_option(help, flag, None))
    {
        return Err(error(
            "unsupported_capability",
            "This provider build does not advertise the requested launch controls.",
        ));
    }
    Ok(())
}

/// stdout when the provider printed there, otherwise stderr.
///
/// Claude Code, Codex and Grok write `--help` to stdout. Antigravity's `agy`
/// writes it to stderr (Go `flag`). A check that only read stdout would treat
/// a real install as having no launch controls.
fn command_text<'a>(stdout: &'a [u8], stderr: &'a [u8]) -> Result<&'a str, std::str::Utf8Error> {
    std::str::from_utf8(if stdout.is_empty() { stderr } else { stdout })
}

/// Verify the exact option and value within its own help block. A similarly
/// named flag or a value mentioned by a different option is not capability proof.
fn advertised_option(help: &str, flag: &str, value: Option<&str>) -> bool {
    let mut found = false;
    let mut description = String::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        let is_option = trimmed.starts_with("--")
            || (trimmed.starts_with('-') && trimmed.as_bytes().get(2) == Some(&b','));
        if is_option {
            if found {
                break;
            }
            found = trimmed
                .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
                .any(|word| word == flag);
        }
        if found {
            description.push_str(line);
            description.push('\n');
        }
    }
    found
        && value.is_none_or(|value| {
            description
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|word| word == value)
        })
}

/// Permission modes, in the order a chooser should offer them: least
/// authority first, so the dangerous end of the range is the far end rather
/// than a neighbour of the safe default.
///
/// Read by `agentDefaults.contract.test.ts`, which fails if the frontend's
/// list drifts from this one. The frontend needs the *names* to render a
/// chooser and to say which mode a tab will start in; it must never carry the
/// flags, because a transcribed flag table is a table that drifts.
pub(crate) const PERMISSION_MODES: [&str; 6] = [
    "inspect",
    "ask",
    "edit",
    "auto_review",
    "preapproved",
    "bypass",
];

/// The one mode that turns a safety control off, named once so the two places
/// that must treat it specially cannot disagree about which one it is.
pub(crate) const BYPASS_MODE: &str = "bypass";

/// Launchers that accept a permission mode, derived by asking [`policy`]
/// rather than by writing the list down a second time.
///
/// Every launcher the tab strip offers is tried against every mode, and one
/// that can express them all is included. Derived because the alternative —
/// a hand-kept list — is the thing that goes stale when a provider gains or
/// loses a policy, and a stale list here is a chooser offering a mode that
/// refuses at spawn.
pub(crate) fn permission_launchers() -> Vec<String> {
    crate::terminal::AGENT_LAUNCHERS
        .iter()
        .filter(|launcher| {
            PERMISSION_MODES
                .iter()
                .all(|mode| policy(launcher, mode).is_ok())
        })
        .map(|launcher| (*launcher).to_owned())
        .collect()
}

/// Whether this launcher/mode pair is one a launch could actually apply.
///
/// Asks [`policy`] rather than restating its arms, so a pair this accepts is
/// exactly a pair that expands. Used to validate stored defaults at the point
/// they are saved *and* again when they are read, because the file they live
/// in is editable by hand.
pub(crate) fn validate_permission_default(launcher: &str, mode: &str) -> Result<(), String> {
    if !is_terminal_provider(launcher) {
        return Err(format!("{launcher} does not take a permission mode"));
    }
    policy(launcher, mode)
        .map(|_| ())
        .map_err(|error| error.message)
}

/// Expands a stored default permission mode into that provider's own flags,
/// ahead of the arguments the caller already built.
///
/// This exists so the terminal tab strip and the workbench handoff share one
/// table. [`policy`] is the only place that knows which flag means "plan" for
/// which CLI; the frontend knows mode *names* and nothing else, so there is no
/// second copy to drift.
///
/// **Order matters and is why the flags go in front.** Claude Code's prompt
/// form is `-- <text>`, after which every argument is positional: a permission
/// flag appended behind the prompt would be read as part of the prompt rather
/// than obeyed. The caller's own arguments (notification flags, then the
/// prompt) keep their relative order behind these.
///
/// Refuses rather than ignores in three cases, all of them the same rule: a
/// control the user asked for that cannot be applied must never look like one
/// that was.
///
/// * A mode for a launcher with no policy (`shell`, `manvi`) — silently
///   dropping it would leave a tab the user believes is in plan mode running
///   with the CLI's own default.
/// * An unknown mode — the same, one layer down.
/// * An acknowledgement that does not match the mode. Exactly as
///   [`arguments`] does it: `bypass` needs one and every other mode must not
///   carry one. The symmetry is the point — a caller that hardcoded
///   `acknowledged: true` would fail on its very first ordinary launch, rather
///   than working until the day someone stores `bypass` and silently getting
///   the session nobody agreed to.
pub(crate) fn apply_permission_mode(
    program: Option<&str>,
    mode: Option<&str>,
    acknowledged: bool,
    args: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, String> {
    let Some(mode) = mode.map(str::trim).filter(|mode| !mode.is_empty()) else {
        return Ok(args);
    };
    let provider = program.map(str::trim).unwrap_or_default();
    if !is_terminal_provider(provider) {
        return Err(format!(
            "{} does not take a permission mode.",
            if provider.is_empty() {
                "The login shell"
            } else {
                provider
            }
        ));
    }
    if (mode == BYPASS_MODE) != acknowledged {
        return Err(if acknowledged {
            "Only bypass takes an acknowledgment.".into()
        } else {
            "Bypass requires acknowledgment for this attempt.".to_owned()
        });
    }
    let (flags, _) = policy(provider, mode).map_err(|error| error.message)?;
    let mut expanded: Vec<String> = flags.into_iter().map(String::from).collect();
    expanded.extend(args.unwrap_or_default());
    Ok(Some(expanded))
}

fn policy(provider: &str, mode: &str) -> Result<(Vec<&'static str>, bool), WorkbenchError> {
    let flags = match (provider, mode) {
        ("codex", "inspect") => vec!["--sandbox", "read-only", "--ask-for-approval", "never"],
        ("codex", "ask") => vec!["--sandbox", "read-only", "--ask-for-approval", "on-request"],
        ("codex", "edit") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
        ],
        ("codex", "preapproved") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
        ],
        ("codex", "auto_review") => vec!["--approve-for-me"],
        ("codex", "bypass") => vec!["--dangerously-bypass-approvals-and-sandbox"],
        ("claude", "inspect") => vec!["--permission-mode", "plan"],
        ("claude", "ask") => vec!["--permission-mode", "manual"],
        ("claude", "edit") => vec!["--permission-mode", "acceptEdits"],
        ("claude", "auto_review") => vec!["--permission-mode", "auto"],
        ("claude", "preapproved") => vec!["--permission-mode", "dontAsk"],
        ("claude", "bypass") => vec!["--permission-mode", "bypassPermissions"],
        // Grok's modes are Claude-compatible except `ask`: it has `default`
        // (prompt for permissions) rather than `manual`.
        ("grok", "inspect") => vec!["--permission-mode", "plan"],
        ("grok", "ask") => vec!["--permission-mode", "default"],
        ("grok", "edit") => vec!["--permission-mode", "acceptEdits"],
        ("grok", "auto_review") => vec!["--permission-mode", "auto"],
        ("grok", "preapproved") => vec!["--permission-mode", "dontAsk"],
        ("grok", "bypass") => vec!["--permission-mode", "bypassPermissions"],
        ("agy", "inspect") => vec!["--mode", "plan"],
        ("agy", "ask") => vec![],
        ("agy", "edit") => vec!["--mode", "accept-edits"],
        ("agy", "auto_review") => vec!["--sandbox"],
        ("agy", "preapproved") => vec!["--mode", "accept-edits", "--sandbox"],
        ("agy", "bypass") => vec!["--dangerously-skip-permissions"],
        _ => {
            return Err(error(
                "unsupported_capability",
                "Unsupported provider permission mode.",
            ))
        }
    };
    Ok((flags, mode == "inspect"))
}

/// What a launched agent is told about finishing. How to work is not said
/// here: the brief file is the store's export, which opens with the agent
/// guidance every handoff carries (`dc_store::workbench::AGENT_GUIDANCE`), so
/// a terminal agent and a managed one are told the same thing. The guidance
/// names no completion step because only this launch knows the mode; the
/// brief's `Task: <id> (revision N)` line follows it. The tool is
/// `gitpulse_complete_task` on the GitPulse MCP server, which changes only the
/// status and records the summary. Manvi's managed lane states the same scope
/// in `serve.managedTurn`.
pub(crate) const COMPLETION: &str = "Carry out the saved task within the selected permission mode. When every acceptance criterion is met and your verification passed, mark the task done with the gitpulse_complete_task tool, using the id on the brief's Task: line and a short summary of what changed and how you verified it; if anything is unfinished or needs a person's judgement, use status review instead and say what remains.";

pub(super) fn arguments(
    provider: &str,
    mode: &str,
    acknowledged: bool,
    cwd: &str,
    brief: &Path,
    extras: &Extras<'_>,
) -> Result<Vec<String>, WorkbenchError> {
    if (mode == "bypass") != acknowledged {
        return Err(error(
            "invalid_input",
            "Bypass requires acknowledgment for this attempt.",
        ));
    }
    let (flags, inspect) = policy(provider, mode)?;
    let path = brief
        .to_str()
        .ok_or_else(|| error("file_error", "Task brief path is not Unicode."))?;
    let quoted = serde_json::to_string(path).map_err(|e| error("file_error", e.to_string()))?;
    let mut args: Vec<String> = flags.into_iter().map(String::from).collect();
    args.extend(
        launch_flags(provider, &extras.launch)
            .map_err(|message| error("invalid_input", message))?,
    );
    match provider {
        "codex" => args.extend(["--cd".into(), cwd.into()]),
        "grok" => args.extend(["--cwd".into(), cwd.into()]),
        // Antigravity ignores positional arguments as prompts.
        "agy" => args.push("--prompt-interactive".into()),
        "claude" => {
            if let Some(id) = extras.run_id.filter(|id| is_canonical_uuid(id)) {
                args.extend(["--session-id".into(), id.into()]);
            }
            if let Some(dir) = extras.brief_dir {
                let dir = dir
                    .to_str()
                    .ok_or_else(|| error("file_error", "Task brief path is not Unicode."))?;
                args.extend(["--add-dir".into(), dir.into()]);
            }
            // `--add-dir` takes any number of values, so without the separator
            // it would swallow the prompt as a second directory.
            args.push("--".into());
        }
        _ => {}
    }
    let scope = if inspect {
        "Inspect and propose a plan; do not modify files or change the task's status."
    } else {
        COMPLETION
    };
    args.push(format!("Read the UTF-8 task brief at {quoted}. It opens with how to work in this repository (its own instructions, GitPulse, DevMap and DevCouncil, and the verification rules), then the user's saved task and repository references. {scope} Do not publish changes (push, merge, release) on the user's behalf."));
    if args.iter().any(|a| a.len() > 16 * 1024 || a.contains('\0')) {
        return Err(error(
            "invalid_input",
            "The launch arguments exceed terminal limits.",
        ));
    }
    Ok(args)
}

#[cfg(test)]
mod permission_default_tests {
    use super::{
        apply_permission_mode, permission_launchers, policy, validate_permission_default,
        BYPASS_MODE, PERMISSION_MODES,
    };

    fn args(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// The load-bearing property. Claude Code's `-- <text>` makes everything
    /// after it positional, so a permission flag that landed behind the prompt
    /// would be read as prompt text and the session would run unrestricted
    /// while appearing configured.
    #[test]
    fn policy_flags_precede_the_callers_own_arguments() {
        let out = apply_permission_mode(
            Some("claude"),
            Some("inspect"),
            false,
            args(&["--settings", "{}", "--", "do the thing"]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            vec![
                "--permission-mode",
                "plan",
                "--settings",
                "{}",
                "--",
                "do the thing"
            ]
        );
        let separator = out.iter().position(|a| a == "--").unwrap();
        assert!(
            out.iter().position(|a| a == "--permission-mode").unwrap() < separator,
            "a permission flag after `--` is prompt text, not a permission"
        );
    }

    #[test]
    fn every_mode_expands_for_every_launcher_that_offers_them() {
        for launcher in permission_launchers() {
            for mode in PERMISSION_MODES {
                let acknowledged = mode == BYPASS_MODE;
                let out =
                    apply_permission_mode(Some(&launcher), Some(mode), acknowledged, None).unwrap();
                assert!(
                    out.is_some(),
                    "{launcher}/{mode} expanded to nothing at all"
                );
                assert_eq!(
                    out.unwrap(),
                    policy(&launcher, mode).unwrap().0,
                    "{launcher}/{mode} disagreed with the policy table"
                );
            }
        }
    }

    /// `agy`'s `ask` is the one pair with no flags. It must still be offered
    /// and still expand — to an empty list, which is a mode that was applied,
    /// not a mode that was skipped.
    #[test]
    fn a_mode_whose_policy_is_empty_is_still_applied() {
        assert_eq!(policy("agy", "ask").unwrap().0, Vec::<&str>::new());
        let out = apply_permission_mode(Some("agy"), Some("ask"), false, args(&["--keep"]))
            .unwrap()
            .unwrap();
        assert_eq!(out, vec!["--keep"]);
    }

    #[test]
    fn bypass_without_acknowledgment_is_refused_for_every_launcher() {
        for launcher in permission_launchers() {
            let refused = apply_permission_mode(Some(&launcher), Some(BYPASS_MODE), false, None);
            assert!(
                refused.is_err(),
                "{launcher} produced a bypassed session with no acknowledgment"
            );
        }
    }

    /// The symmetry that makes a hardcoded `acknowledged: true` fail loudly on
    /// an ordinary launch instead of lying dormant until bypass is stored.
    #[test]
    fn an_acknowledgment_without_bypass_is_refused() {
        for mode in PERMISSION_MODES.iter().filter(|m| **m != BYPASS_MODE) {
            assert!(
                apply_permission_mode(Some("claude"), Some(mode), true, None).is_err(),
                "{mode} accepted an acknowledgment it does not need"
            );
        }
    }

    /// A mode asked for and not applied must be a refusal, never a quiet
    /// passthrough: a tab the user believes is in plan mode would otherwise
    /// run with the CLI's own default.
    #[test]
    fn a_launcher_with_no_policy_refuses_a_mode_rather_than_dropping_it() {
        for launcher in [None, Some(""), Some("shell"), Some("manvi"), Some("bash")] {
            let refused = apply_permission_mode(launcher, Some("inspect"), false, None);
            assert!(
                refused.is_err(),
                "{launcher:?} silently ignored a permission mode"
            );
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_rather_than_guessed() {
        for mode in ["", "  ", "plan", "yolo", "BYPASS", "inspect ", "--sandbox"] {
            let out = apply_permission_mode(Some("claude"), Some(mode), false, args(&["--keep"]));
            if mode.trim().is_empty() {
                // Nothing asked for, so the caller's arguments pass through.
                assert_eq!(out.unwrap().unwrap(), vec!["--keep"]);
            } else if mode == "inspect " {
                // Trimmed, then honoured: a stored value with stray space is a
                // typo, not a different mode.
                assert_eq!(
                    out.unwrap().unwrap(),
                    vec!["--permission-mode", "plan", "--keep"]
                );
            } else {
                assert!(out.is_err(), "{mode:?} was accepted");
            }
        }
    }

    #[test]
    fn no_mode_leaves_the_arguments_exactly_as_they_came() {
        assert_eq!(
            apply_permission_mode(Some("claude"), None, false, args(&["--settings", "{}"]))
                .unwrap()
                .unwrap(),
            vec!["--settings", "{}"]
        );
        assert!(
            apply_permission_mode(None, None, false, None)
                .unwrap()
                .is_none(),
            "a plain shell gained arguments it never asked for"
        );
    }

    /// The chooser's list is derived, so this pins what it derives *to* — a
    /// provider silently dropping out of the list would otherwise be invisible.
    #[test]
    fn permission_launchers_are_the_terminal_providers_and_not_the_tab_strip() {
        let mut launchers = permission_launchers();
        launchers.sort();
        assert_eq!(launchers, vec!["agy", "claude", "codex", "grok"]);
        assert!(
            !launchers.contains(&"manvi".to_owned()),
            "manvi has no policy; offering it a mode would refuse at spawn"
        );
    }

    /// Sweeps the whole grid plus the ways a caller could be wrong, and
    /// asserts the one property that must hold across all of it: every call
    /// either applies exactly the policy table's flags, or refuses. There is
    /// no third outcome, and in particular no "returned the arguments
    /// unchanged while the caller believes a mode was applied".
    #[test]
    fn every_input_either_applies_the_table_or_refuses_and_never_silently_passes() {
        let launchers = [
            "claude", "codex", "grok", "agy", "manvi", "shell", "", "sh", "CLAUDE",
        ];
        let modes = [
            "inspect",
            "ask",
            "edit",
            "auto_review",
            "preapproved",
            "bypass",
            "plan",
            "yolo",
            "",
            "  ",
            "BYPASS",
            "bypass\u{0}",
            "../bypass",
        ];
        let (mut applied, mut refused, mut passthrough) = (0usize, 0usize, 0usize);
        for launcher in launchers {
            for mode in modes {
                for acknowledged in [false, true] {
                    let carried = args(&["--carried"]);
                    let outcome = apply_permission_mode(
                        Some(launcher),
                        Some(mode),
                        acknowledged,
                        carried.clone(),
                    );
                    let Ok(out) = outcome else {
                        refused += 1;
                        continue;
                    };
                    let out = out.expect("an applied mode must produce arguments");
                    if mode.trim().is_empty() {
                        // Nothing was asked for, so nothing was applied and the
                        // caller's own arguments survive untouched.
                        assert_eq!(out, vec!["--carried"]);
                        passthrough += 1;
                        continue;
                    }
                    // Anything else that succeeded must be exactly the table's
                    // flags followed by what the caller passed.
                    let (flags, _) =
                        policy(launcher, mode.trim()).expect("a mode applied without a policy arm");
                    let mut expected: Vec<String> = flags.into_iter().map(String::from).collect();
                    expected.push("--carried".into());
                    assert_eq!(out, expected, "{launcher}/{mode}/ack={acknowledged}");
                    // And bypass only ever with an acknowledgement.
                    assert_eq!(mode.trim() == BYPASS_MODE, acknowledged);
                    applied += 1;
                }
            }
        }
        // Carrying all three numbers rather than one total: a sweep where
        // everything refused would "pass" a refusal-only assertion while
        // proving nothing about the applied path.
        assert_eq!(
            applied,
            permission_launchers().len() * PERMISSION_MODES.len(),
            "expected exactly one applied outcome per real launcher/mode pair"
        );
        assert!(refused > 0 && passthrough > 0, "a branch went untested");
    }

    /// A caller that hands over an enormous argument list must not have the
    /// flags silently dropped or reordered; expansion is a prepend and stays
    /// one whatever the volume.
    #[test]
    fn expansion_prepends_regardless_of_how_many_arguments_the_caller_brought() {
        let carried: Vec<String> = (0..10_000).map(|i| format!("--arg{i}")).collect();
        let out = apply_permission_mode(
            Some("claude"),
            Some("inspect"),
            false,
            Some(carried.clone()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.len(), carried.len() + 2);
        assert_eq!(&out[..2], &["--permission-mode", "plan"]);
        assert_eq!(&out[2..], &carried[..]);
    }

    #[test]
    fn stored_defaults_are_validated_against_the_policy_table() {
        assert!(validate_permission_default("claude", "edit").is_ok());
        for (launcher, mode) in [
            ("manvi", "edit"),
            ("shell", "edit"),
            ("claude", "yolo"),
            ("", "edit"),
            ("claude", ""),
        ] {
            assert!(
                validate_permission_default(launcher, mode).is_err(),
                "{launcher}={mode} passed validation"
            );
        }
    }

    /// Bypass is storable — that is the whole point of the acknowledgement
    /// design — but storing it must not be what applies it.
    #[test]
    fn bypass_validates_as_a_stored_default_yet_still_needs_acknowledgment() {
        assert!(validate_permission_default("claude", BYPASS_MODE).is_ok());
        assert!(apply_permission_mode(Some("claude"), Some(BYPASS_MODE), false, None).is_err());
        assert_eq!(
            apply_permission_mode(Some("claude"), Some(BYPASS_MODE), true, None)
                .unwrap()
                .unwrap(),
            vec!["--permission-mode", "bypassPermissions"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_canonical_uuid, notify_flags, with_launch_flags, BriefFile, CapturedOutput, Extras,
        LaunchOptions,
    };

    /// Launch settings with no model choice: what every test written before
    /// model controls existed was describing.
    fn opts(notify: bool, setting_sources: Option<&str>) -> LaunchOptions<'_> {
        LaunchOptions {
            notify,
            setting_sources,
            model: None,
        }
    }

    /// The launch as these tests mostly want it: no session id, no granted
    /// directory, no notification flags — the argv the policy alone produces.
    fn arguments(
        provider: &str,
        mode: &str,
        acknowledged: bool,
        cwd: &str,
        brief: &std::path::Path,
    ) -> Result<Vec<String>, super::WorkbenchError> {
        super::arguments(provider, mode, acknowledged, cwd, brief, &Extras::default())
    }

    /// The options a current Claude Code advertises that a task launch uses,
    /// laid out the way its `--help` lays them out.
    const CLAUDE_HELP: &str = "  --add-dir <directories...>  Additional directories to allow tool\n                              access to\n  --permission-mode <mode>  Permission mode\n    (choices: \"plan\", \"manual\")\n  --session-id <uuid>  Use a specific session ID for the\n                       conversation (must be a valid UUID)\n  --settings <file-or-json>  Path to a settings JSON file\n";

    const RUN: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

    #[test]
    fn claude_is_given_its_notification_channel_and_nothing_else() {
        let flags = notify_flags("claude");
        assert_eq!(flags[0], "--settings");
        // `--settings` merges key by key, so a second key here would silently
        // override something the user set.
        let parsed: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(flags[1]).unwrap();
        assert_eq!(parsed.keys().collect::<Vec<_>>(), ["preferredNotifChannel"]);
        assert_eq!(parsed["preferredNotifChannel"], "terminal_bell");
        assert_eq!(flags.len(), 2);
    }

    #[test]
    fn codex_is_given_toml_overrides_quoted_as_toml_and_not_as_shell() {
        let flags = notify_flags("codex");
        assert_eq!(flags.iter().filter(|flag| **flag == "-c").count(), 3);
        assert!(flags.contains(&"tui.notifications=true"));
        // The inner quotes are the TOML string. Nothing expands argv, so
        // stripping them would hand Codex a bare word.
        assert!(flags.contains(&r#"tui.notification_method="osc9""#));
        assert!(flags.contains(&r#"tui.notification_condition="always""#));
    }

    #[test]
    fn no_flag_is_invented_for_a_cli_whose_setting_has_not_been_read() {
        for launcher in crate::terminal::AGENT_LAUNCHERS {
            let flags = notify_flags(launcher);
            if !matches!(launcher, "claude" | "codex") {
                assert!(flags.is_empty(), "{launcher}: {flags:?}");
            }
            for flag in flags {
                assert!(!flag.contains('\0') && !flag.contains('\n'), "{flag}");
            }
        }
        for unknown in ["shell", "manvi", "", "claude ", "Claude"] {
            assert!(notify_flags(unknown).is_empty(), "{unknown:?}");
        }
    }

    #[test]
    fn a_plain_agent_tab_gets_policy_then_notification_then_its_prompt() {
        let prompt = Some(vec!["--".to_owned(), "Fix the failing test".to_owned()]);
        let with = with_launch_flags(Some("claude"), &opts(true, None), prompt.clone()).unwrap();
        let out = super::apply_permission_mode(Some("claude"), Some("edit"), false, with)
            .unwrap()
            .unwrap();
        let at = |flag: &str| out.iter().position(|a| a == flag).unwrap();
        assert!(at("--permission-mode") < at("--settings"), "{out:?}");
        assert!(at("--settings") < at("--"), "{out:?}");
        assert_eq!(out.last().unwrap(), "Fix the failing test");
        assert_eq!(out.iter().filter(|a| *a == "--settings").count(), 1);

        // Off means exactly what the caller sent, including "nothing at all".
        assert_eq!(
            with_launch_flags(Some("claude"), &opts(false, None), prompt.clone()).unwrap(),
            prompt
        );
        assert_eq!(
            with_launch_flags(Some("claude"), &opts(false, None), None).unwrap(),
            None
        );
        assert_eq!(
            with_launch_flags(None, &opts(true, None), prompt.clone()).unwrap(),
            prompt
        );
        assert_eq!(
            with_launch_flags(Some("grok"), &opts(true, None), None).unwrap(),
            None
        );
        // A promptless Codex tab is flags only.
        assert_eq!(
            with_launch_flags(Some("codex"), &opts(true, None), None)
                .unwrap()
                .unwrap(),
            notify_flags("codex")
        );
    }

    /// The brief's guidance is shared by every mode, so it must not carry the
    /// completion rule: an inspect launch is told not to change the task's
    /// status, and a brief that also told it to mark the task done would
    /// contradict its own launch prompt. Only the edit-capable prompt names
    /// the tool.
    #[test]
    fn only_an_edit_capable_launch_is_told_to_complete_the_task() {
        assert!(!dc_store::workbench::AGENT_GUIDANCE.contains("gitpulse_complete_task"));
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        for provider in ["claude", "codex", "grok", "agy"] {
            let inspect = arguments(provider, "inspect", false, "/c", &brief.path).unwrap();
            let prompt = inspect.last().unwrap();
            assert!(
                prompt.starts_with("Read the UTF-8 task brief"),
                "{provider}"
            );
            assert!(prompt.contains("DevMap and DevCouncil"), "{provider}");
            assert!(!prompt.contains("gitpulse_complete_task"), "{provider}");
            let edit = arguments(provider, "edit", false, "/c", &brief.path).unwrap();
            assert!(
                edit.last().unwrap().contains("gitpulse_complete_task"),
                "{provider}"
            );
        }
    }

    /// The defect: a task attempt's argv was built only from the policy and
    /// the brief pointer, so the agent most likely to stop on a permission
    /// prompt in a hidden tab was the one launched with no way to say so.
    #[test]
    fn a_task_attempt_gets_the_same_notification_flags_as_a_plain_tab() {
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        for provider in ["claude", "codex"] {
            let on = super::arguments(
                provider,
                "edit",
                false,
                "/checkout",
                &brief.path,
                &Extras {
                    launch: opts(true, None),
                    ..Extras::default()
                },
            )
            .unwrap();
            let flags = notify_flags(provider);
            assert!(
                on.windows(flags.len()).any(|run| run == flags),
                "{provider}: {on:?}"
            );
            assert!(on.last().unwrap().starts_with("Read the UTF-8 task brief"));
            let off = arguments(provider, "edit", false, "/checkout", &brief.path).unwrap();
            assert!(
                !off.iter().any(|a| flags.contains(&a.as_str()) && a != "-c"),
                "{provider}: {off:?}"
            );
            assert!(!off.iter().any(|a| a == "-c" || a == "--settings"));
        }
    }

    #[test]
    fn a_claude_attempt_is_resumable_by_its_run_id_and_may_read_its_brief() {
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        let args = super::arguments(
            "claude",
            "preapproved",
            false,
            "/checkout",
            &brief.path,
            &Extras {
                run_id: Some(RUN),
                brief_dir: Some(&brief.dir),
                launch: opts(true, None),
            },
        )
        .unwrap();
        assert!(
            args.windows(2).any(|pair| pair == ["--session-id", RUN]),
            "{args:?}"
        );
        let dir = brief.dir.to_str().unwrap();
        assert!(
            args.windows(2).any(|pair| pair == ["--add-dir", dir]),
            "{args:?}"
        );
        // `--add-dir` is variadic: only the separator stops it reading the
        // prompt as a second directory.
        let separator = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(separator, args.len() - 2, "{args:?}");
        assert!(args.iter().position(|a| a == "--add-dir").unwrap() < separator);
        assert!(args.iter().position(|a| a == "--settings").unwrap() < separator);
        assert!(args[separator + 1].contains(&*brief.path.to_string_lossy()));

        // An id Claude would refuse is left out, never reshaped into one.
        for id in ["run", "6F1C2A7E0D4B4C1E9A553B0E8F2D9C11", "../../run", ""] {
            let args = super::arguments(
                "claude",
                "edit",
                false,
                "/checkout",
                &brief.path,
                &Extras {
                    run_id: Some(id),
                    ..Extras::default()
                },
            )
            .unwrap();
            assert!(!args.iter().any(|a| a == "--session-id"), "{id}: {args:?}");
        }
        // Only Claude is handed these; the others would not know them.
        for provider in ["codex", "grok", "agy"] {
            let args = super::arguments(
                provider,
                "edit",
                false,
                "/checkout",
                &brief.path,
                &Extras {
                    run_id: Some(RUN),
                    brief_dir: Some(&brief.dir),
                    launch: opts(false, None),
                },
            )
            .unwrap();
            assert!(
                !args
                    .iter()
                    .any(|a| a == "--session-id" || a == "--add-dir" || a == "--"),
                "{provider}: {args:?}"
            );
        }
    }

    #[test]
    fn a_uuid_is_only_the_canonical_form() {
        assert!(is_canonical_uuid(RUN));
        assert!(is_canonical_uuid("6F1C2A7E-0D4B-4C1E-9A55-3B0E8F2D9C11"));
        for not in [
            "",
            "run",
            "6f1c2a7e0d4b4c1e9a553b0e8f2d9c11",
            "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c1",
            "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c111",
            "6f1c2a7e_0d4b-4c1e-9a55-3b0e8f2d9c11",
            "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9cxz",
            "{6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c1}",
            "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c1\u{e9}",
        ] {
            assert!(!is_canonical_uuid(not), "{not:?}");
        }
    }

    /// Every flag the launch passes is one the build was asked about.
    #[test]
    fn a_claude_build_must_advertise_every_flag_the_launch_will_pass() {
        let version = on_stdout("2.1.289 (Claude Code)\n");
        let help = on_stdout(CLAUDE_HELP);
        super::advertises("claude", "ask", &opts(true, None), &version, &help).unwrap();
        for missing in ["--session-id", "--add-dir", "--settings"] {
            let narrower: String = CLAUDE_HELP
                .lines()
                .filter(|line| !line.trim_start().starts_with(missing))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(
                super::advertises(
                    "claude",
                    "ask",
                    &opts(true, None),
                    &version,
                    &on_stdout(&narrower)
                )
                .unwrap_err()
                .code,
                "unsupported_capability",
                "{missing}"
            );
        }
        // Without notifications, `--settings` is not passed and not required.
        let no_settings: String = CLAUDE_HELP
            .lines()
            .filter(|line| !line.trim_start().starts_with("--settings"))
            .map(|line| format!("{line}\n"))
            .collect();
        super::advertises(
            "claude",
            "ask",
            &opts(false, None),
            &version,
            &on_stdout(&no_settings),
        )
        .unwrap();
    }

    /// The user's narrowing of Claude's settings files reaches both launch
    /// paths ahead of the prompt, reaches no other CLI, and is absent — the
    /// CLI's own default — when nothing was chosen.
    #[test]
    fn a_claude_launch_loads_only_the_settings_sources_the_user_chose() {
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        let task = |provider: &str, sources: Option<&str>| {
            super::arguments(
                provider,
                "edit",
                false,
                "/checkout",
                &brief.path,
                &Extras {
                    run_id: Some(RUN),
                    brief_dir: Some(&brief.dir),
                    launch: opts(true, sources),
                },
            )
            .unwrap()
        };
        let args = task("claude", Some("user"));
        let at = |args: &[String], flag: &str| args.iter().position(|a| a == flag);
        let flag = at(&args, "--setting-sources").expect("not passed");
        assert_eq!(args[flag + 1], "user");
        assert!(flag < at(&args, "--").unwrap(), "{args:?}");
        assert_eq!(args.iter().filter(|a| *a == "--setting-sources").count(), 1);
        assert!(at(&task("claude", None), "--setting-sources").is_none());
        for provider in ["codex", "grok", "agy"] {
            assert!(
                at(&task(provider, Some("user")), "--setting-sources").is_none(),
                "{provider}"
            );
        }

        let prompt = Some(vec!["--".to_owned(), "Fix the failing test".to_owned()]);
        let tab = super::apply_permission_mode(
            Some("claude"),
            Some("edit"),
            false,
            with_launch_flags(
                Some("claude"),
                &opts(false, Some("user,local")),
                prompt.clone(),
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        let flag = at(&tab, "--setting-sources").expect("a plain tab was not narrowed");
        assert_eq!(tab[flag + 1], "user,local");
        assert!(at(&tab, "--permission-mode").unwrap() < flag, "{tab:?}");
        assert!(flag < at(&tab, "--").unwrap(), "{tab:?}");
        assert_eq!(
            with_launch_flags(Some("grok"), &opts(false, Some("user")), prompt.clone()).unwrap(),
            prompt
        );
        assert_eq!(
            with_launch_flags(None, &opts(false, Some("user")), prompt.clone()).unwrap(),
            prompt
        );
    }

    /// A build that does not know `--setting-sources` would refuse to start,
    /// or read "user" as the prompt, so a launch that will pass it asks the
    /// build first — and one that will not pass it does not.
    #[test]
    fn a_claude_build_must_advertise_setting_sources_only_when_they_are_chosen() {
        let version = on_stdout("2.1.289 (Claude Code)\n");
        let with = format!(
            "{CLAUDE_HELP}  --setting-sources <sources>  Comma-separated list of setting sources\n"
        );
        super::advertises(
            "claude",
            "ask",
            &opts(true, Some("user")),
            &version,
            &on_stdout(&with),
        )
        .unwrap();
        super::advertises(
            "claude",
            "ask",
            &opts(true, None),
            &version,
            &on_stdout(CLAUDE_HELP),
        )
        .unwrap();
        assert_eq!(
            super::advertises(
                "claude",
                "ask",
                &opts(true, Some("user")),
                &version,
                &on_stdout(CLAUDE_HELP)
            )
            .unwrap_err()
            .code,
            "unsupported_capability"
        );
    }

    #[test]
    fn codex_must_advertise_its_config_flag_only_when_it_will_be_passed() {
        let version = on_stdout("codex-cli 0.130.0\n");
        let base = "  -s, --sandbox <MODE>\n    [possible values: read-only, workspace-write]\n  -a, --ask-for-approval <POLICY>\n    - on-request: ask\n  -C, --cd <DIR>\n    Set root\n";
        let with_config = format!("  -c, --config <key=value>\n    Override a value\n{base}");
        super::advertises(
            "codex",
            "edit",
            &opts(true, None),
            &version,
            &on_stdout(&with_config),
        )
        .unwrap();
        super::advertises(
            "codex",
            "edit",
            &opts(false, None),
            &version,
            &on_stdout(base),
        )
        .unwrap();
        assert_eq!(
            super::advertises(
                "codex",
                "edit",
                &opts(true, None),
                &version,
                &on_stdout(base)
            )
            .unwrap_err()
            .code,
            "unsupported_capability"
        );
    }

    #[test]
    fn the_brief_lives_alone_in_a_private_directory_that_goes_with_it() {
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        assert_eq!(brief.path.parent(), Some(brief.dir.as_path()));
        assert_eq!(brief.dir.parent(), Some(root.path()));
        assert_eq!(std::fs::read_dir(&brief.dir).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&brief.dir), 0o700);
            assert_eq!(mode(&brief.path), 0o600);
        }
        let other = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        assert_ne!(other.dir, brief.dir);
        let (dir, path) = (brief.dir.clone(), brief.path.clone());
        drop(brief);
        assert!(!path.exists() && !dir.exists());
        assert!(other.path.exists());
        // An empty brief leaves nothing behind.
        assert!(BriefFile::under(root.path(), "").is_err());
        drop(other);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    /// A GitPulse that is SIGKILLed never runs `BriefFile`'s `Drop`, so its
    /// briefs outlive it. Before this sweep nothing removed them.
    #[test]
    fn a_crashed_writers_briefs_are_removed_and_nothing_else_is() {
        use super::remove_abandoned_briefs;
        use crate::workbench::process_birth::Liveness;

        let root = tempfile::tempdir().unwrap();
        // What a crash leaves: the brief on disk, the destructor never run.
        let crashed = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        let crashed_dir = crashed.dir.clone();
        std::mem::forget(crashed);

        // A matching name holding something a brief directory never does.
        let decoy = root.path().join("gitpulse-task-7-1-1");
        std::fs::create_dir(&decoy).unwrap();
        std::fs::write(decoy.join("task-brief.md"), "x").unwrap();
        std::fs::write(decoy.join("someone-elses.txt"), "keep me").unwrap();
        // A name that only looks like one, and a symlink at a matching name.
        let unrelated = root.path().join("gitpulse-task-notes");
        std::fs::create_dir(&unrelated).unwrap();
        #[cfg(unix)]
        let link = {
            let target = tempfile::tempdir().unwrap();
            let link = root.path().join("gitpulse-task-8-1-1");
            std::os::unix::fs::symlink(target.path(), &link).unwrap();
            (link, target)
        };

        // A writer that cannot be checked keeps everything.
        let kept = remove_abandoned_briefs(root.path(), |_, _| Liveness::Unknown("denied".into()));
        assert_eq!(kept, 0);
        assert!(
            crashed_dir.exists(),
            "an unchecked writer must never lose its brief"
        );
        // So does one that is still running.
        assert_eq!(
            remove_abandoned_briefs(root.path(), |_, _| Liveness::Alive),
            0
        );
        assert!(crashed_dir.exists());

        // A gone writer loses exactly the directory BriefFile made.
        let removed = remove_abandoned_briefs(root.path(), |_, _| Liveness::Gone);
        assert_eq!(removed, 1);
        assert!(!crashed_dir.exists(), "the crashed writer's brief survived");
        assert!(
            decoy.join("someone-elses.txt").exists(),
            "a foreign file was deleted"
        );
        assert!(unrelated.exists());
        #[cfg(unix)]
        {
            assert!(
                link.0.symlink_metadata().is_ok(),
                "a symlink was followed or removed"
            );
            assert!(link.1.path().exists());
        }
    }

    /// Against the real process table: this process wrote the brief and is
    /// alive, so its live brief is never swept from under it.
    #[test]
    fn a_live_writers_brief_is_kept_by_the_real_liveness_check() {
        let root = tempfile::tempdir().unwrap();
        let live = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        let removed = super::remove_abandoned_briefs(
            root.path(),
            crate::workbench::process_birth::running_since,
        );
        assert_eq!(removed, 0);
        assert!(live.path.exists());
    }

    #[test]
    #[ignore = "requires explicitly installed Claude Code, Codex, Grok and Antigravity binaries; probes help only"]
    fn installed_provider_help_supports_requested_modes() {
        let root = tempfile::tempdir().unwrap();
        for provider in ["claude", "codex", "grok", "agy"] {
            let program = super::program(provider).unwrap();
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                super::check(
                    &program,
                    root.path().to_str().unwrap(),
                    provider,
                    mode,
                    &opts(true, None),
                )
                .unwrap_or_else(|error| panic!("{provider}/{mode}: {}", error.message));
            }
        }
    }
    #[test]
    fn capability_values_belong_to_the_exact_advertised_option() {
        let help = "  -s, --sandbox <MODE>\n    [possible values: read-only, workspace-write]\n  -a, --ask-for-approval <POLICY>\n    - on-request: ask\n    - never: deny\n  -C, --cd <DIR>\n    Set root\n";
        assert!(super::advertised_option(
            help,
            "--sandbox",
            Some("workspace-write")
        ));
        assert!(!super::advertised_option(help, "--sandbox", Some("never")));
        assert!(super::advertised_option(
            help,
            "--ask-for-approval",
            Some("never")
        ));
        assert!(!super::advertised_option(help, "--ask", None));
        assert!(!super::advertised_option(
            "  --permission-mode-legacy <MODE>\n    auto\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option(
            "  --permission-mode <MODE>\n    automatic\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option("", "--cd", None));
    }
    #[cfg(unix)]
    #[test]
    fn probe_refuses_a_build_that_advertises_the_flag_without_the_requested_value() {
        let version = on_stdout("2.1.263 (Claude Code)\n");
        let help = on_stdout(CLAUDE_HELP);
        assert!(super::advertises("claude", "ask", &opts(false, None), &version, &help).is_ok());
        assert_eq!(
            super::advertises("claude", "bypass", &opts(false, None), &version, &help)
                .unwrap_err()
                .code,
            "unsupported_capability",
            "the flag is advertised but the value `bypass` needs is not"
        );
    }
    #[test]
    fn large_source_is_a_private_file_and_never_an_argument() {
        let root = tempfile::tempdir().unwrap();
        let text = "Unchanged $(never_execute) 🧪\n".repeat(20_000);
        let brief = BriefFile::under(root.path(), &text).unwrap();
        assert_eq!(std::fs::read_to_string(&brief.path).unwrap(), text);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&brief.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let args = arguments("codex", "edit", false, "/checkout with spaces", &brief.path).unwrap();
        assert!(args
            .iter()
            .all(|a| !a.contains("never_execute") && a.len() < 16 * 1024));
        assert!(args.iter().any(|a| a == "/checkout with spaces"));
        let path = brief.path.clone();
        drop(brief);
        assert!(!path.exists());
    }
    #[test]
    fn an_agent_that_may_edit_is_told_how_to_mark_its_task_done_and_an_inspector_is_not() {
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        for provider in ["codex", "claude", "grok", "agy"] {
            for mode in ["ask", "edit", "auto_review", "preapproved"] {
                let prompt = arguments(provider, mode, false, "/checkout", &brief.path)
                    .unwrap()
                    .pop()
                    .unwrap();
                assert!(
                    prompt.contains("gitpulse_complete_task"),
                    "{provider}/{mode}"
                );
                assert!(prompt.contains("Task: line"), "{provider}/{mode}");
                assert!(prompt.contains("status review"), "{provider}/{mode}");
                // Marking the task is allowed; publishing still is not.
                assert!(
                    prompt.contains("Do not publish changes"),
                    "{provider}/{mode}"
                );
            }
            let inspect = arguments(provider, "inspect", false, "/checkout", &brief.path)
                .unwrap()
                .pop()
                .unwrap();
            assert!(!inspect.contains("gitpulse_complete_task"), "{provider}");
            assert!(inspect.contains("change the task's status"), "{provider}");
        }
    }
    #[test]
    fn each_provider_mode_is_explicit_and_bypass_is_never_inherited() {
        for provider in ["codex", "claude", "grok", "agy"] {
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                let args = arguments(
                    provider,
                    mode,
                    mode == "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md"),
                )
                .unwrap();
                let names_bypass = args
                    .iter()
                    .any(|a| a.contains("bypass") || a.contains("dangerously-skip-permissions"));
                assert_eq!(
                    names_bypass,
                    mode == "bypass",
                    "{provider}/{mode}: {args:?}"
                );
                assert!(arguments(
                    provider,
                    mode,
                    mode != "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md")
                )
                .is_err());
            }
        }
        assert!(arguments(
            "shell",
            "edit",
            false,
            "/repo",
            std::path::Path::new("/brief.md")
        )
        .is_err());
    }
    #[test]
    fn grok_is_a_first_class_terminal_provider() {
        match super::program("grok") {
            Ok(path) => assert!(
                path.contains("grok"),
                "resolved grok path must name grok: {path}"
            ),
            Err(error) => assert_eq!(
                error.code, "not_installed",
                "an unknown provider is invalid_input; a missing install is not_installed: {} {}",
                error.code, error.message
            ),
        }
        assert_eq!(super::program("gemini").unwrap_err().code, "invalid_input");
        let args = arguments(
            "grok",
            "inspect",
            false,
            "/checkout with spaces",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode", "plan"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--cwd", "/checkout with spaces"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains("bypass")));
        let ask = arguments(
            "grok",
            "ask",
            false,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            ask.windows(2)
                .any(|pair| pair == ["--permission-mode", "default"]),
            "{ask:?}"
        );
    }
    #[test]
    fn antigravity_is_a_first_class_terminal_provider() {
        match super::program("agy") {
            Ok(path) => assert!(
                path.contains("agy"),
                "resolved agy path must name agy: {path}"
            ),
            Err(error) => assert_eq!(
                error.code, "not_installed",
                "an unknown provider is invalid_input; a missing install is not_installed: {} {}",
                error.code, error.message
            ),
        }
        let inspect = arguments(
            "agy",
            "inspect",
            false,
            "/checkout with spaces",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            inspect.windows(2).any(|pair| pair == ["--mode", "plan"]),
            "{inspect:?}"
        );
        assert!(
            inspect
                .windows(2)
                .any(|pair| pair[0] == "--prompt-interactive" && pair[1].contains("brief.md")),
            "{inspect:?}"
        );
        assert!(!inspect.iter().any(|a| a.contains("dangerously-skip")));
        let ask = arguments(
            "agy",
            "ask",
            false,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert_eq!(ask[0], "--prompt-interactive", "{ask:?}");
        let bypass = arguments(
            "agy",
            "bypass",
            true,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(bypass.iter().any(|a| a == "--dangerously-skip-permissions"));
    }
    #[test]
    fn grok_probe_requires_identity_permission_modes_and_cwd() {
        let version = on_stdout("grok 1.0.34 (deadbeef) [stable]\n");
        let help = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, acceptEdits, auto, dontAsk, bypassPermissions, plan]\n      --cwd <CWD>\n \
                      Working directory\n",
        );
        super::advertises("grok", "ask", &opts(false, None), &version, &help).unwrap();
        super::advertises("grok", "bypass", &opts(false, None), &version, &help).unwrap();

        // The same build without the mode `ask` needs, and without `--cwd`.
        let narrower = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, plan]\n",
        );
        assert_eq!(
            super::advertises("grok", "ask", &opts(false, None), &version, &narrower)
                .unwrap_err()
                .code,
            "unsupported_capability"
        );
    }

    #[test]
    fn antigravity_help_on_stderr_is_still_capability_proof() {
        // Help on stderr, version on stdout — the real `agy` layout, and the
        // one a reader of stdout alone would report as having no controls.
        let version = on_stdout("1.1.22\n");
        let help = on_stderr(
            "Usage of agy:\n  --mode                          Set the agent execution mode for \
             this session (accept-edits, plan)\n  --prompt-interactive            Run an initial \
             prompt interactively\n  --dangerously-skip-permissions  Auto-approve all tool \
             permission requests\n  --sandbox                       Run in a sandbox\n",
        );
        super::advertises("agy", "ask", &opts(false, None), &version, &help).unwrap();
        super::advertises("agy", "inspect", &opts(false, None), &version, &help).unwrap();
        super::advertises("agy", "bypass", &opts(false, None), &version, &help).unwrap();

        let without_controls =
            on_stderr("Usage of agy:\n  --prompt-interactive            prompt\n");
        assert_eq!(
            super::advertises(
                "agy",
                "ask",
                &opts(false, None),
                &version,
                &without_controls
            )
            .unwrap_err()
            .code,
            "unsupported_capability"
        );
    }

    /// An answer too large to be a version or a help screen is not one, however
    /// well it reads. Only a fake can produce this: a stub script that printed
    /// 128KiB would be testing the shell's buffering as much as the bound.
    #[test]
    fn an_unbounded_answer_is_not_capability_proof() {
        let version = on_stdout("grok 1.0.34 (deadbeef) [stable]\n");
        let help = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, acceptEdits, auto, dontAsk, bypassPermissions, plan]\n      --cwd <CWD>\n \
                      Working directory\n",
        );
        super::advertises("grok", "ask", &opts(false, None), &version, &help)
            .expect("the bounded case still passes");

        let mut flood = help.clone();
        flood.stdout.resize(128 * 1024 + 1, b' ');
        assert_eq!(
            super::advertises("grok", "ask", &opts(false, None), &version, &flood)
                .unwrap_err()
                .code,
            "capability_error",
            "an over-cap help screen is an unusable answer, not an unsupported build"
        );

        let mut chatty = version.clone();
        chatty.stderr.resize(1025, b' ');
        assert_eq!(
            super::advertises("grok", "ask", &opts(false, None), &chatty, &help)
                .unwrap_err()
                .code,
            "capability_error"
        );
    }

    /// What the provider printed, on the stream it printed it to. Building the
    /// answer directly is what takes these cases off the spawn path: the
    /// decision under test reads two byte strings and an exit status, and a
    /// `#!/bin/sh` stub is only one way — the load-sensitive way — to produce
    /// them.
    fn on_stdout(text: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
            success: true,
            status_code: 0,
        }
    }

    fn on_stderr(text: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: Vec::new(),
            stderr: text.as_bytes().to_vec(),
            success: true,
            status_code: 0,
        }
    }
}

#[cfg(test)]
mod model_tests {
    use super::{
        advertises, apply_permission_mode, launch_flags, model_fields, model_launchers,
        validate_model_choice, validate_model_id, with_launch_flags, BriefFile, CapturedOutput,
        Extras, LaunchOptions, EFFORT_LEVELS, MAX_FALLBACK_MODELS, MAX_MODEL_ID_LEN, MODEL_FIELDS,
    };
    use crate::tool_config::ModelChoice;

    fn s(value: &str) -> Option<String> {
        Some(value.to_owned())
    }

    fn full_claude() -> ModelChoice {
        ModelChoice {
            model: s("opus"),
            effort: s("high"),
            fallback: Some(vec!["sonnet".into(), "haiku".into()]),
            advisor: s("fable"),
        }
    }

    fn with_model(choice: &ModelChoice) -> LaunchOptions<'_> {
        LaunchOptions {
            model: Some(choice),
            ..LaunchOptions::default()
        }
    }

    fn settings_object(flags: &[String]) -> serde_json::Map<String, serde_json::Value> {
        let at = flags
            .iter()
            .position(|flag| flag == "--settings")
            .expect("no --settings");
        match serde_json::from_str(&flags[at + 1]).unwrap() {
            serde_json::Value::Object(object) => object,
            other => panic!("--settings was not an object: {other}"),
        }
    }

    fn on(text: &str, stderr: bool) -> CapturedOutput {
        let bytes = text.as_bytes().to_vec();
        CapturedOutput {
            stdout: if stderr { Vec::new() } else { bytes.clone() },
            stderr: if stderr { bytes } else { Vec::new() },
            success: true,
            status_code: 0,
        }
    }

    /// The options of Claude Code 2.1.289 a model launch uses, laid out the
    /// way its `--help` lays them out (continuation lines included, because
    /// that is where `--effort` lists its levels).
    const CLAUDE_HELP: &str = "  --add-dir <directories...>            Additional directories to allow tool\n                                        access to\n  --effort <level>                      Effort level for the current session\n                                        (low, medium, high, xhigh, max)\n  --fallback-model <model>              Enable automatic fallback to specified\n                                        model(s) when the default model is\n  --model <model>                       Model for the current session.\n  --permission-mode <mode>              Permission mode to use for the session\n                                        (choices: \"acceptEdits\", \"auto\",\n                                        \"bypassPermissions\", \"manual\",\n                                        \"dontAsk\", \"plan\")\n  --session-id <uuid>                   Use a specific session ID for the\n  --settings <file-or-json>             Path to a settings JSON file or a JSON\n";

    /// Antigravity 1.2.17's help, which it writes to stderr.
    const AGY_HELP: &str = "Usage of agy:\n  --dangerously-skip-permissions  Auto-approve all tool permission requests without prompting\n  --effort                        Reasoning effort for the current CLI session (low|medium|high|xhigh|max)\n  --mode                          Set the agent execution mode for this session (accept-edits, plan)\n  --model                         Model for the current CLI session\n  --prompt-interactive            Run an initial prompt interactively and continue the session\n  --sandbox                       Run in a sandbox with terminal restrictions enabled\n";

    #[test]
    fn each_launcher_takes_exactly_the_controls_its_cli_advertises() {
        assert_eq!(model_fields("claude"), MODEL_FIELDS.to_vec());
        assert_eq!(model_fields("agy"), vec!["model", "effort"]);
        assert_eq!(model_fields("codex"), vec!["model"]);
        assert_eq!(model_fields("grok"), vec!["model"]);
        for none in ["manvi", "shell", "", "Claude", "claude "] {
            assert!(model_fields(none).is_empty(), "{none:?}");
        }
        let launchers = model_launchers();
        assert_eq!(
            launchers.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["agy", "claude", "codex", "grok"]
        );
        // Every field of every launcher is in the vocabulary, in its order.
        for fields in launchers.values() {
            let positions: Vec<usize> = fields
                .iter()
                .map(|field| MODEL_FIELDS.iter().position(|f| f == field).unwrap())
                .collect();
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn a_full_claude_choice_is_its_own_flags_and_one_settings_key() {
        let choice = full_claude();
        let flags = launch_flags("claude", &with_model(&choice)).unwrap();
        assert_eq!(
            flags,
            vec![
                "--model",
                "opus",
                "--effort",
                "high",
                "--fallback-model",
                "sonnet,haiku",
                "--settings",
                r#"{"advisorModel":"fable"}"#,
            ]
        );
    }

    /// `--settings` takes one value, so a second occurrence would replace the
    /// first: choosing an advisor must not cost the notification channel.
    #[test]
    fn the_advisor_joins_the_notification_settings_rather_than_replacing_them() {
        let choice = full_claude();
        let flags = launch_flags(
            "claude",
            &LaunchOptions {
                notify: true,
                model: Some(&choice),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(flags.iter().filter(|f| *f == "--settings").count(), 1);
        let object = settings_object(&flags);
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["advisorModel", "preferredNotifChannel"]);
        assert_eq!(object["advisorModel"], "fable");
        assert_eq!(object["preferredNotifChannel"], "terminal_bell");

        // Without an advisor the notification object is untouched: only keys
        // the user chose are ever written.
        let plain = ModelChoice {
            model: s("sonnet"),
            ..ModelChoice::default()
        };
        let flags = launch_flags(
            "claude",
            &LaunchOptions {
                notify: true,
                model: Some(&plain),
                ..LaunchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            settings_object(&flags).keys().collect::<Vec<_>>(),
            ["preferredNotifChannel"]
        );
        // And an advisor alone is the one key.
        let advisor = ModelChoice {
            advisor: s("opus"),
            ..ModelChoice::default()
        };
        let flags = launch_flags("claude", &with_model(&advisor)).unwrap();
        assert_eq!(flags, vec!["--settings", r#"{"advisorModel":"opus"}"#]);
    }

    #[test]
    fn a_field_the_cli_does_not_take_is_refused_never_dropped() {
        for (launcher, choice) in [
            (
                "codex",
                ModelChoice {
                    effort: s("high"),
                    ..ModelChoice::default()
                },
            ),
            (
                "grok",
                ModelChoice {
                    fallback: Some(vec!["a".into()]),
                    ..ModelChoice::default()
                },
            ),
            (
                "agy",
                ModelChoice {
                    advisor: s("opus"),
                    ..ModelChoice::default()
                },
            ),
            (
                "agy",
                ModelChoice {
                    model: s("gemini-3.8-flash-high"),
                    fallback: Some(vec!["x".into()]),
                    ..ModelChoice::default()
                },
            ),
            ("manvi", full_claude()),
            ("shell", full_claude()),
        ] {
            assert!(
                launch_flags(launcher, &with_model(&choice)).is_err(),
                "{launcher}: {choice:?} was applied in part"
            );
            assert!(validate_model_choice(launcher, &choice).is_err());
        }
        // A model alone is fine for every terminal provider.
        for launcher in ["claude", "agy", "codex", "grok"] {
            let choice = ModelChoice {
                model: s("m-1"),
                ..ModelChoice::default()
            };
            assert_eq!(
                launch_flags(launcher, &with_model(&choice)).unwrap(),
                vec!["--model", "m-1"],
                "{launcher}"
            );
        }
    }

    #[test]
    fn an_empty_choice_adds_nothing() {
        let empty = ModelChoice::default();
        for launcher in ["claude", "agy", "codex", "grok"] {
            assert!(launch_flags(launcher, &with_model(&empty))
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn model_names_are_judged_by_shape() {
        for good in [
            "opus",
            "opus[1m]",
            "opusplan",
            "claude-opus-5-5",
            "claude-opus-4-1@20250805",
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-opus",
            "gemini-3.8-flash-high",
            "gpt_oss.120b",
            "9",
        ] {
            validate_model_id("model", good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        let long = "a".repeat(MAX_MODEL_ID_LEN + 1);
        for bad in [
            "",
            "-m",
            "--model",
            "--",
            "a,b",
            "a b",
            " opus",
            "opus\n",
            "op\u{0}us",
            "op\tus",
            "ópus",
            "opus;rm",
            "$(x)",
            "`x`",
            "a\"b",
            "{\"x\":1}",
            "[1m]",
            ".hidden",
            "/abs",
            long.as_str(),
        ] {
            assert!(validate_model_id("model", bad).is_err(), "{bad:?} passed");
        }
        assert!(validate_model_id("model", &"a".repeat(MAX_MODEL_ID_LEN)).is_ok());
    }

    #[test]
    fn effort_and_fallback_are_bounded() {
        for level in EFFORT_LEVELS {
            let choice = ModelChoice {
                effort: s(level),
                ..ModelChoice::default()
            };
            validate_model_choice("claude", &choice).unwrap();
            validate_model_choice("agy", &choice).unwrap();
        }
        for bad in [
            "",
            "HIGH",
            "ultra",
            "ultracode",
            "high ",
            "minimal",
            "-high",
        ] {
            let choice = ModelChoice {
                effort: s(bad),
                ..ModelChoice::default()
            };
            assert!(validate_model_choice("claude", &choice).is_err(), "{bad:?}");
        }
        let fallback = |list: Vec<&str>| ModelChoice {
            fallback: Some(list.into_iter().map(str::to_owned).collect()),
            ..ModelChoice::default()
        };
        assert!(validate_model_choice("claude", &fallback(vec![])).is_err());
        assert!(validate_model_choice("claude", &fallback(vec!["a", "a"])).is_err());
        assert!(validate_model_choice("claude", &fallback(vec!["a,b"])).is_err());
        let most: Vec<String> = (0..MAX_FALLBACK_MODELS).map(|i| format!("m{i}")).collect();
        let most_refs: Vec<&str> = most.iter().map(String::as_str).collect();
        validate_model_choice("claude", &fallback(most_refs.clone())).unwrap();
        let mut over = most_refs;
        over.push("extra");
        assert!(validate_model_choice("claude", &fallback(over)).is_err());
    }

    /// The prompt form `-- <text>` makes everything after it positional, so a
    /// model flag behind it would be prompt text and the session would run
    /// on the CLI's own model while looking configured.
    #[test]
    fn model_flags_precede_the_prompt_on_both_launch_paths() {
        let choice = full_claude();
        // A plain tab: policy, launch flags, then the caller's prompt.
        let tab = apply_permission_mode(
            Some("claude"),
            Some("edit"),
            false,
            with_launch_flags(
                Some("claude"),
                &LaunchOptions {
                    notify: true,
                    setting_sources: Some("user"),
                    model: Some(&choice),
                },
                Some(vec!["--".into(), "Fix the failing test".into()]),
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        let at = |args: &[String], flag: &str| args.iter().position(|a| a == flag).unwrap();
        let separator = at(&tab, "--");
        for flag in [
            "--permission-mode",
            "--model",
            "--effort",
            "--fallback-model",
            "--settings",
            "--setting-sources",
        ] {
            assert!(at(&tab, flag) < separator, "{flag} after `--`: {tab:?}");
        }
        assert!(
            at(&tab, "--permission-mode") < at(&tab, "--model"),
            "{tab:?}"
        );
        assert_eq!(tab.last().unwrap(), "Fix the failing test");

        // A task attempt.
        let root = tempfile::tempdir().unwrap();
        let brief = BriefFile::under(root.path(), "# Task brief v1\n").unwrap();
        let args = super::arguments(
            "claude",
            "edit",
            false,
            "/checkout",
            &brief.path,
            &Extras {
                run_id: None,
                brief_dir: Some(&brief.dir),
                launch: LaunchOptions {
                    notify: true,
                    setting_sources: None,
                    model: Some(&choice),
                },
            },
        )
        .unwrap();
        let separator = at(&args, "--");
        assert_eq!(separator, args.len() - 2, "{args:?}");
        for flag in [
            "--model",
            "--effort",
            "--fallback-model",
            "--settings",
            "--add-dir",
        ] {
            assert!(at(&args, flag) < separator, "{flag}: {args:?}");
        }
        assert_eq!(args.iter().filter(|a| *a == "--settings").count(), 1);

        // Antigravity: model flags come before `--prompt-interactive`, whose
        // value is the prompt.
        let agy = ModelChoice {
            model: s("gemini-3.8-flash-high"),
            effort: s("low"),
            ..ModelChoice::default()
        };
        let args = super::arguments(
            "agy",
            "edit",
            false,
            "/checkout",
            &brief.path,
            &Extras {
                launch: with_model(&agy),
                ..Extras::default()
            },
        )
        .unwrap();
        let prompt = at(&args, "--prompt-interactive");
        assert!(
            at(&args, "--model") < prompt && at(&args, "--effort") < prompt,
            "{args:?}"
        );
        assert_eq!(prompt, args.len() - 2);
    }

    /// The model controls a task launch will pass are the ones it proves.
    #[test]
    fn a_build_must_advertise_every_model_control_and_the_chosen_level() {
        let version = on("2.1.289 (Claude Code)\n", false);
        let help = on(CLAUDE_HELP, false);
        let choice = full_claude();
        advertises("claude", "ask", &with_model(&choice), &version, &help).unwrap();

        for missing in ["--model", "--effort", "--fallback-model", "--settings"] {
            let narrower: String = CLAUDE_HELP
                .lines()
                .filter(|line| !line.trim_start().starts_with(missing))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(
                advertises(
                    "claude",
                    "ask",
                    &with_model(&choice),
                    &version,
                    &on(&narrower, false)
                )
                .unwrap_err()
                .code,
                "unsupported_capability",
                "{missing}"
            );
        }

        // A build that lists fewer levels refuses the one it lacks, rather
        // than reading `xhigh` however it likes.
        let fewer = CLAUDE_HELP.replace("(low, medium, high, xhigh, max)", "(low, medium, high)");
        let xhigh = ModelChoice {
            effort: s("xhigh"),
            ..ModelChoice::default()
        };
        assert_eq!(
            advertises(
                "claude",
                "ask",
                &with_model(&xhigh),
                &version,
                &on(&fewer, false)
            )
            .unwrap_err()
            .code,
            "unsupported_capability"
        );
        let high = ModelChoice {
            effort: s("high"),
            ..ModelChoice::default()
        };
        advertises(
            "claude",
            "ask",
            &with_model(&high),
            &version,
            &on(&fewer, false),
        )
        .unwrap();

        // A level named only by a *different* option is not proof.
        let elsewhere = "  --effort <level>  Effort\n  --other  (xhigh)\n  --permission-mode <mode>  (manual)\n  --session-id <uuid>  id\n  --add-dir <d>  dir\n";
        assert!(advertises(
            "claude",
            "ask",
            &with_model(&xhigh),
            &version,
            &on(elsewhere, false)
        )
        .is_err());

        // An invalid choice is refused as input, before any help is read.
        let bad = ModelChoice {
            model: s("--dangerously-skip-permissions"),
            ..ModelChoice::default()
        };
        assert_eq!(
            advertises("claude", "ask", &with_model(&bad), &version, &help)
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }

    #[test]
    fn antigravity_proves_its_model_controls_from_help_on_stderr() {
        let version = on("1.2.17\n", false);
        let help = on(AGY_HELP, true);
        let choice = ModelChoice {
            model: s("gemini-3.8-flash-high"),
            effort: s("max"),
            ..ModelChoice::default()
        };
        advertises("agy", "ask", &with_model(&choice), &version, &help).unwrap();
        let older = on(
            &AGY_HELP
                .lines()
                .filter(|line| !line.contains("--effort") && !line.contains("--model "))
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
            true,
        );
        assert!(advertises("agy", "ask", &with_model(&choice), &version, &older).is_err());
        // Without a model choice the older build is still launchable.
        advertises("agy", "ask", &LaunchOptions::default(), &version, &older).unwrap();
    }

    /// The whole grid plus the ways a stored value could be wrong. Every call
    /// either applies exactly the table's flags or refuses; there is no third
    /// outcome, and in particular no partial application.
    #[test]
    fn every_model_input_either_applies_the_table_or_refuses() {
        let launchers = [
            "claude", "agy", "codex", "grok", "manvi", "shell", "", "CLAUDE",
        ];
        let values: Vec<String> = [
            "opus",
            "opus[1m]",
            "gemini-3.8-flash-high",
            "high",
            "xhigh",
            "-x",
            "--",
            "a,b",
            "a b",
            "",
            "\u{0}",
            "ópus",
            "--settings",
        ]
        .iter()
        .map(|v| (*v).to_owned())
        .chain([
            "m".repeat(MAX_MODEL_ID_LEN),
            "m".repeat(MAX_MODEL_ID_LEN + 1),
            "x".repeat(10 * 1024),
        ])
        .collect();
        let (mut applied, mut refused) = (0usize, 0usize);
        for launcher in launchers {
            for field in MODEL_FIELDS {
                for value in &values {
                    let mut choice = ModelChoice::default();
                    match field {
                        "model" => choice.model = Some(value.clone()),
                        "effort" => choice.effort = Some(value.clone()),
                        "fallback" => choice.fallback = Some(vec![value.clone()]),
                        _ => choice.advisor = Some(value.clone()),
                    }
                    let prompt = Some(vec!["--".to_owned(), "PROMPT".to_owned()]);
                    let Ok(out) = with_launch_flags(Some(launcher), &with_model(&choice), prompt)
                    else {
                        refused += 1;
                        assert!(validate_model_choice(launcher.trim(), &choice).is_err());
                        continue;
                    };
                    let out = out.unwrap();
                    applied += 1;
                    // Applied means: the launcher takes the field, the value
                    // is well-formed, and it sits before the separator.
                    assert!(
                        model_fields(launcher).contains(&field),
                        "{launcher}/{field}"
                    );
                    validate_model_choice(launcher, &choice).unwrap();
                    let separator = out.iter().position(|a| a == "--").unwrap();
                    assert_eq!(&out[separator..], ["--", "PROMPT"], "{out:?}");
                    for arg in &out[..separator] {
                        assert!(!arg.contains('\0') && arg.len() <= 1024, "{arg:?}");
                    }
                    let carried = if field == "advisor" {
                        out[..separator].iter().any(|a| a.contains(value.as_str()))
                    } else {
                        out[..separator].contains(value)
                    };
                    assert!(carried, "{launcher}/{field}/{value:?} lost: {out:?}");
                }
            }
        }
        assert!(applied > 0 && refused > 0, "a branch went untested");
        // Each terminal provider takes each of its fields with each valid
        // value: model/advisor/fallback take 6 of these values, effort 2.
        let expected: usize = ["claude", "agy", "codex", "grok"]
            .iter()
            .map(|launcher| {
                model_fields(launcher)
                    .iter()
                    .map(|field| if *field == "effort" { 2 } else { 6 })
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(applied, expected);
    }

    /// Reads the installed CLIs, so it runs only on request. It proves the
    /// claim the table's comment makes: each advertised control is in the
    /// real `--help`, and the effort levels are each listed.
    #[test]
    #[ignore = "requires installed Claude Code, Codex, Grok and Antigravity; reads --help only"]
    fn installed_clis_advertise_every_model_control_offered_for_them() {
        let root = tempfile::tempdir().unwrap();
        for launcher in ["claude", "agy", "codex", "grok"] {
            let program = super::program(launcher).unwrap();
            let fields = model_fields(launcher);
            let choice = ModelChoice {
                model: s("probe-model"),
                effort: fields.contains(&"effort").then(|| "xhigh".to_owned()),
                fallback: fields
                    .contains(&"fallback")
                    .then(|| vec!["probe-fallback".to_owned()]),
                advisor: fields.contains(&"advisor").then(|| "opus".to_owned()),
            };
            super::check(
                &program,
                root.path().to_str().unwrap(),
                launcher,
                "ask",
                &LaunchOptions {
                    notify: true,
                    setting_sources: None,
                    model: Some(&choice),
                },
            )
            .unwrap_or_else(|error| panic!("{launcher}: {}", error.message));
        }
    }
}

#[cfg(test)]
mod launch_choice_tests {
    use super::{launch_model_choice, model_choice_from_record, model_choice_record};
    use crate::tool_config::ModelChoice;
    use serde_json::json;

    fn choice(model: Option<&str>, effort: Option<&str>) -> ModelChoice {
        ModelChoice {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            ..ModelChoice::default()
        }
    }

    /// The handoff form overrides one launch, field by field, over the saved
    /// default; what it leaves out still comes from the default.
    #[test]
    fn a_launch_override_replaces_only_the_fields_it_names() {
        let default = ModelChoice {
            fallback: Some(vec!["sonnet".into()]),
            ..choice(Some("sonnet"), Some("medium"))
        };
        let launch = choice(Some("opus"), None);
        let effective = launch_model_choice("claude", Some(&default), Some(&launch))
            .unwrap()
            .unwrap();
        assert_eq!(effective.model.as_deref(), Some("opus"));
        assert_eq!(effective.effort.as_deref(), Some("medium"));
        assert_eq!(effective.fallback, Some(vec!["sonnet".into()]));
        // Neither side choosing anything is no choice, recorded as null.
        assert_eq!(launch_model_choice("claude", None, None).unwrap(), None);
        assert_eq!(
            launch_model_choice("codex", None, Some(&choice(Some("gpt-5"), None)))
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("gpt-5")
        );
    }

    /// Validated as a whole: an override that names a field the CLI does not
    /// take, or a level it does not have, is refused before an attempt exists.
    #[test]
    fn an_override_the_launcher_cannot_apply_is_refused() {
        assert!(launch_model_choice("codex", None, Some(&choice(None, Some("high")))).is_err());
        assert!(launch_model_choice("claude", None, Some(&choice(None, Some("extreme")))).is_err());
        assert!(
            launch_model_choice("claude", None, Some(&choice(Some("opus; rm -rf"), None))).is_err()
        );
    }

    /// What the store records on the run reads back as the same choice, so
    /// the launch applies exactly what the attempt says it ran on.
    #[test]
    fn the_recorded_choice_round_trips_through_the_run() {
        let original = ModelChoice {
            fallback: Some(vec!["sonnet".into(), "haiku".into()]),
            advisor: Some("opus".into()),
            ..choice(Some("opus[1m]"), Some("high"))
        };
        let record = model_choice_record(&original);
        assert_eq!(
            record,
            json!({"model":"opus[1m]","effort":"high","fallback":"sonnet,haiku","advisor":"opus"})
        );
        assert_eq!(model_choice_from_record(&record).unwrap(), Some(original));
        assert_eq!(model_choice_from_record(&json!(null)).unwrap(), None);
        assert!(model_choice_from_record(&json!({"temperature":"1"})).is_err());
        assert!(model_choice_from_record(&json!({"model":7})).is_err());
    }
}
