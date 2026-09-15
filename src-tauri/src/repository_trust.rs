//! Explicit execution authority for a repository, independent of its data.
//!
//! Inspection never starts Git. A grant binds the *repository* — the common
//! Git directory, by canonical path and filesystem identity — and covers every
//! working tree that repository itself vouches for, so a linked worktree is
//! not a second decision. Membership is proved from the approved Git
//! directory's own records, never from what a candidate directory claims about
//! itself. The desktop is the only IPC surface that can grant trust; MCP and
//! repository-local policy cannot.

use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

pub const REQUIRED: &str = "REPOSITORY_TRUST_REQUIRED";
/// The Worktrees-panel control this refusal points at, spelled exactly as the
/// button is labelled.
///
/// A refusal that names a control by a different name than the UI gives it is
/// worse than one that names no control: the reader searches for the words
/// they were handed, does not find them, and concludes the control is gone.
/// This message said "Extend Trust" while the button read "Extend trust to
/// every worktree", and someone following it had to read the source to find
/// a banner that was on screen the whole time.
///
/// `repository-trust-control-contract.test.ts` pins this against the button's
/// own label, because the two live in different languages and nothing else
/// would notice them drifting apart.
pub const EXTEND_TRUST_CONTROL: &str = "Extend trust to every worktree";
/// A refusal that approving the repository cannot fix, so it must not carry
/// [`REQUIRED`]: the desktop reads that marker as "ask for approval and retry"
/// (`repoStore.openRepo`, `externalTools`), and a prompt whose approval
/// changes nothing is a loop, not a diagnosis. This one says the *name* of the
/// directory is writable by other principals, which is a filesystem change to
/// make, not a decision to take.
pub const SHARED_DIRECTORY: &str = "REPOSITORY_DIRECTORY_IS_SHARED";
const MAX_METADATA: u64 = 16 * 1024;
const MAX_RECORD: u64 = 128 * 1024;
const MAX_SESSION_GRANTS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DirectoryIdentity {
    path: PathBuf,
    created_ns: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    file_identity: (u64, [u8; 16]),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    version: u32,
    checkout: DirectoryIdentity,
    git_dir: DirectoryIdentity,
    common_dir: DirectoryIdentity,
}

/// What an approval binds.
///
/// The unit is the repository, because that is the unit of everything trust
/// decides about: one `config`, one set of hooks, one object database, shared
/// by every working tree. `approved` is not a second condition — it records
/// which checkout the human was looking at, and keeps that exact checkout
/// covered even in a layout the repository cannot vouch for.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Grant {
    version: u32,
    repository: DirectoryIdentity,
    approved: DirectoryIdentity,
}

const GRANT_VERSION: u32 = 2;

/// How far the stored approval that covers a checkout actually reaches.
///
/// A boolean cannot carry this, and the difference is the whole of what went
/// wrong once already: `Checkout` and `Repository` both mean "this path may
/// run Git", so a caller reading one bit cannot tell a complete approval from
/// a partial one. It then reports "already trusted" for a repository whose
/// every worktree is refused, and the human is never asked the question that
/// would fix it. This is the module's own invariant turned on scope — a check
/// that covers part of something must not report what full coverage reports.
///
/// Ordered by reach, so `>=` is a meaningful test rather than a match arm per
/// caller.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TrustScope {
    /// Nothing stored reaches this checkout.
    None,
    /// An approval recorded before the repository became the unit of trust.
    /// It authorizes exactly the checkout it named — never its siblings — and
    /// is deliberately not widened by reading it. Extending it is a decision
    /// the human still has to take.
    Checkout,
    /// The repository, and every working tree it vouches for.
    Repository,
}

impl TrustScope {
    /// Whether anything at all authorizes this checkout. The gate's question,
    /// unchanged by the distinction above: a legacy record still admits the
    /// one checkout it named.
    pub fn admits(self) -> bool {
        self != TrustScope::None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustPreview {
    pub path: String,
    pub git_dir: String,
    pub common_dir: String,
    /// Opaque snapshot returned to the grant command unchanged.
    pub identity: String,
    /// What the stored approval reaches. Replaces a `trusted` boolean: the UI
    /// has to be able to offer an upgrade, which means it has to be able to
    /// see that one is missing.
    pub scope: TrustScope,
    /// Whether this checkout has any linked worktrees beyond itself, so an
    /// offer to extend is made only where extending changes something.
    /// Metadata only — the registry is counted, never executed.
    pub worktrees: u32,
}

/// Session grants, keyed by canonical repository (common Git directory) path.
fn sessions() -> &'static Mutex<HashMap<PathBuf, Grant>> {
    static GRANTS: OnceLock<Mutex<HashMap<PathBuf, Grant>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn directory(path: &Path) -> Result<DirectoryIdentity, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?;
    #[cfg(windows)]
    let handle = crate::fs_entry::pin_directory(&path)
        .map_err(|e| format!("Cannot pin repository identity: {e}"))?;
    #[cfg(windows)]
    let metadata = handle.metadata().map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let metadata =
        fs::metadata(&path).map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("Not a directory: {}", path.display()));
    }
    // Birth time distinguishes a replacement even when an inode is recycled.
    // Filesystems that cannot supply identity fail closed rather than creating
    // an approval that silently applies to a future replacement.
    let created_ns = metadata
        .created()
        .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
        .map_err(|e| {
            format!(
                "Cannot establish repository identity for {}: {e}",
                path.display()
            )
        })?
        .as_nanos();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(DirectoryIdentity {
        path,
        created_ns,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(windows)]
        file_identity: crate::fs_entry::windows_file_identity(&handle)
            .map_err(|e| format!("Cannot establish repository file identity: {e}"))?,
    })
}

fn bounded_text(path: &Path, limit: u64) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A substituted FIFO must not park the trust prompt indefinitely.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("Expected a regular file: {}", path.display()));
    }
    let mut text = String::new();
    file.take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if text.len() as u64 > limit {
        return Err(format!(
            "Repository metadata exceeds {limit} bytes: {}",
            path.display()
        ));
    }
    Ok(text)
}

