//! Merge conflict sessions: snapshot, save, preview.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_conflict_snapshot(
    repo_path: String,
    file_path: String,
) -> Result<ConflictSnapshot, IpcError> {
    off_thread(move || conflict_session::snapshot(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_save_conflict(
    repo_path: String,
    request: ConflictSaveRequest,
) -> Result<Guarded<ConflictSaveOutcome>, IpcError> {
    off_thread(move || {
        save_conflict_folded(
            &repo_path,
            &request,
            |file, op| guard_file(&repo_path, file, op),
            |argv| guard(&repo_path, argv),
        )
    })
    .await
}

/// Saves a conflict resolution with every judged line, the target file's
/// included, folded into one verdict. Authorizes the literal target and the
/// exact object/index commands; the native transaction builds argv once for
/// judging and execution.
pub(super) fn save_conflict_folded(
    repo_path: &str,
    request: &ConflictSaveRequest,
    judge_file: impl Fn(&str, &str) -> Result<crate::harness::PolicyVerdict, String>,
    judge_command: impl Fn(&[&str]) -> Result<crate::harness::PolicyVerdict, String>,
) -> Result<Guarded<ConflictSaveOutcome>, CommandError> {
    let fold = VerdictFold::default();
    let output = conflict_session::save_with_gate(
        repo_path,
        request,
        |file, op| {
            fold.record(judge_file(file, op)?);
            Ok(())
        },
        |argv| {
            fold.record(judge_command(argv)?);
            Ok(())
        },
    )?;
    let policy = fold
        .finish()
        .ok_or("Conflict transaction did not produce a verdict")?;
    Ok(Guarded { policy, output })
}

#[tauri::command(async)]
pub fn cmd_preview_conflict(document: ConflictDocument) -> Result<String, IpcError> {
    ConflictResolver::render_preview(&document).map_err(IpcError::from)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProposalResolutionChoiceInput {
    AcceptProposal,
    SessionChoice(crate::diff::conflict_session::ConflictFileChoice),
}

impl From<ProposalResolutionChoiceInput> for conflict_session::ProposalResolutionChoice {
    fn from(value: ProposalResolutionChoiceInput) -> Self {
        match value {
            ProposalResolutionChoiceInput::AcceptProposal => Self::AcceptProposal,
            ProposalResolutionChoiceInput::SessionChoice(choice) => Self::SessionChoice(choice),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalFileResolutionInput {
    pub file_path: String,
    pub revision: Option<String>,
    pub choice: ProposalResolutionChoiceInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProposalTreeInput {
    pub proposal_ref: String,
    pub expected_proposal_commit_oid: Option<String>,
    pub target_tip_oid: Option<String>,
    pub resolutions: Vec<ProposalFileResolutionInput>,
}

#[tauri::command(async)]
pub async fn cmd_load_conflict_proposal(
    repo_path: String,
    proposal_ref: String,
) -> Result<conflict_session::ProposalSessionSnapshot, IpcError> {
    off_thread(move || conflict_session::load_proposal_for_session(&repo_path, &proposal_ref)).await
}

#[tauri::command(async)]
pub async fn cmd_build_resolved_proposal_tree(
    repo_path: String,
    request: BuildProposalTreeInput,
) -> Result<conflict_session::ResolvedProposalTree, IpcError> {
    off_thread(move || {
        crate::engine::process::mark_user_action(&repo_path);
        let engine_req = conflict_session::BuildProposalTreeRequest {
            proposal_ref: request.proposal_ref,
            expected_proposal_commit_oid: request.expected_proposal_commit_oid,
            target_tip_oid: request.target_tip_oid,
            resolutions: request
                .resolutions
                .into_iter()
                .map(|r| conflict_session::ProposalFileResolution {
                    file_path: r.file_path,
                    revision: r.revision,
                    choice: r.choice.into(),
                })
                .collect(),
        };
        conflict_session::build_resolved_proposal_tree(&repo_path, &engine_req)
    })
    .await
}

#[cfg(test)]
#[path = "conflict_tests.rs"]
mod tests;
