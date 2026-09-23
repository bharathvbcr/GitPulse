pub mod budget;
pub mod cow_clone;
pub mod deadbranch;
pub mod git_cli;
pub mod git_reader;
pub mod git_writer;
pub mod portless;
pub mod provenance;
pub mod remotes;
pub mod repo_op;
pub mod stash;
pub mod submodules;
pub mod worktree;
pub mod worktree_hooks;

pub use cow_clone::{reflink_copy_dir, reflink_ignored_caches, ReflinkResult};
pub use portless::{detect_worktree_routes, hash_port, read_portless_routes, WorktreeRouteInfo};
pub use worktree_hooks::{
    execute_worktree_hooks, load_worktree_hooks, HookExecutionResult, WorktreeHooksConfig,
};

pub use deadbranch::{
    DeadbranchBackupInfo, DeadbranchCleanResult, DeadbranchConfig, DeadbranchRestoreResult,
    DeadbranchScanResult, StaleBranchInfo,
};
pub use git_cli::{
    find_git_root, resolve_git_dir, resolve_repo, sandbox_join, sandbox_write, validate_repo,
    ResolvedRepo,
};
pub use git_reader::{
    AuthorOwnership, BlameLine, BranchInfo, BranchStatsReport, BranchStatsUpdate,
    CodeAgeDistribution, CommitDetails, CommitFileChange, DoraReport, FileStatus, GitReader,
    KnowledgeReport, OrphanedFile, PulseCommitSummary, PulseExtensionChurn, PulseFileChurn,
    PulseReport, ReflogEntry, RepoLanguageStat, TagInfo, TagList,
};
pub use git_writer::{GitWriter, RebaseActionKind, RebaseStep, ResetMode};
pub use provenance::{ProvenanceFreshness, SessionEpisodeNote, VerificationNote};
pub use remotes::{RemoteChange, RemoteInfo, RemoteList};
pub use repo_op::{OperationAction, OperationKind, RepoOperation};
pub use stash::{StashAction, StashEntry};
pub use submodules::{SubmoduleChange, SubmoduleInfo, SubmoduleList, SubmoduleState};
pub use worktree::{
    add_worktree_extended, agent_kind, agent_session_slug, merge_and_teardown_worktree,
    merge_teardown_argv, MergeTeardownResult, WorktreeDiffStat, WorktreeDivergence, WorktreeInfo,
};