fn metadata_target(base: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim_end_matches(['\r', '\n']);
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err("Invalid Git directory pointer".into());
    }
    let path = Path::new(raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

fn identity(repo_path: &str) -> Result<Identity, String> {
    let checkout = directory(&crate::engine::git_cli::validate_repo_path(repo_path)?)?;
    let dotgit = checkout.path.join(".git");
    let git_path = if dotgit.is_dir() {
        dotgit
    } else if dotgit.is_file() {
        let contents = bounded_text(&dotgit, MAX_METADATA)?;
        let raw = contents
            .strip_prefix("gitdir: ")
            .ok_or("Invalid .git file: expected gitdir pointer")?;
        metadata_target(&checkout.path, raw)?
    } else {
        checkout.path.clone()
    };
    let git_dir = directory(&git_path)?;
    let common_pointer = git_dir.path.join("commondir");
    let common_dir = match fs::symlink_metadata(&common_pointer) {
        Ok(_) => directory(&metadata_target(
            &git_dir.path,
            &bounded_text(&common_pointer, MAX_METADATA)?,
        )?)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
        Err(error) => return Err(format!("Cannot inspect common Git directory: {error}")),
    };
    if !git_dir.path.join("HEAD").is_file() || !common_dir.path.join("objects").is_dir() {
        return Err("Incomplete Git repository metadata".into());
    }
    Ok(Identity {
        version: 1,
        checkout,
        git_dir,
        common_dir,
    })
}

/// Whether the repository itself vouches for this checkout.
///
/// Filesystem metadata only, and every fact is read from *inside* the
/// directory the human approved. A checkout that merely names a trusted common
/// directory is making a claim, and a claim must never be its own evidence:
/// forging what is read here costs write access to the approved repository's
/// own Git directory, where a `core.fsmonitor` would already run, so it buys
/// an attacker nothing they did not already have.
///
/// An unreadable or unexpected layout answers "not a member" rather than
/// failing: the caller's next step is to ask the human about this exact
/// checkout, which is a better outcome than an error no approval can clear.
fn member_of_repository(current: &Identity) -> bool {
    // A bare repository is its own single member; there is no work tree to
    // distinguish from it.
    if current.checkout == current.common_dir {
        return true;
    }
    if current.git_dir == current.common_dir {
        // The shape of a main work tree — but *holding* the repository is what
        // makes one, and a gitfile or a symlink names it from outside instead.
        // Only a real directory at `.git` is possession.
        return fs::symlink_metadata(current.checkout.path.join(".git"))
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
    }
    // A linked work tree: its Git directory must live in the approved
    // repository's own registry, and that registry entry must name this
    // checkout back. Containment is what makes the back-pointer mean anything
    // — a Git directory the claimant fabricates elsewhere can say `commondir:
    // <trusted>` and point its `gitdir` wherever it likes.
    let Ok(registry) = current.common_dir.path.join("worktrees").canonicalize() else {
        return false;
    };
    if current.git_dir.path.parent() != Some(registry.as_path()) {
        return false;
    }
    let pointer = current.git_dir.path.join("gitdir");
    let Ok(raw) = bounded_text(&pointer, MAX_METADATA) else {
        return false;
    };
    let Ok(named) = metadata_target(&current.git_dir.path, &raw) else {
        return false;
    };
    // Git registers the work tree's gitfile, so compare that entry rather than
    // what it resolves to.
    if named.file_name() != Some(std::ffi::OsStr::new(".git")) {
        return false;
    }
    named
        .parent()
        .and_then(|holder| holder.canonicalize().ok())
        .is_some_and(|holder| holder == current.checkout.path)
}

/// Where a repository's approval is stored.
///
/// These hashes are only bounded filenames, never authentication checks. A
/// collision cannot grant trust: the stored identity must match below. The
/// suffix keeps this namespace disjoint from [`legacy_record`], so a
/// pre-repository approval can never be read back as a repository one.
fn repository_record(root: &Path, repository: &Path) -> PathBuf {
    let mut key = DefaultHasher::new();
    repository.hash(&mut key);
    root.join(format!("{:016x}.repository.json", key.finish()))
}

/// Where approvals were stored before the unit of trust became the repository:
/// one record per checkout, holding the whole inspected [`Identity`].
///
/// Still honoured, so upgrading does not silently re-prompt for every
/// repository a user already approved. It authorizes exactly the checkout it
/// named and nothing else — the family is only ever extended by a grant made
/// under the current scheme.
fn legacy_record(root: &Path, checkout: &Path) -> PathBuf {
    let mut key = DefaultHasher::new();
    checkout.hash(&mut key);
    root.join(format!("{:016x}.json", key.finish()))
}

/// Reads a stored record, refusing anything that is not a plain regular file.
fn stored<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read repository trust: {error}")),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Repository trust record is not a regular file".into())
        }
        Ok(_) => {}
    }
    serde_json::from_str(&bounded_text(path, MAX_RECORD)?)
        .map(Some)
        .map_err(|e| format!("Invalid repository trust record: {e}"))
}

/// The `-v1` names the *storage layout* — a directory of per-record JSON files
/// — which has not changed; what a record contains carries its own version.
/// Renaming this would orphan the approvals [`legacy_record`] deliberately
/// still reads, which is the whole of the upgrade path.
fn persistent_root() -> Result<PathBuf, String> {
    crate::tool_config::default_config_dir()
        .map(|dir| dir.join("repository-trust-v1"))
        .ok_or_else(|| "Cannot resolve GitPulse repository trust storage".into())
}

/// Whether `grant` reaches `current`: the same repository, and either the
/// exact checkout that was approved or one that repository vouches for.
fn covers(grant: &Grant, current: &Identity) -> bool {
    grant.version == GRANT_VERSION
        && grant.repository == current.common_dir
        && (grant.approved == current.checkout || member_of_repository(current))
}

/// What the persisted records reach for `current`.
///
/// The repository namespace is consulted first and wins outright: once a grant
/// under the current scheme covers this checkout, an older record for it is
/// spent history, not a downgrade.
fn read_grant(root: &Path, current: &Identity) -> Result<TrustScope, String> {
    if let Some(grant) = stored::<Grant>(&repository_record(root, &current.common_dir.path))? {
        if covers(&grant, current) {
            return Ok(TrustScope::Repository);
        }
    }
    let Some(saved) = stored::<Identity>(&legacy_record(root, &current.checkout.path))? else {
        return Ok(TrustScope::None);
    };
    // Unchanged: exactly the checkout this record named, proved by whole
    // identity equality. Reporting it as `Checkout` rather than `true` widens
    // nothing — it only stops the caller from mistaking it for family-wide.
    Ok(if saved.version == 1 && saved == *current {
        TrustScope::Checkout
    } else {
        TrustScope::None
    })
}

fn scope(current: &Identity) -> Result<TrustScope, String> {
    let session = sessions()
        .lock()
        .map_err(|_| "Repository trust state is unavailable")?
        .get(&current.common_dir.path)
        .cloned();
    if session.is_some_and(|grant| covers(&grant, current)) {
        return Ok(TrustScope::Repository);
    }
    read_grant(&persistent_root()?, current)
}

fn trusted(current: &Identity) -> Result<bool, String> {
    scope(current).map(TrustScope::admits)
}

/// How many working trees this repository has, counted from its own registry.
///
/// Metadata only, and deliberately not `git worktree list`: this is read while
/// deciding whether the repository may run Git at all, so it must not be the
/// thing that runs it. A registry that cannot be listed counts as the one
/// working tree we are looking at, which makes an offer to extend disappear
/// rather than appear on a guess.
fn worktree_count(current: &Identity) -> u32 {
    let registry = current.common_dir.path.join("worktrees");
    let linked = match fs::read_dir(&registry) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("gitdir").is_file())
            .count(),
        Err(_) => 0,
    };
    // The main working tree is not in the registry; a bare repository has none
    // to add.
    let main = u32::from(current.checkout != current.common_dir);
    linked.try_into().unwrap_or(u32::MAX).saturating_add(main)
}

pub fn inspect(repo_path: &str) -> Result<TrustPreview, String> {
    let current = identity(repo_path)?;
    Ok(TrustPreview {
        path: current.checkout.path.to_string_lossy().into_owned(),
        git_dir: current.git_dir.path.to_string_lossy().into_owned(),
        common_dir: current.common_dir.path.to_string_lossy().into_owned(),
        identity: serde_json::to_string(&current).map_err(|e| e.to_string())?,
        scope: scope(&current)?,
        worktrees: worktree_count(&current),
    })
}

/// Metadata-only family resolution must not execute an unapproved sibling's
/// Git configuration merely to discover a shared ledger or watch directory.
pub(crate) fn git_directories(repo: &Path) -> Result<(PathBuf, PathBuf), String> {
    let current = identity(repo.to_str().ok_or("Repository path is not UTF-8")?)?;
    Ok((current.git_dir.path, current.common_dir.path))
}

/// Require a current grant. Called at admission and again at process spawn.
pub fn require(repo: &Path) -> Result<(), String> {
    require_identified(repo).map(|_| ())
}

