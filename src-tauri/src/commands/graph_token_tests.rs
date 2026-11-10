//! The graph's revision token: a reload that nothing has changed is answered
//! without a history walk, lane solve or row on the wire, and the token never
//! answers for a state or a view it was not issued for.
//!
//! Stamps are judged settled by `ref_cache::eager`, which runs a test with no
//! racy window; the pages are read with `commit_graph_page`, on the test's own
//! thread, because that override is thread-local.

use super::*;
use crate::ipc_error::CommandError;

fn graph_token_repo() -> tempfile::TempDir {
    let repo = crate::test_support::git_repo();
    for n in 0..3 {
        crate::test_support::git_in(
            repo.path(),
            &["commit", "--allow-empty", "-m", &format!("feat: {n}")],
        );
    }
    repo
}

fn page(root: &str, max: usize, known: Option<&str>) -> Result<CommitGraphPayload, CommandError> {
    commit_graph_page(
        root.to_string(),
        max,
        None,
        None,
        None,
        None,
        known.map(str::to_string),
    )
}

fn history_walks(root: &std::path::Path) -> usize {
    crate::engine::git_cli::spawn_log::spawns_in(root)
        .iter()
        .filter(|argv| argv.iter().any(|a| a == "--topo-order"))
        .count()
}

/// A token from one load answers the next reload of the same view with no
/// rows, no refs and no history walk: the O(1)-on-the-wire contract.
#[test]
fn a_known_graph_token_is_answered_unchanged_without_a_walk_or_rows() {
    crate::engine::ref_cache::eager(|| {
        let repo = graph_token_repo();
        let root = repo.path().to_string_lossy().into_owned();
        let full = page(&root, 100, None).expect("first load");
        assert!(!full.unchanged);
        assert_eq!(full.rows.len(), 3);
        let token = full
            .graph_token
            .clone()
            .expect("a settled load issues a token");
        let full_bytes = serde_json::to_vec(&full).unwrap().len();

        let walks = history_walks(repo.path());
        let again = page(&root, 100, Some(&token)).expect("reload");
        assert!(again.unchanged, "the token still names the state");
        assert!(again.rows.is_empty() && again.refs.is_empty());
        assert_eq!(again.head_id, full.head_id, "HEAD is current on any answer");
        assert_eq!(again.graph_token.as_deref(), Some(token.as_str()));
        assert_eq!(history_walks(repo.path()), walks, "no history walk ran");
        let unchanged_bytes = serde_json::to_vec(&again).unwrap().len();
        assert!(
            unchanged_bytes * 4 < full_bytes,
            "unchanged {unchanged_bytes} B against full {full_bytes} B"
        );
    });
}

/// Every input of the page is part of the token: another window size, another
/// query, a revision, a moved ref and a new branch each get a full answer.
#[test]
fn a_graph_token_does_not_answer_for_a_state_or_view_it_was_not_issued_for() {
    crate::engine::ref_cache::eager(|| {
        let repo = graph_token_repo();
        let root = repo.path().to_string_lossy().into_owned();
        let token = page(&root, 100, None).unwrap().graph_token.unwrap();

        assert!(
            !page(&root, 50, Some(&token)).unwrap().unchanged,
            "window size"
        );
        let other_query = commit_graph_page(
            root.clone(),
            100,
            Some("feat".into()),
            None,
            None,
            None,
            Some(token.clone()),
        )
        .unwrap();
        assert!(!other_query.unchanged, "query");
        let revision = commit_graph_page(
            root.clone(),
            100,
            None,
            Some("main".into()),
            None,
            None,
            Some(token.clone()),
        )
        .unwrap();
        assert!(
            !revision.unchanged && revision.graph_token.is_none(),
            "revision"
        );

        crate::test_support::git_in(repo.path(), &["commit", "--allow-empty", "-m", "feat: 3"]);
        let moved = page(&root, 100, Some(&token)).unwrap();
        assert!(!moved.unchanged, "a ref moved");
        assert_eq!(moved.rows.len(), 4);
        assert_ne!(moved.graph_token.as_deref(), Some(token.as_str()));

        crate::test_support::git_in(repo.path(), &["branch", "side"]);
        assert!(
            !page(&root, 100, Some(&moved.graph_token.unwrap()))
                .unwrap()
                .unchanged,
            "a new branch is a new decoration"
        );
    });
}

/// The ref cache never serves an answer older than its TTL, because the stamp
/// cannot see every input; a token is held to the same bound.
#[test]
fn a_graph_token_older_than_the_ref_cache_ttl_is_answered_with_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let clock = crate::engine::ref_cache::ManualClock::install();
        let repo = graph_token_repo();
        let root = repo.path().to_string_lossy().into_owned();
        let ttl = crate::engine::ref_cache::TTL;
        let first = page(&root, 100, None).unwrap().graph_token.unwrap();

        clock.advance(ttl - std::time::Duration::from_secs(2));
        let inside = page(&root, 100, Some(&first)).unwrap();
        assert!(
            inside.unchanged,
            "a token inside the TTL still names the state"
        );
        assert_eq!(
            inside.graph_token.as_deref(),
            Some(first.as_str()),
            "an unchanged answer does not renew the token: its age counts from the real read"
        );

        clock.advance(std::time::Duration::from_secs(4));
        let past = page(&root, 100, Some(&first)).unwrap();
        assert!(!past.unchanged, "a token past the TTL is refused");
        assert_eq!(past.rows.len(), 3, "and the refusal is a whole page");
        let fresh = past.graph_token.expect("a fresh token comes with it");
        assert_ne!(fresh, first);
        assert!(page(&root, 100, Some(&fresh)).unwrap().unchanged);
    });
}

