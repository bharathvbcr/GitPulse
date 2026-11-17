//! AI and Apple Intelligence commands.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_ai_status(base_url: Option<String>, model: Option<String>) -> crate::ai::AiStatus {
    // Asking Manvi what it runs on spawns it, and endpoint discovery probes
    // local HTTP servers with real timeouts.
    off_thread(move || {
        let manvi = crate::harness::manvi_use::read();
        Ok::<_, String>(crate::ai::status(
            base_url.as_deref(),
            model.as_deref(),
            manvi,
        ))
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
        manvi: crate::harness::manvi_use::ManviStatus {
            state: "failed".into(),
            detail: e.to_string(),
            ..Default::default()
        },
    })
}

/// Changes what Manvi runs on (`manvi use cloud|local …`), the one writer of
/// that choice, and answers what Manvi now reports. Human-initiated: only the
/// Manvi pane's cloud/local toggle and Claude picker, and picking a local model
/// in the Manvi pill or the Local model servers card, call it; never a
/// background path, and never the loopback consent dialog. The shapes
/// are checked before Manvi is asked ([`crate::harness::manvi_use::validate`]):
/// a `claude-` model id for cloud; a loopback base URL and a plain model id
/// for local. The live profile worker is retired so the next call spawns for
/// the new choice.
#[tauri::command(async)]
pub async fn cmd_manvi_use(
    mode: String,
    model: Option<String>,
    base_url: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::harness::manvi_use::ManviStatus, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        let status =
            crate::harness::manvi_use::write(&mode, model.as_deref(), base_url.as_deref())?;
        workbench.retire_worker();
        Ok(status)
    })
    .await
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

/// Where one AI request goes, and why it did not go where Manvi says when it
/// did not.
///
/// Manvi's own config decides (`manvi use`) once someone has chosen a mode:
/// in either mode the request goes through Manvi's `work.assist.complete` on
/// the worker for that selection, the same worker task enhancements use.
/// GitPulse's own loopback client ([`Route::Local`], with the caller's saved
/// selection and the consent gate) is the fallback for a Manvi that is not
/// installed, has no `manvi use`, has no chosen mode (`not_chosen`: its
/// built-in default is cloud, which nobody picked), or lacks the op in local
/// mode, and the reason comes back as a warning to put on the result. A mode
/// GitPulse cannot use, a cloud Manvi without the op, and any other failure to
/// start the worker are errors, never a quiet move to loopback.
pub(super) fn resolve_route(
    status: &crate::harness::manvi_use::ManviStatus,
    served: impl FnOnce(&crate::harness::sidecar::ModelSelection) -> Result<bool, (String, String)>,
    manvi_route: impl FnOnce(crate::harness::sidecar::ModelSelection) -> crate::ai::ManviRoute,
    fallback: Option<crate::ai::AiSelection>,
) -> Result<(crate::ai::Route, Option<String>), String> {
    use crate::ai::Route;
    let fall_back = |why: String| {
        Ok((
            Route::Local(fallback.clone()),
            Some(format!(
                "{why} GitPulse used its own connection to a local model server instead."
            )),
        ))
    };
    match status.state.as_str() {
        "ready" => {}
        "not_installed" | "needs_update" | "failed" | "not_chosen" => {
            return fall_back(status.detail.clone())
        }
        other => return Err(format!("Manvi reported an unknown state `{other}`.")),
    }
    let Some(selection) = status.typed.clone() else {
        return Err(status.unsupported.clone());
    };
    let cloud = status.mode == "cloud";
    match served(&selection) {
        Ok(true) => Ok((Route::Manvi(manvi_route(selection)), None)),
        Ok(false) if cloud => Err(crate::ai::OLD_MANVI_MESSAGE.to_string()),
        Ok(false) => fall_back(
            "This Manvi does not offer work.assist.complete (update Manvi to send local-model requests through it)."
                .into(),
        ),
        Err((code, message)) if code == "not_installed" && !cloud => fall_back(message),
        Err((_, message)) => Err(message),
    }
}

/// [`resolve_route`] for the running app: reads Manvi's choice and asks the
/// workbench's worker for that selection whether it serves the op.
fn route_for(
    base_url: Option<String>,
    model: Option<String>,
    workbench: &crate::workbench::WorkbenchState,
) -> Result<(crate::ai::Route, Option<String>), CommandError> {
    let status = crate::harness::manvi_use::read();
    resolve_route(
        &status,
        |selection| {
            workbench
                .assist_served(selection)
                .map_err(|e| (e.code, e.message))
        },
        |selection| workbench.assist_route(selection),
        selection(base_url, model),
    )
    .map_err(CommandError::from)
}

/// Puts the reason for a fallback at the head of a result's warnings.
fn with_route_warning(
    result: Result<crate::ai::AiGeneration, CommandError>,
    warning: Option<String>,
) -> Result<crate::ai::AiGeneration, CommandError> {
    result.map(|mut generation| {
        if let Some(warning) = warning {
            generation.warnings.insert(0, warning);
        }
        generation
    })
}

