//! Renaming and moving notes with the links that point at them.
//!
//! A rename that leaves every `[[Old Name]]` behind is how a link graph
//! rots: the links still *look* right, the panel calls them broken, and the
//! reader has no idea which of the two is lying. The rewrite here answers to
//! the vault's own resolution rules — a target is rewritten only when this
//! index resolves it to the note being moved, so same-stem notes elsewhere
//! keep their links.

use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::index::{validated_relative_path, Vault, DEFAULT_MAX_NOTE_BYTES};
use super::note::{has_markdown_extension, stem, strip_markdown_extension};
use crate::md::model::{BlockKind, SpanKind, Utf16Mapper};
use crate::md::parse_checked;

/// Byte ranges of a document holding code or machine-read text, not prose.
///
/// A rename rewrites *links* — and only links. Fenced code blocks, indented
/// code, inline code spans, math and Mermaid sources, and frontmatter all
/// contain text that merely looks like `[x](Note.md)` or `[[Note]]`; it is
/// sample content the reader marked as literal, and editing it is silent
/// data corruption. Ranges come from the canonical parse rather than a
/// second fence scanner, so what this protects is exactly what the editor
/// renders as code — one owner per behaviour.
#[derive(Debug, Clone, Default)]
pub struct ProtectedRanges(Vec<Range<usize>>);

impl ProtectedRanges {
    /// Protects nothing — for callers rewriting structure-free text.
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// Derives protection from the same parse the editor uses.
    ///
    /// The parser reports UTF-16 offsets; the scanner works in bytes, so
    /// every range crosses `Utf16Mapper` on the way in.
    pub fn for_document(source: &str) -> Self {
        let Ok(parsed) = parse_checked(source) else {
            // A rename may only touch prose the canonical parser classified.
            // Refusing a parse therefore protects the whole note; treating it
            // as an empty model would rewrite code-like or malformed source
            // while claiming a complete transaction.
            // Bound first: `vec![0..n]` trips `single_range_in_vec_init`,
            // whose suggested rewrite collects a `Vec<usize>` of every offset
            // — a different type, and megabytes of it. One range covering the
            // whole note is exactly what is meant.
            let whole_note = 0..source.len();
            return Self(vec![whole_note]);
        };
        let mapper = Utf16Mapper::new(source);
        let mut ranges: Vec<Range<usize>> = Vec::new();

        for block in &parsed.blocks {
            let protected = block.kind == BlockKind::CodeBlock as u16
                || block.kind == BlockKind::MermaidBlock as u16
                || block.kind == BlockKind::MathBlock as u16
                || block.kind == BlockKind::Frontmatter as u16;
            if protected && block.end > block.start {
                ranges.push(mapper.to_byte(block.start)..mapper.to_byte(block.end));
            }
        }
        for span in &parsed.spans {
            if span.kind == SpanKind::InlineCode as u16 && span.end > span.start {
                ranges.push(mapper.to_byte(span.start)..mapper.to_byte(span.end));
            }
        }

        // The parse yields document-order, mostly disjoint ranges; sort and
        // merge so `is_protected` can binary-search. Overlap between an
        // inline span and its enclosing block would otherwise be harmless,
        // but merging keeps the invariant explicit.
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        Self(merged)
    }

    /// Whether `position` falls inside any protected region.
    ///
    /// A token starting exactly at a region's end is *not* protected: the
    /// boundary belongs to the live document.
    fn covers(&self, position: usize) -> bool {
        let index = self.0.partition_point(|range| range.end <= position);
        self.0
            .get(index)
            .is_some_and(|range| range.start <= position)
    }
}

/// The result of a successful rename.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenameOutcome {
    /// Notes whose text was rewritten (the moved note itself excluded).
    pub rewritten_notes: u32,
    /// Individual link occurrences rewritten across those notes.
    pub rewritten_links: u32,
    /// Rewrites that could not be committed after the source had moved.
    pub failed_rewrites: u32,
    /// False whenever any planned rewrite failed after the move.
    pub complete: bool,
}

struct StagedRewrite {
    index: usize,
    path: PathBuf,
    temporary: PathBuf,
    text: String,
    links: u32,
}

impl Vault {
    /// Moves the note at `from` to `to`, rewriting every link that resolved
    /// to it, and re-indexes.
    ///
    /// The file is moved first; only then is the vault updated in memory. A
    /// failed move therefore leaves the index exactly as it was — reporting
    /// "renamed" while the file stayed put would be worse than refusing.
    ///
    /// Returns `None` when `from` is not a known note or the destination is
    /// taken, so callers can surface a real message instead of guessing.
    pub fn rename_note(&mut self, from: &str, to: &str) -> Option<RenameOutcome> {
        self.rename_note_with_commit(from, to, commit_staged_write)
    }

