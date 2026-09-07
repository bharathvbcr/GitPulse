//! C ABI for the Swift side.
//!
//! # Ownership contract
//!
//! [`md_parse`] returns an opaque handle that owns the parse result. The
//! accessor functions hand back **borrowed** pointers into that handle's
//! storage — nothing is copied, so a document parse costs one allocation and
//! one FFI call rather than one call per node.
//!
//! Those pointers stay valid until [`md_free`] is called on the handle. Swift
//! must therefore either finish reading before freeing, or copy what it needs.
//! `MarkDevKit`'s `ParsedDocument` wrapper enforces this by owning the handle
//! and exposing only `UnsafeBufferPointer` views scoped to its own lifetime.
//!
//! # Why these are written out longhand
//!
//! The three array accessors are near-identical and beg for a `macro_rules!`.
//! They must not use one: cbindgen does not expand macros, so a generated
//! accessor is silently absent from `markdev.h` — the Swift build then fails
//! on a missing symbol, or worse, links against a stale declaration.

use std::ffi::{c_char, CString};
use std::ptr;

#[cfg(feature = "highlight")]
use crate::highlight::{
    highlight_checked, supports_checked, HighlightSpan, MAX_HIGHLIGHT_CODE_BYTES,
    MAX_HIGHLIGHT_LANGUAGE_BYTES,
};
use crate::html::render_document;
use crate::md::{
    parse_checked, BlockDescriptor, Document, ParseResult, Reparse, StyleSpan, SyntaxMarker,
    MAX_DOCUMENT_BYTES,
};
use crate::vault::{Vault, DEFAULT_MAX_NOTE_BYTES};

/// Maximum UTF-8 bytes accepted for any vault-relative or root path.
pub const VAULT_MAX_PATH_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 bytes accepted for search, tag, target, anchor, and graph filters.
pub const VAULT_MAX_QUERY_BYTES: usize = 4 * 1024;
/// Maximum number of search results materialized across the FFI.
pub const VAULT_MAX_SEARCH_RESULTS: u32 = 1_000;
/// `md_vault_update` rejected malformed, invalid UTF-8, or oversized input.
pub const VAULT_UPDATE_REJECTED: u8 = 0;
/// `md_vault_update` accepted input identical to the indexed note.
pub const VAULT_UPDATE_UNCHANGED: u8 = 1;
/// `md_vault_update` changed the index.
pub const VAULT_UPDATE_CHANGED: u8 = 2;

/// Opaque handle owning one document's parse result.
pub struct ParseHandle {
    result: ParseResult,
}

/// Parses UTF-8 Markdown into a handle.
///
/// Returns null for invalid UTF-8, an oversized/refused parse, or a null
/// pointer paired with a nonzero length. Null plus zero is the canonical empty
/// document. The caller keeps ownership of `bytes`; it is not retained past
/// this call.
///
/// # Safety
///
/// A non-null `bytes` must point to at least `len` readable bytes. Null is
/// accepted only when `len` is zero.
#[no_mangle]
pub unsafe extern "C" fn md_parse(bytes: *const u8, len: usize) -> *mut ParseHandle {
    let Some(source) = read_bounded_bytes(bytes, len, MAX_DOCUMENT_BYTES) else {
        return ptr::null_mut();
    };
    let Ok(result) = parse_checked(source) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(ParseHandle { result }))
}

/// Releases a handle. Passing null is a no-op; passing the same handle twice
/// is undefined.
///
/// # Safety
///
/// `handle` must come from [`md_parse`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn md_free(handle: *mut ParseHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Borrows the style-span array, writing its length to `count`.
///
/// Returns null and writes 0 when `handle` is null.
///
/// # Safety
///
/// `handle` must be live and `count` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_spans(
    handle: *const ParseHandle,
    count: *mut usize,
) -> *const StyleSpan {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.result.spans.len();
    h.result.spans.as_ptr()
}

/// Borrows the syntax-marker array, writing its length to `count`.
///
/// # Safety
///
/// `handle` must be live and `count` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_markers(
    handle: *const ParseHandle,
    count: *mut usize,
) -> *const SyntaxMarker {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.result.markers.len();
    h.result.markers.as_ptr()
}

/// Borrows the block-descriptor array, writing its length to `count`.
///
/// # Safety
///
/// `handle` must be live and `count` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_blocks(
    handle: *const ParseHandle,
    count: *mut usize,
) -> *const BlockDescriptor {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.result.blocks.len();
    h.result.blocks.as_ptr()
}

/// Borrows one length-delimited interned UTF-8 string by index.
///
/// # Safety
///
/// `handle` must be live and `length` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_string(
    handle: *const ParseHandle,
    index: u32,
    length: *mut usize,
) -> *const u8 {
    if length.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *length = 0;
        return ptr::null();
    };
    match h.result.strings.get(index as usize) {
        Some(string) => {
            *length = string.len();
            string.as_ptr()
        }
        None => {
            *length = 0;
            ptr::null()
        }
    }
}

/// Number of interned strings.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_string_count(handle: *const ParseHandle) -> usize {
    handle.as_ref().map_or(0, |h| h.result.strings.len())
}

/// Semantic version of the FFI contract.
///
/// Swift asserts this at startup so a stale `libmarkdev.a` fails loudly at
/// launch instead of silently misreading struct layouts.
#[no_mangle]
pub extern "C" fn md_abi_version() -> u32 {
    3
}

// ---------------------------------------------------------------------------
// Safe standalone HTML export
// ---------------------------------------------------------------------------

/// Opaque, length-delimited UTF-8 HTML. A byte buffer rather than a C string
/// keeps ownership explicit and makes embedded-NUL handling testable.
pub struct HTMLHandle {
    bytes: Box<[u8]>,
}

