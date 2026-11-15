//! AI and Apple Intelligence commands.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_ai_status(base_url: Option<String>, model: Option<String>) -> crate::ai::AiStatus {
    // Endpoint discovery probes local HTTP servers with real timeouts.
    off_thread(move || {
        Ok::<_, String>(crate::ai::status(base_url.as_deref(), model.as_deref()))
            .map_err(CommandError::from)
    })
    .await
    .map_err(|e| e.message)
    .unwrap_or_else(|e| crate::ai::AiStatus {
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
    off_thread(move || Ok::<_, String>(crate::ai::apple::status()).map_err(CommandError::from))
        .await
        .map_err(|e| e.message)
        .unwrap_or_else(|error| crate::ai::apple::AppleIntelligenceStatus {
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
    off_thread(move || {
        Ok::<_, String>(crate::ai::apple::generate(&request)).map_err(CommandError::from)
    })
    .await
    .map_err(|e| e.message)
    .unwrap_or_else(|error| {
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


/// Where one AI request goes. `provider` `None` or `"local"` is today's
/// behavior exactly: the saved loopback selection (or the consent gate when
/// there is none). Anything else goes through the workbench's one selection
/// parser, so a Claude choice means what it means for task enhancements (no
/// `base_url`, a `claude-` model id) and is carried by the same profile worker.
pub(super) fn route(
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: &crate::workbench::WorkbenchState,
) -> Result<crate::ai::Route, IpcError> {
    if matches!(provider.as_deref(), None | Some("local")) {
        return Ok(crate::ai::Route::Local(selection(base_url, model)));
    }
    match crate::workbench::parse_model_selection_parts(provider, base_url, model) {
        Ok(Some(crate::harness::sidecar::ModelSelection::Claude { model })) => {
            Ok(crate::ai::Route::Claude(workbench.assist_route(model)))
        }
        Ok(_) => Err("A model is required for a Claude selection.".into()),
        Err(error) => Err(error.message.into()),
    }
}

#[tauri::command(async)]
pub async fn cmd_ai_generate_commit_message(
    repo_path: String,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let route = route(provider, base_url, model, workbench.inner())?;
    off_thread(move || crate::ai::generate_commit_message(&repo_path, route)).await
}

#[tauri::command(async)]
pub async fn cmd_ai_explain_commit(
    repo_path: String,
    commit_id: String,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let route = route(provider, base_url, model, workbench.inner())?;
    off_thread(move || crate::ai::explain_commit(&repo_path, &commit_id, route)).await
}

#[tauri::command(async)]
pub async fn cmd_ai_fix_health(
    repo_path: String,
    report: String,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let route = route(provider, base_url, model, workbench.inner())?;
    off_thread(move || crate::ai::fix_health(&repo_path, &report, route)).await
}

#[tauri::command(async)]
pub async fn cmd_ai_coverage_report(
    repo_path: String,
    report: String,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let route = route(provider, base_url, model, workbench.inner())?;
    off_thread(move || crate::ai::coverage_report(&repo_path, &report, route)).await
}

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let route = route(provider, base_url, model, workbench.inner())?;
    off_thread(move || crate::ai::suggest_branch_name(&repo_path, route)).await
}

#[cfg(test)]
mod assist_tests {
    use super::*;
    use crate::ai::Route;

    fn pick(
        provider: Option<&str>,
        base_url: Option<&str>,
        model: Option<&str>,
    ) -> Result<Route, IpcError> {
        route(
            provider.map(str::to_string),
            base_url.map(str::to_string),
            model.map(str::to_string),
            &crate::workbench::WorkbenchState::default(),
        )
    }

    #[test]
    fn no_provider_or_local_keeps_todays_loopback_behavior() {
        for provider in [None, Some("local")] {
            match pick(provider, Some("http://127.0.0.1:11434/v1"), Some("qwen")).unwrap() {
                Route::Local(Some(selection)) => assert_eq!(selection.model, "qwen"),
                other => panic!("expected a local selection, got {other:?}"),
            }
            assert!(matches!(pick(provider, None, None).unwrap(), Route::Local(None)));
        }
    }

    #[test]
    fn a_claude_choice_routes_to_manvi_and_never_to_loopback() {
        match pick(Some("anthropic"), None, Some("claude-sonnet-5-5")).unwrap() {
            Route::Claude(cloud) => assert_eq!(cloud.model, "claude-sonnet-5-5"),
            other => panic!("expected the Claude route, got {other:?}"),
        }
    }

    #[test]
    fn a_claude_choice_that_names_an_endpoint_or_a_non_claude_model_is_refused() {
        assert!(pick(Some("anthropic"), Some("http://127.0.0.1:1/v1"), Some("claude-x")).is_err());
        assert!(pick(Some("anthropic"), None, Some("gpt-5")).is_err());
        assert!(pick(Some("anthropic"), None, None).is_err());
        assert!(pick(Some("openai"), None, Some("claude-x")).is_err());
    }
}
