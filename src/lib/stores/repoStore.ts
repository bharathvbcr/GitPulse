import type { RebaseStep } from "../rebase/planner";
import { derived, get, readable, writable, type Readable } from "svelte/store";
import { invoke, ipcErrorKind } from "../ipc/invoke";
import { formatErrorRaw } from "../ui/formatError";
import { describeError, mirrorUserFacingError } from "../ui/copy/describeFailure";
import { diagnostics } from "../diagnostics/diagnostics";
import { harnessStore } from "./harnessStore";
import { toastStore } from "./toastStore";
import {
  type BranchInfo,
  type DeadbranchCleanResult,
  type DeadbranchRestoreResult,
  type MergeTeardownResult,
  type TagInfo,
  type WorktreeCreated,
  type WorktreeRemoval,
} from "../branches/types";
import type { DeleteOutcome } from "../files/types";
import { readFileWriteError } from "../files/fileWrite";
import { filterStore } from "./filterStore";
import { graphStore, type InvokeFn } from "./graphStore";
import { sessionsByRepo, terminalSessions } from "../terminal/sessionRegistry";
import { requestRepositoryTrust } from "../repos/repositoryTrust";
import {
  displayName,
  isCaseInsensitiveFs,
  sameRepo,
  type PathIdentityOptions,
} from "../repos/paths";
import {
  browserStorage,
  savePersistedWorkspace,
  workspaceToPersisted,
  type ViewTab,
} from "../repos/persist";
import { emptyWorkspace, MAX_OPEN_TABS, type WorkspaceTabs } from "../repos/tabModel";
import * as workspaceSync from "../codeintel/workspaceSync";
import { autoInit } from "../codeintel/autoInit";
import { liveIndex } from "../codeintel/liveIndex";
import { familyFromCommonDir } from "../repos/repoFamily";
import { noteActiveCheckout } from "../repos/stackState";
import { type StashAction, type StashEntry, type StashSaveOptions } from "../repos/stash";
import type { MergeMode } from "../branches/mergeSelection";
import {
  parseGitPreflight,
  type GitIdentity,
  type GitPreflight,
  type IdentityScope,
} from "../repos/gitPreflight";
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
  operationStatesEqual,
  type OperationAction,
  type OperationState,
  type RepoOperation,
} from "../repos/operation";
import { shallowRecordListEqual } from "../repos/statusPoll";
import { readBackgroundDocument } from "../runtime/foreground";
import { decideCadence } from "../runtime/loadCadence";
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
import {
  collectRepoFacts,
  pathsTrustedForBackground,
  toWipInput,
  type RepoFacts,
} from "../repos/facts";
import { watchStatesEqual, type WatchState } from "../repos/watchState";
import { createSessionBook } from "../repos/sessionBook";
import { createCommitDraftStore } from "./commitDraftStore";
import { flushOnPageHide } from "../async/deferredWrite";
import type { FilePatch } from "../diff/patchBuilder";
import type { ReleasePublishResult } from "../ops/model";
import {
  ACTIVATION_FRESH_MS,
  createHydrationController,
  facetForChange,
  mergeFacets,
  repositoryTrustRefused,
  STATS_DRAIN_MAX_BATCHES,
  STATS_PUBLISH_EVERY,
  type BranchGraphFacetPayload,
  type FileStatusFacetPayload,
  type HydrationFacet,
  type MetadataFacetPayload,
} from "../repos/hydration";
import {
  createStatusPoller,
  STATUS_UNAVAILABLE_AFTER_FAILURES,
} from "../repos/statusPoller";
import { createWatchCoordinator } from "../repos/watchCoordinator";
import { createTabActions } from "../repos/tabActions";
import { createViewNavigation } from "../repos/viewNavigation";
import {
  createMutationRunner,
  mutationOutput,
  recordPolicyVerdict,
  REPOSITORY_CHANGED,
  WORKTREE_ONLY,
} from "../repos/mutationRunner";
import type {
  MutationOutcome,
  MutationTarget,
  ResetMode,
  IndexAction,
  SelectionKind,
  FileStatus,
  ResolvedRepo,
  BranchStatsReport,
  OpenRepoTab,
  DiffPayload,
  RepoSession,
  RepoState,
  InternalState,
  RepoStoreDeps,
} from "../repos/storeTypes";
import { createProjectionCache, emptyProjected, project } from "../repos/projection";

export type {
  MutationOutcome,
  MutationTarget,
  ResetMode,
  IndexAction,
  SelectionKind,
  FileStatus,
  ResolvedRepo,
  BranchStatsReport,
  OpenRepoTab,
  DiffPayload,
  RepoSession,
  RepoState,
  RepoStoreDeps,
};
export type { BranchInfo, TagInfo, ViewTab };
export type { OperationAction, OperationState, RepoOperation };
export type { StashAction, StashEntry, RemoteChange, SubmoduleChange };
export type { WatchState };
export type { BulkRunReport, WorkspaceWip };
export type { RepoFacts };
export type { InvokeFn };
export type {
  FileStatusFacetPayload,
  BranchGraphFacetPayload,
  MetadataFacetPayload,
  HydrationFacet,
};
export {
  facetForChange,
  mergeFacets,
  STATS_DRAIN_MAX_BATCHES,
  ACTIVATION_FRESH_MS,
  STATUS_UNAVAILABLE_AFTER_FAILURES,
  STATS_PUBLISH_EVERY,
  repositoryTrustRefused,
  pathsTrustedForBackground,
};

