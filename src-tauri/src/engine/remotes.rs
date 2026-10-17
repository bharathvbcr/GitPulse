//! Remote management: the "cloud" half of a repository's identity.
//!
//! Before this module the client could `fetch`, `pull` and `push`, but could
//! not tell you what remotes existed, let alone add or repoint one — so a
//! clone with no `origin`, a fork that needed an `upstream`, or a remote whose
//! URL had rotated all required dropping to a terminal.
//!
//! Listing reads `git config` rather than parsing `git remote -v`. The config
//! form is NUL-separable and gives fetch and push URLs as distinct keys, while
//! `remote -v`'s output is whitespace-columned text whose parse breaks on a
//! URL containing a space (legal in a local path) and which reports the same
//! remote twice.
//!
//! Every URL that reaches argv goes through
//! [`crate::engine::git_writer::validate_clone_url`] — the same validator the
//! clone path uses, rather than a second copy that could drift. It is what
//! keeps a "URL" of `--upload-pack=…` from becoming a flag.

use crate::engine::git_cli::{git_captured, git_text, git_text_network, validate_repo};
use crate::engine::git_writer::{repo_mutation_lock, validate_clone_url, validate_ref_name};
use crate::ledger::redact::url_for_display;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Upper bound on remotes reported. Far above any real configuration; present
/// so a corrupt config cannot produce an unbounded payload.
const MAX_REMOTES: usize = 200;

/// One configured remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub name: String,
    /// `remote.<name>.url` in its display form: the password or token is
    /// removed by [`crate::ledger::redact::url_for_display`], so the raw URL
    /// never crosses IPC. Absent for a remote configured with only a pushurl,
    /// which is unusual but legal.
    pub fetch_url: Option<String>,
    /// `remote.<name>.pushurl` in display form when set. `None` means pushes
    /// use `fetch_url`; the two are deliberately not collapsed, because
    /// "pushes go somewhere else" is exactly what a user needs to see.
    pub push_url: Option<String>,
    /// True when either configured URL embeds a password or token that the
    /// display form dropped. Submitting a display URL back through
    /// [`RemoteChange::SetUrl`] keeps those credentials.
    pub has_credentials: bool,
    /// Count of `refs/remotes/<name>/*` refs currently held locally.
    pub tracking_branches: usize,
    /// True for the remote GitPulse treats as the default (`origin`, or the
    /// only remote when there is exactly one).
    pub is_default: bool,
}

/// `cmd_list_remotes` payload. A bare `Vec<RemoteInfo>` could not say when
/// the listing cap dropped remotes, so a corrupt config with 201 entries
/// looked like a complete 200-remote repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteList {
    pub remotes: Vec<RemoteInfo>,
    pub truncated: bool,
}

/// Parses `git config --get-regexp -z` output.
///
/// Each record is `key\nvalue` terminated by NUL — the newline separates the
/// two, so a value containing spaces (or newlines beyond the first) is
/// unambiguous where the whitespace-columned porcelain is not.
fn parse_config_records(raw: &str) -> Vec<(String, String)> {
    raw.split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let (key, value) = record.split_once('\n')?;
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// Splits `remote.<name>.<field>` into its name and field.
///
/// A remote name may itself contain dots (`git remote add my.fork …` is
/// legal), so the field is taken from the LAST dot and the name is everything
/// between — splitting on the first dot would name the remote `my`.
fn split_remote_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix("remote.")?;
    let (name, field) = rest.rsplit_once('.')?;
    if name.is_empty() || field.is_empty() {
        return None;
    }
    Some((name.to_string(), field.to_ascii_lowercase()))
}

/// Splits a remote-tracking name (the part after `refs/remotes/`) into the
/// remote that owns it and the branch, by the LONGEST configured remote name
/// that prefixes it: a remote named `team/fork` and one named `team` can both
/// prefix `team/fork/main`, and the longer one owns the ref. The canonical
/// owner of the question, so no reader attributes by "first path segment" and
/// files `team/fork`'s branches under `team`.
///
/// With no configured remote matching (a legacy `.git/remotes` definition, or
/// an empty name list) the first segment is the remote, as git's own default
/// layout has it.
pub(crate) fn split_tracking_ref<'a>(
    remotes: &[String],
    rest: &'a str,
) -> Option<(&'a str, &'a str)> {
    let owner = remotes
        .iter()
        .filter(|name| {
            rest.len() > name.len()
                && rest.starts_with(name.as_str())
                && rest.as_bytes()[name.len()] == b'/'
        })
        .map(String::len)
        .max();
    match owner {
        Some(len) => Some((&rest[..len], &rest[len + 1..])),
        None => rest.split_once('/'),
    }
}