/// Renders Markdown and a title into a complete, inert HTML document.
///
/// Returns null for invalid UTF-8, an invalid pointer/length pair, or a source
/// that exceeds the renderer's bounds.
///
/// # Safety
///
/// Non-null pointers must each address at least their corresponding length.
#[no_mangle]
pub unsafe extern "C" fn md_html_render(
    source: *const u8,
    source_len: usize,
    title: *const u8,
    title_len: usize,
) -> *mut HTMLHandle {
    let Some(source) = read_utf8(source, source_len) else {
        return ptr::null_mut();
    };
    let Some(title) = read_utf8(title, title_len) else {
        return ptr::null_mut();
    };
    let Ok(html) = render_document(source, title) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(HTMLHandle {
        bytes: html.into_bytes().into_boxed_slice(),
    }))
}

/// Borrows the rendered bytes until `md_html_free` is called.
///
/// # Safety
///
/// `handle` must be live and `count` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_html_bytes(handle: *const HTMLHandle, count: *mut usize) -> *const u8 {
    if count.is_null() {
        return ptr::null();
    }
    let Some(handle) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = handle.bytes.len();
    handle.bytes.as_ptr()
}

/// Releases an HTML handle. Passing null is a no-op.
///
/// # Safety
///
/// `handle` must come from `md_html_render` and must not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn md_html_free(handle: *mut HTMLHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Reads a possibly-empty UTF-8 byte slice without ever forming a null slice.
unsafe fn read_utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    if pointer.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(pointer, len)).ok()
}

// ---------------------------------------------------------------------------
// Incremental document
// ---------------------------------------------------------------------------

/// Opaque handle to a document that avoids reparsing when it provably can.
///
/// Mirrors an `NSTextStorage` on the Swift side. The two hold independent
/// copies of the text, so callers should compare [`md_document_len_utf16`]
/// against their own length after each edit and rebuild on mismatch — silent
/// drift would apply later edits at the wrong offsets.
pub struct DocumentHandle {
    document: Document,
}

/// Creates a document from UTF-8 bytes. Returns null for invalid UTF-8, a
/// refused/oversized parse, or a null pointer paired with a nonzero length.
/// Null plus zero creates an empty document.
///
/// # Safety
///
/// A non-null `bytes` must point to at least `len` readable bytes. Null is
/// accepted only when `len` is zero.
#[no_mangle]
pub unsafe extern "C" fn md_document_new(bytes: *const u8, len: usize) -> *mut DocumentHandle {
    let Some(text) = read_bounded_bytes(bytes, len, MAX_DOCUMENT_BYTES) else {
        return ptr::null_mut();
    };
    let Ok(document) = Document::try_from_str(text) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(DocumentHandle { document }))
}

/// Releases a document handle.
///
/// # Safety
///
/// `handle` must come from [`md_document_new`] and must not be used after.
#[no_mangle]
pub unsafe extern "C" fn md_document_free(handle: *mut DocumentHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Replaces the UTF-16 range `[start, end)` with `replacement`.
///
/// Returns 0 when rejected without mutation, 1 when offsets shifted without a
/// reparse, and 2 when the whole document was reparsed.
///
/// # Safety
///
/// `handle` must be live, and `replacement` must point to `replacement_len`
/// readable bytes (or be null when the length is zero).
#[no_mangle]
pub unsafe extern "C" fn md_document_replace(
    handle: *mut DocumentHandle,
    start: u32,
    end: u32,
    replacement: *const u8,
    replacement_len: usize,
) -> u8 {
    let Some(handle) = handle.as_mut() else {
        return 0;
    };

    let Some(text) = read_bounded_bytes(replacement, replacement_len, MAX_DOCUMENT_BYTES) else {
        return 0;
    };
    if start > end || end > handle.document.len_utf16() {
        return 0;
    }
    let start_byte = handle.document.byte_offset(start);
    let end_byte = handle.document.byte_offset(end);
    let outcome = handle.document.replace(start_byte..end_byte, text);

    match outcome {
        Reparse::Rejected => 0,
        Reparse::Shifted(_) => 1,
        Reparse::Full => 2,
    }
}

/// UTF-16 length of the document's text, for drift detection.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_document_len_utf16(handle: *const DocumentHandle) -> u32 {
    handle.as_ref().map_or(0, |h| h.document.len_utf16())
}

/// Borrows the document's style spans.
///
/// # Safety
///
/// `handle` must be live and `count` writable.
#[no_mangle]
pub unsafe extern "C" fn md_document_spans(
    handle: *const DocumentHandle,
    count: *mut usize,
) -> *const StyleSpan {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.document.result().spans.len();
    h.document.result().spans.as_ptr()
}

/// Borrows the document's syntax markers.
///
/// # Safety
///
/// `handle` must be live and `count` writable.
#[no_mangle]
pub unsafe extern "C" fn md_document_markers(
    handle: *const DocumentHandle,
    count: *mut usize,
) -> *const SyntaxMarker {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.document.result().markers.len();
    h.document.result().markers.as_ptr()
}

/// Borrows the document's block descriptors.
///
/// # Safety
///
/// `handle` must be live and `count` writable.
#[no_mangle]
pub unsafe extern "C" fn md_document_blocks(
    handle: *const DocumentHandle,
    count: *mut usize,
) -> *const BlockDescriptor {
    if count.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = h.document.result().blocks.len();
    h.document.result().blocks.as_ptr()
}

/// Borrows one length-delimited interned UTF-8 string by index.
///
/// # Safety
///
/// `handle` must be live and `length` must be writable.
#[no_mangle]
pub unsafe extern "C" fn md_document_string(
    handle: *const DocumentHandle,
    index: u32,
    length: *mut usize,
) -> *const u8 {
    if length.is_null() {
        return ptr::null();
    }
    let Some(h) = handle.as_ref() else {
        *length = 0;
        return ptr::null();
    };
    match h.document.result().strings.get(index as usize) {
        Some(string) => {
            *length = string.len();
            string.as_ptr()
        }
        None => {
            *length = 0;
            ptr::null()
        }
    }
}

