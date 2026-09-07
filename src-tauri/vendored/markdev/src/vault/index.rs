//! The vault: notes, the link graph between them, tags, and search.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::note::{
    has_url_scheme, percent_decode_once, strip_markdown_extension, Note, NoteLinkKind, WikiLink,
};
use super::search::SearchIndex;

/// A link pointing at a note, with the line it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlink {
    /// Vault-relative path of the note containing the link.
    pub path: String,
    pub title: String,
    /// The line as written, for context in the panel.
    pub context: String,
    pub line: u32,
    /// Where in the source note the link sits, for jump-to-source.
    pub offset: u32,
}

/// A note that names this one without linking to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkedMention {
    pub path: String,
    pub title: String,
    pub context: String,
    pub line: u32,
    /// UTF-16 offset of the mention in its note, so it can be turned into a
    /// link — and jumped to, since the editor indexes by UTF-16.
    pub offset: u32,
}

/// A search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub context: String,
    pub line: u32,
    pub score: u32,
}

/// A tag and how many notes carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

/// Largest note the index will hold in memory by default (16 MiB).
pub const DEFAULT_MAX_NOTE_BYTES: usize = 16 * 1_048_576;

/// Largest aggregate body-text payload retained by an initial vault scan
/// (256 MiB). Parsed metadata adds overhead, so this deliberately bounds the
/// raw input well below a process-sized allocation.
pub const DEFAULT_MAX_VAULT_BYTES: usize = 256 * 1_048_576;

/// Accepts only a non-empty lexical path made entirely of normal relative
/// components. Filesystem mutation and in-memory mutation share this owner so
/// a caller cannot insert a path the rename boundary would later refuse.
pub(crate) fn validated_relative_path(value: &str) -> Option<&Path> {
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    if value
        .split(['/', '\\'])
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return None;
    }
    let mut components = path.components();
    let first = components.next()?;
    if !matches!(first, Component::Normal(_))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path)
}

/// Resource bounds for a recursive vault scan.
///
/// The Swift catch-up walker uses the same defaults. Keeping both limits
/// explicit prevents a corrupt or adversarial tree from turning vault open
/// into unbounded recursion or memory growth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultScanLimits {
    /// The root is depth zero. Files directly inside a directory at this
    /// depth are still considered; child directories are not descended into.
    pub max_depth: usize,
    /// Every visible or ignored directory entry examined consumes one unit.
    pub max_entries: usize,
    /// Maximum bytes read from any one note.
    pub max_note_bytes: usize,
    /// Maximum aggregate note bytes read and retained by the scan.
    pub max_total_bytes: usize,
}

impl Default for VaultScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 48,
            max_entries: 100_000,
            max_note_bytes: DEFAULT_MAX_NOTE_BYTES,
            max_total_bytes: DEFAULT_MAX_VAULT_BYTES,
        }
    }
}

/// What the initial filesystem scan was actually able to cover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultScanStatus {
    pub scan_performed: bool,
    pub visited_entries: usize,
    /// In-policy Markdown candidates encountered during the bounded walk.
    pub discovered_files: usize,
    /// Metadata-snapshot bytes for every discovered candidate.
    pub discovered_bytes: u64,
    /// Candidates admitted by the per-note and aggregate byte budgets.
    pub selected_files: usize,
    /// Metadata-snapshot bytes for selected candidates.
    pub selected_bytes: u64,
    /// Selected files successfully read as bounded UTF-8 and parsed.
    pub indexed_files: usize,
    /// Actual UTF-8 bytes retained in indexed notes.
    pub indexed_bytes: u64,
    /// Discovered files not indexed for any reason. This aggregate prevents a
    /// caller from mistaking a byte-capped sample for a complete inventory.
    pub skipped_files: usize,
    pub skipped_symlinks: usize,
    pub unreadable_directories: usize,
    pub unreadable_entries: usize,
    pub unreadable_files: usize,
    pub oversized_files: usize,
    pub hit_depth_limit: bool,
    pub hit_entry_limit: bool,
    pub hit_total_byte_limit: bool,
}

