import { get } from "svelte/store";
import type { createCommitDraftStore } from "../stores/commitDraftStore";
import type { InvokeFn } from "../stores/graphStore";
import { harnessStore } from "../stores/harnessStore";
import { interfaceStore } from "../stores/interfaceStore";
import { askConfirm as defaultAskConfirm } from "../stores/modalStore";
import { describeError } from "../ui/copy/describeFailure";
import { sanitizeHistoryList, type HistoryList } from "../views/historyList";
import { collapsedInactiveGroups, explicitlyExpandedGroups } from "./groupState";
import {
  repositoryTrustRefused,
  type createHydrationController,
} from "./hydration";
import { identityKey, sameRepo, type PathIdentityOptions } from "./paths";
import {
  emptyWorkspace,
  loadPersistedWorkspace,
  type StorageLike,
  type ViewTab,
} from "./persist";
import {
  createSession,
  project,
  type createProjectionCache,
} from "./projection";
import { familyFromCommonDir } from "./repoFamily";
import { requestRepositoryTrust as defaultRequestTrust } from "./repositoryTrust";
import type { createSessionBook } from "./sessionBook";
import { expandedStacks, lastUsedCheckouts } from "./stackState";
import type { createStatusPoller } from "./statusPoller";
import type {
  InternalState,
  MutationTarget,
  RepoSession,
  RepoStoreDeps,
  ResolvedRepo,
} from "./storeTypes";
import {
  activationStops,
  canMoveUnit,
  cycleStop,
  jumpTargetIndex,
  moveUnit,
  moveUnitToEdge,
  unitForTab,
} from "./stripNav";
import { type TabColor } from "./tabColors";
import {
  computeTabLayout,
  normalizeGroupName,
  type GroupingOptions,
  type StackingOptions,
} from "./tabGroups";
import {
  activateTab as activateWorkspaceTab,
  arrangeTabs as arrangeWorkspaceTabs,
  closeTab as closeWorkspaceTab,
  collapseAllGroups as collapseWorkspaceAllGroups,
  collapseInactiveGroups as collapseWorkspaceInactiveGroups,
  expandAllGroups as expandWorkspaceAllGroups,
  groupByParentFolder as groupWorkspaceByParentFolder,
  isGroupCollapsed as isWorkspaceGroupCollapsed,
  MAX_OPEN_TABS,
  moveTabTo as moveWorkspaceTabTo,
  openTab as openWorkspaceTab,
  pinTab as pinWorkspaceTab,
  removeRecent as removeWorkspaceRecent,
  renameGroup as renameWorkspaceGroup,
  restoreGroupColors,
  setGroupCollapsed as setWorkspaceGroupCollapsed,
  setGroupColor as setWorkspaceGroupColor,
  setTabColor as setWorkspaceTabColor,
  setTabGroup as setWorkspaceTabGroup,
  toggleGroupCollapsed as toggleWorkspaceGroupCollapsed,
  ungroupTabs as ungroupWorkspaceTabs,
  type TabRecord,
  type WorkspaceTabs,
} from "./tabModel";
import { confirmTerminalLoss } from "./terminalLoss";
import type { createWatchCoordinator } from "./watchCoordinator";
import { WATCH_PARKED } from "./watchState";

/** What a person reads when a tab cannot be opened at all. */
export function openRefusalMessage(reason: "invalid" | "capacity"): string {
  return reason === "capacity"
    ? `Too many open repositories (max ${MAX_OPEN_TABS}). Close a tab to open another.`
    : "Invalid repository path";
}

export type TabSessionPatch =
  | Partial<RepoSession>
  | ((current: RepoSession) => Partial<RepoSession>);

