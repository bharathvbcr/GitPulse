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
    assert!(sandbox_write_expecting(path, "a.md", Some("one\n"), "two\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // Stale expectation: nothing is written.
    assert!(!sandbox_write_expecting(path, "a.md", Some("one\n"), "three\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "two\n");
    // A file that is gone is not "the bytes I expected" either.
    assert!(!sandbox_write_expecting(path, "gone.md", Some("one\n"), "three\n").unwrap());
    assert!(!dir.path().join("gone.md").exists());
    // No expectation is `sandbox_write`: written whatever is there.
    assert!(sandbox_write_expecting(path, "a.md", None, "four\n").unwrap());
    assert_eq!(read(&dir, "a.md"), "four\n");
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

// ---------------------------------------------------------------------------
// Adversarial cases (second-pass tester). Each test states what it attacks.
// ---------------------------------------------------------------------------

/// xorshift64*: a deterministic generator, so a failing seed replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const ADV_DIRS: &[&str] = &[
    "docs",
    "a b",
    "(d)",
    "é",
    "..x",
    "x..",
    "a..b",
    ".dot",
    "emoji😀",
    "dir'q",
];
// Not generated here, each for a reason of its own:
//   `#` in a name: the vault cannot resolve such a link at all (it reads the
//     `#` as an anchor even when percent-escaped), so there is nothing to compare;
//     see `adversarial_a_destination_with_a_hash_...`.
//   NFD names (`e\u{301}`): git precomposes them on macOS, so the vault's name is
//     not the generated one; see `adversarial_a_decomposed_spelling_...`.
const ADV_NAMES: &[&str] = &[
    "a.md",
    "b c.md",
    "(p).md",
    "50%.md",
    "q?.md",
    "é.md",
    "😀.md",
    ".hid.md",
    "a..b.md",
    "x.mdx",
    "100%41.md",
    "UP.md",
    "[br].md",
    "a'b.md",
    "a\"b.md",
];

fn adv_key(path: &str) -> String {
    let stem = path
        .strip_suffix(".mdx")
        .or_else(|| path.strip_suffix(".md"))
        .unwrap_or(path);
    stem.to_lowercase()
}

fn adv_path(rng: &mut Rng) -> String {
    let depth = rng.below(5);
    let mut parts: Vec<&str> = (0..depth).map(|_| rng.pick(ADV_DIRS)).collect();
    parts.push(rng.pick(ADV_NAMES));
    parts.join("/")
}

/// `target` seen from the folder `dir`, built independently of the code under test.
fn adv_relative(dir: &str, target: &str) -> String {
    let from: Vec<&str> = dir.split('/').filter(|c| !c.is_empty()).collect();
    let parts: Vec<&str> = target.split('/').collect();
    let (file, down) = parts.split_last().unwrap();
    let mut common = 0;
    while common < from.len() && common < down.len() && from[common] == down[common] {
        common += 1;
    }
    let mut out = "../".repeat(from.len() - common);
    for d in &down[common..] {
        out.push_str(d);
        out.push('/');
    }
    out.push_str(file);
    out
}

/// What a careful author would type: percent-escape (or bracket) what CommonMark
/// would otherwise misread.
fn adv_spell(rng: &mut Rng, linking: &str, target: &str) -> String {
    let dir = linking.rsplit_once('/').map_or("", |(d, _)| d);
    let rooted = rng.chance(15);
    let mut path = if rooted {
        format!("/{target}")
    } else {
        adv_relative(dir, target)
    };
    if !rooted && !path.starts_with("../") && rng.chance(25) {
        path = format!("./{path}");
    }
    if target.ends_with(".md") && rng.chance(30) {
        path.truncate(path.len() - 3);
    }
    let bracketed = rng.chance(30);
    let mut out = String::new();
    for c in path.chars() {
        let always = matches!(c, '(' | ')' | '<' | '>' | '#' | '%');
        let space = c == ' ' && !bracketed;
        if always || space || (c == '?' && rng.chance(50)) {
            out.push_str(&format!("%{:02X}", c as u8));
        } else {
            out.push(c);
        }
    }
    let mut link = if bracketed { format!("<{out}>") } else { out };
    if rng.chance(25) {
        link.push_str(rng.pick(&["#sec", "#", "#h1"]));
    }
    if rng.chance(20) {
        link.push_str(rng.pick(&[" \"t\"", " 't'", " \"a(b\""]));
    }
    link
}

/// Many random vaults; for each a random note moves to a random place.
/// Property: every markdown link resolves to the note it resolved to before
/// (the moved note under its new path), or the outcome reports it in `left_broken`.
#[test]
fn adversarial_random_vaults_keep_every_link_or_report_it() {
    let mut failures: Vec<String> = Vec::new();
    let (mut resolved_links, mut total_links, mut silent, mut reported) = (0, 0, 0, 0);
    let mut skipped_by_git = 0usize;
    for seed in 1..=120u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut paths: Vec<String> = Vec::new();
        let mut keys = std::collections::HashSet::new();
        while paths.len() < 6 {
            let p = adv_path(&mut rng);
            if keys.insert(adv_key(&p)) {
                paths.push(p);
            }
        }
        let mut to = adv_path(&mut rng);
        while keys.contains(&adv_key(&to)) {
            to = adv_path(&mut rng);
        }
        let from = paths[rng.below(paths.len())].clone();
        let mut files: Vec<(String, String)> = Vec::new();
        for p in &paths {
            let mut text = String::from("# H\n\n## sec\n\n");
            for _ in 0..(3 + rng.below(4)) {
                let target = paths[rng.below(paths.len())].clone();
                let spelled = adv_spell(&mut rng, p, &target);
                text.push_str(&format!("- [l]({spelled})\n"));
            }
            files.push((p.clone(), text));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let dir = repo_with(&refs);
        let path = dir.path().to_str().unwrap();
        let before = load_vault(path, true).unwrap();
        let mut expected: Vec<(String, usize, Option<String>)> = Vec::new();
        for note in before.vault.notes() {
            for (position, link) in note.links.iter().enumerate() {
                let landed = before.vault.resolve_link(&note.path, link).map(|r| {
                    if r.path == from {
                        to.clone()
                    } else {
                        r.path
                    }
                });
                total_links += 1;
                resolved_links += landed.is_some() as usize;
                let source = if note.path == from {
                    to.clone()
                } else {
                    note.path.clone()
                };
                expected.push((source, position, landed));
            }
        }
        let held: std::collections::HashSet<&str> = before
            .vault
            .notes()
            .iter()
            .map(|n| n.path.as_str())
            .collect();
        if !paths.iter().all(|p| held.contains(p.as_str())) {
            // git precomposes Unicode on macOS, so an NFD spelling is stored as
            // NFC and the generated name is not the vault's name: not a rename case.
            skipped_by_git += 1;
            total_links -= expected.len();
            resolved_links -= expected.iter().filter(|e| e.2.is_some()).count();
            continue;
        }
        let outcome = match rename_doc(path, &from, &to, |_| Ok(())) {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!("seed {seed}: {from} -> {to}: rename refused: {e}"));
                continue;
            }
        };
        let after = refresh_and_load(path);
        let mut bad = Vec::new();
        for (source, position, landed) in &expected {
            let Some(note) = after.vault.note(source) else {
                bad.push(format!("{source} missing"));
                continue;
            };
            let Some(link) = note.links.get(*position) else {
                bad.push(format!("{source} lost link {position}"));
                continue;
            };
            let now = after.vault.resolve_link(source, link).map(|r| r.path);
            // A link the vault could not follow before (a literal `%41` in a note
            // name is decoded twice by its index) is not one the rename can break.
            if landed.is_some() && now != *landed {
                bad.push(format!(
                    "{source}#{position} written {:?}: {landed:?} became {now:?}",
                    link.target
                ));
            }
        }
        if !bad.is_empty() {
            if !outcome.left_broken.is_empty() {
                // Reported to the caller: the outcome did its job. (Every such
                // case here is a new name holding a literal `%41`, which the
                // vault's index decodes twice and so cannot follow.)
                reported += 1;
                continue;
            }
            silent += 1;
            failures.push(format!(
                "seed {seed}: {from} -> {to}\n    left_broken={:?}\n    {}\n    texts (path, before, after): {:?}",
                outcome.left_broken,
                bad.join("\n    "),
                files
                    .iter()
                    .map(|(p, t)| {
                        let now = if *p == from { &to } else { p };
                        (p.clone(), t.clone(), std::fs::read_to_string(dir.path().join(now)).ok())
                    })
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        skipped_by_git < 80,
        "git renamed too many generated paths: {skipped_by_git}"
    );
    assert!(
        resolved_links * 2 > total_links,
        "generator is not producing resolvable links: {resolved_links}/{total_links}"
    );
    assert!(
        failures.is_empty(),
        "{} of 120 vaults broke a link ({silent} with NOTHING in left_broken, {reported} reported); first 6:\n{}",
        failures.len(),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn adversarial_a_destination_with_a_hash_is_escaped_so_it_is_not_read_as_an_anchor() {
    let dir = repo_with(&[
        ("docs/A.md", "[r](Roadmap.md)\n"),
        ("docs/Roadmap.md", "# R\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/Roadmap.md", "docs/C#1.md", |_| Ok(())).expect("rename");
    // The vault does not see this link afterwards (it parses no link for it, and
    // reports none broken), so the outcome stays silent. Any markdown reader
    // follows `C#1.md` as the page `C` at anchor `1.md`.
    assert_eq!(read(&dir, "docs/A.md"), "[r](C%231.md)\n", "{outcome:?}");
}

#[test]
fn adversarial_a_decomposed_spelling_of_a_composed_note_name_resolves_the_same() {
    // The file is stored precomposed; the author typed the decomposed form.
    let dir = repo_with(&[
        ("docs/\u{e9}.md", "# e acute\n"),
        ("docs/L.md", "[x](e\u{301}.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let before = load_vault(path, true).unwrap();
    let note = before.vault.note("docs/L.md").unwrap();
    let resolves = before
        .vault
        .resolve_link("docs/L.md", &note.links[0])
        .is_some();
    let outcome =
        rename_doc(path, "docs/\u{e9}.md", "notes/\u{e9}.md", |_| Ok(())).expect("rename");
    let after = refresh_and_load(path);
    let note = after.vault.note("docs/L.md").unwrap();
    let now = after
        .vault
        .resolve_link("docs/L.md", &note.links[0])
        .is_some();
    assert_eq!(resolves, now, "{outcome:?} {:?}", read(&dir, "docs/L.md"));
}

#[test]
fn adversarial_a_wikilink_to_a_target_with_a_hash_or_pipe_is_not_left_silently_broken() {
    for to in ["docs/Plan#1.md", "docs/Plan|x.md", "docs/Plan]].md"] {
        let dir = repo_with(&[
            ("docs/A.md", "[[docs/Roadmap]] and [[Roadmap]]\n"),
            ("docs/Roadmap.md", "# R\n"),
        ]);
        let path = dir.path().to_str().unwrap();
        let outcome = rename_doc(path, "docs/Roadmap.md", to, |_| Ok(())).expect("rename");
        let after = refresh_and_load(path);
        let broken = after.vault.broken_links();
        assert!(
            broken.is_empty() || !outcome.left_broken.is_empty(),
            "{to}: wikilinks broke silently: broken={broken:?} outcome={outcome:?} text={:?}",
            read(&dir, "docs/A.md")
        );
    }
}

fn adv_unchanged(files: &[(&str, &str)], from: &str, to: &str, untouched: &[&str]) {
    let dir = repo_with(files);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, from, to, |_| Ok(())).expect("rename");
    for name in untouched {
        let original = files.iter().find(|(p, _)| p == name).unwrap().1;
        assert_eq!(read(&dir, name), original, "{name} must be byte-identical");
    }
}

#[test]
fn adversarial_links_that_are_not_links_are_left_alone() {
    let guide = "# G\n\n\
        ```\n[f](Roadmap.md)\n```\n\n\
        ~~~md\n[t](Roadmap.md)\n~~~\n\n\
        Inline `[i](Roadmap.md)` code and ``[j](Roadmap.md)`` too.\n\n\
        <a href=\"Roadmap.md\">html</a> <https://x.test/Roadmap.md> <Roadmap.md>\n\n\
        \x20   [indented](Roadmap.md)\n";
    adv_unchanged(
        &[("docs/G.md", guide), ("docs/Roadmap.md", "# R\n")],
        "docs/Roadmap.md",
        "plans/Roadmap.md",
        &["docs/G.md"],
    );
}

#[test]
fn adversarial_front_matter_links_are_not_rewritten_in_place_of_data() {
    // Front matter is YAML: a `](` in a string is not a markdown link.
    let note = "---\ntitle: \"see [x](Roadmap.md)\"\nrelated: Roadmap.md\n---\n# G\n";
    adv_unchanged(
        &[("docs/G.md", note), ("docs/Roadmap.md", "# R\n")],
        "docs/Roadmap.md",
        "plans/Roadmap.md",
        &["docs/G.md"],
    );
}

#[test]
fn adversarial_an_image_whose_source_is_a_note_follows_the_note_or_is_left_alone_consistently() {
    let dir = repo_with(&[
        ("docs/G.md", "![x](Roadmap.md) ![y](pic.png)\n"),
        ("docs/pic.png", "png"),
        ("docs/Roadmap.md", "# R\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    let g = read(&dir, "docs/G.md");
    // A non-note image is never touched.
    assert!(g.contains("![y](pic.png)"), "{g}");
    // The image-of-a-note is either rewritten to resolve, or left exactly.
    assert!(
        g.contains("![x](../plans/Roadmap.md)") || g.contains("![x](Roadmap.md)"),
        "{g}"
    );
}

#[test]
fn adversarial_the_moved_notes_non_note_assets_are_a_silent_residual() {
    // `pic.png` next to the moved note stops resolving after the move. The brief
    // lists this as out of scope; this pins what the outcome says about it.
    let dir = repo_with(&[
        ("docs/A.md", "![y](pic.png) [d](data.csv)\n"),
        ("docs/pic.png", "png"),
        ("docs/data.csv", "1"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "notes/A.md"), "![y](pic.png) [d](data.csv)\n");
    assert!(
        outcome.left_broken.is_empty(),
        "asset links are invisible to the vault, so the outcome cannot warn: {outcome:?}"
    );
}

#[test]
fn adversarial_moves_into_and_out_of_the_notes_own_subfolder_keep_links() {
    let files = [
        (
            "docs/A.md",
            "[b](B.md) [s](sub/S.md) [self](A.md) [u](../Top.md) [r](/docs/B.md)\n",
        ),
        ("docs/B.md", "[a](A.md) [a2](./A.md#h) <A.md>\n"),
        ("docs/sub/S.md", "[a](../A.md) [b](../B.md)\n"),
        ("Top.md", "[a](docs/A.md)\n"),
    ];
    for (from, to) in [
        ("docs/A.md", "docs/sub/A.md"),
        ("docs/sub/S.md", "docs/S.md"),
        ("docs/A.md", "A.md"),
        ("Top.md", "docs/Top.md"),
    ] {
        let dir = repo_with(&files);
        assert_rename_preserves_every_link(&dir, from, to);
    }
}

#[test]
fn adversarial_there_and_back_again_restores_every_link_byte() {
    let files = [
        (
            "docs/A.md",
            "[b](B.md) [s](sub/S.md) [self](A.md#h) [u](../Top.md)\n",
        ),
        ("docs/B.md", "[a](A.md) [a2](A.md#h)\n"),
        ("docs/sub/S.md", "[a](../A.md)\n"),
        ("Top.md", "[a](docs/A.md)\n"),
    ];
    let dir = repo_with(&files);
    let path = dir.path().to_str().unwrap();
    rename_doc(path, "docs/A.md", "x/y/A.md", |_| Ok(())).expect("there");
    rename_doc(path, "x/y/A.md", "docs/A.md", |_| Ok(())).expect("back");
    for (name, text) in files {
        assert_eq!(read(&dir, name), text, "{name} did not come back");
    }
}

#[test]
fn adversarial_two_notes_linking_each_other_when_one_moves() {
    let dir = repo_with(&[
        ("a/A.md", "[b](../b/B.md) [self](A.md)\n"),
        ("b/B.md", "[a](../a/A.md#x) [b](B.md)\n"),
    ]);
    assert_rename_preserves_every_link(&dir, "a/A.md", "b/deeper/A2.md");
}

#[test]
fn adversarial_a_same_named_sibling_in_another_folder_is_the_one_each_link_reached() {
    let files = [
        ("docs/A.md", "[s](Sib.md) [t](../notes/Sib.md) [u](Sib)\n"),
        ("docs/Sib.md", "# docs sib\n"),
        ("notes/Sib.md", "# notes sib\n"),
    ];
    for to in ["notes/A.md", "other/A.md", "docs/sub/A.md"] {
        let dir = repo_with(&files);
        assert_rename_preserves_every_link(&dir, "docs/A.md", to);
    }
}

#[test]
fn adversarial_moving_onto_a_name_that_already_has_a_sibling_stem_keeps_links() {
    // `docs/B.mdx` exists; the moved note becomes `docs/B.md`: both read as `docs/b`.
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/B.mdx", "# B mdx\n"),
        ("docs/L.md", "[a](A.md) [b](B.mdx) [b2](B)\n"),
    ]);
    // The vault reads `B.md` and `B.mdx` as one name, so the move would retarget
    // every link written for the other note: it is refused, and nothing moves.
    let path = dir.path().to_str().unwrap();
    let refused = rename_doc(path, "docs/A.md", "docs/B.md", |_| Ok(())).unwrap_err();
    assert!(
        refused.contains("shares its link name") && refused.contains("docs/B.mdx"),
        "{refused}"
    );
    assert!(dir.path().join("docs/A.md").exists());
    assert!(!dir.path().join("docs/B.md").exists());
    assert_eq!(read(&dir, "docs/L.md"), "[a](A.md) [b](B.mdx) [b2](B)\n");
    // A different stem, or the same note changing its own extension, is fine.
    rename_doc(path, "docs/A.md", "docs/C.mdx", |_| Ok(())).expect("a free name");
}

#[test]
fn a_sibling_whose_own_name_holds_a_percent_escape_still_blocks_the_move() {
    // `docs/100%41.mdx` is filed under `docs/100%41`; as a link destination
    // `100%41` would decode to `100a` and miss it, so the check compares the
    // vault's own key.
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/100%41.mdx", "# sibling\n"),
        ("docs/L.md", "[[docs/100%41]] and [[A]]\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let refused = rename_doc(path, "docs/A.md", "docs/100%41.md", |_| Ok(())).unwrap_err();
    assert!(
        refused.contains("shares its link name") && refused.contains("docs/100%41.mdx"),
        "{refused}"
    );
    assert!(dir.path().join("docs/A.md").exists());
    assert_eq!(read(&dir, "docs/L.md"), "[[docs/100%41]] and [[A]]\n");
}

#[test]
fn adversarial_a_previously_broken_link_is_not_blamed_on_the_rename() {
    let dir = repo_with(&[
        ("docs/A.md", "# A\n"),
        ("docs/L.md", "[gone](Gone.md) [a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "docs/A.md", "docs/Gone.md", |_| Ok(())).expect("rename");
    assert!(outcome.left_broken.is_empty(), "{outcome:?}");
}

#[test]
fn adversarial_an_untracked_note_that_links_at_the_moved_note_is_reported() {
    let dir = repo_with(&[
        ("docs/Roadmap.md", "# R\n"),
        ("docs/Tracked.md", "[r](Roadmap.md)\n"),
    ]);
    write(dir.path(), "docs/Untracked.md", "[r](Roadmap.md)\n");
    let path = dir.path().to_str().unwrap();
    let outcome =
        rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(())).expect("rename");
    assert_eq!(read(&dir, "docs/Untracked.md"), "[r](Roadmap.md)\n");
    // The stale link in the note nobody scanned must show up somewhere: either
    // it is counted as not scanned, or it is in left_broken.
    assert!(
        outcome.scan.not_scanned > 0 || !outcome.left_broken.is_empty(),
        "an untracked note was silently left pointing at the old path: {outcome:?}"
    );
}

// ---- concurrency ---------------------------------------------------------

#[test]
fn adversarial_the_moved_note_edited_while_the_gate_runs_keeps_the_users_bytes() {
    let dir = repo_with(&[
        ("docs/A.md", "[b](B.md)\n"),
        ("docs/B.md", "# B\n"),
        ("docs/C.md", "[a](A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let a = dir.path().join("docs/A.md");
    let outcome = rename_doc(path, "docs/A.md", "notes/A.md", |_| {
        std::fs::write(&a, "# typed meanwhile [b](B.md)\n").unwrap();
        Ok(())
    })
    .expect("rename");
    assert_eq!(read(&dir, "notes/A.md"), "# typed meanwhile [b](B.md)\n");
    assert_eq!(outcome.skipped_conflict, ["notes/A.md"], "{outcome:?}");
    assert!(!outcome.complete);
}

#[test]
fn adversarial_a_note_deleted_while_the_gate_runs_is_not_resurrected() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::remove_file(&guide).unwrap();
        Ok(())
    })
    .expect("rename");
    assert!(!guide.exists(), "the rewrite recreated a deleted note");
    assert_eq!(outcome.skipped_conflict, ["docs/Guide.md"], "{outcome:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_a_note_swapped_for_a_symlink_is_never_written_through() {
    let dir = nested_fixture_repo();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n").unwrap();
    let path = dir.path().to_str().unwrap();
    let guide = dir.path().join("docs/Guide.md");
    let outcome = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::remove_file(&guide).unwrap();
        std::os::unix::fs::symlink(&secret, &guide).unwrap();
        Ok(())
    })
    .expect("rename");
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n",
        "wrote through a symlink: {outcome:?}"
    );
    assert!(!outcome.complete, "{outcome:?}");
}

#[test]
fn adversarial_a_held_index_lock_refuses_the_rename_before_anything_moves() {
    let dir = nested_fixture_repo();
    let path = dir.path().to_str().unwrap();
    let lock = dir.path().join(".git/index.lock");
    let result = rename_doc(path, "docs/Roadmap.md", "plans/Roadmap.md", |_| {
        std::fs::write(&lock, "").unwrap();
        Ok(())
    });
    std::fs::remove_file(&lock).ok();
    assert!(result.is_err(), "{result:?}");
    assert!(dir.path().join("docs/Roadmap.md").exists());
    // Known limit: `GitWriter::move_path` creates the destination's parent
    // directory before it runs `git mv`, so a refused move leaves an empty
    // `plans/` behind. Nothing else is left: no file moved or written there.
    // Follow-up (orchestrator): create the parent only after the move succeeds.
    let leftover: Vec<_> = std::fs::read_dir(dir.path().join("plans"))
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "files left behind: {leftover:?}");
    assert_eq!(
        read(&dir, "docs/Guide.md"),
        "# Guide\n\nSee [[Roadmap]] and [x](Roadmap.md).\n"
    );
}

#[test]
fn adversarial_a_lock_taken_after_the_move_is_reported_per_file_not_as_an_error() {
    // Racy by construction: a watcher thread takes the index lock the moment
    // the destination appears. Whatever interleaving happens, the outcome
    // must tell the truth about what is on disk and in the index.
    let dir = crowded_repo();
    let path = dir.path().to_str().unwrap().to_string();
    let to = dir.path().join("plans/Roadmap.md");
    let lock = dir.path().join(".git/index.lock");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let (lock2, to2) = (lock.clone(), to.clone());
    let watcher = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            if to2.exists() {
                let _ = std::fs::write(&lock2, "");
                return;
            }
        }
    });
    let outcome = rename_doc(&path, "docs/Roadmap.md", "plans/Roadmap.md", |_| Ok(()));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
    std::fs::remove_file(&lock).ok();
    let outcome = outcome.expect("the move happened, so this is a reported outcome");
    for edit in &outcome.failed {
        assert!(edit.error.contains("not staged"), "{outcome:?}");
        assert!(
            read(&dir, &edit.path).contains("plans/Roadmap.md"),
            "{}",
            edit.path
        );
    }
    assert_eq!(
        outcome.complete,
        outcome.failed.is_empty() && outcome.skipped_conflict.is_empty()
    );
}

#[test]
fn adversarial_destinations_outside_the_repository_are_refused() {
    let mut accepted = Vec::new();
    for to in [
        "../Out.md",
        ".git/hooks/x.md",
        ".git/x.md",
        ".GIT/x.md",
        "docs/Guide.md/x.md",
        "/abs/x.md",
        "a/../../x.md",
        "a/./b/../../../x.md",
    ] {
        let fresh = nested_fixture_repo();
        let path = fresh.path().to_str().unwrap();
        if rename_doc(path, "docs/Roadmap.md", to, |_| Ok(())).is_ok() {
            accepted.push(to);
        }
    }
    assert!(accepted.is_empty(), "destinations accepted: {accepted:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_a_destination_through_a_symlinked_folder_does_not_leave_the_repository() {
    let dir = nested_fixture_repo();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let path = dir.path().to_str().unwrap();
    let r = rename_doc(path, "docs/Roadmap.md", "escape/Roadmap.md", |_| Ok(()));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(dir.path().join("docs/Roadmap.md").exists());
}

// ---- truncation ----------------------------------------------------------

fn numbered_repo(count: usize) -> tempfile::TempDir {
    let dir = git_repo();
    for n in 0..count {
        write(dir.path(), &format!("n{n:02}.md"), "[t](n00.md)\n");
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-qm", "n"]);
    dir
}

#[test]
fn adversarial_the_cap_is_exact_not_off_by_one() {
    for (count, truncated, hidden) in [(3usize, false, 0u32), (4, true, 1)] {
        let dir = numbered_repo(count);
        let path = dir.path().to_str().unwrap();
        probe::set_max_doc_files(Some(3));
        let outcome = rename_doc(path, "n00.md", "z.md", |_| Ok(()));
        probe::set_max_doc_files(None);
        let outcome = outcome.expect("rename");
        assert_eq!(
            outcome.scan.truncated, truncated,
            "count {count}: {outcome:?}"
        );
        assert_eq!(
            outcome.scan.not_scanned, hidden,
            "count {count}: {outcome:?}"
        );
        assert_eq!(outcome.scan.note_count as usize + hidden as usize, count);
    }
}

#[test]
fn adversarial_a_note_past_the_cap_cannot_be_the_one_renamed() {
    let dir = numbered_repo(5);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let r = rename_doc(path, "n04.md", "z.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    assert!(r.is_err(), "{r:?}");
    assert!(dir.path().join("n04.md").exists());
}

#[test]
fn adversarial_an_unread_destination_hidden_by_the_cap_is_still_refused() {
    let dir = numbered_repo(5);
    let path = dir.path().to_str().unwrap();
    probe::set_max_doc_files(Some(2));
    let r = rename_doc(path, "n00.md", "n04.md", |_| Ok(()));
    probe::set_max_doc_files(None);
    assert!(r.is_err(), "{r:?}");
}

#[cfg(unix)]
#[test]
fn adversarial_an_unreadable_note_is_counted_and_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("locked.md", "[b](b.md)\n"),
    ]);
    let locked = dir.path().join("locked.md");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(()));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    let outcome = outcome.expect("rename");
    assert_eq!(outcome.scan.skipped_unreadable, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(read(&dir, "locked.md"), "[b](b.md)\n");
}

#[test]
fn adversarial_a_tracked_note_missing_from_disk_is_counted_not_fatal() {
    let dir = repo_with(&[
        ("a.md", "[b](b.md)\n"),
        ("b.md", "# B\n"),
        ("gone.md", "[b](b.md)\n"),
    ]);
    std::fs::remove_file(dir.path().join("gone.md")).unwrap();
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
}

#[test]
fn adversarial_an_oversized_note_is_counted_and_untouched() {
    let big = format!("[b](b.md)\n{}", "x".repeat(DEFAULT_MAX_NOTE_BYTES + 1));
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n"), ("big.md", &big)]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(outcome.scan.skipped_oversized, 1, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(read(&dir, "big.md"), big);
}

#[test]
fn adversarial_the_byte_budget_overflow_is_counted_as_not_scanned() {
    let body = format!("[b](b.md)\n{}", "x".repeat(12 * 1024 * 1024));
    let dir = repo_with(&[
        ("b.md", "# B\n"),
        ("c1.md", &body),
        ("c2.md", &body),
        ("c3.md", &body),
    ]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert!(outcome.scan.truncated, "{outcome:?}");
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
    assert_eq!(outcome.scan.note_count, 3, "{outcome:?}");
}

#[test]
fn adversarial_a_non_utf8_note_with_a_link_shaped_byte_run_is_never_rewritten() {
    let raw: Vec<u8> = b"\xff[b](b.md)\n".to_vec();
    let dir = repo_with(&[("a.md", "[b](b.md)\n"), ("b.md", "# B\n")]);
    std::fs::write(dir.path().join("latin.md"), &raw).unwrap();
    git_in(dir.path(), &["add", "latin.md"]);
    let path = dir.path().to_str().unwrap();
    let outcome = rename_doc(path, "b.md", "z.md", |_| Ok(())).expect("rename");
    assert_eq!(std::fs::read(dir.path().join("latin.md")).unwrap(), raw);
    assert_eq!(outcome.scan.not_scanned, 1, "{outcome:?}");
}

// ---- case-insensitive filesystems ------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn adversarial_a_rename_that_differs_from_its_folder_only_in_case_keeps_links() {
    let dir = repo_with(&[
        ("Docs/A.md", "[o](Other.md) [s](A.md)\n"),
        ("Docs/Other.md", "[a](A.md) [a2](./A.md)\n"),
        ("README.md", "[a](Docs/A.md)\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    let insensitive = dir.path().join("docs/A.md").exists();
    let outcome = rename_doc(path, "Docs/A.md", "docs/B.md", |_| Ok(()));
    if insensitive {
        // The two spellings are one folder: a plain rename inside it must keep
        // every link landing, or refuse cleanly.
        match outcome {
            Ok(outcome) => {
                let after = refresh_and_load(path);
                assert!(
                    after.vault.broken_links().is_empty(),
                    "{:?} {outcome:?}",
                    after.vault.broken_links()
                );
            }
            Err(_) => assert!(dir.path().join("Docs/A.md").exists()),
        }
    } else {
        outcome.expect("rename");
    }
}
