//! Library surface for `gitpulse-worktree` and `gitpulse-rustc`.
//!
//! Exposes the shared zero-copy content-addressed Rust build cache (`build_cache`)
//! used by both the `gitpulse-rustc` `RUSTC_WRAPPER` binary and the
//! `gitpulse-worktree build` / `cache-stats` subcommands.

pub mod build_cache;
pub mod campaign_formal;
