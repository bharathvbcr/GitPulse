//! Ref decorations: which branches, tags and HEAD point at which commit.
//!
//! The graph is read as much through its labels as through its lanes — a lane
//! with no name on it is just a coloured line — so the rows carry the refs that
//! point at them. They are resolved once per graph load with a single
//! `for-each-ref`, rather than per row, because a repository with thousands of
//! refs would otherwise pay a process spawn for each one.

use serde::{Deserialize, Serialize};

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_reader::REFS_TAG_CAP;

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

/// Reads every branch, remote branch and tag, and marks the checked-out one.
pub fn list_ref_decorations(repo_path: &str) -> Result<Vec<RefDecoration>, String> {
    let repo = validate_repo(repo_path)?;

    // A branch name can contain anything but the ASCII control characters and a
    // few specials, so the fields are NUL-separated and the records
    // \x01-separated rather than parsed out of whitespace. Tags carry their
    // creatordate so a CI-tagged monorepo with tens of thousands of them can
    // be capped to the newest REFS_TAG_CAP instead of shipping every chip on
    // every graph load.
    let raw = git_text(
        &repo,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(refname)%00%(objecttype)%00%(*objectname)%00%(creatordate:unix)%01",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;

    let current_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut decorations = Vec::new();
    // Tags are held aside so the cap can keep the NEWEST ones; branches and
    // remotes go straight through uncapped.
    let mut tags: Vec<(RefDecoration, i64)> = Vec::new();
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
        if decoration.kind == RefKind::Tag {
            tags.push((decoration, creatordate));
        } else {
            decorations.push(decoration);
        }
    }

    if tags.len() > REFS_TAG_CAP {
        // Newest first (creatordate desc, name desc as deterministic
        // tie-break), keep the newest REFS_TAG_CAP, then hand back only those.
        tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.name.cmp(&a.0.name)));
        tags.truncate(REFS_TAG_CAP);
    }
    decorations.extend(tags.into_iter().map(|(d, _)| d));

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
            },
            d.name.clone(),
        )
    });
    Ok(decorations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str], env: &[(&str, &str)]| {
            let mut cmd = Command::new("git");
            cmd.args(["-c", "user.name=t", "-c", "user.email=t@t"])
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
            Command::new("git")
                .args(["tag", "-a", "-m", "t", &format!("v0.0.{i}")])
                .env(
                    "GIT_COMMITTER_DATE",
                    format!("2026-01-{:02}T00:00:00Z", i + 1),
                )
                .current_dir(dir.path())
                .output()
                .expect("git tag");
        }
        let decorations = list_ref_decorations(dir.path().to_str().unwrap()).unwrap();
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
            Command::new("git")
                .args(["tag", "-a", "-m", "t", &format!("v0.0.{i}")])
                .env(
                    "GIT_COMMITTER_DATE",
                    format!("2026-{month:02}-{day:02}T00:00:00Z"),
                )
                .current_dir(dir.path())
                .output()
                .expect("git tag");
        }
        let decorations = list_ref_decorations(dir.path().to_str().unwrap()).unwrap();
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