impl VaultScanStatus {
    /// `true` only when the entire in-policy tree was inspected and read.
    pub fn is_complete(&self) -> bool {
        self.scan_performed
            && !self.hit_depth_limit
            && !self.hit_entry_limit
            && !self.hit_total_byte_limit
            && self.skipped_files == 0
            && self.unreadable_directories == 0
            && self.unreadable_entries == 0
            && self.unreadable_files == 0
            && self.oversized_files == 0
    }
}

/// A link a note points *out* at, with where it lands.
///
/// The mirror of [`Backlink`], and it carries the resolution rather than
/// leaving the caller to ask again: "which notes is this one connected to"
/// is one question, and answering it in two calls invites a caller to pair
/// a link with a resolution made against a different index state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingLink {
    /// The target as written, without the `#anchor` or `|alias`.
    pub target: String,
    /// Heading anchor, when the link was `[[Note#Heading]]`.
    pub anchor: Option<String>,
    /// What the reader sees — the alias when there is one.
    pub display: String,
    pub line: u32,
    /// UTF-16 offset of the link in this note.
    pub offset: u32,
    /// Vault-relative path of the note it resolves to, or `None` when the
    /// link is broken. A broken link is reported rather than dropped: a note
    /// linking at something that does not exist yet is ordinary in a vault,
    /// and a caller that wants only the live ones can filter.
    pub path: Option<String>,
}

/// A resolved link destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub path: String,
    /// UTF-16 offset of the anchored heading, when the link had one.
    pub offset: Option<u32>,
}

/// An indexed vault.
#[derive(Debug, Default)]
pub struct Vault {
    // `pub(crate)`: rename.rs is part of this vault's own machinery — moving
    // a note means touching its file, every link that resolved to it, and
    // then the index, which no public accessor sequence can express.
    pub(crate) root: PathBuf,
    pub(crate) notes: Vec<Note>,
    /// Vault-relative path to index in `notes`.
    pub(crate) by_path: HashMap<String, usize>,
    /// Lowercased name (stem, title, or alias) to the notes answering to it.
    by_name: HashMap<String, Vec<usize>>,
    /// Target note index to the links pointing at it.
    backlinks: HashMap<usize, Vec<(usize, usize)>>,
    search: SearchIndex,
    scan_status: VaultScanStatus,
}

impl Vault {
    /// Reads Markdown under `root` with the standard safety bounds.
    /// Inspect [`Vault::scan_status`] before treating the inventory as complete.
    pub fn open(root: impl AsRef<Path>) -> Vault {
        Self::open_with_limits(root, VaultScanLimits::default())
    }

    /// Reads Markdown under `root` while enforcing caller-selected bounds.
    ///
    /// Symlinks are never followed. The canonical root is the containment
    /// boundary, and every directory and file is checked against it again
    /// before it is read. See [`Vault::scan_status`] before treating absence
    /// from the returned index as proof that a path is absent from disk.
    pub fn open_with_limits(root: impl AsRef<Path>, limits: VaultScanLimits) -> Vault {
        let requested_root = root.as_ref().to_path_buf();
        let Ok(root) = std::fs::canonicalize(&requested_root) else {
            let mut vault = Vault::build(requested_root, Vec::new());
            vault.scan_status.scan_performed = true;
            vault.scan_status.unreadable_directories = 1;
            return vault;
        };

        let (notes, scan_status) = collect(&root, limits);
        let mut vault = Vault::build(root, notes);
        vault.scan_status = scan_status;
        vault
    }

