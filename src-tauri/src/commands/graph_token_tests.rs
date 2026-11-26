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
    crate::engine::process::spawn_log::spawns_in(root)
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
    let mut shapes: Vec<Vec<String>> = crate::engine::process::spawn_log::spawns_in(root)
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
        // The page asks for HEAD's branch from two of its scoped threads (the
        // decorations and the default branch). Those threads have their own
        // (test) ref-cache tables, so the two asks share one child only when
        // both are queued at the spawn gate together: one or two children, by
        // the schedule, on either side (Linux CI ran one against two). Every
        // other child is still compared exactly.
        let head_ask = ["symbolic-ref", "--quiet", "HEAD"].map(String::from);
        let split = |shapes: Vec<Vec<String>>| -> (Vec<Vec<String>>, usize) {
            let (asks, rest): (Vec<_>, Vec<_>) = shapes
                .into_iter()
                .partition(|argv| argv.ends_with(&head_ask));
            (rest, asks.len())
        };
        let (stale_rest, stale_asks) = split(spawn_shapes(with_token.path()));
        let (none_rest, none_asks) = split(spawn_shapes(without.path()));
        assert_eq!(
            stale_rest, none_rest,
            "a stale token spawns exactly what no token spawns"
        );
        for (side, asks) in [("stale token", stale_asks), ("no token", none_asks)] {
            assert!(
                (1..=2).contains(&asks),
                "{side}: HEAD's branch is asked by two threads, so one or two children, not {asks}"
            );
        }
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

/// A linear history of `commits` on `refs/heads/win`, with HEAD on it, written
/// with one `git fast-import` so a window larger than a screen costs no more
/// than a few spawns. Timestamps are fixed and old, as the other fixtures'.
fn window_repo(commits: usize) -> tempfile::TempDir {
    let repo = crate::test_support::git_repo();
    let mut stream = String::new();
    for n in 1..=commits {
        let message = format!("commit {n}");
        stream.push_str(&format!(
            "commit refs/heads/win\nmark :{n}\ncommitter T <t@example.com> {} +0000\ndata {}\n{message}\n",
            1_700_000_000 + n * 60,
            message.len()
        ));
        if n > 1 {
            stream.push_str(&format!("from :{}\n", n - 1));
        }
        stream.push_str("M 100644 inline f.txt\ndata 2\nx\n\n");
    }
    crate::engine::git_cli::git_with_stdin(
        repo.path(),
        &["fast-import", "--quiet"],
        stream.as_bytes(),
    )
    .expect("fast-import");
    crate::test_support::git_in(repo.path(), &["symbolic-ref", "HEAD", "refs/heads/win"]);
    repo
}

fn payload_bytes(payload: &CommitGraphPayload) -> usize {
    serde_json::to_vec(payload).unwrap().len()
}

/// A ref change that leaves the loaded window's commits and their order alone
/// is answered with the refs and no rows, whatever the ref change was: a new
/// branch on a commit inside the window, a tag, a ref that moves deep in the
/// history beyond the window. The bytes then depend on the refs, not on how
/// many rows the window holds.
#[test]
fn a_ref_change_that_leaves_the_window_alone_is_answered_without_rows() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        crate::test_support::git_in(repo.path(), &["branch", "old", "win~250"]);
        let full = page(&root, 100, None).expect("first load");
        assert_eq!(full.rows.len(), 100);
        let full_bytes = payload_bytes(&full);
        let mut token = full
            .graph_token
            .clone()
            .expect("a settled load issues a token");

        // (change, a ref name the answer must now carry)
        let changes: [(&[&str], &str); 3] = [
            (&["branch", "side", "win~40"], "side"),
            (&["tag", "v1", "win~5"], "v1"),
            (&["branch", "-f", "old", "win~260"], "old"),
        ];
        for (change, name) in changes {
            crate::test_support::git_in(repo.path(), change);
            let answer = page(&root, 100, Some(&token)).expect("reload");
            assert!(
                answer.rows.is_empty(),
                "{change:?}: the window is untouched, so no row is resent (got {})",
                answer.rows.len()
            );
            assert!(
                answer.window_unchanged && !answer.unchanged,
                "{change:?}: said so, and is not the unchanged answer"
            );
            assert!(
                answer.refs.iter().any(|r| r.name == name),
                "{change:?}: the refreshed decorations name {name}"
            );
            assert!(
                payload_bytes(&answer) * 20 < full_bytes,
                "{change:?}: {} B against a full page of {full_bytes} B",
                payload_bytes(&answer)
            );
            token = answer
                .graph_token
                .clone()
                .expect("the state is vouched for again");
        }

        // The new token names the new state: the next reload is the cheapest answer.
        let settled = page(&root, 100, Some(&token)).expect("reload");
        assert!(settled.unchanged && settled.rows.is_empty() && settled.refs.is_empty());
    });
}

