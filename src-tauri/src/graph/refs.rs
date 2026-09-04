//! Ref decorations: which branches, tags and HEAD point at which commit.
//!
//! The graph is read as much through its labels as through its lanes — a lane
//! with no name on it is just a coloured line — so the rows carry the refs that
//! point at them. They are resolved once per graph load with a single
//! `for-each-ref`, rather than per row, because a repository with thousands of
//! refs would otherwise pay a process spawn for each one.
//!
//! Which refs are read is NOT decided here: [`crate::graph::ref_scope`] owns
//! that for the walk and this listing together, so the graph can never draw a
//! lane this module has no name for.

use serde::{Deserialize, Serialize};

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_reader::REFS_TAG_CAP;
use crate::graph::ref_scope::{self, HiddenHistory, RefScope};

/// What a ref is, so the UI can style and order them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Local,
    Remote,
    Tag,
    /// A detached HEAD. A named branch's HEAD is reported on the branch itself
    /// via `is_head`, not as a separate ref.
    Head,
    /// A ref outside branches, remotes and tags — an agent-harness checkpoint,
    /// a prefetch mirror, `refs/stash`. Only ever produced under
    /// [`RefScope::All`], which is also the only scope that walks them, and
    /// named by its full ref path because such refs have no short form a user
    /// would recognise.
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefDecoration {
    /// Short name: `main`, `origin/main`, `v1.2.0`.
    pub name: String,
    pub kind: RefKind,
    /// The commit this ref resolves to. For an annotated tag this is the
    /// commit it peels to, not the tag object, so it lands on a graph row.
    pub commit_id: String,
    /// True for the branch HEAD is currently on.
    pub is_head: bool,
}

/// Ceiling on decorations for refs outside branches, remotes and tags.
///
/// Only [`RefScope::All`] can produce them, and only that scope walks them —
/// but a CI mirror carries six figures of `refs/pull/*`, and shipping one
/// decoration each over IPC on every graph load would stall the UI on a
/// payload nobody can read. Branches and remotes stay uncapped on purpose:
/// those are a person's own work, and there are never a hundred thousand.
pub const REFS_OTHER_CAP: usize = 200;

/// A decoration listing, and what it had to leave out.
///
/// The counts exist because the caps are old but the reporting was not: a
/// repository with 250 tags showed 200 chips and said nothing, which is a
/// truncated answer wearing a complete answer's clothes. Callers turn a
/// non-zero count into a payload warning.
#[derive(Debug, Clone, Default)]
pub struct RefListing {
    pub decorations: Vec<RefDecoration>,
    /// Tags beyond [`REFS_TAG_CAP`], dropped oldest-first.
    pub tags_dropped: usize,
    /// Non-named refs beyond [`REFS_OTHER_CAP`], dropped oldest-first.
    pub other_dropped: usize,
}

impl RefListing {
    /// One sentence naming what the listing left out, or `None` when it is
    /// complete.
    pub fn truncation_warning(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.tags_dropped > 0 {
            parts.push(format!(
                "{} older tag(s) beyond the newest {REFS_TAG_CAP}",
                self.tags_dropped
            ));
        }
        if self.other_dropped > 0 {
            parts.push(format!(
                "{} ref(s) outside branches, remotes and tags beyond the newest {REFS_OTHER_CAP}",
                self.other_dropped
            ));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "Ref labels are capped for this repository: {} are not shown. Rows they point at \
             are still drawn, just unlabelled.",
            parts.join(", and ")
        ))
    }
}

/// Ceiling on the hidden-history probe. It bounds one `rev-list` on a
/// repository whose custom namespaces hold their own deep history; the result
/// is then reported as a floor rather than rounded down silently.
pub const HIDDEN_PROBE_CAP: usize = 5_000;

/// Ceiling on the ref listing that names the hidden namespaces. A CI mirror
/// can hold six figures of `refs/pull/*`; naming the namespace does not
/// require reading every ref in it.
pub const HIDDEN_REF_SCAN_CAP: usize = 5_000;

/// Counts the history a [`RefScope::Named`] walk leaves out, and names the
/// namespaces holding it.
///
/// The commit probe runs first and the ref listing only when it finds
/// something: with no refs outside the named set every tip is immediately
/// uninteresting and `rev-list` returns at once, so the common repository pays
/// one cheap process and never the full ref scan.
pub fn probe_hidden_history(repo_path: &str) -> Result<HiddenHistory, String> {
    let repo = validate_repo(repo_path)?;
    let max_count = format!("--max-count={}", HIDDEN_PROBE_CAP);
    let mut args: Vec<&str> = vec!["rev-list", max_count.as_str(), "--all", "--not"];
    args.extend_from_slice(ref_scope::history_rev_args(RefScope::Named));
    let stdout = git_text(&repo, &args)?;
    let commits = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    if commits == 0 {
        return Ok(HiddenHistory::default());
    }

    let count_arg = format!("--count={}", HIDDEN_REF_SCAN_CAP);
    let listed = git_text(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            count_arg.as_str(),
            "refs/",
        ],
    )?;
    Ok(HiddenHistory {
        commits,
        capped: commits >= HIDDEN_PROBE_CAP,
        namespaces: ref_scope::hidden_ref_namespaces(listed.lines()),
    })
}