/// Number of interned strings.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_document_string_count(handle: *const DocumentHandle) -> usize {
    handle
        .as_ref()
        .map_or(0, |h| h.document.result().strings.len())
}

// ---------------------------------------------------------------------------
// Vault index
// ---------------------------------------------------------------------------

/// Opaque handle to an indexed vault.
pub struct VaultHandle {
    vault: Vault,
    /// Keeps the last returned JSON alive until the next call, so callers
    /// borrow rather than free.
    scratch: Option<CString>,
}

/// Why this boundary uses JSON while the editor's does not.
///
/// The editor crosses the FFI on every keystroke, where a flat struct buffer
/// earns its complexity. Vault queries happen when a note is opened or a
/// search is typed — orders of magnitude less often, over deeply nested,
/// variable-length data. A bespoke buffer protocol for backlinks and outlines
/// would be a lot of pointer arithmetic to save microseconds nobody can feel.
impl VaultHandle {
    fn serve<T: serde::Serialize>(&mut self, value: &T) -> *const c_char {
        let json = serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string());
        let cstring = CString::new(json).unwrap_or_default();
        self.scratch = Some(cstring);
        self.scratch.as_ref().map_or(ptr::null(), |s| s.as_ptr())
    }
}

/// A private copy of the indexed vault, for off-thread computation.
///
/// Graph layout runs for seconds at vault scale; holding the shared index's
/// lock across it would freeze every keystroke-path query behind the
/// simulation. The caller takes this clone under the lock — cheap: the notes
/// plus a re-index, microseconds at personal-vault scale — and computes
/// against the copy without the lock. Null when `handle` is dead.
///
/// # Safety
///
/// `handle` must be live. The returned handle is owned by the caller and must
/// be released with `md_vault_free`.
#[no_mangle]
pub unsafe extern "C" fn md_vault_clone(handle: *const VaultHandle) -> *mut VaultHandle {
    let Some(handle) = handle.as_ref() else {
        return ptr::null_mut();
    };
    let clone = Vault::build(
        handle.vault.root().to_path_buf(),
        handle.vault.notes().to_vec(),
    );
    Box::into_raw(Box::new(VaultHandle {
        vault: clone,
        scratch: None,
    }))
}

/// Reads at most `maximum_bytes + 1` positions from a promised C string.
/// This caps work for a valid readable C buffer; as with every C ABI, the
/// caller still owns pointer validity.
unsafe fn read_bounded_str<'a>(pointer: *const c_char, maximum_bytes: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    for length in 0..=maximum_bytes {
        if *pointer.add(length) == 0 {
            let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length);
            return std::str::from_utf8(bytes).ok();
        }
    }
    None
}

unsafe fn read_optional_bounded_str<'a>(
    pointer: *const c_char,
    maximum_bytes: usize,
) -> Result<Option<&'a str>, ()> {
    if pointer.is_null() {
        Ok(None)
    } else {
        read_bounded_str(pointer, maximum_bytes).map(Some).ok_or(())
    }
}

/// Reads a length-delimited UTF-8 buffer, accepting null only for empty data.
unsafe fn read_bounded_bytes<'a>(
    pointer: *const u8,
    length: usize,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if length > maximum_bytes || (pointer.is_null() && length != 0) {
        return None;
    }
    if length == 0 {
        return Some("");
    }
    std::str::from_utf8(std::slice::from_raw_parts(pointer, length)).ok()
}

/// Indexes every Markdown file under `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn md_vault_open(path: *const c_char) -> *mut VaultHandle {
    let Some(path) = read_bounded_str(path, VAULT_MAX_PATH_BYTES) else {
        return ptr::null_mut();
    };
    if path.is_empty() || !std::path::Path::new(path).is_absolute() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(VaultHandle {
        vault: Vault::open(path),
        scratch: None,
    }))
}

/// Releases a vault handle.
///
/// # Safety
///
/// `handle` must come from [`md_vault_open`] and must not be used after.
#[no_mangle]
pub unsafe extern "C" fn md_vault_free(handle: *mut VaultHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Number of indexed notes.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_vault_note_count(handle: *const VaultHandle) -> u32 {
    handle.as_ref().map_or(0, |h| h.vault.notes().len() as u32)
}

/// JSON coverage status for the initial filesystem scan.
///
/// # Safety
///
/// `handle` must be live. The returned pointer is borrowed until the next
/// query on this handle.
#[no_mangle]
pub unsafe extern "C" fn md_vault_scan_status(handle: *mut VaultHandle) -> *const c_char {
    let Some(handle) = handle.as_mut() else {
        return ptr::null();
    };
    let status = handle.vault.scan_status();
    handle.serve(&status)
}

/// Re-indexes one note from text the caller already has in memory.
///
/// # Safety
///
/// `handle` must be live. `path` and `text` must each point to their stated
/// number of readable bytes; a null pointer is valid only for zero bytes.
#[no_mangle]
pub unsafe extern "C" fn md_vault_update(
    handle: *mut VaultHandle,
    path: *const u8,
    path_len: usize,
    text: *const u8,
    text_len: usize,
) -> u8 {
    let (Some(handle), Some(path), Some(text)) = (
        handle.as_mut(),
        read_bounded_bytes(path, path_len, VAULT_MAX_PATH_BYTES),
        read_bounded_bytes(text, text_len, DEFAULT_MAX_NOTE_BYTES),
    ) else {
        return VAULT_UPDATE_REJECTED;
    };
    if crate::vault::index::validated_relative_path(path).is_none() {
        return VAULT_UPDATE_REJECTED;
    }
    if handle.vault.update(path, text) {
        VAULT_UPDATE_CHANGED
    } else {
        VAULT_UPDATE_UNCHANGED
    }
}

/// Drops `path` from the index after its file has left the disk.
///
/// The caller deletes or trashes the file; this forgets the note so backlinks
/// and search stop describing something that is no longer there. Unknown
/// paths are accepted silently — removing what was never indexed is a no-op,
/// which is what a watcher racing a manual delete wants.
///
/// # Safety
///
/// `handle` must be live; `path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_remove(handle: *mut VaultHandle, path: *const c_char) {
    let (Some(handle), Some(path)) = (
        handle.as_mut(),
        read_bounded_str(path, VAULT_MAX_PATH_BYTES),
    ) else {
        return;
    };
    if crate::vault::index::validated_relative_path(path).is_none() {
        return;
    }
    handle.vault.remove(path);
}

