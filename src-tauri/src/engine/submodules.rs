//! Submodules: what a repository embeds, and whether those embeds are usable.
//!
//! GitPulse previously knew submodules existed only as disk usage — the
//! storage auditor counts `.git/modules` — so a clone whose submodules were
//! never initialized presented as a repository full of empty directories with
//! no explanation and no way to fix it from the app.
//!
//! Two sources are combined, because neither is sufficient alone:
//!
//! * `.gitmodules`, read through `git config -z`, is authoritative for name,
//!   path and URL, and is NUL-separated so a path containing spaces survives.
//! * `git submodule status` is the only source for the *state* flag and the
//!   recorded commit, but its output is a single whitespace-delimited line
//!   whose path field is ambiguous against the trailing `(describe)` suffix.
//!
//! Matching them by path gives both halves without trusting either one's weak
//! spot. A submodule recorded in the index but missing from `.gitmodules` is
//! still reported — that mismatch is itself a defect the user needs to see.

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_writer::repo_mutation_lock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Upper bound on submodules reported in one call. Large monorepos reach the
/// low hundreds; the bound exists so a corrupt `.gitmodules` cannot produce an
/// unbounded payload.
const MAX_SUBMODULES: usize = 500;

/// The state `git submodule status` reports through its leading flag column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmoduleState {
    /// Not initialized — the working directory is empty. Flag `-`.
    Uninitialized,
    /// Checked out at the commit the superproject records. Flag ` `.
    UpToDate,
    /// Checked out at a DIFFERENT commit than the superproject records.
    /// Flag `+`. Committing the superproject now would move the pointer.
    CommitDiffers,
    /// Merge conflicts inside the submodule. Flag `U`.
    Conflicted,
}

impl SubmoduleState {
    fn from_flag(flag: char) -> Option<Self> {
        match flag {
            '-' => Some(SubmoduleState::Uninitialized),
            ' ' => Some(SubmoduleState::UpToDate),
            '+' => Some(SubmoduleState::CommitDiffers),
            'U' => Some(SubmoduleState::Conflicted),
            _ => None,
        }
    }

    /// True when the submodule is not usable as checked out.
    pub fn needs_attention(self) -> bool {
        !matches!(self, SubmoduleState::UpToDate)
    }
}

/// One embedded repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleInfo {
    /// The `[submodule "<name>"]` section name. Falls back to the path when
    /// the entry exists only in the index.
    pub name: String,
    /// Path within the superproject.
    pub path: String,
    /// Configured URL, absent when the entry is not in `.gitmodules`.
    pub url: Option<String>,
    /// The commit the superproject records, when status reported one.
    pub oid: Option<String>,
    /// `git describe` output status appends, e.g. `heads/main`.
    pub described: Option<String>,
    pub state: SubmoduleState,
    /// True when the entry appears in the index but not in `.gitmodules`, so
    /// no URL exists to initialize it from.
    pub orphaned: bool,
}

/// `cmd_list_submodules` payload. A bare `Vec<SubmoduleInfo>` could not say
/// when the listing cap dropped embeds, so a 501-submodule repo looked like
/// a complete 500-submodule one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleList {
    pub submodules: Vec<SubmoduleInfo>,
    pub truncated: bool,
}

/// Parses one `git submodule status` line.
///
/// Shape: `<flag><oid> <path>` with an optional ` (<describe>)` suffix. The
/// path is taken as everything between the oid and that suffix rather than by
/// splitting on whitespace, because a submodule path may contain spaces and
/// splitting would truncate it to its first word — silently reporting a
/// different, non-existent submodule.
fn parse_status_line(
    line: &str,
) -> Option<(SubmoduleState, Option<String>, String, Option<String>)> {
    let mut chars = line.chars();
    let state = SubmoduleState::from_flag(chars.next()?)?;
    let rest = chars.as_str();
    let (oid, after_oid) = match rest.split_once(' ') {
        Some((oid, after)) => (oid, after),
        // A flag with no space after the oid is malformed; treating the whole
        // remainder as a path would invent an entry.
        None => return None,
    };
    let oid =
        (!oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit())).then(|| oid.to_string());

    let after_oid = after_oid.trim_end();
    // Strip a trailing parenthesised describe, if present.
    let (path, described) = match after_oid.strip_suffix(')').and_then(|head| {
        head.rfind(" (")
            .map(|at| (&head[..at], head[at + 2..].to_string()))
    }) {
        Some((path, described)) => (path, Some(described)),
        None => (after_oid, None),
    };
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    Some((state, oid, path.to_string(), described))
}

