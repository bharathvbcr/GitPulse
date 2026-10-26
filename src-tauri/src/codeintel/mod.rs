//! Code intelligence module: in-process devmap querying over devmap's resolved state directory.
//!
//! Links `devmap-query` and `devmap-store` directly without requiring a background
//! daemon or Unix socket. Provides fast symbol search, impact analysis, dependency
//! tracing, and dead code detection.

use crate::engine::git_cli::validate_repo;
use devmap_query::{
    Cancel, Request, ResolutionAvailability, Response, Rung, RungHistogram, SourceFreshness,
    StoreQueryEngine,
};
use devmap_resolve::model::ResolvedEdge;
use devmap_store::{Store, CURRENT_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default token budget for in-process query operations.
pub mod suspects;
pub use suspects::{
    suspects, CodeintelSuspect, CodeintelSuspectScope, CodeintelSuspectsPayload,
    CodeintelTouchedSymbol,
};

pub const DEFAULT_CODEINTEL_BUDGET: u32 = 2000;

/// Hard cap on symbols returned for one file. A megabyte of generated source
/// can declare thousands of symbols; the collision and diff callers need a
/// bounded page, not the whole corpus.
pub const MAX_FILE_SYMBOLS: usize = 256;

/// Schema version this GitPulse build can read. Pinned to the linked
/// `devmap_store` constant so a re-vendor that drifts fails a test rather than
/// shipping a dead panel.
pub const SUPPORTED_STORE_SCHEMA: i32 = CURRENT_SCHEMA_VERSION;

/// Upstream `MAX_NEIGHBOR_TARGETS` — chunk affected-tests / neighbors seeds here.
pub const MAX_NEIGHBOR_TARGETS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelSymbolHit {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub span_start_line: u32,
    pub span_end_line: u32,
    pub source_span: String,
    /// Why `source_span` is empty, when it is empty because the file could not
    /// be read rather than because the symbol has no body.
    ///
    /// The engine computes this (`SymbolHit::source_unavailable_reason`) for
    /// exactly the case where the map names a file the working tree no longer
    /// has — a stale generation, a different checkout, a deleted file. Dropping
    /// it made a hit whose source could not be read render identically to one
    /// whose source is genuinely blank.
    pub source_unavailable_reason: Option<String>,
    /// Bytes withheld by the query budget, distinct from unreadable source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span_omitted_bytes: Option<u32>,
    pub score: f32,
}

/// One symbol declared in a single file, with line spans for hunk joins.
///
/// Unlike a search hit this carries no source body and no score — the caller
/// already named the file, and the join is on line ranges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeintelFileSymbol {
    pub symbol_name: String,
    pub qualified_name: String,
    pub kind: String,
    pub span_start_line: u32,
    pub span_end_line: u32,
}

/// Bounded symbols-in-file read against one DevMap generation.
///
/// `head_matches` is the whole point of carrying `requested_head_sha` and
/// `generation_head_sha` together: a caller that joins these spans to a
/// working-tree diff must refuse the join when the index was built at a
/// different commit. An empty `items` with `available: true` and
/// `head_matches: true` means the file genuinely has no indexed symbols;
/// `available: false` means the check could not run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelFileSymbols {
    pub available: bool,
    pub reason: Option<String>,
    pub file_path: String,
    pub requested_head_sha: String,
    pub generation_id: Option<u32>,
    pub generation_head_sha: Option<String>,
    /// True only when `generation_head_sha` equals `requested_head_sha`.
    pub head_matches: bool,
    pub items: Vec<CodeintelFileSymbol>,
    pub total: u32,
    pub shown: u32,
    pub truncated: bool,
}