    /// Builds a vault from notes already in memory. Used by tests, and by
    /// callers that have the text but not the files.
    pub fn build(root: PathBuf, notes: Vec<Note>) -> Vault {
        let mut vault = Vault {
            root,
            notes,
            ..Default::default()
        };
        vault.reindex();
        vault
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Coverage of the filesystem walk that produced this index.
    pub fn scan_status(&self) -> VaultScanStatus {
        self.scan_status
    }

    pub fn note(&self, path: &str) -> Option<&Note> {
        self.by_path.get(path).map(|&index| &self.notes[index])
    }

    /// Replaces one note's content and rebuilds the derived indexes.
    ///
    /// The link graph is global — editing one note can create or break
    /// backlinks anywhere — so the graph is rebuilt rather than patched. At
    /// personal-vault scale that is microseconds, and it removes a whole
    /// class of stale-edge bugs.
    pub fn update(&mut self, path: &str, source: &str) -> bool {
        if validated_relative_path(path).is_none() {
            return false;
        }
        if self
            .by_path
            .get(path)
            .is_some_and(|&index| self.notes[index].text == source)
        {
            return false;
        }
        let note = Note::parse(path.to_string(), source);
        match self.by_path.get(path) {
            Some(&index) => self.notes[index] = note,
            None => self.notes.push(note),
        }
        self.reindex();
        true
    }

    pub fn remove(&mut self, path: &str) {
        self.notes.retain(|note| note.path != path);
        self.reindex();
    }

    pub(crate) fn reindex(&mut self) {
        self.notes.sort_by(|a, b| a.path.cmp(&b.path));

        self.by_path = self
            .notes
            .iter()
            .enumerate()
            .map(|(index, note)| (note.path.clone(), index))
            .collect();

        self.by_name.clear();
        for (index, note) in self.notes.iter().enumerate() {
            for name in note.names() {
                self.by_name
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(index);
            }
            // The relative path without its extension also resolves, so
            // `[[Projects/Roadmap]]` works alongside `[[Roadmap]]`.
            let without_extension = strip_markdown_extension(&note.path).to_string();
            self.by_name
                .entry(without_extension.to_lowercase())
                .or_default()
                .push(index);
        }
        for targets in self.by_name.values_mut() {
            targets.sort_unstable();
            targets.dedup();
        }

        self.backlinks.clear();
        for (source_index, note) in self.notes.iter().enumerate() {
            for (link_index, link) in note.links.iter().enumerate() {
                if let Some(target) = self.resolve_link_index(&note.path, link) {
                    self.backlinks
                        .entry(target)
                        .or_default()
                        .push((source_index, link_index));
                }
            }
        }

        self.search = SearchIndex::build(&self.notes);
    }

    /// Index of the note a wiki / name target names.
    ///
    /// Ambiguity resolves to the shallowest path, then alphabetically — the
    /// same rule Obsidian uses, so a vault moved between the two behaves the
    /// same way. Returning "the first match found" instead would make link
    /// resolution depend on directory iteration order.
    pub(crate) fn lookup(&self, target: &str) -> Option<usize> {
        let key = target.trim().trim_start_matches("./").to_lowercase();
        if key.is_empty() {
            return None;
        }
        let candidates = self
            .by_name
            .get(&key)
            .or_else(|| self.by_name.get(&format!("{key}.md")))?;

        candidates.iter().copied().min_by_key(|&index| {
            let path = &self.notes[index].path;
            (path.matches('/').count(), path.clone())
        })
    }

    /// Source-relative resolution for a Markdown note destination.
    ///
    /// No global stem fallback: `[x](CONTRIBUTING.md)` from `docs/` does not
    /// open root `CONTRIBUTING.md` when the sibling is missing.
    pub(crate) fn lookup_from(&self, from_path: &str, target: &str) -> Option<usize> {
        let key = normalize_markdown_target(from_path, target)?;
        // `by_name` holds the path-without-extension spelling registered in
        // `reindex`, which is exactly this key for a vault-relative path.
        let candidates = self.by_name.get(&key)?;
        if key.contains('/') {
            // A path-shaped key must name that path, not a same-stem note
            // elsewhere that also answered to a shorter name.
            candidates.iter().copied().find(|&index| {
                strip_markdown_extension(&self.notes[index].path).eq_ignore_ascii_case(&key)
            })
        } else {
            candidates.iter().copied().min_by_key(|&index| {
                let path = &self.notes[index].path;
                (path.matches('/').count(), path.clone())
            })
        }
    }

    /// Resolves one indexed link under the rules for its kind.
    pub(crate) fn resolve_link_index(&self, source_path: &str, link: &WikiLink) -> Option<usize> {
        match link.kind {
            NoteLinkKind::Wiki => self.lookup(&link.target),
            NoteLinkKind::Markdown => self.lookup_from(source_path, &link.target),
        }
    }

    /// Resolves a link the same way backlinks, the graph, and `links()` do.
    pub fn resolve_link(&self, source_path: &str, link: &WikiLink) -> Option<Resolution> {
        let index = self.resolve_link_index(source_path, link)?;
        self.resolution_at(index, link.anchor.as_deref())
    }

    /// Resolves a `[[wikilink]]` target, with its heading anchor if any.
    pub fn resolve(&self, target: &str, anchor: Option<&str>) -> Option<Resolution> {
        let index = self.lookup(target)?;
        self.resolution_at(index, anchor)
    }

    /// Resolves a Markdown destination relative to `from_path`.
    pub fn resolve_from(
        &self,
        from_path: &str,
        target: &str,
        anchor: Option<&str>,
    ) -> Option<Resolution> {
        let index = self.lookup_from(from_path, target)?;
        self.resolution_at(index, anchor)
    }

    fn resolution_at(&self, index: usize, anchor: Option<&str>) -> Option<Resolution> {
        let note = self.notes.get(index)?;
        let offset = anchor.and_then(|anchor| {
            note.headings
                .iter()
                .find(|heading| heading.text.eq_ignore_ascii_case(anchor.trim()))
                .map(|heading| heading.offset)
        });
        Some(Resolution {
            path: note.path.clone(),
            offset,
        })
    }

    /// The links `path` points out at, in the order they appear in the note.
    ///
    /// Duplicates are kept — a note may link the same target three times, and
    /// which occurrence a caller cares about is the caller's business.
    pub fn links(&self, path: &str) -> Vec<OutgoingLink> {
        let Some(&index) = self.by_path.get(path) else {
            return Vec::new();
        };
        let source_path = &self.notes[index].path;
        self.notes[index]
            .links
            .iter()
            .map(|link| OutgoingLink {
                target: link.target.clone(),
                anchor: link.anchor.clone(),
                display: link.display.clone(),
                line: link.line,
                offset: link.offset,
                path: self
                    .resolve_link_index(source_path, link)
                    .map(|target| self.notes[target].path.clone()),
            })
            .collect()
    }

    /// Links pointing at `path`, newest-path-first is not meaningful here so
    /// they come in vault order.
    pub fn backlinks(&self, path: &str) -> Vec<Backlink> {
        let Some(&target) = self.by_path.get(path) else {
            return Vec::new();
        };
        let Some(sources) = self.backlinks.get(&target) else {
            return Vec::new();
        };

        sources
            .iter()
            .map(|&(source_index, link_index)| {
                let source = &self.notes[source_index];
                let link = &source.links[link_index];
                Backlink {
                    path: source.path.clone(),
                    title: source.title.clone(),
                    context: line_text(&source.text, link.line),
                    line: link.line,
                    offset: link.offset,
                }
            })
            .collect()
    }

    /// Notes that mention this note's name in prose without linking to it.
    ///
    /// Only whole-word, case-insensitive matches count. Substring matching
    /// would report "Roadmap" inside "Roadmapping" and fill the panel with
    /// noise, which is how this feature usually gets turned off.
    pub fn unlinked_mentions(&self, path: &str) -> Vec<UnlinkedMention> {
        let Some(&target) = self.by_path.get(path) else {
            return Vec::new();
        };

        let linked: HashSet<usize> = self
            .backlinks
            .get(&target)
            .map(|sources| sources.iter().map(|&(index, _)| index).collect())
            .unwrap_or_default();

        let names = self.notes[target].names();
        let mut mentions = Vec::new();

        for (index, note) in self.notes.iter().enumerate() {
            if index == target || linked.contains(&index) {
                continue;
            }
            for name in &names {
                if let Some(offset) = find_whole_word(&note.text, name) {
                    let line = line_number(&note.text, offset);
                    mentions.push(UnlinkedMention {
                        path: note.path.clone(),
                        title: note.title.clone(),
                        context: line_text(&note.text, line),
                        line,
                        // The editor consumes UTF-16; the search runs in bytes.
                        offset: super::note::utf16_offset(&note.text, offset),
                    });
                    break;
                }
            }
        }

        mentions
    }

    /// Full-text search across the vault.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        self.search.query(query, &self.notes, limit)
    }