/// Assembles remotes from config records and remote-tracking ref names.
///
/// Split out from the git calls so the assembly is directly testable against
/// hostile shapes without a repository on disk.
fn assemble(records: &[(String, String)], tracking_refs: &[String]) -> RemoteList {
    let mut by_name: BTreeMap<String, RemoteInfo> = BTreeMap::new();
    let mut truncated = false;
    for (key, value) in records {
        let Some((name, field)) = split_remote_key(key) else {
            continue;
        };
        if by_name.len() >= MAX_REMOTES && !by_name.contains_key(&name) {
            truncated = true;
            continue;
        }
        let entry = by_name.entry(name.clone()).or_insert_with(|| RemoteInfo {
            name,
            fetch_url: None,
            push_url: None,
            has_credentials: false,
            tracking_branches: 0,
            is_default: false,
        });
        match field.as_str() {
            "url" => {
                let shown = url_for_display(value);
                entry.has_credentials |= shown.has_credentials;
                entry.fetch_url = Some(shown.display);
            }
            "pushurl" => {
                let shown = url_for_display(value);
                entry.has_credentials |= shown.has_credentials;
                entry.push_url = Some(shown.display);
            }
            // `fetch`, `tagopt` and friends are configuration this surface does
            // not present; they still prove the remote exists, which is why the
            // entry above is created before this match.
            _ => {}
        }
    }

    let names: Vec<String> = by_name.keys().cloned().collect();
    for refname in tracking_refs {
        let Some(rest) = refname.strip_prefix("refs/remotes/") else {
            continue;
        };
        // Longest-prefix match: a remote named `origin/mirror` and a remote
        // named `origin` can both prefix `refs/remotes/origin/mirror/main`,
        // and the longer one owns the ref.
        if let Some((owner, _)) = split_tracking_ref(&names, rest) {
            if let Some(entry) = by_name.get_mut(owner) {
                entry.tracking_branches += 1;
            }
        }
    }

    let mut remotes: Vec<RemoteInfo> = by_name.into_values().collect();
    // Default: `origin` if present, else the sole remote. With two unnamed-by-
    // convention remotes there is no default, and claiming one would be a
    // guess the push path would then act on.
    let default_name = if remotes.iter().any(|r| r.name == "origin") {
        Some("origin".to_string())
    } else if remotes.len() == 1 {
        Some(remotes[0].name.clone())
    } else {
        None
    };
    if let Some(default_name) = default_name {
        for remote in &mut remotes {
            remote.is_default = remote.name == default_name;
        }
    }
    // `origin` first, then alphabetical: the default is what a user looks for.
    remotes.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    RemoteList { remotes, truncated }
}

/// Lists configured remotes with their URLs and tracking-ref counts.
pub fn list(repo_path: &str) -> Result<RemoteList, String> {
    let repo = validate_repo(repo_path)?;
    // `--get-regexp` exits 1 with nothing on stderr when no key matches: a
    // repository with no remotes, an honest empty answer. Any other outcome (a
    // refused or timed-out spawn, a config git cannot parse, a cut-off stream)
    // is a read that did not happen, and used to read as "no remotes".
    let run = git_captured(
        repo.as_path(),
        &["config", "--get-regexp", "-z", "^remote\\..*"],
    )?;
    let records = if run.success && run.incomplete.is_none() {
        parse_config_records(&String::from_utf8_lossy(&run.stdout))
    } else if run.status_code == 1 && run.incomplete.is_none() && run.stderr.is_empty() {
        Vec::new()
    } else {
        return Err(format!(
            "Could not read the remote configuration: {}",
            match &run.incomplete {
                Some(reason) => format!("git config {}", reason.describe()),
                None => String::from_utf8_lossy(&run.stderr).trim().to_string(),
            }
        ));
    };
    let tracking: Vec<String> = git_text(
        repo.as_path(),
        &["for-each-ref", "--format=%(refname)", "refs/remotes/"],
    )
    .map_err(|e| format!("Could not list remote-tracking branches: {e}"))?
    .lines()
    .map(str::trim)
    .map(str::to_string)
    .collect();
    Ok(assemble(&records, &tracking))
}

