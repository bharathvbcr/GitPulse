//! Saving text the app made (a theme file, a Pulse card, retained terminal
//! output) to a file the person picks in the native save panel.
//!
//! The webview cannot save on its own: WKWebView ignores `<a download>`, so a
//! browser-style download did nothing in the packaged app. This is the one
//! native path for every such export.
//!
//! The webview never names a path. It sends a closed [`ExportKind`], a
//! suggested file name and the text; the save panel is opened here and the
//! file is written only where the person chose. A pick-then-write pair of
//! commands would hand the webview a write-anywhere primitive, which is why
//! the dialog and the write are one command.

use std::io::ErrorKind;
use std::path::Path;

use serde::Deserialize;

use super::off_thread;
use crate::ipc_error::{CommandError, IpcError};

/// The most text one export may carry. Retained terminal output is the
/// largest real export; a theme or a card is a few kilobytes.
pub(crate) const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;

/// Longest suggested name, in characters, before the extension. The panel
/// lets the person type anything; this bounds only what the webview proposes.
const MAX_SUGGESTED_STEM_CHARS: usize = 120;

/// What is being exported. Each kind fixes the panel's title, its file-type
/// filter and the extension, so none of those come from the webview.
///
/// Mirrored by `ExportKind` in src/lib/desktop/saveFile.ts, which
/// scripts/enum-variant-contract.test.ts pins in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    TerminalOutput,
    Theme,
    PulseCard,
}

/// The panel's fixed wording and the file type for one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExportFormat {
    pub title: &'static str,
    pub filter: &'static str,
    pub extension: &'static str,
    pub default_stem: &'static str,
}

impl ExportKind {
    pub(crate) fn format(self) -> ExportFormat {
        match self {
            ExportKind::TerminalOutput => ExportFormat {
                title: "Export retained terminal output",
                filter: "Text",
                extension: "txt",
                default_stem: "terminal-output",
            },
            ExportKind::Theme => ExportFormat {
                title: "Export theme",
                filter: "GitPulse theme",
                extension: "json",
                default_stem: "gitpulse-theme",
            },
            ExportKind::PulseCard => ExportFormat {
                title: "Export Pulse card",
                filter: "SVG image",
                extension: "svg",
                default_stem: "gitpulse-pulse-card",
            },
        }
    }
}

/// What the panel will be opened with, once the request has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportPlan {
    pub format: ExportFormat,
    pub file_name: String,
}

/// Checks an export before any panel opens: the size cap, and a suggested
/// name reduced to a bare file name with the kind's extension.
pub(crate) fn plan_export(
    kind: ExportKind,
    suggested: &str,
    data_len: usize,
) -> Result<ExportPlan, String> {
    if data_len > MAX_EXPORT_BYTES {
        return Err(format!(
            "The export is larger than {} MiB, which is more than GitPulse writes to one file.",
            MAX_EXPORT_BYTES / (1024 * 1024)
        ));
    }
    let format = kind.format();
    Ok(ExportPlan {
        format,
        file_name: suggested_file_name(format, suggested),
    })
}

/// A suggested name the panel can show as-is: no folder part, no control
/// characters, not hidden, bounded, and ending in the kind's extension.
fn suggested_file_name(format: ExportFormat, suggested: &str) -> String {
    let cleaned: String = suggested
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim();
    let suffix = format!(".{}", format.extension);
    let stem = crate::fs_entry::strip_suffix_ascii_ci(cleaned, &suffix).unwrap_or(cleaned);
    let stem = stem.trim().trim_start_matches('.').trim();
    let stem: String = stem.chars().take(MAX_SUGGESTED_STEM_CHARS).collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() {
        format.default_stem
    } else {
        stem
    };
    format!("{stem}{suffix}")
}

/// Writes the export to the path the person picked, replacing what is there
/// (the panel has already asked about replacing it).
pub(crate) fn write_export(path: &Path, data: &[u8]) -> Result<(), String> {
    std::fs::write(path, data).map_err(|error| write_failure(path, &error))
}

/// One plain sentence for a failed write, naming the file.
fn write_failure(path: &Path, error: &std::io::Error) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the file".to_owned());
    match error.kind() {
        ErrorKind::PermissionDenied => {
            format!("GitPulse isn't allowed to write {name} in that folder. Pick another folder.")
        }
        ErrorKind::NotFound => format!("The folder chosen for {name} no longer exists."),
        ErrorKind::ReadOnlyFilesystem => format!("{name} is on a disk that is read-only."),
        ErrorKind::StorageFull => format!("There isn't enough disk space to save {name}."),
        ErrorKind::IsADirectory => {
            format!("{name} is a folder, so it can't be replaced by a file.")
        }
        _ => format!("{name} could not be written ({error})."),
    }
}

