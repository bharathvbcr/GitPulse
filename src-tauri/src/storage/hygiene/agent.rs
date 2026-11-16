//! Build folders agents leave in places the repository scan never looks: the
//! per-session scratchpads Claude Code keeps under the system temp directory,
//! scratch build directories inside a repository, and temp directories nobody
//! can account for.
//!
//! Nothing here deletes by itself. [`scan_sessions`] and [`scan_repository`]
//! build the cleaner's preview (candidates with the evidence for each, and
//! every item kept with its reason), and [`remove`] is the cleaner run's
//! delete step: it re-judges the candidate from scratch (it trusts nothing the
//! preview said) and then removes it through the same reviewed-entry engine
//! the repository scope uses (`tree::snapshot`, `require_age`,
//! `remove_reviewed`).
//!
//! A directory is a candidate only when all of these hold, and a check that
//! could not run keeps it:
//! - it carries producer evidence: `.rustc_info.json` (a file Cargo writes) and
//!   a `CACHEDIR.TAG` with the cache-directory signature. That signature is
//!   the generic one every tool that follows the cache-directory convention
//!   writes, not Cargo's own, so `.rustc_info.json` and the layout check below
//!   carry the weight. A directory merely named `target` is kept, whatever
//!   else is true of it;
//! - its top level holds only what Cargo, rustdoc and the coverage tools write
//!   (see `foreign_top_level`), so a folder someone keeps their own files in
//!   is kept;
//! - it is reached without crossing a symlink and lies inside the scope root;
//! - its relative path and every entry inside it pass DevCouncil's name
//!   protections (`protected_artifact`, and `protected_entry` through the
//!   strict snapshot), so a refusal there is a refusal here;
//! - no live process uses it (working directory, executable, argument or
//!   environment), and no process of the user's was unreadable;
//! - for a session scratchpad, no live agent session has this session id, and
//!   every agent detector could look;
//! - nothing inside was modified within the retention period.
//!
//! Leaked temp directories are report-only. An origin would be provable only
//! if the directory carried a creator marker naming a process that is gone;
//! nothing writes one, so there is no deletion path for them at all.
use super::global::CleanerCandidate;
use super::tree;
use crate::sysload::procscan::DirUse;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) const SCOPE_SESSION: &str = "agent-session-scratch";
pub(super) const SCOPE_REPOSITORY: &str = "agent-repo-scratch";
pub(super) const SCOPE_CLAUDE: &str = "agent-claude-target";

/// The signature in `CACHEDIR.TAG`. It is the cache-directory convention's
/// generic signature, shared by every tool that follows it, not Cargo's own.
const CARGO_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";
/// Session folders examined per scan; past it the rest are counted.
const MAX_SESSION_FOLDERS: usize = 4096;
/// Directory levels below a scratchpad or scratch root searched for targets.
const SEARCH_DEPTH: usize = 3;
/// What a running Claude session without a usable id is reported as. The scan
/// states it once as its own issue line, so it never hides in the count of
/// folders left alone; the UI shows that line as its own sentence.
pub(crate) const SESSION_UNMATCHED: &str = "cannot be matched to a session folder";
const MAX_TARGETS_PER_ROOT: usize = 16;
/// What the strict walk of one Cargo target may spend. A real target has tens
/// of thousands of entries (measured read-only on this machine: a 48 GiB
/// build folder held 137,894 and walked in 4 s, a 25 GiB one 65,674 in 1.3 s),
/// so the default 250,000 / 15 s was close to enough and a target past it was
/// always kept. 600,000 / 45 s is about four times the largest measured. The
/// snapshot costs about 250 bytes an entry, so the worst case is ~150 MB held
/// for the walk and released after the judgement.
pub(crate) const CARGO_TARGET_BUDGET: tree::Budget = tree::Budget {
    entries: 600_000,
    seconds: 45,
};
/// The most kept items one preview carries.
const MAX_KEPT: usize = 200;
/// Leaked temp directories reported, largest first.
const MAX_LEAKED: usize = 20;
/// Entries of the temp directory looked at for leaked folders.
const MAX_TEMP_ENTRIES: usize = 512;
/// Smallest temp directory worth reporting.
const LEAK_FLOOR_BYTES: u64 = 256 * 1024 * 1024;
const NO_PROOF: &str = "no owner can be proven: no creator marker names a process, and the name pattern is shared by every tool that makes temp directories";

/// Something the preview left alone, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeptItem {
    pub path: String,
    pub scope: String,
    pub reason: String,
}

/// A temp directory GitPulse reports and never removes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeakedTemp {
    pub path: String,
    /// `None` when the walk did not finish.
    pub bytes: Option<u64>,
    pub age_days: u64,
    pub reason: String,
}

/// What a scan found.
#[derive(Debug, Default)]
pub(super) struct Scan {
    pub candidates: Vec<CleanerCandidate>,
    pub kept: Vec<KeptItem>,
    pub issues: Vec<String>,
    /// Kept items past the list's cap: counted, not listed.
    pub unlisted: usize,
    /// Every folder the scope covers was examined.
    pub complete: bool,
    /// Folders a check could not run for (git refused, timed out or was shed,
    /// a directory could not be listed). They are kept, but never as an answer:
    /// the run counts them as unexamined, which brings the next run forward.
    pub unexamined: u32,
}

/// Prefix of the kept reason for a folder whose check did not run. The UI keys
/// on it to say "could not be checked" and not "skipped" or "in use".
pub(crate) const COULD_NOT_CHECK: &str = "could not be checked:";