    /// Every tag with the number of notes carrying it, most used first.
    pub fn tags(&self) -> Vec<TagCount> {
        let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
        for note in &self.notes {
            for tag in &note.tags {
                *counts.entry(tag.as_str()).or_default() += 1;
            }
        }
        let mut tags: Vec<TagCount> = counts
            .into_iter()
            .map(|(tag, count)| TagCount {
                tag: tag.to_string(),
                count,
            })
            .collect();
        tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        tags
    }

    /// Notes carrying `tag`.
    pub fn notes_with_tag(&self, tag: &str) -> Vec<String> {
        self.notes
            .iter()
            .filter(|note| note.tags.iter().any(|existing| existing == tag))
            .map(|note| note.path.clone())
            .collect()
    }

    /// Link targets that resolve to nothing, so broken links can be surfaced.
    pub fn broken_links(&self) -> Vec<(String, String)> {
        let mut broken = Vec::new();
        for note in &self.notes {
            for link in &note.links {
                if self.resolve_link_index(&note.path, link).is_none() {
                    broken.push((note.path.clone(), link.target.clone()));
                }
            }
        }
        broken
    }
}

/// Walks the in-policy tree without following filesystem aliases.
fn collect(root: &Path, limits: VaultScanLimits) -> (Vec<Note>, VaultScanStatus) {
    let mut notes = Vec::new();
    let mut status = VaultScanStatus {
        scan_performed: true,
        ..VaultScanStatus::default()
    };
    let mut pending = vec![(root.to_path_buf(), 0_usize)];

    'directories: while let Some((directory, depth)) = pending.pop() {
        // A directory may have been replaced with a symlink after its parent
        // was listed. Resolve it immediately before use and fail closed if it
        // no longer belongs to the canonical vault.
        let Ok(canonical_directory) = std::fs::canonicalize(&directory) else {
            status.unreadable_directories = status.unreadable_directories.saturating_add(1);
            continue;
        };
        let Ok(metadata) = std::fs::symlink_metadata(&directory) else {
            status.unreadable_directories = status.unreadable_directories.saturating_add(1);
            continue;
        };
        if metadata.file_type().is_symlink() || !canonical_directory.starts_with(root) {
            status.skipped_symlinks = status.skipped_symlinks.saturating_add(1);
            continue;
        }

        let Ok(entries) = std::fs::read_dir(&directory) else {
            status.unreadable_directories = status.unreadable_directories.saturating_add(1);
            continue;
        };

        for entry in entries {
            if status.visited_entries >= limits.max_entries {
                status.hit_entry_limit = true;
                break 'directories;
            }
            status.visited_entries = status.visited_entries.saturating_add(1);

            let Ok(entry) = entry else {
                status.unreadable_entries = status.unreadable_entries.saturating_add(1);
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(file_type) = entry.file_type() else {
                status.unreadable_entries = status.unreadable_entries.saturating_add(1);
                continue;
            };

            // `DirEntry::file_type` and `symlink_metadata` do not follow the
            // link. Skipping every symlink is intentionally stricter than
            // following only links that currently resolve inside: it remains
            // safe if their target is changed between checks.
            if file_type.is_symlink() {
                status.skipped_symlinks = status.skipped_symlinks.saturating_add(1);
                continue;
            }

            if name.starts_with('.') || IGNORED.contains(&name.as_str()) {
                continue;
            }

            if file_type.is_dir() {
                if depth >= limits.max_depth {
                    status.hit_depth_limit = true;
                } else {
                    pending.push((path, depth + 1));
                }
                continue;
            }

            if !file_type.is_file() || !is_markdown(&path) {
                continue;
            }

            status.discovered_files = status.discovered_files.saturating_add(1);

            // Repeat both checks immediately before reading. This catches an
            // entry swapped for a symlink or moved outside after `read_dir`.
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                status.unreadable_files = status.unreadable_files.saturating_add(1);
                continue;
            };
            let Ok(canonical_path) = std::fs::canonicalize(&path) else {
                status.unreadable_files = status.unreadable_files.saturating_add(1);
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.file_type().is_file()
                || !canonical_path.starts_with(root)
            {
                status.skipped_symlinks = status
                    .skipped_symlinks
                    .saturating_add(usize::from(metadata.file_type().is_symlink()));
                continue;
            }

            let declared_bytes = metadata.len();
            status.discovered_bytes = status.discovered_bytes.saturating_add(declared_bytes);
            let note_limit = u64::try_from(limits.max_note_bytes).unwrap_or(u64::MAX);
            if declared_bytes > note_limit {
                status.oversized_files = status.oversized_files.saturating_add(1);
                continue;
            }
            let total_limit = u64::try_from(limits.max_total_bytes).unwrap_or(u64::MAX);
            let Some(proposed_selected_bytes) = status.selected_bytes.checked_add(declared_bytes)
            else {
                status.hit_total_byte_limit = true;
                continue;
            };
            if proposed_selected_bytes > total_limit {
                status.hit_total_byte_limit = true;
                continue;
            }
            status.selected_files = status.selected_files.saturating_add(1);
            status.selected_bytes = proposed_selected_bytes;

            let remaining_bytes = total_limit.saturating_sub(status.indexed_bytes);
            let text = match read_regular_file(
                &canonical_path,
                limits.max_note_bytes,
                usize::try_from(remaining_bytes).unwrap_or(usize::MAX),
            ) {
                Ok(ReadRegularFile::Text(text)) => text,
                Ok(ReadRegularFile::OversizedNote) => {
                    status.oversized_files = status.oversized_files.saturating_add(1);
                    continue;
                }
                Ok(ReadRegularFile::TotalBudgetExceeded) => {
                    status.hit_total_byte_limit = true;
                    continue;
                }
                Err(_) => {
                    status.unreadable_files = status.unreadable_files.saturating_add(1);
                    continue;
                }
            };
            let Ok(relative) = path.strip_prefix(root) else {
                status.unreadable_files = status.unreadable_files.saturating_add(1);
                continue;
            };
            status.indexed_files = status.indexed_files.saturating_add(1);
            status.indexed_bytes = status
                .indexed_bytes
                .saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
            notes.push(Note::parse(
                relative.to_string_lossy().replace('\\', "/"),
                &text,
            ));
        }
    }

    status.skipped_files = status.discovered_files.saturating_sub(status.indexed_files);

    (notes, status)
}

