//! The terminal host: PTYs held by a process of their own, `gitpulse-ptyd`,
//! so that a GitPulse app that quits or dies takes no terminal with it.
//!
//! `docs/TERMINAL_HOST.md` is the design. In short: [`server`] runs the same
//! engine the app runs in-process and answers one connected app over a private
//! unix socket ([`proto`]); [`client`] is the app's side, behind the unchanged
//! `terminal::` functions; [`receipts`] keeps every exit on disk until an app
//! has recorded it.

pub mod client;
pub mod proto;
mod receipts;
pub mod server;

use std::path::PathBuf;

pub use proto::PROTOCOL;

/// The host's executable, found beside the app's own (`Contents/MacOS`).
pub const BINARY: &str = "gitpulse-ptyd";
/// An explicit path to the host executable, for development builds and tests.
pub const BINARY_ENV: &str = "GITPULSE_PTYD";
/// Where receipts live inside the host directory.
const RECEIPTS_DIR: &str = "receipts";

/// The host directory in GitPulse's config directory.
pub fn default_dir() -> Option<PathBuf> {
    crate::tool_config::default_config_dir().map(|dir| dir.join("pty-host"))
}

/// The socket a host of `protocol` listens on. Versioned, so an app never
/// speaks to a host of another protocol by accident.
pub fn socket_name(protocol: u32) -> String {
    format!("ptyd-v{protocol}.sock")
}

/// The one-owner lock a host of `protocol` holds.
pub fn lock_name(protocol: u32) -> String {
    format!("ptyd-v{protocol}.lock")
}

/// The protocol a socket name in the host directory is for.
pub fn socket_protocol(name: &str) -> Option<u32> {
    name.strip_prefix("ptyd-v")?
        .strip_suffix(".sock")?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_names_carry_their_protocol_and_nothing_else_parses_as_one() {
        assert_eq!(socket_name(1), "ptyd-v1.sock");
        assert_eq!(socket_protocol(&socket_name(7)), Some(7));
        for other in [
            "ptyd-v.sock",
            "ptyd-vx.sock",
            "ptyd-v1.lock",
            "agent-notify.sock",
            "ptyd-v1.sock.tmp",
        ] {
            assert_eq!(socket_protocol(other), None, "{other}");
        }
    }
}