/// Why a judgement did not approve a folder. The two are different facts: a
/// folder that was examined and failed a rule, and one a check could not be
/// run for. Only the second asks for another look soon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Examined, and a rule keeps it.
    Kept(String),
    /// A check could not run, so the folder is kept without having been judged.
    Unexamined(String),
}

impl Refusal {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Kept(why) => why.clone(),
            Self::Unexamined(why) => format!("{COULD_NOT_CHECK} {why}"),
        }
    }
}

impl From<String> for Refusal {
    fn from(why: String) -> Self {
        Self::Kept(why)
    }
}

impl From<&str> for Refusal {
    fn from(why: &str) -> Self {
        Self::Kept(why.into())
    }
}

impl Scan {
    fn keep(&mut self, path: &Path, scope: &str, reason: impl Into<String>) {
        if self.kept.len() < MAX_KEPT {
            self.kept.push(KeptItem {
                path: path.to_string_lossy().into_owned(),
                scope: scope.into(),
                reason: reason.into(),
            });
        } else {
            self.unlisted += 1;
        }
    }

    /// Keeps a folder with the reason its judgement gave.
    fn refuse(&mut self, path: &Path, scope: &str, refusal: &Refusal) {
        if matches!(refusal, Refusal::Unexamined(_)) {
            self.unexamined = self.unexamined.saturating_add(1);
            self.complete = false;
        }
        self.keep(path, scope, refusal.message());
    }

    /// Whether this scan's own deadline passed. Says so once, and marks the
    /// scan incomplete, so a cut-off scan is never read as a clean one.
    fn out_of_time(&mut self, ctx: &Context<'_>, what: &str) -> bool {
        if ctx.deadline > Instant::now() {
            return false;
        }
        if self.complete {
            self.issues.push(format!(
                "{what}: the agent scratch scan reached its time limit, so the rest were not examined"
            ));
        }
        self.complete = false;
        true
    }

    /// Folds this scan into the inventory, within its candidate and kept-list
    /// caps. Whatever did not fit, or was not examined, marks the inventory
    /// partial and not a preview of this scope.
    pub(super) fn absorb_into(self, inventory: &mut super::global::CleanerInventory) {
        const CANDIDATE_CAP: usize = 512;
        for candidate in self.candidates {
            if inventory.candidates.len() >= CANDIDATE_CAP {
                inventory.partial = true;
                inventory.scopes_complete = false;
                inventory.issues.push(format!(
                    "More than {CANDIDATE_CAP} candidates were found (the candidate cap), so the rest of the agent scratch builds were not listed and this preview does not cover the agent scratch scope. Remove some by hand, then preview again."
                ));
                break;
            }
            inventory.candidates.push(candidate);
        }
        let room = MAX_KEPT.saturating_sub(inventory.kept.len());
        let overflow = self.kept.len().saturating_sub(room);
        inventory.kept.extend(self.kept.into_iter().take(room));
        let unlisted = self.unlisted + overflow;
        if unlisted > 0 {
            inventory.issues.push(format!(
                "{unlisted} more agent scratch folder(s) were left alone and are not listed: the list shows the first {MAX_KEPT} (the kept-list cap)"
            ));
        }
        if self.unexamined > 0 {
            inventory.shed_repos = inventory.shed_repos.saturating_add(self.unexamined);
            inventory.issues.push(format!(
                "{} agent scratch folder(s) could not be checked and were kept; the run counts them as unexamined and tries again sooner",
                self.unexamined
            ));
        }
        inventory.issues.extend(self.issues);
        if !self.complete {
            inventory.partial = true;
            inventory.scopes_complete = false;
        }
    }
}

/// The user running this program.
#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
pub(crate) fn current_uid() -> u32 {
    u32::MAX
}

/// The live Claude sessions as far as they could be told apart. A scratchpad
/// folder belongs to a Claude session, so only the Claude detector answers
/// "is this one running": a Codex, Antigravity, DevCouncil or Manvi session
/// cannot hold one, and neither it nor its detector failing keeps a folder.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveSessions {
    /// Registry session ids of Claude sessions known to be running.
    pub ids: BTreeSet<String>,
    /// Running Claude sessions (or registry entries) with no usable session
    /// id: they cannot be matched to a folder, so none can be shown finished.
    pub unidentified: usize,
    /// Why the Claude observation is not complete, when its detector could not
    /// look.
    pub incomplete: Option<String>,
}

impl LiveSessions {
    pub(crate) fn observe() -> Self {
        let observation = crate::insights::live_sessions::observe();
        // The registry names a session id for most sessions; one it does not name
        // may still show its id on its own command line. A table that could not
        // be read recovers nothing, and those sessions stay unidentified.
        let table = crate::sysload::procscan::read_use_table().unwrap_or_default();
        Self::from_observation_with(&observation, &|pid| {
            table
                .iter()
                .find(|p| u32::try_from(p.pid).is_ok_and(|p| p == pid))
                .and_then(|p| p.argv.clone())
        })
    }

    #[cfg(test)]
    pub(crate) fn from_observation(
        observation: &crate::insights::live_sessions::Observation,
    ) -> Self {
        Self::from_observation_with(observation, &|_| None)
    }

