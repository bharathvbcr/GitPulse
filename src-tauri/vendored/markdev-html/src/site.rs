//! A whole vault exported as a linked static site.
//!
//! Every note becomes a page at the same relative path with an `.html`
//! extension, rendered exactly as **Export as HTML…** renders one note, and
//! links between notes point at each other's pages. An `index.html` at the
//! site root lists every page by folder. Pictures are embedded in each page;
//! audio, video and PDFs are linked where they live in the vault.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    encode_path_component, render_document, render_document_with_options, ExportOptions,
    FileAccess, LinkBase, SiteLayout, MAX_SOURCE_BYTES,
};

/// Most notes one site export renders.
pub const MAX_SITE_NOTES: usize = 20_000;
/// Most directory entries one site export examines while collecting notes.
const MAX_SITE_SCAN_ENTRIES: usize = 200_000;

/// What a site export did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SiteReport {
    /// Pages written, not counting the index.
    pub pages: usize,
    /// Vault-relative paths of notes that were not exported, with why.
    pub skipped: Vec<String>,
    /// The site's front page.
    pub index: PathBuf,
    /// Whether the vault had more notes than [`MAX_SITE_NOTES`].
    pub truncated: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SiteExportError {
    /// The vault is not a readable folder.
    VaultUnreadable,
    /// The site would be written over the vault itself.
    OutputIsVault,
    /// The site folder could not be created or written.
    OutputUnwritable(String),
}

/// Renders every note under `vault_root` into `output_root`.
///
/// `output_root` is created if needed. It may sit inside the vault (its own
/// pages are then skipped when collecting notes) but must not be the vault.
pub fn export_site(vault_root: &Path, output_root: &Path) -> Result<SiteReport, SiteExportError> {
    let vault = fs::canonicalize(vault_root).map_err(|_| SiteExportError::VaultUnreadable)?;
    if !vault.is_dir() {
        return Err(SiteExportError::VaultUnreadable);
    }
    fs::create_dir_all(output_root)
        .map_err(|e| SiteExportError::OutputUnwritable(e.to_string()))?;
    let output = fs::canonicalize(output_root)
        .map_err(|e| SiteExportError::OutputUnwritable(e.to_string()))?;
    if output == vault {
        return Err(SiteExportError::OutputIsVault);
    }

    let (notes, truncated) = collect_notes(&vault, &output);
    let mut report = SiteReport {
        truncated,
        ..SiteReport::default()
    };
    let mut written: Vec<(PathBuf, String)> = Vec::new();

    for note in notes {
        let Ok(relative) = note.strip_prefix(&vault) else {
            continue;
        };
        let relative = relative.to_path_buf();
        let display = relative.to_string_lossy().replace('\\', "/");
        let Some(source) = read_note(&note) else {
            report
                .skipped
                .push(format!("{display}: unreadable or too large"));
            continue;
        };
        let page = output.join(&relative).with_extension("html");
        let Some(folder) = page.parent() else {
            continue;
        };
        if let Err(error) = fs::create_dir_all(folder) {
            report.skipped.push(format!("{display}: {error}"));
            continue;
        }
        let title = note
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| display.clone());
        let options = ExportOptions {
            asset_base: note.parent(),
            vault_root: Some(&vault),
            link_base: LinkBase::Directory(folder),
            site: Some(SiteLayout {
                vault_root: &vault,
                output_root: &output,
            }),
            file_access: FileAccess::Unrestricted,
            remote_media: false,
            max_embedded_bytes: None,
        };
        let html = match render_document_with_options(&source, &title, &options) {
            Ok(html) => html,
            Err(error) => {
                report.skipped.push(format!("{display}: {error:?}"));
                continue;
            }
        };
        if let Err(error) = write_atomically(&page, html.as_bytes()) {
            report.skipped.push(format!("{display}: {error}"));
            continue;
        }
        report.pages += 1;
        written.push((relative.with_extension("html"), title));
    }

    let site_title = vault
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Vault".to_owned());
    let index = output.join("index.html");
    let html = render_document(&index_markdown(&site_title, &written), &site_title)
        .map_err(|e| SiteExportError::OutputUnwritable(format!("{e:?}")))?;
    write_atomically(&index, html.as_bytes())
        .map_err(|e| SiteExportError::OutputUnwritable(e.to_string()))?;
    report.index = index;
    Ok(report)
}

/// Every Markdown note in the vault, sorted, skipping hidden folders,
/// `node_modules`, and the site folder itself.
fn collect_notes(vault: &Path, output: &Path) -> (Vec<PathBuf>, bool) {
    let mut notes = Vec::new();
    let mut queue = VecDeque::from([vault.to_path_buf()]);
    let mut seen = 0usize;
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            seen += 1;
            if seen > MAX_SITE_SCAN_ENTRIES {
                return (notes, true);
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if path != output {
                    queue.push_back(path);
                }
            } else if kind.is_file() {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".md") || lower.ends_with(".markdown") {
                    if notes.len() >= MAX_SITE_NOTES {
                        return (notes, true);
                    }
                    notes.push(path);
                }
            }
        }
    }
    notes.sort();
    (notes, false)
}

fn read_note(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > MAX_SOURCE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Writes beside the destination and renames over it, so a failed export
/// never leaves a half-written page behind.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The front page: every note by folder, linking to its page.
fn index_markdown(title: &str, pages: &[(PathBuf, String)]) -> String {
    let mut folders: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (relative, name) in pages {
        let folder = relative
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let href: Vec<String> = relative
            .components()
            .map(|c| encode_path_component(&c.as_os_str().to_string_lossy()))
            .collect();
        folders
            .entry(folder)
            .or_default()
            .push((escape_markdown(name), href.join("/")));
    }
    let mut markdown = format!("# {}\n\n", escape_markdown(title));
    markdown.push_str(&format!(
        "{} {}\n\n",
        pages.len(),
        if pages.len() == 1 { "note" } else { "notes" }
    ));
    for (folder, entries) in folders {
        if !folder.is_empty() {
            markdown.push_str(&format!("## {}\n\n", escape_markdown(&folder)));
        }
        for (name, href) in entries {
            markdown.push_str(&format!("- [{name}](<{href}>)\n"));
        }
        markdown.push('\n');
    }
    markdown
}

/// Escapes text so Markdown reads it literally.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if "\\`*_{}[]<>()#+-.!|~=%^$".contains(character) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}
