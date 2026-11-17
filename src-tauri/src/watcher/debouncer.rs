use super::filter::{EventFilter, ROOT_WATCH_MODE};
use super::scope::{self, EntryKind, Plan, WatchScope};
use super::{IgnoreRules, SCAN_COALESCE};
use crate::supervisor::lock_or_recover;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Events the native callback may have queued for the debounce loop. A burst
/// past this is not buffered further: the callback records an overflow, and
/// the loop answers it with one full refresh (see [`RepoFileWatcher::take_overflow`]).
/// The queue used to be unbounded, so a stalled consumer grew it without limit.
pub(crate) const EVENT_QUEUE_CAPACITY: usize = 4096;

/// The most synthetic entry events one re-registration announces; past it one
/// path-less event (a full refresh) is announced instead.
const MAX_ANNOUNCED_ENTRIES: usize = 32;

pub struct RepoFileWatcher {
    registration: Arc<Registration>,
    retired: Receiver<()>,
    pub receiver: Receiver<Result<Event, notify::Error>>,
    overflow: Arc<AtomicBool>,
    /// A second sender on `receiver`, kept so that something other than the
    /// native callback can wake whoever is blocked on it (see
    /// [`Self::waker`]). It also means the receiver no longer disconnects when
    /// the backend dies; [`Self::backend_alive`] is how that is noticed.
    wake: SyncSender<Result<Event, notify::Error>>,
    /// Shared with the native callback, which filters with it; the debounce
    /// loop publishes the ignore rules it classifies with (see [`super::filter`]).
    filter: Arc<EventFilter>,
}

/// What the native callback does with an event, and what a re-registration
/// does with the synthetic ones it injects: the SAME path, so the poll-skip
/// epoch moves and the loop refreshes exactly as for a native event.
pub(crate) struct Forwarder {
    tx: SyncSender<Result<Event, notify::Error>>,
    overflow: Arc<AtomicBool>,
    filter: Arc<EventFilter>,
}

impl Forwarder {
    fn deliver(&self, res: Result<Event, notify::Error>) {
        if self.filter.admit(&res) {
            forward_event(&self.tx, &self.overflow, res);
        }
    }
}

/// Where a registration watches, fixed for its life.
struct Target {
    git_dir: PathBuf,
    common: Option<PathBuf>,
    /// The worktree root, when there is one separate from the git directory.
    root: Option<PathBuf>,
    root_mode: RecursiveMode,
    /// A narrowed registration is possible at all: macOS, a recursive root.
    narrowable: bool,
}

struct State {
    watcher: Option<RecommendedWatcher>,
    plan: Plan,
    registered: Vec<(PathBuf, RecursiveMode)>,
    /// What the caller last asked for. An evidence-driven widening does not
    /// change it, so a re-watch that repeats the same request is a no-op.
    requested: WatchScope,
    /// Widened for evidence: whole for the rest of this watch's life, until the
    /// caller asks for a scope again.
    permanent: bool,
    last: Option<Instant>,
    /// A change was due while the rate bound was closed.
    pending: bool,
    rules: Arc<IgnoreRules>,
    /// The rules the current plan was computed from.
    planned_from: Option<Arc<IgnoreRules>>,
    /// The rules loaded before the first registration, for the loop's first
    /// load to reuse.
    initial_rules: Option<Arc<IgnoreRules>>,
    logged_reason: Option<String>,
    shut: bool,
    #[cfg(target_os = "macos")]
    ancestors: Option<super::ancestors::AncestorWatch>,
}

enum Mode {
    /// Plan again from the published rules.
    Recompute,
    /// Ask (a) and (b) again of the exclusions in force.
    Revalidate,
}

