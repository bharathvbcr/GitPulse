//! App-wide policy, durable scheduling, bounded runs and shared UI state.
//! No UI timer owns deletion; the native worker survives page changes.
use super::{discovery, tree};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
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
        }
    }
}
impl CleanerConfig {
    pub fn validate(&self) -> Result<(), String> {
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
        if self.enabled && (self.roots.is_empty() || self.next_run_at == 0) {
            return Err("A schedule needs selected roots and a next run time".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanerCandidate {
    pub repo_path: String,
    pub path: String,
    pub provider: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanerInventory {
    pub candidates: Vec<CleanerCandidate>,
    pub repositories: u32,
    pub visited_entries: u32,
    pub partial: bool,
    pub issues: Vec<String>,
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
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    config: CleanerConfig,
    history: Vec<CleanerRun>,
    cancel_run: Option<String>,
    #[serde(default)]
    background_error: Option<String>,
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
}

#[cfg(test)]
type ActivityCheck = fn(&Path) -> Result<(), String>;

pub struct Cleaner {
    dir: PathBuf,
    cancel: AtomicBool,
    active: AtomicBool,
    scanning: AtomicBool,
    current: Mutex<Option<(String, String)>>,
    #[cfg(test)]
    activity_override: Option<ActivityCheck>,
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
            state_after_read: None,
        }
    }
    fn activity(&self, path: &Path) -> Result<(), String> {
        #[cfg(test)]
        if let Some(check) = self.activity_override {
            return check(path);
        }
        super::activity_check(path)
    }
    fn ensure_dir(&self) -> Result<(), String> {
        // Refuse existing symlink ancestors before creating app-owned state.
        for ancestor in self.dir.ancestors() {
            if ancestor.try_exists().map_err(|e| e.to_string())? {
                tree::no_symlinks(ancestor)?;
                break;
            }
        }
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        tree::no_symlinks(&self.dir)
    }
    fn open_lock(&self, name: &str) -> Result<File, String> {
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
    fn lock(&self, name: &str) -> Result<CleanerLock, String> {
        let file = self.open_lock(name)?;
        file.try_lock()
            .map_err(|e| format!("Cleaner is busy or its lock is unavailable: {e}"))?;
        Ok(CleanerLock(file))
    }
    fn read(&self) -> Result<Saved, String> {
        let path = self.dir.join("state.json");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
            Err(e) => return Err(e.to_string()),
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
                .any(|r| r.items.len() > 100 || r.issues.len() > 128)
        {
            return Err("Cleaner history exceeds its bounds".into());
        }
        Ok(saved)
    }
    fn update<T>(&self, edit: impl FnOnce(&mut Saved) -> Result<T, String>) -> Result<T, String> {
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
    pub fn state(&self) -> Result<CleanerState, String> {
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
                    return Err(format!("Run lock is unavailable: {error}"));
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
        Ok(CleanerState {
            config: saved.config,
            history: saved.history,
            running,
            supported: cfg!(unix),
            background_supported: super::background::supported(),
            background_error: saved.background_error,
            agent_rules: devmap_query::hygiene::AGENT_RULES.into(),
        })
    }
    pub fn save(&self, config: CleanerConfig) -> Result<CleanerState, String> {
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
        configure: impl FnOnce(bool) -> Result<(), String>,
    ) -> Result<CleanerState, String> {
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
                    saved.background_error = Some(error.clone());
                    Ok(())
                })?;
                self.stop_current()?;
                return Err(format!(
                    "Schedule disabled because background setup failed: {error}"
                ));
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
    fn stop_current(&self) -> Result<(), String> {
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
    pub fn cancel(&self) -> Result<CleanerState, String> {
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
    fn still_authorized(&self, config: &CleanerConfig, id: &str) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("Cleanup cancelled".into());
        }
        let saved = self.read()?;
        if saved.config.revision != config.revision || saved.cancel_run.as_deref() == Some(id) {
            return Err("Cleanup cancelled or settings changed".into());
        }
        Ok(())
    }
    pub fn inventory(&self) -> Result<CleanerInventory, String> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err("Global inventory is already running".into());
        }
        let _reset = Reset(&self.scanning);
        if self.active.load(Ordering::SeqCst) {
            return Err("Wait for cleanup to finish before inspecting".into());
        }
        self.cancel.store(false, Ordering::SeqCst);
        let config = self.state()?.config;
        self.inspect(&config)
    }
    fn inspect(&self, config: &CleanerConfig) -> Result<CleanerInventory, String> {
        let roots = discovery::roots(&config.roots, false)?;
        let excludes = discovery::roots(&config.exclusions, true)?;
        let found = discovery::discover(&roots, &excludes, &self.cancel);
        let mut result = CleanerInventory {
            repositories: found.repos.len() as u32,
            visited_entries: found.visited,
            partial: found.partial,
            issues: found.issues,
            candidates: vec![],
        };
        let start = Instant::now();
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
                    if report.scan.truncated || report.scan.permission_denied > 0 {
                        result.partial = true;
                        if result.issues.len() < 128 {
                            result
                                .issues
                                .push(format!("Partial repository scan: {repo_path}"));
                        }
                    }
                    for artifact in report.artifacts {
                        if result.candidates.len() >= 512 {
                            result.partial = true;
                            break;
                        }
                        if artifact.tracked_files > 0
                            || artifact.unignored
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
                            });
                        }
                    }
                }
                Err(e) => {
                    result.partial = true;
                    if result.issues.len() < 128 {
                        result.issues.push(format!("{repo_path}: {e}"));
                    }
                }
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
    pub fn start(
        self: &Arc<Self>,
        scheduled: bool,
        revision: u64,
        now: u64,
    ) -> Result<CleanerState, String> {
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
            let run=CleanerRun {id:format!("{}-{}-{}",std::process::id(),now,super::SEQUENCE.fetch_add(1,Ordering::Relaxed)),trigger:if scheduled {"scheduled"} else {"manual"}.into(),started_at:now,finished_at:0,status:"running".into(),repositories:0,partial:false,issues:vec![],items:vec![]};
            saved.cancel_run=None;saved.history.insert(0,run.clone());saved.history.truncate(20);
            Ok((config.clone(),run))
        })?;
        self.cancel.store(false, Ordering::SeqCst);
        let service = Arc::clone(self);
        let run_id = run.id.clone();
        std::thread::Builder::new()
            .name("gitpulse-cleaner-run".into())
            .spawn(move || {
                let _run_lock = run_lock;
                let _reset = Reset(&service.active);
                let mut run = run;
                let done = Arc::new(AtomicBool::new(false));
                let _monitor_stop = MonitorStop(Arc::clone(&done));
                let finished = Arc::clone(&done);
                let watched = Arc::clone(&service);
                let watched_config = config.clone();
                let watched_id = run.id.clone();
                let monitor = std::thread::Builder::new()
                    .name("gitpulse-cleaner-cancel".into())
                    .spawn(move || {
                        while !finished.load(Ordering::SeqCst) {
                            if watched
                                .still_authorized(&watched_config, &watched_id)
                                .is_err()
                            {
                                if let Err(e) = watched.stop_current() {
                                    log::error!("Could not stop revoked cleanup: {e}");
                                }
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    });
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
                    Err(e) => Err(format!("Cancellation monitor unavailable: {e}")),
                };
                if let Err(error) = outcome {
                    run.status = if service.cancel.load(Ordering::SeqCst) {
                        "cancelled"
                    } else {
                        "incomplete"
                    }
                    .into();
                    if run.issues.len() < 128 {
                        run.issues.push(error);
                    }
                }
                run.finished_at = super::now();
                if let Err(e) = service.record(&run) {
                    log::error!("Cleaner history could not be saved: {e}");
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
                    log::error!("Cleaner dispatch failure could not be saved: {failure}");
                }
                e.to_string()
            })?;
        std::mem::forget(reservation);
        self.state()
    }
    fn record(&self, run: &CleanerRun) -> Result<(), String> {
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
    fn perform(&self, config: &CleanerConfig, run: &mut CleanerRun) -> Result<(), String> {
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
        mut gate: impl FnMut(&str, Option<&[String]>, &str) -> Result<(), String>,
    ) -> Result<(), String> {
        self.still_authorized(config, &run.id)?;
        let inventory = self.inspect(config)?;
        run.repositories = inventory.repositories;
        run.partial = inventory.partial;
        run.issues = inventory.issues;
        if run.partial {
            run.status = "incomplete".into();
            return Err("Incomplete inventory: no cleanup was started".into());
        }
        let started = Instant::now();
        let mut attempted = 0u64;
        for candidate in inventory.candidates {
            if run.items.len() >= config.max_targets as usize
                || started.elapsed() > Duration::from_secs(300)
            {
                run.partial = true;
                run.issues
                    .push("Run limit reached; remaining candidates were not attempted".into());
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
            let action = (|| {
                quiet_repo(&candidate.repo_path)?;
                let plan = super::prepare_with_activity(
                    &candidate.repo_path,
                    &format!("local:{}", candidate.path),
                    config.retention_days,
                    |path| self.activity(path),
                )?;
                if plan.bytes > config.max_run_bytes.saturating_sub(attempted) {
                    super::cancel(&candidate.repo_path, &plan.id)?;
                    return Err("Candidate exceeds the remaining byte budget".into());
                }
                attempted = attempted.saturating_add(plan.bytes);
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
                        quiet_repo(&candidate.repo_path)?;
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
                Ok::<(), String>(())
            })();
            if let Err(e) = action {
                row.status = "skipped".into();
                row.message = e;
            }
            if let Some(last) = run
                .items
                .last_mut()
                .filter(|r| r.repo_path == row.repo_path && r.path == row.path)
            {
                *last = row;
            } else {
                run.items.push(row);
            }
            // A failed journal write stops the next mutation. It never silently
            // proceeds with an operation whose history cannot be persisted.
            self.record(run)?;
            if journal_failed {
                return Err("Cleanup stopped because its journal could not be saved".into());
            }
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

fn quiet_repo(repo: &str) -> Result<(), String> {
    let root = crate::engine::git_cli::validate_repo(repo)?;
    let status = crate::engine::git_cli::git_with_stdin(
        &root,
        &[
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
        ],
        &[],
    )?;
    if !status.is_empty() {
        return Err("Repository has uncommitted work; global cleanup preserves it".into());
    }
    let tasks = crate::tasks::view(repo);
    if !tasks.error.is_empty() {
        return Err(format!(
            "Task activity could not be checked: {}",
            tasks.error
        ));
    }
    if !tasks.leases.is_empty() {
        return Err("Repository has active task leases".into());
    }
    Ok(())
}

pub(super) fn mutation_lock() -> Result<CleanerLock, String> {
    service()?.lock("mutation.lock")
}

pub fn current_time() -> u64 {
    super::now()
}

pub fn service() -> Result<&'static Arc<Cleaner>, String> {
    let dir = crate::tool_config::default_config_dir()
        .ok_or("Application config directory unavailable")?
        .join("hygiene");
    if !dir.is_absolute() {
        return Err("Application config path is not absolute".into());
    }
    Ok(SERVICE.get_or_init(|| Arc::new(Cleaner::new(dir))))
}

pub fn start_scheduler() -> Result<(), String> {
    let cleaner = Arc::clone(service()?);
    if SCHEDULER_STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("gitpulse-cleaner-clock".into())
        .spawn(move || loop {
            match cleaner.state() {
                Ok(state)
                    if state.config.enabled
                        && !state.running
                        && super::now() >= state.config.next_run_at =>
                {
                    if let Err(e) = cleaner.start(true, state.config.revision, super::now()) {
                        log::warn!("Scheduled cleaner could not start: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("Cleaner schedule is unavailable: {e}"),
            }
            std::thread::sleep(Duration::from_secs(30));
        })
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
pub fn run_due_headless() -> Result<(), String> {
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
        ));
    }
    Ok(())
}
