//! A layout for the verbose `code_graph.json` that keeps unchanged bytes where
//! they were.
//!
//! The file is compact JSON, so one inserted node moves every byte after it.
//! Written over a copy-on-write clone of the previous file (see
//! `artifacts::write_atomic`), that shift makes every later 4 KiB block differ,
//! and a linked worktree's export stops sharing disk blocks with the main
//! checkout's it was cloned from: 84% of the blocks of a 71 MB graph differed
//! after editing twenty files that added twenty nodes.
//!
//! The fix is whitespace, which JSON ignores. The document is cut into
//! *segments* — every top-level field, and runs of roughly
//! [`ELEMENTS_PER_CHUNK`] elements of each top-level array — and each segment is
//! followed by a *gutter* of spaces. A segment that grows spends its own gutter;
//! one that outgrows it borrows from the gutters that follow until the layout is
//! back on the old offsets, so the shift stays local. Everything after that
//! point is byte-for-byte where it was.
//!
//! Only the bytes between segments change: the value the file parses to is the
//! compact serialisation's, exactly. A gutter sits after a top-level comma (or
//! at the end of the file), never inside a string, number or nested value.
//!
//! The previous layout is read back from the previous file itself: the same
//! scanner finds its segments, so no sidecar can disagree with the file.

use std::collections::HashMap;

/// A new segment starts after an array element whose hash is `0 mod` this, so
/// boundaries are decided by content and survive an insertion elsewhere.
pub const ELEMENTS_PER_CHUNK: u64 = 24;

/// Containers deeper than this are never cut.
const MAX_CUT_DEPTH: usize = 3;

/// Fresh gutter: this fraction (1/16) of a segment, at least [`MIN_GUTTER`],
/// which is room for one more typical node or edge record.
const GUTTER_DIVISOR: usize = 16;
const MIN_GUTTER: usize = 384;

/// Below this size the blocks are too few to be worth padding.
pub const MIN_LAID_OUT_BYTES: usize = 1 << 20;

/// A previous layout whose gutters are under 1.5% of the file is spent (or was
/// never padded), and one over 25% is mostly holes left by deleted segments.
/// Either is laid out afresh, which rewrites the file once.
const MIN_GUTTER_PERMILLE: usize = 15;
const MAX_GUTTER_PERMILLE: usize = 250;

#[derive(Debug, Clone, Copy)]
struct Segment {
    /// First byte of the segment.
    start: usize,
    /// One past the last byte of its content (the comma that ends it).
    content_end: usize,
    /// One past the gutter: where the next segment starts.
    end: usize,
    /// Identity of the content, for matching a segment to its old self.
    hash: u64,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ (hash >> 29)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && is_space(bytes[at]) {
        at += 1;
    }
    at
}

fn trim(mut slice: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = slice {
        if !is_space(*first) {
            break;
        }
        slice = rest;
    }
    while let [rest @ .., last] = slice {
        if !is_space(*last) {
            break;
        }
        slice = rest;
    }
    slice
}