/// Nothing the stamp covers moves when an `include.path` file switches replace
/// refs off, yet the history git prints changes. The TTL is what bounds how
/// long a held graph can stay wrong.
#[test]
fn a_change_the_stamp_cannot_see_is_picked_up_once_the_token_expires() {
    crate::engine::ref_cache::eager(|| {
        let clock = crate::engine::ref_cache::ManualClock::install();
        let repo = graph_token_repo();
        let outside = tempfile::tempdir().expect("a directory outside .git");
        let included = outside.path().join("extra.gitconfig");
        std::fs::write(&included, "").unwrap();
        crate::test_support::git_in(
            repo.path(),
            &["config", "include.path", included.to_str().unwrap()],
        );
        // The middle commit is replaced by a parentless copy of itself.
        crate::test_support::git_in(repo.path(), &["replace", "--graft", "HEAD~1"]);
        let root = repo.path().to_string_lossy().into_owned();
        let first = page(&root, 100, None).unwrap();
        assert_eq!(
            first.rows.len(),
            2,
            "the replace ref hides the oldest commit"
        );
        let token = first.graph_token.unwrap();

        std::fs::write(&included, "[core]\n\tuseReplaceRefs = false\n").unwrap();
        clock.advance(std::time::Duration::from_secs(1));
        let inside = page(&root, 100, Some(&token)).unwrap();
        assert!(
            inside.unchanged,
            "no stamped file moved, so inside the TTL the held graph is kept"
        );

        clock.advance(crate::engine::ref_cache::TTL);
        let past = page(&root, 100, Some(&token)).unwrap();
        assert!(!past.unchanged);
        assert_eq!(
            past.rows.len(),
            3,
            "the full history is back once the token expires"
        );
    });
}

fn spawn_shapes(root: &std::path::Path) -> Vec<Vec<String>> {
    let needle = root.to_string_lossy().into_owned();
    let mut shapes: Vec<Vec<String>> = crate::engine::git_cli::spawn_log::spawns_in(root)
        .into_iter()
        .map(|argv| {
            argv.iter()
                .map(|arg| arg.replace(&needle, "<repo>"))
                .collect()
        })
        .collect();
    shapes.sort();
    shapes
}

/// A token that is already stale (here: issued for another repository, so its
/// view differs) must cost what no token costs. Which requests wait for the
/// probes is decided by [`token_may_hold`] from the view, the stamp and the
/// clock alone, so a stale token is turned away before any probe, and the
/// work it then does is the work a request without a token does: the same git
/// children, the same page.
#[test]
fn a_stale_token_is_turned_away_before_the_probes_and_does_the_work_of_no_token() {
    crate::engine::ref_cache::eager(|| {
        let clock = crate::engine::ref_cache::ManualClock::install();
        let holder = graph_token_repo();
        let root = holder.path().to_string_lossy().into_owned();
        let token = page(&root, 100, None).unwrap().graph_token.unwrap();
        let view = view_key(&root, 100, None, RefScope::Named);
        let stamp = graph_stamp(&root, RefScope::Named).expect("a settled stamp");

        assert!(token_may_hold(&token, view, stamp), "the state it names");
        assert!(!token_may_hold(&token, view, stamp ^ 1), "a moved ref");
        assert!(!token_may_hold(&token, view ^ 1, stamp), "another window");
        assert!(!token_may_hold("garbage", view, stamp), "not a token");
        assert!(
            !token_may_hold("0000000000000000-1", view, stamp),
            "too short"
        );
        clock.advance(crate::engine::ref_cache::TTL);
        assert!(!token_may_hold(&token, view, stamp), "past the TTL");

        let with_token = graph_token_repo();
        let without = graph_token_repo();
        let stale = page(with_token.path().to_str().unwrap(), 100, Some(&token)).unwrap();
        let none = page(without.path().to_str().unwrap(), 100, None).unwrap();
        assert!(!stale.unchanged);
        assert_eq!(stale.rows.len(), none.rows.len());
        let summaries = |p: &CommitGraphPayload| -> Vec<String> {
            p.rows.iter().map(|r| r.summary.clone()).collect()
        };
        assert_eq!(summaries(&stale), summaries(&none));
        assert_eq!(
            spawn_shapes(with_token.path()),
            spawn_shapes(without.path()),
            "a stale token spawns exactly what no token spawns"
        );
    });
}

/// A write inside the stamp's tick cannot be told apart from the state
/// stamped, so no token is issued and nothing is answered unchanged.
#[test]
fn an_unsettled_stamp_issues_no_token_and_answers_nothing_unchanged() {
    let repo = graph_token_repo();
    let root = repo.path().to_string_lossy().into_owned();
    let full = page(&root, 100, None).expect("load");
    assert!(
        full.graph_token.is_none(),
        "refs written moments ago are inside the racy window"
    );
    assert!(
        !page(&root, 100, Some("0000000000000000"))
            .unwrap()
            .unchanged
    );
}

/// A degraded facet is retried on the next reload, not vouched for.
#[test]
fn a_load_with_a_warning_issues_no_token() {
    crate::engine::ref_cache::eager(|| {
        let repo = graph_token_repo();
        let head = std::fs::read_to_string(repo.path().join(".git/HEAD")).unwrap();
        let branch_ref = head.trim().trim_start_matches("ref: ").to_string();
        std::fs::write(
            repo.path().join(".git").join(branch_ref),
            "not an object id\n",
        )
        .unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let degraded = page(&root, 100, None).expect("a degraded HEAD still loads");
        assert!(!degraded.warnings.is_empty(), "{:?}", degraded.warnings);
        assert!(degraded.graph_token.is_none());
    });
}
