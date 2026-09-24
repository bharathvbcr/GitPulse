//! Local AI features: commit messages, commit explanations, branch names.
//!
//! Explanations, branch names, health plans and coverage reports run against a
//! model server on this machine. Commit messages are classified from the staged
//! patch first (`commit_brief`); a model only phrases that classification, and
//! the classified text is the message when no model can be reached. The pieces
//! are split three ways, and the split is the point:
//!
//! * `discovery` and `http` find the server and carry the request. Loopback
//!   only — the transport itself refuses any other address, so a diff cannot
//!   leave the machine through a mistyped setting.
//! * The MANVI harness answers the questions a host cannot answer for itself:
//!   how large this model's context really is and where that number came from
//!   (`capability.probe`), whether the assembled request fits (`chat.prepare`),
//!   and what the reply actually contains once thinking tags and unparsed tool
//!   calls are separated out (`chat.settle`).
//! * `prompt` decides what to send and reads the answer back.
//!
//! ## What is and is not covered by tests
//!
//! `generate_commit_message` is exercised here against a loopback server, against
//! a server that refuses the connection, and against a reply that invents an
//! issue number. The other entry points (`explain_commit`, `suggest_branch_name`,
//! `fix_health`, `coverage_report`) still need a model server; the live check
//! for a real one is `tests/local_ai_live.rs`, gated behind `GITPULSE_LIVE_AI=1`.
//!
//! What the model server can do to this application is covered without it.
//! `http` is tested against adversarial servers — including one that dribbles
//! bytes forever, to prove the overall deadline holds — and against every
//! refusal the transport makes, in `tests/ai_transport_integration.rs`.
//! `prompt` parses replies as pure functions. So the untrusted half is
//! exercised; what is not is the orchestration between them.
//!
//! With no harness installed the features still work, on a declared context
//! window and a local reply parser, and every result says which of the two it
//! was — `AiGeneration::warnings` carries the difference rather than hiding it.

pub mod apple;
pub mod commit_brief;
pub mod discovery;
pub mod http;
pub mod prompt;

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::git_cli::{git_text, git_text_capped, validate_repo};
use crate::engine::GitReader;
use crate::harness::protocol::{
    PrepareResult, ProbeResult, SettleResult, OP_CAPABILITY_PROBE, OP_CHAT_PREPARE, OP_CHAT_SETTLE,
    OP_LOCAL_SCAN,
};
use crate::harness::sidecar;
use crate::harness::HarnessStatus;

/// Context window assumed when nothing better is known. Matches the harness's
/// own declared fallback, so the two do not disagree about an unprobed server.
const FALLBACK_CONTEXT_WINDOW: i64 = 32_768;
/// Tokens held back from the window for the system prompt, the instructions
/// and the reply itself.
const RESERVED_TOKENS: i64 = 2_048;
/// Cap on concise replies such as commit messages and branch names.
const MAX_OUTPUT_TOKENS: i64 = 1_024;
/// Coverage and dependency reports can make thinking-capable models consume
/// the concise cap before reaching their visible answer. Keep those advisory
/// replies bounded too, but leave enough room for reasoning plus the requested
/// plan. The visible answer is still required and reasoning is never promoted.
const MAX_ADVISORY_OUTPUT_TOKENS: i64 = 4_096;
/// A local model on a laptop can take a while on first token: the model may
/// have to be loaded into memory before it generates anything.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(180);
const PROBE_TIMEOUT_MS: i64 = 6_000;

/// One AI feature's selected endpoint and model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiSelection {
    pub base_url: String,
    pub model: String,
}

/// What the AI panel shows: what is installed, what is running, what it serves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiStatus {
    pub harness: HarnessStatus,
    pub endpoints: Vec<discovery::DiscoveredEndpoint>,
    pub selected: Option<AiSelection>,
    /// The selected model's dimensions, when the harness could probe them.
    pub model_info: Option<ProbeResult>,
    /// Why there is no model info, when there is none.
    pub model_detail: String,
    /// True when a request could be made right now.
    pub ready: bool,
    pub detail: String,
}

/// The token budget a request went out under.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetReport {
    /// True when the harness planned the budget; false when it was estimated
    /// here because the harness could not be reached.
    pub planned_by_harness: bool,
    pub before_tokens: i64,
    pub threshold_tokens: i64,
    /// True when the request exceeded the threshold and could not be shrunk
    /// further — the reply may be truncated by the server's window.
    pub insufficient: bool,
    pub calibration_samples: i64,
}

/// One completed generation, with the provenance of every number in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiGeneration {
    pub text: String,
    /// Thinking the harness separated out, kept for display but never used as
    /// the answer.
    pub reasoning: String,
    pub model: String,
    pub base_url: String,
    pub context_window: i64,
    /// Where the context window came from: a server endpoint, or a default.
    pub context_source: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// True when the server stopped the reply at the output cap.
    pub truncated: bool,
    pub diff_truncated: bool,
    pub diff_used_bytes: i64,
    pub diff_total_bytes: i64,
    pub budget: BudgetReport,
    /// Everything the user should know that is not the answer itself.
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
}

/// Which feature is asking. Also scopes the harness's calibration ledger, so
/// commit messages and explanations calibrate separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    CommitMessage,
    ExplainCommit,
    BranchName,
    HealthFix,
    /// Its own feature rather than a HealthFix mode: the calibration ledger is
    /// keyed per feature, so coverage analyses calibrate on their report shape
    /// instead of inheriting dependency-plan estimates.
    CoverageReport,
}

impl Feature {
    fn slug(self) -> &'static str {
        match self {
            Feature::CommitMessage => "commit-message",
            Feature::ExplainCommit => "explain-commit",
            Feature::BranchName => "branch-name",
            Feature::HealthFix => "health-fix",
            Feature::CoverageReport => "coverage-report",
        }
    }

    fn max_output_tokens(self) -> i64 {
        match self {
            Feature::HealthFix | Feature::CoverageReport => MAX_ADVISORY_OUTPUT_TOKENS,
            Feature::CommitMessage | Feature::ExplainCommit | Feature::BranchName => {
                MAX_OUTPUT_TOKENS
            }
        }
    }
}

/// Upper bound on tracked calibration sessions. Session ids pair a feature
/// with a repo path, so a long-lived instance opened over hundreds of
/// repositories would otherwise grow the map without limit; oldest-inserted
/// eviction keeps it bounded and deterministic (same pattern as the churn
/// cache in engine/git_reader.rs).
const OBSERVED_TOKENS_CAPACITY: usize = 512;

/// Prompt tokens the server reported for the previous request in a session.
///
/// This is what makes the harness's estimator self-correcting: it runs high
/// against a real tokenizer, and feeding back the server's own count is how it
/// converges. Keyed by session id, so each feature calibrates on its own shape
/// of request. Bounded at [`OBSERVED_TOKENS_CAPACITY`] entries; eviction is
/// oldest-inserted, and re-observing a session refreshes its recency.
struct ObservedTokens {
    map: HashMap<String, i64>,
    order: VecDeque<String>,
}

