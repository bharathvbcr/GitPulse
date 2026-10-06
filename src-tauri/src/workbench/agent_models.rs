//! Which models each agent CLI can be started with, as each CLI says.
//!
//! The model settings row offers these as suggestions; the stored value stays
//! free text, because a list is only ever as complete as its source:
//!
//! * **Antigravity** lists models itself: `agy models` prints one
//!   `slug<TAB>label` line per model on stdout (1.2.17; "Fetching available
//!   models..." goes to stderr). It asks Antigravity's service, so it needs the
//!   network and the user's sign-in, and the list is per account.
//! * **Claude Code** publishes no list: it has no models command, hidden or
//!   public (2.1.289). The Anthropic API does (`/v1/models`), but only with
//!   credentials, and GitPulse does not read them. What it can say honestly is
//!   the CLI's own aliases plus the models the user's own Claude settings name
//!   — `availableModels` being Claude Code's word for the allowed set.
//! * **Codex** has no models command. **Grok** has `grok models`, which talks
//!   to a background leader process it may start; a settings pane must not
//!   start daemons, so it is not asked.
//!
//! A listing that failed is an `error` beside an empty list, never an empty
//! list on its own: "could not ask" and "has no models" are different answers.

use super::terminal_command::{self, validate_model_id};
use crate::engine::git_cli::CapturedOutput;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The most entries one catalog carries. Far above any real list; a CLI that
/// printed more is reported as truncated rather than rendered without bound.
pub(crate) const MAX_CATALOG_MODELS: usize = 256;
/// How long `agy models` may take. Measured at 1.2–4 s on a signed-in account.
const LISTING_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a successful listing is reused before the CLI is asked again.
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// The longest label kept. A longer one is dropped, never cut mid-word.
const MAX_LABEL_LEN: usize = 120;
/// The most of a failing CLI's stderr a reader is shown.
const MAX_ERROR_EXCERPT: usize = 300;
/// The largest Claude settings file read. Claude Code's own `--settings` cap.
const MAX_SETTINGS_BYTES: u64 = 2 * 1024 * 1024;

/// Claude Code's model aliases, read from the 2.1.289 binary's own alias
/// list (`sonnet, opus, haiku, fable, best, sonnet[1m], opus[1m], fable[1m],
/// opusplan`) plus `default`, in the order a reader is likely to want them.
/// The labels say only what the binary's own strings say.
const CLAUDE_ALIASES: [(&str, Option<&str>); 10] = [
    ("default", Some("Your account's default")),
    ("best", None),
    ("fable", None),
    ("opus", None),
    ("sonnet", None),
    ("haiku", None),
    ("opusplan", Some("Opus to plan, Sonnet to carry out")),
    ("fable[1m]", Some("1M-token context")),
    ("opus[1m]", Some("1M-token context")),
    ("sonnet[1m]", Some("1M-token context")),
];

/// One model a launcher can be started with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentModelOption {
    /// What the launch passes: an alias, a full name or a slug.
    pub id: String,
    /// What the CLI calls it, when it said.
    pub label: Option<String>,
    /// Where this entry came from: `cli` (the CLI listed it), `alias` (one of
    /// the CLI's built-in aliases), `settings` (named in the user's Claude
    /// settings) or `allowed` (in those settings' `availableModels`).
    pub source: String,
}

/// What a launcher's models are, and how that was found out.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentModelCatalog {
    pub launcher: String,
    pub models: Vec<AgentModelOption>,
    /// `listed` when the CLI listed its models; `known` when it has no way
    /// to, and these are its aliases and the user's own settings.
    pub listing: String,
    /// The command that produced the list, for a reader to run themselves.
    pub command: Option<String>,
    /// Milliseconds since the epoch when this answer was produced.
    pub fetched_at: u64,
    /// Whether this answer came from the cache rather than a fresh ask.
    pub cached: bool,
    /// Lines or entries refused because they were not a model name.
    pub skipped: u32,
    /// Whether entries past [`MAX_CATALOG_MODELS`] were left out.
    pub truncated: bool,
    /// Why the list could not be produced, when it could not.
    pub error: Option<String>,
}

/// Which launchers have a catalog and how it is produced. Derived for the
/// settings view, so the panel offers a model list exactly where there is one.
pub(crate) fn listing_kind(launcher: &str) -> Option<&'static str> {
    match launcher {
        "agy" => Some("listed"),
        "claude" => Some("known"),
        _ => None,
    }
}