impl CodeintelFileSymbols {
    fn unavailable(
        file_path: impl Into<String>,
        requested_head_sha: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            file_path: file_path.into(),
            requested_head_sha: requested_head_sha.into(),
            generation_id: None,
            generation_head_sha: None,
            head_matches: false,
            items: Vec::new(),
            total: 0,
            shown: 0,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelEdge {
    pub source_file: String,
    pub target_file: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelDeadSymbol {
    pub symbol_name: String,
    pub file_path: String,
    pub confidence: f32,
    pub is_exempt: bool,
    pub exemption_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelRungHistogram {
    pub deterministic: usize,
    pub high: usize,
    pub speculative: usize,
    pub filtered_out: usize,
}

impl From<RungHistogram> for CodeintelRungHistogram {
    fn from(h: RungHistogram) -> Self {
        Self {
            deterministic: h.deterministic,
            high: h.high,
            speculative: h.speculative,
            filtered_out: h.filtered_out,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelResponse<T> {
    /// Whole-tree freshness for the generation behind this answer. Always an
    /// object: `fresh: null` plus a reason means the check did not run — never
    /// confuse that with a verified stale/fresh bool.
    pub source_freshness: SourceFreshness,
    pub available: bool,
    pub reason: Option<String>,
    pub items: Vec<T>,
    pub total: u32,
    pub shown: u32,
    pub truncated: bool,
    /// Set when the producer stopped early — distinct from budget truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_incomplete: Option<String>,
    /// Population across the resolution ladder before any `min_rung` filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rungs: Option<CodeintelRungHistogram>,
}

impl<T> CodeintelResponse<T> {
    fn unverified_freshness() -> SourceFreshness {
        SourceFreshness::unverified(
            "this query did not verify whole-tree freshness; ask status for a live check",
        )
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            source_freshness: Self::unverified_freshness(),
            available: false,
            reason: Some(reason.into()),
            items: Vec::new(),
            total: 0,
            shown: 0,
            truncated: false,
            walk_incomplete: None,
            rungs: None,
        }
    }

    pub fn ok(items: Vec<T>, total: u32, shown: u32, truncated: bool) -> Self {
        Self {
            source_freshness: Self::unverified_freshness(),
            available: true,
            reason: None,
            items,
            total,
            shown,
            truncated,
            walk_incomplete: None,
            rungs: None,
        }
    }
}

fn merge_source_freshness(left: SourceFreshness, right: SourceFreshness) -> SourceFreshness {
    match (left.fresh, right.fresh) {
        (Some(false), _) => left,
        (_, Some(false)) => right,
        (Some(true), Some(true)) => left,
        _ => SourceFreshness::unverified(
            "combined answer did not verify whole-tree freshness for every facet",
        ),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelStatus {
    pub available: bool,
    #[serde(default)]
    pub is_fresh: Option<bool>,
    #[serde(default)]
    pub freshness_reason: Option<String>,
    /// Independent checks; null means this reader could not verify that facet.
    #[serde(default)]
    pub source_freshness: Option<bool>,
    #[serde(default)]
    pub analyzer_freshness: Option<bool>,
    #[serde(default)]
    pub pending_count: Option<usize>,
    pub db_path: String,
    pub generation_id: Option<u32>,
    pub total_files: Option<u32>,
    pub total_symbols: Option<u32>,
    pub total_edges: Option<u32>,
    pub reason: Option<String>,
}

/// Locates the code intelligence SQLite database for a repository.
pub fn devmap_db_path(repo_path: &str) -> PathBuf {
    map_path(Path::new(repo_path))
}

fn map_path(repo: &Path) -> PathBuf {
    devmap_query::paths::store_path(repo)
}

/// Resolves `repo_path` through the same gate every other repository-scoped
/// surface uses, and returns the canonical work tree.
///
/// The six entry points below took the caller's string and joined it straight
/// onto the map path, checking only that the resulting file existed. So `""`
/// resolved against the *server process's own* working directory and answered
/// about whatever repository it happened to be started in, and `"../other"`
/// reached out of the repository the caller named — a cross-repository read
/// driven purely by an argument documented as "absolute path to a git
/// repository". `gitpulse_status`, `gitpulse_ledger_events` and
/// `gitpulse_task_view` already refuse both; these did not.
///
/// [`validate_repo`] canonicalizes, so `..` segments and symlinks are resolved
/// *before* the map path is built rather than left for the filesystem to follow
/// afterwards, and it is the existing owner of this decision rather than a
/// second path policy standing beside it.
fn resolve_repo(repo_path: &str) -> Result<PathBuf, String> {
    validate_repo(repo_path)
}

fn rewrite_schema_mismatch(raw: &str) -> Option<String> {
    // Vendored store refuses future schemas with this wording. Surface the
    // handshake the UI can act on rather than the rusqlite parameter name.
    const PREFIX: &str = "unsupported future schema version ";
    let idx = raw.find(PREFIX)?;
    let rest = &raw[idx + PREFIX.len()..];
    let version: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if version.is_empty() {
        return None;
    }
    Some(format!(
        "map built by devmap schema {version}, this build reads {SUPPORTED_STORE_SCHEMA}"
    ))
}

fn open_store(repo: &Path) -> Result<Store, String> {
    let db_path = map_path(repo);
    if !db_path.exists() {
        return Err(format!(
            "No devmap database at {}",
            db_path.to_string_lossy()
        ));
    }
    let store = Store::open_read_only(&db_path).map_err(|e| {
        let raw = e.to_string();
        if let Some(friendly) = rewrite_schema_mismatch(&raw) {
            return friendly;
        }
        format!("Failed to open devmap database: {raw}")
    })?;
    store
        .validate_repo_root(repo)
        .map_err(|error| error.to_string())?;
    Ok(store)
}

/// Opens the store AND requires that it actually hold an indexed generation.
///
/// The five query surfaces below all answered from a store that opened but held
/// nothing, and `CodeintelResponse::ok` with an empty list is indistinguishable
/// from a completed search that found no matches. So a repository whose map had
/// never been built — or was still building — reported "no callers", "no
/// dependencies" and "no dead symbols": a check that could not run, rendering
/// exactly like one that ran and found nothing clean.
///
/// `status` deliberately does NOT use this. It has to tell "no database",
/// "no generation yet" and "database unreadable" apart, because those call for
/// three different actions, and collapsing them here would lose that.
fn open_indexed_store(repo: &Path) -> Result<Store, String> {
    let store = open_store(repo)?;
    match store.latest_generation_id() {
        Ok(Some(_)) => Ok(store),
        Ok(None) => Err(format!(
            "No generation indexed in {}; the code map has not been built yet",
            map_path(repo).to_string_lossy()
        )),
        Err(e) => Err(format!("Failed to read devmap generation: {e}")),
    }
}

/// Validates the repository and opens its built map — the whole preamble every
/// query surface shares.
pub(crate) fn open_repo_map(repo_path: &str) -> Result<Store, String> {
    open_indexed_store(&resolve_repo(repo_path)?)
}

/// Refuses a blank query argument instead of answering it.
///
/// A whitespace-only search query is not a search that found nothing: the
/// engine short-circuits it to an empty *Available* response, which renders as
/// "no symbol matches that" for a caller who asked nothing. The other surfaces
/// funnel a blank argument into "… is not indexed" or "… has no indexed
/// traversal start" — true statements that name the map rather than the
/// mistake, and that read as a finding about the repository.
fn require_argument(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be blank"));
    }
    Ok(())
}

/// Carries the engine's own refusal across the wire boundary.
///
/// `devmap-query` answers `Unavailable { reason }` whenever it could not resolve
/// what was asked: the file is not indexed, the file could not be parsed, the
/// target has no indexed traversal start, there is no indexed path between two
/// symbols. Every call site here used to funnel that through
/// [`CodeintelResponse::ok`], which hardcodes `available: true, reason: None` —
/// so a typo'd or newly added symbol came back `total: 0, items: []`, byte for
/// byte what a symbol with genuinely no callers returns. An agent following this
/// repository's own instruction to check its blast radius before editing read
/// that as "nothing calls this, safe to change". The reason the engine went to
/// the trouble of computing is the only thing separating the two cases, so it is
/// what crosses the boundary.
fn from_engine<S, T>(response: Response<S>, map: impl Fn(S) -> T) -> CodeintelResponse<T> {
    let Response {
        items,
        total,
        shown,
        truncated,
        resolution,
        walk_incomplete,
        rungs,
        source_freshness,
        ..
    } = response;
    let mut out = match resolution {
        ResolutionAvailability::Unavailable { reason } => CodeintelResponse::unavailable(reason),
        ResolutionAvailability::Available => CodeintelResponse::ok(
            items.into_iter().map(map).collect(),
            total,
            shown,
            truncated,
        ),
    };
    out.walk_incomplete = walk_incomplete.and_then(|reason| {
        fold_qualification_reason(std::iter::once(reason), QUALIFICATION_MAX_CHARS)
    });
    out.rungs = rungs.map(CodeintelRungHistogram::from);
    out.source_freshness = source_freshness;
    out
}

fn parse_min_rung(min_rung: Option<&str>) -> Result<Option<Rung>, String> {
    match min_rung.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some("deterministic") => Ok(Some(Rung::Deterministic)),
        Some("high") => Ok(Some(Rung::High)),
        Some("speculative") => Ok(Some(Rung::Speculative)),
        Some(other) => Err(format!(
            "min_rung must be deterministic|high|speculative, got {other}"
        )),
    }
}

fn map_edge(edge: ResolvedEdge) -> CodeintelEdge {
    CodeintelEdge {
        source_file: edge.source_file,
        target_file: edge.target_file,
        source_symbol: edge.source_symbol,
        target_symbol: edge.target_symbol,
        confidence: edge.confidence.0,
    }
}

/// Cooperative cancel token for long walks — thin alias over the kernel's.
pub type QueryCancel = Cancel;

/// Wall-clock backstop for long UI walks. Freeing the await without tripping
/// this flag leaves the blocking traversal running on the pool.
pub const QUERY_CANCEL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The one shared deadline timer behind every cancellable query.
///
/// A single long-lived thread sleeps on a condvar until the earliest
/// registered deadline (with no timeout at all when nothing is registered, so
/// an idle process takes no wakeups) and trips that query's cancel flag.
/// Registering is a map insert, not a thread, so query rate cannot exhaust
/// threads and the deadline cannot silently disappear under load.
mod deadline_timer {
    use super::Cancel;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    /// Most deadlines held at once. A registration past it fails closed; it is
    /// far above any UI call rate times the 30 s deadline.
    pub(super) const MAX_PENDING: usize = 65_536;

    /// Identifies one registration so it can be withdrawn on completion.
    pub(super) type TimerKey = (Instant, u64);

    #[derive(Default)]
    struct State {
        next_id: u64,
        due: BTreeMap<TimerKey, Cancel>,
        closed: bool,
    }

    #[derive(Default)]
    struct Shared {
        state: Mutex<State>,
        wake: Condvar,
        /// Times the worker loop has come round, so a test can tell an idle
        /// sleep from a spin.
        #[cfg(test)]
        passes: std::sync::atomic::AtomicU64,
        /// Worker passes that should panic, so a test can drive the restart path.
        #[cfg(test)]
        panics_left: std::sync::atomic::AtomicU32,
    }

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn run(&self) {
            // Declared before `state`, so it drops after the lock is released.
            let _fail_closed = TripPendingOnUnwind(self);
            let mut state = self.lock();
            loop {
                if state.closed {
                    return;
                }
                #[cfg(test)]
                self.passes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                #[cfg(test)]
                if self
                    .panics_left
                    .try_update(
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                        |n| n.checked_sub(1),
                    )
                    .is_ok()
                {
                    panic!("injected deadline timer panic");
                }
                let now = Instant::now();
                let mut fired = Vec::new();
                while let Some(entry) = state.due.first_entry() {
                    if entry.key().0 > now {
                        break;
                    }
                    fired.push(entry.remove());
                }
                if !fired.is_empty() {
                    // Trip outside the lock so registration is never held up.
                    drop(state);
                    for cancel in fired {
                        cancel.cancel();
                    }
                    state = self.lock();
                    continue;
                }
                let wait = state
                    .due
                    .keys()
                    .next()
                    .map(|(at, _)| at.saturating_duration_since(now));
                state = match wait {
                    None => self
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                    Some(wait) => {
                        self.wake
                            .wait_timeout(state, wait)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
            }
        }
    }

    /// A worker that panics may be restarted, or may be given up on for good,
    /// and either way the deadlines it held are no longer guaranteed. Tripping
    /// them is the fail-closed answer: a cancelled query reports itself, an
    /// untimed one runs on unseen.
    struct TripPendingOnUnwind<'a>(&'a Shared);

    impl Drop for TripPendingOnUnwind<'_> {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                return;
            }
            let pending = std::mem::take(&mut self.0.lock().due);
            for cancel in pending.into_values() {
                cancel.cancel();
            }
        }
    }

    pub(super) struct DeadlineTimer {
        shared: Arc<Shared>,
        thread: JoinHandle<()>,
    }

    impl DeadlineTimer {
        pub(super) fn start() -> std::io::Result<Self> {
            let shared = Arc::new(Shared::default());
            let worker = Arc::clone(&shared);
            let thread =
                crate::supervisor::spawn_service(super::DEADLINE_THREAD, move || worker.run())?;
            Ok(Self { shared, thread })
        }

        /// False once the supervised thread has given up (repeated panics), at
        /// which point nothing registered would ever fire.
        pub(super) fn is_running(&self) -> bool {
            !self.thread.is_finished()
        }

        /// Trip `cancel` after `after`. Errors, rather than leaving the query
        /// untimed, when the timer cannot honour the deadline.
        pub(super) fn register(
            &self,
            cancel: &Cancel,
            after: Duration,
        ) -> Result<TimerKey, String> {
            if !self.is_running() {
                return Err("the deadline timer thread has stopped".into());
            }
            let at = Instant::now()
                .checked_add(after)
                .ok_or_else(|| "the query deadline overflows the clock".to_string())?;
            let mut state = self.shared.lock();
            if state.closed {
                return Err("the deadline timer is shut down".into());
            }
            if state.due.len() >= MAX_PENDING {
                return Err(format!("{MAX_PENDING} query deadlines are already pending"));
            }
            let key = (at, state.next_id);
            state.next_id += 1;
            let earliest = state.due.keys().next().is_none_or(|first| key < *first);
            state.due.insert(key, cancel.clone());
            drop(state);
            if earliest {
                self.shared.wake.notify_one();
            }
            Ok(key)
        }

        /// Withdraw a registration. False when it already fired or was removed.
        pub(super) fn deregister(&self, key: &TimerKey) -> bool {
            self.shared.lock().due.remove(key).is_some()
        }

        #[cfg(test)]
        pub(super) fn pending(&self) -> usize {
            self.shared.lock().due.len()
        }
    }

    impl Drop for DeadlineTimer {
        fn drop(&mut self) {
            self.shared.lock().closed = true;
            self.shared.wake.notify_all();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_support::coverage_relaxed;
        use std::sync::atomic::Ordering;
        use std::sync::Weak;

        fn passes(timer: &DeadlineTimer) -> u64 {
            timer.shared.passes.load(Ordering::Relaxed)
        }

        #[test]
        fn an_idle_timer_sleeps_without_waking() {
            let timer = DeadlineTimer::start().expect("start timer");
            std::thread::sleep(coverage_relaxed(Duration::from_millis(300)));
            // One pass to find nothing to do, then an untimed wait: a spin or a
            // poll interval would count in the hundreds.
            assert!(passes(&timer) <= 2, "idle passes: {}", passes(&timer));
        }

        #[test]
        fn a_far_deadline_is_one_timed_wait_not_a_poll() {
            let timer = DeadlineTimer::start().expect("start timer");
            let cancel = Cancel::new();
            timer
                .register(&cancel, Duration::from_secs(3600))
                .expect("register");
            std::thread::sleep(coverage_relaxed(Duration::from_millis(300)));
            assert!(passes(&timer) <= 3, "passes: {}", passes(&timer));
            assert!(!cancel.is_cancelled());
        }

        fn timer_with_policy(
            name: &'static str,
            policy: crate::supervisor::Policy,
        ) -> DeadlineTimer {
            let shared = Arc::new(Shared::default());
            let worker = Arc::clone(&shared);
            let thread = crate::supervisor::spawn_service_with(name, policy, move || worker.run())
                .expect("start timer");
            DeadlineTimer { shared, thread }
        }

        fn fragile(max_fast_failures: u32) -> crate::supervisor::Policy {
            crate::supervisor::Policy {
                base: Duration::from_millis(1),
                cap: Duration::from_millis(2),
                healthy_after: Duration::from_secs(60),
                max_fast_failures,
            }
        }

        fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
            let limit = Instant::now() + coverage_relaxed(Duration::from_secs(5));
            while !done() {
                assert!(Instant::now() < limit, "timed out: {what}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        #[test]
        fn a_worker_that_gives_up_leaves_no_registered_query_untimed() {
            const THREAD: &str = "codeintel-cancel-deadline";
            let timer = timer_with_policy(THREAD, fragile(2));
            let parked = Cancel::new();
            timer
                .register(&parked, Duration::from_secs(3600))
                .expect("register");
            // Every pass from here panics, until the supervisor gives up.
            timer.shared.panics_left.store(100, Ordering::SeqCst);
            let earlier = Cancel::new();
            timer
                .register(&earlier, Duration::from_secs(3599))
                .expect("register wakes the worker into its panics");
            wait_for("the supervisor gives up", || !timer.is_running());
            assert!(
                parked.is_cancelled() && earlier.is_cancelled(),
                "queries registered with a timer that then stopped for good would never be \
                 timed out; they must be tripped, not left running untimed"
            );
            timer
                .register(&Cancel::new(), Duration::from_secs(1))
                .expect_err("a stopped timer refuses new registrations");

            // Replacing the stopped timer clears the stopped record it left.
            let mut slot = Some(Arc::new(timer));
            let fresh = super::super::running_timer(&mut slot, DeadlineTimer::start)
                .expect("replacement timer");
            assert!(fresh.is_running());
            assert!(
                !crate::supervisor::degraded()
                    .iter()
                    .any(|d| d.subsystem == THREAD),
                "the replaced timer's stopped record must not outlive it"
            );
        }

        #[test]
        fn a_worker_the_supervisor_restarts_keeps_firing_into_the_same_state() {
            let timer = timer_with_policy("codeintel-cancel-deadline-restart", fragile(8));
            timer.shared.panics_left.store(1, Ordering::SeqCst);
            timer
                .register(&Cancel::new(), Duration::from_secs(3600))
                .expect("register wakes the worker into its panic");
            wait_for("the one panic is spent", || {
                timer.shared.panics_left.load(Ordering::SeqCst) == 0
            });
            let after = Cancel::new();
            timer
                .register(&after, Duration::from_millis(40))
                .expect("register after the restart");
            wait_for("a deadline set after the restart fires", || {
                after.is_cancelled()
            });
            assert!(timer.is_running());
        }

        #[test]
        fn short_deadlines_registered_behind_a_long_one_all_fire_on_time() {
            let timer = Arc::new(DeadlineTimer::start().expect("start timer"));
            let parked = Cancel::new();
            timer
                .register(&parked, Duration::from_secs(3600))
                .expect("register");
            let slack = coverage_relaxed(Duration::from_millis(750));
            let handles: Vec<_> = (0..16)
                .map(|t| {
                    let timer = Arc::clone(&timer);
                    std::thread::spawn(move || {
                        for round in 0..25u64 {
                            std::thread::sleep(Duration::from_micros((t * 37 + round * 11) % 900));
                            let batch: Vec<_> = [20u64, 40, 80]
                                .into_iter()
                                .map(|ms| {
                                    let cancel = Cancel::new();
                                    let after = Duration::from_millis(ms);
                                    let at = Instant::now();
                                    timer.register(&cancel, after).expect("register");
                                    (cancel, after, at)
                                })
                                .collect();
                            for (cancel, after, at) in batch {
                                while !cancel.is_cancelled() {
                                    assert!(
                                        at.elapsed() < after + slack,
                                        "a {after:?} deadline had not fired after {:?}",
                                        at.elapsed()
                                    );
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("registering thread");
            }
            assert!(!parked.is_cancelled());
            assert_eq!(timer.pending(), 1, "only the parked deadline remains");
        }

        #[test]
        fn dropping_the_timer_stops_its_thread() {
            let timer = DeadlineTimer::start().expect("start timer");
            let cancel = Cancel::new();
            timer
                .register(&cancel, Duration::from_secs(3600))
                .expect("register");
            let shared: Weak<Shared> = Arc::downgrade(&timer.shared);
            drop(timer);
            let limit = Instant::now() + coverage_relaxed(Duration::from_secs(5));
            while shared.upgrade().is_some() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                shared.upgrade().is_none(),
                "the worker still holds the timer state after the timer dropped"
            );
        }

        #[test]
        fn a_full_timer_refuses_instead_of_leaving_a_query_untimed() {
            let timer = DeadlineTimer::start().expect("start timer");
            let cancel = Cancel::new();
            {
                let mut state = timer.shared.lock();
                for id in 0..MAX_PENDING as u64 {
                    state.due.insert(
                        (Instant::now() + Duration::from_secs(3600), id),
                        cancel.clone(),
                    );
                }
            }
            let err = timer
                .register(&Cancel::new(), Duration::from_secs(1))
                .expect_err("a full timer must refuse");
            assert!(err.contains("pending"), "{err}");
        }
    }
}

use deadline_timer::{DeadlineTimer, TimerKey};

/// The process-wide timer, started on first use and started again if its
/// thread ever stopped. An error means no deadline can be honoured right now.
fn shared_timer() -> Result<std::sync::Arc<DeadlineTimer>, String> {
    static TIMER: std::sync::Mutex<Option<std::sync::Arc<DeadlineTimer>>> =
        std::sync::Mutex::new(None);
    let mut slot = TIMER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    running_timer(&mut slot, DeadlineTimer::start)
}

/// The timer in `slot` if its thread is alive, else a freshly `start`ed one
/// placed there.
fn running_timer(
    slot: &mut Option<std::sync::Arc<DeadlineTimer>>,
    start: impl FnOnce() -> std::io::Result<DeadlineTimer>,
) -> Result<std::sync::Arc<DeadlineTimer>, String> {
    if let Some(timer) = slot.as_ref().filter(|timer| timer.is_running()) {
        return Ok(std::sync::Arc::clone(timer));
    }
    let timer = std::sync::Arc::new(
        start().map_err(|e| format!("could not start the deadline timer: {e}"))?,
    );
    *slot = Some(std::sync::Arc::clone(&timer));
    crate::supervisor::note_recovered(DEADLINE_SUBSYSTEM);
    // The replaced thread's own "stopped" record would otherwise outlive it.
    crate::supervisor::note_recovered(DEADLINE_THREAD);
    Ok(timer)
}

const DEADLINE_SUBSYSTEM: &str = "codeintel deadline";
/// The timer thread's name, which is also its supervisor record's subsystem.
const DEADLINE_THREAD: &str = "codeintel-cancel-deadline";

/// A registered query: its cancel flag and the deadline entry to withdraw
/// when it finishes.
struct Registered {
    cancel: Cancel,
    deadline: (std::sync::Arc<DeadlineTimer>, TimerKey),
    /// Another live query held this token when this one registered, so a
    /// `finish` by token cannot say which of them it belongs to.
    token_reused: bool,
}

impl Registered {
    /// Take the deadline back when the query is done. A reused token's
    /// deadline is left to expire instead: withdrawing it on the other
    /// query's finish would leave the one still running untimed.
    fn withdraw(&self) {
        if !self.token_reused {
            self.deadline.0.deregister(&self.deadline.1);
        }
    }
}

fn cancel_registry() -> &'static std::sync::Mutex<std::collections::HashMap<String, Registered>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Registered>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Register a cancel flag for `token`, also tripped after [`QUERY_CANCEL_DEADLINE`].
///
/// UI dismiss (file switch, unmount) calls [`cancel_query`] with the same token
/// so the walk stops rather than only having its answer ignored. If the
/// deadline cannot be registered the returned flag is already tripped and the
/// failure is logged: a query is never handed out untimed.
pub fn begin_cancellable_query(token: Option<&str>) -> Cancel {
    begin_cancellable_query_with(token, QUERY_CANCEL_DEADLINE, shared_timer)
}

fn begin_cancellable_query_with(
    token: Option<&str>,
    deadline: std::time::Duration,
    timer: impl FnOnce() -> Result<std::sync::Arc<DeadlineTimer>, String>,
) -> Cancel {
    let cancel = Cancel::new();
    let registration = timer().and_then(|timer| {
        let key = timer.register(&cancel, deadline)?;
        Ok((timer, key))
    });
    let deadline = match registration {
        Ok(deadline) => deadline,
        Err(reason) => {
            log::error!(target: "codeintel", "cancellable query refused, no deadline: {reason}");
            crate::supervisor::note_degraded(DEADLINE_SUBSYSTEM, &reason);
            cancel.cancel();
            return cancel;
        }
    };
    // A query without a token has no handle to finish by, so its entry waits
    // out its deadline; that is a map slot for 30 s, never a thread. Reusing a
    // live token replaces the registry entry; both queries keep their
    // deadlines (see `Registered::withdraw`).
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let mut map = cancel_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token_reused = map.contains_key(token);
        map.insert(
            token.to_string(),
            Registered {
                cancel: cancel.clone(),
                deadline,
                token_reused,
            },
        );
    }
    cancel
}

/// Trip a previously registered query cancel token (user dismiss / navigation).
pub fn cancel_query(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let removed = cancel_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(token);
    match removed {
        Some(registered) => {
            registered.cancel.cancel();
            registered.withdraw();
            true
        }
        None => false,
    }
}

/// Drop a finished token so the registry cannot grow with every request, and
/// withdraw its deadline so the timer holds nothing for a completed query.
pub fn finish_cancellable_query(token: Option<&str>) {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return;
    };
    let removed = cancel_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(token);
    if let Some(registered) = removed {
        registered.withdraw();
    }
}

/// An unavailable status, carrying the path we looked at and why we stopped.
fn status_unavailable(db_path: String, reason: String) -> CodeintelStatus {
    CodeintelStatus {
        available: false,
        is_fresh: None,
        freshness_reason: Some(reason.clone()),
        source_freshness: None,
        analyzer_freshness: None,
        pending_count: None,
        db_path,
        generation_id: None,
        total_files: None,
        total_symbols: None,
        total_edges: None,
        reason: Some(reason),
    }
}

/// Reports the availability and metrics of the repository's code intelligence graph.
pub fn status(repo_path: &str) -> CodeintelStatus {
    // A repository path we refused to resolve has no map path to report. The
    // empty string is what `HealthPanel` already renders as "unknown"; echoing
    // the joined candidate instead would print a relative
    // `.devcouncil/codeintel/devmap.sqlite` for `repo_path: ""` — the exact
    // resolve-against-our-own-cwd behaviour this gate exists to refuse.
    let repo = match resolve_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return status_unavailable(String::new(), e),
    };
    let db_str = map_path(&repo).to_string_lossy().into_owned();

    let store = match open_store(&repo) {
        Ok(s) => s,
        Err(e) => return status_unavailable(db_str, e),
    };

    // `Store::status` is devmap's own summary of a generation, and it answers
    // with `COUNT(*)` over the generation's primary-key range. This used to call
    // `latest_file_hashes()` and `latest_edges(0.0)` and immediately throw the
    // rows away through `.len()`, materializing the entire edge table — every
    // edge as four owned strings — to learn one integer. On this repository's own
    // 65 MB map (703 files, 53,622 edges) that cost 261 ms and ~29 MiB of peak
    // RSS on a call that runs on every `gitpulse_status` and every
    // `gitpulse_insights`.
    let summary = match store.status(&db_str) {
        Ok(summary) => summary,
        Err(e) => return status_unavailable(db_str, format!("Database error: {e}")),
    };
    let Some(gen_id) = summary.latest_generation else {
        return status_unavailable(db_str, "No generation indexed in database".into());
    };

    CodeintelStatus {
        available: true,
        is_fresh: Some(summary.is_fresh()),
        freshness_reason: summary.freshness_reason(),
        source_freshness: summary.source_freshness,
        analyzer_freshness: summary.analyzer_freshness,
        pending_count: Some(summary.pending_count),
        db_path: db_str,
        generation_id: Some(gen_id),
        total_files: latest_file_count(&store, gen_id),
        // Was hardcoded `None`, so the hook renderer's "N symbols across M
        // files" always said zero symbols and the health panel always showed a
        // dash. `Store::status` counts them in the same pass as the edges.
        total_symbols: u32::try_from(summary.node_count).ok(),
        total_edges: u32::try_from(summary.edge_count).ok(),
        reason: None,
    }
}

/// Files in generation `gen_id`, without reading the generation's file rows.
///
/// devmap has no `COUNT(*)` over `generation_files` on its public surface, but
/// it records the number in `build_history` inside the generation's own write
/// transaction — so for the generation that row describes, it *is* that count
/// (measured identical to `latest_file_hashes().len()` on the 65 MB map, at
/// 49 µs against 50 ms). The generation id is checked rather than assumed:
/// history rows are retained 500 deep while generations are pruned to 2, and a
/// database migrated up from before the table existed carries generations with
/// no history row at all. When the newest row describes some other generation,
/// the count is recomputed the slow, certain way rather than attributed to the
/// wrong build.
fn latest_file_count(store: &Store, gen_id: u32) -> Option<u32> {
    if let Ok(history) = store.build_history(1) {
        if let Some(row) = history.first().filter(|row| row.generation_id == gen_id) {
            return u32::try_from(row.files).ok();
        }
    }
    store
        .latest_file_hashes()
        .ok()
        .and_then(|files| u32::try_from(files.len()).ok())
}

/// Searches symbols using in-process code graph index.
pub fn search(
    repo_path: &str,
    query: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelSymbolHit> {
    if let Err(e) = require_argument("query", query) {
        return CodeintelResponse::unavailable(e);
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let req = Request {
        query: query.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.search(req) {
        Ok(res) => from_engine(res, |hit| CodeintelSymbolHit {
            symbol_name: hit.symbol_name,
            file_path: hit.file_path,
            kind: hit.kind,
            span_start_line: hit.span.0,
            span_end_line: hit.span.1,
            source_span: hit.source_span,
            source_unavailable_reason: hit.source_unavailable_reason,
            source_span_omitted_bytes: hit.source_span_omitted_bytes,
            score: hit.score,
        }),
        Err(e) => CodeintelResponse::unavailable(format!("Search failed: {e}")),
    }
}

/// Lists symbols declared in one file from the latest DevMap generation.
///
/// `head_sha` is the commit the caller intends to join against (typically that
/// worktree's `HEAD`). The page reports whether the indexed generation was
/// built at that SHA; a mismatch must not be treated as a verified span join.
///
/// Line ranges are computed from the blob at `generation_head_sha` when the
/// content hash matches, so dirty working-tree bytes never silently rebase
/// stored coordinates onto a different revision.
pub fn symbols_for_file(repo_path: &str, file_path: &str, head_sha: &str) -> CodeintelFileSymbols {
    if let Err(e) = require_argument("file_path", file_path) {
        return CodeintelFileSymbols::unavailable(file_path, head_sha, e);
    }
    if let Err(e) = require_argument("head_sha", head_sha) {
        return CodeintelFileSymbols::unavailable(file_path, head_sha, e);
    }
    if head_sha.chars().any(char::is_whitespace) || head_sha.len() > 128 {
        return CodeintelFileSymbols::unavailable(
            file_path,
            head_sha,
            "head_sha must be whitespace-free and at most 128 characters",
        );
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelFileSymbols::unavailable(file_path, head_sha, e),
    };
    let page = match store.latest_symbols_for_file(file_path) {
        Ok(Some(page)) => page,
        Ok(None) => {
            return CodeintelFileSymbols::unavailable(
                file_path,
                head_sha,
                "No generation indexed; the code map has not been built yet",
            );
        }
        Err(e) => {
            return CodeintelFileSymbols::unavailable(
                file_path,
                head_sha,
                format!("Failed to read symbols for file: {e}"),
            );
        }
    };

    let head_matches = page.head_sha == head_sha;
    let mut reason = None;
    if !head_matches {
        reason = Some(format!(
            "indexed generation head_sha {} is not the requested {}",
            page.head_sha, head_sha
        ));
    }

    let source = if page.rows.is_empty() {
        Some(String::new())
    } else {
        blob_for_line_spans(repo_path, file_path, &page.head_sha, &page.rows)
    };
    let total = page.rows.len() as u32;
    let truncated = page.rows.len() > MAX_FILE_SYMBOLS;
    let items: Vec<CodeintelFileSymbol> = page
        .rows
        .into_iter()
        .take(MAX_FILE_SYMBOLS)
        .map(|row| {
            let (span_start_line, span_end_line) = match &source {
                Some(text) => {
                    let span = devmap_extract::model::Span {
                        start_byte: row.span_start,
                        end_byte: row.span_end,
                    };
                    span.line_range(text)
                }
                None => (0, 0),
            };
            CodeintelFileSymbol {
                symbol_name: row.name,
                qualified_name: row.qualified_name,
                kind: row.kind,
                span_start_line,
                span_end_line,
            }
        })
        .collect();
    let shown = items.len() as u32;
    if source.is_none() && reason.is_none() {
        reason = Some(format!(
            "could not read blob for {file_path} at {} to convert byte spans to lines",
            page.head_sha
        ));
    }

    CodeintelFileSymbols {
        available: true,
        reason,
        file_path: file_path.to_string(),
        requested_head_sha: head_sha.to_string(),
        generation_id: Some(page.generation),
        generation_head_sha: Some(page.head_sha),
        head_matches,
        items,
        total,
        shown,
        truncated,
    }
}

/// Source bytes for line conversion, verified against the stored content hash.
///
/// Prefers the blob at `generation_head_sha`. Falls back to the working tree
/// when that revision cannot be read but the on-disk bytes still match the
/// indexed hash — the same identity check search uses — so a fixture (or a
/// dirty tree whose file was not rewritten) can still convert spans.
fn blob_for_line_spans(
    repo_path: &str,
    file_path: &str,
    generation_head_sha: &str,
    rows: &[devmap_store::StoredSymbol],
) -> Option<String> {
    let expected = rows.first().map(|row| row.content_hash)?;
    let matching = |commit: Option<&str>| -> Option<String> {
        let blob =
            crate::engine::git_reader::GitReader::get_file_blob_view(repo_path, file_path, commit)
                .ok()?;
        let text = blob.text?;
        if devmap_extract::content_hash(&text) != expected {
            return None;
        }
        Some(text)
    };
    matching(Some(generation_head_sha)).or_else(|| matching(None))
}

/// Computes blast radius / impact for a symbol or file.
pub fn impact(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    impact_at_rung(
        repo_path,
        target,
        token_budget,
        None,
        WalkBounds::default(),
        None,
    )
}

/// Impact narrowed to a named resolution rung.
pub fn impact_at_rung(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    bounds: WalkBounds,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    at_rung_query(
        AtRungQuery {
            repo_path,
            label: "target",
            query: target,
            token_budget,
            min_rung,
            bounds,
            cancel,
            failure: "Impact computation failed",
        },
        |engine, req, rung| engine.impact_at_rung(req, rung),
    )
}

/// Finds dependencies for a file.
pub fn dependencies(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    dependencies_at_rung(
        repo_path,
        file_path,
        token_budget,
        None,
        WalkBounds::default(),
        None,
    )
}

/// Dependencies narrowed to a named resolution rung.
pub fn dependencies_at_rung(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    bounds: WalkBounds,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    at_rung_query(
        AtRungQuery {
            repo_path,
            label: "file_path",
            query: file_path,
            token_budget,
            min_rung,
            bounds,
            cancel,
            failure: "Dependencies lookup failed",
        },
        |engine, req, rung| engine.dependencies_at_rung(req, rung),
    )
}

/// The arguments one rung-narrowed edge query shares: what is asked, of which
/// repository, how it may be cut off, and how a failure is worded.
struct AtRungQuery<'a> {
    repo_path: &'a str,
    /// Argument name quoted when `query` is blank.
    label: &'a str,
    query: &'a str,
    token_budget: Option<u32>,
    min_rung: Option<&'a str>,
    bounds: WalkBounds,
    cancel: Option<Cancel>,
    failure: &'a str,
}

/// Validates, opens the store, and runs one rung-narrowed edge query.
///
/// The single owner of the blank-argument, rung-parse, store-open, cancel and
/// request-shape steps for [`impact_at_rung`] and [`dependencies_at_rung`].
fn at_rung_query<E: std::fmt::Display>(
    q: AtRungQuery<'_>,
    run: impl FnOnce(
        &StoreQueryEngine<'_>,
        Request<String>,
        Option<Rung>,
    ) -> Result<Response<ResolvedEdge>, E>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument(q.label, q.query) {
        return CodeintelResponse::unavailable(e);
    }
    let rung = match parse_min_rung(q.min_rung) {
        Ok(r) => r,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let (min_confidence, max_depth) = match q.bounds.resolve() {
        Ok(b) => b,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let store = match open_repo_map(q.repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = q.cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: q.query.to_string(),
        token_budget: q.token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence,
        max_depth,
    };

    match run(&engine, req, rung) {
        Ok(res) => from_engine(res, map_edge),
        Err(e) => CodeintelResponse::unavailable(format!("{}: {e}", q.failure)),
    }
}

/// Identifies dead / unreferenced symbols.
pub fn dead_symbols(
    repo_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelDeadSymbol> {
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);

    match engine.dead_symbols(budget) {
        Ok(res) => from_engine(res, |dead| CodeintelDeadSymbol {
            symbol_name: dead.symbol_name,
            file_path: dead.file_path,
            confidence: dead.confidence,
            is_exempt: dead.is_exempt,
            exemption_reason: dead.exemption_reason,
        }),
        Err(e) => CodeintelResponse::unavailable(format!("Dead symbols analysis failed: {e}")),
    }
}

/// Traces path between two symbols.
pub fn trace_between(
    repo_path: &str,
    from: &str,
    to: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    trace_between_at_rung(
        repo_path,
        from,
        to,
        token_budget,
        None,
        WalkBounds::default(),
        None,
    )
}

/// Trace narrowed to a named resolution rung.
pub fn trace_between_at_rung(
    repo_path: &str,
    from: &str,
    to: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    bounds: WalkBounds,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("from", from).and_then(|()| require_argument("to", to)) {
        return CodeintelResponse::unavailable(e);
    }
    let rung = match parse_min_rung(min_rung) {
        Ok(r) => r,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let (min_confidence, max_depth) = match bounds.resolve() {
        Ok(b) => b,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: (from.to_string(), to.to_string()),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence,
        max_depth,
    };

    match engine.trace_between(req) {
        Ok(mut res) => {
            if rung.is_some() {
                let (kept, histogram) =
                    devmap_query::rung::narrow(std::mem::take(&mut res.items), rung);
                res.items = kept;
                res.rungs = Some(histogram);
            }
            from_engine(res, map_edge)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Trace between failed: {e}")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelNeighbors {
    pub target: String,
    pub callers: CodeintelResponse<CodeintelEdge>,
    pub callees: CodeintelResponse<CodeintelEdge>,
}

/// Deepest walk the MCP surface advertises for `depth`.
pub const MAX_WALK_DEPTH: usize = 64;

/// Walk depth used when the caller names none (the engine's long-standing cap).
const DEFAULT_WALK_DEPTH: usize = 10;

/// How far and how confidently a graph walk may reach.
///
/// `None` keeps the long-standing defaults (depth 10, confidence 0.0), which is
/// what the app commands pass. A value outside its range (`min_confidence` in
/// `[0, 1]`, `max_depth` in `1..=`[`MAX_WALK_DEPTH`]) is refused rather than
/// clamped, so a caller never reads a wider answer as the narrow one it asked
/// for. The one owner of that range check for every query that walks edges.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WalkBounds {
    pub max_depth: Option<usize>,
    pub min_confidence: Option<f32>,
}

impl WalkBounds {
    /// The `(min_confidence, max_depth)` the engine takes, or why they are invalid.
    fn resolve(self) -> Result<(f32, usize), String> {
        let min_confidence = self.min_confidence.unwrap_or(0.0);
        if !(0.0..=1.0).contains(&min_confidence) {
            return Err(format!(
                "min_confidence must be within [0, 1], got {min_confidence}"
            ));
        }
        let max_depth = self.max_depth.unwrap_or(DEFAULT_WALK_DEPTH);
        if !(1..=MAX_WALK_DEPTH).contains(&max_depth) {
            return Err(format!(
                "depth must be within 1..={MAX_WALK_DEPTH}, got {max_depth}"
            ));
        }
        Ok((min_confidence, max_depth))
    }
}

/// Callers and callees for one or more targets (chunked at [`MAX_NEIGHBOR_TARGETS`]).
///
/// `bounds` limits the walk; see [`WalkBounds`].
pub fn neighbors(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    bounds: WalkBounds,
) -> Result<Vec<CodeintelNeighbors>, String> {
    if targets.is_empty() {
        return Err("neighbors requires at least one target".into());
    }
    let (min_confidence, max_depth) = bounds.resolve()?;
    let rung = parse_min_rung(min_rung)?;
    let store = open_repo_map(repo_path)?;
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    let mut out = Vec::new();
    for chunk in targets.chunks(MAX_NEIGHBOR_TARGETS) {
        let report = engine
            .neighbors_at_rung(chunk, budget, min_confidence, max_depth, rung)
            .map_err(|e| format!("neighbors failed: {e}"))?;
        for entry in report {
            out.push(CodeintelNeighbors {
                target: entry.target,
                callers: from_engine(entry.callers, map_edge),
                callees: from_engine(entry.callees, map_edge),
            });
        }
    }
    Ok(out)
}

/// Wraps [`neighbors`] in the answer envelope every other query carries.
///
/// The bare `Vec` is what the app command and the frontend consume, but it
/// serialises to a JSON array: it cannot satisfy an object `outputSchema` and
/// has nowhere to say "truncated", "walk stopped at depth" or "freshness not
/// checked". Each target's own callers/callees keep their envelopes; this one
/// summarises them — `truncated` and `walk_incomplete` are raised when any
/// facet raised them, `source_freshness` is the weakest of them, and
/// `available` is false (with the first reason) when any facet could not
/// answer, so a refusal never renders as "no callers". A failed walk becomes
/// an unavailable envelope, as it does for every sibling query.
pub fn neighbors_envelope(
    result: Result<Vec<CodeintelNeighbors>, String>,
) -> CodeintelResponse<CodeintelNeighbors> {
    let items = match result {
        Ok(items) => items,
        Err(reason) => return CodeintelResponse::unavailable(reason),
    };
    let reason = items
        .iter()
        .flat_map(|entry| [&entry.callers, &entry.callees])
        .find(|facet| !facet.available)
        .map(|facet| {
            facet
                .reason
                .clone()
                .unwrap_or_else(|| "a facet of this answer was unavailable".to_string())
        });
    let truncated = items
        .iter()
        .any(|entry| entry.callers.truncated || entry.callees.truncated);
    let walk_incomplete = fold_qualification_reason(
        items.iter().flat_map(|entry| {
            [&entry.callers, &entry.callees]
                .into_iter()
                .filter_map(|facet| facet.walk_incomplete.clone())
        }),
        QUALIFICATION_MAX_CHARS,
    );
    let source_freshness = items
        .iter()
        .flat_map(|entry| [&entry.callers, &entry.callees])
        .map(|facet| facet.source_freshness.clone())
        .reduce(merge_source_freshness)
        .unwrap_or_else(CodeintelResponse::<CodeintelNeighbors>::unverified_freshness);
    let count = items.len() as u32;
    let mut out = CodeintelResponse::ok(items, count, count, truncated);
    out.available = reason.is_none();
    out.reason = reason;
    out.walk_incomplete = walk_incomplete;
    out.source_freshness = source_freshness;
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelBlastLayer {
    pub depth: usize,
    pub nodes: Vec<String>,
    pub node_count: u32,
    pub nodes_omitted: u32,
    pub lowest_confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelBlastRadius {
    pub seeds: Vec<String>,
    pub unmatched_targets: Vec<String>,
    pub layers: CodeintelResponse<CodeintelBlastLayer>,
    pub total_impacted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelLayeredImpact {
    pub available: bool,
    pub reason: Option<String>,
    pub edges: CodeintelResponse<CodeintelEdge>,
    pub blast_radius: CodeintelBlastRadius,
}

fn map_blast_radius(radius: devmap_query::BlastRadius) -> CodeintelBlastRadius {
    CodeintelBlastRadius {
        seeds: radius.seeds,
        unmatched_targets: radius.unmatched_targets,
        layers: from_engine(radius.layers, |layer| CodeintelBlastLayer {
            depth: layer.depth,
            nodes: layer.nodes,
            node_count: layer.node_count,
            nodes_omitted: layer.nodes_omitted,
            lowest_confidence: layer.lowest_confidence,
        }),
        total_impacted: radius.total_impacted,
    }
}

/// Layered impact for one target. Do not combine with `min_rung` — the kernel
/// refuses that pairing; the UI must not offer both.
pub fn impact_layered(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
) -> CodeintelLayeredImpact {
    impact_layered_with_cancel(repo_path, target, token_budget, None)
}

/// Layered impact with an optional cooperative cancel token.
pub fn impact_layered_with_cancel(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
    cancel: Option<Cancel>,
) -> CodeintelLayeredImpact {
    if let Err(e) = require_argument("target", target) {
        return CodeintelLayeredImpact {
            available: false,
            reason: Some(e),
            edges: CodeintelResponse::unavailable("target must not be blank"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no target"),
                total_impacted: 0,
            },
        };
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelLayeredImpact {
                available: false,
                reason: Some(e.clone()),
                edges: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![target.to_string()],
                    layers: CodeintelResponse::unavailable(e),
                    total_impacted: 0,
                },
            }
        }
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: target.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };
    match engine.impact_layered(req) {
        Ok(layered) => {
            let edges = from_engine(layered.edges, map_edge);
            let available = edges.available;
            let reason = edges.reason.clone();
            CodeintelLayeredImpact {
                available,
                reason,
                blast_radius: map_blast_radius(layered.blast_radius),
                edges,
            }
        }
        Err(e) => CodeintelLayeredImpact {
            available: false,
            reason: Some(format!("layered impact failed: {e}")),
            edges: CodeintelResponse::unavailable(format!("layered impact failed: {e}")),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: vec![target.to_string()],
                layers: CodeintelResponse::unavailable(format!("layered impact failed: {e}")),
                total_impacted: 0,
            },
        },
    }
}

/// Compose layered impact over a changed-file set. Each seed is one
/// `impact_layered` call, capped at [`MAX_NEIGHBOR_TARGETS`]. Omitted seeds
/// are unavailable with a distinct fan-out reason — not cancelled, not unmatched.
/// Cancellation is checked between walked targets so a dismissed pane cannot
/// finish the whole set.
pub fn impact_layered_many(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
) -> Vec<CodeintelLayeredImpact> {
    impact_layered_many_with_cancel(repo_path, targets, token_budget, None)
}

/// Layered-many with a shared cancel token (checked between walked targets).
pub fn impact_layered_many_with_cancel(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    cancel: Option<Cancel>,
) -> Vec<CodeintelLayeredImpact> {
    targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i >= MAX_NEIGHBOR_TARGETS {
                return omitted_layered_impact(t);
            }
            if let Some(cancel) = cancel.as_ref() {
                if cancel.is_cancelled() {
                    return cancelled_layered_impact(t);
                }
            }
            impact_layered_with_cancel(repo_path, t, token_budget, cancel.clone())
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelAffectedTest {
    pub path: String,
    pub depth: usize,
    pub symbols: Vec<String>,
    pub reached_symbols: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelAffectedTests {
    pub available: bool,
    pub reason: Option<String>,
    pub targets: Vec<String>,
    pub tests: CodeintelResponse<CodeintelAffectedTest>,
    pub blast_radius: CodeintelBlastRadius,
    /// True when the map was stale, an answer incomplete or truncated, or a seed unmatched —
    /// callers must fall back to the full suite and say why.
    pub fail_closed: bool,
    pub fail_closed_reason: Option<String>,
}

/// Affected test **files** for a set of seeds. Chunks at [`MAX_NEIGHBOR_TARGETS`].
pub fn affected_tests(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    bounds: WalkBounds,
) -> CodeintelAffectedTests {
    if targets.is_empty() {
        return CodeintelAffectedTests {
            available: false,
            reason: Some("affected_tests requires at least one target".into()),
            targets: Vec::new(),
            tests: CodeintelResponse::unavailable("no targets"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no targets"),
                total_impacted: 0,
            },
            fail_closed: true,
            fail_closed_reason: Some("no seeds provided".into()),
        };
    }
    let (min_confidence, max_depth) = match bounds.resolve() {
        Ok(b) => b,
        Err(e) => return affected_tests_unavailable(targets, e),
    };
    let repo = match resolve_repo(repo_path) {
        Ok(r) => r,
        Err(e) => {
            return CodeintelAffectedTests {
                available: false,
                reason: Some(e.clone()),
                targets: targets.to_vec(),
                tests: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: targets.to_vec(),
                    layers: CodeintelResponse::unavailable(e.clone()),
                    total_impacted: 0,
                },
                fail_closed: true,
                fail_closed_reason: Some(e),
            }
        }
    };
    let store = match open_indexed_store(&repo) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelAffectedTests {
                available: false,
                reason: Some(e.clone()),
                targets: targets.to_vec(),
                tests: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: targets.to_vec(),
                    layers: CodeintelResponse::unavailable(e.clone()),
                    total_impacted: 0,
                },
                fail_closed: true,
                fail_closed_reason: Some(e),
            }
        }
    };
    // A map with pending paths is not a complete picture of this checkout.
    // Querying it and treating the answer as "these are the tests to run"
    // is exactly the failure mode fail_closed exists to prevent.
    let db_str = map_path(&repo).to_string_lossy().into_owned();
    let freshness_failure = match store.status(&db_str) {
        Ok(summary) => summary
            .freshness_reason()
            .map(|reason| (true, format!("code map is stale or degraded: {reason}"))),
        Err(error) => Some((false, format!("code map freshness check failed: {error}"))),
    };
    if let Some((available, reason)) = freshness_failure {
        return CodeintelAffectedTests {
            available,
            reason: Some(reason.clone()),
            targets: targets.to_vec(),
            tests: CodeintelResponse::unavailable(reason.clone()),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: targets.to_vec(),
                layers: CodeintelResponse::unavailable(reason.clone()),
                total_impacted: 0,
            },
            fail_closed: true,
            fail_closed_reason: Some(reason),
        };
    }
    affected_tests_from_store(
        &store,
        targets,
        token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence,
        max_depth,
    )
}

/// A refused affected-tests query: nothing was selected, so the caller must
/// fall back to the full suite and is told why.
fn affected_tests_unavailable(targets: &[String], reason: String) -> CodeintelAffectedTests {
    CodeintelAffectedTests {
        available: false,
        reason: Some(reason.clone()),
        targets: targets.to_vec(),
        tests: CodeintelResponse::unavailable(reason.clone()),
        blast_radius: CodeintelBlastRadius {
            seeds: Vec::new(),
            unmatched_targets: targets.to_vec(),
            layers: CodeintelResponse::unavailable(reason.clone()),
            total_impacted: 0,
        },
        fail_closed: true,
        fail_closed_reason: Some(reason),
    }
}

fn affected_tests_from_store(
    store: &Store,
    targets: &[String],
    budget: u32,
    min_confidence: f32,
    depth: usize,
) -> CodeintelAffectedTests {
    let engine = StoreQueryEngine::new(store);
    let mut tests = None;
    let mut layers = None;
    let mut unmatched = Vec::new();
    let mut seeds = Vec::new();
    let mut total_impacted = 0u32;
    let chunks = targets.chunks(MAX_NEIGHBOR_TARGETS);
    let chunk_count = u32::try_from(chunks.len()).unwrap_or(u32::MAX).max(1);
    for (index, chunk) in chunks.enumerate() {
        // One caller budget across all batches, including the remainder.
        let chunk_budget =
            budget / chunk_count + u32::from(index < (budget % chunk_count) as usize);
        match engine.affected_tests(chunk, chunk_budget, min_confidence, depth) {
            Ok(report) => {
                unmatched.extend(report.blast_radius.unmatched_targets);
                seeds.extend(report.blast_radius.seeds);
                total_impacted = total_impacted.saturating_add(report.blast_radius.total_impacted);
                append_affected_response(
                    &mut tests,
                    from_engine(report.tests, |t| CodeintelAffectedTest {
                        path: t.path,
                        depth: t.depth,
                        symbols: t.symbols,
                        reached_symbols: t.reached_symbols,
                    }),
                );
                append_affected_response(
                    &mut layers,
                    from_engine(report.blast_radius.layers, |layer| CodeintelBlastLayer {
                        depth: layer.depth,
                        nodes: layer.nodes,
                        node_count: layer.node_count,
                        nodes_omitted: layer.nodes_omitted,
                        lowest_confidence: layer.lowest_confidence,
                    }),
                );
            }
            Err(e) => {
                return CodeintelAffectedTests {
                    available: false,
                    reason: Some(format!("affected_tests failed: {e}")),
                    targets: targets.to_vec(),
                    tests: CodeintelResponse::unavailable(format!("affected_tests failed: {e}")),
                    blast_radius: CodeintelBlastRadius {
                        seeds: Vec::new(),
                        unmatched_targets: targets.to_vec(),
                        layers: CodeintelResponse::unavailable(format!(
                            "affected_tests failed: {e}"
                        )),
                        total_impacted: 0,
                    },
                    fail_closed: true,
                    fail_closed_reason: Some(format!("affected_tests failed: {e}")),
                };
            }
        }
    }
    let mut tests = tests.unwrap_or_else(|| CodeintelResponse::unavailable("no target batches"));
    let mut layers = layers.unwrap_or_else(|| CodeintelResponse::unavailable("no target batches"));
    if chunk_count > 1 {
        // Batches can overlap and observe different generations. Do not claim
        // a unique, coherent union from their independently bounded answers.
        let scope = format!("combined {chunk_count} independently queried target batches; totals count occurrences across batches and may repeat symbols or files");
        append_affected_reason(&mut tests.walk_incomplete, Some(scope.clone()));
        append_affected_reason(&mut layers.walk_incomplete, Some(scope));
    }
    let available = tests.available && layers.available;
    let mut reason = tests.reason.clone();
    append_affected_reason(&mut reason, layers.reason.clone());
    let mut fail_closed_reason = reason.clone();
    append_affected_reason(&mut fail_closed_reason, tests.walk_incomplete.clone());
    append_affected_reason(&mut fail_closed_reason, layers.walk_incomplete.clone());
    if tests.truncated || layers.truncated {
        append_affected_reason(
            &mut fail_closed_reason,
            Some("query budget omitted affected tests or blast-radius layers".into()),
        );
    }
    if !unmatched.is_empty() {
        append_affected_reason(
            &mut fail_closed_reason,
            Some(format!(
                "{} seed(s) matched nothing in the map",
                unmatched.len()
            )),
        );
    }
    if !available && fail_closed_reason.is_none() {
        fail_closed_reason = Some("query unavailable".into());
    }
    CodeintelAffectedTests {
        available,
        reason,
        targets: targets.to_vec(),
        tests,
        blast_radius: CodeintelBlastRadius {
            seeds,
            unmatched_targets: unmatched,
            layers,
            total_impacted,
        },
        fail_closed: fail_closed_reason.is_some(),
        fail_closed_reason,
    }
}

const QUALIFICATION_MAX_CHARS: usize = 720;
const QUERY_CANCELLED_REASON: &str = "query cancelled";
/// Keep in sync with `MAX_PREVIEW_FILES` and TS `CODEINTEL_FANOUT_CAP`.
const FANOUT_OMITTED_REASON: &str = "layered-impact fan-out capped; this seed was not walked";

fn cancelled_layered_impact(target: &str) -> CodeintelLayeredImpact {
    CodeintelLayeredImpact {
        available: false,
        reason: Some(QUERY_CANCELLED_REASON.into()),
        edges: CodeintelResponse::unavailable(QUERY_CANCELLED_REASON),
        blast_radius: CodeintelBlastRadius {
            seeds: vec![target.to_string()],
            unmatched_targets: Vec::new(),
            layers: CodeintelResponse::unavailable(QUERY_CANCELLED_REASON),
            total_impacted: 0,
        },
    }
}

fn omitted_layered_impact(target: &str) -> CodeintelLayeredImpact {
    CodeintelLayeredImpact {
        available: false,
        reason: Some(FANOUT_OMITTED_REASON.into()),
        edges: CodeintelResponse::unavailable(FANOUT_OMITTED_REASON),
        blast_radius: CodeintelBlastRadius {
            seeds: vec![target.to_string()],
            unmatched_targets: Vec::new(),
            layers: CodeintelResponse::unavailable(FANOUT_OMITTED_REASON),
            total_impacted: 0,
        },
    }
}

fn append_affected_reason(target: &mut Option<String>, incoming: Option<String>) {
    *target = fold_qualification_reason(
        [target.take(), incoming].into_iter().flatten(),
        QUALIFICATION_MAX_CHARS,
    );
}

/// Fold walk_incomplete-style qualifications the way the UI composer does:
/// split on kernel joiners, fingerprint by stripping digits, merge numeric
/// slots into a range. Exact-string concat repeats the corpus disclaimer once
/// per seed whose unrecorded-edge count differs. Already-folded text (en-dash
/// ranges, "(N seeds)") is normalized so incremental appends keep merging.
fn fold_qualification_reason(
    parts: impl IntoIterator<Item = String>,
    max_chars: usize,
) -> Option<String> {
    let mut clauses = Vec::new();
    for part in parts {
        for chunk in part.split('·') {
            for clause in chunk.split("; ") {
                let item = clause.trim();
                if !item.is_empty() && item.chars().any(|c| c.is_alphanumeric()) {
                    clauses.push(item.to_string());
                }
            }
        }
    }
    if clauses.is_empty() {
        return None;
    }

    #[derive(Clone)]
    struct Slot {
        min: i64,
        max: i64,
    }
    struct Group {
        first: String,
        count: usize,
        template: String,
        slots: Vec<Slot>,
        mergeable: bool,
        identical: bool,
    }

    fn strip_suffix(clause: &str) -> &str {
        let trimmed = clause.trim();
        if let Some(at) = trimmed.rfind(" (") {
            let rest = &trimmed[at + 2..];
            if rest.ends_with(" seeds)") || rest.ends_with(" similar)") {
                return trimmed[..at].trim_end();
            }
        }
        trimmed
    }

    fn seed_weight(clause: &str) -> usize {
        const JS_SAFE: i64 = 9_007_199_254_740_991;
        let trimmed = clause.trim();
        let Some(at) = trimmed.rfind(" (") else {
            return 1;
        };
        let rest = &trimmed[at + 2..];
        let digits = rest
            .strip_suffix(" seeds)")
            .or_else(|| rest.strip_suffix(" similar)"));
        let Some(digits) = digits else {
            return 1;
        };
        digits
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0 && *n <= JS_SAFE)
            .map(|n| n as usize)
            .unwrap_or(1)
    }

    fn fingerprint(clause: &str) -> String {
        let mut out = String::new();
        let mut chars = strip_suffix(clause).chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_digit() {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&'\u{2013}') {
                    chars.next();
                    while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                        chars.next();
                    }
                }
                out.push('#');
            } else {
                out.push(c);
            }
        }
        out.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    }

    fn template(clause: &str) -> String {
        let mut out = String::new();
        let mut chars = strip_suffix(clause).chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_digit() {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&'\u{2013}') {
                    chars.next();
                    while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                        chars.next();
                    }
                }
                out.push('\0');
            } else {
                out.push(c);
            }
        }
        out
    }

    fn parse_slots(clause: &str) -> Option<Vec<Slot>> {
        const JS_SAFE: i64 = 9_007_199_254_740_991;
        fn parse_slot(digits: &str) -> Option<i64> {
            let n: i64 = digits.parse().ok()?;
            (-JS_SAFE..=JS_SAFE).contains(&n).then_some(n)
        }
        let mut out = Vec::new();
        let mut current = String::new();
        let mut chars = strip_suffix(clause).chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_digit() {
                current.push(c);
            } else if c == '\u{2013}' && !current.is_empty() {
                let min = parse_slot(&current)?;
                current.clear();
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    current.push(chars.next().unwrap());
                }
                let max = parse_slot(&current)?;
                current.clear();
                out.push(Slot {
                    min: min.min(max),
                    max: min.max(max),
                });
            } else if !current.is_empty() {
                let n = parse_slot(&current)?;
                current.clear();
                out.push(Slot { min: n, max: n });
                if c.is_ascii_digit() {
                    current.push(c);
                }
            }
        }
        if !current.is_empty() {
            let n = parse_slot(&current)?;
            out.push(Slot { min: n, max: n });
        }
        Some(out)
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for clause in clauses {
        let key = fingerprint(&clause);
        if let Some(&at) = index.get(&key) {
            let group = &mut groups[at];
            if clause == group.first || strip_suffix(&clause) == strip_suffix(&group.first) {
                group.count = group.count.max(seed_weight(&clause));
                continue;
            }
            group.count = group.count.saturating_add(seed_weight(&clause));
            group.identical = false;
            match parse_slots(&clause) {
                Some(slots)
                    if group.mergeable
                        && template(&clause) == group.template
                        && slots.len() == group.slots.len() =>
                {
                    for (i, slot) in slots.iter().enumerate() {
                        group.slots[i].min = group.slots[i].min.min(slot.min);
                        group.slots[i].max = group.slots[i].max.max(slot.max);
                    }
                }
                _ => group.mergeable = false,
            }
        } else {
            let slots = parse_slots(&clause);
            let mergeable = slots.is_some();
            index.insert(key, groups.len());
            groups.push(Group {
                first: clause.clone(),
                count: seed_weight(&clause),
                template: template(&clause),
                slots: slots.unwrap_or_default(),
                mergeable,
                identical: true,
            });
        }
    }

    let merged: Vec<String> = groups
        .into_iter()
        .map(|group| {
            if group.identical {
                let weight = seed_weight(&group.first);
                if group.count > 1 && weight != group.count {
                    return format!("{} ({} seeds)", strip_suffix(&group.first), group.count);
                }
                return group.first;
            }
            let base = strip_suffix(&group.first).to_string();
            if !group.mergeable {
                return if group.count > 1 {
                    format!("{base} ({} similar)", group.count)
                } else {
                    base
                };
            }
            let mut slot = 0;
            let mut out = String::new();
            let mut current = String::new();
            let mut chars = base.chars().peekable();
            while let Some(c) = chars.next() {
                if c.is_ascii_digit() {
                    current.push(c);
                } else if c == '\u{2013}' && !current.is_empty() {
                    while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                        chars.next();
                    }
                    current.clear();
                    let s = &group.slots[slot];
                    slot += 1;
                    if s.min == s.max {
                        out.push_str(&s.min.to_string());
                    } else {
                        out.push_str(&format!("{}\u{2013}{}", s.min, s.max));
                    }
                } else {
                    if !current.is_empty() {
                        let s = &group.slots[slot];
                        slot += 1;
                        if s.min == s.max {
                            out.push_str(&s.min.to_string());
                        } else {
                            out.push_str(&format!("{}\u{2013}{}", s.min, s.max));
                        }
                        current.clear();
                    }
                    out.push(c);
                }
            }
            if !current.is_empty() {
                let s = &group.slots[slot];
                if s.min == s.max {
                    out.push_str(&s.min.to_string());
                } else {
                    out.push_str(&format!("{}\u{2013}{}", s.min, s.max));
                }
            }
            if group.count > 1 {
                format!("{out} ({} seeds)", group.count)
            } else {
                out
            }
        })
        .collect();

    Some(bound_joined(&merged, max_chars.max(1)))
}

fn bound_joined(parts: &[String], max_chars: usize) -> String {
    let full = parts.join("; ");
    if full.chars().count() <= max_chars {
        return full;
    }
    let mut kept: Vec<&str> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let omitted = parts.len() - i - 1;
        let suffix = if omitted > 0 {
            format!("; {omitted} more distinct qualification(s) omitted")
        } else {
            String::new()
        };
        let candidate = if kept.is_empty() {
            part.clone()
        } else {
            format!("{}; {part}", kept.join("; "))
        };
        if candidate.chars().count() + suffix.chars().count() <= max_chars {
            kept.push(part);
            continue;
        }
        if kept.is_empty() {
            let room = max_chars.saturating_sub(suffix.chars().count()).max(1);
            return clip_text(&(clip_text(part, room) + &suffix), max_chars);
        }
        return clip_text(
            &format!(
                "{}; {} more distinct qualification(s) omitted",
                kept.join("; "),
                parts.len() - i
            ),
            max_chars,
        );
    }
    clip_text(&kept.join("; "), max_chars)
}

fn clip_text(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".into();
    }
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i >= max_chars - 1 {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// Both lists in an affected-test answer retain the same envelope semantics.
fn append_affected_response<T>(
    target: &mut Option<CodeintelResponse<T>>,
    incoming: CodeintelResponse<T>,
) {
    let Some(existing) = target else {
        *target = Some(incoming);
        return;
    };
    existing.available &= incoming.available;
    existing.total = existing.total.saturating_add(incoming.total);
    existing.shown = existing.shown.saturating_add(incoming.shown);
    existing.truncated |= incoming.truncated;
    existing.items.extend(incoming.items);
    append_affected_reason(&mut existing.reason, incoming.reason);
    append_affected_reason(&mut existing.walk_incomplete, incoming.walk_incomplete);
    existing.source_freshness =
        merge_source_freshness(existing.source_freshness.clone(), incoming.source_freshness);
    existing.rungs = match (existing.rungs.take(), incoming.rungs) {
        (Some(left), Some(right)) => Some(CodeintelRungHistogram {
            deterministic: left.deterministic.saturating_add(right.deterministic),
            high: left.high.saturating_add(right.high),
            speculative: left.speculative.saturating_add(right.speculative),
            filtered_out: left.filtered_out.saturating_add(right.filtered_out),
        }),
        _ => None,
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelExploreDefinition {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelExplore {
    pub available: bool,
    pub reason: Option<String>,
    pub definitions: CodeintelResponse<CodeintelExploreDefinition>,
    pub blast_radius: CodeintelBlastRadius,
    pub limit: u32,
}

/// Explore a symbol: definitions plus blast radius.
pub fn explore(
    repo_path: &str,
    query: &str,
    token_budget: Option<u32>,
    limit: Option<u32>,
    bounds: WalkBounds,
) -> CodeintelExplore {
    let (min_confidence, max_depth) = match bounds.resolve() {
        Ok(b) => b,
        Err(e) => {
            return CodeintelExplore {
                available: false,
                reason: Some(e.clone()),
                definitions: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![query.to_string()],
                    layers: CodeintelResponse::unavailable(e),
                    total_impacted: 0,
                },
                limit: 0,
            }
        }
    };
    if let Err(e) = require_argument("query", query) {
        return CodeintelExplore {
            available: false,
            reason: Some(e),
            definitions: CodeintelResponse::unavailable("query must not be blank"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no query"),
                total_impacted: 0,
            },
            limit: 0,
        };
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelExplore {
                available: false,
                reason: Some(e.clone()),
                definitions: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![query.to_string()],
                    layers: CodeintelResponse::unavailable(e),
                    total_impacted: 0,
                },
                limit: 0,
            }
        }
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    let limit = limit.unwrap_or(20) as usize;
    match engine.explore(query, limit, budget, min_confidence, max_depth) {
        Ok(report) => {
            let definitions = from_engine(report.definitions, |d| CodeintelExploreDefinition {
                symbol_name: d.symbol_name,
                file_path: d.file_path,
                kind: d.kind,
                id: d.id,
            });
            CodeintelExplore {
                available: definitions.available,
                reason: definitions.reason.clone(),
                blast_radius: map_blast_radius(report.blast_radius),
                definitions,
                limit: report.limit,
            }
        }
        Err(e) => CodeintelExplore {
            available: false,
            reason: Some(format!("explore failed: {e}")),
            definitions: CodeintelResponse::unavailable(format!("explore failed: {e}")),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: vec![query.to_string()],
                layers: CodeintelResponse::unavailable(format!("explore failed: {e}")),
                total_impacted: 0,
            },
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelCloneGroup {
    pub size: usize,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelClones {
    pub available: bool,
    pub reason: Option<String>,
    pub groups: CodeintelResponse<CodeintelCloneGroup>,
    pub signed_symbols: usize,
    pub unsigned_symbols: usize,
}

/// Duplicate-code groups with coverage honesty.
pub fn clones(repo_path: &str, token_budget: Option<u32>) -> CodeintelClones {
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelClones {
                available: false,
                reason: Some(e.clone()),
                groups: CodeintelResponse::unavailable(e),
                signed_symbols: 0,
                unsigned_symbols: 0,
            }
        }
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    match engine.clones(budget, None, 0) {
        Ok(report) => {
            let groups = from_engine(report.groups, |g| CodeintelCloneGroup {
                size: g.members.len(),
                members: g
                    .members
                    .into_iter()
                    .map(|m| format!("{}::{}", m.file_path, m.symbol_name))
                    .collect(),
            });
            CodeintelClones {
                available: groups.available,
                reason: groups.reason.clone(),
                groups,
                signed_symbols: report.signed_symbols,
                unsigned_symbols: report.unsigned_symbols,
            }
        }
        Err(e) => CodeintelClones {
            available: false,
            reason: Some(format!("clones failed: {e}")),
            groups: CodeintelResponse::unavailable(format!("clones failed: {e}")),
            signed_symbols: 0,
            unsigned_symbols: 0,
        },
    }
}

/// Speculative edit preview.
///
/// GitPulse links `devmap-query` with `default-features = false` so it never
/// pulls tree-sitter grammars into the desktop binary. Preview needs that
/// parse frontend; reporting unavailable (with a reason naming the CLI /
/// global `devmap mcp`) is honest. An empty breakage list must never mean
/// "the edit is safe" when the check could not run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelPreview {
    pub available: bool,
    pub reason: Option<String>,
    pub file_path: String,
    pub delta_available: bool,
}

pub fn preview(
    repo_path: &str,
    file: &str,
    content: &str,
    _token_budget: Option<u32>,
) -> CodeintelPreview {
    let _ = (repo_path, content);
    CodeintelPreview {
        available: false,
        reason: Some(
            "preview requires the parse frontend; gitpulse-mcp is parser-free. \
             Use `devmap preview` or the global `devmap mcp` server."
                .into(),
        ),
        file_path: file.to_string(),
        delta_available: false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;

    #[test]
    fn a_reader_refuses_a_store_bound_to_another_worktree() {
        let repo = repo_with_empty_store();
        let other = tempfile::tempdir().unwrap();
        let store = open_fixture_store(repo.path());
        store.bind_repo_root(other.path()).unwrap();
        drop(store);
        let error = open_store(repo.path())
            .err()
            .expect("cross-worktree store was accepted");
        assert!(error.contains("belongs to worktree"), "{error}");
    }

    #[test]
    fn supported_schema_matches_linked_store_constant() {
        assert_eq!(
            SUPPORTED_STORE_SCHEMA, CURRENT_SCHEMA_VERSION,
            "SUPPORTED_STORE_SCHEMA must track the linked devmap_store constant"
        );
        const {
            assert!(
                SUPPORTED_STORE_SCHEMA >= 19,
                "GitPulse must read schema 19+ maps"
            );
        }
    }

    #[test]
    fn schema_mismatch_reason_names_both_versions() {
        let friendly =
            rewrite_schema_mismatch("unsupported future schema version 19").expect("parse");
        assert!(friendly.contains("schema 19"), "{friendly}");
        assert!(
            friendly.contains(&format!("reads {SUPPORTED_STORE_SCHEMA}")),
            "{friendly}"
        );
    }

    #[test]
    fn devmap_db_path_follows_the_canonical_state_directory_precedence() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let standalone = repo.path().join(".devmap");
        let legacy = repo.path().join(".devcouncil");

        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            standalone.join("codeintel/devmap.sqlite"),
            "a fresh repository uses the standalone layout"
        );

        std::fs::create_dir_all(&legacy).expect("legacy state");
        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            legacy.join("codeintel/devmap.sqlite"),
            "a legacy-only repository remains readable"
        );

        std::fs::create_dir_all(&standalone).expect("standalone state");
        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            standalone.join("codeintel/devmap.sqlite"),
            "a migrated repository prefers the standalone layout"
        );
    }

    #[test]
    fn devmap_home_override_is_observed_in_an_isolated_process() {
        if let Some(expected) = std::env::var_os("GITPULSE_DEVMAP_HOME_CHILD") {
            assert_eq!(
                devmap_db_path("/ignored/repository"),
                PathBuf::from(expected).join("codeintel/devmap.sqlite")
            );
            return;
        }

        let home = tempfile::TempDir::new().expect("override");
        let (mut command, _harness) = crate::test_support::isolated_libtest_command(
            "codeintel::tests::devmap_home_override_is_observed_in_an_isolated_process",
        );
        let status = command
            .env("DEVMAP_HOME", home.path())
            .env("GITPULSE_DEVMAP_HOME_CHILD", home.path())
            .status_locked()
            .expect("run isolated test process");
        assert!(
            status.success(),
            "isolated DEVMAP_HOME resolver test failed"
        );
    }

    #[test]
    fn codeintel_status_on_nonexistent_repo() {
        let stat = status("/nonexistent/repo/path");
        assert!(!stat.available);
        assert!(stat.reason.is_some());
    }

    #[test]
    fn codeintel_search_on_nonexistent_repo() {
        let res = search("/nonexistent/repo/path", "test", None);
        assert!(!res.available);
        assert!(res.items.is_empty());
    }

    /// A real, empty Git work tree.
    ///
    /// Every entry point now goes through `validate_repo`, so a bare temporary
    /// directory is refused before the map is ever looked for — which is the
    /// point, but it means these fixtures have to be repositories to reach the
    /// behaviour under test. `git init` rather than a hand-made `.git`
    /// directory: the gate is the real one, so the fixture is too.
    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::test_support::trust_repo(dir.path());
        dir
    }