/// [`require`], returning the identity it validated.
///
/// Admission needs the identity itself, not just the verdict: it has to prove
/// that the object this grant was checked against is the same object the child
/// will be anchored to. Re-reading the path for that would be one more lookup a
/// rename could redirect, so the single sample that was approved is the one
/// that gets compared.
fn require_identified(repo: &Path) -> Result<Identity, String> {
    let current = identity(repo.to_str().ok_or("Repository path is not UTF-8")?)?;
    if trusted(&current)? {
        return Ok(current);
    }
    // Name the repository too when this is a linked worktree. One approval
    // covers the family, and the checkout a user can actually reach for is
    // more often the one the repository lives in than an agent's scratch
    // worktree — so saying only "open this path" sends them the long way
    // round, or nowhere at all when the worktree is gone by the time they read
    // it.
    let mut refusal = format!(
        "{REQUIRED}: Open {} in GitPulse and trust it before running repository commands.",
        current.checkout.path.display()
    );
    if current.git_dir != current.common_dir {
        refusal.push_str(&format!(
            " This is a linked worktree: approving any working tree of the repository at {} covers all of them.",
            current.common_dir.path.display()
        ));
        // The one case where a reader can have done everything right and still
        // be here. Telling someone who already approved this repository to go
        // approve it is advice they have followed and watched fail; naming the
        // older scheme is the difference between a dead end and one click.
        if predates_worktree_coverage(&current) {
            // Why they are being asked again, then the fallback — and nothing
            // else. The sentence above already says approving any working tree
            // covers the family, so the first line's "open this one and trust
            // it" is the cheap path in full; restating it here made the
            // message longer and said the same thing twice.
            //
            // The panel is named for the one case that path cannot serve: this
            // worktree is already gone by the time someone reads this. Naming
            // the button as it is actually labelled, and saying where it is,
            // is the difference between that and a hunt.
            refusal.push_str(&format!(
                " That repository was approved before GitPulse covered worktrees, \
                 so the earlier approval reaches only the checkout it named — \
                 which is why you are asked again here. If this worktree is \
                 already gone, open that repository instead and use \
                 \"{EXTEND_TRUST_CONTROL}\" in the Worktrees section of the \
                 left sidebar."
            ));
        }
    }
    Err(refusal)
}

/// Whether this repository's own main checkout carries a pre-repository
/// approval — the state in which a human has approved this repository and is
/// being refused anyway.
///
/// Best effort by construction: it answers a question about wording, never
/// about authority, so every way of not knowing is `false` and the caller
/// falls back to the generic message. The main working tree is the one that
/// holds the common directory, so it is `commondir`'s parent; that candidate
/// is then put through the same identity and membership proofs as any other
/// checkout rather than trusted for its shape.
fn predates_worktree_coverage(current: &Identity) -> bool {
    persistent_root().is_ok_and(|root| approval_predates_worktree_coverage(&root, current))
}

/// [`predates_worktree_coverage`] over a given store.
///
/// Split so the layout guard below can be exercised against a record that
/// actually exists. Tested through the real store it is unreachable: every
/// fixture repository is absent from it, so the final lookup answers `None`
/// whatever the guard did, and a test of the guard passes by accident. That
/// tautology survived a mutation that deleted the guard outright.
fn approval_predates_worktree_coverage(root: &Path, current: &Identity) -> bool {
    let Some(main) = current.common_dir.path.parent() else {
        return false;
    };
    let Some(main) = main.to_str() else {
        return false;
    };
    let Ok(main) = identity(main) else {
        return false;
    };
    // The candidate has to be *this* repository's main working tree: same
    // common directory, and holding it directly rather than pointing at it.
    // Without both, a neighbouring repository's approval would be offered as
    // the explanation for this refusal.
    if main.common_dir != current.common_dir || main.git_dir != main.common_dir {
        return false;
    }
    read_grant(root, &main) == Ok(TrustScope::Checkout)
}

/// Whether `message` is — or carries, once a caller has wrapped it — the
/// refusal [`require`] returns.
///
/// A refusal is not a verdict about the repository. It says this process was
/// never allowed to look, so nothing was examined and nothing was found
/// wanting. Callers that turn an error string into a category must ask here
/// rather than matching [`REQUIRED`] themselves: the one classifier that did
/// not ask fell through to its "we looked, and it is broken" arm and told
/// users that six perfectly valid checkouts were `[invalid_worktree]`.
///
/// A substring rather than a prefix test because the marker travels inside
/// whatever context its caller wrapped around it.
pub fn refused(message: &str) -> bool {
    message.contains(REQUIRED)
}

fn save_grant(root: &Path, current: &Grant) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(root).map_err(|e| format!("Cannot create repository trust storage: {e}"))?;
    let path = repository_record(root, &current.repository.path);
    let temporary = root.join(format!(
        ".grant-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| format!("Cannot prepare repository trust: {e}"))?;
    let result = (|| {
        let data = serde_json::to_vec(current).map_err(|e| e.to_string())?;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Cannot save repository trust: {e}"))?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| format!("Cannot publish repository trust: {e}"))
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Cannot remove incomplete trust record: {error}");
            }
        }
    }
    result
}

/// The exact inspected identity must accompany a deliberate approval. A
/// session grant is useful to callers with an in-process approval UI and tests;
/// it uses the same identity checks and never grants another process authority.
pub fn grant(repo_path: &str, expected_identity: &str, remember: bool) -> Result<(), String> {
    if expected_identity.len() as u64 > MAX_RECORD {
        return Err("Repository identity is too large".into());
    }
    let current = identity(repo_path)?;
    if serde_json::to_string(&current).map_err(|e| e.to_string())? != expected_identity {
        return Err(
            "Repository changed while trust was being requested. Inspect and approve it again."
                .into(),
        );
    }
    let granted = Grant {
        version: GRANT_VERSION,
        repository: current.common_dir,
        approved: current.checkout,
    };
    if remember {
        save_grant(&persistent_root()?, &granted)
    } else {
        let mut grants = sessions()
            .lock()
            .map_err(|_| "Repository trust state is unavailable")?;
        if grants.len() >= MAX_SESSION_GRANTS && !grants.contains_key(&granted.repository.path) {
            return Err("Too many session repository grants".into());
        }
        grants.insert(granted.repository.path.clone(), granted);
        Ok(())
    }
}

/// Revocation blocks subsequent operations; already-started processes retain
/// the authority granted when they started and must be stopped separately.
///
/// It reaches the whole repository, because the grant did. Leaving one working
/// tree approved because it was approved under an older scheme, or before its
/// siblings, would make "revoked" mean "revoked except where you forgot" —
/// so every record naming this repository goes, not only the one keyed by it.
pub fn revoke(repo_path: &str) -> Result<(), String> {
    let repo = crate::engine::git_cli::validate_repo_path(repo_path)?;
    // A checkout that has already been removed cannot name its family any
    // more; revoking then falls back to the path itself, which is all that is
    // left to identify.
    let repository = identity(repo_path).ok().map(|id| id.common_dir.path);
    sessions()
        .lock()
        .map_err(|_| "Repository trust state is unavailable")?
        .retain(|key, grant| {
            Some(key.as_path()) != repository.as_deref() && grant.approved.path != repo
        });
    forget_records(&persistent_root()?, repository.as_deref(), &repo)
}

