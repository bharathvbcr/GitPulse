//! Which directories a macOS watch leaves out of its stream, and what it
//! registers instead.
//!
//! FSEvents is recursive in the kernel, and `fseventsd` pays for every event
//! under a registered root whether or not anyone wants it. A tab that registers
//! its whole worktree therefore makes `fseventsd` work for every file a build
//! writes under `target/`, and every tab covering a checkout that holds other
//! checkouts (`.claude/worktrees/*/target`) pays for all of them. Only the
//! registered roots reduce that work (exclusion paths do not), so a narrowed
//! watch registers the cover of "root minus the excluded set" instead.
//!
//! # The excluded set
//!
//! A directory D under the worktree root is excluded only when ALL of these hold:
//!
//! * (a) `git check-ignore` names D itself. The ignore listing GitPulse already
//!   loads is not a sound basis: it prints `globs/` for a directory whose files
//!   are ignored one by one, and a new non-ignored file there still shows in
//!   `git status`.
//! * (b) no tracked path lies under D (`git ls-files -- D` prints nothing). A
//!   force-added file under an ignored directory keeps showing as modified.
//! * (c) D is heavy: a build-output name, a `CACHEDIR.TAG`, or a nested checkout
//!   (`D/.git`, `D/*/.git`). This keeps the change aimed at what was measured
//!   and leaves generic ignored directories (`out/`, `dist/`) on today's
//!   coverage.
//!
//! Anything that cannot be established means "do not exclude", and a failure of
//! any step means the whole worktree is registered, exactly as before. Unknown
//! is never clean.
//!
//! # What is registered
//!
//! The git directory (and the common one), recursively, as before; then every
//! child entry of every ancestor of an excluded directory (the "split
//! ancestors", the root always among them), except the excluded directories
//! and the split ancestors themselves, which are expanded rather than
//! registered. A new top-level entry is under no registered path, so the split
//! ancestors are also watched by a kqueue ([`super::ancestors`]), which asks the
//! registration to rebuild itself when their entry names change.

use super::{is_build_dir_name, IgnoreRules, IGNORE_QUERY_TIMEOUT};
use crate::engine::git_cli::git_captured_with_stdin_timeout;
use notify::RecursiveMode;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Paths one narrowed registration may hold. Past it the watch registers the
/// whole root, as it always did.
pub(crate) const MAX_REGISTERED_PATHS: usize = 256;
/// Heavy candidates one pass will ask git about. Fewer exclusions only ever
/// registers more, so the cut is safe.
const MAX_HEAVY_CANDIDATES: usize = 128;
/// Entries of a candidate looked at for a nested checkout; past it, not heavy.
const MAX_NESTED_SCAN: usize = 512;
/// Entries of one split ancestor read before the layout is called too big.
const MAX_ANCESTOR_ENTRIES: usize = MAX_REGISTERED_PATHS * 4;

/// How much of the worktree a watch registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WatchScope {
    /// Leave heavy ignored directories out of the stream (macOS).
    #[default]
    Narrow,
    /// Register the whole worktree root.
    Whole,
}

impl WatchScope {
    /// `None` and `"narrow"` are the default; `"whole"` is today's scope.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("narrow") => Ok(Self::Narrow),
            Some("whole") => Ok(Self::Whole),
            Some(other) => Err(format!(
                "Unknown watch scope `{other}` (expected `narrow` or `whole`)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Narrow => "narrow",
            Self::Whole => "whole",
        }
    }
}

/// What a directory entry is, as far as the registration cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Dir,
    /// Not registered (its target is registered, if it is in the tree); its
    /// target is part of the identity, so a retarget changes the entry.
    Symlink(OsString),
    Other,
}

/// One narrowed registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// Worktree-relative, `/`-separated, original case, sorted, no nesting.
    pub excluded: Vec<String>,
    /// Directories whose children are registered one by one; `""` is the root.
    pub ancestors: BTreeSet<String>,
    /// Every entry of every split ancestor when the layout was made.
    pub entries: BTreeMap<String, BTreeMap<OsString, EntryKind>>,
    /// Everything registered, git directories first.
    pub paths: Vec<(PathBuf, RecursiveMode)>,
}

/// What a watch should register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The whole root, as before. `Some` says why a narrowing was refused.
    Whole(Option<String>),
    Narrow(Layout),
}

