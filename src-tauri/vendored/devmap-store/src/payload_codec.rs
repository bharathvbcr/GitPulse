//! The stored form of a payload's extraction document (schema 29).
//!
//! `file_payloads.extraction_json` held the extraction's JSON as `TEXT`, ~100 KB
//! a row and 190 MB of a 445 MB store. From schema 29 it holds one zstd frame
//! as a `BLOB` instead. The column keeps its name, and so does the
//! `generation_files` view over it, so what a reader asks for has not changed;
//! only the storage class of the answer has.
//!
//! # One rule for telling the two forms apart
//!
//! SQLite's storage class: `BLOB` is a zstd frame, `TEXT` is JSON as v28 wrote
//! it. No marker byte or length prefix is added, because the class already
//! cannot be confused (a `TEXT` value is never a `BLOB`, whatever its bytes)
//! and a frame carries its own magic. A store opened by a writer holds no `TEXT`
//! row (the open compresses the stragglers), but a reader accepts either, since
//! a hand-built or foreign row that was never compressed is a valid document and
//! not a fault.
//!
//! # What identity means
//!
//! [`crate::db::payload_digest`] covers the JSON *text*, and the writer takes it
//! before compressing. Decompressing returns the very bytes that were
//! compressed, so a payload's identity, and the digest a v28 store already
//! stamped on it, do not change; compression is a storage detail beneath
//! identity and not part of it.
//!
//! # Refusals
//!
//! A frame that will not decompress, that declares no size, that declares more
//! than [`MAX_DOCUMENT_BYTES`], or that decodes to something other than UTF-8 is
//! a refusal naming what was being read, never an empty or partial document: the
//! caller's `what` is the path or the content identity the payload belongs to.
//! The size is read from the frame header and checked *before* anything is
//! allocated, and the decoder is then given exactly that many bytes of room, so
//! a hostile store cannot make a reader allocate more than the cap by lying in
//! the header (the decoder refuses a frame whose real size differs from the
//! one it declared) or by chaining frames (the second frame has no room).

use std::borrow::Cow;
use std::cell::RefCell;

use devmap_extract::model::{WiringAnnotation, WiringKind};
use rusqlite::types::ValueRef;

use crate::db::refusal;

/// zstd level for a stored document.
///
/// Chosen by measurement over this repository's 1,896 stored extractions
/// (200.6 MB, per-document frames with a checksum, min of 5 in one process,
/// Appendix C of `docs/devmap/design/store-write-cost-proposal.md`):
///
/// ```text
///   level   ratio   compress (whole corpus)   decompress
///     1     11.17x        130 ms                 58 ms
///     3     11.03x        144 ms                 56 ms
///     5     11.76x        437 ms                 56 ms
///     9     13.29x        886 ms                 50 ms
/// ```
///
/// Level 1 is as small as level 3 on this data and the cheapest to write. The
/// next ratio gain, level 9's two points, costs six times the write for a store
/// whose biggest column is already 11x smaller.
pub(crate) const ZSTD_LEVEL: i32 = 1;

/// The most a stored document may decompress to, and so the most a writer will
/// store and a reader will allocate for one.
///
/// A source file over 1 MiB is refused by discovery, and the largest
/// extraction measured on this repository's own store is 2.1 MB, thirty times
/// under this. The cap exists for the hostile store: the allocation a reader makes is
/// bounded by it however the header was forged.
pub(crate) const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

thread_local! {
    // A zstd context is a few hundred KB to set up and the writer compresses
    // one document per changed file on a pool of threads, so each thread keeps
    // its own rather than building one per call.
    static COMPRESSOR: RefCell<Option<zstd::bulk::Compressor<'static>>> =
        const { RefCell::new(None) };
    static DECOMPRESSOR: RefCell<Option<zstd::bulk::Decompressor<'static>>> =
        const { RefCell::new(None) };
}

/// Compress one extraction document for storage.
pub(crate) fn compress_document(json: &str) -> rusqlite::Result<Vec<u8>> {
    if json.len() > MAX_DOCUMENT_BYTES {
        return Err(refusal(format!(
            "an extraction document of {} bytes is over the {MAX_DOCUMENT_BYTES}-byte limit the \
             store will keep; refusing to write what a reader would refuse",
            json.len()
        )));
    }
    COMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let mut compressor = zstd::bulk::Compressor::new(ZSTD_LEVEL)
                .map_err(|error| refusal(format!("zstd could not start: {error}")))?;
            // Four bytes a document buy detection of a flipped bit: without
            // them a damaged frame can decode to different, well-formed bytes
            // and reach the JSON parser as though it were what was written.
            compressor
                .set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                .map_err(|error| refusal(format!("zstd could not start: {error}")))?;
            *slot = Some(compressor);
        }
        slot.as_mut()
            .expect("the compressor was just created")
            .compress(json.as_bytes())
            .map_err(|error| refusal(format!("zstd could not compress a document: {error}")))
    })
}