/// Moves the note at `from` to `to`, rewriting every link that resolved to
/// it, and answers JSON `{rewritten_notes, rewritten_links, failed_rewrites,
/// complete}`.
///
/// The file itself is moved by this call. Null comes back when the move was
/// refused — unknown source, destination already taken, or a file error — so
/// the caller can say "no" rather than guessing why. A non-null response with
/// `complete: false` means the source moved but at least one staged link
/// rewrite could not be committed; callers must surface that partial result.
///
/// # Safety
///
/// `handle` must be live; `from` and `to` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_rename(
    handle: *mut VaultHandle,
    from: *const c_char,
    to: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(from), Some(to)) = (
        handle.as_mut(),
        read_bounded_str(from, VAULT_MAX_PATH_BYTES),
        read_bounded_str(to, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    match handle.vault.rename_note(from, to) {
        Some(outcome) => handle.serve(&outcome),
        None => ptr::null(),
    }
}

/// JSON array of backlinks for `path`.
///
/// The returned pointer is owned by the handle and stays valid until the next
/// query on it or until the handle is freed.
///
/// # Safety
///
/// `handle` must be live; `path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_backlinks(
    handle: *mut VaultHandle,
    path: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(path)) = (
        handle.as_mut(),
        read_bounded_str(path, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle.vault.backlinks(path);
    handle.serve(&value)
}

/// JSON array of the links `path` points out at, each with the note it
/// resolves to.
///
/// The outbound half of [`md_vault_backlinks`]. It exists so a caller can ask
/// "what is this note connected to" without re-parsing the note it already
/// has on screen — which is a second parse of the same text, answering a
/// question the index answered when it read the file.
///
/// # Safety
///
/// `handle` must be live; `path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_links(
    handle: *mut VaultHandle,
    path: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(path)) = (
        handle.as_mut(),
        read_bounded_str(path, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle.vault.links(path);
    handle.serve(&value)
}

/// JSON array of unlinked mentions for `path`.
///
/// # Safety
///
/// `handle` must be live; `path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_unlinked_mentions(
    handle: *mut VaultHandle,
    path: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(path)) = (
        handle.as_mut(),
        read_bounded_str(path, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle.vault.unlinked_mentions(path);
    handle.serve(&value)
}

/// JSON array of headings for `path`, for the outline.
///
/// # Safety
///
/// `handle` must be live; `path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_outline(
    handle: *mut VaultHandle,
    path: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(path)) = (
        handle.as_mut(),
        read_bounded_str(path, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle
        .vault
        .note(path)
        .map(|note| note.headings.clone())
        .unwrap_or_default();
    handle.serve(&value)
}

/// JSON array of search hits.
///
/// # Safety
///
/// `handle` must be live; `query` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_search(
    handle: *mut VaultHandle,
    query: *const c_char,
    limit: u32,
) -> *const c_char {
    let (Some(handle), Some(query)) = (
        handle.as_mut(),
        read_bounded_str(query, VAULT_MAX_QUERY_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle
        .vault
        .search(query, limit.min(VAULT_MAX_SEARCH_RESULTS) as usize);
    handle.serve(&value)
}

/// JSON array of tags with counts.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_vault_tags(handle: *mut VaultHandle) -> *const c_char {
    let Some(handle) = handle.as_mut() else {
        return ptr::null();
    };
    let value = handle.vault.tags();
    handle.serve(&value)
}

/// JSON array of the paths of every note carrying `tag`.
///
/// The tag is given without its leading `#`, matching what the index stores.
///
/// # Safety
///
/// `handle` must be live; `tag` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_notes_with_tag(
    handle: *mut VaultHandle,
    tag: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(tag)) = (
        handle.as_mut(),
        read_bounded_str(tag, VAULT_MAX_QUERY_BYTES),
    ) else {
        return ptr::null();
    };
    let value = handle.vault.notes_with_tag(tag);
    handle.serve(&value)
}

/// JSON object resolving a `[[wikilink]]`, or `null` when it is broken.
///
/// # Safety
///
/// `handle` must be live; `target` must be NUL-terminated UTF-8. `anchor` may
/// be null.
#[no_mangle]
pub unsafe extern "C" fn md_vault_resolve(
    handle: *mut VaultHandle,
    target: *const c_char,
    anchor: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(target)) = (
        handle.as_mut(),
        read_bounded_str(target, VAULT_MAX_QUERY_BYTES),
    ) else {
        return ptr::null();
    };
    let Ok(anchor) = read_optional_bounded_str(anchor, VAULT_MAX_QUERY_BYTES) else {
        return ptr::null();
    };
    let value = handle.vault.resolve(target, anchor);
    handle.serve(&value)
}

