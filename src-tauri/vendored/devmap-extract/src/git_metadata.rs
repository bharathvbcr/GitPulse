//! Filesystem metadata shared by ordinary checkouts, linked worktrees and
//! submodules. Git's `gitdir:` pointer is relative to the worktree; `commondir`
//! is relative to the private Git directory. No process is spawned per event.

use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GitMetadata {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

fn pointer(path: &Path) -> anyhow::Result<Option<String>> {
    const MAX_POINTER_BYTES: u64 = 64 * 1024;
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("Git pointer {path:?} is not a regular file")
        }
        Ok(metadata) if metadata.len() > MAX_POINTER_BYTES => {
            anyhow::bail!("Git pointer {path:?} exceeds {MAX_POINTER_BYTES} bytes")
        }
        Ok(_) => {}
    }
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(MAX_POINTER_BYTES + 1)
        .read_to_string(&mut text)?;
    anyhow::ensure!(
        text.len() as u64 <= MAX_POINTER_BYTES,
        "Git pointer {path:?} grew past its bound"
    );
    let text = text.trim_end_matches(['\r', '\n']);
    anyhow::ensure!(
        !text.is_empty() && !text.contains(['\0', '\r', '\n']),
        "invalid Git pointer {path:?}"
    );
    Ok(Some(text.to_owned()))
}

/// Resolve only the metadata belonging to this worktree, never another nested
/// checkout. Missing metadata is a non-Git tree; unreadable metadata is an error.
pub fn git_metadata(root: &Path) -> anyhow::Result<Option<GitMetadata>> {
    let marker = root.join(".git");
    let git_dir = match std::fs::metadata(&marker) {
        Ok(metadata) if metadata.is_dir() => marker,
        Ok(_) => {
            let text = pointer(&marker)?
                .ok_or_else(|| anyhow::anyhow!("Git pointer vanished: {marker:?}"))?;
            let target = text
                .strip_prefix("gitdir: ")
                .ok_or_else(|| anyhow::anyhow!("invalid gitdir pointer {marker:?}"))?;
            anyhow::ensure!(!target.is_empty(), "empty gitdir pointer {marker:?}");
            root.join(target)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    .canonicalize()?;
    anyhow::ensure!(
        git_dir.is_dir(),
        "Git metadata {git_dir:?} is not a directory"
    );
    let common_dir = match pointer(&git_dir.join("commondir"))? {
        Some(target) => git_dir.join(target).canonicalize()?,
        None => git_dir.clone(),
    };
    anyhow::ensure!(
        common_dir.is_dir(),
        "Git common metadata {common_dir:?} is not a directory"
    );
    Ok(Some(GitMetadata {
        git_dir,
        common_dir,
    }))
}