/// Every launcher with a catalog, for the settings view.
pub(crate) fn listing_launchers() -> std::collections::BTreeMap<String, String> {
    crate::terminal::AGENT_LAUNCHERS
        .iter()
        .filter_map(|launcher| {
            listing_kind(launcher).map(|kind| ((*launcher).to_owned(), kind.to_owned()))
        })
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn catalog(launcher: &str, listing: &str, command: Option<&str>) -> AgentModelCatalog {
    AgentModelCatalog {
        launcher: launcher.to_owned(),
        models: Vec::new(),
        listing: listing.to_owned(),
        command: command.map(str::to_owned),
        fetched_at: now_ms(),
        cached: false,
        skipped: 0,
        truncated: false,
        error: None,
    }
}

impl AgentModelCatalog {
    /// Adds `id` once, refusing a malformed one and stopping at the cap.
    fn push(&mut self, id: &str, label: Option<String>, source: &str) {
        if validate_model_id("model", id).is_err() {
            self.skipped = self.skipped.saturating_add(1);
            return;
        }
        if self.models.iter().any(|model| model.id == id) {
            return;
        }
        if self.models.len() >= MAX_CATALOG_MODELS {
            self.truncated = true;
            return;
        }
        self.models.push(AgentModelOption {
            id: id.to_owned(),
            label,
            source: source.to_owned(),
        });
    }
}

/// A label made safe to render: printable, bounded, or absent.
fn clean_label(label: &str) -> Option<String> {
    let label = label.trim();
    (!label.is_empty()
        && label.chars().count() <= MAX_LABEL_LEN
        && !label.chars().any(char::is_control))
    .then(|| label.to_owned())
}

/// The first line of what a failing CLI said, bounded, for the reader.
fn excerpt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("Fetching available models"))
        .unwrap_or("");
    let line: String = line
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ERROR_EXCERPT)
        .collect();
    line
}

/// `agy models`' answer as a catalog. Pure, so every shape of answer — a
/// failure, a garbled line, a flood — is testable without the CLI.
pub(crate) fn agy_catalog(answer: Result<CapturedOutput, String>) -> AgentModelCatalog {
    let mut out = catalog("agy", "listed", Some("agy models"));
    let output = match answer {
        Ok(output) => output,
        Err(error) => {
            out.error = Some(format!(
                "agy models could not run: {}",
                excerpt(error.as_bytes())
            ));
            return out;
        }
    };
    if !output.success {
        let said = excerpt(&output.stderr);
        out.error = Some(if said.is_empty() {
            format!("agy models exited with status {}.", output.status_code)
        } else {
            format!(
                "agy models exited with status {}: {said}",
                output.status_code
            )
        });
        return out;
    }
    let Ok(text) = std::str::from_utf8(&output.stdout) else {
        out.error = Some("agy models printed something that is not UTF-8 text.".into());
        return out;
    };
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (slug, label) = line.split_once('\t').unwrap_or((line, ""));
        out.push(slug.trim(), clean_label(label), "cli");
    }
    if out.models.is_empty() {
        // Exit 0 with nothing usable is not "this account has no models":
        // every signed-in account has some, so the answer was not a listing.
        out.error = Some(if out.skipped > 0 {
            format!(
                "agy models printed {} line(s), none of them a model name.",
                out.skipped
            )
        } else {
            "agy models listed no models. Is Antigravity signed in?".to_owned()
        });
    }
    out
}

/// Claude Code's catalog: its aliases, then what the user's settings name.
/// `settings` is the user's `~/.claude/settings.json`, if it could be read.
pub(crate) fn claude_catalog(settings: Option<&[u8]>) -> AgentModelCatalog {
    let mut out = catalog("claude", "known", None);
    for (alias, label) in CLAUDE_ALIASES {
        out.push(alias, label.map(str::to_owned), "alias");
    }
    let Some(bytes) = settings else {
        return out;
    };
    let Ok(serde_json::Value::Object(settings)) = serde_json::from_slice(bytes) else {
        // An unreadable settings file costs its entries, not the aliases.
        out.skipped = out.skipped.saturating_add(1);
        return out;
    };
    for key in ["model", "advisorModel"] {
        if let Some(serde_json::Value::String(id)) = settings.get(key) {
            out.push(id.trim(), None, "settings");
        }
    }
    if let Some(serde_json::Value::Array(allowed)) = settings.get("availableModels") {
        for entry in allowed.iter().take(MAX_CATALOG_MODELS) {
            match entry {
                serde_json::Value::String(id) => out.push(id.trim(), None, "allowed"),
                _ => out.skipped = out.skipped.saturating_add(1),
            }
        }
    }
    out
}