    /// `argv_of` answers a live session's command line from the process table.
    pub(crate) fn from_observation_with(
        observation: &crate::insights::live_sessions::Observation,
        argv_of: &dyn Fn(u32) -> Option<Vec<String>>,
    ) -> Self {
        let mut out = Self::default();
        for kind in observation.iter().filter(|k| k.kind == "claude") {
            if !kind.ok {
                out.incomplete = Some(format!(
                    "the {} detector could not look: {}",
                    kind.kind, kind.error
                ));
            }
            if kind.truncated {
                out.incomplete = Some(format!("the {} registry was read only in part", kind.kind));
            }
            out.unidentified += kind.unverified.len();
            for session in &kind.live {
                match session.session_id.as_deref().filter(|id| !id.is_empty()) {
                    Some(id) => {
                        out.ids.insert(id.to_string());
                    }
                    None => match argv_of(session.pid) {
                        // Helper modes of the executable are not sessions: the
                        // session that started one is counted on its own.
                        Some(argv) if crate::sysload::procscan::is_claude_helper(&argv) => {}
                        Some(argv) => match crate::sysload::procscan::claude_session_id_in(&argv) {
                            Some(id) => {
                                out.ids.insert(id);
                            }
                            None => out.unidentified += 1,
                        },
                        None => out.unidentified += 1,
                    },
                }
            }
        }
        // No Claude entry at all is a detector that did not run, not a machine
        // with no Claude sessions.
        if !observation.iter().any(|k| k.kind == "claude") {
            out.incomplete = Some("the claude detector reported nothing".into());
        }
        out
    }
}

/// What a judgement reads from the machine. Injectable, so no test reads the
/// real process table or the real clock.
pub(crate) struct Context<'a> {
    pub now: u64,
    pub retention_days: u32,
    pub live: &'a LiveSessions,
    pub use_of: &'a dyn Fn(&Path) -> Result<DirUse, String>,
    pub cancel: &'a AtomicBool,
    /// The scan's own deadline, separate from every other part of a run.
    pub deadline: Instant,
    /// What the strict walk of one Cargo target may spend; see
    /// [`CARGO_TARGET_BUDGET`].
    pub target_budget: tree::Budget,
}

/// What a judgement approves: the reviewed tree to remove and why it was
/// believed idle.
pub(super) struct Verdict {
    pub snapshot: tree::Snapshot,
    pub evidence: Vec<String>,
    /// The folder whose processes were read: the target itself, or the session
    /// folder around it. The delete path reads it again just before removal.
    pub guarded: PathBuf,
}

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Producer evidence that this directory is a Cargo build directory:
/// `.rustc_info.json` (a file Cargo writes) beside a `CACHEDIR.TAG` with the
/// generic cache-directory signature.
pub(crate) fn cargo_evidence(dir: &Path) -> Result<(), String> {
    let tag = dir.join("CACHEDIR.TAG");
    let regular = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    if !regular(&tag) {
        return Err("no CACHEDIR.TAG, so Cargo did not make it".into());
    }
    let mut text = String::new();
    let signed = std::fs::File::open(&tag)
        .and_then(|file| file.take(1024).read_to_string(&mut text))
        .is_ok()
        && text.starts_with(CARGO_SIGNATURE);
    if !signed {
        return Err("its CACHEDIR.TAG does not carry the cache-directory signature".into());
    }
    if !regular(&dir.join(".rustc_info.json")) {
        return Err("no .rustc_info.json, so it is not a Cargo target directory".into());
    }
    Ok(())
}

/// The names a Cargo build folder is given: `target`, `<name>-target` (the
/// `--target-dir .claude/<name>-target` convention) and `target-<name>`.
fn named_like_a_build_folder(name: &str) -> bool {
    name == "target" || name.ends_with("-target") || name.starts_with("target-")
}

