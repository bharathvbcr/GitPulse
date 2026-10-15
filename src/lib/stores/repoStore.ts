import type { RebaseStep } from "../rebase/planner";
import { derived, get, writable } from "svelte/store";
import { invalidateSharedReads, invoke } from "../ipc/invoke";
import { formatError } from "../ui/formatError";
import { diagnostics } from "../diagnostics/diagnostics";
import { askConfirm } from "./modalStore";
import { confirmTerminalLoss } from "../repos/terminalLoss";
import { createDiffSelection } from "../repos/diffSelection";
// One-way: the terminal registry knows nothing about repositories, so this
// cannot form a cycle. `sessionFocus` takes the store by injection for the
// same reason.
import { sessionsByRepo, terminalSessions } from "../terminal/sessionRegistry";
import { requestRepositoryTrust } from "../repos/repositoryTrust";
import { harnessStore, type PolicyVerdict } from "./harnessStore";
import { parseTagList, type BranchInfo, type TagInfo } from "../branches/types";
import { filterStore } from "./filterStore";
import { graphStore } from "./graphStore";
import { interfaceStore } from "./interfaceStore";
import type { InvokeFn } from "./graphStore";
import {
  disambiguateLabels,
  displayName,
  identityKey,
  isCaseInsensitiveFs,
  sameRepo,
  type PathIdentityOptions,
} from "../repos/paths";
import {
  activateTab,
  closeOtherTabs,
  closeTab,
  closeTabsToTheRight,
  emptyWorkspace,
  MAX_OPEN_TABS,
  openTab,
  type TabRecord,
  pinTab as pinWorkspaceTab,
  removeRecent as removeWorkspaceRecent,
  moveTabTo as moveWorkspaceTabTo,
  arrangeTabs as arrangeWorkspaceTabs,
  setTabGroup as setWorkspaceTabGroup,
  setTabColor as setWorkspaceTabColor,
  setGroupColor as setWorkspaceGroupColor,
  restoreGroupColors,
  setGroupCollapsed as setWorkspaceGroupCollapsed,
  toggleGroupCollapsed as toggleWorkspaceGroupCollapsed,
  isGroupCollapsed as isWorkspaceGroupCollapsed,
  renameGroup as renameWorkspaceGroup,
  ungroupTabs as ungroupWorkspaceTabs,
  closeGroup as closeWorkspaceGroup,
  groupByParentFolder as groupWorkspaceByParentFolder,
  collapseAllGroups as collapseWorkspaceAllGroups,
  expandAllGroups as expandWorkspaceAllGroups,
  type WorkspaceTabs,
} from "../repos/tabModel";
import {
  browserStorage,
  loadPersistedWorkspace,
  savePersistedWorkspace,
  workspaceToPersisted,
  type ViewTab,
} from "../repos/persist";
import * as workspaceSync from "../codeintel/workspaceSync";
import { autoInit } from "../codeintel/autoInit";
import { liveIndex } from "../codeintel/liveIndex";
import { type TabColor } from "../repos/tabColors";
import { familyFromCommonDir } from "../repos/repoFamily";
import { computeTabLayout, type StackingOptions } from "../repos/tabGroups";
import {
  activationStops,
  canMoveUnit,
  cycleStop,
  moveUnit,
  moveUnitToEdge,
  unitForTab,
} from "../repos/stripNav";
import { expandedStacks, lastUsedCheckouts, noteActiveCheckout } from "../repos/stackState";
import { isSectionOnScreen, resolveSection } from "../views/viewRegistry";
import { parseStashList, type StashAction, type StashEntry, type StashSaveOptions } from "../repos/stash";
import type { MergeMode } from "../branches/mergeSelection";
import { parseGitPreflight, type GitIdentity, type GitPreflight, type IdentityScope } from "../repos/gitPreflight";
import {
  AUTO_FETCH_TICK_MS,
  createAutoFetchScheduler,
  loadAutoFetchPrefs,
  parseAutoFetchOutcome,
  saveAutoFetchPrefs,
  withAutoFetchSetting,
  type AutoFetchOutcome,
  type AutoFetchPrefs,
  type AutoFetchSetting,
} from "../repos/autoFetch";
import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
import { hasUnstagedChanges } from "../files/fileStatus";
import { indexSelectionPaths } from "../repos/bulkOps";
import {
  WATCH_ACTIVE,
  WATCH_PARKED,
  WATCH_UNKNOWN,
  needsFullPoll,
  watchFailed,
  watchStatesEqual,
  type WatchState,
} from "../repos/watchState";
import { createWatchPool } from "../repos/watchPool";
import { parseRemoteList, type RemoteChange, type RemoteInfo } from "../repos/remotes";
import { parseSubmoduleList, type SubmoduleChange, type SubmoduleInfo } from "../repos/submodules";
import {
  runAcrossRepos,
  type BulkRunReport,
  type RepoTarget,
  type RunOptions,
} from "../repos/workspaceOps";
import { mapItems, DEFAULT_FAN_OUT } from "../async/pool";
import {
  summarizeWorkspace,
  bulkSkipReason,
  type RepoWipInput,
  type WorkspaceWip,
} from "../repos/wipSummary";
import { toWipInput, type RepoFacts } from "../repos/facts";
import {
  IDLE_OPERATION,
  operationStatesEqual,
  type OperationAction,
  type OperationState,
  type RepoOperation,
} from "../repos/operation";
import type { RepoChange } from "../repos/events";
import {
  changeFromStatusPoll,
  shallowRecordListEqual,
  shouldRunStatusPoll,
  statusPollRestMs,
  statusesEqual,
} from "../repos/statusPoll";
import {
  bindForegroundChanges,
  readBackgroundDocument,
  readHiddenDocument,
} from "../runtime/foreground";
import {
  WATCHER_REFRESH_DEBOUNCE_MS,
  createWatcherRefreshPolicy,
} from "../repos/watcherRefresh";
import { decideCadence, readEventLoopDelay } from "../runtime/loadCadence";
import { debounce, type Debounced } from "../async/debounce";
import { createSessionBook } from "../repos/sessionBook";
import { createCommitDraftStore } from "./commitDraftStore";
import { flushOnPageHide } from "../async/deferredWrite";
import type { FilePatch } from "../diff/patchBuilder";
import type { ReleasePublishResult } from "../ops/model";

import type { MutationOutcome, MutationTarget, ResetMode, IndexAction, FileStatus, ResolvedRepo, BranchStatsReport, DiffPayload, RepoSession, RepoState, InternalState, RepoStoreDeps } from "../repos/storeTypes";
import { createProjectionCache, createSession, emptyProjected, project } from "../repos/projection";

export type { MutationOutcome, MutationTarget, ResetMode, IndexAction, SelectionKind, FileStatus, ResolvedRepo, BranchStatsReport, OpenRepoTab, DiffPayload, RepoSession, RepoState, RepoStoreDeps } from "../repos/storeTypes";
export type { BranchInfo, TagInfo, ViewTab };
export type { OperationAction, OperationState, RepoOperation };
export type { StashAction, StashEntry, RemoteChange, SubmoduleChange };
export type { WatchState };
export type { BulkRunReport, WorkspaceWip };
export type { RepoFacts };
export type { InvokeFn };


const MENU_RECENT_CAP = 12;
/** Trailing debounce for localStorage writes and the native-menu rebuild. */
const PERSIST_DEBOUNCE_MS = 300;
/**
 * Upper bound of cmd_branch_stats batches drained per fetch. The backend
 * computes at most 96 unique uncached tips per call, so 64 batches cover
 * ~6100 unique tips; past the bound draining stops silently and churn
 * resumes on the next refresh.
 */
export const STATS_DRAIN_MAX_BATCHES = 64;
/**
 * How recent a landed snapshot must be for activating its tab to skip the read
 * (given a live watch). Short because the skip trusts the watcher for the
 * interval; long enough to cover switching back and forth between two tabs.
 */
export const ACTIVATION_FRESH_MS = 5_000;
/** Publish coalesced stats every N batches (and always on the final drain). */
export const STATS_PUBLISH_EVERY = 8;
/**
 * Watcher events for a repo are dropped for this long after one of our own
 * mutations succeeds there: they are echoes of the mutation's own `.git`
 * writes, and honoring them means every mutation costs TWO full refreshes.
 * The window exceeds Rust DEBOUNCE_MAX_WAIT=2000ms plus the 200ms watcher
 * debounce (WATCHER_REFRESH_DEBOUNCE_MS), so a real echo can never slip past
 * it. The check runs when the event arrives, before the refresh policy
 * delays anything, so a background tab's longer wait does not widen it.
 * Trade-off: an
 * unrelated external change landing inside the window is picked up by the
 * next status poll or later watcher event instead of refreshing immediately —
 * accepted, because it halves per-mutation load.
 */
const WATCHER_ECHO_SUPPRESS_MS = 2500;
/** The refusal a mutation returns when its captured target no longer names the repository. */
const REPOSITORY_CHANGED = "The repository changed while this was waiting; nothing was run.";
/**
 * Mutation kinds that rewrite what the open worktree diff pane shows (the
 * staged/unstaged split moves, or the file disappears). After these, the open
 * selection is refetched so the pane stops displaying pre-mutation content.
 */
const REFETCH_SELECTION_KINDS = new Set([
  "stage",
  "unstage",
  "stage-all",
  "unstage-all",
  "stage-patch",
  "unstage-patch",
  "discard",
  "commit",
]);

const REPOSITORY_TRUST_REQUIRED = "REPOSITORY_TRUST_REQUIRED";

/** What a person reads when a tab cannot be opened at all. */
function openRefusalMessage(reason: "invalid" | "capacity"): string {
  return reason === "capacity"
    ? `Too many open repositories (max ${MAX_OPEN_TABS}). Close a tab to open another.`
    : "Invalid repository path";
}

export function repositoryTrustRefused(message: string): boolean {
  return message.includes(REPOSITORY_TRUST_REQUIRED);
}

/**
 * Open paths that may receive background work. A session with
 * `trustRequired`, or one whose checkout is `missing`, is omitted, and it is
 * not used as the active key. A missing checkout used to stay in: every
 * workspace sync re-tried it and re-reported it, once per activation.
 */
export function pathsTrustedForBackground(
  tabs: readonly { path: string; trustRequired?: boolean; missing?: boolean; parked?: boolean }[],
  activePath: string | null,
): { activeKey: string | null; retainedKeys: string[]; liveKeys: string[] } {
  const retainedKeys: string[] = [];
  const liveKeys: string[] = [];
  let activeBlocked = false;
  for (const tab of tabs) {
    if (tab.trustRequired || tab.missing) {
      if (tab.path === activePath) activeBlocked = true;
      continue;
    }
    retainedKeys.push(tab.path);
    // Parked: open but not watched (repos/watchPool.ts). The active tab is
    // never parked, so this cannot drop the repository being looked at.
    if (!tab.parked || tab.path === activePath) liveKeys.push(tab.path);
  }
  return {
    activeKey: activePath && !activeBlocked ? activePath : null,
    retainedKeys,
    liveKeys,
  };
}