/// Deletes every stored approval that names this repository or this checkout.
///
/// Unreadable entries are left alone deliberately: a record this cannot parse
/// is one [`read_grant`] cannot parse either, so it authorizes nothing and
/// removing it would only be tidying. A directory that cannot be listed is a
/// different matter — the revocation would be silently partial, so it fails.
fn forget_records(root: &Path, repository: Option<&Path>, checkout: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cannot revoke repository trust: {error}")),
    };
    for entry in entries {
        let path = entry
            .map_err(|error| format!("Cannot revoke repository trust: {error}"))?
            .path();
        let named = match stored::<Grant>(&path) {
            Ok(Some(grant)) => {
                Some(grant.repository.path.as_path()) == repository
                    || grant.approved.path == checkout
            }
            // Not a repository record, or not readable as one. A pre-repository
            // record still authorizes the single checkout it names.
            _ => matches!(
                stored::<Identity>(&path),
                Ok(Some(saved))
                    if Some(saved.common_dir.path.as_path()) == repository
                        || saved.checkout.path == checkout
            ),
        };
        if !named {
            continue;
        }
        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("Cannot revoke repository trust: {error}"));
            }
        }
    }
    Ok(())
}

/// Whatever must outlive admission for the spawn to land where it was
/// admitted, so callers hold it across `spawn` on every platform.
///
/// What that amounts to differs, and the empty Unix shape is not an oversight:
/// there the guarantee travels inside the `Command` itself, as a `pre_exec`
/// closure owning the pinned descriptor, so nothing is left for the caller to
/// keep alive. On Windows it is these directory handles, whose share mode is
/// the only thing stopping a component of the path from being renamed while
/// the child starts.
pub(crate) struct AdmittedCwd {
    #[cfg(windows)]
    _pinned: Vec<std::fs::File>,
}

/// All bounded child processes share this last admission check. Global probes
/// get the executable volume's root, never an inherited repository directory.
/// A project tool's explicit directory is checked against its nearest repo.
///
/// The directory is *pinned*, not merely inspected. `Command::current_dir`
/// stores a path and the operating system resolves it again at spawn, so
/// admitting the object a path names and then letting the spawn re-walk that
/// path authorizes one directory and runs in another: renaming any component
/// and leaving a symlink in its place redirects the child into a checkout that
/// was never approved, whose `core.fsmonitor` and aliases then execute. Every
/// directory is pinned, not only one already inside a repository — otherwise
/// the same substitution turns an unremarkable working directory into a
/// repository after the point where admission decided none was involved.
pub(crate) fn check_command(cmd: &mut std::process::Command) -> Result<AdmittedCwd, String> {
    if cmd.get_current_dir().is_none() {
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| format!("Cannot resolve a neutral process directory: {error}"))?;
        let root = executable
            .ancestors()
            .last()
            .ok_or("Missing executable volume root")?;
        cmd.current_dir(root);
    }
    let cwd = cmd
        .get_current_dir()
        .ok_or("Missing process directory")?
        .to_path_buf();
    admit_directory(cmd, &cwd)
}

/// Resolve a directory, pin it, prove the pinned object is still the one the
/// path names, and require trust for whatever repository holds it.
///
/// One owner for that sequence: both the process seam and the PTY seam need
/// exactly it, and they differ only in what they can do about a path that
/// another principal could still substitute afterwards.
#[cfg(unix)]
fn pin_and_admit(cwd: &Path) -> Result<crate::fs_entry::PinnedDir, String> {
    use std::os::unix::fs::MetadataExt;
    // Canonicalize first. A canonical path holds no symlink by construction,
    // so the no-follow walk below accepts every legitimate location — macOS
    // temporary directories under `/var`, a symlinked home — and refuses only
    // a component substituted *after* this resolution, which is the attack.
    let resolved = cwd
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", cwd.display()))?;
    let pinned = crate::fs_entry::pin_dir_nofollow(&resolved)
        .map_err(|error| format!("Cannot pin process directory {}: {error}", cwd.display()))?;
    let held = pinned.dir.metadata().map_err(|e| e.to_string())?;
    let named = fs::metadata(&resolved).map_err(|e| e.to_string())?;
    if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
        return Err("Process directory changed while it was being admitted".into());
    }
    let approved = match crate::engine::git_cli::find_git_root(&resolved) {
        Some(repo) => Some(require_identified(&repo)?),
        None => None,
    };
    if !pinned.exclusive {
        // The child will be anchored to the pinned descriptor rather than to
        // the path, so the repository that was trusted has to be the pinned
        // object's OWN repository. `find_git_root` answers about the path, and
        // between the pin and that walk a rename can make the two describe
        // different trees — a swap that is reverted before the walk leaves the
        // descriptor inside an untrusted checkout while the path shows a
        // trusted one, or none at all. Reading it back through the descriptor
        // is the only answer a rename cannot reach.
        //
        // Paid only on this path, like the re-anchor itself: where every
        // component is beyond other users' reach the two walks cannot diverge,
        // and the ancestor walk would cost the hot `git` seam for nothing.
        let anchored = crate::fs_entry::repository_identity_of_pin(&pinned.dir)
            .map_err(|error| format!("Cannot identify the pinned repository: {error}"))?;
        let trusted = approved
            .as_ref()
            .map(|identity| (identity.checkout.device, identity.checkout.inode));
        if anchored != trusted {
            return Err(
                "Process directory changed while it was being admitted: the approved repository \
                 is not the one this process would run in"
                    .into(),
            );
        }
    }
    Ok(pinned)
}

/// Admit a directory for an interface that cannot re-anchor its own child.
///
/// A `std::process::Command` can be handed the pinned descriptor and corrected
/// after the fact; a PTY cannot. `portable_pty` builds the command itself,
/// owns the only `pre_exec` hook on it, and exposes no slave descriptor, so
/// the child's working directory comes from resolving the path and nothing can
/// fix it afterwards. Where the path is provably beyond another principal's
/// reach that resolution is safe. Where it is not, this refuses: starting a
/// long-lived interactive session in a directory that may have been swapped
/// between approval and launch is the one outcome the gate exists to prevent,
/// and there is no correcting hook to fall back on.
pub(crate) fn require_unsubstitutable_dir(dir: &Path) -> Result<AdmittedCwd, String> {
    #[cfg(unix)]
    {
        let pinned = pin_and_admit(dir)?;
        if let Some(holder) = pinned.shared_holder {
            return Err(format!(
                "{SHARED_DIRECTORY}: cannot start a session in {} because {} is writable by other \
                 users, so that path can be pointed at a different directory after this checkout \
                 is approved. Approving the repository again will not change this — remove group \
                 and other write access from that directory, or set its sticky bit.",
                dir.display(),
                holder.display()
            ));
        }
        Ok(AdmittedCwd {})
    }
    #[cfg(windows)]
    {
        // Windows holds the path open instead: the pinned chain omits
        // FILE_SHARE_DELETE, so no component can be renamed while the caller
        // keeps this guard alive across the spawn.
        admit_directory_windows(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err("Process admission cannot pin a directory on this platform".into())
    }
}

/// The pinned descriptor and the path must name the same object, or a
/// component moved between the two resolutions and neither answer describes
/// what would run.
#[cfg(unix)]
fn admit_directory(cmd: &mut std::process::Command, cwd: &Path) -> Result<AdmittedCwd, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let pinned = pin_and_admit(cwd)?;
    if pinned.exclusive {
        // Nobody outside this user can rename a component of this path, so the
        // spawn's own resolution reaches the object just admitted. Re-anchoring
        // anyway would buy nothing and cost the `posix_spawn` fast path: a
        // `pre_exec` closure forces `fork` + `exec` of a process holding the
        // parent's whole address space, measured at +123% per spawn by
        // `benches/process_spawn.rs`, on the seam every `git` call goes through.
        return Ok(AdmittedCwd {});
    }
    // A shared ancestor can be substituted by someone else between here and the
    // spawn, so stop trusting the path and hand the child the descriptor.
    let dir = pinned.dir;
    // SAFETY: `fchdir` is async-signal-safe and the closure allocates nothing,
    // takes no lock, and touches no other state — the only requirements between
    // `fork` and `exec`. The descriptor stays open until exec closes it.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fchdir(dir.as_raw_fd()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(AdmittedCwd {})
}