/// Parses `git config -f .gitmodules --list -z` into path/url keyed by name.
fn parse_gitmodules(raw: &str) -> BTreeMap<String, (Option<String>, Option<String>)> {
    let mut by_name: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for record in raw.split('\0').filter(|r| !r.is_empty()) {
        let Some((key, value)) = record.split_once('\n') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("submodule.") else {
            continue;
        };
        // A submodule name routinely contains dots and slashes ("vendor/lib"),
        // so the field is the LAST segment and the name is everything before.
        let Some((name, field)) = rest.rsplit_once('.') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let entry = by_name.entry(name.to_string()).or_default();
        match field.to_ascii_lowercase().as_str() {
            "path" => entry.0 = Some(value.to_string()),
            "url" => entry.1 = Some(value.to_string()),
            _ => {}
        }
    }
    by_name
}

/// Joins the two sources into the reported list.
fn assemble(status_raw: &str, gitmodules_raw: &str) -> SubmoduleList {
    let configured = parse_gitmodules(gitmodules_raw);
    // Index by path: it is the only field both sources share.
    let by_path: BTreeMap<&str, (&String, Option<&String>)> = configured
        .iter()
        .filter_map(|(name, (path, url))| path.as_deref().map(|p| (p, (name, url.as_ref()))))
        .collect();

    let mut out = Vec::new();
    let mut truncated = false;
    for line in status_raw.lines() {
        if line.is_empty() {
            continue;
        }
        if out.len() >= MAX_SUBMODULES {
            truncated = true;
            break;
        }
        let Some((state, oid, path, described)) = parse_status_line(line) else {
            continue;
        };
        let matched = by_path.get(path.as_str());
        out.push(SubmoduleInfo {
            name: matched
                .map(|(n, _)| (*n).clone())
                .unwrap_or_else(|| path.clone()),
            url: matched.and_then(|(_, u)| u.cloned()),
            // An entry the index knows and `.gitmodules` does not cannot be
            // initialized — there is no URL to clone from. Surfacing it is the
            // only way the user learns why `submodule update` will not help.
            orphaned: matched.is_none(),
            path,
            oid,
            described,
            state,
        });
    }
    SubmoduleList {
        submodules: out,
        truncated,
    }
}

/// Lists the superproject's submodules and their states.
pub fn list(repo_path: &str) -> Result<SubmoduleList, String> {
    let repo = validate_repo(repo_path)?;
    list_in(&repo)
}

fn list_in(repo: &Path) -> Result<SubmoduleList, String> {
    // Both calls fail on a repository with no submodules at all (no
    // `.gitmodules`, nothing in the index). That is an empty answer, not an
    // error worth failing a status refresh over.
    let status = git_text(repo, &["submodule", "status"]).unwrap_or_default();
    let gitmodules =
        git_text(repo, &["config", "-f", ".gitmodules", "--list", "-z"]).unwrap_or_default();
    Ok(assemble(&status, &gitmodules))
}

/// What may be done to submodules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SubmoduleChange {
    /// Clone and check out submodules that are not yet initialized.
    /// `path: None` covers every submodule.
    Update {
        path: Option<String>,
        recursive: bool,
    },
    /// Rewrite each submodule's recorded URL from `.gitmodules` — the fix
    /// after an upstream moves.
    Sync {
        path: Option<String>,
        recursive: bool,
    },
    /// Remove a submodule's working tree and its config entry.
    Deinit { path: String, force: bool },
}

impl SubmoduleChange {
    pub fn argv(&self) -> Vec<&str> {
        match self {
            SubmoduleChange::Update { path, recursive } => {
                let mut argv = vec!["git", "submodule", "update", "--init"];
                if *recursive {
                    argv.push("--recursive");
                }
                if let Some(path) = path {
                    // `--` ends option parsing so a path can never be read as
                    // a flag, matching the reader side's pathspec discipline.
                    argv.push("--");
                    argv.push(path);
                }
                argv
            }
            SubmoduleChange::Sync { path, recursive } => {
                let mut argv = vec!["git", "submodule", "sync"];
                if *recursive {
                    argv.push("--recursive");
                }
                if let Some(path) = path {
                    argv.push("--");
                    argv.push(path);
                }
                argv
            }
            SubmoduleChange::Deinit { path, force } => {
                let mut argv = vec!["git", "submodule", "deinit"];
                if *force {
                    argv.push("--force");
                }
                argv.push("--");
                argv.push(path);
                argv
            }
        }
    }

