//! App-wide policy, durable scheduling, bounded runs and shared UI state.
//! No UI timer owns deletion; the native worker survives page changes.
use super::{discovery, tree};
use crate::ipc_error::{CommandError, IpcErrorKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
/// Pauses before asking a shed check again within one run.
#[cfg(not(test))]
const DEFERRAL_BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(40),
];
#[cfg(test)]
const DEFERRAL_BACKOFF: [Duration; 3] = [Duration::from_millis(20); 3];
/// The most one run waits on a loaded host in all, inside the run's own
/// five-minute limit and the headless worker's fifteen.
const DEFERRAL_WAIT_BUDGET: Duration = Duration::from_secs(90);
/// How soon the next scheduled run comes after one the host shed, doubled
/// for each further shed run and never later than the saved interval.
const DEFERRAL_RETRY_FIRST_SECS: u64 = 15 * 60;
/// Shed runs in a row that bring the schedule forward before it falls back
/// to the saved interval: 15, 30, 60 and 120 minutes. A full inventory is
/// itself load, so a host that stays under pressure is not asked every few
/// minutes all night.
const MAX_DEFERRAL_RETRIES: u32 = 4;

/// Whether the host-load governor refused this check: it was never judged.
/// Read from the producer's typed kind, never from the words.
fn shed_under_load(error: &CommandError) -> bool {
    error.origin() == Some(IpcErrorKind::DeferredUnderLoad)
}
static SERVICE: OnceLock<Arc<Cleaner>> = OnceLock::new();
static SCHEDULER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CleanerConfig {
    pub version: u32,
    pub revision: u64,
    pub roots: Vec<String>,
    pub exclusions: Vec<String>,
    pub enabled: bool,
    #[serde(default)]
    pub run_when_closed: bool,
    pub interval_hours: u32,
    pub next_run_at: u64,
    pub retention_days: u32,
    pub max_run_bytes: u64,
    pub max_targets: u32,
    /// Behaviour that makes the cleaner delete more. Every scope is off by
    /// default, and no run uses a scope until a person has previewed the
    /// current set in the app (`Saved::previewed_scopes`). Omitted from the
    /// saved state while all off, so an older build can still read the file.
    #[serde(default, skip_serializing_if = "ReclaimScopes::is_default")]
    pub scopes: ReclaimScopes,
    /// How many linked worktrees may hold a cloned build cache. Read by
    /// worktree creation, in the app and in the `WorktreeCreate` hook, which
    /// is why it lives in this file rather than in app storage. Omitted from
    /// the saved state while at its default.
    #[serde(default, skip_serializing_if = "CloneBudget::is_default")]
    pub clone_budget: CloneBudget,
}

/// Opt-in reclaim scopes. Each one widens what the cleaner may delete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReclaimScopes {
    /// Inventory linked worktrees under the roots, and offer the regenerable
    /// build caches worktree creation clones into them (`target`, `.turbo`)
    /// when no process works in the worktree, its tree is clean, no task lease
    /// names its branch, nothing in the cache is open, and nothing in it was
    /// modified for `worktree_idle_days`. Budgeted by clone-private bytes.
    #[serde(default)]
    pub worktree_caches: bool,
    /// Judge the uncommitted-work veto per tree: a repository's output is
    /// refused only for uncommitted work in the project directory that
    /// produced it, not anywhere in the repository.
    #[serde(default)]
    pub per_tree_vetoes: bool,
    /// Remove linked worktrees the stale-worktree report calls stale (no
    /// process inside, clean, unlocked, integrated or idle past
    /// `worktree_idle_days`) with `git worktree remove`, never forced.
    #[serde(default)]
    pub prune_worktrees: bool,
    /// Idle days for the two worktree scopes. 7 to 3650: the scheduled
    /// minimum DevCouncil's retention policy sets.
    #[serde(default = "default_worktree_idle_days")]
    pub worktree_idle_days: u32,
}
fn default_worktree_idle_days() -> u32 {
    7
}
impl Default for ReclaimScopes {
    fn default() -> Self {
        Self {
            worktree_caches: false,
            per_tree_vetoes: false,
            prune_worktrees: false,
            worktree_idle_days: default_worktree_idle_days(),
        }
    }
}
impl ReclaimScopes {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn any(&self) -> bool {
        self.worktree_caches || self.per_tree_vetoes || self.prune_worktrees
    }
    /// What a run may use: the saved scopes when a person previewed exactly
    /// this set, otherwise none of them.
    fn effective(self, previewed: Option<ReclaimScopes>) -> ReclaimScopes {
        if previewed == Some(self) {
            self
        } else {
            ReclaimScopes {
                worktree_idle_days: self.worktree_idle_days,
                ..ReclaimScopes::default()
            }
        }
    }
}