    /// A repository directory with a devmap database at the canonical path.
    ///
    /// `Store::open` creates the schema, so this yields a real, readable
    /// database that simply has no generation in it yet — the middle state
    /// between "no map" and "a map with answers".
    fn repo_with_empty_store() -> tempfile::TempDir {
        let dir = git_repo();
        drop(open_fixture_store(dir.path()));
        dir
    }

    /// Opens (creating and migrating) the map database for a fixture repository.
    fn open_fixture_store(repo: &Path) -> Store {
        let db = map_path(repo);
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir codeintel");
        Store::open(&db).expect("store opens and creates its schema")
    }

    /// The three unavailable states must be distinguishable.
    ///
    /// `available: false` is the same boolean for a repository that has no map,
    /// one whose map holds no generation, and one whose database cannot be
    /// read at all — but they call for three different actions (build a map,
    /// wait for indexing, repair a file). Only `reason` separates them, so a
    /// reason that went missing or collapsed to one string would leave the UI
    /// reporting "no code intelligence" for a corrupt database.
    #[test]
    fn the_three_unavailable_states_do_not_read_alike() {
        // A real repository that simply has no map. It must be a real one: a
        // path that does not exist is now refused by the path gate before the
        // map is ever looked for, which is a fourth state ("that is not a
        // repository") and not the one under test here.
        let unmapped = git_repo();
        let missing = status(&unmapped.path().to_string_lossy());
        assert!(!missing.available);
        let missing_reason = missing.reason.expect("missing map states a reason");
        assert!(
            missing_reason.contains("No devmap database"),
            "{missing_reason}"
        );

        let empty = repo_with_empty_store();
        let indexed = status(&empty.path().to_string_lossy());
        assert!(!indexed.available, "an empty store is not available");
        let indexed_reason = indexed.reason.expect("empty store states a reason");
        assert!(
            indexed_reason.contains("No generation indexed"),
            "{indexed_reason}"
        );
        assert_ne!(
            missing_reason, indexed_reason,
            "a repository with no map and one still indexing must not read alike"
        );

        // A file that exists at the database path but is not a database.
        let corrupt = git_repo();
        let db = map_path(corrupt.path());
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir");
        std::fs::write(&db, b"this is not a sqlite file").expect("write junk");
        let broken = status(&corrupt.path().to_string_lossy());
        assert!(!broken.available, "a corrupt database is not available");
        let broken_reason = broken.reason.expect("corrupt store states a reason");
        assert_ne!(
            broken_reason, indexed_reason,
            "a corrupt database must not read as one that is merely empty"
        );
    }