impl Plan {
    /// The same registration, whatever the reason for it.
    pub(crate) fn same_registration(&self, other: &Plan) -> bool {
        match (self, other) {
            (Plan::Whole(_), Plan::Whole(_)) => true,
            (Plan::Narrow(a), Plan::Narrow(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) fn excluded(&self) -> &[String] {
        match self {
            Plan::Narrow(layout) => &layout.excluded,
            Plan::Whole(_) => &[],
        }
    }
}

/// The plan for `rules`: runs the (c) screen, then (a) and (b) on what passed.
pub(crate) fn plan(
    root: &Path,
    git_dir: &Path,
    common: Option<&Path>,
    rules: &IgnoreRules,
) -> Plan {
    finish(root, git_dir, common, excluded_set(root, rules))
}

/// The plan for the exclusions `current` once (a) and (b) are asked again: what
/// a write to the index (`git add -f target/x`) may have changed.
pub(crate) fn replan(
    root: &Path,
    git_dir: &Path,
    common: Option<&Path>,
    current: &[String],
) -> Plan {
    let survivors = confirm(root, current).map(drop_nested);
    finish(root, git_dir, common, survivors)
}

fn finish(
    root: &Path,
    git_dir: &Path,
    common: Option<&Path>,
    excluded: Result<Vec<String>, String>,
) -> Plan {
    match excluded {
        Err(reason) => Plan::Whole(Some(reason)),
        Ok(excluded) if excluded.is_empty() => Plan::Whole(None),
        Ok(excluded) => match layout(root, git_dir, common, excluded) {
            Ok(layout) => Plan::Narrow(layout),
            Err(reason) => Plan::Whole(Some(reason)),
        },
    }
}

/// The directories to leave out, or why that could not be established.
fn excluded_set(root: &Path, rules: &IgnoreRules) -> Result<Vec<String>, String> {
    if let Some(error) = &rules.load_error {
        return Err(format!("ignore listing failed or truncated: {error}"));
    }
    let mut heavy = Vec::new();
    for dir in &rules.dirs {
        if is_heavy(root, dir)? {
            heavy.push(dir.clone());
            if heavy.len() >= MAX_HEAVY_CANDIDATES {
                break;
            }
        }
    }
    if heavy.is_empty() {
        // Nothing to ask git about: no spawn.
        return Ok(Vec::new());
    }
    confirm(root, &heavy).map(drop_nested)
}

/// (c). A directory that has vanished is not a candidate; any other failure to
/// look is an error, never "not heavy".
fn is_heavy(root: &Path, relative: &str) -> Result<bool, String> {
    let dir = root.join(relative);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("could not look at {relative}: {e}")),
    }
    let name = relative.rsplit('/').next().unwrap_or(relative);
    if is_build_dir_name(name) || exists(&dir.join("CACHEDIR.TAG"))? || exists(&dir.join(".git"))? {
        return Ok(true);
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("could not read {relative}: {e}")),
    };
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_NESTED_SCAN {
            return Ok(false);
        }
        let entry = entry.map_err(|e| format!("could not read {relative}: {e}"))?;
        if exists(&entry.path().join(".git"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn exists(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        // A parent that is not a directory is "not there" too.
        Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => Ok(false),
        Err(e) => Err(format!("could not look at {}: {e}", path.display())),
    }
}

/// (a) then (b) for `candidates`: the ones git ignores by a rule matching the
/// directory itself and that hold no tracked path. A directory named `:target`
/// or `a*b` is itself to both queries (see the comments below).
pub(crate) fn confirm(root: &Path, candidates: &[String]) -> Result<Vec<String>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // `check-ignore` takes pathspecs but refuses `--literal-pathspecs` (it
    // supports no magic at all), so a name that starts with `:` would be read as
    // magic. A leading `./` makes every name a plain path, and it does not
    // expand wildcards. The answer is read back without it.
    let mut stdin = Vec::new();
    for candidate in candidates {
        stdin.extend_from_slice(b"./");
        stdin.extend_from_slice(candidate.as_bytes());
        stdin.push(0);
    }
    let run = git_captured_with_stdin_timeout(
        root,
        &["check-ignore", "-z", "--stdin"],
        &stdin,
        IGNORE_QUERY_TIMEOUT,
    )
    .map_err(|e| format!("check-ignore failed: {}", e.message()))?;
    if let Some(reason) = &run.incomplete {
        return Err(format!("check-ignore output {}", reason.describe()));
    }
    // Exit 1 is "none of them is ignored", which is an answer.
    if !run.success && run.status_code != 1 {
        return Err(format!(
            "check-ignore exited {}: {}",
            run.status_code,
            String::from_utf8_lossy(&run.stderr[..run.stderr.len().min(300)])
        ));
    }
    let named: BTreeSet<&[u8]> = run
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| name.strip_prefix(b"./").unwrap_or(name))
        .collect();
    let ignored: Vec<&String> = candidates
        .iter()
        .filter(|candidate| named.contains(candidate.as_bytes()))
        .collect();
    if ignored.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--"];
    args.extend(ignored.iter().map(|candidate| candidate.as_str()));
    let run = git_captured_with_stdin_timeout(root, &args, &[], IGNORE_QUERY_TIMEOUT)
        .map_err(|e| format!("ls-files failed: {}", e.message()))?;
    if let Some(reason) = &run.incomplete {
        return Err(format!("ls-files output {}", reason.describe()));
    }
    if !run.success {
        return Err(format!(
            "ls-files exited {}: {}",
            run.status_code,
            String::from_utf8_lossy(&run.stderr[..run.stderr.len().min(300)])
        ));
    }
    let tracked = String::from_utf8_lossy(&run.stdout);
    let tracked: Vec<&str> = tracked.split('\0').filter(|p| !p.is_empty()).collect();
    Ok(ignored
        .into_iter()
        .filter(|candidate| {
            !tracked
                .iter()
                .any(|path| is_same_or_under(path, candidate.as_str()))
        })
        .cloned()
        .collect())
}

