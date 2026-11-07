//! Recommendations and the apply plan, on top of the detector's report.
//!
//! Everything here is pure: a report and the git version in, offers and plans
//! out. Nothing is read or spawned, so every rule is a unit test. The one place
//! a plan is run is `engine::tuning_apply`, behind the command gate.
//!
//! # Owner decision, 2026-11-06
//!
//! GitPulse may apply recommended git tuning, but only through a user-clicked,
//! previewed, gated action. This is the single exception to the 2026-10-12
//! "GitPulse never writes repo config" rule (ft-7450105d), and it is as narrow
//! as the code can make it: three changes, each one argv of a fixed shape
//! ([`change_argv`]), accepted back only through [`is_apply_argv`]. The only
//! config keys written are [`CONFIG_WRITE_KEYS`]; `tests/git_config_posture.rs`
//! pins that, and that this file is the only one that builds such an argv.
//!
//! # Thresholds
//!
//! Each `*_MIN_*` constant is the smallest repository the measurements in
//! docs/PERFORMANCE.md ("2026-11-06: repository tuning thresholds") show the
//! change paying for itself. Below it nothing is offered, and
//! [`plan_apply`] refuses a change that is not currently offered.

use super::{config_true, IndexReadState, RepoTuningReport};
use crate::engine::git_reader::ConfigScope;
use serde::{Deserialize, Serialize};

/// `core.fsmonitor` slowed `git status` 1.29-1.73x below ~10k tracked files and
/// was 20-45 percent faster at 40k (spike ft-7450105d). Offered from here.
pub const FSMONITOR_MIN_TRACKED_FILES: u64 = 40_000;
/// `feature.manyFiles` against a plain repository that has had the same one
/// index-refreshing `git status` (`scripts/bench-git-tuning.mjs files`, with an
/// identical-arm control): GitPulse-style `status` reads took 0.63 to 0.69x as
/// long at 20,000, 40,000 and 100,000 files in six of eight runs, not less at
/// 40,000 in one, and 0.89x in one under heavy load. 20,000 is the smallest size
/// measured.
/// The gain is the untracked cache's; see docs/PERFORMANCE.md.
pub const MANYFILES_MIN_TRACKED_FILES: u64 = 20_000;
/// Loose objects past which git's own auto-gc should already have acted
/// (`gc.auto`'s default); a history walk over loose objects measured 2 to 9x
/// slower than the same history packed already at 5,000.
pub const MAINTENANCE_MIN_LOOSE_OBJECTS: u64 = 6_700;

/// A boolean `core.fsmonitor`, and the built-in daemon it turns on, arrived in
/// git 2.36.0. Git's own `Documentation/config/core.txt` at v2.35.1 and v2.35.2
/// describes only a hook command; at v2.36.0 it adds "if `core.fsmonitor` is set
/// to true" for the built-in daemon and says older git reads `true` as a hook
/// path. (2.35.2 is a security release of the 2.35 line, not a feature release.)
pub const FSMONITOR_BOOLEAN_MIN_GIT: (u32, u32, u32) = (2, 36, 0);
/// `index.skipHash` indexes are reported corrupt by `git fsck` below this.
pub const SKIPHASH_FSCK_MIN_GIT: (u32, u32, u32) = (2, 40, 0);
/// Git older than this refuses an index with the v4 / skipHash format at all.
pub const INDEX_V4_MIN_GIT: (u32, u32, u32) = (2, 13, 0);

/// The apply is killed, and the rest not run, after this many seconds.
pub const TUNING_APPLY_TIMEOUT_SECS: u64 = 120;

/// The only config keys GitPulse may write. `core.splitIndex` is deliberately
/// absent: with `feature.manyFiles` it breaks git's own add-then-commit (git
/// 2.54 / Apple Git-157; `watcher/mod.rs`, "never share a repository").
pub const CONFIG_WRITE_KEYS: [&str; 2] = ["feature.manyFiles", "core.fsmonitor"];
/// The git this app drives, as the advisor needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitEnv {
    /// `None` when git could not be probed: nothing version-dependent is offered.
    pub version: Option<(u32, u32, u32)>,
    /// Whether `core.fsmonitor`'s built-in daemon was measured on this
    /// platform (macOS; Windows runs the same daemon but was not measured).
    pub fsmonitor_measured: bool,
}

