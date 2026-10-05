use super::*;

/// The shape of the nine briefs that motivated this reader: frontmatter
/// first, then a `# Task brief v1` body that repeats the metadata.
const AGENT_BRIEF: &str = r#"---
id: "gp-attention-lse-return"
title: "Return LogSumExp (LSE) from Forward Attention to Accelerate Backward Pass"
status: ready
priority: 1
severity: medium
type: refactor
owner: "unassigned"
due: "none"
labels:
  - "attention"
  - "performance"
repositories:
  - "ojas"
planned_files:
  - "ojas-core/src/backend.rs"
  - "ojas-autograd/src/tape.rs"
acceptance_criteria:
  - "Update Backend::causal_sdpa_forward trait signature to return (Tensor, Tensor)"
  - "Verify bit-level and numerical equivalence with existing backward test suites"
---

# Task brief v1

## Title
Return LogSumExp (LSE) from Forward Attention to Accelerate Backward Pass

Task: gp-attention-lse-return
Type: refactor
Status: ready
Priority: 1 (High)
Severity: medium
Owner: unassigned
Due: none
Labels: attention, performance

## Repositories
- ojas

## Description
Backend::causal_sdpa_forward currently returns only the activation output.

This recomputation wastes an estimated 10%–25% of backward GPU time.

## Acceptance criteria
- [ ] Update Backend::causal_sdpa_forward trait signature to return (Tensor, Tensor)
- [ ] Verify bit-level and numerical equivalence with existing backward test suites

## Planned files
- ojas-core/src/backend.rs
- ojas-autograd/src/tape.rs
"#;

#[test]
fn reads_an_agent_written_brief_in_full() {
    let brief = parse_brief(AGENT_BRIEF).unwrap();
    assert_eq!(brief.key.as_deref(), Some("gp-attention-lse-return"));
    assert_eq!(
        brief.title,
        "Return LogSumExp (LSE) from Forward Attention to Accelerate Backward Pass"
    );
    assert_eq!(brief.status.as_deref(), Some("ready"));
    assert_eq!(brief.priority, Some(1));
    assert_eq!(brief.severity.as_deref(), Some("medium"));
    assert_eq!(brief.kind.as_deref(), Some("refactor"));
    assert_eq!(brief.owner, None, "`unassigned` is the absence of an owner");
    assert_eq!(brief.due, None);
    assert_eq!(brief.labels, ["attention", "performance"]);
    assert_eq!(brief.repositories, ["ojas"]);
    assert_eq!(
        brief.planned_files,
        ["ojas-core/src/backend.rs", "ojas-autograd/src/tape.rs"]
    );
    assert_eq!(brief.acceptance_criteria.len(), 2);
    assert_eq!(
        brief.description,
        "Backend::causal_sdpa_forward currently returns only the activation output.\n\nThis recomputation wastes an estimated 10%–25% of backward GPU time."
    );
    assert_eq!(brief.logs, None);
}

#[test]
fn reads_the_board_export_shape_without_frontmatter() {
    let exported = r#"# Task brief v1

## Title
Fix crash on startup

Task: gp-crash-fix (revision 4)
Updated (Unix seconds): 1774900000
Type: bug
Status: In Progress
Severity: critical
Owner: @charlie
Priority: 0 (Urgent)
Due (Unix seconds): 1775000000
Labels: stability, core
Enhancement field locks: none

## Repositories
- GitPulse [repo-1] (revision 2) — primary

Home workspace: Default

## Description
Application crashes when reading corrupt cache file.

## Acceptance criteria
- [ ] Reproduce crash with fixture
- [x] Add defensive error handling

## Raw logs
Pasted evidence. Keep stack frames, timestamps, error codes and quoted text exactly as written.

````
thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'
```
   src/cache.rs:42:10
````
Trailing commentary is not evidence.
"#;
    let brief = parse_brief(exported).unwrap();
    assert_eq!(brief.key.as_deref(), Some("gp-crash-fix"));
    assert_eq!(brief.status.as_deref(), Some("in_progress"));
    assert_eq!(brief.priority, Some(0));
    assert_eq!(brief.owner.as_deref(), Some("@charlie"));
    assert_eq!(brief.due.as_deref(), Some("1775000000"));
    assert_eq!(brief.labels, ["stability", "core"]);
    assert_eq!(brief.repositories, ["GitPulse"]);
    assert_eq!(
        brief.acceptance_criteria,
        [
            "Reproduce crash with fixture",
            "Add defensive error handling"
        ]
    );
    assert_eq!(
        brief.logs.as_deref(),
        Some("thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'\n```\n   src/cache.rs:42:10"),
        "a shorter fence inside a longer one is content, and indentation survives"
    );
    assert_eq!(
        brief.description,
        "Application crashes when reading corrupt cache file."
    );
}