    fn rename_note_with_commit<F>(
        &mut self,
        from: &str,
        to: &str,
        mut commit: F,
    ) -> Option<RenameOutcome>
    where
        F: FnMut(&Path, &Path) -> io::Result<()>,
    {
        let from_relative = validated_relative_path(from)?;
        let to_relative = validated_relative_path(to)?;
        let source_index = *self.by_path.get(from)?;
        if from == to
            || self
                .by_path
                .iter()
                .any(|(path, &index)| index != source_index && path.eq_ignore_ascii_case(to))
        {
            return None;
        }
        let root = std::fs::canonicalize(&self.root).ok()?;
        if !std::fs::symlink_metadata(&root)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_dir())
        {
            return None;
        }
        let from_disk = existing_regular_file(&root, from_relative)?;
        let to_disk = root.join(to_relative);

        // The index cannot referee this: it stores exact-case paths, and the
        // filesystem may be case-insensitive — asking it to move
        // `Notes/A.md` onto indexed `Work/a.md` by renaming to `Work/A.md`
        // would overwrite a note the index still believes exists. The
        // *filesystem* is the authority on whether anything is already there.
        // A case-only rename of the file onto itself (`A.md` → `a.md`) is
        // allowed through: same directory entry, nothing to lose.
        let destination_is_source = match std::fs::symlink_metadata(&to_disk) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                    return None;
                }
                let same_file = std::fs::canonicalize(&to_disk)
                    .ok()
                    .is_some_and(|destination| destination == from_disk);
                if !same_file {
                    return None;
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return None,
        };

        let old_stem = stem(from);
        let new_stem = stem(to);
        let new_path = without_extension(to);

        // When another note answers to the same stem the move will leave
        // behind, rewriting `[[Roadmap]]` to `[[Roadmap]]`'s twin would point
        // readers at whichever note resolution happens to prefer. Such links
        // are rewritten to the destination's full path instead — longer, but
        // unambiguous by construction.
        let stem_ambushed = self.notes.iter().enumerate().any(|(index, note)| {
            index != source_index && stem(&note.path).eq_ignore_ascii_case(&new_stem)
        });

        // Collected first against immutable state, applied after: the
        // rewrite consults `lookup`, which reads this very index, and a
        // mutation mid-scan would be the index answering questions about a
        // half-renamed world.
        let mut edits: Vec<(usize, PathBuf, String, u32)> = Vec::new();
        for (index, note) in self.notes.iter().enumerate() {
            if index == source_index {
                continue;
            }

            let mut resolve_wiki = |written: &str| -> Option<String> {
                if self.lookup(written) != Some(source_index) {
                    return None;
                }
                if written.eq_ignore_ascii_case(&old_stem) {
                    // Keep the reader's terse style unless ambiguity forbids.
                    (!stem_ambushed)
                        .then(|| new_stem.clone())
                        .or(Some(new_path.clone()))
                } else {
                    Some(new_path.clone())
                }
            };
            let source_note_path = note.path.clone();
            let mut resolve_markdown = |written: &str| -> Option<String> {
                // Same source-relative rules as the index — never a global
                // name lookup that would rewrite a sibling path into a
                // different note's spelling.
                if self.lookup_from(&source_note_path, written) != Some(source_index) {
                    return None;
                }
                Some(new_path.clone())
            };

            let (text, links) = {
                // One parse per candidate note: a rename is a user-visible
                // action measured in clicks, not per keystroke, and the
                // protection must describe this exact text.
                let protected = ProtectedRanges::for_document(&note.text);
                rewrite_links_in(
                    &note.text,
                    &protected,
                    &mut resolve_wiki,
                    &mut resolve_markdown,
                )
            };
            if links > 0 {
                if text.len() > DEFAULT_MAX_NOTE_BYTES {
                    return None;
                }
                // `Vault::build` accepts caller-provided notes, so the
                // mutation boundary must not assume every indexed path came
                // from the hardened scanner. Preflight every file we might
                // rewrite before moving the source.
                let relative = validated_relative_path(&note.path)?;
                let path = existing_regular_file(&root, relative)?;
                edits.push((index, path, text, links as u32));
            }
        }

        // Stage every rewrite before the source moves. Permission, capacity,
        // and stale-temp failures therefore leave the rename wholly refused;
        // only a final atomic replacement can produce a truthful partial.
        let mut staged = Vec::with_capacity(edits.len());
        for (index, path, text, links) in edits {
            let temporary = match stage_atomic_write(&path, &text) {
                Ok(temporary) => temporary,
                Err(error) => {
                    eprintln!(
                        "markdev: could not stage rewrite for {}: {error}",
                        path.display()
                    );
                    cleanup_staged_rewrites(&staged);
                    return None;
                }
            };
            staged.push(StagedRewrite {
                index,
                path,
                temporary,
                text,
                links,
            });
        }

        if let Some(parent) = to_disk.parent() {
            if let Err(error) = create_contained_directories(&root, parent) {
                eprintln!("markdev: could not create {}: {error}", parent.display());
                cleanup_staged_rewrites(&staged);
                return None;
            }
        }
        if let Err(error) = move_file(&from_disk, &to_disk, destination_is_source) {
            eprintln!("markdev: could not move {from} -> {to}: {error}");
            cleanup_staged_rewrites(&staged);
            return None;
        }

        let mut outcome = RenameOutcome {
            rewritten_notes: 0,
            rewritten_links: 0,
            failed_rewrites: 0,
            complete: true,
        };
        for rewrite in staged {
            // Atomic replace, matching what the app's own saves do: a crash
            // mid-write must leave the old note, never a truncated one.
            if let Err(error) = commit(&rewrite.temporary, &rewrite.path) {
                eprintln!(
                    "markdev: could not rewrite {}: {error}",
                    rewrite.path.display()
                );
                let _ = std::fs::remove_file(&rewrite.temporary);
                outcome.failed_rewrites = outcome.failed_rewrites.saturating_add(1);
                outcome.complete = false;
                continue;
            }
            self.notes[rewrite.index].text = rewrite.text;
            outcome.rewritten_notes = outcome.rewritten_notes.saturating_add(1);
            outcome.rewritten_links = outcome.rewritten_links.saturating_add(rewrite.links);
        }

        self.notes[source_index].path = to.to_string();
        self.reindex();

        Some(outcome)
    }
}

