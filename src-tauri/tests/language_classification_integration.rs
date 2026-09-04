//! Totality of the language classifiers.
//!
//! These run over every path and every file in a scanned repository, so their
//! inputs are whatever a repository happens to contain: invalid UTF-8, control
//! bytes in filenames, paths that are only separators, names longer than any
//! filesystem should allow. A panic here aborts a scan of an otherwise fine
//! repository, and the functions are pure, so the property worth asserting is
//! simply that they are total.

use gitpulse_lib::analyzer::LanguageDetector as D;

/// Paths chosen to break assumptions rather than to be realistic.
fn adversarial_paths() -> Vec<String> {
    let mut paths: Vec<String> = [
        "",
        " ",
        ".",
        "..",
        "/",
        "//",
        "///",
        "./",
        "../",
        "a/../b",
        "a//b",
        "/absolute/path.rs",
        "C:\\windows\\path.rs",
        "\\\\?\\UNC\\share\\file.rs",
        "no-extension",
        ".hidden",
        ".hidden.rs",
        "trailing.",
        "double..rs",
        "file.RS",
        "file.rs.bak",
        "archive.tar.gz",
        "-leading-dash.rs",
        "spaces in name.rs",
        "tab\there.rs",
        "newline\nhere.rs",
        "null\0byte.rs",
        "héllo.rs",
        "日本語.rs",
        "👩‍👩‍👧‍👦.rs",
        "node_modules/x/y.js",
        "target/debug/build.rs",
        "vendor/lib.go",
        "package-lock.json",
        "Cargo.lock",
        "dist/bundle.js",
        "src/generated/api.ts",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    // Pathological shapes: very long, very deep, and separator-only.
    paths.push("a".repeat(4096));
    paths.push(format!("{}.rs", "a".repeat(4096)));
    paths.push("x/".repeat(1024));
    paths.push(format!("{}file.rs", "deep/".repeat(512)));
    paths
}

#[test]
fn path_classifiers_are_total() {
    for path in adversarial_paths() {
        // Each of these runs for every file in a scan; none may panic.
        let info = D::detect_from_path(&path);
        let _ = D::is_image_path(&path);
        let _ = D::normalize_rel_path(&path);
        let _ = D::is_ignored_source_path(&path);
        let _ = D::is_lockfile_path(&path);
        let _ = D::is_generated_path(&path);
        let _ = D::should_count_for_stats(&path, &info);
        let _ = D::coverage_family_hint(&path, &info);
        let _ = D::comment_prefix(info.name);
        let _ = D::coverage_family(info.name);
        // Counting runs for every file in a language-stats scan, and a
        // panic there aborts the whole report rather than one file.
        let counts = gitpulse_lib::analyzer::LocCounter::count_for_language(
            "a\n/* b */\n\n# c\n\"\"\"d\"\"\"\n<!-- e -->\n",
            info.name,
        );
        assert_eq!(
            counts.code_lines + counts.comment_lines + counts.blank_lines,
            counts.total_lines,
            "{} must partition every line",
            info.name
        );
    }
}

#[test]
fn detection_from_bytes_is_total_over_arbitrary_content() {
    // File content is arbitrary bytes: binaries, truncated UTF-8, lone
    // surrogates encoded by a careless tool, NULs. Detection must classify or
    // decline, never abort the scan.
    let payloads: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x00],
        vec![0xff, 0xfe, 0xfd],
        vec![0xef, 0xbb, 0xbf], // UTF-8 BOM
        vec![0xff, 0xfe],       // UTF-16 LE BOM
        vec![0xed, 0xa0, 0x80], // lone surrogate
        vec![0xc3],             // truncated two-byte sequence
        b"#!/usr/bin/env python\nprint(1)\n".to_vec(),
        b"fn main() {}\n".to_vec(),
        (0u8..=255).collect(),
        vec![b'a'; 1024 * 1024],
        b"\x7fELF\x02\x01\x01\x00".to_vec(), // ELF header
    ];
    for path in ["x.rs", "x", "x.bin", "", "x.py"] {
        for payload in &payloads {
            let info = D::detect_from_bytes(path, payload);
            // The verdict must be usable: a language string that is never
            // empty, so callers do not have to special-case it.
            assert!(
                !info.name.is_empty(),
                "empty language name for {path:?} with {} bytes",
                payload.len()
            );
        }
    }
}

#[test]
fn normalizing_a_path_is_idempotent() {
    // The scan normalizes before comparing paths; if normalizing twice differed
    // from once, equal paths could compare unequal depending on call order.
    for path in adversarial_paths() {
        let once = D::normalize_rel_path(&path);
        let twice = D::normalize_rel_path(&once);
        assert_eq!(once, twice, "normalize is not idempotent for {path:?}");
    }
}

#[test]
fn classification_does_not_depend_on_extension_case() {
    // A repository written on a case-insensitive filesystem carries .RS and
    // .Rs; treating them as unknown would silently drop files from the stats.
    for (lower, upper) in [("a.rs", "a.RS"), ("a.py", "a.PY"), ("a.ts", "a.Ts")] {
        assert_eq!(
            D::detect_from_path(lower).name,
            D::detect_from_path(upper).name,
            "{lower} and {upper} disagree"
        );
    }
}
