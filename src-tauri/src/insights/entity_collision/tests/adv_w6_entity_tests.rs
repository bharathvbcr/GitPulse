//! W6-L1 adversarial review of `classify_incoming`: every case that cannot be
//! placed in HEAD's lines must ask (FileLevel), and every case that can be
//! placed must name the symbol the edit really lands in. A `DisjointSymbols`
//! answer where the edit is really in a symbol the other party changed is a
//! SECURITY-class break: Disjoint turns the Ask into a notice, and a headless
//! agent then edits over a live collision.
//!
//! Included from `entity_collision::tests`, so the fixtures there are in scope.

use super::*;
use crate::test_support::{git_in, write};

fn other_edits_bar() -> tempfile::TempDir {
    rust_edit(
        &rust_symbols(),
        &replace_line(RUST_TEXT, 21, "    let y = 9;"),
    )
}

/// `here` is the checkout the agent edits in: HEAD is RUST_TEXT, working file
/// is `edited`. The incoming edit replaces `old`.
fn judged(other: &tempfile::TempDir, edited: &str, edit: IncomingEdit) -> EntityCollisionVerdict {
    let here = rust_edit(&rust_symbols(), edited);
    classify_incoming(FILE, &[party_of(other)], &party_of(&here), &edit)
}

fn must_not_be_disjoint(what: &str, v: &EntityCollisionVerdict) {
    assert_ne!(
        v.kind,
        EntityCollisionKind::DisjointSymbols,
        "SECURITY: {what} read as Disjoint though the other party changed `bar`: {v:?}"
    );
}

/// Whatever this worktree has already done above the target, the incoming edit
/// into `bar` (where the other party works) must be Shared. Sweeps insertions,
/// deletions, replacements and a CR-only change above `bar`, in many sizes.
#[test]
fn w6_own_hunks_above_the_target_never_shift_the_edit_into_a_disjoint_symbol() {
    let other = other_edits_bar();
    let base: Vec<&str> = RUST_TEXT.lines().collect();
    // Line numbers (1-based) of free lines above `bar`: 1 `use`, 2 blank,
    // 15 blank, 16-18 foo, 19 blank.
    let mut shapes: Vec<(String, String)> = Vec::new();
    for at in [0usize, 1, 2, 14, 15, 16, 17, 18, 19] {
        for n in 1..=6usize {
            let mut ins = base.clone();
            for k in 0..n {
                ins.insert(at, "    // added");
                let _ = k;
            }
            shapes.push((format!("insert {n} at {at}"), ins.join("\n") + "\n"));
        }
    }
    for from in [1usize, 2, 15, 16, 17, 19] {
        for n in 1..=3usize {
            if from + n > 20 {
                continue;
            }
            let mut del = base.clone();
            del.drain(from - 1..from - 1 + n);
            shapes.push((format!("delete {n} at {from}"), del.join("\n") + "\n"));
        }
    }
    for (what, edited) in shapes {
        let v = judged(&other, &edited, replacing("    let y = 2;"));
        must_not_be_disjoint(&what, &v);
        // An own insertion inside `foo` maps to a symbol, so the edit into
        // `bar` is placed and must be Shared.
        if what == "insert 1 at 16" || what == "insert 3 at 17" {
            assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{what}: {v:?}");
            assert_eq!(v.shared_symbols, vec!["bar".to_string()], "{what}");
        }
    }
}

/// The same property with the other party's change in `foo` and the edit into
/// `foo`, under this party's own edits ABOVE it (shift) and BELOW it (none).
#[test]
fn w6_own_hunks_below_and_around_the_target_keep_it_shared() {
    let other = rust_edit(
        &rust_symbols(),
        &replace_line(RUST_TEXT, 17, "    let x = 9;"),
    );
    let base: Vec<&str> = RUST_TEXT.lines().collect();
    for at in [19usize, 20, 21, 22] {
        let mut below = base.clone();
        below.insert(at, "// below");
        let v = judged(
            &other,
            &(below.join("\n") + "\n"),
            replacing("    let x = 1;"),
        );
        must_not_be_disjoint(&format!("below {at}"), &v);
        if at == 20 || at == 21 {
            assert_eq!(
                v.kind,
                EntityCollisionKind::SharedSymbol,
                "below {at}: {v:?}"
            );
        }
    }
    // Own edit inside the very line being replaced.
    let own = replace_line(RUST_TEXT, 17, "    let x = 1; // mine");
    let v = judged(&other, &own, replacing("    let x = 1; // mine"));
    assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
}

/// A MultiEdit is shared if ANY of its edits lands in a symbol the other party
/// changed, in either position.
#[test]
fn w6_a_multiedit_is_shared_when_either_edit_hits_the_other_partys_symbol() {
    let other = other_edits_bar();
    for olds in [
        vec!["    let x = 1;", "    let y = 2;"],
        vec!["    let y = 2;", "    let x = 1;"],
    ] {
        let v = judged(
            &other,
            RUST_TEXT,
            IncomingEdit::Replacing(olds.iter().map(|s| s.to_string()).collect()),
        );
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{olds:?}: {v:?}");
    }
}