fn read_claude_settings(home: Option<&std::ffi::OsStr>) -> Option<Vec<u8>> {
    let path = std::path::Path::new(home?)
        .join(".claude")
        .join("settings.json");
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    use std::io::Read;
    file.take(MAX_SETTINGS_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

static CACHE: Mutex<Option<HashMap<String, (Instant, AgentModelCatalog)>>> = Mutex::new(None);

/// `launcher`'s catalog. A successful CLI listing is reused for
/// [`CACHE_TTL`] unless `refresh`; a failure is never cached, so asking again
/// asks again. The lock is held across the listing on purpose: a second
/// request waits for the first and then reads its answer, rather than running
/// the CLI twice.
pub(crate) fn models_for(launcher: &str, refresh: bool) -> Result<AgentModelCatalog, String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match listing_kind(launcher) {
        Some("known") => Ok(claude_catalog(
            read_claude_settings(home.as_deref()).as_deref(),
        )),
        Some("listed") => {
            let mut cache = CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entries = cache.get_or_insert_with(HashMap::new);
            if !refresh {
                if let Some((at, answer)) = entries.get(launcher) {
                    if at.elapsed() < CACHE_TTL {
                        let mut answer = answer.clone();
                        answer.cached = true;
                        return Ok(answer);
                    }
                }
            }
            let answer = agy_catalog(list_with_cli(launcher));
            if answer.error.is_none() {
                entries.insert(launcher.to_owned(), (Instant::now(), answer.clone()));
            } else {
                entries.remove(launcher);
            }
            Ok(answer)
        }
        _ => Err(format!("{launcher} has no model list GitPulse can read.")),
    }
}

/// Runs the CLI's own listing command from the temporary directory: never the
/// open repository, because an agent CLI may write project files where it runs.
fn list_with_cli(launcher: &str) -> Result<CapturedOutput, String> {
    let program = terminal_command::program(launcher).map_err(|error| error.message)?;
    crate::engine::git_cli::capture_command(
        &program,
        &["models"],
        Some(&std::env::temp_dir()),
        LISTING_TIMEOUT,
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(
        stdout: &str,
        stderr: &str,
        success: bool,
        code: i32,
    ) -> Result<CapturedOutput, String> {
        Ok(CapturedOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success,
            status_code: code,
        })
    }

    /// The real 1.2.17 answer's shape: tab-separated, labels with spaces and
    /// parentheses, the progress line on stderr.
    const AGY: &str = "gemini-3.8-flash-high\tGemini 3.8 Flash (High)\ngemini-3.1-pro-low\tGemini 3.1 Pro (Low)\nclaude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\ngpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n";

    #[test]
    fn an_antigravity_listing_becomes_its_slugs_and_labels() {
        let out = agy_catalog(answer(AGY, "Fetching available models...\n", true, 0));
        assert_eq!(out.error, None);
        assert_eq!(out.listing, "listed");
        assert_eq!(out.command.as_deref(), Some("agy models"));
        assert_eq!(
            out.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "gemini-3.8-flash-high",
                "gemini-3.1-pro-low",
                "claude-opus-4-6-thinking",
                "gpt-oss-120b-medium"
            ]
        );
        assert_eq!(
            out.models[0].label.as_deref(),
            Some("Gemini 3.8 Flash (High)")
        );
        assert!(out.models.iter().all(|m| m.source == "cli"));
        assert_eq!((out.skipped, out.truncated), (0, false));
    }

    /// Every listed id is one a save would accept, so choosing a suggestion
    /// can never be refused.
    #[test]
    fn a_garbled_line_is_skipped_and_counted_never_offered() {
        let text = "good-1\tGood\n-flag\tLooks like an option\n\tNo slug\nhas space\tX\na,b\tComma\ngood-2\n\u{1b}[31mred\tEscape\ngood-1\tDuplicate\n   \n";
        let out = agy_catalog(answer(text, "", true, 0));
        assert_eq!(
            out.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["good-1", "good-2"]
        );
        assert_eq!(out.models[1].label, None);
        assert_eq!(out.skipped, 5);
        for model in &out.models {
            terminal_command::validate_model_id("model", &model.id).unwrap();
        }
    }

    #[test]
    fn a_failure_is_an_error_beside_an_empty_list_never_an_empty_list_alone() {
        for (result, says) in [
            (
                answer(
                    "",
                    "Fetching available models...\nError: not signed in\n",
                    false,
                    1,
                ),
                "not signed in",
            ),
            (answer("", "", false, 7), "status 7"),
            (
                answer("", "Fetching available models...\n", true, 0),
                "listed no models",
            ),
            (
                answer("-x\n--y\n", "", true, 0),
                "none of them a model name",
            ),
            (Err("timed out after 20s".to_owned()), "timed out"),
            (
                Ok(CapturedOutput {
                    stdout: vec![0xff, 0xfe],
                    stderr: vec![],
                    success: true,
                    status_code: 0,
                }),
                "not UTF-8",
            ),
        ] {
            let out = agy_catalog(result);
            assert!(out.models.is_empty());
            let error = out.error.expect("a failed listing reported no error");
            assert!(error.contains(says), "{error:?} should say {says:?}");
            assert!(error.len() < MAX_ERROR_EXCERPT + 80, "{error}");
        }
    }

    #[test]
    fn a_flood_is_bounded_and_says_so() {
        let text: String = (0..MAX_CATALOG_MODELS + 50)
            .map(|i| format!("m-{i}\tModel {i}\n"))
            .collect();
        let out = agy_catalog(answer(&text, "", true, 0));
        assert_eq!(out.models.len(), MAX_CATALOG_MODELS);
        assert!(out.truncated);
        assert_eq!(out.error, None);
        let long_label = format!("x\t{}\n", "L".repeat(MAX_LABEL_LEN + 1));
        assert_eq!(
            agy_catalog(answer(&long_label, "", true, 0)).models[0].label,
            None
        );
        let noisy = format!("bad\t{}\n", "e".repeat(10_000));
        let failed = agy_catalog(answer("", &noisy, false, 2)).error.unwrap();
        assert!(failed.len() < MAX_ERROR_EXCERPT + 80);
    }

    #[test]
    fn claude_offers_its_aliases_then_what_the_users_settings_name() {
        let out = claude_catalog(None);
        assert_eq!(out.listing, "known");
        assert_eq!(out.command, None);
        assert_eq!(out.error, None);
        assert_eq!(out.models.len(), CLAUDE_ALIASES.len());
        assert!(out.models.iter().all(|m| m.source == "alias"));
        for (alias, _) in CLAUDE_ALIASES {
            terminal_command::validate_model_id("model", alias).unwrap();
        }

        let settings = br#"{"model":"claude-opus-5-5","advisorModel":"fable","availableModels":["sonnet","claude-sonnet-5-5"," us.anthropic.claude-opus-4-1-20250805-v1:0 ",7,"--x"],"effortLevel":"high"}"#;
        let out = claude_catalog(Some(settings));
        let tail: Vec<(&str, &str)> = out.models[CLAUDE_ALIASES.len()..]
            .iter()
            .map(|m| (m.id.as_str(), m.source.as_str()))
            .collect();
        assert_eq!(
            tail,
            [
                ("claude-opus-5-5", "settings"),
                ("claude-sonnet-5-5", "allowed"),
                ("us.anthropic.claude-opus-4-1-20250805-v1:0", "allowed"),
            ]
        );
        // `fable` and `sonnet` are aliases already: listed once, as aliases.
        assert_eq!(out.models.iter().filter(|m| m.id == "sonnet").count(), 1);
        assert_eq!(out.skipped, 2, "7 and --x");
    }

    #[test]
    fn an_unreadable_claude_settings_file_costs_its_entries_not_the_aliases() {
        for bad in [&b"{not json"[..], b"[1,2]", b"\"opus\"", b""] {
            let out = claude_catalog(Some(bad));
            assert_eq!(out.models.len(), CLAUDE_ALIASES.len(), "{bad:?}");
            assert_eq!(out.error, None);
            assert_eq!(out.skipped, 1);
        }
        let wrong_shapes = br#"{"model":7,"advisorModel":null,"availableModels":"opus"}"#;
        assert_eq!(
            claude_catalog(Some(wrong_shapes)).models.len(),
            CLAUDE_ALIASES.len()
        );
    }

    #[test]
    fn claude_settings_are_read_from_home_and_bounded() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_claude_settings(Some(home.path().as_os_str())), None);
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        let path = home.path().join(".claude").join("settings.json");
        std::fs::write(&path, br#"{"model":"opus"}"#).unwrap();
        assert_eq!(
            read_claude_settings(Some(home.path().as_os_str())).as_deref(),
            Some(&br#"{"model":"opus"}"#[..])
        );
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_SETTINGS_BYTES + 1).unwrap();
        assert_eq!(read_claude_settings(Some(home.path().as_os_str())), None);
        assert_eq!(read_claude_settings(None), None);
    }

    #[test]
    fn only_launchers_with_a_real_source_have_a_catalog() {
        let launchers = listing_launchers();
        assert_eq!(
            launchers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            [("agy", "listed"), ("claude", "known")]
        );
        for launcher in ["codex", "grok", "manvi", "shell", ""] {
            assert!(models_for(launcher, false).is_err(), "{launcher}");
        }
        // Every launcher with a catalog takes a model.
        for launcher in launchers.keys() {
            assert!(terminal_command::model_fields(launcher).contains(&"model"));
        }
    }

    /// Reads the installed CLI, so it runs only on request. It proves the
    /// parser against the real answer, not a transcription of it.
    #[test]
    #[ignore = "requires an installed, signed-in Antigravity; makes a network call"]
    fn the_installed_antigravity_lists_models_this_parser_reads() {
        let out = models_for("agy", true).unwrap();
        assert_eq!(out.error, None, "{out:?}");
        assert!(!out.models.is_empty());
        assert_eq!(out.skipped, 0, "a real line was refused: {out:?}");
        let again = models_for("agy", false).unwrap();
        assert!(again.cached);
        assert_eq!(again.models, out.models);
    }
}