impl GitEnv {
    /// This machine's git (the cached preflight probe) and platform.
    pub fn current() -> Self {
        let preflight = crate::engine::git_cli::git_preflight();
        Self {
            version: preflight
                .version
                .as_deref()
                .and_then(crate::engine::git_cli::parse_git_version),
            fsmonitor_measured: cfg!(target_os = "macos"),
        }
    }

    fn at_least(self, min: (u32, u32, u32)) -> bool {
        self.version.is_some_and(|v| v >= min)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningChange {
    /// `git config --local feature.manyFiles true`
    ManyFiles,
    /// `git config --local core.fsmonitor true`
    Fsmonitor,
    /// `git maintenance start`
    Maintenance,
}

impl TuningChange {
    pub const ALL: [Self; 3] = [Self::ManyFiles, Self::Fsmonitor, Self::Maintenance];

    fn title(self) -> &'static str {
        match self {
            Self::ManyFiles => "Turn on feature.manyFiles",
            Self::Fsmonitor => "Turn on the file system monitor",
            Self::Maintenance => "Start background maintenance",
        }
    }
}

/// One change worth offering, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningOffer {
    pub change: TuningChange,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningAdvice {
    pub report: RepoTuningReport,
    pub offers: Vec<TuningOffer>,
    /// Things the person should know even though nothing is offered for them,
    /// such as a `core.splitIndex` / `feature.manyFiles` conflict.
    pub warnings: Vec<String>,
    /// An apply is running in this repository's common directory right now.
    #[serde(default)]
    pub apply_in_flight: bool,
}

fn is_set(report: &RepoTuningReport, lower: &str) -> bool {
    report.key(lower).is_some_and(|k| k.value.is_some())
}

/// Whether `feature.manyFiles` is enabled (any scope).
fn many_files_on(report: &RepoTuningReport) -> bool {
    report.is_true("feature.manyfiles")
}

/// The reason `feature.manyFiles` and `core.splitIndex` must not meet, when
/// writing `set` (lowercase key names that would become true) would bring them
/// together with each other or with what the repository already has. Refused
/// in both orders: manyFiles requested over a split index, and a split index
/// requested over manyFiles. `None` when there is no conflict.
pub fn split_index_conflict(report: &RepoTuningReport, set: &[&str]) -> Option<String> {
    let sets = |key: &str| set.contains(&key);
    let many = sets("feature.manyfiles");
    let split = sets("core.splitindex");
    let reason = |first: &str, second: &str| {
        format!(
            "{first} together with {second} breaks git's own add-then-commit (a staged file reads \
             as untracked; verified on git 2.54 / Apple Git-157), so GitPulse never sets both"
        )
    };
    if many && (split || report.is_true("core.splitindex")) {
        return Some(reason("feature.manyFiles", "core.splitIndex"));
    }
    if split && (many || many_files_on(report)) {
        return Some(reason("core.splitIndex", "feature.manyFiles"));
    }
    if many {
        // The index file itself, and every sibling worktree's, say more than
        // config does: a split index carried by the file crashes `git status`
        // under manyFiles on git 2.54 (wt-status.c BUG), whatever the config says.
        if report.index.split_index == Some(true) {
            return Some(
                "this worktree's index is a split index (a link extension or a sharedindex file), \
                 and feature.manyFiles over a split index breaks git status on git 2.54, so \
                 GitPulse will not turn it on"
                    .to_string(),
            );
        }
        if let Some(blocker) = report.family.many_files_blocker() {
            return Some(format!("feature.manyFiles is refused: {blocker}"));
        }
    }
    None
}

fn offer(change: TuningChange, reason: String) -> TuningOffer {
    TuningOffer {
        change,
        title: change.title().to_string(),
        reason,
    }
}

fn many_files_offer(report: &RepoTuningReport, env: GitEnv) -> Option<TuningOffer> {
    let entries = report.index.entries?;
    if report.index.state != IndexReadState::Present
        || !report.index.entries_exact
        || entries < MANYFILES_MIN_TRACKED_FILES
        || is_set(report, "feature.manyfiles")
        || report.is_true("core.splitindex")
        || report.index.split_index != Some(false)
        || report.family.many_files_blocker().is_some()
        || !env.at_least(INDEX_V4_MIN_GIT)
    {
        return None;
    }
    // An index that already is v4 with a zero trailer has what the key adds.
    if report.index.version == Some(4) && report.index.skip_hash == Some(true) {
        return None;
    }
    Some(offer(
        TuningChange::ManyFiles,
        format!(
            "{entries} tracked files (offered from {MANYFILES_MIN_TRACKED_FILES}): git status \
             measured about a third faster once git has refreshed the index with the untracked \
             cache on, which is the first ordinary git status after this. The index format \
             change was not measured to help."
        ),
    ))
}

fn fsmonitor_offer(report: &RepoTuningReport, env: GitEnv) -> Option<TuningOffer> {
    let entries = report.index.entries?;
    if !env.fsmonitor_measured
        || report.index.state != IndexReadState::Present
        || !report.index.entries_exact
        || entries < FSMONITOR_MIN_TRACKED_FILES
        // Any value is the person's choice: false, a hook path, or true.
        || is_set(report, "core.fsmonitor")
        || !env.at_least(FSMONITOR_BOOLEAN_MIN_GIT)
    {
        return None;
    }
    Some(offer(
        TuningChange::Fsmonitor,
        format!(
            "{entries} tracked files (offered from {FSMONITOR_MIN_TRACKED_FILES}): git status \
             measured 20 to 45 percent faster at this size, and slower below about 10,000."
        ),
    ))
}

fn maintenance_offer(report: &RepoTuningReport, env: GitEnv) -> Option<TuningOffer> {
    if report.maintenance.registered != Some(false) || env.version.is_none() {
        return None;
    }
    // A person who turned automatic gc off, or maintenance off, runs their own.
    let gc_off = report.objects.gc_auto == Some(0);
    let maintenance_off = report
        .maintenance
        .auto
        .as_deref()
        .is_some_and(|v| !config_true(v));
    if gc_off || maintenance_off {
        return None;
    }
    // Only loose objects are offered for: the loose-objects task measurably
    // restores a history walk (docs/PERFORMANCE.md). A pile of packs is
    // reported by the detector but not offered for, because one incremental
    // repack run did not consolidate them and a multi-pack-index alone did not
    // speed the walk up.
    let loose = report
        .objects
        .loose_objects
        .filter(|l| *l >= MAINTENANCE_MIN_LOOSE_OBJECTS)
        .filter(|_| report.objects.loose_over_gc_auto == Some(true))?;
    let reason = format!(
        "about {loose} loose objects, past gc.auto (offered from \
         {MAINTENANCE_MIN_LOOSE_OBJECTS}). A history walk over loose objects measured 3 to 13x \
         slower than over a pack at 15,000, and the loose-objects task restored it. It also \
         schedules an hourly background fetch of every remote and takes the object database \
         lock while tasks run."
    );
    Some(offer(TuningChange::Maintenance, reason))
}

/// What is worth offering for this repository right now. Below each
/// threshold, and wherever an input is unknown, nothing is offered.
pub fn recommend(report: &RepoTuningReport, env: GitEnv) -> Vec<TuningOffer> {
    if report.config_note.is_some() {
        return Vec::new();
    }
    [
        many_files_offer(report, env),
        fsmonitor_offer(report, env),
        maintenance_offer(report, env),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Warnings that stand on their own, shown with the offers.
pub fn warnings(report: &RepoTuningReport) -> Vec<String> {
    let mut out = Vec::new();
    if report.is_true("core.splitindex") {
        out.push(
            "core.splitIndex is on. With feature.manyFiles it breaks git's own add-then-commit, \
             so GitPulse will not turn manyFiles on here."
                .to_string(),
        );
    }
    if report.index.split_index == Some(true) && !report.is_true("core.splitindex") {
        out.push(
            "This worktree's index is a split index, so GitPulse will not turn \
             feature.manyFiles on here."
                .to_string(),
        );
    }
    if let Some(blocker) = report.family.many_files_blocker() {
        out.push(format!(
            "GitPulse will not turn feature.manyFiles on: {blocker}."
        ));
    }
    if many_files_on(report) {
        out.push(
            "feature.manyFiles is on; GitPulse will not turn core.splitIndex on beside it."
                .to_string(),
        );
    }
    out
}

/// Detector, offers and warnings together: what the Storage panel shows.
pub fn advise(report: RepoTuningReport, env: GitEnv) -> TuningAdvice {
    TuningAdvice {
        offers: recommend(&report, env),
        warnings: warnings(&report),
        report,
        apply_in_flight: false,
    }
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningScope {
    /// The repository's own `config` (shared by its linked worktrees).
    RepoConfig,
    /// The user's global git config.
    GlobalConfig,
    /// The per-user OS scheduler (launchd, cron or a systemd timer).
    OsScheduler,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningWrite {
    pub scope: TuningScope,
    /// What lands there, in words (`maintenance.repo = <this repository>`).
    pub what: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningStep {
    pub change: TuningChange,
    /// The exact line, program included. Empty when `skipped`.
    pub argv: Vec<String>,
    pub writes: Vec<TuningWrite>,
    /// Each command that reverses it, program included.
    pub undo: Vec<Vec<String>>,
    /// What the undo does not cover, and what to run alongside it.
    pub undo_note: Option<String>,
    /// Facts the person must see before confirming.
    pub notes: Vec<String>,
    /// Why nothing will run for this change (it is already in place).
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningPlan {
    pub common_dir: String,
    pub steps: Vec<TuningStep>,
    /// The run is stopped, and later steps not started, after this many seconds.
    pub timeout_secs: u64,
}

impl TuningPlan {
    /// The argvs that will actually run, in order.
    pub fn runnable(&self) -> Vec<Vec<String>> {
        self.steps
            .iter()
            .filter(|s| s.skipped.is_none())
            .map(|s| s.argv.clone())
            .collect()
    }
}

pub(super) fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// The one argv a change runs. The only builder of a config-writing argv in
/// the application.
pub fn change_argv(change: TuningChange) -> Vec<String> {
    match change {
        TuningChange::ManyFiles => {
            strings(&["git", "config", "--local", "feature.manyFiles", "true"])
        }
        TuningChange::Fsmonitor => strings(&["git", "config", "--local", "core.fsmonitor", "true"]),
        TuningChange::Maintenance => strings(&["git", "maintenance", "start"]),
    }
}

/// Whether `argv` is exactly one of the lines [`change_argv`] builds.
pub fn is_apply_argv(argv: &[String]) -> bool {
    TuningChange::ALL
        .iter()
        .any(|change| change_argv(*change) == argv)
}

/// A key an undo may unset. A closed set, so no undo can name another key.
#[derive(Clone, Copy)]
pub(super) enum UnsetKey {
    ManyFiles,
    Fsmonitor,
    MaintenanceAuto,
    MaintenanceStrategy,
}

pub(super) fn unset_argv(key: UnsetKey) -> Vec<String> {
    match key {
        UnsetKey::ManyFiles => {
            strings(&["git", "config", "--local", "--unset", "feature.manyFiles"])
        }
        UnsetKey::Fsmonitor => strings(&["git", "config", "--local", "--unset", "core.fsmonitor"]),
        UnsetKey::MaintenanceAuto => {
            strings(&["git", "config", "--local", "--unset", "maintenance.auto"])
        }
        UnsetKey::MaintenanceStrategy => strings(&[
            "git",
            "config",
            "--local",
            "--unset",
            "maintenance.strategy",
        ]),
    }
}

fn step_for(change: TuningChange, report: &RepoTuningReport) -> TuningStep {
    match change {
        TuningChange::ManyFiles => TuningStep {
            change,
            argv: change_argv(change),
            writes: vec![TuningWrite {
                scope: TuningScope::RepoConfig,
                what: "feature.manyFiles = true".to_string(),
            }],
            undo: vec![unset_argv(UnsetKey::ManyFiles)],
            undo_note: Some(
                "The index keeps its format until git next rewrites it; unsetting the key makes \
                 that rewrite use git's defaults again."
                    .to_string(),
            ),
            notes: vec![
                "Nothing gets faster at the click: the gain needs git to rewrite the index with \
                 the untracked cache, which happens on the first ordinary git status (or any \
                 index write) afterwards. In the measurement that untracked cache was the whole \
                 gain; after such a refresh the index stayed format 2 and was not smaller, so \
                 the format parts were not shown to help."
                    .to_string(),
                format!(
                    "feature.manyFiles sets index.skipHash=true, index.version=4 and \
                 core.untrackedCache=true. Git older than {}.{} reports a corrupt index when \
                 `git fsck` runs on a skipHash index, and git older than {}.{} refuses the index \
                 format altogether, so anyone using this repository with an older git is affected.",
                    SKIPHASH_FSCK_MIN_GIT.0,
                    SKIPHASH_FSCK_MIN_GIT.1,
                    INDEX_V4_MIN_GIT.0,
                    INDEX_V4_MIN_GIT.1
                ),
            ],
            skipped: None,
        },
        TuningChange::Fsmonitor => TuningStep {
            change,
            argv: change_argv(change),
            writes: vec![TuningWrite {
                scope: TuningScope::RepoConfig,
                what: "core.fsmonitor = true".to_string(),
            }],
            undo: vec![
                strings(&["git", "fsmonitor--daemon", "stop"]),
                unset_argv(UnsetKey::Fsmonitor),
            ],
            undo_note: Some(
                "Stop the daemon first, then unset the key so it is not started again.".to_string(),
            ),
            notes: vec![
                "The first git command in this repository after this starts a long-lived \
                 `git fsmonitor--daemon`. GitPulse does not own, supervise or stop it, and it \
                 keeps running after GitPulse quits. Stop it with `git fsmonitor--daemon stop`."
                    .to_string(),
            ],
            skipped: None,
        },
        TuningChange::Maintenance => {
            let mut undo = vec![strings(&["git", "maintenance", "unregister"])];
            if report.maintenance.auto.is_none() {
                undo.push(unset_argv(UnsetKey::MaintenanceAuto));
            }
            if report.maintenance.strategy.is_none() {
                undo.push(unset_argv(UnsetKey::MaintenanceStrategy));
            }
            TuningStep {
                change,
                argv: change_argv(change),
                writes: vec![
                    TuningWrite {
                        scope: TuningScope::GlobalConfig,
                        what: "maintenance.repo += this repository".to_string(),
                    },
                    TuningWrite {
                        scope: TuningScope::RepoConfig,
                        what: "maintenance.auto = false, maintenance.strategy = incremental"
                            .to_string(),
                    },
                    TuningWrite {
                        scope: TuningScope::OsScheduler,
                        what: "a per-user job that runs hourly (launchd, cron or a systemd \
                               timer; the daily tasks run at midnight) for every registered \
                               repository"
                            .to_string(),
                    },
                ],
                undo,
                undo_note: Some(
                    "`git maintenance unregister` removes this repository from the list, so \
                     the scheduled tasks, prefetch included, stop running for it, but it leaves \
                     the scheduler job and the repository's maintenance.auto and \
                     maintenance.strategy values. `git maintenance stop` removes the scheduler job \
                     for every registered repository, so run it only if no other repository \
                     uses background maintenance."
                        .to_string(),
                ),
                notes: vec![
                    "Maintenance with the incremental strategy turns git's automatic gc off for \
                     this repository. `git help maintenance` lists its schedule: commit-graph \
                     hourly, prefetch hourly, loose-objects daily, incremental-repack daily, and \
                     gc disabled."
                        .to_string(),
                    "Background network: the hourly prefetch task \"updates the object directory \
                     with the latest objects from all registered remotes. For each remote, a git \
                     fetch command is run\", into refs/prefetch/ and without updating tags. It \
                     runs unattended, as you, with your credential helpers. Exclude a remote with \
                     remote.<name>.skipFetchAll. `git maintenance unregister` stops it for this \
                     repository."
                        .to_string(),
                    "Object database lock: per `git help maintenance`, \"each git maintenance run \
                     command takes a lock on the repository's object database\", which keeps \
                     other concurrent maintenance runs out, and a run that finds it held skips \
                     its task. The page says `git gc` \"does not take the lock in the same way\" \
                     and should not be combined with maintenance. The page does not say whether \
                     ordinary reads wait on the lock, so GitPulse does not claim they are \
                     unaffected while a task runs."
                        .to_string(),
                ],
                skipped: None,
            }
        }
    }
}

/// Why `change` is already in place, when it is.
fn already_in_place(change: TuningChange, report: &RepoTuningReport) -> Option<String> {
    match change {
        TuningChange::ManyFiles if report.is_true("feature.manyfiles") => {
            Some("feature.manyFiles is already on".to_string())
        }
        TuningChange::Fsmonitor if report.is_true("core.fsmonitor") => {
            Some("core.fsmonitor is already on".to_string())
        }
        TuningChange::Maintenance if report.maintenance.registered == Some(true) => {
            Some("this repository is already registered for maintenance".to_string())
        }
        _ => None,
    }
}

/// The preview of applying `requested`: exact argv, scope and undo for each
/// change, or the reason it may not be applied. Refuses a change that is not
/// offered (below its threshold, or an input unknown), `feature.manyFiles`
/// against `core.splitIndex`, and a boolean `core.fsmonitor` on git 2.35.1 or
/// older. A change already in place is a step that will not run, and says so.
pub fn plan_apply(
    report: &RepoTuningReport,
    env: GitEnv,
    common_dir: &str,
    requested: &[TuningChange],
) -> Result<TuningPlan, String> {
    if requested.is_empty() {
        return Err("Nothing was selected to apply".to_string());
    }
    if let Some(why) = &report.config_note {
        return Err(format!(
            "Nothing is applied while the config is unreadable: {why}"
        ));
    }
    let mut wanted: Vec<TuningChange> = Vec::new();
    for change in requested {
        if !wanted.contains(change) {
            wanted.push(*change);
        }
    }
    // Hazards first: they are refusals no threshold can excuse.
    let mut would_set: Vec<&str> = Vec::new();
    for change in &wanted {
        if already_in_place(*change, report).is_some() {
            continue;
        }
        match change {
            TuningChange::ManyFiles => would_set.push("feature.manyfiles"),
            TuningChange::Fsmonitor => {
                if !env.at_least(FSMONITOR_BOOLEAN_MIN_GIT) {
                    return Err(format!(
                        "core.fsmonitor=true needs git {}.{}.{} or newer: older git reads the \
                         value as a hook path{}",
                        FSMONITOR_BOOLEAN_MIN_GIT.0,
                        FSMONITOR_BOOLEAN_MIN_GIT.1,
                        FSMONITOR_BOOLEAN_MIN_GIT.2,
                        if env.version.is_none() {
                            " (the installed git could not be checked)"
                        } else {
                            ""
                        }
                    ));
                }
                would_set.push("core.fsmonitor");
            }
            TuningChange::Maintenance => {}
        }
    }
    if let Some(conflict) = split_index_conflict(report, &would_set) {
        return Err(conflict);
    }
    let offers = recommend(report, env);
    let mut steps = Vec::new();
    for change in wanted {
        if let Some(why) = already_in_place(change, report) {
            let mut step = step_for(change, report);
            step.argv = Vec::new();
            step.skipped = Some(why);
            steps.push(step);
            continue;
        }
        if !offers.iter().any(|o| o.change == change) {
            return Err(format!(
                "{} is not offered for this repository: it is below its measured threshold, or \
                 something it depends on could not be read",
                change.title()
            ));
        }
        steps.push(step_for(change, report));
    }
    Ok(TuningPlan {
        common_dir: common_dir.to_string(),
        steps,
        timeout_secs: TUNING_APPLY_TIMEOUT_SECS,
    })
}

/// A scope label for the wire format of a key's origin (display only).
pub fn origin_label(scope: Option<ConfigScope>) -> &'static str {
    match scope {
        Some(ConfigScope::System) => "system config",
        Some(ConfigScope::Global) => "global config",
        Some(ConfigScope::Local) => "repository config",
        Some(ConfigScope::Worktree) => "worktree config",
        Some(ConfigScope::Command) => "command line",
        Some(ConfigScope::Unknown) | None => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningStepStatus {
    /// The command ran and git reported success.
    Applied,
    /// Already in place; nothing ran.
    Skipped,
    /// The command ran and failed, or timed out.
    Failed,
    /// An earlier step failed or the person stopped the run first.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningStepResult {
    pub change: TuningChange,
    pub argv: Vec<String>,
    pub status: TuningStepStatus,
    /// Git's own words for a failure, or why a step was skipped.
    pub detail: Option<String>,
}

/// How a confirmed apply ended. A failed or timed-out step is a result, with
/// the steps before it still reported as applied: they were written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningOutcome {
    /// The person stopped the run before it finished.
    pub cancelled: bool,
    /// This click joined an apply another click had already started here, and
    /// reports that apply's result.
    pub joined: bool,
    pub elapsed_ms: u64,
    pub results: Vec<TuningStepResult>,
    /// The detector's answer after the run.
    pub advice: TuningAdvice,
}