    /// A store carrying a trigger is refused at open, on every read surface.
    ///
    /// SQL stored in a map's schema runs on the first write by whoever opens
    /// it, and no devmap schema declares a trigger, so `devmap-store` refuses
    /// the file. The refusal has to reach the caller as a reason: an answer
    /// from such a store, or an empty one, would be a check that never ran
    /// reporting like one that found nothing. GitPulse itself never creates a
    /// trigger in a map; the only triggers it owns live in the task board's
    /// own database.
    #[test]
    fn a_map_carrying_a_trigger_is_refused_not_answered() {
        let repo = repo_with_empty_store();
        let root = repo.path().to_string_lossy().to_string();
        rusqlite::Connection::open(map_path(repo.path()))
            .expect("raw connection")
            .execute_batch(
                "CREATE TRIGGER planted AFTER INSERT ON generations BEGIN SELECT 1; END;",
            )
            .expect("plant a trigger");

        let status = status(&root);
        assert!(!status.available, "a trigger-bearing map is not available");
        let reason = status.reason.expect("the refusal states a reason");
        assert!(reason.contains("trigger"), "{reason}");
        let hits = search(&root, "anything", None);
        assert!(!hits.available, "search must not answer from it");
        assert!(hits.reason.unwrap_or_default().contains("trigger"));
    }