enum ReadRegularFile {
    Text(String),
    OversizedNote,
    TotalBudgetExceeded,
}

fn read_regular_file(
    path: &Path,
    maximum_note_bytes: usize,
    remaining_total_bytes: usize,
) -> io::Result<ReadRegularFile> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // macOS provides the stronger O_NOFOLLOW_ANY: unlike O_NOFOLLOW, it
    // rejects a symlink swapped into any parent component during the scan.
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW_ANY: i32 = 0x2000_0000;
        const O_NONBLOCK: i32 = 0x0004;
        options.custom_flags(O_NOFOLLOW_ANY | O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vault entry is no longer a regular file",
        ));
    }
    let note_limit = u64::try_from(maximum_note_bytes).unwrap_or(u64::MAX);
    if metadata.len() > note_limit {
        return Ok(ReadRegularFile::OversizedNote);
    }
    let total_limit = u64::try_from(remaining_total_bytes).unwrap_or(u64::MAX);
    if metadata.len() > total_limit {
        return Ok(ReadRegularFile::TotalBudgetExceeded);
    }

    // Metadata is only a snapshot: cap the read too, so a file growing after
    // `metadata()` cannot allocate beyond the contract.
    let maximum_read_bytes = maximum_note_bytes.min(remaining_total_bytes);
    let read_limit = u64::try_from(maximum_read_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let allocation_limit = u64::try_from(maximum_read_bytes).unwrap_or(u64::MAX);
    let capacity = usize::try_from(metadata.len().min(allocation_limit)).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum_note_bytes {
        return Ok(ReadRegularFile::OversizedNote);
    }
    if bytes.len() > remaining_total_bytes {
        return Ok(ReadRegularFile::TotalBudgetExceeded);
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(ReadRegularFile::Text(text))
}

const IGNORED: &[&str] = &["node_modules", "DerivedData", "target", ".build"];

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mdx" | "mkd")
    )
}

