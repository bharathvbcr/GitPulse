use gitpulse_lib::diff::{ConflictResolutionChoice, ConflictResolver, FileSegment};

#[test]
fn diff3_preview_preserves_the_order_of_all_three_sides() {
    let source =
        "<<<<<<< current\nours\n||||||| base\nancestor\n=======\ntheirs\n>>>>>>> incoming\n";
    let doc = ConflictResolver::parse("file", source);
    let preview = ConflictResolver::render_preview(&doc).unwrap();
    let reparsed = ConflictResolver::parse("file", &preview);
    let FileSegment::Conflict(chunk) = &reparsed.segments[0] else {
        panic!("missing chunk")
    };
    assert_eq!(chunk.ours_content, "ours");
    assert_eq!(chunk.base_content.as_deref(), Some("ancestor"));
    assert_eq!(chunk.theirs_content, "theirs");
}

#[test]
fn malformed_markers_cannot_be_finalized() {
    for source in [
        "<<<<<<< current\nours\n",
        "<<<<<<< current\nours\n>>>>>>> incoming\n",
        "<<<<<<< current\nours\n=======\none\n=======\ntwo\n>>>>>>> incoming\n",
        "<<<<<<< current\n<<<<<<< nested\nours\n=======\ntheirs\n>>>>>>> incoming\n",
        "<<<<<<< current\nours\n========\ntheirs\n>>>>>>> incoming\n",
        ">>>>>>> orphan\n",
    ] {
        let mut doc = ConflictResolver::parse("file", source);
        for segment in &mut doc.segments {
            if let FileSegment::Conflict(chunk) = segment {
                chunk.resolution = ConflictResolutionChoice::AcceptOurs;
            }
        }
        assert!(
            ConflictResolver::render_resolved(&doc).is_err(),
            "accepted malformed source: {source:?}"
        );
    }
}

#[test]
fn marker_like_code_is_not_a_conflict() {
    let source = "<<<<<<<operator\n=======expression\n>>>>>>>operator\nheading\n=======\n";
    let doc = ConflictResolver::parse("file", source);
    assert_eq!(doc.total_conflicts, 0);
    assert_eq!(ConflictResolver::render_resolved(&doc).unwrap(), source);
}

#[test]
fn pasted_conflict_markers_cannot_be_finalized() {
    let mut doc =
        ConflictResolver::parse("file", "<<<<<<< current\na\n=======\nb\n>>>>>>> incoming\n");
    let FileSegment::Conflict(chunk) = &mut doc.segments[0] else {
        panic!("missing chunk")
    };
    chunk.resolution = ConflictResolutionChoice::Custom("<<<<<<< leftover\nvalue".into());
    assert!(ConflictResolver::render_resolved(&doc).is_err());
}

#[test]
fn a_selected_blank_physical_line_is_not_deleted() {
    let mut doc = ConflictResolver::parse(
        "file",
        "head\n<<<<<<< current\n\n=======\nb\n>>>>>>> incoming\ntail\n",
    );
    let FileSegment::Conflict(chunk) = &mut doc.segments[1] else {
        panic!("missing chunk")
    };
    chunk.resolution = ConflictResolutionChoice::AcceptOurs;
    assert_eq!(
        ConflictResolver::render_resolved(&doc).unwrap(),
        "head\n\ntail\n"
    );
}

#[test]
fn forged_chunk_counts_are_rejected() {
    let mut doc = ConflictResolver::parse("file", "normal\n");
    doc.total_conflicts = 12;
    assert!(ConflictResolver::render_resolved(&doc).is_err());
}

#[test]
fn preview_bounds_metadata_and_line_expansion_before_allocating_output() {
    let source = "<<<<<<< current\na\n=======\nb\n>>>>>>> incoming\n";
    let mut doc = ConflictResolver::parse("file", source);
    let FileSegment::Conflict(chunk) = &mut doc.segments[0] else {
        panic!("missing chunk")
    };
    chunk.ours_label = "x".repeat(9 * 1024 * 1024);
    assert!(
        ConflictResolver::render_preview(&doc).is_err(),
        "unbounded marker label"
    );
    let mut doc = ConflictResolver::parse("file", source);
    doc.normal_crlf_flags = vec![vec![false; 100_001]];
    assert!(
        ConflictResolver::render_preview(&doc).is_err(),
        "unbounded EOL metadata"
    );
    let mut doc = ConflictResolver::parse("file", source);
    let FileSegment::Conflict(chunk) = &mut doc.segments[0] else {
        panic!("missing chunk")
    };
    chunk.resolution = ConflictResolutionChoice::Custom("\n".repeat(100_001));
    assert!(
        ConflictResolver::render_resolved(&doc).is_err(),
        "unbounded line expansion"
    );
}

#[test]
fn parser_and_preview_preserve_sides_across_a_bounded_adversarial_corpus() {
    let values = [
        "",
        "\n",
        "λ🦀",
        "line\n\nlast",
        "=======expression",
        "<<<<<<<operator",
        "a\rb",
    ];
    for width in [3, 7, 12, 64] {
        for crlf in [false, true] {
            for ours in values {
                for theirs in values {
                    let source = format!(
                        "{} current\n{ours}\n{} base\nancestor\n{}\n{theirs}\n{} incoming\n",
                        "<".repeat(width),
                        "|".repeat(width),
                        "=".repeat(width),
                        ">".repeat(width)
                    );
                    let source = if crlf {
                        source.replace('\n', "\r\n")
                    } else {
                        source
                    };
                    let doc =
                        ConflictResolver::parse_checked_with_marker_size("file", &source, width)
                            .unwrap();
                    assert_eq!(doc.total_conflicts, 1);
                    let preview = ConflictResolver::render_preview(&doc).unwrap();
                    let reparsed =
                        ConflictResolver::parse_checked_with_marker_size("file", &preview, width)
                            .unwrap();
                    assert_eq!(doc, reparsed);
                }
            }
        }
    }
}

#[test]
fn large_conflict_sets_have_explicit_limits_without_partial_success() {
    let block = "<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n";
    let mut doc = ConflictResolver::parse_checked("file", &block.repeat(2000)).unwrap();
    assert_eq!(doc.total_conflicts, 2000);
    for segment in &mut doc.segments {
        if let FileSegment::Conflict(chunk) = segment {
            chunk.resolution = ConflictResolutionChoice::AcceptOurs;
        }
    }
    assert_eq!(
        ConflictResolver::render_resolved(&doc).unwrap(),
        "a\n".repeat(2000)
    );
    assert!(ConflictResolver::parse_checked("file", &block.repeat(2001)).is_err());
    assert!(ConflictResolver::parse_checked("file", &"x".repeat(4 * 1024 * 1024 + 1)).is_err());
}