/// Cargo targets below `root`, never through a symlink, and the directories
/// named like build folders that carry no producer evidence.
fn find_targets(root: &Path, scan: &mut Scan, scope: &str, ctx: &Context<'_>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut wrappers: Vec<PathBuf> = Vec::new();
    let mut capped = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if scan.out_of_time(ctx, &root.display().to_string()) {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                scan.refuse(
                    &dir,
                    scope,
                    &Refusal::Unexamined(format!("it could not be listed: {e}")),
                );
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            match cargo_evidence(&path) {
                Ok(()) if scope == SCOPE_REPOSITORY && !named_like_a_build_folder(&name) => {
                    // Inside a repository nothing says who made a folder, so only
                    // one named like a build folder is offered.
                    scan.keep(
                        &path,
                        scope,
                        "it carries Cargo's markers but is not named like a build folder (target, <name>-target, target-<name>)",
                    );
                }
                Ok(()) => {
                    if found.len() >= MAX_TARGETS_PER_ROOT {
                        if !capped {
                            capped = true;
                            scan.issues.push(format!(
                                "{root}: more than {MAX_TARGETS_PER_ROOT} Cargo build folders under one scratch root (the per-root cap), so the rest were not examined. Remove old ones there by hand or move them out of {root}, then preview again.",
                                root = root.display()
                            ));
                        }
                        scan.complete = false;
                    } else {
                        found.push(path);
                    }
                }
                Err(why) if named_like_a_build_folder(&name) => {
                    scan.keep(&path, scope, format!("named like a build folder but {why}"));
                    // A session's scratchpad often holds a plain folder named
                    // like a build folder that wraps the real targets
                    // (`target-x/<profile>-target`). Its children are looked at
                    // one level down, each judged as a target in its own right;
                    // the outer folder is never offered or removed.
                    if scope == SCOPE_SESSION {
                        wrappers.push(path);
                    }
                }
                Err(_) if depth + 1 < SEARCH_DEPTH && name != ".git" && name != "node_modules" => {
                    stack.push((path, depth + 1));
                }
                Err(_) => {}
            }
        }
    }
    for wrapper in wrappers {
        let Ok(children) = std::fs::read_dir(&wrapper) else {
            continue;
        };
        for child in children.flatten() {
            let path = child.path();
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
                || cargo_evidence(&path).is_err()
            {
                continue;
            }
            if found.len() >= MAX_TARGETS_PER_ROOT {
                scan.complete = false;
                break;
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Everything a removal must still hold, judged now from the filesystem and
/// the process table. `scope_root` is the directory the scope covers; `target`
/// must lie inside it, with no symlink on the way.
fn judge(
    scope_root: &Path,
    target: &Path,
    relative_to: &Path,
    session: Option<(&str, &Path)>,
    ctx: &Context<'_>,
) -> Result<Verdict, Refusal> {
    let root = crate::engine::process::canonicalize_plain(scope_root)
        .map_err(|e| Refusal::Unexamined(format!("its scope root could not be resolved: {e}")))?;
    if !target.is_absolute()
        || target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("its path is not a plain absolute path".into());
    }
    tree::no_symlinks(target)?;
    let real = crate::engine::process::canonicalize_plain(target)
        .map_err(|e| Refusal::Unexamined(format!("it could not be resolved: {e}")))?;
    if real != target || !real.starts_with(&root) || real == root {
        return Err("it is not inside the folder this scope covers".into());
    }
    let relative = target
        .strip_prefix(relative_to)
        .map_err(|_| "it is not inside the folder its session owns".to_string())?;
    if devmap_query::hygiene::protected_artifact(&relative.to_string_lossy()) {
        return Err(format!(
            "its path {} is on the preserved-name list (dependencies, environments, shared caches or persistent local state)",
            relative.display()
        )
        .into());
    }
    cargo_evidence(target)?;
    if let Some(foreign) = foreign_top_level(target)? {
        return Err(format!(
            "it holds {foreign} at its top level, which Cargo, rustdoc and the coverage tools do not write"
        )
        .into());
    }
    let mut evidence = vec![
        "Cargo target: .rustc_info.json (Cargo's own file) beside the generic CACHEDIR.TAG, and a top level of only what Cargo, rustdoc and coverage tooling write".to_string(),
    ];
    let guarded = match session {
        Some((id, session_dir)) => {
            // A detector that could not look is not an answer.
            if let Some(why) = &ctx.live.incomplete {
                return Err(Refusal::Unexamined(format!(
                    "whether the session is finished could not be told: {why}"
                )));
            }
            if ctx.live.ids.contains(id) {
                return Err(format!("in use: agent session {id} is running").into());
            }
            // The check ran and has an answer: no session folder can be shown
            // finished while a running session cannot be matched to one. The
            // scan says so once, in its own line.
            if ctx.live.unidentified > 0 {
                return Err(format!(
                    "owner unproven: {} running Claude session(s) {SESSION_UNMATCHED}, so session {id} cannot be shown finished",
                    ctx.live.unidentified
                )
                .into());
            }
            evidence.push(format!("no running Claude session has the id {id}"));
            session_dir
        }
        None => target,
    };
    let other_users = unused_now(ctx, guarded)?;
    evidence.push(format!(
        "no process uses {} (working directory, executable and arguments read; environment too where the OS shows it, which macOS does not for Apple platform programs{})",
        guarded.display(),
        if other_users > 0 {
            format!("; {other_users} other-user process(es) not inspected")
        } else {
            String::new()
        }
    ));
    let snapshot = tree::snapshot_within(target, true, ctx.cancel, ctx.target_budget).map_err(
        |e| match e {
            tree::WalkError::Refused(why) => Refusal::Kept(why),
            tree::WalkError::Budget => Refusal::Kept(format!(
                "too large to verify within {} entries / {} s, so it is kept",
                ctx.target_budget.entries, ctx.target_budget.seconds
            )),
            tree::WalkError::CouldNotRead(why) => Refusal::Unexamined(why),
            tree::WalkError::Cancelled => Refusal::Unexamined("the walk was cancelled".into()),
        },
    )?;
    if let Some(inside) = tree::crossing_device(&snapshot) {
        return Err(format!(
            "it holds a different filesystem (a mount point) at {}",
            inside.display()
        )
        .into());
    }
    tree::require_age(&snapshot, ctx.retention_days, at(ctx.now))
        .map_err(|e| format!("too recent: {e}"))?;
    let newest_days = ctx
        .now
        .saturating_sub(u64::try_from(snapshot.newest / 1_000_000_000).unwrap_or(0))
        / 86_400;
    evidence.push(format!(
        "newest file is {newest_days} day(s) old; retention is {} day(s)",
        ctx.retention_days
    ));
    Ok(Verdict {
        snapshot,
        evidence,
        guarded: guarded.to_path_buf(),
    })
}

/// Reads the process table for `guarded` and refuses unless nothing may be
/// using it. Run at judgement and again immediately before removal.
fn unused_now(ctx: &Context<'_>, guarded: &Path) -> Result<usize, Refusal> {
    let reading = (ctx.use_of)(guarded)
        .map_err(|e| Refusal::Unexamined(format!("the process table could not be read: {e}")))?;
    if reading.provably_unused() {
        return Ok(reading.other_users);
    }
    let why = reading
        .why_kept()
        .unwrap_or_else(|| "a process may be using it".into());
    Err(if reading.users.is_empty() {
        // Processes it could not read: nothing was found and nothing could be
        // ruled out. That is a check that could not finish.
        Refusal::Unexamined(why)
    } else {
        Refusal::Kept(format!("in use: {why}"))
    })
}

/// What Cargo, rustdoc and the coverage tools write directly in a target
/// directory, as far as this machine and Cargo's build-cache page show.
///
/// Files, by where they were seen: `CACHEDIR.TAG` and `.rustc_info.json` in
/// every real target here; `*.profraw` in `scratchpad/target/llvm-cov-target`
/// (cargo-llvm-cov). Accepted on the coordinator's list and not found on this
/// machine or in the build-cache page: `.future-incompat-report.json`,
/// `.rustdoc_fingerprint.json`, `*.profdata`, `.DS_Store`.
/// Directories: profile directories (the build-cache page: "debug", "release",
/// "User-defined profiles are stored in a directory with the same name as the
/// profile", which is why a name cannot be listed: a profile directory is the
/// one that holds `.cargo-lock`, seen in `debug/` and `release/` here); a target
/// triple directory (the page: `target/<tuple>/debug/`; seen as
/// `aarch64-apple-darwin/` holding `release/`), recognised by holding a profile
/// directory; `doc` and `package` (named on the page); `tmp` (in most real
/// targets here); and a marked Cargo target of its own (`llvm-cov-target`).
/// Anything else, a symlink included, is returned by name so the person can act
/// on it. `Err` when the listing could not be read.
fn foreign_top_level(target: &Path) -> Result<Option<String>, Refusal> {
    const FILES: [&str; 4] = [
        "CACHEDIR.TAG",
        ".rustc_info.json",
        ".future-incompat-report.json",
        ".rustdoc_fingerprint.json",
    ];
    // A profile directory holds `.cargo-lock` (seen in `debug/` and `release/`
    // here), `.fingerprint` and `deps` (the documented layout).
    // A directory that cannot be looked into is not a foreign one: the check
    // could not run, which is a different answer from "not Cargo's".
    let is_profile = |dir: &Path| -> Result<bool, Refusal> {
        for name in [".cargo-lock", ".fingerprint", "deps"] {
            match std::fs::symlink_metadata(dir.join(name)) {
                Ok(_) => return Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(Refusal::Unexamined(format!(
                        "{} could not be read: {e}",
                        dir.display()
                    )));
                }
            }
        }
        Ok(false)
    };
    let entries = std::fs::read_dir(target)
        .map_err(|e| Refusal::Unexamined(format!("its top level could not be listed: {e}")))?;
    let mut names: Vec<(String, PathBuf, std::fs::Metadata)> = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| Refusal::Unexamined(format!("its top level could not be listed: {e}")))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| Refusal::Unexamined(format!("its top level could not be read: {e}")))?;
        names.push((entry.file_name().to_string_lossy().into_owned(), path, meta));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path, meta) in names {
        let accepted = if meta.is_file() {
            FILES.contains(&name.as_str())
                || name.ends_with(".profraw")
                || name.ends_with(".profdata")
                || name == ".DS_Store"
        } else if meta.is_dir() {
            if matches!(name.as_str(), "doc" | "package" | "tmp")
                || is_profile(&path)?
                || cargo_evidence(&path).is_ok()
            {
                true
            } else {
                // A target-triple directory holds profile directories.
                let inner = std::fs::read_dir(&path).map_err(|e| {
                    Refusal::Unexamined(format!("{} could not be listed: {e}", path.display()))
                })?;
                let mut triple = false;
                for e in inner {
                    let e = e.map_err(|e| {
                        Refusal::Unexamined(format!("{} could not be listed: {e}", path.display()))
                    })?;
                    if std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir())
                        && is_profile(&e.path())?
                    {
                        triple = true;
                        break;
                    }
                }
                triple
            }
        } else {
            false
        };
        if !accepted {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

fn candidate_from(
    scope: &str,
    scope_root: &Path,
    target: &Path,
    verdict: Verdict,
    mut evidence: Vec<String>,
    measure: bool,
    deadline: Instant,
) -> CleanerCandidate {
    let (bytes, private_bytes) = if measure {
        let usage = crate::storage::measure_linked_worktree(
            &target.to_string_lossy(),
            "target",
            None,
            deadline,
        );
        (usage.bytes, usage.private_bytes)
    } else {
        (verdict.snapshot.bytes, None)
    };
    evidence.extend(verdict.evidence);
    CleanerCandidate {
        repo_path: scope_root.to_string_lossy().into_owned(),
        path: target.to_string_lossy().into_owned(),
        provider: "Rust / Cargo scratch build".into(),
        bytes,
        scope: scope.into(),
        private_bytes,
        branch: None,
        evidence,
    }
}

/// A session id is a folder name this scope will reason about: a plain name,
/// never a path.
fn plain_session_id(name: &str) -> bool {
    (8..=96).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Scratchpad build folders of sessions that are provably finished, under
/// `<tmp_root>/claude-<uid>/<project>/<session>/scratchpad`.
pub(super) fn scan_sessions(tmp_root: &Path, uid: u32, ctx: &Context<'_>, measure: bool) -> Scan {
    let mut scan = Scan {
        complete: true,
        ..Scan::default()
    };
    let scope_root = tmp_root.join(format!("claude-{uid}"));
    let projects = match std::fs::read_dir(&scope_root) {
        Ok(projects) => projects,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(e) => {
            scan.complete = false;
            scan.issues
                .push(format!("{} could not be listed: {e}", scope_root.display()));
            return scan;
        }
    };
    let mut folders = 0usize;
    let mut blocked = 0usize;
    for project in projects.flatten() {
        let project_dir = project.path();
        if !std::fs::symlink_metadata(&project_dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&project_dir) else {
            scan.refuse(
                &project_dir,
                SCOPE_SESSION,
                &Refusal::Unexamined("it could not be listed".into()),
            );
            continue;
        };
        for session in sessions.flatten() {
            let session_dir = session.path();
            let id = session.file_name().to_string_lossy().into_owned();
            if !std::fs::symlink_metadata(&session_dir).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            folders += 1;
            if folders > MAX_SESSION_FOLDERS {
                scan.complete = false;
                continue;
            }
            if scan.out_of_time(ctx, "session scratchpads") {
                continue;
            }
            let scratchpad = session_dir.join("scratchpad");
            if !std::fs::symlink_metadata(&scratchpad).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            if !plain_session_id(&id) {
                scan.keep(&session_dir, SCOPE_SESSION, "its name is not a session id");
                continue;
            }
            let project_name = project.file_name().to_string_lossy().into_owned();
            for target in find_targets(&scratchpad, &mut scan, SCOPE_SESSION, ctx) {
                if scan.out_of_time(ctx, "session scratchpads") {
                    break;
                }
                match judge(
                    &scope_root,
                    &target,
                    &scratchpad,
                    Some((id.as_str(), session_dir.as_path())),
                    ctx,
                ) {
                    Ok(verdict) => {
                        let evidence = vec![format!(
                            "made by agent session {id} (project {project_name}); its scratchpad is the only thing here that is touched"
                        )];
                        scan.candidates.push(candidate_from(
                            SCOPE_SESSION,
                            &scope_root,
                            &target,
                            verdict,
                            evidence,
                            measure,
                            ctx.deadline,
                        ));
                    }
                    Err(refusal) => {
                        if matches!(&refusal, Refusal::Kept(why) if why.contains(SESSION_UNMATCHED))
                        {
                            blocked += 1;
                        }
                        scan.refuse(&target, SCOPE_SESSION, &refusal);
                    }
                }
            }
        }
    }
    if blocked > 0 {
        scan.issues.push(format!(
            "No session scratchpad build can be offered: {} running Claude session(s) {SESSION_UNMATCHED}, so none of the {blocked} folder(s) found can be shown finished. They are offered again once that count is zero.",
            ctx.live.unidentified
        ));
    }
    if folders > MAX_SESSION_FOLDERS {
        scan.issues.push(format!(
            "{} session folder(s) past the first {MAX_SESSION_FOLDERS} under {} were not examined (the session-folder cap). Clear finished session folders there, then preview again.",
            folders - MAX_SESSION_FOLDERS,
            scope_root.display()
        ));
    }
    scan
}

/// Scratch build directories inside a repository: children of
/// `.gitpulse/scratch` and `tmp`. Ignored by git, with nothing tracked inside.
pub(super) fn scan_repository(repo: &Path, ctx: &Context<'_>, measure: bool) -> Scan {
    let mut scan = Scan {
        complete: true,
        ..Scan::default()
    };
    for root in [repo.join(".gitpulse/scratch"), repo.join("tmp")] {
        if !std::fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        for target in find_targets(&root, &mut scan, SCOPE_REPOSITORY, ctx) {
            if scan.out_of_time(ctx, "repository scratch builds") {
                break;
            }
            match judge_repository(repo, &target, ctx) {
                Ok(verdict) => scan.candidates.push(candidate_from(
                    SCOPE_REPOSITORY,
                    repo,
                    &target,
                    verdict,
                    vec!["scratch build directory inside the repository; no session records who made it".into()],
                    measure,
                    ctx.deadline,
                )),
                Err(refusal) => scan.refuse(&target, SCOPE_REPOSITORY, &refusal),
            }
        }
    }
    scan
}

/// Cargo target directories directly under a checkout's `.claude/`: the
/// folders sessions put there with `--target-dir .claude/<name>-target`, and
/// the copies a new session worktree is seeded with. Recognised by the
/// directory itself (a name that is `target` or ends in `-target`, both Cargo
/// markers, `.claude` as its parent), never through `local_provider`: the
/// preserved-name list refuses `.claude`, rightly, for its config and skills.
/// Nothing else under `.claude` is read past its listing or ever removed.
pub(super) fn scan_claude_targets(checkouts: &[PathBuf], ctx: &Context<'_>, measure: bool) -> Scan {
    let mut scan = Scan {
        complete: true,
        ..Scan::default()
    };
    let mut seen = BTreeSet::new();
    for checkout in checkouts {
        if !seen.insert(checkout.clone()) {
            continue;
        }
        let claude = checkout.join(".claude");
        if !std::fs::symlink_metadata(&claude).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&claude) else {
            scan.refuse(
                &claude,
                SCOPE_CLAUDE,
                &Refusal::Unexamined("it could not be listed".into()),
            );
            continue;
        };
        if scan.out_of_time(ctx, ".claude build folders") {
            break;
        }
        let mut targets: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (name == "target" || name.ends_with("-target"))
                    && std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir())
            })
            .map(|e| e.path())
            .collect();
        targets.sort();
        for target in targets {
            if let Err(why) = cargo_evidence(&target) {
                scan.keep(
                    &target,
                    SCOPE_CLAUDE,
                    format!("named like a build folder but {why}"),
                );
                continue;
            }
            if scan.out_of_time(ctx, ".claude build folders") {
                break;
            }
            match judge_claude(checkout, &target, ctx) {
                Ok(verdict) => {
                    let mut evidence = vec![
                        "under .claude/: made by a session that built with --target-dir .claude/<name>-target, or copied from one".into(),
                    ];
                    if let Some(seeded) = seeded_copy_note(checkout, &verdict) {
                        evidence.push(seeded);
                    }
                    scan.candidates.push(candidate_from(
                        SCOPE_CLAUDE,
                        checkout,
                        &target,
                        verdict,
                        evidence,
                        measure,
                        ctx.deadline,
                    ));
                }
                Err(refusal) => scan.refuse(&target, SCOPE_CLAUDE, &refusal),
            }
        }
    }
    scan
}