    /// Every read surface must refuse rather than answer emptily.
    ///
    /// An empty `items` list with `available: true` would say "this symbol has
    /// no callers" for a repository whose map was never built — the exact
    /// shape of a check that could not run reporting as one that ran and found
    /// nothing. Enumerated over all five surfaces so a newly added one that
    /// forgets the guard is not covered by nothing.
    #[test]
    fn no_read_surface_answers_emptily_when_there_is_no_map() {
        let repo = git_repo();
        let root = repo.path().to_string_lossy().to_string();

        let surfaces: Vec<(&str, bool, Option<String>)> = vec![
            {
                let r = search(&root, "anything", None);
                ("search", r.available, r.reason)
            },
            {
                let r = impact(&root, "anything", None);
                ("impact", r.available, r.reason)
            },
            {
                let r = dependencies(&root, "anything", None);
                ("dependencies", r.available, r.reason)
            },
            {
                let r = dead_symbols(&root, None);
                ("dead_symbols", r.available, r.reason)
            },
            {
                let r = trace_between(&root, "a", "b", None);
                ("trace_between", r.available, r.reason)
            },
            {
                let r = symbols_for_file(&root, "src/caller.rs", "abc");
                ("symbols_for_file", r.available, r.reason)
            },
        ];
        assert_eq!(surfaces.len(), 6, "all read surfaces must be covered");
        for (name, available, reason) in surfaces {
            assert!(!available, "{name} claimed availability with no map");
            let reason = reason.unwrap_or_else(|| panic!("{name} gave no reason"));
            assert!(
                reason.contains("No devmap database"),
                "{name} reason does not name the missing map: {reason}"
            );
        }
    }

    /// The same five surfaces against a database that opens but holds nothing.
    ///
    /// This is the state a repository sits in while its first index builds, and
    /// it must not be reported as a completed search that found no matches.
    #[test]
    fn no_read_surface_answers_emptily_for_a_store_without_a_generation() {
        let repo = repo_with_empty_store();
        let root = repo.path().to_string_lossy().to_string();

        assert!(!status(&root).available);
        let claiming: Vec<&str> = [
            ("search", search(&root, "anything", None).available),
            ("impact", impact(&root, "anything", None).available),
            (
                "dependencies",
                dependencies(&root, "anything", None).available,
            ),
            ("dead_symbols", dead_symbols(&root, None).available),
            (
                "trace_between",
                trace_between(&root, "a", "b", None).available,
            ),
            (
                "symbols_for_file",
                symbols_for_file(&root, "src/caller.rs", "abc").available,
            ),
        ]
        .into_iter()
        .filter_map(|(name, available)| available.then_some(name))
        .collect();
        assert!(
            claiming.is_empty(),
            "these surfaces reported availability for a store with no generation, \
             while status() reports the same repository as unavailable: {claiming:?}"
        );
    }

    /* ── A repository whose map holds one real generation ─────────────────── */

    /// The source of `src/caller.rs`, written to disk so one search hit has a
    /// readable span and the other does not.
    const CALLER_SOURCE: &str = "fn probe_caller() { probe_callee(); }\n";