impl ObservedTokens {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &str) -> Option<i64> {
        self.map.get(key).copied()
    }

    fn insert(&mut self, key: String, value: i64) {
        if self.map.insert(key.clone(), value).is_some() {
            self.order.retain(|existing| existing != &key);
        }
        self.order.push_back(key);
        while self.order.len() > OBSERVED_TOKENS_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.order.len()
    }
}

fn observed_tokens() -> &'static Mutex<ObservedTokens> {
    static OBSERVED: OnceLock<Mutex<ObservedTokens>> = OnceLock::new();
    OBSERVED.get_or_init(|| Mutex::new(ObservedTokens::new()))
}

fn session_id(feature: Feature, repo_path: &str) -> String {
    format!("gitpulse:{}:{}", feature.slug(), repo_path)
}

/// Positive probes live a minute; negative ones only ten seconds, so a model
/// server that was still starting up is asked again soon rather than locked
/// out by its own earlier absence.
const PROBE_CACHE_TTL: Duration = Duration::from_secs(60);
const PROBE_CACHE_NEGATIVE_TTL: Duration = Duration::from_secs(10);

/// One capability probe, kept until its TTL runs out.
#[derive(Clone)]
struct CachedProbe {
    fetched_at: Instant,
    outcome: Result<ProbeResult, String>,
}

impl CachedProbe {
    fn fresh(&self) -> bool {
        let ttl = if self.outcome.is_ok() {
            PROBE_CACHE_TTL
        } else {
            PROBE_CACHE_NEGATIVE_TTL
        };
        self.fetched_at.elapsed() < ttl
    }
}

fn probe_cache() -> &'static Mutex<HashMap<(String, String), CachedProbe>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), CachedProbe>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Asks the harness for a model's real dimensions, memoized per
/// (base_url, model).
///
/// `status` polls and every generation used to re-run the probe — up to ten
/// seconds each — for dimensions that rarely change within a session. A hit
/// inside the TTL answers instantly; expired entries and unknown endpoints
/// re-probe, and a failed probe is remembered only briefly so a server that
/// comes back is noticed quickly.
fn probe_model(base_url: &str, model: &str) -> Result<ProbeResult, String> {
    let key = (base_url.to_string(), model.to_string());
    if let Some(hit) = probe_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).filter(|entry| entry.fresh()).cloned())
    {
        return hit.outcome;
    }
    let outcome = fetch_probe(base_url, model);
    if let Ok(mut cache) = probe_cache().lock() {
        // Entries for (url, model) pairs nobody asks about anymore would
        // otherwise accumulate one per selection ever made; drop them here.
        cache.retain(|_, entry| entry.fresh());
        cache.insert(
            key,
            CachedProbe {
                fetched_at: Instant::now(),
                outcome: outcome.clone(),
            },
        );
    }
    outcome
}

/// The uncached probe: one harness `capability.probe` round trip.
fn fetch_probe(base_url: &str, model: &str) -> Result<ProbeResult, String> {
    #[cfg(test)]
    if let Some(outcome) = test_probe_override(base_url, model) {
        return outcome;
    }
    let params = serde_json::json!({
        "base_url": base_url,
        "model": model,
        "declared_context_window": FALLBACK_CONTEXT_WINDOW,
        "max_output_tokens": MAX_ADVISORY_OUTPUT_TOKENS,
        "timeout_ms": PROBE_TIMEOUT_MS,
    });
    sidecar::call_typed::<ProbeResult>(
        OP_CAPABILITY_PROBE,
        params,
        Duration::from_millis(PROBE_TIMEOUT_MS as u64 + 4_000),
    )
    .map_err(|e| e.message())
}

/// Test seam standing in for the harness's `capability.probe`, so cache
/// behavior is testable without a sidecar. Exists only in test builds.
#[cfg(test)]
type FakeProbe = std::sync::Arc<dyn Fn(&str, &str) -> Result<ProbeResult, String> + Send + Sync>;

#[cfg(test)]
static FAKE_PROBE: Mutex<Option<FakeProbe>> = Mutex::new(None);

#[cfg(test)]
fn test_probe_override(base_url: &str, model: &str) -> Option<Result<ProbeResult, String>> {
    let fake = FAKE_PROBE.lock().ok().and_then(|slot| slot.clone())?;
    Some(fake(base_url, model))
}

/// Discovers what is available, without sending a prompt anywhere.
pub fn status(explicit_base_url: Option<&str>, preferred_model: Option<&str>) -> AiStatus {
    let harness = HarnessStatus::probe();
    let endpoints = discovery::sweep(explicit_base_url);
    let chosen = discovery::choose(&endpoints, preferred_model);

    let (selected, model_info, model_detail) = match chosen {
        Some((endpoint, model)) => {
            let selection = AiSelection {
                base_url: endpoint.base_url.clone(),
                model: model.clone(),
            };
            match probe_model(&selection.base_url, &selection.model) {
                Ok(info) => (Some(selection), Some(info), String::new()),
                Err(detail) => (Some(selection), None, detail),
            }
        }
        None => (None, None, String::new()),
    };

    let embedding_only = model_info.as_ref().is_some_and(|m| m.embedding);
    let ready = selected.is_some() && !embedding_only;
    let detail = if selected.is_none() {
        let tried: Vec<String> = endpoints
            .iter()
            .filter(|e| !e.reachable)
            .map(|e| format!("{} ({})", e.base_url, e.detail))
            .collect();
        format!(
            "No local model server answered. Tried: {}",
            if tried.is_empty() {
                "nothing".to_string()
            } else {
                tried.join(", ")
            }
        )
    } else if embedding_only {
        "The selected model is an embedding model and cannot generate text.".to_string()
    } else {
        String::new()
    };

    AiStatus {
        harness,
        endpoints,
        selected,
        model_info,
        model_detail,
        ready,
        detail,
    }
}

/// One assembled request, before it goes out.
struct Turn {
    system: String,
    user: String,
    diff: prompt::BudgetedDiff,
}