/// The answer's size does not grow with the window: the same refs on a window
/// three times the size cost the same bytes.
#[test]
fn a_refs_only_answer_costs_the_same_for_a_small_and_a_large_window() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let small = page(&root, 100, None).unwrap().graph_token.unwrap();
        let large = page(&root, 300, None).unwrap().graph_token.unwrap();
        crate::test_support::git_in(repo.path(), &["branch", "side", "win~40"]);
        let small = page(&root, 100, Some(&small)).unwrap();
        let large = page(&root, 300, Some(&large)).unwrap();
        assert!(small.rows.is_empty() && large.rows.is_empty());
        assert!(
            payload_bytes(&small).abs_diff(payload_bytes(&large)) < 64,
            "{} B for a window of 100, {} B for 300",
            payload_bytes(&small),
            payload_bytes(&large)
        );
    });
}

/// A ref move that changes the window is a full page, rows and all, never a
/// patch: the lanes are solved over the whole new window.
#[test]
fn a_ref_move_that_changes_the_window_is_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let first = page(&root, 100, None).unwrap();
        let token = first.graph_token.clone().unwrap();
        crate::test_support::git_in(repo.path(), &["commit", "--allow-empty", "-m", "new tip"]);
        let moved = page(&root, 100, Some(&token)).unwrap();
        assert_eq!(moved.rows.len(), 100, "the whole new window comes back");
        assert_eq!(moved.rows[0].summary, "new tip");
        assert_ne!(moved.rows[0].id, first.rows[0].id);
        assert!(!moved.unchanged);
    });
}

/// A replace ref rewrites the parents git prints for a commit inside the
/// window while every branch tip stays put: still a full page.
#[test]
fn a_replace_graft_inside_the_window_with_unchanged_tips_is_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let token = page(&root, 100, None).unwrap().graph_token.unwrap();
        crate::test_support::git_in(repo.path(), &["replace", "--graft", "win~50"]);
        let grafted = page(&root, 100, Some(&token)).unwrap();
        assert!(
            grafted.rows.len() == 51 && !grafted.unchanged,
            "the grafted history is drawn: {} rows",
            grafted.rows.len()
        );
    });
}

/// The straight mainline column follows the default branch. Moving it back
/// while another branch keeps the old tip leaves every commit in the window
/// where it was, and still changes how the lanes are solved: a full page.
#[test]
fn the_default_branch_moving_to_an_ancestor_is_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        crate::test_support::git_in(repo.path(), &["branch", "main", "win"]);
        let first = page(&root, 100, None).unwrap();
        let token = first.graph_token.clone().unwrap();
        let tip = first.rows[0].id.clone();
        assert_eq!(first.mainline_id.as_deref(), Some(tip.as_str()));
        crate::test_support::git_in(repo.path(), &["branch", "-f", "main", "win~3"]);
        let moved = page(&root, 100, Some(&token)).unwrap();
        assert!(!moved.rows.is_empty(), "lanes are re-solved, not patched");
        assert_ne!(moved.mainline_id.as_deref(), Some(tip.as_str()));
    });
}