/// For a linked worktree: whether nothing in the folder is newer than the
/// worktree itself, which makes it a byte-for-byte seeded copy that was never
/// built in. A linked worktree's `.git` is a file written when it was created.
fn seeded_copy_note(checkout: &Path, verdict: &Verdict) -> Option<String> {
    let meta = std::fs::symlink_metadata(checkout.join(".git")).ok()?;
    if !meta.is_file() {
        return None;
    }
    let created = meta
        .created()
        .or_else(|_| meta.modified())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let newest = u64::try_from(verdict.snapshot.newest / 1_000_000_000).ok()?;
    // Copying keeps modification times, so a copy's files predate the copy.
    (newest <= created.saturating_add(60)).then(|| {
        "byte-for-byte seeded copy that was never built in: nothing in it is newer than this worktree's creation"
            .to_string()
    })
}

/// A `.claude` build folder is judged like a scratch target in a repository,
/// except that git need not ignore it (it is not in the project's tree) and
/// the path must be exactly `<checkout>/.claude/<name>-target`.
fn judge_claude(checkout: &Path, target: &Path, ctx: &Context<'_>) -> Result<Verdict, Refusal> {
    let claude = checkout.join(".claude");
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("it has no name")?;
    if target.parent() != Some(claude.as_path()) || !(name == "target" || name.ends_with("-target"))
    {
        return Err("it is not a build folder directly under .claude".into());
    }
    let rel = format!(".claude/{name}");
    match git_tracks(checkout, &rel) {
        GitAnswer::No => {}
        GitAnswer::Yes => return Err("it holds files git tracks".into()),
        GitAnswer::Unknown(why) => return Err(Refusal::Unexamined(why)),
    }
    judge(&claude, target, &claude, None, ctx)
}