fn is_same_or_under(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// An excluded directory under another excluded directory adds nothing.
fn drop_nested(mut dirs: Vec<String>) -> Vec<String> {
    dirs.sort();
    dirs.dedup();
    let mut kept: Vec<String> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !kept.iter().any(|outer| is_same_or_under(&dir, outer)) {
            kept.push(dir);
        }
    }
    kept
}

fn read_entries(dir: &Path) -> Result<BTreeMap<OsString, EntryKind>, String> {
    let mut entries = BTreeMap::new();
    let reader =
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    for entry in reader {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        if entries.len() >= MAX_ANCESTOR_ENTRIES {
            return Err(format!("{} has too many entries", dir.display()));
        }
        let kind = match entry.file_type() {
            Ok(kind) if kind.is_symlink() => EntryKind::Symlink(
                std::fs::read_link(entry.path())
                    .map(PathBuf::into_os_string)
                    .unwrap_or_default(),
            ),
            Ok(kind) if kind.is_dir() => EntryKind::Dir,
            Ok(kind) if kind.is_file() => EntryKind::File,
            Ok(_) => EntryKind::Other,
            Err(e) => return Err(format!("could not look at {}: {e}", entry.path().display())),
        };
        entries.insert(entry.file_name(), kind);
    }
    Ok(entries)
}

fn layout(
    root: &Path,
    git_dir: &Path,
    common: Option<&Path>,
    excluded: Vec<String>,
) -> Result<Layout, String> {
    let mut ancestors = BTreeSet::new();
    for dir in &excluded {
        let mut current = dir.as_str();
        loop {
            current = current.rsplit_once('/').map_or("", |(parent, _)| parent);
            ancestors.insert(current.to_string());
            if current.is_empty() {
                break;
            }
        }
    }
    let mut paths = vec![(git_dir.to_path_buf(), RecursiveMode::Recursive)];
    if let Some(common) = common {
        if common != git_dir && common.exists() {
            paths.push((common.to_path_buf(), RecursiveMode::Recursive));
        }
    }
    let mut entries = BTreeMap::new();
    for ancestor in &ancestors {
        let base = if ancestor.is_empty() {
            root.to_path_buf()
        } else {
            root.join(ancestor)
        };
        let names = read_entries(&base)?;
        for (name, kind) in &names {
            let relative = if ancestor.is_empty() {
                name.to_string_lossy().into_owned()
            } else {
                format!("{ancestor}/{}", name.to_string_lossy())
            };
            if excluded.contains(&relative) || ancestors.contains(&relative) {
                continue;
            }
            let path = base.join(name);
            if path == git_dir || common.is_some_and(|common| path == common) {
                continue;
            }
            match kind {
                EntryKind::File => paths.push((path, RecursiveMode::NonRecursive)),
                EntryKind::Dir => paths.push((path, RecursiveMode::Recursive)),
                EntryKind::Symlink(_) | EntryKind::Other => {}
            }
        }
        entries.insert(ancestor.clone(), names);
    }
    if paths.len() > MAX_REGISTERED_PATHS {
        return Err(format!(
            "{} paths to register, over the {MAX_REGISTERED_PATHS} limit",
            paths.len()
        ));
    }
    Ok(Layout {
        excluded,
        ancestors,
        entries,
        paths,
    })
}

