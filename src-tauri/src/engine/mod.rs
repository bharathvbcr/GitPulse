pub mod git_cli;
pub mod git_reader;
pub mod git_writer;
pub mod provenance;
pub mod remotes;
pub mod repo_op;
pub mod stash;
pub mod submodules;
pub mod worktree;

pub use git_cli::{
    find_git_root, resolve_git_dir, resolve_repo, sandbox_join, sandbox_write, validate_repo,
    ResolvedRepo,
};
pub use git_reader::{
    BlameLine, BranchInfo, BranchStatsReport, BranchStatsUpdate, CommitDetails, CommitFileChange,
    FileStatus, GitReader, ReflogEntry, RepoLanguageStat, TagInfo, TagList,
};
pub use git_writer::{GitWriter, RebaseActionKind, RebaseStep, ResetMode};
pub use provenance::{ProvenanceFreshness, SessionEpisodeNote, VerificationNote};
pub use remotes::{RemoteChange, RemoteInfo, RemoteList};
pub use repo_op::{OperationAction, OperationKind, RepoOperation};
pub use stash::{StashAction, StashEntry};
pub use submodules::{SubmoduleChange, SubmoduleInfo, SubmoduleList, SubmoduleState};
pub use worktree::WorktreeInfo;
