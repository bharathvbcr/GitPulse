//! Which refs the commit graph is about — the single owner of that answer.
//!
//! The graph is read through its labels as much as its lanes, so a lane with
//! no name on it is unexplainable: the reader sees a coloured rail leaving
//! the window and has no way to learn what it is or why it exists. That was a
//! real defect, not a theoretical one. The history walk asked git for `--all`
//! — *every* namespace under `refs/` — while the decoration listing read only
//! `refs/heads`, `refs/remotes` and `refs/tags`. Any other namespace
//! therefore opened lanes that nothing could label.
//!
//! Machine-written namespaces are common and they are not small: agent
//! harnesses keep per-turn checkpoints (`refs/cmux/last-turn/*`,
//! `refs/codex/turn-diffs/*`), `git maintenance` writes
//! `refs/prefetch/remotes/*`, CI mirrors write `refs/pull/*`. In one real
//! repository 18 `refs/cmux/last-turn/*` refs contributed 36 of 101 commits
//! and 34 of 35 lanes: without them that history is a single straight rail.
//!
//! So walk set and label set are derived from ONE list here. A scope answers
//! both questions, and a contract test pins that they agree — the two can no
//! longer drift apart, which is the only reason the defect was possible.
//!
//! [`RefScope::All`] keeps every ref walkable for people who deliberately
//! park history in a custom namespace, and labels those refs too, so the
//! invariant ("everything drawn is named") holds in both scopes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which refs the graph walks and labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefScope {
    /// Branches, remote-tracking branches, tags and HEAD: the refs a user
    /// names, checks out and pushes. The default.
    #[default]
    Named,
    /// Every ref under `refs/`, plus HEAD — git's own `--all`. Custom
    /// namespaces open lanes, and are labelled by their full ref path.
    All,
}

/// The namespaces [`RefScope::Named`] covers, as `for-each-ref` patterns.
///
/// These are the prefixes [`crate::graph::list_ref_decorations`] knows how to
/// turn into a short name, and — by the contract test below — exactly the
/// prefixes [`history_rev_args`] asks git to walk.
pub const NAMED_REF_PATTERNS: [&str; 3] = ["refs/heads", "refs/remotes", "refs/tags"];

/// Walk arguments that select no refs of their own.
///
/// `--ignore-missing` is load-bearing, not defensive. Naming `HEAD` makes
/// `git log` fail outright when HEAD is unborn — a brand-new repository, and
/// every orphan branch (`git checkout --orphan`), where HEAD is a symref to a
/// branch with no commits. `--all` never had to care because it names no
/// individual rev. Without this the graph goes blank on an orphan branch even
/// though `main` still holds the whole history, turning an ordinary git state
/// into an empty pane. It applies ONLY to the scope's own arguments: an
/// explicit `revision` from the caller bypasses this list entirely, so a bad
/// revision still fails loudly instead of quietly walking nothing.
const WALK_TOLERANCE: [&str; 1] = ["--ignore-missing"];

/// Ref selectors for [`RefScope::Named`] — the `git log` spelling of exactly
/// the namespaces [`NAMED_REF_PATTERNS`] labels, plus HEAD.
///
/// HEAD is listed explicitly because `--branches --remotes --tags` does not
/// cover a detached HEAD, and a graph that cannot draw the commit you are
/// sitting on is worse than one that draws too much.
const NAMED_REF_SELECTORS: [&str; 4] = ["HEAD", "--branches", "--remotes", "--tags"];

/// The named walk, assembled from its two halves so there is no second copy
/// of the ref selectors to drift from the first.
const NAMED_WALK_ARGS: [&str; WALK_TOLERANCE.len() + NAMED_REF_SELECTORS.len()] = {
    let mut out = [""; WALK_TOLERANCE.len() + NAMED_REF_SELECTORS.len()];
    let mut i = 0;
    while i < WALK_TOLERANCE.len() {
        out[i] = WALK_TOLERANCE[i];
        i += 1;
    }
    let mut j = 0;
    while j < NAMED_REF_SELECTORS.len() {
        out[WALK_TOLERANCE.len() + j] = NAMED_REF_SELECTORS[j];
        j += 1;
    }
    out
};

/// Revision arguments for `git log`, in place of `--all`.
pub fn history_rev_args(scope: RefScope) -> &'static [&'static str] {
    match scope {
        RefScope::Named => &NAMED_WALK_ARGS,
        RefScope::All => &["--all"],
    }
}