/// Runs one feature end to end.
fn run(
    feature: Feature,
    repo_path: &str,
    selection: Option<AiSelection>,
    build: impl Fn(usize) -> Result<Turn, String>,
) -> Result<AiGeneration, String> {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let feature_output_cap = feature.max_output_tokens();

    // 1. Where to send it.
    let selection = match selection.filter(|s| !s.base_url.is_empty() && !s.model.is_empty()) {
        Some(s) => s,
        None => {
            let endpoints = discovery::sweep(None);
            let (endpoint, model) = discovery::choose(&endpoints, None).ok_or_else(|| {
                let tried: Vec<String> = endpoints
                    .iter()
                    .map(|e| format!("{} ({})", e.base_url, e.detail))
                    .collect();
                format!(
                    "No local model server is running. Tried: {}. Start Ollama, LM Studio, \
                     llama.cpp, vLLM or Jan, then try again.",
                    tried.join(", ")
                )
            })?;
            AiSelection {
                base_url: endpoint.base_url.clone(),
                model,
            }
        }
    };

    // 2. How much it can hold, and who says so.
    let (context_window, context_source, max_output) =
        match probe_model(&selection.base_url, &selection.model) {
            Ok(info) => {
                if info.embedding {
                    return Err(format!(
                        "'{}' is an embedding model and cannot generate text. Choose a chat model.",
                        selection.model
                    ));
                }
                let max_output = if info.max_output_tokens > 0 {
                    info.max_output_tokens.min(feature_output_cap)
                } else {
                    feature_output_cap
                };
                (info.context_window.max(1_024), info.describe, max_output)
            }
            Err(detail) => {
                warnings.push(format!(
                    "Context window not probed ({}); assuming {} tokens.",
                    detail, FALLBACK_CONTEXT_WINDOW
                ));
                (
                    FALLBACK_CONTEXT_WINDOW,
                    format!(
                        "{} tokens (declared default, not probed)",
                        FALLBACK_CONTEXT_WINDOW
                    ),
                    feature_output_cap,
                )
            }
        };

    // 3. Assemble, then let the harness say whether it fits. It plans the
    //    budget; shrinking the diff is ours to do, because the diff is ours.
    let mut budget_bytes = prompt::diff_budget_bytes(context_window, RESERVED_TOKENS + max_output);
    let mut turn = build(budget_bytes)?;
    let session = session_id(feature, repo_path);
    let observed = observed_tokens()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session)
        .unwrap_or(0);

    let mut budget = BudgetReport::default();
    for attempt in 0..2 {
        match prepare(&session, &turn, context_window, max_output, observed) {
            Ok(plan) => {
                budget = BudgetReport {
                    planned_by_harness: true,
                    before_tokens: plan.before_tokens,
                    threshold_tokens: plan.threshold_tokens,
                    insufficient: plan.insufficient,
                    calibration_samples: plan.calibration_samples,
                };
                let over = plan.threshold_tokens > 0 && plan.before_tokens > plan.threshold_tokens;
                if over && attempt == 0 {
                    // Halve what we offer and ask again, rather than sending a
                    // request the model cannot hold.
                    budget_bytes /= 2;
                    turn = build(budget_bytes)?;
                    continue;
                }
                if over || plan.insufficient {
                    warnings.push(format!(
                        "Request is about {} tokens against a {} token compaction threshold; the \
                         reply may be cut short.",
                        plan.before_tokens, plan.threshold_tokens
                    ));
                }
                break;
            }
            Err(detail) => {
                warnings.push(format!(
                    "Token budget not planned by the harness ({}).",
                    detail
                ));
                break;
            }
        }
    }

    // 4. The request itself. The host owns this call: the harness's chat plane
    //    is advisory and never makes it.
    let completion = complete(&selection, &turn, max_output)?;

    if completion.prompt_tokens > 0 {
        observed_tokens()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.clone(), completion.prompt_tokens);
    }

    // 5. Read the reply. The harness separates thinking from answer and
    //    checks the server's truncation claim against the token counts.
    let (text, reasoning, truncated) = match settle(&turn, &completion, max_output) {
        Ok(settled) => {
            if !settled.format.is_empty() {
                warnings.push(format!(
                    "Reply arrived in '{}' form: this server has no native tool parser for the \
                     model it serves.",
                    settled.format
                ));
            }
            if settled.prefill_disproved {
                warnings.push(
                    "Server delivered reasoning on its own channel; the thinking-prefill \
                     assumption was dropped for this reply."
                        .into(),
                );
            }
            (settled.text, settled.reasoning, settled.truncated)
        }
        Err(detail) => {
            warnings.push(format!(
                "Reply parsed locally rather than by the harness ({}).",
                detail
            ));
            let stripped = prompt::strip_think_tags(&completion.content);
            let truncated = completion.finish_reason == "length"
                || (max_output > 0 && completion.completion_tokens >= max_output);
            (stripped, completion.reasoning.clone(), truncated)
        }
    };

    if truncated {
        warnings.push("The model stopped at the output cap, so the reply is incomplete.".into());
    }
    if turn.diff.truncated {
        warnings.push(format!(
            "Attached context exceeded the window: {} of {} bytes were shown{}.",
            turn.diff.used_bytes,
            turn.diff.total_bytes,
            if turn.diff.omitted_files > 0 {
                format!(", {} file(s) not shown", turn.diff.omitted_files)
            } else {
                String::new()
            }
        ));
    }

    Ok(AiGeneration {
        text,
        reasoning,
        model: selection.model,
        base_url: selection.base_url,
        context_window,
        context_source,
        prompt_tokens: completion.prompt_tokens,
        completion_tokens: completion.completion_tokens,
        truncated,
        diff_truncated: turn.diff.truncated,
        diff_used_bytes: turn.diff.used_bytes as i64,
        diff_total_bytes: turn.diff.total_bytes as i64,
        budget,
        warnings,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn prepare(
    session: &str,
    turn: &Turn,
    context_window: i64,
    reserved_output: i64,
    observed: i64,
) -> Result<PrepareResult, String> {
    let params = serde_json::json!({
        "session_id": session,
        "system": turn.system,
        "messages": [{"role": "user", "text": turn.user}],
        "context_window": context_window,
        "reserved_output": reserved_output,
        "observed_prompt_tokens": observed,
    });
    sidecar::call_typed::<PrepareResult>(OP_CHAT_PREPARE, params, sidecar::DEFAULT_CALL_TIMEOUT)
        .map_err(|e| e.message())
}

fn settle(turn: &Turn, completion: &Completion, max_output: i64) -> Result<SettleResult, String> {
    let _ = turn;
    let params = serde_json::json!({
        "content": completion.content,
        "server_parsed_calls": false,
        "reasoning_out_of_band": !completion.reasoning.is_empty(),
        "output_tokens": completion.completion_tokens,
        "max_tokens_applied": max_output,
        "finish_reason": completion.finish_reason,
    });
    sidecar::call_typed::<SettleResult>(OP_CHAT_SETTLE, params, sidecar::DEFAULT_CALL_TIMEOUT)
        .map_err(|e| e.message())
}

struct Completion {
    content: String,
    reasoning: String,
    finish_reason: String,
    prompt_tokens: i64,
    completion_tokens: i64,
}

fn complete(selection: &AiSelection, turn: &Turn, max_output: i64) -> Result<Completion, String> {
    let endpoint = http::parse_base_url(&selection.base_url)?;
    let body = serde_json::json!({
        "model": selection.model,
        "messages": [
            {"role": "system", "content": turn.system},
            {"role": "user", "content": turn.user},
        ],
        "temperature": 0.2,
        "top_p": 0.95,
        "max_tokens": max_output,
        "stream": false,
    })
    .to_string();

    let response = http::request(
        &endpoint,
        "POST",
        "/chat/completions",
        Some(&body),
        COMPLETION_TIMEOUT,
    )?;
    if response.status != 200 {
        return Err(format!(
            "{} answered HTTP {} for model '{}': {}",
            selection.base_url,
            response.status,
            selection.model,
            truncate(response.body.trim(), 300)
        ));
    }

    let parsed: Value = serde_json::from_str(&response.body)
        .map_err(|e| format!("model server returned a non-JSON reply: {}", e))?;
    let choice = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| {
            format!(
                "model server returned no choices: {}",
                truncate(&response.body, 300)
            )
        })?;
    let message = choice.get("message").unwrap_or(&Value::Null);

    Ok(Completion {
        content: message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reasoning: message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        prompt_tokens: parsed
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        completion_tokens: parsed
            .get("usage")
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// How much of a staged patch is read for classification.
///
/// Past this the patch is cut and the message says so. The cut is what keeps a
/// multi-megabyte diff from being treated as a complete description of the change.
const COMMIT_PATCH_CAP: usize = 2 * 1024 * 1024;

/// The staged patch, and whether the read stopped early.
fn read_staged_patch(repo_path: &str) -> Result<(String, bool), String> {
    let repo = validate_repo(repo_path)?;
    let (text, incomplete) = git_text_capped(
        &repo,
        &["diff", "--cached", "--no-color", "--no-ext-diff", "-U3"],
        COMMIT_PATCH_CAP,
    )?;
    Ok((text, incomplete.is_some()))
}

/// Which writer phrases the subject. The patch has already been classified.
#[derive(Debug)]
enum CommitEngine {
    Local(AiSelection),
    OnDevice,
    Patch,
}

fn usable_selection(selection: Option<AiSelection>) -> Option<AiSelection> {
    selection.filter(|item| !item.base_url.is_empty() && !item.model.is_empty())
}

/// The on-device model is asked only when it has something to decide.
///
/// A cut patch is missing files, so a phrase written from it overclaims. A
/// high-confidence classification already has its type and subject; the
/// on-device guides are written for task titles and will not keep a frozen
/// `docs:` prefix reliably. Low-confidence edits are the case where a short
/// rephrase can choose `feat` or `fix` from the brief.
fn should_ask_on_device(
    no_server: bool,
    ready: bool,
    patch_truncated: bool,
    high_confidence: bool,
) -> bool {
    no_server && ready && !patch_truncated && !high_confidence
}

/// A server the caller named wins, then one found on loopback, then the
/// on-device model, then the message already written from the patch.
fn choose_commit_engine(
    explicit: Option<AiSelection>,
    discovered: Option<AiSelection>,
    on_device: bool,
) -> CommitEngine {
    if let Some(selection) = usable_selection(explicit) {
        return CommitEngine::Local(selection);
    }
    if let Some(selection) = usable_selection(discovered) {
        return CommitEngine::Local(selection);
    }
    if on_device {
        return CommitEngine::OnDevice;
    }
    CommitEngine::Patch
}

/// The working-tree diff against HEAD, staged or not.
fn working_diff(repo_path: &str) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    let staged = git_text(
        &repo,
        &["diff", "--cached", "--no-color", "--no-ext-diff", "-U3"],
    )?;
    let unstaged = git_text(&repo, &["diff", "--no-color", "--no-ext-diff", "-U3"])?;
    Ok(format!("{}{}", staged, unstaged))
}

/// Writes a commit message for the staged changes.
///
/// The patch is classified first, and that classification is already a
/// message. A loopback model, when one is selected or discovered, phrases the
/// message from the brief plus a budgeted diff. Otherwise Apple Intelligence
/// may phrase the subject from the brief alone. If neither can, the classified
/// message is the result — a missing model is not an empty composer.
pub fn generate_commit_message(
    repo_path: &str,
    selection: Option<AiSelection>,
) -> Result<AiGeneration, String> {
    let started = Instant::now();
    let (diff, patch_truncated) = read_staged_patch(repo_path)?;
    if diff.trim().is_empty() {
        if patch_truncated {
            return Err(
                "The staged patch could not be read, so there is no change to describe.".into(),
            );
        }
        return Err("Nothing is staged, so there is no change to describe.".into());
    }

    let (staged_paths, status_error) = match GitReader::get_status(repo_path) {
        Ok(rows) => (
            Some(
                rows.into_iter()
                    .filter(|status| status.is_staged)
                    .map(|status| status.path)
                    .collect::<Vec<_>>(),
            ),
            None,
        ),
        Err(error) => (None, Some(error)),
    };
    let (branch, branch_error) = match GitReader::list_branches(repo_path) {
        Ok(branches) => (
            branches
                .into_iter()
                .find(|item| item.is_current)
                .map(|item| item.name)
                .unwrap_or_default(),
            None,
        ),
        Err(error) => (String::new(), Some(error)),
    };
    let (subjects, history_error) = match GitReader::read_commit_history(repo_path, 12, None) {
        Ok(commits) => (
            commits
                .into_iter()
                .map(|commit| commit.summary)
                .collect::<Vec<_>>(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error)),
    };
    let style = prompt::style_hint_from_history(&subjects);
    let mut draft = commit_brief::draft_change(
        &diff,
        staged_paths.as_deref(),
        status_error.as_deref(),
        &subjects,
        &branch,
        patch_truncated,
    );
    if let Some(error) = branch_error {
        draft
            .warnings
            .push(format!("The current branch could not be read ({error})."));
    }
    if let Some(error) = history_error {
        draft.warnings.push(format!(
            "Recent commit subjects could not be read ({error})."
        ));
    }

    let explicit = usable_selection(selection);
    let (discovered, on_device) = if explicit.is_some() {
        (None, false)
    } else {
        let discovered =
            discovery::choose(&discovery::sweep(None), None).map(|(endpoint, model)| AiSelection {
                base_url: endpoint.base_url.clone(),
                model,
            });
        // A cut patch is not something the small model should phrase: the brief
        // would be missing files and the subject would overclaim.
        let on_device = discovered.is_none()
            && should_ask_on_device(
                true,
                !patch_truncated && !draft.high_confidence && apple::status().ready(),
                patch_truncated,
                draft.high_confidence,
            );
        (discovered, on_device)
    };

    match choose_commit_engine(explicit, discovered, on_device) {
        CommitEngine::Local(selection) => Ok(finish_with_local_model(
            repo_path, selection, &draft, &style, &branch, &diff, started,
        )),
        CommitEngine::OnDevice => Ok(finish_on_device(&draft, started)),
        CommitEngine::Patch => Ok(generation_from_patch(&draft, started, None)),
    }
}

fn finish_with_local_model(
    repo_path: &str,
    selection: AiSelection,
    draft: &commit_brief::CommitDraft,
    style: &str,
    branch: &str,
    diff: &str,
    started: Instant,
) -> AiGeneration {
    let outcome = run(
        Feature::CommitMessage,
        repo_path,
        Some(selection),
        |budget| {
            let budgeted = prompt::budget_diff(diff, budget);
            Ok(Turn {
                system: prompt::commit_message_system(style),
                user: prompt::commit_message_user(branch, &draft.brief, &budgeted.text),
                diff: budgeted,
            })
        },
    );
    match outcome {
        Ok(mut generation) => {
            match commit_brief::guard_local_message(&generation.text, draft) {
                Ok(guarded) => {
                    generation.text = guarded.text;
                    if guarded.dropped_lines > 0 {
                        generation.warnings.push(format!(
                            "Dropped {} line(s) that added an issue number or a trailer the patch does not contain.",
                            guarded.dropped_lines
                        ));
                    }
                }
                Err(why) => {
                    generation.text = draft.message.clone();
                    generation.warnings.push(format!(
                        "The model reply was not used ({why}). The message was written from the patch."
                    ));
                }
            }
            if generation.text.trim().is_empty() {
                generation.text = draft.message.clone();
                generation.warnings.push(
                    "The model returned an empty commit message. The message was written from the patch."
                        .into(),
                );
            }
            generation.warnings.extend(draft.warnings.iter().cloned());
            generation
        }
        Err(error) => generation_from_patch(
            draft,
            started,
            Some(&format!(
                "The model could not be reached ({error}). The message was written from the patch."
            )),
        ),
    }
}

fn finish_on_device(draft: &commit_brief::CommitDraft, started: Instant) -> AiGeneration {
    let request = apple::AppleIntelligenceRequest {
        kind: "commit_subject".to_string(),
        fields: vec!["title".to_string()],
        notes: draft.brief.clone(),
        title: draft.subject.clone(),
        description: String::new(),
        // The brief already names the branch. A second copy would spend the
        // small window on a fact the model has.
        context: String::new(),
    };
    match apple::generate(&request) {
        Ok(result) => {
            match commit_brief::accept_on_device_subject(
                result.title.as_deref().unwrap_or(""),
                draft,
            ) {
                Ok(subject) => {
                    let mut generation = generation_from_patch(draft, started, None);
                    generation.text = commit_brief::assemble(&subject, &draft.body);
                    generation.model = "apple-intelligence".to_string();
                    generation.base_url = "on-device".to_string();
                    generation.context_window = apple::ON_DEVICE_CONTEXT_TOKENS;
                    generation.context_source =
                        "on-device model; the patch was classified in the app and was not sent"
                            .to_string();
                    generation.warnings.push(
                        "The subject was phrased on this Mac. The body was written from the patch."
                            .into(),
                    );
                    generation
                }
                Err(why) => generation_from_patch(
                    draft,
                    started,
                    Some(&format!(
                        "Apple Intelligence replied, but the subject was not used ({why}). The message was written from the patch."
                    )),
                ),
            }
        }
        Err(error) => generation_from_patch(
            draft,
            started,
            Some(&format!(
                "Apple Intelligence did not phrase the subject ({}). The message was written from the patch.",
                error.message
            )),
        ),
    }
}

fn generation_from_patch(
    draft: &commit_brief::CommitDraft,
    started: Instant,
    extra: Option<&str>,
) -> AiGeneration {
    let mut warnings = draft.warnings.clone();
    if let Some(extra) = extra {
        warnings.push(extra.to_string());
    }
    let bytes = i64::try_from(draft.patch_bytes).unwrap_or(i64::MAX);
    AiGeneration {
        text: draft.message.clone(),
        reasoning: String::new(),
        model: "gitpulse".to_string(),
        base_url: String::new(),
        context_window: 0,
        context_source: "written from the patch; no model ran".to_string(),
        prompt_tokens: 0,
        completion_tokens: 0,
        truncated: false,
        diff_truncated: draft.patch_truncated,
        diff_used_bytes: bytes,
        diff_total_bytes: bytes,
        budget: BudgetReport::default(),
        warnings,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// Explains an existing commit.
pub fn explain_commit(
    repo_path: &str,
    commit_id: &str,
    selection: Option<AiSelection>,
) -> Result<AiGeneration, String> {
    let details = GitReader::get_commit_details(repo_path, commit_id)?;
    // `.text` only: the AI budget already trims the diff to the model's
    // context, and a truncation notice inside the prompt would read as commit
    // content. The generation's own warnings carry the fact instead.
    let diff = GitReader::get_commit_diff(repo_path, commit_id)?;
    let diff_truncation_reason = diff.truncation_reason.clone();
    let diff = diff.text;
    let subject = details.summary.clone();
    let author = format!("{} <{}>", details.author_name, details.author_email);
    let date = details.author_date.clone();
    let body = details.body.clone();

    let mut generation = run(Feature::ExplainCommit, repo_path, selection, |budget| {
        let budgeted = prompt::budget_diff(&diff, budget);
        Ok(Turn {
            system: prompt::explain_system(),
            user: prompt::explain_user(&subject, &author, &date, &body, &budgeted.text),
            diff: budgeted,
        })
    })?;
    generation.text = prompt::strip_think_tags(&generation.text);
    if generation.text.is_empty() {
        return Err("The model returned an empty explanation.".into());
    }
    if let Some(reason) = &diff_truncation_reason {
        // An explanation written from a prefix must not read as one written
        // from the whole commit — and it must not name the wrong reason for
        // the prefix either. This said "exceeded the read budget" for every
        // cut, including a diff the engine simply failed to read to the end,
        // which points whoever reads the warning at the wrong remedy.
        generation.warnings.push(format!(
            "This commit's diff {reason}, so the explanation covers only the part that \
             was read."
        ));
    }
    Ok(generation)
}

/// Suggests a branch name for the work in progress.
pub fn suggest_branch_name(
    repo_path: &str,
    selection: Option<AiSelection>,
) -> Result<AiGeneration, String> {
    let diff = working_diff(repo_path)?;
    if diff.trim().is_empty() {
        return Err(
            "The working tree is clean, so there is no work to name a branch after.".into(),
        );
    }
    let mut generation = run(Feature::BranchName, repo_path, selection, |budget| {
        let budgeted = prompt::budget_diff(&diff, budget.min(24_000));
        Ok(Turn {
            system: prompt::branch_name_system(),
            user: format!(
                "Changes in progress:\n```diff\n{}\n```\n\nSuggest one branch name.",
                budgeted.text
            ),
            diff: budgeted,
        })
    })?;
    generation.text = prompt::clean_branch_name(&generation.text);
    if generation.text.is_empty() {
        return Err("The model returned no usable branch name.".into());
    }
    Ok(generation)
}

/// Asks the local model for a remediation plan for a dependency-health report.
///
/// The report arrives already rendered by the frontend's formatter; this only
/// budgets it against the context window. The plan is advisory output — no
/// fix is ever applied by GitPulse itself.
pub fn fix_health(
    repo_path: &str,
    report_text: &str,
    selection: Option<AiSelection>,
) -> Result<AiGeneration, String> {
    if report_text.trim().is_empty() {
        return Err("The health report is empty, so there is nothing to plan a fix for.".into());
    }
    run_advisory_with_empty_retry("remediation plan", |retry| {
        run(Feature::HealthFix, repo_path, selection.clone(), |budget| {
            let budgeted = prompt::budget_text(report_text, budget.min(48_000));
            let mut system = prompt::health_fix_system();
            append_visible_answer_retry(&mut system, retry);
            Ok(Turn {
                system,
                user: prompt::health_fix_user(&budgeted.text),
                diff: budgeted,
            })
        })
    })
}

/// Asks the local model for an analysis of a rendered test-coverage report.
///
/// The report arrives already rendered by the frontend's formatter; this only
/// budgets it against the context window. The output is advisory — GitPulse
/// never applies anything itself.
pub fn coverage_report(
    repo_path: &str,
    report_text: &str,
    selection: Option<AiSelection>,
) -> Result<AiGeneration, String> {
    if report_text.trim().is_empty() {
        return Err("The coverage report is empty, so there is nothing to analyze.".into());
    }
    run_advisory_with_empty_retry("coverage analysis", |retry| {
        run(
            Feature::CoverageReport,
            repo_path,
            selection.clone(),
            |budget| {
                let budgeted = prompt::budget_text(report_text, budget.min(48_000));
                let mut system = prompt::coverage_report_system();
                append_visible_answer_retry(&mut system, retry);
                Ok(Turn {
                    system,
                    user: prompt::coverage_report_user(&budgeted.text),
                    diff: budgeted,
                })
            },
        )
    })
}

/// Some thinking-capable local models occasionally spend a complete turn in
/// reasoning and return no visible answer. Never promote chain-of-thought into
/// user-facing text; retry exactly once with an explicit visible-answer
/// instruction, preserve the first attempt's warnings, and fail loudly if the
/// second completion is also empty.
fn run_advisory_with_empty_retry(
    answer_name: &str,
    mut generate: impl FnMut(bool) -> Result<AiGeneration, String>,
) -> Result<AiGeneration, String> {
    let mut prior_warnings = Vec::new();
    for retry in [false, true] {
        let mut generation = generate(retry)?;
        generation.text = prompt::strip_think_tags(&generation.text)
            .trim()
            .to_string();
        if !generation.text.is_empty() {
            if retry {
                prior_warnings.push(format!(
                    "The first model completion contained no visible {answer_name}; GitPulse retried once."
                ));
            }
            prior_warnings.append(&mut generation.warnings);
            generation.warnings = prior_warnings;
            return Ok(generation);
        }
        prior_warnings.append(&mut generation.warnings);
    }
    Err(format!(
        "The model returned an empty {answer_name} twice; GitPulse stopped after one bounded retry."
    ))
}

fn append_visible_answer_retry(system: &mut String, retry: bool) {
    if retry {
        system.push_str(
            "\nIMPORTANT: The previous completion contained reasoning but no visible answer. \
             Return the requested visible final answer now; do not return reasoning alone.",
        );
    }
}

/// Discovers the model servers running on this machine.
///
/// Wraps the harness's `local.scan`, whose probe identifies each runtime by
/// asking it rather than by assuming whichever one conventionally holds the
/// port that answered. Capabilities are requested: the extra request per model
/// is worth it for a listing an operator reads and chooses from, and without
/// them every model would render with its capability flags unknown.
pub fn scan_local_servers() -> Result<crate::harness::ScanResult, String> {
    let params = serde_json::json!({
        "timeout_ms": LOCAL_SCAN_TIMEOUT_MS,
        "capabilities": true,
    });
    sidecar::call_typed::<crate::harness::ScanResult>(
        OP_LOCAL_SCAN,
        params,
        // The harness bounds each endpoint probe; this bounds the whole call,
        // with room for the sweep plus one model listing per server.
        Duration::from_millis(LOCAL_SCAN_TIMEOUT_MS as u64 + 6_000),
    )
    .map_err(|e| e.message())
}

/// Per-endpoint probe budget for a discovery sweep.
///
/// Well under the harness's own 30s ceiling: dispatch is serial, so this is
/// how long a scan may hold a policy verdict behind it, and a mutation waiting
/// on model discovery is the wrong trade.
const LOCAL_SCAN_TIMEOUT_MS: i64 = 1_500;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Serializes the tests below: they swap the fake probe and share the
    /// process-wide cache.
    static PROBE_TEST_LOCK: Mutex<()> = Mutex::new(());

    type Hits = Arc<AtomicUsize>;

    /// Installs a fake uncached probe that counts every call, so a test can
    /// assert exactly how many probes a sequence of `probe_model` calls ran.
    fn install_fake_probe(outcome: Result<ProbeResult, String>) -> Hits {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let per_call = outcome;
        *FAKE_PROBE.lock().expect("fake probe registry") = Some(Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            per_call.clone()
        }));
        hits
    }

    struct ClearFakeProbe;
    impl Drop for ClearFakeProbe {
        fn drop(&mut self) {
            *FAKE_PROBE.lock().expect("fake probe registry") = None;
        }
    }

    /// A loopback model server that answers one `/chat/completions` and hands
    /// the request body back, so a test can assert what was actually sent.
    ///
    /// Real socket, real HTTP, real JSON: the transport is part of what these
    /// tests exist to cover, and a mocked `complete()` would prove nothing
    /// about the request the model server receives.
    fn fake_model_server(content: &str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let (tx, rx) = std::sync::mpsc::channel();
        let body = serde_json::json!({
            "choices": [{
                "message": { "content": content },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 7 },
        })
        .to_string();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read headers, then exactly Content-Length bytes of body.
            let mut raw = Vec::new();
            let mut byte = [0u8; 1];
            while !raw.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => raw.push(byte[0]),
                    _ => return,
                }
            }
            let head = String::from_utf8_lossy(&raw).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let mut payload = vec![0u8; length];
            if stream.read_exact(&mut payload).is_err() {
                return;
            }
            let _ = tx.send(String::from_utf8_lossy(&payload).to_string());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (format!("http://127.0.0.1:{port}/v1"), rx)
    }

    /// A repository with one staged change, which is what the commit-message
    /// feature requires before it will say anything at all.
    fn repo_with_staged_change() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .expect("git runs");
        };
        run(&["init", "-b", "main"]);
        std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "chore: seed"]);
        std::fs::write(dir.path().join("README.md"), "# hello\n").unwrap();
        run(&["add", "README.md"]);
        crate::test_support::trust_repo(dir.path());
        dir
    }

    fn probing_selection(base_url: String) -> (AiSelection, Hits) {
        let hits = install_fake_probe(Ok(ProbeResult {
            context_window: 8192,
            describe: "fake probe".into(),
            ..Default::default()
        }));
        (
            AiSelection {
                base_url,
                model: "test-model".into(),
            },
            hits,
        )
    }

    /// The whole pipeline with NO harness installed — which is the ordinary
    /// case for a user who has never run MANVI, and was covered by nothing:
    /// `local_ai_live.rs` needs a real model server and is opt-in, so in CI
    /// every one of these five entry points was exercised by no test at all.
    #[test]
    fn generate_commit_message_runs_end_to_end_and_admits_the_harness_did_not() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;
        let _no_harness = NoHarness::install();

        let repo = repo_with_staged_change();
        let (base_url, requests) = fake_model_server("feat: add a readme");
        let (selection, _hits) = probing_selection(base_url);

        let generation = generate_commit_message(&repo.path().to_string_lossy(), Some(selection))
            .expect("generation succeeds without a harness");

        assert_eq!(generation.text, "feat: add a readme");
        assert_eq!(generation.prompt_tokens, 11);
        assert_eq!(generation.completion_tokens, 7);

        // The harness could not be asked. Degrading is correct; degrading
        // SILENTLY is not — an unparsed reply must never read like a parsed
        // one, so the absence has to reach the caller as a warning.
        assert!(
            generation
                .warnings
                .iter()
                .any(|w| w.contains("parsed locally rather than by the harness")),
            "no warning that the harness never ran: {:?}",
            generation.warnings
        );

        // The staged change must actually reach the model, or the feature is
        // describing nothing.
        let sent = requests.recv_timeout(TEST_WAIT).expect("request captured");
        assert!(sent.contains("README.md"), "diff not sent: {sent}");
        assert!(
            sent.contains("Factual brief") || sent.contains("Classified staged change"),
            "classified brief not sent: {sent}"
        );
        assert!(sent.contains("test-model"), "model not sent: {sent}");
    }

    #[test]
    fn commit_message_refuses_to_invent_one_for_an_empty_stage() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;

        // Nothing staged: the feature must refuse rather than ask a model to
        // describe a change that does not exist.
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        crate::test_support::trust_repo(dir.path());
        let error = generate_commit_message(
            &dir.path().to_string_lossy(),
            Some(AiSelection {
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "unused".into(),
            }),
        )
        .expect_err("empty stage must refuse");
        assert!(error.contains("Nothing is staged"), "{error}");
    }

    #[test]
    fn commit_engine_prefers_a_named_server_then_discovery_then_on_device() {
        let named = AiSelection {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "named".into(),
        };
        let found = AiSelection {
            base_url: "http://127.0.0.1:10/v1".into(),
            model: "found".into(),
        };
        match choose_commit_engine(Some(named), Some(found.clone()), true) {
            CommitEngine::Local(selection) => assert_eq!(selection.model, "named"),
            other => panic!("a named server must win: {other:?}"),
        }
        match choose_commit_engine(None, Some(found), true) {
            CommitEngine::Local(selection) => assert_eq!(selection.model, "found"),
            other => panic!("a discovered server must beat the on-device model: {other:?}"),
        }
        assert!(matches!(
            choose_commit_engine(None, None, true),
            CommitEngine::OnDevice
        ));
        assert!(matches!(
            choose_commit_engine(None, None, false),
            CommitEngine::Patch
        ));
        // An empty selection is "none", not a server with a blank name.
        assert!(matches!(
            choose_commit_engine(Some(AiSelection::default()), None, false),
            CommitEngine::Patch
        ));
    }

    #[test]
    fn the_on_device_model_is_asked_only_when_the_patch_left_the_type_open() {
        assert!(should_ask_on_device(true, true, false, false));
        assert!(!should_ask_on_device(false, true, false, false), "a server wins");
        assert!(!should_ask_on_device(true, false, false, false), "not ready");
        assert!(
            !should_ask_on_device(true, true, true, false),
            "a cut patch must not be phrased"
        );
        assert!(
            !should_ask_on_device(true, true, false, true),
            "a settled classification must not be rephrased"
        );
    }

    /// A staged change is enough to describe. A model server that cannot be
    /// reached must not be the reason the composer stays empty.
    #[test]
    fn a_dead_model_still_describes_the_staged_diff() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;
        let _no_harness = NoHarness::install();

        let repo = repo_with_staged_change();
        let generation = generate_commit_message(
            &repo.path().to_string_lossy(),
            Some(AiSelection {
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "unused".into(),
            }),
        )
        .expect("a staged change still has a message");

        assert_eq!(generation.text, "docs: add readme");
        assert!(
            generation
                .warnings
                .iter()
                .any(|warning| warning.to_ascii_lowercase().contains("patch")),
            "the fallback must say the message came from the patch: {:?}",
            generation.warnings
        );
        assert_ne!(generation.model, "unused");
    }

    /// The model may phrase the subject. It may not attach an issue or a
    /// co-author the patch does not contain.
    #[test]
    fn an_invented_issue_number_is_not_kept() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;
        let _no_harness = NoHarness::install();

        let repo = repo_with_staged_change();
        let (base_url, _requests) = fake_model_server(
            "feat: add a readme\n\nFixes #404\n\nCo-authored-by: Mallory <m@example.com>\n",
        );
        let (selection, _hits) = probing_selection(base_url);
        let generation = generate_commit_message(&repo.path().to_string_lossy(), Some(selection))
            .expect("generation succeeds");

        assert!(
            generation.text.starts_with("feat: add a readme"),
            "subject was discarded: {}",
            generation.text
        );
        assert!(
            !generation.text.contains("404"),
            "invented issue survived: {}",
            generation.text
        );
        assert!(
            !generation
                .text
                .to_ascii_lowercase()
                .contains("co-authored-by"),
            "invented trailer survived: {}",
            generation.text
        );
        assert!(
            generation.warnings.iter().any(|warning| {
                let lower = warning.to_ascii_lowercase();
                lower.contains("issue") || lower.contains("trailer") || lower.contains("co-author")
            }),
            "no warning that a line was dropped: {:?}",
            generation.warnings
        );
    }

    #[test]
    fn suggest_branch_name_and_explain_commit_reach_the_model() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;
        let _no_harness = NoHarness::install();

        let repo = repo_with_staged_change();
        let root = repo.path().to_string_lossy().to_string();

        let (base_url, requests) = fake_model_server("add-readme");
        let (selection, _hits) = probing_selection(base_url);
        let branch =
            suggest_branch_name(&root, Some(selection)).expect("branch suggestion succeeds");
        assert!(!branch.text.is_empty(), "empty branch name");
        let sent = requests.recv_timeout(TEST_WAIT).expect("request captured");
        // `working_diff` folds staged and unstaged together, so the change in
        // flight has to reach the model or the name describes nothing.
        assert!(sent.contains("README.md"), "diff not sent: {sent}");

        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo.path())
                .output()
                .expect("rev-parse runs")
                .stdout,
        )
        .expect("utf8 oid")
        .trim()
        .to_string();
        let (base_url, requests) = fake_model_server("This commit seeds the repository.");
        let (selection, _hits) = probing_selection(base_url);
        let explained = explain_commit(&root, &head, Some(selection)).expect("explain succeeds");
        assert!(explained.text.contains("seeds the repository"));
        assert!(requests.recv_timeout(TEST_WAIT).is_ok(), "no request sent");
    }

    /// Generous: it bounds a hang, it does not measure speed.
    const TEST_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

    /// Forces "no harness installed" for the duration of a test.
    ///
    /// Without this the outcome depends on whether a real `manvi` happens to
    /// be on the developer's PATH: on this machine one is, so the first run of
    /// these tests silently exercised the harness-present path and asserted
    /// nothing about the degraded one. A test whose meaning changes with the
    /// machine is the same defect as a check that quietly does not run.
    struct NoHarness(
        // Field order is drop order: the override is removed while the serial
        // guard is still held.
        crate::harness::sidecar::TestBinaryBinding,
        #[allow(dead_code)] crate::harness::sidecar::SidecarTestGuard,
    );
    impl NoHarness {
        fn install() -> Self {
            let guard = crate::harness::sidecar::test_serial();
            let binary = crate::harness::sidecar::bind_test_binary(
                &guard,
                "/nonexistent/gitpulse-test/manvi",
            );
            Self(binary, guard)
        }
    }

    #[test]
    fn positive_probe_is_served_from_cache_within_ttl() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;

        let info = ProbeResult {
            context_window: 131_072,
            describe: "probed once".into(),
            ..Default::default()
        };
        let hits = install_fake_probe(Ok(info));

        let first = probe_model("http://127.0.0.1:9", "cache-hit").expect("first probe");
        let second = probe_model("http://127.0.0.1:9", "cache-hit").expect("cached probe");

        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "two probes inside the TTL must reach the harness once"
        );
        assert_eq!(first.context_window, second.context_window);
        assert_eq!(first.describe, second.describe);
    }

    #[test]
    fn expired_probe_entry_is_refetched() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;

        let info = ProbeResult {
            context_window: 8_192,
            ..Default::default()
        };
        let hits = install_fake_probe(Ok(info));

        let key = ("http://127.0.0.1:9".to_string(), "cache-expiry".to_string());
        probe_model(&key.0, &key.1).expect("first probe");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Age the cache entry past its TTL, then ask again.
        let stale = Instant::now()
            .checked_sub(PROBE_CACHE_TTL + Duration::from_secs(1))
            .expect("representable timestamp");
        probe_cache()
            .lock()
            .expect("probe cache")
            .get_mut(&key)
            .expect("entry cached")
            .fetched_at = stale;

        probe_model(&key.0, &key.1).expect("refetched probe");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "an expired entry must re-run the probe"
        );
    }

    #[test]
    fn negative_probe_is_cached_without_a_second_hit() {
        let _serial = PROBE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _clear = ClearFakeProbe;

        let hits = install_fake_probe(Err("model server unreachable".into()));

        let first_err =
            probe_model("http://127.0.0.1:9", "cache-negative").expect_err("server is down");
        let second_err =
            probe_model("http://127.0.0.1:9", "cache-negative").expect_err("still cached as down");

        assert_eq!(first_err, second_err);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a quick retry after a failure must be served from the negative cache"
        );

        // But only briefly: the negative TTL must be shorter than the
        // positive one, so a recovering server is not locked out.
        assert!(PROBE_CACHE_NEGATIVE_TTL < PROBE_CACHE_TTL);

        // Once the (short) negative TTL lapses, the harness is asked again.
        let key = (
            "http://127.0.0.1:9".to_string(),
            "cache-negative".to_string(),
        );
        let stale = Instant::now()
            .checked_sub(PROBE_CACHE_NEGATIVE_TTL + Duration::from_secs(1))
            .expect("representable timestamp");
        probe_cache()
            .lock()
            .expect("probe cache")
            .get_mut(&key)
            .expect("negative entry cached")
            .fetched_at = stale;

        probe_model(&key.0, &key.1).expect_err("server is still down");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "a lapsed negative entry must re-run the probe"
        );
    }

    /// The calibration ledger must stay bounded: inserting past the capacity
    /// evicts oldest-inserted sessions deterministically, re-observing a
    /// session refreshes its recency, and the map never grows past the cap.
    #[test]
    fn observed_tokens_cap_evicts_oldest_inserted() {
        let mut store = ObservedTokens::new();
        let key = |i: usize| format!("session-{i}");

        for i in 0..OBSERVED_TOKENS_CAPACITY {
            store.insert(key(i), i as i64);
        }
        assert_eq!(store.len(), OBSERVED_TOKENS_CAPACITY);
        assert_eq!(store.get(&key(0)), Some(0), "cap boundary not yet crossed");

        // One past the cap: session 0 is evicted, everything else survives.
        store.insert(key(OBSERVED_TOKENS_CAPACITY), 999);
        assert_eq!(store.len(), OBSERVED_TOKENS_CAPACITY, "cap is hard");
        assert_eq!(store.get(&key(0)), None, "oldest-inserted must be evicted");
        assert_eq!(
            store.get(&key(OBSERVED_TOKENS_CAPACITY)),
            Some(999),
            "newest insert must survive"
        );

        // Re-observing refreshes recency: bumping session 1 keeps it alive
        // through the next eviction, which takes session 2 instead.
        store.insert(key(1), 1111);
        store.insert("session-extra".to_string(), 42);
        assert_eq!(
            store.get(&key(1)),
            Some(1111),
            "refreshed entry must survive"
        );
        assert_eq!(
            store.get(&key(2)),
            None,
            "un-refreshed oldest must be evicted"
        );
    }

    /// The empty-report guard sits before `run` in coverage_report's control
    /// flow, so a whitespace-only report must fail locally — no endpoint
    /// discovery, no sidecar, no HTTP — with the coverage-specific error.
    #[test]
    fn coverage_report_rejects_a_whitespace_only_report_without_a_server() {
        let err = coverage_report("/definitely/not/a/repository", "   \n\t ", None).unwrap_err();
        assert_eq!(
            err,
            "The coverage report is empty, so there is nothing to analyze."
        );
    }

    #[test]
    fn advisory_features_have_a_larger_but_bounded_output_budget() {
        assert_eq!(Feature::CommitMessage.max_output_tokens(), 1_024);
        assert_eq!(Feature::ExplainCommit.max_output_tokens(), 1_024);
        assert_eq!(Feature::BranchName.max_output_tokens(), 1_024);
        assert_eq!(Feature::HealthFix.max_output_tokens(), 4_096);
        assert_eq!(Feature::CoverageReport.max_output_tokens(), 4_096);
    }

    #[test]
    fn advisory_retry_recovers_an_empty_first_visible_reply_without_exposing_reasoning() {
        let mut attempts = 0;
        let generation = run_advisory_with_empty_retry("coverage analysis", |retry| {
            attempts += 1;
            assert_eq!(retry, attempts == 2);
            Ok(AiGeneration {
                text: if retry {
                    "Visible plan"
                } else {
                    "<think>private"
                }
                .into(),
                reasoning: "private reasoning".into(),
                model: "test-model".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                context_window: 8_192,
                context_source: "test".into(),
                prompt_tokens: 10,
                completion_tokens: 5,
                truncated: false,
                diff_truncated: false,
                diff_used_bytes: 0,
                diff_total_bytes: 0,
                budget: BudgetReport::default(),
                warnings: vec![format!("attempt {attempts}")],
                elapsed_ms: 1,
            })
        })
        .expect("second visible answer");

        assert_eq!(attempts, 2);
        assert_eq!(generation.text, "Visible plan");
        assert_eq!(generation.reasoning, "private reasoning");
        assert!(generation
            .warnings
            .iter()
            .any(|warning| warning.contains("retried once")));
        assert_eq!(
            generation
                .warnings
                .iter()
                .filter(|warning| warning.starts_with("attempt "))
                .count(),
            2
        );
    }
}