/// Folds a Markdown destination into a vault-relative key without extension.
///
/// Leading `/` is vault-root-relative. `..` that would leave the vault is
/// refused. Schemes, protocol-relative URLs, bare anchors, and empty paths
/// are refused.
pub(crate) fn normalize_markdown_target(from_path: &str, target: &str) -> Option<String> {
    let decoded = percent_decode_once(target);
    let trimmed = decoded.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
        return None;
    }
    if has_url_scheme(trimmed) {
        return None;
    }
    let path_part = trimmed
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or(trimmed)
        .trim();
    if path_part.is_empty() {
        return None;
    }

    let (base_dir, relative) = if let Some(rest) = path_part.strip_prefix('/') {
        ("", rest.trim_start_matches('/'))
    } else {
        let relative = path_part.trim_start_matches("./");
        (parent_dir(from_path), relative)
    };
    if relative.is_empty() {
        return None;
    }

    let folded = fold_vault_path(base_dir, relative)?;
    let without_extension = strip_markdown_extension(&folded);
    if without_extension.is_empty() {
        return None;
    }
    Some(without_extension.to_lowercase())
}

fn parent_dir(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

/// Joins `relative` onto `base_dir` while folding `.` / `..`. Climbing above
/// the vault root returns `None`.
fn fold_vault_path(base_dir: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if !base_dir.is_empty() {
        parts.extend(base_dir.split('/').filter(|part| !part.is_empty()));
    }
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

#[cfg(all(test, target_os = "macos"))]
mod read_tests {
    use super::read_regular_file;
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    #[test]
    fn bounded_reader_rejects_a_fifo_without_waiting_for_a_writer() {
        const O_NONBLOCK: i32 = 0x0004;
        let root = std::env::temp_dir().join(format!(
            "markdev-vault-fifo-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fifo = root.join("Swapped.md");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("launch mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
        let fifo = std::fs::canonicalize(fifo).expect("canonical FIFO path");

        // The delayed nonblocking writer exists only to release the old,
        // vulnerable O_RDONLY open. A hardened reader returns before it runs.
        let writer_path = fifo.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(O_NONBLOCK)
                .open(writer_path);
        });

        let started = Instant::now();
        let result = read_regular_file(&fifo, 1_024, 1_024);
        let elapsed = started.elapsed();
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);

        assert!(result.is_err(), "a FIFO was accepted as note text");
        assert!(
            elapsed < Duration::from_millis(100),
            "opening a FIFO blocked for {elapsed:?}"
        );
    }
}

/// The text of a 1-based line, trimmed for display.
fn line_text(text: &str, line: u32) -> String {
    text.lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn line_number(text: &str, byte: usize) -> u32 {
    text[..byte.min(text.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Byte offset of `needle` in `haystack` as a whole word, case-insensitively.
///
/// The search cannot simply run on a lowercased copy: folding can change
/// byte length ('İ' lowers to i plus a combining dot), which shifts every
/// offset after it onto the wrong character — and the offset is the one
/// thing this function exists to produce. It therefore lowercases once into
/// a parallel string that remembers, for each of its own bytes, the byte of
/// `haystack` it came from; matches are found in the copy and mapped back,
/// and word boundaries are checked on the original.
fn find_whole_word(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut lowered = String::with_capacity(haystack.len());
    let mut origins: Vec<usize> = Vec::with_capacity(haystack.len() + 1);
    for (byte, ch) in haystack.char_indices() {
        for folded in ch.to_lowercase() {
            // One entry per *byte* of the folded character: the map is
            // indexed by `lowered`'s byte offsets, and a fold such as the
            // combining dot above occupies two.
            let mut encoded = [0u8; 4];
            for _ in folded.encode_utf8(&mut encoded).as_bytes() {
                origins.push(byte);
            }
            lowered.push(folded);
        }
    }
    origins.push(haystack.len());

    let lower_needle = needle.to_lowercase();
    let mut from = 0usize;
    while let Some(found) = lowered[from..].find(&lower_needle) {
        let start = from + found;
        let end = start + lower_needle.len();
        let orig_start = origins[start];
        let orig_end = origins[end];

        let before_ok = orig_start == 0
            || !haystack[..orig_start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = orig_end >= haystack.len()
            || !haystack[orig_end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');

        if before_ok && after_ok {
            return Some(orig_start);
        }
        from = start + lower_needle.len().max(1);
        if from >= lowered.len() {
            break;
        }
    }
    None
}