/// What may be done to a remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RemoteChange {
    Add {
        name: String,
        url: String,
    },
    Remove {
        name: String,
    },
    Rename {
        name: String,
        new_name: String,
    },
    /// Repoints the fetch URL, or the push URL when `push` is set.
    SetUrl {
        name: String,
        url: String,
        push: bool,
    },
    /// Deletes remote-tracking refs whose upstream branch is gone.
    Prune {
        name: String,
    },
}

impl RemoteChange {
    /// The argv this change would execute, for the gate and the executor.
    ///
    /// Borrowed from `self` so the judged line and the run line are literally
    /// the same strings, not two renderings that could diverge.
    pub fn argv(&self) -> Vec<&str> {
        match self {
            RemoteChange::Add { name, url } => vec!["git", "remote", "add", name, url],
            RemoteChange::Remove { name } => vec!["git", "remote", "remove", name],
            RemoteChange::Rename { name, new_name } => {
                vec!["git", "remote", "rename", name, new_name]
            }
            RemoteChange::SetUrl { name, url, push } => {
                let mut argv = vec!["git", "remote", "set-url"];
                if *push {
                    argv.push("--push");
                }
                argv.push(name);
                argv.push(url);
                argv
            }
            RemoteChange::Prune { name } => vec!["git", "remote", "prune", name],
        }
    }

    /// The warning an add or set-url earns when its URL embeds credentials.
    fn credential_warning(&self) -> Option<&'static str> {
        match self {
            RemoteChange::Add { url, .. } | RemoteChange::SetUrl { url, .. }
                if url_for_display(url).has_credentials =>
            {
                Some(CREDENTIAL_WARNING)
            }
            _ => None,
        }
    }

    /// True when the change reaches the network and needs the longer deadline.
    fn is_network(&self) -> bool {
        matches!(self, RemoteChange::Prune { .. })
    }

    /// Rejects anything that must never reach argv.
    ///
    /// Remote names go through the ref-name validator: they become path
    /// components under `refs/remotes/`, so the rules that protect a branch
    /// name protect a remote name for the same reasons.
    fn validate(&self) -> Result<(), String> {
        match self {
            RemoteChange::Add { name, url } => {
                validate_ref_name(name)?;
                validate_clone_url(url)
            }
            RemoteChange::Remove { name } | RemoteChange::Prune { name } => validate_ref_name(name),
            RemoteChange::Rename { name, new_name } => {
                validate_ref_name(name)?;
                validate_ref_name(new_name)?;
                if name == new_name {
                    return Err("The new name is the same as the current one".into());
                }
                Ok(())
            }
            RemoteChange::SetUrl { name, url, .. } => {
                validate_ref_name(name)?;
                validate_clone_url(url)
            }
        }
    }
}

/// Applies a remote change, judging the rendered argv under the repo lock.
pub fn apply_with<J, V>(
    repo_path: &str,
    change: &RemoteChange,
    judge: J,
) -> Result<(V, String), String>
where
    J: FnOnce(&[&str]) -> Result<V, String>,
{
    let repo = validate_repo(repo_path)?;
    change.validate()?;
    let lock = repo_mutation_lock(&repo);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Existence is checked under the lock so the error is about the state the
    // command will actually meet, and so "add a remote that already exists"
    // reports the collision rather than git's terser message.
    let existing = list_names(&repo)?;
    match change {
        RemoteChange::Add { name, .. } => {
            if existing.iter().any(|e| e == name) {
                return Err(format!("A remote named '{name}' already exists"));
            }
        }
        RemoteChange::Rename { name, new_name } => {
            if !existing.iter().any(|e| e == name) {
                return Err(format!("No remote named '{name}'"));
            }
            if existing.iter().any(|e| e == new_name) {
                return Err(format!("A remote named '{new_name}' already exists"));
            }
        }
        RemoteChange::Remove { name }
        | RemoteChange::SetUrl { name, .. }
        | RemoteChange::Prune { name } => {
            if !existing.iter().any(|e| e == name) {
                return Err(format!("No remote named '{name}'"));
            }
        }
    }

    // The webview only ever sees display URLs, so a Set URL submitted with the
    // display form of the stored URL means "keep what is there", credentials
    // included. The warning below is judged on what the caller typed, not on
    // what was re-attached.
    let warning = change.credential_warning();
    let resolved = keep_stored_credentials(&repo, change)?;
    let change = resolved.as_ref().unwrap_or(change);

    let argv = change.argv();
    let verdict = judge(&argv)?;
    let output = if change.is_network() {
        git_text_network(repo.as_path(), &argv[1..])?
    } else {
        git_text(repo.as_path(), &argv[1..])?
    };
    let output = match warning {
        Some(warning) if output.trim().is_empty() => warning.to_string(),
        Some(warning) => format!("{output}\n{warning}"),
        None => output,
    };
    Ok((verdict, output))
}