export interface TabActionsDeps {
  invoke: InvokeFn;
  storage: StorageLike | null;
  options: PathIdentityOptions;
  graph: NonNullable<RepoStoreDeps["graph"]>;
  terminals: NonNullable<RepoStoreDeps["terminals"]>;
  projection: ReturnType<typeof createProjectionCache>;
  sessionBook: ReturnType<typeof createSessionBook>;
  commitDrafts: ReturnType<typeof createCommitDraftStore>;
  watchCoordinator: Pick<
    ReturnType<typeof createWatchCoordinator>,
    | "noteTabActivated"
    | "admitWatch"
    | "hasWatch"
    | "acquireWatch"
    | "unwatch"
    | "refreshOwedFor"
    | "onActivated"
  >;
  hydration: Pick<
    ReturnType<typeof createHydrationController>,
    "resolvePath" | "resolvedFields" | "snapshotIsFresh" | "hydrate"
  > & Partial<Pick<ReturnType<typeof createHydrationController>, "cancelStatus">>;
  statusPoller: Pick<
    ReturnType<typeof createStatusPoller>,
    "ensure" | "stop" | "rearm" | "resetDuration"
  >;
  getInternal(): InternalState;
  setWorkspaceError(error: string | null): void;
  setSessions(sessions: Record<string, RepoSession>): void;
  replaceWorkspace(next: WorkspaceTabs): void;
  putSession(session: RepoSession): void;
  activeSession(): RepoSession | undefined;
  applyToSession(id: string, generation: number, patchOrFn: TabSessionPatch): boolean;
  bumped(session: RepoSession): RepoSession;
  resolveTarget(target: MutationTarget): RepoSession | null;
  syncFilterFromSession(session: RepoSession | undefined): void;
  revealGraph(session: RepoSession | undefined): void;
  publish(): void;
  holdPublish(): () => void;
  commitEdit(closed?: readonly string[]): void;
  flushPersist(force?: boolean): void;
  suspendPersist(): () => void;
  resetLoadedWorkspace(epoch: number): void;
  getOpenEpoch(): number;
  bumpOpenEpoch(): number;
  setError(error: string | null): void;
  askConfirm?: typeof defaultAskConfirm;
  requestRepositoryTrust?: typeof defaultRequestTrust;
  forgetRepository?: (path: string) => void;
}

/**
 * Owns repository tab lifecycle (open, activate, close/prune, trust, strip
 * navigation, grouping, recents, workspace restore, and mutation target lookup).
 */