/// Cut a JSON object into segments. `None` when `bytes` is not one object
/// followed only by whitespace, which makes it no previous layout at all.
fn scan(bytes: &[u8]) -> Option<Vec<Segment>> {
    let first = skip_space(bytes, 0);
    if bytes.get(first) != Some(&b'{') {
        return None;
    }
    let mut segments = Vec::new();
    let mut stack: Vec<u8> = Vec::with_capacity(8);
    let mut in_string = false;
    let mut escaped = false;
    let mut segment_start = 0usize;
    // Where the element or member being read begins, per container depth.
    let mut element_start = [0usize; MAX_CUT_DEPTH + 1];
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                stack.push(byte);
                if stack.len() <= MAX_CUT_DEPTH {
                    element_start[stack.len()] = i + 1;
                }
            }
            b'}' | b']' => {
                stack.pop()?;
                if stack.is_empty() {
                    let end = skip_space(bytes, i + 1);
                    if end != bytes.len() {
                        return None;
                    }
                    segments.push(Segment {
                        start: segment_start,
                        content_end: i + 1,
                        end,
                        hash: fnv1a(&bytes[segment_start..=i]),
                    });
                    return Some(segments);
                }
            }
            b',' => {
                let depth = stack.len();
                // Fields of the document always end a segment. Below them, an
                // element of a top-level array or a member of a top-level object
                // does when its hash says so, and so does one level further down
                // inside an object (`meta` holds lists of thousands of names);
                // inside an array of records it would cut every record's fields.
                let candidate = depth == 2 || (depth == 3 && stack[1] == b'{');
                let boundary = depth == 1
                    || (candidate
                        && fnv1a(trim(&bytes[element_start[depth]..i]))
                            .is_multiple_of(ELEMENTS_PER_CHUNK));
                if depth <= MAX_CUT_DEPTH {
                    element_start[depth] = i + 1;
                }
                if boundary {
                    let next = skip_space(bytes, i + 1);
                    segments.push(Segment {
                        start: segment_start,
                        content_end: i + 1,
                        end: next,
                        hash: fnv1a(&bytes[segment_start..=i]),
                    });
                    segment_start = next;
                    element_start[depth] = next;
                    i = next;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn gutter_for(content_len: usize) -> usize {
    MIN_GUTTER.max(content_len / GUTTER_DIVISOR)
}

/// Extend `out` with spaces up to `len` bytes (never shortens it).
fn pad_to(out: &mut Vec<u8>, len: usize) {
    if out.len() < len {
        out.resize(len, b' ');
    }
}

/// Every segment with a fresh gutter.
fn fresh(bytes: &[u8], new: &[Segment]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / GUTTER_DIVISOR + 1024);
    for segment in new {
        let content = &bytes[segment.start..segment.content_end];
        out.extend_from_slice(content);
        let at = out.len() + gutter_for(content.len());
        pad_to(&mut out, at);
    }
    out
}

/// Whether a scanned previous file still carries a usable amount of slack.
fn is_usable(old: &[Segment]) -> bool {
    let Some(last) = old.last() else {
        return false;
    };
    let gutters: usize = old.iter().map(|s| s.end - s.content_end).sum();
    let total = last.end.max(1);
    let permille = gutters * 1000 / total;
    (MIN_GUTTER_PERMILLE..=MAX_GUTTER_PERMILLE).contains(&permille)
}

/// Lay `new` out over the offsets of `old`.
///
/// Segments whose content is unchanged are *anchors*: matched to their old
/// selves in order, they go back onto their old offsets. What lies between two
/// anchors is a *run* of changed segments, laid out inside the span the old run
/// occupied. Treating the run as one region, not segment by segment, is what
/// makes a split or a merge (an edited element that became, or stopped being, a
/// boundary) cost nothing beyond the region itself.
fn relayout(bytes: &[u8], new: &[Segment], old: &[Segment]) -> Vec<u8> {
    let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
    for (j, segment) in old.iter().enumerate() {
        by_hash.entry(segment.hash).or_default().push(j);
    }
    let mut anchors: Vec<(usize, usize)> = Vec::new();
    let mut floor = 0usize;
    for (i, segment) in new.iter().enumerate() {
        let Some(candidates) = by_hash.get(&segment.hash) else {
            continue;
        };
        let at = candidates.partition_point(|&j| j < floor);
        if let Some(&j) = candidates.get(at) {
            anchors.push((i, j));
            floor = j + 1;
        }
    }

    let content = |i: usize| &bytes[new[i].start..new[i].content_end];
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / GUTTER_DIVISOR + 1024);
    let (mut next_new, mut next_old) = (0usize, 0usize);
    for (anchor_new, anchor_old) in anchors
        .iter()
        .copied()
        .chain(std::iter::once((new.len(), old.len())))
    {
        let new_run = next_new..anchor_new;
        let old_run = next_old..anchor_old;
        if !new_run.is_empty() {
            let required: usize = new_run.clone().map(|i| content(i).len()).sum();
            // An old run that is empty has no span to reuse: the segments are
            // new, and the anchors after them give back the room they take.
            let span =
                (!old_run.is_empty()).then(|| (old[old_run.start].start, old[old_run.end - 1].end));
            if let Some((region_start, _)) = span {
                pad_to(&mut out, region_start);
            }
            let room = span.map_or(0, |(_, region_end)| region_end.saturating_sub(out.len()));
            let count = new_run.len();
            for (k, i) in new_run.enumerate() {
                out.extend_from_slice(content(i));
                let gutter = if room >= required {
                    // Fits: share what is left of the old span out evenly.
                    let spare = room - required;
                    spare / count + if k + 1 == count { spare % count } else { 0 }
                } else {
                    gutter_for(content(i).len())
                };
                let at = out.len() + gutter;
                pad_to(&mut out, at);
            }
        }
        if anchor_new < new.len() {
            // Unchanged: its old offset, or as close as the shift in front of
            // it allows, with its gutter shrunk only as far as that shift
            // demands.
            pad_to(&mut out, old[anchor_old].start);
            out.extend_from_slice(content(anchor_new));
            pad_to(&mut out, old[anchor_old].end);
        }
        next_new = anchor_new + 1;
        next_old = anchor_old + 1;
    }
    out
}