/// `for-each-ref` patterns for the decoration listing.
///
/// `RefScope::All` reads the whole of `refs/` because under that scope every
/// ref can open a lane, and every lane must be nameable.
pub fn decoration_patterns(scope: RefScope) -> &'static [&'static str] {
    match scope {
        RefScope::Named => &NAMED_REF_PATTERNS,
        RefScope::All => &["refs/"],
    }
}

/// True when `refname` is one [`RefScope::Named`] both walks and labels.
///
/// The match is on PATH COMPONENTS, not on the string prefix. `--branches` is
/// `refs/heads/*`, so `refs/headsfoo/x` and `refs/heads-extra/x` are ordinary
/// custom refs that git does not walk — but a `starts_with("refs/heads")` test
/// calls them branches. That misclassification fails in the worst direction:
/// the walk skips such a ref (git is right) while the census believes it is
/// named, so its commits are neither drawn NOR reported. Every rule here is
/// checked against git itself in `tests/graph_ref_scope_stress.rs`.
pub fn is_named_ref(refname: &str) -> bool {
    NAMED_REF_PATTERNS.iter().any(|pattern| {
        refname.len() > pattern.len() + 1
            && refname.starts_with(pattern)
            && refname.as_bytes()[pattern.len()] == b'/'
    })
}

/// Top-level namespace of a ref outside the named set — `refs/cmux/x/y` ⇒
/// `refs/cmux`. `None` for named refs and for anything not under `refs/`.
fn hidden_namespace_of(refname: &str) -> Option<&str> {
    if is_named_ref(refname) {
        return None;
    }
    let rest = refname.strip_prefix("refs/")?;
    match rest.find('/') {
        // `refs/stash` and other single-segment refs are their own namespace.
        None => Some(refname),
        Some(cut) => Some(&refname[..("refs/".len() + cut)]),
    }
}

/// Census of the refs a [`RefScope::Named`] walk leaves out, grouped by
/// top-level namespace and counted.
///
/// Reported rather than silently dropped: history that is not shown must not
/// be indistinguishable from history that does not exist. Callers turn this
/// into a payload warning naming what is hidden and how to see it.
pub fn hidden_ref_namespaces<'a>(
    refnames: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, usize> {
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for refname in refnames {
        if let Some(namespace) = hidden_namespace_of(refname.trim()) {
            *census.entry(namespace.to_string()).or_insert(0) += 1;
        }
    }
    census
}

/// How much history a named-scope walk leaves out, and which namespaces hold
/// the refs it skipped.
///
/// The two are counted separately on purpose. A ref outside the named set
/// does not necessarily hide anything: `refs/archive/v1` pointing at an
/// ancestor of `main` is walked and drawn like any other commit on the rail.
/// Only commits reachable from NO named ref are actually missing, so the
/// commit count is the claim and the namespace list is context for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HiddenHistory {
    /// Commits reachable only from refs outside the named set.
    pub commits: usize,
    /// True when the probe stopped at its cap, so `commits` is a floor.
    pub capped: bool,
    /// Ref count per top-level namespace (`refs/cmux` => 18).
    pub namespaces: BTreeMap<String, usize>,
}