    /// A repository with a built map: two connected files and one island.
    ///
    /// devmap's writer (`Store::save_generation`) sits behind the `parse`
    /// feature, which GitPulse turns off on purpose — it reads a map DevCouncil
    /// built and never indexes anything itself — so there is no library call
    /// here that can produce a populated generation. The rows go in directly,
    /// through the schema `Store::open` has just created and migrated. That is
    /// also what keeps the fixture honest: a schema change fails at the INSERT
    /// rather than leaving this quietly describing a shape the reader stopped
    /// reading.
    ///
    /// The graph is the smallest one that separates the answers under test:
    ///
    ///   * `src/caller.rs::probe_caller` calls `src/callee.rs::probe_callee`,
    ///     so both files have dependencies and both symbols are reachable.
    ///   * `src/island.rs` is indexed with no edge touching it at all — a file
    ///     that genuinely has no dependencies, as against one that is not in
    ///     the map.
    ///   * only `src/caller.rs` exists on disk, so one search hit can read its
    ///     source span and the other cannot.
    pub(crate) fn repo_with_one_generation() -> tempfile::TempDir {
        let dir = git_repo();
        let root = dir.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/caller.rs"), CALLER_SOURCE).expect("write caller");

        let store = open_fixture_store(dir.path());
        let db = map_path(dir.path());
        drop(store);

        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        let files = ["src/caller.rs", "src/callee.rs", "src/island.rs"];
        for (id, path) in (1i64..).zip(files) {
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .expect("insert path");
        }
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, 'fixture', ?1, ?2)",
            (
                // Only `latest_analysis` reads this column, and nothing on the
                // paths under test does — but a well-formed summary keeps the
                // row readable if something starts to.
                r#"{"total_files":3,"total_symbols":2,"total_edges":1,
                    "dead_symbols":[],"communities":[],"status":"Ok",
                    "unresolved_calls":0}"#,
                &root,
            ),
        )
        .expect("insert generation");

        for id in 1i64..=3 {
            // Since schema v17 `generation_files` is a view over
            // `file_payloads` + `generation_file_rows` — insert the base tables.
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', '\"ConfigScanner\"', 'null', 'v1', 'v1')",
                // Signed SQLite value emitted by the real devmap writer for
                // CALLER_SOURCE. The readable-source assertion below pins the
                // fixture bytes and this identity together.
                (
                    id,
                    if id == 1 {
                        -477_759_399_776_341_435i64
                    } else {
                        id
                    },
                ),
            )
            .expect("insert file payload");
            conn.execute(
                "INSERT INTO generation_file_rows (generation_id, file_id, payload_id)
                 VALUES (1, ?1, ?1)",
                [id],
            )
            .expect("insert generation file row");
        }

        // `search` reads nodes through the FTS index, whose rowid encodes the
        // generation and the node ordinal — see `Store::fts_rowid`.
        for (ordinal, (file_id, name)) in [(1i64, "probe_caller"), (2, "probe_callee")]
            .into_iter()
            .enumerate()
        {
            let ordinal = ordinal as i64;
            let span_end = if file_id == 1 {
                CALLER_SOURCE.len() as i64
            } else {
                20
            };
            conn.execute(
                "INSERT INTO generation_nodes
                   (generation_id, ordinal, file_id, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (1, ?1, ?2, ?3, ?3, 'Function', 0, ?4, 1)",
                (ordinal, file_id, name, span_end),
            )
            .expect("insert node");
            let fts_rowid = (1i64 << 32) | ordinal;
            conn.execute(
                "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
                 VALUES (?1, ?2, ?2, ?3)",
                (fts_rowid, name, files[(file_id - 1) as usize]),
            )
            .expect("insert fts row");
            conn.execute(
                "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, 1)",
                [fts_rowid],
            )
            .expect("insert fts map row");
        }

        // Since schema v18 `generation_edges` is a view over `edge_rows`
        // joined to generations by validity range.
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (0, 1, 2, 'probe_caller', 'probe_callee', 'Calls', 0.9, 1, NULL)",
            [],
        )
        .expect("insert edge");
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (1, 0.0, 'fixture', 3, 2, 1, 0, 0, 0, 1, NULL, 0)",
            [],
        )
        .expect("insert build history");
        conn.close().expect("close fixture db");
        dir
    }

    /// Puts an empty, fully migrated map database at `repo`'s store path.
    ///
    /// Creating the schema is the dominant write of a seeded fixture (about
    /// 2 MiB of pages per database), and the property tests in
    /// `insights::entity_collision` seed one per party per generated case: 484
    /// MiB for a single test, enough to trip the suite's write watchdog. The
    /// schema does not depend on the repository, so it is created once per test
    /// process and each fixture gets a clone of that file (`fs::copy` clones on
    /// APFS, and is a plain copy elsewhere; neither re-runs the migrations).
    fn install_empty_fixture_store(repo: &Path) {
        static TEMPLATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let template = TEMPLATE.get_or_init(|| {
            let dir = crate::test_support::env::hermetic::root().join("devmap-store-template");
            std::fs::create_dir_all(&dir).expect("mkdir store template");
            // Unique per process, so two test binaries never share one file.
            let path = dir.join(format!("{}.sqlite", std::process::id()));
            drop(Store::open(&path).expect("store opens and creates its schema"));
            for sidecar in ["-wal", "-shm"] {
                let mut name = path.clone().into_os_string();
                name.push(sidecar);
                assert!(
                    !Path::new(&name).exists(),
                    "the template must be a single checkpointed file"
                );
            }
            path
        });
        let db = map_path(repo);
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir codeintel");
        std::fs::copy(template, &db).expect("clone the template store");
        // `pending_state.epoch` is the store's identity, minted by `randomblob`
        // when the schema is created: a clone must not share its template's, or
        // two fixtures would acknowledge each other's claims. Same minting.
        let conn = rusqlite::Connection::open(&db).expect("open the cloned store");
        let changed = conn
            .execute(
                "UPDATE pending_state SET epoch = lower(hex(randomblob(16))) WHERE singleton = 1",
                [],
            )
            .expect("give the clone its own identity");
        assert_eq!(changed, 1, "the template store has one identity row");
        conn.close().expect("close the cloned store");
    }

    /// Adversarial review of `install_empty_fixture_store` (W5-L3). A store
    /// that one test writes to must not reach the template or a sibling clone.
    #[test]
    fn fixture_store_clones_do_not_share_writes() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        install_empty_fixture_store(a.path());
        install_empty_fixture_store(b.path());
        let (db_a, db_b) = (map_path(a.path()), map_path(b.path()));
        let conn = rusqlite::Connection::open(&db_a).unwrap();
        conn.execute(
            "INSERT INTO paths (id, path) VALUES (1, 'only-in-a.rs')",
            [],
        )
        .unwrap();
        drop(conn);
        let count = |db: &Path| -> i64 {
            rusqlite::Connection::open(db)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM paths", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count(&db_a), 1);
        assert_eq!(count(&db_b), 0, "a sibling clone saw another test's write");
        let c = tempfile::tempdir().unwrap();
        install_empty_fixture_store(c.path());
        assert_eq!(
            count(&map_path(c.path())),
            0,
            "the template was written through"
        );
        assert_ne!(
            std::fs::metadata(&db_a).unwrap().len(),
            0,
            "the clone is a real file"
        );
    }

    /// Each fixture store should be its own store: `pending_state.epoch` is the
    /// store identity ("a claim from one store must not acknowledge another's
    /// identically named path"), minted by `randomblob` at migration time. A
    /// clone of one template gives every fixture the SAME epoch, so a test
    /// that seeds two parties cannot tell them apart by identity.
    ///
    /// FAILS on the lane (known break, low severity: nothing in GitPulse reads
    /// the epoch of a fixture store today).
    #[test]
    fn fixture_stores_have_distinct_store_identities() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        install_empty_fixture_store(a.path());
        install_empty_fixture_store(b.path());
        let epoch = |repo: &Path| -> String {
            rusqlite::Connection::open(map_path(repo))
                .unwrap()
                .query_row(
                    "SELECT epoch FROM pending_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_ne!(
            epoch(a.path()),
            epoch(b.path()),
            "two fixtures share one store epoch"
        );
    }

    /// One indexed symbol for [`seed_file_symbols`]: `(name, qualified_name,
    /// kind, first_line, last_line)`, lines 1-based and inclusive.
    pub(crate) type SeedSymbol<'a> = (&'a str, &'a str, &'a str, u32, u32);

    /// Gives a repository whose HEAD already holds `text` at `file` a one-file
    /// map generation at `head_sha`, with the given symbols.
    ///
    /// GitPulse cannot write generations (the `parse` feature is off), so the
    /// rows go in by SQL exactly as [`repo_with_one_generation`] does. The
    /// content hash is the real `devmap_extract::content_hash` of `text`, which
    /// is what lets `symbols_for_file` convert byte spans to lines. Spans are
    /// derived from the line numbers: they start at the first line's first byte
    /// and end on the last line's last byte before its newline.
    pub(crate) fn seed_file_symbols(
        repo: &Path,
        head_sha: &str,
        file: &str,
        text: &str,
        symbols: &[SeedSymbol<'_>],
    ) {
        let mut line_starts = vec![0usize];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let line_end = |line: u32| {
            line_starts
                .get(line as usize)
                .map_or(text.len(), |next| next - 1)
        };
        let line_start = |line: u32| line_starts[(line - 1) as usize];

        install_empty_fixture_store(repo);
        let conn = rusqlite::Connection::open(map_path(repo)).expect("open fixture db");
        conn.execute("INSERT INTO paths (id, path) VALUES (1, ?1)", [file])
            .expect("insert path");
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                head_sha,
                r#"{"total_files":1,"total_symbols":0,"total_edges":0,
                    "dead_symbols":[],"communities":[],"status":"Ok",
                    "unresolved_calls":0}"#,
                repo.to_string_lossy().as_ref(),
            ),
        )
        .expect("insert generation");
        conn.execute(
            "INSERT INTO file_payloads
               (payload_id, file_id, content_hash, language,
                parse_outcome_json, engine_json, extraction_json,
                grammar_version, analyzer_version)
             VALUES (1, 1, ?1, 'rust', '\"Clean\"', '\"ConfigScanner\"', 'null', 'v1', 'v1')",
            [devmap_extract::content_hash(text) as i64],
        )
        .expect("insert file payload");
        conn.execute(
            "INSERT INTO generation_file_rows (generation_id, file_id, payload_id)
             VALUES (1, 1, 1)",
            [],
        )
        .expect("insert generation file row");
        for (ordinal, (name, qualified, kind, first, last)) in symbols.iter().enumerate() {
            conn.execute(
                "INSERT INTO generation_nodes
                   (generation_id, ordinal, file_id, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (1, ?1, 1, ?2, ?3, ?4, ?5, ?6, 1)",
                (
                    ordinal as i64,
                    name,
                    qualified,
                    kind,
                    line_start(*first) as i64,
                    line_end(*last) as i64,
                ),
            )
            .expect("insert node");
        }
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (1, 0.0, ?1, 1, ?2, 0, 0, 0, 0, 1, NULL, 0)",
            (head_sha, symbols.len() as i64),
        )
        .expect("insert build history");
        conn.close().expect("close fixture db");
    }

    /// Depth and confidence reach the affected-tests walk: the test file is two
    /// hops from `probe_island` over a 0.9 and a 0.3 edge.
    #[test]
    fn affected_tests_walk_honours_depth_and_min_confidence() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (1, 2, 3, 'probe_callee', 'probe_island', 'Calls', 0.3, 1, NULL)",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let targets = vec!["probe_island".to_owned()];
        let found = |min_confidence: f32, depth: usize| {
            affected_tests_from_store(&store, &targets, 2000, min_confidence, depth)
                .tests
                .items
                .len()
        };
        assert_eq!(found(0.0, 1), 0, "one hop does not reach the test file");
        assert_eq!(found(0.0, 2), 1, "two hops do");
        assert_eq!(found(0.5, 2), 0, "the 0.3 edge is below min_confidence");
    }

    /* ── Finding 1: a refusal must not render as an answer ────────────────── */

    #[test]
    fn affected_test_aggregation_keeps_budget_omissions_and_refusals() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let targets = vec!["probe_callee".to_owned()];
        let raw = StoreQueryEngine::new(&store)
            .affected_tests(&targets, 0, 0.0, 10)
            .unwrap();
        assert!(raw.tests.truncated);
        assert_eq!(raw.tests.total, 1);
        assert!(raw.blast_radius.layers.truncated);
        let report = affected_tests_from_store(&store, &targets, 0, 0.0, 10);
        assert_eq!(report.tests.total, 1);
        assert_eq!(report.tests.shown, 0);
        assert!(report.tests.truncated);
        assert_eq!(
            report.blast_radius.layers.total,
            raw.blast_radius.layers.total
        );
        assert!(report.blast_radius.layers.truncated);
        assert!(report.fail_closed);
        assert!(report.fail_closed_reason.is_some());

        let empty = Store::open_in_memory().unwrap();
        let refused = affected_tests_from_store(&empty, &targets, 2000, 0.0, 10);
        assert!(!refused.tests.available);
        assert!(!refused.blast_radius.layers.available);
        assert!(refused.fail_closed);
    }

    #[test]
    fn zero_depth_affected_results_keep_the_limit_and_fail_closed() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let report = affected_tests_from_store(&store, &["probe_callee".into()], 2000, 0.0, 0);
        assert!(report.tests.available);
        assert!(report.tests.items.is_empty());
        assert!(report.blast_radius.layers.items.is_empty());
        assert!(report
            .tests
            .walk_incomplete
            .as_deref()
            .unwrap()
            .contains("depth 0"));
        assert!(report.fail_closed);
        assert!(report
            .fail_closed_reason
            .as_deref()
            .unwrap()
            .contains("depth 0"));
    }

    #[test]
    fn affected_test_batches_share_budget_and_disclose_overlap() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let targets = vec!["probe_callee".to_owned(); MAX_NEIGHBOR_TARGETS + 1];
        let report = affected_tests_from_store(&store, &targets, 100, 0.0, 10);
        let engine = StoreQueryEngine::new(&store);
        let mut expected_shown = 0;
        let mut tokens = 0;
        for chunk in targets.chunks(MAX_NEIGHBOR_TARGETS) {
            let part = engine.affected_tests(chunk, 50, 0.0, 10).unwrap();
            tokens += part.tests.tokens_used + part.blast_radius.layers.tokens_used;
            expected_shown += part.tests.shown;
        }
        assert!(tokens <= 100);
        assert_eq!(report.tests.shown, expected_shown);
        assert_eq!(report.tests.total, 2);
        assert!(report
            .tests
            .walk_incomplete
            .as_deref()
            .unwrap()
            .contains("count occurrences"));
        assert!(report.blast_radius.layers.walk_incomplete.is_some());
        assert!(report.fail_closed);
    }

    #[test]
    fn affected_response_merge_keeps_independent_failure_metadata() {
        let mut first = CodeintelResponse::ok(vec![1], 2, 1, true);
        first.walk_incomplete = Some("parse loss".into());
        first.source_freshness = SourceFreshness::verified(true, 1);
        let mut second = CodeintelResponse::unavailable("index unavailable");
        second.walk_incomplete = Some("depth capped".into());
        second.source_freshness = SourceFreshness::verified(false, 1);
        let mut combined = Some(first);
        append_affected_response(&mut combined, second);
        let result = combined.unwrap();
        assert!(!result.available);
        assert_eq!(result.reason.as_deref(), Some("index unavailable"));
        assert_eq!(result.total, 2);
        assert_eq!(result.shown, 1);
        assert!(result.truncated);
        assert_eq!(result.source_freshness.fresh, Some(false));
        let reason = result.walk_incomplete.unwrap();
        assert!(reason.contains("parse loss") && reason.contains("depth capped"));
    }

    #[test]
    fn qualification_fold_collapses_numeric_walk_variants() {
        let coverage = "28637 of 69195 unresolved attribution site(s) have no indexed target after excluding 40548 known builtin, runtime-global, and external-import site(s); these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies";
        let mut folded = None;
        for i in 0..25u32 {
            let unrecorded = 700 + i * 500;
            append_affected_reason(
                &mut folded,
                Some(format!(
                    "the walk did not complete: stopped at depth 10, {unrecorded} traversed edges unrecorded; the result is a lower bound, not the full blast radius; {coverage}"
                )),
            );
        }
        let text = folded.expect("folded");
        assert!(
            text.len() < 1200,
            "folded walk_incomplete stayed huge: {} chars",
            text.len()
        );
        assert_eq!(text.matches("repository-wide counts").count(), 1);
        assert_eq!(text.matches("the walk did not complete").count(), 1);
        assert!(text.contains("25 seeds"), "{text}");
        assert!(text.contains('\u{2013}'), "{text}");
    }

    #[test]
    fn qualification_fold_keeps_merging_a_prior_summary() {
        let coverage = "these repository-wide counts are not specific to this target";
        let mut folded = None;
        for i in 0..10u32 {
            append_affected_reason(
                &mut folded,
                Some(format!(
                    "the walk did not complete: stopped at depth 10, {} traversed edges unrecorded; {coverage}",
                    100 + i
                )),
            );
        }
        append_affected_reason(
            &mut folded,
            Some(format!(
                "the walk did not complete: stopped at depth 10, 5000 traversed edges unrecorded; {coverage}"
            )),
        );
        let text = folded.expect("folded");
        assert!(text.contains("11 seeds"), "{text}");
        assert!(text.contains("5000") || text.contains('\u{2013}'), "{text}");
        assert_eq!(text.matches("repository-wide counts").count(), 1);
    }

    #[test]
    fn qualification_fold_does_not_suffix_identical_copies() {
        let mut folded = None;
        for _ in 0..179 {
            append_affected_reason(
                &mut folded,
                Some("repository-wide counts are not specific to this target".into()),
            );
        }
        let text = folded.expect("folded");
        assert_eq!(
            text,
            "repository-wide counts are not specific to this target"
        );
        assert!(!text.contains("seeds"));
    }

    #[test]
    fn qualification_fold_counts_similar_suffix_weight() {
        let mut folded = None;
        append_affected_reason(
            &mut folded,
            Some("overflow 9007199254740993 traversed".into()),
        );
        append_affected_reason(
            &mut folded,
            Some("overflow 9007199254740994 traversed".into()),
        );
        append_affected_reason(
            &mut folded,
            Some("overflow 9007199254740995 traversed".into()),
        );
        let text = folded.expect("folded");
        assert!(text.contains("3 similar"), "{text}");
        assert!(!text.contains('\u{2013}'), "{text}");
    }

    #[test]
    fn qualification_fold_never_exceeds_a_budget_smaller_than_the_omit_suffix() {
        let parts = (0..20u8)
            .map(|i| format!("unique{}", (b'A' + i) as char))
            .collect::<Vec<_>>();
        let text = fold_qualification_reason(parts, 25).expect("folded");
        assert!(
            text.chars().count() <= 25,
            "fold exceeded budget: {} chars ({text:?})",
            text.chars().count()
        );
    }

    #[test]
    fn qualification_fold_keeps_prior_seed_weight() {
        let text = fold_qualification_reason(
            ["depth capped".into(), "depth capped (10 seeds)".into()],
            720,
        )
        .expect("folded");
        assert!(text.contains("10 seeds"), "{text}");
        assert_eq!(text.matches("depth capped").count(), 1, "{text}");
    }

    #[test]
    fn qualification_fold_does_not_double_a_summary() {
        let coverage = "these repository-wide counts are not specific to this target";
        let mut folded = None;
        for i in 0..8u32 {
            append_affected_reason(
                &mut folded,
                Some(format!(
                    "the walk did not complete: stopped at depth 10, {} traversed edges unrecorded; {coverage}",
                    80 + i
                )),
            );
        }
        let first = folded.clone().expect("folded");
        assert!(first.contains("8 seeds"), "{first}");
        append_affected_reason(&mut folded, Some(first.clone()));
        let twice = folded.expect("twice");
        assert_eq!(twice, first);
        assert!(twice.contains("8 seeds"), "{twice}");
        assert!(!twice.contains("16 seeds"), "{twice}");
    }

    #[test]
    fn clip_text_cuts_on_a_unicode_scalar_boundary() {
        let text = clip_text(&format!("😀{}", "x".repeat(20)), 3);
        assert!(text.chars().count() <= 3);
        assert!(text.starts_with('😀'), "{text}");
        assert!(text.ends_with('…'), "{text}");
    }

    #[test]
    fn layered_many_caps_fanout_and_reports_omitted_seeds() {
        let targets: Vec<String> = (0..MAX_NEIGHBOR_TARGETS + 4)
            .map(|i| format!("f{i}.ts"))
            .collect();
        let out = impact_layered_many("/tmp/not-a-repo", &targets, Some(50));
        assert_eq!(out.len(), targets.len());
        let omitted = out
            .iter()
            .filter(|item| item.reason.as_deref() == Some(FANOUT_OMITTED_REASON))
            .count();
        assert_eq!(
            omitted,
            4,
            "{:?}",
            out.iter().map(|i| &i.reason).collect::<Vec<_>>()
        );
        for item in &out[MAX_NEIGHBOR_TARGETS..] {
            assert_eq!(item.reason.as_deref(), Some(FANOUT_OMITTED_REASON));
            assert!(
                item.blast_radius.unmatched_targets.is_empty(),
                "{:?}",
                item.blast_radius.unmatched_targets
            );
        }
    }

    #[test]
    fn cancelled_layered_many_does_not_mark_seeds_unmatched() {
        let cancel = Cancel::new();
        cancel.cancel();
        let out = impact_layered_many_with_cancel(
            "/tmp/not-a-repo",
            &["a.ts".into(), "b.ts".into()],
            Some(50),
            Some(cancel),
        );
        assert_eq!(out.len(), 2);
        for item in &out {
            assert!(!item.available);
            assert_eq!(item.reason.as_deref(), Some(QUERY_CANCELLED_REASON));
            assert!(
                item.blast_radius.unmatched_targets.is_empty(),
                "{:?}",
                item.blast_radius.unmatched_targets
            );
            assert_eq!(item.blast_radius.seeds.len(), 1);
        }
    }

    #[test]
    fn from_engine_folds_concatenated_walk_incomplete() {
        let store = Store::open_in_memory().unwrap();
        let coverage = "28637 of 69195 unresolved attribution site(s) have no indexed target after excluding 40548 known builtin, runtime-global, and external-import site(s); these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies";
        let blob = (0..25u32)
            .map(|i| {
                let unrecorded = 700 + i * 500;
                format!(
                    "the walk did not complete: stopped at depth 10, {unrecorded} traversed edges unrecorded; the result is a lower bound, not the full blast radius; {coverage}"
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let mut response = StoreQueryEngine::new(&store)
            .impact(Request {
                query: "missing".into(),
                token_budget: 2000,
                min_confidence: 0.0,
                max_depth: 3,
            })
            .unwrap();
        response.walk_incomplete = Some(blob);
        let mapped = from_engine(response, |edge| edge.source_symbol);
        let text = mapped.walk_incomplete.expect("folded");
        assert!(
            text.len() < 1200,
            "from_engine left walk_incomplete huge: {} chars",
            text.len()
        );
        assert_eq!(text.matches("repository-wide counts").count(), 1);
        assert!(text.contains("25 seeds"), "{text}");
    }

    #[test]
    fn unavailable_answers_preserve_coverage_and_resolution_metadata() {
        let store = Store::open_in_memory().unwrap();
        let mut response = StoreQueryEngine::new(&store)
            .impact(Request {
                query: "missing".into(),
                token_budget: 2000,
                min_confidence: 0.0,
                max_depth: 3,
            })
            .unwrap();
        response.walk_incomplete = Some("repository-wide attribution coverage is unknown".into());
        response.source_freshness = SourceFreshness::verified(false, 1);
        response.rungs = Some(RungHistogram {
            deterministic: 2,
            high: 1,
            speculative: 3,
            filtered_out: 4,
        });
        let mapped = from_engine(response, |edge| edge.source_symbol);
        assert!(!mapped.available);
        assert_eq!(mapped.source_freshness.fresh, Some(false));
        assert_eq!(
            mapped.walk_incomplete.as_deref(),
            Some("repository-wide attribution coverage is unknown")
        );
        assert_eq!(mapped.rungs.unwrap().filtered_out, 4);
    }

    /// The defect this whole module's honesty rests on.
    ///
    /// `dependencies` answers `Unavailable { reason }` for a path the map does
    /// not contain, and an ordinary empty `Available` response for a file that
    /// is indexed and genuinely depends on nothing. Both used to leave here as
    /// `available: true, reason: null, total: 0, items: []` — byte for byte
    /// identical — because every call site funnelled the engine's response
    /// through `CodeintelResponse::ok`, which hardcodes availability. This
    /// repository's own instructions tell an agent to check its blast radius
    /// before editing and to trust the result, so a typo'd or newly added
    /// symbol read as "nothing depends on this, safe to change".
    #[test]
    fn an_unresolvable_target_and_a_target_with_genuinely_no_edges_do_not_read_alike() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let island = dependencies(&root, "src/island.rs", None);
        let absent = dependencies(&root, "src/not/in/the/map.rs", None);

        assert!(
            island.available,
            "an indexed file with no edges is a completed lookup: {:?}",
            island.reason
        );
        assert_eq!(island.reason, None);
        assert_eq!(island.total, 0);
        assert!(island.items.is_empty());

        assert!(
            !absent.available,
            "a file the map does not contain must not report as a clean empty result"
        );
        let reason = absent.reason.clone().expect("a refusal states its reason");
        assert!(
            reason.contains("src/not/in/the/map.rs") && reason.contains("not indexed"),
            "the engine's own reason did not survive the boundary: {reason}"
        );
        assert_ne!(
            (island.available, island.reason),
            (absent.available, absent.reason),
            "a lookup that could not run is indistinguishable from one that ran and found nothing"
        );
    }

    /// Each engine refusal, on each surface that can produce one.
    ///
    /// The engine has four distinct ways to say "I could not answer that" and
    /// they arrive on three different surfaces. Enumerated rather than sampled,
    /// so a surface whose mapping regresses to `CodeintelResponse::ok` is not
    /// covered by nothing — and each reason is required to name what was asked,
    /// which is what proves the engine's own words came through rather than a
    /// generic refusal invented here.
    #[test]
    fn every_engine_refusal_reaches_the_caller_with_the_engine_s_own_reason() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let cases: Vec<(&str, bool, Option<String>, &str)> = vec![
            {
                let r = impact(&root, "no_such_symbol_12345", None);
                ("impact", r.available, r.reason, "no_such_symbol_12345")
            },
            {
                let r = dependencies(&root, "src/nowhere.rs", None);
                ("dependencies", r.available, r.reason, "src/nowhere.rs")
            },
            {
                let r = trace_between(&root, "probe_caller", "no_such_symbol_12345", None);
                (
                    "trace_between",
                    r.available,
                    r.reason,
                    "no_such_symbol_12345",
                )
            },
        ];
        for (surface, available, reason, needle) in cases {
            assert!(
                !available,
                "{surface} reported an unresolvable target as an answer"
            );
            let reason = reason.unwrap_or_else(|| panic!("{surface} refused without a reason"));
            assert!(
                reason.contains(needle),
                "{surface} refusal does not name what was asked: {reason}"
            );
        }
    }

    /// A resolvable target still answers, so the guard did not simply refuse
    /// everything.
    ///
    /// A fix that turned every response into `available: false` would satisfy
    /// the tests above and destroy the module. These are the same three
    /// surfaces on inputs the map can resolve.
    #[test]
    fn a_resolvable_target_still_answers_after_refusals_are_carried_through() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let blast = impact(&root, "probe_callee", None);
        assert!(blast.available, "impact refused: {:?}", blast.reason);
        assert_eq!(blast.total, 1, "the one caller in the fixture");
        assert_eq!(blast.items[0].source_symbol, "probe_caller");

        let deps = dependencies(&root, "src/caller.rs", None);
        assert!(deps.available, "dependencies refused: {:?}", deps.reason);
        assert_eq!(deps.total, 1);

        let path = trace_between(&root, "probe_caller", "probe_callee", None);
        assert!(path.available, "trace refused: {:?}", path.reason);
        assert_eq!(path.total, 1);
    }

    /// A blank query is a caller mistake, not a search that found nothing.
    ///
    /// The engine short-circuits a whitespace-only query to an empty
    /// *Available* response, which renders as "no symbol matches that" for a
    /// caller who asked nothing at all — the same collapse as an unresolvable
    /// target, arriving through a different door.
    #[test]
    fn a_blank_query_argument_is_refused_rather_than_answered_as_no_matches() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let blank = search(&root, "   ", None);
        assert!(
            !blank.available,
            "a blank query reported as a completed search"
        );
        assert!(blank
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("query")));

        let real = search(&root, "probe", None);
        assert!(real.available, "search refused: {:?}", real.reason);
        assert_eq!(real.total, 2, "both fixture symbols match the prefix");

        // The other three argument-taking surfaces, so a blank argument is not
        // reported as a fact about the repository on any of them.
        for (surface, available) in [
            ("impact", impact(&root, "  ", None).available),
            ("dependencies", dependencies(&root, "", None).available),
            (
                "trace_from",
                trace_between(&root, "", "probe_callee", None).available,
            ),
            (
                "trace_to",
                trace_between(&root, "probe_caller", " ", None).available,
            ),
        ] {
            assert!(!available, "{surface} answered a blank argument");
        }
    }

    /// A hit whose source could not be read must say so.
    ///
    /// `SymbolHit::source_unavailable_reason` is computed by the engine for
    /// exactly the case where the map names a file the working tree no longer
    /// has, and this module dropped it — so a hit whose source could not be
    /// read rendered as one whose source is genuinely empty. The fixture writes
    /// only one of its two files to disk, so both cases appear in a single
    /// response.
    #[test]
    fn a_hit_whose_source_could_not_be_read_says_so_instead_of_showing_a_blank_span() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let hits = search(&root, "probe", None);
        assert!(hits.available, "search refused: {:?}", hits.reason);

        let readable = hits
            .items
            .iter()
            .find(|hit| hit.file_path == "src/caller.rs")
            .expect("the file that exists on disk");
        assert_eq!(readable.source_unavailable_reason, None);
        assert_eq!(readable.source_span, CALLER_SOURCE);

        let missing = hits
            .items
            .iter()
            .find(|hit| hit.file_path == "src/callee.rs")
            .expect("the file that does not exist on disk");
        assert!(missing.source_span.is_empty());
        let reason = missing
            .source_unavailable_reason
            .as_deref()
            .expect("an unreadable source states why");
        assert!(
            reason.contains("src/callee.rs"),
            "the reason does not name the file it is about: {reason}"
        );
    }

    /// Symbols-in-file is the span read collision and entity-diff both need.
    /// Search already carries spans on hits, but nothing listed every symbol
    /// in one file — and nothing said when the generation was not the SHA
    /// the caller asked for.
    #[test]
    fn symbols_for_file_lists_spans_and_reports_a_head_sha_mismatch() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let matched = symbols_for_file(&root, "src/caller.rs", "fixture");
        assert!(matched.available, "{:?}", matched.reason);
        assert!(matched.head_matches, "fixture is the generation head_sha");
        assert_eq!(matched.generation_head_sha.as_deref(), Some("fixture"));
        assert_eq!(matched.items.len(), 1);
        assert_eq!(matched.items[0].symbol_name, "probe_caller");
        assert_eq!(matched.items[0].span_start_line, 1);
        assert!(
            matched.items[0].span_end_line >= 1,
            "expected a real line span, got {:?}",
            matched.items[0]
        );

        let mismatched = symbols_for_file(&root, "src/caller.rs", "deadbeefdeadbeef");
        assert!(mismatched.available, "mismatch is still a completed read");
        assert!(
            !mismatched.head_matches,
            "a different SHA must not claim head_matches"
        );
        let reason = mismatched.reason.expect("mismatch states why");
        assert!(
            reason.contains("deadbeefdeadbeef") && reason.contains("fixture"),
            "reason must name both SHAs: {reason}"
        );
        // Spans remain usable for display, but callers must check head_matches
        // before joining them to a working-tree diff.
        assert_eq!(mismatched.items.len(), 1);
    }

    #[test]
    fn symbols_for_file_blank_arguments_are_refused() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();
        assert!(!symbols_for_file(&root, "  ", "fixture").available);
        assert!(!symbols_for_file(&root, "src/caller.rs", "").available);
    }

    #[test]
    fn edited_source_is_withheld_without_losing_the_stored_hit() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();
        std::fs::write(repo.path().join("src/caller.rs"), "fn renamed() {}\n").unwrap();
        let hits = search(&root, "probe_caller", None);
        assert!(hits.available, "{:?}", hits.reason);
        let hit = hits
            .items
            .first()
            .expect("the indexed symbol remains visible");
        assert_eq!(hit.symbol_name, "probe_caller");
        assert!(hit.source_span.is_empty());
        assert!(hit
            .source_unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("changed")));
    }

    #[test]
    fn staleness_audit_query_envelopes_preserve_unknown_freshness() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let responses = [
            serde_json::to_value(search(root, "probe", None)).unwrap(),
            serde_json::to_value(dependencies(root, "src/caller.rs", None)).unwrap(),
            serde_json::to_value(impact(root, "probe_callee", None)).unwrap(),
            serde_json::to_value(trace_between(root, "probe_caller", "probe_callee", None))
                .unwrap(),
            serde_json::to_value(search(root, "missing", None)).unwrap(),
            serde_json::to_value(search(root, " ", None)).unwrap(),
        ];
        for response in responses {
            let freshness = response
                .get("source_freshness")
                .expect("query envelopes must carry source_freshness");
            assert!(
                freshness.is_object(),
                "source_freshness must be a structured object: {response}"
            );
            assert_eq!(
                freshness.get("fresh"),
                Some(&serde_json::Value::Null),
                "query availability does not verify source freshness: {response}"
            );
            assert!(
                freshness.get("reason").and_then(|v| v.as_str()).is_some(),
                "unverified freshness must carry a reason: {response}"
            );
        }
    }

    #[test]
    fn staleness_audit_adapter_preserves_each_engine_freshness_verdict() {
        let repo = repo_with_one_generation();
        let store = open_repo_map(repo.path().to_str().unwrap()).unwrap();
        let engine = StoreQueryEngine::new(&store);
        for verdict in [
            SourceFreshness::unverified("not checked"),
            SourceFreshness::verified(false, 1),
            SourceFreshness::verified(true, 1),
        ] {
            let mut response = engine
                .search(Request {
                    query: "probe_caller".into(),
                    token_budget: 2000,
                    min_confidence: 0.0,
                    max_depth: 1,
                })
                .unwrap();
            response.source_freshness = verdict.clone();
            let mapped = from_engine(response, |hit| hit.symbol_name);
            assert_eq!(mapped.source_freshness, verdict);
            assert_eq!(mapped.items, vec!["probe_caller"]);
        }
        let missing = git_repo();
        let unavailable = status(missing.path().to_str().unwrap());
        assert!(!unavailable.available);
        assert_eq!(unavailable.is_fresh, None);
        assert_eq!(unavailable.pending_count, None);
        assert!(unavailable.freshness_reason.is_some());
    }

    #[test]
    fn staleness_audit_budgeted_source_reports_omitted_bytes() {
        let repo = repo_with_one_generation();
        let response = serde_json::to_value(search(
            repo.path().to_str().unwrap(),
            "probe_caller",
            Some(80),
        ))
        .unwrap();
        assert_eq!(response["shown"], 1);
        let hit = &response["items"][0];
        assert_eq!(hit["source_unavailable_reason"], serde_json::Value::Null);
        assert_eq!(
            hit["source_span"].as_str().unwrap().len()
                + hit["source_span_omitted_bytes"]
                    .as_u64()
                    .expect("budget omission must be preserved") as usize,
            CALLER_SOURCE.len()
        );
    }

    fn freshness_audit_matching_source_fixture() -> tempfile::TempDir {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute("DELETE FROM generation_file_rows WHERE file_id <> 1", [])
            .unwrap();
        repo
    }

    /// Restamps every payload with the identity this build computes, so the
    /// fixture describes a store the current analyzer wrote.
    fn stamp_current_payload_identity(repo: &std::path::Path) {
        let (grammar, analyzer) = devmap_extract::cache::current_payload_identity("rust");
        let conn = rusqlite::Connection::open(map_path(repo)).unwrap();
        conn.execute(
            "UPDATE file_payloads SET grammar_version = ?1, analyzer_version = ?2",
            (&grammar, &analyzer),
        )
        .unwrap();
    }

    #[test]
    fn freshness_audit_matching_bytes_do_not_certify_an_obsolete_analyzer() {
        // The fixture's payloads carry `v1` stamps no analyzer ever wrote.
        // Matching source bytes must not carry them to `is_fresh`.
        let repo = freshness_audit_matching_source_fixture();
        let response = status(repo.path().to_str().unwrap());
        assert_eq!(response.source_freshness, Some(true), "{response:?}");
        assert_eq!(response.analyzer_freshness, Some(false));
        assert_eq!(response.is_fresh, Some(false));
        assert!(response
            .freshness_reason
            .unwrap()
            .contains("stored extraction payload is obsolete"));
    }

    /// GitPulse links devmap without the parsing frontend. Until the vendored
    /// store could compute a payload identity in that configuration, no store
    /// was ever fresh here: analyzer freshness was always unverified, so
    /// `is_fresh` could not be true even for a store the current analyzer
    /// had just written.
    #[test]
    fn freshness_audit_a_current_analyzer_identity_is_certified_without_grammars() {
        let repo = freshness_audit_matching_source_fixture();
        stamp_current_payload_identity(repo.path());
        let response = status(repo.path().to_str().unwrap());
        assert_eq!(response.source_freshness, Some(true), "{response:?}");
        assert_eq!(response.analyzer_freshness, Some(true), "{response:?}");
        assert_eq!(response.is_fresh, Some(true), "{response:?}");
    }

    #[test]
    fn freshness_audit_pending_and_absent_generation_are_unverified() {
        let repo = freshness_audit_matching_source_fixture();
        let store = Store::open(map_path(repo.path())).unwrap();
        store
            .enqueue_pending_paths(&["src/caller.rs".to_string()])
            .unwrap();
        let response = status(repo.path().to_str().unwrap());
        assert_eq!(response.source_freshness, None);
        assert_eq!(response.analyzer_freshness, None);
        assert_eq!(response.is_fresh, Some(false));
        assert!(response.freshness_reason.unwrap().contains("pending"));
        let empty = status(git_repo().path().to_str().unwrap());
        assert_eq!(empty.source_freshness, None);
        assert_eq!(empty.analyzer_freshness, None);
        assert!(!empty.available);
    }

    #[test]
    fn freshness_audit_repeated_edits_additions_and_deletions_never_look_current() {
        let repo = freshness_audit_matching_source_fixture();
        let root = repo.path().to_str().unwrap();
        let path = repo.path().join("src/caller.rs");
        for iteration in 0..100 {
            std::fs::write(&path, format!("fn changed_{iteration}() {{}}\n")).unwrap();
            assert_eq!(status(root).source_freshness, Some(false));
            std::fs::write(&path, CALLER_SOURCE).unwrap();
            assert_eq!(status(root).source_freshness, Some(true));
            let new_path = repo.path().join("src/additional.rs");
            std::fs::write(&new_path, "fn added() {}\n").unwrap();
            assert_eq!(status(root).source_freshness, Some(false));
            std::fs::remove_file(new_path).unwrap();
            std::fs::remove_file(&path).unwrap();
            assert_eq!(status(root).source_freshness, Some(false));
            std::fs::write(&path, CALLER_SOURCE).unwrap();
            let current = status(root);
            assert_eq!(current.source_freshness, Some(true));
            assert_eq!(current.analyzer_freshness, Some(false));
            assert_eq!(current.is_fresh, Some(false));
        }
    }

    #[test]
    fn freshness_audit_parser_uncertainty_cannot_hide_missing_source_files() {
        let repo = repo_with_one_generation();
        let response = status(repo.path().to_str().unwrap());
        assert_eq!(response.is_fresh, Some(false));
        assert!(response.freshness_reason.as_deref().is_some_and(|reason| reason.contains("source tree differs")),
            "the fixture has missing source files, but parser uncertainty hid the source check: {response:?}");
    }

    #[test]
    fn staleness_audit_status_preserves_the_stores_degraded_reason() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let response = serde_json::to_value(status(root)).unwrap();
        assert_eq!(response["available"], true, "stale navigation stays usable");
        assert_eq!(response.get("is_fresh"), Some(&serde_json::json!(false)));
        assert!(
            response["freshness_reason"]
                .as_str()
                .is_some_and(|s| s.contains("analyzer")),
            "fixture analyzer identity cannot be certified by the embedded reader: {response}"
        );
        assert_eq!(response.get("pending_count"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn staleness_audit_failed_status_cannot_authorize_test_selection() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "INSERT INTO generation_coverage_gaps (generation_id, gap, path, reason)
            VALUES (1, 'parse_failed', 'src/caller.rs', X'FF')",
            [],
        )
        .unwrap();
        drop(conn);
        let store = open_repo_map(root).unwrap();
        let error = store
            .status("")
            .expect_err("corrupt status evidence must fail");
        assert!(error.to_string().contains("Invalid column type"), "{error}");
        let selected = affected_tests(
            root,
            &["probe_callee".to_string()],
            None,
            WalkBounds::default(),
        );
        assert!(
            selected.fail_closed,
            "status read failed but selection was authorized: {selected:?}"
        );
        assert!(
            !selected.tests.available,
            "unverified test selection must be unavailable"
        );
        assert!(
            selected
                .fail_closed_reason
                .as_deref()
                .is_some_and(|s| s.contains("Invalid column type")),
            "the actual status failure must be preserved: {selected:?}"
        );
    }

    /* ── Finding 2: the repository path is an argument, not a fact ────────── */

    /// No entry point may answer about a repository it did not validate.
    ///
    /// `repo_path` is documented as an absolute path to a git repository, and
    /// nothing checked. `""` joined to the map path yields a *relative* path,
    /// which the filesystem resolves against the server process's own working
    /// directory — so an empty argument read whatever repository the process
    /// happened to be started in, and `"../elsewhere"` reached out of the one
    /// the caller named. Both are cross-repository reads driven purely by an
    /// argument.
    ///
    /// The map is built inside a directory that is deliberately *not* a git
    /// repository: before this gate every surface answered from it, so a
    /// refusal here cannot be produced by the map simply being absent.
    #[test]
    fn no_entry_point_answers_from_a_repository_path_it_did_not_validate() {
        let not_a_repo = tempfile::TempDir::new().expect("tempdir");
        drop(open_fixture_store(not_a_repo.path()));
        let unvalidated = not_a_repo.path().to_string_lossy().to_string();

        for (label, root, expected) in [
            ("empty", "", "Invalid repository path"),
            ("relative", "some/relative/repo", "must be absolute"),
            ("parent-relative", "../sibling", "must be absolute"),
            ("not a git repository", &unvalidated, "Not a Git repository"),
        ] {
            let stat = status(root);
            assert!(!stat.available, "status answered for {label} repo_path");
            let reason = stat.reason.unwrap_or_else(|| panic!("{label}: no reason"));
            assert!(
                reason.contains(expected),
                "{label}: refusal blames the wrong thing: {reason}"
            );
            assert!(
                !reason.contains("devmap.sqlite"),
                "{label}: refused for a missing map rather than for the path: {reason}"
            );
            assert!(
                stat.db_path.is_empty(),
                "{label}: reported a map path built from a path we refused: {}",
                stat.db_path
            );

            // Each surface is reduced to `(name, available, reason)` at the
            // call site rather than collected first: `search` and
            // `dead_symbols` answer over symbols while `impact`,
            // `dependencies` and `trace_between` answer over edges, so the
            // responses have no common type until the generic is dropped.
            let probes: Vec<(&str, bool, Option<String>)> = vec![
                {
                    let r = search(root, "probe", None);
                    ("search", r.available, r.reason)
                },
                {
                    let r = impact(root, "probe_callee", None);
                    ("impact", r.available, r.reason)
                },
                {
                    let r = dependencies(root, "src/caller.rs", None);
                    ("dependencies", r.available, r.reason)
                },
                {
                    let r = dead_symbols(root, None);
                    ("dead_symbols", r.available, r.reason)
                },
                {
                    let r = trace_between(root, "probe_caller", "probe_callee", None);
                    ("trace_between", r.available, r.reason)
                },
                {
                    let r = symbols_for_file(root, "src/caller.rs", "abc");
                    ("symbols_for_file", r.available, r.reason)
                },
            ];
            let refused: Vec<&str> = probes
                .into_iter()
                .filter_map(|(name, available, reason)| {
                    let refused = !available && reason.is_some_and(|r| r.contains(expected));
                    (!refused).then_some(name)
                })
                .collect();
            assert!(
                refused.is_empty(),
                "{label}: these surfaces did not refuse the path: {refused:?}"
            );
        }
    }

    /* ── Finding 3: counting without materializing what is counted ────────── */

    /// The fast counts must equal the rows they stand in for.
    ///
    /// `status` used to call `latest_file_hashes()` and `latest_edges(0.0)` and
    /// throw both result sets away through `.len()`, materializing the whole
    /// edge table — every edge as four owned strings — to learn one integer, on
    /// a call that runs on every `gitpulse_status` and every `gitpulse_insights`.
    /// The replacement counts in SQL, so the contract is that it counts the same
    /// thing: asserted against the row-returning queries themselves rather than
    /// against numbers written down here.
    #[test]
    fn status_counts_agree_with_the_rows_they_stand_in_for() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let stat = status(&root);
        assert!(stat.available, "status refused: {:?}", stat.reason);
        assert_eq!(stat.generation_id, Some(1));

        let store = Store::open(map_path(repo.path())).expect("open fixture store");
        let files = store.latest_file_hashes().expect("file hashes").len() as u32;
        let edges = store.latest_edges(0.0).expect("edges").len() as u32;
        assert_eq!(stat.total_files, Some(files), "file count drifted");
        assert_eq!(stat.total_edges, Some(edges), "edge count drifted");
        assert_eq!(
            stat.total_symbols,
            Some(2),
            "total_symbols was hardcoded None, so the hook renderer's \
             'N symbols across M files' always said zero"
        );
    }

    /// The file count falls back rather than attributing another build's number.
    ///
    /// `build_history` is where devmap records how many files a generation held,
    /// written inside that generation's own transaction — but history rows are
    /// retained 500 deep while generations are pruned to 2, and a database
    /// migrated up from before the table existed carries generations with no
    /// history row at all. Taking the newest row unconditionally would report
    /// some other build's file count as this one's.
    #[test]
    fn a_generation_whose_history_row_is_missing_still_counts_its_own_files() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();
        let expected = status(&root).total_files;

        let conn = rusqlite::Connection::open(map_path(repo.path())).expect("open fixture db");
        conn.execute("DELETE FROM build_history", [])
            .expect("drop history");
        // A row describing a generation that is not the latest: taken at face
        // value it would report 9,999 files for a three-file generation.
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (2, 1.0, 'other', 9999, 9999, 9999, 0, 0, 0, 1, NULL, 0)",
            [],
        )
        .expect("insert foreign history row");
        conn.close().expect("close fixture db");

        assert_eq!(
            status(&root).total_files,
            expected,
            "a history row for another generation was reported as this one's file count"
        );
    }

    /// Pending paths make the map stale: affected_tests must fail closed
    /// rather than hand CI a partial test list wearing a complete badge.
    ///
    /// Uses a minimal generation row only — the richer `repo_with_one_generation`
    /// fixture inserts into `generation_files`, which is a view on current
    /// schemas and is out of scope to rewrite here.
    #[test]
    fn affected_tests_fail_closed_when_the_map_has_pending_paths() {
        let dir = git_repo();
        let root = dir.path().to_string_lossy().to_string();
        let store = open_fixture_store(dir.path());
        let db = map_path(dir.path());
        drop(store);

        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, 'fixture', '{}', ?1)",
            [&root],
        )
        .expect("insert generation");
        conn.close().expect("close");

        let store = Store::open(&db).expect("reopen store");
        store
            .enqueue_pending_paths(&["src/new_file.rs".into()])
            .expect("enqueue pending");
        drop(store);

        let report = affected_tests(
            &root,
            &["src/caller.rs".into()],
            None,
            WalkBounds::default(),
        );
        assert!(
            report.fail_closed,
            "a stale map must not produce a trusted affected-tests list"
        );
        let reason = report
            .fail_closed_reason
            .expect("stale map states why it failed closed");
        assert!(
            reason.contains("stale") && reason.contains("pending"),
            "reason must name staleness, got {reason}"
        );
        assert!(
            report.tests.items.is_empty(),
            "must not return test paths from a stale map"
        );
    }

    /// Threads in this process, from the kernel's own count.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn process_thread_count() -> usize {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: an all-zero `proc_taskinfo` is a valid value (plain integers).
            let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
            // SAFETY: `info` is live and `size` is its exact size; the call
            // writes at most `size` bytes into it.
            let written = unsafe {
                libc::proc_pidinfo(
                    libc::getpid(),
                    libc::PROC_PIDTASKINFO,
                    0,
                    (&mut info as *mut libc::proc_taskinfo).cast(),
                    size,
                )
            };
            assert_eq!(written, size, "proc_pidinfo did not fill proc_taskinfo");
            info.pti_threadnum as usize
        }
        #[cfg(target_os = "linux")]
        {
            std::fs::read_dir("/proc/self/task")
                .expect("read /proc/self/task")
                .count()
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_thousand_instantly_finished_queries_leave_no_threads_behind() {
        // Warm the shared timer so its one long-lived thread is in the baseline.
        drop(begin_cancellable_query(None));
        let baseline = process_thread_count();
        for i in 0..1000 {
            let token = format!("thread-count-{i}");
            let cancel = begin_cancellable_query(Some(&token));
            assert!(!cancel.is_cancelled());
            finish_cancellable_query(Some(&token));
        }
        let after = process_thread_count();
        // Other tests start and stop threads concurrently, so allow a constant
        // of slack; a thread per query would add ~1000.
        assert!(
            after <= baseline + 50,
            "thread count grew from {baseline} to {after} after 1000 fast queries"
        );
    }

    fn local_timer() -> std::sync::Arc<DeadlineTimer> {
        std::sync::Arc::new(DeadlineTimer::start().expect("start timer"))
    }

    fn begin_on(
        timer: &std::sync::Arc<DeadlineTimer>,
        token: Option<&str>,
        deadline: std::time::Duration,
    ) -> Cancel {
        begin_cancellable_query_with(token, deadline, || Ok(std::sync::Arc::clone(timer)))
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let limit = std::time::Instant::now()
            + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5));
        while !done() {
            assert!(std::time::Instant::now() < limit, "timed out: {what}");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    const SHORT: std::time::Duration = std::time::Duration::from_millis(60);
    const LONG: std::time::Duration = std::time::Duration::from_secs(3600);

    #[test]
    fn a_query_still_running_at_the_deadline_sees_cancel_tripped() {
        let timer = local_timer();
        let cancel = begin_on(&timer, None, SHORT);
        wait_until("deadline trips the flag", || cancel.is_cancelled());
        assert_eq!(timer.pending(), 0, "a fired deadline must leave the heap");
    }

    #[test]
    fn a_failed_registration_is_logged_and_returns_a_tripped_cancel() {
        // Warm the shared timer so a concurrent test cannot restart it (and
        // clear the degraded record) between the failure and the assertion.
        shared_timer().expect("shared timer");
        let cancel = begin_cancellable_query_with(Some("refused-token"), LONG, || {
            Err("simulated thread exhaustion".to_string())
        });
        assert!(
            cancel.is_cancelled(),
            "a query whose deadline could not be registered must not run untimed"
        );
        assert!(
            !cancel_query("refused-token"),
            "a refused query must not be left registered"
        );
        assert!(
            crate::supervisor::degraded()
                .iter()
                .any(|d| d.subsystem == DEADLINE_SUBSYSTEM),
            "the refusal must be recorded as a degraded subsystem"
        );
    }

    #[test]
    fn an_unrepresentable_deadline_fails_closed() {
        let timer = local_timer();
        let cancel = begin_on(&timer, None, std::time::Duration::MAX);
        assert!(cancel.is_cancelled());
        assert_eq!(timer.pending(), 0);
    }

    #[test]
    fn deadlines_fire_in_deadline_order_not_registration_order() {
        let timer = local_timer();
        let long_a = begin_on(&timer, None, LONG);
        let mid = begin_on(&timer, None, std::time::Duration::from_millis(200));
        let short = begin_on(&timer, None, SHORT);
        let long_b = begin_on(&timer, None, LONG);
        wait_until("short deadline", || short.is_cancelled());
        wait_until("mid deadline", || mid.is_cancelled());
        assert!(!long_a.is_cancelled() && !long_b.is_cancelled());
        assert_eq!(timer.pending(), 2, "only the two long deadlines remain");
    }

    #[test]
    fn an_earlier_registration_wakes_a_timer_waiting_on_a_later_one() {
        let timer = local_timer();
        let long = begin_on(&timer, None, LONG);
        // The worker is now parked on the one-hour deadline; a short one
        // registered afterwards must still fire on time.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let short = begin_on(&timer, None, SHORT);
        wait_until("short deadline behind a long one", || short.is_cancelled());
        assert!(!long.is_cancelled());
    }

    #[test]
    fn concurrent_registrations_all_fire_and_all_withdraw_cleanly() {
        let timer = local_timer();
        let threads = 8;
        let per_thread = 200;
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let timer = std::sync::Arc::clone(&timer);
                std::thread::spawn(move || {
                    let mut kept = Vec::new();
                    for i in 0..per_thread {
                        if i % 2 == 0 {
                            kept.push(begin_on(&timer, None, SHORT));
                        } else {
                            let token = format!("concurrent-{t}-{i}");
                            let cancel = begin_on(&timer, Some(&token), LONG);
                            finish_cancellable_query(Some(&token));
                            assert!(!cancel.is_cancelled());
                        }
                    }
                    kept
                })
            })
            .collect();
        let kept: Vec<Cancel> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("registering thread"))
            .collect();
        assert_eq!(kept.len(), threads * per_thread / 2);
        wait_until("every short deadline fires", || {
            kept.iter().all(Cancel::is_cancelled)
        });
        assert_eq!(timer.pending(), 0, "withdrawn and fired entries are gone");
    }

    #[test]
    fn finishing_a_query_withdraws_its_deadline() {
        let timer = local_timer();
        let _cancel = begin_on(&timer, Some("finish-withdraws"), LONG);
        assert_eq!(timer.pending(), 1);
        finish_cancellable_query(Some("finish-withdraws"));
        assert_eq!(timer.pending(), 0);
    }

    #[test]
    fn dismissing_a_query_trips_it_and_withdraws_its_deadline() {
        let timer = local_timer();
        let cancel = begin_on(&timer, Some("dismiss-withdraws"), LONG);
        assert!(cancel_query("dismiss-withdraws"));
        assert!(cancel.is_cancelled());
        assert_eq!(timer.pending(), 0);
    }

    #[test]
    fn finishing_after_the_deadline_already_fired_is_harmless() {
        let timer = local_timer();
        let cancel = begin_on(&timer, Some("fired-then-finished"), SHORT);
        wait_until("deadline fires", || cancel.is_cancelled());
        finish_cancellable_query(Some("fired-then-finished"));
        finish_cancellable_query(Some("fired-then-finished"));
        assert!(!cancel_query("fired-then-finished"));
        assert_eq!(timer.pending(), 0);
    }

    #[test]
    fn a_reused_token_keeps_the_earlier_querys_deadline() {
        let timer = local_timer();
        let first = begin_on(&timer, Some("reused-token"), SHORT);
        let second = begin_on(&timer, Some("reused-token"), LONG);
        // The registry now names the second query; the first still has its
        // deadline and must not run untimed.
        wait_until("first query's deadline", || first.is_cancelled());
        assert!(!second.is_cancelled());
        finish_cancellable_query(Some("reused-token"));
        // The second's entry is not withdrawn: the token no longer says which
        // of the two queries finished. It expires on its own deadline.
        assert_eq!(timer.pending(), 1, "the second's deadline stays in force");
    }

    #[test]
    fn finishing_an_earlier_query_does_not_untime_a_later_one_on_the_same_token() {
        let timer = local_timer();
        let _first = begin_on(&timer, Some("shared-token"), LONG);
        let second = begin_on(&timer, Some("shared-token"), SHORT);
        // The first query finishes while the second is still running.
        finish_cancellable_query(Some("shared-token"));
        wait_until("the second query keeps its deadline", || {
            second.is_cancelled()
        });
    }
}