/// `compact` (one JSON object, compact) laid out so that segments the previous
/// file already held keep their offsets. The result parses to the same value
/// as `compact`.
///
/// `previous` is the file being replaced, if any. `max_bytes` is the ceiling
/// readers enforce: when padding would carry the file over it but `compact`
/// fits, `compact` is returned as it is. A document too small to be worth
/// padding, or one this scanner cannot cut, is also returned as it is.
pub fn lay_out(compact: &str, previous: Option<&[u8]>, max_bytes: u64) -> Vec<u8> {
    let bytes = compact.as_bytes();
    if bytes.len() < MIN_LAID_OUT_BYTES {
        return bytes.to_vec();
    }
    let Some(new) = scan(bytes) else {
        return bytes.to_vec();
    };
    let out = match previous.and_then(scan).filter(|old| is_usable(old)) {
        Some(old) => relayout(bytes, &new, &old),
        None => fresh(bytes, &new),
    };
    if out.len() as u64 > max_bytes && bytes.len() as u64 <= max_bytes {
        return bytes.to_vec();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const BLOCK: usize = 4096;

    /// A graph-shaped document: two big arrays, a field between them whose size
    /// moves, scalars, and strings built to break a careless scanner.
    fn document(nodes: usize, edges: usize, dead: usize) -> Value {
        let node = |i: usize| {
            json!({
                "id": format!("src/file_{}.rs::item_{i}", i / 9),
                "kind": "function",
                "reason": format!("quote \" comma , bracket ] brace }} backslash \\ \\\" é 日本 {i}"),
                "line": i,
            })
        };
        let edge = |i: usize| {
            json!({
                "source": format!("src/file_{}.rs::item_{i}", i / 9),
                "target": format!("src/file_{}.rs::item_{}", (i + 7) / 9, i + 7),
                "kind": "calls",
                "extras": {"note": "a, b", "list": [1, 2, [3, 4]]},
            })
        };
        json!({
            "content_fingerprint": "c2:0123456789012345678901234567890123456789",
            "dead_code": (0..dead).map(|i| json!({"id": format!("d{i}")})).collect::<Vec<_>>(),
            "edges": (0..edges).map(edge).collect::<Vec<_>>(),
            "meta": {"counts": [1, 2, 3], "text": "x, y]"},
            "nodes": (0..nodes).map(node).collect::<Vec<_>>(),
            "schema_version": 2,
        })
    }

    fn compact(value: &Value) -> String {
        serde_json::to_string(value).unwrap()
    }

    fn parsed(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("laid-out file must be valid JSON")
    }

    fn dirty_blocks(old: &[u8], new: &[u8]) -> usize {
        let blocks = new.len().div_ceil(BLOCK);
        (0..blocks)
            .filter(|b| {
                let range =
                    |x: &[u8]| x[(b * BLOCK).min(x.len())..((b + 1) * BLOCK).min(x.len())].to_vec();
                range(old) != range(new)
            })
            .count()
    }

    const CAP: u64 = 128 * 1024 * 1024;

    #[test]
    fn a_laid_out_file_parses_to_the_compact_value() {
        let value = document(6000, 9000, 300);
        let text = compact(&value);
        assert!(text.len() > MIN_LAID_OUT_BYTES);
        let laid = lay_out(&text, None, CAP);
        assert!(laid.len() > text.len(), "a fresh layout carries gutters");
        assert_eq!(parsed(&laid), value);
        let again = lay_out(&compact(&value), Some(&laid), CAP);
        assert_eq!(parsed(&again), value);
    }

    #[test]
    fn strings_holding_commas_quotes_and_brackets_are_never_split() {
        let value = document(4000, 4000, 10);
        let text = compact(&value);
        let segments = scan(text.as_bytes()).expect("a compact object scans");
        for segment in &segments {
            // Every segment is a run of whole tokens: cut the text there and the
            // pieces must concatenate to the original, with no byte inside a
            // string having been separated from its neighbours.
            let content = &text.as_bytes()[segment.start..segment.content_end];
            assert!(content.ends_with(b",") || content.ends_with(b"}"));
        }
        let rejoined: Vec<u8> = segments
            .iter()
            .flat_map(|s| text.as_bytes()[s.start..s.content_end].iter().copied())
            .collect();
        assert_eq!(rejoined, text.as_bytes());
        assert_eq!(parsed(&lay_out(&text, None, CAP)), value);
    }

    #[test]
    fn rebuilding_an_unchanged_graph_reproduces_the_file_byte_for_byte() {
        let text = compact(&document(6000, 9000, 300));
        let first = lay_out(&text, None, CAP);
        let second = lay_out(&text, Some(&first), CAP);
        assert_eq!(first, second);
    }

    #[test]
    fn editing_one_node_dirties_a_bounded_number_of_blocks() {
        let mut value = document(6000, 9000, 300);
        let first = lay_out(&compact(&value), None, CAP);
        value["nodes"][3000]["reason"] =
            json!("edited, and longer than it was before by a good margin");
        let second = lay_out(&compact(&value), Some(&first), CAP);
        assert_eq!(parsed(&second), value);
        assert_eq!(first.len(), second.len(), "the edit fits in its gutter");
        let dirty = dirty_blocks(&first, &second);
        assert!(dirty <= 8, "one edited node dirtied {dirty} blocks");
        let compact_dirty = dirty_blocks(
            compact(&document(6000, 9000, 300)).as_bytes(),
            compact(&value).as_bytes(),
        );
        assert!(
            compact_dirty > 50 * dirty,
            "compact JSON shifts: {compact_dirty}"
        );
    }

    #[test]
    fn twenty_insertions_across_the_arrays_stay_local() {
        let mut value = document(6000, 9000, 300);
        let first = lay_out(&compact(&value), None, CAP);
        for k in 0..20 {
            let nodes = value["nodes"].as_array_mut().unwrap();
            let at = 100 + k * 290;
            nodes.insert(
                at,
                json!({"id": format!("new::{k}"), "kind": "function", "line": k}),
            );
            let edges = value["edges"].as_array_mut().unwrap();
            let at = 200 + k * 430;
            edges.insert(
                at,
                json!({"source": format!("new::{k}"), "target": "x", "kind": "contains"}),
            );
        }
        let second = lay_out(&compact(&value), Some(&first), CAP);
        assert_eq!(parsed(&second), value);
        let dirty = dirty_blocks(&first, &second);
        let total = second.len().div_ceil(BLOCK);
        // Forty insertions, each rewriting the rest of its own segment and no
        // more: a few blocks apiece, wherever in the file they land.
        assert!(dirty <= 40 * 3, "{dirty} of {total} blocks dirty");
        let plain = dirty_blocks(
            compact(&document(6000, 9000, 300)).as_bytes(),
            compact(&value).as_bytes(),
        );
        assert!(
            plain > 3 * dirty,
            "plain {plain} vs laid out {dirty} of {total}"
        );
    }

    #[test]
    fn a_segment_outgrowing_its_gutter_borrows_from_the_ones_after_it() {
        let mut value = document(6000, 9000, 300);
        let first = lay_out(&compact(&value), None, CAP);
        // Far more than one gutter (128 B .. 1/16 of a segment).
        let big = "z".repeat(3000);
        value["nodes"][2000]["reason"] = json!(big);
        let second = lay_out(&compact(&value), Some(&first), CAP);
        assert_eq!(parsed(&second), value);
        let dirty = dirty_blocks(&first, &second);
        let total = second.len().div_ceil(BLOCK);
        assert!(
            dirty * 10 < total,
            "borrowing spread the shift: {dirty} of {total}"
        );
        // The layout came back onto the old offsets: the last quarter of the
        // file is byte-for-byte what it was, at the same positions.
        assert_eq!(first.len(), second.len());
        let from = first.len() - first.len() / 4;
        assert_eq!(first[from..], second[from..]);
    }

    #[test]
    fn an_edit_that_splits_or_merges_a_segment_stays_inside_its_region() {
        let value = document(6000, 9000, 300);
        let base = compact(&value);
        let first = lay_out(&base, None, CAP);
        let base_segments = scan(base.as_bytes()).unwrap().len();
        // Change one element until its new hash flips whether it ends a segment.
        let mut found = None;
        for k in 0..400 {
            let mut edited = value.clone();
            edited["nodes"][2500]["line"] = json!(100_000 + k);
            let text = compact(&edited);
            if scan(text.as_bytes()).unwrap().len() != base_segments {
                found = Some(edited);
                break;
            }
        }
        let edited = found.expect("some edit must move a segment boundary");
        let second = lay_out(&compact(&edited), Some(&first), CAP);
        assert_eq!(parsed(&second), edited);
        let dirty = dirty_blocks(&first, &second);
        let total = second.len().div_ceil(BLOCK);
        assert!(
            dirty <= 8,
            "a split/merge dirtied {dirty} of {total} blocks"
        );
    }

    #[test]
    fn a_long_list_inside_an_object_is_cut_too() {
        // `meta` carries lists of thousands of names. An insertion near the start
        // of one must not rewrite the rest of the list.
        let mut value = document(6000, 9000, 300);
        value["meta"]["names"] = (0..30_000)
            .map(|i| json!(format!("src/mod_{i}.rs::name_{i}")))
            .collect();
        let first = lay_out(&compact(&value), None, CAP);
        value["meta"]["names"]
            .as_array_mut()
            .unwrap()
            .insert(100, json!("src/new.rs::added"));
        let second = lay_out(&compact(&value), Some(&first), CAP);
        assert_eq!(parsed(&second), value);
        let dirty = dirty_blocks(&first, &second);
        assert!(
            dirty <= 6,
            "one insertion into a list inside `meta` dirtied {dirty} blocks"
        );
    }

    #[test]
    fn a_previous_file_with_no_gutters_is_laid_out_afresh() {
        let text = compact(&document(6000, 9000, 300));
        let laid = lay_out(&text, Some(text.as_bytes()), CAP);
        assert_eq!(laid, lay_out(&text, None, CAP));
    }

    #[test]
    fn a_previous_file_that_is_not_an_object_is_no_layout() {
        let text = compact(&document(6000, 9000, 300));
        for junk in [&b"not json"[..], b"[1,2,3]", b"", b"{\"a\":1} trailing"] {
            assert_eq!(lay_out(&text, Some(junk), CAP), lay_out(&text, None, CAP));
        }
    }

    #[test]
    fn padding_never_carries_a_file_over_the_reader_ceiling() {
        let text = compact(&document(6000, 9000, 300));
        let cap = text.len() as u64 + 10;
        assert_eq!(lay_out(&text, None, cap), text.as_bytes());
    }

    #[test]
    fn a_small_document_is_left_compact() {
        let text = compact(&document(10, 10, 1));
        assert_eq!(lay_out(&text, None, CAP), text.as_bytes());
    }
}
