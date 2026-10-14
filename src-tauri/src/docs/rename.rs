//! Link-preserving markdown rename.
//!
//! The vendored `rewrite_links_in` finds each link and asks a callback for the
//! text to put in its place; it does not decide what that text says. A
//! markdown destination is relative to the note that holds it, so the text is
//! worked out here, per linking note, by [`express_link`]. Everything is
//! planned against one snapshot before anything is written, so the gate can
//! judge every path the rename will touch, and each write is made only over the
//! bytes the plan was made from.

use super::{invalidate, load_vault, normalize_rel, refresh, BrokenLink, DocsStatus};
use crate::engine::git_cli::{sandbox_join, sandbox_write_expecting, validate_repo};
use crate::engine::git_writer::{GitWriter, MovePlan};
use markdev_vault::note::{has_markdown_extension, stem, strip_markdown_extension};
use markdev_vault::rename::{rewrite_links_in, ProtectedRanges};
use markdev_vault::{Note, Vault, DEFAULT_MAX_NOTE_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// One file a rename could not rewrite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocRenameFailure {
    pub path: String,
    pub error: String,
}

/// Outcome of a link-preserving markdown rename.
///
/// The rename is not rolled back when some rewrites cannot be made: a rollback
/// is a second mutation that can itself fail halfway and overwrite what
/// someone typed meanwhile. `complete` says whether every planned rewrite
/// landed; the lists say which did not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocRenameOutcome {
    pub from: String,
    pub to: String,
    /// Paths whose link text was rewritten, written and staged. Includes `to`
    /// when the moved note's own links changed.
    pub rewritten_paths: Vec<String>,
    pub links_rewritten: u32,
    /// Every planned rewrite was written and staged.
    pub complete: bool,
    /// Paths whose bytes changed after the plan was made, so nothing was
    /// written over them.
    pub skipped_conflict: Vec<String>,
    /// Paths that could not be written or staged.
    pub failed: Vec<DocRenameFailure>,
    /// Links that resolved before the rename and would not after it, as the
    /// rewritten vault reads them. A reference-style definition
    /// (`[ref]: Note.md`) is the case this exists for: it is not rewritten.
    pub left_broken: Vec<BrokenLink>,
    /// What the vault the rename was planned from covered. A note it did not
    /// scan was neither searched for links nor rewritten.
    pub scan: DocsStatus,
}

/// Everything a rename is about to do, handed to the gate before the first
/// byte moves.
pub struct DocRenamePlan<'a> {
    /// The move `GitWriter::move_path` decided on.
    pub mv: &'a MovePlan,
    /// Every path whose content will be rewritten after the move. The moved
    /// note appears under its new path.
    pub rewrites: &'a [String],
}

/// One planned rewrite: the bytes it was planned from and the bytes it writes.
pub(super) struct PlannedEdit {
    /// Where the file lives once the move is done.
    path: String,
    original: String,
    text: String,
    links: u32,
}

