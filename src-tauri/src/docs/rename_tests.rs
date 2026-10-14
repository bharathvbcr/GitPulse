//! Link-preserving rename: what each rewritten link says, where, and to whom.

use super::*;
use crate::engine::git_writer::MovePlan;
use crate::test_support::{git_in, git_repo, write};

/// A committed repository holding exactly `files`.
fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = git_repo();
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "fixture"]);
    dir
}

fn read(dir: &tempfile::TempDir, rel: &str) -> String {
    std::fs::read_to_string(dir.path().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Notes in folders, with a markdown link to the moved note from each place.
fn nested_fixture_repo() -> tempfile::TempDir {
    repo_with(&[
        (
            "docs/Guide.md",
            "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n",
        ),
        (
            "docs/Intro.md",
            "# Intro\n\nBack to [Guide](Guide.md).\n\n```md\n[[Roadmap]] [f](Roadmap.md)\n```\n",
        ),
        (
            "docs/Roadmap.md",
            "# Roadmap\n\nSee [guide](Guide.md#top).\n",
        ),
        ("README.md", "# Repo\n\n[R](docs/Roadmap.md)\n"),
    ])
}

#[test]
fn a_sibling_link_keeps_its_directory_when_the_target_is_renamed_in_place() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "docs/Plan.md", |_| Ok(())).expect("rename");

    assert!(
        read(&dir, "docs/Guide.md").contains("[x](Plan.md)"),
        "{}",
        read(&dir, "docs/Guide.md")
    );
    let built = load_vault(path, true).unwrap();
    let resolved = built
        .vault
        .resolve_from("docs/Guide.md", "Plan.md", None)
        .expect("the rewritten link resolves");
    assert_eq!(resolved.path, "docs/Plan.md");
    assert!(
        built.vault.broken_links().is_empty(),
        "{:?}",
        built.vault.broken_links()
    );
    // The fenced sample still says what it said.
    assert!(read(&dir, "docs/Intro.md").contains("```md\n[[Roadmap]] [f](Roadmap.md)\n```"));
}

#[test]
fn a_link_follows_the_target_into_another_directory_relative_to_the_linking_note() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");

    assert!(
        read(&dir, "docs/Guide.md").contains("[x](../plans/Roadmap.md)"),
        "{}",
        read(&dir, "docs/Guide.md")
    );
    assert!(
        read(&dir, "README.md").contains("[R](plans/Roadmap.md)"),
        "{}",
        read(&dir, "README.md")
    );
    let built = load_vault(path, true).unwrap();
    assert!(built.vault.broken_links().is_empty());
    assert_eq!(
        built
            .vault
            .resolve_from("docs/Guide.md", "../plans/Roadmap.md", None)
            .unwrap()
            .path,
        "plans/Roadmap.md"
    );
}

/// Every markdown link before the rename, followed to its note, ends at the
/// same note after it (the moved note having changed its path), and the set of
/// broken links is the one it was.
fn assert_rename_preserves_every_link(
    dir: &tempfile::TempDir,
    from: &str,
    to: &str,
) -> (Vec<String>, String) {
    let path = dir.path().to_str().unwrap();
    let before = load_vault(path, true).unwrap();
    let mut expected: Vec<(String, usize, Option<String>)> = Vec::new();
    for note in before.vault.notes() {
        for (position, link) in note.links.iter().enumerate() {
            let landed = before.vault.resolve_link(&note.path, link).map(|r| {
                if r.path == from {
                    to.to_string()
                } else {
                    r.path
                }
            });
            let source = if note.path == from { to } else { &note.path };
            expected.push((source.to_string(), position, landed));
        }
    }
    let mut broken_before: Vec<(String, String)> = before
        .vault
        .broken_links()
        .into_iter()
        .map(|(s, t)| (if s == from { to.to_string() } else { s }, t))
        .collect();
    broken_before.sort();

    let outcome = rename_doc(path, from, to, |_| Ok(())).expect("rename");

    let after = refresh_and_load(path);
    for (source, position, landed) in expected {
        let note = after
            .vault
            .note(&source)
            .unwrap_or_else(|| panic!("{source} missing after the rename"));
        let link = note
            .links
            .get(position)
            .unwrap_or_else(|| panic!("{source} lost link {position}"));
        let now = after.vault.resolve_link(&source, link).map(|r| r.path);
        assert_eq!(
            now, landed,
            "{source} link {position} ({:?}) moved to a different note",
            link.target
        );
    }
    let mut broken_after = after.vault.broken_links();
    broken_after.sort();
    assert_eq!(
        broken_after, broken_before,
        "the set of broken links changed"
    );
    (outcome.rewritten_paths, read(dir, to))
}

