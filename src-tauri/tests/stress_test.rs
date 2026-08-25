use gitpulse_lib::analyzer::LocCounter;
use gitpulse_lib::diff::{
    compute_word_diff, ConflictResolver, DiffLineType, FilePatch, PatchBuilder, UnifiedDiffHunk,
    UnifiedDiffLine,
};
use gitpulse_lib::engine::GitWriter;
use gitpulse_lib::graph::{LaneSolver, RawCommitNode, TopologyIndex};
use std::process::Command as StdCommand;
use std::time::Instant;

fn init_repo_with_base_file() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let git = |args: &[&str]| {
        let out = StdCommand::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let _ = StdCommand::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    std::fs::write(dir.path().join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(&["add", "--", "lib.rs"]);
    git(&["commit", "-m", "base"]);
    dir
}

fn addition(content: &str, selected: bool) -> UnifiedDiffLine {
    UnifiedDiffLine {
        line_type: DiffLineType::Addition,
        old_line_no: None,
        new_line_no: Some(2),
        content: content.to_string(),
        is_selected: selected,
    }
}

/// End-to-end: a valid multi-hunk selection flows validate -> build ->
/// `git apply --cached` and lands staged in the index.
#[test]
fn selective_staging_applies_a_valid_patch_to_the_index() {
    let repo = init_repo_with_base_file();
    let file_patch = FilePatch {
        old_path: "lib.rs".to_string(),
        new_path: "lib.rs".to_string(),
        hunks: vec![UnifiedDiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 3,
            header: String::new(),
            lines: vec![
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(1),
                    new_line_no: Some(1),
                    content: "fn a() {}".to_string(),
                    is_selected: false,
                },
                addition("fn staged() {}", true),
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(2),
                    new_line_no: Some(3),
                    content: "fn b() {}".to_string(),
                    is_selected: false,
                },
            ],
        }],
    };
    PatchBuilder::validate_file_patch(&file_patch).expect("patch must validate");
    let patch = PatchBuilder::build_selective_patch(&file_patch, true);
    GitWriter::apply_patch_to_index(repo.path().to_str().unwrap(), &patch)
        .expect("apply patch to index");

    let staged = StdCommand::new("git")
        .args(["show", ":lib.rs"])
        .current_dir(repo.path())
        .output()
        .expect("read index");
    assert!(staged.status.success());
    let text = String::from_utf8_lossy(&staged.stdout);
    assert!(text.contains("fn staged() {}"), "staged: {text}");
    assert!(text.contains("fn a() {}") && text.contains("fn b() {}"));
    // The worktree never saw the addition.
    let worktree = std::fs::read_to_string(repo.path().join("lib.rs")).unwrap();
    assert!(!worktree.contains("fn staged() {}"));
}

/// The command layer's guard rail: hostile FilePatch values are rejected by
/// the builder validation before any git invocation happens.
#[test]
fn selective_staging_rejects_hostile_patches_before_git_runs() {
    let repo = init_repo_with_base_file();
    let mut header_smuggle = addition("fn ok() {}", true);
    header_smuggle.content =
        "fn ok() {}\n--- a/evil.rs\n+++ b/evil.rs\n@@ -0,0 +1,1 @@\n+pwned".into();
    let cases = vec![
        FilePatch {
            old_path: "../../outside.rs".to_string(),
            new_path: "../../outside.rs".to_string(),
            hunks: Vec::new(),
        },
        FilePatch {
            old_path: "/etc/passwd".to_string(),
            new_path: "/etc/passwd".to_string(),
            hunks: Vec::new(),
        },
        FilePatch {
            old_path: "lib.rs".to_string(),
            new_path: "lib.rs".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![header_smuggle],
            }],
        },
    ];
    for case in &cases {
        let err =
            PatchBuilder::validate_file_patch(case).expect_err("hostile patch must be rejected");
        assert!(!err.is_empty());
        // And nothing was staged.
        let out = StdCommand::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(repo.path())
            .output()
            .expect("diff");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    }
}

