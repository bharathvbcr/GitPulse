//! W6-L4 adversarial review (ft-181d43a9): for ASCII input the new
//! `strip_suffix_ascii_ci` call sites must behave exactly like the slices they
//! replaced. The oracles below are the OLD implementations, copied verbatim
//! from `git show 80cb5fe2`. All expected to PASS (attacks that held).
//!
//! `trim_git_suffix` and `is_python_test_path` are private, so the first is
//! reached through `parse_github_remote_url` and `scan_briefs` is public; the
//! coverage `.py` stem has no public seam and is covered by reasoning in the
//! review report (same helper, same shape as scan_briefs' `.md`).

use gitpulse_lib::github::parse_github_remote_url;
use gitpulse_lib::tasks::scan_briefs;
use proptest::prelude::*;

/// OLD `trim_git_suffix`, verbatim.
fn old_trim_git_suffix(path: &str) -> &str {
    if path.len() >= 4 && path[path.len() - 4..].eq_ignore_ascii_case(".git") {
        &path[..path.len() - 4]
    } else {
        path
    }
}

fn valid_component(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 100
        && !c.starts_with('-')
        && !c.starts_with('.')
        && c.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn name_strategy() -> impl Strategy<Value = String> {
    (
        "[A-Za-z0-9._-]{0,8}",
        prop::sample::select(vec![
            "", ".git", ".GIT", ".Git", ".gIT", "git", ".gi", ".git.git", ".GIT.git", "..git",
        ]),
    )
        .prop_map(|(stem, suffix)| format!("{stem}{suffix}"))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn trim_git_suffix_is_unchanged_for_ascii(
        name in name_strategy(),
        shape in 0usize..6,
        slash in prop::bool::ANY,
    ) {
        let tail = if slash { "/" } else { "" };
        let url = match shape {
            0 => format!("https://github.com/o/{name}{tail}"),
            1 => format!("HTTPS://GitHub.com/o/{name}{tail}"),
            2 => format!("git@github.com:o/{name}{tail}"),
            3 => format!("ssh://git@github.com/o/{name}{tail}"),
            4 => format!("git://github.com/o/{name}{tail}"),
            _ => format!("http://github.com/o/{name}{tail}"),
        };
        let expected = {
            let n = old_trim_git_suffix(&name);
            if valid_component(n) { Some(n.to_string()) } else { None }
        };
        let got = parse_github_remote_url(&url).map(|r| r.name);
        prop_assert_eq!(got, expected, "{}", url);
    }

    #[test]
    fn scan_briefs_md_detection_is_unchanged_for_ascii(
        files in prop::collection::vec(
            (
                prop::bool::weighted(0.15),
                "[a-z0-9_-]{0,6}",
                prop::sample::select(vec![
                    ".md", ".MD", ".mD", ".Md", ".md ", ".mdx", "md", ".txt", ".m", "", "..md", ".md.bak",
                ]),
            ),
            1..8,
        ),
    ) {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("tasks")).unwrap();
        let mut expected = std::collections::BTreeSet::new();
        for (i, (dot, stem, suffix)) in files.iter().enumerate() {
            // The index keeps names distinct on case-insensitive filesystems.
            let name = format!("{}n{i}_{stem}{suffix}", if *dot { "." } else { "" });
            std::fs::write(root.join("tasks").join(&name), "x").unwrap();
            let old_is_brief = name.len() > 3
                && name[name.len() - 3..].eq_ignore_ascii_case(".md")
                && !name.starts_with('.');
            if old_is_brief {
                expected.insert(format!("tasks/{name}"));
            }
        }
        let scan = scan_briefs(root.to_str().unwrap(), Some("tasks")).unwrap();
        let got: std::collections::BTreeSet<String> =
            scan.files.iter().map(|f| f.file.clone()).collect();
        prop_assert_eq!(got, expected);
    }
}

/// Non-ASCII names that used to panic scan_briefs/parse must now be skipped or
/// listed without a panic, and ASCII `.md` files beside them still list.
#[test]
fn scan_briefs_survives_non_ascii_names_beside_real_briefs() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("tasks")).unwrap();
    for name in ["ok.md", "日日.md", "é.md", "aé", "日", "x日.MD", "Zoë"] {
        std::fs::write(root.join("tasks").join(name), "x").unwrap();
    }
    let scan = scan_briefs(root.to_str().unwrap(), Some("tasks")).unwrap();
    let listed: std::collections::BTreeSet<_> =
        scan.files.iter().map(|f| f.file.as_str()).collect();
    assert!(listed.contains("tasks/ok.md"));
    assert!(listed.contains("tasks/日日.md"));
    assert!(!listed.contains("tasks/aé"));
    assert!(!listed.contains("tasks/Zoë"));
}