/// One sentence naming what a named-scope walk left out, or `None` when it
/// left nothing out.
///
/// `None` when no commit is missing — including the case where custom
/// namespaces exist but every commit they name is already on a branch. A
/// breadcrumb that fires when nothing is actually hidden is noise, and noise
/// is how a real one gets ignored.
pub fn hidden_ref_warning(hidden: &HiddenHistory) -> Option<String> {
    if hidden.commits == 0 {
        return None;
    }
    let floor = if hidden.capped { " or more" } else { "" };
    let commits = hidden.commits;
    let head = format!(
        "{commits}{floor} commit(s) reachable only from refs outside branches, remotes and \
         tags are not drawn."
    );
    let tail = "Set the graph's ref scope to \"All refs\" to include them.";

    if hidden.namespaces.is_empty() {
        // Should be unreachable: the probe counts commits and the census reads
        // the same repository's refs. If it ever happens, say what is known
        // rather than trailing off after "live in:" — a sentence that names
        // nothing reads as a rendering bug and gets ignored, which is how a
        // real report about missing history goes unnoticed.
        return Some(format!(
            "{head} The refs holding them could not be attributed to a namespace. {tail}"
        ));
    }

    // Ordered by SIZE, not by name. Alphabetical order with a bound reports
    // whichever namespaces happen to sort first and drops the rest — so a
    // repository with ten tiny namespaces and one holding ten thousand refs
    // would name the ten and hide the one that explains the graph.
    let mut ranked: Vec<(&String, &usize)> = hidden.namespaces.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    // The list is bounded so a pathological ref layout cannot turn one
    // breadcrumb into a wall of text.
    const MAX_NAMED: usize = 6;
    let mut shown: Vec<String> = ranked
        .iter()
        .take(MAX_NAMED)
        .map(|(namespace, count)| {
            // No `/*` suffix: `refs/stash` is a ref, not a directory, and
            // writing `refs/stash/*` describes children it does not have.
            let plural = if **count == 1 { "ref" } else { "refs" };
            format!("{namespace} ({count} {plural})")
        })
        .collect();
    if ranked.len() > MAX_NAMED {
        shown.push(format!("and {} more", ranked.len() - MAX_NAMED));
    }
    Some(format!(
        "{head} Those refs live in: {}. {tail}",
        shown.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this module exists to make impossible: the set of refs the
    /// graph WALKS and the set it LABELS were two independent hard-coded
    /// lists, and drifted. Under the named scope every namespace git is asked
    /// to walk must be one the decoration listing reads, and vice versa.
    #[test]
    fn named_scope_walks_exactly_what_it_labels() {
        let walked = history_rev_args(RefScope::Named);
        let labelled = decoration_patterns(RefScope::Named);
        assert_eq!(
            labelled, &NAMED_REF_PATTERNS,
            "the decoration listing must read every named namespace"
        );
        // `git log` spells the same three namespaces as option flags.
        for (pattern, flag) in [
            ("refs/heads", "--branches"),
            ("refs/remotes", "--remotes"),
            ("refs/tags", "--tags"),
        ] {
            assert!(
                labelled.contains(&pattern),
                "{pattern} is walked via {flag} but never labelled"
            );
            assert!(
                walked.contains(&flag),
                "{pattern} is labelled but {flag} is missing from the walk"
            );
        }
        assert!(
            walked.contains(&"HEAD"),
            "a detached HEAD is labelled by list_ref_decorations and must be walked"
        );
        // Everything that is not a tolerance flag must be a ref selector the
        // labeller covers: the walk must not reach a namespace by any other
        // spelling, which is the drift that produced unnameable lanes.
        let selectors: Vec<&str> = walked
            .iter()
            .copied()
            .filter(|arg| !WALK_TOLERANCE.contains(arg))
            .collect();
        assert_eq!(
            selectors.len(),
            NAMED_REF_PATTERNS.len() + 1,
            "the walk selects {selectors:?}: one selector per labelled namespace, plus HEAD, \
             and nothing else — an extra selector is a namespace with no label"
        );
    }

    /// `--ignore-missing` is what keeps naming HEAD safe on an unborn branch.
    /// Dropping it makes the graph fail entirely on any orphan branch, so it
    /// is pinned rather than left to look like a stray flag someone can tidy
    /// away. The behavioural proof lives in `tests/adversarial_repos.rs`.
    #[test]
    fn the_named_walk_tolerates_an_unborn_head() {
        assert!(history_rev_args(RefScope::Named).contains(&"--ignore-missing"));
        assert!(
            !history_rev_args(RefScope::All).contains(&"--ignore-missing"),
            "--all names no individual rev and must keep failing loudly"
        );
    }

    #[test]
    fn all_scope_is_gits_own_all() {
        assert_eq!(history_rev_args(RefScope::All), &["--all"]);
        assert_eq!(decoration_patterns(RefScope::All), &["refs/"]);
    }

    #[test]
    fn named_refs_are_recognised_and_bare_namespace_roots_are_not() {
        assert!(is_named_ref("refs/heads/main"));
        assert!(is_named_ref("refs/remotes/origin/main"));
        assert!(is_named_ref("refs/tags/v1.0"));
        assert!(is_named_ref("refs/heads/nested/deep/branch"));
        // A ref that IS the prefix names nothing inside it.
        assert!(!is_named_ref("refs/heads"));
        assert!(!is_named_ref("refs/heads/"));
        assert!(!is_named_ref("refs/cmux/last-turn/abc"));
        assert!(!is_named_ref("refs/stash"));
        assert!(!is_named_ref("HEAD"));
    }

    /// The match is on path components. These names share the string prefix
    /// but are NOT what `--branches`/`--remotes`/`--tags` walk, and calling
    /// them named hides their commits from both the graph and the report.
    #[test]
    fn a_namespace_that_merely_starts_with_a_named_prefix_is_not_named() {
        for sneaky in [
            "refs/headsfoo/x",
            "refs/heads-extra/x",
            "refs/remotesx/x",
            "refs/remotes-mirror/x",
            "refs/tagsy/x",
            "refs/tags-archive/x",
        ] {
            assert!(
                !is_named_ref(sneaky),
                "{sneaky} was classified as a named ref"
            );
            assert!(
                hidden_ref_namespaces([sneaky]).len() == 1,
                "{sneaky} must be counted as hidden"
            );
        }
    }

    /// A count with no namespace behind it must still read as a sentence.
    #[test]
    fn hidden_commits_with_no_attributable_namespace_still_report() {
        let hidden = HiddenHistory {
            commits: 3,
            capped: false,
            namespaces: BTreeMap::new(),
        };
        let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
        assert!(warning.contains("3 commit(s)"), "{warning}");
        assert!(
            !warning.contains("live in: ."),
            "trailing-off sentence: {warning}"
        );
    }

    #[test]
    fn census_groups_by_top_level_namespace() {
        let census = hidden_ref_namespaces([
            "refs/heads/main",
            "refs/remotes/origin/main",
            "refs/tags/v1.0",
            "refs/cmux/last-turn/aaa",
            "refs/cmux/last-turn/bbb",
            "refs/codex/turn-diffs/checkpoints/x/y/z",
            "refs/stash",
            "HEAD",
        ]);
        assert_eq!(census.get("refs/cmux"), Some(&2));
        assert_eq!(census.get("refs/codex"), Some(&1));
        assert_eq!(census.get("refs/stash"), Some(&1));
        assert_eq!(census.len(), 3, "named refs must not be counted as hidden");
    }

    #[test]
    fn a_clean_repository_produces_no_warning() {
        let census = hidden_ref_namespaces(["refs/heads/main", "refs/tags/v1.0"]);
        assert!(census.is_empty());
        assert_eq!(hidden_ref_warning(&HiddenHistory::default()), None);
    }

    /// A custom namespace whose commits are all already on a branch hides
    /// NOTHING — `refs/archive/v1` pointing at an ancestor of `main` is drawn
    /// like any other commit on the rail. Warning about it would report a
    /// complete graph as incomplete.
    #[test]
    fn refs_that_hide_no_commits_produce_no_warning() {
        let hidden = HiddenHistory {
            commits: 0,
            capped: false,
            namespaces: hidden_ref_namespaces(["refs/archive/v1"]),
        };
        assert_eq!(
            hidden.namespaces.len(),
            1,
            "the ref is still outside the set"
        );
        assert_eq!(hidden_ref_warning(&hidden), None);
    }

    #[test]
    fn the_warning_counts_commits_names_the_namespaces_and_gives_the_way_out() {
        let hidden = HiddenHistory {
            commits: 36,
            capped: false,
            namespaces: hidden_ref_namespaces(["refs/cmux/a", "refs/cmux/b", "refs/stash"]),
        };
        let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
        assert!(warning.contains("36 commit(s)"), "{warning}");
        assert!(!warning.contains("36 or more"), "{warning}");
        assert!(warning.contains("refs/cmux (2 refs)"), "{warning}");
        assert!(warning.contains("refs/stash (1 ref)"), "{warning}");
        assert!(warning.contains("All refs"), "{warning}");
    }

    /// A capped probe reports a floor, never a number that reads as exact.
    #[test]
    fn a_capped_probe_reports_its_count_as_a_floor() {
        let hidden = HiddenHistory {
            commits: 5_000,
            capped: true,
            namespaces: hidden_ref_namespaces(["refs/pull/1/head"]),
        };
        let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
        assert!(warning.contains("5000 or more commit(s)"), "{warning}");
    }

    #[test]
    fn the_warning_bounds_its_own_length() {
        let names: Vec<String> = (0..20).map(|i| format!("refs/ns{i:02}/x")).collect();
        let hidden = HiddenHistory {
            commits: 20,
            capped: false,
            namespaces: hidden_ref_namespaces(names.iter().map(String::as_str)),
        };
        let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
        assert!(warning.contains("and 14 more"), "{warning}");
    }
}