// Pre-fix, the body's copy overwrote the frontmatter: `backlog`, priority 3.
#[test]
fn frontmatter_wins_over_a_stale_body_copy() {
    let brief = parse_brief("---\ntitle: A\nstatus: ready\npriority: 0\n---\n# Task brief v1\nStatus: backlog\nPriority: 3\nOwner: @body\n").unwrap();
    assert_eq!(brief.status.as_deref(), Some("ready"));
    assert_eq!(brief.priority, Some(0));
    assert_eq!(
        brief.owner.as_deref(),
        Some("@body"),
        "the body still fills a gap"
    );
}

// Pre-fix: "Could not extract task title from content".
#[test]
fn a_title_line_with_a_colon_is_still_the_title() {
    let brief = parse_brief("## Title\nPhase 2: ship it\nStatus: ready\n").unwrap();
    assert_eq!(brief.title, "Phase 2: ship it");
    assert_eq!(brief.status.as_deref(), Some("ready"));
}

// Pre-fix the writer emitted `\"` and the reader kept the backslashes.
#[test]
fn quoted_scalars_are_unescaped() {
    let brief =
        parse_brief("---\ntitle: \"Say \\\"hi\\\" \\\\ bye \\u00e9\"\nowner: 'it''s me'\n---\n")
            .unwrap();
    assert_eq!(brief.title, "Say \"hi\" \\ bye é");
    assert_eq!(brief.owner.as_deref(), Some("it's me"));
}

// Pre-fix: `banana` was accepted; `10` and `-1` both became priority 1.
#[test]
fn unknown_or_out_of_range_values_are_errors_not_guesses() {
    for (front, needle) in [
        ("status: banana", "unknown status"),
        ("priority: 10", "out of range"),
        ("priority: -1", "priority"),
        ("priority: soon", "priority"),
        ("severity: catastrophic", "unknown severity"),
        ("title: two\nstatus: ready\nstatus: done", "appears twice"),
        ("labels: [a, [b]]", "nested"),
        ("labels: [a, b", "close with"),
        ("title: \"unterminated", "unterminated"),
        ("  indented: x", "indented"),
        ("description: |\n  block", "block scalars"),
        ("not a pair", "expected `key: value`"),
    ] {
        let text = format!("---\ntitle: T\n{front}\n---\n");
        let text = if front.starts_with("title:") {
            format!("---\n{front}\n---\n")
        } else {
            text
        };
        let error = parse_brief(&text).expect_err(front);
        assert!(error.contains(needle), "{front:?} → {error}");
    }
    assert_eq!(
        parse_brief("---\ntitle: T\npriority: P2\n---\n")
            .unwrap()
            .priority,
        Some(2)
    );
    assert_eq!(
        parse_brief("---\ntitle: T\npriority: urgent\n---\n")
            .unwrap()
            .priority,
        Some(0)
    );
    assert_eq!(
        parse_brief("---\ntitle: T\nstatus: In-Progress\n---\n")
            .unwrap()
            .status
            .as_deref(),
        Some("in_progress")
    );
}

// Pre-fix `\n----- not end` closed the frontmatter.
#[test]
fn only_a_bare_dash_line_closes_frontmatter() {
    let error = parse_brief("---\ntitle: A\n----- not end\n").unwrap_err();
    assert!(
        error.contains("indented or list line") || error.contains("never closed"),
        "{error}"
    );
    let error = parse_brief("---\ntitle: A\nstatus: ready\n").unwrap_err();
    assert!(error.contains("never closed"), "{error}");
    assert_eq!(
        parse_brief("---\ntitle: A\n...\nbody\n").unwrap().title,
        "A"
    );
}

// Pre-fix `validate_title` admitted `\n`, which broke the frontmatter it wrote.
#[test]
fn a_title_is_one_line_within_the_board_limit() {
    assert!(validate_title("line1\nline2").is_err());
    assert!(validate_title("tab\there").is_err());
    assert!(validate_title(&"é".repeat(MAX_TASK_TITLE)).is_ok());
    assert!(validate_title(&"é".repeat(MAX_TASK_TITLE + 1)).is_err());
}

#[test]
fn hand_written_markdown_is_a_brief_too() {
    let brief = parse_brief("# Fix the flaky watcher test\r\n\r\nIt fails one run in ten.\r\n\r\n## Context\r\nSeen on CI only.\r\n\r\n## Checklist\r\n1. Reproduce it\r\n2. Fix the race\r\n   in the debouncer\r\n").unwrap();
    assert_eq!(brief.title, "Fix the flaky watcher test");
    assert!(brief.description.contains("It fails one run in ten."));
    assert!(
        brief.description.contains("## Context\nSeen on CI only."),
        "{}",
        brief.description
    );
    assert_eq!(
        brief.acceptance_criteria,
        ["Reproduce it", "Fix the race in the debouncer"]
    );
    assert_eq!(brief.key, None);
}