/// What a git question came back with. A question git could not answer is its
/// own answer: collapsing it into `No` is how a refused spawn, an untrusted
/// repository or a timeout would read as "git does not ignore it" or "nothing
/// is tracked".
#[derive(Debug, PartialEq, Eq)]
enum GitAnswer {
    Yes,
    No,
    Unknown(String),
}

/// Runs one read-only git question and hands back its exit status, or why no
/// status was obtained (the host shed the spawn, the repository is not trusted,
/// it timed out, git could not start).
fn git_status(dir: &Path, args: &[&str]) -> Result<(i32, String), String> {
    // Every git question the cleaner asks about a candidate, at preview and at
    // delete time alike, goes through here, so the relief is granted here and
    // covers exactly this one read-only spawn.
    match crate::engine::process::with_disk_relief(|| {
        crate::engine::git_cli::git_captured(dir, args)
    }) {
        Ok(run) => {
            if let Some(why) = &run.incomplete {
                return Err(format!("git {} output was cut short: {why:?}", args[0]));
            }
            Ok((
                run.status_code,
                String::from_utf8_lossy(&run.stdout).into_owned(),
            ))
        }
        Err(e) => Err(format!("git {} did not run: {}", args[0], e.message())),
    }
}

/// Whether git ignores `rel` (a path relative to `dir`).
fn git_ignores(dir: &Path, rel: &str) -> GitAnswer {
    match git_status(dir, &["check-ignore", "-q", "--", rel]) {
        Ok((0, _)) => GitAnswer::Yes,
        Ok((1, _)) => GitAnswer::No,
        Ok((code, _)) => GitAnswer::Unknown(format!("git check-ignore exited with status {code}")),
        Err(why) => GitAnswer::Unknown(why),
    }
}

