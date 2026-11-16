//! Process runner, admission gate, environment pinning, and shared-read
//! coalescing.
//!
//! Extracted from `engine::git_cli` so every non-git subprocess (`devmap`, `gh`,
//! `kingfisher`, tool installers, repository hooks, external diff/probes)
//! flows through this seam without importing `git_cli`.

mod clock;
mod env;
mod gate;
#[cfg(unix)]
mod pipe_drain;
mod qos;
pub(crate) mod read_cancel;
mod runner;
mod shared_budget;
mod shared_read;
#[cfg(any(not(unix), test))]
mod thread_io;

#[cfg(test)]
mod awaiting_adversarial_tests;
#[cfg(test)]
mod budget_churn_tests;
#[cfg(all(test, unix))]
mod grace_adversarial_tests;
#[cfg(test)]
pub(crate) mod read_cancel_tests;
#[cfg(test)]
mod run_errors_adversarial_tests;
#[cfg(test)]
mod scrubbed_env_adversarial_tests;
#[cfg(test)]
mod tests;

pub use env::find_external_tool_with;
#[cfg(any(windows, test))]
pub(crate) use env::simplified_windows_path;
#[cfg(all(test, target_os = "macos"))]
pub(crate) use env::XCRUN_GIT_SHIM;
pub(crate) use env::{
    build_capture_command, canonicalize_plain, extended_child_path, external_tool_fallback_dirs,
    find_external_tool, find_in_dirs, pin_git_environment, plain_path_text,
    resolve_spawn_program_in, resolve_spawn_program_with, PathSource,
};
#[cfg(test)]
pub(crate) use env::{
    build_capture_command_in, is_injected_git_env, lc_ctype_from_inherited, strip_injected_git_env,
};
#[cfg(test)]
pub(in crate::engine::process) use env::{
    default_child_path_with_env, gui_launch_fallback_dirs, restrict_env_to_allowlist,
    restrict_env_to_allowlist_plus, spawn_candidate_names, MAX_INHERITED_PATH_BYTES,
};
#[cfg(target_os = "macos")]
pub(crate) use env::{developer_dirs, past_xcrun_git_shim, REAL_GIT_DIRS};

#[cfg(not(test))]
pub use gate::run_process_with_shared_spawn_budget;
#[cfg(test)]
pub(in crate::engine::process) use gate::{
    configured_spawn_gate, current_spawn_source, format_gate_report, grant_post_action_credit,
    mark_interactive, marked_repo_is_none, post_action_credits, refusal_message_at, report_label,
    spawn_gate, spawn_limit, AdmissionCounters, BucketLevel, LabelCounts, RefusingBucket,
    SpawnPermit, CANCELLED_BEFORE_SPAWN, POST_ACTION_CREDIT_REPOS, POST_ACTION_CREDIT_WINDOW,
    UNATTRIBUTED_SOURCE,
};
pub(crate) use gate::{
    current_admission, grant_post_action_credit_now, is_deferred_under_load, is_slot_wait_timeout,
    mark_user_action, production_spawn_rate, run_command_scope, with_admission,
    with_background_processes, with_spawn_source, Admission, SPAWN_QUEUE_BUDGET, TIMEOUT_MARKER,
};
pub use gate::{
    current_admission_name, run_process_as_background, run_process_awaiting_background_budget,
    run_process_with_unlimited_spawn_rate,
};
#[cfg(test)]
pub(crate) use gate::{
    frozen_gate, hold_credit_open, leak_gate, refusal_message, rev_parse_launches_from, spawn_log,
    take_post_action_credit, with_background_budget_wait, Refusal, SpawnGate,
    POST_ACTION_CREDIT_SPAWNS, SLOT_WAIT_SUFFIX,
};

#[cfg(all(test, target_os = "macos"))]
pub(crate) use qos::band_outcomes;
pub(crate) use qos::with_background_band;
pub use qos::QosTier;

#[cfg(test)]
pub(crate) use read_cancel::CANCELLED_MESSAGE;

pub use runner::{
    byte_tail, capture_command, found_but_failed_detail, is_spawn_not_found, run_captured,
    run_command, run_command_in, CapturedOutput, CapturedRun, Incomplete, RunOptions, RunOutcome,
    DEFAULT_DRAIN_GRACE, DEFAULT_KILL_GRACE, DEFAULT_TIMEOUT, MAX_DRAINED_OUTPUT_BYTES,
    MAX_OUTPUT_BYTES, NETWORK_TIMEOUT,
};
pub(crate) use runner::{
    capture_command_scrubbed, capture_command_with_env, classify_tool_probe, is_run_timeout,
    is_timed_out, last_process_failure, note_process_failure, process_failures, run_bounded,
    run_bounded_capped, run_observed, text_from_bytes, BoundedRun, ExitFacts, KeepDeadlinePrefix,
    OutputStream, ProcessObserver, RunError, ToolProbe,
};
#[cfg(test)]
pub(crate) use runner::{
    forced_failure_active, run_observed_with, run_with_gate_options, spawn_labels_during,
    with_forced_deferral, with_forced_spawn_failure, with_forced_spawn_failure_of,
    with_forced_spawn_failure_then,
};
#[cfg(test)]
pub(in crate::engine::process) use runner::{
    is_cancellation, is_timeout_error, next_poll_backoff, run_with_gate, spawn_error,
    DRAIN_JOIN_GRACE, POLL_BACKOFF_MAX, POLL_BACKOFF_START,
};
pub(in crate::engine::process) use runner::{observe_output, Drained, Stop};

pub(crate) use shared_read::run_read_shared;
#[cfg(test)]
pub(in crate::engine::process) use shared_read::{run_read_shared_with, shared_reads};