#[cfg(windows)]
fn admit_directory(_cmd: &mut std::process::Command, cwd: &Path) -> Result<AdmittedCwd, String> {
    admit_directory_windows(cwd)
}

#[cfg(windows)]
fn admit_directory_windows(cwd: &Path) -> Result<AdmittedCwd, String> {
    // Canonical first, for the reason the Unix arm gives: `pin_directory`
    // refuses a reparse point, and a junction anywhere in the caller's spelling
    // is ordinary, not hostile.
    let resolved = cwd
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", cwd.display()))?;
    let pinned = crate::fs_entry::pin_dir_chain(&resolved, false)
        .map_err(|error| format!("Cannot pin process directory {}: {error}", cwd.display()))?;
    let leaf = pinned.last().ok_or("Missing pinned process directory")?;
    let held = crate::fs_entry::windows_file_identity(leaf)
        .map_err(|error| format!("Cannot identify process directory: {error}"))?;
    let named = crate::fs_entry::pin_directory(&resolved)
        .and_then(|handle| crate::fs_entry::windows_file_identity(&handle))
        .map_err(|error| format!("Cannot identify process directory: {error}"))?;
    if held != named {
        return Err("Process directory changed while it was being admitted".into());
    }
    if let Some(repo) = crate::engine::git_cli::find_git_root(&resolved) {
        require(&repo)?;
    }
    // The handles stay open across the spawn: without FILE_SHARE_DELETE no
    // component of this path can be renamed or removed while the child starts.
    // That also removes the reason the Unix arm reads the repository back
    // through the pinned descriptor: the pin is taken before this trust check,
    // and nothing can rename a component while it is held, so the path cannot
    // come to mean something else between the two reads.
    Ok(AdmittedCwd { _pinned: pinned })
}