/// A later MultiEdit entry that only exists after the earlier one has been
/// applied cannot be located in the file as it is: it must ask.
#[test]
fn w6_a_multiedit_entry_that_depends_on_an_earlier_one_asks() {
    let other = other_edits_bar();
    let v = judged(
        &other,
        RUST_TEXT,
        IncomingEdit::Replacing(vec![
            "    let x = 1;".to_string(),
            "    let x = 1; // introduced by the first edit".to_string(),
        ]),
    );
    assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
}

/// A MultiEdit whose first entry makes a SECOND copy of a later entry's text
/// would be ambiguous in the tool, and a copy that already exists is ambiguous
/// here: both must ask, never pick one.
#[test]
fn w6_a_duplicated_old_string_asks_even_when_one_copy_is_clear_of_the_other_party() {
    let other = other_edits_bar();
    // `    let ` occurs in foo AND bar; `}\n` occurs many times.
    for old in ["    let ", "}\n", "fn "] {
        let v = judged(&other, RUST_TEXT, replacing(old));
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{old:?}: {v:?}");
    }
}

/// CRLF: a single-line old_string in a CRLF file is placed on the right line;
/// a multi-line LF old_string (which the tool would not find either) asks.
#[test]
fn w6_crlf_files_place_by_line_and_ask_when_the_newlines_differ() {
    let crlf = RUST_TEXT.replace('\n', "\r\n");
    let make = |edited: &str| {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        git_in(dir.path(), &["config", "core.autocrlf", "false"]);
        write(dir.path(), FILE, &crlf);
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        let head = git_text(dir.path(), &["rev-parse", "HEAD"]).unwrap();
        crate::codeintel::tests::seed_file_symbols(
            dir.path(),
            head.trim(),
            FILE,
            &crlf,
            &rust_symbols(),
        );
        write(dir.path(), FILE, edited);
        dir
    };
    let other = make(&crlf.replace("    let y = 2;", "    let y = 9;"));
    let here = make(&crlf);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    assert_eq!(
        v.kind,
        EntityCollisionKind::SharedSymbol,
        "crlf single line: {v:?}"
    );
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("fn bar() {\n    let y = 2;"),
    );
    must_not_be_disjoint("LF old_string in a CRLF file", &v);
    assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
}

/// Unicode: an old_string that differs from the file only by normalisation
/// form is not found byte-for-byte, so it must ask rather than guess.
#[test]
fn w6_a_differently_normalised_old_string_asks() {
    let nfc = "    let caf\u{e9} = 1;";
    let nfd = "    let cafe\u{301} = 1;";
    let text = RUST_TEXT.replace("    let y = 2;", nfc);
    let other = worktree(
        &text,
        &rust_symbols(),
        &replace_line(&text, 21, "    let y = 9;"),
    );
    let here = rust_edit_text(&text, &text);
    let v = classify_incoming(FILE, &[party_of(&other)], &party_of(&here), &replacing(nfd));
    must_not_be_disjoint("an NFD old_string against an NFC file", &v);
    assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
    // Multi-byte text before the target must not skew the line count.
    let v = classify_incoming(FILE, &[party_of(&other)], &party_of(&here), &replacing(nfc));
    assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
}

fn rust_edit_text(head_text: &str, edited: &str) -> tempfile::TempDir {
    worktree(head_text, &rust_symbols(), edited)
}

/// A file past the bounded read is not located: it asks.
#[test]
fn w6_a_file_larger_than_the_bounded_read_asks() {
    let mut big = RUST_TEXT.to_string();
    big.push_str(&"// filler line to push the file past the bound\n".repeat(100_000));
    assert!(big.len() > MAX_INCOMING_FILE_BYTES);
    let other = worktree(
        &big,
        &rust_symbols(),
        &replace_line(&big, 21, "    let y = 9;"),
    );
    let here = worktree(&big, &rust_symbols(), &big);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    must_not_be_disjoint("an oversized file", &v);
}