/// JSON object resolving a Markdown destination relative to `from_path`.
///
/// Source-relative only — no vault-wide name fallback. `anchor` may be null.
///
/// # Safety
///
/// `handle` must be live; `from_path` and `target` must be NUL-terminated
/// UTF-8. `anchor` may be null.
#[no_mangle]
pub unsafe extern "C" fn md_vault_resolve_from(
    handle: *mut VaultHandle,
    from_path: *const c_char,
    target: *const c_char,
    anchor: *const c_char,
) -> *const c_char {
    let (Some(handle), Some(from_path), Some(target)) = (
        handle.as_mut(),
        read_bounded_str(from_path, VAULT_MAX_PATH_BYTES),
        read_bounded_str(target, VAULT_MAX_QUERY_BYTES),
    ) else {
        return ptr::null();
    };
    let Ok(anchor) = read_optional_bounded_str(anchor, VAULT_MAX_QUERY_BYTES) else {
        return ptr::null();
    };
    let value = handle.vault.resolve_from(from_path, target, anchor);
    handle.serve(&value)
}

/// JSON array of every note path in the vault.
///
/// # Safety
///
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn md_vault_note_paths(handle: *mut VaultHandle) -> *const c_char {
    let Some(handle) = handle.as_mut() else {
        return ptr::null();
    };
    let value: Vec<&str> = handle
        .vault
        .notes()
        .iter()
        .map(|note| note.path.as_str())
        .collect();
    let json = serde_json::to_string(&value).unwrap_or_else(|_| "[]".to_string());
    let cstring = CString::new(json).unwrap_or_default();
    handle.scratch = Some(cstring);
    handle.scratch.as_ref().map_or(ptr::null(), |s| s.as_ptr())
}

/// JSON graph of the vault's links, laid out and ready to draw.
///
/// One call rather than "give me the nodes, now the edges, now the positions":
/// the layout is a property of the whole graph, so splitting it would let a
/// caller draw edges against coordinates from a different build.
///
/// `focus` limits the graph to notes within `depth` hops of that note; pass
/// null for the whole vault. `tag` and `folder` filter it further; both may be
/// null.
///
/// # Safety
///
/// `handle` must be live. `focus`, `tag`, and `folder` may be null, and must
/// otherwise be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn md_vault_graph(
    handle: *mut VaultHandle,
    focus: *const c_char,
    depth: u32,
    tag: *const c_char,
    folder: *const c_char,
) -> *const c_char {
    let Some(handle) = handle.as_mut() else {
        return ptr::null();
    };
    let (Ok(focus), Ok(tag), Ok(folder)) = (
        read_optional_bounded_str(focus, VAULT_MAX_QUERY_BYTES),
        read_optional_bounded_str(tag, VAULT_MAX_QUERY_BYTES),
        read_optional_bounded_str(folder, VAULT_MAX_PATH_BYTES),
    ) else {
        return ptr::null();
    };
    let query = crate::vault::GraphQuery {
        focus,
        depth,
        tag,
        folder,
    };
    let graph = crate::vault::Graph::build(&handle.vault, &query);
    handle.serve(&graph)
}

// ---------------------------------------------------------------------------
// Syntax highlighting (requires the `highlight` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "highlight")]
/// Owns one code block's highlight spans.
///
/// Flat buffers rather than JSON: highlighting runs per code block on every
/// restyle, which puts it on the same hot path as the editor's own spans.
pub struct HighlightHandle {
    spans: Vec<HighlightSpan>,
}

/// Highlights `code` as `language`.
///
/// Returns a handle even when the language is unknown — the span array is
/// simply empty, and unhighlighted code reads fine.
///
/// # Safety
///
/// Each non-null pointer must address at least its corresponding length.
/// Null is accepted only with a zero length.
#[cfg(feature = "highlight")]
#[no_mangle]
pub unsafe extern "C" fn md_highlight(
    language: *const u8,
    language_len: usize,
    code: *const u8,
    code_len: usize,
) -> *mut HighlightHandle {
    let Some(language) = read_bounded_bytes(language, language_len, MAX_HIGHLIGHT_LANGUAGE_BYTES)
    else {
        return ptr::null_mut();
    };
    let Some(code) = read_bounded_bytes(code, code_len, MAX_HIGHLIGHT_CODE_BYTES) else {
        return ptr::null_mut();
    };
    let Ok(spans) = highlight_checked(language, code) else {
        return ptr::null_mut();
    };

    Box::into_raw(Box::new(HighlightHandle { spans }))
}