#[cfg(not(any(unix, windows)))]
fn admit_directory(_cmd: &mut std::process::Command, _cwd: &Path) -> Result<AdmittedCwd, String> {
    Err("Process admission cannot pin a directory on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn equal_creation_times_do_not_authorize_a_replaced_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkout");
        fs::create_dir(&path).unwrap();
        let approved = directory(&path).unwrap();
        fs::rename(&path, root.path().join("original")).unwrap();
        fs::create_dir(&path).unwrap();
        let mut replacement = directory(&path).unwrap();
        // Creation timestamps are mutable on Windows. Even an exact match
        // must not make the replacement equal to the previously approved object.
        replacement.created_ns = approved.created_ns;
        assert_ne!(approved, replacement);
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    fn approval_for(current: &Identity) -> Grant {
        Grant {
            version: GRANT_VERSION,
            repository: current.common_dir.clone(),
            approved: current.checkout.clone(),
        }
    }

    #[test]
    fn persistent_records_require_a_complete_matching_repository() {
        let repo = fixture();
        let other = fixture();
        let storage = tempfile::tempdir().unwrap();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::None
        );
        save_grant(storage.path(), &approval_for(&current)).unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::Repository
        );
        let record = repository_record(storage.path(), &current.common_dir.path);
        for content in [
            "{".to_owned(),
            "null".to_owned(),
            "x".repeat(MAX_RECORD as usize + 1),
        ] {
            fs::write(&record, content).unwrap();
            assert!(read_grant(storage.path(), &current).is_err());
        }
        let mut changed = approval_for(&current);
        changed.version = GRANT_VERSION + 1;
        fs::write(&record, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::None
        );
        let foreign = identity(other.path().to_str().unwrap()).unwrap();
        fs::write(
            &record,
            serde_json::to_vec(&approval_for(&foreign)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::None
        );

        // `approved` is an audit trail, not a second lock. A record for *this*
        // repository covers a member of it whichever checkout the human was
        // looking at — that is the whole point of binding the repository, and
        // a later "tightening" back to an exact-checkout match would quietly
        // restore the per-worktree prompt.
        let mut elsewhere = approval_for(&current);
        elsewhere.approved = foreign.checkout.clone();
        fs::write(&record, serde_json::to_vec(&elsewhere).unwrap()).unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::Repository
        );
    }

    /// Upgrading must not re-prompt for every repository already approved, and
    /// must not retroactively widen those approvals either: a record written
    /// before the repository became the unit authorizes the one checkout it
    /// named, and is read from its own filename namespace so it can never be
    /// mistaken for a repository-wide one.
    #[test]
    fn a_pre_repository_record_still_authorizes_exactly_its_checkout() {
        let repo = fixture();
        let storage = tempfile::tempdir().unwrap();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let legacy = legacy_record(storage.path(), &current.checkout.path);
        fs::write(&legacy, serde_json::to_vec(&current).unwrap()).unwrap();
        // Admitted, and reported as reaching only this checkout. Both halves
        // matter: the first is the no-re-prompt promise, the second is what
        // lets the UI offer the extension instead of claiming full coverage.
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::Checkout
        );
        assert!(read_grant(storage.path(), &current).unwrap().admits());

        // Deliberately the *current* grant version: a record in the legacy
        // namespace claiming to be a newer one is still refused there, so the
        // two schemes cannot be crossed by editing a version number.
        let mut stale = current.clone();
        stale.version = GRANT_VERSION;
        fs::write(&legacy, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert_eq!(
            read_grant(storage.path(), &current).unwrap(),
            TrustScope::None
        );

        // The same bytes at the repository key authorize nothing: the two
        // namespaces are disjoint, so an old record cannot be replayed as a
        // family-wide one.
        fs::remove_file(&legacy).unwrap();
        fs::write(
            repository_record(storage.path(), &current.common_dir.path),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        assert!(read_grant(storage.path(), &current).is_err());
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// One repository with a linked worktree, approved under the pre-repository
    /// scheme — the state every install that trusted anything before
    /// `GRANT_VERSION` 2 is in.
    fn legacy_approved_repository() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
    ) {
        let repo = fixture();
        git_in(repo.path(), &["commit", "--allow-empty", "-m", "init"]);
        let parent = tempfile::tempdir().unwrap();
        let linked = parent.path().join("linked");
        git_in(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "gitpulse-link",
                linked.to_str().unwrap(),
            ],
        );
        let storage = tempfile::tempdir().unwrap();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        fs::write(
            legacy_record(storage.path(), &main.checkout.path),
            serde_json::to_vec(&main).unwrap(),
        )
        .unwrap();
        (repo, parent, storage, linked)
    }

    /// The gap this scope exists to close, kept as a regression.
    ///
    /// A pre-repository approval admits the checkout it named and refuses
    /// every worktree of the very repository it approved. That refusal is
    /// correct — widening it silently is the one thing this module must not do
    /// — but it is only survivable if the difference is *visible*, because a
    /// caller that cannot see it reports "already trusted" and never offers
    /// the upgrade. Under the boolean this replaced, both rows below read
    /// `true`/`false` with nothing to distinguish them from a repository that
    /// was never approved at all.
    #[test]
    fn a_legacy_approval_is_reported_as_reaching_only_its_own_checkout() {
        let (repo, _parent, storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();

        // Unchanged authority: the approved checkout runs, its worktree does not.
        assert!(read_grant(storage.path(), &main).unwrap().admits());
        assert!(!read_grant(storage.path(), &worktree).unwrap().admits());

        // Newly visible: *why* — a partial approval, not a missing one.
        assert_eq!(
            read_grant(storage.path(), &main).unwrap(),
            TrustScope::Checkout
        );
        assert_eq!(
            read_grant(storage.path(), &worktree).unwrap(),
            TrustScope::None
        );

        // The repository does vouch for the worktree, so the only thing
        // standing between the human and coverage is being asked.
        assert_eq!(worktree.common_dir, main.common_dir);
        assert!(member_of_repository(&worktree));
        assert!(!repository_record(storage.path(), &main.common_dir.path).exists());
    }

    /// Extending is one approval, and it reaches the family from either side.
    ///
    /// Run from both checkouts because they take different paths through
    /// `covers`: the checkout the human approved matches `approved` outright,
    /// while its sibling has to be proved a member.
    #[test]
    fn extending_a_legacy_approval_covers_the_whole_repository() {
        for approve_worktree in [false, true] {
            let (repo, _parent, storage, linked) = legacy_approved_repository();
            let main = identity(repo.path().to_str().unwrap()).unwrap();
            let worktree = identity(linked.to_str().unwrap()).unwrap();
            let approved = if approve_worktree {
                worktree.clone()
            } else {
                main.clone()
            };

            save_grant(storage.path(), &approval_for(&approved)).unwrap();

            assert_eq!(
                read_grant(storage.path(), &main).unwrap(),
                TrustScope::Repository,
                "main checkout, approved via worktree={approve_worktree}"
            );
            assert_eq!(
                read_grant(storage.path(), &worktree).unwrap(),
                TrustScope::Repository,
                "linked worktree, approved via worktree={approve_worktree}"
            );

            // The legacy record is still on disk and must not be what answers
            // now: a repository grant is strictly more reach, so reading it
            // second would report a downgrade for the checkout it named.
            assert!(legacy_record(storage.path(), &main.checkout.path).exists());
            assert_eq!(
                read_grant(storage.path(), &main).unwrap(),
                TrustScope::Repository
            );
        }
    }

    /// An offer to extend is worth making only where it changes something, so
    /// the count that gates it has to be right for each shape — and must never
    /// run Git to find out.
    #[test]
    fn worktrees_are_counted_from_the_registry_without_running_git() {
        let (repo, _parent, _storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();
        assert_eq!(worktree_count(&main), 2, "main checkout plus one linked");
        assert_eq!(worktree_count(&worktree), 2, "same repository, same answer");

        let lone = fixture();
        let lone = identity(lone.path().to_str().unwrap()).unwrap();
        assert_eq!(worktree_count(&lone), 1, "no registry at all");

        // A registry entry that is not a worktree is not counted: an offer
        // pinned on a stray directory would appear where extending does
        // nothing.
        fs::create_dir_all(main.common_dir.path.join("worktrees").join("debris")).unwrap();
        assert_eq!(worktree_count(&main), 2, "entry without a gitdir pointer");
    }

    /// The refusal a linked worktree gets has to name the older scheme when
    /// that is what is holding it — and must not invent one when it is not.
    /// Someone told to approve a repository they already approved has been
    /// handed a dead end.
    #[test]
    fn a_refusal_names_the_older_scheme_only_when_it_is_the_cause() {
        let (repo, _parent, _storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();

        // Nothing approved: the generic refusal, with no claim about history.
        // (`predates_worktree_coverage` reads the real persistent root, which
        // holds no record for this temporary repository either way.)
        assert!(!predates_worktree_coverage(&worktree));
        let refusal = require(&worktree.checkout.path).unwrap_err();
        assert!(refusal.contains(REQUIRED));
        assert!(refusal.contains("linked worktree"));
        assert!(
            !refusal.contains(EXTEND_TRUST_CONTROL),
            "must not claim an earlier approval that does not exist: {refusal}"
        );

        // The main checkout is not a linked worktree, so it never gets the
        // worktree half of the message at all.
        let refusal = require(&main.checkout.path).unwrap_err();
        assert!(!refusal.contains("linked worktree"), "{refusal}");
    }
    /// The wire spelling the desktop switches on. A rename here silently turns
    /// every `scope` comparison in `repositoryTrust.ts` into a false, which
    /// reads as "never trusted" and re-prompts for everything.
    #[test]
    fn scope_serializes_to_the_spellings_the_desktop_matches_on() {
        for (scope, wire) in [
            (TrustScope::None, "\"none\""),
            (TrustScope::Checkout, "\"checkout\""),
            (TrustScope::Repository, "\"repository\""),
        ] {
            assert_eq!(serde_json::to_string(&scope).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<TrustScope>(wire).unwrap(),
                scope,
                "round trip"
            );
        }
        // `admits` is the gate's question and must stay true for both approved
        // states — the split is about reach, never about authority.
        assert!(!TrustScope::None.admits());
        assert!(TrustScope::Checkout.admits());
        assert!(TrustScope::Repository.admits());
        assert!(TrustScope::Repository > TrustScope::Checkout);
    }

    /// Counting must survive a registry that is not a readable directory of
    /// worktrees, because the count decides whether a banner appears and a
    /// panic or a wild number there is a worse outcome than no offer.
    #[test]
    fn a_hostile_worktree_registry_cannot_fabricate_an_offer() {
        let repo = fixture();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let registry = current.common_dir.path.join("worktrees");

        // No registry: just this checkout.
        assert_eq!(worktree_count(&current), 1);

        // A *file* where the registry should be reads as unlistable, which is
        // the same answer as empty rather than an error or a panic.
        fs::write(&registry, b"not a directory").unwrap();
        assert_eq!(worktree_count(&current), 1);
        fs::remove_file(&registry).unwrap();

        // Entries that are files, empty directories, or directories whose
        // `gitdir` is itself a directory are all not worktrees.
        fs::create_dir_all(&registry).unwrap();
        fs::write(registry.join("a-file"), b"x").unwrap();
        fs::create_dir(registry.join("empty")).unwrap();
        fs::create_dir_all(registry.join("gitdir-is-a-dir").join("gitdir")).unwrap();
        assert_eq!(worktree_count(&current), 1, "nothing here is a worktree");

        // Only a real `gitdir` pointer counts.
        fs::create_dir(registry.join("real")).unwrap();
        fs::write(registry.join("real").join("gitdir"), b"/somewhere/.git").unwrap();
        assert_eq!(worktree_count(&current), 2);
    }

    /// Deriving the main checkout from `commondir`'s parent is a guess about
    /// layout, so it has to be *checked* rather than trusted. Every shape that
    /// makes the guess wrong must answer "no", because the message it gates
    /// tells someone they already approved this repository.
    ///
    /// Run against a store that really holds the record, so the layout guard
    /// is what decides. Pointed at the real store every case answers "no"
    /// because no fixture is in it, and the test passes without testing.
    #[test]
    fn the_older_scheme_is_only_claimed_where_the_layout_proves_it() {
        // The positive control: a genuine legacy approval of this very
        // repository's main checkout, seen from its worktree.
        let (repo, _parent, storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();
        assert!(
            approval_predates_worktree_coverage(storage.path(), &worktree),
            "this is exactly the state the message describes"
        );

        // Extending it is no longer the older scheme, so the message stops.
        save_grant(storage.path(), &approval_for(&main)).unwrap();
        assert!(!approval_predates_worktree_coverage(
            storage.path(),
            &worktree
        ));

        // A bare repository: `commondir`'s parent is the directory that holds
        // it, which is not a checkout of it.
        let holder = tempfile::tempdir().unwrap();
        let bare_path = holder.path().join("bare.git");
        let status = std::process::Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare_path)
            .status()
            .unwrap();
        assert!(status.success());
        let bare = identity(bare_path.to_str().unwrap()).unwrap();
        assert!(!approval_predates_worktree_coverage(storage.path(), &bare));

        // The case the common-directory guard exists for, and the only shape
        // that reaches it: a repository whose Git directory was placed inside
        // *another* repository's checkout, with `--separate-git-dir`. Deriving
        // the main checkout from `commondir`'s parent then lands on the
        // neighbour — which is a real checkout, is legacy-approved, and has
        // nothing to do with this refusal. A plainly nested `git init` does
        // not test this: its own `commondir` parent is its own checkout, so
        // the guard never fires either way.
        let outer = fixture();
        git_in(outer.path(), &["commit", "--allow-empty", "-m", "outer"]);
        let outer_id = identity(outer.path().to_str().unwrap()).unwrap();
        fs::write(
            legacy_record(storage.path(), &outer_id.checkout.path),
            serde_json::to_vec(&outer_id).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_grant(storage.path(), &outer_id).unwrap(),
            TrustScope::Checkout,
            "the neighbour really is legacy-approved"
        );

        let elsewhere = tempfile::tempdir().unwrap();
        let separate = elsewhere.path().join("checkout");
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(format!(
                "--separate-git-dir={}",
                outer.path().join("borrowed-gitdir").display()
            ))
            .arg(&separate)
            .status()
            .unwrap();
        assert!(status.success());
        let separate = identity(separate.to_str().unwrap()).unwrap();
        assert_eq!(
            separate.common_dir.path.parent(),
            Some(outer_id.checkout.path.as_path()),
            "the derived candidate really is the approved neighbour"
        );
        assert!(
            !approval_predates_worktree_coverage(storage.path(), &separate),
            "a different repository's record must never explain this refusal"
        );
    }

    /// A repository grant and a legacy record for the same checkout can both
    /// exist — the extension writes one and never deletes the other. The newer
    /// scheme has strictly more reach, so it has to win, or extending would
    /// report a downgrade for the very checkout the human started from.
    #[test]
    fn a_repository_grant_outranks_a_legacy_record_for_the_same_checkout() {
        let (repo, _parent, storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();
        assert_eq!(
            read_grant(storage.path(), &main).unwrap(),
            TrustScope::Checkout
        );

        save_grant(storage.path(), &approval_for(&main)).unwrap();
        assert!(legacy_record(storage.path(), &main.checkout.path).exists());
        assert_eq!(
            read_grant(storage.path(), &main).unwrap(),
            TrustScope::Repository
        );

        // And a *corrupt* repository record does not silently fall back to the
        // legacy one: an unreadable approval is an error, not a lesser scope.
        fs::write(
            repository_record(storage.path(), &main.common_dir.path),
            b"{",
        )
        .unwrap();
        assert!(read_grant(storage.path(), &main).is_err());
        assert!(read_grant(storage.path(), &worktree).is_err());
    }

    /// Revocation has to reach both namespaces after an extension, or
    /// "revoked" means "revoked except the half you cannot see".
    /// Both new reads sit on paths that run per refusal and per panel load, so
    /// their cost has to be bounded by the registry rather than by the work
    /// tree, and has to stay far under the hook's 5s budget even when a
    /// repository has an unreasonable number of worktrees.
    ///
    /// The ceiling is deliberately loose. This measures a directory listing
    /// and a few stats; a tight wall-clock bound here would fail on a loaded
    /// machine and say nothing about the code. The number that matters is
    /// printed, and the assertion only catches a change of complexity class.
    #[test]
    fn counting_and_explaining_stay_cheap_on_an_unreasonable_registry() {
        let repo = fixture();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let registry = current.common_dir.path.join("worktrees");
        fs::create_dir_all(&registry).unwrap();
        const ENTRIES: usize = 2000;
        for n in 0..ENTRIES {
            let entry = registry.join(format!("wt-{n}"));
            fs::create_dir(&entry).unwrap();
            fs::write(entry.join("gitdir"), b"/nowhere/.git").unwrap();
        }

        let started = std::time::Instant::now();
        let counted = worktree_count(&current);
        let counting = started.elapsed();
        assert_eq!(counted as usize, ENTRIES + 1);

        // The whole preview, which is what `cmd_repository_trust` costs.
        let started = std::time::Instant::now();
        let preview = inspect(repo.path().to_str().unwrap()).unwrap();
        let previewing = started.elapsed();
        assert_eq!(preview.worktrees as usize, ENTRIES + 1);

        // And the refusal path, which runs this once per worktree that could
        // not be read.
        let storage = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let explained = approval_predates_worktree_coverage(storage.path(), &current);
        let explaining = started.elapsed();
        assert!(!explained, "nothing is approved in this store");

        println!(
            "registry={ENTRIES} count={counting:?} inspect={previewing:?} explain={explaining:?}"
        );
        for (label, taken) in [
            ("count", counting),
            ("inspect", previewing),
            ("explain", explaining),
        ] {
            assert!(
                taken < crate::hooks::BUDGET / 5,
                "{label} took {taken:?} on {ENTRIES} entries, which threatens the {:?} hook budget",
                crate::hooks::BUDGET
            );
        }
    }

    /// A registry entry whose name or pointer is hostile must not turn a count
    /// into a traversal, a hang, or a panic. Counting reads the directory and
    /// stats one file per entry; it must never follow what an entry points at.
    #[test]
    fn counting_never_follows_what_a_registry_entry_points_at() {
        let repo = fixture();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let registry = current.common_dir.path.join("worktrees");
        fs::create_dir_all(&registry).unwrap();

        // A pointer naming a directory that does not exist, one naming the
        // registry itself (a cycle if it were followed), and one that is a
        // symlink loop. All three are counted as entries and none is walked.
        for (name, target) in [
            ("missing", "/nonexistent/place/.git"),
            ("cyclic", registry.to_str().unwrap()),
        ] {
            let entry = registry.join(name);
            fs::create_dir(&entry).unwrap();
            fs::write(entry.join("gitdir"), target.as_bytes()).unwrap();
        }
        let looped = registry.join("looped");
        fs::create_dir(&looped).unwrap();
        std::os::unix::fs::symlink(&looped, looped.join("gitdir")).unwrap();

        let started = std::time::Instant::now();
        let counted = worktree_count(&current);
        assert!(
            started.elapsed() < crate::hooks::BUDGET,
            "counting must not hang"
        );
        // `looped`'s gitdir is a symlink to a directory, so it is not a file
        // and does not count; the other two do.
        assert_eq!(counted, 3, "main checkout plus the two real pointers");
    }

    #[test]
    fn revoking_an_extended_repository_removes_both_records() {
        let (repo, _parent, storage, linked) = legacy_approved_repository();
        let main = identity(repo.path().to_str().unwrap()).unwrap();
        let worktree = identity(linked.to_str().unwrap()).unwrap();
        save_grant(storage.path(), &approval_for(&main)).unwrap();

        forget_records(
            storage.path(),
            Some(&main.common_dir.path),
            &main.checkout.path,
        )
        .unwrap();

        assert_eq!(read_grant(storage.path(), &main).unwrap(), TrustScope::None);
        assert_eq!(
            read_grant(storage.path(), &worktree).unwrap(),
            TrustScope::None
        );
        assert!(!legacy_record(storage.path(), &main.checkout.path).exists());
        assert!(!repository_record(storage.path(), &main.common_dir.path).exists());
    }

    #[test]
    fn watcher_cannot_start_before_trust_and_can_stop_after_revocation() {
        let repo = fixture();
        let path = repo.path().to_str().unwrap();
        let state = crate::watcher::WatcherState::default();
        assert!(
            crate::watcher::start_watch_inner(&state, path.into(), |_| {})
                .unwrap_err()
                .contains(REQUIRED)
        );
        let preview = inspect(path).unwrap();
        grant(path, &preview.identity, false).unwrap();
        crate::watcher::start_watch_inner(&state, path.into(), |_| {}).unwrap();
        revoke(path).unwrap();
        crate::watcher::unwatch(&state, path.into()).unwrap();
    }

    /// THE CLASS: admission resolved a path, the spawn resolved it again, and
    /// only the second one decided where the child actually ran. Substituting
    /// the directory in between authorized one checkout and executed another —
    /// whose `core.fsmonitor` and aliases are exactly what the gate exists to
    /// keep from running. No race is needed to prove it: the substitution
    /// happens between the two calls the real seam makes back to back.
    ///
    /// The holding directory is world-writable, which is precisely when
    /// another principal could do this and therefore when admission stops
    /// trusting the path and hands the child a descriptor instead.
    #[cfg(unix)]
    #[test]
    fn a_substituted_directory_cannot_redirect_an_admitted_child() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o777)).unwrap();
        let admitted_path = base.join("admitted");
        let elsewhere = base.join("elsewhere");
        fs::create_dir(&admitted_path).unwrap();
        fs::create_dir(&elsewhere).unwrap();

        let mut cmd = std::process::Command::new("/bin/pwd");
        cmd.current_dir(&admitted_path);
        // Named, not dropped: on Windows this holds the directory handles open
        // across the spawn; on Unix the re-anchor lives in the command itself.
        let _admission = check_command(&mut cmd).expect("a plain directory is admitted");

        // The directory that was admitted keeps its identity under a new name;
        // its old name now points somewhere that was never admitted.
        let moved = base.join("moved");
        fs::rename(&admitted_path, &moved).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &admitted_path).unwrap();

        let output = cmd.output().expect("spawn pwd");
        let ran_in = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
            .canonicalize()
            .unwrap();
        assert_eq!(
            ran_in, moved,
            "the child ran in a directory that was never admitted"
        );
        assert_ne!(ran_in, elsewhere.canonicalize().unwrap());
    }

    /// The fast path is taken on exactly one condition, so that condition is
    /// asserted directly rather than inferred from a timing difference: a
    /// private chain is exclusive, and one world-writable ancestor is enough
    /// to lose it.
    #[cfg(unix)]
    /// No-narrowing for the strict path, and the one that would bite hardest
    /// if the descriptor-side repository walk disagreed with `find_git_root`:
    /// an ordinary trusted checkout under a world-writable holder must still
    /// be admitted, from its root and from a subdirectory, every single time.
    ///
    /// The two walks answer the same question by different means, so a
    /// mismatch in how either reads `.git` — a worktree's `.git` file, a
    /// symlink, a bare layout — would show up here as a refusal of a
    /// repository nobody was attacking.
    #[cfg(unix)]
    #[test]
    fn a_trusted_checkout_under_a_shared_holder_is_still_admitted_every_time() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let holder = base.join("holder");
        fs::create_dir(&holder).unwrap();
        let checkout = holder.join("checkout");
        fs::create_dir(&checkout).unwrap();
        let nested = checkout.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&checkout)
            .status()
            .expect("git init");
        assert!(init.success());
        crate::test_support::trust_repo(&checkout);
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(
            !crate::fs_entry::pin_dir_nofollow(&checkout)
                .unwrap()
                .exclusive,
            "the layout must put admission on the strict path, or this proves nothing"
        );

        for cwd in [&checkout, &nested] {
            for attempt in 0..25 {
                let mut cmd = std::process::Command::new("/bin/pwd");
                cmd.current_dir(cwd);
                let _admission = check_command(&mut cmd).unwrap_or_else(|error| {
                    panic!(
                        "attempt {attempt} refused a trusted checkout at {}: {error}",
                        cwd.display()
                    )
                });
                let output = cmd.output().expect("spawn pwd");
                let ran_in = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
                    .canonicalize()
                    .unwrap();
                assert_eq!(&ran_in, cwd, "attempt {attempt} landed elsewhere");
            }
        }
    }

    // Mode bits decide this property, and `PermissionsExt::from_mode` exists
    // only on unix — the same gate its sibling above already carries.
    #[cfg(unix)]
    #[test]
    fn only_a_privately_held_path_skips_the_child_side_re_anchor() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let nested = base.join("outer").join("inner");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive,
            "a chain under a private temporary directory is reachable only by this user"
        );
        fs::set_permissions(base.join("outer"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive,
            "a world-writable holder lets another user rename the component under it"
        );
        // The leaf's own permissions are not what decides this: renaming an
        // entry needs write on the directory holding it.
        fs::set_permissions(base.join("outer"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            crate::fs_entry::pin_dir_nofollow(&nested)
                .unwrap()
                .exclusive
        );
    }

    /// Pinning must not narrow what is admissible: a repository reached
    /// through a symlinked ancestor is ordinary, and the no-follow walk runs
    /// on the canonical path precisely so it stays that way.
    #[cfg(unix)]
    #[test]
    fn a_directory_reached_through_a_symlinked_ancestor_is_still_admitted() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        fs::create_dir_all(real.join("inner")).unwrap();
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let mut cmd = std::process::Command::new("/bin/pwd");
        cmd.current_dir(alias.join("inner"));
        let _admission = check_command(&mut cmd).expect("a symlinked ancestor is not an attack");
        let output = cmd.output().expect("spawn pwd");
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
                .canonicalize()
                .unwrap(),
            real.join("inner").canonicalize().unwrap()
        );
    }

    /// Load-bearing for the whole admission design, not just for grants:
    /// discovery walks a path to find the repository that must be trusted, and
    /// that walk races anything able to rewrite the tree. It stays harmless
    /// only because approval is bound to a *location as well as an object* —
    /// so no attacker can move an already-approved checkout into the path
    /// being walked and have the grant come with it.
    #[test]
    fn a_grant_does_not_travel_with_the_directory_it_approved() {
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("approved");
        fs::create_dir(&original).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&original)
            .status()
            .unwrap();
        assert!(status.success());

        let path = original.to_str().unwrap();
        let preview = inspect(path).unwrap();
        grant(path, &preview.identity, false).unwrap();
        require(&original).expect("the approved checkout is trusted where it was approved");

        let moved = parent.path().join("relocated");
        fs::rename(&original, &moved).unwrap();
        assert!(
            require(&moved).is_err(),
            "the grant followed the directory to a path that was never approved"
        );
        assert!(
            require(&original).is_err(),
            "a vanished path still answered as trusted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_trust_records_and_fifo_metadata_fail_closed_without_waiting() {
        use std::os::unix::fs::symlink;
        let repo = fixture();
        let storage = tempfile::tempdir().unwrap();
        let current = identity(repo.path().to_str().unwrap()).unwrap();
        let data = storage.path().join("forged");
        fs::write(&data, serde_json::to_vec(&approval_for(&current)).unwrap()).unwrap();
        symlink(
            &data,
            repository_record(storage.path(), &current.common_dir.path),
        )
        .unwrap();
        assert!(read_grant(storage.path(), &current).is_err());
        // The same refusal for a pre-repository record, which is read through
        // the same guard rather than a second, laxer one.
        fs::remove_file(repository_record(storage.path(), &current.common_dir.path)).unwrap();
        symlink(&data, legacy_record(storage.path(), &current.checkout.path)).unwrap();
        assert!(read_grant(storage.path(), &current).is_err());
        let pointer = repo.path().join(".git/commondir");
        let cpath = std::ffi::CString::new(pointer.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: cpath is a live NUL-terminated path in our owned fixture.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(inspect(repo.path().to_str().unwrap()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
