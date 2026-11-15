//! Engine and parser tests. Credential-shaped strings are assembled at run
//! time so this file does not itself carry a token any scanner would stop.

use super::*;

/// A GitHub classic PAT shape: `ghp_` and 36 alphanumerics.
pub(crate) fn github_pat() -> String {
    format!("ghp_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5")
}

/// An AWS access key id shape: `AKIA` and 16 upper-case alphanumerics.
pub(crate) fn aws_key_id() -> String {
    format!("AKIA{}", "Q3ZT7KXR2M5NWB8D")
}

#[test]
fn the_engine_names_the_vendored_table_version() {
    let manifest = include_str!("../../vendored/dc-redact/Cargo.toml");
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("dc-redact manifest has a version");
    assert_eq!(ENGINE, format!("dc-redact {version}"));
}

#[test]
fn a_line_hit_names_the_shape_path_and_line_and_never_the_value() {
    let mut tally = Tally::default();
    let token = github_pat();
    tally.scan_line("src/a.rs", 7, format!("let t = \"{token}\";").as_bytes());
    assert_eq!(tally.hits_total, 1);
    assert_eq!(tally.hits[0].path, "src/a.rs");
    assert_eq!(tally.hits[0].line, 7);
    assert!(!tally.hits[0].rule.is_empty());
    let wire = serde_json::to_string(&tally).unwrap();
    assert!(!wire.contains(&token), "{wire}");
    assert!(!wire.contains(&token[..12]), "a prefix leaked: {wire}");
    assert!(!tally.is_clean());
}

#[test]
fn the_detector_is_exactly_dc_redact_line_by_line() {
    // Parity: the engine adds no shapes and drops none. Every line of a mixed
    // corpus is judged by both, and they must agree on every one.
    let corpus = [
        format!("token = {}", github_pat()),
        format!("aws_access_key_id={}", aws_key_id()),
        "let task_name = \"task-runner-disk-risk-skeleton\";".to_string(),
        "integrity sha512-AbCdEf0123456789AbCdEf0123456789AbCdEf0123456789==".to_string(),
        "id: 123e4567-e89b-12d3-a456-426614174000".to_string(),
        "password = \"${DB_PASSWORD}\"".to_string(),
        format!(
            "url = https://{}:{}@example.com/x",
            "user", "hunter2hunter2"
        ),
        String::new(),
        "\u{00e9}t\u{00e9} \u{2014} plain prose".to_string(),
    ];
    for line in &corpus {
        let mut tally = Tally::default();
        tally.scan_line("f", 1, line.as_bytes());
        assert_eq!(
            tally.hits_total == 1,
            dc_redact::find_secret(line).is_some(),
            "disagreement on {line:?}"
        );
    }
}

#[test]
fn invalid_utf8_and_crlf_do_not_hide_a_token() {
    let mut line = vec![0xff, 0xfe, b' '];
    line.extend_from_slice(github_pat().as_bytes());
    line.extend_from_slice(b"\r");
    let mut tally = Tally::default();
    tally.scan_line("bin.dat", 1, &line);
    assert_eq!(tally.hits_total, 1);
}

#[test]
fn a_blob_is_numbered_from_one_and_counts_every_line() {
    let blob = format!("a\nb\nkey={}\n", aws_key_id());
    let mut tally = Tally::default();
    tally.scan_blob("x.bin", blob.as_bytes());
    assert_eq!(tally.inputs_scanned, 1);
    assert_eq!(tally.lines_scanned, 4, "three lines and the empty tail");
    assert_eq!(tally.hits[0].line, 3);
}

/// A blob with lines long enough to be windowed, keys placed across window
/// edges and inside overlaps, CRLF endings and a short-line tail.
fn windowed_corpus() -> Vec<u8> {
    let step = LINE_WINDOW - WINDOW_OVERLAP;
    let mut blob = Vec::new();
    // Line 1: a key across the first window's edge.
    let mut line = vec![b' '; LINE_WINDOW * 3];
    let key = format!(" {} ", github_pat());
    let at = LINE_WINDOW - 10;
    line[at..at + key.len()].copy_from_slice(key.as_bytes());
    blob.extend_from_slice(&line);
    blob.extend_from_slice(b"\r\n");
    // Line 2: a key wholly inside an overlap, so two windows hold it.
    let mut line = vec![b' '; LINE_WINDOW * 2];
    let key = format!(" {} ", aws_key_id());
    let at = step + 100;
    line[at..at + key.len()].copy_from_slice(key.as_bytes());
    blob.extend_from_slice(&line);
    blob.push(b'\n');
    // Line 3: long and clean. Line 4: short with a key. Line 5: no newline.
    blob.extend_from_slice(&vec![b'z'; LINE_WINDOW + 1]);
    blob.push(b'\n');
    blob.extend_from_slice(format!("k={}\n", github_pat()).as_bytes());
    blob.extend_from_slice(b"tail");
    blob
}

#[test]
fn a_long_line_is_read_in_windows_that_keep_a_key_at_the_edge_whole() {
    let blob = windowed_corpus();
    let mut tally = Tally::default();
    tally.scan_blob("big.bin", &blob);
    let lines: Vec<u32> = tally.hits.iter().map(|h| h.line).collect();
    assert_eq!(
        lines,
        [1, 2, 4],
        "one hit per line, a key in two windows counted once"
    );
    assert_eq!(tally.lines_scanned, 5);
    let line_bytes: usize = blob
        .split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l).len())
        .sum();
    assert_eq!(
        tally.bytes_scanned as usize, line_bytes,
        "each byte counted once, overlaps included"
    );
}

