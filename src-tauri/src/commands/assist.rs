//! AI and Apple Intelligence commands.

use super::*;

#[tauri::command(async)]
pub async fn cmd_ai_status(base_url: Option<String>, model: Option<String>) -> crate::ai::AiStatus {
    // Endpoint discovery probes local HTTP servers with real timeouts.
    off_thread(move || Ok::<_, String>(crate::ai::status(base_url.as_deref(), model.as_deref())))
        .await
        .map_err(|e| e.message).unwrap_or_else(|e| crate::ai::AiStatus {
            harness: harness_join_failure(e.to_string()),
            endpoints: Vec::new(),
            selected: None,
            model_info: None,
            model_detail: String::new(),
            ready: false,
            detail: e.to_string(),
        })
}

/// Whether Apple Intelligence can write a task on this Mac, and if not, why.
///
/// Infallible by design. Every negative answer is a *state* the reader may be
/// able to act on ("turn it on in Settings"), not an error to be swallowed by
/// a catch, and a command that could reject would let a caller lose the
/// difference between "off" and "this build has no bridge".
#[tauri::command(async)]
pub async fn cmd_apple_intelligence_status() -> crate::ai::apple::AppleIntelligenceStatus {
    // The framework's availability check can touch the model assets.
    off_thread(move || Ok::<_, String>(crate::ai::apple::status()))
        .await
        .map_err(|e| e.message).unwrap_or_else(|error| crate::ai::apple::AppleIntelligenceStatus {
            compiled: true,
            state: "unavailable".to_string(),
            reason: Some("worker_error".to_string()),
            detail: error,
        })
}

/// Writes one task's title and/or description with the on-device model.
///
/// The caller has already created the enhancement in the local store and
/// publishes this result with `enhancements.complete`, so this command owns
/// exactly one thing: producing text without it leaving the Mac.
#[tauri::command(async)]
pub async fn cmd_apple_intelligence_draft(
    request: crate::ai::apple::AppleIntelligenceRequest,
) -> Result<crate::ai::apple::AppleIntelligenceDraft, crate::ai::apple::AppleIntelligenceError> {
    // `off_thread` reports a lost worker as a String; a generation refusal is
    // already a coded error. Both have to arrive as the same type, and the
    // join failure keeps its own code rather than borrowing a model's.
    off_thread(move || Ok::<_, String>(crate::ai::apple::generate(&request)))
        .await
        .map_err(|e| e.message).unwrap_or_else(|error| {
            Err(crate::ai::apple::AppleIntelligenceError {
                code: "worker_error".to_string(),
                message: error,
            })
        })
}

pub(super) fn selection(
    base_url: Option<String>,
    model: Option<String>,
) -> Option<crate::ai::AiSelection> {
    match (base_url, model) {
        (Some(base_url), Some(model)) if !base_url.is_empty() && !model.is_empty() => {
            Some(crate::ai::AiSelection { base_url, model })
        }
        _ => None,
    }
}

#[tauri::command(async)]
pub async fn cmd_ai_generate_commit_message(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    off_thread(move || crate::ai::generate_commit_message(&repo_path, selection(base_url, model)))
        .await
}

#[tauri::command(async)]
pub async fn cmd_ai_explain_commit(
    repo_path: String,
    commit_id: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    off_thread(move || {
        crate::ai::explain_commit(&repo_path, &commit_id, selection(base_url, model))
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_fix_health(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    off_thread(move || crate::ai::fix_health(&repo_path, &report, selection(base_url, model))).await
}

#[tauri::command(async)]
pub async fn cmd_ai_coverage_report(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    off_thread(move || crate::ai::coverage_report(&repo_path, &report, selection(base_url, model)))
        .await
}

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    off_thread(move || crate::ai::suggest_branch_name(&repo_path, selection(base_url, model))).await
}