#[test]
fn test_100k_commit_graph_stress_and_throughput() {
    let mut commits = Vec::with_capacity(100_000);
    for i in (1..=100_000).rev() {
        let id = format!("commit_{:06}", i);
        let parents = if i == 100_000 {
            vec![]
        } else if i % 10 == 0 {
            vec![
                format!("commit_{:06}", i + 1),
                format!("commit_{:06}", i + 2),
            ]
        } else {
            vec![format!("commit_{:06}", i + 1)]
        };

        commits.push(RawCommitNode {
            id,
            parent_ids: parents,
            timestamp: 1700000000 + (100_000 - i) as i64,
            author_name: "Engineer".to_string(),
            author_email: "eng@example.com".to_string(),
            summary: format!("Commit message {}", i),
        });
    }

    let start = Instant::now();
    let mut solver = LaneSolver::new(12);
    let visual_rows = solver.solve(&commits);
    let duration = start.elapsed();

    assert_eq!(visual_rows.len(), 100_000);

    let index = TopologyIndex::build(&visual_rows);
    assert_eq!(index.len(), 100_000);

    let memory_bytes = index.len() * std::mem::size_of::<gitpulse_lib::graph::CommitRowMetadata>();
    assert_eq!(memory_bytes, 1_600_000);

    println!(
        "Processed 100,000 commits in {:?} ({:.0} commits/sec), Index RAM: {:.2} MB",
        duration,
        100_000.0 / duration.as_secs_f64(),
        memory_bytes as f64 / 1_048_576.0
    );

    let slice = index.slice(50_000, 50);
    assert_eq!(slice.len(), 50);
}

#[test]
fn test_extreme_octopus_merge_20_parents() {
    let mut solver = LaneSolver::new(12);
    let mut parents = Vec::new();
    let mut commits = Vec::new();

    for i in 1..=20 {
        parents.push(format!("parent_{:02}", i));
    }

    commits.push(RawCommitNode {
        id: "big_octopus".to_string(),
        parent_ids: parents.clone(),
        timestamp: 2000,
        author_name: "Architect".to_string(),
        author_email: "arch@example.com".to_string(),
        summary: "Octopus merge of 20 branches".to_string(),
    });

    for p in &parents {
        commits.push(RawCommitNode {
            id: p.clone(),
            parent_ids: vec!["root".to_string()],
            timestamp: 1000,
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: format!("Branch {}", p),
        });
    }

    commits.push(RawCommitNode {
        id: "root".to_string(),
        parent_ids: vec![],
        timestamp: 500,
        author_name: "Root".to_string(),
        author_email: "root@example.com".to_string(),
        summary: "Initial commit".to_string(),
    });

    let visual_rows = solver.solve(&commits);
    assert_eq!(visual_rows.len(), 22);
    assert_eq!(visual_rows[0].connections.len(), 20);
}

#[test]
fn test_adversarial_word_diff_inputs() {
    let old_line = "const greeting = 'こんにちは世界 🚀'; // old comment";
    let new_line = "const greeting = 'こんばんは世界 🌟'; // new comment";

    let diff = compute_word_diff(old_line, new_line);
    assert!(!diff.original_segments.is_empty());
    assert!(!diff.modified_segments.is_empty());

    let empty_diff = compute_word_diff("", "new content added");
    assert_eq!(empty_diff.modified_segments[0].text, "new content added");
}

#[test]
fn test_massive_minified_line_diff_protection() {
    let huge_line_1 = "x".repeat(60_000);
    let huge_line_2 = "y".repeat(60_000);

    let start = Instant::now();
    let diff = compute_word_diff(&huge_line_1, &huge_line_2);
    let elapsed = start.elapsed();

    assert!(elapsed.as_millis() < 50);
    assert_eq!(diff.original_segments.len(), 1);
    assert_eq!(diff.modified_segments.len(), 1);
}