fn refresh_and_load(path: &str) -> Arc<CachedVault> {
    invalidate(path);
    load_vault(path, true).unwrap()
}

#[test]
fn every_link_spelling_still_reaches_its_note_after_a_move_between_directories() {
    let dir = repo_with(&[
        (
            "docs/deep/Target.md",
            "# Target\n\n## intro\n\n[self](Target.md#intro) [sib](Sib.md) [up](../Up.md#h) \
             [root](/README.md) [t](Sib.md \"title\") [n](<Sib.md>)\n",
        ),
        (
            "docs/deep/Sib.md",
            "[a](Target.md \"t\") [b](<Target.md>) [c](./Target.md#intro) [d](../deep/Target.md) \
             [e](/docs/deep/Target.md) [f](Target.md#intro) [g](%54arget.md) [h](Target.md 'single')\n",
        ),
        (
            "docs/Up.md",
            "[a](deep/Target.md) [b](./deep/Target.md \"title\") [c](<deep/Target.md#intro>)\n",
        ),
        ("Page.mdx", "[a](docs/deep/Target.md#intro)\n"),
        ("README.md", "[a](docs/deep/Target.md) [b](docs/Up.md)\n"),
        ("docs/Wiki.md", "[[Target]] and [[docs/deep/Target]]\n"),
    ]);
    let (rewritten, moved) =
        assert_rename_preserves_every_link(&dir, "docs/deep/Target.md", "archive/Moved Target.md");
    assert!(
        rewritten.contains(&"docs/deep/Sib.md".to_string()),
        "{rewritten:?}"
    );
    assert!(moved.contains("[self](Moved%20Target.md#intro)"), "{moved}");
    assert!(moved.contains("[sib](../docs/deep/Sib.md)"), "{moved}");
    assert!(
        moved.contains("[t](../docs/deep/Sib.md \"title\")"),
        "{moved}"
    );
    assert!(moved.contains("[n](<../docs/deep/Sib.md>)"), "{moved}");
    assert!(moved.contains("[root](/README.md)"), "{moved}");
    // Titles and the bracketed form are carried through byte for byte.
    let sib = read(&dir, "docs/deep/Sib.md");
    assert!(sib.contains("\"t\")"), "{sib}");
    assert!(sib.contains("'single')"), "{sib}");
}

#[test]
fn a_move_that_changes_the_extension_still_resolves() {
    let dir = repo_with(&[
        ("a/Note.md", "# Note\n"),
        ("b/Link.md", "[n](../a/Note.md#top) [m](../a/Note.md)\n"),
    ]);
    assert_rename_preserves_every_link(&dir, "a/Note.md", "c/Note.mdx");
    assert!(read(&dir, "b/Link.md").contains("../c/Note.mdx#top"));
}

#[test]
fn the_moved_notes_own_relative_links_are_re_expressed_from_its_new_folder() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n[b](B.md)\n"), ("docs/B.md", "# B\n")]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| Ok(())).expect("rename");

    assert_eq!(read(&dir, "notes/A.md"), "# A\n\n[b](../docs/B.md)\n");
    assert!(
        outcome.rewritten_paths.contains(&"notes/A.md".to_string()),
        "{outcome:?}"
    );
    let built = load_vault(path, true).unwrap();
    assert_eq!(
        built
            .vault
            .resolve_from("notes/A.md", "../docs/B.md", None)
            .unwrap()
            .path,
        "docs/B.md"
    );
    assert!(built.vault.broken_links().is_empty());
}