export function createRepoStore(deps: RepoStoreDeps = {}) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  const storage = deps.storage === undefined ? browserStorage() : deps.storage;
  const options: PathIdentityOptions = {
    caseInsensitive: deps.caseInsensitive ?? isCaseInsensitiveFs(),
  };
  const graph = deps.graph ?? graphStore;
  const filters = deps.filter ?? filterStore;
  const terminals = deps.terminals ?? {
    countFor: (repoPath: string) => sessionsByRepo(get(terminalSessions), options).get(repoPath) ?? 0,
    hostedFor: (repoPath: string) =>
      sessionsByRepo(get(terminalSessions).filter((record) => !!record.checkout), options).get(repoPath) ?? 0,
  };

  /** Rows `publish` can reuse; see `ProjectionCache`. */
  const projection = createProjectionCache();
  let internal: InternalState = {
    workspace: emptyWorkspace(),
    sessions: {},
    workspaceError: null,
  };

  const { subscribe, set } = writable<RepoState>(emptyProjected());
  // A full watcher/manual refresh can observe M→M content edits while every
  // status field stays equal. Keep this signal separate from the UI snapshot
  // so content readers update without invalidating every repository subscriber.
  const contentRevisions = writable<Record<string, string>>({});
  let openEpoch = 0;
  let syncingFilter = false;
  let shortcutLocked = false;

  // --- work-tree status poll ---------------------------------------------
  // One lazy timer for the whole workspace. While the window is in the
  // background the timer is gone, not merely ignored: a callback that returns
  // still woke the renderer every period. When the event loop is late the next
  // arm stretches instead of spending another git status on a machine that is
  // already behind. Background includes a blurred webview, not only a hidden one.
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let pollWanted = false;
  let unbindPollForeground: (() => void) | null = null;
  let pollInflight = false;
  /** Monotonic poll ordering; a superseded tick's result is discarded. */
  let pollSequenceSource = 0;
  // Per-tab and per-path records the poll, hydrate, stats and mutation paths
  // keep. One owner, so every close route forgets all of them (`sessionBook.ts`).
  const sessionBook = createSessionBook((a, b) => sameRepo(a, b, options));
  const { familyUnknown, snapshotRuns, pollRuns, statsInflight, mutationEchoUntil, statsDrained } = sessionBook;

  // Diff selection: file, commit, commit file and range share one fetch path
  // (`repos/diffSelection.ts`), which also owns the token that orders them.
  const diffSelection = createDiffSelection({
    invoke: invokeFn,
    activeSession: () => activeSession(),
    applyToSession: (id, generation, patch) => applyToSession(id, generation, patch),
    openEpoch: () => openEpoch,
  });

  /**
   * How many poll ticks between re-asserting the active repository's watch.
   *
   * A watch can die AFTER it was established — the backend reaps a session
   * whose event stream closes, whose thread panics, or whose repository
   * disappears, and its own log says "UI refresh for this repo is dead until
   * it is re-watched". Nothing told the frontend, so the session went on
   * believing it was live and the compensating full poll never engaged.
   *
   * Re-asserting repairs rather than merely reports: `cmd_watch_repo` returns
   * immediately when the session is still registered, and creates a fresh
   * watcher when it was reaped. Once a minute keeps a dead watch's blind
   * window bounded to about a minute without putting a subprocess on the
   * 6-second path. Timed rather than counted: a slow repository's poll rests
   * ten times its cost (`statusPollRestMs`, up to two minutes), and ten of
   * those ticks would leave a dead watch blind for twenty.
   */
  const WATCH_REASSERT_EVERY_MS = 60_000;
  let lastWatchReassertAt = Date.now();
  /** How long the last poll took, which sets how long the next one waits. */
  let pollLastDurationMs: number | null = null;

  function onStatusPollVisibility(): void {
    if (!pollWanted) return;
    if (readBackgroundDocument()) {
      if (pollTimer !== null) {
        clearTimeout(pollTimer);
        pollTimer = null;
      }
      return;
    }
    armStatusPoll();
  }

  function bindStatusPollVisibility(): void {
    if (unbindPollForeground !== null || typeof document === "undefined") return;
    unbindPollForeground = bindForegroundChanges(
      document,
      typeof window === "undefined" ? null : window,
      onStatusPollVisibility,
    );
  }

  function unbindStatusPollVisibility(): void {
    unbindPollForeground?.();
    unbindPollForeground = null;
  }

  function armStatusPoll(): void {
    if (!pollWanted || pollTimer !== null || typeof setTimeout === "undefined") return;
    const decision = decideCadence({
      baseMs: statusPollRestMs({ lastDurationMs: pollLastDurationMs }),
      lagMs: readEventLoopDelay(),
      paused: readBackgroundDocument(),
    });
    if (!decision.run) return;
    pollTimer = setTimeout(() => {
      pollTimer = null;
      void runStatusPoll().finally(() => {
        if (pollWanted) armStatusPoll();
      });
    }, decision.delayMs);
  }

  /**
   * Re-arms a waiting poll against the current rest. The rest was chosen for
   * the repository that was active when it was armed; after a switch it may
   * be ten times a slow tab's status, and the tab now showing deserves its
   * own cadence from the moment it shows.
   */
  function rearmStatusPoll() {
    if (!pollWanted || pollTimer === null) return;
    clearTimeout(pollTimer);
    pollTimer = null;
    armStatusPoll();
  }

  function ensureStatusPoll() {
    if (pollWanted || typeof setTimeout === "undefined") return;
    pollWanted = true;
    bindStatusPollVisibility();
    armStatusPoll();
  }

  /** Stops the workspace poll; the next activation restarts it lazily. */
  function stopStatusPoll() {
    pollWanted = false;
    if (pollTimer !== null) {
      clearTimeout(pollTimer);
      pollTimer = null;
    }
    unbindStatusPollVisibility();
  }

  /** Who hears what a status poll found moved; see `changeFromStatusPoll`. */
  const polledChangeListeners = new Set<(path: string, change: RepoChange) => void>();

  async function runStatusPoll() {
    const session = activeSession();
    const hidden = readBackgroundDocument();
    if (
      !shouldRunStatusPoll({
        hidden,
        hasSession: Boolean(session),
        isLoading: Boolean(session?.isLoading),
        inflight: pollInflight,
      })
    ) {
      return;
    }
    // A refused repository stays on the one error hydrate already recorded.
    // Polling it again would re-run Git and re-log the same refusal.
    if (session!.trustRequired) return;
    const path = session!.path;
    const generation = session!.generation;
    const sessionId = session!.id;

    // Periodically re-assert the watch so a watcher that died after startup
    // self-heals, and so its loss becomes visible if it cannot. Deliberately
    // not awaited: the status tick must not wait on it, and its own result is
    // generation-guarded before it lands.
    if (Date.now() - lastWatchReassertAt >= WATCH_REASSERT_EVERY_MS) {
      lastWatchReassertAt = Date.now();
      void acquireWatch(sessionId, path, true);
    }

    // A repository with no live watcher gets a FULL refresh on this tick
    // instead of the statuses-only one. The watcher is what refreshes
    // branches, the graph, the parked-operation banner and the stash stack on
    // a tab the user is already sitting on; without it those go stale forever
    // while the file list keeps updating, which reads as "everything is
    // current". This is the compensation that makes the indicator honest
    // rather than merely apologetic. Bounded to the ACTIVE session, so the
    // extra cost is one snapshot per interval and only while degraded.
    if (needsFullPoll(session!.watch)) {
      pollInflight = true;
      const startedAt = Date.now();
      try {
        await hydrate(sessionId, path, generation);
      } finally {
        pollInflight = false;
        pollLastDurationMs = Date.now() - startedAt;
      }
      return;
    }
    // Ordering tokens: the result must lose to BOTH a newer poll and any
    // hydrate started after this tick — otherwise a slow poll lands after a
    // watcher refresh's snapshot and clobbers fresher statuses for up to a
    // full poll interval.
    pollSequenceSource += 1;
    const run = pollSequenceSource;
    const snapshotRunAtStart = snapshotRuns.get(sessionId);
    pollRuns.set(sessionId, run);
    pollInflight = true;
    const startedAt = Date.now();
    try {
      const statuses = await invokeFn<FileStatus[]>("cmd_get_status", {
        repoPath: path,
      });
      pollLastDurationMs = Date.now() - startedAt;
      if (pollRuns.get(sessionId) !== run) return;
      if (snapshotRuns.get(sessionId) !== snapshotRunAtStart) return;
      // A quiet repo returns byte-identical statuses every 6s; republishing
      // them would re-run every subscriber effect app-wide (visible churn in
      // the diff pane). Skip when the session still holds exactly these
      // statuses — a generation change re-applies via applyToSession below.
      const live = internal.sessions[sessionId];
      if (
        live &&
        live.generation === generation &&
        statusesEqual(statuses, live.statuses)
      ) {
        return;
      }
      const before = live && live.generation === generation ? live.statuses : [];
      applyToSession(sessionId, generation, { statuses });
      // The watcher cannot see nested edits (its worktree watch is
      // non-recursive); the poll can, so it announces them to the views
      // derived from files.
      const polled = changeFromStatusPoll(before, statuses);
      if (polled) for (const listener of polledChangeListeners) listener(path, polled);
    } catch {
      /* a repo that vanished reports through the next full refresh */
    } finally {
      if (pollRuns.get(sessionId) === run) pollRuns.delete(sessionId);
      pollInflight = false;
    }
  }
  // -----------------------------------------------------------------------
  let lastPersistedPayload: string | null = null;
  /** Recents payload last handed to the native menu; IPC fires only on change. */
  let lastSentRecentsJson: string | null = null;
  /** Last open-tab set synced into workspace.json — skip no-op publishes. */
  let lastWorkspaceSyncKey: string | null = null;
  const commitDrafts = createCommitDraftStore();
  /** The tab list the drafts were last pruned against. */
  let draftTabs: readonly TabRecord[] | null = null;
  let lastLiveScopeKey: string | null = null;
  let persistTimer: ReturnType<typeof setTimeout> | null = null;
  /**
   * User edits that may shrink the tab list or clear groups advance this.
   * Restoring a session does not: a half-finished restore must not be able
   * to replace the durable copy.
   */
  let workspaceEpoch = 0;
  /** Paths closed since the last successful save. Coalesce honors these drops. */
  let droppedPaths: string[] = [];
  /** Non-zero while restore is applying the durable copy. Writes wait. */
  let persistSuspended = 0;

  function familyOfTab(id: string) {
    return familyFromCommonDir(internal.sessions[id]?.commonDir, options);
  }

  /**
   * Seats a newly opened checkout with the open checkouts of its repository:
   * in their group, just after the last of them. Without it an agent's
   * worktree tab landed at the far end of the strip, and when the repository
   * sat in a user group the worktree opened ungrouped — a stack is keyed by
   * (group, family), so it could never join and read as a second repository.
   *
   * The group comes from the family's checkout the reader used last, else the
   * last one on the strip. An explicit group from the caller always wins.
   */
  function placeBesideFamily(
    id: string,
    commonDir: string | null | undefined,
    explicitGroup: boolean,
    activate: boolean,
  ) {
    const family = familyFromCommonDir(commonDir, options);
    if (!family) return;
    let ws = internal.workspace;
    const members = ws.tabs.filter(
      (tab) => tab.id !== id && familyOfTab(tab.id)?.key === family.key,
    );
    if (members.length === 0) return;
    if (!explicitGroup) {
      const lastUsedId = lastUsedCheckouts().get(family.key);
      const anchor =
        members.find((tab) => tab.id === lastUsedId) ?? members[members.length - 1];
      const anchorGroup = anchor.group ?? null;
      const current = ws.tabs.find((tab) => tab.id === id);
      if (current && (current.group ?? null) !== anchorGroup) {
        ws = setWorkspaceTabGroup(ws, id, anchorGroup);
        // Shown means visible: a folded group would hide the tab just opened.
        if (activate && anchorGroup) ws = setWorkspaceGroupCollapsed(ws, anchorGroup, false);
      }
    }
    const from = ws.tabs.findIndex((tab) => tab.id === id);
    if (from < 0) return;
    const group = ws.tabs[from].group ?? null;
    let last = -1;
    ws.tabs.forEach((tab, index) => {
      if (tab.id === id || (tab.group ?? null) !== group) return;
      if (familyOfTab(tab.id)?.key === family.key) last = index;
    });
    if (last >= 0) ws = moveWorkspaceTabTo(ws, id, from > last ? last + 1 : last);
    if (ws !== internal.workspace) replaceWorkspace(ws);
  }

  function beginShortcut(): boolean {
    if (shortcutLocked) return false;
    shortcutLocked = true;
    queueMicrotask(() => {
      shortcutLocked = false;
    });
    return true;
  }

  /** How the strip stacks worktrees right now; the one input the bar and cycling share. */
  function stripStacking(): StackingOptions {
    return {
      enabled: get(interfaceStore).stackWorktreeTabs,
      expanded: get(expandedStacks),
      lastUsed: lastUsedCheckouts(),
      identity: (path: string) => identityKey(path, options),
    };
  }

  /** The strip as drawn, for shortcuts that step through what the reader sees. */
  function drawnLayout() {
    const projected = project(internal, options, projection);
    return computeTabLayout(
      projected.openTabs,
      projected.collapsedGroups,
      undefined,
      projected.groupColors,
      stripStacking(),
    );
  }

  /**
   * Restore opens every saved tab before any is read, and each open used to
   * publish two or three times: a full projection, a persist check and a
   * background-scope comparison per publish, over every tab, which made
   * opening a large workspace quadratic in its size. While held, publishing
   * is owed instead and paid once on release.
   */
  let publishHeld = 0;
  let publishOwed = false;

  function holdPublish(): () => void {
    publishHeld += 1;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      publishHeld -= 1;
      if (publishHeld === 0 && publishOwed) {
        publishOwed = false;
        publish();
      }
    };
  }

  function publish() {
    if (publishHeld > 0) {
      publishOwed = true;
      return;
    }
    const activeId = internal.workspace.activeId;
    if (activeId) noteActiveCheckout(familyOfTab(activeId)?.key, activeId);
    if (draftTabs !== internal.workspace.tabs) {
      draftTabs = internal.workspace.tabs;
      commitDrafts.retain(draftTabs.map((tab) => tab.id));
    }
    commitDrafts.setActiveTab(activeId);
    set(project(internal, options, projection));
    persist();
    const active = internal.workspace.activeId
      ? internal.sessions[internal.workspace.activeId]
      : undefined;
    const trusted = pathsTrustedForBackground(
      internal.workspace.tabs.map((tab) => ({
        path: tab.path,
        trustRequired: internal.sessions[tab.id]?.trustRequired === true,
        missing: internal.sessions[tab.id]?.missing === true,
        parked: internal.sessions[tab.id]?.watch.status === "parked",
      })),
      active?.path ?? null,
    );
    // Two scopes, each re-applied only when it changes: parking a tab moves
    // the live set but not membership, and every apply walks every key.
    const visible = !readBackgroundDocument();
    const syncKey = JSON.stringify([trusted.activeKey, trusted.retainedKeys]);
    if (syncKey !== lastWorkspaceSyncKey) {
      lastWorkspaceSyncKey = syncKey;
      // Membership — the registry and the init that writes it — is every
      // open repository: searching the open tabs means all of them.
      workspaceSync.scheduleWorkspaceSync(trusted.activeKey, trusted.retainedKeys);
      autoInit.setScope({ activeKey: trusted.activeKey, retainedKeys: trusted.retainedKeys, visible });
    }
    const liveKey = JSON.stringify([trusted.activeKey, trusted.liveKeys]);
    if (liveKey !== lastLiveScopeKey) {
      lastLiveScopeKey = liveKey;
      // Background index work is not: a parked repository gets nothing in
      // the background, the index included, and catches up when opened.
      liveIndex.setScope({ activeKey: trusted.activeKey, retainedKeys: trusted.liveKeys, visible });
    }
  }

  /**
   * Records a user edit that is allowed to shrink the durable tab list.
   * Paths named here stay closed; every other tab the snapshot forgot is
   * put back by coalesce.
   */
  function commitEdit(closed: readonly string[] = []) {
    workspaceEpoch += 1;
    for (const path of closed) {
      if (path && !droppedPaths.includes(path)) droppedPaths.push(path);
    }
    if (droppedPaths.length > MAX_OPEN_TABS * 2) {
      droppedPaths = droppedPaths.slice(-MAX_OPEN_TABS * 2);
    }
  }

  /**
   * Persists workspace state. The write + menu IPC are debounced on a short
   * trailing delay so per-keystroke state (the search query) cannot
   * storm localStorage and the native menu; `flushPersist` runs the live
   * snapshot immediately whenever a critical mutation lands.
   */
  function persist() {
    if (persistSuspended > 0) return;
    if (persistTimer !== null || typeof setTimeout === "undefined") return;
    persistTimer = setTimeout(() => flushPersist(), PERSIST_DEBOUNCE_MS);
  }

  function flushPersist(force = false) {
    if (persistTimer !== null) {
      clearTimeout(persistTimer);
      persistTimer = null;
    }
    if (persistSuspended > 0) return;
    const data = workspaceToPersisted(internal.workspace, internal.sessions, workspaceEpoch);
    const recents = internal.workspace.recents.slice(0, MENU_RECENT_CAP);
    const payload = JSON.stringify({ data, recents, dropped: droppedPaths });
    if (!force && payload === lastPersistedPayload) return;
    const closed = droppedPaths.slice();
    if (!savePersistedWorkspace(storage, data, closed, options)) {
      // The write did not land (quota, private mode). Leave the dedup marker
      // clear so the same snapshot is retried instead of being skipped.
      lastPersistedPayload = null;
      if (typeof setTimeout !== "undefined" && persistTimer === null) {
        persistTimer = setTimeout(() => flushPersist(), PERSIST_DEBOUNCE_MS);
      }
    } else {
      lastPersistedPayload = payload;
      droppedPaths = [];
    }
    const recentsJson = JSON.stringify(recents);
    if (recentsJson !== lastSentRecentsJson) {
      lastSentRecentsJson = recentsJson;
      void invokeFn("cmd_set_recent_menu", { paths: recents }).catch((error) => {
        if (lastSentRecentsJson === recentsJson) lastSentRecentsJson = null;
        diagnostics.warn("desktop:recent-menu", error);
      });
    }
  }

  // For the life of the store, not only while the poll runs. `pagehide` is
  // fired at window; this was once bound on document, where it never fired.
  flushOnPageHide(() => flushPersist(true));

  function replaceWorkspace(next: WorkspaceTabs) {
    internal = { ...internal, workspace: next };
  }

  /**
   * Writes one session in place. The map used to be copied on every write,
   * one object spread over every open repository per change, which is
   * quadratic over a restore of a large workspace. Nothing outside the store
   * sees the map and nothing inside holds an old one across an await (every
   * whole-map replacement below copies and assigns in one synchronous step),
   * and a changed row is still detected by the session object, which is new.
   */
  function putSession(session: RepoSession) {
    internal.sessions[session.id] = session;
  }

  function activeSession(): RepoSession | undefined {
    const id = internal.workspace.activeId;
    return id ? internal.sessions[id] : undefined;
  }

  /**
   * The session a captured target still names, or null. A tab id always names
   * one repository, so a missing session, a different path, a checkout that is
   * gone and a repository that is no longer trusted are all "the repository
   * changed" — the mutation is refused, not retargeted at another tab.
   */
  function resolveTarget(target: MutationTarget): RepoSession | null {
    const session = internal.sessions[target.tabId];
    if (!session || !sameRepo(session.path, target.path, options)) return null;
    if (session.missing || session.trustRequired) return null;
    return session;
  }

  /**
   * True when `patch` would leave the session's RENDERED state untouched.
   * IPC snapshots arrive with fresh array identities every cycle; without
   * this gate every watcher refresh republished new root state app-wide,
   * re-running each subscriber effect even though nothing visible changed.
   * Array fields compare element-wise over all own keys, so a deep-equal
   * snapshot is recognized and dropped while any new backend field still
   * forces a (safe-direction) publish.
   */
  function patchIsNoop(
    session: RepoSession,
    patch: Partial<RepoSession>,
  ): boolean {
    for (const key of Object.keys(patch) as (keyof RepoSession)[]) {
      const incoming = patch[key];
      if (incoming === session[key]) continue;
      // `operation` is the one object-valued field, and the snapshot rebuilds
      // it every poll — reference equality would republish the whole store to
      // every subscriber every six seconds on a repository where nothing
      // happened. Compared through its owner rather than a generic deep-equal,
      // so no other field silently acquires expensive comparison semantics.
      if (key === "watch") {
        if (watchStatesEqual(session.watch, incoming as unknown as WatchState)) {
          continue;
        }
        return false;
      }
      if (key === "operation") {
        if (
          operationStatesEqual(
            session.operation,
            incoming as unknown as OperationState,
          )
        ) {
          continue;
        }
        return false;
      }
      if (
        Array.isArray(incoming) &&
        Array.isArray(session[key]) &&
        shallowRecordListEqual(
          session[key] as unknown as Record<string, unknown>[],
          incoming as unknown as Record<string, unknown>[],
        )
      ) {
        continue;
      }
      return false;
    }
    return true;
  }

  function applyToSession(
    id: string,
    generation: number,
    /**
     * A patch, or a function given the LIVE session that returns one.
     *
     * The function form exists for fields that merge rather than replace —
     * `viewSections` above all. A caller that spreads a session snapshot
     * captured before an await would silently undo any section change made
     * while its fetch was in flight; reading the live session here cannot.
     */
    patchOrFn: Partial<RepoSession> | ((current: RepoSession) => Partial<RepoSession>),
  ) {
    const session = internal.sessions[id];
    if (!session || session.generation !== generation) return false;
    const patch = typeof patchOrFn === "function" ? patchOrFn(session) : patchOrFn;
    // A no-op patch must not publish: subscribers treat every store emission
    // as invalidation. Report "advanced" so callers do not retry the work.
    if (patchIsNoop(session, patch)) return false;
    putSession({ ...session, ...patch });
    publish();
    return true;
  }

  /**
   * Bumps a session into a new activation epoch. A new generation orphans any
   * in-flight branch-stats fetch — its settle path is generation-guarded and
   * will never clear this flag — so pending must reset here.
   */
  function bumped(session: RepoSession): RepoSession {
    return {
      ...session,
      generation: session.generation + 1,
      statsPending: false,
    };
  }

  function syncFilterFromSession(session: RepoSession | undefined) {
    syncingFilter = true;
    try {
      if (!session) {
        filters.clear();
        return;
      }
      filters.setSearch(session.searchQuery);
      filters.selectBranch(session.selectedBranch);
    } finally {
      syncingFilter = false;
    }
  }

  /**
   * Presents the session's repository in the graph pane (cached rows render
   * instantly). It deliberately does NOT call loadGraph: the App-level effect
   * owns fetches keyed on path/revision/query — activation re-renders cached
   * rows without a refetch. Freshness comes from refresh(), which loadGraphs
   * directly for the active session on watcher events and after mutations.
   */
  function revealGraph(session: RepoSession | undefined) {
    if (!session) {
      graph.showRepo(null);
      return;
    }
    graph.showRepo(session.path);
  }

  filters.subscribe((value) => {
    if (syncingFilter) return;
    const session = activeSession();
    if (!session) return;
    if (
      session.searchQuery === value.searchQuery &&
      session.selectedBranch === value.selectedBranch
    ) {
      return;
    }
    putSession({
      ...session,
      searchQuery: value.searchQuery,
      selectedBranch: value.selectedBranch,
    });
    publish();
  });

  async function loadSnapshot(path: string): Promise<{
    branches: BranchInfo[];
    statuses: FileStatus[];
    tags: TagInfo[];
    currentBranch: string | null;
    defaultBranch: string | null;
    operation: OperationState;
    stashEntries: StashEntry[];
    stashFailed: boolean;
    stashTruncated: boolean;
    tagsTruncated: boolean;
    tagsFailed: boolean;
    fetchedAt: number | null;
  }> {
    const [branches, statuses, tags, operation, stash, fetchedAt] = await Promise.all([
      invokeFn<BranchInfo[]>("cmd_list_branches", { repoPath: path }),
      invokeFn<FileStatus[]>("cmd_get_status", { repoPath: path }),
      invokeFn<unknown>("cmd_list_tags", { repoPath: path })
        .then((raw) => parseTagList(raw))
        .catch(() => ({ tags: [] as TagInfo[], truncated: false, failed: true })),
      // A failed probe is recorded as a failure, never folded into "idle".
      // Reporting "no operation in progress" because the check itself broke
      // is what strands a user mid-merge in a UI insisting all is well. It
      // does not fail the snapshot, though: branches and statuses are still
      // worth rendering, and the marker says the state is unknown.
      invokeFn<RepoOperation | null>("cmd_repo_operation", { repoPath: path })
        .then((value) => ({ operation: value ?? null, probeFailed: false }))
        .catch(() => ({ operation: null, probeFailed: true })),
      // Same fail-soft-but-honest treatment as the operation probe: an empty
      // stash list and an unreadable one must not render the same, because a
      // forgotten stash is work that exists nowhere else.
      invokeFn<unknown>("cmd_stash_list", { repoPath: path })
        .then((raw) => ({ ...parseStashList(raw), failed: false }))
        .catch(() => ({ entries: [] as StashEntry[], failed: true, truncated: false })),
      invokeFn<number | null>("cmd_last_fetch_at", { repoPath: path })
        .then((value) => (typeof value === "number" ? value : null))
        .catch(() => null),
    ]);
    const currentBranch = branches.find((b) => b.is_current)?.name || null;
    const defaultBranch =
      branches.find((b) => b.is_default)?.name || currentBranch || "main";
    return {
      branches,
      statuses,
      tags: tags.tags,
      currentBranch,
      defaultBranch,
      operation,
      stashEntries: stash.entries,
      stashFailed: stash.failed,
      stashTruncated: stash.truncated,
      tagsTruncated: tags.truncated,
      tagsFailed: tags.failed,
      fetchedAt,
    };
  }

  /**
   * Starts the filesystem watch and REPORTS the outcome.
   *
   * Still best-effort in the sense that a failure never blocks opening a
   * repository — but the failure is no longer invisible. `cmd_watch_repo`
   * fails for ordinary reasons (the backend's watch table is full, the
   * platform refuses another inotify handle), and a repository with no watcher
   * silently stops receiving branch, graph, operation and stash updates while
   * looking exactly like one that is live.
   */
  async function watch(path: string): Promise<WatchState> {
    try {
      await invokeFn("cmd_watch_repo", { repoPath: path });
      return WATCH_ACTIVE;
    } catch (err: unknown) {
      return watchFailed(err);
    }
  }

  /**
   * Tells the backend the user just brought `path` to the front, so the
   * hydrate that follows is admitted like the user action it is instead of
   * queuing behind background refreshes (`cmd_note_tab_activated`). Only for
   * activations a person caused: restore and background opens stay ordinary
   * reads, or a restart would hand every restored tab a free burst.
   *
   * Best effort. A refusal here is the same refusal the hydrate meets next,
   * and the hydrate is what reports it.
   */
  async function noteTabActivated(path: string): Promise<void> {
    try {
      await invokeFn("cmd_note_tab_activated", { repoPath: path });
    } catch {
      /* reported by the hydrate that follows */
    }
  }

  /**
   * The open repositories holding a native watch, by tab id. Tabs are not
   * bounded by watches any more; this is (see repos/watchPool.ts).
   */
  const watchPool = createWatchPool();
  /**
   * One queue of native watch calls per tab, so the backend ends up holding
   * the last decision made rather than whichever call landed last: a tab
   * parked and brought back while its unwatch is in flight must not be left
   * unwatched behind a pool that thinks it is live.
   */
  const watchOps = new Map<string, Promise<void>>();

  function serialWatch(id: string, op: () => Promise<void>): Promise<void> {
    const run = (watchOps.get(id) ?? Promise.resolve()).then(op);
    const tail = run.catch(() => {});
    watchOps.set(id, tail);
    void tail.then(() => {
      if (watchOps.get(id) === tail) watchOps.delete(id);
    });
    return run;
  }

  /**
   * Watch status is a fact about the backend, not about one load of the
   * view, so it lands on the live session whatever its generation.
   */
  function setWatch(id: string, state: WatchState): void {
    const session = internal.sessions[id];
    if (session) applyToSession(id, session.generation, { watch: state });
  }

  /**
   * The one way into the pool. A slot whose tab is gone (re-keyed, closed by
   * a path that skipped `unwatch`, or dropped by a restore that replaced
   * every session) would otherwise shrink the pool for good.
   */
  function admitWatch(id: string, foreground: boolean) {
    for (const key of watchPool.keys()) {
      if (!internal.sessions[key] && key !== id) watchPool.release(key);
    }
    return watchPool.admit(id, foreground);
  }

  /**
   * Gives a tab a live watch when the pool admits it, and records the
   * outcome — watching, degraded or parked — on its session. `foreground` is
   * a repository a person is looking at: it always gets a slot, taking the
   * least recently used one; anything else takes only a free slot, so a
   * background open never steals a watch from a repository someone used.
   *
   * A watch that fails keeps its slot. Releasing it would let the active
   * tab's periodic re-assert evict another repository every minute for as
   * long as its own watch keeps failing.
   */
  async function acquireWatch(id: string, path: string, foreground: boolean): Promise<void> {
    const { admitted, evicted } = admitWatch(id, foreground);
    if (!admitted) {
      setWatch(id, WATCH_PARKED);
      return;
    }
    // The newcomer's watch is asked for first, and never waits on the
    // evicted one's teardown (an unwatch waits for its callback to release):
    // the backend's table is larger than the pool. The eviction is recorded
    // now, not after that watch lands, or a tab re-admitted in between would
    // be marked parked while it holds a live watch.
    const admission = serialWatch(id, async () => {
      if (!watchPool.has(id)) return;
      const state = await watch(path);
      if (watchPool.has(id)) setWatch(id, state);
    });
    if (evicted) void parkWatch(evicted);
    await admission;
  }

  /** Releases an evicted tab's native watch and says it is parked. */
  async function parkWatch(id: string): Promise<void> {
    const path = internal.sessions[id]?.path;
    if (!path || watchPool.has(id)) return;
    setWatch(id, WATCH_PARKED);
    // Nothing will refresh it until it is shown again.
    watcherRefreshPolicy.forget(path);
    await serialWatch(id, async () => {
      if (!watchPool.has(id)) await unwatchNative(path);
    });
  }

  async function unwatchNative(path: string): Promise<void> {
    try {
      await invokeFn("cmd_unwatch_repo", { repoPath: path });
    } catch {
      /* unwatch is best-effort */
    }
  }

  async function unwatch(path: string) {
    // A refresh still owed to a repo nobody watches would run against a
    // session that is gone, or worse, one reopened since.
    watcherRefreshPolicy.forget(path);
    const id = identityKey(path, options);
    watchPool.release(id);
    await serialWatch(id, () => unwatchNative(path));
  }

  /**
   * The one routine every tab-close route ends in (closeTab, closeOtherTabs,
   * closeTabsToTheRight): forgets the tab's per-id and per-path records
   * (`sessionBook.ts`), drops its graph and stops its watch, and lets the
   * harness store drop the repository's ledger cache once no other tab shows
   * it. Call it after the tab is out of `internal.sessions`.
   */
  async function releaseSession(id: string, path: string | null): Promise<void> {
    sessionBook.forget(id, path);
    if (path === null) return;
    const stillOpen = Object.values(internal.sessions).some((other) => sameRepo(other.path, path, options));
    if (!stillOpen) harnessStore.forgetRepository(path);
    graph.evict(path);
    await unwatch(path);
  }

  async function resolvePath(path: string): Promise<ResolvedRepo> {
    return invokeFn<ResolvedRepo>("cmd_resolve_repo", { repoPath: path });
  }

  /**
   * What a successful resolve says about a session — the one place those
   * fields are copied, so no path that resolves (open, a trust grant, a
   * re-resolve on hydrate) can update the path and forget the family.
   */
  function resolvedFields(id: string, resolved: ResolvedRepo) {
    familyUnknown.delete(id);
    return {
      path: resolved.path,
      name: resolved.name,
      isBare: resolved.is_bare,
      commonDir: resolved.common_dir ?? null,
    };
  }

  /**
   * Folds live churn (branch stats merged after previous drains) into a fresh
   * snapshot so content-identical cycles compare equal. The backend snapshot
   * carries bare branches; without this merge every refresh differed from the
   * enriched live state by `compared_to`/churn fields alone and republished
   * forever. A branch keeps its live object when name+remote+tip all match —
   * the same identity the stats drain merges under — and otherwise takes the
   * snapshot's (tip moved ⇒ stale churn must not survive).
   */
  function withCarriedChurn(
    live: RepoSession,
    snapshot: {
      branches: BranchInfo[];
      statuses: FileStatus[];
      tags: TagInfo[];
      currentBranch: string | null;
      defaultBranch: string | null;
      operation: OperationState;
      stashEntries: StashEntry[];
      stashFailed: boolean;
      stashTruncated: boolean;
      tagsTruncated: boolean;
      tagsFailed: boolean;
      fetchedAt: number | null;
    },
  ) {
    if (live.branches.length === 0) return snapshot;
    const key = (b: Pick<BranchInfo, "name" | "is_remote" | "remote_name">) =>
      `${b.is_remote ? "remote" : "local"}:${b.remote_name ?? ""}:${b.name}`;
    const liveByKey = new Map(live.branches.map((b) => [key(b), b]));
    const branches = snapshot.branches.map((b) => {
      const carried = liveByKey.get(key(b));
      return carried && carried.tip_commit_id === b.tip_commit_id ? carried : b;
    });
    return { ...snapshot, branches };
  }

  /**
   * Asks the backend which repository a session belongs to when no resolve
   * has answered yet. A failure leaves it listed for the next hydrate; it
   * never retries on its own.
   */
  async function learnFamily(id: string, path: string, generation: number) {
    if (!familyUnknown.has(id)) return;
    familyUnknown.delete(id);
    let resolved: ResolvedRepo;
    try {
      resolved = await resolvePath(path);
    } catch {
      if (internal.sessions[id]) familyUnknown.add(id);
      return;
    }
    const session = internal.sessions[id];
    if (!session || session.generation !== generation) {
      if (session) familyUnknown.add(id);
      return;
    }
    // The path stays the tab's: a canonical spelling that differs is an alias
    // only openRepo may adopt, since it re-keys the tab.
    const { commonDir, name, isBare } = resolvedFields(id, resolved);
    applyToSession(id, generation, { commonDir, name, isBare });
  }

  /**
   * Asks the person to trust a tab that is waiting for it, and on a grant
   * loads it — wherever it is. The one owner of that sequence: a click on the
   * tab (activateTab) and an agent waiting on its checkout from the task sheet
   * (trustTab) both come here, so a grant always carries the family and only
   * the tab on screen counts as an activation.
   */
  async function grantTrust(id: string): Promise<boolean> {
    const current = internal.sessions[id];
    if (!current?.trustRequired) return false;
    const trustedPath = await requestRepositoryTrust(current.path, "Trust and Open", invokeFn);
    const live = internal.sessions[id];
    if (!live?.trustRequired) return false;
    if (!trustedPath) return false;
    let granted: ReturnType<typeof resolvedFields>;
    try {
      granted = resolvedFields(id, await resolvePath(trustedPath));
    } catch (err: unknown) {
      const message = formatError(err);
      applyToSession(id, live.generation, {
        isLoading: false,
        error: message,
        trustRequired: repositoryTrustRefused(message) || live.trustRequired,
      });
      return false;
    }
    const latest = internal.sessions[id];
    if (!latest) return false;
    // The grant is the first resolve this tab has had: it carries the
    // family, not only the path. Copying the path alone left every
    // checkout restored untrusted outside its repository's stack.
    const path = granted.path;
    const activation = bumped({ ...latest, ...granted });
    const onScreen = internal.workspace.activeId === id;
    putSession({ ...activation, isLoading: true });
    if (onScreen) {
      syncFilterFromSession(activation);
      revealGraph(activation);
    }
    publish();
    await acquireWatch(id, path, onScreen);
    if (onScreen) {
      watcherRefreshPolicy.onActivated(path);
      await noteTabActivated(path);
    }
    await hydrate(id, path, activation.generation);
    const after = internal.sessions[id];
    if (after && after.generation === activation.generation && !after.trustRequired) {
      ensureStatusPoll();
      flushPersist();
    }
    return true;
  }

  /**
   * Reads the tab's snapshot. At most one runs per tab: a request while one is
   * running does not start a second, it is answered by one trailing run that
   * every request meanwhile shares (`hydrateFlight.ts`). Every caller — a
   * watcher event, a poll, an activation, a mutation's refresh — comes through
   * here, so the cost of a burst of them is one snapshot plus one more.
   */
  function hydrate(id: string, path: string, generation: number): Promise<void> {
    return sessionBook.hydrates.run(id, generation, () => hydrateNow(id, path, generation));
  }

  /**
   * Whether the tab's snapshot is recent enough that activating it need not
   * read again: it landed within {@link ACTIVATION_FRESH_MS}, the tab holds a
   * live watch (a degraded or parked one reports nothing), the last read did
   * not fail, and no read is in flight — that one belongs to the epoch being
   * left, and the store would discard its answer.
   */
  function snapshotIsFresh(id: string, generation: number): boolean {
    const session = internal.sessions[id];
    const landedAt = sessionBook.hydratedAt.get(id);
    return (
      !!session &&
      session.generation === generation &&
      session.hasHydrated &&
      !session.error &&
      !session.trustRequired &&
      !needsFullPoll(session.watch) &&
      landedAt !== undefined &&
      Date.now() - landedAt < ACTIVATION_FRESH_MS &&
      sessionBook.hydrates.current(id) === null
    );
  }

  async function hydrateNow(id: string, path: string, generation: number) {
    // A trailing run starts after the tab may have been closed.
    if (!internal.sessions[id]) return;
    const run = (snapshotRuns.get(id) ?? 0) + 1;
    snapshotRuns.set(id, run);
    // Synchronous unless there is something to learn: an extra yield on every
    // hydrate would reorder the snapshot fetches the run token arbitrates.
    if (familyUnknown.has(id)) {
      await learnFamily(id, path, generation);
      if (snapshotRuns.get(id) !== run) return;
    }
    try {
      const raw = await loadSnapshot(path);
      if (snapshotRuns.get(id) !== run) return;
      const live = internal.sessions[id];
      const snapshot = live ? withCarriedChurn(live, raw) : raw;
      applyToSession(id, generation, {
        ...snapshot,
        isLoading: false,
        error: null,
        trustRequired: false,
        missing: false,
        hasHydrated: true,
      });
      // Stats re-drain even when the snapshot was a no-op: a previously
      // failed drain must retry on the next refresh, and the backend
      // memoizes per-tip so an unchanged repo costs one cheap call.
      if (internal.sessions[id]?.generation === generation) {
        sessionBook.hydratedAt.set(id, Date.now());
        contentRevisions.update((revisions) => ({
          ...Object.fromEntries(Object.values(internal.sessions)
            .filter((session) => revisions[session.path] !== undefined)
            .map((session) => [session.path, revisions[session.path]])),
          [path]: `${generation}:${run}`,
        }));
        void fetchBranchStats(id, path, generation);
      }
    } catch (err: unknown) {
      if (snapshotRuns.get(id) !== run) return;
      const message = formatError(err);
      // A deferral reaching here has already been retried through the whole
      // backoff by src/lib/ipc/invoke.ts. The snapshot already rendered stays;
      // the toast and diagnostics show the deferral as a warning, not an error.
      applyToSession(id, generation, {
        isLoading: false,
        error: message,
        ...(repositoryTrustRefused(message) ? { trustRequired: true } : {}),
      });
    }
  }

  // --- progressive branch churn ------------------------------------------
  // Churn arrives via cmd_branch_stats after the snapshot renders. One logical
  // fetch per session at a time: capped reports re-invoke inside the same
  // in-flight slot until drained, and a refresh racing one lets it finish —
  // the tip guard keeps stale merges out, and the next refresh fetches again.

  function branchStatsKey(
    name: string,
    isRemote: boolean,
    remoteName?: string | null,
  ): string {
    return `${isRemote ? "remote" : "local"}:${remoteName ?? ""}:${name}`;
  }

  /**
   * What the churn of a branch list was measured against: every branch and the
   * tip it points at. Churn is relative to the default branch, so a moved
   * default invalidates every other row's numbers even though their own tips
   * did not move — which is why the whole list is the signature, not each row's
   * tip on its own.
   */
  function branchTipSignature(branches: readonly BranchInfo[]): string {
    return branches
      .map((b) => `${branchStatsKey(b.name, b.is_remote, b.remote_name)}@${b.tip_commit_id}${b.is_default ? "*" : ""}`)
      .sort()
      .join("\n");
  }

  async function fetchBranchStats(
    id: string,
    path: string,
    generation: number,
  ) {
    if (statsInflight.has(id)) return;
    // Nothing to ask when the last drain finished cleanly and no tip has moved
    // since: the answer is what the rows already carry. A refresh that changed
    // only the work tree, or nothing at all, used to cost a backend call (and
    // its refs listing) every time.
    const known = internal.sessions[id];
    if (
      known &&
      known.generation === generation &&
      !known.statsFailed &&
      statsDrained.get(id) === branchTipSignature(known.branches)
    ) {
      return;
    }
    statsInflight.add(id);
    // Raise the churn marker only when some branch actually misses stats
    // (or the last attempt failed): after a completed drain every rendered
    // row carries stats, and flipping the flag on every refresh cycle made
    // those markers blink on quiet repos.
    const liveNow = internal.sessions[id];
    if (
      liveNow &&
      liveNow.generation === generation &&
      (liveNow.statsFailed ||
        liveNow.branches.some((b) => b.compared_to === undefined))
    ) {
      applyToSession(id, generation, { statsPending: true });
    }
    const start = internal.sessions[id];
    if (!start || start.generation !== generation) {
      statsInflight.delete(id);
      return;
    }
    // The list the backend is being asked about. A refresh that moves a tip
    // while the drain runs leaves that tip without numbers, so the drain may
    // only vouch for the list it started with.
    const startSignature = branchTipSignature(start.branches);
    // Stats are collected here and merged into the LIVE branch list at each
    // publish. Writing back a copy of the list as it stood when the drain
    // began reverted a refresh that landed meanwhile: a deleted branch came
    // back and a new one vanished.
    const collected = new Map<string, { tip: string; stats: Partial<BranchInfo> }>();
    const withCollectedStats = (list: BranchInfo[]): BranchInfo[] =>
      list.map((branch) => {
        const hit = collected.get(branchStatsKey(branch.name, branch.is_remote, branch.remote_name));
        return hit && hit.tip === branch.tip_commit_id ? { ...branch, ...hit.stats } : branch;
      });
    let dirty = false;
    // `pending` keeps the in-flight marker lit across intermediate publishes;
    // `failed` lands only on the final settle so a mid-drain hiccup that a
    // later batch recovers from never flashes the failure marker.
    const flush = (pending: boolean, failed: boolean) => {
      // A settle that changes nothing must not publish: the pending/failed
      // flip is what makes sidebar churn markers blink on every refresh.
      const live = internal.sessions[id];
      if (
        !dirty &&
        live &&
        live.generation === generation &&
        live.statsPending === pending &&
        live.statsFailed === failed
      ) {
        return;
      }
      if (dirty) {
        applyToSession(id, generation, (current) => ({
          branches: withCollectedStats(current.branches),
          statsPending: pending,
          statsFailed: failed,
        }));
        dirty = false;
        return;
      }
      applyToSession(id, generation, {
        statsPending: pending,
        statsFailed: failed,
      });
    };
    try {
      // Only the LAST batch's failure count matters: uncached failures are
      // retried every round, so an early hiccup a later batch recovered from
      // must not taint the settle.
      let lastBatchHadFailures = false;
      let drainedCleanly = false;
      for (let batch = 0; batch < STATS_DRAIN_MAX_BATCHES; batch += 1) {
        const report = await invokeFn<BranchStatsReport>("cmd_branch_stats", {
          repoPath: path,
        });
        const session = internal.sessions[id];
        if (!session || session.generation !== generation) return;

        lastBatchHadFailures = report.compute_failures > 0;

        const updates = new Map(
          report.updates.map((update) => [
            branchStatsKey(update.name, update.is_remote, update.remote_name),
            update,
          ]),
        );
        let batchTouched = false;
        for (const [key, update] of updates) {
          collected.set(key, {
            tip: update.tip_commit_id,
            stats: {
              additions: update.additions,
              deletions: update.deletions,
              files_changed: update.files_changed,
              commits_ahead_of_base: update.commits_ahead_of_base,
              commits_behind_base: update.commits_behind_base,
              compared_to: report.compared_to,
            },
          });
        }
        for (const branch of session.branches) {
          const update = updates.get(branchStatsKey(branch.name, branch.is_remote, branch.remote_name));
          if (update && update.tip_commit_id === branch.tip_commit_id) batchTouched = true;
        }
        if (batchTouched) dirty = true;
        const drained = !report.capped;
        if (dirty && (drained || (batch + 1) % STATS_PUBLISH_EVERY === 0)) {
          flush(report.capped, false);
        }
        if (drained) {
          drainedCleanly = true;
          break;
        }
      }
      // Clean drain retires any earlier failure marker; exhausting the batch
      // bound or losing walks to errors must NOT read as success — BranchList
      // renders its "churn unavailable" marker off statsFailed.
      flush(false, !drainedCleanly || lastBatchHadFailures);
      const settled = internal.sessions[id];
      if (
        settled &&
        settled.generation === generation &&
        drainedCleanly &&
        !lastBatchHadFailures &&
        branchTipSignature(settled.branches) === startSignature
      ) {
        statsDrained.set(id, startSignature);
      } else {
        statsDrained.delete(id);
      }
    } catch {
      statsDrained.delete(id);
      // Final failure: stop posing zeros as data — BranchList renders its
      // "churn unavailable" marker off statsFailed instead.
      flush(false, true);
    } finally {
      statsInflight.delete(id);
    }
  }
  // ------------------------------------------------------------------------

  // --- watcher-storm coalescing -------------------------------------------
  // One trailing debounce per changed repo path; a later event re-arms the
  // same timer instead of queueing overlapping refreshes.
  const watcherRefreshTimers = new Map<string, Debounced<[]>>();

  function scheduleWatcherRefresh(key: string, run: () => void) {
    let timer = watcherRefreshTimers.get(key);
    if (!timer) {
      timer = debounce(run, WATCHER_REFRESH_DEBOUNCE_MS);
      watcherRefreshTimers.set(key, timer);
    }
    timer();
  }

  // Which watcher events may cost a refresh, and when: the active tab of a
  // shown window at once, background tabs on a bounded schedule, nothing while
  // hidden. See watcherRefresh.ts for why the cost had to stop scaling with
  // the number of busy tabs.
  const watcherRefreshPolicy = createWatcherRefreshPolicy({
    now: () => Date.now(),
    setTimer: (run, delayMs) => setTimeout(run, delayMs),
    clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    isActive: (path) => {
      const active = activeSession();
      return !!active && sameRepo(active.path, path, options);
    },
    isHidden: readHiddenDocument,
    refresh: (path) => store.refresh(path),
  });
  let unbindRefreshVisibility: (() => void) | null = null;
  function bindRefreshVisibility(): void {
    if (unbindRefreshVisibility !== null || typeof document === "undefined") return;
    unbindRefreshVisibility = bindForegroundChanges(
      document,
      typeof window === "undefined" ? null : window,
      () => watcherRefreshPolicy.onVisibilityChange(),
    );
  }

  const mutationActivity = writable<Record<string, string[]>>({});
  const mutations = new Map<number, { path: string; kind: string }>();
  let mutationSequence = 0;
  function beginMutation(path: string, kind: string): () => void {
    const token = ++mutationSequence;
    const publishActivity = () => {
      const activity: Record<string, string[]> = {};
      for (const entry of mutations.values()) {
        (activity[entry.path] ??= []).push(entry.kind);
      }
      mutationActivity.set(activity);
    };
    mutations.set(token, { path, kind });
    publishActivity();
    return () => { mutations.delete(token); publishActivity(); };
  }

  /**
   * The one echo rule. Our own `.git` writes bounce back from the watcher, and
   * they are dropped (the mutation refreshes explicitly) while a mutation holds
   * the path and for WATCHER_ECHO_SUPPRESS_MS after one SUCCEEDED. A mutation
   * that failed leaves no window: it never refreshes, so the watcher is what
   * shows what it left behind.
   *
   * An event dropped while a mutation HOLDS the path is remembered, because
   * the mutation may end without refreshing (a failure, a `skipRefresh`
   * success, a refusal after queueing); `replayDroppedEchoes` then hands it to
   * the refresh policy so a change made during the hold is not lost.
   */
  function holdsPath(path: string): boolean {
    for (const entry of mutations.values()) if (entry.path === path) return true;
    return false;
  }
  const droppedWhileHeld = new Set<string>();
  function isEchoOfOwnWrite(path: string): boolean {
    if (holdsPath(path)) {
      droppedWhileHeld.add(path);
      return true;
    }
    const until = mutationEchoUntil.get(path);
    return until !== undefined && Date.now() < until;
  }
  /** Called when a mutation ends: replays a dropped event if nothing refreshed after it. */
  function replayDroppedEchoes(path: string, refreshed: boolean): void {
    if (holdsPath(path) || !droppedWhileHeld.delete(path) || refreshed) return;
    const session = Object.values(internal.sessions).find((item) => sameRepo(item.path, path, options));
    if (!session || session.trustRequired) return;
    bindRefreshVisibility();
    watcherRefreshPolicy.onChange(session.path);
  }

  /**
   * One chain of mutations per repository path. Two git commands writing the
   * same repository at once (a bulk pull and a single pull, two commits) race
   * on its index and refs, so each waits for the one before it. Another
   * repository's chain is never involved, so a hung mutation holds up only its
   * own path.
   */
  const pathQueues = new Map<string, Promise<void>>();
  function enterPathQueue(path: string): Promise<() => void> {
    const previous = pathQueues.get(path) ?? Promise.resolve();
    let release!: () => void;
    const mine = new Promise<void>((resolve) => { release = resolve; });
    const tail = previous.then(() => mine);
    pathQueues.set(path, tail);
    void tail.then(() => { if (pathQueues.get(path) === tail) pathQueues.delete(path); });
    return previous.then(() => release);
  }

  const store = {
    subscribe,
    /**
     * The active repository's path, and nothing else.
     *
     * The store publishes a fresh object for every change — a status poll, a
     * loading flag, an error — so an `$effect` that reads `$repoStore.x`
     * re-runs on all of them: it re-lists, re-fetches, resubscribes, cancels
     * its own load mid-flight and resets the reader's local state. A derived
     * string is compared by value, so this notifies only when the active
     * repository changes. It is the one key effects use for "which
     * repository"; `repoStoreEffects.contract.test.ts` keeps `$repoStore`
     * out of effect bodies.
     */
    currentPath: derived({ subscribe }, (state) => state.currentPath),
    /** The active tab's commit message; see `commitDraftStore`. */
    commitDraft: commitDrafts,
    mutationActivity: { subscribe: mutationActivity.subscribe },
    /**
     * The active repository as a mutation target, or null with none open.
     *
     * A flow that asks before it acts — a confirm, a name prompt, an Undo
     * toast — captures this BEFORE it awaits and hands it to the mutation, so
     * the action lands on the repository that was asked about, or is refused
     * when that repository changed.
     */
    captureTarget: (): MutationTarget | null => {
      const session = activeSession();
      return session ? { tabId: session.id, path: session.path } : null;
    },
    contentRevisions: { subscribe: contentRevisions.subscribe },
    setError: (error: string | null) => {
      // Every user-facing error funnels through here; mirror it into the
      // diagnostics log so the banner's dismissal never loses it.
      if (error) diagnostics.error("repo", error);
      const session = activeSession();
      // Publishing an unchanged error retriggers every $repoStore subscriber,
      // including App's forwarding $effect that calls setError to clear —
      // which is the freeze that shipped as effect_update_depth_exceeded.
      if (
        internal.workspaceError === error &&
        (session === undefined || session.error === error)
      ) {
        return;
      }
      if (session) {
        putSession({ ...session, error });
      }
      internal = { ...internal, workspaceError: error };
      publish();
    },
    trustRepo: (path: string) => requestRepositoryTrust(path, "Trust Repository", invokeFn),
    /**
     * Asks to trust a tab waiting for it without bringing it on screen — for
     * a surface (the task sheet) whose reader is not looking at that tab.
     * Resolves to whether trust was granted and the tab loaded.
     */
    trustTab: (id: string) => grantTrust(id),
    /** Per-tab and per-path records held beyond the sessions themselves; zero once every tab is closed. */
    retainedSessionRecords: (): number => sessionBook.size(),
    /**
     * Why `openRepo(rawPath)` would be refused before anything is resolved,
     * or null. Only refusals that waiting cannot fix are knowable here: an
     * invalid path, or every tab slot taken by other repositories. Same rule
     * and words as the refusal itself, so a caller never re-derives the bound.
     */
    openRefusal: (rawPath: string): string | null => {
      const probe = openTab(internal.workspace, rawPath, options, { activate: false });
      return probe.ok ? null : openRefusalMessage(probe.reason);
    },
    /**
     * The repository family a checkout belongs to (see repos/repoFamily.ts),
     * without opening a tab for it: an open tab's own answer, else the
     * backend's resolve. Null when it cannot be read — no repository, not
     * trusted, or a failed resolve — which callers treat as "stands alone",
     * never as a guess.
     */
    familyOf: async (rawPath: string): Promise<string | null> => {
      const key = identityKey(rawPath, options);
      if (!key) return null;
      const open = internal.workspace.tabs.find((tab) => tab.id === key);
      if (open) {
        const known = familyFromCommonDir(internal.sessions[open.id]?.commonDir, options);
        if (known) return known.key;
      }
      try {
        const resolved = await resolvePath(rawPath);
        return familyFromCommonDir(resolved?.common_dir, options)?.key ?? null;
      } catch {
        return null;
      }
    },
    openRepo: async (
      rawPath: string,
      extras: {
        allowBroken?: boolean;
        /** Runs only while this successfully hydrated open still owns navigation. */
        onReady?: (canonicalPath: string) => void;
        activate?: boolean;
        pinned?: boolean;
        /** Set when opening a tab that already belongs to a group. */
        group?: string | null;
        /** Own tab color. Omitted leaves whatever the open tab already has. */
        color?: TabColor | null;
        /** Restore must not advance the epoch; a partial walk cannot shrink the saved list. */
        keepEpoch?: boolean;
        /**
         * Create, resolve and watch the tab, but leave the snapshot to the
         * caller. Restore uses it to put every tab — and so every family —
         * on the strip before the first git read.
         */
        skipHydrate?: boolean;
        /**
         * Keep the tab when the repository is not trusted yet, without
         * prompting. Restore uses this so closing the app cannot drop every
         * repository that would have asked.
         */
        deferTrust?: boolean;
        restore?: {
          viewTab?: ViewTab;
          viewSections?: Record<string, string>;
          searchQuery?: string;
          selectedBranch?: string | null;
          terminalOpen?: boolean;
        };
      } = {},
    ) => {
      // Only an open that may take the screen is navigation. A background
      // open (a task agent's checkout, a restored tab) bumping the epoch
      // cancelled whatever the reader was opening at that moment.
      const requestId = extras.activate === false ? openEpoch : ++openEpoch;
      internal = { ...internal, workspaceError: null };
      let resolved: ResolvedRepo | null = null;
      let deferredTrustMessage: string | null = null;
      try {
        resolved = await resolvePath(rawPath);
      } catch (err: unknown) {
        const message = formatError(err);
        const refused = repositoryTrustRefused(message);
        if (refused && !extras.deferTrust) {
          try {
            const trustedPath = await requestRepositoryTrust(rawPath, "Trust and Open", invokeFn);
            if (!trustedPath) return false;
            resolved = await resolvePath(trustedPath);
          } catch (trustError: unknown) {
            internal = { ...internal, workspaceError: formatError(trustError) };
            publish();
            return false;
          }
        } else if (refused) {
          deferredTrustMessage = message;
        } else if (!extras.allowBroken) {
          internal = { ...internal, workspaceError: formatError(err) };
          publish();
          return false;
        }
      }
      const path = resolved?.path ?? rawPath;
      const activate =
        extras.activate === false ? false : requestId === openEpoch;
      // Canonical identity: once cmd_resolve_repo succeeds, resolved.path is
      // THE identity for this repository — tabs, sessions, graph cache, and
      // watchers all key off it. A tab can still sit under the pre-canonical
      // string when a restore entry was opened while the path was broken;
      // adopting it here keeps one physical repo to one tab instead of a
      // duplicate watcher, a split graph cache, and change events that match
      // nothing. Until resolution succeeds, restore entries stay
      // string-normalized only.
      let workspace = internal.workspace;
      let carriedSession: RepoSession | undefined;
      let carriedId: string | undefined;
      const rawKey = identityKey(rawPath, options);
      const canonicalKey = identityKey(path, options);
      if (resolved && rawKey && canonicalKey && rawKey !== canonicalKey) {
        const aliases = workspace.tabs.filter(
          (tab) =>
            tab.id !== canonicalKey &&
            identityKey(tab.path, options) === rawKey,
        );
        if (aliases.length > 0) {
          const aliasIds = new Set(aliases.map((tab) => tab.id));
          const aliasPaths = new Map(aliases.map((tab) => [tab.id, tab.path]));
          for (const alias of aliases) {
            if (internal.sessions[alias.id]) {
              carriedSession = internal.sessions[alias.id];
              carriedId = alias.id;
            }
            // Broken aliases never loaded a graph, but showRepo may have been
            // pointed at the alias string on activation.
            graph.evict(alias.path);
          }
          workspace = {
            ...workspace,
            tabs: workspace.tabs.filter((tab) => !aliasIds.has(tab.id)),
            // A removed alias must not remain the active pointer; this open
            // is the natural successor even without an activate request.
            activeId:
              workspace.activeId !== null && aliasIds.has(workspace.activeId)
                ? canonicalKey
                : workspace.activeId,
            recents: workspace.recents.filter(
              (item) => identityKey(item, options) !== rawKey,
            ),
            lastClosed: workspace.lastClosed.filter(
              (item) => identityKey(item, options) !== rawKey,
            ),
          };
          const sessions = { ...internal.sessions };
          for (const id of aliasIds) {
            delete sessions[id];
            // An alias spelling is not a closed repository: its journal key
            // resolves to the surviving tab, so only the tab's own records go.
            sessionBook.forget(id, aliasPaths.get(id) ?? null);
          }
          internal = { ...internal, sessions };
        }
      }
      const hadCanonical = workspace.tabs.some(
        (tab) => tab.id === canonicalKey,
      );
      const carriedPinned = hadCanonical ? undefined : carriedSession?.pinned;
      const opened = openTab(workspace, path, options, {
        pinned: extras.pinned ?? carriedPinned,
        activate,
        ...(extras.group !== undefined ? { group: extras.group } : {}),
        ...(extras.color !== undefined ? { color: extras.color } : {}),
      });
      if (!opened.ok) {
        internal = {
          ...internal,
          workspaceError: openRefusalMessage(opened.reason),
        };
        publish();
        return false;
      }
      if (!extras.keepEpoch && opened.created) commitEdit();
      replaceWorkspace({
        ...opened.workspace,
        lastClosed: opened.workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      // A restore keeps the order it was saved in; anything else opened
      // beside an open checkout of the same repository lands next to it.
      if (opened.created && !extras.keepEpoch) {
        placeBesideFamily(opened.id, resolved?.common_dir, extras.group !== undefined, activate);
      }
      const existing = internal.sessions[opened.id];
      // A fresh resolve is the answer, including "could not read it"; a
      // failed one keeps what the last good resolve said, and leaves a family
      // never learned for the next hydrate to ask about.
      const fromResolve = resolved ? resolvedFields(opened.id, resolved) : null;
      if (!resolved && !existing?.commonDir) familyUnknown.add(opened.id);
      const session = existing
        ? {
            ...bumped(existing),
            path,
            name: fromResolve?.name ?? existing.name,
            isBare: fromResolve?.isBare ?? existing.isBare,
            commonDir: fromResolve ? fromResolve.commonDir : existing.commonDir,
            pinned:
              opened.workspace.tabs.find((tab) => tab.id === opened.id)
                ?.pinned ?? existing.pinned,
            isLoading: resolved ? !existing.hasHydrated : false,
            error: resolved
              ? null
              : deferredTrustMessage ??
                String(internal.workspaceError ?? "Repository is unavailable"),
            trustRequired: resolved
              ? false
              : deferredTrustMessage !== null || existing.trustRequired,
          }
        : createSession(
            {
              id: opened.id,
              path,
              pinned: (extras.pinned ?? carriedPinned) === true,
            },
            {
              name: fromResolve?.name,
              isBare: fromResolve?.isBare,
              commonDir: fromResolve?.commonDir ?? null,
              // An adopted alias tab hands over its state; an explicit
              // restore payload always wins over what the alias carried.
              activeTab: extras.restore?.viewTab ?? carriedSession?.activeTab,
              viewSections:
                extras.restore?.viewSections ?? carriedSession?.viewSections,
              searchQuery:
                extras.restore?.searchQuery ?? carriedSession?.searchQuery,
              selectedBranch:
                extras.restore?.selectedBranch ??
                carriedSession?.selectedBranch,
              terminalOpen:
                extras.restore?.terminalOpen ?? carriedSession?.terminalOpen,
              isAmending: carriedSession?.isAmending ?? false,
              isLoading: Boolean(resolved),
              error: resolved
                ? null
                : deferredTrustMessage ?? "Repository is unavailable",
              trustRequired: deferredTrustMessage !== null,
            },
          );
      // The draft follows the adopted alias the way the rest of its session
      // does: only into a tab created for it, never over an existing one.
      if (!existing && carriedId !== undefined) commitDrafts.move(carriedId, opened.id);
      putSession(session);
      const shouldPresent =
        activate && internal.workspace.activeId === opened.id;
      if (shouldPresent) {
        syncFilterFromSession(session);
        graph.showRepo(session.path);
      }
      publish();
      if (!resolved) return true;
      // An open that takes the screen always renders; only a background one
      // may wait — and restore's background opens wait for the watch too,
      // which restore hands out once the active tab is presented, within
      // the pool, rather than one native registration per tab up front.
      if (extras.skipHydrate && !shouldPresent) return true;
      // Recorded before the hydrate so the first snapshot already carries an
      // honest live/degraded/parked answer, rather than briefly claiming live
      // updates for a repository that never got a watcher.
      await acquireWatch(opened.id, path, shouldPresent);
      if (shouldPresent && !extras.keepEpoch) await noteTabActivated(path);
      await hydrate(opened.id, path, session.generation);
      const latest = internal.sessions[opened.id];
      if (
        shouldPresent &&
        latest &&
        internal.workspace.activeId === opened.id
      ) {
        revealGraph(latest);
      }
      if (shouldPresent && requestId === openEpoch && latest &&
          latest.generation === session.generation && !latest.error &&
          latest.activeTab === session.activeTab && latest.viewSections === session.viewSections &&
          latest.selectedFilePath === session.selectedFilePath && latest.selectedCommitId === session.selectedCommitId &&
          latest.selectedDiffPending === session.selectedDiffPending &&
          internal.workspace.activeId === opened.id) extras.onReady?.(path);
      ensureStatusPoll();
      flushPersist();
      return true;
    },
    pickAndOpenRepo: async () => {
      try {
        const folder = await invokeFn<string | null>("cmd_pick_folder");
        if (folder) {
          await store.openRepo(folder);
        }
      } catch (err: unknown) {
        internal = { ...internal, workspaceError: formatError(err) };
        publish();
      }
    },
    revokeTrust: async (id: string) => {
      const session = internal.sessions[id];
      if (!session) return;
      const approved = await askConfirm({
        title: "Revoke repository trust?",
        message: `${session.path}\n\nThis closes the tab and blocks new GitPulse operations. Trust was granted to the repository, so this revokes it for every worktree of it, not only this checkout: any other tab open on the same repository stays open but stops working until it is trusted again. Already-running terminals and agent tasks retain their permissions; stop them separately if needed.`,
        confirmLabel: "Revoke Trust",
        destructive: true,
      });
      if (!approved) return;
      try {
        await invokeFn("cmd_revoke_repository_trust", { repoPath: session.path });
        await store.closeTab(id);
      } catch (error: unknown) {
        store.setError(formatError(error));
      }
    },
    activateTab: async (id: string, extras: { force?: boolean } = {}) => {
      openEpoch += 1;
      const current = internal.sessions[id];
      // Restore passes force and must not prompt. A click on a tab whose
      // hydrate or open was refused is the moment the person can approve it,
      // including when that tab is already the active one.
      if (!extras.force && current?.trustRequired) {
        if (internal.workspace.activeId !== id) {
          const next = activateTab(internal.workspace, id);
          if (next === internal.workspace) return;
          replaceWorkspace(next);
        }
        syncFilterFromSession(current);
        publish();
        await grantTrust(id);
        return;
      }
      if (!extras.force && internal.workspace.activeId === id) return;
      if (internal.workspace.activeId !== id) {
        const next = activateTab(internal.workspace, id);
        if (next === internal.workspace) return;
        replaceWorkspace(next);
      }
      const session = internal.sessions[id];
      if (!session) {
        publish();
        return;
      }
      if (session.trustRequired) {
        syncFilterFromSession(session);
        publish();
        return;
      }
      syncFilterFromSession(session);
      const activation = bumped(session);
      // Only a session with nothing rendered yet presents a spinner; a
      // background refresh of rendered content must not strobe it.
      putSession({ ...activation, isLoading: !session.hasHydrated });
      publish();
      revealGraph(activation);
      // Asked before `onActivated`, which cancels what is owed: a refresh the
      // policy still owes this repository is a reason to read it now.
      const refreshOwed = watcherRefreshPolicy
        .owed()
        .some((key) => sameRepo(key, session.path, options));
      watcherRefreshPolicy.onActivated(session.path);
      pollLastDurationMs = null;
      rearmStatusPoll();
      // `force` is restore presenting a saved tab, not a person clicking it.
      if (!extras.force) await noteTabActivated(session.path);
      // A parked tab comes back live here; a watched one moves to the front
      // of the pool, so the repository someone is looking at is never the
      // one evicted. After the note, which must precede every native call
      // this activation makes.
      await acquireWatch(id, session.path, true);
      // A snapshot that landed seconds ago under a live watcher is what a
      // fresh read would return: the watcher reports anything since. Reading
      // again on every switch is six git reads and a stats drain for nothing.
      if (extras.force || refreshOwed || !snapshotIsFresh(id, activation.generation)) {
        await hydrate(id, session.path, activation.generation);
      }
      ensureStatusPoll();
      flushPersist();
    },
    closeTab: async (id: string) => {
      const session = internal.sessions[id];
      // Closing the tab unmounts its terminal panel, which kills every shell
      // in it. Ask first, and only when there is something to lose — a
      // confirmation on every close would train the user to dismiss it, which
      // is how the one that mattered gets dismissed too.
      if (session && !(await confirmTerminalLoss(terminals, [session.path]))) return;
      const result = closeTab(internal.workspace, id);
      if (result.reason === "missing") return;
      commitEdit(result.closedPath ? [result.closedPath] : []);
      replaceWorkspace(result.workspace);
      const { [id]: _removed, ...rest } = internal.sessions;
      internal = { ...internal, sessions: rest };
      stopStatusPoll();
      await releaseSession(id, session?.path ?? null);
      const next = activeSession();
      if (next) {
        // Activation-by-close: bump the neighbor so the App-owned graph
        // effect refetches exactly once for it.
        putSession(bumped(next));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    closeActiveTab: async () => {
      if (!beginShortcut()) return;
      const id = internal.workspace.activeId;
      if (id) await store.closeTab(id);
    },
    closeOtherTabs: async (id: string) => {
      const keep = internal.sessions[id];
      if (!keep) return;
      const removed = internal.workspace.tabs.filter((tab) => tab.id !== id);
      if (!(await confirmTerminalLoss(terminals, removed.map((tab) => tab.path)))) return;
      commitEdit(removed.map((tab) => tab.path));
      replaceWorkspace(closeOtherTabs(internal.workspace, id));
      internal = { ...internal, sessions: { [id]: keep } };
      stopStatusPoll();
      for (const tab of removed) await releaseSession(tab.id, tab.path);
      if (internal.workspace.activeId === id) {
        putSession(bumped(keep));
      }
      ensureStatusPoll();
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    closeTabsToTheRight: async (id: string) => {
      const index = internal.workspace.tabs.findIndex((tab) => tab.id === id);
      if (index < 0) return;
      const removed = internal.workspace.tabs.slice(index + 1);
      if (!(await confirmTerminalLoss(terminals, removed.map((tab) => tab.path)))) return;
      commitEdit(removed.map((tab) => tab.path));
      replaceWorkspace(closeTabsToTheRight(internal.workspace, id));
      const remaining = new Set(internal.workspace.tabs.map((tab) => tab.id));
      const sessions: Record<string, RepoSession> = {};
      for (const [key, session] of Object.entries(internal.sessions)) {
        if (remaining.has(key)) sessions[key] = session;
      }
      internal = { ...internal, sessions };
      stopStatusPoll();
      for (const tab of removed) await releaseSession(tab.id, tab.path);
      const revealed = activeSession();
      // When the active tab was among those removed, tabModel reassigns
      // activeId to the clicked tab — bump it so the App-owned graph effect
      // treats it as a fresh activation.
      if (revealed) {
        putSession(bumped(revealed));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    // Next/previous and number keys step through the strip as drawn — a
    // folded worktree stack is one stop, a group's tabs sit together — so
    // the keyboard and the native menu land where the eye expects.
    nextTab: async () => {
      if (!beginShortcut()) return;
      const next = cycleStop(drawnLayout(), internal.workspace.activeId, 1);
      if (next && next !== internal.workspace.activeId) {
        await store.activateTab(next);
      }
    },
    prevTab: async () => {
      if (!beginShortcut()) return;
      const next = cycleStop(drawnLayout(), internal.workspace.activeId, -1);
      if (next && next !== internal.workspace.activeId) {
        await store.activateTab(next);
      }
    },
    /**
     * Applies an order the strip computed from what it draws. A regroup, when
     * given, moves those tabs into that group first (and opens it, so a drop
     * never makes the dropped tab vanish). Refused whole when the order is
     * not a permutation of the open tabs.
     */
    arrangeTabs: (orderedIds: readonly string[], regroup?: { ids: readonly string[]; group: string | null }) => {
      let next = internal.workspace;
      if (regroup) {
        for (const id of regroup.ids) next = setWorkspaceTabGroup(next, id, regroup.group);
        if (regroup.group) next = setWorkspaceGroupCollapsed(next, regroup.group, false);
      }
      const arranged = arrangeWorkspaceTabs(next, orderedIds);
      const alreadyInOrder =
        orderedIds.length === next.tabs.length && next.tabs.every((tab, i) => tab.id === orderedIds[i]);
      // A stale plan must not half-apply: refused order means no regroup either.
      if (arranged === next && !alreadyInOrder) return false;
      if (arranged === internal.workspace) return false;
      commitEdit();
      replaceWorkspace(arranged);
      publish();
      flushPersist();
      return true;
    },
    activateTabAt: async (index: number) => {
      if (!Number.isInteger(index) || index < 0) return;
      const id = activationStops(drawnLayout())[index];
      if (id) await store.activateTab(id);
    },
    // Moves step through the strip as drawn (see repos/stripNav.ts): a folded
    // worktree stack moves with every checkout in it, a group's tab stays in
    // its group, and a step never swaps with a tab the reader cannot see. The
    // tab bar, the command palette and the keyboard all come through here.
    moveTabBy: (id: string, delta: number): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnit(layout, unit, delta) : null;
      return order ? store.arrangeTabs(order) : false;
    },
    moveTabToEdge: (id: string, edge: "start" | "end"): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnitToEdge(layout, unit, edge) : null;
      return order ? store.arrangeTabs(order) : false;
    },
    canMoveTab: (id: string, delta: -1 | 1): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      return unit !== null && canMoveUnit(layout, unit, delta);
    },
    pinTab: (id: string, pinned: boolean) => {
      commitEdit();
      replaceWorkspace(pinWorkspaceTab(internal.workspace, id, pinned));
      const session = internal.sessions[id];
      if (session) putSession({ ...session, pinned });
      publish();
    },
    setTabGroup: (id: string, group: string | null) => {
      commitEdit();
      replaceWorkspace(setWorkspaceTabGroup(internal.workspace, id, group));
      publish();
      flushPersist();
    },
    setTabColor: (id: string, color: TabColor | null) => {
      const next = setWorkspaceTabColor(internal.workspace, id, color);
      if (next === internal.workspace) return;
      commitEdit();
      replaceWorkspace(next);
      publish();
      flushPersist();
    },
    setGroupColor: (groupName: string, color: TabColor | null) => {
      const next = setWorkspaceGroupColor(internal.workspace, groupName, color);
      if (next === internal.workspace) return;
      commitEdit();
      replaceWorkspace(next);
      publish();
      flushPersist();
    },
    groupByParentFolder: () => {
      commitEdit();
      replaceWorkspace(
        groupWorkspaceByParentFolder(internal.workspace, (tab) => familyOfTab(tab.id)?.root),
      );
      publish();
      flushPersist();
    },
    ungroupTabs: (groupName?: string) => {
      commitEdit();
      replaceWorkspace(ungroupWorkspaceTabs(internal.workspace, groupName));
      publish();
      flushPersist();
    },
    closeGroup: async (groupName: string) => {
      const groupTabs = internal.workspace.tabs.filter((t) => t.group === groupName);
      if (groupTabs.length === 0) return;
      if (!(await confirmTerminalLoss(terminals, groupTabs.map((t) => t.path)))) return;
      const { workspace, closedPaths } = closeWorkspaceGroup(internal.workspace, groupName);
      commitEdit(groupTabs.map((tab) => tab.path));
      replaceWorkspace(workspace);
      const remaining = new Set(internal.workspace.tabs.map((tab) => tab.id));
      const sessions: Record<string, RepoSession> = {};
      for (const [key, session] of Object.entries(internal.sessions)) {
        if (remaining.has(key)) sessions[key] = session;
      }
      internal = { ...internal, sessions };
      stopStatusPoll();
      // The fourth close route: it must end in the same release as the other
      // three, or its tabs' records and journals outlive them.
      const closedSet = new Set(closedPaths);
      for (const tab of groupTabs) {
        if (closedSet.has(tab.path)) await releaseSession(tab.id, tab.path);
      }
      const revealed = activeSession();
      if (revealed) {
        putSession(bumped(revealed));
        ensureStatusPoll();
      }
      syncFilterFromSession(activeSession());
      publish();
      revealGraph(activeSession());
      flushPersist();
    },
    renameGroup: (oldName: string, newName: string) => {
      commitEdit();
      replaceWorkspace(renameWorkspaceGroup(internal.workspace, oldName, newName));
      publish();
      flushPersist();
    },
    toggleGroupCollapsed: (groupName: string) => {
      commitEdit();
      replaceWorkspace(toggleWorkspaceGroupCollapsed(internal.workspace, groupName));
      publish();
      flushPersist();
    },
    setGroupCollapsed: (groupName: string, collapsed: boolean) => {
      commitEdit();
      replaceWorkspace(setWorkspaceGroupCollapsed(internal.workspace, groupName, collapsed));
      publish();
      flushPersist();
    },
    isGroupCollapsed: (groupName: string) => {
      return isWorkspaceGroupCollapsed(internal.workspace, groupName);
    },
    collapseAllGroups: () => {
      commitEdit();
      replaceWorkspace(collapseWorkspaceAllGroups(internal.workspace));
      publish();
      flushPersist();
    },
    expandAllGroups: () => {
      commitEdit();
      replaceWorkspace(expandWorkspaceAllGroups(internal.workspace));
      publish();
      flushPersist();
    },
    reopenLastClosed: async () => {
      const path = internal.workspace.lastClosed[0];
      if (!path) return;
      await store.openRepo(path, { activate: true });
    },
    clearRecents: () => {
      replaceWorkspace({ ...internal.workspace, recents: [] });
      publish();
      flushPersist();
    },
    removeRecent: (path: string) => {
      replaceWorkspace(
        removeWorkspaceRecent(internal.workspace, path, options),
      );
      publish();
    },
    removeRepo: async (path: string) => {
      const tab = internal.workspace.tabs.find((item) =>
        sameRepo(item.path, path, options),
      );
      if (tab) {
        await store.closeTab(tab.id);
      }
      replaceWorkspace({
        ...internal.workspace,
        recents: internal.workspace.recents.filter(
          (item) => !sameRepo(item, path, options),
        ),
        lastClosed: internal.workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      publish();
      flushPersist();
    },
    refresh: async (repoPath?: string) => {
      const session = repoPath
        ? Object.values(internal.sessions).find((item) =>
            sameRepo(item.path, repoPath, options),
          )
        : activeSession();
      if (!session || session.trustRequired) return;
      const generation = session.generation;
      // Spinner only while nothing is rendered; a rendered error stays until
      // this refresh's own outcome replaces or retires it (hydrate settle).
      applyToSession(session.id, generation, {
        isLoading: !session.hasHydrated,
      });
      await hydrate(session.id, session.path, generation);
      const latest = internal.sessions[session.id];
      if (latest && internal.workspace.activeId === session.id) {
        // Reload with the visible filter context: the store normalizes the
        // query and the backend applies every term, so the cached payload
        // always answers exactly the query the scheduler keys on.
        void graph.loadGraph(latest.path, latest.searchQuery, latest.selectedBranch);
      }
    },
    /** Subscribes to worktree changes the status poll observed (the watcher
     *  misses nested edits). Returns the unsubscribe. */
    onPolledChange: (listener: (path: string, change: RepoChange) => void): (() => void) => {
      polledChangeListeners.add(listener);
      return () => void polledChangeListeners.delete(listener);
    },
    handleRepoChanged: async (changedPath?: string | null) => {
      // The one entry for "the repository changed under us": a read already
      // running began before this change, so no refresh asked for from here
      // may join it (ipc/invoke.ts). Before every early return below, because
      // a dropped echo or an unopened path still leaves a poll's read running
      // that predates it.
      invalidateSharedReads();
      // Watcher events arrive per file; a checkout or rebase fires many at
      // once. Each changed path collapses onto its own trailing window so a
      // storm becomes one refresh; explicit refresh() calls stay undelayed.
      if (!changedPath) {
        if (activeSession()?.trustRequired) return;
        scheduleWatcherRefresh("", () => void store.refresh());
        return;
      }
      const session = Object.values(internal.sessions).find((item) =>
        sameRepo(item.path, changedPath, options),
      );
      if (!session || session.trustRequired) return;
      // Drop echoes of our own writes (isEchoOfOwnWrite): the explicit
      // refresh() in runMutating already fetches fresh state, so acting on the
      // echo would refresh the whole session twice per mutation. An unrelated
      // external change inside the window is picked up by the next poll or event.
      if (isEchoOfOwnWrite(session.path)) return;
      bindRefreshVisibility();
      watcherRefreshPolicy.onChange(session.path);
    },
    /**
     * The watcher's `repo-gone`: this checkout's git directory was deleted or
     * moved, and its watch has ended. The tab stays (with its pin, group and
     * terminals) and says what happened; background work stops visiting it.
     * Nothing else would ever tell it: a dead path never emits a change.
     */
    markRepoGone: (gonePath: string) => {
      const session = Object.values(internal.sessions).find((item) =>
        sameRepo(item.path, gonePath, options),
      );
      if (!session || session.missing) return;
      applyToSession(session.id, session.generation, {
        missing: true,
        isLoading: false,
        error: `This checkout no longer exists at ${session.path}. Close the tab, or recreate the checkout and refresh.`,
      });
    },
    restoreWorkspace: async () => {
      stopStatusPoll();
      persistSuspended += 1;
      try {
        const persisted = loadPersistedWorkspace(storage, options);
        workspaceEpoch = persisted.epoch ?? 0;
        droppedPaths = [];
        replaceWorkspace({
          ...emptyWorkspace(),
          recents: persisted.recents,
          lastClosed: persisted.lastClosed,
        });
        internal = { ...internal, sessions: {} };
        familyUnknown.clear();
        // Two phases, in the persisted order. First every tab is opened —
        // resolved and watched, no git read — so the whole strip, and every
        // repository's family, is there at once: one tab per await used to
        // draw a worktree as its own repository until its repository's tab
        // finished hydrating. Then the previously-active tab is presented
        // (its hydrate first, so the workspace is usable soonest) and the
        // rest hydrate behind it.
        const ordered = [...persisted.tabs];
        let activated = false;
        const isActive = (path: string) =>
          !!persisted.activePath && sameRepo(path, persisted.activePath, options);
        // A tab that never resolved (not trusted, or missing) must not be
        // hydrated here. Hydration is a git command, and restore is not the
        // moment to ask for trust or to run hooks.
        const presentRestored = async (id: string) => {
          const session = internal.sessions[id];
          if (session?.error && !session.hasHydrated) {
            replaceWorkspace(activateTab(internal.workspace, id));
            syncFilterFromSession(session);
            publish();
            return;
          }
          await store.activateTab(id, { force: true });
        };

        // One publish for the whole strip, not a few per tab.
        const releasePublish = holdPublish();
        try {
          for (const tab of ordered) {
            // Always append (activate: false) so restore cannot shuffle tab
            // order. keepEpoch + a suspended save: quitting halfway cannot
            // replace the durable list with the tabs opened so far.
            await store.openRepo(tab.path, {
              allowBroken: true,
              deferTrust: true,
              keepEpoch: true,
              activate: false,
              skipHydrate: true,
              pinned: tab.pinned,
              group: tab.group ?? null,
              ...(tab.color ? { color: tab.color } : {}),
              restore: {
                viewTab: tab.viewTab,
                viewSections: tab.viewSections,
                searchQuery: tab.searchQuery,
                selectedBranch: tab.selectedBranch,
                terminalOpen: tab.terminalOpen,
              },
            });
          }
        } finally {
          releasePublish();
        }
        const activeTab = persisted.activePath
          ? ordered.find((tab) => isActive(tab.path))
          : undefined;
        const activeSessionTab = activeTab
          ? internal.workspace.tabs.find((item) => sameRepo(item.path, activeTab.path, options))
          : undefined;
        if (activeSessionTab) {
          activated = true;
          await presentRestored(activeSessionTab.id);
        }
        // Applied after every tab exists, so a collapsed group is not
        // discarded for having no members yet. The active tab's group was
        // expanded when that tab was presented; putting the saved list back
        // wholesale would collapse it again.
        const activeGroup = activated
          ? internal.workspace.tabs.find((tab) => tab.id === internal.workspace.activeId)?.group
          : null;
        const collapsedGroups = (persisted.collapsedGroups ?? []).filter(
          (group) => !activeGroup || group !== activeGroup,
        );
        replaceWorkspace(
          restoreGroupColors(
            { ...internal.workspace, collapsedGroups },
            persisted.groupColors,
          ),
        );
        publish();
        if (!activated) {
          const desired = persisted.activePath
            ? internal.workspace.tabs.find((tab) =>
                sameRepo(tab.path, persisted.activePath ?? "", options),
              )
            : internal.workspace.tabs[0];
          if (desired) {
            await presentRestored(desired.id);
          } else {
            syncFilterFromSession(undefined);
            graph.showRepo(null);
            publish();
          }
        }
        // Phase two: the other resolved tabs, in strip order, take the watch
        // pool's free slots and hydrate. The rest are parked, unread, until
        // someone opens them: reading every restored tab was one full
        // hydrate per tab against a shared spawn budget, which is why the
        // number of open repositories used to be capped. A tab that never
        // resolved stays unread, as above; one the reader opened or closed
        // meanwhile is skipped by the generation check. Slots are reserved,
        // and the rest parked, in one held pass: parking a tab at a time
        // published once per tab.
        const toRead: string[] = [];
        const releaseParking = holdPublish();
        try {
          for (const tab of internal.workspace.tabs) {
            const session = internal.sessions[tab.id];
            if (!session || session.hasHydrated || session.error || session.trustRequired) continue;
            if (admitWatch(tab.id, false).admitted) {
              toRead.push(tab.id);
            } else {
              applyToSession(tab.id, session.generation, { watch: WATCH_PARKED, isLoading: false });
            }
          }
        } finally {
          releaseParking();
        }
        for (const id of toRead) {
          const session = internal.sessions[id];
          if (!session || session.hasHydrated || session.error || session.trustRequired) continue;
          await acquireWatch(id, session.path, false);
          if (watchPool.has(id)) {
            await hydrate(id, session.path, session.generation);
          } else {
            // Its reserved slot went to a repository someone opened meanwhile.
            applyToSession(id, session.generation, { isLoading: false });
          }
        }
        ensureStatusPoll();
      } finally {
        persistSuspended -= 1;
        flushPersist(true);
      }
    },
    /** Writes the live workspace immediately. Quit calls this before the process exits. */
    flushPersistedWorkspace: () => {
      flushPersist(true);
    },
    /** All uncommitted-change entry points share the existing file/diff view. */
    previewUncommitted: async (repoPath?: string, isStaged?: boolean): Promise<void> => {
      const session = activeSession();
      const target = repoPath ?? session?.path;
      if (!target) return;
      if (!session || !sameRepo(session.path, target, options) || session.isLoading || session.error) {
        let preview: Promise<void> | undefined;
        await store.openRepo(target, {
          onReady: () => { preview = store.previewUncommitted(undefined, isStaged); },
        });
        await preview;
        return;
      }
      const file = session.statuses.find(status => isStaged === undefined || (isStaged ? status.is_staged : hasUnstagedChanges(status)))
        ?? session.statuses[0];
      const side = isStaged === false && file && hasUnstagedChanges(file) ? false : file?.is_staged ?? false;
      openEpoch += 1;
      // Invalidate late commit/range reads even when this worktree became clean.
      diffSelection.supersede();
      applyToSession(session.id, session.generation, {
        selectedFilePath: file?.path ?? null,
        selectedCommitId: null,
        selectedDiff: null,
        selectedDiffTruncated: false,
        selectedDiffTruncationReason: null,
        selectedDiffError: null,
        selectedDiffPending: Boolean(file),
        selectedIsStaged: side,
        selectionKind: "file",
        activeTab: "history",
        viewSections: { ...session.viewSections, history: "diff" },
      });
      interfaceStore.setFleetOpen(false);
      if (file) await store.selectFileDiff(file.path, side);
    },
    selectFileDiff: (filePath: string, isStaged: boolean = false) =>
      diffSelection.select({
        read: (invoke, repoPath, ignoreWhitespace) =>
          invoke<DiffPayload>("cmd_get_file_diff", { repoPath, filePath, isStaged, ignoreWhitespace }),
        filePath,
        commitId: null,
        isStaged,
        kind: "file",
        history: "unlessNavigated",
        worktree: true,
      }),
    selectFilePath: (filePath: string | null) => {
      const session = activeSession();
      if (!session) return;
      // Records the shared file selection WITHOUT fetching a diff or moving
      // tabs: Coverage's file list and Blame's explorer converge on this one
      // site so the selection survives tab switches, and whichever viewer is
      // open reacts through its own effect. The diff fetch remains
      // selectFileDiff's job. Null clears the selection when the final editor
      // tab closes, preventing that closed path from reopening on remount.
      applyToSession(session.id, session.generation, {
        selectedFilePath: filePath,
        selectedCommitId: null,
        selectionKind: "file",
      });
    },
    setIgnoreWhitespace: (next: boolean) => {
      const session = activeSession();
      if (!session) return;
      const previous = session.selectedIgnoreWhitespace;
      if (previous === next) return;
      // Read the old session's fields BEFORE applyToSession: putSession
      // replaces the stored object with a fresh immutable copy.
      const {
        selectedFilePath: filePath,
        selectedIsStaged: isStaged,
        selectionKind,
      } = session;
      applyToSession(session.id, session.generation, {
        selectedIgnoreWhitespace: next,
      });
      // Only worktree-file selections can be refetched with -w; commit/range
      // selections just record the preference for the next file click.
      if (selectionKind !== "file" || !filePath) return;
      void store.selectFileDiff(filePath, isStaged);
    },
    selectCommitDiff: (commitId: string) =>
      diffSelection.select({
        read: (invoke, repoPath) => invoke<DiffPayload>("cmd_get_commit_diff", { repoPath, commitId }),
        filePath: null,
        commitId,
        isStaged: false,
        kind: "commit",
        history: "never",
      }),
    /**
     * Opens one file of a commit while keeping the commit as the selection owner.
     */
    selectCommitFileDiff: (commitId: string, filePath: string) =>
      diffSelection.select({
        read: (invoke, repoPath) =>
          invoke<DiffPayload>("cmd_get_commit_file_diff", { repoPath, commitId, filePath }),
        filePath,
        commitId,
        isStaged: false,
        kind: "commit",
        history: "always",
      }),
    selectRangeDiff: (from: string, to: string) =>
      diffSelection.select({
        read: (invoke, repoPath) => invoke<DiffPayload>("cmd_get_range_diff", { repoPath, from, to }),
        filePath: `${from}...${to}`,
        commitId: null,
        isStaged: false,
        kind: "range",
        history: "always",
      }),
    stageFile: async (filePath: string) =>
      runMutating("stage", filePath, (path) =>
        invokeFn("cmd_stage_file", { repoPath: path, filePath }),
      ),
    unstageFile: async (filePath: string) => {
      const session = activeSession();
      if (!session) return { ok: false, error: "No active repository" };
      const file = session.statuses.find((entry) => entry.path === filePath && entry.is_staged);
      const filePaths = file ? indexSelectionPaths([file], "unstage") : [filePath];
      return runMutating("unstage", filePath, (path) => filePaths.length > 1
        ? invokeFn("cmd_change_index", { repoPath: path, filePaths, action: "unstage" })
        : invokeFn("cmd_unstage_file", { repoPath: path, filePath }), { session });
    },
    stageSelectivePatch: async (
      filePatch: FilePatch,
      isStaging: boolean = true,
    ) => {
      if (isStaging) {
        return runMutating("stage-patch", filePatch.new_path, (path) =>
          invokeFn("cmd_stage_selective_patch", { repoPath: path, filePatch }),
        );
      } else {
        return runMutating("unstage-patch", filePatch.new_path, (path) =>
          invokeFn("cmd_unstage_selective_patch", {
            repoPath: path,
            filePatch,
          }),
        );
      }
    },
    stageAll: () => runStageBatch("stage"),
    unstageAll: () => runStageBatch("unstage"),
    discardChanges: async (filePath: string, target?: MutationTarget | null) =>
      runMutating("discard", filePath, (path) =>
        invokeFn("cmd_discard_changes", { repoPath: path, filePath }),
        { target },
      ),
    commit: async (message: string, amend: boolean = false) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_commit", { repoPath: path, message, amend }),
      ),
    /**
     * Stage remaining worktree changes and commit the index as one mutation.
     * Conflicts and empty messages are refused by the backend; the frontend
     * surfaces those as the same MutationOutcome as a gated `commit`.
     */
    quickCommit: async (message: string) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_quick_commit", { repoPath: path, message }),
      ),
    checkoutBranch: async (branchName: string) =>
      runMutating("checkout", branchName, (path) =>
        invokeFn("cmd_checkout_branch", { repoPath: path, branchName }),
      ),
    createBranch: async (branchName: string, startPoint?: string, target?: MutationTarget | null) =>
      runMutating("branch", branchName, (path) =>
        invokeFn("cmd_create_branch", {
          repoPath: path,
          branchName,
          startPoint,
        }),
        { target },
      ),
    renameBranch: async (oldName: string, newName: string, target?: MutationTarget | null) =>
      runMutating("branch-rename", `${oldName} → ${newName}`, (path) =>
        invokeFn("cmd_rename_branch", { repoPath: path, oldName, newName }),
        { target },
      ),
    deleteBranch: async (branchName: string, force: boolean = false, target?: MutationTarget | null) =>
      runMutating(
        force ? "branch-delete-force" : "branch-delete",
        branchName,
        (path) =>
          invokeFn("cmd_delete_branch", { repoPath: path, branchName, force }),
        { target },
      ),
    // --- stash ----------------------------------------------------------

    /**
     * Applies, pops, or drops a stash entry.
     *
     * Takes the entry rather than an index so the object id it was listed with
     * always travels with it: the backend re-resolves the index under its lock
     * and refuses the pair on a mismatch, which is what keeps a stale list from
     * dropping a stash that someone else pushed in the meantime.
     */
    stashAction: async (action: StashAction, entry: StashEntry) =>
      runMutating(`stash-${action}`, entry.selector, (path) =>
        invokeFn("cmd_stash_action", {
          repoPath: path,
          action,
          index: entry.index,
          expectedOid: entry.oid,
        }),
      ),

    /** The diff a stash entry holds, addressed by object id. */
    stashShow: async (oid: string): Promise<DiffPayload> => {
      const session = activeSession();
      if (!session) throw new Error("No repository is open.");
      return invokeFn<DiffPayload>("cmd_stash_show", { repoPath: session.path, oid });
    },

    // --- replaying and rewinding commits --------------------------------

    /**
     * Replays commits onto the current branch. A conflict parks the repository,
     * which the operation banner then offers a way out of.
     */
    cherryPick: async (commits: string[], noCommit = false) =>
      runMutating("cherry-pick", commits.join(", "), (path) =>
        invokeFn("cmd_cherry_pick", { repoPath: path, commits, noCommit }),
      ),

    /** Records the inverse of the given commits as new commits. */
    revertCommits: async (commits: string[], noCommit = false) =>
      runMutating("revert", commits.join(", "), (path) =>
        invokeFn("cmd_revert", { repoPath: path, commits, noCommit }),
      ),

    /**
     * Moves the current branch to `target`.
     *
     * `"hard"` destroys uncommitted work irrecoverably; callers are expected to
     * have confirmed with the user first — this is the last layer that could,
     * and it deliberately does not second-guess an explicit instruction.
     */
    resetTo: async (mode: ResetMode, target: string, repository?: MutationTarget | null) =>
      runMutating(`reset-${mode}`, target, (path) =>
        invokeFn("cmd_reset", { repoPath: path, mode, target }),
        { target: repository },
      ),

    // --- remotes and submodules ----------------------------------------

    listRemotes: async (): Promise<{ remotes: RemoteInfo[]; truncated: boolean }> => {
      const session = activeSession();
      if (!session) return { remotes: [], truncated: false };
      const raw = await invokeFn<unknown>("cmd_list_remotes", { repoPath: session.path });
      const parsed = parseRemoteList(raw);
      if (parsed.failed) {
        throw new Error("The remote list was not readable.");
      }
      return { remotes: parsed.remotes, truncated: parsed.truncated };
    },

    remoteChange: async (change: RemoteChange) =>
      runMutating(`remote-${change.kind}`, change.name, (path) =>
        invokeFn("cmd_remote_change", { repoPath: path, change }),
      ),

    listSubmodules: async (): Promise<{ submodules: SubmoduleInfo[]; truncated: boolean }> => {
      const session = activeSession();
      if (!session) return { submodules: [], truncated: false };
      const raw = await invokeFn<unknown>("cmd_list_submodules", { repoPath: session.path });
      const parsed = parseSubmoduleList(raw);
      if (parsed.failed) {
        throw new Error("The submodule list was not readable.");
      }
      return { submodules: parsed.submodules, truncated: parsed.truncated };
    },

    submoduleChange: async (change: SubmoduleChange) =>
      runMutating(
        `submodule-${change.kind}`,
        ("path" in change && change.path) || "all submodules",
        (path) => invokeFn("cmd_submodule_change", { repoPath: path, change }),
      ),

    createTag: async (tagName: string, commitId?: string, message?: string, target?: MutationTarget | null) =>
      runMutating("tag", tagName, (path) =>
        invokeFn("cmd_create_tag", {
          repoPath: path,
          tagName,
          commitId: commitId ?? null,
          message: message ?? null,
        }),
        { target },
      ),

    deleteTag: async (tagName: string, target?: MutationTarget | null) =>
      runMutating("tag-delete", tagName, (path) =>
        invokeFn("cmd_delete_tag", { repoPath: path, tagName }),
        { target },
      ),

    // --- workspace-wide -------------------------------------------------

    /**
     * The work-in-progress answer for every open repository.
     *
     * Derived from live session state rather than refetched, so it is free to
     * call and always agrees with what the tabs are showing.
     */
    workspaceWip: (): WorkspaceWip => summarizeWorkspace(wipInputs()),

    /**
     * Live facts for every open repository, in tab order.
     *
     * Derived from session state rather than refetched, so it is free to call
     * and always agrees with what the tabs are showing. The Fleet grid's
     * cheapest tier is exactly this call.
     */
    repoFacts: (): RepoFacts[] => repoFacts(),

    /**
     * Runs `fetch` (or `pull`) across every open repository.
     *
     * Repositories that are parked mid-operation, still loading, or holding
     * conflicts are SKIPPED and reported as skipped — never silently counted as
     * fetched. After the sweep every visited repository is refreshed so the
     * tabs reflect what actually landed.
     */
    runAcrossOpenRepos: async (
      kind: "fetch" | "pull",
      options: RunOptions & {
        /**
         * Restrict the run to these open repositories.
         *
         * Omitted, every open tab is visited — the workspace-header behaviour.
         * Given, this is still the same run: a single-row fetch from the Fleet
         * grid goes through the identical skip rules, so a repository parked
         * mid-rebase is reported as skipped there too rather than being
         * fetched by a second, laxer code path.
         *
         * A path that is not an open tab is simply absent from the targets; it
         * is never invented into one.
         */
        only?: readonly string[];
      } = {},
    ): Promise<BulkRunReport> => {
      const byPath = new Map(wipInputs().map((input) => [input.path, input]));
      const restrict = options.only ? new Set(options.only) : null;
      const targets: RepoTarget[] = internal.workspace.tabs
        .filter((tab) => restrict === null || restrict.has(tab.path))
        .map((tab) => ({
          path: tab.path,
          label: byPath.get(tab.path)?.label ?? displayName(tab.path),
        }));
      const tabIds = new Map(internal.workspace.tabs.map((tab) => [tab.path, tab.id]));
      // One token for the whole sweep. With a concurrency below the number of
      // repositories there are gaps between one repository finishing and the
      // next starting; without this the quit guard would see an idle store in
      // each. No repository path is "", so no per-repository lookup matches it.
      const endSweep = beginMutation("", `${kind}-all`);
      try {
        const report = await runAcrossRepos(
          targets,
          async (target) => {
            const input = byPath.get(target.path);
            const skip = input ? bulkSkipReason(input) : "Repository is no longer open.";
            if (skip) return { skip };
            // The snapshot above was taken before the sweep began. While this
            // repository waited its turn the tab may have closed, its checkout
            // gone missing or its trust been withdrawn: the live answer is the
            // same one every targeted mutation uses.
            const tabId = tabIds.get(target.path);
            const mutationTarget = tabId ? { tabId, path: target.path } : null;
            if (!mutationTarget || !resolveTarget(mutationTarget)) {
              return { skip: "Repository is no longer open, or is unavailable." };
            }
            // The same wrapper every other mutation uses: activity for the
            // quit guard, the echo window armed before the work, the policy
            // verdict and the journal entry. Both commands are named
            // literally rather than selected into a variable: the IPC
            // contract checker verifies every invoked command against the
            // Rust registry statically, and a computed name is a hole in
            // that check rather than a shortcut.
            const outcome = await runMutating(
              kind,
              target.label,
              (path) =>
                kind === "fetch"
                  ? invokeFn("cmd_fetch", { repoPath: path })
                  : invokeFn("cmd_pull", { repoPath: path }),
              { target: mutationTarget, skipRefresh: true },
            );
            // The sweep classifies by a throw: that is what makes it "failed".
            if (!outcome.ok) throw new Error(outcome.error ?? `${kind} failed`);
          },
          options,
        );
        // Refresh only what actually ran: re-hydrating a skipped repository
        // would cost a full snapshot for a repository nothing happened to.
        //
        // Bounded, not `Promise.all`. Each refresh is itself five concurrent
        // commands, so fanning out over all 64 possible repositories issued
        // ~320 git-spawning calls in one instant and exhausted the process's
        // file descriptors — every later spawn failing with "Too many open
        // files" until restart.
        await mapItems(
          report.results.filter((result) => result.status === "ok"),
          DEFAULT_FAN_OUT,
          (result) => store.refresh(result.path),
        );
        return report;
      } finally {
        endSweep();
      }
    },

    /**
     * Aborts, continues, or skips the parked operation.
     *
     * The kind is deliberately NOT sent: the backend re-detects it under the
     * repository lock, so a banner rendered before someone aborted from the
     * terminal cannot send `git rebase --abort` at a repository that has since
     * become idle. The refresh that `runMutating` triggers is what clears the
     * banner, so the UI never has to guess whether the operation ended.
     */
    operationAction: async (action: OperationAction) =>
      runMutating(`operation-${action}`, action, (path) =>
        invokeFn("cmd_repo_operation_action", { repoPath: path, action }),
      ),

    rebaseInteractive: (ontoCommit: string, steps: RebaseStep[]) =>
      runMutating("rebase", ontoCommit, (path) =>
        invokeFn("cmd_rebase_interactive", { repoPath: path, ontoCommit, steps })),

    mergeBranch: async (branchName: string, mode: MergeMode = "default") =>
      runMutating(mode === "squash" ? "merge-squash" : "merge", branchName, (path) =>
        invokeFn("cmd_merge_branch", { repoPath: path, branchName, mode }),
      ),
    fetch: async (remote?: string) =>
      runMutating("fetch", remote ?? "origin", (path) =>
        invokeFn("cmd_fetch", { repoPath: path, remote }),
      ),
    /**
     * `rebase`: true rebases local commits onto the upstream (`--rebase`),
     * false merges (`--no-rebase`), undefined follows `pull.rebase`.
     */
    pull: async (remote?: string, branch?: string, rebase?: boolean) =>
      runMutating(
        rebase ? "pull-rebase" : "pull",
        [remote, branch].filter(Boolean).join(" ") || "upstream",
        (path) => invokeFn("cmd_pull", { repoPath: path, remote, branch, rebase }),
      ),
    /** `pull.rebase` for the active repository, or null when unset (git merges). */
    pullRebaseConfig: async (): Promise<string | null> => {
      const session = activeSession();
      if (!session) return null;
      const value = await invokeFn<unknown>("cmd_pull_rebase_config", { repoPath: session.path });
      return typeof value === "string" ? value : null;
    },
    /**
     * Stops the hook-running git command (commit, merge, rebase, …) running
     * in the active repository, hook and all. Resolves to how many were
     * stopped: zero means nothing was running.
     */
    cancelHooks: async (): Promise<number> => {
      const session = activeSession();
      if (!session) return 0;
      const stopped = await invokeFn<unknown>("cmd_cancel_git_hooks", { repoPath: session.path });
      return typeof stopped === "number" ? stopped : 0;
    },
    /** The identity a commit in the active repository would be recorded under. */
    gitIdentity: async (): Promise<GitIdentity | null> => {
      const session = activeSession();
      if (!session) return null;
      return invokeFn<GitIdentity>("cmd_git_identity", { repoPath: session.path });
    },
    setGitIdentity: async (name: string, email: string, scope: IdentityScope) =>
      runMutating<GitIdentity>(
        "identity",
        scope === "global" ? "global identity" : "repository identity",
        (path) => invokeFn("cmd_set_git_identity", { repoPath: path, name, email, scope }),
        { skipRefresh: true },
      ),
    /** Whether a usable git is installed; asked once at startup. */
    gitPreflight: async (): Promise<GitPreflight> =>
      parseGitPreflight(await invokeFn<unknown>("cmd_git_preflight", {})),
    autoFetchSetting: (path: string): AutoFetchSetting | null => autoFetchPrefs[path] ?? null,
    setAutoFetch: (path: string, setting: AutoFetchSetting) => {
      autoFetchPrefs = withAutoFetchSetting(autoFetchPrefs, path, setting);
      try {
        saveAutoFetchPrefs(autoFetchPrefs);
      } catch (error) {
        diagnostics.warn("autoFetch", `could not save auto-fetch settings: ${formatError(error)}`);
      }
    },
    /**
     * Starts the opt-in auto-fetch timer. Returns its disposer. One
     * repository per tick at most; see `repos/autoFetch.ts` for the budget.
     */
    startAutoFetch: (): (() => void) => {
      const scheduler = createAutoFetchScheduler({
        prefs: () => autoFetchPrefs,
        now: () => Date.now(),
        candidates: () => {
          const byPath = new Map(wipInputs().map((input) => [input.path, input]));
          const activeId = internal.workspace.activeId;
          return internal.workspace.tabs.map((tab) => {
            const input = byPath.get(tab.path);
            return {
              path: tab.path,
              active: tab.id === activeId,
              skip: input ? bulkSkipReason(input) : "not loaded",
            };
          });
        },
        fetch: (path) => autoFetchRepo(path),
        onError: (path, error) =>
          diagnostics.warn("autoFetch", `auto-fetch of ${path} failed: ${formatError(error)}`),
      });
      return createAdaptiveTimer(() => void scheduler.tick(), AUTO_FETCH_TICK_MS);
    },
    push: async (remote?: string, branch?: string, force: boolean = false) =>
      runMutating(
        force ? "push-force" : "push",
        [remote, branch].filter(Boolean).join(" ") || "upstream",
        (path) =>
          invokeFn("cmd_push", { repoPath: path, remote, branch, force }),
      ),
    /**
     * File a GitHub issue on a checkout's remote. Without `target` that is the
     * active repository; a task board names its task's own checkout, which
     * need not be open at all. Either way the verdict and the action land in
     * the same harness record. Nothing in `.git` changes, so the explicit
     * form neither refreshes a session nor files its error on one — the
     * caller that asked owns the failure.
     */
    reportIssue: async (
      title: string,
      body: string,
      labels: string[] = [],
      target?: { repoPath: string },
    ): Promise<MutationOutcome<string>> => {
      const create = (path: string) =>
        invokeFn("cmd_github_create_issue", { repoPath: path, title, body, labels });
      const label = title.slice(0, 80);
      if (!target) return runMutating<string>("issue-report", label, create);
      const path = target.repoPath.trim();
      if (!path) return { ok: false, error: "No repository checkout was given for the issue." };
      try {
        const result = await create(path);
        const policy = recordPolicyVerdict(result, path);
        harnessStore.recordAction({ repoPath: path, kind: "issue-report", label, ok: true, verdict: policy ?? null });
        const output = mutationOutput<string>(result);
        return output === undefined ? { ok: true, policy } : { ok: true, policy, output };
      } catch (err: unknown) {
        harnessStore.recordAction({ repoPath: path, kind: "issue-report", label, ok: false });
        return { ok: false, error: formatError(err) };
      }
    },
    publishRelease: async (tag: string, message: string) =>
      runMutating<ReleasePublishResult>("release-publish", tag, (path) =>
        invokeFn("cmd_publish_release", { repoPath: path, tag, message }),
      ),
    stashSave: async (message?: string, options?: StashSaveOptions) =>
      runMutating("stash", message ?? "", (path) =>
        invokeFn("cmd_stash_save", { repoPath: path, message, options }),
      ),
    stashPop: async () => {
      // The menu/palette "Pop" used to call `git stash pop` on stash@{0}
      // sight-unseen. The stash stack is shared with every worktree and
      // agent: that path is what silently applied (or dropped) someone
      // else's entry. Pop the entry this session last listed, by object
      // id; refuse if the list could not be read or is empty.
      const session = activeSession();
      if (!session) return { ok: false as const, error: "No repository is open." };
      if (session.stashFailed) {
        return {
          ok: false as const,
          error:
            "The stash list could not be read, so popping it would target the wrong entry.",
        };
      }
      const top = session.stashEntries[0];
      if (!top) return { ok: false as const, error: "Nothing is stashed." };
      return runMutating("unstash", top.selector, (path) =>
        invokeFn("cmd_stash_action", {
          repoPath: path,
          action: "pop",
          index: top.index,
          expectedOid: top.oid,
        }),
      );
    },
    /**
     * Switch views, optionally landing on a particular section.
     *
     * Omitting `section` keeps whichever lens that view was last left on —
     * the point of remembering it per view. Passing one is how a caller says
     * "show them *this*", which is what the retired top-level tabs did by
     * being separate destinations.
     */
    setActiveTab: (tab: ViewTab, section?: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session) return;
      const resolved = section ? resolveSection(tab, section) : null;
      applyToSession(session.id, session.generation, {
        activeTab: tab,
        ...(resolved
          ? { viewSections: { ...session.viewSections, [tab]: resolved } }
          : {}),
      });
      // The view tab is persisted state: flush so quitting right after a
      // switch does not restore the previous one.
      flushPersist();
    },
    /** Change the lens within a view without leaving it. */
    setViewSection: (tab: ViewTab, section: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session) return;
      const resolved = resolveSection(tab, section);
      if (!resolved || session.viewSections[tab] === resolved) return;
      applyToSession(session.id, session.generation, {
        viewSections: { ...session.viewSections, [tab]: resolved },
      });
      flushPersist();
    },
    /**
     * Shows or hides the terminal dock on the ACTIVE repository tab.
     *
     * Scoped to one tab on purpose: a shell belongs to a working tree, and a
     * workspace-wide flag made every repository the user switched to inherit
     * — and start — a terminal they never opened.
     *
     * Hiding does not end the shells. `TerminalDock` keeps a hidden panel
     * mounted so its scrollback survives; closing the repository tab is what
     * disposes it. Returns whether anything changed, so a caller that opens
     * the dock to reveal something can tell a no-op from a real open.
     */
    setTerminalOpen: (open: boolean): boolean => {
      const session = activeSession();
      if (!session || session.terminalOpen === open) return false;
      if (!applyToSession(session.id, session.generation, { terminalOpen: open })) return false;
      flushPersist();
      return true;
    },
    toggleTerminal: (): boolean => {
      const session = activeSession();
      if (!session) return false;
      if (!applyToSession(session.id, session.generation, { terminalOpen: !session.terminalOpen })) return false;
      flushPersist();
      return true;
    },
    inspectCommitInHistory: (commitId: string) => {
      openEpoch += 1;
      const session = activeSession();
      if (!session || !commitId) return;
      applyToSession(session.id, session.generation, {
        selectedCommitId: commitId,
        selectedFilePath: null,
        selectedDiff: null,
        selectedDiffTruncated: false,
        selectedDiffTruncationReason: null,
        selectedDiffError: null,
        selectedIsStaged: false,
        selectionKind: "commit",
        activeTab: "history",
      });
      flushPersist();
    },
    setAmending: (isAmending: boolean) => {
      const session = activeSession();
      if (!session) return;
      applyToSession(session.id, session.generation, { isAmending });
    },
  };

  /**
   * Runs one mutating Git action and refreshes the session it belongs to.
   *
   * Gated commands come back as `{ policy, output }`: the harness's verdict
   * travels with the result so the UI can tell an action the gate approved from
   * one that ran with no gate available. The verdict is recorded here, in the
   * single place every mutation passes through, rather than at each call site
   * where one would eventually be forgotten — and the same pass files the
   * action into the agent journal, so an agent-driven session stays
   * reconstructible after the fact.
   */
  /**
   * Per-repository live facts, one record per open tab.
   *
   * `unpushedCommits` comes from the current branch's ahead count; a session
   * that has not hydrated reports zero, and `hydrated: false` is what tells
   * every consumer to treat that zero as unknown rather than as "nothing to
   * push". The same rule governs every other count here.
   *
   * This is the single extraction from session state. The work-in-progress
   * summary and the Fleet grid both narrow from it (see repos/facts.ts)
   * instead of walking `internal.sessions` themselves.
   */
  function repoFacts(): RepoFacts[] {
    const labels = disambiguateLabels(internal.workspace.tabs.map((tab) => tab.path));
    return internal.workspace.tabs.map((tab) => {
      const session = internal.sessions[tab.id];
      const statuses = session?.statuses ?? [];
      const current = session?.branches.find(
        (branch) => branch.is_current || branch.name === session?.currentBranch,
      );
      let additions = 0;
      let deletions = 0;
      let churnPartial = false;
      for (const file of statuses) {
        additions += file.additions;
        deletions += file.deletions;
        // Rust omits the key entirely while empty, so this is absent on the
        // overwhelming majority of rows. Where it is present, the sums above
        // are floors and must be rendered as such.
        if (file.warnings && file.warnings.length > 0) churnPartial = true;
      }
      return {
        path: tab.path,
        label: labels.get(tab.path) ?? displayName(tab.path),
        branch: session?.currentBranch ?? null,
        isBare: Boolean(session?.isBare),
        changedFiles: statuses.length,
        conflictedFiles: statuses.filter((file) => file.is_conflicted).length,
        stagedFiles: statuses.filter((file) => file.is_staged).length,
        additions,
        deletions,
        churnPartial,
        unpushedCommits: current?.ahead_count ?? 0,
        behindCommits: current?.behind_count ?? 0,
        // An unreadable stash list must not read as an empty one; consumers
        // treat a failed probe as unknown through `stashFailed`/`loadFailed`.
        stashEntries: session?.stashEntries.length ?? 0,
        stashFailed: Boolean(session?.stashFailed || session?.stashTruncated),
        operation: session?.operation ?? IDLE_OPERATION,
        watch: session?.watch ?? WATCH_UNKNOWN,
        loadFailed: Boolean(session?.error),
        loadError: session?.error ?? null,
        hydrated: Boolean(session?.hasHydrated),
      };
    });
  }

  let autoFetchPrefs: AutoFetchPrefs = loadAutoFetchPrefs();

  /**
   * One automatic fetch. Not a `runMutating`: nobody clicked, so a failure is
   * logged rather than filed on the session as the user's error, and only
   * the tab in front is refreshed — a background tab catches up when shown.
   */
  async function autoFetchRepo(path: string): Promise<AutoFetchOutcome> {
    const result = await invokeFn<unknown>("cmd_auto_fetch", { repoPath: path });
    recordPolicyVerdict(result, path);
    const outcome = parseAutoFetchOutcome(mutationOutput(result));
    if (outcome.status === "fetched") {
      mutationEchoUntil.set(path, Date.now() + WATCHER_ECHO_SUPPRESS_MS);
      const session = activeSession();
      if (session?.path === path) await store.refresh(path);
    }
    return outcome;
  }

  /** The work-in-progress model's narrower view of the same facts. */
  function wipInputs(): RepoWipInput[] {
    return repoFacts().map(toWipInput);
  }

  async function runStageBatch(kind: IndexAction): Promise<MutationOutcome> {
    const session = activeSession();
    if (!session) return { ok: false, error: "No active repository" };
    const files = session.statuses.filter((file) => kind === "unstage" ? file.is_staged : hasUnstagedChanges(file));
    if (files.length === 0) return { ok: true };
    if (kind === "stage" && files.some((file) => file.is_conflicted)) {
      return { ok: false, error: "Resolve conflicts before staging all files. Stage each resolved file explicitly." };
    }
    const filePaths = indexSelectionPaths(files, kind);
    const finishActivity = beginMutation(session.path, `${kind}-all`);
    try {
      const outcome = await runMutating(`${kind}-all`, `${filePaths.length} paths`, (path) =>
        invokeFn("cmd_change_index", { repoPath: path, filePaths, action: kind }), { session, trackActivity: false });
      // Native batches can stop after a completed chunk. Keep activity alive
      // through recovery and retain the diagnostic after hydration.
      if (!outcome.ok) {
        await store.refresh(session.path);
        applyToSession(session.id, session.generation, { error: outcome.error ?? "Index update failed" });
      }
      return outcome;
    } finally {
      finishActivity();
    }
  }

  async function runMutating<T = unknown>(
    kind: string,
    label: string,
    action: (path: string) => Promise<unknown>,
    opts: {
      skipRefresh?: boolean;
      session?: RepoSession;
      /**
       * The repository a prompt, confirm or toast was raised for. Given, the
       * mutation runs there even if another tab is active now, and is refused
       * when that tab is gone, repo-gone or untrusted. `null` is a caller that
       * had nothing to capture: refused, never "whichever tab is active".
       * Omitted, the active tab is the target.
       */
      target?: MutationTarget | null;
      trackActivity?: boolean;
    } = {},
  ): Promise<MutationOutcome<T>> {
    let session: RepoSession | undefined;
    if (opts.target !== undefined) {
      session = (opts.target && resolveTarget(opts.target)) || undefined;
      if (!session) return { ok: false, error: REPOSITORY_CHANGED, refused: true, surfaced: false };
    } else {
      session = opts.session ?? activeSession();
      if (!session) return { ok: false, error: "No repository is open." };
    }
    const path = session.path;
    const generation = session.generation;
    // Counted from the moment it is asked for, queued or running, so the quit
    // guard and the menu see a mutation that is still waiting its turn.
    const finishActivity = opts.trackActivity === false ? () => {} : beginMutation(path, kind);
    const release = await enterPathQueue(path);
    let succeeded = false;
    let refreshed = false;
    try {
      // The wait may have been long: the tab can be gone or its checkout
      // missing by now.
      const stillNamed = opts.target !== undefined
        ? opts.target !== null && resolveTarget(opts.target) !== null
        : internal.sessions[session.id] !== undefined;
      if (!stillNamed) return { ok: false, error: REPOSITORY_CHANGED, refused: true, surfaced: false };
      const result = await action(path);
      succeeded = true;
      const policy = recordPolicyVerdict(result, path);
      harnessStore.recordAction({
        repoPath: path,
        kind,
        label,
        ok: true,
        verdict: policy ?? null,
      });
      const still = internal.sessions[session.id];
      if (still && still.generation === generation && !opts.skipRefresh) {
        await store.refresh(path);
        refreshed = true;
        // The refresh updates statuses/branches but nothing refetches the
        // open diff pane, which would otherwise show pre-mutation content
        // (e.g. after partial staging) until the user clicked elsewhere.
        // Commit/range selections have no worktree diff to refetch — and
        // neither does a closed diff pane: refetching one unconditionally
        // would yank the user back to Diff from wherever they navigated.
        if (
          internal.workspace.activeId === session.id &&
          REFETCH_SELECTION_KINDS.has(kind) &&
          still.selectionKind === "file" &&
          still.selectedFilePath &&
          !still.selectedCommitId &&
          isSectionOnScreen(still.activeTab, still.viewSections, "history", "diff")
        ) {
          void store.selectFileDiff(
            still.selectedFilePath,
            still.selectedIsStaged,
          );
        }
      }
      const output = mutationOutput<T>(result);
      return output === undefined
        ? { ok: true, policy }
        : { ok: true, policy, output };
    } catch (err: unknown) {
      // The error is filed on the session, as it always was, and also returned:
      // a caller that must react to a refusal — the commit box keeping the
      // message it was about to commit — should not have to watch shared state
      // to find out whether its own call went through.
      const error = formatError(err);
      applyToSession(session.id, generation, { error });
      harnessStore.recordAction({ repoPath: path, kind, label, ok: false });
      // Surfaced when the banner the app toasts from is showing THIS error: a
      // concurrent failure with the same message shares one banner.
      return {
        ok: false,
        error,
        surfaced: internal.workspace.activeId === session.id && internal.sessions[session.id]?.error === error,
      };
    } finally {
      // The queue is released FIRST: a subscriber that throws while activity
      // is published below must not strand every later mutation on this path.
      release();
      try {
        // The trailing window opens when the mutation, refresh included, ends,
        // and only for one that succeeded (see isEchoOfOwnWrite).
        if (succeeded) mutationEchoUntil.set(path, Date.now() + WATCHER_ECHO_SUPPRESS_MS);
        finishActivity();
      } finally {
        replayDroppedEchoes(path, refreshed);
      }
    }
  }

  return store;
}

/**
 * Files a `Guarded<T>` result's verdict with the harness store, and clears the
 * last verdict for an action that carried none.
 */
function recordPolicyVerdict(
  result: unknown,
  repoPath: string,
): PolicyVerdict | undefined {
  if (result && typeof result === "object" && "policy" in result) {
    const policy = (result as { policy: PolicyVerdict }).policy;
    if (policy && typeof policy.status === "string") {
      harnessStore.recordVerdict(policy, repoPath);
      return policy;
    }
  }
  harnessStore.recordVerdict(null, repoPath);
  return undefined;
}

function mutationOutput<T>(result: unknown): T | undefined {
  if (result && typeof result === "object" && "output" in result) {
    return (result as { output: T }).output;
  }
  return result === undefined ? undefined : (result as T);
}

export const repoStore = createRepoStore();
/** `repoStore.currentPath`, importable as a store of its own. */
export const currentRepoPath = repoStore.currentPath;