/// [`GitWriter::move_path`] plus staged link rewrites via `rewrite_links_in`.
///
/// Reviewable in the diff viewer before commit. Does **not** call
/// `Vault::rename_note`. `gate` judges the move and every rewrite before
/// anything moves.
pub fn rename_doc(
    repo_path: &str,
    from: &str,
    to: &str,
    gate: impl FnOnce(&DocRenamePlan) -> Result<(), String>,
) -> Result<DocRenameOutcome, String> {
    let repo = validate_repo(repo_path)?;
    let from = normalize_rel(from)?;
    let to = normalize_rel(to)?;
    if from == to {
        return Err("source and destination paths are the same".into());
    }
    if !has_markdown_extension(&from) || !has_markdown_extension(&to) {
        return Err("doc rename only moves markdown notes".into());
    }

    // Brought current from the cached snapshot: only files whose stat key
    // moved since the last refresh are read again.
    let built = load_vault(repo_path, true)?;
    let vault = &built.vault;
    if !vault.notes().iter().any(|n| n.path == from) {
        return Err(format!("note not in vault: {from}"));
    }
    if vault.notes().iter().any(|n| n.path == to) {
        return Err(format!("destination already exists in vault: {to}"));
    }
    // Filesystem authority: refuse overwriting an on-disk file the index
    // somehow missed.
    let to_abs = sandbox_join(&repo, &to)?;
    if to_abs.exists() {
        return Err(format!("destination already exists on disk: {to}"));
    }

    let edits = plan_edits(vault, &from, &to)?;
    let left_broken = links_the_rename_breaks(&repo, vault, &from, &to, &edits);
    let rewrites: Vec<String> = edits.iter().map(|edit| edit.path.clone()).collect();

    // Move first so a failed mv leaves rewrites unapplied. The gate runs inside
    // the move's own critical section, on the plan it decided on.
    GitWriter::move_path(repo_path, &from, &to, |mv| {
        gate(&DocRenamePlan {
            mv,
            rewrites: &rewrites,
        })
    })?;

    let mut links_rewritten = 0u32;
    let mut rewritten_paths = Vec::with_capacity(edits.len());
    let mut skipped_conflict = Vec::new();
    let mut failed = Vec::new();
    // Never `?` here: the move has happened, so one file's failure must not
    // hide what the others did.
    for (attempt, edit) in edits.into_iter().enumerate() {
        match write_planned(repo_path, &edit, attempt) {
            Ok(true) => match GitWriter::stage_file(repo_path, &edit.path) {
                Ok(()) => {
                    links_rewritten = links_rewritten.saturating_add(edit.links);
                    rewritten_paths.push(edit.path);
                }
                Err(error) => failed.push(DocRenameFailure {
                    path: edit.path,
                    error: format!("rewritten but not staged: {error}"),
                }),
            },
            Ok(false) => skipped_conflict.push(edit.path),
            Err(error) => failed.push(DocRenameFailure {
                path: edit.path,
                error,
            }),
        }
    }

    // The moved and rewritten notes carry new stat keys, so this re-reads
    // just those. If it fails, drop the pre-rename snapshot rather than keep
    // serving it: the next query then builds from disk.
    if refresh(repo_path).is_err() {
        invalidate(repo_path);
    }

    Ok(DocRenameOutcome {
        from,
        to,
        rewritten_paths,
        links_rewritten,
        complete: skipped_conflict.is_empty() && failed.is_empty(),
        skipped_conflict,
        failed,
        left_broken,
        scan: built.status.clone(),
    })
}

/// Writes one planned rewrite over exactly the bytes it was planned from.
/// `Ok(false)`: the file changed since, and was left as it is.
fn write_planned(repo_path: &str, edit: &PlannedEdit, attempt: usize) -> Result<bool, String> {
    #[cfg(test)]
    if super::probe::write_fault_at() == Some(attempt) {
        return Err("injected write fault".into());
    }
    let _ = attempt;
    sandbox_write_expecting(repo_path, &edit.path, &edit.original, &edit.text)
}

/// Every rewrite the rename makes, against the snapshot, before any is applied.
pub(super) fn plan_edits(vault: &Vault, from: &str, to: &str) -> Result<Vec<PlannedEdit>, String> {
    let old_stem = stem(from);
    let new_stem = stem(to);
    let new_path = strip_markdown_extension(to).to_string();
    let stem_ambushed = vault
        .notes()
        .iter()
        .any(|note| note.path != from && stem(&note.path).eq_ignore_ascii_case(&new_stem));

    let mut edits = Vec::new();
    for note in vault.notes() {
        let moved = note.path == from;
        let linking_old = note.path.as_str();
        let linking_new = if moved { to } else { linking_old };
        let mut resolve_wiki = |written: &str| -> Option<String> {
            // Same rule as MarkDev's rename_note: rewrite only when the
            // written target resolves to the note being moved.
            let resolved = vault.resolve(written, None)?;
            if resolved.path != from {
                return None;
            }
            if written.eq_ignore_ascii_case(&old_stem) {
                (!stem_ambushed)
                    .then(|| new_stem.clone())
                    .or(Some(new_path.clone()))
            } else {
                Some(new_path.clone())
            }
        };
        let mut resolve_markdown = |written: &str| -> Option<String> {
            let dest = split_written(written).dest;
            let resolved = vault.resolve_from(linking_old, dest, None)?;
            let to_the_moved_note = resolved.path == from;
            if !moved && !to_the_moved_note {
                return None;
            }
            let target = if to_the_moved_note {
                to
            } else {
                resolved.path.as_str()
            };
            let replacement = express_link(written, parent_dir(linking_new), target)?;
            if replacement == written {
                return None;
            }
            if moved && !to_the_moved_note {
                // The moved note's other links say what they said when they
                // already reach the same note from the new folder.
                let from_new = vault.resolve_from(linking_new, dest, None);
                if from_new.is_some_and(|now| now.path == resolved.path) {
                    return None;
                }
            }
            Some(replacement)
        };

        let protected = ProtectedRanges::for_document(&note.text);
        let (text, links) = rewrite_links_in(
            &note.text,
            &protected,
            &mut resolve_wiki,
            &mut resolve_markdown,
        );
        if links == 0 || text == note.text {
            continue;
        }
        if text.len() > DEFAULT_MAX_NOTE_BYTES {
            return Err(format!(
                "rewrite of {} exceeds the note size limit",
                note.path
            ));
        }
        edits.push(PlannedEdit {
            path: linking_new.to_string(),
            original: note.text.clone(),
            text,
            links: links as u32,
        });
    }
    Ok(edits)
}