    /// True when the change clones from the network.
    fn is_network(&self) -> bool {
        matches!(self, SubmoduleChange::Update { .. })
    }

    fn target_path(&self) -> Option<&str> {
        match self {
            SubmoduleChange::Update { path, .. } | SubmoduleChange::Sync { path, .. } => {
                path.as_deref()
            }
            SubmoduleChange::Deinit { path, .. } => Some(path.as_str()),
        }
    }
}

/// Rejects a submodule path that must never reach argv.
///
/// The path is a pathspec inside the superproject, so it is held to the same
/// containment rules as any other file path this app passes to git: relative,
/// no traversal, no NUL, no leading dash.
fn validate_submodule_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') || path.chars().any(char::is_control) {
        return Err("Invalid submodule path".into());
    }
    if path.starts_with('-') {
        return Err("Submodule path must not start with '-'".into());
    }
    if Path::new(path).is_absolute() {
        return Err("Submodule path must be relative to the repository".into());
    }
    for component in path.split('/') {
        if component == ".." {
            return Err("Submodule path escapes the repository".into());
        }
    }
    Ok(())
}

/// Applies a submodule change, judging the rendered argv under the repo lock.
pub fn apply_with<J, V>(
    repo_path: &str,
    change: &SubmoduleChange,
    judge: J,
) -> Result<(V, String), String>
where
    J: FnOnce(&[&str]) -> Result<V, String>,
{
    let repo = validate_repo(repo_path)?;
    if let Some(path) = change.target_path() {
        validate_submodule_path(path)?;
    }
    let lock = repo_mutation_lock(&repo);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // A named path must actually be a submodule. Without this, a typo becomes
    // git's "pathspec did not match" — which reads as a missing file rather
    // than a missing submodule.
    if let Some(path) = change.target_path() {
        let known = list_in(&repo)?;
        if !known.submodules.iter().any(|s| s.path == path) {
            return Err(format!("No submodule at '{path}'"));
        }
    }

    let argv = change.argv();
    let verdict = judge(&argv)?;
    let output = if change.is_network() {
        crate::engine::git_cli::git_text_network(repo.as_path(), &argv[1..])?
    } else {
        git_text(repo.as_path(), &argv[1..])?
    };
    Ok((verdict, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_state_flag() {
        let cases = [
            (" abc123 vendor/lib (heads/main)", SubmoduleState::UpToDate),
            ("-abc123 vendor/lib", SubmoduleState::Uninitialized),
            (
                "+abc123 vendor/lib (heads/main)",
                SubmoduleState::CommitDiffers,
            ),
            ("Uabc123 vendor/lib", SubmoduleState::Conflicted),
        ];
        for (line, expected) in cases {
            let (state, _, path, _) = parse_status_line(line).expect(line);
            assert_eq!(state, expected, "line: {line}");
            assert_eq!(path, "vendor/lib");
        }
    }

    #[test]
    fn a_path_containing_spaces_survives_intact() {
        // Splitting on whitespace would truncate this to "vendor/my", naming a
        // submodule that does not exist.
        let (_, _, path, described) =
            parse_status_line(" abc123 vendor/my great lib (heads/main)").unwrap();
        assert_eq!(path, "vendor/my great lib");
        assert_eq!(described.as_deref(), Some("heads/main"));
    }

    #[test]
    fn an_uninitialized_entry_has_no_describe_suffix() {
        let (state, oid, path, described) = parse_status_line("-abc123 vendor/lib").unwrap();
        assert_eq!(state, SubmoduleState::Uninitialized);
        assert_eq!(oid.as_deref(), Some("abc123"));
        assert_eq!(path, "vendor/lib");
        assert_eq!(described, None);
    }

    #[test]
    fn malformed_lines_are_dropped_rather_than_invented() {
        for line in ["", "?abc123 vendor/lib", " ", "xnot-a-flag", " abc123"] {
            assert!(
                parse_status_line(line).is_none(),
                "line {line:?} must not produce an entry"
            );
        }
    }

    #[test]
    fn a_non_hex_oid_is_reported_as_absent_rather_than_as_data() {
        let (_, oid, path, _) = parse_status_line(" not-an-oid vendor/lib (heads/main)").unwrap();
        assert_eq!(oid, None);
        assert_eq!(path, "vendor/lib");
    }

    #[test]
    fn gitmodules_names_containing_dots_and_slashes_parse_whole() {
        let raw = "submodule.vendor/my.lib.path\nvendor/my.lib\0submodule.vendor/my.lib.url\nhttps://example.test/l.git\0";
        let parsed = parse_gitmodules(raw);
        let entry = parsed.get("vendor/my.lib").expect("name kept whole");
        assert_eq!(entry.0.as_deref(), Some("vendor/my.lib"));
        assert_eq!(entry.1.as_deref(), Some("https://example.test/l.git"));
    }

    #[test]
    fn joins_status_with_configured_url_by_path() {
        let status = " abc123 vendor/lib (heads/main)\n";
        let gitmodules =
            "submodule.the-lib.path\nvendor/lib\0submodule.the-lib.url\nhttps://example.test/l.git\0";
        let out = assemble(status, gitmodules).submodules;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "the-lib", "the section name, not the path");
        assert_eq!(out[0].url.as_deref(), Some("https://example.test/l.git"));
        assert!(!out[0].orphaned);
        assert_eq!(out[0].state, SubmoduleState::UpToDate);
    }

    #[test]
    fn an_entry_missing_from_gitmodules_is_reported_as_orphaned() {
        // It cannot be initialized — there is no URL — and hiding it would
        // leave the user with an empty directory and no explanation.
        let out = assemble("-abc123 vendor/ghost\n", "").submodules;
        assert_eq!(out.len(), 1);
        assert!(out[0].orphaned);
        assert_eq!(out[0].url, None);
        assert_eq!(out[0].name, "vendor/ghost", "falls back to the path");
    }

    #[test]
    fn a_repository_with_no_submodules_lists_empty() {
        assert!(assemble("", "").submodules.is_empty());
        assert!(!assemble("", "").truncated);
    }

    #[test]
    fn the_listing_is_bounded_and_says_so() {
        let status: String = (0..MAX_SUBMODULES + 25)
            .map(|i| format!(" abc123 vendor/lib{i} (heads/main)\n"))
            .collect();
        let list = assemble(&status, "");
        assert_eq!(list.submodules.len(), MAX_SUBMODULES);
        assert!(
            list.truncated,
            "a cap that hid submodules must say so, not look like the whole tree"
        );
    }

    #[test]
    fn only_up_to_date_needs_no_attention() {
        assert!(!SubmoduleState::UpToDate.needs_attention());
        for state in [
            SubmoduleState::Uninitialized,
            SubmoduleState::CommitDiffers,
            SubmoduleState::Conflicted,
        ] {
            assert!(state.needs_attention(), "{state:?}");
        }
    }

    #[test]
    fn a_targeted_change_ends_option_parsing_before_the_path() {
        // Without `--`, a submodule path is indistinguishable from a flag.
        let change = SubmoduleChange::Update {
            path: Some("vendor/lib".into()),
            recursive: true,
        };
        assert_eq!(
            change.argv(),
            vec![
                "git",
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--",
                "vendor/lib"
            ]
        );

        let all = SubmoduleChange::Update {
            path: None,
            recursive: false,
        };
        assert_eq!(all.argv(), vec!["git", "submodule", "update", "--init"]);
    }

    #[test]
    fn deinit_always_terminates_options_and_carries_force_before_the_path() {
        let change = SubmoduleChange::Deinit {
            path: "vendor/lib".into(),
            force: true,
        };
        assert_eq!(
            change.argv(),
            vec!["git", "submodule", "deinit", "--force", "--", "vendor/lib"]
        );
    }

    #[test]
    fn hostile_submodule_paths_never_reach_argv() {
        for path in [
            "--exec=sh",
            "../escape",
            "vendor/../../etc",
            "/absolute/path",
            "",
            "with\0nul",
        ] {
            assert!(
                validate_submodule_path(path).is_err(),
                "path {path:?} must be refused before argv"
            );
        }
        assert!(validate_submodule_path("vendor/my great lib").is_ok());
    }

    #[test]
    fn only_update_is_treated_as_network_work() {
        assert!(SubmoduleChange::Update {
            path: None,
            recursive: false
        }
        .is_network());
        assert!(!SubmoduleChange::Sync {
            path: None,
            recursive: false
        }
        .is_network());
        assert!(!SubmoduleChange::Deinit {
            path: "x".into(),
            force: false
        }
        .is_network());
    }
}