/// The build-cache clone budget worktree creation enforces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloneBudget {
    /// Linked worktrees of one repository that may hold a cloned Cargo
    /// `target` at once; 0 is no limit. Past it, a new worktree is created
    /// without one and its report says why.
    #[serde(default = "default_max_target_clones")]
    pub max_target_clones: u32,
    /// Advisory total of clone-private bytes the Storage panel compares the
    /// linked worktrees' sizes against; 0 is no limit. Never enforced at
    /// creation: measuring it there would walk every worktree.
    #[serde(default = "default_max_private_bytes")]
    pub max_private_bytes: u64,
}
/// No limit by default. Measured 2026-11-14: a worktree's cloned `target` held
/// 1.8 MB of private bytes against 66 GB logical, while a worktree that built
/// cold held 26.7 GB. Refusing the clone makes the next build cold, so a count
/// limit costs disk rather than saving it; it stays available as a setting.
pub(crate) const DEFAULT_MAX_TARGET_CLONES: u32 = 0;
/// The advisory total for linked worktrees' private bytes: 50 GiB, about two
/// cold Cargo builds of this size. A warning in Storage, never enforced.
pub(crate) const DEFAULT_MAX_WORKTREE_PRIVATE_BYTES: u64 = 50 * 1024 * 1024 * 1024;
fn default_max_target_clones() -> u32 {
    DEFAULT_MAX_TARGET_CLONES
}
fn default_max_private_bytes() -> u64 {
    DEFAULT_MAX_WORKTREE_PRIVATE_BYTES
}
impl Default for CloneBudget {
    fn default() -> Self {
        Self {
            max_target_clones: DEFAULT_MAX_TARGET_CLONES,
            max_private_bytes: DEFAULT_MAX_WORKTREE_PRIVATE_BYTES,
        }
    }
}
impl CloneBudget {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
impl Default for CleanerConfig {
    fn default() -> Self {
        Self {
            version: VERSION,
            revision: 0,
            roots: vec![],
            exclusions: vec![],
            enabled: false,
            run_when_closed: false,
            interval_hours: 168,
            next_run_at: 0,
            retention_days: 30,
            max_run_bytes: 10 * 1024 * 1024 * 1024,
            max_targets: 20,
            scopes: ReclaimScopes::default(),
            clone_budget: CloneBudget::default(),
        }
    }
}
impl CleanerConfig {
    pub fn validate(&self) -> Result<(), CommandError> {
        if self.version != VERSION {
            return Err("Unsupported cleaner policy version".into());
        }
        devmap_query::hygiene::validate_retention(self.retention_days, true)?;
        if !(1..=720).contains(&self.interval_hours)
            || !(1..=100).contains(&self.max_targets)
            || !(1..=1024u64.pow(4)).contains(&self.max_run_bytes)
        {
            return Err(
                "Interval, byte budget or target count is outside its supported bounds".into(),
            );
        }
        if self.roots.len() > 16
            || self.exclusions.len() > 128
            || self.roots.iter().chain(&self.exclusions).any(|p| {
                p.len() > 4096 || p.chars().any(char::is_control) || !Path::new(p).is_absolute()
            })
        {
            return Err("Invalid cleaner path inventory".into());
        }
        devmap_query::hygiene::validate_retention(self.scopes.worktree_idle_days, true)
            .map_err(|e| format!("Worktree idle days: {e}"))?;
        if self.clone_budget.max_target_clones > 1000
            || self.clone_budget.max_private_bytes > 1024u64.pow(5)
        {
            return Err("Clone budget is outside its supported bounds".into());
        }
        if self.enabled && (self.roots.is_empty() || self.next_run_at == 0) {
            return Err("A schedule needs selected roots and a next run time".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanerCandidate {
    pub repo_path: String,
    /// Relative to `repo_path`, except for a `prune-worktree` candidate,
    /// where it is the worktree's absolute path and `repo_path` its
    /// repository.
    pub path: String,
    pub provider: String,
    pub bytes: u64,
    /// `repository` (the default), `worktree-cache` or `prune-worktree`: the
    /// reclaim scope that offered it.
    #[serde(default = "repository_scope")]
    pub scope: String,
    /// What removing it frees now (clone-private bytes), when known. The
    /// worktree scopes budget with this; `bytes` is logical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_bytes: Option<u64>,
    /// The worktree's branch, for the per-tree lease check. Not on the wire.
    #[serde(skip)]
    pub branch: Option<String>,
}
const SCOPE_REPOSITORY: &str = "repository";
const SCOPE_WORKTREE_CACHE: &str = "worktree-cache";
const SCOPE_PRUNE_WORKTREE: &str = "prune-worktree";
fn repository_scope() -> String {
    SCOPE_REPOSITORY.into()
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanerInventory {
    pub candidates: Vec<CleanerCandidate>,
    pub repositories: u32,
    pub visited_entries: u32,
    pub partial: bool,
    pub issues: Vec<String>,
    /// Repositories whose inventory could not be enumerated at all, so no
    /// candidate from them can be trusted: the scan returned an error
    /// (including `REPOSITORY_TRUST_REQUIRED`), git declined to answer whether
    /// their directories are ignored or tracked, or the workspace candidate cap
    /// cut their artifact list off mid-enumeration. Their artifacts are never
    /// removed.
    ///
    /// Deliberately NOT recorded for a merely truncated repository scan. That
    /// is a statement about the repository's totals and artifact list, not
    /// about any one tree, and it is routinely true of a healthy repository
    /// (more linked worktrees than the sizing cap, more artifact directories
    /// than the reporting cap). Whether a particular tree was enumerated to
    /// the end is `ArtifactDir::scan_complete`, which `inspect` filters on per
    /// candidate; keying this set on `scan.truncated` instead reinstated the
    /// whole-repository veto and left the largest trees unreclaimable.
    /// Workspace-level incompleteness is reported by `partial`.
    ///
    /// Internal to the run path, never part of the IPC payload: `serde(skip)`
    /// keeps the wire contract that scripts/check-coverage-types.mjs pins
    /// against src/lib/storage/hygiene/globalTypes.ts unchanged.
    #[serde(skip)]
    pub incomplete_repos: BTreeSet<String>,
    /// Of those, repositories whose scan the host shed under load: not read,
    /// and not judged either. Internal to the run path like `incomplete_repos`.
    #[serde(skip)]
    pub shed_repos: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanerItem {
    pub repo_path: String,
    pub path: String,
    pub status: String,
    pub message: String,
    pub bytes_before: u64,
    pub bytes_after: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanerRun {
    pub id: String,
    pub trigger: String,
    pub started_at: u64,
    pub finished_at: u64,
    pub status: String,
    pub repositories: u32,
    pub partial: bool,
    pub issues: Vec<String>,
    pub items: Vec<CleanerItem>,
    /// Candidates that were refused or could not be examined but have no row
    /// in `items`, because the history keeps at most [`MAX_RUN_ITEMS`] rows.
    /// Omitted from the saved state while zero, so a file written by this
    /// build stays readable by a build that predates the field.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unlisted: u32,
    /// Candidates that were never judged because the host shed the checks
    /// under load (status `unexamined`), counted whether or not they are
    /// listed. Not a refusal: GitPulse retries them sooner than the schedule.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unexamined: u32,
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
/// The most rows one run keeps: [`Cleaner::read`] refuses a history holding
/// more, and an unreadable history stops every later run.
const MAX_RUN_ITEMS: usize = 100;
/// The most issues one run keeps, for the same reason.
const MAX_RUN_ISSUES: usize = 128;
fn push_issue(issues: &mut Vec<String>, issue: String) {
    if issues.len() < MAX_RUN_ISSUES {
        issues.push(issue);
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    config: CleanerConfig,
    history: Vec<CleanerRun>,
    cancel_run: Option<String>,
    #[serde(default)]
    background_error: Option<String>,
    /// Consecutive runs that ended with checks the host shed under load, each
    /// of which brought the next scheduled run forward. Reset by a run the
    /// host let examine everything. Omitted while zero so an older build can
    /// still read the file.
    #[serde(default, skip_serializing_if = "is_zero")]
    deferral_retries: u32,
    /// The reclaim scopes as they stood when a person last previewed them in
    /// the app (`inventory`). Written only there, never from the policy the
    /// frontend saves, so an edit cannot carry its own acknowledgement. Any
    /// change to the scopes clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previewed_scopes: Option<ReclaimScopes>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanerState {
    pub config: CleanerConfig,
    pub history: Vec<CleanerRun>,
    pub running: bool,
    pub supported: bool,
    pub background_supported: bool,
    pub background_error: Option<String>,
    pub agent_rules: String,
    /// The saved reclaim scopes include at least one, and a person has
    /// previewed exactly this set since it last changed: runs use them.
    pub scopes_previewed: bool,
}

#[cfg(test)]
type ActivityCheck = fn(&Path) -> Result<(), String>;
#[cfg(test)]
type QuietCheck = fn(&str) -> Result<(), CommandError>;

pub struct Cleaner {
    dir: PathBuf,
    cancel: AtomicBool,
    active: AtomicBool,
    scanning: AtomicBool,
    current: Mutex<Option<(String, String)>>,
    #[cfg(test)]
    activity_override: Option<ActivityCheck>,
    /// Replaces the repository-state check, so a test can make the host shed
    /// it without putting the machine under memory pressure.
    #[cfg(test)]
    quiet_override: Option<QuietCheck>,
    #[cfg(test)]
    state_after_read: Option<Box<dyn Fn() + Send + Sync>>,
}
// Explicit unlock matters after fork/dup: closing only the owner's descriptor
// can leave a shared file description locked until a child reaches exec.
pub(super) struct CleanerLock(File);
impl std::ops::Deref for CleanerLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl Drop for CleanerLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            log::error!("Cleaner lock could not be released: {error}");
        }
    }
}
struct MonitorStop(Arc<AtomicBool>);
impl Drop for MonitorStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Reset<'a>(&'a AtomicBool);
impl Drop for Reset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Cleaner {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cancel: AtomicBool::new(false),
            active: AtomicBool::new(false),
            scanning: AtomicBool::new(false),
            current: Mutex::new(None),
            #[cfg(test)]
            activity_override: None,
            #[cfg(test)]
            quiet_override: None,
            #[cfg(test)]
            state_after_read: None,
        }
    }
    /// The state veto for one candidate, judged at the scope that offered
    /// it: the whole repository by default, the producing project directory
    /// under `per_tree_vetoes`, the linked worktree itself for the worktree
    /// scopes.
    fn quiet_tree(
        &self,
        candidate: &CleanerCandidate,
        scopes: ReclaimScopes,
    ) -> Result<(), CommandError> {
        #[cfg(test)]
        if let Some(check) = self.quiet_override {
            return check(&candidate.repo_path);
        }
        match candidate.scope.as_str() {
            SCOPE_WORKTREE_CACHE => {
                quiet_worktree(&candidate.repo_path, candidate.branch.as_deref())
            }
            SCOPE_PRUNE_WORKTREE => quiet_worktree(&candidate.path, candidate.branch.as_deref()),
            _ if scopes.per_tree_vetoes => quiet_project(&candidate.repo_path, &candidate.path),
            _ => quiet_repo(&candidate.repo_path),
        }
    }
    /// [`Self::quiet`], asked again after a short pause when the host shed it,
    /// while this run's wait budget lasts. Waits are cancellable and recheck
    /// the run's authority. The check keeps its own admission class: raising
    /// it to a user action would defeat the governor on a host that is
    /// shedding work because it is near its limit.
    fn quiet_with_backoff(
        &self,
        config: &CleanerConfig,
        run_id: &str,
        candidate: &CleanerCandidate,
        scopes: ReclaimScopes,
        waited: &mut Duration,
    ) -> Result<(), CommandError> {
        let mut pauses = DEFERRAL_BACKOFF.iter();
        loop {
            match self.quiet_tree(candidate, scopes) {
                Err(error) if shed_under_load(&error) => {
                    let Some(pause) = pauses
                        .next()
                        .filter(|pause| *waited + **pause <= DEFERRAL_WAIT_BUDGET)
                    else {
                        return Err(error);
                    };
                    self.pause(*pause, config, run_id)?;
                    *waited += *pause;
                }
                other => return other,
            }
        }
    }
    fn pause(
        &self,
        total: Duration,
        config: &CleanerConfig,
        run_id: &str,
    ) -> Result<(), CommandError> {
        let until = Instant::now() + total;
        loop {
            self.still_authorized(config, run_id)?;
            let now = Instant::now();
            if now >= until {
                return Ok(());
            }
            std::thread::sleep((until - now).min(Duration::from_millis(200)));
        }
    }
    /// After a run some of whose checks the host shed, brings an enabled
    /// schedule's next run forward instead of leaving those trees for a whole
    /// interval: 15 minutes, doubling per shed run in a row, at most
    /// [`MAX_DEFERRAL_RETRIES`] times, never later than the saved next run.
    /// A run that examined everything resets the count. Returns the new next
    /// run time when it moved.
    fn retry_sooner_after_shedding(
        &self,
        ran_with: &CleanerConfig,
        run: &CleanerRun,
        now: u64,
    ) -> Result<Option<u64>, CommandError> {
        self.update(|saved| {
            if run.unexamined == 0 {
                saved.deferral_retries = 0;
                return Ok(None);
            }
            let config = &mut saved.config;
            if !config.enabled
                || config.revision != ran_with.revision
                || saved.deferral_retries >= MAX_DEFERRAL_RETRIES
            {
                return Ok(None);
            }
            let delay = (DEFERRAL_RETRY_FIRST_SECS << saved.deferral_retries)
                .min(u64::from(config.interval_hours) * 3600);
            saved.deferral_retries += 1;
            let at = now.saturating_add(delay);
            if at >= config.next_run_at {
                return Ok(None);
            }
            config.next_run_at = at;
            config.revision = config
                .revision
                .checked_add(1)
                .ok_or("Policy revision exhausted")?;
            Ok(Some(at))
        })
    }
    fn activity(&self, path: &Path) -> Result<(), CommandError> {
        #[cfg(test)]
        if let Some(check) = self.activity_override {
            return check(path).map_err(CommandError::from);
        }
        super::activity_check(path)
    }
    fn ensure_dir(&self) -> Result<(), CommandError> {
        // Refuse existing symlink ancestors before creating app-owned state.
        for ancestor in self.dir.ancestors() {
            if ancestor.try_exists().map_err(|e| e.to_string())? {
                tree::no_symlinks(ancestor)?;
                break;
            }
        }
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        tree::no_symlinks(&self.dir).map_err(CommandError::from)
    }
    fn open_lock(&self, name: &str) -> Result<File, CommandError> {
        self.ensure_dir()?;
        let path = self.dir.join(name);
        if path.try_exists().map_err(|e| e.to_string())? {
            tree::no_symlinks(&path)?;
        }
        let mut options = File::options();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let file = options.open(path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Cleaner lock is not a regular file".into());
        }
        Ok(file)
    }
    fn lock(&self, name: &str) -> Result<CleanerLock, CommandError> {
        let file = self.open_lock(name)?;
        file.try_lock()
            .map_err(|e| format!("Cleaner is busy or its lock is unavailable: {e}"))?;
        Ok(CleanerLock(file))
    }
    fn read(&self) -> Result<Saved, CommandError> {
        let path = self.dir.join("state.json");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
            Err(e) => return Err(e.to_string().into()),
            Ok(m) if !m.is_file() || m.len() > MAX_STATE_BYTES => {
                return Err("Cleaner state is not a bounded regular file".into())
            }
            Ok(_) => {}
        }
        tree::no_symlinks(&path)?;
        let mut bytes = vec![];
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("Cleaner state exceeded its budget".into());
        }
        let saved: Saved =
            serde_json::from_slice(&bytes).map_err(|e| format!("Cleaner state is invalid: {e}"))?;
        saved.config.validate()?;
        if saved.history.len() > 20
            || saved
                .history
                .iter()
                .any(|r| r.items.len() > MAX_RUN_ITEMS || r.issues.len() > MAX_RUN_ISSUES)
        {
            return Err("Cleaner history exceeds its bounds".into());
        }
        Ok(saved)
    }
    fn update<T>(
        &self,
        edit: impl FnOnce(&mut Saved) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let _lock = self.lock("state.lock")?;
        let mut saved = self.read()?;
        let result = edit(&mut saved)?;
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("Cleaner state exceeded its budget".into());
        }
        devmap_query::write_atomic(&self.dir.join("state.json"), &bytes)
            .map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn state(&self) -> Result<CleanerState, CommandError> {
        // Establish run ownership before reading history. An idle observation
        // holds the lease through the snapshot so a worker cannot start behind
        // it. A busy observation may briefly show a just-completed run as busy,
        // but can never label its stale unfinished history as interrupted.
        let run_lease = if self.active.load(Ordering::SeqCst) {
            None
        } else {
            let file = self.open_lock("run.lock")?;
            match file.try_lock() {
                Ok(()) => Some(CleanerLock(file)),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(format!("Run lock is unavailable: {error}").into());
                }
            }
        };
        let running = run_lease.is_none();
        let mut saved = self.read()?;
        #[cfg(test)]
        if let Some(after_read) = &self.state_after_read {
            after_read();
        }
        if !running {
            for run in saved.history.iter_mut().filter(|r| r.finished_at == 0) {
                run.status = "interrupted".into();
                if run.issues.len() < 128 {
                    run.issues
                        .push("The previous process stopped before reporting completion".into());
                }
            }
        }
        let scopes_previewed =
            saved.config.scopes.any() && saved.previewed_scopes == Some(saved.config.scopes);
        Ok(CleanerState {
            config: saved.config,
            history: saved.history,
            running,
            supported: cfg!(unix),
            background_supported: super::background::supported(),
            background_error: saved.background_error,
            agent_rules: devmap_query::hygiene::AGENT_RULES.into(),
            scopes_previewed,
        })
    }
    pub fn save(&self, config: CleanerConfig) -> Result<CleanerState, CommandError> {
        if config.enabled && config.run_when_closed && !super::background::supported() {
            return Err(
                "Closed-app scheduling requires an installed macOS application bundle".into(),
            );
        }
        self.save_with_background(config, super::background::configure)
    }
    fn save_with_background(
        &self,
        mut config: CleanerConfig,
        configure: impl FnOnce(bool) -> Result<(), CommandError>,
    ) -> Result<CleanerState, CommandError> {
        let _settings_lock = self.lock("settings.lock")?;
        config.validate()?;
        config.roots = discovery::roots(&config.roots, false)?
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        config.exclusions = discovery::roots(&config.exclusions, true)?
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if config.enabled && !cfg!(unix) {
            return Err("Scheduled cleanup is unavailable on this platform".into());
        }
        if config.enabled && config.next_run_at > super::now().saturating_add(366 * 86400) {
            return Err("Schedule must start within the next year".into());
        }
        let background_change = self.update(|saved| {
            if config.revision != saved.config.revision {
                return Err("Settings changed in another window; reload before saving".into());
            }
            config.revision = config
                .revision
                .checked_add(1)
                .ok_or("Policy revision exhausted")?;
            if config.scopes != saved.config.scopes {
                // A changed scope set has not been seen by anyone yet.
                saved.previewed_scopes = None;
            }
            let previous = saved.config.enabled && saved.config.run_when_closed;
            let next = config.enabled && config.run_when_closed;
            let reconcile = previous || next || saved.background_error.is_some();
            saved.config = config.clone();
            // Commit disabled authority before any OS operation. A crash or a
            // failed recovery write cannot strand an enabled schedule.
            if reconcile {
                saved.config.enabled = false;
                saved.config.run_when_closed = false;
            }
            saved.background_error = reconcile
                .then(|| "Background setup was interrupted; save the policy to retry".into());
            if let Some(run) = saved.history.first().filter(|r| r.finished_at == 0) {
                saved.cancel_run = Some(run.id.clone());
            }
            Ok(reconcile.then_some(next))
        })?;
        self.stop_current()?;
        if let Some(enable) = background_change {
            if let Err(error) = configure(enable) {
                // Never show an active schedule after registration failed. A
                // leftover launchd job can only read this disabled authority.
                self.update(|saved| {
                    saved.background_error = Some(error.message().to_string());
                    Ok(())
                })?;
                self.stop_current()?;
                return Err(error.wrap(|error| {
                    format!("Schedule disabled because background setup failed: {error}")
                }));
            }
            self.update(|saved| {
                if saved.config.revision != config.revision {
                    return Err("Settings changed during background setup".into());
                }
                config.revision = config
                    .revision
                    .checked_add(1)
                    .ok_or("Policy revision exhausted")?;
                saved.config = config;
                saved.background_error = None;
                Ok(())
            })?;
        }
        self.state()
    }
    fn stop_current(&self) -> Result<(), CommandError> {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some((repo, id)) = self
            .current
            .lock()
            .map_err(|_| "Active cleaner plan is unavailable")?
            .as_ref()
        {
            super::cancel(repo, id)?;
        }
        Ok(())
    }
    pub fn cancel(&self) -> Result<CleanerState, CommandError> {
        self.stop_current()?;
        self.update(|saved| {
            if let Some(run) = saved.history.first().filter(|r| r.finished_at == 0) {
                saved.cancel_run = Some(run.id.clone());
            }
            Ok(())
        })?;
        self.stop_current()?;
        self.state()
    }
    fn still_authorized(&self, config: &CleanerConfig, id: &str) -> Result<(), CommandError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("Cleanup cancelled".into());
        }
        let saved = self.read()?;
        if saved.config.revision != config.revision || saved.cancel_run.as_deref() == Some(id) {
            return Err("Cleanup cancelled or settings changed".into());
        }
        Ok(())
    }
    pub fn inventory(&self) -> Result<CleanerInventory, CommandError> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err("Global inventory is already running".into());
        }
        let _reset = Reset(&self.scanning);
        if self.active.load(Ordering::SeqCst) {
            return Err("Wait for cleanup to finish before inspecting".into());
        }
        self.cancel.store(false, Ordering::SeqCst);
        let config = self.state()?.config;
        // The preview shows every enabled scope, previewed or not: seeing
        // what a scope would take is the point of it.
        let inventory = self.inspect(&config, config.scopes, true)?;
        if config.scopes.any() {
            self.update(|saved| {
                if saved.config.scopes == config.scopes {
                    saved.previewed_scopes = Some(config.scopes);
                }
                Ok(())
            })?;
        }
        Ok(inventory)
    }
    fn inspect(
        &self,
        config: &CleanerConfig,
        scopes: ReclaimScopes,
        measure: bool,
    ) -> Result<CleanerInventory, CommandError> {
        let roots = discovery::roots(&config.roots, false)?;
        let excludes = discovery::roots(&config.exclusions, true)?;
        let found = discovery::discover(&roots, &excludes, &self.cancel);
        let mut result = CleanerInventory {
            repositories: found.repos.len() as u32,
            visited_entries: found.visited,
            partial: found.partial,
            issues: found.issues,
            candidates: vec![],
            incomplete_repos: BTreeSet::new(),
            shed_repos: 0,
        };
        let start = Instant::now();
        let discovered: BTreeSet<PathBuf> = found.repos.clone();
        for repo in found.repos {
            if start.elapsed() > Duration::from_secs(120)
                || self.cancel.load(Ordering::SeqCst)
                || result.candidates.len() >= 512
            {
                result.partial = true;
                result
                    .issues
                    .push("Inventory budget reached or cancelled".into());
                break;
            }
            let repo_path = repo.to_str().ok_or("Non-UTF-8 repository")?;
            match crate::storage::scan_storage(repo_path) {
                Ok(report) => {
                    // A truncated repository scan sets `partial` and says so,
                    // and it deliberately does NOT record the repository as
                    // incomplete. `scan.truncated` means the repository's
                    // totals are floors and its artifact list may be a sample;
                    // it is true for reasons that belong to no particular tree
                    // (linked worktrees past the sizing cap, a 65th artifact
                    // directory, a flat source directory past the entry cap).
                    // Recording the repository here dropped every one of its
                    // candidates, which is how a 64 GiB `rust/target` that had
                    // been enumerated to the end stayed unreclaimable because
                    // the repository it lives in has 17 linked worktrees and
                    // the sizing cap is 8. Per-tree completeness is carried by
                    // `ArtifactDir::scan_complete` and filtered below.
                    //
                    // "Something here was not examined" is carried by
                    // `partial`, and it is derived from per-tree completeness
                    // directly, below, rather than inferred from
                    // `scan.truncated`. The refusal in `perform_with_gate`
                    // rests on that flag: without this, a workspace whose every
                    // tree was cut short would produce no candidates, no
                    // recorded incomplete repository and therefore no refusal,
                    // and would report a clean, complete run of zero items. It
                    // is a derivation and not an assertion so it cannot drift
                    // if the walk's own propagation ever changes.
                    if report.artifacts.iter().any(|a| !a.scan_complete) {
                        result.partial = true;
                        if result.issues.len() < 128 {
                            result
                                .issues
                                .push(format!("Artifact tree not read in full: {repo_path}"));
                        }
                    }
                    if report.scan.truncated || report.scan.permission_denied > 0 {
                        result.partial = true;
                        if result.issues.len() < 128 {
                            result
                                .issues
                                .push(format!("Partial repository scan: {repo_path}"));
                        }
                    }
                    if report
                        .artifacts
                        .iter()
                        .any(|artifact| artifact.checks_unexamined)
                    {
                        result.partial = true;
                        result.incomplete_repos.insert(repo_path.to_string());
                        if result.issues.len() < 128 {
                            result.issues.push(format!(
                                "Ignore or tracked-file check did not run: {repo_path}"
                            ));
                        }
                    }
                    for artifact in report.artifacts {
                        if result.candidates.len() >= 512 {
                            result.partial = true;
                            // This repository's artifacts were cut off
                            // mid-enumeration, so the ones already collected
                            // do not represent a tree that was read in full.
                            result.incomplete_repos.insert(repo_path.to_string());
                            break;
                        }
                        // Removal needs this tree read in full: a walk that
                        // stopped inside it cannot say whether something
                        // unexpected is in there, and its byte figure is a
                        // floor. The repository-wide `scan.truncated` above is
                        // a different claim and is deliberately not consulted
                        // here — a repository is partial for reasons that have
                        // nothing to do with any one tree (linked worktrees
                        // past the sizing cap, artifacts past the reporting
                        // cap, a flat source directory past the entry cap),
                        // and reading it as "nothing here was examined"
                        // disqualified every tree the scan had in fact read to
                        // the end.
                        if !artifact.scan_complete
                            || artifact.tracked_files > 0
                            || artifact.unignored
                            || artifact.checks_unexamined
                            || excludes
                                .iter()
                                .any(|p| repo.join(&artifact.path).starts_with(p))
                        {
                            continue;
                        }
                        if let Ok(provider) =
                            super::providers::local_provider(&repo, &artifact.path)
                        {
                            result.candidates.push(CleanerCandidate {
                                repo_path: repo_path.into(),
                                path: artifact.path,
                                provider: provider.into(),
                                bytes: artifact.bytes,
                                scope: SCOPE_REPOSITORY.into(),
                                private_bytes: artifact.private_bytes,
                                branch: None,
                            });
                        }
                    }
                }
                Err(e) => {
                    result.partial = true;
                    if shed_under_load(&e) {
                        result.shed_repos = result.shed_repos.saturating_add(1);
                    }
                    // Includes REPOSITORY_TRUST_REQUIRED: a repository the user
                    // has never trusted in the UI is one whose contents were
                    // never read, not one known to hold nothing worth keeping.
                    result.incomplete_repos.insert(repo_path.to_string());
                    if result.issues.len() < 128 {
                        result
                            .issues
                            .push(format!("{repo_path}: {e}", e = e.message()));
                    }
                }
            }
            // After the repository's own scan, so a scope that walks many
            // worktrees cannot spend the inventory budget before the
            // repository itself is read.
            if scopes.worktree_caches || scopes.prune_worktrees {
                self.worktree_candidates(
                    &repo,
                    &roots,
                    &excludes,
                    &discovered,
                    scopes,
                    measure,
                    start,
                    &mut result,
                );
            }
        }
        result.candidates.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.repo_path.cmp(&b.repo_path))
                .then_with(|| a.path.cmp(&b.path))
        });
        result.issues.truncate(128);
        Ok(result)
    }
    /// Candidates the worktree scopes offer in one repository's linked
    /// worktrees: their regenerable build caches (`worktree_caches`) and the
    /// stale worktrees themselves (`prune_worktrees`). Only worktrees under a
    /// configured root and outside every exclusion, and never one discovery
    /// already found as a repository of its own.
    #[allow(clippy::too_many_arguments)]
    fn worktree_candidates(
        &self,
        repo: &Path,
        roots: &[PathBuf],
        excludes: &[PathBuf],
        discovered: &BTreeSet<PathBuf>,
        scopes: ReclaimScopes,
        measure: bool,
        start: Instant,
        result: &mut CleanerInventory,
    ) {
        let Some(repo_path) = repo.to_str() else {
            return;
        };
        let inside = |p: &Path| {
            roots.iter().any(|r| p.starts_with(r)) && !excludes.iter().any(|e| p.starts_with(e))
        };
        let over_budget = |result: &CleanerInventory| {
            start.elapsed() > Duration::from_secs(120)
                || self.cancel.load(Ordering::SeqCst)
                || result.candidates.len() >= 512
        };
        let note = |result: &mut CleanerInventory, error: &CommandError, what: String| {
            result.partial = true;
            if shed_under_load(error) {
                result.shed_repos = result.shed_repos.saturating_add(1);
            }
            push_issue(&mut result.issues, format!("{what}: {}", error.message()));
        };
        if scopes.worktree_caches {
            let infos = match crate::engine::worktree::list_worktrees_lite(repo_path) {
                Ok(infos) => infos,
                Err(e) => {
                    note(
                        result,
                        &e,
                        format!("{repo_path}: linked worktrees could not be listed"),
                    );
                    return;
                }
            };
            for info in infos
                .iter()
                .filter(|i| !i.is_main && !i.is_bare && !i.is_locked && !i.is_prunable)
            {
                if over_budget(result) {
                    result.partial = true;
                    push_issue(
                        &mut result.issues,
                        "Inventory budget reached or cancelled".into(),
                    );
                    return;
                }
                let Ok(path) = crate::engine::git_cli::canonicalize_plain(Path::new(&info.path))
                else {
                    continue;
                };
                if !inside(&path) || discovered.contains(&path) {
                    continue;
                }
                // A worktree something is working in is in use, not a
                // finding: it is passed over without an issue.
                match crate::sysload::procscan::working_in(&path) {
                    Ok(pids) if pids.is_empty() => {}
                    Ok(_) => continue,
                    Err(e) => {
                        result.partial = true;
                        push_issue(
                            &mut result.issues,
                            format!(
                                "{}: processes could not be read, so its caches were left alone: {e}",
                                info.path
                            ),
                        );
                        continue;
                    }
                }
                let worktree = path.to_string_lossy().into_owned();
                match crate::storage::scan_storage(&worktree) {
                    Ok(report) => {
                        for artifact in report.artifacts {
                            let name = Path::new(&artifact.path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or_default();
                            if !crate::engine::cow_clone::STANDARD_CACHE_DIRS.contains(&name)
                                || !artifact.scan_complete
                                || artifact.tracked_files > 0
                                || artifact.unignored
                                || artifact.checks_unexamined
                                || !inside(&path.join(&artifact.path))
                            {
                                continue;
                            }
                            if result.candidates.len() >= 512 {
                                result.partial = true;
                                result.incomplete_repos.insert(worktree.clone());
                                break;
                            }
                            if let Ok(provider) =
                                super::providers::local_provider(&path, &artifact.path)
                            {
                                result.candidates.push(CleanerCandidate {
                                    repo_path: worktree.clone(),
                                    path: artifact.path,
                                    provider: provider.into(),
                                    bytes: artifact.bytes,
                                    scope: SCOPE_WORKTREE_CACHE.into(),
                                    private_bytes: artifact.private_bytes,
                                    branch: info.branch.clone(),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        result.incomplete_repos.insert(worktree.clone());
                        note(result, &e, worktree.clone());
                    }
                }
            }
        }
        if scopes.prune_worktrees && !over_budget(result) {
            match crate::storage::stale_worktrees::stale_worktrees_with(
                repo_path,
                scopes.worktree_idle_days,
                &crate::sysload::procscan::working_in,
                measure,
                current_time(),
            ) {
                Ok(report) => {
                    for issue in report.issues {
                        push_issue(&mut result.issues, format!("{repo_path}: {issue}"));
                    }
                    for row in report.worktrees.into_iter().filter(|w| w.stale) {
                        let Ok(path) =
                            crate::engine::git_cli::canonicalize_plain(Path::new(&row.path))
                        else {
                            continue;
                        };
                        if !inside(&path) || discovered.contains(&path) {
                            continue;
                        }
                        result.candidates.push(CleanerCandidate {
                            repo_path: repo_path.into(),
                            path: path.to_string_lossy().into_owned(),
                            provider: "Stale linked worktree".into(),
                            bytes: row.size.as_ref().map_or(0, |s| s.bytes),
                            scope: SCOPE_PRUNE_WORKTREE.into(),
                            private_bytes: row.size.as_ref().and_then(|s| s.private_bytes),
                            branch: row.branch,
                        });
                    }
                }
                Err(e) => note(
                    result,
                    &e,
                    format!("{repo_path}: stale worktrees could not be listed"),
                ),
            }
        }
    }
    pub fn start(
        self: &Arc<Self>,
        scheduled: bool,
        revision: u64,
        now: u64,
    ) -> Result<CleanerState, CommandError> {
        if !cfg!(unix) {
            return Err("Cleanup is unavailable on this platform".into());
        }
        if self.active.swap(true, Ordering::SeqCst) {
            return Err("Cleanup is already running".into());
        }
        let reservation = Reset(&self.active);
        if self.scanning.load(Ordering::SeqCst) {
            return Err("Wait for the inventory to finish".into());
        }
        let run_lock = self.lock("run.lock")?;
        let (config,run)=self.update(|saved| {
            let config=&mut saved.config;
            if config.revision!=revision {return Err("Policy changed; reload before running".into());}
            if config.roots.is_empty() {return Err("Choose and save at least one project root".into());}
            if scheduled && (!config.enabled || now<config.next_run_at) {return Err("Schedule is not due".into());}
            if let Some(old)=saved.history.first_mut().filter(|r|r.finished_at==0) {old.status="interrupted".into();old.finished_at=now;if old.issues.len()<128 {old.issues.push("Previous process ended before reporting completion; no operation is replayed".into());}}
            if config.enabled {
                config.next_run_at=now.checked_add(u64::from(config.interval_hours)*3600).ok_or("Schedule time overflow")?;
                config.revision=config.revision.checked_add(1).ok_or("Policy revision exhausted")?;
            }
            let run=CleanerRun {id:format!("{}-{}-{}",std::process::id(),now,super::SEQUENCE.fetch_add(1,Ordering::Relaxed)),trigger:if scheduled {"scheduled"} else {"manual"}.into(),started_at:now,finished_at:0,status:"running".into(),repositories:0,partial:false,issues:vec![],items:vec![],unlisted:0,unexamined:0};
            saved.cancel_run=None;saved.history.insert(0,run.clone());saved.history.truncate(20);
            Ok((config.clone(),run))
        })?;
        self.cancel.store(false, Ordering::SeqCst);
        let service = Arc::clone(self);
        let run_id = run.id.clone();
        crate::supervisor::spawn_once("gitpulse-cleaner-run", "cleaner run", move || {
            let _run_lock = run_lock;
            let _reset = Reset(&service.active);
            let mut run = run;
            let done = Arc::new(AtomicBool::new(false));
            let _monitor_stop = MonitorStop(Arc::clone(&done));
            let finished = Arc::clone(&done);
            let watched = Arc::clone(&service);
            let watched_config = config.clone();
            let watched_id = run.id.clone();
            let monitor = crate::supervisor::spawn_once(
                "gitpulse-cleaner-cancel",
                "cleaner cancellation monitor",
                move || {
                    while !finished.load(Ordering::SeqCst) {
                        if watched
                            .still_authorized(&watched_config, &watched_id)
                            .is_err()
                        {
                            if let Err(e) = watched.stop_current() {
                                log::error!("Could not stop revoked cleanup: {e}", e = e.message());
                            }
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                },
            );
            let outcome = match monitor {
                Ok(monitor) => {
                    let outcome = service.perform(&config, &mut run);
                    done.store(true, Ordering::SeqCst);
                    if monitor.join().is_err() {
                        Err("Cancellation monitor failed".into())
                    } else {
                        outcome
                    }
                }
                Err(e) => Err(format!("Cancellation monitor unavailable: {e}").into()),
            };
            if let Err(error) = outcome {
                run.status = if service.cancel.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "incomplete"
                }
                .into();
                push_issue(&mut run.issues, error.into_message());
            }
            run.finished_at = super::now();
            match service.retry_sooner_after_shedding(&config, &run, run.finished_at) {
                Ok(Some(at)) => push_issue(
                    &mut run.issues,
                    format!(
                        "{} check(s) were shed under load; the next scheduled run was brought forward to {} minute(s) from now",
                        run.unexamined,
                        at.saturating_sub(run.finished_at) / 60
                    ),
                ),
                Ok(None) => {}
                Err(e) => log::warn!(
                    "Could not bring the schedule forward after a shed run: {e}",
                    e = e.message()
                ),
            }
            if let Err(e) = service.record(&run) {
                log::error!("Cleaner history could not be saved: {e}", e = e.message());
            }
        })
        .map_err(|e| {
            if let Err(failure) = self.update(|s| {
                if let Some(r) = s.history.iter_mut().find(|r| r.id == run_id) {
                    r.status = "failed".into();
                    r.finished_at = now;
                    r.issues.push(e.to_string());
                }
                Ok(())
            }) {
                log::error!(
                    "Cleaner dispatch failure could not be saved: {failure}",
                    failure = failure.message()
                );
            }
            e.to_string()
        })?;
        std::mem::forget(reservation);
        self.state()
    }
    fn record(&self, run: &CleanerRun) -> Result<(), CommandError> {
        self.update(|saved| {
            let stored = saved
                .history
                .iter_mut()
                .find(|r| r.id == run.id)
                .ok_or("Run history was replaced")?;
            *stored = run.clone();
            Ok(())
        })
    }
    fn perform(&self, config: &CleanerConfig, run: &mut CleanerRun) -> Result<(), CommandError> {
        self.perform_with_gate(config, run, |repo, argv, path| {
            if let Some(argv) = argv {
                crate::harness::guard_command(
                    repo,
                    &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                )?;
            }
            crate::harness::guard_file(repo, path, "delete")?;
            Ok(())
        })
    }
    fn perform_with_gate(
        &self,
        config: &CleanerConfig,
        run: &mut CleanerRun,
        mut gate: impl FnMut(&str, Option<&[String]>, &str) -> Result<(), CommandError>,
    ) -> Result<(), CommandError> {
        self.still_authorized(config, &run.id)?;
        // Only scopes a person previewed as they now stand are used, by a
        // scheduled run and a manual one alike: the in-app scheduler runs with
        // nobody watching too.
        let scopes = config.scopes.effective(self.read()?.previewed_scopes);
        let inventory = self.inspect(config, scopes, false)?;
        run.repositories = inventory.repositories;
        run.partial = inventory.partial;
        run.issues = inventory.issues;
        if config.scopes.any() && !scopes.any() {
            push_issue(
                &mut run.issues,
                "Reclaim scopes are enabled but have not been previewed since they last changed, so this run used none of them. Inspect saved roots in GitPulse to preview them.".into(),
            );
        }
        // An incomplete workspace inventory is a reason not to claim complete
        // coverage; it is not a reason to leave every tree alone. Each
        // candidate is already gated on its own repository being read in full
        // and on that repository being clean at removal time, so the refusal
        // belongs to the repositories that were not read, not to the run.
        //
        // Refusing globally left 20 consecutive scheduled runs reclaiming 0
        // bytes: two repositories that were never trusted and a traversal
        // budget on six large ones held a 57-repository workspace hostage.
        run.unexamined = inventory.shed_repos;
        if inventory.shed_repos > 0 {
            push_issue(
                &mut run.issues,
                format!(
                    "{} repositor(ies) could not be scanned: the host was shedding work under load, so nothing in them was judged",
                    inventory.shed_repos
                ),
            );
        }
        let incomplete_repos = inventory.incomplete_repos;
        let (candidates, skipped) = eligible_candidates(inventory.candidates, &incomplete_repos);
        // The refusal below leans on this: a dropped candidate means its
        // repository was recorded incomplete, and every site that records one
        // also sets `partial`. A new site that recorded a repository without
        // setting `partial` would let an all-unread workspace report a clean,
        // complete run of zero items, so it fails loudly here under test
        // rather than quietly claiming coverage it does not have.
        debug_assert!(
            skipped == 0 || run.partial,
            "an incompletely read repository must mark the run partial"
        );
        // Nothing eligible is a refusal only when something could not be read.
        // Every repository recorded in `incomplete_repos` also set `partial`,
        // so an all-unread workspace still refuses here, while a complete scan
        // that simply found nothing to remove stays an ordinary successful run
        // rather than being reported as an incomplete one.
        if candidates.is_empty() && run.partial {
            run.status = "incomplete".into();
            return Err("Incomplete inventory: no cleanup was started".into());
        }
        if skipped > 0 {
            run.partial = true;
            push_issue(
                &mut run.issues,
                format!(
                    "{skipped} artifact(s) were left alone: their repository was not read in full"
                ),
            );
        }
        let started = Instant::now();
        let mut attempted = 0u64;
        // `max_targets` limits removals attempted, not rows written. Counting
        // every refused row against it let 14 refused candidates and 6 dirty
        // repositories fill a 20-target run that then removed nothing, run
        // after run, while the trees past the 20th were never looked at.
        let max_targets = config.max_targets as usize;
        let mut attempted_targets = 0usize;
        // A repository refused for its own state (uncommitted work, leases,
        // unreadable task state) refuses every candidate in it, so it is asked
        // once per run, not once per candidate. The removal gate still asks
        // again before anything is deleted.
        let mut refused_repos: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut waited_on_host = Duration::ZERO;
        for candidate in candidates {
            if attempted_targets >= max_targets || started.elapsed() > Duration::from_secs(300) {
                run.partial = true;
                push_issue(
                    &mut run.issues,
                    "Run limit reached; remaining candidates were not attempted".into(),
                );
                break;
            }
            self.still_authorized(config, &run.id)?;
            let mut row = CleanerItem {
                repo_path: candidate.repo_path.clone(),
                path: candidate.path.clone(),
                status: "skipped".into(),
                message: String::new(),
                bytes_before: candidate.bytes,
                bytes_after: None,
            };
            let mut journal_failed = false;
            let veto_key = veto_key(&candidate, scopes);
            let action = (|| {
                if let Some(reason) = refused_repos.get(&veto_key) {
                    return Err(CommandError::from(reason.clone()));
                }
                if let Err(error) = self.quiet_with_backoff(
                    config,
                    &run.id,
                    &candidate,
                    scopes,
                    &mut waited_on_host,
                ) {
                    // A shed check is not an answer about the repository, so
                    // the next candidate in it asks again.
                    if !shed_under_load(&error) {
                        refused_repos.insert(veto_key.clone(), error.message().to_string());
                    }
                    return Err(error);
                }
                if candidate.scope == SCOPE_PRUNE_WORKTREE {
                    // `git worktree remove`, never forced: git itself refuses a
                    // worktree with uncommitted or untracked work.
                    let argv =
                        crate::engine::worktree::remove_worktree_argv(&candidate.path, false);
                    attempted_targets += 1;
                    row.status = "running".into();
                    run.items.push(row.clone());
                    if let Err(error) = self.record(run) {
                        journal_failed = true;
                        return Err(error);
                    }
                    let _mutation_lock = self.lock("mutation.lock")?;
                    self.still_authorized(config, &run.id)?;
                    self.quiet_tree(&candidate, scopes)?;
                    if discovery::roots(&config.exclusions, true)?
                        .iter()
                        .any(|p| Path::new(&candidate.path).starts_with(p))
                    {
                        return Err("Target is excluded".into());
                    }
                    gate(&candidate.repo_path, Some(argv.as_slice()), &candidate.path)?;
                    self.still_authorized(config, &run.id)?;
                    crate::engine::worktree::remove_worktree(
                        &candidate.repo_path,
                        &candidate.path,
                        false,
                    )?;
                    row.status = "completed".into();
                    row.message = "Removed the stale linked worktree with git worktree remove. Its branch is kept.".into();
                    row.bytes_after = Some(0);
                    return Ok(());
                }
                let retention = if candidate.scope == SCOPE_WORKTREE_CACHE {
                    scopes.worktree_idle_days
                } else {
                    config.retention_days
                };
                let plan = super::prepare_with_activity(
                    &candidate.repo_path,
                    &format!("local:{}", candidate.path),
                    retention,
                    |path| self.activity(path),
                )?;
                // A worktree's cloned cache is logically whole and privately a
                // fraction of that: it is budgeted by what removing it frees.
                let charge = if candidate.scope == SCOPE_WORKTREE_CACHE {
                    candidate.private_bytes.unwrap_or(plan.bytes)
                } else {
                    plan.bytes
                };
                if charge > config.max_run_bytes.saturating_sub(attempted) {
                    super::cancel(&candidate.repo_path, &plan.id)?;
                    return Err("Candidate exceeds the remaining byte budget".into());
                }
                attempted = attempted.saturating_add(charge);
                attempted_targets += 1;
                row.status = "running".into();
                row.bytes_before = plan.bytes;
                run.items.push(row.clone());
                if let Err(error) = self.record(run) {
                    journal_failed = true;
                    super::cancel(&candidate.repo_path, &plan.id)?;
                    return Err(error);
                }
                *self.current.lock().map_err(|_| "Active plan unavailable")? =
                    Some((candidate.repo_path.clone(), plan.id.clone()));
                let _mutation_lock = self.lock("mutation.lock")?;
                let result = super::execute_with_activity(
                    &candidate.repo_path,
                    &plan.id,
                    |argv, path| {
                        self.still_authorized(config, &run.id)?;
                        self.quiet_tree(&candidate, scopes)?;
                        let exclusions = discovery::roots(&config.exclusions, true)?;
                        if exclusions.iter().any(|p| Path::new(path).starts_with(p)) {
                            return Err("Target is excluded".into());
                        }
                        gate(&candidate.repo_path, argv, path)?;
                        self.still_authorized(config, &run.id)?;
                        Ok(())
                    },
                    |path| self.activity(path),
                );
                *self.current.lock().map_err(|_| "Active plan unavailable")? = None;
                let (_, out) = result?;
                row.status = if out.success { "completed" } else { "failed" }.into();
                row.message = out.message;
                row.bytes_before = out.bytes_before;
                row.bytes_after = out.bytes_after;
                Ok::<(), String>(()).map_err(CommandError::from)
            })();
            if let Err(e) = action {
                if shed_under_load(&e) {
                    // Never judged: the governor refused the check before it
                    // ran. A "skipped" row would read as a policy answer.
                    row.status = "unexamined".into();
                    row.message = format!(
                        "Could not examine: {} Nothing was judged; GitPulse tries again sooner than the schedule.",
                        e.message()
                    );
                    run.unexamined = run.unexamined.saturating_add(1);
                } else {
                    row.status = "skipped".into();
                    row.message = e.into_message();
                }
            }
            if let Some(last) = run
                .items
                .last_mut()
                .filter(|r| r.repo_path == row.repo_path && r.path == row.path)
            {
                *last = row;
            } else if run.items.len() + (max_targets - attempted_targets) < MAX_RUN_ITEMS {
                // A row that was never attempted is listed only while room is
                // left for every removal the run may still attempt: the saved
                // history refuses a run of more than `MAX_RUN_ITEMS` rows, and
                // an unreadable history stops every later run.
                run.items.push(row);
            } else {
                run.unlisted = run.unlisted.saturating_add(1);
            }
            // A failed journal write stops the next mutation. It never silently
            // proceeds with an operation whose history cannot be persisted.
            self.record(run)?;
            if journal_failed {
                return Err("Cleanup stopped because its journal could not be saved".into());
            }
        }
        if run.unlisted > 0 {
            push_issue(
                &mut run.issues,
                format!(
                    "{} more refused or unexamined candidate(s) are not listed: a run keeps at most {MAX_RUN_ITEMS} rows",
                    run.unlisted
                ),
            );
        }
        run.status = if self.cancel.load(Ordering::SeqCst) {
            "cancelled"
        } else if run.partial || run.items.iter().any(|r| r.status != "completed") {
            "completed_with_skips"
        } else {
            "completed"
        }
        .into();
        Ok(())
    }
}

/// Splits an inventory's candidates into those whose repository was read in
/// full and those that must be left alone, returning the eligible ones and how
/// many were dropped.
///
/// Pure, so the policy that decides what may be deleted is testable without a
/// scheduler, an app-support directory or a git repository.
fn eligible_candidates(
    candidates: Vec<CleanerCandidate>,
    incomplete_repos: &BTreeSet<String>,
) -> (Vec<CleanerCandidate>, usize) {
    let before = candidates.len();
    let kept: Vec<CleanerCandidate> = candidates
        .into_iter()
        .filter(|c| !incomplete_repos.contains(&c.repo_path))
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

fn quiet_repo(repo: &str) -> Result<(), CommandError> {
    let root = crate::engine::git_cli::validate_repo(repo)?;
    if !status_in(&root, None)?.is_empty() {
        return Err("Repository has uncommitted work; global cleanup preserves it".into());
    }
    leases_clear(repo, None)
}

/// `git status` output for `root`, limited to `under` (repo-relative) when
/// given.
fn status_in(root: &Path, under: Option<&str>) -> Result<Vec<u8>, CommandError> {
    let spec = under.map(|dir| format!(":(literal){dir}"));
    let mut args = vec![
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
    ];
    if let Some(spec) = &spec {
        args.extend(["--", spec.as_str()]);
    }
    crate::engine::git_cli::git_with_stdin(root, &args, &[])
}

/// No active task lease applies. With `branch`, a lease applies when it names
/// that branch or names none (then it could be anywhere); without, every lease
/// applies.
fn leases_clear(repo: &str, branch: Option<Option<&str>>) -> Result<(), CommandError> {
    let tasks = crate::tasks::view(repo);
    if !tasks.error.is_empty() {
        return Err(format!("Task activity could not be checked: {}", tasks.error).into());
    }
    let applies = |lease: &crate::tasks::TaskLease| match branch {
        None => true,
        Some(branch) => lease.branch.is_none() || lease.branch.as_deref() == branch,
    };
    if tasks.leases.iter().any(applies) {
        return Err("Repository has active task leases".into());
    }
    Ok(())
}

/// The per-tree veto: uncommitted work in the directory that produced the
/// artifact (its parent) refuses it; work elsewhere in the repository does
/// not. An artifact at the repository root falls back to the whole
/// repository. Leases stay repository-wide.
fn quiet_project(repo: &str, artifact: &str) -> Result<(), CommandError> {
    let root = crate::engine::git_cli::validate_repo(repo)?;
    let project = Path::new(artifact)
        .parent()
        .and_then(Path::to_str)
        .filter(|p| !p.is_empty());
    if !status_in(&root, project)?.is_empty() {
        return Err(match project {
            Some(project) => {
                format!("{project} has uncommitted work; global cleanup preserves its output")
                    .into()
            }
            None => "Repository has uncommitted work; global cleanup preserves it".into(),
        });
    }
    leases_clear(repo, None)
}

/// The worktree-scope veto: no process works anywhere in the worktree, its
/// tree is clean, and no task lease names its branch. A process table that
/// could not be read refuses.
fn quiet_worktree(worktree: &str, branch: Option<&str>) -> Result<(), CommandError> {
    let root = crate::engine::git_cli::validate_repo(worktree)?;
    match crate::sysload::procscan::working_in(&root) {
        Ok(pids) if pids.is_empty() => {}
        Ok(pids) => {
            return Err(format!("{} process(es) are working in this worktree", pids.len()).into())
        }
        Err(e) => {
            return Err(format!(
            "Whether a process works in this worktree could not be read, so it was left alone: {e}"
        )
            .into())
        }
    }
    if !status_in(&root, None)?.is_empty() {
        return Err("Worktree has uncommitted work; global cleanup preserves it".into());
    }
    leases_clear(worktree, Some(branch))
}

/// What one state refusal covers, so a refusal is asked once per run per
/// tree it judges: the repository, the producing project, or the worktree.
fn veto_key(candidate: &CleanerCandidate, scopes: ReclaimScopes) -> String {
    match candidate.scope.as_str() {
        SCOPE_PRUNE_WORKTREE => candidate.path.clone(),
        SCOPE_REPOSITORY if scopes.per_tree_vetoes => format!(
            "{}\0{}",
            candidate.repo_path,
            Path::new(&candidate.path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        _ => candidate.repo_path.clone(),
    }
}

pub(super) fn mutation_lock() -> Result<CleanerLock, CommandError> {
    service()?.lock("mutation.lock")
}

/// The build-cache clone budget worktree creation enforces, read without a
/// lock from the saved policy. A policy that cannot be read yields the
/// default budget, never "no limit".
pub fn clone_budget() -> CloneBudget {
    match service().and_then(|cleaner| cleaner.read()) {
        Ok(saved) => saved.config.clone_budget,
        Err(error) => {
            log::warn!(
                "Clone budget unreadable, using the default: {}",
                error.message()
            );
            CloneBudget::default()
        }
    }
}

pub fn current_time() -> u64 {
    super::now()
}

pub fn service() -> Result<&'static Arc<Cleaner>, CommandError> {
    let dir = crate::tool_config::default_config_dir()
        .ok_or("Application config directory unavailable")?
        .join("hygiene");
    if !dir.is_absolute() {
        return Err("Application config path is not absolute".into());
    }
    Ok(SERVICE.get_or_init(|| Arc::new(Cleaner::new(dir))))
}

pub fn start_scheduler() -> Result<(), CommandError> {
    let cleaner = Arc::clone(service()?);
    if SCHEDULER_STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    crate::supervisor::spawn_loop(
        "gitpulse-cleaner-clock",
        Duration::from_secs(30),
        move || match cleaner.state() {
            Ok(state)
                if state.config.enabled
                    && !state.running
                    && super::now() >= state.config.next_run_at =>
            {
                if let Err(e) = cleaner.start(true, state.config.revision, super::now()) {
                    log::warn!("Scheduled cleaner could not start: {e}", e = e.message());
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("Cleaner schedule is unavailable: {e}", e = e.message()),
        },
    )
    .map_err(|e| {
        SCHEDULER_STARTED.store(false, Ordering::SeqCst);
        e.to_string()
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;

/// The same executable can be woken by launchd without constructing a webview.
/// Missed intervals are claimed once by the shared durable scheduler.
pub fn run_due_headless() -> Result<(), CommandError> {
    let cleaner = service()?;
    let state = cleaner.state()?;
    if !state.config.enabled
        || !state.config.run_when_closed
        || state.running
        || super::now() < state.config.next_run_at
    {
        return Ok(());
    }
    cleaner.start(true, state.config.revision, super::now())?;
    let deadline = Instant::now() + Duration::from_secs(900);
    while cleaner.active.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            cleaner.cancel()?;
            return Err("Background cleanup exceeded its time limit".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let state = cleaner.state()?;
    if let Some(run) = state
        .history
        .first()
        .filter(|r| matches!(r.status.as_str(), "failed" | "incomplete" | "interrupted"))
    {
        return Err(format!(
            "Background cleanup {}: {}",
            run.status,
            run.issues.join("; ")
        )
        .into());
    }
    Ok(())
}