/// Shown after an add or set-url whose URL embeds a password or token.
const CREDENTIAL_WARNING: &str = "Warning: this URL embeds a password or token, which git stores in plain text in .git/config. Prefer a credential helper (git config credential.helper) and a URL without credentials.";

/// When `change` is a `SetUrl` whose URL is the display form of the URL
/// currently stored, returns the change carrying the stored raw URL instead,
/// so editing a display URL cannot silently strip the credentials. `None`
/// means the change goes ahead as submitted.
fn keep_stored_credentials(
    repo: &Path,
    change: &RemoteChange,
) -> Result<Option<RemoteChange>, String> {
    let RemoteChange::SetUrl { name, url, push } = change else {
        return Ok(None);
    };
    let key = if *push { "pushurl" } else { "url" };
    let stored = match git_text(repo, &["config", "--get", &format!("remote.{name}.{key}")]) {
        Ok(value) => value.trim().to_string(),
        // No pushurl set: pushes use the fetch URL, which is what the display
        // the user edited was showing.
        Err(_) if *push => git_text(repo, &["config", "--get", &format!("remote.{name}.url")])
            .map(|value| value.trim().to_string())
            .map_err(|e| format!("Could not read the stored URL of '{name}': {e}"))?,
        Err(e) => return Err(format!("Could not read the stored URL of '{name}': {e}")),
    };
    let shown = url_for_display(&stored);
    if shown.has_credentials && shown.display == *url {
        let resolved = RemoteChange::SetUrl {
            name: name.clone(),
            url: stored,
            push: *push,
        };
        resolved.validate()?;
        return Ok(Some(resolved));
    }
    Ok(None)
}

/// Refuses `name` unless it is a configured remote, so a path or an arbitrary
/// word can never reach git's remote argument (git reads a path there as a
/// repository to fetch from or push to). Checked before any network spawn.
pub(crate) fn require_configured(repo: &Path, name: &str) -> Result<(), String> {
    if list_names(repo)?
        .iter()
        .any(|configured| configured == name)
    {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a configured remote. Add it with Add remote, or pick one that is configured."
        ))
    }
}

