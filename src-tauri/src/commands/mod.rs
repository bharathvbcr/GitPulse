use crate::analyzer::{
    CommitFilter, ConventionalCommit, ConventionalCommitParser, CoverageReport, CoverageScanner,
    DepsHealthReport, DepsScanner, FileCoverage, LanguageDetector, LanguageInfo, LineCounts,
    LocCounter,
};
use crate::diff::conflict_session::{
    self, ConflictSaveOutcome, ConflictSaveRequest, ConflictSnapshot,
};
use crate::diff::{
    compute_word_diff, ConflictDocument, ConflictResolver, FilePatch, IntraLineDiff, PatchBuilder,
};
use crate::engine::git_cli::{
    git_text, resolve_repo, sandbox_write_edited_text, validate_repo, ResolvedRepo,
};
use crate::engine::git_reader::{
    BlameLine, CommitDetails, CommitFileChange, DiffPayload, DoraReport, FileBlob, KnowledgeReport,
    LanguageStatsReport, PulseReport, ReflogEntry, ResetPreview,
};
use crate::engine::git_writer::{
    reworded_message, validate_oid_or_revision, validate_ref_name, AutoFetchOutcome, CloneOptions,
    CloneProgress, GitIdentity, IdentityScope, IndexAction, MergeMode, RebaseStep,
    StashSaveOptions,
};
use crate::engine::{
    BranchInfo, BranchStatsReport, FileStatus, GitReader, GitWriter, OperationAction, RemoteChange,
    RemoteList, RepoOperation, ResetMode, StashAction, SubmoduleChange, SubmoduleList,
    WorktreeInfo,
};
use crate::github::{
    checkout_pull_request, create_issue, discover_github_remote, issue_create_argv,
    load_code_scanning_alerts, load_dependabot_alerts, load_github_context,
    load_workflow_runs_report, pr_checkout_argv, validate_issue_payload, CodeScanningReport,
    DependabotReport, GitHubContext,
};
use crate::graph::{
    mainline_chain_ids, simplify_history_reporting, BezierGeometryCalculator, CubicBezierCurve,
    LaneSolver, MainlineHint, RawCommitNode, RefDecoration, RefKind, RefScope, VisualCommitRow,
};
use crate::stack::StackTreeEngine;
use crate::watcher::{start_watch, unwatch, WatcherState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, State};

mod analysis;
mod app;
mod assist;
mod audit;
mod commit_graph;
mod conflict;
mod forge;
mod history;
mod inspect;
mod intel;
mod knowledge;
mod policy;
mod refs;
mod remote;
mod repo;
mod staging;
mod terminals;
mod worktrees;

pub use analysis::*;
pub use app::*;
pub use assist::*;
pub use audit::*;
pub use commit_graph::*;
pub use conflict::*;
pub use forge::*;
pub use history::*;
pub use inspect::*;
pub use intel::*;
pub use knowledge::*;
pub use policy::*;
pub use refs::*;
pub use remote::*;
pub use repo::*;
pub use staging::*;
pub use terminals::*;
pub use worktrees::*;

/// Runs a blocking body on Tauri's blocking thread pool instead of an async
/// worker thread. Every git/gh/npm subprocess call, repository scan and
/// sidecar round trip below goes through this seam.
async fn off_thread<F, T>(body: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let mut timing = crate::logging::performance::CommandTiming::queue(std::any::type_name::<F>());
    // A test's governor injection is thread-local; carry it to the pool thread
    // so a test can drive the real command under pressure.
    #[cfg(test)]
    let injected = crate::sysload::injected_governor_input();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(test)]
        let _carried = injected.map(crate::sysload::inject_governor);
        timing.start();
        // Every command starts as a deferrable read. `guard` promotes the
        // rest of a mutation's body to a user action, and this scope is what
        // ends that promotion before the pool thread runs anything else — and
        // keeps a mutation's deferral from reading as safe to retry.
        let result = crate::engine::git_cli::run_command_scope(body);
        timing.finish(&result);
        result
    })
    .await
    .map_err(|e| format!("background task failed: {e}"))?
}

#[cfg(test)]
mod assemble_tests;
#[cfg(test)]
mod tests;