#[test]
fn fenced_text_is_never_read_as_headings_or_metadata() {
    let brief = parse_brief("---\ntitle: T\n---\n## Description\n```\n## Acceptance criteria\nStatus: done\n- [ ] not a criterion\n```\n").unwrap();
    assert!(brief.acceptance_criteria.is_empty());
    assert_eq!(brief.status, None);
    assert!(brief.description.contains("Status: done"));
}

#[test]
fn adversarial_inputs_fail_cleanly_and_bounds_hold() {
    assert!(parse_brief("").is_err());
    assert!(parse_brief("   \n\n").is_err());
    assert!(parse_brief("a\0b").is_err());
    assert!(parse_brief("---\n---\nno title").is_err());
    assert!(parse_brief("\u{feff}---\ntitle: BOM\n---\n").is_ok());
    let many: String = (0..=MAX_TASK_LABELS)
        .map(|i| format!("  - l{i}\n"))
        .collect();
    assert!(parse_brief(&format!("---\ntitle: T\nlabels:\n{many}---\n"))
        .unwrap_err()
        .contains("at most"));
    let dupes: String = (0..1000).map(|_| "  - same\n").collect();
    assert_eq!(
        parse_brief(&format!("---\ntitle: T\nlabels:\n{dupes}---\n"))
            .unwrap()
            .labels,
        ["same"]
    );
    assert!(parse_brief(&format!(
        "---\ntitle: T\nid: {}\n---\n",
        "a".repeat(MAX_TASK_KEY + 1)
    ))
    .is_err());
    assert!(parse_brief("---\ntitle: T\nid: ../escape\n---\n").is_err());
    let huge = format!(
        "---\ntitle: T\n---\n## Description\n{}\n",
        "x".repeat(MAX_TASK_DESCRIPTION + 1)
    );
    assert!(parse_brief(&huge).unwrap_err().contains("description"));
    assert!(parse_brief("## Title\nT\n## Raw logs\n```\nnever closed\n")
        .unwrap_err()
        .contains("never closed"));
    // Deep nesting and long lines do not recurse or quadratic-blow.
    let deep = format!("---\ntitle: T\n---\n{}", "#".repeat(60_000));
    assert!(parse_brief(&deep).is_ok());
}

/// Every title that survives validation survives a quoted round trip.
#[test]
fn quoting_round_trips_an_adversarial_corpus() {
    let corpus = [
        "plain",
        "a: b",
        "\"quoted\"",
        "back\\slash",
        "#hash",
        "- dash",
        "---",
        "[bracket]",
        "it's",
        "ü日本語🚀",
        "trailing \\",
        "  padded  ",
        "{brace}",
        "a #b",
        "'single'",
        "\\u0041",
    ];
    for title in corpus {
        let escaped = title.replace('\\', "\\\\").replace('"', "\\\"");
        let brief = parse_brief(&format!(
            "---\ntitle: \"{escaped}\"\nlabels: [\"{escaped}\"]\n---\n"
        ))
        .unwrap();
        assert_eq!(brief.title, title.trim(), "{title:?}");
        assert_eq!(brief.labels, [title.trim()], "{title:?}");
    }
}

fn repo_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
    for (name, body) in files {
        std::fs::write(dir.path().join("tasks").join(name), body).unwrap();
    }
    dir
}

fn scan(dir: &tempfile::TempDir) -> BriefScan {
    scan_briefs(dir.path().to_str().unwrap(), None).unwrap()
}

// Pre-fix: `total: 1`, and bad.md appeared nowhere.
#[test]
fn a_malformed_file_is_reported_by_name_not_dropped() {
    let dir = repo_with(&[
        ("ok.md", b"---\ntitle: A\n---\n"),
        ("bad.md", b"no title here"),
        ("empty.md", b""),
        ("binary.md", &[0xff, 0xfe, 0x00]),
        (".hidden.md", b"---\ntitle: hidden\n---\n"),
        ("notes.txt", b"---\ntitle: not a brief\n---\n"),
        ("UPPER.MD", b"---\ntitle: Upper\n---\n"),
    ]);
    let scan = scan(&dir);
    assert_eq!(scan.found, 5);
    assert_eq!(scan.read, 5);
    let by_file = |name: &str| {
        scan.files
            .iter()
            .find(|f| f.file == format!("tasks/{name}"))
            .unwrap()
    };
    assert_eq!(by_file("ok.md").brief.as_ref().unwrap().title, "A");
    assert_eq!(
        by_file("ok.md").key.as_deref(),
        Some("ok"),
        "the file name keys a brief with no id"
    );
    assert!(by_file("bad.md")
        .error
        .as_deref()
        .unwrap()
        .contains("no title"));
    assert!(by_file("empty.md")
        .error
        .as_deref()
        .unwrap()
        .contains("empty"));
    assert!(by_file("binary.md")
        .error
        .as_deref()
        .unwrap()
        .contains("UTF-8"));
    assert!(by_file("UPPER.MD").brief.is_some());
}