/// True when any split ancestor's entries are not what `layout` recorded. A
/// directory that cannot be read counts as changed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn entries_changed(root: &Path, layout: &Layout) -> bool {
    layout.entries.iter().any(|(ancestor, recorded)| {
        let base = if ancestor.is_empty() {
            root.to_path_buf()
        } else {
            root.join(ancestor)
        };
        read_entries(&base).map_or(true, |now| &now != recorded)
    })
}

/// Absolute paths of the split ancestors, for the kqueue.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn ancestor_dirs(root: &Path, layout: &Layout) -> Vec<PathBuf> {
    layout
        .ancestors
        .iter()
        .map(|ancestor| {
            if ancestor.is_empty() {
                root.to_path_buf()
            } else {
                root.join(ancestor)
            }
        })
        .collect()
}

/// What changed between two narrowed layouts, as the entries to announce:
/// created (the new kind) or removed. Directories that left the excluded set
/// are announced as created, since nothing under them was watched until now.
pub(crate) fn differences(
    root: &Path,
    old: &Layout,
    new: &Layout,
) -> Vec<(PathBuf, Option<EntryKind>)> {
    let mut changes = Vec::new();
    for ancestor in old.ancestors.iter().chain(new.ancestors.iter()) {
        let base = if ancestor.is_empty() {
            root.to_path_buf()
        } else {
            root.join(ancestor)
        };
        let before = old.entries.get(ancestor);
        let after = new.entries.get(ancestor);
        if let Some(after) = after {
            for (name, kind) in after {
                if before.and_then(|before| before.get(name)) != Some(kind) {
                    changes.push((base.join(name), Some(kind.clone())));
                }
            }
        }
        if let Some(before) = before {
            for name in before.keys() {
                if after.is_none_or(|after| !after.contains_key(name)) {
                    changes.push((base.join(name), None));
                }
            }
        }
    }
    for dir in old.excluded.iter().filter(|d| !new.excluded.contains(d)) {
        changes.push((root.join(dir), Some(EntryKind::Dir)));
    }
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    changes.dedup_by(|a, b| a.0 == b.0);
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, trust_repo};
    use tempfile::TempDir;

    fn repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        trust_repo(dir.path());
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn plan_of(root: &Path) -> Plan {
        let rules = IgnoreRules::load(root);
        assert!(rules.load_error.is_none(), "{:?}", rules.load_error);
        plan(root, &root.join(".git"), None, &rules)
    }

    fn excluded_of(plan: &Plan) -> Vec<String> {
        plan.excluded().to_vec()
    }

    #[test]
    fn a_scope_is_parsed_from_the_setting_and_the_default_is_narrow() {
        assert_eq!(WatchScope::parse(None), Ok(WatchScope::Narrow));
        assert_eq!(WatchScope::parse(Some("narrow")), Ok(WatchScope::Narrow));
        assert_eq!(WatchScope::parse(Some("whole")), Ok(WatchScope::Whole));
        assert!(WatchScope::parse(Some("Whole")).is_err());
        assert!(WatchScope::parse(Some("")).is_err());
        assert_eq!(WatchScope::default(), WatchScope::Narrow);
    }

    #[test]
    fn a_directory_ignored_by_its_own_rule_and_named_like_build_output_is_excluded() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\n");
        write(&root, "target/debug/x.o", "x");
        write(&root, "src/main.rs", "fn main() {}");
        write(&root, "README.md", "r");
        let plan = plan_of(&root);
        assert_eq!(excluded_of(&plan), vec!["target".to_string()]);
        let Plan::Narrow(layout) = plan else {
            panic!("expected a narrowed plan")
        };
        assert_eq!(layout.ancestors, BTreeSet::from([String::new()]));
        let registered: Vec<_> = layout
            .paths
            .iter()
            .map(|(p, _)| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(registered.contains(&".git".to_string()));
        assert!(registered.contains(&"README.md".to_string()));
        assert!(registered.contains(&"src".to_string()));
        assert!(registered.contains(&".gitignore".to_string()));
        assert!(!registered.contains(&"target".to_string()));
        let kind = |name: &str| {
            layout
                .paths
                .iter()
                .find(|(p, _)| p.ends_with(name))
                .map(|(_, mode)| *mode)
        };
        assert_eq!(kind("README.md"), Some(RecursiveMode::NonRecursive));
        assert_eq!(kind("src"), Some(RecursiveMode::Recursive));
    }

    #[test]
    fn ignored_files_matched_one_by_one_do_not_exclude_their_directory() {
        let (_dir, root) = repo();
        // The listing prints `globs/` and `target/`; git does not ignore either
        // directory by a rule of its own, so a new plain file there would show.
        write(&root, ".gitignore", "*.o\n*.log\n");
        write(&root, "globs/a.o", "x");
        write(&root, "globs/b.log", "x");
        write(&root, "target/a.o", "x");
        let rules = IgnoreRules::load(&root);
        assert!(rules.dirs.iter().any(|d| d == "globs"), "{:?}", rules.dirs);
        assert!(rules.dirs.iter().any(|d| d == "target"), "{:?}", rules.dirs);
        assert_eq!(
            confirm(&root, &["globs".to_string(), "target".to_string()]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(plan_of(&root), Plan::Whole(None));
    }

    #[test]
    fn a_directory_that_re_includes_a_file_is_not_excluded() {
        let (_dir, root) = repo();
        write(
            &root,
            ".gitignore",
            "keepdir/*\n!keepdir/keep\nnode_modules/*\n!node_modules/keep\n",
        );
        write(&root, "keepdir/other", "x");
        write(&root, "keepdir/keep", "x");
        write(&root, "node_modules/other", "x");
        write(&root, "node_modules/keep", "x");
        assert_eq!(
            confirm(&root, &["keepdir".to_string(), "node_modules".to_string()]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(excluded_of(&plan_of(&root)), Vec::<String>::new());
    }

    #[test]
    fn a_directory_holding_a_tracked_file_is_not_excluded() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\nbuild/\n");
        write(&root, "target/keep.rs", "x");
        write(&root, "target/other.o", "x");
        write(&root, "build/keep.rs", "x");
        git_in(&root, &["add", "-f", "target/keep.rs", "build/keep.rs"]);
        git_in(&root, &["commit", "-m", "track under ignored dirs"]);
        // (b) on its own: both are ignored by a rule of their own, both hold a
        // tracked path.
        assert_eq!(
            confirm(&root, &["target".to_string(), "build".to_string()]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(excluded_of(&plan_of(&root)), Vec::<String>::new());
    }

    #[test]
    fn a_sibling_without_tracked_files_is_still_excluded() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\ntarget2/\n");
        write(&root, "target/keep.rs", "x");
        write(&root, "target2/a.o", "x");
        git_in(&root, &["add", "-f", "target/keep.rs"]);
        let plan = plan_of(&root);
        assert_eq!(excluded_of(&plan), vec!["target2".to_string()]);
    }

    #[test]
    fn a_nested_checkout_directory_is_excluded_and_its_parent_is_split() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", ".claude/worktrees/\n");
        write(&root, ".claude/settings.json", "{}");
        write(&root, ".claude/worktrees/w1/.git", "gitdir: /elsewhere\n");
        write(&root, ".claude/worktrees/w1/src/x.rs", "x");
        write(&root, "a.txt", "a");
        let plan = plan_of(&root);
        assert_eq!(excluded_of(&plan), vec![".claude/worktrees".to_string()]);
        let Plan::Narrow(layout) = plan else {
            panic!("expected a narrowed plan")
        };
        assert_eq!(
            layout.ancestors,
            BTreeSet::from([String::new(), ".claude".to_string()])
        );
        let registered: Vec<_> = layout
            .paths
            .iter()
            .map(|(p, _)| {
                // As git spells it: `/`, whatever the platform's separator.
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert!(registered.contains(&".claude/settings.json".to_string()));
        assert!(
            !registered.contains(&".claude".to_string()),
            "{registered:?}"
        );
        assert!(!registered
            .iter()
            .any(|p| p.starts_with(".claude/worktrees")));
    }

    #[test]
    fn a_colon_named_directory_is_handled_literally() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "/:target/\n");
        if cfg!(windows) {
            // Windows cannot hold the name, so there is nothing for it to
            // exclude: the ignore line names no directory, and the plan is
            // the whole root.
            let refused = std::fs::create_dir(root.join(":target")).unwrap_err();
            assert_eq!(refused.kind(), std::io::ErrorKind::InvalidFilename);
            write(&root, "a.txt", "a");
            assert_eq!(plan_of(&root), Plan::Whole(None));
            return;
        }
        write(
            &root,
            ":target/CACHEDIR.TAG",
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        );
        write(&root, ":target/a.o", "x");
        write(&root, "a.txt", "a");
        assert_eq!(excluded_of(&plan_of(&root)), vec![":target".to_string()]);
    }

    #[test]
    fn an_ignored_directory_that_is_not_heavy_is_not_excluded() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "out/\n");
        write(&root, "out/a.o", "x");
        assert_eq!(plan_of(&root), Plan::Whole(None));
    }

    #[test]
    fn nested_exclusions_collapse_to_the_outer_one() {
        assert_eq!(
            drop_nested(vec![
                "a/b/target".into(),
                "a".into(),
                "ab".into(),
                "a/b".into()
            ]),
            vec!["a".to_string(), "ab".to_string()]
        );
    }

    #[test]
    fn over_the_path_cap_the_whole_root_is_registered() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\n");
        write(&root, "target/a.o", "x");
        for i in 0..MAX_REGISTERED_PATHS + 20 {
            write(&root, &format!("f{i}.txt"), "x");
        }
        let Plan::Whole(Some(reason)) = plan_of(&root) else {
            panic!("expected a refusal")
        };
        assert!(reason.contains("limit"), "{reason}");
    }

    #[test]
    fn a_failed_or_truncated_listing_registers_the_whole_root() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\n");
        write(&root, "target/a.o", "x");
        let mut rules = IgnoreRules::load(&root);
        rules.load_error = Some("ignore directory list truncated".into());
        rules.truncated = true;
        let Plan::Whole(Some(reason)) = plan(&root, &root.join(".git"), None, &rules) else {
            panic!("expected a refusal")
        };
        assert!(
            reason.contains("ignore listing failed or truncated"),
            "{reason}"
        );
    }

    #[test]
    fn a_failed_git_query_registers_the_whole_root() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\n");
        write(&root, "target/a.o", "x");
        let rules = IgnoreRules::load(&root);
        // Not a repository: both git calls fail, which is not "nothing ignored".
        let elsewhere = TempDir::new().unwrap();
        let plan = plan(
            elsewhere.path(),
            &root.join(".git"),
            None,
            &IgnoreRules {
                dirs: vec!["target".into()],
                ..rules
            },
        );
        // `target` does not exist under the other root, so nothing is heavy and
        // nothing is asked; the failure path is the confirm step itself.
        assert_eq!(plan, Plan::Whole(None));
        write(elsewhere.path(), "target/CACHEDIR.TAG", "x");
        let rules = IgnoreRules {
            dirs: vec!["target".into()],
            ..IgnoreRules::builtin()
        };
        assert!(matches!(
            super::plan(elsewhere.path(), &root.join(".git"), None, &rules),
            Plan::Whole(Some(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_changed_entry_name_or_a_retargeted_link_is_a_change_and_an_idle_directory_is_not() {
        let (_dir, root) = repo();
        write(&root, ".gitignore", "target/\n");
        write(&root, "target/a.o", "x");
        write(&root, "a.txt", "a");
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
        let Plan::Narrow(layout) = plan_of(&root) else {
            panic!("expected a narrowed plan")
        };
        assert!(!entries_changed(&root, &layout));
        // The atomic-save shape: a temporary name came and went.
        write(&root, ".a.txt.tmp", "b");
        std::fs::rename(root.join(".a.txt.tmp"), root.join("a.txt")).unwrap();
        assert!(!entries_changed(&root, &layout));
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("elsewhere", root.join("link")).unwrap();
        assert!(entries_changed(&root, &layout));
        let Plan::Narrow(after) = plan_of(&root) else {
            panic!("expected a narrowed plan")
        };
        assert_eq!(
            differences(&root, &layout, &after),
            vec![(
                root.join("link"),
                Some(after.entries[""][std::ffi::OsStr::new("link")].clone())
            )]
        );
        write(&root, "new.txt", "n");
        assert!(entries_changed(&root, &layout));
    }
}