#[test]
fn test_extended_lanes_density_over_64() {
    let mut solver = LaneSolver::new(12);
    let mut commits = Vec::new();
    let mut merge_parents = Vec::new();

    // Create 70 distinct parent targets for an octopus merge
    for i in 1..=70 {
        let pid = format!("p_{:03}", i);
        merge_parents.push(pid);
    }

    // Top merge commit pointing to 70 distinct parents simultaneously
    commits.push(RawCommitNode {
        id: "mega_merge".to_string(),
        parent_ids: merge_parents.clone(),
        timestamp: 3000,
        author_name: "Architect".to_string(),
        author_email: "arch@example.com".to_string(),
        summary: "Mega merge 70".to_string(),
    });

    for p in &merge_parents {
        commits.push(RawCommitNode {
            id: p.clone(),
            parent_ids: vec!["root".to_string()],
            timestamp: 1000,
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: format!("Branch {}", p),
        });
    }

    commits.push(RawCommitNode {
        id: "root".to_string(),
        parent_ids: vec![],
        timestamp: 500,
        author_name: "Root".to_string(),
        author_email: "root@example.com".to_string(),
        summary: "Root commit".to_string(),
    });

    let visual_rows = solver.solve(&commits);
    assert_eq!(visual_rows.len(), 72);

    let index = TopologyIndex::build(&visual_rows);
    assert_eq!(index.len(), 72);
    // At row 0 (mega_merge), all 70 parents are allocated discrete lanes, triggering extended lane tracking (>64)
    assert!(
        index.rows[0].has_extended_lanes == 1
            || index
                .extended_lanes
                .iter()
                .any(|(_, lanes)| lanes.iter().any(|&l| l >= 64))
    );
}

#[test]
fn test_conflict_resolver_diff3_with_base() {
    let raw = r#"def calculate():
<<<<<<< HEAD
    return 42 * 2
||||||| merged common ancestors
    return 42
=======
    return 42 * 100
>>>>>>> feature-calc
"#;

    let doc = ConflictResolver::parse("calc.py", raw);
    assert_eq!(doc.total_conflicts, 1);

    if let gitpulse_lib::diff::FileSegment::Conflict(ref chunk) = doc.segments[1] {
        assert_eq!(chunk.base_content.as_deref(), Some("    return 42"));
        assert_eq!(chunk.ours_content, "    return 42 * 2");
        assert_eq!(chunk.theirs_content, "    return 42 * 100");
    }
}

#[test]
fn test_large_file_loc_analyzer() {
    let mut large_code = String::with_capacity(1_000_000);
    for i in 0..20_000 {
        if i % 4 == 0 {
            large_code.push_str("// Comment line\n");
        } else if i % 4 == 1 {
            large_code.push('\n');
        } else {
            large_code.push_str("let x = calculate_val();\n");
        }
    }

    let counts = LocCounter::count(&large_code, Some("//"));
    assert_eq!(counts.total_lines, 20_000);
    assert_eq!(counts.comment_lines, 5_000);
    assert_eq!(counts.blank_lines, 5_000);
    assert_eq!(counts.code_lines, 10_000);
}

#[test]
fn test_ref_name_injection_adversarial_suite() {
    use gitpulse_lib::engine::git_writer::{
        validate_oid, validate_oid_or_revision, validate_ref_name,
    };

    let malicious_refs = vec![
        "-u",
        "--upload-pack=echo pwned",
        "--exec=calc",
        "refs/heads/../../etc/passwd",
        "foo..bar",
        "foo//bar",
        "branch.lock",
        "branch/",
        ".hidden",
        "@",
        "HEAD@{1}",
        "foo\0bar",
        "foo\nbar",
        "foo; rm -rf /",
        "foo`calc`",
        "foo$BAR",
        "foo|bar",
        "foo&bar",
        "foo*bar",
        "foo?bar",
        "foo[bar]",
        "foo:bar",
        "foo^bar",
        "foo~bar",
        "foo\\bar",
    ];

    for bad_ref in malicious_refs {
        assert!(
            validate_ref_name(bad_ref).is_err(),
            "Expected bad ref '{}' to fail validation",
            bad_ref
        );
    }

    let valid_refs = vec![
        "main",
        "master",
        "develop",
        "feature/auth-oauth2",
        "fix/issue-123_test",
        "release/v1.0.0",
        "chore/deps-update",
    ];

    for good_ref in valid_refs {
        assert!(
            validate_ref_name(good_ref).is_ok(),
            "Expected valid ref '{}' to pass validation",
            good_ref
        );
    }

    assert!(validate_oid("4b825dc642cb6eb9a060e54bf8d69288fbee4904").is_ok());
    assert!(validate_oid("4b825dc").is_ok());
    assert!(validate_oid("-u").is_err());
    assert!(validate_oid("4b825dc; rm -rf").is_err());

    assert!(validate_oid_or_revision("HEAD~5").is_ok());
    assert!(validate_oid_or_revision("HEAD^").is_ok());
    assert!(validate_oid_or_revision("main").is_ok());
    assert!(validate_oid_or_revision("-u").is_err());
    assert!(validate_oid_or_revision("HEAD; rm -rf").is_err());
}

