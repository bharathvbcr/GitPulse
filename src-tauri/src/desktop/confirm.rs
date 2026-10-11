//! A confirmation the OS draws, which webview script cannot answer.
//!
//! The in-app modals that precede a trust grant or a tool-path change are the
//! user interface. They are not a security boundary: they are DOM, so any
//! script that reaches the IPC bridge can call the command behind them
//! directly. Commands that widen what GitPulse will execute therefore ask here
//! too, from Rust, through a dialog the page cannot click.
//!
//! Callers take the answer as a `&dyn Fn(&str, &str) -> bool` and are handed
//! [`native`] in production, so the decision logic is tested with a closure
//! and no dialog ever opens under test.

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

/// Shows a modal Yes/No warning and returns true only for an explicit Yes.
///
/// Fails closed: a host that cannot draw the dialog (no display, no portal)
/// answers No, which refuses the action instead of approving it unseen.
pub fn native(title: &str, message: &str) -> bool {
    matches!(
        MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title(title)
            .set_description(message)
            .set_buttons(MessageButtons::YesNo)
            .show(),
        MessageDialogResult::Yes
    )
}