/// Opens the native save panel for one export and writes the text where the
/// person chose. `Ok(Some(name))` is the saved file's name; `Ok(None)` is a
/// cancelled panel, which writes nothing and is not an error.
///
/// The write runs on the blocking pool through [`off_thread`], the seam every
/// command's file and process work goes through.
#[tauri::command(async)]
pub async fn cmd_save_export(
    kind: ExportKind,
    file_name: String,
    data: String,
) -> Result<Option<String>, IpcError> {
    let plan = plan_export(kind, &file_name, data.len()).map_err(IpcError::from)?;
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_title(plan.format.title)
        .set_file_name(&plan.file_name)
        .add_filter(plan.format.filter, &[plan.format.extension])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let saved_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| plan.file_name.clone());
    off_thread(move || write_export(&path, data.as_bytes()).map_err(CommandError::from)).await?;
    Ok(Some(saved_name))
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn write_export_writes_the_bytes_and_replaces_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("theme.json");
        std::fs::write(&path, b"old contents that are longer").expect("seed");
        write_export(&path, b"{\"schema\":\"gitpulse-theme/1\"}").expect("write");
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"{\"schema\":\"gitpulse-theme/1\"}"
        );
    }

    #[test]
    fn write_export_into_a_missing_folder_is_one_plain_sentence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gone").join("card.svg");
        let error = write_export(&path, b"<svg/>").expect_err("missing folder");
        assert_eq!(error, "The folder chosen for card.svg no longer exists.");
        assert!(!path.exists());
    }

    #[test]
    fn write_export_over_a_folder_names_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("taken.svg");
        std::fs::create_dir(&path).expect("folder in the way");
        let error = write_export(&path, b"<svg/>").expect_err("a folder is in the way");
        assert!(error.contains("taken.svg"), "{error}");
        assert!(path.is_dir(), "the folder is left as it was");
    }

    #[cfg(unix)]
    #[test]
    fn write_export_to_a_folder_without_write_permission_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("folder");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
            .expect("read-only folder");
        let path = locked.join("theme.json");
        let result = write_export(&path, b"{}");
        // Restore before asserting so the tempdir can always be removed.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("restore");
        // Root ignores mode bits; only assert the sentence where the write was refused.
        if let Err(error) = result {
            assert_eq!(
                error,
                "GitPulse isn't allowed to write theme.json in that folder. Pick another folder."
            );
        }
    }

    #[test]
    fn an_export_over_the_cap_is_refused_before_any_panel_opens() {
        let error = plan_export(ExportKind::TerminalOutput, "out.txt", MAX_EXPORT_BYTES + 1)
            .expect_err("over the cap");
        assert!(error.contains("16 MiB"), "{error}");
        assert!(plan_export(ExportKind::TerminalOutput, "out.txt", MAX_EXPORT_BYTES).is_ok());
    }

    #[test]
    fn each_kind_fixes_its_own_title_filter_and_extension() {
        let theme = plan_export(ExportKind::Theme, "Dusk.json", 10).expect("plan");
        assert_eq!(theme.format.extension, "json");
        assert_eq!(theme.file_name, "Dusk.json");
        let card = plan_export(ExportKind::PulseCard, "repo-pulse-card.svg", 10).expect("plan");
        assert_eq!(card.format.extension, "svg");
        assert_eq!(card.format.title, "Export Pulse card");
        let terminal =
            plan_export(ExportKind::TerminalOutput, "terminal-output.txt", 10).expect("plan");
        assert_eq!(terminal.format.extension, "txt");
        assert_eq!(terminal.file_name, "terminal-output.txt");
    }

    #[test]
    fn a_suggested_name_cannot_carry_a_folder_or_hide_the_file() {
        let name = |kind, suggested| plan_export(kind, suggested, 0).expect("plan").file_name;
        assert_eq!(
            name(ExportKind::Theme, "../../etc/passwd"),
            "-..-etc-passwd.json"
        );
        assert_eq!(
            name(ExportKind::Theme, "C:\\Users\\x.json"),
            "C--Users-x.json"
        );
        assert_eq!(name(ExportKind::Theme, ".hidden.json"), "hidden.json");
        assert_eq!(name(ExportKind::Theme, "Dark/Light"), "Dark-Light.json");
        assert_eq!(name(ExportKind::Theme, "line\nbreak"), "line break.json");
    }

    #[test]
    fn a_suggested_name_gets_the_kind_extension_and_a_default_when_empty() {
        let name = |kind, suggested| plan_export(kind, suggested, 0).expect("plan").file_name;
        assert_eq!(name(ExportKind::PulseCard, "card"), "card.svg");
        assert_eq!(name(ExportKind::PulseCard, "card.SVG"), "card.svg");
        assert_eq!(name(ExportKind::PulseCard, "card.json"), "card.json.svg");
        // A multi-byte stem right before the extension: the cut must land on
        // the character boundary in front of ".svg", not inside "é".
        assert_eq!(name(ExportKind::PulseCard, "carte-é.SVG"), "carte-é.svg");
        assert_eq!(name(ExportKind::PulseCard, "é"), "é.svg");
        assert_eq!(name(ExportKind::PulseCard, ""), "gitpulse-pulse-card.svg");
        assert_eq!(name(ExportKind::Theme, "  .json "), "gitpulse-theme.json");
        assert_eq!(
            name(ExportKind::TerminalOutput, "..."),
            "terminal-output.txt"
        );
    }

    #[test]
    fn a_long_suggested_name_is_bounded() {
        let long = "é".repeat(500);
        let file_name = plan_export(ExportKind::Theme, &long, 0)
            .expect("plan")
            .file_name;
        assert_eq!(
            file_name.chars().count(),
            MAX_SUGGESTED_STEM_CHARS + ".json".len()
        );
        assert!(file_name.ends_with(".json"));
    }

    #[test]
    fn the_kind_arrives_in_snake_case() {
        let kinds: Vec<ExportKind> =
            serde_json::from_str(r#"["terminal_output","theme","pulse_card"]"#).expect("parse");
        assert_eq!(
            kinds,
            vec![
                ExportKind::TerminalOutput,
                ExportKind::Theme,
                ExportKind::PulseCard
            ]
        );
        assert!(serde_json::from_str::<ExportKind>(r#""anything_else""#).is_err());
    }
}