/// Resolves an existing regular file and proves it remains beneath `root`.
fn existing_regular_file(root: &Path, relative: &Path) -> Option<PathBuf> {
    let path = root.join(relative);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    canonical.starts_with(root).then_some(canonical)
}

/// Creates a destination parent without accepting a symlink at any existing
/// component, then proves the resulting directory resolves under `root`.
fn create_contained_directories(root: &Path, parent: &Path) -> io::Result<()> {
    let relative = parent.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "destination left the vault",
        )
    })?;
    reject_symlinked_components(root, relative, true)?;
    std::fs::create_dir_all(parent)?;
    reject_symlinked_components(root, relative, false)?;

    let canonical = std::fs::canonicalize(parent)?;
    if !canonical.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "destination parent resolved outside the vault",
        ));
    }
    Ok(())
}

fn reject_symlinked_components(
    root: &Path,
    relative: &Path,
    allow_missing_tail: bool,
) -> io::Result<()> {
    let mut current = root.to_path_buf();
    let mut missing = false;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination contained a non-normal component",
            ));
        };
        current.push(component);
        if missing {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "destination parent contains a symlink",
                ));
            }
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "destination parent contains a non-directory",
                ));
            }
            Ok(_) => {}
            Err(error) if allow_missing_tail && error.kind() == io::ErrorKind::NotFound => {
                missing = true;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// `a/b/name.md` becomes `a/b/name`; non-markdown extensions are kept.
fn without_extension(path: &str) -> String {
    strip_markdown_extension(path).to_string()
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const TEMPORARY_ATTEMPTS: usize = 32;

/// Writes bounded replacement bytes to a uniquely named sibling and flushes
/// them. A fixed name made one stale crash artifact a permanent denial of
/// service; process plus monotonic sequence avoids that without deleting an
/// unknown file another process may still own.
fn stage_atomic_write(path: &Path, text: &str) -> io::Result<PathBuf> {
    if text.len() > DEFAULT_MAX_NOTE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rewritten note exceeds the note byte limit",
        ));
    }
    let permissions = std::fs::symlink_metadata(path)?.permissions();
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "note has no parent directory")
    })?;

    for _ in 0..TEMPORARY_ATTEMPTS {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".markdev-tmp-{}-{sequence}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        deny_symlink_traversal(&mut options);
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let staged = (|| {
            file.set_permissions(permissions.clone())?;
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })();
        drop(file);
        if let Err(error) = staged {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        return Ok(temporary);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique rewrite temporary",
    ))
}