/// Reads the refs `scope` covers, and marks the checked-out one.
pub fn list_ref_decorations(repo_path: &str, scope: RefScope) -> Result<RefListing, String> {
    let repo = validate_repo(repo_path)?;

    // A branch name can contain anything but the ASCII control characters and a
    // few specials, so the fields are NUL-separated and the records
    // \x01-separated rather than parsed out of whitespace. Tags carry their
    // creatordate so a CI-tagged monorepo with tens of thousands of them can
    // be capped to the newest REFS_TAG_CAP instead of shipping every chip on
    // every graph load.
    let mut for_each_ref: Vec<&str> = vec![
        "for-each-ref",
        "--format=%(objectname)%00%(refname)%00%(objecttype)%00%(*objectname)%00%(creatordate:unix)%01",
    ];
    for_each_ref.extend_from_slice(ref_scope::decoration_patterns(scope));
    let raw = git_text(&repo, &for_each_ref)?;

    let current_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut decorations = Vec::new();
    // Tags are held aside so the cap can keep the NEWEST ones; branches and
    // remotes go straight through uncapped.
    let mut tags: Vec<(RefDecoration, i64)> = Vec::new();
    // Refs outside branches, remotes and tags carry their own cap: only the
    // all-refs scope produces them, and only in bulk.
    let mut others: Vec<(RefDecoration, i64)> = Vec::new();
    for record in raw.split('\x01') {
        let record = record.trim_matches(['\n', '\r']);
        if record.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\x00').collect();
        if fields.len() < 3 {
            continue;
        }
        let object_id = fields[0].trim();
        let refname = fields[1].trim();
        let peeled = fields.get(3).map(|s| s.trim()).unwrap_or("");
        // An annotated tag's own object is not on the graph; the commit it
        // peels to is.
        let commit_id = if peeled.is_empty() { object_id } else { peeled };
        let creatordate = fields
            .get(4)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);

        let (kind, name) = if let Some(short) = refname.strip_prefix("refs/heads/") {
            (RefKind::Local, short)
        } else if let Some(short) = refname.strip_prefix("refs/remotes/") {
            // `origin/HEAD` is a symbolic pointer, not a branch a user checks
            // out from the graph; showing it duplicates the branch it names.
            if short.ends_with("/HEAD") {
                continue;
            }
            (RefKind::Remote, short)
        } else if let Some(short) = refname.strip_prefix("refs/tags/") {
            (RefKind::Tag, short)
        } else if scope == RefScope::All {
            // Under the scope that WALKS these, they must be labelled too, or
            // the lane they open is unexplainable again. The full path is the
            // name: `cmux/last-turn/<sha>` has no meaningful short form.
            match refname.strip_prefix("refs/") {
                Some(rest) if !rest.is_empty() => (RefKind::Other, rest),
                _ => continue,
            }
        } else {
            continue;
        };

        if commit_id.is_empty() || name.is_empty() {
            continue;
        }
        let decoration = RefDecoration {
            name: name.to_string(),
            kind,
            commit_id: commit_id.to_string(),
            is_head: kind == RefKind::Local && !current_branch.is_empty() && name == current_branch,
        };
        match decoration.kind {
            RefKind::Tag => tags.push((decoration, creatordate)),
            RefKind::Other => others.push((decoration, creatordate)),
            _ => decorations.push(decoration),
        }
    }

    // Newest first (creatordate desc, name desc as a deterministic tie-break),
    // keep the newest N, and REMEMBER how many were dropped so the caller can
    // say so. A cap that reports nothing turns a partial label set into an
    // apparently complete one.
    fn cap_newest(items: &mut Vec<(RefDecoration, i64)>, cap: usize) -> usize {
        if items.len() <= cap {
            return 0;
        }
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.name.cmp(&a.0.name)));
        let dropped = items.len() - cap;
        items.truncate(cap);
        dropped
    }
    let tags_dropped = cap_newest(&mut tags, REFS_TAG_CAP);
    let other_dropped = cap_newest(&mut others, REFS_OTHER_CAP);
    decorations.extend(tags.into_iter().map(|(d, _)| d));
    decorations.extend(others.into_iter().map(|(d, _)| d));

    // A detached HEAD points at a commit no branch names; without this the
    // graph shows no "you are here" at all.
    if current_branch.is_empty() {
        if let Ok(head) = git_text(&repo, &["rev-parse", "HEAD"]) {
            let head = head.trim().to_string();
            if !head.is_empty() {
                decorations.push(RefDecoration {
                    name: "HEAD".to_string(),
                    kind: RefKind::Head,
                    commit_id: head,
                    is_head: true,
                });
            }
        }
    }

    // HEAD first, then local branches, then remotes, then tags: the order the
    // chips are drawn in, decided once here rather than in the renderer.
    decorations.sort_by_key(|d| {
        (
            !d.is_head,
            match d.kind {
                RefKind::Head => 0,
                RefKind::Local => 1,
                RefKind::Remote => 2,
                RefKind::Tag => 3,
                RefKind::Other => 4,
            },
            d.name.clone(),
        )
    });
    Ok(RefListing {
        decorations,
        tags_dropped,
        other_dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str], env: &[(&str, &str)]| {
            let mut cmd = Command::new("git");
            cmd.args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir.path());
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("git helper");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"], &[]);
        run(&["config", "user.name", "t"], &[]);
        run(&["config", "user.email", "t@t"], &[]);
        run(&["config", "commit.gpgsign", "false"], &[]);
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        run(&["add", "--", "f.txt"], &[]);
        run(&["commit", "-m", "init"], &[]);
        dir
    }

    fn tag_names(decorations: &[RefDecoration]) -> Vec<String> {
        decorations
            .iter()
            .filter(|d| d.kind == RefKind::Tag)
            .map(|d| d.name.clone())
            .collect()
    }

    #[test]
    fn all_tags_are_kept_when_under_the_cap() {
        let dir = init_repo();
        for i in 0..5 {
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["tag", "-a", "-m", "t", &format!("v0.0.{i}")])
                .env(
                    "GIT_COMMITTER_DATE",
                    format!("2026-01-{:02}T00:00:00Z", i + 1),
                )
                .current_dir(dir.path())
                .output()
                .expect("git tag");
            assert!(
                out.status.success(),
                "tag v0.0.{i} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let listing = list_ref_decorations(dir.path().to_str().unwrap(), RefScope::Named).unwrap();
        assert_eq!(listing.tags_dropped, 0, "five tags are under the cap");
        assert_eq!(listing.truncation_warning(), None);
        let decorations = listing.decorations;
        let names = tag_names(&decorations);
        assert_eq!(names.len(), 5);
    }

    /// A CI-tagged monorepo can carry tens of thousands of tags; shipping
    /// every chip on every graph load bloats the IPC payload. The decoration
    /// listing caps tags at REFS_TAG_CAP and keeps the NEWEST ones.
    #[test]
    fn tag_decorations_cap_to_the_newest_two_hundred() {
        let dir = init_repo();
        for i in 0..250u32 {
            // Spread dates so recency ordering is unambiguous: v0.0.249 is
            // the newest, v0.0.0 the oldest.
            let day = 1 + (i % 28);
            let month = 1 + i / 28;
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["tag", "-a", "-m", "t", &format!("v0.0.{i}")])
                .env(
                    "GIT_COMMITTER_DATE",
                    format!("2026-{month:02}-{day:02}T00:00:00Z"),
                )
                .current_dir(dir.path())
                .output()
                .expect("git tag");
            assert!(
                out.status.success(),
                "tag v0.0.{i} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let listing = list_ref_decorations(dir.path().to_str().unwrap(), RefScope::Named).unwrap();
        assert_eq!(
            listing.tags_dropped, 50,
            "the cap dropped 50 tags and must say so — a truncated label set that reports \
             nothing is indistinguishable from a complete one"
        );
        let note = listing
            .truncation_warning()
            .expect("a truncated listing must produce a warning");
        assert!(note.contains("50 older tag(s)"), "{note}");
        let decorations = listing.decorations;
        let mut names = tag_names(&decorations);
        assert_eq!(names.len(), REFS_TAG_CAP, "cap must hold");
        names.sort();
        // The 50 oldest tags must be gone; every kept name is in the newest 200.
        for i in 0..50u32 {
            assert!(
                !names.contains(&format!("v0.0.{i}")),
                "oldest tag {i} survived"
            );
        }
        for i in 50..250u32 {
            assert!(
                names.contains(&format!("v0.0.{i}")),
                "newest tag {i} dropped"
            );
        }
        // Branches are never subject to the cap.
        assert!(
            decorations
                .iter()
                .any(|d| d.kind == RefKind::Local && d.name == "main"),
            "branch decoration missing"
        );
    }
}