#[test]
fn test_corrupt_dag_cycle_and_disconnected_subgraphs() {
    let mut solver = LaneSolver::new(12);

    // Create a cyclic graph: c1 -> c2 -> c1 (simulating corrupt or circular git graph)
    let cyclic_commits = vec![
        RawCommitNode {
            id: "c1".to_string(),
            parent_ids: vec!["c2".to_string()],
            timestamp: 2000,
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: "Commit 1".to_string(),
        },
        RawCommitNode {
            id: "c2".to_string(),
            parent_ids: vec!["c1".to_string()],
            timestamp: 1000,
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            summary: "Commit 2".to_string(),
        },
    ];

    let rows = solver.solve(&cyclic_commits);
    assert_eq!(rows.len(), 2);
    assert!(!rows[0].connections.is_empty());
}

#[test]
fn test_unclosed_conflict_markers_resilience() {
    let broken_conflict = r#"
line 1
<<<<<<< HEAD
ours line
=======
theirs line without end marker
"#;

    let doc = ConflictResolver::parse("broken.txt", broken_conflict);
    // Unclosed markers fallback to normal text segments to prevent data loss
    assert_eq!(doc.total_conflicts, 0);
    let resolved = ConflictResolver::render_resolved(&doc).unwrap();
    assert!(resolved.contains("ours line"));
    assert!(resolved.contains("theirs line without end marker"));
}

#[test]
fn test_stack_tree_cyclic_ancestry_safety() {
    use gitpulse_lib::stack::StackTreeEngine;
    use std::collections::HashMap;

    let mut branch_tips = HashMap::new();
    branch_tips.insert("feat-a".to_string(), "c1".to_string());
    branch_tips.insert("feat-b".to_string(), "c2".to_string());

    let mut commit_parents = HashMap::new();
    // Circular link in commit graph
    commit_parents.insert("c1".to_string(), vec!["c2".to_string()]);
    commit_parents.insert("c2".to_string(), vec!["c1".to_string()]);

    let nodes = StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, "main");
    assert_eq!(nodes.len(), 2);

    let breadcrumbs = StackTreeEngine::get_ancestry_breadcrumbs(&nodes, "feat-a");
    assert!(!breadcrumbs.breadcrumb_chain.is_empty());
}

#[test]
fn test_topology_slice_hostile_window_requests() {
    use gitpulse_lib::graph::VisualCommitRow;

    let rows: Vec<VisualCommitRow> = (0..500)
        .map(|i| VisualCommitRow {
            id: format!("c{i}"),
            parent_ids: vec![],
            summary: "s".to_string(),
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            timestamp: 1700000000 + i as i64,
            lane: 0,
            color_index: 0,
            active_lanes: vec![0, 1],
            active_lane_colors: vec![0, 1],
            connections: vec![],
            is_merge: false,
            is_root: false,
        })
        .collect();
    let index = TopologyIndex::build(&rows);
    assert_eq!(index.len(), 500);

    // Hostile viewport arguments must clamp, never panic or overflow.
    assert!(index.slice(usize::MAX, 1).is_empty());
    assert!(index.slice(50_000, 50).is_empty());
    assert_eq!(index.slice(0, usize::MAX).len(), 500);
    assert_eq!(index.slice(499, usize::MAX).len(), 1);
    assert_eq!(index.slice(250, usize::MAX).len(), 250);

    // Sweep every window boundary, including counts that would overflow
    // `start + count` if the math were unguarded.
    for start in 0..500 {
        for count in [1usize, 499, 500, usize::MAX] {
            let window = index.slice(start, count);
            let expected = (500 - start).min(count);
            assert_eq!(
                window.len(),
                expected,
                "slice({start}, {count:#x}) clamped wrong"
            );
        }
    }
}