/// The native watcher and everything that decides what it registers.
///
/// Owned through an `Arc` by the debounce loop, the kqueue thread on a
/// narrowed registration, and the session (so a command can change the scope):
/// any of them may re-register. A re-registration builds the NEW stream and
/// commits it before the old one is dropped, so the two overlap rather than
/// leaving a gap (a fresh `FSEventStream` starts at "now" and loses whatever
/// landed between a stop and a start).
///
/// Lock order: `refresh_lock`, then `state`. The native callback never takes
/// either.
pub(crate) struct Registration {
    /// What the kqueue thread holds, so it never keeps a watch alive.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    me: Weak<Registration>,
    target: Target,
    forwarder: Arc<Forwarder>,
    /// The clone every native callback holds. Taken by [`Self::shutdown`]
    /// before the watcher is dropped, or the disconnect that says the backend
    /// has released its callbacks never arrives.
    lifetime: Mutex<Option<Sender<()>>>,
    /// Serialises plan-then-install, so two threads do not plan against each
    /// other's stale view. Never taken by [`Self::widen_for_evidence`].
    refresh_lock: Mutex<()>,
    state: Mutex<State>,
    reregistrations: AtomicU64,
}

impl Registration {
    fn whole_paths(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let target = &self.target;
        let mut paths = vec![(target.git_dir.clone(), RecursiveMode::Recursive)];
        if let Some(common) = &target.common {
            if *common != target.git_dir && common.exists() {
                paths.push((common.clone(), RecursiveMode::Recursive));
            }
        }
        if let Some(root) = &target.root {
            paths.push((root.clone(), target.root_mode));
        }
        paths
    }