/// Links that are broken in the vault the rename would leave and were not
/// broken in the one it starts from, by multiset difference so a note with two
/// identical dead links is not credited with one of them twice.
pub(super) fn links_the_rename_breaks(
    repo: &Path,
    vault: &Vault,
    from: &str,
    to: &str,
    edits: &[PlannedEdit],
) -> Vec<BrokenLink> {
    let edited: HashMap<&str, &str> = edits
        .iter()
        .map(|edit| (edit.path.as_str(), edit.text.as_str()))
        .collect();
    let notes: Vec<Note> = vault
        .notes()
        .iter()
        .map(|note| {
            let path = if note.path == from {
                to
            } else {
                note.path.as_str()
            };
            match edited.get(path) {
                Some(text) => Note::parse(path, text),
                None if note.path == from => Note::parse(path, &note.text),
                None => note.clone(),
            }
        })
        .collect();
    let after = Vault::build(repo.to_path_buf(), notes);

    let mut before: HashMap<(String, String), usize> = HashMap::new();
    for (source, target) in vault.broken_links() {
        let source = if source == from {
            to.to_string()
        } else {
            source
        };
        *before.entry((source, target)).or_default() += 1;
    }
    let mut broken = Vec::new();
    for (source, target) in after.broken_links() {
        match before.get_mut(&(source.clone(), target.clone())) {
            Some(count) if *count > 0 => *count -= 1,
            _ => broken.push(BrokenLink { source, target }),
        }
    }
    broken
}

/// A markdown destination as the scanner hands it over: `<`, the path, `>`,
/// then whatever follows (a title), each kept so it can be put back verbatim.
struct WrittenDest<'a> {
    open: &'a str,
    dest: &'a str,
    close: &'a str,
    tail: &'a str,
}

/// The scanner cuts a destination at its first `#`, so an angle-bracketed
/// anchor link arrives as `<a b.md` with no closing bracket.
fn split_written(written: &str) -> WrittenDest<'_> {
    if let Some(rest) = written.strip_prefix('<') {
        return match rest.find('>') {
            Some(end) => WrittenDest {
                open: "<",
                dest: &rest[..end],
                close: ">",
                tail: &rest[end + 1..],
            },
            None => WrittenDest {
                open: "<",
                dest: rest,
                close: "",
                tail: "",
            },
        };
    }
    match written.find(char::is_whitespace) {
        Some(end) => WrittenDest {
            open: "",
            dest: &written[..end],
            close: "",
            tail: &written[end..],
        },
        None => WrittenDest {
            open: "",
            dest: written,
            close: "",
            tail: "",
        },
    }
}

/// How the link `written` should read so that, from the note whose folder is
/// `linking_dir`, it reaches the note now at `target`.
///
/// A leading `/` is vault-root-relative and stays so; anything else is relative
/// to the linking note, so it is rebuilt from the two folders. The spelling the
/// author chose is kept where it can be: the extension (or its absence), a
/// leading `./`, angle brackets, a title, and percent-escaping.
fn express_link(written: &str, linking_dir: &str, target: &str) -> Option<String> {
    let parts = split_written(written);
    let dest = parts.dest;
    if dest.is_empty() {
        return None;
    }
    let rooted = dest.starts_with('/');
    let path = if rooted {
        format!("/{target}")
    } else {
        relative_path(linking_dir, target)
    };
    let path = if has_markdown_extension(dest) {
        path
    } else {
        strip_markdown_extension(&path).to_string()
    };
    let path = if !rooted && dest.starts_with("./") && !path.starts_with("../") {
        format!("./{path}")
    } else {
        path
    };
    let escaped = has_percent_escape(dest);
    let path = escape_path(&path, escaped, !parts.open.is_empty());
    Some(format!(
        "{}{}{}{}",
        parts.open, path, parts.close, parts.tail
    ))
}