fn commit_staged_write(temporary: &Path, path: &Path) -> io::Result<()> {
    std::fs::rename(temporary, path)
}

fn cleanup_staged_rewrites(staged: &[StagedRewrite]) {
    for rewrite in staged {
        let _ = std::fs::remove_file(&rewrite.temporary);
    }
}

/// Moves a file, falling back to copy-then-remove when `rename(2)` refuses —
/// vaults live on whatever volume the reader chose, and those do not always
/// agree about cross-device renames.
fn move_file(from: &Path, to: &Path, destination_is_source: bool) -> std::io::Result<()> {
    if destination_is_source {
        move_file_with(from, to, DEFAULT_MAX_NOTE_BYTES, |source, destination| {
            std::fs::rename(source, destination)
        })
    } else {
        move_file_with(from, to, DEFAULT_MAX_NOTE_BYTES, rename_without_replacing)
    }
}

/// Atomically refuses a destination created after collision preflight.
#[cfg(target_os = "macos")]
fn rename_without_replacing(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const AT_FDCWD: i32 = -2;
    const RENAME_EXCL: u32 = 0x0000_0004;
    unsafe extern "C" {
        fn renameatx_np(
            from_fd: i32,
            from: *const std::ffi::c_char,
            to_fd: i32,
            to: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source contains NUL"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination contains NUL"))?;
    let result =
        unsafe { renameatx_np(AT_FDCWD, from.as_ptr(), AT_FDCWD, to.as_ptr(), RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_without_replacing(from: &Path, to: &Path) -> io::Result<()> {
    // `std::fs::rename` may overwrite a destination created after preflight.
    // Linking is exclusive on every supported filesystem; unlinking the old
    // name completes the move while retaining the source on every failure.
    std::fs::hard_link(from, to)?;
    if let Err(error) = std::fs::remove_file(from) {
        let _ = std::fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}

fn move_file_with<F>(from: &Path, to: &Path, maximum_bytes: usize, rename: F) -> io::Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    match rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_then_remove(from, to, maximum_bytes)
        }
        Err(error) => Err(error),
    }
}

fn copy_then_remove(from: &Path, to: &Path, maximum_bytes: usize) -> io::Result<()> {
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    harden_read_open(&mut source_options);
    let mut source = source_options.open(from)?;
    let source_metadata = source.metadata()?;
    if !source_metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rename source is no longer a regular file",
        ));
    }
    if source_metadata.len() > u64::try_from(maximum_bytes).unwrap_or(u64::MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rename source exceeds the copy byte limit",
        ));
    }
    let permissions = source_metadata.permissions();
    let mut destination_options = std::fs::OpenOptions::new();
    destination_options.write(true).create_new(true);
    deny_symlink_traversal(&mut destination_options);
    let mut destination = destination_options.open(to)?;

    let copy_result = (|| {
        let maximum_bytes = u64::try_from(maximum_bytes).unwrap_or(u64::MAX);
        let copied = io::copy(
            &mut Read::by_ref(&mut source).take(maximum_bytes.saturating_add(1)),
            &mut destination,
        )?;
        if copied > maximum_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rename source grew beyond the copy byte limit",
            ));
        }
        destination.set_permissions(permissions)?;
        destination.sync_all()
    })();
    drop(destination);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_file(to);
        return Err(error);
    }

    if let Err(error) = std::fs::remove_file(from) {
        // A failed fallback must leave the source as the sole authoritative
        // copy. Cleanup is best-effort, but the source is never touched again.
        let _ = std::fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}

fn deny_symlink_traversal(options: &mut std::fs::OpenOptions) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW_ANY: i32 = 0x2000_0000;
        options.custom_flags(O_NOFOLLOW_ANY);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = options;
}

fn harden_read_open(options: &mut std::fs::OpenOptions) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW_ANY: i32 = 0x2000_0000;
        const O_NONBLOCK: i32 = 0x0004;
        options.custom_flags(O_NOFOLLOW_ANY | O_NONBLOCK);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = options;
}