/// The refs of a moved state are never paired with the token of the state
/// before. When the stamp has not settled by the time the refs-only answer is
/// built, no token comes back: the held one would otherwise stay valid beside
/// refs read after it, and a fingerprint that returned to it (a loose ref
/// created then deleted) would be answered unchanged with the moved refs still
/// drawn.
#[test]
fn a_refs_only_answer_with_an_unsettled_stamp_never_carries_the_held_token() {
    let repo = window_repo(300);
    let root = repo.path().to_string_lossy().into_owned();
    let held = crate::engine::ref_cache::eager(|| {
        page(&root, 100, None)
            .unwrap()
            .graph_token
            .expect("a settled load issues a token")
    });
    // Outside `eager` this write is inside the racy window, so the stamp is
    // unsettled while the answer is built.
    crate::test_support::git_in(repo.path(), &["branch", "side", "win~40"]);
    let moved = page(&root, 100, Some(&held)).unwrap();
    assert!(
        moved.window_unchanged && moved.refs.iter().any(|r| r.name == "side"),
        "the window is untouched, so this is the refs-only answer"
    );
    assert_ne!(
        moved.graph_token.as_deref(),
        Some(held.as_str()),
        "fresh refs must not ride the token of the state before them"
    );
    assert!(
        moved.graph_token.is_none(),
        "an unsettled stamp vouches for nothing"
    );
}

/// Each of these changes what the walk returns or how the lanes are solved, or
/// is a request the token never vouches for, so none may be answered refs-only.
#[test]
fn a_second_parent_inside_the_window_with_the_same_commit_set_is_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let first = page(&root, 100, None).unwrap();
        let token = first.graph_token.clone().unwrap();
        // win~10 gains win~20 as a second parent: both are already in the
        // window, so the commits are the same and only parent_ids change.
        crate::test_support::git_in(
            repo.path(),
            &["replace", "--graft", "win~10", "win~11", "win~20"],
        );
        let answer = page(&root, 100, Some(&token)).unwrap();
        assert_eq!(
            answer.rows.len(),
            first.rows.len(),
            "the commit set is unchanged"
        );
        assert!(
            !answer.rows.is_empty() && !answer.unchanged && !answer.window_unchanged,
            "parent_ids changed, so the lanes are re-solved: a full page"
        );
    });
}

#[test]
fn a_filtered_view_is_never_answered_refs_only() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let filtered = |known: Option<&str>| {
            commit_graph_page(
                root.clone(),
                100,
                Some("commit".to_string()),
                None,
                None,
                None,
                known.map(str::to_string),
            )
            .unwrap()
        };
        let token = filtered(None)
            .graph_token
            .expect("a filtered view is tokened");
        crate::test_support::git_in(repo.path(), &["branch", "side", "win~40"]);
        let answer = filtered(Some(&token));
        assert!(
            !answer.window_unchanged,
            "a filter's notices come from assembly"
        );
        assert!(!answer.rows.is_empty());
    });
}

#[test]
fn a_degraded_facet_whose_window_still_matches_is_a_full_page() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let token = page(&root, 100, None).unwrap().graph_token.unwrap();
        // A ref file git cannot parse degrades the ref listing; the walked
        // window is the same one the token names.
        std::fs::write(
            repo.path().join(".git/refs/heads/broken"),
            "not an object id\n",
        )
        .unwrap();
        let answer = page(&root, 100, Some(&token)).unwrap();
        if answer.warnings.is_empty() {
            panic!("the fixture must degrade a facet: {answer:?}");
        }
        assert!(!answer.window_unchanged && !answer.unchanged);
        assert!(!answer.rows.is_empty(), "a degraded load is resent in full");
        assert!(answer.graph_token.is_none(), "and vouched for by nothing");
    });
}

#[test]
fn a_token_in_the_old_48_character_format_is_refused() {
    crate::engine::ref_cache::eager(|| {
        let repo = window_repo(300);
        let root = repo.path().to_string_lossy().into_owned();
        let token = page(&root, 100, None).unwrap().graph_token.unwrap();
        let (hashes, issued) = token.rsplit_once('-').expect("issued suffix");
        assert_eq!(hashes.len(), 64, "four 16-digit hashes");
        let old = format!("{}-{issued}", &hashes[..48]);
        let answer = page(&root, 100, Some(&old)).unwrap();
        assert!(!answer.unchanged && !answer.window_unchanged);
        assert!(!answer.rows.is_empty());
    });
}