/// Whether git tracks anything at or under `rel`.
fn git_tracks(dir: &Path, rel: &str) -> GitAnswer {
    match git_status(dir, &["ls-files", "--", rel]) {
        Ok((0, listing)) if listing.trim().is_empty() => GitAnswer::No,
        Ok((0, _)) => GitAnswer::Yes,
        Ok((code, _)) => GitAnswer::Unknown(format!("git ls-files exited with status {code}")),
        Err(why) => GitAnswer::Unknown(why),
    }
}

/// The scratch root a repository scratch build lives under: `.gitpulse/scratch`
/// or `tmp` of the checkout. Candidates are judged relative to it, so the
/// root's own name is not a reason to refuse what is inside.
fn repository_scratch_root(checkout: &Path, target: &Path) -> Result<PathBuf, String> {
    [checkout.join(".gitpulse/scratch"), checkout.join("tmp")]
        .into_iter()
        .find(|root| {
            target
                .strip_prefix(root)
                .is_ok_and(|rest| rest != Path::new(""))
        })
        .ok_or_else(|| "it is not inside this checkout's .gitpulse/scratch or tmp".to_string())
}

fn judge_repository(checkout: &Path, target: &Path, ctx: &Context<'_>) -> Result<Verdict, Refusal> {
    let scratch = repository_scratch_root(checkout, target)?;
    let rel = target
        .strip_prefix(checkout)
        .map_err(|_| "it is not inside the repository".to_string())?
        .to_string_lossy()
        .into_owned();
    // Producer-owned and ignored: not tracked, and git ignores it, so removing
    // it cannot lose work and it never showed in status.
    match git_ignores(checkout, &rel) {
        GitAnswer::Yes => {}
        GitAnswer::No => {
            return Err("git does not ignore it, so it could be work in progress".into());
        }
        GitAnswer::Unknown(why) => return Err(Refusal::Unexamined(why)),
    }
    match git_tracks(checkout, &rel) {
        GitAnswer::No => {}
        GitAnswer::Yes => return Err("it holds files git tracks".into()),
        GitAnswer::Unknown(why) => return Err(Refusal::Unexamined(why)),
    }
    judge(&scratch, target, &scratch, None, ctx)
}

