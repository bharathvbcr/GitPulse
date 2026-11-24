//! `gitpulse-protocol`: mobile-safe shared protocol, Noise `IK` E2EE tunnel,
//! P-256 hardware approval verification, and deterministic canonicalization crate.
//!
//! Zero desktop/Tauri/PTY/SQLite/tree-sitter dependencies so the identical crate
//! compiles for macOS, Linux, Windows, iOS (`XCFramework` + NSE), and Android (`.so`).

#![deny(missing_docs)]

pub mod canonical;
pub mod crypto;
pub mod error;
pub mod uniffi_api;
pub mod wire;

pub use error::{ProtocolError, Result};