// Pre-fix the scan read a symlink's target wherever it pointed.
#[cfg(unix)]
#[test]
fn symlinks_never_reach_outside_the_repository() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.md"),
        "---\ntitle: OUTSIDE SECRET\n---\n",
    )
    .unwrap();
    let dir = repo_with(&[]);
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        dir.path().join("tasks/leak.md"),
    )
    .unwrap();
    let scan = scan(&dir);
    assert_eq!(scan.files.len(), 1);
    assert!(scan.files[0].brief.is_none());
    assert!(
        scan.files[0]
            .error
            .as_deref()
            .unwrap()
            .contains("symbolic link"),
        "{:?}",
        scan.files[0].error
    );

    // A tasks directory that is itself a link out is refused as a whole.
    let linked = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), linked.path().join("tasks")).unwrap();
    let error = scan_briefs(linked.path().to_str().unwrap(), None).unwrap_err();
    assert!(error.contains("outside the repository"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_fifo_or_a_directory_named_md_is_reported_without_blocking() {
    let dir = repo_with(&[]);
    std::fs::create_dir(dir.path().join("tasks/dir.md")).unwrap();
    let fifo = std::ffi::CString::new(dir.path().join("tasks/pipe.md").to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let scan = scan(&dir);
    assert_eq!(scan.files.len(), 2);
    assert!(scan
        .files
        .iter()
        .all(|f| f.error.as_deref() == Some("is not a regular file")));
}

#[test]
fn oversized_files_and_duplicate_keys_are_reported() {
    let big = vec![b'x'; MAX_BRIEF_BYTES + 1];
    let dir = repo_with(&[
        ("a.md", b"---\nid: same\ntitle: First\n---\n"),
        ("b.md", b"---\nid: same\ntitle: Second\n---\n"),
        ("big.md", &big),
    ]);
    let scan = scan(&dir);
    assert_eq!(
        scan.files
            .iter()
            .find(|f| f.file == "tasks/a.md")
            .unwrap()
            .brief
            .as_ref()
            .unwrap()
            .title,
        "First"
    );
    let b = scan.files.iter().find(|f| f.file == "tasks/b.md").unwrap();
    assert!(
        b.brief.is_none()
            && b.error
                .as_deref()
                .unwrap()
                .contains("already used by tasks/a.md")
    );
    assert!(scan
        .files
        .iter()
        .find(|f| f.file == "tasks/big.md")
        .unwrap()
        .error
        .as_deref()
        .unwrap()
        .contains("KiB"));
}

#[test]
fn a_missing_directory_is_not_an_empty_one() {
    let dir = tempfile::tempdir().unwrap();
    let scan = scan_briefs(dir.path().to_str().unwrap(), None).unwrap();
    assert!(!scan.directory_exists);
    let empty = repo_with(&[]);
    assert!(
        scan_briefs(empty.path().to_str().unwrap(), None)
            .unwrap()
            .directory_exists
    );
}

#[test]
fn a_large_directory_reports_both_numbers() {
    let dir = repo_with(&[]);
    for i in 0..(MAX_BRIEF_FILES + 7) {
        std::fs::write(
            dir.path().join(format!("tasks/t{i:04}.md")),
            format!("---\ntitle: T{i}\n---\n"),
        )
        .unwrap();
    }
    let scan = scan(&dir);
    assert_eq!(scan.found, MAX_BRIEF_FILES + 7);
    assert_eq!(scan.read, MAX_BRIEF_FILES);
    assert!(scan.truncated);
}

#[test]
fn the_tasks_directory_must_stay_inside_the_repository() {
    for bad in ["../etc", "/etc", "a/../../b", "", "a\\b", "./tasks"] {
        assert!(validate_tasks_dir(Some(bad)).is_err(), "{bad}");
    }
    assert!(validate_tasks_dir(Some("docs/tasks")).is_ok());
    assert!(scan_briefs("relative/path", None).is_err());
}

#[test]
fn file_names_that_are_not_keys_still_get_a_stable_one() {
    let dir = repo_with(&[("My Task (draft).md", b"---\ntitle: T\n---\n")]);
    assert_eq!(scan(&dir).files[0].key.as_deref(), Some("my-task-draft"));
}