/// The cleaner run's delete step for a candidate this module offered. The
/// candidate is judged again from scratch; `budget` is what the run's byte
/// cap still allows. Returns the bytes the reviewed tree held.
#[cfg(test)]
pub(crate) fn remove(
    candidate: &CleanerCandidate,
    ctx: &Context<'_>,
    budget: u64,
) -> Result<u64, String> {
    remove_judged(candidate, ctx, budget).map_err(|refusal| refusal.message())
}

/// [`remove`] keeping the difference between a candidate a rule keeps and one
/// a check could not be run for, which the cleaner records as unexamined.
pub(super) fn remove_judged(
    candidate: &CleanerCandidate,
    ctx: &Context<'_>,
    budget: u64,
) -> Result<u64, Refusal> {
    let scope_root = Path::new(&candidate.repo_path);
    let target = Path::new(&candidate.path);
    let verdict = match candidate.scope.as_str() {
        SCOPE_SESSION => {
            let (id, session_dir, scratchpad) = session_of(scope_root, target)?;
            judge(
                scope_root,
                target,
                &scratchpad,
                Some((id.as_str(), session_dir.as_path())),
                ctx,
            )
        }
        SCOPE_REPOSITORY => judge_repository(scope_root, target, ctx),
        SCOPE_CLAUDE => judge_claude(scope_root, target, ctx),
        other => return Err(format!("{other} is not an agent scratch scope").into()),
    }?;
    let bytes = verdict.snapshot.bytes;
    if bytes > budget {
        return Err("Candidate exceeds the remaining byte budget".into());
    }
    // The judgement above read the process table, then walked the tree. A build
    // may have started in between, so it is read once more right before the
    // first deletion. A build that starts after this is not prevented; removal
    // stops at the first entry that changed since the review.
    let _ = unused_now(ctx, &verdict.guarded)?;
    tree::remove_reviewed(target, &verdict.snapshot, ctx.cancel)?;
    Ok(bytes)
}

/// The session id, session folder and scratchpad a scratch build path
/// belongs to: exactly `<root>/<project>/<session>/scratchpad/...`.
fn session_of(scope_root: &Path, target: &Path) -> Result<(String, PathBuf, PathBuf), String> {
    let relative = target
        .strip_prefix(scope_root)
        .map_err(|_| "it is not inside the scope root".to_string())?;
    let parts: Vec<&std::ffi::OsStr> = relative
        .components()
        .map(std::path::Component::as_os_str)
        .collect();
    if parts.len() < 4 || parts[2] != "scratchpad" {
        return Err("it is not inside <project>/<session>/scratchpad".into());
    }
    let id = parts[1].to_string_lossy().into_owned();
    if !plain_session_id(&id) {
        return Err("its folder name is not a session id".into());
    }
    let session_dir = scope_root.join(parts[0]).join(parts[1]);
    let scratchpad = session_dir.join("scratchpad");
    Ok((id, session_dir, scratchpad))
}

/// Temp directories of the user's temp area nobody can account for, reported
/// for a person to look at. Never a candidate.
///
/// Anything that stops the listing from being whole (the directory cannot be
/// read, more entries than the cap, the scan's own deadline) is an issue in
/// `issues`: an empty list is never the answer to a listing that failed.
pub(super) fn leaked_temp(
    user_temp: &Path,
    ctx: &Context<'_>,
    issues: &mut Vec<String>,
) -> Vec<LeakedTemp> {
    let entries = match std::fs::read_dir(user_temp) {
        Ok(entries) => entries,
        Err(e) => {
            issues.push(format!(
                "{} could not be listed, so leaked temp directories were not looked for: {e}",
                user_temp.display()
            ));
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    let mut seen = 0usize;
    for entry in entries.flatten() {
        seen += 1;
        if seen > MAX_TEMP_ENTRIES {
            issues.push(format!(
                "{} holds more than {MAX_TEMP_ENTRIES} entries; the rest were not looked at for leaked temp directories",
                user_temp.display()
            ));
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !name.starts_with(".tmp") || !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        if ctx.deadline <= Instant::now() {
            issues.push(format!(
                "{}: the leaked temp look reached its time limit, so the rest were not examined",
                user_temp.display()
            ));
            break;
        }
        let measured = tree::snapshot(&path, false, ctx.cancel).ok();
        let bytes = measured.as_ref().map(|s| s.bytes);
        if bytes.is_some_and(|b| b < LEAK_FLOOR_BYTES) {
            continue;
        }
        let age_days = measured.as_ref().map_or(0, |s| {
            ctx.now
                .saturating_sub(u64::try_from(s.newest / 1_000_000_000).unwrap_or(0))
                / 86_400
        });
        let use_note = match (ctx.use_of)(&path) {
            Ok(r) if r.provably_unused() => "no live process uses it".to_string(),
            Ok(r) => r.why_kept().unwrap_or_default(),
            Err(e) => format!("the process table could not be read: {e}"),
        };
        found.push(LeakedTemp {
            path: path.to_string_lossy().into_owned(),
            bytes,
            age_days,
            reason: format!("report only: {NO_PROOF}; {use_note}"),
        });
    }
    found.sort_by_key(|leak| std::cmp::Reverse(leak.bytes));
    found.truncate(MAX_LEAKED);
    found
}

#[cfg(test)]
mod tests;