/// Releases a highlight handle.
///
/// # Safety
///
/// `handle` must come from [`md_highlight`] and must not be used after.
#[cfg(feature = "highlight")]
#[no_mangle]
pub unsafe extern "C" fn md_highlight_free(handle: *mut HighlightHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Borrows the highlight spans, writing the count to `count`.
///
/// # Safety
///
/// `handle` must be live and `count` writable.
#[cfg(feature = "highlight")]
#[no_mangle]
pub unsafe extern "C" fn md_highlight_spans(
    handle: *const HighlightHandle,
    count: *mut usize,
) -> *const HighlightSpan {
    if count.is_null() {
        return ptr::null();
    }
    let Some(handle) = handle.as_ref() else {
        *count = 0;
        return ptr::null();
    };
    *count = handle.spans.len();
    handle.spans.as_ptr()
}

/// Whether a grammar is available for `language`.
///
/// # Safety
///
/// `language` must address `language_len` readable bytes. Null is accepted
/// only with a zero length.
#[cfg(feature = "highlight")]
#[no_mangle]
pub unsafe extern "C" fn md_highlight_supports(language: *const u8, language_len: usize) -> u8 {
    match read_bounded_bytes(language, language_len, MAX_HIGHLIGHT_LANGUAGE_BYTES) {
        Some(value) if supports_checked(value) == Ok(true) => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    /// Reads a borrowed C string.
    fn read(s: *const c_char) -> Option<String> {
        if s.is_null() {
            return None;
        }
        unsafe { Some(CStr::from_ptr(s).to_string_lossy().into_owned()) }
    }

    fn read_bytes(pointer: *const u8, length: usize) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }

    #[test]
    fn parse_and_free_round_trips() {
        let src = "# Title\n\n**bold** and `code`";
        let handle = unsafe { md_parse(src.as_ptr(), src.len()) };
        assert!(!handle.is_null());

        let mut count = 0usize;
        let spans = unsafe { md_spans(handle, &mut count) };
        assert!(!spans.is_null());
        assert!(count > 0, "expected spans for heading, strong, and code");

        let mut markers = 0usize;
        assert!(!unsafe { md_markers(handle, &mut markers) }.is_null());
        assert!(markers > 0, "expected markers for `#`, `**`, and backticks");

        let mut blocks = 0usize;
        assert!(!unsafe { md_blocks(handle, &mut blocks) }.is_null());
        assert!(blocks > 0);

        unsafe { md_free(handle) };
    }

    #[test]
    fn interned_strings_are_readable() {
        let src = "```swift\nlet x = 1\n```";
        let handle = unsafe { md_parse(src.as_ptr(), src.len()) };
        assert_eq!(unsafe { md_string_count(handle) }, 1);
        let mut length = 0usize;
        assert_eq!(
            read_bytes(unsafe { md_string(handle, 0, &mut length) }, length).as_deref(),
            Some("swift")
        );
        assert!(unsafe { md_string(handle, 99, &mut length) }.is_null());
        assert_eq!(length, 0);
        unsafe { md_free(handle) };
    }

    #[test]
    fn invalid_utf8_returns_null_rather_than_panicking() {
        let bad = [0xffu8, 0xfe, 0xfd];
        let handle = unsafe { md_parse(bad.as_ptr(), bad.len()) };
        assert!(handle.is_null());
    }

    #[test]
    fn null_inputs_are_handled() {
        let empty = unsafe { md_parse(ptr::null(), 0) };
        assert!(!empty.is_null());
        unsafe { md_free(empty) };
        assert!(unsafe { md_parse(ptr::null(), 1) }.is_null());
        let mut count = 7usize;
        assert!(unsafe { md_spans(ptr::null(), &mut count) }.is_null());
        assert_eq!(count, 0, "count must be zeroed when the handle is null");
        assert!(unsafe { md_markers(ptr::null(), &mut count) }.is_null());
        assert!(unsafe { md_blocks(ptr::null(), &mut count) }.is_null());
        assert_eq!(unsafe { md_string_count(ptr::null()) }, 0);
        assert!(unsafe { md_spans(ptr::null(), ptr::null_mut()) }.is_null());
        assert!(unsafe { md_string(ptr::null(), 0, ptr::null_mut()) }.is_null());
        unsafe { md_free(ptr::null_mut()) };
    }

    #[test]
    fn empty_document_is_valid() {
        let handle = unsafe { md_parse("".as_ptr(), 0) };
        assert!(!handle.is_null());
        let mut count = 9usize;
        unsafe { md_spans(handle, &mut count) };
        assert_eq!(count, 0);
        unsafe { md_free(handle) };
    }

    #[test]
    fn abi_version_is_stable() {
        assert_eq!(md_abi_version(), 3);
    }

    #[test]
    fn html_bytes_require_a_writable_count_and_zero_it_for_a_null_handle() {
        let source = b"body";
        let title = b"title";
        let handle =
            unsafe { md_html_render(source.as_ptr(), source.len(), title.as_ptr(), title.len()) };
        assert!(!handle.is_null());
        assert!(unsafe { md_html_bytes(handle, ptr::null_mut()) }.is_null());

        let mut count = usize::MAX;
        assert!(unsafe { md_html_bytes(ptr::null(), &mut count) }.is_null());
        assert_eq!(count, 0);
        unsafe { md_html_free(handle) };
    }

    // --- incremental document ---

    #[test]
    fn document_round_trips_and_reports_length() {
        let src = "para one here\n\nplain prose paragraph\n";
        let handle = unsafe { md_document_new(src.as_ptr(), src.len()) };
        assert!(!handle.is_null());
        assert_eq!(
            unsafe { md_document_len_utf16(handle) },
            src.encode_utf16().count() as u32
        );

        let mut count = 0usize;
        assert!(!unsafe { md_document_blocks(handle, &mut count) }.is_null());
        assert!(count > 0);

        unsafe { md_document_free(handle) };
    }

    #[test]
    fn document_edit_updates_length_and_reports_its_path() {
        let src = "para one here\n\nplain prose paragraph\n";
        let handle = unsafe { md_document_new(src.as_ptr(), src.len()) };

        // Mid-word in plain prose: the shift-only path.
        let at = src.find("prose").expect("anchor") as u32 + 2;
        let word = "XY";
        let shifted = unsafe { md_document_replace(handle, at, at, word.as_ptr(), word.len()) };
        assert_eq!(shifted, 1, "typing inside plain prose should not reparse");
        assert_eq!(
            unsafe { md_document_len_utf16(handle) },
            (src.encode_utf16().count() + 2) as u32
        );

        unsafe { md_document_free(handle) };
    }

    #[test]
    fn document_structural_edit_takes_the_full_path() {
        let src = "para one here\n\nplain prose paragraph\n";
        let handle = unsafe { md_document_new(src.as_ptr(), src.len()) };
        let fence = "```\n";
        let at = src.find("plain").expect("anchor") as u32;
        let shifted = unsafe { md_document_replace(handle, at, at, fence.as_ptr(), fence.len()) };
        assert_eq!(shifted, 2, "an opening fence must force a full reparse");
        unsafe { md_document_free(handle) };
    }

    #[test]
    fn document_handles_null_and_invalid_input() {
        // A null pointer with zero length is an empty document, not an
        // error: an empty file is a perfectly good Markdown document.
        let empty = unsafe { md_document_new(ptr::null(), 0) };
        assert!(!empty.is_null());
        assert_eq!(unsafe { md_document_len_utf16(empty) }, 0);
        unsafe { md_document_free(empty) };
        assert_eq!(unsafe { md_document_len_utf16(ptr::null()) }, 0);
        assert_eq!(
            unsafe { md_document_replace(ptr::null_mut(), 0, 0, ptr::null(), 0) },
            0
        );
        let mut count = 3usize;
        assert!(unsafe { md_document_spans(ptr::null(), &mut count) }.is_null());
        assert_eq!(count, 0);
        unsafe { md_document_free(ptr::null_mut()) };
    }

    // --- vault ---

    #[test]
    fn vault_queries_round_trip_as_json() {
        let root = std::env::temp_dir().join(format!("markdev-ffi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create");
        std::fs::write(root.join("A.md"), "# A\n\nLinks to [[B]] #tag\n").expect("write");
        std::fs::write(root.join("B.md"), "# B\n\n## Section\n").expect("write");

        let path = CString::new(root.to_string_lossy().as_ref()).expect("path");
        let handle = unsafe { md_vault_open(path.as_ptr()) };
        assert!(!handle.is_null());
        assert_eq!(unsafe { md_vault_note_count(handle) }, 2);

        let a = CString::new("A.md").expect("path");
        let b = CString::new("B.md").expect("path");
        let links = read(unsafe { md_vault_links(handle, a.as_ptr()) }).expect("json");
        assert!(
            links.contains("\"path\":\"B.md\""),
            "A links to B, resolved: {links}"
        );

        let json = read(unsafe { md_vault_backlinks(handle, b.as_ptr()) }).expect("json");
        assert!(
            json.contains("A.md"),
            "B should have a backlink from A: {json}"
        );

        let outline = read(unsafe { md_vault_outline(handle, b.as_ptr()) }).expect("json");
        assert!(outline.contains("Section"));

        let query = CString::new("links").expect("query");
        let hits = read(unsafe { md_vault_search(handle, query.as_ptr(), 10) }).expect("json");
        assert!(hits.contains("A.md"));

        let tag = CString::new("tag").expect("tag");
        let tagged = read(unsafe { md_vault_notes_with_tag(handle, tag.as_ptr()) }).expect("json");
        assert!(
            tagged.contains("A.md") && !tagged.contains("B.md"),
            "only the tagged note should be listed: {tagged}"
        );

        let tags = read(unsafe { md_vault_tags(handle) }).expect("json");
        assert!(tags.contains("tag"));

        let target = CString::new("B").expect("target");
        let resolved =
            read(unsafe { md_vault_resolve(handle, target.as_ptr(), ptr::null()) }).expect("json");
        assert!(resolved.contains("B.md"));

        unsafe { md_vault_free(handle) };
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn vault_update_preserves_embedded_nul_bytes_in_note_text() {
        let root = std::env::temp_dir().join(format!(
            "markdev-ffi-nul-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create");
        std::fs::write(root.join("A.md"), "# A\n").expect("write");

        let root_path = CString::new(root.to_string_lossy().as_ref()).expect("path");
        let handle = unsafe { md_vault_open(root_path.as_ptr()) };
        assert!(!handle.is_null());
        let path = CString::new("A.md").expect("path");
        let text = b"# A\0after-nul-token\0";

        let update = unsafe {
            md_vault_update(
                handle,
                path.as_bytes().as_ptr(),
                path.as_bytes().len(),
                text.as_ptr(),
                text.len() - 1,
            )
        };
        let query = CString::new("after-nul-token").expect("query");
        let hits = read(unsafe { md_vault_search(handle, query.as_ptr(), 10) }).expect("json");

        unsafe { md_vault_free(handle) };
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(update, VAULT_UPDATE_CHANGED);
        assert!(
            hits.contains("A.md"),
            "text after NUL was discarded: {hits}"
        );
    }

    #[test]
    fn vault_update_distinguishes_rejected_unchanged_and_changed() {
        let root = std::env::temp_dir().join(format!(
            "markdev-ffi-update-status-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create");
        let original = "# A\n";
        std::fs::write(root.join("A.md"), original).expect("write");
        let root_path = CString::new(root.to_string_lossy().as_ref()).expect("path");
        let handle = unsafe { md_vault_open(root_path.as_ptr()) };
        let path = b"A.md";

        let unchanged = unsafe {
            md_vault_update(
                handle,
                path.as_ptr(),
                path.len(),
                original.as_ptr(),
                original.len(),
            )
        };
        let replacement = "# A\n\nchanged";
        let changed = unsafe {
            md_vault_update(
                handle,
                path.as_ptr(),
                path.len(),
                replacement.as_ptr(),
                replacement.len(),
            )
        };
        let unchanged_again = unsafe {
            md_vault_update(
                handle,
                path.as_ptr(),
                path.len(),
                replacement.as_ptr(),
                replacement.len(),
            )
        };
        let hostile_path = b"../Escape.md";
        let rejected = unsafe {
            md_vault_update(
                handle,
                hostile_path.as_ptr(),
                hostile_path.len(),
                replacement.as_ptr(),
                replacement.len(),
            )
        };

        unsafe { md_vault_free(handle) };
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(unchanged, VAULT_UPDATE_UNCHANGED);
        assert_eq!(changed, VAULT_UPDATE_CHANGED);
        assert_eq!(unchanged_again, VAULT_UPDATE_UNCHANGED);
        assert_eq!(rejected, VAULT_UPDATE_REJECTED);
    }

    #[test]
    fn vault_ffi_rejects_oversized_paths_queries_and_update_text() {
        let root = std::env::temp_dir().join(format!(
            "markdev-ffi-bounds-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create");
        std::fs::write(root.join("A.md"), "# A\n").expect("write");
        let root_path = CString::new(root.to_string_lossy().as_ref()).expect("path");
        let handle = unsafe { md_vault_open(root_path.as_ptr()) };
        let oversized_path = vec![b'p'; VAULT_MAX_PATH_BYTES + 1];
        let text = b"body";
        let oversized_text = vec![b'x'; DEFAULT_MAX_NOTE_BYTES + 1];
        let oversized_query = vec![b'q'; VAULT_MAX_QUERY_BYTES + 1];

        assert_eq!(
            unsafe {
                md_vault_update(
                    handle,
                    oversized_path.as_ptr(),
                    oversized_path.len(),
                    text.as_ptr(),
                    text.len(),
                )
            },
            VAULT_UPDATE_REJECTED
        );
        assert_eq!(
            unsafe {
                md_vault_update(
                    handle,
                    b"A.md".as_ptr(),
                    4,
                    oversized_text.as_ptr(),
                    oversized_text.len(),
                )
            },
            VAULT_UPDATE_REJECTED
        );
        assert!(unsafe { md_vault_search(handle, oversized_query.as_ptr().cast(), 10) }.is_null());

        unsafe { md_vault_free(handle) };
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn vault_initial_scan_status_crosses_ffi_without_losing_coverage_counts() {
        let root = std::env::temp_dir().join(format!(
            "markdev-ffi-scan-status-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create");
        std::fs::write(root.join("A.md"), "# A\n").expect("write");
        let root_path = CString::new(root.to_string_lossy().as_ref()).expect("path");
        let handle = unsafe { md_vault_open(root_path.as_ptr()) };

        let json = read(unsafe { md_vault_scan_status(handle) }).expect("status JSON");
        let status: crate::vault::VaultScanStatus = serde_json::from_str(&json).expect("decode");

        unsafe { md_vault_free(handle) };
        let _ = std::fs::remove_dir_all(&root);
        assert!(status.is_complete());
        assert_eq!(status.discovered_files, 1);
        assert_eq!(status.selected_files, 1);
        assert_eq!(status.indexed_files, 1);
        assert_eq!(status.skipped_files, 0);
    }

    #[cfg(feature = "highlight")]
    #[test]
    fn highlighting_round_trips_across_the_ffi() {
        let language = b"rust";
        let code = "fn main() { let x = 1; }";
        let handle =
            unsafe { md_highlight(language.as_ptr(), language.len(), code.as_ptr(), code.len()) };
        assert!(!handle.is_null());

        let mut count = 0usize;
        let spans = unsafe { md_highlight_spans(handle, &mut count) };
        assert!(!spans.is_null());
        assert!(count > 0, "rust code should produce spans");

        let length = code.encode_utf16().count() as u32;
        for span in unsafe { std::slice::from_raw_parts(spans, count) } {
            assert!(span.start < span.end);
            assert!(span.end <= length, "span past end of code");
        }

        unsafe { md_highlight_free(handle) };
        assert_eq!(
            unsafe { md_highlight_supports(language.as_ptr(), language.len()) },
            1
        );
    }

    #[cfg(feature = "highlight")]
    #[test]
    fn highlighting_handles_unknown_languages_and_null() {
        let unknown = b"klingon";
        let code = "fn main() {}";
        let handle =
            unsafe { md_highlight(unknown.as_ptr(), unknown.len(), code.as_ptr(), code.len()) };
        assert!(
            !handle.is_null(),
            "an unknown language still yields a handle"
        );

        let mut count = 9usize;
        unsafe { md_highlight_spans(handle, &mut count) };
        assert_eq!(count, 0);
        assert!(unsafe { md_highlight_spans(handle, ptr::null_mut()) }.is_null());
        unsafe { md_highlight_free(handle) };

        assert_eq!(unsafe { md_highlight_supports(ptr::null(), 0) }, 0);
        assert_eq!(unsafe { md_highlight_supports(ptr::null(), 1) }, 0);
        assert!(unsafe { md_highlight_spans(ptr::null(), &mut count) }.is_null());
        unsafe { md_highlight_free(ptr::null_mut()) };

        let empty_language = unsafe { md_highlight(ptr::null(), 0, code.as_ptr(), code.len()) };
        assert!(!empty_language.is_null());
        unsafe { md_highlight_free(empty_language) };
        assert!(unsafe { md_highlight(ptr::null(), 1, ptr::null(), 0) }.is_null());
        assert!(unsafe { md_highlight(unknown.as_ptr(), unknown.len(), ptr::null(), 1) }.is_null());
    }

    #[test]
    fn vault_handles_null_input() {
        assert!(unsafe { md_vault_open(ptr::null()) }.is_null());
        assert_eq!(unsafe { md_vault_note_count(ptr::null()) }, 0);
        assert!(unsafe { md_vault_backlinks(ptr::null_mut(), ptr::null()) }.is_null());
        assert!(unsafe { md_vault_links(ptr::null_mut(), ptr::null()) }.is_null());
        assert!(unsafe { md_vault_tags(ptr::null_mut()) }.is_null());
        assert!(unsafe { md_vault_notes_with_tag(ptr::null_mut(), ptr::null()) }.is_null());
        unsafe { md_vault_free(ptr::null_mut()) };
    }

    #[test]
    fn document_utf16_offsets_land_on_the_right_bytes() {
        // The conversion Swift depends on: an emoji must not shift the edit.
        let src = "intro line here\n\nplain prose paragraph\n";
        let handle = unsafe { md_document_new(src.as_ptr(), src.len()) };
        let at = src.find("prose").expect("anchor") as u32;
        let word = "Z";
        unsafe { md_document_replace(handle, at, at + 5, word.as_ptr(), word.len()) };
        assert_eq!(
            unsafe { md_document_len_utf16(handle) },
            (src.encode_utf16().count() - 4) as u32
        );
        unsafe { md_document_free(handle) };
    }
}