#[test]
fn where_a_stream_is_cut_never_changes_the_verdict() {
    let blob = windowed_corpus();
    let mut whole = Tally::default();
    whole.scan_blob("big.bin", &blob);
    // Seeded xorshift: chunk sizes from 1 byte to past a window.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    for _round in 0..24 {
        let mut pieces = Tally::default();
        let mut lines = BlobLines::new(&mut pieces, "big.bin");
        let mut at = 0;
        while at < blob.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let size = 1 + (state as usize) % (LINE_WINDOW * 2);
            let end = (at + size).min(blob.len());
            lines.feed(&blob[at..end]);
            at = end;
        }
        lines.finish();
        assert_eq!(pieces, whole);
    }
}

#[test]
fn a_skip_is_never_clean() {
    let mut tally = Tally::default();
    assert!(tally.is_clean());
    tally.skip("big.bin", SkipReason::Oversized, Some(1 << 30));
    assert!(!tally.is_clean());
    assert_eq!(tally.skipped_total, 1);
}

#[test]
fn reported_rows_are_capped_but_counted() {
    let mut tally = Tally::default();
    let line = format!("k={}", github_pat());
    for n in 0..(MAX_REPORTED_HITS as u32 + 50) {
        tally.scan_line("f", n + 1, line.as_bytes());
        tally.skip("g", SkipReason::Unreadable, None);
    }
    assert_eq!(tally.hits.len(), MAX_REPORTED_HITS);
    assert_eq!(tally.hits_total, MAX_REPORTED_HITS as u64 + 50);
    assert_eq!(tally.skipped.len(), MAX_REPORTED_SKIPS);
    assert_eq!(tally.skipped_total, MAX_REPORTED_SKIPS as u64 + 50);
}

const PATCH: &str = "diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -3,0 +4,2 @@ fn main() {
+first added
++++ an added line that starts with three pluses
@@ -10 +11 @@
-removed
+replaced
\\ No newline at end of file
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..3333333
Binary files /dev/null and b/img.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
";

#[test]
fn the_patch_parser_keeps_new_side_numbers_and_payload_bytes() {
    let sections = parse_patch(PATCH.as_bytes()).expect("parses");
    assert_eq!(sections.len(), 3);
    let added: Vec<(u32, &str)> = sections[0]
        .added
        .iter()
        .map(|(n, r)| (*n, &PATCH[r.clone()]))
        .collect();
    assert_eq!(
        added,
        [
            (4, "first added"),
            (5, "+++ an added line that starts with three pluses"),
            (11, "replaced"),
        ]
    );
    assert!(!sections[0].binary);
    assert!(sections[1].binary && sections[1].added.is_empty());
    assert!(!sections[2].binary && sections[2].added.is_empty());
}

#[test]
fn a_patch_that_disagrees_with_its_own_headers_is_refused() {
    // Header promises two added lines; one arrives before the next section.
    let short = "diff --git a/x b/x\n@@ -0,0 +1,2 @@\n+only one\ndiff --git a/y b/y\n";
    assert_eq!(
        parse_patch(short.as_bytes()),
        Err(PatchError::HunkCountMismatch)
    );
    // Header promises one; two arrive.
    let long = "diff --git a/x b/x\n@@ -0,0 +1 @@\n+one\n+two\n";
    assert_eq!(
        parse_patch(long.as_bytes()),
        Err(PatchError::HunkCountMismatch)
    );
    // A truncated final hunk.
    let cut = "diff --git a/x b/x\n@@ -0,0 +1,3 @@\n+one\n";
    assert_eq!(
        parse_patch(cut.as_bytes()),
        Err(PatchError::HunkCountMismatch)
    );
    assert_eq!(parse_patch(b"+orphan\n"), Err(PatchError::OrphanLine));
    let bad = "diff --git a/x b/x\n@@ -0,0 +x @@\n";
    assert_eq!(parse_patch(bad.as_bytes()), Err(PatchError::BadHunkHeader));
    let odd = "diff --git a/x b/x\n@@ -0,0 +1 @@\n+one\n?what\n";
    assert_eq!(
        parse_patch(odd.as_bytes()),
        Err(PatchError::UnexpectedHunkLine)
    );
}

#[test]
fn an_empty_patch_is_no_sections() {
    assert_eq!(parse_patch(b""), Ok(Vec::new()));
}

#[test]
fn raw_entries_take_the_destination_of_a_rename_and_spot_blobless_entries() {
    let oid = |c: char| c.to_string().repeat(40);
    let zero = oid('0');
    let raw = format!(
        ":100644 100644 {a} {b} M\0src/a.rs\0\
         :100644 100644 {a} {b} R087\0old name\0new\nname\0\
         :100644 000000 {a} {zero} D\0gone\0\
         :160000 160000 {a} {b} M\0sub\0\
         :000000 000000 {zero} {zero} U\0conflict\0",
        a = oid('a'),
        b = oid('b'),
    );
    let entries = parse_raw(raw.as_bytes()).expect("parses");
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["src/a.rs", "new\nname", "gone", "sub", "conflict"]);
    let blobs: Vec<bool> = entries.iter().map(RawEntry::has_blob).collect();
    assert_eq!(blobs, [true, true, false, false, false]);
    assert_eq!(parse_raw(b"not raw\0x\0"), None);
    assert_eq!(parse_raw(b""), Some(Vec::new()));
}