pub(crate) fn list_names(repo: &Path) -> Result<Vec<String>, String> {
    let raw = git_text(repo, &["remote"])?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_config_records_with_values_containing_spaces() {
        // A local-path remote may legally contain spaces; the whitespace-
        // columned `remote -v` porcelain cannot express that unambiguously,
        // which is why listing reads config instead.
        let raw = "remote.origin.url\n/Volumes/My Disk/repo.git\0remote.origin.fetch\n+refs/heads/*:refs/remotes/origin/*\0";
        let parsed = parse_config_records(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].1, "/Volumes/My Disk/repo.git");
    }

    #[test]
    fn a_remote_name_containing_dots_is_not_split_at_the_first_one() {
        assert_eq!(
            split_remote_key("remote.my.fork.url"),
            Some(("my.fork".to_string(), "url".to_string()))
        );
        assert_eq!(
            split_remote_key("remote.origin.pushurl"),
            Some(("origin".to_string(), "pushurl".to_string()))
        );
        assert_eq!(split_remote_key("branch.main.remote"), None);
        assert_eq!(split_remote_key("remote.origin"), None);
    }

    #[test]
    fn assembles_fetch_and_push_urls_without_collapsing_them() {
        // "pushes go somewhere else" is precisely what a user must be able to
        // see; folding pushurl into url would hide a misdirected push.
        let out = assemble(
            &records(&[
                ("remote.origin.url", "https://example.test/a.git"),
                ("remote.origin.pushurl", "ssh://git@example.test/a.git"),
            ]),
            &[],
        )
        .remotes;
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].fetch_url.as_deref(),
            Some("https://example.test/a.git")
        );
        assert_eq!(
            out[0].push_url.as_deref(),
            Some("ssh://git@example.test/a.git")
        );
    }

    #[test]
    fn origin_is_the_default_and_sorts_first() {
        let out = assemble(
            &records(&[
                ("remote.upstream.url", "https://example.test/up.git"),
                ("remote.origin.url", "https://example.test/a.git"),
                ("remote.alpha.url", "https://example.test/al.git"),
            ]),
            &[],
        )
        .remotes;
        assert_eq!(out[0].name, "origin");
        assert!(out[0].is_default);
        assert_eq!(out[1].name, "alpha");
        assert_eq!(out[2].name, "upstream");
        assert!(!out[1].is_default && !out[2].is_default);
    }

    #[test]
    fn a_sole_remote_is_the_default_even_when_not_named_origin() {
        let out = assemble(
            &records(&[("remote.fork.url", "https://example.test/f.git")]),
            &[],
        )
        .remotes;
        assert!(out[0].is_default);
    }

    #[test]
    fn no_default_is_claimed_when_several_remotes_lack_an_origin() {
        // Guessing here would let the push path act on a remote the user never
        // chose.
        let out = assemble(
            &records(&[
                ("remote.fork.url", "https://example.test/f.git"),
                ("remote.upstream.url", "https://example.test/u.git"),
            ]),
            &[],
        )
        .remotes;
        assert!(out.iter().all(|r| !r.is_default));
    }

    #[test]
    fn tracking_refs_are_counted_against_their_longest_matching_remote() {
        let out = assemble(
            &records(&[
                ("remote.origin.url", "https://example.test/a.git"),
                ("remote.origin/mirror.url", "https://example.test/m.git"),
            ]),
            &[
                "refs/remotes/origin/main".to_string(),
                "refs/remotes/origin/dev".to_string(),
                "refs/remotes/origin/mirror/main".to_string(),
                "refs/heads/main".to_string(),
            ],
        )
        .remotes;
        let origin = out.iter().find(|r| r.name == "origin").unwrap();
        let mirror = out.iter().find(|r| r.name == "origin/mirror").unwrap();
        assert_eq!(
            origin.tracking_branches, 2,
            "the mirror's ref is not origin's"
        );
        assert_eq!(mirror.tracking_branches, 1);
    }

    #[test]
    fn a_remote_known_only_by_a_fetch_refspec_still_appears() {
        // Its URL is unknown, and reporting the remote with `fetch_url: None`
        // is honest; omitting it would hide a remote that fetch can use.
        let out = assemble(
            &records(&[("remote.weird.fetch", "+refs/heads/*:refs/remotes/weird/*")]),
            &[],
        )
        .remotes;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "weird");
        assert_eq!(out[0].fetch_url, None);
    }

    #[test]
    fn hostile_urls_never_reach_argv() {
        for url in [
            "--upload-pack=touch /tmp/pwn",
            "-oProxyCommand=sh",
            "ext::sh -c 'id'",
            "",
        ] {
            let change = RemoteChange::Add {
                name: "evil".into(),
                url: url.into(),
            };
            assert!(
                change.validate().is_err(),
                "URL {url:?} must be refused before argv"
            );
        }
    }

    #[test]
    fn hostile_remote_names_never_reach_argv() {
        for name in ["--exec=sh", "", "a..b", "with\0nul", "ends/", ".hidden"] {
            let change = RemoteChange::Remove { name: name.into() };
            assert!(
                change.validate().is_err(),
                "remote name {name:?} must be refused before argv"
            );
        }
    }

    #[test]
    fn renaming_a_remote_to_its_own_name_is_refused() {
        let change = RemoteChange::Rename {
            name: "origin".into(),
            new_name: "origin".into(),
        };
        assert!(change.validate().is_err());
    }

    #[test]
    fn rendered_argv_places_the_push_flag_before_the_name() {
        // `git remote set-url <name> --push <url>` is not the accepted order;
        // the flag must precede the positional arguments.
        let change = RemoteChange::SetUrl {
            name: "origin".into(),
            url: "https://example.test/a.git".into(),
            push: true,
        };
        assert_eq!(
            change.argv(),
            vec![
                "git",
                "remote",
                "set-url",
                "--push",
                "origin",
                "https://example.test/a.git"
            ]
        );

        let fetch_side = RemoteChange::SetUrl {
            name: "origin".into(),
            url: "https://example.test/a.git".into(),
            push: false,
        };
        assert_eq!(
            fetch_side.argv(),
            vec![
                "git",
                "remote",
                "set-url",
                "origin",
                "https://example.test/a.git"
            ]
        );
    }

    #[test]
    fn only_prune_is_treated_as_network_work() {
        // Everything else is a local config edit; giving them the 30-minute
        // network deadline would hide a hung call for half an hour.
        assert!(RemoteChange::Prune {
            name: "origin".into()
        }
        .is_network());
        assert!(!RemoteChange::Add {
            name: "origin".into(),
            url: "https://example.test/a.git".into()
        }
        .is_network());
    }

    #[test]
    fn assemble_caps_payload_and_sets_truncated() {
        // Omitting the flag is how a 201-remote config looks like a complete
        // 200-remote repository.
        let pairs: Vec<(String, String)> = (0..MAX_REMOTES + 9)
            .map(|i| {
                (
                    format!("remote.r{i:03}.url"),
                    format!("https://example.test/{i}.git"),
                )
            })
            .collect();
        let list = assemble(&pairs, &[]);
        assert_eq!(list.remotes.len(), MAX_REMOTES, "payload must be capped");
        assert!(
            list.truncated,
            "a cap that hid remotes must say so, not look like the whole config"
        );
    }

    #[test]
    fn assemble_at_the_cap_is_complete() {
        let pairs: Vec<(String, String)> = (0..MAX_REMOTES)
            .map(|i| {
                (
                    format!("remote.r{i:03}.url"),
                    format!("https://example.test/{i}.git"),
                )
            })
            .collect();
        let list = assemble(&pairs, &[]);
        assert_eq!(list.remotes.len(), MAX_REMOTES);
        assert!(
            !list.truncated,
            "an exact-fit listing is complete, not truncated"
        );
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-q", "-b", "main"]);
        dir
    }

    /// The shared vector for the display rule. The frontend no longer carries a
    /// second redactor, so this table is the one statement of it.
    const DISPLAY_VECTOR: &[(&str, &str, bool)] = &[
        (
            "https://user:tok@host/o/r.git",
            "https://user@host/o/r.git",
            true,
        ),
        (
            "https://oauth2:glpat-abcdefghij0123456789@gitlab.com/x/y.git",
            "https://oauth2@gitlab.com/x/y.git",
            true,
        ),
        (
            "https://ghp_abcdefghijklmnopqrstuvwxyz0123456789@github.com/o/r.git",
            "https://github.com/o/r.git",
            true,
        ),
        ("https://:token@host/o/r.git", "https://host/o/r.git", true),
        (
            "https://alice@host/o/r.git",
            "https://alice@host/o/r.git",
            false,
        ),
        ("ssh://user@host/o/r.git", "ssh://user@host/o/r.git", false),
        (
            "ssh://user:pw@host/o/r.git",
            "ssh://user@host/o/r.git",
            true,
        ),
        ("git@host:o/r.git", "git@host:o/r.git", false),
        ("https://host/o/r.git", "https://host/o/r.git", false),
        ("/Volumes/Disk/repo.git", "/Volumes/Disk/repo.git", false),
        ("../sibling", "../sibling", false),
        // Percent-encoded userinfo: the encoded password still goes.
        (
            "https://me:p%40ss%3Aword@host/o/r.git",
            "https://me@host/o/r.git",
            true,
        ),
        // An unencoded `@` in the password: userinfo ends at the last one.
        (
            "https://me:p@ss@host/o/r.git",
            "https://me@host/o/r.git",
            true,
        ),
        // IPv6 hosts, with and without a port.
        (
            "https://u:t@[::1]:8443/o/r.git",
            "https://u@[::1]:8443/o/r.git",
            true,
        ),
        (
            "ssh://git@[2001:db8::1]/o/r.git",
            "ssh://git@[2001:db8::1]/o/r.git",
            false,
        ),
        // An `@` in the path is not userinfo.
        ("https://host/o/r@v1.git", "https://host/o/r@v1.git", false),
    ];

    #[test]
    fn display_urls_drop_credentials_and_keep_accounts() {
        for (raw, shown, creds) in DISPLAY_VECTOR {
            let got = url_for_display(raw);
            assert_eq!(got.display, *shown, "display of {raw}");
            assert_eq!(got.has_credentials, *creds, "has_credentials of {raw}");
        }
    }

    #[test]
    fn listing_never_carries_a_raw_credential() {
        let out = assemble(
            &records(&[
                ("remote.origin.url", "https://user:tok@host/o/r.git"),
                (
                    "remote.origin.pushurl",
                    "https://ghp_abcdefghijklmnopqrstuvwxyz0123456789@host/o/r.git",
                ),
                ("remote.plain.url", "ssh://git@host/o/r.git"),
            ]),
            &[],
        );
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains("tok@"), "{json}");
        assert!(!json.contains("ghp_"), "{json}");
        let origin = out.remotes.iter().find(|r| r.name == "origin").unwrap();
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://user@host/o/r.git")
        );
        assert_eq!(origin.push_url.as_deref(), Some("https://host/o/r.git"));
        assert!(origin.has_credentials);
        let plain = out.remotes.iter().find(|r| r.name == "plain").unwrap();
        assert!(!plain.has_credentials);
    }

    fn stored_url(dir: &tempfile::TempDir) -> String {
        git_text(dir.path(), &["config", "--get", "remote.origin.url"])
            .unwrap()
            .trim()
            .to_string()
    }

    fn set_url(path: &str, url: &str) -> String {
        let change = RemoteChange::SetUrl {
            name: "origin".into(),
            url: url.into(),
            push: false,
        };
        apply_with(path, &change, |_| Ok(())).unwrap().1
    }

    #[test]
    fn set_url_with_the_display_form_keeps_the_stored_credentials() {
        let dir = repo();
        let path = dir.path().to_str().unwrap();
        crate::test_support::git_in(
            dir.path(),
            &["remote", "add", "origin", "https://user:tok@host/o/r.git"],
        );
        let shown = list(path).unwrap().remotes.remove(0).fetch_url.unwrap();
        assert_eq!(shown, "https://user@host/o/r.git");

        // Submitting the unchanged display URL means "keep what is there".
        let output = set_url(path, &shown);
        assert!(!output.contains("tok"), "{output}");
        assert!(
            output.is_empty(),
            "no warning for credentials the user did not type: {output}"
        );
        assert_eq!(stored_url(&dir), "https://user:tok@host/o/r.git");

        // A different URL replaces everything, and credentials typed into it
        // earn the warning.
        let output = set_url(path, "https://new:secret@other/o/r.git");
        assert!(output.contains("credential helper"), "{output}");
        assert_eq!(stored_url(&dir), "https://new:secret@other/o/r.git");

        // A URL without credentials replaces the stored one and drops them.
        set_url(path, "https://other/o/r.git");
        assert_eq!(stored_url(&dir), "https://other/o/r.git");
        assert!(!list(path).unwrap().remotes[0].has_credentials);
    }

    #[test]
    fn add_with_embedded_credentials_warns_and_a_clean_add_does_not() {
        let dir = repo();
        let path = dir.path().to_str().unwrap();
        let add = |name: &str, url: &str| {
            let change = RemoteChange::Add {
                name: name.into(),
                url: url.into(),
            };
            apply_with(path, &change, |_| Ok(())).unwrap().1
        };
        let warned = add("a", "https://u:t@host/r.git");
        assert!(warned.contains("credential helper"), "{warned}");
        let clean = add("b", "ssh://git@host/r.git");
        assert!(clean.is_empty(), "{clean}");
    }

    /// No remote is an answer; a read that failed is not.
    #[test]
    fn a_repository_without_remotes_lists_none_but_a_failed_read_is_an_error() {
        let dir = repo();
        let path = dir.path().to_str().unwrap();
        let none = list(path).expect("an unconfigured repository has no remotes");
        assert!(none.remotes.is_empty());

        for failing in ["--get-regexp", "for-each-ref"] {
            let refused =
                crate::engine::git_cli::with_forced_spawn_failure_of(failing, || list(path));
            assert!(
                refused.is_err(),
                "a refused {failing} read listed {refused:?} instead of failing"
            );
        }
    }
}