export function createTabActions(deps: TabActionsDeps) {
  const {
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
    statusPoller,
  } = deps;
  const { familyUnknown } = sessionBook;
  const askConfirmFn = deps.askConfirm ?? defaultAskConfirm;
  const requestTrustFn = deps.requestRepositoryTrust ?? defaultRequestTrust;
  const forgetRepositoryFn =
    deps.forgetRepository ?? ((path: string) => harnessStore.forgetRepository(path));

  let shortcutLocked = false;

  function familyOfTab(id: string) {
    return familyFromCommonDir(deps.getInternal().sessions[id]?.commonDir, options);
  }

  /**
   * Seats a newly opened checkout with the open checkouts of its repository:
   * in their group, just after the last of them.
   */
  function placeBesideFamily(
    id: string,
    commonDir: string | null | undefined,
    explicitGroup: boolean,
    activate: boolean,
  ): void {
    const family = familyFromCommonDir(commonDir, options);
    if (!family) return;
    let ws = deps.getInternal().workspace;
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
        if (activate && anchorGroup) {
          ws = setWorkspaceGroupCollapsed(ws, anchorGroup, false);
        }
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
    if (ws !== deps.getInternal().workspace) deps.replaceWorkspace(ws);
  }

  function beginShortcut(): boolean {
    if (shortcutLocked) return false;
    shortcutLocked = true;
    queueMicrotask(() => {
      shortcutLocked = false;
    });
    return true;
  }

  function stripStacking(): StackingOptions {
    return {
      enabled: get(interfaceStore).stackWorktreeTabs,
      expanded: get(expandedStacks),
      lastUsed: lastUsedCheckouts(),
      identity: (path: string) => identityKey(path, options),
    };
  }

  function stripGrouping(): GroupingOptions {
    const iface = get(interfaceStore);
    return {
      collapseInactive: iface.collapseInactiveGroupTabs,
      collapsedInactiveGroups: get(collapsedInactiveGroups),
      expandedGroups: get(explicitlyExpandedGroups),
      keepActiveShells: !iface.collapseInactiveIncludeShells,
    };
  }

  function drawnLayout() {
    const projected = project(deps.getInternal(), options, projection);
    return computeTabLayout(
      projected.openTabs,
      projected.collapsedGroups,
      undefined,
      projected.groupColors,
      stripStacking(),
      stripGrouping(),
    );
  }

  async function releaseSession(id: string, path: string | null): Promise<void> {
    hydration.cancelStatus?.(id);
    sessionBook.forget(id, path);
    if (path === null) return;
    const stillOpen = Object.values(deps.getInternal().sessions).some((other) =>
      sameRepo(other.path, path, options),
    );
    if (!stillOpen) forgetRepositoryFn(path);
    graph.evict(path);
    await watchCoordinator.unwatch(path);
  }

  /**
   * Shared commit/prune/release/activate tail for all four tab-close routes:
   * `closeTab`, `closeOtherTabs`, `closeTabsToTheRight`, and `closeGroup`.
   */
  async function retireClosedTabs(opts: {
    nextWorkspace: WorkspaceTabs;
    closedTabs: readonly { id: string; path: string | null }[];
    closedPaths: readonly string[];
  }): Promise<void> {
    deps.commitEdit(opts.closedPaths);
    deps.replaceWorkspace(opts.nextWorkspace);
    const remaining = new Set(opts.nextWorkspace.tabs.map((tab) => tab.id));
    const sessions: Record<string, RepoSession> = {};
    for (const [key, session] of Object.entries(deps.getInternal().sessions)) {
      if (remaining.has(key)) sessions[key] = session;
    }
    deps.setSessions(sessions);
    statusPoller.stop();
    for (const tab of opts.closedTabs) {
      await releaseSession(tab.id, tab.path);
    }
    const revealed = deps.activeSession();
    if (revealed) {
      deps.putSession(deps.bumped(revealed));
      statusPoller.ensure();
    }
    deps.syncFilterFromSession(deps.activeSession());
    deps.publish();
    deps.revealGraph(deps.activeSession());
    deps.flushPersist();
  }

  async function grantTrust(id: string): Promise<boolean> {
    const current = deps.getInternal().sessions[id];
    if (!current?.trustRequired) return false;
    const trustedPath = await requestTrustFn(current.path, "Trust and Open", invokeFn);
    const live = deps.getInternal().sessions[id];
    if (!live?.trustRequired) return false;
    if (!trustedPath) return false;
    let granted: ReturnType<typeof hydration.resolvedFields>;
    try {
      granted = hydration.resolvedFields(id, await hydration.resolvePath(trustedPath));
    } catch (err: unknown) {
      const message = describeError(err, "repo");
      deps.applyToSession(id, live.generation, {
        isLoading: false,
        error: message,
        trustRequired: repositoryTrustRefused(err) || live.trustRequired,
      });
      return false;
    }
    const latest = deps.getInternal().sessions[id];
    if (!latest) return false;
    const path = granted.path;
    const activation = deps.bumped({ ...latest, ...granted });
    const onScreen = deps.getInternal().workspace.activeId === id;
    deps.putSession({ ...activation, isLoading: true });
    if (onScreen) {
      deps.syncFilterFromSession(activation);
      deps.revealGraph(activation);
    }
    deps.publish();
    await watchCoordinator.acquireWatch(id, path, onScreen);
    if (onScreen) {
      watchCoordinator.onActivated(path);
      await watchCoordinator.noteTabActivated(path);
    }
    await hydration.hydrate(id, path, activation.generation);
    const after = deps.getInternal().sessions[id];
    if (after && after.generation === activation.generation && !after.trustRequired) {
      statusPoller.ensure();
      deps.flushPersist();
    }
    return true;
  }

  async function openRepo(
    rawPath: string,
    extras: {
      allowBroken?: boolean;
      onReady?: (canonicalPath: string) => void;
      activate?: boolean;
      pinned?: boolean;
      group?: string | null;
      color?: TabColor | null;
      keepEpoch?: boolean;
      skipHydrate?: boolean;
      deferTrust?: boolean;
      restore?: {
        viewTab?: ViewTab;
        viewSections?: Record<string, string>;
        historyList?: HistoryList;
        searchQuery?: string;
        selectedBranch?: string | null;
        terminalOpen?: boolean;
      };
    } = {},
  ): Promise<boolean> {
    const requestId =
      extras.activate === false ? deps.getOpenEpoch() : deps.bumpOpenEpoch();
    deps.setWorkspaceError(null);
    let resolved: ResolvedRepo | null = null;
    let deferredTrustMessage: string | null = null;
    try {
      resolved = await hydration.resolvePath(rawPath);
    } catch (err: unknown) {
      const message = describeError(err, "repo");
      const refused = repositoryTrustRefused(err);
      if (refused && !extras.deferTrust) {
        try {
          const trustedPath = await requestTrustFn(rawPath, "Trust and Open", invokeFn);
          if (!trustedPath) return false;
          resolved = await hydration.resolvePath(trustedPath);
        } catch (trustError: unknown) {
          deps.setWorkspaceError(describeError(trustError, "repo"));
          deps.publish();
          return false;
        }
      } else if (refused) {
        deferredTrustMessage = message;
      } else if (!extras.allowBroken) {
        deps.setWorkspaceError(describeError(err, "repo"));
        deps.publish();
        return false;
      }
    }
    const path = resolved?.path ?? rawPath;
    const activate = extras.activate === false ? false : requestId === deps.getOpenEpoch();
    let workspace = deps.getInternal().workspace;
    let carriedSession: RepoSession | undefined;
    let carriedId: string | undefined;
    const rawKey = identityKey(rawPath, options);
    const canonicalKey = identityKey(path, options);
    if (resolved && rawKey && canonicalKey && rawKey !== canonicalKey) {
      const aliases = workspace.tabs.filter(
        (tab) => tab.id !== canonicalKey && identityKey(tab.path, options) === rawKey,
      );
      if (aliases.length > 0) {
        const aliasIds = new Set(aliases.map((tab) => tab.id));
        const aliasPaths = new Map(aliases.map((tab) => [tab.id, tab.path]));
        for (const alias of aliases) {
          if (deps.getInternal().sessions[alias.id]) {
            carriedSession = deps.getInternal().sessions[alias.id];
            carriedId = alias.id;
          }
          graph.evict(alias.path);
        }
        workspace = {
          ...workspace,
          tabs: workspace.tabs.filter((tab) => !aliasIds.has(tab.id)),
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
        const sessions = { ...deps.getInternal().sessions };
        for (const id of aliasIds) {
          delete sessions[id];
          sessionBook.forget(id, aliasPaths.get(id) ?? null);
        }
        deps.setSessions(sessions);
      }
    }
    const hadCanonical = workspace.tabs.some((tab) => tab.id === canonicalKey);
    const carriedPinned = hadCanonical ? undefined : carriedSession?.pinned;
    const opened = openWorkspaceTab(workspace, path, options, {
      pinned: extras.pinned ?? carriedPinned,
      activate,
      ...(extras.group !== undefined ? { group: extras.group } : {}),
      ...(extras.color !== undefined ? { color: extras.color } : {}),
    });
    if (!opened.ok) {
      deps.setWorkspaceError(openRefusalMessage(opened.reason));
      deps.publish();
      return false;
    }
    if (!extras.keepEpoch && opened.created) deps.commitEdit();
    deps.replaceWorkspace({
      ...opened.workspace,
      lastClosed: opened.workspace.lastClosed.filter(
        (item) => !sameRepo(item, path, options),
      ),
    });
    if (opened.created && !extras.keepEpoch) {
      placeBesideFamily(
        opened.id,
        resolved?.common_dir,
        extras.group !== undefined,
        activate,
      );
    }
    const existing = deps.getInternal().sessions[opened.id];
    const fromResolve = resolved ? hydration.resolvedFields(opened.id, resolved) : null;
    if (!resolved && !existing?.commonDir) familyUnknown.add(opened.id);
    const fallbackError =
      deferredTrustMessage ??
      deps.getInternal().workspaceError ??
      "Repository is unavailable";
    const session = existing
      ? {
          ...deps.bumped(existing),
          path,
          name: fromResolve?.name ?? existing.name,
          isBare: fromResolve?.isBare ?? existing.isBare,
          commonDir: fromResolve ? fromResolve.commonDir : existing.commonDir,
          pinned:
            opened.workspace.tabs.find((tab) => tab.id === opened.id)?.pinned ??
            existing.pinned,
          isLoading: resolved ? !existing.hasHydrated : false,
          error: resolved ? null : fallbackError,
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
            activeTab: extras.restore?.viewTab ?? carriedSession?.activeTab,
            viewSections: extras.restore?.viewSections ?? carriedSession?.viewSections,
            historyList: extras.restore?.historyList ?? carriedSession?.historyList,
            searchQuery: extras.restore?.searchQuery ?? carriedSession?.searchQuery,
            selectedBranch:
              extras.restore?.selectedBranch ?? carriedSession?.selectedBranch,
            terminalOpen: extras.restore?.terminalOpen ?? carriedSession?.terminalOpen,
            isAmending: carriedSession?.isAmending ?? false,
            isLoading: Boolean(resolved),
            error: resolved
              ? null
              : deferredTrustMessage ?? "Repository is unavailable",
            trustRequired: deferredTrustMessage !== null,
          },
        );
    if (!existing && carriedId !== undefined) commitDrafts.move(carriedId, opened.id);
    deps.putSession(session);
    const shouldPresent = activate && deps.getInternal().workspace.activeId === opened.id;
    if (shouldPresent) {
      deps.syncFilterFromSession(session);
      graph.showRepo(session.path);
    }
    deps.publish();
    if (!resolved) return true;
    if (extras.skipHydrate && !shouldPresent) return true;
    await watchCoordinator.acquireWatch(opened.id, path, shouldPresent);
    if (shouldPresent && !extras.keepEpoch) {
      await watchCoordinator.noteTabActivated(path);
    }
    await hydration.hydrate(opened.id, path, session.generation);
    const latest = deps.getInternal().sessions[opened.id];
    if (
      shouldPresent &&
      latest &&
      deps.getInternal().workspace.activeId === opened.id
    ) {
      deps.revealGraph(latest);
    }
    if (
      shouldPresent &&
      requestId === deps.getOpenEpoch() &&
      latest &&
      latest.generation === session.generation &&
      !latest.error &&
      latest.activeTab === session.activeTab &&
      latest.viewSections === session.viewSections &&
      latest.selectedFilePath === session.selectedFilePath &&
      latest.selectedCommitId === session.selectedCommitId &&
      latest.selectedDiffPending === session.selectedDiffPending &&
      deps.getInternal().workspace.activeId === opened.id
    ) {
      extras.onReady?.(path);
    }
    statusPoller.ensure();
    deps.flushPersist();
    return true;
  }

  async function activateTab(id: string, extras: { force?: boolean } = {}): Promise<void> {
    deps.bumpOpenEpoch();
    const current = deps.getInternal().sessions[id];
    if (!extras.force && current?.trustRequired) {
      if (deps.getInternal().workspace.activeId !== id) {
        const next = activateWorkspaceTab(deps.getInternal().workspace, id);
        if (next === deps.getInternal().workspace) return;
        deps.replaceWorkspace(next);
      }
      deps.syncFilterFromSession(current);
      deps.publish();
      await grantTrust(id);
      return;
    }
    if (!extras.force && deps.getInternal().workspace.activeId === id) return;
    if (deps.getInternal().workspace.activeId !== id) {
      const next = activateWorkspaceTab(deps.getInternal().workspace, id);
      if (next === deps.getInternal().workspace) return;
      deps.replaceWorkspace(next);
    }
    const session = deps.getInternal().sessions[id];
    if (!session) {
      deps.publish();
      return;
    }
    if (session.trustRequired) {
      deps.syncFilterFromSession(session);
      deps.publish();
      return;
    }
    deps.syncFilterFromSession(session);
    const activation = deps.bumped(session);
    deps.putSession({ ...activation, isLoading: !session.hasHydrated, error: null });
    deps.publish();
    deps.revealGraph(activation);
    const refreshOwed = watchCoordinator.refreshOwedFor(session.path);
    watchCoordinator.onActivated(session.path);
    statusPoller.resetDuration();
    statusPoller.rearm();
    if (!extras.force) await watchCoordinator.noteTabActivated(session.path);
    await watchCoordinator.acquireWatch(id, session.path, true);
    if (
      extras.force ||
      refreshOwed ||
      !hydration.snapshotIsFresh(id, activation.generation)
    ) {
      await hydration.hydrate(id, session.path, activation.generation);
    }
    statusPoller.ensure();
    deps.flushPersist();
  }

  async function closeTab(id: string): Promise<void> {
    const session = deps.getInternal().sessions[id];
    if (session && !(await confirmTerminalLoss(terminals, [session.path]))) return;
    const result = closeWorkspaceTab(deps.getInternal().workspace, id);
    if (result.reason === "missing") return;
    await retireClosedTabs({
      nextWorkspace: result.workspace,
      closedTabs: [{ id, path: session?.path ?? null }],
      closedPaths: result.closedPath ? [result.closedPath] : [],
    });
  }

  /**
   * The one path behind "Close Other Tabs", "Close Tabs to the Right" and
   * "Close Group". `pick` names the tabs from the workspace as it is when the
   * person is asked, and the confirmation covers exactly those. The dialog can
   * stay up while the workspace changes (an agent opens a worktree, another
   * route closes a tab), so after it resolves only the tabs that were listed
   * and are still open are closed, each released through `retireClosedTabs`.
   * A tab opened meanwhile was never listed, so it stays open: closing it
   * unasked would lose a terminal nobody agreed to end, and asking again would
   * make the person confirm a list they already answered.
   *
   * `anchorId` is the tab the action is relative to. It is where the active
   * tab goes when the active one closes, and the action is void if it was
   * closed behind the dialog.
   */
  async function closeShownTabs(
    pick: (workspace: WorkspaceTabs) => readonly TabRecord[],
    anchorId: string | null,
  ): Promise<void> {
    const listed = deps.getInternal().workspace;
    if (anchorId !== null && !listed.tabs.some((tab) => tab.id === anchorId)) return;
    const shown = pick(listed);
    if (shown.length === 0) return;
    if (!(await confirmTerminalLoss(terminals, shown.map((tab) => tab.path)))) return;
    const before = deps.getInternal().workspace;
    if (anchorId !== null && !before.tabs.some((tab) => tab.id === anchorId)) return;
    const shownIds = new Set(shown.map((tab) => tab.id));
    let workspace = before;
    const closed: { id: string; path: string }[] = [];
    for (const tab of before.tabs) {
      if (!shownIds.has(tab.id)) continue;
      const result = closeWorkspaceTab(workspace, tab.id);
      if (result.reason === "missing") continue;
      workspace = result.workspace;
      closed.push({ id: tab.id, path: tab.path });
    }
    if (closed.length === 0) return;
    if (anchorId !== null && closed.some((tab) => tab.id === before.activeId)) {
      workspace = activateWorkspaceTab(workspace, anchorId);
    }
    await retireClosedTabs({
      nextWorkspace: workspace,
      closedTabs: closed,
      closedPaths: closed.map((tab) => tab.path),
    });
  }

  function arrangeTabs(
    orderedIds: readonly string[],
    regroup?: { ids: readonly string[]; group: string | null },
  ): boolean {
    let next = deps.getInternal().workspace;
    if (regroup) {
      for (const id of regroup.ids) next = setWorkspaceTabGroup(next, id, regroup.group);
      if (regroup.group) next = setWorkspaceGroupCollapsed(next, regroup.group, false);
    }
    const arranged = arrangeWorkspaceTabs(next, orderedIds);
    const alreadyInOrder =
      orderedIds.length === next.tabs.length &&
      next.tabs.every((tab, i) => tab.id === orderedIds[i]);
    if (arranged === next && !alreadyInOrder) return false;
    if (arranged === deps.getInternal().workspace) return false;
    deps.commitEdit();
    deps.replaceWorkspace(arranged);
    deps.publish();
    deps.flushPersist();
    return true;
  }

  return {
    trustTab: (id: string) => grantTrust(id),
    openRefusal: (rawPath: string): string | null => {
      const probe = openWorkspaceTab(deps.getInternal().workspace, rawPath, options, {
        activate: false,
      });
      return probe.ok ? null : openRefusalMessage(probe.reason);
    },
    familyOf: async (rawPath: string): Promise<string | null> => {
      const key = identityKey(rawPath, options);
      if (!key) return null;
      const open = deps.getInternal().workspace.tabs.find((tab) => tab.id === key);
      if (open) {
        const known = familyFromCommonDir(
          deps.getInternal().sessions[open.id]?.commonDir,
          options,
        );
        if (known) return known.key;
      }
      try {
        const resolved = await hydration.resolvePath(rawPath);
        return familyFromCommonDir(resolved?.common_dir, options)?.key ?? null;
      } catch {
        return null;
      }
    },
    openRepo,
    pickAndOpenRepo: async () => {
      try {
        const folder = await invokeFn<string | null>("cmd_pick_folder");
        if (folder) {
          await openRepo(folder);
        }
      } catch (err: unknown) {
        deps.setWorkspaceError(describeError(err, "repo"));
        deps.publish();
      }
    },
    revokeTrust: async (id: string) => {
      const session = deps.getInternal().sessions[id];
      if (!session) return;
      const approved = await askConfirmFn({
        title: "Revoke repository trust?",
        message: `${session.path}\n\nThis closes the tab and blocks new GitPulse operations. Trust was granted to the repository, so this revokes it for every worktree of it, not only this checkout: any other tab open on the same repository stays open but stops working until it is trusted again. Already-running terminals and agent tasks retain their permissions; stop them separately if needed.`,
        confirmLabel: "Revoke Trust",
        destructive: true,
      });
      if (!approved) return;
      try {
        await invokeFn("cmd_revoke_repository_trust", { repoPath: session.path });
        await closeTab(id);
      } catch (error: unknown) {
        deps.setError(describeError(error, "repo"));
      }
    },
    activateTab,
    closeTab,
    closeActiveTab: async () => {
      if (!beginShortcut()) return;
      const id = deps.getInternal().workspace.activeId;
      if (id) await closeTab(id);
    },
    closeOtherTabs: (id: string) =>
      closeShownTabs(
        (workspace) => workspace.tabs.filter((tab) => tab.id !== id),
        id,
      ),
    closeTabsToTheRight: (id: string) =>
      closeShownTabs((workspace) => {
        const index = workspace.tabs.findIndex((tab) => tab.id === id);
        return index < 0 ? [] : workspace.tabs.slice(index + 1);
      }, id),
    nextTab: async () => {
      if (!beginShortcut()) return;
      const activeId = deps.getInternal().workspace.activeId;
      const next = cycleStop(drawnLayout(), activeId, 1);
      if (next && next !== activeId) {
        await activateTab(next);
      }
    },
    prevTab: async () => {
      if (!beginShortcut()) return;
      const activeId = deps.getInternal().workspace.activeId;
      const next = cycleStop(drawnLayout(), activeId, -1);
      if (next && next !== activeId) {
        await activateTab(next);
      }
    },
    arrangeTabs,
    activateTabAt: async (index: number) => {
      if (!Number.isInteger(index) || index < 0) return;
      const id = activationStops(drawnLayout())[index];
      if (id) await activateTab(id);
    },
    activateTabByDigit: async (digit: number) => {
      const stops = activationStops(drawnLayout());
      const index = jumpTargetIndex(digit, stops.length);
      if (index !== null) await activateTab(stops[index]);
    },
    jumpStopCount: (): number => activationStops(drawnLayout()).length,
    moveTabBy: (id: string, delta: number): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnit(layout, unit, delta) : null;
      return order ? arrangeTabs(order) : false;
    },
    moveTabToEdge: (id: string, edge: "start" | "end"): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      const order = unit ? moveUnitToEdge(layout, unit, edge) : null;
      return order ? arrangeTabs(order) : false;
    },
    canMoveTab: (id: string, delta: -1 | 1): boolean => {
      const layout = drawnLayout();
      const unit = unitForTab(layout, id);
      return unit !== null && canMoveUnit(layout, unit, delta);
    },
    pinTab: (id: string, pinned: boolean) => {
      deps.commitEdit();
      deps.replaceWorkspace(pinWorkspaceTab(deps.getInternal().workspace, id, pinned));
      const session = deps.getInternal().sessions[id];
      if (session) deps.putSession({ ...session, pinned });
      deps.publish();
    },
    setTabGroup: (id: string, group: string | null) => {
      deps.commitEdit();
      deps.replaceWorkspace(setWorkspaceTabGroup(deps.getInternal().workspace, id, group));
      deps.publish();
      deps.flushPersist();
    },
    setTabColor: (id: string, color: TabColor | null) => {
      const next = setWorkspaceTabColor(deps.getInternal().workspace, id, color);
      if (next === deps.getInternal().workspace) return;
      deps.commitEdit();
      deps.replaceWorkspace(next);
      deps.publish();
      deps.flushPersist();
    },
    setGroupColor: (groupName: string, color: TabColor | null) => {
      const next = setWorkspaceGroupColor(
        deps.getInternal().workspace,
        groupName,
        color,
      );
      if (next === deps.getInternal().workspace) return;
      deps.commitEdit();
      deps.replaceWorkspace(next);
      deps.publish();
      deps.flushPersist();
    },
    groupByParentFolder: () => {
      deps.commitEdit();
      deps.replaceWorkspace(
        groupWorkspaceByParentFolder(
          deps.getInternal().workspace,
          (tab) => familyOfTab(tab.id)?.root,
        ),
      );
      deps.publish();
      deps.flushPersist();
    },
    ungroupTabs: (groupName?: string) => {
      deps.commitEdit();
      deps.replaceWorkspace(ungroupWorkspaceTabs(deps.getInternal().workspace, groupName));
      deps.publish();
      deps.flushPersist();
    },
    closeGroup: (groupName: string) => {
      const group = normalizeGroupName(groupName);
      if (!group) return Promise.resolve();
      return closeShownTabs(
        (workspace) => workspace.tabs.filter((tab) => tab.group === group),
        null,
      );
    },
    renameGroup: (oldName: string, newName: string) => {
      deps.commitEdit();
      deps.replaceWorkspace(
        renameWorkspaceGroup(deps.getInternal().workspace, oldName, newName),
      );
      deps.publish();
      deps.flushPersist();
    },
    toggleGroupCollapsed: (groupName: string) => {
      deps.commitEdit();
      deps.replaceWorkspace(
        toggleWorkspaceGroupCollapsed(deps.getInternal().workspace, groupName),
      );
      deps.publish();
      deps.flushPersist();
    },
    setGroupCollapsed: (groupName: string, collapsed: boolean) => {
      deps.commitEdit();
      deps.replaceWorkspace(
        setWorkspaceGroupCollapsed(deps.getInternal().workspace, groupName, collapsed),
      );
      deps.publish();
      deps.flushPersist();
    },
    isGroupCollapsed: (groupName: string) => {
      return isWorkspaceGroupCollapsed(deps.getInternal().workspace, groupName);
    },
    collapseAllGroups: () => {
      deps.commitEdit();
      deps.replaceWorkspace(collapseWorkspaceAllGroups(deps.getInternal().workspace));
      deps.publish();
      deps.flushPersist();
    },
    collapseInactiveGroups: () => {
      deps.commitEdit();
      deps.replaceWorkspace(
        collapseWorkspaceInactiveGroups(deps.getInternal().workspace),
      );
      deps.publish();
      deps.flushPersist();
    },
    expandAllGroups: () => {
      deps.commitEdit();
      deps.replaceWorkspace(expandWorkspaceAllGroups(deps.getInternal().workspace));
      deps.publish();
      deps.flushPersist();
    },
    reopenLastClosed: async () => {
      const path = deps.getInternal().workspace.lastClosed[0];
      if (!path) return;
      await openRepo(path, { activate: true });
    },
    clearRecents: () => {
      deps.replaceWorkspace({ ...deps.getInternal().workspace, recents: [] });
      deps.publish();
      deps.flushPersist();
    },
    removeRecent: (path: string) => {
      deps.replaceWorkspace(
        removeWorkspaceRecent(deps.getInternal().workspace, path, options),
      );
      deps.publish();
    },
    removeRepo: async (path: string) => {
      const tab = deps.getInternal().workspace.tabs.find((item) =>
        sameRepo(item.path, path, options),
      );
      if (tab) {
        await closeTab(tab.id);
      }
      deps.replaceWorkspace({
        ...deps.getInternal().workspace,
        recents: deps.getInternal().workspace.recents.filter(
          (item) => !sameRepo(item, path, options),
        ),
        lastClosed: deps.getInternal().workspace.lastClosed.filter(
          (item) => !sameRepo(item, path, options),
        ),
      });
      deps.publish();
      deps.flushPersist();
    },
    restoreWorkspace: async () => {
      statusPoller.stop();
      const resumePersist = deps.suspendPersist();
      try {
        const persisted = loadPersistedWorkspace(storage, options);
        deps.resetLoadedWorkspace(persisted.epoch ?? 0);
        deps.replaceWorkspace({
          ...emptyWorkspace(),
          recents: persisted.recents,
          lastClosed: persisted.lastClosed,
        });
        deps.setSessions({});
        familyUnknown.clear();
        const ordered = [...persisted.tabs];
        let activated = false;
        const isActive = (path: string) =>
          !!persisted.activePath && sameRepo(path, persisted.activePath, options);
        const presentRestored = async (id: string) => {
          const session = deps.getInternal().sessions[id];
          if (session?.error && !session.hasHydrated) {
            deps.replaceWorkspace(activateWorkspaceTab(deps.getInternal().workspace, id));
            deps.syncFilterFromSession(session);
            deps.publish();
            return;
          }
          await activateTab(id, { force: true });
        };

        const releasePublish = deps.holdPublish();
        try {
          for (const tab of ordered) {
            await openRepo(tab.path, {
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
                historyList: sanitizeHistoryList(tab.historyList),
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
          ? deps.getInternal().workspace.tabs.find((item) =>
              sameRepo(item.path, activeTab.path, options),
            )
          : undefined;
        if (activeSessionTab) {
          activated = true;
          await presentRestored(activeSessionTab.id);
        }
        const activeGroup = activated
          ? deps.getInternal().workspace.tabs.find(
              (tab) => tab.id === deps.getInternal().workspace.activeId,
            )?.group
          : null;
        const collapsedGroups = (persisted.collapsedGroups ?? []).filter(
          (group) => !activeGroup || group !== activeGroup,
        );
        deps.replaceWorkspace(
          restoreGroupColors(
            { ...deps.getInternal().workspace, collapsedGroups },
            persisted.groupColors,
          ),
        );
        deps.publish();
        if (!activated) {
          const desired = persisted.activePath
            ? deps.getInternal().workspace.tabs.find((tab) =>
                sameRepo(tab.path, persisted.activePath ?? "", options),
              )
            : deps.getInternal().workspace.tabs[0];
          if (desired) {
            await presentRestored(desired.id);
          } else {
            deps.syncFilterFromSession(undefined);
            graph.showRepo(null);
            deps.publish();
          }
        }
        const toRead: string[] = [];
        const releaseParking = deps.holdPublish();
        try {
          for (const tab of deps.getInternal().workspace.tabs) {
            const session = deps.getInternal().sessions[tab.id];
            if (
              !session ||
              session.hasHydrated ||
              session.error ||
              session.trustRequired
            ) {
              continue;
            }
            if (watchCoordinator.admitWatch(tab.id, false).admitted) {
              toRead.push(tab.id);
            } else {
              deps.applyToSession(tab.id, session.generation, {
                watch: WATCH_PARKED,
                isLoading: false,
              });
            }
          }
        } finally {
          releaseParking();
        }
        for (const id of toRead) {
          const session = deps.getInternal().sessions[id];
          if (
            !session ||
            session.hasHydrated ||
            session.error ||
            session.trustRequired
          ) {
            continue;
          }
          await watchCoordinator.acquireWatch(id, session.path, false);
          if (watchCoordinator.hasWatch(id)) {
            await hydration.hydrate(id, session.path, session.generation);
          } else {
            deps.applyToSession(id, session.generation, { isLoading: false });
          }
        }
        statusPoller.ensure();
      } finally {
        resumePersist();
      }
    },
    targetForPath: async (
      rawPath: string,
      lookup: { open?: boolean } = {},
    ): Promise<MutationTarget | null> => {
      const find = (spelling: string): MutationTarget | null => {
        for (const tab of deps.getInternal().workspace.tabs) {
          const session = deps.getInternal().sessions[tab.id];
          if (!session || !sameRepo(session.path, spelling, options)) continue;
          const target = { tabId: session.id, path: session.path };
          return deps.resolveTarget(target) ? target : null;
        }
        return null;
      };
      const existing = find(rawPath);
      if (existing || lookup.open === false) return existing;
      if (!(await openRepo(rawPath, { activate: false }))) return null;
      return (
        find(rawPath) ??
        find(
          (await hydration.resolvePath(rawPath).catch(() => null))?.path ?? rawPath,
        )
      );
    },
  };
}