const MENU_RECENT_CAP = 12;
/** Trailing debounce for localStorage writes and the native-menu rebuild. */
const PERSIST_DEBOUNCE_MS = 300;

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
      sessionsByRepo(
        get(terminalSessions).filter((record) => !!record.checkout),
        options,
      ).get(repoPath) ?? 0,
  };

  /** Rows `publish` can reuse; see `ProjectionCache`. */
  const projection = createProjectionCache();
  let internal: InternalState = {
    workspace: emptyWorkspace(),
    sessions: {},
    workspaceError: null,
  };

  const { subscribe, set } = writable<RepoState>(emptyProjected());

  function field<T>(select: (state: RepoState) => T): Readable<T> {
    return readable(select(get({ subscribe })), (setValue) => {
      let held: { value: T } | null = null;
      return subscribe((state) => {
        const value = select(state);
        if (held !== null && Object.is(held.value, value)) return;
        held = { value };
        setValue(value);
      });
    });
  }

  const contentRevisions = writable<Record<string, string>>({});
  let openEpoch = 0;
  let syncingFilter = false;

  const sessionBook = createSessionBook((a, b) => sameRepo(a, b, options));
  const { snapshotRuns, pollRuns, statusPollApplied, statsInflight } = sessionBook;

  let lastPersistedPayload: string | null = null;
  let lastSentRecentsJson: string | null = null;
  let lastWorkspaceSyncKey: string | null = null;
  const commitDrafts = createCommitDraftStore();
  let draftTabs: readonly { id: string }[] | null = null;
  let lastLiveScopeKey: string | null = null;
  let persistTimer: ReturnType<typeof setTimeout> | null = null;
  let workspaceEpoch = 0;
  let droppedPaths: string[] = [];
  let persistSuspended = 0;

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
    if (activeId) {
      const family = familyFromCommonDir(internal.sessions[activeId]?.commonDir, options);
      noteActiveCheckout(family?.key, activeId);
    }
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
    const visible = !readBackgroundDocument();
    const syncKey = JSON.stringify([trusted.activeKey, trusted.retainedKeys]);
    if (syncKey !== lastWorkspaceSyncKey) {
      lastWorkspaceSyncKey = syncKey;
      workspaceSync.scheduleWorkspaceSync(trusted.activeKey, trusted.retainedKeys);
      autoInit.setScope({
        activeKey: trusted.activeKey,
        retainedKeys: trusted.retainedKeys,
        visible,
      });
    }
    const liveKey = JSON.stringify([trusted.activeKey, trusted.liveKeys]);
    if (liveKey !== lastLiveScopeKey) {
      lastLiveScopeKey = liveKey;
      liveIndex.setScope({
        activeKey: trusted.activeKey,
        retainedKeys: trusted.liveKeys,
        visible,
      });
    }
  }

  function commitEdit(closed: readonly string[] = []) {
    workspaceEpoch += 1;
    for (const path of closed) {
      if (path && !droppedPaths.includes(path)) droppedPaths.push(path);
    }
    if (droppedPaths.length > MAX_OPEN_TABS * 2) {
      droppedPaths = droppedPaths.slice(-MAX_OPEN_TABS * 2);
    }
  }

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

  function replaceWorkspace(next: WorkspaceTabs) {
    internal = { ...internal, workspace: next };
  }

  function putSession(session: RepoSession) {
    internal.sessions[session.id] = session;
  }

  function activeSession(): RepoSession | undefined {
    const id = internal.workspace.activeId;
    return id ? internal.sessions[id] : undefined;
  }

  function resolveTarget(target: MutationTarget): RepoSession | null {
    const session = internal.sessions[target.tabId];
    if (!session || !sameRepo(session.path, target.path, options)) return null;
    if (session.missing || session.trustRequired) return null;
    return session;
  }

  function patchIsNoop(session: RepoSession, patch: Partial<RepoSession>): boolean {
    for (const key of Object.keys(patch) as (keyof RepoSession)[]) {
      const incoming = patch[key];
      if (incoming === session[key]) continue;
      if (key === "watch") {
        if (watchStatesEqual(session.watch, incoming as unknown as WatchState)) continue;
        return false;
      }
      if (key === "operation") {
        if (
          operationStatesEqual(session.operation, incoming as unknown as OperationState)
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
    patchOrFn: Partial<RepoSession> | ((current: RepoSession) => Partial<RepoSession>),
  ) {
    const session = internal.sessions[id];
    if (!session || session.generation !== generation) return false;
    const patch = typeof patchOrFn === "function" ? patchOrFn(session) : patchOrFn;
    if (patchIsNoop(session, patch)) return false;
    putSession({ ...session, ...patch });
    publish();
    return true;
  }

  function bumped(session: RepoSession): RepoSession {
    return {
      ...session,
      generation: session.generation + 1,
      // A drain still running for the old generation hands its work to the new
      // one when it stops (`fetchBranchStats`), and that call decides when the
      // numbers are complete. Clearing the flag here would show partial stats
      // as finished until then.
      statsPending: statsInflight.has(session.id) ? session.statsPending : false,
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

  function setError(error: string | null): void {
    if (error) mirrorUserFacingError("repo", error);
    const session = activeSession();
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
  }

  const hydration = createHydrationController({
    invoke: invokeFn,
    sessionById: (id) => internal.sessions[id],
    applyToSession,
    sessionBook,
    onContentRevision: (path, revision) => {
      contentRevisions.update((revisions) => ({
        ...Object.fromEntries(
          Object.values(internal.sessions)
            .filter((session) => revisions[session.path] !== undefined)
            .map((session) => [session.path, revisions[session.path]]),
        ),
        [path]: revision,
      }));
    },
  });

  async function refresh(repoPath?: string, facet: HydrationFacet = "all") {
    const session = repoPath
      ? Object.values(internal.sessions).find((item) => sameRepo(item.path, repoPath, options))
      : activeSession();
    if (!session || session.trustRequired) return;
    const generation = session.generation;
    applyToSession(session.id, generation, {
      isLoading: !session.hasHydrated,
    });
    await hydration.hydrate(session.id, session.path, generation, facet);
    const latest = internal.sessions[session.id];
    if (
      latest &&
      internal.workspace.activeId === session.id &&
      (facet === "all" || facet === "branches")
    ) {
      void graph.loadGraph(latest.path, latest.searchQuery, latest.selectedBranch);
    }
  }

  let mutationRunner!: ReturnType<typeof createMutationRunner>;

  const watchCoordinator = createWatchCoordinator({
    invoke: invokeFn,
    options,
    sessionBook,
    sessionById: (id) => internal.sessions[id],
    activeSession,
    findSessionByPath: (repoPath) =>
      Object.values(internal.sessions).find((item) => sameRepo(item.path, repoPath, options)),
    applyToSession,
    refresh,
    holdsPath: (path) => mutationRunner.holdsPath(path),
  });

  const statusPoller = createStatusPoller({
    invoke: invokeFn,
    activeSession,
    sessionById: (id) => internal.sessions[id],
    applyToSession,
    snapshotRunOf: (id) => snapshotRuns.get(id),
    getPollRun: (id) => pollRuns.get(id),
    setPollRun: (id, run) => {
      pollRuns.set(id, run);
    },
    deletePollRun: (id) => {
      pollRuns.delete(id);
    },
    notePollApplied: (id) => {
      statusPollApplied.set(id, (statusPollApplied.get(id) ?? 0) + 1);
    },
    acquireWatch: watchCoordinator.acquireWatch,
    hydrate: (id, path, generation) => hydration.hydrate(id, path, generation),
    decideCadence,
    isBackground: readBackgroundDocument,
  });

  function ensureStatusPoll() {
    statusPoller.ensure();
  }

  function stopStatusPoll() {
    statusPoller.stop();
  }

  async function runStatusPoll() {
    await statusPoller.run();
  }
  // Retain the lifecycle wrapper name pinned by repoStore.lifecycle.test.ts.
  void runStatusPoll;

  // For the life of the store, not only while the poll runs. `pagehide` is
  // fired at window; this binds through the shared flush registry.
  flushOnPageHide(() => flushPersist(true));

  const tabActions = createTabActions({
    invoke: invokeFn,
    storage,
    options,
    graph,
    terminals,
    projection,
    sessionBook,
    commitDrafts,
    watchCoordinator,
    hydration,
    statusPoller: {
      ensure: ensureStatusPoll,
      stop: stopStatusPoll,
      rearm: () => statusPoller.rearm(),
      resetDuration: () => statusPoller.resetDuration(),
    },
    getInternal: () => internal,
    setWorkspaceError: (workspaceError) => {
      internal = { ...internal, workspaceError };
    },
    setSessions: (sessions) => {
      internal = { ...internal, sessions };
    },
    replaceWorkspace,
    putSession,
    activeSession,
    applyToSession,
    bumped,
    resolveTarget,
    syncFilterFromSession,
    revealGraph,
    publish,
    holdPublish,
    commitEdit,
    flushPersist,
    suspendPersist: () => {
      persistSuspended += 1;
      let resumed = false;
      return () => {
        if (resumed) return;
        resumed = true;
        persistSuspended -= 1;
        flushPersist(true);
      };
    },
    resetLoadedWorkspace: (epoch) => {
      workspaceEpoch = epoch;
      droppedPaths = [];
    },
    getOpenEpoch: () => openEpoch,
    bumpOpenEpoch: () => ++openEpoch,
    setError,
  });

  const viewNav = createViewNavigation({
    invoke: invokeFn,
    options,
    activeSession,
    applyToSession,
    getOpenEpoch: () => openEpoch,
    bumpOpenEpoch: () => ++openEpoch,
    openRepo: tabActions.openRepo,
    flushPersist,
  });

  mutationRunner = createMutationRunner({
    activeSession,
    sessionById: (id) => internal.sessions[id],
    activeId: () => internal.workspace.activeId,
    resolveTarget,
    applyToSession,
    refresh,
    selectFileDiff: viewNav.selectFileDiff,
    markEchoSuccess: watchCoordinator.markEchoSuccess,
    replayDroppedEchoes: watchCoordinator.replayDroppedEchoes,
  });

  const { beginMutation, runMutating } = mutationRunner;

  /** The secret scan refused the push: the one failure "Push anyway" answers. */
  const secretScanRefused = (err: unknown) => ipcErrorKind(err) === "secret_scan_blocked";

  /**
   * A push the secret scan refused, offered once more past the scan. The
   * toast carries the refusal (shape, commit, path and line, never a value);
   * its "Push anyway" button is the only caller of `again`, which pushes the
   * repository that was refused, not whichever tab is active by then. No
   * member takes the override from a caller, so no other path can send it.
   */
  function offerPushPastSecretScan(
    outcome: MutationOutcome,
    again: () => Promise<MutationOutcome>,
  ): MutationOutcome {
    if (outcome.ok || !secretScanRefused(outcome.cause)) return outcome;
    // Sticky (duration 0): an action toast otherwise expires, and this one is
    // the only place the refusal's findings are shown.
    toastStore.error(
      outcome.error ?? "Push blocked by the secret scan.",
      { label: "Push anyway", onClick: async () => (await again()).ok },
      0,
    );
    return { ...outcome, surfaced: true };
  }

  function repoFacts(): RepoFacts[] {
    return collectRepoFacts(internal.workspace.tabs, internal.sessions);
  }

  let autoFetchPrefs: AutoFetchPrefs = loadAutoFetchPrefs();

  async function autoFetchRepo(path: string): Promise<AutoFetchOutcome> {
    const result = await invokeFn<unknown>("cmd_auto_fetch", { repoPath: path });
    recordPolicyVerdict(result, path);
    const outcome = parseAutoFetchOutcome(mutationOutput(result));
    if (outcome.status === "fetched") {
      watchCoordinator.markEchoSuccess(path);
      const session = activeSession();
      if (session?.path === path) await refresh(path);
    }
    return outcome;
  }

  function wipInputs(): RepoWipInput[] {
    return repoFacts().map(toWipInput);
  }

  async function runStageBatch(kind: IndexAction): Promise<MutationOutcome> {
    const session = activeSession();
    if (!session) return { ok: false, error: "No active repository" };
    const files = session.statuses.filter((file) =>
      kind === "unstage" ? file.is_staged : hasUnstagedChanges(file),
    );
    if (files.length === 0) return { ok: true };
    if (kind === "stage" && files.some((file) => file.is_conflicted)) {
      return {
        ok: false,
        error: "Resolve conflicts before staging all files. Stage each resolved file explicitly.",
      };
    }
    const filePaths = indexSelectionPaths(files, kind);
    const finishActivity = beginMutation(session.path, `${kind}-all`);
    try {
      const outcome = await runMutating(
        `${kind}-all`,
        `${filePaths.length} paths`,
        (path) => invokeFn("cmd_change_index", { repoPath: path, filePaths, action: kind }),
        { session, trackActivity: false, ...WORKTREE_ONLY },
      );
      if (!outcome.ok) {
        await refresh(session.path, "status");
        applyToSession(session.id, session.generation, {
          error: outcome.error ?? "Index update failed",
        });
      }
      return outcome;
    } finally {
      finishActivity();
    }
  }

  const store = {
    subscribe,
    currentPath: derived({ subscribe }, (state) => state.currentPath),
    statuses: field((state) => state.statuses),
    branches: field((state) => state.branches),
    openTabs: field((state) => state.openTabs),
    conflictedCount: field((state) => state.conflictedCount),
    stagedCount: field((state) => state.stagedCount),
    commitDraft: commitDrafts,
    mutationActivity: mutationRunner.mutationActivity,
    captureTarget: (): MutationTarget | null => {
      const session = activeSession();
      return session ? { tabId: session.id, path: session.path } : null;
    },
    targetIsActive: (target: MutationTarget | null): boolean => {
      const session = activeSession();
      return Boolean(target && session && resolveTarget(target) === session);
    },
    contentRevisions: { subscribe: contentRevisions.subscribe },
    setError,
    trustRepo: (path: string) => requestRepositoryTrust(path, "Trust Repository", invokeFn),
    retainedSessionRecords: (): number => sessionBook.size(),
    ...tabActions,
    refresh,
    hydrateFacet: async (repoPath?: string, facet: HydrationFacet = "all") =>
      refresh(repoPath, facet),
    loadFileStatusFacet: hydration.loadFileStatusFacet,
    loadBranchGraphFacet: hydration.loadBranchGraphFacet,
    loadMetadataFacet: hydration.loadMetadataFacet,
    onPolledChange: statusPoller.onPolledChange,
    handleRepoChanged: watchCoordinator.handleRepoChanged,
    markRepoGone: watchCoordinator.markRepoGone,
    flushPersistedWorkspace: () => {
      flushPersist(true);
    },
    ...viewNav,
    stageFile: async (filePath: string) =>
      runMutating(
        "stage",
        filePath,
        (path) => invokeFn("cmd_stage_file", { repoPath: path, filePath }),
        WORKTREE_ONLY,
      ),
    unstageFile: async (filePath: string) => {
      const session = activeSession();
      if (!session) return { ok: false, error: "No active repository" };
      const file = session.statuses.find(
        (entry) => entry.path === filePath && entry.is_staged,
      );
      const filePaths = file ? indexSelectionPaths([file], "unstage") : [filePath];
      return runMutating(
        "unstage",
        filePath,
        (path) =>
          filePaths.length > 1
            ? invokeFn("cmd_change_index", { repoPath: path, filePaths, action: "unstage" })
            : invokeFn("cmd_unstage_file", { repoPath: path, filePath }),
        { session, ...WORKTREE_ONLY },
      );
    },
    stageSelectivePatch: async (filePatch: FilePatch, isStaging: boolean = true) => {
      if (isStaging) {
        return runMutating(
          "stage-patch",
          filePatch.new_path,
          (path) => invokeFn("cmd_stage_selective_patch", { repoPath: path, filePatch }),
          WORKTREE_ONLY,
        );
      } else {
        return runMutating(
          "unstage-patch",
          filePatch.new_path,
          (path) => invokeFn("cmd_unstage_selective_patch", { repoPath: path, filePatch }),
          WORKTREE_ONLY,
        );
      }
    },
    stageAll: () => runStageBatch("stage"),
    unstageAll: () => runStageBatch("unstage"),
    discardChanges: async (filePath: string, target?: MutationTarget | null) =>
      runMutating(
        "discard",
        filePath,
        (path) => invokeFn("cmd_discard_changes", { repoPath: path, filePath }),
        { target, ...WORKTREE_ONLY },
      ),
    // `allowSecrets` is the person's "Commit anyway" after the secret scan
    // refused this commit (`secret_scan_blocked`); nothing else passes it.
    commit: async (message: string, amend: boolean = false, allowSecrets: boolean = false) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_commit", { repoPath: path, message, amend, allowSecrets }),
      ),
    quickCommit: async (message: string, allowSecrets: boolean = false) =>
      runMutating("commit", message.split("\n")[0].slice(0, 80), (path) =>
        invokeFn("cmd_quick_commit", { repoPath: path, message, allowSecrets }),
      ),
    checkoutBranch: async (branchName: string) =>
      runMutating("checkout", branchName, (path) =>
        invokeFn("cmd_checkout_branch", { repoPath: path, branchName }),
      ),
    checkoutDetached: async (revision: string) =>
      runMutating("checkout", revision, (path) =>
        invokeFn("cmd_checkout_detached", { repoPath: path, revision }),
      ),
    createBranch: async (
      branchName: string,
      startPoint?: string,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        "branch",
        branchName,
        (path) => invokeFn("cmd_create_branch", { repoPath: path, branchName, startPoint }),
        { target, facet: "branches" },
      ),
    renameBranch: async (
      oldName: string,
      newName: string,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        "branch-rename",
        `${oldName} → ${newName}`,
        (path) => invokeFn("cmd_rename_branch", { repoPath: path, oldName, newName }),
        { target, facet: "branches" },
      ),
    deleteBranch: async (
      branchName: string,
      force: boolean = false,
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating(
        force ? "branch-delete-force" : "branch-delete",
        branchName,
        (path) => invokeFn("cmd_delete_branch", { repoPath: path, branchName, force }),
        { target, facet: "branches", expected: handled ? () => true : undefined },
      ),
    stashAction: async (action: StashAction, entry: StashEntry) =>
      runMutating(`stash-${action}`, entry.selector, (path) =>
        invokeFn("cmd_stash_action", {
          repoPath: path,
          action,
          index: entry.index,
          expectedOid: entry.oid,
        }),
      ),
    stashShow: async (oid: string): Promise<DiffPayload> => {
      const session = activeSession();
      if (!session) throw new Error("No repository is open.");
      return invokeFn<DiffPayload>("cmd_stash_show", { repoPath: session.path, oid });
    },
    cherryPick: async (commits: string[], noCommit = false) =>
      runMutating("cherry-pick", commits.join(", "), (path) =>
        invokeFn("cmd_cherry_pick", { repoPath: path, commits, noCommit }),
      ),
    revertCommits: async (commits: string[], noCommit = false) =>
      runMutating("revert", commits.join(", "), (path) =>
        invokeFn("cmd_revert", { repoPath: path, commits, noCommit }),
      ),
    resetTo: async (
      mode: ResetMode,
      target: string,
      repository?: MutationTarget | null,
    ) =>
      runMutating(
        `reset-${mode}`,
        target,
        (path) => invokeFn("cmd_reset", { repoPath: path, mode, target }),
        { target: repository },
      ),
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
    listSubmodules: async (): Promise<{
      submodules: SubmoduleInfo[];
      truncated: boolean;
    }> => {
      const session = activeSession();
      if (!session) return { submodules: [], truncated: false };
      const raw = await invokeFn<unknown>("cmd_list_submodules", {
        repoPath: session.path,
      });
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
    createTag: async (
      tagName: string,
      commitId?: string,
      message?: string,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        "tag",
        tagName,
        (path) =>
          invokeFn("cmd_create_tag", {
            repoPath: path,
            tagName,
            commitId: commitId ?? null,
            message: message ?? null,
          }),
        { target },
      ),
    deleteTag: async (tagName: string, target?: MutationTarget | null) =>
      runMutating(
        "tag-delete",
        tagName,
        (path) => invokeFn("cmd_delete_tag", { repoPath: path, tagName }),
        { target },
      ),
    createFile: async (filePath: string, target?: MutationTarget | null) =>
      runMutating(
        "create",
        filePath,
        (path) => invokeFn("cmd_create_file", { repoPath: path, filePath }),
        {
          target,
          expected: (err) => {
            const refusal = readFileWriteError(err);
            return refusal?.code === "already_exists" && refusal.path === filePath;
          },
        },
      ),
    movePath: async (from: string, to: string, target?: MutationTarget | null) =>
      runMutating(
        "move",
        `${from} → ${to}`,
        (path) => invokeFn("cmd_move_path", { repoPath: path, from, to }),
        { target },
      ),
    deletePath: async (filePath: string, target?: MutationTarget | null) =>
      runMutating<DeleteOutcome>(
        "delete",
        filePath,
        (path) => invokeFn("cmd_delete_path", { repoPath: path, path: filePath }),
        { target },
      ),
    pruneWorktrees: async (target?: MutationTarget | null) =>
      runMutating(
        "worktree-prune",
        "stale worktree metadata",
        (path) => invokeFn("cmd_prune_worktree", { repoPath: path }),
        { target },
      ),
    worktreeCowSync: async (
      worktreePath: string,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        "worktree-cow-sync",
        worktreePath,
        (path) =>
          invokeFn("cmd_worktree_cow_sync", { repoPath: path, targetPath: worktreePath }),
        { target },
      ),
    addWorktree: async (
      request: {
        targetPath: string;
        newBranch: string | null;
        startPoint: string | null;
        detach: boolean;
        cowCaches: boolean;
        runHooks: boolean;
        seedDevmap?: boolean;
        zeroCopyDevmap?: boolean;
        sparsePaths?: string[] | null;
        forkFrom?: string | null;
      },
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating<WorktreeCreated>(
        "worktree",
        request.newBranch ? `${request.newBranch} → ${request.targetPath}` : request.targetPath,
        (path) =>
          invokeFn("cmd_add_worktree", {
            repoPath: path,
            targetPath: request.targetPath,
            newBranch: request.newBranch,
            startPoint: request.startPoint,
            detach: request.detach,
            cowCaches: request.cowCaches,
            runHooks: request.runHooks,
            ...(request.seedDevmap !== undefined ? { seedDevmap: request.seedDevmap } : {}),
            ...(request.zeroCopyDevmap !== undefined
              ? { zeroCopyDevmap: request.zeroCopyDevmap }
              : {}),
            ...(request.sparsePaths !== undefined ? { sparsePaths: request.sparsePaths } : {}),
            ...(request.forkFrom !== undefined ? { forkFrom: request.forkFrom } : {}),
          }),
        { target, expected: handled ? () => true : undefined },
      ),
    removeWorktree: async (
      targetPath: string,
      force: boolean,
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating<WorktreeRemoval>(
        "worktree-remove",
        targetPath,
        (path) => invokeFn("cmd_remove_worktree", { repoPath: path, targetPath, force }),
        { target, expected: handled ? () => true : undefined },
      ),
    lockWorktree: async (
      targetPath: string,
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating(
        "worktree-lock",
        targetPath,
        (path) =>
          invokeFn("cmd_lock_worktree", { repoPath: path, targetPath, reason: null }),
        { target, expected: handled ? () => true : undefined },
      ),
    unlockWorktree: async (
      targetPath: string,
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating(
        "worktree-unlock",
        targetPath,
        (path) => invokeFn("cmd_unlock_worktree", { repoPath: path, targetPath }),
        { target, expected: handled ? () => true : undefined },
      ),
    worktreeMergeTeardown: async (
      request: {
        worktreePath: string;
        targetBranch: string | null;
        squash: boolean;
        expectedTip: string;
        requireWriteTree?: boolean;
        taskId?: string;
      },
      target?: MutationTarget | null,
      handled: boolean = false,
    ) =>
      runMutating<MergeTeardownResult>(
        "worktree-merge",
        `${request.worktreePath} merge-teardown`,
        (path) =>
          invokeFn("cmd_worktree_merge_teardown", {
            repoPath: path,
            worktreePath: request.worktreePath,
            targetBranch: request.targetBranch,
            squash: request.squash,
            expectedTip: request.expectedTip,
            requireWriteTree: request.requireWriteTree === true,
            ...(request.taskId ? { taskId: request.taskId } : {}),
          }),
        { target, expected: handled ? () => true : undefined },
      ),
    deadbranchClean: async (
      branches: string[],
      force: boolean,
      createBackup: boolean,
      target?: MutationTarget | null,
    ) =>
      runMutating<DeadbranchCleanResult>(
        "deadbranch-clean",
        `${branches.length} branches`,
        (path) =>
          invokeFn("cmd_deadbranch_clean", {
            repoPath: path,
            branches,
            force,
            createBackup,
          }),
        { target },
      ),
    deadbranchRestore: async (backupPath: string, target?: MutationTarget | null) =>
      runMutating<DeadbranchRestoreResult>(
        "deadbranch-restore",
        backupPath,
        (path) =>
          invokeFn("cmd_deadbranch_restore", {
            repoPath: path,
            backupPath,
            branches: null,
          }),
        { target },
      ),
    workspaceWip: (): WorkspaceWip => summarizeWorkspace(wipInputs()),
    repoFacts: (): RepoFacts[] => repoFacts(),
    runAcrossOpenRepos: async (
      kind: "fetch" | "pull",
      runOptions: RunOptions & { only?: readonly string[] } = {},
    ): Promise<BulkRunReport> => {
      const byPath = new Map(wipInputs().map((input) => [input.path, input]));
      const restrict = runOptions.only ? new Set(runOptions.only) : null;
      const targets: RepoTarget[] = internal.workspace.tabs
        .filter((tab) => restrict === null || restrict.has(tab.path))
        .map((tab) => ({
          path: tab.path,
          label: byPath.get(tab.path)?.label ?? displayName(tab.path),
        }));
      const tabIds = new Map(internal.workspace.tabs.map((tab) => [tab.path, tab.id]));
      const endSweep = beginMutation("", `${kind}-all`);
      try {
        const report = await runAcrossRepos(
          targets,
          async (target) => {
            const input = byPath.get(target.path);
            const skip = input ? bulkSkipReason(input) : "Repository is no longer open.";
            if (skip) return { skip };
            const tabId = tabIds.get(target.path);
            const mutationTarget = tabId ? { tabId, path: target.path } : null;
            if (!mutationTarget || !resolveTarget(mutationTarget)) {
              return { skip: "Repository is no longer open, or is unavailable." };
            }
            const outcome = await runMutating(
              kind,
              target.label,
              (path) =>
                kind === "fetch"
                  ? invokeFn("cmd_fetch", { repoPath: path })
                  : invokeFn("cmd_pull", { repoPath: path }),
              { target: mutationTarget, skipRefresh: true },
            );
            if (!outcome.ok) throw new Error(outcome.error ?? `${kind} failed`);
          },
          runOptions,
        );
        await mapItems(
          report.results.filter((result) => result.status === "ok"),
          DEFAULT_FAN_OUT,
          (result) => refresh(result.path),
        );
        return report;
      } finally {
        endSweep();
      }
    },
    operationAction: async (action: OperationAction) =>
      runMutating(`operation-${action}`, action, (path) =>
        invokeFn("cmd_repo_operation_action", { repoPath: path, action }),
      ),
    rebaseInteractive: (ontoCommit: string, steps: RebaseStep[]) =>
      runMutating("rebase", ontoCommit, (path) =>
        invokeFn("cmd_rebase_interactive", { repoPath: path, ontoCommit, steps }),
      ),
    mergeBranch: async (branchName: string, mode: MergeMode = "default") =>
      runMutating(mode === "squash" ? "merge-squash" : "merge", branchName, (path) =>
        invokeFn("cmd_merge_branch", { repoPath: path, branchName, mode }),
      ),
    fetch: async (remote?: string, target?: MutationTarget | null) =>
      runMutating(
        "fetch",
        remote ?? "origin",
        (path) => invokeFn("cmd_fetch", { repoPath: path, remote }),
        { target },
      ),
    pull: async (
      remote?: string,
      branch?: string,
      rebase?: boolean,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        rebase ? "pull-rebase" : "pull",
        [remote, branch].filter(Boolean).join(" ") || "upstream",
        (path) => invokeFn("cmd_pull", { repoPath: path, remote, branch, rebase }),
        { target },
      ),
    pullRebaseConfig: async (): Promise<string | null> => {
      const session = activeSession();
      if (!session) return null;
      const value = await invokeFn<unknown>("cmd_pull_rebase_config", {
        repoPath: session.path,
      });
      return typeof value === "string" ? value : null;
    },
    cancelHooks: async (): Promise<number> => {
      const session = activeSession();
      if (!session) return 0;
      const stopped = await invokeFn<unknown>("cmd_cancel_git_hooks", {
        repoPath: session.path,
      });
      return typeof stopped === "number" ? stopped : 0;
    },
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
    gitPreflight: async (): Promise<GitPreflight> =>
      parseGitPreflight(await invokeFn<unknown>("cmd_git_preflight", {})),
    autoFetchSetting: (path: string): AutoFetchSetting | null => autoFetchPrefs[path] ?? null,
    setAutoFetch: (path: string, setting: AutoFetchSetting) => {
      autoFetchPrefs = withAutoFetchSetting(autoFetchPrefs, path, setting);
      try {
        saveAutoFetchPrefs(autoFetchPrefs);
      } catch (error) {
        diagnostics.warn(
          "autoFetch",
          `couldn't save auto-fetch settings: ${formatErrorRaw(error)}`,
        );
      }
    },
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
          diagnostics.warn(
            "autoFetch",
            `auto-fetch of ${path} failed: ${formatErrorRaw(error)}`,
          ),
      });
      return createAdaptiveTimer(() => void scheduler.tick(), AUTO_FETCH_TICK_MS);
    },
    push: async (
      remote?: string,
      branch?: string,
      force: boolean = false,
      target?: MutationTarget | null,
    ): Promise<MutationOutcome> => {
      const session = target === undefined ? activeSession() : undefined;
      const refused: MutationTarget | null =
        target !== undefined ? target : session ? { tabId: session.id, path: session.path } : null;
      const run = (allowSecrets: boolean, at: MutationTarget | null | undefined) =>
        runMutating(
          force ? "push-force" : "push",
          [remote, branch].filter(Boolean).join(" ") || "upstream",
          (path) =>
            invokeFn("cmd_push", {
              repoPath: path,
              remote,
              branch,
              force,
              ...(allowSecrets ? { allowSecrets } : {}),
            }),
          { target: at, expected: allowSecrets ? undefined : secretScanRefused },
        );
      return offerPushPastSecretScan(await run(false, target), () => run(true, refused));
    },
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
        harnessStore.recordAction({
          repoPath: path,
          kind: "issue-report",
          label,
          ok: true,
          verdict: policy ?? null,
        });
        const output = mutationOutput<string>(result);
        return output === undefined ? { ok: true, policy } : { ok: true, policy, output };
      } catch (err: unknown) {
        harnessStore.recordAction({ repoPath: path, kind: "issue-report", label, ok: false });
        return { ok: false, error: describeError(err, "repo") };
      }
    },
    // `allowSecrets` only from the release pane's "Publish anyway" button,
    // under a `secret_scan_blocked` refusal of this tag's push.
    publishRelease: async (tag: string, message: string, allowSecrets: boolean = false) =>
      runMutating<ReleasePublishResult>("release-publish", tag, (path) =>
        invokeFn("cmd_publish_release", {
          repoPath: path,
          tag,
          message,
          ...(allowSecrets ? { allowSecrets } : {}),
        }),
      ),
    stashSave: async (
      message?: string,
      stashOptions?: StashSaveOptions,
      target?: MutationTarget | null,
    ) =>
      runMutating(
        "stash",
        message ?? "",
        (path) =>
          invokeFn("cmd_stash_save", { repoPath: path, message, options: stashOptions }),
        { target },
      ),
    stashPop: async (target?: MutationTarget | null) => {
      const session =
        target === undefined ? activeSession() : (target && resolveTarget(target)) || undefined;
      if (!session) {
        return target === undefined
          ? { ok: false as const, error: "No repository is open." }
          : {
              ok: false as const,
              error: REPOSITORY_CHANGED,
              refused: true,
              surfaced: false,
            };
      }
      if (session.stashFailed) {
        return {
          ok: false as const,
          error:
            "The stash list couldn't be read, so popping it would target the wrong entry.",
        };
      }
      const top = session.stashEntries[0];
      if (!top) return { ok: false as const, error: "Nothing is stashed." };
      return runMutating(
        "unstash",
        top.selector,
        (path) =>
          invokeFn("cmd_stash_action", {
            repoPath: path,
            action: "pop",
            index: top.index,
            expectedOid: top.oid,
          }),
        { target: { tabId: session.id, path: session.path } },
      );
    },
  };

  return store;
}

export const repoStore = createRepoStore();
/** `repoStore.currentPath`, importable as a store of its own. */
export const currentRepoPath = repoStore.currentPath;
