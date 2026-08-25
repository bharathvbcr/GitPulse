//! Ref decorations: which branches, tags and HEAD point at which commit.
//!
//! The graph is read as much through its labels as through its lanes — a lane
//! with no name on it is just a coloured line — so the rows carry the refs that
//! point at them. They are resolved once per graph load with a single
//! `for-each-ref`, rather than per row, because a repository with thousands of
//! refs would otherwise pay a process spawn for each one.

use serde::{Deserialize, Serialize};

use crate::engine::git_cli::{git_text, validate_repo};

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
    // \x01-separated rather than parsed out of whitespace.
    let raw = git_text(
        &repo,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(refname)%00%(objecttype)%00%(*objectname)%01",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;

    let current_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut decorations = Vec::new();
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
        decorations.push(RefDecoration {
            name: name.to_string(),
            kind,
            commit_id: commit_id.to_string(),
            is_head: kind == RefKind::Local && !current_branch.is_empty() && name == current_branch,
        });
    }

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