    /// Builds a new native watcher over `paths` and starts it. The caller
    /// drops whatever it replaces only after this returns.
    fn build(&self, paths: &[(PathBuf, RecursiveMode)]) -> Result<RecommendedWatcher, String> {
        let life = lock_or_recover(&self.lifetime)
            .clone()
            .ok_or_else(|| "the watch has been shut down".to_string())?;
        let forwarder = Arc::clone(&self.forwarder);
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                // The sender's lifetime follows every native callback owner.
                // It is never sent to: disconnect is the shutdown receipt.
                std::hint::black_box(&life);
                #[cfg(all(test, target_os = "macos"))]
                forwarder.filter.count_callback();
                forwarder.deliver(res);
            },
            Config::default().with_poll_interval(Duration::from_millis(150)),
        )
        .map_err(|e| format!("Failed to create filesystem watcher: {}", e))?;
        let mut registrar = watcher.paths_mut();
        for (path, mode) in paths {
            registrar.add(path, *mode).map_err(|e| {
                if *path == self.target.git_dir {
                    format!("Failed to watch git directory: {}", e)
                } else if self.target.common.as_ref() == Some(path) {
                    format!("Failed to watch common git directory: {}", e)
                } else if self.target.root.as_ref() == Some(path) {
                    format!("Failed to watch work tree root: {}", e)
                } else {
                    format!("Failed to watch {}: {}", path.display(), e)
                }
            })?;
        }
        // One stream creation for the whole set. An early `?` above drops the
        // registrar without committing, which leaves the watcher stopped
        // rather than partially armed.
        registrar
            .commit()
            .map_err(|e| format!("Failed to install repository watches: {}", e))?;
        Ok(watcher)
    }

    /// Registers `plan` first, with nothing to replace. Falls back to the whole
    /// root when a narrowed registration cannot be made.
    fn start(
        target: Target,
        forwarder: Arc<Forwarder>,
        lifetime: Sender<()>,
        scope: WatchScope,
    ) -> Result<Arc<Self>, String> {
        let filter = Arc::clone(&forwarder.filter);
        let narrowing = scope == WatchScope::Narrow && target.narrowable;
        let mut rules = Arc::new(IgnoreRules::builtin());
        let mut initial_rules = None;
        let mut plan = Plan::Whole(None);
        if narrowing {
            // Before the first registration, so it costs one RPC to fseventsd,
            // not two; the loop reuses these instead of listing again.
            if let Some(root) = &target.root {
                let loaded = Arc::new(IgnoreRules::load(root));
                filter.publish_rules(Arc::clone(&loaded));
                plan = scope::plan(root, &target.git_dir, target.common.as_deref(), &loaded);
                initial_rules = Some(Arc::clone(&loaded));
                rules = loaded;
            }
        }
        let planned_from = initial_rules.clone();
        let registration = Arc::new_cyclic(|me| Registration {
            me: me.clone(),
            target,
            forwarder,
            lifetime: Mutex::new(Some(lifetime)),
            refresh_lock: Mutex::new(()),
            state: Mutex::new(State {
                watcher: None,
                plan: Plan::Whole(None),
                registered: Vec::new(),
                requested: scope,
                permanent: false,
                last: None,
                pending: false,
                rules,
                planned_from,
                initial_rules,
                logged_reason: None,
                shut: false,
                #[cfg(target_os = "macos")]
                ancestors: None,
            }),
            reregistrations: AtomicU64::new(0),
        });
        {
            let mut state = lock_or_recover(&registration.state);
            let narrow_first = matches!(plan, Plan::Narrow(_));
            match registration.swap_in(&mut state, plan, false) {
                Ok(()) => {}
                Err(reason) if narrow_first => {
                    log::info!(
                        target: "watcher",
                        "{}: registering the whole worktree: {reason}",
                        registration.describe()
                    );
                    registration.swap_in(&mut state, Plan::Whole(Some(reason)), false)?;
                }
                Err(reason) => return Err(reason),
            }
        }
        filter.bind(&registration);
        Ok(registration)
    }

    fn describe(&self) -> String {
        self.target
            .root
            .as_ref()
            .unwrap_or(&self.target.git_dir)
            .display()
            .to_string()
    }

    /// Makes `plan` the registration. The new stream is live before the old one
    /// is dropped. `announce` injects the synthetic events a re-registration
    /// owes (the first registration owes none).
    fn swap_in(&self, state: &mut State, plan: Plan, announce: bool) -> Result<(), String> {
        if state.shut {
            return Err("the watch has been shut down".into());
        }
        let paths = match &plan {
            Plan::Whole(_) => self.whole_paths(),
            Plan::Narrow(layout) => layout.paths.clone(),
        };
        #[cfg(target_os = "macos")]
        self.sync_ancestors(state, &plan)?;
        let watcher = self.build(&paths)?;
        // The new stream is running; only now does the old one stop.
        let old = state.watcher.replace(watcher);
        drop(old);
        let previous = std::mem::replace(&mut state.plan, plan);
        state.registered = paths;
        state.pending = false;
        self.forwarder
            .filter
            .set_excluded(state.plan.excluded().to_vec());
        self.note_reason(state);
        if announce {
            state.last = Some(Instant::now());
            self.reregistrations.fetch_add(1, Ordering::AcqRel);
            self.announce(&previous, &state.plan);
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn sync_ancestors(&self, state: &mut State, plan: &Plan) -> Result<(), String> {
        match (plan, &self.target.root) {
            (Plan::Narrow(layout), Some(root)) => {
                let dirs = scope::ancestor_dirs(root, layout);
                match &state.ancestors {
                    Some(ancestors) => ancestors.set_dirs(dirs),
                    None => {
                        state.ancestors = Some(super::ancestors::AncestorWatch::start(
                            self.me.clone(),
                            dirs,
                        )?);
                    }
                }
            }
            _ => {
                if let Some(ancestors) = state.ancestors.take() {
                    // May be this very thread: stop it, never wait for it.
                    ancestors.stop();
                }
            }
        }
        Ok(())
    }

    /// Logs a refusal to narrow once per distinct reason.
    fn note_reason(&self, state: &mut State) {
        if let Plan::Whole(Some(reason)) = &state.plan {
            if state.logged_reason.as_ref() != Some(reason) {
                log::info!(
                    target: "watcher",
                    "{}: registering the whole worktree: {reason}",
                    self.describe()
                );
                state.logged_reason = Some(reason.clone());
            }
        }
    }

    /// The events that make the loop refresh after a re-registration: the
    /// entries that appeared, vanished or came under the stream, or else the
    /// worktree root. Whatever landed before the new stream started is lost to
    /// it; these are what make the next read see it.
    fn announce(&self, old: &Plan, new: &Plan) {
        let Some(root) = &self.target.root else {
            return;
        };
        let mut events = Vec::new();
        if let (Plan::Narrow(old), Plan::Narrow(new)) = (old, new) {
            let changes = scope::differences(root, old, new);
            if changes.len() > MAX_ANNOUNCED_ENTRIES {
                // Too many to name: a path-less event, which the classifier
                // reads as "unknown" and answers with a full refresh. A root-only
                // event would not (the root names no file), and entries that
                // landed before the new stream started would wait for the poll.
                events.push(Event::new(EventKind::Other));
            } else {
                for (path, kind) in changes {
                    let kind = match kind {
                        Some(EntryKind::Dir) => EventKind::Create(CreateKind::Folder),
                        Some(EntryKind::File) => EventKind::Create(CreateKind::File),
                        Some(_) => EventKind::Create(CreateKind::Any),
                        None => EventKind::Remove(RemoveKind::Any),
                    };
                    events.push(Event::new(kind).add_path(path));
                }
            }
        }
        if events.is_empty() {
            events.push(Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.clone()));
        }
        for event in events {
            self.forwarder.deliver(Ok(event));
        }
    }

    fn window_open(last: Option<Instant>) -> bool {
        last.is_none_or(|last| last.elapsed() >= SCAN_COALESCE)
    }

    /// Plans again and installs the result when it differs. Narrowing waits for
    /// the rate bound; widening to the whole root never does.
    fn refresh(&self, mode: Mode, immediate: bool) {
        if !self.target.narrowable {
            return;
        }
        let Some(root) = &self.target.root else {
            return;
        };
        let _serial = lock_or_recover(&self.refresh_lock);
        let (rules, current) = {
            let state = lock_or_recover(&self.state);
            if state.shut || state.requested == WatchScope::Whole || state.permanent {
                return;
            }
            (Arc::clone(&state.rules), state.plan.excluded().to_vec())
        };
        let common = self.target.common.as_deref();
        let desired = match mode {
            Mode::Recompute => scope::plan(root, &self.target.git_dir, common, &rules),
            Mode::Revalidate => scope::replan(root, &self.target.git_dir, common, &current),
        };
        let mut state = lock_or_recover(&self.state);
        if state.shut || state.requested == WatchScope::Whole || state.permanent {
            return;
        }
        if matches!(mode, Mode::Recompute) {
            state.planned_from = Some(rules);
        }
        if desired.same_registration(&state.plan) {
            self.note_reason_of(&mut state, &desired);
            return;
        }
        let widening = matches!(desired, Plan::Whole(_));
        if !widening && !immediate && !Self::window_open(state.last) {
            state.pending = true;
            return;
        }
        self.install(&mut state, desired);
    }

    fn note_reason_of(&self, state: &mut State, desired: &Plan) {
        if let Plan::Whole(Some(reason)) = desired {
            if state.logged_reason.as_ref() != Some(reason) {
                log::info!(
                    target: "watcher",
                    "{}: registering the whole worktree: {reason}",
                    self.describe()
                );
                state.logged_reason = Some(reason.clone());
            }
        }
    }

    /// Installs `desired`; a narrowed registration that cannot be made becomes
    /// the whole root.
    fn install(&self, state: &mut State, desired: Plan) {
        let narrowing = matches!(desired, Plan::Narrow(_));
        let Err(reason) = self.swap_in(state, desired, true) else {
            return;
        };
        if state.shut {
            return;
        }
        log::warn!(
            target: "watcher",
            "{}: re-registering the watch failed: {reason}",
            self.describe()
        );
        if narrowing {
            if let Err(reason) = self.swap_in(state, Plan::Whole(Some(reason)), true) {
                log::error!(
                    target: "watcher",
                    "{}: falling back to the whole worktree failed: {reason}",
                    self.describe()
                );
            }
        }
    }

    // ---- what the loop, the kqueue thread and the command thread call -----

    /// The rules the loop loaded before the first registration, once.
    pub(crate) fn take_initial_rules(&self) -> Option<Arc<IgnoreRules>> {
        lock_or_recover(&self.state).initial_rules.take()
    }

    /// The loop loaded (or reloaded) the ignore rules: narrow, widen or
    /// re-narrow to match.
    pub(crate) fn apply_rules(&self, rules: &Arc<IgnoreRules>) {
        {
            let mut state = lock_or_recover(&self.state);
            state.rules = Arc::clone(rules);
            if state
                .planned_from
                .as_ref()
                .is_some_and(|planned| Arc::ptr_eq(planned, rules))
            {
                return;
            }
        }
        self.refresh(Mode::Recompute, false);
    }

    /// True when exclusions are in force, which is when an index write may have
    /// made one of them wrong.
    pub(crate) fn has_exclusions(&self) -> bool {
        !lock_or_recover(&self.state).plan.excluded().is_empty()
    }

    /// Asks (a) and (b) again of the exclusions in force (two spawns, no new
    /// listing). What makes `git add -f target/x` drop `target` from the set.
    pub(crate) fn revalidate(&self) {
        self.refresh(Mode::Revalidate, false);
    }

    /// When a re-registration the rate bound held back may go ahead.
    pub(crate) fn pending_at(&self) -> Option<Instant> {
        let state = lock_or_recover(&self.state);
        if !state.pending || state.shut {
            return None;
        }
        Some(
            state
                .last
                .map_or_else(Instant::now, |last| last + SCAN_COALESCE),
        )
    }

    /// Performs a held-back re-registration whose window has ended.
    pub(crate) fn run_pending(&self) {
        {
            let mut state = lock_or_recover(&self.state);
            if !state.pending || state.shut || !Self::window_open(state.last) {
                return;
            }
            // Claimed here, so the loop and the kqueue thread never both plan
            // for one request.
            state.pending = false;
        }
        self.refresh(Mode::Recompute, false);
    }

    /// The kqueue says a split ancestor changed: if its entry names are not
    /// what was registered, plan again. An atomic save (a temporary name that
    /// came and went) leaves the names as they were and costs nothing.
    #[cfg(target_os = "macos")]
    pub(crate) fn ancestors_changed(&self) {
        let Some(root) = &self.target.root else {
            return;
        };
        let layout = {
            let state = lock_or_recover(&self.state);
            match &state.plan {
                Plan::Narrow(layout) if !state.shut => layout.clone(),
                _ => return,
            }
        };
        if scope::entries_changed(root, &layout) {
            {
                // The rate bound covers the planning too (two git spawns), not
                // only the re-registration: a burst of top-level churn must not
                // be a burst of git processes.
                let mut state = lock_or_recover(&self.state);
                if !Self::window_open(state.last) {
                    state.pending = true;
                    return;
                }
            }
            self.refresh(Mode::Recompute, false);
        }
    }

    /// Evidence that the narrowing is wrong (a status row under an excluded
    /// directory, a convicted poll): the whole root, immediately, for the rest
    /// of this watch's life.
    pub(crate) fn widen_for_evidence(&self, reason: &str) {
        let mut state = lock_or_recover(&self.state);
        if state.shut {
            return;
        }
        if !state.permanent {
            state.permanent = true;
            if matches!(state.plan, Plan::Narrow(_)) {
                log::warn!(
                    target: "watcher",
                    "{}: registering the whole worktree for the rest of this watch: {reason}",
                    self.describe()
                );
            }
        }
        if matches!(state.plan, Plan::Narrow(_)) {
            self.install(&mut state, Plan::Whole(Some(format!("evidence: {reason}"))));
        }
    }

    /// An explicit scope from the caller. The same scope again changes
    /// nothing; a different one re-arms whatever evidence had widened.
    pub(crate) fn set_scope(&self, scope: WatchScope) {
        {
            let mut state = lock_or_recover(&self.state);
            if state.shut || state.requested == scope {
                return;
            }
            state.requested = scope;
            state.permanent = false;
            state.pending = false;
            if scope == WatchScope::Whole {
                if matches!(state.plan, Plan::Narrow(_)) {
                    self.install(&mut state, Plan::Whole(None));
                }
                return;
            }
        }
        self.refresh(Mode::Recompute, true);
    }

    /// Stops the native watcher. True when this call released one. Waits no
    /// more than half a second for the kqueue thread.
    pub(crate) fn shutdown(&self) -> bool {
        let mut released = false;
        let ancestors = {
            let mut state = lock_or_recover(&self.state);
            state.shut = true;
            // Before the watcher is dropped: its callbacks hold the other
            // clones, and the loss of the last one is the shutdown receipt.
            drop(lock_or_recover(&self.lifetime).take());
            if let Some(watcher) = state.watcher.take() {
                drop(watcher);
                released = true;
            }
            #[cfg(target_os = "macos")]
            let ancestors = state.ancestors.take();
            #[cfg(not(target_os = "macos"))]
            let ancestors = None::<()>;
            ancestors
        };
        #[cfg(target_os = "macos")]
        if let Some(mut ancestors) = ancestors {
            ancestors.stop();
            ancestors.join(Duration::from_millis(500));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = ancestors;
        released
    }

    // ---- observation, for tests and logs ---------------------------------

    /// Re-registrations made since the first registration.
    #[allow(dead_code, reason = "an accessor for tests and logs")]
    pub(crate) fn reregistrations(&self) -> u64 {
        self.reregistrations.load(Ordering::Acquire)
    }

    /// The scope last asked for.
    #[allow(dead_code, reason = "an accessor for tests and logs")]
    pub(crate) fn scope(&self) -> WatchScope {
        lock_or_recover(&self.state).requested
    }

    /// True when the current registration leaves directories out.
    #[allow(dead_code, reason = "an accessor for tests and logs")]
    pub(crate) fn narrowed(&self) -> bool {
        matches!(lock_or_recover(&self.state).plan, Plan::Narrow(_))
    }

    /// The directories left out, worktree-relative.
    #[allow(dead_code, reason = "an accessor for tests and logs")]
    pub(crate) fn excluded(&self) -> Vec<String> {
        lock_or_recover(&self.state).plan.excluded().to_vec()
    }

    /// Every path the native stream was given.
    #[allow(dead_code, reason = "an accessor for tests and logs")]
    pub(crate) fn registered_paths(&self) -> Vec<PathBuf> {
        lock_or_recover(&self.state)
            .registered
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }
}

impl RepoFileWatcher {
    pub(crate) fn filter(&self) -> Arc<EventFilter> {
        Arc::clone(&self.filter)
    }

    /// The registration, shared with the session so a command can change the
    /// scope of a live watch.
    pub(crate) fn registration(&self) -> Arc<Registration> {
        Arc::clone(&self.registration)
    }

    /// A sender onto this watcher's event channel, for waking its reader.
    pub(crate) fn waker(&self) -> SyncSender<Result<Event, notify::Error>> {
        self.wake.clone()
    }

    /// False once the native backend has released its callback, which is what
    /// the channel's disconnecting used to say before it had a second sender.
    /// A watcher whose backend was shut down on purpose is not alive either.
    pub(crate) fn backend_alive(&self) -> bool {
        !matches!(
            self.retired.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        )
    }

    #[cfg(test)]
    pub(crate) fn simulate_overflow(&self) {
        self.overflow.store(true, Ordering::Release);
    }

    /// True once if events were dropped because the queue was full since the
    /// last call. Dropped events are unknowable, so the caller refreshes
    /// everything — the same answer a `Rescan` event gets.
    pub(crate) fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }

    /// Wait for the backend to release its callback and outstanding native
    /// reads. notify's Windows Drop only queues Stop; dropping its Rust handle
    /// alone does not mean ReadDirectoryChangesW has relinquished the tree.
    pub(crate) fn shutdown(&mut self) -> Result<(), String> {
        // Whatever this watch said before it ended is not to be relied on after.
        self.filter.skip().end();
        let started = Instant::now();
        if self.registration.shutdown() {
            let budget = Duration::from_secs(2).saturating_sub(started.elapsed());
            match self.retired.recv_timeout(budget) {
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(()),
                _ => Err(
                    "Filesystem watcher did not release its native resources within 2 seconds"
                        .into(),
                ),
            }
        } else {
            Ok(())
        }
    }

    /// Watches only a resolved git directory recursively (work-tree `.git`,
    /// linked-worktree git dir, or bare repo). See [`Self::watch_repo`] for
    /// the full-repo form.
    pub fn watch(git_dir: &Path) -> Result<Self, String> {
        Self::watch_repo(git_dir, None, None)
    }

    /// Watches a repository for the change detector: the resolved git dir
    /// recursively, the shared common dir when it differs (linked worktrees
    /// keep refs in the main repo's git dir, so branch/ref writes made
    /// anywhere must fire), and — when the repository has a separate
    /// worktree root — that root too.
    ///
    /// How the worktree root is watched depends on the platform
    /// ([`ROOT_WATCH_MODE`]).
    ///
    /// On macOS it is RECURSIVE. FSEvents is recursive in the kernel, and the
    /// stream already covers the root because `.git` lives under it; the
    /// backend's `NonRecursive` mode was only a user-space filter that threw
    /// nested paths away (`framework/notify/src/fsevent.rs`, `callback_impl`).
    /// Choosing `Recursive` removes that filter, adds no stream and no
    /// registration, and lets a nested edit be seen. What would then flood the
    /// bounded event queue (a build writing thousands of files under `target/`)
    /// is dropped by [`EventFilter::admit`] on the callback thread, before it
    /// takes a slot; see [`super::filter`] for what that gate may be wrong about.
    /// It is still `fseventsd` that pays for those events, which is what
    /// [`WatchScope::Narrow`] (the default here) avoids; see [`super::scope`].
    ///
    /// On Linux and Windows it stays NON-recursive (top-level entries only):
    /// inotify watches one descriptor per directory and ReadDirectoryChangesW
    /// reports a whole tree, so recursion there is bounded by descriptors or
    /// pays for the whole tree. Deeper edits still reach us through the
    /// index/HEAD writes they cause inside `.git`, and the status poll covers
    /// the rest. The scope changes nothing there.
    ///
    /// # Registration is atomic (was: lossy on macOS)
    ///
    /// Every path is accumulated through one `Watcher::paths_mut()` handle and
    /// installed by a single `commit()`, rather than by successive `watch()`
    /// calls.
    ///
    /// This is not tidiness. notify's fsevent backend implements each
    /// `watch()` as `stop()` + append-path + `run()`, and `run()` recreates
    /// the whole `FSEventStream` with a fresh `kFSEventStreamEventIdSinceNow`
    /// epoch — so a change landing between two successive registrations fell
    /// into no epoch and was missed permanently. With three paths to register
    /// (git dir, common dir, worktree root) that left two windows on every
    /// watch startup, and the six-second status poll existed partly to paper
    /// over them. `FsEventPathsMut` stops once on construction, only appends
    /// in `add()`, and runs once in `commit()`: one stream creation for the
    /// whole set, and no window at all. A later re-registration (see
    /// [`Registration`]) builds its new stream before dropping the old one for
    /// the same reason.
    ///
    /// The `PathsMut` API is uniform across backends (inotify and the polling
    /// fallback implement it as plain add/remove), so this is not a
    /// macOS-specific code path — it is the same call everywhere, and it is
    /// simply also correct on FSEvents.
    pub fn watch_repo(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
    ) -> Result<Self, String> {
        Self::watch_repo_scoped(git_dir, worktree_root, common_dir, WatchScope::default())
    }

    /// [`Self::watch_repo`] with the scope named.
    pub fn watch_repo_scoped(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
        scope: WatchScope,
    ) -> Result<Self, String> {
        Self::watch_repo_inner(git_dir, worktree_root, common_dir, ROOT_WATCH_MODE, scope)
    }

    /// [`Self::watch_repo`] with the worktree root's mode named, so a test can
    /// exercise the mode the other platforms use on this one.
    #[cfg(test)]
    pub(crate) fn watch_repo_with(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
        root_mode: RecursiveMode,
    ) -> Result<Self, String> {
        Self::watch_repo_inner(
            git_dir,
            worktree_root,
            common_dir,
            root_mode,
            WatchScope::default(),
        )
    }

    fn watch_repo_inner(
        git_dir: &Path,
        worktree_root: Option<&Path>,
        common_dir: Option<&Path>,
        root_mode: RecursiveMode,
        scope: WatchScope,
    ) -> Result<Self, String> {
        if !git_dir.exists() {
            return Err(format!(
                "Git directory does not exist: {}",
                git_dir.display()
            ));
        }

        // Validate before touching the watcher: a missing worktree root used to
        // be reported only AFTER two paths were already live, which left a
        // half-registered watcher behind on the error path.
        if let Some(root) = worktree_root {
            if !root.exists() {
                return Err(format!(
                    "Repository work tree does not exist: {}",
                    root.display()
                ));
            }
        }

        let (tx, rx) = sync_channel(EVENT_QUEUE_CAPACITY);
        let wake = tx.clone();
        let overflow = Arc::new(AtomicBool::new(false));
        let (lifetime, retired) = channel::<()>();

        let mut internal_roots = vec![git_dir.to_path_buf()];
        if let Some(common) = common_dir {
            if !internal_roots.iter().any(|root| root == common) {
                internal_roots.push(common.to_path_buf());
            }
        }
        let root = worktree_root.filter(|root| *root != git_dir);
        let filter = EventFilter::new(root, internal_roots, root_mode);
        let forwarder = Arc::new(Forwarder {
            tx,
            overflow: Arc::clone(&overflow),
            filter: Arc::clone(&filter),
        });
        let target = Target {
            git_dir: git_dir.to_path_buf(),
            common: common_dir.map(Path::to_path_buf),
            root: root.map(Path::to_path_buf),
            root_mode,
            narrowable: cfg!(target_os = "macos")
                && root_mode == RecursiveMode::Recursive
                && root.is_some(),
        };
        let registration = Registration::start(target, forwarder, lifetime, scope)?;

        Ok(Self {
            registration,
            retired,
            receiver: rx,
            overflow,
            wake,
            filter,
        })
    }
}

/// Hands one native event to the debounce loop without ever blocking the
/// backend's callback thread: a full queue records an overflow instead.
fn forward_event(
    tx: &SyncSender<Result<Event, notify::Error>>,
    overflow: &AtomicBool,
    res: Result<Event, notify::Error>,
) {
    match tx.try_send(res) {
        Err(TrySendError::Full(_)) => overflow.store(true, Ordering::Release),
        // The consumer is gone; the loop's own disconnect handling reports it.
        Err(TrySendError::Disconnected(_)) | Ok(()) => {}
    }
}

impl Drop for RepoFileWatcher {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::warn!(target: "watcher", "{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_records_one_overflow_instead_of_growing() {
        let (tx, rx) = sync_channel(2);
        let overflow = AtomicBool::new(false);
        for _ in 0..5 {
            forward_event(&tx, &overflow, Ok(Event::new(notify::EventKind::Any)));
        }
        assert_eq!(rx.try_iter().count(), 2, "the queue is bounded");
        assert!(
            overflow.swap(false, Ordering::AcqRel),
            "the drop is recorded"
        );
        assert!(!overflow.load(Ordering::Acquire));
    }
}
