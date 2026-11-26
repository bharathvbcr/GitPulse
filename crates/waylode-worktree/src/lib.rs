//! Library surface for `waylode-worktree` and `waylode-rustc`.
//!
//! Exposes the shared zero-copy content-addressed Rust build cache (`build_cache`)
//! used by both the `waylode-rustc` `RUSTC_WRAPPER` binary and the
//! `waylode-worktree build` / `cache-stats` subcommands.

pub mod build_cache;
pub mod campaign_formal;