#[test]
fn a_same_folder_rename_leaves_the_moved_note_byte_identical() {
    let text = "# A\n\n[b](B.md) [w](./B.md#x \"t\") [[B]] [r](/docs/B.md)\r\nend\r\n";
    let dir = repo_with(&[("docs/A.md", text), ("docs/B.md", "# B\n\n## x\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "docs/Z.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "docs/Z.md"), text);
}

#[test]
fn a_self_link_in_the_moved_note_follows_it() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n## intro\n\n[top](A.md#intro)\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "notes/Renamed.md", |_| Ok(())).expect("rename");

    assert_eq!(
        read(&dir, "notes/Renamed.md"),
        "# A\n\n## intro\n\n[top](Renamed.md#intro)\n"
    );
    let built = load_vault(path, true).unwrap();
    assert_eq!(
        built
            .vault
            .resolve_from("notes/Renamed.md", "Renamed.md", None)
            .unwrap()
            .path,
        "notes/Renamed.md"
    );
}

#[test]
fn a_self_wikilink_follows_a_stem_change() {
    let dir = repo_with(&[("docs/A.md", "# A\n\n[[A]] and [[A#x]]\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "docs/Alpha.md", |_| Ok(())).expect("rename");
    assert_eq!(
        read(&dir, "docs/Alpha.md"),
        "# A\n\n[[Alpha]] and [[Alpha#x]]\n"
    );
}

#[test]
fn crlf_notes_change_only_inside_the_link() {
    let original = "# Guide\r\n\r\nSee [x](Roadmap.md) now.\r\nLast line\r\n";
    let dir = repo_with(&[("docs/Guide.md", original), ("docs/Roadmap.md", "# R\n")]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        std::fs::read(dir.path().join("docs/Guide.md")).unwrap(),
        original
            .replace("(Roadmap.md)", "(../plans/Roadmap.md)")
            .into_bytes()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_case_only_rename_on_a_case_insensitive_volume_is_refused() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let probe = dir.path().join("docs/GUIDE.md");
    if !probe.exists() {
        return; // a case-sensitive volume: the situation does not arise
    }
    let refused = rename_doc(path, "docs/Roadmap.md", "docs/ROADMAP.md", |_| Ok(()));
    assert!(refused.is_err(), "{refused:?}");
    assert!(dir.path().join("docs/Roadmap.md").exists());
}

/// Min-of-N timing of the rename path on the synthetic 5,000-note corpus the
/// refresh benchmark uses, with the in-memory pre-check timed on its own so
/// "rename without it" is the difference.
///
/// `cargo test --release --manifest-path src-tauri/Cargo.toml --lib
/// docs::rename_tests::docs_rename_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "manual timing workload"]
fn docs_rename_benchmark() {
    use std::time::Instant;
    const RUNS: usize = 7;
    fn min_ms(runs: usize, mut run: impl FnMut()) -> f64 {
        (0..runs)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min)
    }
    let dir = git_repo();
    for n in 0..MAX_DOC_FILES {
        write(
            dir.path(),
            &format!("notes/{:02}/note-{n:05}.md", n % 50),
            &format!("# Note {n}\n[[note-00000]] [m](../00/note-00000.md) #corpus body-{n}\n"),
        );
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "benchmark corpus"]);
    let path = dir.path().to_str().unwrap();
    probe::set_clock_ahead(Duration::from_secs(5));
    let repo = validate_repo(path).unwrap();
    let built = load_vault(path, true).unwrap();
    load_vault(path, true).unwrap();

    // A note nothing links at, and the one every note links at.
    for (label, from, to) in [
        ("leaf", "notes/07/note-00007.md", "moved/note-00007.md"),
        ("hub", "notes/00/note-00000.md", "moved/note-00000.md"),
    ] {
        let mut edits = Vec::new();
        let plan_ms = min_ms(RUNS, || {
            edits = super::rename::plan_edits(&built.vault, from, to).unwrap();
        });
        let precheck_ms = min_ms(RUNS, || {
            super::rename::links_the_rename_breaks(&repo, &built.vault, from, to, &edits);
        });
        let (mut a, mut b) = (from.to_string(), to.to_string());
        let end_to_end_ms = min_ms(if label == "hub" { 1 } else { RUNS }, || {
            rename_doc(path, &a, &b, |_| Ok(())).unwrap();
            std::mem::swap(&mut a, &mut b);
        });
        println!(
            "docs_rename_bench target={label} notes={} rewrites={} plan_ms={plan_ms:.2} \
             precheck_ms={precheck_ms:.2} rename_end_to_end_ms={end_to_end_ms:.2} \
             precheck_share={:.1}%",
            built.status.note_count,
            edits.len(),
            100.0 * precheck_ms / end_to_end_ms
        );
    }
}

/// Four notes that all link at `Roadmap.md`, so a rename rewrites four files.
fn crowded_repo() -> tempfile::TempDir {
    repo_with(&[
        ("README.md", "[r](docs/Roadmap.md)\n"),
        ("docs/A.md", "[r](Roadmap.md)\n"),
        ("docs/B.md", "[r](Roadmap.md)\n"),
        ("docs/C.md", "[r](Roadmap.md)\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ])
}

fn staged(dir: &tempfile::TempDir) -> String {
    git_text(dir.path(), &["diff", "--cached", "--name-status"]).unwrap()
}

#[test]
fn a_note_edited_after_the_plan_is_left_as_the_user_wrote_it() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::write(&guide, "# Guide\n\nI was typing [x](Roadmap.md).\n").unwrap();
        Ok(())
    })
    .expect("rename");

    assert_eq!(outcome.skipped_conflict, ["docs/Guide.md"], "{outcome:?}");
    assert!(!outcome.complete);
    assert_eq!(
        std::fs::read_to_string(&guide).unwrap(),
        "# Guide\n\nI was typing [x](Roadmap.md).\n"
    );
    // The rest of the plan still lands.
    assert_eq!(
        outcome.rewritten_paths,
        ["README.md", "plans/Roadmap.md"],
        "{outcome:?}"
    );
    assert!(read(&dir, "README.md").contains("(plans/Roadmap.md)"));
}

#[test]
fn an_expecting_write_replaces_only_the_bytes_it_expected() {
    use crate::engine::git_cli::sandbox_write_expecting;
    let dir = repo_with(&[("a.md", "one\n")]);
    let path = dir.path().to_str().unwrap();
    assert!(sandbox_write_expecting(path, "a.md", "one\n", "two\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // Stale expectation: nothing is written.
    assert!(!sandbox_write_expecting(path, "a.md", "one\n", "three\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // A file that is gone is not "the bytes I expected" either.
    assert!(!sandbox_write_expecting(path, "gone.md", "one\n", "three\n").unwrap());
    assert!(!dir.path().join("gone.md").exists());
}

#[test]
fn a_write_fault_on_one_note_is_reported_and_the_others_are_still_written() {
    let dir = crowded_repo();
    let path = dir.path().to_str().unwrap();
    probe::set_write_fault_at(Some(1));
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(()));
    probe::set_write_fault_at(None);
    let outcome = outcome.expect("a partial rename is reported, not an error");

    assert!(!outcome.complete, "{outcome:?}");
    assert_eq!(outcome.failed.len(), 1, "{outcome:?}");
    assert_eq!(outcome.failed[0].path, "docs/A.md");
    assert!(outcome.failed[0].error.contains("injected"), "{outcome:?}");
    assert_eq!(
        outcome.rewritten_paths,
        ["README.md", "docs/B.md", "docs/C.md"],
        "{outcome:?}"
    );
    assert_eq!(outcome.links_rewritten, 3);
    assert_eq!(read(&dir, "docs/A.md"), "[r](Roadmap.md)\n");
    let staged = staged(&dir);
    for rewritten in ["README.md", "docs/B.md", "docs/C.md"] {
        assert!(
            staged.contains(rewritten),
            "{rewritten} not staged: {staged}"
        );
    }
    assert!(!staged.contains("docs/A.md"), "{staged}");
}

#[test]
fn a_refused_rewrite_path_refuses_the_whole_rename_before_anything_moves() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let before = read(&dir, "docs/Guide.md");
    let refused = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |plan| {
        if plan.rewrites.iter().any(|p| p == "docs/Guide.md") {
            Err("docs/Guide.md is outside the task".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(
        refused.unwrap_err(),
        "docs/Guide.md is outside the task".to_string()
    );
    assert!(dir.path().join("docs/Roadmap.md").exists());
    assert!(!dir.path().join("plans").exists());
    assert_eq!(read(&dir, "docs/Guide.md"), before);
    assert_eq!(
        git_text(dir.path(), &["status", "--porcelain"]).unwrap(),
        ""
    );
}

#[test]
fn the_gate_is_shown_the_move_and_every_path_the_rename_will_write() {
    let dir = repo_with(&[
        ("docs/A.md", "[b](B.md)\n"),
        ("docs/B.md", "# B\n"),
        ("docs/C.md", "[a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let mut seen = Vec::new();
    rename_doc(path, "docs/A.md", "notes/A.md", |plan| {
        assert!(matches!(plan.mv, MovePlan::Git { .. }));
        seen = plan.rewrites.to_vec();
        Ok(())
    })
    .expect("rename");
    // The moved note under its new path, then the note that links at it.
    assert_eq!(seen, ["notes/A.md", "docs/C.md"]);
}

#[test]
fn a_file_cap_that_hides_notes_is_reported_not_hidden() {
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("c.md", "[b](b.md)\n"),
        ("d.md", "[b](b.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    let outcome = outcome.expect("rename");

    assert!(outcome.scan.truncated, "{outcome:?}");
    assert_eq!(outcome.scan.note_count, 2);
    assert_eq!(outcome.scan.not_scanned, 2);
    assert_eq!(outcome.rewritten_paths, ["a.md"]);
    // The notes it never saw still point at the old name: that is what the
    // count is for.
    assert_eq!(read(&dir, "c.md"), "[b](b.md)\n");
}

#[test]
fn a_note_that_is_not_utf8_is_counted_and_never_written() {
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n")]);
    std::fs::write(dir.path().join("bin.md"), [0xff, 0xfe, b'[', b']']).unwrap();
    git_in(dir.path(), &["add", "bin.md"]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.skipped_unreadable, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(
        std::fs::read(dir.path().join("bin.md")).unwrap(),
        [0xff, 0xfe, b'[', b']']
    );
}

#[test]
fn a_reference_definition_is_reported_because_it_is_not_rewritten() {
    let dir = repo_with(&[
        ("docs/Ref.md", "[x][ref]\n\n[ref]: Roadmap.md\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        outcome.left_broken,
        [BrokenLink {
            source: "docs/Ref.md".into(),
            target: "Roadmap.md".into(),
        }],
        "{outcome:?}"
    );
    assert_eq!(read(&dir, "docs/Ref.md"), "[x][ref]\n\n[ref]: Roadmap.md\n");
}

#[test]
fn a_clean_rename_reports_nothing_left_broken_and_is_complete() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert!(outcome.complete, "{outcome:?}");
    assert!(outcome.left_broken.is_empty(), "{outcome:?}");
    assert!(outcome.skipped_conflict.is_empty() && outcome.failed.is_empty());
    assert_eq!(outcome.scan.not_scanned, 0);
}

#[test]
fn a_link_that_cannot_survive_a_title_with_a_paren_still_round_trips() {
    let dir = repo_with(&[
        ("docs/A.md", "[r](Roadmap.md \"a)b\") end\n"),
        ("docs/Roadmap.md", "# Roadmap\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(
        read(&dir, "docs/A.md"),
        "[r](../plans/Roadmap.md \"a)b\") end\n"
    );
}