#[tauri::command(async)]
pub async fn cmd_ai_generate_commit_message(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        // Unlike the other features, a commit message does not fail when no
        // route can be chosen: it is written from the patch, with the reason
        // as a warning (`generate_commit_message_routed`).
        let (route, warning) = match route_for(base_url, model, &workbench) {
            Ok((route, warning)) => (Ok(route), warning),
            Err(why) => (Err(why), None),
        };
        with_route_warning(
            crate::ai::generate_commit_message_routed(&repo_path, route),
            warning,
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_explain_commit(
    repo_path: String,
    commit_id: String,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        let (route, warning) = route_for(base_url, model, &workbench)?;
        with_route_warning(
            crate::ai::explain_commit(&repo_path, &commit_id, route),
            warning,
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_fix_health(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        let (route, warning) = route_for(base_url, model, &workbench)?;
        with_route_warning(crate::ai::fix_health(&repo_path, &report, route), warning)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_coverage_report(
    repo_path: String,
    report: String,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        let (route, warning) = route_for(base_url, model, &workbench)?;
        with_route_warning(
            crate::ai::coverage_report(&repo_path, &report, route),
            warning,
        )
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_ai_suggest_branch_name(
    repo_path: String,
    base_url: Option<String>,
    model: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<crate::ai::AiGeneration, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        let (route, warning) = route_for(base_url, model, &workbench)?;
        with_route_warning(crate::ai::suggest_branch_name(&repo_path, route), warning)
    })
    .await
}

#[cfg(test)]
mod assist_tests {
    use super::*;
    use crate::ai::Route;
    use crate::harness::manvi_use::ManviStatus;
    use crate::harness::sidecar::ModelSelection;
    use std::sync::Arc;

    fn ready(mode: &str, typed: Option<ModelSelection>) -> ManviStatus {
        ManviStatus {
            state: "ready".into(),
            mode: mode.into(),
            provider: if mode == "cloud" {
                "anthropic"
            } else {
                "local"
            }
            .into(),
            unsupported: if typed.is_none() {
                "unsupported mode".into()
            } else {
                String::new()
            },
            typed,
            ..Default::default()
        }
    }

    fn claude() -> ModelSelection {
        ModelSelection::Claude {
            model: "claude-sonnet-5-5".into(),
        }
    }

    fn local() -> ModelSelection {
        ModelSelection::Local {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen3".into(),
        }
    }

    fn fake_route(selection: ModelSelection) -> crate::ai::ManviRoute {
        let (model, provider) = match selection {
            ModelSelection::Claude { model } => (model, "anthropic"),
            ModelSelection::Local { model, .. } => (model, "local"),
        };
        crate::ai::ManviRoute {
            model,
            provider,
            destination: "via Manvi".into(),
            complete: Arc::new(|_| {
                Err(crate::ai::ManviError {
                    code: "x".into(),
                    message: "x".into(),
                })
            }),
        }
    }

    fn fallback() -> Option<crate::ai::AiSelection> {
        Some(crate::ai::AiSelection {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "saved".into(),
        })
    }

    #[test]
    fn local_mode_routes_to_assist_complete_and_never_to_the_loopback_client() {
        let (route, warning) = resolve_route(
            &ready("local", Some(local())),
            |_| Ok(true),
            fake_route,
            fallback(),
        )
        .unwrap();
        match route {
            Route::Manvi(manvi) => {
                assert_eq!((manvi.provider, manvi.model.as_str()), ("local", "qwen3"))
            }
            other => panic!("expected the Manvi route, got {other:?}"),
        }
        assert_eq!(warning, None);
    }

    #[test]
    fn cloud_mode_routes_to_assist_complete_with_the_claude_selection() {
        let (route, _) = resolve_route(
            &ready("cloud", Some(claude())),
            |_| Ok(true),
            fake_route,
            None,
        )
        .unwrap();
        assert!(matches!(route, Route::Manvi(m) if m.provider == "anthropic"));
    }

    #[test]
    fn a_local_manvi_without_the_op_falls_back_with_a_warning_naming_why() {
        let (route, warning) = resolve_route(
            &ready("local", Some(local())),
            |_| Ok(false),
            fake_route,
            fallback(),
        )
        .unwrap();
        assert!(matches!(route, Route::Local(Some(s)) if s.model == "saved"));
        let warning = warning.unwrap();
        assert!(warning.contains("work.assist.complete"), "{warning}");
        assert!(warning.contains("its own connection"), "{warning}");
    }

    #[test]
    fn a_cloud_manvi_without_the_op_is_an_error_not_a_loopback_fallback() {
        let error = resolve_route(
            &ready("cloud", Some(claude())),
            |_| Ok(false),
            fake_route,
            fallback(),
        )
        .err()
        .unwrap();
        assert_eq!(error, crate::ai::OLD_MANVI_MESSAGE);
    }

    #[test]
    fn manvi_not_installed_or_without_use_falls_back_naming_why() {
        for (state, detail) in [
            (
                "not_installed",
                "Manvi is not installed, so its model choice cannot be read.",
            ),
            (
                "needs_update",
                "This Manvi has no working `manvi use --json`. Update Manvi.",
            ),
            ("failed", "Could not ask Manvi: timed out"),
        ] {
            let status = ManviStatus {
                state: state.into(),
                detail: detail.into(),
                ..Default::default()
            };
            let (route, warning) = resolve_route(
                &status,
                |_| panic!("no worker is asked when Manvi cannot be read"),
                fake_route,
                fallback(),
            )
            .unwrap();
            assert!(matches!(route, Route::Local(Some(_))), "{state}");
            assert!(warning.unwrap().starts_with(detail), "{state}");
        }
    }

    #[test]
    fn a_worker_that_cannot_start_is_an_error_in_cloud_and_a_fallback_only_when_not_installed() {
        let cloud = ready("cloud", Some(claude()));
        let error = resolve_route(
            &cloud,
            |_| Err(("unavailable".into(), "no key".into())),
            fake_route,
            fallback(),
        )
        .err()
        .unwrap();
        assert_eq!(error, "no key");
        let cloud_missing = resolve_route(
            &cloud,
            |_| Err(("not_installed".into(), "gone".into())),
            fake_route,
            fallback(),
        );
        assert!(cloud_missing.is_err(), "cloud never falls back to loopback");

        let local_status = ready("local", Some(local()));
        let (route, warning) = resolve_route(
            &local_status,
            |_| Err(("not_installed".into(), "Manvi is not installed.".into())),
            fake_route,
            fallback(),
        )
        .unwrap();
        assert!(matches!(route, Route::Local(_)));
        assert!(warning.unwrap().starts_with("Manvi is not installed."));
        let other = resolve_route(
            &local_status,
            |_| Err(("busy".into(), "busy".into())),
            fake_route,
            fallback(),
        );
        assert!(other.is_err());
    }

    /// What `manvi use --json` answers when nobody has saved a choice: Manvi's
    /// built-in default, cloud, from the catalogue (`provider_from: default`).
    const UNCHOSEN: &str = r#"{"mode":"cloud","provider":"anthropic","provider_from":"default","user_config":"/home/u/.config/manvi/config.toml","shadowed_by":[],"model":"claude-sonnet-5-5","model_from":"default","local":{"base_url":"http://127.0.0.1:8000/v1","base_url_from":"default","model":"","model_from":"default"},"cloud":{"model":"claude-sonnet-5-5","model_from":"default"},"warnings":[]}"#;

    fn unchosen() -> ManviStatus {
        crate::harness::manvi_use::interpret(crate::harness::manvi_use::UseRun {
            success: true,
            stdout: UNCHOSEN.as_bytes().to_vec(),
            stderr: String::new(),
        })
    }

    #[test]
    fn an_unchosen_manvi_default_never_routes_a_diff_to_the_cloud() {
        // A saved loopback selection is used as before, behind the consent
        // gate; the worker is never asked and the warning says what to choose.
        let (route, warning) = resolve_route(
            &unchosen(),
            |_| panic!("no worker is asked while Manvi's model is not chosen"),
            fake_route,
            fallback(),
        )
        .unwrap();
        assert!(
            matches!(&route, Route::Local(Some(s)) if s.model == "saved"),
            "{route:?}"
        );
        let warning = warning.unwrap();
        assert!(warning.contains("not been chosen"), "{warning}");
        assert!(warning.contains("Manvi pane"), "{warning}");
        assert!(warning.contains("its own connection"), "{warning}");

        // With nothing saved the loopback client sweeps and asks first.
        let (route, _) = resolve_route(
            &unchosen(),
            |_| panic!("no worker is asked while Manvi's model is not chosen"),
            fake_route,
            None,
        )
        .unwrap();
        assert!(matches!(route, Route::Local(None)), "{route:?}");
    }

    #[test]
    fn a_mode_gitpulse_cannot_use_is_an_error() {
        let error = resolve_route(&ready("other", None), |_| Ok(true), fake_route, fallback())
            .err()
            .unwrap();
        assert_eq!(error, "unsupported mode");
    }

    #[test]
    fn the_saved_loopback_selection_is_read_as_before() {
        let some = selection(
            Some("http://127.0.0.1:11434/v1".into()),
            Some("qwen".into()),
        )
        .unwrap();
        assert_eq!(some.model, "qwen");
        assert!(selection(None, None).is_none());
        assert!(selection(Some(String::new()), Some("m".into())).is_none());
    }
}