/// Decompress a stored frame. `what` names the payload for the refusal.
pub(crate) fn inflate_document(frame: &[u8], what: &str) -> rusqlite::Result<String> {
    let declared = match zstd::zstd_safe::get_frame_content_size(frame) {
        Ok(Some(declared)) => declared,
        Ok(None) => {
            return Err(refusal(format!(
                "stored extraction for {what} is a zstd frame that does not declare its size; \
                 this store never writes one, so it is not this store's"
            )))
        }
        Err(_) => {
            return Err(refusal(format!(
                "stored extraction for {what} is a BLOB but not a zstd frame ({} bytes)",
                frame.len()
            )))
        }
    };
    let declared = usize::try_from(declared)
        .ok()
        .filter(|declared| *declared <= MAX_DOCUMENT_BYTES)
        .ok_or_else(|| {
            refusal(format!(
                "stored extraction for {what} declares {declared} bytes decompressed, over the \
                 {MAX_DOCUMENT_BYTES}-byte limit; refusing to allocate for it"
            ))
        })?;
    let bytes = DECOMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                zstd::bulk::Decompressor::new()
                    .map_err(|error| refusal(format!("zstd could not start: {error}")))?,
            );
        }
        slot.as_mut()
            .expect("the decompressor was just created")
            .decompress(frame, declared)
            .map_err(|error| {
                refusal(format!(
                    "stored extraction for {what} did not decompress (a damaged or truncated \
                     frame of {} bytes): {error}",
                    frame.len()
                ))
            })
    })?;
    if bytes.len() != declared {
        return Err(refusal(format!(
            "stored extraction for {what} decompressed to {} bytes, not the {declared} its frame \
             declares",
            bytes.len()
        )));
    }
    String::from_utf8(bytes).map_err(|error| {
        refusal(format!(
            "stored extraction for {what} is not UTF-8: {error}"
        ))
    })
}

/// The JSON of a stored extraction, as the column holds it.
///
/// Borrowed when the row is `TEXT`, decompressed when it is a `BLOB`.
pub(crate) fn decode_value<'a>(value: ValueRef<'a>, what: &str) -> rusqlite::Result<Cow<'a, str>> {
    match value {
        ValueRef::Text(text) => std::str::from_utf8(text)
            .map(Cow::Borrowed)
            .map_err(|error| {
                refusal(format!(
                    "stored extraction for {what} is not UTF-8: {error}"
                ))
            }),
        ValueRef::Blob(frame) => inflate_document(frame, what).map(Cow::Owned),
        other => Err(refusal(format!(
            "stored extraction for {what} is {:?}, neither TEXT nor a compressed BLOB",
            other.data_type()
        ))),
    }
}

/// A stored extraction taken off the connection but not yet decoded.
///
/// The cached-read path of an incremental build decodes one document per
/// unchanged file on rayon, and the connection mutex is held for the SELECT
/// alone. Taking the bytes under the guard and decoding after it is released
/// keeps decompression off the mutex exactly as parsing already is.
#[cfg(feature = "parse")]
pub(crate) enum StoredDocument {
    Text(String),
    Frame(Vec<u8>),
}

#[cfg(feature = "parse")]
impl StoredDocument {
    pub(crate) fn from_value(value: ValueRef<'_>, what: &str) -> rusqlite::Result<Self> {
        match value {
            ValueRef::Text(text) => {
                String::from_utf8(text.to_vec())
                    .map(Self::Text)
                    .map_err(|error| {
                        refusal(format!(
                            "stored extraction for {what} is not UTF-8: {error}"
                        ))
                    })
            }
            ValueRef::Blob(frame) => Ok(Self::Frame(frame.to_vec())),
            other => Err(refusal(format!(
                "stored extraction for {what} is {:?}, neither TEXT nor a compressed BLOB",
                other.data_type()
            ))),
        }
    }

    pub(crate) fn into_json(self, what: &str) -> rusqlite::Result<String> {
        match self {
            Self::Text(text) => Ok(text),
            Self::Frame(frame) => inflate_document(&frame, what),
        }
    }
}

/// The `RuntimeEntryPoint` annotations of an extraction, as the JSON array
/// `file_payloads.entry_wiring_json` stores.
///
/// What `affected_tests` needs of a payload, promoted out of the compressed
/// document so that the query on its hot path reads a few bytes and
/// decompresses nothing. The filter by *test-harness reason* stays in the
/// reader, where it was: it is a judgement the extractor can change, and a
/// stored verdict would go stale where a stored annotation does not.
///
/// The writer calls this with the extraction in hand and the migration with the
/// one it just decoded, so both agree by construction.
pub(crate) fn entry_wiring_json(wiring: &[WiringAnnotation]) -> rusqlite::Result<String> {
    let entries: Vec<&WiringAnnotation> = wiring
        .iter()
        .filter(|annotation| annotation.kind == WiringKind::RuntimeEntryPoint)
        .collect();
    serde_json::to_string(&entries)
        .map_err(|error| refusal(format!("entry wiring could not be serialized: {error}")))
}

/// [`entry_wiring_json`] of a stored extraction's JSON text. `what` names the
/// payload for the refusal.
pub(crate) fn entry_wiring_of_document(json: &str, what: &str) -> rusqlite::Result<String> {
    #[derive(serde::Deserialize)]
    struct Wiring {
        #[serde(default)]
        wiring: Vec<WiringAnnotation>,
    }
    let parsed: Wiring = serde_json::from_str(json)
        .map_err(|error| refusal(format!("stored extraction for {what} is invalid: {error}")))?;
    entry_wiring_json(&parsed.wiring)
}
