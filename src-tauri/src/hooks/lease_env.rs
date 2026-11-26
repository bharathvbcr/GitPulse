//! The lease wrapper's environment names, on every platform.
//!
//! The wrapper itself (`lease_wrapper`) is Unix-only, but the names it sets
//! are read everywhere: memory relief recognises a leased build by them and a
//! launched session has a stale inherited value removed. `lease_wrapper`
//! re-exports these, so both spellings name one constant.

/// Set to the slot number for everything a leased build starts, so a nested
/// `cargo` passes straight through instead of waiting on its own parent.
pub const HELD_ENV: &str = "WAYLODE_LEASE_HELD";
pub const LEGACY_HELD_ENV: &str = "GITPULSE_LEASE_HELD";

/// The shell shim's own "already gated" marker (`~/.claude-bin/cargo`).
pub const SHIM_HELD_ENV: &str = "CARGO_SLOT_HELD";

/// Set on the sessions Waylode launches to the folder of `cargo` / `go`
/// links, which their PATH starts with.
pub const BIN_ENV: &str = "WAYLODE_LEASE_BIN";
pub const LEGACY_BIN_ENV: &str = "GITPULSE_LEASE_BIN";