/// A file with a NUL byte (binary to git) and one with invalid UTF-8 ask.
#[test]
fn w6_binary_and_non_utf8_files_ask() {
    let other = other_edits_bar();
    let make = |bytes: &[u8]| {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        std::fs::write(dir.path().join(FILE), RUST_TEXT).unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        let head = git_text(dir.path(), &["rev-parse", "HEAD"]).unwrap();
        crate::codeintel::tests::seed_file_symbols(
            dir.path(),
            head.trim(),
            FILE,
            RUST_TEXT,
            &rust_symbols(),
        );
        std::fs::write(dir.path().join(FILE), bytes).unwrap();
        dir
    };
    let mut with_nul = RUST_TEXT.as_bytes().to_vec();
    with_nul.extend_from_slice(b"\0tail\n");
    let mut latin1 = RUST_TEXT
        .replace("    let y = 2;", "    let y = 2; // ")
        .into_bytes();
    latin1.push(0xE9);
    latin1.push(b'\n');
    let (mut verdicts, mut bad) = (Vec::new(), Vec::new());
    for (what, bytes) in [("nul", with_nul), ("latin1", latin1)] {
        let here = make(&bytes);
        let v = classify_incoming(
            FILE,
            &[party_of(&other)],
            &party_of(&here),
            &replacing("    let x = 1;"),
        );
        verdicts.push(format!("{what}: {:?}", v.kind));
        if v.kind != EntityCollisionKind::FileLevel {
            bad.push(format!("{what}: {v:?}"));
        }
    }
    eprintln!("binary/non-utf8 verdicts: {verdicts:?}");
    assert!(bad.is_empty(), "SECURITY: {bad:?}");
}

/// An Edit on a file that is not in this worktree's HEAD (new, staged only)
/// asks.
#[test]
fn w6_an_edit_of_a_file_not_yet_in_head_asks() {
    let other = other_edits_bar();
    let here = tempfile::TempDir::new().unwrap();
    git_in(here.path(), &["init", "-b", "main"]);
    write(here.path(), "other.txt", "x\n");
    git_in(here.path(), &["add", "."]);
    git_in(here.path(), &["commit", "-m", "base"]);
    write(here.path(), FILE, RUST_TEXT);
    git_in(here.path(), &["add", FILE]);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
    // And untracked entirely.
    git_in(here.path(), &["rm", "--cached", "-q", FILE]);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
}

/// The staged half: this worktree staged a change above, then reverted it in
/// the working tree. The union of staged and working hunks is used for the
/// footprint, but only the working diff places the edit; the working file is
/// HEAD's lines, so the edit must still land in `bar`.
#[test]
fn w6_a_staged_then_reverted_change_does_not_shift_the_placement() {
    let other = other_edits_bar();
    let here = rust_edit(&rust_symbols(), RUST_TEXT);
    write(
        here.path(),
        FILE,
        &insert_after(RUST_TEXT, 16, "    // staged line"),
    );
    git_in(here.path(), &["add", FILE]);
    write(here.path(), FILE, RUST_TEXT);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
}

/// `git diff` configuration in the checkout must not change the numbering:
/// a textconv driver, `diff.noprefix`, and an external diff are all set.
#[test]
fn w6_diff_config_does_not_misplace_the_edit() {
    let other = other_edits_bar();
    let here = rust_edit(
        &rust_symbols(),
        &insert_after(RUST_TEXT, 16, "    // own line above"),
    );
    git_in(here.path(), &["config", "diff.noprefix", "true"]);
    git_in(here.path(), &["config", "diff.algorithm", "patience"]);
    git_in(here.path(), &["config", "diff.context", "9"]);
    git_in(here.path(), &["config", "diff.interHunkContext", "9"]);
    git_in(here.path(), &["config", "diff.renames", "copies"]);
    git_in(here.path(), &["config", "core.quotepath", "true"]);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
}

/// A working file that is a symlink is not followed.
#[cfg(unix)]
#[test]
fn w6_a_symlinked_working_file_asks() {
    let other = other_edits_bar();
    let here = rust_edit(&rust_symbols(), RUST_TEXT);
    let real = here.path().join("real.rs");
    std::fs::write(&real, RUST_TEXT).unwrap();
    std::fs::remove_file(here.path().join(FILE)).unwrap();
    std::os::unix::fs::symlink(&real, here.path().join(FILE)).unwrap();
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let y = 2;"),
    );
    must_not_be_disjoint("a symlinked working file", &v);
}

/// A working file git calls binary (a NUL byte) yields a `git diff HEAD` with no
/// hunk headers, so nothing numbers the working lines. Lines this worktree
/// inserted above the target then shift it, and an edit into `foo` is placed
/// on HEAD's line of the same number, which is `bar`.
#[test]
#[allow(non_snake_case)]
fn w6_BREAK_a_binary_diff_leaves_the_edit_unplaced_and_reads_disjoint() {
    // The other party changed `foo` (HEAD line 17).
    let other = rust_edit(
        &rust_symbols(),
        &replace_line(RUST_TEXT, 17, "    let x = 9;"),
    );
    // Here: five lines inserted at the top, one carrying a NUL, so foo's
    // `let x = 1;` is working line 22 while HEAD's line 22 is in `bar`.
    let mut edited = String::from("\0\n// 2\n// 3\n// 4\n// 5\n");
    edited.push_str(RUST_TEXT);
    let here = rust_edit(&rust_symbols(), &edited);
    let v = classify_incoming(
        FILE,
        &[party_of(&other)],
        &party_of(&here),
        &replacing("    let x = 1;"),
    );
    must_not_be_disjoint("an edit into `foo` in a file git treats as binary", &v);
}