/// `target` as seen from the folder `from_dir`: the common folders dropped, a
/// `../` for each one left in `from_dir`.
fn relative_path(from_dir: &str, target: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|c| !c.is_empty()).collect();
    let (target_dir, file) = target.rsplit_once('/').unwrap_or(("", target));
    let down: Vec<&str> = target_dir.split('/').filter(|c| !c.is_empty()).collect();
    let common = from
        .iter()
        .zip(&down)
        .take_while(|(left, right)| left == right)
        .count();
    let mut out = "../".repeat(from.len() - common);
    for component in &down[common..] {
        out.push_str(component);
        out.push('/');
    }
    out.push_str(file);
    out
}

fn has_percent_escape(text: &str) -> bool {
    text.as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

/// Escapes what would end or change a destination. Parentheses and angle
/// brackets always go: the scanner ends a link at the first `)`. A space goes
/// in a bare destination, and stays raw inside `<…>`. A destination that was
/// written percent-escaped is read back through one decode, so there `%` goes
/// too; so it does when the path itself holds something that decode would eat.
fn escape_path(path: &str, escaped: bool, bracketed: bool) -> String {
    let escaped = escaped || has_percent_escape(path);
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            ' ' if escaped || !bracketed => out.push_str("%20"),
            '%' if escaped => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_climb_out_of_the_linking_folder_and_descend_into_the_target() {
        assert_eq!(relative_path("docs", "docs/Plan.md"), "Plan.md");
        assert_eq!(relative_path("docs", "plans/Plan.md"), "../plans/Plan.md");
        assert_eq!(relative_path("", "plans/Plan.md"), "plans/Plan.md");
        assert_eq!(relative_path("a/b/c", "a/x/Plan.md"), "../../x/Plan.md");
        assert_eq!(relative_path("a/b", "a/b/c/Plan.md"), "c/Plan.md");
        assert_eq!(relative_path("a", "Plan.md"), "../Plan.md");
    }

    #[test]
    fn a_link_keeps_the_spelling_its_author_chose() {
        let up = |written: &str| express_link(written, "docs", "plans/Plan.md");
        assert_eq!(up("Roadmap.md").unwrap(), "../plans/Plan.md");
        assert_eq!(up("Roadmap").unwrap(), "../plans/Plan");
        assert_eq!(up("./Roadmap.md").unwrap(), "../plans/Plan.md");
        assert_eq!(up("/docs/Roadmap.md").unwrap(), "/plans/Plan.md");
        assert_eq!(up("Roadmap.md \"t\"").unwrap(), "../plans/Plan.md \"t\"");
        assert_eq!(up("<Roadmap.md>").unwrap(), "<../plans/Plan.md>");
        assert_eq!(up("<Roadmap.md").unwrap(), "<../plans/Plan.md");
        assert_eq!(
            express_link("./Roadmap.md", "docs", "docs/Plan.md").unwrap(),
            "./Plan.md"
        );
        assert_eq!(
            express_link("Roadmap.mdx", "docs", "docs/Plan.md").unwrap(),
            "Plan.md"
        );
    }

    #[test]
    fn what_would_end_or_change_a_destination_is_escaped() {
        let to = |written: &str| express_link(written, "", "a b/(c).md").unwrap();
        assert_eq!(to("x.md"), "a%20b/%28c%29.md");
        assert_eq!(to("<x.md>"), "<a b/%28c%29.md>");
        assert_eq!(to("%78.md"), "a%20b/%28c%29.md");
        assert_eq!(
            express_link("x.md", "", "100%41.md").unwrap(),
            "100%2541.md"
        );
    }

    #[test]
    fn an_empty_destination_is_not_a_link_to_rewrite() {
        assert_eq!(express_link("", "docs", "a.md"), None);
        assert_eq!(express_link("<>", "docs", "a.md"), None);
    }
}