/// Rewrites wikilink and Markdown-link targets in `source`.
///
/// Each closure is asked with the target exactly as written and answers:
/// `None` — leave it alone, or `Some(replacement)` — put this in its place.
/// Splitting "should change" from "change into what" is how the alias, the
/// anchor, and Markdown's `.md` suffix all survive without this function
/// knowing anything about either vault.
pub fn rewrite_links_in(
    source: &str,
    protected: &ProtectedRanges,
    wiki: &mut dyn FnMut(&str) -> Option<String>,
    markdown: &mut dyn FnMut(&str) -> Option<String>,
) -> (String, usize) {
    let mut result = String::with_capacity(source.len());
    let mut rewritten = 0;
    let mut cursor = 0;

    while let Some(offset) = source[cursor..].find("[[") {
        let open = cursor + offset;

        // Code first, before anything about the token is decided: a `[[`
        // inside a fence is sample text, and scanning into it at all risks
        // the scanner's opinion outranking the reader's.
        if protected.covers(open) {
            // Emit up to the marker verbatim and step past it; the next
            // iteration resumes the search *after* this opener, so a fence
            // full of brackets costs one skip each rather than a rescan.
            result.push_str(&source[cursor..open + 2]);
            cursor = open + 2;
            continue;
        }

        match source[open..].find("]]") {
            Some(close_offset) => {
                let close = open + close_offset + 2;

                // An unclosed inner `[[` makes this outer one prose — but
                // its characters are still somebody's text. Keep them all
                // and resume one byte in, letting the scanner rediscover
                // whatever real token sits inside: `a [[b [[c]] d` must end
                // as `a [[b [[c]] d` with `[c]]` rewritten and every byte
                // of `b ` accounted for.
                if source[open + 2..close - 2].contains("[[") {
                    result.push_str(&source[cursor..=open]);
                    cursor = open + 1;
                    continue;
                }

                let token = &source[open + 2..close - 2];
                // Target is everything before `|` (alias) or `#` (anchor),
                // both of which survive a rename untouched.
                let cut = token.find(['|', '#']).unwrap_or(token.len());
                let (target, rest) = token.split_at(cut);

                if let Some(replacement) = wiki(target.trim()) {
                    result.push_str(&source[cursor..open + 2]);
                    result.push_str(&replacement);
                    result.push_str(rest);
                    result.push_str("]]");
                    rewritten += 1;
                } else {
                    // Left alone is still emitted: rejection decides *what
                    // happens to the link*, never whether its text exists.
                    result.push_str(&source[cursor..close]);
                }
                cursor = close;
            }
            None => {
                // No closing pair anywhere ahead: everything from here is
                // prose.
                break;
            }
        }
    }
    result.push_str(&source[cursor.min(source.len())..]);

    // The markdown pass scans the *rewritten* text, whose offsets moved
    // wherever a wikilink was replaced with a path of a different length.
    // Protection therefore comes from a fresh parse of exactly what this
    // pass is about to scan — the parse of the original describes a string
    // nobody is holding any more. Replacements are note paths and stems,
    // so they cannot themselves introduce fences or math; the fresh parse
    // sees the same code regions at their new positions.
    let markdown_protected = ProtectedRanges::for_document(&result);
    let (result, markdown_rewritten) =
        rewrite_markdown_targets(&result, &markdown_protected, markdown);
    rewritten += markdown_rewritten;

    (result, rewritten)
}

fn rewrite_markdown_targets(
    source: &str,
    protected: &ProtectedRanges,
    markdown: &mut dyn FnMut(&str) -> Option<String>,
) -> (String, usize) {
    let mut result = String::with_capacity(source.len());
    let mut rewritten = 0;
    let mut cursor = 0;

    while let Some(open_offset) = source[cursor..].find("](") {
        let open = cursor + open_offset + 2;

        // Same rule as the wiki scanner: inside code, `](Note.md)` is
        // sample text. Emit through the paren and move past it.
        if protected.covers(open) {
            result.push_str(&source[cursor..open]);
            cursor = open;
            continue;
        }

        result.push_str(&source[cursor..open]);

        let Some(close_offset) = source[open..].find(')') else {
            break;
        };
        let close = open + close_offset;
        let raw = &source[open..close];
        let trimmed = raw.trim();

        // Anchors ride along, exactly as they do in wikilinks.
        let cut = trimmed.find('#').unwrap_or(trimmed.len());
        let (target, anchor) = trimmed.split_at(cut);
        let target = target.trim();
        // Ask with the written spelling (extension included). The callback
        // owns source-relative lookup; this pass only preserves the suffix
        // the reader wrote.
        let written_extension = trailing_markdown_extension(target);

        if let Some(mut replacement) = markdown(target) {
            // Replacements come back without an extension (vault path stems).
            // Put back whatever suffix the link used, so `.mdx` stays `.mdx`.
            if let Some(extension) = written_extension {
                if !has_markdown_extension(&replacement) {
                    replacement.push('.');
                    replacement.push_str(extension);
                }
            }
            result.push_str(&replacement);
            result.push_str(anchor);
            result.push(')');
            rewritten += 1;
        } else {
            result.push_str(raw);
            result.push(')');
        }
        cursor = close + 1;
    }
    result.push_str(&source[cursor.min(source.len())..]);

    (result, rewritten)
}

fn trailing_markdown_extension(path: &str) -> Option<&str> {
    let stripped = strip_markdown_extension(path);
    if stripped.len() == path.len() {
        None
    } else {
        Some(&path[stripped.len() + 1..])
    }
}

#[cfg(test)]
mod move_tests {
    use super::{move_file_with, rewrite_links_in, ProtectedRanges};
    use crate::md::MAX_DOCUMENT_BYTES;
    use crate::vault::Vault;
    use std::cell::Cell;
    use std::io;

    #[test]
    fn parser_refusal_protects_every_byte_from_link_rewrites() {
        let mut source = String::from("[[Target]] and [target](Target.md) ");
        source.push_str(&"x".repeat(MAX_DOCUMENT_BYTES + 1 - source.len()));
        let protected = ProtectedRanges::for_document(&source);
        let calls = Cell::new(0usize);
        let (rewritten, count) = rewrite_links_in(
            &source,
            &protected,
            &mut |_| {
                calls.set(calls.get() + 1);
                Some("Moved".to_string())
            },
            &mut |_| {
                calls.set(calls.get() + 1);
                Some("Moved".to_string())
            },
        );

        assert_eq!(rewritten, source);
        assert_eq!(count, 0);
        assert_eq!(
            calls.get(),
            0,
            "unclassified source must never reach a rewriter"
        );
    }

    #[test]
    fn non_cross_device_rename_failure_never_copies_or_removes_the_source() {
        let root = std::env::temp_dir().join(format!(
            "markdev-move-refusal-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let source = root.join("Source.md");
        let destination = root.join("Destination.md");
        std::fs::write(&source, "irreplaceable").unwrap();

        let result = move_file_with(&source, &destination, 1_024, |_, _| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
        });

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "irreplaceable");
        assert!(!std::fs::exists(&destination).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_device_rename_failure_uses_the_bounded_copy_fallback() {
        let root = std::env::temp_dir().join(format!(
            "markdev-move-cross-device-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let source = root.join("Source.md");
        let destination = root.join("Destination.md");
        std::fs::write(&source, "move me").unwrap();

        let result = move_file_with(&source, &destination, 1_024, |_, _| {
            Err(io::Error::from_raw_os_error(18))
        });

        result.unwrap();
        assert!(!std::fs::exists(&source).unwrap());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "move me");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_device_copy_refuses_oversized_growth_and_preserves_source() {
        let root = std::env::temp_dir().join(format!(
            "markdev-move-oversized-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let source = root.join("Source.md");
        let destination = root.join("Destination.md");
        std::fs::write(&source, vec![b'x'; 1_025]).unwrap();

        let result = move_file_with(&source, &destination, 1_024, |_, _| {
            Err(io::Error::from_raw_os_error(18))
        });

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::metadata(&source).unwrap().len(), 1_025);
        assert!(!std::fs::exists(&destination).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_reports_a_typed_partial_after_a_commit_failure() {
        let root = std::env::temp_dir().join(format!(
            "markdev-rename-partial-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Source.md"), "# Source").unwrap();
        std::fs::write(root.join("Ref.md"), "See [[Source]].\n").unwrap();
        let mut vault = Vault::open(&root);

        let outcome = vault
            .rename_note_with_commit("Source.md", "Moved.md", |_, _| {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            })
            .expect("the source move happened");

        assert!(!outcome.complete);
        assert_eq!(outcome.failed_rewrites, 1);
        assert_eq!(outcome.rewritten_notes, 0);
        assert_eq!(outcome.rewritten_links, 0);
        assert!(!std::fs::exists(root.join("Source.md")).unwrap());
        assert!(std::fs::exists(root.join("Moved.md")).unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join("Ref.md")).unwrap(),
            "See [[Source]].\n"
        );
        assert!(vault.note("Source.md").is_none());
        assert!(vault.note("Moved.md").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
