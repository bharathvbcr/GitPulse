<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { platformChord, shortcutTextLabel } from "../ui/platformCopy";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { closeWithConfirmation, sessionByNativeId, terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { loadAgentDefaults } from "../stores/agentDefaultsStore";
  import type { FocusOutcome } from "../terminal/sessionFocus";
  import { isPromptLauncher, terminalLaunchRequests } from "../terminal/launchRequests";
  import { requestPin } from "../files/pinRequests";
  import { doubleClickFollows } from "../sidebar/layoutStore";
  import { taskTerminalRequests, taskTerminalHolds, consumeTaskTerminalRequest, heldElsewhere, holdTaskTerminal, launchFor, releaseTaskTerminal, requestFor, type HostCandidate, type TaskTerminalRequest } from "../terminal/taskLaunches";
  import { isCaseInsensitiveFs, sameRepo } from "../repos/paths";
  import { consoleLaunchRequests, consumeConsoleLaunch } from "../terminal/consoleLaunches";
  import { boundedCommand, retainCommand, retainExecutions, followsConsoleOutput } from "../terminal/consoleHistory";
  import { harnessStore } from "../stores/harnessStore";
  import { dockSection, dockSectionForChord, dockStatus, type DockSection } from "../terminal/dockSections";
  import { invoke } from "../ipc/invoke";
  import {
    Terminal,
    Play,
    LoaderCircle,
    Trash2,
    Clipboard,
    Check,
    AlertCircle,
    Shield,
    Clock,
    SquareTerminal,
    X,
    Plus,
    Search,
    ChevronDown,
    Maximize2,
    Pin,
    PinOff,
    Minimize2,
    Columns2,
    Rows2,
    FolderTree,
    PanelLeft,
    PanelLeftClose,
    FileText,
    ExternalLink,
    Send,
    GitBranch,
    Pencil,
    ChevronLeft,
    ChevronRight,
    Eye,
    RotateCw,
  } from "@lucide/svelte";
  import { tokenizeCommand } from "../terminal/tokenize";
  import type { TerminalRunResult } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { copyText } from "../desktop/clipboard";
  import { describeError } from "../ui/copy/describeFailure";
  import TerminalSession from "./TerminalSession.svelte";
  import BrowserSession from "./BrowserSession.svelte";
  import TerminalFilePane from "./TerminalFilePane.svelte";
  import ExplorerRail from "./files/ExplorerRail.svelte";
  import DockHeader from "./DockHeader.svelte";
  import DockAgentsSection, { type DockAgentRow } from "./DockAgentsSection.svelte";
  import {
    clampExplorerSidebarWidth,
    resolveExplorerFileAction,
    TERMINAL_EXPLORER_WIDTH_DEFAULT,
    TERMINAL_EXPLORER_WIDTH_MAX,
    TERMINAL_EXPLORER_WIDTH_MIN,
    terminalExplorerSidebar,
    type ExplorerOpenTarget,
  } from "../terminal/explorerSidebar";
  import {
    NO_FOLLOW_TARGET,
    adoptFollowPane,
    decideFollow,
    followPaneOpen,
    type FollowPaneCandidate,
    type FollowTarget,
  } from "../terminal/followAgent";
  import { CONTEXT_UNKNOWN, closeQuestion, startDirFrom } from "../terminal/sessionContext";
  import { askConfirm, askText } from "../stores/modalStore";
  import { toastStore } from "../stores/toastStore";
  import { openTaskForRun } from "../workbench/taskOpen";
  import { groupSessionsByRepository, sessionRow, summarizeCheckout, type CheckoutTab } from "../terminal/checkoutLabel";
  import { sessionCountLabel, sessionIsLive, sessionListCommand, sessionListNotice, sessionStatusLabel } from "../terminal/sessionList";
  import { forgetEndedTail } from "../terminal/endedBuffer";
  import { panelTabCounts } from "../terminal/panelTabCounts";
  import { activityAttentionLabel, activityToAgentStatus, sessionActivity } from "../terminal/sessionActivity";
  import { scrollbackSnapshots } from "../terminal/scrollbackSnapshot";
  import { detachedAdoptionSettled, reclaimAdoptedSession } from "../terminal/detachedSessions";
  import { isTauri } from "../platform";
  import {
    describeEndState,
    dismissSessionLedgerRow,
    filterOffersForRepo,
    reopenAllSessionLedgerOffers,
    reopenBannerHeading,
    reopenSessionLedgerOffer,
    reopenableOffers,
    sessionLedgerStore,
    subscribeSessionLedger,
  } from "../terminal/sessionLedger";
  import { formatDroppedPathsForSession, relativeToRepo } from "../terminal/dropFormatting";
  import { classifyDropRegion, terminalDropZones, type DropRegion } from "../terminal/dropZoneRegistry";
  import {
    agentEvents,
    type TerminalAgentTouchedFilePayload,
    type TerminalChildClosedPayload,
    type TerminalChildSpawnedPayload,
    type TerminalOpenFileRequestPayload,
  } from "../terminal/agentEvents";
  import type { AgentOscStatus } from "../terminal/commandBlocks";
  import { getCollisionRisk } from "../insights/client";
  import type { CollisionRisk } from "../insights/types";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import {
    adjacentPane,
    allGroupedPaneIds,
    applyTabStripDrop,
    buildTabStripSegments,
    chooseSplitPartner,
    pairJoinTargets,
    type PairJoinTarget,
    chooseSplitReplacementSlot,
    classifyTabStripDropZone,
    coLocateTabsForGroups,
    computeSplitLayout,
    computeTabDropHint,
    cycleSplitGroup,
    cycleVisibleSplitPane,
    detachTabToStripEnd,
    equalizeSplits,
    findGroupForPane,
    flipSplitOrientation,
    hasPane,
    leafPaneIds,
    MAX_PERSISTED_SNIPPET_CHARS,
    MAX_SPLIT_PANES,
    minimumSplitHeight,
    movePaneToSplit,
    moveSplitGroupByStep,
    moveSplitGroupToStripEnd,
    moveTabWithGroups,
    normalizeSplitGroups,
    orderGroupsByTabOrder,
    parsePersistedSplitWorkspace,
    recordGroupActivePane,
    remapSplitGroups,
    removePaneFromGroups,
    removeSplitGroup,
    renameSplitGroup,
    reorderSplitGroupInStrip,
    replacePane,
    resizeBranch,
    serializeSplitWorkspace,
    splitGroupDisplayLabel,
    splitPane,
    summarizeHiddenSplitSiblings,
    summarizeSplitGroupAttention,
    swapPanes,
    swapWithAdjacentSplitPane,
    terminalSplitChord,
    upsertSplitGroup,
    visiblePaneIds,
    type PersistedSplitLeafMeta,
    type SashDescriptor,
    type SplitDirection,
    type SplitGroup,
    type SplitNode,
    type TabDropZone,
    type TabStripDropMode,
  } from "../terminal/splitTree";
  import ScrollCue from "./ScrollCue.svelte";
  import LauncherIcon from "./LauncherIcon.svelte";
  import RepoLogo from "./RepoLogo.svelte";
  import {
    activateTab,
    buildPairHandoffPrompt,
    childTabsForParent,
    dockTabTypesFor,
    offeredDockTabKind,
    resolvePairHandoffTargets,
    sessionTypesFor,
    canOpenTab,
    MAX_STRIP_TABS,
    closeTab,
    cycleTab,
    initialState,
    launcherLabel,
    isHandoffCapableLauncher,
    openAttachedChildTab,
    openCheckoutTab,
    openTab,
    sanitizeWorktreeSlug,
    tabFor,
    paneOnScreen,
    setTabAutoWake,
    setTabPurpose,
    setTabResume,
    setTabStartDir,
    setTabInitialCommand,
    type BrowserPageState,
    type PairHandoffTarget,
    type ResumeLaunch,
    setTabTitle,
    setTabUrl,
    toggleTabPinned,
    browserTabFor,
    openBrowserTab,
    fileTabFor,
    canRepointFileTab,
    keepFileTabFor,
    normalizePaneFilePath,
    openFileTab,
    openSnippetTab,
    setTabFile,
    renameTab,
    tabDisplayTitle,
    tabLabel,
    effectiveSessionTitle,
    isGenericLauncherTitle,
    stripTitleSpinner,
    terminalTabChord,
    terminalTabDestination,
    type SessionKind,
    type LauncherKind,
    type TabKind,
    type TabState,
    type TerminalTab,
  } from "../terminal/tabs";
  import {
    crowdedTabStrip,
    filterLaunchers,
    filterSessionRecords,
    filterTerminalTabs,
    stepSearchIndex,
  } from "../terminal/tabSearch";
  import { bindHorizontalWheel, scrollChildIntoHorizontalView } from "../dom/overflowHint";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo, popoverExit } from "../ui/popover";

  /** The shared wire shape; aliased for this panel's existing call sites. */
  type TerminalRunResponse = TerminalRunResult;

  let {
    repoPath = null,
    visible = true,
    dockDragging = false,
    onClose,
    expanded = false,
    onToggleExpanded,
    dockMode = "docked",
    onToggleDockMode,
    onGoToSession,
    onOpenInCodeView,
    onCreateAgentWorktree,
    checkouts = [],
  }: {
    repoPath?: string | null;
    /** False while another repository's panel (or a closed dock) is showing. */
    visible?: boolean;
    /** True while the dock height separator is actively dragged. */
    dockDragging?: boolean;
    onClose?: () => void;
    expanded?: boolean;
    onToggleExpanded?: () => void;
    /** Whether the dock floats over the view or takes its own row; the dock owns the choice. */
    dockMode?: "docked" | "overlay";
    onToggleDockMode?: () => void;
    /**
     * Brings a session from the cross-repository Sessions list on screen,
     * switching repository tabs when it lives in another one.
     *
     * Supplied by the dock because it needs the repository store, which this
     * panel deliberately cannot see — see `goToSession`.
     */
    onGoToSession?: (session: TerminalSessionRecord) => Promise<FocusOutcome>;
    onOpenInCodeView?: (path: string, line: number | null) => void;
    /**
     * Provisions a sibling Git worktree on behalf of this panel's repository
     * tab via the dock's `MutationTarget`, keeping this panel decoupled from
     * the repository store.
     */
    onCreateAgentWorktree?: (
      targetPath: string,
      newBranch: string,
    ) => Promise<{ ok: true; path: string } | { ok: false; error: string | null }>;
    /**
     * The open repository tabs, for naming the checkout each listed session
     * runs in (repository, checkout, agent worktree), and for deciding which
     * queued task terminals this panel hosts (`taskLaunches.hostTabFor`).
     * Handed in by the dock for the same reason as `onGoToSession`; without
     * it a row falls back to what its path alone says, and the panel takes
     * only requests for its own checkout.
     */
    checkouts?: readonly (CheckoutTab & HostCandidate)[];
  } = $props();

  interface ExecutionEntry {
    id: string;
    command: string;
    timestamp: number;
    running: boolean;
    result?: TerminalRunResponse;
    error?: string;
  }

  let commandInput = $state("");
  let history = $state<string[]>([]);
  let historyIndex = $state(-1);
  let savedDraft = $state("");
  let executions = $state<ExecutionEntry[]>([]);
  let running = $state(false);
  let consoleFollowing = $state(true);
  let discardedExecutions = $state(0);
  let validationError = $state<string | null>(null);
  /**
   * A session-level failure (Go to, Task link, Close). Kept apart from the
   * Console input's `validationError` and rendered panel-level, because those
   * actions run in Shell mode, where the Console's input bar is not on screen.
   */
  let sessionError = $state<string | null>(null);
  let copiedId = $state<string | null>(null);

  let inputEl = $state<HTMLInputElement | null>(null);
  let scrollContainer = $state<HTMLDivElement | null>(null);
  let tabScroller: HTMLDivElement | undefined = $state();
  /** Copy-feedback reset timer; cleared on teardown so it cannot fire post-unmount. */
  let copiedResetTimer: ReturnType<typeof setTimeout> | null = null;
  let adoptionTimedOut = $state(false);
  let followAgent = $state(false);
  /**
   * The one pane Follow Agent drives (`terminal/followAgent.ts`). Follow
   * never navigates a pane it did not open or get handed, never opens a
   * second one, and ends when the user closes that pane or opens a file of
   * their own in it — otherwise every later agent edit popped a new split.
   */
  let followTarget = $state<FollowTarget>(NO_FOLLOW_TARGET);
  const followPaneFile = (tab: FollowPaneCandidate) => normalizePaneFilePath(tab.filePath) ?? null;

  /** Turns follow on, owning `paneId` (a preset's empty file pane) or nothing yet. */
  function startFollowing(paneId: string | null = null) {
    followTarget = paneId ? { paneId, filePath: null } : NO_FOLLOW_TARGET;
    followAgent = true;
  }

  function stopFollowing() {
    followAgent = false;
    followTarget = NO_FOLLOW_TARGET;
  }

  /** Records the pane follow just pointed (or opened) as its own, with the file now in it. */
  function adoptFollowPaneById(paneId: string | undefined) {
    followTarget = adoptFollowPane(
      paneId ? tabState.tabs.find((tab) => tab.id === paneId) : undefined,
      followPaneFile,
    );
  }

  // Closing follow's pane ends follow at once, so the toggle never reads
  // "following" while nothing will be. A takeover is judged at the next edit
  // (`decideFollow`), since follow itself repoints the pane between edits.
  $effect(() => {
    const target = followTarget;
    const tabs = tabState.tabs;
    if (!followAgent || target.paneId === null) return;
    if (!followPaneOpen(target, tabs)) untrack(stopFollowing);
  });

  onMount(() => {
    inputEl?.focus();
    // The session limit is an agent default; read it before the first
    // session so the panel stops where the backend does.
    void loadAgentDefaults().catch(() => {});
    const adoptionFallbackTimer = isTauri()
      ? setTimeout(() => {
          adoptionTimedOut = true;
        }, 1500)
      : null;
    const unsubAgentPanel = agentEvents.subscribePanelEvents({
      onChildSpawned: handleChildSpawned,
      onChildClosed: handleChildClosed,
      onOpenFile: handleAgentOpenFile,
      onTouchedFile: handleAgentTouchedFile,
    });
    const unsubSessionLedger = subscribeSessionLedger();
    window.addEventListener("pagehide", flushPendingSplitPersist);
    return () => {
      window.removeEventListener("pagehide", flushPendingSplitPersist);
      flushPendingSplitPersist();
      unsubAgentPanel();
      unsubSessionLedger();
      if (adoptionFallbackTimer !== null) clearTimeout(adoptionFallbackTimer);
      if (copiedResetTimer !== null) {
        clearTimeout(copiedResetTimer);
        copiedResetTimer = null;
      }
    };
  });

  // ---------------------------------------------------------------------
  // Interactive shells (PTY), one per tab. They run OUTSIDE the MANVI gate by
  // nature: a shell can execute anything, so claiming gate coverage here would
  // be a check that cannot run reporting what a check that ran reports. The
  // bounded Console tab is the gated surface.
  //
  // Session ownership lives in TerminalSession, one instance per tab. This
  // component owns only the strip: which tabs exist, which is focused, and
  // what each is called. The repository this panel talks to is a prop from
  // the dock — one panel per open repo tab, hidden rather than remounted —
  // so a tab switch cannot kill the shells, and a hidden panel cannot follow
  // `currentPath` into a different worktree.
  // ---------------------------------------------------------------------
  /** The repository store's checkout identity rule, for every path compared here. */
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  type PtyMode = "shell" | "console";
  /**
   * Which dock section is showing. Shell and Files keep the tab strip and the
   * pane canvas on screen; Agents and Console hide them (never unmount: a
   * hidden xterm keeps its PTY) and draw their own lens instead.
   */
  let section = $state<DockSection>(get(dockSection));
  /**
   * "shell" while the pane canvas is on screen (Shell and Files sections),
   * "console" while it is not (Console, and the Agents lens, which treats the
   * panes as off-screen exactly as the Console does). Every on-screen check
   * below reads this, so xterm fit and GPU slots follow the section.
   */
  const mode = $derived<PtyMode>(section === "shell" || section === "files" ? "shell" : "console");
  function setSection(next: DockSection) {
    section = next;
    dockSection.set(next);
  }
  /** Bring the panes back on screen without leaving Files for Shell. */
  function showShell() {
    if (section === "agents" || section === "console") setSection("shell");
  }
  function initialTabs(): TabState {
    const launch = get(terminalLaunchRequests);
    if (launch && repoPath && sameRepo(launch.repoPath, repoPath, pathOpts)) {
      const needsAnchorShell =
        launch.launcher === "file" &&
        launch.fileOpenMode !== "tab" &&
        get(terminalSessions).length < get(terminalSessionLimit);
      if (!needsAnchorShell) return { tabs: [], activeId: null };
    }
    // A panel mounted for a queued request starts empty: the request opener
    // below opens its tab, through the same admission as every other.
    if (requestFor(get(taskTerminalRequests), repoPath, pathOpts, checkouts)) return { tabs: [], activeId: null };
    return initialState();
  }
  let tabState = $state<TabState>(untrack(initialTabs));
  const activeId = $derived(tabState.activeId);
  let sessions = $state<Record<string, TerminalSession | undefined>>({});
  /**
   * Browser tabs, apart from `sessions` on purpose: a browser runs no process,
   * so every terminal-only action (find, scrollback, output, context) reaches
   * `sessions[id]`, finds nothing for a browser tab, and does nothing.
   */
  let browsers = $state<Record<string, BrowserSession | undefined>>({});
  let filePanes = $state<Record<string, TerminalFilePane | undefined>>({});
  let panesContainerEl = $state<HTMLDivElement | null>(null);
  /** The session types this host can open; browser sessions are macOS-only. */
  const sessionTypes = $derived(sessionTypesFor($hostPlatform.browser_sessions));
  const codeViewClick = $derived(platformChord("⌘/Ctrl+Click", "Ctrl+Click", $hostPlatform.os));
  /** All dock tab kinds selectable in Session Typer, including File Explorer. */
  const dockTabTypes = $derived(dockTabTypesFor($hostPlatform.browser_sessions));
  /** What the reader chose last; shown as `nextLauncher` once this host is known to offer it. */
  let chosenLauncher = $state<TabKind>(get(interfaceStore).terminalLauncher);
  const nextLauncher = $derived(offeredDockTabKind(chosenLauncher, $hostPlatform.browser_sessions));
  const activeLauncher = $derived(tabState.tabs.find((tab) => tab.id === activeId)?.launcher ?? null);
  const activeIsNonTerminal = $derived(activeLauncher === "browser" || activeLauncher === "file");
  let splitGroups = $state<SplitGroup[]>([]);
  let zoomedPaneId = $state<string | null>(null);
  let draggedTabId = $state<string | null>(null);
  let draggedGroupId = $state<string | null>(null);
  let tabDropHint = $state<TabDropZone | null>(null);
  let tabStripDropHint = $state<{
    tabId?: string;
    groupId?: string;
    detach?: boolean;
    splitDropzone?: boolean;
    mode: TabStripDropMode;
  } | null>(null);
  let fileDropHint = $state<{ paneId: string; region: DropRegion } | null>(null);
  let tabContextMenu = $state<{ tabId: string; x: number; y: number } | null>(null);
  let tabContextMenuOpener: HTMLElement | null = null;
  let splitContextMenu = $state<{
    x: number;
    y: number;
    direction: SplitDirection;
    position: "before" | "after";
  } | null>(null);
  let splitContextMenuOpener: HTMLElement | null = null;
  let groupContextMenu = $state<{ groupId: string; x: number; y: number } | null>(null);
  let groupContextMenuOpener: HTMLElement | null = null;
  /** The stored preference; the Files section shows the explorer regardless (see `explorerVisible`). */
  const explorerPreferenceOpen = $derived($terminalExplorerSidebar.open);
  /** On screen: the reader's saved choice, or the Files section peeking at it without writing the choice. */
  const explorerSidebarOpen = $derived(Boolean(repoPath) && (explorerPreferenceOpen || section === "files"));
  const explorerOpenTarget = $derived<ExplorerOpenTarget>($terminalExplorerSidebar.target);
  const explorerSidebarWidth = $derived($terminalExplorerSidebar.width);
  let explorerResizing = $state(false);
  let explorerAsideEl = $state<HTMLElement | null>(null);
  /**
   * Entering Files from a dock that had the explorer closed puts focus on the
   * rail, so the section answers the keyboard the way it answers the mouse.
   * Keyed on the section alone: nothing the rail does may pull focus again.
   */
  let filesFocusReady = false;
  $effect(() => {
    const entering = section === "files";
    const was = untrack(() => filesFocusReady);
    filesFocusReady = entering;
    if (!entering || was || !visible) return;
    if (untrack(() => explorerPreferenceOpen)) return;
    void tick().then(() => {
      explorerAsideEl
        ?.querySelector<HTMLElement>('[role="tree"], [role="treeitem"], input, button:not(:disabled)')
        ?.focus({ preventScroll: true });
    });
  });
  let selectedExplorerFile = $state<string | null>(null);
  let lastExplorerMod = { altKey: false, metaOrCtrlKey: false };
  /** Where the docked explorer's last click sent a file, for its double-click. */
  let lastExplorerOpen: {
    path: string;
    action: ReturnType<typeof resolveExplorerFileAction>;
    at: number;
  } | null = null;
  let inlineRenameTabId = $state<string | null>(null);
  let inlineRenameSurface = $state<"tab" | "pane" | null>(null);
  let inlineRenameDraft = $state("");
  let inlineRenameInput = $state<HTMLInputElement | null>(null);
  let inlineRenameGroupId = $state<string | null>(null);
  let inlineRenameGroupDraft = $state("");
  let inlineRenameGroupInput = $state<HTMLInputElement | null>(null);
  const pinnedTabIds = $derived(new Set(tabState.tabs.filter((tab) => tab.pinned).map((tab) => tab.id)));
  const groupedTabIds = $derived(allGroupedPaneIds(splitGroups));
  const activeGroup = $derived(findGroupForPane(splitGroups, activeId));
  const activeGroupIndex = $derived(
    activeGroup ? splitGroups.findIndex((group) => group.id === activeGroup.id) : -1,
  );
  const activeGroupLabel = $derived(
    activeGroup && activeGroupIndex >= 0
      ? splitGroupDisplayLabel(activeGroup, activeGroupIndex, splitGroups.length)
      : null,
  );
  const splitRoot = $derived<SplitNode | null>(activeGroup?.tree ?? null);
  const visibleIds = $derived(visiblePaneIds(splitRoot, activeId, zoomedPaneId));
  const splitIds = $derived(splitRoot ? visibleIds : null);
  const splitLayout = $derived(computeSplitLayout(splitRoot, activeId, zoomedPaneId));
  const allSplitLeafIds = $derived(splitRoot ? leafPaneIds(splitRoot) : []);
  const splitSlotByTabId = $derived.by(() => {
    const map: Record<string, number> = {};
    allSplitLeafIds.forEach((id, index) => {
      map[id] = index + 1;
    });
    return map;
  });
  const groupMetaByTabId = $derived.by(() => {
    const map: Record<
      string,
      {
        group: SplitGroup;
        groupIndex: number;
        label: string;
        slot: number;
        totalSlots: number;
      }
    > = {};
    splitGroups.forEach((group, groupIdx) => {
      const label = splitGroupDisplayLabel(group, groupIdx, splitGroups.length);
      const leaves = leafPaneIds(group.tree);
      leaves.forEach((id, leafIdx) => {
        map[id] = {
          group,
          groupIndex: groupIdx + 1,
          label,
          slot: leafIdx + 1,
          totalSlots: leaves.length,
        };
      });
    });
    return map;
  });
  const tabStripSegments = $derived(buildTabStripSegments(tabState.tabs, splitGroups));
  const orderedStripTabs = $derived(coLocateTabsForGroups(tabState.tabs, splitGroups));
  const activeSplitSlot = $derived(activeId ? splitSlotByTabId[activeId] ?? null : null);
  /** The pane canvas's least height; below it the pane area scrolls (`minimumSplitHeight`). */
  const paneCanvasMinHeight = $derived(minimumSplitHeight(splitRoot, activeId, zoomedPaneId));
  const visibleFileTab = $derived(
    tabState.tabs.find(
      (tab) =>
        tab.launcher === "file" &&
        !tab.snippet &&
        (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId),
    ) ?? null,
  );
  const hasVisibleFilePane = $derived(
    tabState.tabs.some(
      (tab) => tab.launcher === "file" && (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId),
    ),
  );
  const activeExplorerFile = $derived(selectedExplorerFile ?? visibleFileTab?.filePath ?? null);
  const stripPairDropTargetId = $derived.by(() => {
    if (!draggedTabId || tabState.tabs.length < 2 || leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES) {
      return null;
    }
    if (splitRoot && hasPane(splitRoot, draggedTabId)) return null;
    if (activeId && activeId !== draggedTabId) return activeId;
    return tabState.tabs.find((tab) => tab.id !== draggedTabId)?.id ?? null;
  });
  let sashDragging = $state(false);
  const layoutDragging = $derived(dockDragging || sashDragging || explorerResizing);

  function applyGroupsAndCoLocate(
    nextGroups: readonly SplitGroup[],
    nextTabState: TabState = tabState,
  ) {
    const validIds = new Set(nextTabState.tabs.map((tab) => tab.id));
    const normalized = normalizeSplitGroups(nextGroups, validIds);
    const coLocated = coLocateTabsForGroups(nextTabState.tabs, normalized);
    const orderedGroups = recordGroupActivePane(
      orderGroupsByTabOrder(normalized, coLocated),
      nextTabState.activeId,
    );
    const tabsChanged =
      coLocated !== nextTabState.tabs || nextTabState.activeId !== tabState.activeId;
    if (tabsChanged || nextTabState !== tabState) {
      tabState =
        coLocated === nextTabState.tabs
          ? nextTabState
          : { tabs: coLocated, activeId: nextTabState.activeId };
    }
    splitGroups = orderedGroups;
  }

  function setActiveSplitTree(nextTree: SplitNode | null) {
    if (!nextTree || nextTree.kind !== "branch") {
      if (activeGroup) applyGroupsAndCoLocate(removeSplitGroup(splitGroups, activeGroup.id));
      return;
    }
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: activeGroup?.id,
        name: activeGroup?.name,
        activePaneId: activeId,
      }),
    );
  }

  function upsertGroupForAnchor(
    anchorPaneId: string,
    nextTree: SplitNode | null,
    nextTabState: TabState = tabState,
    preferredActivePaneId?: string | null,
  ) {
    const owner = findGroupForPane(splitGroups, anchorPaneId);
    const targetActive = preferredActivePaneId ?? nextTabState.activeId;
    const stateWithFocus =
      targetActive && nextTabState.activeId !== targetActive
        ? activateTab(nextTabState, targetActive)
        : nextTabState;
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: owner?.id,
        name: owner?.name,
        activePaneId: targetActive,
      }),
      stateWithFocus,
    );
  }

  function ungroupPair(groupId: string) {
    if (activeGroup?.id === groupId) zoomedPaneId = null;
    if (inlineRenameGroupId === groupId) inlineRenameGroupId = null;
    applyGroupsAndCoLocate(removeSplitGroup(splitGroups, groupId));
  }

  /**
   * Adds `tabId` to the pair `target` names and shows that pair — the menu
   * form of dropping the tab on the pair's badge, through the same
   * `applyTabStripDrop` so the two cannot disagree.
   */
  function joinPair(tabId: string, target: PairJoinTarget) {
    zoomedPaneId = null;
    const outcome = applyTabStripDrop(tabState.tabs, splitGroups, {
      movingTabId: tabId,
      targetTabId: target.anchorPaneId,
      mode: "pair",
      pinnedIds: pinnedTabIds,
    });
    if (outcome.refusedReason) {
      sessionError = outcome.refusedReason;
      return;
    }
    const nextState = outcome.activeTabId
      ? activateTab({ ...tabState, tabs: outcome.tabs }, outcome.activeTabId)
      : { ...tabState, tabs: outcome.tabs };
    tabState = nextState;
    splitGroups = recordGroupActivePane(outcome.groups, nextState.activeId);
    void tick().then(() => revealTab(nextState.activeId ?? tabId));
  }

  function moveTabInStrip(id: string, step: -1 | 1) {
    const moved = moveTabWithGroups(tabState.tabs, splitGroups, id, step);
    if (moved.tabs !== tabState.tabs) {
      tabState = { ...tabState, tabs: moved.tabs };
    }
    if (moved.groups !== splitGroups) {
      splitGroups = moved.groups;
    }
  }

  $effect(() => {
    if (!repoPath) return;
    panelTabCounts.report(repoPath, tabState.tabs.length, pathOpts);
  });

  function splitStorageKey(path: string | null | undefined): string | null {
    return path ? `gitpulse.terminal.splitLayout:${path}` : null;
  }

  function applyPersistedLeafMeta(
    state: TabState,
    tabId: string,
    meta: PersistedSplitLeafMeta | undefined,
  ): TabState {
    if (!meta) return state;
    let next = state;
    const current = next.tabs.find((tab) => tab.id === tabId);
    if (!current) return next;
    if (meta.launchName && !current.launchName) {
      next = {
        ...next,
        tabs: next.tabs.map((tab) =>
          tab.id === tabId
            ? { ...tab, launchName: meta.launchName, ...(tab.name ? {} : { name: meta.launchName }) }
            : tab,
        ),
      };
    }
    if (meta.userNamed && meta.name && !current.userNamed) {
      next = renameTab(next, tabId, meta.name);
    } else if (meta.name && !current.name && !current.userNamed) {
      next = {
        ...next,
        tabs: next.tabs.map((tab) => (tab.id === tabId ? { ...tab, name: tab.name ?? meta.name } : tab)),
      };
    }
    const latest = next.tabs.find((tab) => tab.id === tabId);
    if (meta.title && latest && !latest.title) {
      next = setTabTitle(next, tabId, meta.title);
    }
    if (meta.pinned) {
      next = toggleTabPinned(next, tabId, true);
    }
    if (meta.kept) {
      // By id: the restored pane is this tab, whatever its path is spelled as.
      next = {
        ...next,
        tabs: next.tabs.map((tab) => (tab.id === tabId && tab.launcher === "file" ? { ...tab, kept: true } : tab)),
      };
    }
    return next;
  }

  let hydratedSplitForRepo: string | null = null;
  $effect(() => {
    const key = splitStorageKey(repoPath);
    const readyToHydrate = !isTauri() || $detachedAdoptionSettled || adoptionTimedOut;
    const ids = new Set(tabState.tabs.map((tab) => tab.id));
    if (!key || !readyToHydrate || hydratedSplitForRepo === key || ids.size === 0) return;
    hydratedSplitForRepo = key;
    untrack(() => {
      if (splitGroups.length > 0) return;
      try {
        const stored = globalThis.localStorage?.getItem(key);
        const parsed = parsePersistedSplitWorkspace(stored);
        if (!parsed || parsed.groups.length === 0) return;
        const paneIdMap = new Map<string, string>();
        for (const id of ids) paneIdMap.set(id, id);
        let nextState = tabState;
        const usedCurrentTabs = new Set<string>();
        const allPersistedLeaves = parsed.groups.flatMap((group) => leafPaneIds(group.tree));
        for (const leafId of allPersistedLeaves) {
          if (usedCurrentTabs.has(leafId)) continue;
          const meta = parsed.leaves[leafId];
          if (paneIdMap.has(leafId)) {
            usedCurrentTabs.add(leafId);
            nextState = applyPersistedLeafMeta(nextState, leafId, meta);
            continue;
          }
          if (!meta) continue;
          if (meta.launcher === "file") {
            if (meta.snippet) {
              const openedSnippet = openSnippetTab(nextState, {
                title: meta.snippet.title,
                content: meta.snippet.content,
                reuseExisting: false,
                focus: false,
              });
              if (openedSnippet.id) {
                nextState = applyPersistedLeafMeta(openedSnippet.state, openedSnippet.id, meta);
                paneIdMap.set(leafId, openedSnippet.id);
                usedCurrentTabs.add(openedSnippet.id);
              }
              continue;
            }
            const opened = openFileTab(nextState, {
              filePath: meta.filePath,
              checkout: meta.checkout,
              line: meta.revealLine ?? null,
              column: meta.revealColumn ?? null,
              reuseExisting: false,
              focus: false,
            });
            if (opened.id) {
              nextState = applyPersistedLeafMeta(opened.state, opened.id, meta);
              paneIdMap.set(leafId, opened.id);
              usedCurrentTabs.add(opened.id);
            }
            continue;
          }
          if (meta.launcher === "browser") {
            if ($hostPlatform.browser_sessions) {
              const opened = openBrowserTab(nextState, meta.url);
              if (opened.id) {
                nextState = applyPersistedLeafMeta(
                  { ...opened.state, activeId: nextState.activeId ?? opened.id },
                  opened.id,
                  meta,
                );
                paneIdMap.set(leafId, opened.id);
                usedCurrentTabs.add(opened.id);
              }
            }
            continue;
          }
          if (meta.backendSessionId) {
            const reclaimed = reclaimAdoptedSession(meta.backendSessionId);
            if (reclaimed && repoPath && sameRepo(reclaimed.repoPath, repoPath, pathOpts)) {
              const attached = openAttachedChildTab(nextState, {
                sessionId: reclaimed.sessionId,
                parentSessionId: meta.parentSessionId,
                launcher: reclaimed.launcher,
                title: meta.parentSessionId ? (reclaimed.title ?? null) : null,
                focus: false,
              });
              if (attached.id) {
                let hydrated = attached.state;
                if (!meta.parentSessionId && reclaimed.title) {
                  hydrated = setTabTitle(hydrated, attached.id, reclaimed.title);
                }
                nextState = applyPersistedLeafMeta(hydrated, attached.id, meta);
                paneIdMap.set(leafId, attached.id);
                usedCurrentTabs.add(attached.id);
                continue;
              }
            }
          }
          const fallbackExisting = nextState.tabs.find(
            (tab) => !usedCurrentTabs.has(tab.id) && tab.launcher === meta.launcher,
          );
          if (fallbackExisting) {
            nextState = applyPersistedLeafMeta(nextState, fallbackExisting.id, meta);
            paneIdMap.set(leafId, fallbackExisting.id);
            usedCurrentTabs.add(fallbackExisting.id);
          }
        }
        const restoredGroups = remapSplitGroups(parsed.groups, paneIdMap);
        if (restoredGroups.length > 0) {
          const targetGroup =
            (parsed.activeGroupId
              ? restoredGroups.find((group) => group.id === parsed.activeGroupId)
              : null) ?? restoredGroups[0];
          const preferredPane =
            targetGroup.activePaneId ?? leafPaneIds(targetGroup.tree)[0];
          if (preferredPane && parsed.activeGroupId !== null) {
            nextState = activateTab(nextState, preferredPane);
          }
          applyGroupsAndCoLocate(restoredGroups, nextState);
        } else if (nextState !== tabState) {
          tabState = nextState;
        }
      } catch {
        /* localStorage unavailable */
      }
    });
  });

  let lastPersistedSplitKey: string | null = null;
  let lastPersistedSplitJson: string | null = null;
  let lastPersistedStructuralJson: string | null = null;
  let pendingSplitWrite: {
    key: string;
    serialized: string | null;
    structuralJson: string | null;
  } | null = null;
  let splitPersistTimer: ReturnType<typeof setTimeout> | null = null;

  function writeSplitStorage(
    key: string,
    serialized: string | null,
    structuralJson: string | null,
  ) {
    lastPersistedSplitKey = key;
    lastPersistedSplitJson = serialized;
    lastPersistedStructuralJson = structuralJson;
    try {
      if (serialized) globalThis.localStorage?.setItem(key, serialized);
      else globalThis.localStorage?.removeItem(key);
    } catch {
      /* localStorage unavailable */
    }
  }

  function flushPendingSplitPersist() {
    if (splitPersistTimer !== null) {
      clearTimeout(splitPersistTimer);
      splitPersistTimer = null;
    }
    const pending = pendingSplitWrite;
    if (!pending) return;
    pendingSplitWrite = null;
    writeSplitStorage(pending.key, pending.serialized, pending.structuralJson);
  }

  $effect(() => {
    const key = splitStorageKey(repoPath);
    const currentGroups = splitGroups;
    const currentActiveGroupId = activeGroup?.id ?? null;
    if (pendingSplitWrite && pendingSplitWrite.key !== key) {
      flushPendingSplitPersist();
    }
    if (!key || hydratedSplitForRepo !== key) return;
    if (currentGroups.length === 0) {
      if (pendingSplitWrite?.key === key) {
        if (splitPersistTimer !== null) {
          clearTimeout(splitPersistTimer);
          splitPersistTimer = null;
        }
        pendingSplitWrite = null;
      }
      if (
        lastPersistedSplitKey !== key ||
        lastPersistedSplitJson !== null ||
        lastPersistedStructuralJson !== null
      ) {
        writeSplitStorage(key, null, null);
      }
      return;
    }

    const currentTabs = tabState.tabs;
    const draggingSash = sashDragging;
    const groupedLeaves = allGroupedPaneIds(currentGroups);
    const leaves: Record<string, PersistedSplitLeafMeta> = {};
    const structuralLeaves: Record<string, PersistedSplitLeafMeta> = {};
    currentTabs.forEach((tab, slotIndex) => {
      if (!groupedLeaves.has(tab.id)) return;
      const backendId = sessions[tab.id]?.backendSessionId?.() || tab.attachSessionId;
      const structuralMeta: PersistedSplitLeafMeta = {
        launcher: tab.launcher,
        slotIndex,
        ...(tab.name ? { name: tab.name } : {}),
        ...(tab.launchName ? { launchName: tab.launchName } : {}),
        ...(tab.userNamed ? { userNamed: true } : {}),
        ...(tab.pinned ? { pinned: true } : {}),
        ...(tab.kept ? { kept: true } : {}),
        ...(backendId ? { backendSessionId: backendId } : {}),
        ...(tab.parentSessionId ? { parentSessionId: tab.parentSessionId } : {}),
        ...(tab.filePath ? { filePath: tab.filePath } : {}),
        ...(tab.revealLine != null ? { revealLine: tab.revealLine } : {}),
        ...(tab.revealColumn != null ? { revealColumn: tab.revealColumn } : {}),
        ...(tab.snippet
          ? {
              snippet: {
                title: tab.snippet.title,
                content: tab.snippet.content.slice(0, MAX_PERSISTED_SNIPPET_CHARS),
              },
            }
          : {}),
        ...(tab.url ? { url: tab.url } : {}),
        ...(tab.checkout ? { checkout: tab.checkout } : {}),
      };
      leaves[tab.id] = tab.title ? { ...structuralMeta, title: tab.title } : structuralMeta;
      if (!draggingSash) structuralLeaves[tab.id] = structuralMeta;
    });

    const serialized = serializeSplitWorkspace(currentGroups, leaves, currentActiveGroupId);
    const structuralJson = draggingSash
      ? lastPersistedStructuralJson
      : serializeSplitWorkspace(currentGroups, structuralLeaves, currentActiveGroupId);
    const latestKey = pendingSplitWrite ? pendingSplitWrite.key : lastPersistedSplitKey;
    const latestSerialized = pendingSplitWrite
      ? pendingSplitWrite.serialized
      : lastPersistedSplitJson;
    if (latestKey === key && latestSerialized === serialized) return;

    if (
      !draggingSash &&
      (lastPersistedSplitKey !== key || structuralJson !== lastPersistedStructuralJson)
    ) {
      if (splitPersistTimer !== null) {
        clearTimeout(splitPersistTimer);
        splitPersistTimer = null;
      }
      pendingSplitWrite = null;
      writeSplitStorage(key, serialized, structuralJson);
      return;
    }

    pendingSplitWrite = { key, serialized, structuralJson };
    if (splitPersistTimer === null) {
      splitPersistTimer = setTimeout(flushPendingSplitPersist, 250);
    }
  });

  $effect(() => {
    const activeDrag = layoutDragging;
    for (const session of Object.values(sessions)) {
      session?.setDragging(activeDrag);
    }
  });
  let tabOptions = $state(false);
  let sessionListOpen = $state(false);
  let sessionSearchOpen = $state(false);
  let launcherMenuOpen = $state(false);
  let sessionQuery = $state("");
  let sessionListQuery = $state("");
  let launcherQuery = $state("");
  let sessionHighlight = $state(0);
  let launcherHighlight = $state(0);
  let sessionSearchInput = $state<HTMLInputElement | null>(null);
  let launcherSearchInput = $state<HTMLInputElement | null>(null);
  let launcherTriggerEl = $state<HTMLButtonElement | null>(null);
  let sessionListTrigger = $state<HTMLButtonElement | null>(null);
  let sessionListEl = $state<HTMLDivElement | null>(null);
  /** Null until the reader moves: the focused row, or the first, starts selected. */
  let sessionListHighlight = $state<number | null>(null);
  let renameValue = $state("");
  let tabStatuses = $state<Record<string, string>>({});
  /** The loose tab a plain Split re-pairs with the active one, if any. */
  const splitPartner = $derived(
    chooseSplitPartner(
      tabState.tabs,
      splitGroups,
      activeId,
      nextLauncher,
      Object.keys(tabStatuses).filter((id) => tabStatuses[id] === "exited" || tabStatuses[id] === "error"),
    ),
  );
  const splitPartnerTab = $derived(
    splitPartner ? tabState.tabs.find((tab) => tab.id === splitPartner) ?? null : null,
  );
  /** What a plain Split will put in the new pane, for its tooltip and menu. */
  const splitWithLabel = $derived(
    splitPartnerTab
      ? tabLabel(splitPartnerTab)
      : `new ${nextLauncher === "file" ? "Explorer" : launcherLabel(nextLauncher)}`,
  );
  const canPlainSplit = $derived(
    Boolean(activeId) && leafPaneIds(splitRoot).length < MAX_SPLIT_PANES && (Boolean(splitPartner) || canCreateKind(nextLauncher)),
  );
  let tabAgentStatuses = $state<Record<string, AgentOscStatus | null>>({});
  let tabBrowserStates = $state<Record<string, BrowserPageState>>({});
  let unread = $state(new Set<string>());
  const checkoutInfo = $derived(repoPath ? summarizeCheckout(repoPath, checkouts, pathOpts) : null);
  const activeTab = $derived(tabState.tabs.find((tab) => tab.id === activeId));
  const backendSessionIdByTabId = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const entry of $terminalSessions) {
      if (entry.sessionId && (!repoPath || sameRepo(entry.repoPath, repoPath, pathOpts))) {
        map[entry.key] = entry.sessionId;
      }
    }
    for (const tab of tabState.tabs) {
      void tabStatuses[tab.id];
      if (!map[tab.id]) {
        const fallback = sessions[tab.id]?.backendSessionId?.() || tab.attachSessionId;
        if (fallback) map[tab.id] = fallback;
      }
    }
    return map;
  });
  let prevEffectiveAgentStatuses: Record<string, AgentOscStatus | null> = {};
  const effectiveAgentStatuses = $derived.by(() => {
    const activityMap = $sessionActivity;
    const next: Record<string, AgentOscStatus | null> = {};
    for (const tab of tabState.tabs) {
      if (tabStatuses[tab.id] === "exited") continue;
      const osc = tabAgentStatuses[tab.id];
      if (osc) {
        next[tab.id] = osc;
        continue;
      }
      const backendId = backendSessionIdByTabId[tab.id];
      const fallback = backendId ? activityToAgentStatus(activityMap.get(backendId)) : null;
      if (fallback) next[tab.id] = fallback;
    }
    const prevKeys = Object.keys(prevEffectiveAgentStatuses);
    const nextKeys = Object.keys(next);
    if (
      prevKeys.length === nextKeys.length &&
      nextKeys.every(
        (key) =>
          prevEffectiveAgentStatuses[key]?.state === next[key]?.state &&
          prevEffectiveAgentStatuses[key]?.detail === next[key]?.detail,
      )
    ) {
      return prevEffectiveAgentStatuses;
    }
    prevEffectiveAgentStatuses = next;
    return next;
  });
  const activeDisplay = $derived(
    activeTab ? tabDisplayTitle(activeTab, effectiveAgentStatuses[activeTab.id], tabBrowserStates[activeTab.id]) : null,
  );
  const hiddenSplitSummary = $derived(
    summarizeHiddenSplitSiblings(splitRoot, zoomedPaneId, unread, effectiveAgentStatuses),
  );
  const groupAttentions = $derived.by(() => {
    const map: Record<string, ReturnType<typeof summarizeSplitGroupAttention>> = {};
    for (const group of splitGroups) {
      map[group.id] = summarizeSplitGroupAttention(
        group,
        activeGroup?.id === group.id,
        unread,
        effectiveAgentStatuses,
      );
    }
    return map;
  });
  const activeBackendSessionId = $derived(
    activeTab ? (backendSessionIdByTabId[activeTab.id] ?? null) : null,
  );
  const activeChildTabs = $derived(
    childTabsForParent(tabState, activeBackendSessionId, tabStatuses),
  );
  const pairHandoffTargets = $derived(
    splitRoot ? resolvePairHandoffTargets(tabState.tabs, visibleIds, activeId, tabStatuses) : [],
  );
  const pairHandoffTarget = $derived(pairHandoffTargets[0] ?? null);
  let collisionSummary = $state<CollisionRisk | null>(null);
  /** The header's ⋯ menu; the header owns drawing it, the panel closes it with the other chrome. */
  let dockMenuOpen = $state(false);
  const dockStatusSummary = $derived(
    dockStatus({ conflictedCount: checkoutInfo?.conflictedCount ?? 0, collision: collisionSummary }),
  );
  const statusDetail = $derived.by(() => {
    const kind = dockStatusSummary.primary?.kind;
    if (kind === "unverified") return collisionSummary?.error || "Cross-worktree collision check unavailable";
    if (kind === "collision") {
      return (
        collisionSummary?.items.slice(0, 6).map((item) => item.path).join(", ") ||
        "Concurrent dirty files across worktrees or live sessions"
      );
    }
    return null;
  });
  const contextTitle = $derived.by(() => {
    if (!repoPath) return "No repository";
    if (!checkoutInfo) return repoPath;
    const { label, currentBranch } = checkoutInfo;
    return `Checkout: ${label.repository}${label.checkout ? ` · ${label.checkout}` : ""}${currentBranch ? ` (${currentBranch})` : ""}\n${repoPath}`;
  });

  // The Agents lens. Agents are a view over the same tabs the strip shows —
  // never a second list of sessions — so a row here selects a strip tab.
  const agentRowsHere = $derived.by<DockAgentRow[]>(() =>
    tabState.tabs
      .filter((tab) => isHandoffCapableLauncher(tab.launcher))
      .map((tab) => {
        const display = tabDisplayTitle(tab, effectiveAgentStatuses[tab.id], tabBrowserStates[tab.id]);
        const backendId = backendSessionIdByTabId[tab.id];
        const attention =
          tabStatuses[tab.id] === "error"
            ? "Failed"
            : backendId
              ? activityAttentionLabel($sessionActivity.get(backendId))
              : null;
        return {
          id: tab.id,
          launcher: tab.launcher,
          title: display.primary,
          detail: display.secondary ?? null,
          status: tabStatuses[tab.id] === "exited" ? { label: "Ended", cls: "text-textMuted" } : oscStatusBadge(effectiveAgentStatuses[tab.id]),
          attention,
          active: tab.id === activeId,
          unread: unread.has(tab.id),
        };
      }),
  );
  const agentSessionsElsewhere = $derived(
    $terminalSessions.filter(
      (session) =>
        session.launcher !== undefined &&
        isHandoffCapableLauncher(session.launcher) &&
        !sessionRow(session, repoPath, checkouts, pathOpts).here,
    ),
  );
  const agentRowsElsewhere = $derived.by<DockAgentRow[]>(() =>
    agentSessionsElsewhere.map((session) => {
      const info = sessionRow(session, repoPath, checkouts, pathOpts);
      const live = session.sessionId ? $sessionActivity.get(session.sessionId) : undefined;
      const cleaned = (raw: string | null | undefined) =>
        raw ? stripTitleSpinner(raw).replace(/^[✳✦★•]+\s+/u, "").trim() : "";
      const launcher = session.launcher ?? "claude";
      const own = cleaned(session.title);
      const liveTitle = cleaned(live?.title);
      const title =
        (own && !isGenericLauncherTitle(own, launcher) ? own : "") ||
        (liveTitle && !isGenericLauncherTitle(liveTitle, launcher) ? liveTitle : "") ||
        session.label.trim() ||
        launcherLabel(launcher);
      return {
        id: session.key,
        launcher,
        title,
        detail: `${info.repository}${info.checkout ? ` · ${info.checkout}` : ""}`,
        status: { label: sessionStatusLabel(session.status), cls: "text-textMuted" },
        attention: session.attention ?? (session.sessionId ? activityAttentionLabel(live) : null),
        disabled: !session.reveal,
      };
    }),
  );
  const agentLaunchers = $derived(
    sessionTypes
      .filter((type) => isHandoffCapableLauncher(type.kind))
      .map((type) => ({
        kind: type.kind,
        label: type.label,
        disabled: !repoPath || !canCreateKind(type.kind),
        title: capacityTitleFor(type.kind),
      })),
  );
  /** Agents here that want the reader: the dot on the header's Agents button. */
  const agentAttention = $derived(
    agentRowsHere.filter((row) => row.attention || row.unread).length +
      agentRowsElsewhere.filter((row) => row.attention).length,
  );
  /** The active tab is an agent, so the Agents list already highlights it. */
  const activeSessionListed = $derived(agentRowsHere.some((row) => row.active));

  function selectAgentRow(id: string) {
    selectTab(id);
    setSection("shell");
    void tick().then(() => revealTab(id));
  }
  function launchAgent(kind: SessionKind) {
    const created = newTab(kind);
    if (!created) return;
    setSection("shell");
    void tick().then(() => revealTab(created));
  }
  function goToAgentSession(key: string) {
    const session = $terminalSessions.find((candidate) => candidate.key === key);
    if (session) void goToSession(session);
  }

  $effect(() => {
    const here = repoPath;
    if (!visible || !here) {
      collisionSummary = null;
      return;
    }
    let cancelled = false;
    const poll = () => {
      void getCollisionRisk(here)
        .then((risk) => {
          if (!cancelled) collisionSummary = risk;
        })
        .catch(() => {
          if (!cancelled) collisionSummary = null;
        });
    };
    poll();
    const stopTimer = createAdaptiveTimer(poll, 15_000);
    return () => {
      cancelled = true;
      stopTimer();
    };
  });

  function oscStatusBadge(status: AgentOscStatus | null | undefined): { label: string; cls: string } | null {
    if (!status) return null;
    switch (status.state) {
      case "working":
        return { label: "Working", cls: "text-accent" };
      case "blocked":
        return { label: "Input", cls: "text-warn" };
      case "done":
        return { label: "Done", cls: "text-ok" };
      case "error":
        return { label: "Failed", cls: "text-danger" };
      case "idle":
        return { label: "Idle", cls: "text-textMuted" };
      default:
        return null;
    }
  }

  function oscAttentionLabel(status: AgentOscStatus | null | undefined): string | null {
    if (!status) return null;
    if (status.state === "blocked") return "Needs input";
    if (status.state === "error") return "Failed";
    return null;
  }
  // Reads the live limit, so raising it in Settings lets a waiting task
  // terminal open at once rather than when a session next closes.
  // Two bounds, each saying itself: live sessions (the user's limit, across
  // repositories) and this strip's tabs, ended ones included (`MAX_STRIP_TABS`).
  const stripFull = $derived(!canOpenTab(tabState, MAX_STRIP_TABS));
  const canCreate = $derived(!stripFull && $terminalSessions.length < $terminalSessionLimit);
  const capacityTitle = $derived(canCreate ? "New terminal session" : stripFull
    ? `This repository's terminal holds ${MAX_STRIP_TABS} tabs, the most it can — close ended ones to open another`
    : `All ${$terminalSessionLimit} terminal sessions are open — close one in Sessions, or raise the limit in Settings → Agents`);
  /**
   * A browser session holds a tab but no process slot, so only the strip
   * bounds it here; the backend caps open pages (`browser::MAX_PANES`) and
   * says so in the session if that is what stops it.
   */
  function canCreateKind(kind: TabKind): boolean {
    if (kind === "file") return !stripFull;
    if (kind !== "browser") return canCreate;
    return !stripFull && $hostPlatform.browser_sessions;
  }
  function capacityTitleFor(kind: TabKind): string {
    if (kind === "file") {
      return stripFull
        ? `This repository's terminal holds ${MAX_STRIP_TABS} tabs, the most it can — close ended ones to open another`
        : "Open files & Markdown beside terminal";
    }
    if (kind !== "browser") return capacityTitle;
    if (!$hostPlatform.browser_sessions) return "Browser sessions are not available on this computer yet";
    return stripFull
      ? `This repository's terminal holds ${MAX_STRIP_TABS} tabs, the most it can — close ended ones to open another`
      : "New browser session — a local dev server's page";
  }
  let focusTabStrip = false;
  const tabExtras = $derived.by(() => {
    const extras: Record<string, { status?: string; unread?: boolean; agentDetail?: string; groupLabel?: string }> = {};
    for (const tab of tabState.tabs) {
      extras[tab.id] = {
        status: tabStatuses[tab.id],
        unread: unread.has(tab.id),
        agentDetail: effectiveAgentStatuses[tab.id]?.detail ?? undefined,
        groupLabel: groupMetaByTabId[tab.id]?.label,
      };
    }
    return extras;
  });
  const filteredTabs = $derived(filterTerminalTabs(tabState.tabs, sessionQuery, tabExtras));
  const filteredSessions = $derived(filterSessionRecords($terminalSessions, sessionListQuery));
  const sessionGroups = $derived(groupSessionsByRepository(filteredSessions, repoPath, checkouts, pathOpts));
  const sessionListFlat = $derived(sessionGroups.flatMap((group) => group.sessions));
  const sessionListLive = $derived(
    $terminalSessions.reduce((count, session) => count + (sessionIsLive(session.status) ? 1 : 0), 0),
  );
  const sessionListMessage = $derived(
    sessionListNotice($terminalSessions.length, filteredSessions.length, sessionListLive),
  );
  const sessionListIndex = $derived.by(() => {
    const rows = sessionListFlat;
    if (rows.length === 0) return -1;
    if (sessionListHighlight !== null && sessionListHighlight >= 0 && sessionListHighlight < rows.length) {
      return sessionListHighlight;
    }
    const focused = rows.findIndex(
      (session) => session.key === activeId && !!repoPath && sameRepo(repoPath, session.repoPath, pathOpts),
    );
    return focused >= 0 ? focused : 0;
  });
  const filteredLaunchers = $derived(filterLaunchers(dockTabTypes, launcherQuery));
  /**
   * Compact overlay, not an in-flow chrome row. A full-width `terminal-popover`
   * shoved the PTY down by the height of the type list. The other chrome
   * bars stay in the column because they are short toolbars; this one is a
   * picker, so it uses the shared portaled popover (`ui/popover.ts`).
   */
  const launcherMenuDismissal = $derived({
    anchor: { kind: "element" as const, element: launcherTriggerEl, gap: 6 },
    estimate: { width: 240, height: 320 },
    inset: 8,
    revision: `${launcherQuery}:${filteredLaunchers.length}:${explorerSidebarOpen}`,
    dismiss: {
      inside: "[data-terminal-launcher], [data-terminal-launcher-popup]",
      resize: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => {
      launcherMenuOpen = false;
      if (reason === "escape") restoreFocusTo(launcherTriggerEl);
    },
  });

  const tabContextMenuPopover = $derived({
    anchor: {
      kind: "point" as const,
      x: tabContextMenu?.x ?? 0,
      y: tabContextMenu?.y ?? 0,
    },
    estimate: { width: 224, height: 340 },
    inset: 8,
    revision: tabContextMenu ? `${tabContextMenu.tabId}:${tabContextMenu.x}:${tabContextMenu.y}` : "",
    dismiss: {
      inside: "[data-terminal-tab-context-menu]",
      resize: true,
      scroll: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => {
      const opener = tabContextMenuOpener;
      tabContextMenu = null;
      tabContextMenuOpener = null;
      if (reason === "escape") restoreFocusTo(opener);
    },
  });

  const splitContextMenuPopover = $derived({
    anchor: {
      kind: "point" as const,
      x: splitContextMenu?.x ?? 0,
      y: splitContextMenu?.y ?? 0,
    },
    estimate: { width: 248, height: 340 },
    inset: 8,
    revision: splitContextMenu
      ? `${splitContextMenu.direction}:${splitContextMenu.position}:${splitContextMenu.x}:${splitContextMenu.y}:${splitRoot !== null}:${tabState.tabs.length}`
      : "",
    dismiss: {
      inside: "[data-terminal-split-context-menu]",
      resize: true,
      scroll: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => {
      const opener = splitContextMenuOpener;
      splitContextMenu = null;
      splitContextMenuOpener = null;
      if (reason === "escape") restoreFocusTo(opener);
    },
  });

  const groupContextMenuPopover = $derived({
    anchor: {
      kind: "point" as const,
      x: groupContextMenu?.x ?? 0,
      y: groupContextMenu?.y ?? 0,
    },
    estimate: { width: 224, height: 280 },
    inset: 8,
    revision: groupContextMenu ? `${groupContextMenu.groupId}:${groupContextMenu.x}:${groupContextMenu.y}` : "",
    dismiss: {
      inside: "[data-terminal-group-context-menu]",
      resize: true,
      scroll: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => {
      const opener = groupContextMenuOpener;
      groupContextMenu = null;
      groupContextMenuOpener = null;
      if (reason === "escape") restoreFocusTo(opener);
    },
  });

  function resolveContextMenuPoint(
    event: MouseEvent | KeyboardEvent,
    fallbackEl: HTMLElement | null,
  ): { x: number; y: number } {
    if (event instanceof MouseEvent && (event.clientX > 0 || event.clientY > 0)) {
      return { x: event.clientX, y: event.clientY };
    }
    const rect = fallbackEl?.getBoundingClientRect();
    return {
      x: Math.round((rect?.left ?? 16) + 8),
      y: Math.round((rect?.bottom ?? 16) + 4),
    };
  }

  function openTabContextMenu(
    tabId: string,
    event: MouseEvent | KeyboardEvent,
    anchorEl?: HTMLElement | null,
  ) {
    event.preventDefault();
    event.stopPropagation();
    const opener = anchorEl ?? (event.currentTarget instanceof HTMLElement ? event.currentTarget : null);
    const point = resolveContextMenuPoint(event, opener);
    closeChrome();
    tabContextMenuOpener = opener;
    tabContextMenu = { tabId, x: point.x, y: point.y };
  }

  function openSplitContextMenu(
    direction: SplitDirection,
    event: MouseEvent | KeyboardEvent,
    anchorEl?: HTMLElement | null,
  ) {
    event.preventDefault();
    event.stopPropagation();
    const opener = anchorEl ?? (event.currentTarget instanceof HTMLElement ? event.currentTarget : null);
    const point = resolveContextMenuPoint(event, opener);
    closeChrome();
    splitContextMenuOpener = opener;
    splitContextMenu = {
      x: point.x,
      y: point.y,
      direction,
      position: "after",
    };
  }

  function openGroupContextMenu(
    groupId: string,
    event: MouseEvent | KeyboardEvent,
    anchorEl?: HTMLElement | null,
  ) {
    event.preventDefault();
    event.stopPropagation();
    const opener = anchorEl ?? (event.currentTarget instanceof HTMLElement ? event.currentTarget : null);
    const point = resolveContextMenuPoint(event, opener);
    closeChrome();
    groupContextMenuOpener = opener;
    groupContextMenu = { groupId, x: point.x, y: point.y };
  }

  function handleContextMenuKeydown(event: KeyboardEvent) {
    if (
      event.key !== "ArrowDown" &&
      event.key !== "ArrowUp" &&
      event.key !== "Home" &&
      event.key !== "End"
    ) {
      return;
    }
    const items = Array.from(
      event.currentTarget instanceof HTMLElement
        ? event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]:not(:disabled)')
        : [],
    );
    if (items.length === 0) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Home") {
      items[0]?.focus();
      return;
    }
    if (event.key === "End") {
      items[items.length - 1]?.focus();
      return;
    }
    const currentIdx = items.indexOf(document.activeElement as HTMLButtonElement);
    const nextIdx =
      event.key === "ArrowDown"
        ? (currentIdx + 1) % items.length
        : (currentIdx - 1 + items.length) % items.length;
    items[nextIdx]?.focus();
  }

  function closeChrome() {
    dockMenuOpen = false;
    tabOptions = false;
    sessionListOpen = false;
    sessionSearchOpen = false;
    launcherMenuOpen = false;
    tabContextMenu = null;
    tabContextMenuOpener = null;
    splitContextMenu = null;
    splitContextMenuOpener = null;
    groupContextMenu = null;
    groupContextMenuOpener = null;
  }

  function toggleSessionList() {
    const next = !sessionListOpen;
    closeChrome();
    sessionListOpen = next;
    sessionListHighlight = null;
    if (next) void tick().then(() => sessionListEl?.focus());
  }

  function dismissSessionList() {
    sessionListOpen = false;
    restoreFocusTo(sessionListTrigger);
  }

  function revealSessionRow(node: HTMLElement, active: boolean) {
    if (active) node.scrollIntoView({ block: "nearest" });
    return {
      update(next: boolean) {
        if (next) node.scrollIntoView({ block: "nearest" });
      },
    };
  }

  function onSessionListKey(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    const command = sessionListCommand(event.key);
    if (command === "none") return;
    if (command === "up" || command === "down") {
      event.preventDefault();
      if (sessionListFlat.length === 0) return;
      const current = sessionListIndex < 0 ? 0 : sessionListIndex;
      sessionListHighlight = stepSearchIndex(sessionListFlat.length, current, command === "down" ? 1 : -1);
      return;
    }
    if (command === "escape") {
      event.preventDefault();
      event.stopPropagation();
      dismissSessionList();
      return;
    }
    const target = event.target;
    if (target instanceof HTMLElement && target.closest("[data-session-aux]")) return;
    event.preventDefault();
    const session = sessionListFlat[sessionListIndex];
    if (session?.reveal) void goToSession(session);
  }

  function openSessionSearch() {
    sessionQuery = "";
    const activeIndex = tabState.tabs.findIndex((tab) => tab.id === activeId);
    sessionHighlight = activeIndex >= 0 ? activeIndex : 0;
    closeChrome();
    sessionSearchOpen = true;
    void tick().then(() => sessionSearchInput?.focus());
  }

  function openLauncherMenu() {
    launcherQuery = "";
    launcherHighlight = Math.max(0, dockTabTypes.findIndex((launcher) => launcher.kind === nextLauncher));
    closeChrome();
    launcherMenuOpen = true;
    void tick().then(() => launcherSearchInput?.focus());
  }

  function chooseLauncher(kind: TabKind) {
    chosenLauncher = kind;
    if (kind !== "file") {
      interfaceStore.setTerminalLauncher(kind);
    }
    launcherMenuOpen = false;
  }

  function preferredPresetAgent(): LauncherKind {
    return isPromptLauncher(nextLauncher) ? nextLauncher : "claude";
  }

  function openShellPairPreset() {
    launcherMenuOpen = false;
    if (!repoPath) return;
    if (
      tabState.tabs.length + 2 > MAX_STRIP_TABS ||
      $terminalSessions.length + 2 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor("shell");
      return;
    }
    showShell();
    const dir = activeTab?.launcher === "shell" ? (activeTab.startDir ?? null) : null;
    const afterFirst = openTab(tabState, "shell");
    const firstId = afterFirst.activeId;
    const afterSecond = openTab(afterFirst, "shell");
    const secondId = afterSecond.activeId;
    if (!firstId || !secondId || firstId === secondId) return;
    const nextState = dir
      ? setTabStartDir(setTabStartDir(afterSecond, firstId, dir), secondId, dir)
      : afterSecond;
    focusTabStrip = false;
    zoomedPaneId = null;
    const tree = splitPane(null, firstId, secondId, "row", "after", 0.5);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, tree, { activePaneId: firstId }),
      activateTab(nextState, firstId),
    );
  }

  function openAgentShellPairPreset() {
    launcherMenuOpen = false;
    if (!repoPath) return;
    const agentKind = preferredPresetAgent();
    if (
      tabState.tabs.length + 2 > MAX_STRIP_TABS ||
      $terminalSessions.length + 2 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor(agentKind);
      return;
    }
    showShell();
    const dir = activeTab?.launcher === "shell" ? (activeTab.startDir ?? null) : null;
    const afterAgent = openTab(tabState, agentKind);
    const agentId = afterAgent.activeId;
    const afterShell = openTab(afterAgent, "shell");
    const shellId = afterShell.activeId;
    if (!agentId || !shellId || agentId === shellId) return;
    const nextState = dir ? setTabStartDir(afterShell, shellId, dir) : afterShell;
    focusTabStrip = false;
    zoomedPaneId = null;
    const tree = splitPane(null, agentId, shellId, "row", "after", 0.5);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, tree, { activePaneId: agentId }),
      activateTab(nextState, agentId),
    );
  }

  function openAgentDiffPreset() {
    launcherMenuOpen = false;
    if (!repoPath) return;
    const agentKind = preferredPresetAgent();
    if (
      tabState.tabs.length + 2 > MAX_STRIP_TABS ||
      $terminalSessions.length + 1 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor(agentKind);
      return;
    }
    showShell();
    const afterAgent = openTab(tabState, agentKind);
    const agentId = afterAgent.activeId;
    if (!agentId) return;
    const afterFile = openFileTab(afterAgent, { reuseExisting: false, focus: false });
    const fileId = afterFile.id;
    if (!fileId || fileId === agentId) return;
    focusTabStrip = false;
    zoomedPaneId = null;
    startFollowing(fileId);
    const tree = splitPane(null, agentId, fileId, "row", "after", 0.5);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, tree, { activePaneId: agentId }),
      activateTab(afterFile.state, agentId),
    );
  }

  function openPairSplit() {
    launcherMenuOpen = false;
    if (!repoPath) return;
    if (
      tabState.tabs.length + 2 > MAX_STRIP_TABS ||
      $terminalSessions.length + 2 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor("claude");
      return;
    }
    showShell();
    const afterFirst = openTab(tabState, "claude");
    const firstId = afterFirst.activeId;
    const afterSecond = openTab(afterFirst, "agy");
    const secondId = afterSecond.activeId;
    if (!firstId || !secondId || firstId === secondId) return;
    focusTabStrip = false;
    zoomedPaneId = null;
    const tree = splitPane(null, firstId, secondId, "row", "after", 0.5);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, tree, { activePaneId: firstId }),
      activateTab(afterSecond, firstId),
    );
  }

  function openFullStackPreset() {
    launcherMenuOpen = false;
    if (!repoPath) return;
    const agentKind = preferredPresetAgent();
    const withBrowser = $hostPlatform.browser_sessions;
    const neededTabs = withBrowser ? 3 : 2;
    if (
      tabState.tabs.length + neededTabs > MAX_STRIP_TABS ||
      $terminalSessions.length + 2 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor(agentKind);
      return;
    }
    showShell();
    const afterAgent = openTab(tabState, agentKind);
    const agentId = afterAgent.activeId;
    const afterShell = openTab(afterAgent, "shell");
    const shellId = afterShell.activeId;
    if (!agentId || !shellId || agentId === shellId) return;
    let nextState = afterShell;
    let tree = splitPane(null, agentId, shellId, "row", "after", 0.5);
    if (withBrowser) {
      const openedBrowser = openBrowserTab(nextState, undefined);
      if (openedBrowser.id && openedBrowser.id !== shellId) {
        nextState = openedBrowser.state;
        tree = splitPane(tree, shellId, openedBrowser.id, "col", "after", 0.5);
      }
    }
    focusTabStrip = false;
    zoomedPaneId = null;
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, tree, { activePaneId: agentId }),
      activateTab(nextState, agentId),
    );
  }

  let creatingWorktreePreset = $state(false);

  async function openWorktreeAgentPreset() {
    launcherMenuOpen = false;
    const baseRepo = repoPath;
    if (!baseRepo || !onCreateAgentWorktree || creatingWorktreePreset) return;
    const agentKind = preferredPresetAgent();
    if (
      tabState.tabs.length + 2 > MAX_STRIP_TABS ||
      $terminalSessions.length + 1 > $terminalSessionLimit
    ) {
      sessionError = capacityTitleFor(agentKind);
      return;
    }
    const rawSlug = await askText({
      title: "New Isolated Agent Worktree",
      message:
        "Creates a sibling worktree (<repo>.wt-<slug>) on branch wt/<slug> and opens an agent + live file split inside it.",
      placeholder: "agent-1",
      initialValue: "agent-1",
      confirmLabel: "Create & Open",
      validate: (value) =>
        sanitizeWorktreeSlug(value)
          ? null
          : "Enter a valid slug using letters, numbers, dots, underscores, or hyphens.",
    });
    if (rawSlug === null) return;
    const slug = sanitizeWorktreeSlug(rawSlug);
    if (!slug || !repoPath || !sameRepo(repoPath, baseRepo, pathOpts)) return;

    const trimmedRepo = baseRepo.replace(/[\\/]+$/, "");
    const sep = trimmedRepo.includes("\\") ? "\\" : "/";
    const lastSep = Math.max(trimmedRepo.lastIndexOf("/"), trimmedRepo.lastIndexOf("\\"));
    if (lastSep <= 0) {
      sessionError = "Couldn't resolve the parent directory for the sibling worktree.";
      return;
    }
    const parentDir = trimmedRepo.slice(0, lastSep);
    const repoBase = trimmedRepo.slice(lastSep + 1);
    const targetPath = `${parentDir}${sep}${repoBase}.wt-${slug}`;
    const newBranch = `wt/${slug}`;

    creatingWorktreePreset = true;
    try {
      const outcome = await onCreateAgentWorktree(targetPath, newBranch);
      if (!outcome.ok) {
        if (outcome.error) {
          if (visible && repoPath && sameRepo(repoPath, baseRepo, pathOpts)) {
            sessionError = `Couldn't create the isolated worktree: ${outcome.error}`;
          } else {
            toastStore.error(`Couldn't create the isolated worktree: ${outcome.error}`);
          }
        }
        return;
      }
      if (!repoPath || !sameRepo(repoPath, baseRepo, pathOpts)) return;

      const checkoutPath = outcome.path;
      showShell();
      const openedAgent = openCheckoutTab(tabState, agentKind, {
        checkout: checkoutPath,
        name: `${launcherLabel(agentKind)} · ${slug}`,
      });
      if (!openedAgent.id) {
        sessionError = capacityTitleFor(agentKind);
        return;
      }
      const openedFile = openFileTab(openedAgent.state, {
        checkout: checkoutPath,
        reuseExisting: false,
        focus: false,
      });
      focusTabStrip = false;
      zoomedPaneId = null;
      startFollowing(openedFile.id && openedFile.id !== openedAgent.id ? openedFile.id : null);
      const focusedAgent = activateTab(openedFile.state, openedAgent.id);
      if (openedFile.id && openedFile.id !== openedAgent.id) {
        const tree = splitPane(null, openedAgent.id, openedFile.id, "row", "after", 0.5);
        applyGroupsAndCoLocate(
          upsertSplitGroup(splitGroups, tree, {
            name: `${launcherLabel(agentKind)} · ${slug}`,
            activePaneId: openedAgent.id,
          }),
          focusedAgent,
        );
      } else {
        tabState = focusedAgent;
      }
    } catch (err) {
      sessionError = `Couldn't create the isolated worktree: ${describeError(err, "terminal")}`;
    } finally {
      creatingWorktreePreset = false;
    }
  }

  function chooseSearchedTab(id: string) {
    sessionSearchOpen = false;
    selectTab(id);
  }

  function handleSessionSearchKey(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      sessionHighlight = stepSearchIndex(filteredTabs.length, sessionHighlight, event.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const tab = filteredTabs[sessionHighlight] ?? filteredTabs[0];
      if (tab) chooseSearchedTab(tab.id);
    }
  }

  function handleLauncherSearchKey(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      launcherMenuOpen = false;
      restoreFocusTo(launcherTriggerEl);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      launcherHighlight = stepSearchIndex(filteredLaunchers.length, launcherHighlight, event.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const launcher = filteredLaunchers[launcherHighlight] ?? filteredLaunchers[0];
      if (launcher) chooseLauncher(launcher.kind);
    }
  }

  $effect(() => {
    const length = filteredTabs.length;
    untrack(() => {
      if (length <= 0) sessionHighlight = 0;
      else if (sessionHighlight >= length) sessionHighlight = length - 1;
    });
  });

  $effect(() => {
    const length = filteredLaunchers.length;
    untrack(() => {
      if (length <= 0) launcherHighlight = 0;
      else if (launcherHighlight >= length) launcherHighlight = length - 1;
    });
  });

  $effect(() => {
    const scroller = tabScroller;
    if (!scroller) return;
    return bindHorizontalWheel(scroller);
  });

  /**
   * A task terminal (or resumed conversation, or session to show again) that
   * belongs in this dock but cannot open, because every session slot is in
   * use. It waits rather than failing — and used to wait in silence, so the
   * agent seemed never to have started.
   */
  const waitingForCapacity = $derived.by(() => {
    const request = requestFor($taskTerminalRequests, repoPath, pathOpts, checkouts);
    return request && !canCreate && !tabFor(tabState, request) ? request : null;
  });

  const repoLedgerOffers = $derived(filterOffersForRepo($sessionLedgerStore.offers, repoPath, pathOpts));
  const repoReopenableOffers = $derived(reopenableOffers(repoLedgerOffers));
  let reopeningLedgerRowId = $state<string | null>(null);
  let reopeningAllLedger = $state(false);
  let ledgerRowErrors = $state<Record<string, string>>({});

  async function handleReopenLedgerRow(offer: (typeof repoLedgerOffers)[number]) {
    reopeningLedgerRowId = offer.row_id;
    try {
      const outcome = await reopenSessionLedgerOffer(offer);
      if (!outcome.reopened) {
        ledgerRowErrors = { ...ledgerRowErrors, [offer.row_id]: outcome.error };
      } else if (offer.row_id in ledgerRowErrors) {
        const next = { ...ledgerRowErrors };
        delete next[offer.row_id];
        ledgerRowErrors = next;
      }
    } finally {
      reopeningLedgerRowId = null;
    }
  }

  async function handleReopenAllLedgerRows() {
    reopeningAllLedger = true;
    try {
      const summary = await reopenAllSessionLedgerOffers(repoLedgerOffers);
      const nextErrors = { ...ledgerRowErrors };
      for (const item of summary.results) {
        if (!item.reopened) nextErrors[item.offer.row_id] = item.error;
        else delete nextErrors[item.offer.row_id];
      }
      ledgerRowErrors = nextErrors;
    } finally {
      reopeningAllLedger = false;
    }
  }

  async function handleDismissLedgerRow(rowId: string) {
    if (rowId in ledgerRowErrors) {
      const next = { ...ledgerRowErrors };
      delete next[rowId];
      ledgerRowErrors = next;
    }
    try {
      await dismissSessionLedgerRow(rowId);
    } catch (error) {
      ledgerRowErrors = { ...ledgerRowErrors, [rowId]: describeError(error, "terminal") };
    }
  }

  /**
   * Tabs opened for a queued request that do not hold a session slot yet.
   *
   * A slot is reserved when the process starts, after this panel — and every
   * other repository's panel — has already judged one free. Consuming the
   * request when its tab opened lost it whenever starts raced for the last
   * slots: the losers showed "All N terminal sessions are in use", and
   * nothing retried them when slots freed. So a request stays queued until
   * its tab holds a slot (`admitTab`), and a tab refused one is closed and
   * its request left to wait (`refuseTab`).
   */
  const admitting = new Set<string>();
  /** Bumped on a refusal, so the opener runs again once a slot frees. */
  let refusals = $state(0);

  /** This panel, as `taskLaunches` holds requests by (`holdTaskTerminal`). */
  const holder = Symbol("terminal-panel");

  $effect(() => {
    const here = repoPath;
    const held = $taskTerminalHolds;
    const hosted = here
      ? $taskTerminalRequests.filter((request) => !heldElsewhere(request, holder, held) && requestFor([request], here, pathOpts, checkouts))
      : [];
    const limit = $terminalSessionLimit;
    const used = $terminalSessions.length;
    void refusals;
    if (!here || !hosted.length) return;
    untrack(() => {
      let room = Math.min(limit - used, MAX_STRIP_TABS - tabState.tabs.length) - admitting.size;
      for (const request of hosted) {
        const tab = tabFor(tabState, request);
        if (tab && admitting.has(tab.id)) continue;
        if (tab) {
          // Already this panel's: bring it forward, as a reveal asks.
          tabState = openTab(tabState, request.provider, request);
          showShell();
          consumeTaskTerminalRequest(request);
          continue;
        }
        if (room <= 0) break;
        const next = openTab(tabState, request.provider, launchFor(request, here, pathOpts));
        const opened = next.tabs.find((candidate) => !tabState.tabs.includes(candidate));
        if (!opened) break;
        if (!holdTaskTerminal(request, holder)) continue;
        tabState = next;
        showShell();
        admitting.add(opened.id);
        room -= 1;
      }
    });
  });

  /** The queued request a tab of this panel was opened for, if it still stands. */
  function requestOfTab(id: string): TaskTerminalRequest | undefined {
    const tab = tabState.tabs.find((candidate) => candidate.id === id);
    if (!tab) return undefined;
    const one = { tabs: [tab], activeId: null };
    return get(taskTerminalRequests).find((candidate) => tabFor(one, candidate));
  }

  function admitTab(id: string) {
    if (!admitting.has(id)) return;
    const request = requestOfTab(id);
    admitting.delete(id);
    if (request) consumeTaskTerminalRequest(request);
  }

  function refuseTab(id: string): boolean {
    if (!admitting.has(id)) return false;
    // After the session's own start returns: it is still inside that call.
    // Still marked until it is gone, or an opener run in between would take
    // the refused tab for a running one and consume its request. The request
    // stays queued for the next free slot.
    queueMicrotask(() => { dropTab(id, true); refusals += 1; });
    return true;
  }

  $effect(() => {
    const queued = $consoleLaunchRequests[0];
    if (!queued || !visible || !repoPath) return;
    if (running) return;
    untrack(() => {
      const claimed = consumeConsoleLaunch();
      if (!claimed) return;
      setSection("console");
      void execute(claimed.command, claimed.timeoutSecs ?? 1200);
    });
  });

  /**
   * Jumps to a session in the Sessions list, in this repository or another.
   *
   * Switching repositories is the DOCK's job, not this panel's: a panel is
   * bound to the one path it was handed and must never reach for the live
   * `currentPath` — a hidden one that did would render and journal against
   * whichever repository the user switched to while its PTY sat in the
   * worktree it was spawned in. So the capability arrives as a prop and this
   * only reports failure where the user is looking.
   */
  /**
   * The back link: the task this session is an attempt of. Read from the
   * run, which is the one record that names it, so a session adopted after a
   * reload — which knows only its run — links the same way a tab does.
   */
  async function openListedTask(runId: string | null) {
    if (!runId) return;
    sessionListOpen = false;
    try {
      await openTaskForRun(runId);
    } catch (cause) {
      sessionError = `That session's task couldn't be opened: ${describeError(cause, "terminal")}`;
    }
  }

  async function goToSession(session: TerminalSessionRecord) {
    sessionListOpen = false;
    if (!onGoToSession) {
      sessionError = "Jumping between sessions is unavailable in this window.";
      return;
    }
    const outcome = await onGoToSession(session);
    if (!outcome.ok) {
      sessionError = outcome.reason === "unavailable"
        ? "That session couldn't be shown. It may have just exited; the Sessions list refreshes when it does."
        : "That session is no longer running.";
    }
  }

  function newTab(
    launcher: SessionKind,
    initialPrompt?: string,
    autoWake = false,
    resume?: ResumeLaunch,
    customName?: string,
    purpose?: "commit",
  ): string | null {
    if (!repoPath || !canCreateKind(launcher)) return null;
    focusTabStrip = false;
    const openedState = openTab(tabState, launcher, initialPrompt);
    const created = openedState.tabs.find((tab) => !tabState.tabs.includes(tab));
    if (!created) return null;
    tabState = openedState;
    if (autoWake) tabState = setTabAutoWake(tabState, created.id, true);
    if (purpose) tabState = setTabPurpose(tabState, created.id, purpose);
    if (resume) tabState = setTabResume(tabState, created.id, resume, customName);
    return created.id;
  }

  /**
   * A new tab from the toolbar, the chord, or a split. A shell opens where the
   * shell beside it is working, as every terminal does — but only inside this
   * repository, which is the only place a session may start. An agent CLI
   * treats its working directory as the project root, so it keeps the root.
   */
  async function openBeside(launcher: TabKind): Promise<boolean> {
    if (!repoPath || !canCreateKind(launcher)) return false;
    if (launcher === "file") {
      focusTabStrip = false;
      const opened = openFileTab(tabState, { reuseExisting: false, focus: true });
      if (!opened.id) return false;
      tabState = opened.state;
      return true;
    }
    const fromId = tabState.activeId;
    // An unreadable context offers no directory, so the tab starts at the root.
    const read = launcher === "shell" && fromId ? await sessions[fromId]?.readContext() : undefined;
    const dir = startDirFrom(read ?? CONTEXT_UNKNOWN);
    const createdId = newTab(launcher);
    if (!createdId) return false;
    if (dir) tabState = setTabStartDir(tabState, createdId, dir);
    return true;
  }

  /** The Sessions list's close: the panel's question first, then stop the process. */
  async function closeListed(session: TerminalSessionRecord) {
    try {
      await closeWithConfirmation(session);
    } catch (error: unknown) {
      sessionError = describeError(error, "terminal");
    }
  }

  /** Ids with a close question already on screen, so a second press does not ask twice. */
  const closing = new Set<string>();

  /**
   * Close, asking first when a job is running in the foreground or the tab's
   * own process is a running agent (see `closeQuestion`). A shell whose
   * foreground cannot be read asks too, and says it could not tell: an
   * unknown is never read as idle.
   *
   * The tab's × and the chord come through here; the Sessions list asks the
   * same question through the registry's `confirmClose`, so a session in
   * another repository's dock asks too.
   */
  async function requestClose(id: string) {
    if (closing.has(id)) return;
    closing.add(id);
    try {
      if (!(await confirmClose(id))) return;
      // Close ends the process here, explicitly. Dropping the tab only
      // unmounts it, and an unmount keeps whatever is still running.
      try {
        await (sessions[id] ?? browsers[id] ?? filePanes[id])?.close();
      } catch (error: unknown) {
        sessionError = describeError(error, "terminal");
        return;
      }
      if (tabState.tabs.some((candidate) => candidate.id === id)) dropTab(id);
    } finally {
      closing.delete(id);
    }
  }

  /** Whether closing this tab may go ahead: asked when it would interrupt something. */
  async function confirmClose(id: string): Promise<boolean> {
    const tab = tabState.tabs.find((candidate) => candidate.id === id);
    if (!tab) return false;
    // A browser session runs no job to interrupt; its page closes with it.
    if (tab.launcher === "browser") return true;
    if (tab.launcher === "file") return (await filePanes[id]?.confirmClose()) ?? true;
    const status = tabStatuses[id];
    const agent = tab.launcher === "shell" ? null : {
      name: launcherLabel(tab.launcher),
      running: status === "starting" || status === "running",
      taskAttempt: Boolean(tab.taskRunId),
    };
    // A tab with no mounted session to ask is unknown, not idle.
    const question = closeQuestion((await sessions[id]?.readContext()) ?? CONTEXT_UNKNOWN, tabLabel(tab), agent);
    return !question || (await askConfirm({ ...question, confirmLabel: "Close", destructive: true }));
  }

  $effect(() => {
    const request = $terminalLaunchRequests;
    const here = repoPath;
    // By checkout identity, as `requestFor` matches task launches: the caller
    // may hold the same checkout spelled differently from this panel's tab.
    if (!request || !visible || !here || !sameRepo(request.repoPath, here, pathOpts)) return;
    untrack(() => {
      const claimed = terminalLaunchRequests.take(here, pathOpts);
      if (!claimed) return;
      showShell();
      if (claimed.launcher === "browser") {
        claimed.complete(openBrowserFor(claimed.url));
        return;
      }
      if (claimed.launcher === "file") {
        const err =
          claimed.fileOpenMode === "tab"
            ? openFileTabFor(claimed.filePath, claimed.line ?? null, claimed.column ?? null, {
                focus: claimed.fileFocus ?? true,
              })
            : openFileSplitFor(claimed.filePath, claimed.line ?? null, claimed.column ?? null, {
                focus: claimed.fileFocus ?? false,
              });
        if (!err && claimed.fileKeep && claimed.filePath) {
          keepFileShown(claimed.filePath, activeTab?.checkout);
        }
        claimed.complete(err);
        return;
      }
      const createdId = newTab(
        claimed.launcher,
        claimed.prompt,
        claimed.autoWake === true,
        claimed.resume,
        claimed.name,
        claimed.purpose,
      );
      // Only a prompt-carrying launch is an AGENT session; a plain shell
      // started from the palette must not appear in the agent list that
      // Coverage's "View agent session" reads.
      const agent = isPromptLauncher(claimed.launcher) ? claimed.launcher : null;
      if (createdId && agent && (claimed.prompt !== undefined || claimed.resume !== undefined)) terminalLaunchRequests.remember({
        id: createdId, repoPath: claimed.repoPath, launcher: agent, status: "starting",
        reveal() {
          showShell();
          selectTab(createdId);
          void tick().then(() => revealTab(createdId));
        },
      });
      claimed.complete(createdId ? undefined : capacityTitleFor(claimed.launcher));
    });
  });

  onDestroy(() => {
    flushPendingSplitPersist();
    for (const tab of tabState.tabs) {
      terminalLaunchRequests.forget(tab.id);
      forgetEndedTail(tab.id);
      scrollbackSnapshots.forget(tab.id);
    }
    if (repoPath) panelTabCounts.forget(repoPath, pathOpts);
    releaseTaskTerminal(holder);
  });

  function openTabInSplit(
    id: string,
    direction: SplitDirection = "row",
    position: "before" | "after" = "after",
  ) {
    if (!tabState.tabs.some((tab) => tab.id === id)) return;
    focusTabStrip = false;
    const anchor = activeId && activeId !== id ? activeId : tabState.tabs.find((t) => t.id !== id)?.id ?? null;
    const anchorGroup = anchor ? findGroupForPane(splitGroups, anchor) : null;
    if (zoomedPaneId) zoomedPaneId = null;

    let baseGroups = removePaneFromGroups(splitGroups, id);
    const refreshedAnchorGroup = anchor ? findGroupForPane(baseGroups, anchor) : null;
    const baseTree = refreshedAnchorGroup?.tree ?? null;

    if (baseTree && anchor) {
      if (hasPane(baseTree, id)) {
        applyGroupsAndCoLocate(baseGroups, activateTab(tabState, id));
        return;
      }
      if (leafPaneIds(baseTree).length < MAX_SPLIT_PANES) {
        const target = hasPane(baseTree, anchor)
          ? anchor
          : leafPaneIds(baseTree).at(-1) ?? anchor;
        const nextTree = splitPane(baseTree, target, id, direction, position, 0.5);
        applyGroupsAndCoLocate(
          upsertSplitGroup(baseGroups, nextTree, {
            groupId: refreshedAnchorGroup?.id,
            name: refreshedAnchorGroup?.name,
            activePaneId: id,
          }),
          activateTab(tabState, id),
        );
        return;
      }
      const choice = chooseSplitReplacementSlot(baseTree, anchor, pinnedTabIds);
      if (choice?.mode === "replace") {
        const nextTree = replacePane(baseTree, choice.targetPaneId, id);
        applyGroupsAndCoLocate(
          upsertSplitGroup(baseGroups, nextTree, {
            groupId: refreshedAnchorGroup?.id,
            name: refreshedAnchorGroup?.name,
            activePaneId: id,
          }),
          activateTab(tabState, id),
        );
        return;
      }
      sessionError = "All split panes are pinned at the 8-pane limit — unpin or remove a pane first.";
      return;
    }

    if (anchor) {
      const nextTree = splitPane(null, anchor, id, direction, position, 0.5);
      applyGroupsAndCoLocate(
        upsertSplitGroup(baseGroups, nextTree, {
          groupId: anchorGroup?.id,
          name: anchorGroup?.name,
          activePaneId: id,
        }),
        activateTab(tabState, id),
      );
      return;
    }

    tabState = activateTab(tabState, id);
  }

  function selectTab(id: string, keepStripFocus = false) {
    focusTabStrip = keepStripFocus;
    if (zoomedPaneId && zoomedPaneId !== id) zoomedPaneId = null;
    tabState = activateTab(tabState, id);
    const updatedGroups = recordGroupActivePane(splitGroups, id);
    if (updatedGroups !== splitGroups) splitGroups = updatedGroups;
  }

  /**
   * Drops the tab. A Close has already ended its process (`requestClose`);
   * any other teardown leaves a running one detached and listed in Sessions
   * (`sessionLifecycle` `dispose`). Emptying the
   * strip is allowed and leaves the empty state, which offers a new tab — a
   * terminal that silently respawns what you just closed is worse than one
   * that waits to be asked.
   */
  function dropTab(id: string, keepRequest = false) {
    terminalLaunchRequests.forget(id);
    forgetEndedTail(id);
    scrollbackSnapshots.forget(id);
    // A tab closed before it took a slot no longer counts against the room
    // this panel leaves for queued starts, and gives up its hold. Closed by
    // the reader, its request goes too — it was still queued, and would
    // otherwise reopen the tab they just closed. Refused a slot, it waits.
    if (admitting.has(id)) {
      const request = requestOfTab(id);
      if (request) {
        if (keepRequest) releaseTaskTerminal(holder, request);
        else consumeTaskTerminalRequest(request);
      }
      admitting.delete(id);
    }
    focusTabStrip = false;
    if (zoomedPaneId === id) zoomedPaneId = null;
    if (inlineRenameTabId === id) {
      inlineRenameTabId = null;
      inlineRenameSurface = null;
    }
    const ownerGroup = findGroupForPane(splitGroups, id);
    const remainingSplitLeaves = ownerGroup
      ? leafPaneIds(ownerGroup.tree).filter((other) => other !== id)
      : [];
    const partner = remainingSplitLeaves[0] ?? null;
    if (ownerGroup && remainingSplitLeaves.length < 2 && inlineRenameGroupId === ownerGroup.id) {
      inlineRenameGroupId = null;
    }
    splitGroups = removePaneFromGroups(splitGroups, id);
    tabState = closeTab(tabState, id, partner);
    const { [id]: _status, ...statuses } = tabStatuses;
    tabStatuses = statuses;
    const { [id]: _osc, ...oscStatuses } = tabAgentStatuses;
    tabAgentStatuses = oscStatuses;
    const { [id]: _bstate, ...bstates } = tabBrowserStates;
    tabBrowserStates = bstates;
    unread = new Set([...unread].filter((key) => key !== id));
    const { [id]: _gone, ...rest } = sessions;
    sessions = rest;
    const { [id]: _page, ...pages } = browsers;
    browsers = pages;
    const { [id]: _file, ...files } = filePanes;
    filePanes = files;
  }

  /**
   * A browser session on `url` (an agent's request, a clicked localhost link):
   * the tab already on that address is brought forward and loads it again —
   * a request repeated after an edit wants the change — otherwise a tab opens
   * on it. Returns why nothing opened, or undefined.
   */
  function openBrowserFor(url: string | undefined): string | undefined {
    if (!repoPath) return "Open a repository first";
    const existing = url ? browserTabFor(tabState, url) : undefined;
    if (existing) {
      selectTab(existing.id);
      if (url) void tick().then(() => browsers[existing.id]?.show(url));
      return undefined;
    }
    if (!canCreateKind("browser")) return capacityTitleFor("browser");
    const prevActive = tabState.activeId;
    const opened = openBrowserTab(tabState, url);
    if (!opened.id) return capacityTitleFor("browser");
    focusTabStrip = false;
    const anchorGroup = prevActive ? findGroupForPane(splitGroups, prevActive) : null;
    const baseTree = anchorGroup?.tree ?? null;
    if (prevActive && opened.id !== prevActive && leafPaneIds(baseTree).length < MAX_SPLIT_PANES) {
      zoomedPaneId = null;
      const target = baseTree
        ? (hasPane(baseTree, prevActive) ? prevActive : leafPaneIds(baseTree)[0])
        : prevActive;
      const nextTree = target ? splitPane(baseTree, target, opened.id, "row", "after", 0.5) : null;
      upsertGroupForAnchor(prevActive, nextTree, opened.state, opened.id);
    } else if (baseTree && prevActive && !hasPane(baseTree, opened.id)) {
      const choice = chooseSplitReplacementSlot(baseTree, prevActive, pinnedTabIds);
      if (choice?.mode === "replace") {
        const nextTree = replacePane(baseTree, choice.targetPaneId, opened.id);
        upsertGroupForAnchor(prevActive, nextTree, opened.state, opened.id);
      } else if (choice?.mode === "blocked") {
        const preserved = hasPane(baseTree, prevActive)
          ? activateTab(opened.state, prevActive)
          : opened.state;
        tabState = preserved;
        sessionError = "Opened browser tab in the background because all 8 split panes are pinned — unpin or remove a pane to show it.";
      } else {
        tabState = opened.state;
      }
    } else {
      tabState = opened.state;
    }
    return undefined;
  }

  /**
   * Opens or reuses a split `TerminalFilePane` beside the current terminal.
   * Returns an error message when refused, or `undefined` when shown.
   */
  function openFileSplitFor(
    filePath?: string,
    line: number | null = null,
    column: number | null = null,
    options?: {
      focus?: boolean;
      anchorId?: string;
      checkout?: string;
      fallbackPath?: string;
      direction?: SplitDirection;
      position?: "before" | "after";
      /** Reuse only a pane already showing `filePath`, never repoint another file pane (Follow Agent). */
      reuseSameFileOnly?: boolean;
    },
  ): string | undefined {
    if (!repoPath) return "Open a repository first";
    const prevActive = tabState.activeId;
    const anchorId = options?.anchorId ?? prevActive;
    const direction = options?.direction ?? "row";
    const position = options?.position ?? "after";
    const shouldFocus = options?.focus ?? false;
    const anchorGroup = anchorId ? findGroupForPane(splitGroups, anchorId) : null;
    const cleanCheckout = options?.checkout?.trim() || undefined;
    const groupFileTab = anchorGroup
      ? fileTabFor(
          {
            tabs: tabState.tabs.filter((t) => hasPane(anchorGroup.tree, t.id)),
            activeId: anchorId,
          },
          filePath,
          cleanCheckout,
        )
      : undefined;
    const ungroupedFileTab = !groupFileTab
      ? fileTabFor(
          {
            tabs: tabState.tabs.filter((t) => !groupedTabIds.has(t.id)),
            activeId: anchorId,
          },
          filePath,
          cleanCheckout,
        )
      : undefined;
    const anyFileTab = groupFileTab ?? ungroupedFileTab;
    const reuseCandidate =
      options?.reuseSameFileOnly && anyFileTab?.filePath !== normalizePaneFilePath(filePath)
        ? undefined
        : anyFileTab;
    if (!reuseCandidate && !canCreateKind("file")) return capacityTitleFor("file");
    const result = reuseCandidate
      ? (() => {
          const updated = setTabFile(
            tabState,
            reuseCandidate.id,
            filePath ?? reuseCandidate.filePath,
            line,
            column,
            { checkout: cleanCheckout, fallbackPath: options?.fallbackPath },
          );
          return {
            state: shouldFocus ? activateTab(updated, reuseCandidate.id) : updated,
            id: reuseCandidate.id,
          };
        })()
      : openFileTab(tabState, {
          filePath,
          fallbackPath: options?.fallbackPath,
          checkout: cleanCheckout,
          line,
          column,
          reuseExisting: false,
          focus: shouldFocus,
        });
    if (!result.id) return capacityTitleFor("file");
    focusTabStrip = false;
    const baseTree = anchorGroup?.tree ?? null;
    const targetActive = shouldFocus ? result.id : prevActive;
    if (anchorId && result.id !== anchorId && !hasPane(baseTree, result.id)) {
      const splitTarget = baseTree
        ? (hasPane(baseTree, anchorId) ? anchorId : leafPaneIds(baseTree)[0])
        : anchorId;
      if (splitTarget && leafPaneIds(baseTree).length < MAX_SPLIT_PANES) {
        if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
        const nextTree = splitPane(baseTree, splitTarget, result.id, direction, position, 0.5);
        upsertGroupForAnchor(anchorId, nextTree, result.state, targetActive);
      } else if (baseTree && splitTarget) {
        const choice = chooseSplitReplacementSlot(baseTree, splitTarget, pinnedTabIds);
        if (choice?.mode === "replace") {
          if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
          const nextTree = replacePane(baseTree, choice.targetPaneId, result.id);
          upsertGroupForAnchor(anchorId, nextTree, result.state, targetActive);
        } else if (choice?.mode === "blocked") {
          tabState =
            shouldFocus && prevActive && hasPane(baseTree, prevActive)
              ? activateTab(result.state, prevActive)
              : result.state;
          sessionError = "Opened file tab in the background because all 8 split panes are pinned — unpin or remove a pane to show it.";
        } else {
          tabState = result.state;
        }
      } else {
        tabState = result.state;
      }
    } else {
      if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
      if (
        options?.direction &&
        anchorId &&
        result.id !== anchorId &&
        baseTree &&
        hasPane(baseTree, result.id)
      ) {
        const nextTree = movePaneToSplit(baseTree, result.id, anchorId, direction, position);
        upsertGroupForAnchor(anchorId, nextTree, result.state, targetActive);
      } else {
        tabState = result.state;
      }
    }
    return undefined;
  }

  function tabIdForBackendSession(backendSessionId: string): string | null {
    if (!backendSessionId) return null;
    for (const [id, inst] of Object.entries(sessions)) {
      if (inst?.backendSessionId?.() === backendSessionId) return id;
    }
    const byAttach = tabState.tabs.find((tab) => tab.attachSessionId === backendSessionId);
    if (byAttach) return byAttach.id;
    const reg = sessionByNativeId(get(terminalSessions), backendSessionId);
    if (reg && tabState.tabs.some((tab) => tab.id === reg.key)) return reg.key;
    return null;
  }

  function handleChildSpawned(payload: TerminalChildSpawnedPayload) {
    if (!repoPath || !sameRepo(payload.repo_path, repoPath, pathOpts)) return;
    const startDir = relativeToRepo(payload.cwd, repoPath);
    const parentTabId = tabIdForBackendSession(payload.parent_session_id) ?? tabState.activeId;
    const result = openAttachedChildTab(tabState, {
      sessionId: payload.id,
      parentSessionId: payload.parent_session_id,
      launcher: payload.launcher,
      title: payload.title,
      startDir: startDir && startDir !== "." ? startDir : null,
      focus: false,
    });
    if (!result.id) {
      sessionError = capacityTitleFor("shell");
      return;
    }
    focusTabStrip = false;
    showShell();
    const parentGroup = parentTabId ? findGroupForPane(splitGroups, parentTabId) : null;
    const parentTree = parentGroup?.tree ?? null;
    if (payload.split && parentTabId && parentTabId !== result.id && !hasPane(parentTree, result.id)) {
      const splitTarget = parentTree
        ? (hasPane(parentTree, parentTabId) ? parentTabId : leafPaneIds(parentTree)[0])
        : parentTabId;
      if (splitTarget && leafPaneIds(parentTree).length < MAX_SPLIT_PANES) {
        if (zoomedPaneId && zoomedPaneId !== parentTabId) zoomedPaneId = null;
        const nextTree = splitPane(parentTree, splitTarget, result.id, "row", "after", 0.5);
        upsertGroupForAnchor(parentTabId, nextTree, result.state, result.state.activeId);
        return;
      }
    }
    tabState = result.state;
  }

  function handleChildClosed(payload: TerminalChildClosedPayload) {
    if (!repoPath || !sameRepo(payload.repo_path, repoPath, pathOpts)) return;
    const childTabId = tabIdForBackendSession(payload.id);
    if (!childTabId) return;
    if (tabState.tabs.some((tab) => tab.id === childTabId)) {
      dropTab(childTabId);
    }
  }

  async function stopActiveChildren() {
    const targets = [...activeChildTabs];
    for (const child of targets) {
      try {
        await sessions[child.id]?.close();
      } catch (error: unknown) {
        sessionError = describeError(error, "terminal");
        continue;
      }
      if (tabState.tabs.some((candidate) => candidate.id === child.id)) {
        dropTab(child.id);
      }
    }
  }

  function handOffToPairPartner(target: PairHandoffTarget | null = pairHandoffTarget) {
    const partner = target;
    if (!activeTab || !partner) return;
    const visibleFileTab = tabState.tabs.find(
      (tab) => tab.launcher === "file" && visibleIds.includes(tab.id) && tab.filePath,
    );
    const tail =
      sessions[activeTab.id]?.retainedOutput() ||
      sessions[activeTab.id]?.serializeVisualOutput() ||
      "";
    const prompt = buildPairHandoffPrompt({
      fromLauncher: activeTab.launcher,
      fromSlot: partner.fromSlot,
      toLauncher: partner.launcher,
      toSlot: partner.slot,
      checkoutBranch: checkoutInfo?.currentBranch ?? null,
      openFilePath: visibleFileTab?.filePath ?? null,
      openFileLine: visibleFileTab?.revealLine ?? null,
      terminalTail: tail,
    });
    selectTab(partner.id);
    const inserted = sessions[partner.id]?.insertText(prompt, { focus: true }) ?? false;
    if (!inserted) {
      sessionError = `${partner.label} session is not ready to receive input yet.`;
    }
  }

  function handleAgentOpenFile(payload: TerminalOpenFileRequestPayload) {
    if (!repoPath || !sameRepo(payload.repo_path, repoPath, pathOpts)) return;
    const anchorId = tabIdForBackendSession(payload.caller_session_id) ?? tabState.activeId ?? undefined;
    showShell();
    const err = openFileSplitFor(payload.path, payload.line, payload.column, {
      focus: false,
      anchorId,
    });
    if (err) sessionError = err;
  }

  function handleAgentTouchedFile(payload: TerminalAgentTouchedFilePayload) {
    if (!repoPath) return;
    const sourceTabId = tabIdForBackendSession(payload.id);
    const sourceTab = sourceTabId ? tabState.tabs.find((t) => t.id === sourceTabId) : undefined;
    const matchesPanel =
      sameRepo(payload.repo_path, repoPath, pathOpts) ||
      Boolean(sourceTab?.checkout && sameRepo(payload.repo_path, sourceTab.checkout, pathOpts));
    if (!matchesPanel) return;

    // Always live-refresh any file pane already displaying this exact file on disk.
    if (!payload.artifact) {
      for (const tab of tabState.tabs) {
        if (
          tab.launcher === "file" &&
          tab.filePath === payload.path &&
          sameRepo(tab.checkout ?? repoPath, payload.repo_path, pathOpts)
        ) {
          filePanes[tab.id]?.reloadIfClean();
        }
      }
    }

    if (!followAgent) return;
    if (sourceTabId && sourceTabId !== tabState.activeId && !visibleIds.includes(sourceTabId)) {
      return;
    }

    // Follow drives one pane of its own (`terminal/followAgent.ts`). It used
    // to repoint whichever file pane was visible — the user's own — and, once
    // that was closed, open a fresh split for every edit.
    const decision = decideFollow(followTarget, tabState.tabs, followPaneFile);
    if (decision.kind === "stop") {
      stopFollowing();
      return;
    }

    if (payload.artifact) {
      if (sourceTabId) void followAgentArtifact(sourceTabId);
      return;
    }

    const checkout = sourceTab?.checkout;
    if (decision.kind === "retarget") {
      const paneId = decision.paneId;
      const pane = tabState.tabs.find((t) => t.id === paneId);
      if (
        pane &&
        (pane.filePath !== normalizePaneFilePath(payload.path) ||
          (pane.checkout || undefined) !== (checkout || undefined))
      ) {
        tabState = setTabFile(tabState, paneId, payload.path, 1, null, { checkout: checkout ?? "" });
      }
      adoptFollowPaneById(paneId);
      void tick().then(() => filePanes[paneId]?.reloadIfClean());
      return;
    }

    const before = new Set(tabState.tabs.map((tab) => tab.id));
    const err = openFileSplitFor(payload.path, 1, null, {
      focus: false,
      anchorId: sourceTabId ?? tabState.activeId ?? undefined,
      checkout,
      reuseSameFileOnly: true,
    });
    if (err) {
      sessionError = err;
      return;
    }
    const shown = fileTabFor(tabState, payload.path, checkout);
    if (!shown || shown.filePath !== normalizePaneFilePath(payload.path)) return;
    // Only a pane follow itself opened becomes follow's. One the user already
    // had on this file was reused for the reveal and stays theirs.
    if (!before.has(shown.id)) adoptFollowPaneById(shown.id);
    void tick().then(() => filePanes[shown.id]?.reloadIfClean());
  }

  /**
   * A plan artifact goes where the session's own opener puts it: follow's
   * pane (repointed) or a new split. Either way that pane is follow's now,
   * so closing it ends follow instead of the next plan edit reopening it.
   */
  async function followAgentArtifact(sourceTabId: string) {
    const before = new Set(tabState.tabs.map((tab) => tab.id));
    const owned = followTarget.paneId;
    await sessions[sourceTabId]?.openAgentArtifact();
    if (!followAgent) return;
    const added = tabState.tabs.find((tab) => !before.has(tab.id) && tab.launcher === "file");
    const pane = added?.id ?? owned ?? undefined;
    if (pane) adoptFollowPaneById(pane);
  }

  function handleFileDropOnPane(
    paneId: string,
    paths: string[],
    region: DropRegion,
    filesOnly = true,
  ): boolean {
    if (!repoPath || paths.length === 0) return false;
    const tab = tabState.tabs.find((candidate) => candidate.id === paneId);
    const paneRepo = tab?.checkout ?? repoPath;
    if (filesOnly && region !== "center" && paths.length === 1) {
      const rel = relativeToRepo(paths[0], paneRepo);
      if (rel && rel !== ".") {
        const direction: SplitDirection = region === "left" || region === "right" ? "row" : "col";
        const position: "before" | "after" = region === "left" || region === "top" ? "before" : "after";
        const err = openFileSplitFor(rel, 1, null, {
          focus: true,
          anchorId: paneId,
          checkout: tab?.checkout,
          direction,
          position,
        });
        if (err) sessionError = err;
        return true;
      }
    }

    if (!tab) return false;
    if (tab.launcher === "file") {
      const rel = filesOnly && paths.length === 1 ? relativeToRepo(paths[0], paneRepo) : null;
      if (rel && rel !== ".") {
        tabState = setTabFile(tabState, tab.id, rel, 1, null);
        selectTab(tab.id);
      } else {
        sendPathsToPreferredSession(paths);
      }
      return true;
    }
    if (tab.launcher === "browser") {
      sendPathsToPreferredSession(paths);
      return true;
    }
    const formatted = formatDroppedPathsForSession(paths, {
      launcher: tab.launcher,
      repoPath: paneRepo,
    });
    if (!formatted) return false;
    selectTab(tab.id);
    const inserted = sessions[tab.id]?.insertText(formatted, { focus: true }) ?? false;
    if (!inserted) {
      sessionError = "This terminal session is no longer running.";
    }
    return true;
  }

  function canEdgeSplitDrop(paths: readonly string[], paneId?: string): boolean {
    if (!repoPath || paths.length !== 1 || leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES) {
      return false;
    }
    const paneTab = paneId ? tabState.tabs.find((candidate) => candidate.id === paneId) : undefined;
    const rel = relativeToRepo(paths[0], paneTab?.checkout ?? repoPath);
    return rel !== null && rel !== ".";
  }

  function paneDropZone(node: HTMLElement, initialPaneId: string) {
    let currentId = initialPaneId;
    let unregister = registerFor(currentId);

    function registerFor(id: string) {
      return terminalDropZones.register({
        id,
        getRect: () => node.getBoundingClientRect(),
        isVisible: () =>
          Boolean(
            visible &&
              mode === "shell" &&
              (splitIds ? splitIds.includes(id) : activeId === id),
          ),
        canSplitEdges: (paths) => canEdgeSplitDrop(paths, id),
        onHover: (region) => {
          if (region) {
            fileDropHint = { paneId: id, region };
          } else if (fileDropHint?.paneId === id) {
            fileDropHint = null;
          }
        },
        onDrop: (paths, region) => handleFileDropOnPane(id, paths, region),
      });
    }

    return {
      update(nextId: string) {
        if (nextId === currentId) return;
        unregister();
        currentId = nextId;
        unregister = registerFor(currentId);
      },
      destroy() {
        unregister();
      },
    };
  }

  function openFileTabFor(
    filePath?: string,
    line: number | null = null,
    column: number | null = null,
    options?: { focus?: boolean },
  ): string | undefined {
    if (!repoPath) return "Open a repository first";
    const shouldFocus = options?.focus ?? true;
    const checkout = activeTab?.checkout;
    // A kept pane keeps its file: the new one opens in a tab of its own.
    const visibleFile =
      tabState.tabs.find(
        (tab) =>
          tab.launcher === "file" &&
          !tab.snippet &&
          canRepointFileTab(tab, filePath) &&
          (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId) &&
          (tab.checkout || undefined) === (checkout?.trim() || undefined),
      ) ??
      tabState.tabs.find(
        (tab) =>
          tab.launcher === "file" &&
          !tab.snippet &&
          canRepointFileTab(tab, filePath) &&
          (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId),
      );
    if (visibleFile && filePath) {
      const updated = setTabFile(tabState, visibleFile.id, filePath, line, column, { checkout });
      tabState = shouldFocus ? activateTab(updated, visibleFile.id) : updated;
      return undefined;
    }
    if (!canCreateKind("file") && !fileTabFor(tabState, filePath, checkout)) {
      return capacityTitleFor("file");
    }
    focusTabStrip = false;
    const opened = openFileTab(tabState, {
      filePath,
      checkout,
      line,
      column,
      reuseExisting: true,
      focus: shouldFocus,
    });
    if (!opened.id) return capacityTitleFor("file");
    if (shouldFocus && zoomedPaneId && zoomedPaneId !== opened.id) zoomedPaneId = null;
    tabState = opened.state;
    return undefined;
  }

  function toggleExplorerSidebar() {
    // In the Files section the explorer is the section itself: closing it
    // hands back to Shell, where the saved choice applies again.
    if (section === "files") {
      setSection("shell");
      return;
    }
    terminalExplorerSidebar.toggle();
    if (activeId) {
      void tick().then(() => revealTab(activeId));
    }
  }

  function setExplorerOpenTarget(target: ExplorerOpenTarget) {
    terminalExplorerSidebar.setTarget(target);
  }

  function dockFileTabToSidebar(id: string) {
    const targetTab = tabState.tabs.find((candidate) => candidate.id === id);
    if (targetTab?.filePath) {
      selectedExplorerFile = targetTab.filePath;
    }
    terminalExplorerSidebar.setOpen(true);
    if (!targetTab || targetTab.launcher !== "file") return;
    if (!targetTab.filePath && !targetTab.snippet && tabState.tabs.length > 1) {
      void requestClose(id);
      return;
    }
    if (findGroupForPane(splitGroups, id)) {
      removePaneFromSplit(id);
    }
  }

  function focusFirstContextMenuItem(node: HTMLElement) {
    void tick().then(() => {
      node.querySelector<HTMLButtonElement>('button[role="menuitem"]:not(:disabled)')?.focus({ preventScroll: true });
    });
  }

  /** Keeps the file tab now showing `filePath` (a double-click). */
  function keepFileShown(filePath: string, checkout?: string, preferId?: string | null) {
    tabState = keepFileTabFor(tabState, filePath, checkout, preferId).state;
  }

  /**
   * A file chosen inside a file pane (its explorer, a diff row, a caller).
   * A preview pane moves to it; a kept pane keeps its file, and the chosen
   * one opens in a tab beside it, as a click in any other explorer does.
   */
  function selectInFilePane(tab: TerminalTab, filePath: string, line: number | null, column: number | null) {
    const current = tabState.tabs.find((candidate) => candidate.id === tab.id) ?? tab;
    if (canRepointFileTab(current, filePath)) {
      tabState = setTabFile(tabState, current.id, filePath, line, column);
      return;
    }
    if (!canCreateKind("file") && !fileTabFor(tabState, filePath, current.checkout)) {
      sessionError = capacityTitleFor("file");
      return;
    }
    const opened = openFileTab(tabState, {
      filePath,
      checkout: current.checkout,
      line,
      column,
      reuseExisting: true,
      focus: true,
    });
    if (!opened.id) {
      sessionError = capacityTitleFor("file");
      return;
    }
    if (zoomedPaneId && zoomedPaneId !== opened.id) zoomedPaneId = null;
    tabState = opened.state;
  }

  function openFromExplorerSidebar(
    filePath: string,
    line: number | null = null,
    column: number | null = null,
    keep = false,
  ) {
    if (!filePath || !repoPath) return;
    const mods = lastExplorerMod;
    lastExplorerMod = { altKey: false, metaOrCtrlKey: false };
    selectedExplorerFile = filePath;
    const checkout = activeTab?.checkout;
    // A double-click (`keep`) follows where its own clicks sent the file: they
    // already used and cleared the modifier keys (`doubleClickFollows`).
    const now = Date.now();
    const action =
      keep && lastExplorerOpen && doubleClickFollows(lastExplorerOpen, filePath, now)
        ? lastExplorerOpen.action
        : resolveExplorerFileAction({
            defaultTarget: explorerOpenTarget,
            altKey: mods.altKey,
            metaOrCtrlKey: mods.metaOrCtrlKey,
            hasVisibleFilePane,
          });
    lastExplorerOpen = { path: filePath, action, at: now };

    if (action === "open-code" && onOpenInCodeView) {
      // Code owns its tabs: ask it to keep this one rather than preview it.
      if (keep) requestPin(repoPath, filePath);
      onOpenInCodeView(filePath, line);
      return;
    }
    if (action === "open-split") {
      const err = openFileSplitFor(filePath, line, column, {
        focus: false,
        checkout,
      });
      if (err) sessionError = err;
      else if (keep) keepFileShown(filePath, checkout);
      return;
    }
    if (action === "update-visible-pane") {
      // A kept pane is never repointed at another file; the file then opens
      // in a tab of its own below.
      const visibleFile =
        tabState.tabs.find(
          (tab) =>
            tab.launcher === "file" &&
            !tab.snippet &&
            canRepointFileTab(tab, filePath) &&
            (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId) &&
            (tab.checkout || undefined) === (checkout?.trim() || undefined),
        ) ??
        tabState.tabs.find(
          (tab) =>
            tab.launcher === "file" &&
            !tab.snippet &&
            canRepointFileTab(tab, filePath) &&
            (splitRoot ? visibleIds.includes(tab.id) : tab.id === activeId),
        );
      if (visibleFile) {
        tabState = setTabFile(tabState, visibleFile.id, filePath, line, column, { checkout });
        if (keep) keepFileShown(filePath, checkout, visibleFile.id);
        return;
      }
    }
    if (!canCreateKind("file") && !fileTabFor(tabState, filePath, checkout)) {
      sessionError = capacityTitleFor("file");
      return;
    }
    focusTabStrip = false;
    const opened = openFileTab(tabState, {
      filePath,
      checkout,
      line,
      column,
      reuseExisting: true,
      focus: true,
    });
    if (!opened.id) {
      sessionError = capacityTitleFor("file");
      return;
    }
    if (zoomedPaneId && zoomedPaneId !== opened.id) zoomedPaneId = null;
    tabState = opened.state;
    if (keep) keepFileShown(filePath, checkout, opened.id);
  }

  function startExplorerResize(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const handle = event.currentTarget as HTMLElement | null;
    const pointerId = event.pointerId;
    const startX = event.clientX;
    const startWidth = explorerSidebarWidth;
    handle?.setPointerCapture?.(pointerId);
    explorerResizing = true;

    const onMove = (moveEvent: PointerEvent) => {
      terminalExplorerSidebar.setWidthLive(startWidth + (moveEvent.clientX - startX));
    };

    const finish = () => {
      handle?.releasePointerCapture?.(pointerId);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      explorerResizing = false;
      terminalExplorerSidebar.setWidth(explorerSidebarWidth);
      if (activeId) void tick().then(() => revealTab(activeId));
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  }

  function handleExplorerSashKeydown(event: KeyboardEvent) {
    const step = event.shiftKey ? 32 : 16;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      event.stopPropagation();
      terminalExplorerSidebar.setWidth(
        clampExplorerSidebarWidth(explorerSidebarWidth + (event.key === "ArrowRight" ? step : -step)),
      );
    } else if (event.key === "Enter" || event.key === "Home") {
      event.preventDefault();
      event.stopPropagation();
      terminalExplorerSidebar.setWidth(TERMINAL_EXPLORER_WIDTH_DEFAULT);
    }
  }

  function replaceActiveSplitPaneWith(id: string) {
    if (!splitRoot || !activeGroup || hasPane(splitRoot, id)) return;
    const choice = chooseSplitReplacementSlot(splitRoot, activeId, pinnedTabIds);
    if (!choice || choice.mode === "blocked") {
      sessionError = "All split panes are pinned at the 8-pane limit — unpin or remove a pane first.";
      return;
    }
    const baseGroups = removePaneFromGroups(splitGroups, id);
    const refreshed = findGroupForPane(baseGroups, activeGroup.id === findGroupForPane(baseGroups, activeId)?.id ? activeId : leafPaneIds(activeGroup.tree)[0]);
    const targetTree = refreshed?.tree ?? splitRoot;
    const nextTree =
      choice.mode === "replace"
        ? replacePane(targetTree, choice.targetPaneId, id)
        : splitPane(targetTree, choice.anchorPaneId, id, "row", "after", 0.5);
    zoomedPaneId = null;
    applyGroupsAndCoLocate(
      upsertSplitGroup(baseGroups, nextTree, {
        groupId: refreshed?.id ?? activeGroup.id,
        name: refreshed?.name ?? activeGroup.name,
        activePaneId: id,
      }),
      activateTab(tabState, id),
    );
  }

  function preferredRecipientSessionTab() {
    const liveCandidates = tabState.tabs.filter(
      (tab) => tab.launcher !== "browser" && tab.launcher !== "file" && sessions[tab.id]?.isLive(),
    );
    return (
      liveCandidates.find((tab) => visibleIds.includes(tab.id) && isPromptLauncher(tab.launcher)) ??
      liveCandidates.find((tab) => isPromptLauncher(tab.launcher)) ??
      liveCandidates.find((tab) => visibleIds.includes(tab.id)) ??
      liveCandidates[0]
    );
  }

  /** Routes `@file#Lstart-Lend` or `@snapshot.png` attachments to the best live agent/shell session. */
  function sendReferenceToAgent(reference: string) {
    const preferred = preferredRecipientSessionTab();
    if (!preferred) {
      sessionError = "Open an agent or shell session first to send a reference.";
      return;
    }
    selectTab(preferred.id);
    sessions[preferred.id]?.insertText(reference, { focus: true });
  }

  /** Formats dropped/pasted paths for the actual recipient session (`@path` for agents, shell-escaped for shells). */
  function sendPathsToPreferredSession(paths: readonly string[]) {
    const preferred = preferredRecipientSessionTab();
    if (!preferred || preferred.launcher === "browser" || preferred.launcher === "file" || !repoPath) {
      sessionError = "Open an agent or shell session first to send a reference.";
      return;
    }
    const formatted = formatDroppedPathsForSession(paths, {
      launcher: preferred.launcher,
      repoPath: preferred.checkout ?? repoPath,
    });
    if (!formatted) return;
    selectTab(preferred.id);
    sessions[preferred.id]?.insertText(formatted, { focus: true });
  }

  /** Brings a tab's session forward: the terminal's grid, the browser's page, or the file viewer. */
  function revealTab(id: string) {
    (sessions[id] ?? browsers[id] ?? filePanes[id])?.reveal();
  }

  /**
   * ⌥⇧⌘1–4 / Ctrl+Alt+Shift+1–4 switch dock sections. Checked from both the
   * panel's keydown and the xterm hook (`onChord`), so a chord typed with a
   * shell focused switches the section without the shell receiving the bytes.
   */
  function handleSectionChord(event: KeyboardEvent): boolean {
    if (!visible || event.defaultPrevented) return false;
    const target = dockSectionForChord(event, $hostPlatform.os);
    if (!target) return false;
    event.preventDefault();
    event.stopPropagation();
    setSection(target);
    return true;
  }

  function handleChord(event: KeyboardEvent): boolean {
    if (handleSectionChord(event)) return true;
    if (mode !== "shell") return false;
    if (!visible || event.defaultPrevented) return false;
    const splitAction = terminalSplitChord(event, $hostPlatform.os);
    if (splitAction) {
      event.preventDefault();
      event.stopPropagation();
      focusTabStrip = false;
      if (splitAction === "split-right") void splitActive("row");
      else if (splitAction === "split-down") void splitActive("col");
      else if (splitAction === "zoom-pane") toggleZoomPane();
      else if (splitAction === "unsplit") closeSplit();
      else if (splitAction === "focus-next-pane" || splitAction === "focus-prev-pane") {
        if (splitRoot) {
          if (zoomedPaneId) zoomedPaneId = null;
          const nextPane = cycleVisibleSplitPane(
            splitRoot,
            activeId,
            splitAction === "focus-next-pane" ? 1 : -1,
          );
          if (nextPane) selectTab(nextPane);
        }
      } else if (splitAction === "focus-next-group" || splitAction === "focus-prev-group") {
        const nextGroup = cycleSplitGroup(
          splitGroups,
          activeGroup?.id ?? null,
          splitAction === "focus-next-group" ? 1 : -1,
        );
        if (nextGroup) {
          zoomedPaneId = null;
          selectTab(nextGroup.targetPaneId);
          void tick().then(() => revealTab(nextGroup.targetPaneId));
        }
      } else if (splitAction.startsWith("focus-pane-")) {
        const slot = Number.parseInt(splitAction.slice("focus-pane-".length), 10);
        const targetPane = splitRoot && slot >= 1 ? allSplitLeafIds[slot - 1] : undefined;
        if (targetPane) {
          if (zoomedPaneId && zoomedPaneId !== targetPane) zoomedPaneId = null;
          selectTab(targetPane);
        }
      } else if (
        splitAction === "focus-left" ||
        splitAction === "focus-right" ||
        splitAction === "focus-up" ||
        splitAction === "focus-down"
      ) {
        if (zoomedPaneId) zoomedPaneId = null;
        const dir = splitAction.slice("focus-".length) as "left" | "right" | "up" | "down";
        const neighbor = activeId ? adjacentPane(splitRoot, activeId, dir) : null;
        if (neighbor) selectTab(neighbor);
      }
      return true;
    }
    const chord = terminalTabChord(event);
    if (!chord) return false;
    event.preventDefault();
    event.stopPropagation();
    focusTabStrip = false;
    if (chord === "new") void openBeside("shell");
    else if (chord === "close" && tabState.activeId) void requestClose(tabState.activeId);
    else if (chord === "next" || chord === "prev") {
      const next = cycleTab(tabState, chord === "next" ? 1 : -1).activeId;
      if (next) selectTab(next);
    }
    return true;
  }

  function handlePanelKey(event: KeyboardEvent) {
    if (!visible || event.defaultPrevented) return;
    if (event.target instanceof HTMLElement && event.target.hasAttribute("data-inline-rename")) return;
    if (
      event.key === "Escape" &&
      !isImeComposition(event) &&
      (dockMenuOpen || tabOptions || sessionListOpen || sessionSearchOpen || launcherMenuOpen || tabContextMenu || splitContextMenu || groupContextMenu)
    ) {
      const sessionsWereOpen = sessionListOpen;
      closeChrome();
      event.preventDefault(); event.stopPropagation();
      if (sessionsWereOpen) restoreFocusTo(sessionListTrigger);
      else if (activeId) revealTab(activeId);
      return;
    }
    if (handleSectionChord(event)) return;
    if (handleChord(event)) return;
    if (mode === "shell" && activeId) {
      (sessions[activeId] ?? filePanes[activeId])?.handleViewChord(event);
    }
  }

  async function handleTabKey(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      const opener = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
      const targetTabId =
        opener?.closest("[data-terminal-tab]")?.getAttribute("data-terminal-tab") ?? activeId;
      if (targetTabId) {
        openTabContextMenu(targetTabId, event, opener);
      }
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    const id = terminalTabDestination(tabState, event.key);
    if (!id) return;
    event.preventDefault();
    event.stopPropagation();
    selectTab(id, true);
    await tick();
    if (visible && mode === "shell" && activeId === id) {
      tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-tab-${id}"]`)?.focus();
    }
  }

  /**
   * A hidden xterm cannot lay out, so a tab that becomes active has to be told
   * to refit — its ResizeObserver only fires after the browser recomputes
   * layout, and the grid it would paint until then is the stale one.
   */
  // reveal() takes focus, so the effect below may re-run only when the tab
  // being shown changes, never for anything else. Three things used to re-run
  // it and pulled focus out of whatever the user had just opened (the
  // session-type menu's search box) back onto the prompt or the grid:
  //  - `visible` arrives inside the dock's props object, which is rebuilt on
  //    every repository-store emission with the same value, so the effect
  //    depended on the object rather than on the boolean. A derived value
  //    only propagates when it differs, so the effect reads it instead;
  //  - `unread` changes when ANOTHER tab prints its first line;
  //  - `sessions` changes whenever any tab mounts or unmounts.
  const revealedId = $derived(visible && mode === "shell" ? activeId : null);
  $effect(() => {
    const id = revealedId;
    if (!id) return;
    untrack(() => {
      if (unread.has(id)) unread = new Set([...unread].filter((key) => key !== id));
    });
    void tick().then(() => {
      if (!visible || mode !== "shell" || activeId !== id) return;
      // Looked up once the tab has mounted, outside the effect's tracking.
      revealTab(id);
      if (focusTabStrip) tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-tab-${id}"]`)?.focus();
    });
  });

  // Any split sibling brought on screen (unzooming `zoomedPaneId`, splitting an
  // unread background tab into view) clears its unread marker without refocusing.
  $effect(() => {
    if (!visible || mode !== "shell") return;
    const shown = visibleIds;
    untrack(() => {
      if (unread.size === 0) return;
      let changed = false;
      for (const id of shown) {
        if (unread.has(id)) {
          changed = true;
          break;
        }
      }
      if (changed) {
        const shownSet = new Set(shown);
        unread = new Set([...unread].filter((key) => !shownSet.has(key)));
      }
    });
  });

  // OSC titles can widen a tab after it is selected. Keep its entire control
  // (including Close) visible without refocusing the shell on every title.
  $effect(() => {
    const activeScrollLabel = activeTab ? tabLabel(activeTab) : "";
    void activeScrollLabel;
    if (!visible || mode !== "shell") return;
    const id = activeId;
    const scroller = tabScroller;
    void tick().then(() => {
      if (!visible || mode !== "shell" || activeId !== id) return;
      const node = scroller?.querySelector(`[data-terminal-tab="${id}"]`);
      if (scroller && node instanceof HTMLElement) {
        const strip = scroller.getBoundingClientRect();
        const tab = node.getBoundingClientRect();
        scroller.scrollLeft = scrollChildIntoHorizontalView(scroller, {
          offsetLeft: tab.left - strip.left + scroller.scrollLeft,
          offsetWidth: tab.width,
        });
      }
    });
  });

  async function splitActive(
    direction: SplitDirection = "row",
    position: "before" | "after" = "after",
  ) {
    const first = activeId;
    if (!first) return;
    zoomedPaneId = null;
    if (leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES) return;
    // Only a loose tab of the chosen kind re-pairs (so 1× then Split restores
    // the pair); anything else is a new session. `splitPartner` names it in
    // the button's tooltip, so the user sees which before clicking.
    const second = splitPartner;
    if (!second) {
      await splitPaneWithNewKind(first, nextLauncher, direction, position);
      return;
    }
    focusTabStrip = false;
    const nextTree = splitPane(splitRoot, first, second, direction, position, 0.5);
    const owner = findGroupForPane(splitGroups, first);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: owner?.id,
        name: owner?.name,
        activePaneId: first,
      }),
      activateTab(tabState, first),
    );
  }

  async function splitPaneWithNewKind(
    anchorPaneId: string,
    kind: TabKind,
    direction: SplitDirection = "row",
    position: "before" | "after" = "after",
    options?: { startDir?: string | null; initialCommand?: string | null },
  ): Promise<void> {
    if (!repoPath || !tabState.tabs.some((tab) => tab.id === anchorPaneId)) return;
    if (!canCreateKind(kind)) {
      sessionError = capacityTitleFor(kind);
      return;
    }
    const initialOwner = findGroupForPane(splitGroups, anchorPaneId);
    if (leafPaneIds(initialOwner?.tree ?? null).length >= MAX_SPLIT_PANES) return;

    const read =
      kind === "shell" && options?.startDir === undefined
        ? await sessions[anchorPaneId]?.readContext()
        : undefined;
    if (!repoPath || !tabState.tabs.some((tab) => tab.id === anchorPaneId)) return;
    if (!canCreateKind(kind)) {
      sessionError = capacityTitleFor(kind);
      return;
    }
    const owner = findGroupForPane(splitGroups, anchorPaneId);
    const baseTree = owner?.tree ?? null;
    if (leafPaneIds(baseTree).length >= MAX_SPLIT_PANES) return;

    const anchorTab = tabState.tabs.find((tab) => tab.id === anchorPaneId);
    const dir =
      kind === "shell"
        ? (options?.startDir ?? startDirFrom(read ?? CONTEXT_UNKNOWN))
        : null;
    focusTabStrip = false;
    zoomedPaneId = null;

    let nextState = tabState;
    let createdId: string | null = null;
    if (kind === "file") {
      const opened = openFileTab(nextState, {
        checkout: anchorTab?.checkout,
        reuseExisting: false,
        focus: false,
      });
      if (!opened.id) {
        sessionError = capacityTitleFor("file");
        return;
      }
      nextState = opened.state;
      createdId = opened.id;
    } else if (kind === "browser") {
      const opened = openBrowserTab(nextState, undefined);
      if (!opened.id) {
        sessionError = capacityTitleFor("browser");
        return;
      }
      nextState = opened.state;
      createdId = opened.id;
    } else if (anchorTab?.checkout) {
      const opened = openCheckoutTab(nextState, kind, {
        checkout: anchorTab.checkout,
        startDir: dir,
      });
      if (!opened.id) {
        sessionError = capacityTitleFor(kind);
        return;
      }
      nextState = opened.state;
      createdId = opened.id;
    } else {
      const created = openTab(nextState, kind);
      const added = created.tabs.find((tab) => !nextState.tabs.includes(tab));
      if (!added) {
        sessionError = capacityTitleFor(kind);
        return;
      }
      nextState = dir ? setTabStartDir(created, added.id, dir) : created;
      createdId = added.id;
    }

    if (!createdId || createdId === anchorPaneId) return;
    if (kind === "shell" && options?.initialCommand) {
      nextState = setTabInitialCommand(nextState, createdId, options.initialCommand);
    }
    const nextTree = splitPane(baseTree, anchorPaneId, createdId, direction, position, 0.5);
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: owner?.id,
        name: owner?.name,
        activePaneId: createdId,
      }),
      activateTab(nextState, createdId),
    );
  }

  async function runCommandInNewSplitShell(
    anchorTabId: string,
    command: string,
    startDir?: string | null,
  ): Promise<void> {
    const cleaned = command.trim();
    if (!cleaned || !repoPath) return;
    if (!canCreateKind("shell")) {
      sessionError = capacityTitleFor("shell");
      return;
    }
    const owner = findGroupForPane(splitGroups, anchorTabId);
    if (
      tabState.tabs.some((tab) => tab.id === anchorTabId) &&
      leafPaneIds(owner?.tree ?? null).length < MAX_SPLIT_PANES
    ) {
      await splitPaneWithNewKind(anchorTabId, "shell", "row", "after", {
        startDir,
        initialCommand: cleaned,
      });
      return;
    }
    const anchorTab = tabState.tabs.find((tab) => tab.id === anchorTabId);
    let createdId: string | null = null;
    if (anchorTab?.checkout) {
      const opened = openCheckoutTab(tabState, "shell", {
        checkout: anchorTab.checkout,
        startDir: startDir ?? undefined,
      });
      if (!opened.id) {
        sessionError = capacityTitleFor("shell");
        return;
      }
      tabState = opened.state;
      createdId = opened.id;
    } else {
      createdId = newTab("shell");
      if (!createdId) {
        sessionError = capacityTitleFor("shell");
        return;
      }
      if (startDir) tabState = setTabStartDir(tabState, createdId, startDir);
    }
    tabState = setTabInitialCommand(tabState, createdId, cleaned);
  }

  function flipGroupOrientation(groupId: string, targetPaneId?: string | null) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    const nextTree = flipSplitOrientation(group.tree, targetPaneId);
    if (!nextTree || nextTree === group.tree || nextTree.kind !== "branch") return;
    if (zoomedPaneId && hasPane(group.tree, zoomedPaneId)) zoomedPaneId = null;
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: group.id,
        name: group.name,
        activePaneId: group.activePaneId,
      }),
    );
  }

  function swapPaneInGroup(groupId: string, paneId?: string | null) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    const anchor =
      paneId && hasPane(group.tree, paneId)
        ? paneId
        : (group.activePaneId ?? leafPaneIds(group.tree)[0]);
    if (!anchor) return;
    const nextTree = swapWithAdjacentSplitPane(group.tree, anchor);
    if (!nextTree || nextTree === group.tree || nextTree.kind !== "branch") return;
    if (zoomedPaneId && hasPane(group.tree, zoomedPaneId)) zoomedPaneId = null;
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: group.id,
        name: group.name,
        activePaneId: group.activePaneId,
      }),
    );
  }

  function equalizeGroupSplits(groupId: string) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    const nextTree = equalizeSplits(group.tree);
    if (!nextTree || nextTree === group.tree || nextTree.kind !== "branch") return;
    if (zoomedPaneId && hasPane(group.tree, zoomedPaneId)) zoomedPaneId = null;
    applyGroupsAndCoLocate(
      upsertSplitGroup(splitGroups, nextTree, {
        groupId: group.id,
        name: group.name,
        activePaneId: group.activePaneId,
      }),
    );
  }

  function ungroupAllPairs() {
    zoomedPaneId = null;
    inlineRenameGroupId = null;
    applyGroupsAndCoLocate([]);
  }

  function setGroupPinned(groupId: string, pinned: boolean) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    let nextState = tabState;
    for (const leafId of leafPaneIds(group.tree)) {
      nextState = toggleTabPinned(nextState, leafId, pinned);
    }
    tabState = nextState;
  }

  function moveGroupInStrip(groupId: string, step: -1 | 1) {
    const moved = moveSplitGroupByStep(tabState.tabs, splitGroups, groupId, step);
    if (moved.tabs !== tabState.tabs) {
      tabState = { ...tabState, tabs: moved.tabs };
    }
    if (moved.groups !== splitGroups) {
      splitGroups = moved.groups;
    }
  }

  async function closeTabsBatch(ids: readonly string[]) {
    for (const id of ids) {
      if (!tabState.tabs.some((candidate) => candidate.id === id)) continue;
      await requestClose(id);
      if (tabState.tabs.some((candidate) => candidate.id === id)) break;
    }
  }

  async function closeOtherTabs(keepId: string) {
    const ids = orderedStripTabs
      .filter((tab) => tab.id !== keepId && !tab.pinned)
      .map((tab) => tab.id);
    await closeTabsBatch(ids);
  }

  async function closeTabsToRight(anchorId: string) {
    const anchorIdx = orderedStripTabs.findIndex((tab) => tab.id === anchorId);
    if (anchorIdx < 0) return;
    const ids = orderedStripTabs
      .slice(anchorIdx + 1)
      .filter((tab) => !tab.pinned)
      .map((tab) => tab.id);
    await closeTabsBatch(ids);
  }

  async function closeEndedTabs() {
    const ids = orderedStripTabs
      .filter((tab) => tabStatuses[tab.id] === "exited")
      .map((tab) => tab.id);
    await closeTabsBatch(ids);
  }

  async function closePairTabs(groupId: string) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    await closeTabsBatch(leafPaneIds(group.tree));
  }

  function closeSplit() {
    if (!activeGroup) return;
    zoomedPaneId = null;
    ungroupPair(activeGroup.id);
  }

  function activateSplitGroup(groupId: string) {
    const group = splitGroups.find((candidate) => candidate.id === groupId);
    if (!group) return;
    const attention = groupAttentions[group.id];
    const targetId =
      attention?.urgentPaneId ??
      (group.activePaneId && hasPane(group.tree, group.activePaneId)
        ? group.activePaneId
        : leafPaneIds(group.tree)[0]);
    if (!targetId) return;
    showShell();
    zoomedPaneId = null;
    selectTab(targetId);
    void tick().then(() => revealTab(targetId));
  }

  function revealHiddenSplitUrgent() {
    const urgentId = hiddenSplitSummary.urgentPaneId;
    zoomedPaneId = null;
    if (urgentId && tabState.tabs.some((tab) => tab.id === urgentId)) {
      selectTab(urgentId);
      void tick().then(() => revealTab(urgentId));
    } else if (activeId) {
      void tick().then(() => revealTab(activeId));
    }
  }

  function startInlineRename(id: string, surface: "tab" | "pane") {
    const target = tabState.tabs.find((tab) => tab.id === id);
    if (!target) return;
    inlineRenameGroupId = null;
    inlineRenameTabId = id;
    inlineRenameSurface = surface;
    inlineRenameDraft = target.userNamed ? (target.name ?? "") : tabLabel(target);
    void tick().then(() => {
      inlineRenameInput?.focus();
      inlineRenameInput?.select();
    });
  }

  function startGroupInlineRename(groupId: string) {
    const idx = splitGroups.findIndex((candidate) => candidate.id === groupId);
    if (idx < 0) return;
    inlineRenameTabId = null;
    inlineRenameSurface = null;
    inlineRenameGroupId = groupId;
    inlineRenameGroupDraft = splitGroupDisplayLabel(splitGroups[idx], idx, splitGroups.length);
    void tick().then(() => {
      inlineRenameGroupInput?.focus();
      inlineRenameGroupInput?.select();
    });
  }

  function commitGroupInlineRename(groupId: string, restoreFocus = false) {
    if (inlineRenameGroupId !== groupId) return;
    const draft = inlineRenameGroupDraft;
    inlineRenameGroupId = null;
    splitGroups = renameSplitGroup(splitGroups, groupId, draft);
    if (restoreFocus) {
      void tick().then(() => {
        tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-pair-badge-${groupId}"]`)?.focus();
      });
    }
  }

  function cancelGroupInlineRename(restoreFocus = false) {
    const groupId = inlineRenameGroupId;
    inlineRenameGroupId = null;
    if (restoreFocus && groupId) {
      void tick().then(() => {
        tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-pair-badge-${groupId}"]`)?.focus();
      });
    }
  }

  function restoreFocusAfterInlineRename(id: string | null, surface: "tab" | "pane" | null) {
    if (!id || !surface) return;
    void tick().then(() => {
      if (surface === "tab") {
        tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-tab-${id}"]`)?.focus();
      } else {
        revealTab(id);
      }
    });
  }

  function commitInlineRename(id: string, restoreFocus = false) {
    if (inlineRenameTabId !== id) return;
    const draft = inlineRenameDraft;
    const surface = inlineRenameSurface;
    inlineRenameTabId = null;
    inlineRenameSurface = null;
    tabState = renameTab(tabState, id, draft);
    if (restoreFocus) restoreFocusAfterInlineRename(id, surface);
  }

  function cancelInlineRename(restoreFocus = false) {
    const id = inlineRenameTabId;
    const surface = inlineRenameSurface;
    inlineRenameTabId = null;
    inlineRenameSurface = null;
    if (restoreFocus) restoreFocusAfterInlineRename(id, surface);
  }

  async function toggleSplit() {
    if (splitRoot) {
      closeSplit();
      return;
    }
    await splitActive("row");
  }

  function toggleZoomPane() {
    if (!splitRoot || !activeId || !hasPane(splitRoot, activeId)) return;
    zoomedPaneId = zoomedPaneId === activeId ? null : activeId;
    void tick().then(() => {
      if (activeId) revealTab(activeId);
    });
  }

  function zoomSplitPane(id: string) {
    if (!splitRoot || !hasPane(splitRoot, id)) return;
    if (activeId !== id) selectTab(id);
    zoomedPaneId = zoomedPaneId === id ? null : id;
    void tick().then(() => {
      revealTab(id);
    });
  }

  function removePaneFromSplit(id: string) {
    const ownerGroup = findGroupForPane(splitGroups, id);
    if (!ownerGroup) return;
    const remainingIds = leafPaneIds(ownerGroup.tree).filter((leafId) => leafId !== id);
    if (zoomedPaneId === id || remainingIds.length < 2) zoomedPaneId = null;
    applyGroupsAndCoLocate(removePaneFromGroups(splitGroups, id));
    if (activeId === id && remainingIds.length > 0) {
      selectTab(remainingIds[0]);
    }
  }

  function startSashDrag(event: PointerEvent, sash: SashDescriptor) {
    if (event.button !== 0 || !panesContainerEl) return;
    event.preventDefault();
    event.stopPropagation();
    const handle = event.currentTarget as HTMLElement | null;
    const pointerId = event.pointerId;
    handle?.setPointerCapture?.(pointerId);
    sashDragging = true;

    const onMove = (moveEvent: PointerEvent) => {
      if (!panesContainerEl || !splitRoot) return;
      const rect = panesContainerEl.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      if (sash.direction === "row") {
        const relX = (moveEvent.clientX - rect.left) / rect.width;
        const localRatio = sash.parentBox.width > 0 ? (relX - sash.parentBox.left) / sash.parentBox.width : 0.5;
        setActiveSplitTree(resizeBranch(splitRoot, sash.branchId, localRatio));
      } else {
        const relY = (moveEvent.clientY - rect.top) / rect.height;
        const localRatio = sash.parentBox.height > 0 ? (relY - sash.parentBox.top) / sash.parentBox.height : 0.5;
        setActiveSplitTree(resizeBranch(splitRoot, sash.branchId, localRatio));
      }
    };

    const finish = () => {
      handle?.releasePointerCapture?.(pointerId);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      sashDragging = false;
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  }

  function handleSashKeydown(event: KeyboardEvent, sash: SashDescriptor) {
    if (!splitRoot) return;
    const step = event.shiftKey ? 0.1 : 0.05;
    if (sash.direction === "row" && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      event.stopPropagation();
      setActiveSplitTree(resizeBranch(splitRoot, sash.branchId, sash.ratio + (event.key === "ArrowRight" ? step : -step)));
    } else if (sash.direction === "col" && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      event.stopPropagation();
      setActiveSplitTree(resizeBranch(splitRoot, sash.branchId, sash.ratio + (event.key === "ArrowDown" ? step : -step)));
    } else if (event.key === "Enter" || event.key === "Home") {
      event.preventDefault();
      event.stopPropagation();
      setActiveSplitTree(equalizeSplits(splitRoot, sash.branchId));
    }
  }

  function resolveTabDropZone(event: DragEvent, paneId: string): TabDropZone | null {
    const el = event.currentTarget;
    if (!(el instanceof HTMLElement)) return null;
    return computeTabDropHint(el.getBoundingClientRect(), event.clientX, event.clientY, paneId);
  }

  function paneBoxStyle(id: string): string {
    const box = splitLayout.panes[id];
    if (!box) return "left: 0%; top: 0%; width: 100%; height: 100%;";
    return `left: ${(box.left * 100).toFixed(4)}%; top: ${(box.top * 100).toFixed(4)}%; width: ${(box.width * 100).toFixed(4)}%; height: ${(box.height * 100).toFixed(4)}%;`;
  }

  function showTabOptions() {
    const current = tabState.tabs.find((tab) => tab.id === activeId);
    renameValue = current?.userNamed ? (current.name ?? "") : "";
    const next = !tabOptions;
    closeChrome();
    tabOptions = next;
  }

  function saveTabName(event: SubmitEvent) {
    event.preventDefault();
    if (activeId) tabState = renameTab(tabState, activeId, renameValue);
    tabOptions = false;
  }

  function openSnippetSplitFor(title: string, content: string): string | undefined {
    if (!repoPath) return "Open a repository first";
    const anchorId = tabState.activeId;
    const anchorGroup = anchorId ? findGroupForPane(splitGroups, anchorId) : null;
    const cleanTitle = title.trim() || "Command output";
    const groupSnippet = anchorGroup
      ? tabState.tabs.find(
          (tab) => tab.launcher === "file" && !tab.filePath && hasPane(anchorGroup.tree, tab.id),
        )
      : undefined;
    const ungroupedSnippet = !groupSnippet
      ? tabState.tabs.find(
          (tab) => tab.launcher === "file" && !tab.filePath && !groupedTabIds.has(tab.id),
        )
      : undefined;
    const reuseCandidate = groupSnippet ?? ungroupedSnippet;
    if (!reuseCandidate && !canCreateKind("file")) return capacityTitleFor("file");
    const result = reuseCandidate
      ? {
          state: {
            ...tabState,
            tabs: tabState.tabs.map((tab) =>
              tab.id === reuseCandidate.id
                ? {
                    ...tab,
                    filePath: undefined,
                    ...(tab.userNamed ? { userNamed: false, name: tab.launchName } : {}),
                    snippet: { title: cleanTitle.slice(0, 64), content: content.slice(0, 256 * 1024) },
                    revealLine: 1,
                    revealColumn: null,
                    revealSeq: (tab.revealSeq ?? 0) + 1,
                  }
                : tab,
            ),
          },
          id: reuseCandidate.id,
        }
      : openSnippetTab(tabState, { title, content, reuseExisting: false, focus: false });
    if (!result.id) return capacityTitleFor("file");
    focusTabStrip = false;
    const baseTree = anchorGroup?.tree ?? null;
    if (anchorId && result.id !== anchorId && !hasPane(baseTree, result.id)) {
      const splitTarget = baseTree
        ? (hasPane(baseTree, anchorId) ? anchorId : leafPaneIds(baseTree)[0])
        : anchorId;
      if (splitTarget && leafPaneIds(baseTree).length < MAX_SPLIT_PANES) {
        if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
        const nextTree = splitPane(baseTree, splitTarget, result.id, "row", "after", 0.5);
        upsertGroupForAnchor(anchorId, nextTree, result.state, result.state.activeId);
      } else if (baseTree && splitTarget) {
        const choice = chooseSplitReplacementSlot(baseTree, splitTarget, pinnedTabIds);
        if (choice?.mode === "replace") {
          if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
          const nextTree = replacePane(baseTree, choice.targetPaneId, result.id);
          upsertGroupForAnchor(anchorId, nextTree, result.state, result.state.activeId);
        } else {
          tabState = result.state;
          if (choice?.mode === "blocked") {
            sessionError = "Opened snippet tab in the background because all 8 split panes are pinned — unpin or remove a pane to show it.";
          }
        }
      } else {
        tabState = result.state;
      }
    } else {
      if (zoomedPaneId && zoomedPaneId !== result.id) zoomedPaneId = null;
      tabState = result.state;
    }
    return undefined;
  }

  function outputAction(event: Event) {
    if (!(event.currentTarget instanceof HTMLSelectElement) || !activeId) return;
    const session = sessions[activeId], action = event.currentTarget.value;
    event.currentTarget.value = "";
    if (action === "selection") void session?.copySelection();
    else if (action === "copy-md") {
      void Promise.resolve(session?.copyAsMarkdown()).then((copied) => {
        if (!copied) sessionError = "No terminal output available to copy as Markdown.";
      });
    } else if (action === "last-cmd") {
      void Promise.resolve(session?.copyLastCommand()).then((copied) => {
        if (!copied) sessionError = "No completed shell command recorded yet.";
      });
    } else if (action === "run-last-new-shell") {
      if (!session?.runSelectionOrLastCommandInNewShell()) {
        sessionError = "Select a command or run a shell command first.";
      }
    } else if (action === "copy") void session?.copyOutput();
    else if (action === "export") session?.exportOutput();
    else if (action === "last-copy") {
      void Promise.resolve(session?.copyLastCommandOutput()).then((copied) => {
        if (!copied) sessionError = "No completed command block output available yet.";
      });
    } else if (action === "last-split") {
      const block = session?.lastCommandOutput();
      if (!block || !block.output.trim()) {
        sessionError = "No completed command block output available yet.";
        return;
      }
      const err = openSnippetSplitFor("Last command output", block.output);
      if (err) sessionError = err;
    }
  }

  function trimExecutions() {
    const retained = retainExecutions(executions);
    discardedExecutions += executions.length - retained.length;
    executions = retained;
  }

  const QUICK_COMMANDS = [
    "git status",
    "git log -n 5 --oneline",
    "git diff --stat",
    "npm test",
    "cargo check",
  ];

  async function scrollToBottom() {
    await tick();
    if (scrollContainer) {
      scrollContainer.scrollTop = scrollContainer.scrollHeight;
    }
  }

  async function execute(rawCommand?: string, timeoutSecs = 600) {
    const textToRun = (rawCommand ?? commandInput).trim();
    if (!textToRun || running) return;

    if (!repoPath) {
      validationError = "No repository open.";
      return;
    }

    const tokenized = tokenizeCommand(textToRun);
    if (!tokenized.ok) {
      validationError = tokenized.error;
      return;
    }

    if (!boundedCommand(textToRun)) {
      validationError = "Console commands are limited to 64 KiB.";
      return;
    }
    validationError = null;

    // Update command history
    history = retainCommand(history, textToRun);
    historyIndex = -1;
    savedDraft = "";
    commandInput = "";

    const entryId = `exec-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    const entry: ExecutionEntry = {
      id: entryId,
      command: textToRun,
      timestamp: Date.now(),
      running: true,
    };

    executions = [...executions, entry];
    trimExecutions();
    consoleFollowing = true;
    running = true;
    void scrollToBottom();

    try {
      const response = await invoke<TerminalRunResponse>("cmd_terminal_run", {
        repoPath,
        args: tokenized.argv,
        // Long enough for a cold install/build; the backend clamps to [1s, 30min].
        timeoutSecs,
      });

      executions = executions.map((e) =>
        e.id === entryId ? { ...e, running: false, result: response } : e,
      );

      harnessStore.recordAction({
        repoPath,
        kind: "terminal",
        label: textToRun,
        ok: !response.timed_out && response.exit_code === 0,
        verdict: response.policy ?? null,
      });
    } catch (err) {
      const errMessage = describeError(err, "terminal");
      executions = executions.map((e) =>
        e.id === entryId ? { ...e, running: false, error: errMessage } : e,
      );

      harnessStore.recordAction({
        repoPath,
        kind: "terminal",
        label: textToRun,
        ok: false,
      });
    } finally {
      running = false;
      trimExecutions();
      if (consoleFollowing && visible && section === "console") void scrollToBottom();
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Enter/Arrow keys during an IME conversion belong to the composition,
    // not to command execution or history navigation.
    if (isImeComposition(e)) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void execute();
    } else if (e.key === "ArrowUp") {
      if (history.length === 0) return;
      e.preventDefault();
      if (historyIndex === -1) {
        savedDraft = commandInput;
        historyIndex = history.length - 1;
      } else if (historyIndex > 0) {
        historyIndex -= 1;
      }
      commandInput = history[historyIndex] ?? "";
    } else if (e.key === "ArrowDown") {
      if (historyIndex === -1) return;
      e.preventDefault();
      if (historyIndex < history.length - 1) {
        historyIndex += 1;
        commandInput = history[historyIndex] ?? "";
      } else {
        historyIndex = -1;
        commandInput = savedDraft;
      }
    }
  }

  function clearOutput() {
    executions = [];
    discardedExecutions = 0;
    validationError = null;
  }

  async function copyOutput(entry: ExecutionEntry) {
    let text = `$ ${entry.command}\n`;
    if (entry.result) {
      if (entry.result.stdout_tail) text += `${entry.result.stdout_tail}\n`;
      if (entry.result.stderr_tail) text += `${entry.result.stderr_tail}\n`;
    } else if (entry.error) {
      text += `Error: ${entry.error}\n`;
    }
    if (await copyText(text.trim())) {
      copiedId = entry.id;
      if (copiedResetTimer !== null) clearTimeout(copiedResetTimer);
      copiedResetTimer = setTimeout(() => {
        copiedResetTimer = null;
        if (copiedId === entry.id) copiedId = null;
      }, 1500);
    }
  }

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(2)}s`;
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<!-- Justified: keyboard events bubble from the region's interactive controls; the region itself is not a focus stop. -->
<div class="relative flex-1 flex flex-col bg-background h-full min-w-0 text-xs font-sans overflow-hidden" role="region" aria-label="Dock" onkeydown={handlePanelKey}>
  <DockHeader
    {section}
    onSection={setSection}
    os={$hostPlatform.os}
    {agentAttention}
    {activeSessionListed}
    repoName={repoPath?.split(/[\\/]/).pop() ?? null}
    {repoPath}
    {contextTitle}
    checkoutLabel={checkoutInfo?.label.checkout ?? null}
    branch={checkoutInfo?.currentBranch ?? null}
    status={dockStatusSummary}
    {statusDetail}
    activeSession={activeTab && activeDisplay
      ? {
          launcher: activeTab.launcher,
          launcherLabel: launcherLabel(activeTab.launcher),
          primary: activeDisplay.primary,
          secondary: activeDisplay.secondary,
          fullTitle: activeDisplay.fullTitle,
          badge: oscStatusBadge(effectiveAgentStatuses[activeTab.id]),
        }
      : null}
    onReturnToShell={() => {
      setSection("shell");
      if (activeId) void tick().then(() => revealTab(activeId));
    }}
    findDisabled={!activeId || !repoPath || activeIsNonTerminal}
    onFind={() => activeId && sessions[activeId]?.openFind()}
    canClearScrollback={Boolean(activeId && repoPath && !activeIsNonTerminal)}
    onClearScrollback={() => activeId && sessions[activeId]?.clearScrollback()}
    canClearOutput={executions.length > 0}
    onClearOutput={clearOutput}
    sessionCountLabel={sessionCountLabel($terminalSessions.length, $terminalSessionLimit)}
    sessionsAtLimit={$terminalSessions.length >= $terminalSessionLimit}
    {sessionListOpen}
    onToggleSessionList={toggleSessionList}
    {dockMode}
    onToggleDockMode={onToggleDockMode}
    canTabOptions={Boolean(activeId) && section === "shell"}
    tabOptionsOpen={tabOptions}
    onTabOptions={showTabOptions}
    canFindSession={tabState.tabs.length > 0 && section === "shell"}
    onFindSession={() => (sessionSearchOpen ? closeChrome() : openSessionSearch())}
    {expanded}
    {onToggleExpanded}
    {onClose}
    bind:overflowEl={sessionListTrigger}
    bind:menuOpen={dockMenuOpen}
  />

  {#if sessionListOpen}
    <div
      bind:this={sessionListEl}
      id="terminal-session-list"
      class="terminal-popover terminal-session-list px-3 py-2 bg-surface text-meta"
      role="listbox"
      aria-label="Sessions across repositories"
      aria-activedescendant={sessionListIndex >= 0 ? `terminal-session-option-${sessionListIndex}` : undefined}
      tabindex="0"
      onkeydown={onSessionListKey}
    >
      <input
        type="search"
        aria-label="Search terminal sessions"
        placeholder="Search sessions"
        bind:value={sessionListQuery}
        maxlength="128"
        oninput={() => (sessionListHighlight = null)}
        class="gp-field w-full mb-1.5"
      />
      {#if sessionListMessage}
        <p class="mb-1 text-textMuted" data-testid="session-list-notice">{sessionListMessage}</p>
      {/if}
      <!-- One header per repository, drawn on that group's first session so
           every session stays a direct child of this list. A wrapper around
           the group would make the group's first Go to the one a reader
           activates. A flat row repeated the repository on every line, and
           the checkout path only appeared as a tooltip over the empty middle.
           Named by the same rule as the tab strip (`terminal/checkoutLabel.ts`);
           two repositories that share a name stay separate groups. -->
      {#each sessionGroups as group (group.key)}
        {#each group.sessions as session, indexInGroup (session.key)}
            {@const sessionInfo = sessionRow(session, repoPath, checkouts, pathOpts)}
            {@const where = session.checkout ?? session.repoPath}
            {@const index = sessionListFlat.findIndex((candidate) => candidate.key === session.key)}
            {@const focused = session.key === activeId && sessionInfo.here}
            {@const rowLiveAct = session.sessionId ? $sessionActivity.get(session.sessionId) : undefined}
            {@const rowAttention = session.attention ?? (session.sessionId ? activityAttentionLabel($sessionActivity.get(session.sessionId)) : null)}
            {@const rowLauncher = session.launcher ?? "shell"}
            {@const cleanedSessionTitle = session.title ? stripTitleSpinner(session.title).replace(/^[✳✦★•]+\s+/u, "").trim() : ""}
            {@const cleanedLiveTitle = rowLiveAct?.title ? stripTitleSpinner(rowLiveAct.title).replace(/^[✳✦★•]+\s+/u, "").trim() : ""}
            {@const rawRowTitle = (cleanedSessionTitle && !isGenericLauncherTitle(cleanedSessionTitle, rowLauncher)) ? cleanedSessionTitle : (cleanedLiveTitle && !isGenericLauncherTitle(cleanedLiveTitle, rowLauncher)) ? cleanedLiveTitle : ""}
            {@const rowMainTitle = rawRowTitle || session.label.trim() || "Shell"}
            <div
              id="terminal-session-option-{index}"
              class="session-row {focused ? 'bg-accent/10 rounded-lg' : ''} {index === sessionListIndex ? 'ring-1 ring-inset ring-accent/40 rounded-lg' : ''}"
              role="option"
              data-testid="session-row"
              data-focused={focused ? "true" : "false"}
              aria-selected={index === sessionListIndex}
              tabindex="-1"
              use:revealSessionRow={index === sessionListIndex}
              onclick={(event) => {
                const target = event.target;
                if (target instanceof Element && target.closest("[data-testid='session-group']")) return;
                if (session.reveal) void goToSession(session);
              }}
              onkeydown={(event) => {
                if (event.key !== "Enter" || !session.reveal) return;
                event.preventDefault();
                event.stopPropagation();
                void goToSession(session);
              }}
            >
              {#if indexInGroup === 0}
                <div class="session-group-head flex items-center gap-2 pt-1.5 pb-0.5" data-testid="session-group">
                  <RepoLogo path={session.repoPath} name={group.repository} size={16} />
                  <span class="font-medium" data-testid="session-repository">{group.repository}</span>
                  <span class="text-textMuted">{group.sessions.length} {group.sessions.length === 1 ? "session" : "sessions"}</span>
                  {#if group.here}<span class="text-textMuted">This repository</span>{/if}
                </div>
              {/if}
              <span class="min-w-0 flex items-center gap-1.5">
                <LauncherIcon kind={rowLauncher} size={13} />
                {#if session.splitSlot}<span class="gp-chip shrink-0 tabular-nums" data-tone="neutral" data-testid="session-split-slot" title={`Visible in split pane #${session.splitSlot}`}>#{session.splitSlot}</span>{/if}
                {#if session.isActive}<span class="gp-chip shrink-0" data-tone="accent" data-testid="session-active-badge">Active</span>{/if}
                {#if rowAttention}<span class="gp-chip shrink-0" data-tone={rowAttention === "Failed" ? "danger" : "warn"} data-testid="session-attention-badge">{rowAttention}</span>{/if}
                {#if sessionInfo.agent && rawRowTitle && rawRowTitle.toLowerCase() !== sessionInfo.agent.toLowerCase()}<span class="gp-chip shrink-0" data-tone="accent" data-testid="session-agent" title={`Agent: ${sessionInfo.agent}`}><span class="sr-only">Agent: </span>{sessionInfo.agent}</span>{/if}
                <span class="min-w-0 truncate">
                  <span class="font-medium text-textPrimary">{rowMainTitle}</span>
                  {#if sessionInfo.checkout}<span class="text-textMuted" aria-hidden="true"> · </span><span class="sr-only">, checkout </span><span data-testid="session-checkout">{sessionInfo.checkout}</span>{/if}
                  <span class="text-textMuted"> · <span data-testid="session-status">{sessionStatusLabel(session.status)}</span></span>
                  <span class="sr-only">{session.status}</span>
                  {#if session.taskRunId}<span class="text-textMuted"> · task run</span>{:else if session.continuesRunId}<span class="text-textMuted"> · resumed conversation</span>{/if}
                </span>
              </span>
              <span class="session-path truncate font-mono text-textMuted" data-testid="session-path" title={where}>{where}</span>
              <span class="flex items-center gap-1 shrink-0">
                <button
                  type="button"
                  class="gp-btn" data-size="sm"
                  disabled={!session.reveal}
                  title={session.reveal
                    ? (sessionInfo.here ? "Show this session" : `Switch to ${session.repoPath} and show this session`)
                    : "This session's panel is not mounted, so it cannot be shown"}
                  onclick={(event) => {
                    event.stopPropagation();
                    void goToSession(session);
                  }}
                >Go to</button>
                {#if sessionInfo.taskRunId}
                  <button
                    type="button"
                    class="gp-btn" data-size="sm"
                    data-session-aux
                    data-testid="session-open-task"
                    title={`Open the task ${session.title ? `“${session.title}” ` : ""}this session is working on`}
                    onclick={(event) => {
                      event.stopPropagation();
                      void openListedTask(sessionInfo.taskRunId);
                    }}
                  >Task</button>
                {/if}
                <button
                  type="button"
                  class="gp-btn ml-3" data-variant="ghost" data-size="sm"
                  data-session-aux
                  onclick={(event) => {
                    event.stopPropagation();
                    void closeListed(session);
                  }}
                >Close session</button>
              </span>
            </div>
        {/each}
      {/each}
    </div>
  {/if}
  {#if waitingForCapacity && mode === "shell"}
    <div class="gp-notice" data-tone="warn" role="status" data-testid="terminal-waiting-for-capacity">
      {#if stripFull}
        {waitingForCapacity.title} is waiting to open here: this repository's terminal already holds {MAX_STRIP_TABS} tabs. Close ended ones and it opens.
      {:else}
        {waitingForCapacity.title} is waiting to open here: all {$terminalSessionLimit} terminal sessions are in use. Close one in Sessions, or raise the limit in Settings → Agents, and it opens.
      {/if}
    </div>
  {/if}
  {#if sessionError}
    <div class="gp-notice flex items-start gap-1.5" data-tone="danger" role="alert" data-testid="terminal-session-error">
      <AlertCircle size={13} class="shrink-0 mt-0.5" />
      <span class="flex-1" style="overflow-wrap:anywhere">{sessionError}</span>
      <button type="button" class="gp-icon-btn" data-size="sm" aria-label="Dismiss" onclick={() => (sessionError = null)}>✕</button>
    </div>
  {/if}
  {#if repoLedgerOffers.length > 0 && mode === "shell"}
    <div
      class="gp-notice flex flex-col gap-1.5"
      data-tone={repoReopenableOffers.length > 0 ? "warn" : "unknown"}
      role="region"
      aria-label="Recoverable agent sessions"
      data-testid="terminal-session-ledger-banner"
    >
      <div class="flex items-center justify-between gap-2 flex-wrap">
        <span class="font-medium text-textPrimary" data-testid="terminal-session-ledger-heading">
          {reopenBannerHeading(repoReopenableOffers.length)}
        </span>
        {#if repoReopenableOffers.length > 1}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="terminal-reopen-all-btn"
            disabled={reopeningAllLedger}
            onclick={() => void handleReopenAllLedgerRows()}
          >
            <RotateCw size={12} /> {reopenBannerHeading(repoReopenableOffers.length)}
          </button>
        {/if}
      </div>
      <ul class="space-y-1 max-h-40 overflow-y-auto">
        {#each repoLedgerOffers as offer (offer.row_id)}
          {@const rowErr = ledgerRowErrors[offer.row_id] ?? offer.last_error ?? (offer.end_state !== "unreadable" && offer.launcher !== "shell" && !offer.resumable ? offer.reason : null)}
          {@const ledgerTitle = offer.title ? stripTitleSpinner(offer.title).replace(/^[✳✦★•]+\s+/u, "").trim() : ""}
          <li
            class="flex items-start justify-between gap-2 rounded-lg border border-border/65 bg-surface/60 px-2 py-1 text-micro"
            data-testid="terminal-ledger-row"
            data-end-state={offer.end_state}
          >
            <div class="min-w-0 flex-1 space-y-0.5">
              <div class="flex items-center gap-1.5 flex-wrap">
                {#if offer.end_state !== "unreadable"}
                  <LauncherIcon kind={offer.launcher as LauncherKind} size={12} class="shrink-0" />
                {/if}
                <span class="font-medium text-textPrimary">
                  {offer.end_state === "unreadable" ? "Corrupt ledger entry" : launcherLabel(offer.launcher as LauncherKind)}
                </span>
                {#if ledgerTitle && !isGenericLauncherTitle(ledgerTitle, offer.launcher as TabKind)}
                  <span class="text-textPrimary truncate max-w-64" data-testid="terminal-ledger-row-title" title={ledgerTitle}>
                    “{ledgerTitle}”
                  </span>
                {/if}
                <span class="gp-chip" data-tone={offer.end_state === "unreadable" ? "danger" : "neutral"}>
                  {describeEndState(offer.end_state, offer.signal)}
                </span>
                {#if offer.launcher === "shell"}
                  <span class="text-textMuted" data-testid="terminal-ledger-not-restorable">not restorable</span>
                {:else if offer.end_state === "unreadable"}
                  <span class="text-danger" data-testid="terminal-ledger-unreadable">unreadable</span>
                {/if}
                {#if offer.mode}
                  <span class="font-mono text-textMuted">{offer.mode}</span>
                {/if}
              </div>
              {#if offer.cwd}
                <div class="font-mono text-textMuted truncate" title={offer.cwd}>{offer.cwd}</div>
              {/if}
              {#if rowErr}
                <div class="text-danger" role="alert" data-testid="terminal-ledger-row-error" style="overflow-wrap:anywhere">
                  {rowErr}
                </div>
              {/if}
            </div>
            <div class="flex items-center gap-1 shrink-0">
              {#if offer.resumable}
                <button
                  type="button"
                  class="gp-btn"
                  data-size="sm"
                  data-testid="terminal-ledger-reopen-btn"
                  disabled={reopeningAllLedger || reopeningLedgerRowId === offer.row_id}
                  onclick={() => void handleReopenLedgerRow(offer)}
                >
                  <RotateCw size={11} /> Reopen
                </button>
              {/if}
              <button
                type="button"
                class="gp-icon-btn"
                data-size="sm"
                data-testid="terminal-ledger-dismiss-btn"
                aria-label="Dismiss session offer"
                title="Dismiss"
                onclick={() => void handleDismissLedgerRow(offer.row_id)}
              >✕</button>
            </div>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
  <!-- Tab strip + sessions. Rendered in BOTH modes and merely hidden in
       Console, because unmounting a session kills the shell — the same
       hide-don't-kill rule TerminalDock applies to the whole dock. -->
    <div
      class="terminal-strip-row shrink-0 flex items-stretch gap-1 px-2 h-9 min-w-0"
      class:hidden={mode !== "shell"}
      data-crowded={crowdedTabStrip(tabState.tabs.length) ? "true" : "false"}
    >
      <div class="flex items-center shrink-0">
        <button
          type="button"
          class="gp-icon-btn {explorerSidebarOpen ? 'text-accent bg-accent/15' : ''}"
          data-testid="terminal-explorer-sidebar-toggle"
          aria-label={explorerSidebarOpen ? "Hide file explorer sidebar" : "Show file explorer sidebar"}
          aria-pressed={explorerSidebarOpen}
          disabled={!repoPath}
          title={explorerSidebarOpen ? "Hide the files sidebar" : "Show files beside your sessions"}
          onclick={toggleExplorerSidebar}
        >
          {#if explorerSidebarOpen}
            <PanelLeftClose size={13} />
          {:else}
            <PanelLeft size={13} />
          {/if}
        </button>
      </div>
      {#if activeTab && activeDisplay}
        <!-- Pair switcher and split indicator: tab context, so they sit with the
             tabs rather than in the dock header. -->
        <div class="flex items-center gap-1 shrink-0 self-center">
          {#if mode === "shell" && splitRoot && allSplitLeafIds.length > 1}
            <span
              class="gp-chip shrink-0 tabular-nums"
              data-tone="accent"
              data-testid="terminal-header-split-indicator"
              title={zoomedPaneId ? "Split pane zoomed — other split panes stay warm" : `${allSplitLeafIds.length} split panes visible`}
            >
              {#if activeGroup && (splitGroups.length > 1 || activeGroup.name)}
                {activeGroupLabel} · 
              {/if}
              {#if activeSplitSlot}
                Pane {activeSplitSlot} of {allSplitLeafIds.length}{#if zoomedPaneId} · Zoomed{/if}
              {:else}
                {allSplitLeafIds.length} split panes
              {/if}
            </span>
            {#if hiddenSplitSummary.badgeLabel}
              <button
                type="button"
                class="gp-chip shrink-0 cursor-pointer"
                data-tone={hiddenSplitSummary.tone ?? "accent"}
                data-testid="terminal-hidden-split-alert"
                title="Hidden split pane activity while zoomed — click to restore split panes and show"
                onclick={revealHiddenSplitUrgent}
              >
                Hidden: {hiddenSplitSummary.badgeLabel}
              </button>
            {/if}
          {/if}
          {#if mode === "shell" && splitGroups.length > 0 && (splitGroups.length > 1 || !activeGroup)}
            <div
              class="inline-flex items-center gap-1 shrink-0"
              role="group"
              aria-label="Terminal pairs"
              data-testid="terminal-pair-switcher"
            >
              {#each splitGroups as grp, grpIdx (grp.id)}
                {@const isGrpActive = activeGroup?.id === grp.id}
                {@const grpLabel = splitGroupDisplayLabel(grp, grpIdx, splitGroups.length)}
                {@const att = groupAttentions[grp.id]}
                <button
                  type="button"
                  class="gp-chip shrink-0 cursor-pointer tabular-nums"
                  data-tone={isGrpActive ? "accent" : (att?.tone ?? "neutral")}
                  data-testid="terminal-pair-switcher-btn"
                  data-group-id={grp.id}
                  data-group-attention={att?.tone ?? "none"}
                  aria-pressed={isGrpActive}
                  title={!isGrpActive && att?.badgeLabel ? `${grpLabel} — ${att.badgeLabel} · Right-click for pair options` : `Switch to ${grpLabel} · Right-click for pair options`}
                  onclick={() => activateSplitGroup(grp.id)}
                  oncontextmenu={(event) => {
                    openGroupContextMenu(grp.id, event, event.currentTarget as HTMLElement);
                  }}
                  onkeydown={(event) => {
                    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                      openGroupContextMenu(grp.id, event, event.currentTarget as HTMLElement);
                    }
                  }}
                >
                  <Columns2 size={10} class="shrink-0" aria-hidden="true" />
                  {grpLabel}{#if !isGrpActive && att?.badgeLabel}<span class="ml-0.5">●</span>{/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
      <!-- Only the tabs scroll. The launcher group sat inside the scroller
           behind an `ml-auto`, so past a handful of tabs the way to open one
           more scrolled off the right edge. -->
      <div class="relative flex-1 min-w-0 self-stretch">
      {#snippet tabItem(tab: (typeof tabState.tabs)[number])}
        {@const isActiveTab = tab.id === tabState.activeId}
        {@const inVisibleSplit = Boolean(splitRoot && visibleIds.includes(tab.id))}
        {@const inActiveSplitGroup = Boolean(splitRoot && hasPane(splitRoot, tab.id))}
        {@const groupMeta = groupMetaByTabId[tab.id]}
        {@const splitSlot = groupMeta?.slot ?? splitSlotByTabId[tab.id]}
        {@const display = tabDisplayTitle(tab, effectiveAgentStatuses[tab.id], tabBrowserStates[tab.id])}
        {@const isRenamingThisTab = inlineRenameTabId === tab.id && inlineRenameSurface === "tab"}
        {@const canOpenInSplit = tabState.tabs.length >= 2 && (splitRoot ? !inActiveSplitGroup : !isActiveTab) && leafPaneIds(splitRoot).length < MAX_SPLIT_PANES}
        {@const stripHint = tabStripDropHint?.tabId === tab.id ? tabStripDropHint.mode : null}
        <!-- Justified: this wrapper only receives pointer gestures (context menu,
             drag-and-drop reorder); the focusable controls are the buttons inside
             it, so the wrapper itself must not take focus or a role. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          data-terminal-tab={tab.id}
          data-group-id={groupMeta?.group.id}
          data-active={isActiveTab ? "true" : "false"}
          data-in-split={inVisibleSplit ? "true" : "false"}
          data-pinned={tab.pinned ? "true" : "false"}
          oncontextmenu={(event) => {
            if (isRenamingThisTab) return;
            openTabContextMenu(tab.id, event, event.currentTarget as HTMLElement);
          }}
          ondragover={(event) => {
            if (draggedGroupId) {
              if (groupMeta?.group.id === draggedGroupId) return;
              const rect = event.currentTarget.getBoundingClientRect();
              if (rect.width <= 0) return;
              const rx = (event.clientX - rect.left) / rect.width;
              event.preventDefault();
              event.stopPropagation();
              if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
              tabStripDropHint = { tabId: tab.id, mode: rx < 0.5 ? "before" : "after" };
              return;
            }
            if (!draggedTabId || draggedTabId === tab.id) return;
            const rect = event.currentTarget.getBoundingClientRect();
            if (rect.width <= 0) return;
            const rx = (event.clientX - rect.left) / rect.width;
            const dropMode = classifyTabStripDropZone(rx, event.altKey);
            event.preventDefault();
            event.stopPropagation();
            if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
            tabStripDropHint = { tabId: tab.id, mode: dropMode };
          }}
          ondragleave={(event) => {
            if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) return;
            if (tabStripDropHint?.tabId === tab.id) tabStripDropHint = null;
          }}
          ondrop={(event) => {
            if (draggedGroupId) {
              const movingGroup = draggedGroupId;
              const hint = tabStripDropHint?.tabId === tab.id && tabStripDropHint.mode === "after" ? "after" : "before";
              const targetGroupId = groupMeta?.group.id;
              draggedGroupId = null;
              draggedTabId = null;
              tabDropHint = null;
              tabStripDropHint = null;
              if (!movingGroup || targetGroupId === movingGroup) return;
              event.preventDefault();
              event.stopPropagation();
              const reordered = reorderSplitGroupInStrip(tabState.tabs, splitGroups, movingGroup, tab.id, hint);
              tabState = { ...tabState, tabs: reordered.tabs };
              splitGroups = reordered.groups;
              return;
            }
            const moving = draggedTabId;
            const rect = event.currentTarget.getBoundingClientRect();
            const fallbackMode = rect.width > 0
              ? classifyTabStripDropZone((event.clientX - rect.left) / rect.width, event.altKey)
              : "after";
            const hint = tabStripDropHint?.tabId === tab.id ? tabStripDropHint.mode : fallbackMode;
            draggedTabId = null;
            draggedGroupId = null;
            tabDropHint = null;
            tabStripDropHint = null;
            if (!moving || moving === tab.id) return;
            event.preventDefault();
            event.stopPropagation();
            if (hint === "pair") zoomedPaneId = null;
            const outcome = applyTabStripDrop(tabState.tabs, splitGroups, {
              movingTabId: moving,
              targetTabId: tab.id,
              mode: hint,
              pinnedIds: pinnedTabIds,
            });
            if (outcome.refusedReason) {
              sessionError = outcome.refusedReason;
              return;
            }
            if (zoomedPaneId && !outcome.groups.some((g) => hasPane(g.tree, zoomedPaneId ?? ""))) {
              zoomedPaneId = null;
            }
            const nextState = outcome.activeTabId
              ? activateTab({ ...tabState, tabs: outcome.tabs }, outcome.activeTabId)
              : { ...tabState, tabs: outcome.tabs };
            tabState = nextState;
            splitGroups = recordGroupActivePane(outcome.groups, nextState.activeId);
          }}
          class="group relative flex items-center gap-1 pl-2 pr-1 my-1 rounded-lg border text-meta shrink-0 transition-colors {stripHint === 'pair' ? 'ring-1 ring-inset ring-accent bg-accent/15' : ''} {isActiveTab
            ? 'bg-surface border-accent/65 text-textPrimary font-medium'
            : inVisibleSplit
              ? 'bg-surface/60 border-border/65 text-textPrimary'
              : groupMeta
                ? 'bg-surface/35 border-border/35 text-textMuted hover:bg-surface/70 hover:text-textPrimary'
                : 'bg-transparent border-transparent text-textMuted hover:bg-surface/70 hover:text-textPrimary'}"
        >
          {#if stripHint === "before"}
            <span
              aria-hidden="true"
              data-testid="terminal-tab-drop-before"
              class="pointer-events-none absolute -left-[3px] top-0.5 bottom-0.5 w-0.5 rounded-full bg-accent"
              style:z-index={LAYERS.STICKY}
            ></span>
          {:else if stripHint === "after"}
            <span
              aria-hidden="true"
              data-testid="terminal-tab-drop-after"
              class="pointer-events-none absolute -right-[3px] top-0.5 bottom-0.5 w-0.5 rounded-full bg-accent"
              style:z-index={LAYERS.STICKY}
            ></span>
          {/if}
          {#if isActiveTab}
            <span aria-hidden="true" class="w-0.5 h-3.5 rounded-full bg-accent shrink-0"></span>
          {/if}
          {#if isRenamingThisTab}
            <input
              bind:this={inlineRenameInput}
              bind:value={inlineRenameDraft}
              data-inline-rename
              data-testid="terminal-inline-rename-input"
              aria-label="Rename terminal tab"
              maxlength="64"
              class="gp-field h-6 px-1.5 py-0 min-w-20 w-36 text-micro"
              onpointerdown={(event) => event.stopPropagation()}
              onclick={(event) => event.stopPropagation()}
              onkeydown={(event) => {
                if (isImeComposition(event)) return;
                event.stopPropagation();
                if (event.key === "Enter") {
                  event.preventDefault();
                  commitInlineRename(tab.id, true);
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  cancelInlineRename(true);
                }
              }}
              onblur={() => commitInlineRename(tab.id)}
            />
          {/if}
          <button
            type="button"
            role="tab"
            id={`terminal-tab-${tab.id}`}
            aria-controls={`terminal-pane-${tab.id}`}
            aria-selected={isActiveTab}
            tabindex={isActiveTab ? 0 : -1}
            draggable={!isRenamingThisTab}
            ondragstart={(event) => {
              draggedGroupId = null;
              draggedTabId = tab.id;
              event.dataTransfer?.setData("application/x-gitpulse-terminal-tab", tab.id);
              if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
            }}
            ondragend={() => {
              draggedTabId = null;
              draggedGroupId = null;
              tabDropHint = null;
              tabStripDropHint = null;
            }}
            class="terminal-tab-label truncate"
            class:hidden={isRenamingThisTab}
            onclick={(event) => {
              if (event.altKey && (splitRoot ? !inActiveSplitGroup : !isActiveTab)) {
                openTabInSplit(tab.id);
                return;
              }
              selectTab(tab.id);
            }}
            ondblclick={() => startInlineRename(tab.id, "tab")}
            onkeydown={handleTabKey}
            title={`${launcherLabel(tab.launcher)} — ${display.fullTitle}${groupMeta ? ` (${groupMeta.label} #${groupMeta.slot}${isActiveTab ? ", active" : ""})` : isActiveTab ? " (Active)" : ""}${tab.pinned ? " · Pinned in split" : ""}${canOpenInSplit ? " · Drag to reorder · Alt+Drag or Alt+Click to split · Right-click for options" : " · Drag to reorder · Right-click for options"}`}
          >
            <LauncherIcon kind={tab.launcher} size={12} class="mr-1 align-[-2px]" />{#if splitSlot}<span aria-hidden="true" data-testid="terminal-tab-split-slot" class="inline-flex items-center justify-center min-w-3.5 h-3.5 px-1 mr-1 rounded text-micro tabular-nums align-[1px] {isActiveTab ? 'bg-accent/15 text-accent border border-accent/65 font-semibold' : 'bg-surfaceHover text-textMuted border border-border/65'}">{splitSlot}</span>{/if}{#if tab.pinned}<span aria-hidden="true" data-testid="terminal-tab-pinned-icon" class="inline-flex mr-1 align-[-1px] text-accent"><Pin size={10} /></span>{/if}{#if unread.has(tab.id)}<span aria-label="Unread output" class="text-accent">● </span>{/if}{#if tab.parentSessionId}<span class="text-accent mr-0.5" data-testid="terminal-tab-child-badge" title="Child session spawned by parent agent">↳</span>{/if}{tabLabel(tab)}{#if tabStatuses[tab.id] === "exited"}<span class="text-textMuted"> · Ended</span>{:else if tabStatuses[tab.id] === "error"}<span class="text-danger"> · Error</span>{:else if oscStatusBadge(effectiveAgentStatuses[tab.id])}{@const badge = oscStatusBadge(effectiveAgentStatuses[tab.id])}{#if badge}<span class={badge.cls} data-testid="terminal-tab-osc-status" title={effectiveAgentStatuses[tab.id]?.detail ?? badge.label}> · {badge.label}</span>{/if}{/if}
          </button>
          {#if canOpenInSplit}
            <button
              type="button"
              class="gp-icon-btn opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:text-accent"
              data-size="sm"
              data-testid="terminal-tab-open-in-split"
              onclick={(event) => {
                event.stopPropagation();
                openTabInSplit(tab.id);
              }}
              aria-label={`Open ${tabLabel(tab)} in split`}
              title={`Open ${tabLabel(tab)} side-by-side in split (Alt+Click)`}
            >
              <Columns2 size={11} />
            </button>
          {/if}
          <button
            type="button"
            class="gp-icon-btn opacity-50 group-hover:opacity-100 focus-visible:opacity-100 hover:text-danger"
            data-size="sm"
            onclick={() => void requestClose(tab.id)}
            aria-label={`Close ${tabLabel(tab)}`}
            title="Close this session ({shortcutTextLabel('⌃⇧W', $hostPlatform.os)}) — the process is terminated"
          >
            <X size={11} />
          </button>
        </div>
      {/snippet}
      <!-- Justified: role=tablist is a container here, not a widget that takes focus;
           the focusable items are the tab buttons inside it. Its drag handlers only
           accept a drop on the empty strip (event.target === currentTarget). -->
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div
        bind:this={tabScroller}
        data-terminal-tab-scroller
        data-crowded={crowdedTabStrip(tabState.tabs.length) ? "true" : "false"}
        class="terminal-tab-scroller h-full flex items-stretch gap-1 overflow-x-auto overflow-y-hidden"
        role="tablist"
        aria-label="Terminal sessions"
        ondragover={(event) => {
          if (event.target !== event.currentTarget) return;
          if (!draggedTabId && !draggedGroupId) return;
          event.preventDefault();
          if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
          tabStripDropHint = { detach: Boolean(draggedTabId), mode: "after" };
        }}
        ondragleave={(event) => {
          if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) return;
          if (tabStripDropHint?.detach) tabStripDropHint = null;
        }}
        ondrop={(event) => {
          if (event.target !== event.currentTarget) return;
          const moving = draggedTabId;
          const movingGroup = draggedGroupId;
          draggedTabId = null;
          draggedGroupId = null;
          tabDropHint = null;
          tabStripDropHint = null;
          if (movingGroup) {
            event.preventDefault();
            const movedGroup = moveSplitGroupToStripEnd(tabState.tabs, splitGroups, movingGroup);
            tabState = { ...tabState, tabs: movedGroup.tabs };
            splitGroups = movedGroup.groups;
            return;
          }
          if (!moving) return;
          event.preventDefault();
          const detached = detachTabToStripEnd(tabState.tabs, splitGroups, moving);
          if (zoomedPaneId && (zoomedPaneId === moving || !detached.groups.some((g) => hasPane(g.tree, zoomedPaneId ?? "")))) {
            zoomedPaneId = null;
          }
          tabState = { ...tabState, tabs: detached.tabs };
          splitGroups = recordGroupActivePane(detached.groups, tabState.activeId);
        }}
      >
      {#each tabStripSegments as segment (segment.kind === "group" ? segment.group.id : segment.tab.id)}
        {#if segment.kind === "group"}
          {@const isGroupActive = activeGroup?.id === segment.group.id}
          {@const segLabel = splitGroupDisplayLabel(segment.group, segment.groupIndex - 1, splitGroups.length)}
          {@const groupAtt = groupAttentions[segment.group.id]}
          {@const groupDropHint = tabStripDropHint?.groupId === segment.group.id ? tabStripDropHint.mode : null}
          <div
            data-testid="terminal-tab-group"
            data-group-id={segment.group.id}
            data-group-active={isGroupActive ? "true" : "false"}
            class="relative flex items-center gap-0.5 px-1 my-0.5 rounded-lg border shrink-0 transition-colors {groupDropHint === 'pair' ? 'ring-1 ring-inset ring-accent bg-accent/15' : ''} {isGroupActive ? 'bg-accent/10 border-accent/65' : 'bg-surface/40 border-border/65 hover:border-border/90'}"
          >
            {#if groupDropHint === "before"}
              <span
                aria-hidden="true"
                class="pointer-events-none absolute -left-[3px] top-0.5 bottom-0.5 w-0.5 rounded-full bg-accent"
                style:z-index={LAYERS.STICKY}
              ></span>
            {:else if groupDropHint === "after"}
              <span
                aria-hidden="true"
                class="pointer-events-none absolute -right-[3px] top-0.5 bottom-0.5 w-0.5 rounded-full bg-accent"
                style:z-index={LAYERS.STICKY}
              ></span>
            {/if}
            {#if inlineRenameGroupId === segment.group.id}
              <input
                bind:this={inlineRenameGroupInput}
                bind:value={inlineRenameGroupDraft}
                data-inline-rename
                data-testid="terminal-group-rename-input"
                aria-label="Rename terminal pair"
                maxlength="48"
                class="gp-field h-6 px-1.5 py-0 min-w-16 w-28 text-micro"
                onpointerdown={(event) => event.stopPropagation()}
                onclick={(event) => event.stopPropagation()}
                onkeydown={(event) => {
                  if (isImeComposition(event)) return;
                  event.stopPropagation();
                  if (event.key === "Enter") {
                    event.preventDefault();
                    commitGroupInlineRename(segment.group.id, true);
                  } else if (event.key === "Escape") {
                    event.preventDefault();
                    cancelGroupInlineRename(true);
                  }
                }}
                onblur={() => commitGroupInlineRename(segment.group.id)}
              />
            {:else}
              <button
                type="button"
                id={`terminal-pair-badge-${segment.group.id}`}
                data-testid="terminal-tab-group-badge"
                data-group-attention={groupAtt?.tone ?? "none"}
                draggable={inlineRenameGroupId !== segment.group.id}
                ondragstart={(event) => {
                  draggedTabId = null;
                  draggedGroupId = segment.group.id;
                  event.dataTransfer?.setData("application/x-gitpulse-terminal-group", segment.group.id);
                  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
                }}
                ondragend={() => {
                  draggedGroupId = null;
                  draggedTabId = null;
                  tabDropHint = null;
                  tabStripDropHint = null;
                }}
                ondragover={(event) => {
                  if (draggedGroupId && draggedGroupId !== segment.group.id) {
                    const rect = event.currentTarget.getBoundingClientRect();
                    const rx = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0;
                    event.preventDefault();
                    event.stopPropagation();
                    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
                    tabStripDropHint = { groupId: segment.group.id, mode: rx < 0.5 ? "before" : "after" };
                    return;
                  }
                  if (draggedTabId && !segment.tabs.some((t) => t.id === draggedTabId)) {
                    event.preventDefault();
                    event.stopPropagation();
                    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
                    tabStripDropHint = { groupId: segment.group.id, mode: "pair" };
                  }
                }}
                ondragleave={(event) => {
                  if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) return;
                  if (tabStripDropHint?.groupId === segment.group.id) tabStripDropHint = null;
                }}
                ondrop={(event) => {
                  if (draggedGroupId && draggedGroupId !== segment.group.id) {
                    const movingGroup = draggedGroupId;
                    const pos = tabStripDropHint?.groupId === segment.group.id && tabStripDropHint.mode === "after" ? "after" : "before";
                    draggedGroupId = null;
                    draggedTabId = null;
                    tabStripDropHint = null;
                    event.preventDefault();
                    event.stopPropagation();
                    const reordered = reorderSplitGroupInStrip(tabState.tabs, splitGroups, movingGroup, segment.group.id, pos);
                    tabState = { ...tabState, tabs: reordered.tabs };
                    splitGroups = reordered.groups;
                    return;
                  }
                  if (draggedTabId && !segment.tabs.some((t) => t.id === draggedTabId)) {
                    const moving = draggedTabId;
                    const anchorId = segment.group.activePaneId ?? segment.tabs[0]?.id;
                    draggedTabId = null;
                    draggedGroupId = null;
                    tabStripDropHint = null;
                    if (!anchorId) return;
                    event.preventDefault();
                    event.stopPropagation();
                    zoomedPaneId = null;
                    const outcome = applyTabStripDrop(tabState.tabs, splitGroups, {
                      movingTabId: moving,
                      targetTabId: anchorId,
                      mode: "pair",
                      pinnedIds: pinnedTabIds,
                    });
                    if (outcome.refusedReason) {
                      sessionError = outcome.refusedReason;
                      return;
                    }
                    const nextState = outcome.activeTabId
                      ? activateTab({ ...tabState, tabs: outcome.tabs }, outcome.activeTabId)
                      : { ...tabState, tabs: outcome.tabs };
                    tabState = nextState;
                    splitGroups = recordGroupActivePane(outcome.groups, nextState.activeId);
                  }
                }}
                class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-micro font-medium cursor-grab active:cursor-grabbing transition-colors {isGroupActive ? 'text-accent hover:bg-accent/15' : 'text-textMuted hover:text-textPrimary hover:bg-surfaceHover'}"
                onclick={() => activateSplitGroup(segment.group.id)}
                ondblclick={(event) => {
                  event.stopPropagation();
                  startGroupInlineRename(segment.group.id);
                }}
                oncontextmenu={(event) => {
                  openGroupContextMenu(segment.group.id, event, event.currentTarget as HTMLElement);
                }}
                onkeydown={(event) => {
                  if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                    openGroupContextMenu(segment.group.id, event, event.currentTarget as HTMLElement);
                  }
                }}
                title={`Switch to ${segLabel} (${segment.tabs.length} panes) — drag badge to reorder pair, drop a tab here to join pair, double-click to rename, right-click for pair options`}
              >
                <Columns2 size={10} class="shrink-0" />
                <span>{segLabel}</span>
                {#if !isGroupActive && groupAtt?.badgeLabel}
                  <span
                    data-testid="terminal-tab-group-attention"
                    aria-label={`${segLabel}: ${groupAtt.badgeLabel}`}
                    class={groupAtt.tone === "danger" ? "text-danger" : groupAtt.tone === "warn" ? "text-warn" : "text-accent"}
                  >●</span>
                {/if}
              </button>
            {/if}
            {#each segment.tabs as tab (tab.id)}
              {@render tabItem(tab)}
            {/each}
            <button
              type="button"
              class="gp-icon-btn opacity-60 hover:opacity-100 hover:text-textPrimary"
              data-size="sm"
              data-testid="terminal-tab-group-unsplit"
              onclick={(event) => {
                event.stopPropagation();
                ungroupPair(segment.group.id);
              }}
              aria-label={`Ungroup ${segLabel}`}
              title={`Ungroup ${segLabel} (keeps sessions open)`}
            >
              1×
            </button>
          </div>
        {:else}
          {@render tabItem(segment.tab)}
        {/if}
      {/each}
      {#if stripPairDropTargetId}
        <!-- Justified: a transient drop target that exists only while a tab is being
             dragged. It has no pointer-free use; the same pairing is available from
             the tab context menu (Open in Split). -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          data-testid="terminal-tab-split-dropzone"
          ondragover={(event) => {
            if (!draggedTabId || !stripPairDropTargetId) return;
            event.preventDefault();
            event.stopPropagation();
            if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
            tabStripDropHint = { splitDropzone: true, mode: "pair" };
          }}
          ondragleave={() => {
            if (tabStripDropHint?.splitDropzone) tabStripDropHint = null;
          }}
          ondrop={(event) => {
            const moving = draggedTabId;
            const targetId = stripPairDropTargetId;
            draggedTabId = null;
            draggedGroupId = null;
            tabDropHint = null;
            tabStripDropHint = null;
            if (!moving || !targetId || moving === targetId) return;
            event.preventDefault();
            event.stopPropagation();
            zoomedPaneId = null;
            const outcome = applyTabStripDrop(tabState.tabs, splitGroups, {
              movingTabId: moving,
              targetTabId: targetId,
              mode: "pair",
              pinnedIds: pinnedTabIds,
            });
            if (outcome.refusedReason) {
              sessionError = outcome.refusedReason;
              return;
            }
            const nextState = outcome.activeTabId
              ? activateTab({ ...tabState, tabs: outcome.tabs }, outcome.activeTabId)
              : { ...tabState, tabs: outcome.tabs };
            tabState = nextState;
            splitGroups = recordGroupActivePane(outcome.groups, nextState.activeId);
          }}
          class="inline-flex items-center gap-1 px-2 my-1 rounded-lg border border-dashed text-micro shrink-0 transition-colors {tabStripDropHint?.splitDropzone ? 'border-accent/90 bg-accent/15 text-accent' : 'border-border/65 text-textMuted'}"
        >
          <Columns2 size={11} class="shrink-0" />
          <span>{activeGroup ? `Join ${activeGroupLabel ?? "Pair"}` : "Split"}</span>
        </div>
      {/if}
      {#if draggedTabId && groupedTabIds.has(draggedTabId)}
        <!-- Justified: a transient drop target that exists only while a grouped tab is
             being dragged. It has no pointer-free use and takes no focus. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          data-testid="terminal-tab-detach-dropzone"
          ondragover={(event) => {
            if (!draggedTabId) return;
            event.preventDefault();
            event.stopPropagation();
            if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
            tabStripDropHint = { detach: true, mode: "after" };
          }}
          ondragleave={() => {
            if (tabStripDropHint?.detach) tabStripDropHint = null;
          }}
          ondrop={(event) => {
            const moving = draggedTabId;
            draggedTabId = null;
            draggedGroupId = null;
            tabDropHint = null;
            tabStripDropHint = null;
            if (!moving) return;
            event.preventDefault();
            event.stopPropagation();
            const detached = detachTabToStripEnd(tabState.tabs, splitGroups, moving);
            if (zoomedPaneId && (zoomedPaneId === moving || !detached.groups.some((g) => hasPane(g.tree, zoomedPaneId ?? "")))) {
              zoomedPaneId = null;
            }
            tabState = { ...tabState, tabs: detached.tabs };
            splitGroups = recordGroupActivePane(detached.groups, tabState.activeId);
          }}
          class="inline-flex items-center gap-1 px-2 my-1 rounded-lg border border-dashed text-micro shrink-0 transition-colors {tabStripDropHint?.detach ? 'border-accent/90 bg-accent/15 text-accent' : 'border-border/65 text-textMuted'}"
        >
          <Minimize2 size={11} class="shrink-0" />
          <span>Detach</span>
        </div>
      {/if}
      </div>
      <ScrollCue target={tabScroller} axis="x" />
      </div>

      <div class="flex items-center gap-1 shrink-0 pl-1.5">
        {#if activeChildTabs.length > 0}
          <button
            type="button"
            class="gp-btn py-0! px-1.5! text-micro! text-danger border-danger/35"
            data-testid="terminal-stop-children-btn"
            title="Terminate and close child sessions spawned by this agent"
            onclick={() => void stopActiveChildren()}
          >Stop children ({activeChildTabs.length})</button>
        {/if}
        {#each pairHandoffTargets as target (target.id)}
          <button
            type="button"
            class="gp-btn py-0! px-1.5! text-micro!"
            data-testid="terminal-pair-handoff-btn"
            title={`Send bounded context handoff to ${target.label}`}
            onclick={() => handOffToPairPartner(target)}
          ><LauncherIcon kind={target.launcher} size={11} class="shrink-0" />Hand off → {target.label}</button>
        {/each}
        <button
          type="button"
          class="gp-icon-btn {followAgent ? 'text-accent bg-accent/15' : ''}"
          aria-label={followAgent ? "Stop following agent edits" : "Follow agent file edits in split"}
          aria-pressed={followAgent}
          disabled={!repoPath}
          data-testid="terminal-follow-agent-toggle"
          title={followAgent ? "Following agent edits in one split — closing that split stops following" : "Follow agent — show modified files & plans in one split pane"}
          onclick={() => { if (followAgent) stopFollowing(); else startFollowing(); }}
        ><Eye size={13} /></button>
        <!-- Justified: captures right-click split menu even when the inner button's default launcher is disabled. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <span
          class="inline-flex shrink-0"
          oncontextmenu={(event) => {
            if (!repoPath || !activeId) return;
            openSplitContextMenu("row", event, event.currentTarget as HTMLElement);
          }}
        >
          <button
            type="button"
            class="gp-icon-btn"
            aria-label="Split terminal right"
            disabled={!canPlainSplit}
            title={`Split right with ${splitWithLabel} (${platformChord("⌘D", "Ctrl+Shift+D", $hostPlatform.os)}) · Right-click to choose what opens`}
            onclick={() => void splitActive("row")}
            oncontextmenu={(event) => {
              if (!repoPath || !activeId) return;
              openSplitContextMenu("row", event, event.currentTarget as HTMLElement);
            }}
            onkeydown={(event) => {
              if (!repoPath || !activeId) return;
              if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                openSplitContextMenu("row", event, event.currentTarget as HTMLElement);
              }
            }}
          ><Columns2 size={13} /></button>
        </span>
        <!-- Justified: captures right-click split menu even when the inner button's default launcher is disabled. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <span
          class="inline-flex shrink-0"
          oncontextmenu={(event) => {
            if (!repoPath || !activeId) return;
            openSplitContextMenu("col", event, event.currentTarget as HTMLElement);
          }}
        >
          <button
            type="button"
            class="gp-icon-btn"
            aria-label="Split terminal down"
            disabled={!canPlainSplit}
            title={`Split down with ${splitWithLabel} (${platformChord("⌘⇧D", "Ctrl+Shift+Alt+D", $hostPlatform.os)}) · Right-click to choose what opens`}
            onclick={() => void splitActive("col")}
            oncontextmenu={(event) => {
              if (!repoPath || !activeId) return;
              openSplitContextMenu("col", event, event.currentTarget as HTMLElement);
            }}
            onkeydown={(event) => {
              if (!repoPath || !activeId) return;
              if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                openSplitContextMenu("col", event, event.currentTarget as HTMLElement);
              }
            }}
          ><Rows2 size={13} /></button>
        </span>
        {#if splitRoot}
          <button
            type="button"
            class="gp-icon-btn px-1.5! text-micro!"
            aria-label={zoomedPaneId ? "Restore split panes" : "Zoom active pane"}
            aria-pressed={Boolean(zoomedPaneId)}
            title={zoomedPaneId ? `Restore split panes (${platformChord("⌘⇧Enter", "Ctrl+Shift+Enter", $hostPlatform.os)}) · Right-click for split options` : `Zoom active pane (${platformChord("⌘⇧Enter", "Ctrl+Shift+Enter", $hostPlatform.os)}) · Right-click for split options`}
            onclick={toggleZoomPane}
            oncontextmenu={(event) => {
              if (!repoPath || !activeId) return;
              openSplitContextMenu(splitRoot?.kind === "branch" ? splitRoot.direction : "row", event, event.currentTarget as HTMLElement);
            }}
            onkeydown={(event) => {
              if (!repoPath || !activeId) return;
              if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                openSplitContextMenu(splitRoot?.kind === "branch" ? splitRoot.direction : "row", event, event.currentTarget as HTMLElement);
              }
            }}
          >{zoomedPaneId ? (hiddenSplitSummary.badgeLabel ? `Unzoom · ${hiddenSplitSummary.badgeLabel}` : "Unzoom") : "Zoom"}</button>
          <button
            type="button"
            class="gp-icon-btn px-1.5! text-micro!"
            aria-label="Close split view"
            title="Unsplit all panes (keeps tabs open) · Right-click for split options"
            onclick={toggleSplit}
            oncontextmenu={(event) => {
              if (!repoPath || !activeId) return;
              openSplitContextMenu(splitRoot?.kind === "branch" ? splitRoot.direction : "row", event, event.currentTarget as HTMLElement);
            }}
            onkeydown={(event) => {
              if (!repoPath || !activeId) return;
              if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                openSplitContextMenu(splitRoot?.kind === "branch" ? splitRoot.direction : "row", event, event.currentTarget as HTMLElement);
              }
            }}
          >1×</button>
        {/if}
        <div class="relative shrink-0" data-terminal-launcher>
          <button
            bind:this={launcherTriggerEl}
            type="button"
            class="gp-btn max-w-32 truncate" data-size="sm"
            aria-label="New session type"
            aria-haspopup="listbox"
            aria-expanded={launcherMenuOpen}
            aria-controls="terminal-launcher-menu"
            title="Choose what the next session or tab opens"
            onclick={() => launcherMenuOpen ? closeChrome() : openLauncherMenu()}
          >
            <LauncherIcon kind={nextLauncher} size={12} class="shrink-0" />
            <span class="terminal-launcher-label truncate">{nextLauncher === "file" ? "Explorer" : launcherLabel(nextLauncher)}</span>
            <ChevronDown size={11} class="shrink-0" />
          </button>
        </div>
        <button
          type="button"
          class="gp-icon-btn"
          data-terminal-new
          disabled={!repoPath || !canCreateKind(nextLauncher)}
          onclick={() => void openBeside(nextLauncher)}
          oncontextmenu={(event) => {
            if (!repoPath || !activeId) return;
            openSplitContextMenu("row", event, event.currentTarget as HTMLElement);
          }}
          aria-label={nextLauncher === "file" ? "New Explorer tab" : `New ${launcherLabel(nextLauncher)} session`}
          title={capacityTitleFor(nextLauncher)}
        ><Plus size={14} /></button>
      </div>
    </div>

    {#if sessionSearchOpen && mode === "shell"}
      <div class="terminal-popover px-3 py-2 bg-surface" aria-label="Find terminal session">
        <input
          bind:this={sessionSearchInput}
          type="search"
          aria-label="Search this repository's terminals"
          placeholder="Search this repository's terminals"
          bind:value={sessionQuery}
          maxlength="128"
          onkeydown={handleSessionSearchKey}
          class="gp-field w-full mb-1.5"
        />
        <div class="max-h-40 overflow-auto" role="listbox" aria-label="Matching terminals">
          {#each filteredTabs as tab, index (tab.id)}
            {@const display = tabDisplayTitle(tab, effectiveAgentStatuses[tab.id], tabBrowserStates[tab.id])}
            {@const splitSlot = splitSlotByTabId[tab.id]}
            {@const groupMeta = groupMetaByTabId[tab.id]}
            <button
              type="button"
              role="option"
              aria-selected={index === sessionHighlight}
              class="flex w-full items-center gap-2 py-0.5 px-1 rounded text-left {index === sessionHighlight ? 'bg-accent/15 text-textPrimary' : 'text-textMuted hover:bg-surfaceHover'}"
              onclick={() => chooseSearchedTab(tab.id)}
            >
              <LauncherIcon kind={tab.launcher} size={12} />
              <span class="flex-1 min-w-0 truncate">
                {display.primary}{#if display.secondary}<span class="text-textMuted"> · {display.secondary}</span>{/if}
              </span>
              <span class="shrink-0 text-micro">{launcherLabel(tab.launcher)}{#if tab.id === tabState.activeId} · Active{/if}{#if groupMeta} · {groupMeta.label} #{groupMeta.slot}{:else if splitSlot} · Pane #{splitSlot}{/if}{#if tabStatuses[tab.id] === "exited"} · Ended{:else if tabStatuses[tab.id] === "error"} · Error{/if}{#if unread.has(tab.id)} · Unread{/if}</span>
            </button>
          {:else}
            <span class="text-textMuted">No terminals match.</span>
          {/each}
        </div>
      </div>
    {/if}

    {#if launcherMenuOpen && mode === "shell"}
      {@const presetAgentKind = preferredPresetAgent()}
      <div
        id="terminal-launcher-menu"
        use:portal={"body"}
        use:popover={launcherMenuDismissal} out:popoverExit
        data-terminal-launcher-popup
        class="fixed w-60 max-w-[calc(100vw-16px)] max-h-[min(23rem,calc(100vh-24px))] overflow-y-auto gp-menu gp-pop p-1.5 text-meta text-textPrimary"
        style="z-index: {LAYERS.MENU}"
        role="dialog"
        aria-label="New session type menu"
        tabindex="-1"
        onkeydown={handleLauncherSearchKey}
      >
        <input
          bind:this={launcherSearchInput}
          type="search"
          aria-label="Search session types"
          placeholder="Search session types"
          bind:value={launcherQuery}
          maxlength="128"
          class="gp-field w-full mb-1"
        />
        <div class="max-h-48 overflow-auto" role="listbox" aria-label="Session types">
          {#each filteredLaunchers as launcher, index (launcher.kind)}
            <button
              type="button"
              role="option"
              aria-selected={launcher.kind === nextLauncher}
              class="gp-menu-item {index === launcherHighlight ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
              onclick={() => chooseLauncher(launcher.kind)}
            >
              <LauncherIcon kind={launcher.kind} size={13} class="shrink-0" />
              <span class="truncate">{launcher.label}</span>
              {#if launcher.kind === nextLauncher}
                <Check size={12} class="ml-auto text-accent shrink-0" />
              {/if}
            </button>
          {:else}
            <span class="block px-2.5 py-1.5 text-textMuted">No session types match.</span>
          {/each}
        </div>
        <div class="gp-menu-sep my-1"></div>
        <div class="flex flex-col gap-0.5" role="group" aria-label="Sidebar and file explorer">
          <span class="gp-label px-2.5 py-0.5">Sidebar &amp; Explorer</span>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-launcher-toggle-explorer"
            disabled={!repoPath}
            title="Dock or hide the dedicated Explorer sidebar beside terminal panes (never forces a 3rd split pane)"
            onclick={() => {
              launcherMenuOpen = false;
              toggleExplorerSidebar();
            }}
          >
            <FolderTree size={13} class="text-accent shrink-0" />
            <span class="truncate">{explorerSidebarOpen ? "Hide Explorer Sidebar" : "Show Explorer Sidebar"}</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-launcher-split-file"
            disabled={!repoPath || (!hasVisibleFilePane && !canCreateKind("file"))}
            title="Open a Explorer & Markdown pane in a side-by-side split"
            onclick={() => {
              launcherMenuOpen = false;
              const err = openFileSplitFor(undefined, null, null, { focus: true });
              if (err) sessionError = err;
            }}
          >
            <Columns2 size={13} class="text-textMuted shrink-0" />
            <span class="truncate">Open Explorer in Split</span>
          </button>
        </div>
        <div class="gp-menu-sep my-1"></div>
        <div class="flex flex-col gap-0.5" role="group" aria-label="Workspace split presets">
          <span class="gp-label px-2.5 py-0.5">Split presets</span>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-preset-shell-pair"
            disabled={!repoPath || tabState.tabs.length + 2 > MAX_STRIP_TABS || $terminalSessions.length + 2 > $terminalSessionLimit}
            title="Open a paired group of two interactive shells side-by-side"
            onclick={openShellPairPreset}
          >
            <LauncherIcon kind="shell" size={13} class="shrink-0" />
            <span class="truncate">New Shell Pair</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-preset-agent-shell-pair"
            disabled={!repoPath || tabState.tabs.length + 2 > MAX_STRIP_TABS || $terminalSessions.length + 2 > $terminalSessionLimit}
            title="Open a paired coding agent beside an interactive shell"
            onclick={openAgentShellPairPreset}
          >
            <LauncherIcon kind={presetAgentKind} size={13} class="shrink-0" />
            <span class="truncate">Agent + Shell Pair</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-preset-agent-diff"
            disabled={!repoPath || tabState.tabs.length + 2 > MAX_STRIP_TABS || $terminalSessions.length + 1 > $terminalSessionLimit}
            title="Open coding agent beside a live Follow-Agent file/diff split"
            onclick={openAgentDiffPreset}
          >
            <LauncherIcon kind="file" size={13} class="shrink-0" />
            <span class="truncate">Agent + Live Diff</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-pair-split-preset"
            disabled={!repoPath || tabState.tabs.length + 2 > MAX_STRIP_TABS || $terminalSessions.length + 2 > $terminalSessionLimit}
            title="Open Claude Code and Antigravity side-by-side in a 50/50 split"
            onclick={openPairSplit}
          >
            <Columns2 size={13} class="shrink-0 text-accent" />
            <span class="truncate">Pair split: Claude + Antigravity</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-preset-fullstack"
            disabled={!repoPath || tabState.tabs.length + ($hostPlatform.browser_sessions ? 3 : 2) > MAX_STRIP_TABS || $terminalSessions.length + 2 > $terminalSessionLimit}
            title="Open coding agent left with Shell + Browser stacked on the right"
            onclick={openFullStackPreset}
          >
            <LauncherIcon kind="browser" size={13} class="shrink-0" />
            <span class="truncate">Full-Stack: Agent + Shell + Browser</span>
          </button>
          <button
            type="button"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-preset-worktree-agent"
            disabled={!repoPath || !onCreateAgentWorktree || creatingWorktreePreset || tabState.tabs.length + 2 > MAX_STRIP_TABS || $terminalSessions.length + 1 > $terminalSessionLimit}
            title="Provision a fresh sibling Git worktree (CoW caches + the code map) and open an agent + live diff split inside it"
            onclick={() => void openWorktreeAgentPreset()}
          >
            <GitBranch size={13} class="shrink-0 text-accent" />
            <span class="truncate">Isolated Worktree Agent Split…</span>
          </button>
        </div>
      </div>
    {/if}

    {#if tabContextMenu && mode === "shell"}
      {@const ctxTab = tabState.tabs.find((candidate) => candidate.id === tabContextMenu?.tabId)}
      {#if ctxTab}
        {@const ctxInActiveSplitGroup = Boolean(splitRoot && hasPane(splitRoot, ctxTab.id))}
        {@const ctxGroup = findGroupForPane(splitGroups, ctxTab.id)}
        {@const ctxGroupMeta = groupMetaByTabId[ctxTab.id]}
        {@const ctxCanPairWithActive = tabState.tabs.length >= 2 && (splitRoot ? !ctxInActiveSplitGroup : ctxTab.id !== activeId) && leafPaneIds(splitRoot).length < MAX_SPLIT_PANES}
        {@const ctxCanReplaceActive = Boolean(splitRoot && !ctxInActiveSplitGroup)}
        {@const ctxJoinTargets = pairJoinTargets(splitGroups, ctxTab.id).filter((target) => !(ctxCanPairWithActive && target.groupId === activeGroup?.id))}
        {@const canSplitPaneWithNew = Boolean(repoPath && leafPaneIds(ctxGroup?.tree ?? null).length < MAX_SPLIT_PANES && canCreateKind(ctxTab.launcher))}
        {@const ctxKindLabel = ctxTab.launcher === "file" ? "Explorer" : launcherLabel(ctxTab.launcher)}
        {@const ctxStripIdx = orderedStripTabs.findIndex((candidate) => candidate.id === ctxTab.id)}
        {@const closableOtherIds = orderedStripTabs.filter((candidate) => candidate.id !== ctxTab.id && !candidate.pinned).map((candidate) => candidate.id)}
        {@const closableRightIds = ctxStripIdx >= 0 ? orderedStripTabs.slice(ctxStripIdx + 1).filter((candidate) => !candidate.pinned).map((candidate) => candidate.id) : []}
        {@const endedTabIds = orderedStripTabs.filter((candidate) => tabStatuses[candidate.id] === "exited").map((candidate) => candidate.id)}
        <div
          {@attach focusFirstContextMenuItem}
          use:portal={"body"}
          use:popover={tabContextMenuPopover} out:popoverExit
          data-terminal-tab-context-menu
          data-testid="terminal-tab-context-menu"
          class="fixed w-56 max-w-[calc(100vw-16px)] max-h-[min(28rem,calc(100vh-24px))] overflow-y-auto gp-menu gp-pop p-1 text-meta text-textPrimary"
          style="z-index: {LAYERS.MENU}"
          role="menu"
          aria-label={`Tab actions for ${tabLabel(ctxTab)}`}
          tabindex="-1"
          onkeydown={handleContextMenuKeydown}
        >
          <div class="px-2.5 py-1 flex items-center gap-1.5 text-micro text-textMuted truncate">
            <LauncherIcon kind={ctxTab.launcher} size={11} class="shrink-0" />
            <span class="font-medium text-textPrimary truncate flex-1">{tabLabel(ctxTab)}</span>
            {#if ctxGroupMeta}
              <span class="gp-chip shrink-0 tabular-nums" data-tone={ctxGroup?.id === activeGroup?.id ? "accent" : "neutral"}>
                {ctxGroupMeta.label} #{ctxGroupMeta.slot}
              </span>
            {/if}
          </div>
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          {#if ctxCanPairWithActive}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-right"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                openTabInSplit(targetId, "row", "after");
              }}
            >
              <Columns2 size={13} class="shrink-0" />
              <span>Open in Split Right</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-left"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                openTabInSplit(targetId, "row", "before");
              }}
            >
              <Columns2 size={13} class="shrink-0" />
              <span>Open in Split Left</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-down"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                openTabInSplit(targetId, "col", "after");
              }}
            >
              <Rows2 size={13} class="shrink-0" />
              <span>Open in Split Down</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-up"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                openTabInSplit(targetId, "col", "before");
              }}
            >
              <Rows2 size={13} class="shrink-0" />
              <span>Open in Split Up</span>
            </button>
          {:else}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-new-right"
              disabled={!canSplitPaneWithNew}
              onclick={() => {
                const targetId = ctxTab.id;
                const launcher = ctxTab.launcher;
                closeChrome();
                void splitPaneWithNewKind(targetId, launcher, "row", "after");
              }}
            >
              <Columns2 size={13} class="shrink-0" />
              <span class="truncate">Split Right with New {ctxKindLabel}</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-split-new-down"
              disabled={!canSplitPaneWithNew}
              onclick={() => {
                const targetId = ctxTab.id;
                const launcher = ctxTab.launcher;
                closeChrome();
                void splitPaneWithNewKind(targetId, launcher, "col", "after");
              }}
            >
              <Rows2 size={13} class="shrink-0" />
              <span class="truncate">Split Down with New {ctxKindLabel}</span>
            </button>
          {/if}
          {#if ctxCanReplaceActive}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                replaceActiveSplitPaneWith(targetId);
              }}
            >
              <SquareTerminal size={13} class="shrink-0" />
              <span>Replace Active Split Pane</span>
            </button>
          {/if}
          {#each ctxJoinTargets as target (target.groupId)}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-join-pair"
              data-group-id={target.groupId}
              onclick={() => {
                const tabId = ctxTab.id;
                closeChrome();
                joinPair(tabId, target);
              }}
            >
              <Columns2 size={13} class="shrink-0" />
              <span class="truncate">{ctxGroup ? "Move to" : "Add to"} {target.label}</span>
            </button>
          {/each}
          {#if ctxGroup}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-flip-orientation"
              onclick={() => {
                const gId = ctxGroup.id;
                const targetId = ctxTab.id;
                closeChrome();
                flipGroupOrientation(gId, targetId);
              }}
            >
              <RotateCw size={13} class="shrink-0" />
              <span>Flip Split Orientation</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-swap-panes"
              onclick={() => {
                const gId = ctxGroup.id;
                const targetId = ctxTab.id;
                closeChrome();
                swapPaneInGroup(gId, targetId);
              }}
            >
              <Columns2 size={13} class="shrink-0" />
              <span>Swap Split Panes</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-equalize"
              onclick={() => {
                const gId = ctxGroup.id;
                closeChrome();
                equalizeGroupSplits(gId);
              }}
            >
              <Rows2 size={13} class="shrink-0" />
              <span>Equalize Split Sizes</span>
            </button>
            {#if ctxGroup.id === activeGroup?.id}
              <button
                type="button"
                role="menuitem"
                class="gp-menu-item text-textMuted"
                data-testid="terminal-tab-ctx-zoom"
                onclick={() => {
                  const targetId = ctxTab.id;
                  closeChrome();
                  zoomSplitPane(targetId);
                }}
              >
                {#if zoomedPaneId === ctxTab.id}
                  <Minimize2 size={13} class="shrink-0" />
                  <span>Restore Split Panes</span>
                {:else}
                  <Maximize2 size={13} class="shrink-0" />
                  <span>Zoom Pane</span>
                {/if}
              </button>
            {/if}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                removePaneFromSplit(targetId);
              }}
            >
              <Minimize2 size={13} class="shrink-0" />
              <span>Remove from Split</span>
            </button>
          {/if}
          {#if ctxTab.launcher === "file" && !explorerSidebarOpen}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-dock-explorer-sidebar"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                dockFileTabToSidebar(targetId);
              }}
            >
              <FolderTree size={13} class="shrink-0 text-accent" />
              <span>Move to Explorer Sidebar</span>
            </button>
          {/if}
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-tab-ctx-duplicate-kind"
            disabled={!repoPath || !canCreateKind(ctxTab.launcher)}
            onclick={() => {
              const launcher = ctxTab.launcher;
              closeChrome();
              void openBeside(launcher);
            }}
          >
            <Plus size={13} class="shrink-0" />
            <span class="truncate">New {ctxKindLabel} Tab</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              tabState = toggleTabPinned(tabState, targetId);
            }}
          >
            {#if ctxTab.pinned}
              <PinOff size={13} class="shrink-0" />
              <span>Unpin Tab</span>
            {:else}
              <Pin size={13} class="shrink-0" />
              <span>Pin Tab</span>
            {/if}
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              startInlineRename(targetId, "tab");
            }}
          >
            <Pencil size={13} class="shrink-0" />
            <span>Rename Tab</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            disabled={orderedStripTabs[0]?.id === ctxTab.id}
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              moveTabInStrip(targetId, -1);
            }}
          >
            <ChevronLeft size={13} class="shrink-0" />
            <span>Move Tab Left</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            disabled={orderedStripTabs.at(-1)?.id === ctxTab.id}
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              moveTabInStrip(targetId, 1);
            }}
          >
            <ChevronRight size={13} class="shrink-0" />
            <span>Move Tab Right</span>
          </button>
          {#if ctxTab.launcher !== "browser" && ctxTab.launcher !== "file" && sessions[ctxTab.id]}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                selectTab(targetId);
                void tick().then(() => sessions[targetId]?.openFind());
              }}
            >
              <Search size={13} class="shrink-0" />
              <span>Find in Terminal</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              onclick={() => {
                const targetId = ctxTab.id;
                closeChrome();
                sessions[targetId]?.clearScrollback();
              }}
            >
              <Trash2 size={13} class="shrink-0" />
              <span>Clear Scrollback</span>
            </button>
          {/if}
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-danger"
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              void requestClose(targetId);
            }}
          >
            <X size={13} class="shrink-0" />
            <span>Close Tab</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-tab-ctx-close-others"
            disabled={closableOtherIds.length === 0}
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              void closeOtherTabs(targetId);
            }}
          >
            <X size={13} class="shrink-0" />
            <span>Close Other Tabs</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-tab-ctx-close-right"
            disabled={closableRightIds.length === 0}
            onclick={() => {
              const targetId = ctxTab.id;
              closeChrome();
              void closeTabsToRight(targetId);
            }}
          >
            <ChevronRight size={13} class="shrink-0" />
            <span>Close Tabs to the Right</span>
          </button>
          {#if endedTabIds.length > 0}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-tab-ctx-close-ended"
              onclick={() => {
                closeChrome();
                void closeEndedTabs();
              }}
            >
              <Trash2 size={13} class="shrink-0" />
              <span>Close Ended Tabs ({endedTabIds.length})</span>
            </button>
          {/if}
        </div>
      {/if}
    {/if}

    {#if splitContextMenu && mode === "shell"}
      {@const splitMenuDir = splitContextMenu.direction}
      {@const splitMenuPos = splitContextMenu.position}
      {@const canSplitHere = canPlainSplit}
      {@const canSplitCustomHere = Boolean(repoPath && activeId && leafPaneIds(splitRoot).length < MAX_SPLIT_PANES)}
      {@const pairableTabs = orderedStripTabs.filter((candidate) => candidate.id !== activeId && !hasPane(splitRoot, candidate.id)).slice(0, 8)}
      <div
        {@attach focusFirstContextMenuItem}
        use:portal={"body"}
        use:popover={splitContextMenuPopover} out:popoverExit
        data-terminal-split-context-menu
        data-testid="terminal-split-context-menu"
        class="fixed w-60 max-w-[calc(100vw-16px)] max-h-[min(28rem,calc(100vh-24px))] overflow-y-auto gp-menu gp-pop p-1 text-meta text-textPrimary"
        style="z-index: {LAYERS.MENU}"
        role="menu"
        aria-label="Split terminal options"
        tabindex="-1"
        onkeydown={handleContextMenuKeydown}
      >
        <div class="px-1.5 py-1 flex flex-col gap-1">
          <span class="gp-label px-1">Placement</span>
          <div
            class="inline-flex w-full items-center gap-0.5 p-0.5 rounded-lg bg-background/70 border border-border/65"
            role="group"
            aria-label="Split placement direction"
          >
            <button
              type="button"
              class="flex-1 px-1.5 py-0.5 rounded text-micro transition-colors {splitMenuDir === 'row' && splitMenuPos === 'after' ? 'bg-accent/15 text-accent font-medium' : 'text-textMuted hover:text-textPrimary'}"
              aria-pressed={splitMenuDir === "row" && splitMenuPos === "after"}
              data-testid="terminal-split-ctx-place-right"
              onclick={() => {
                if (splitContextMenu) splitContextMenu = { ...splitContextMenu, direction: "row", position: "after" };
              }}
            >Right</button>
            <button
              type="button"
              class="flex-1 px-1.5 py-0.5 rounded text-micro transition-colors {splitMenuDir === 'row' && splitMenuPos === 'before' ? 'bg-accent/15 text-accent font-medium' : 'text-textMuted hover:text-textPrimary'}"
              aria-pressed={splitMenuDir === "row" && splitMenuPos === "before"}
              data-testid="terminal-split-ctx-place-left"
              onclick={() => {
                if (splitContextMenu) splitContextMenu = { ...splitContextMenu, direction: "row", position: "before" };
              }}
            >Left</button>
            <button
              type="button"
              class="flex-1 px-1.5 py-0.5 rounded text-micro transition-colors {splitMenuDir === 'col' && splitMenuPos === 'after' ? 'bg-accent/15 text-accent font-medium' : 'text-textMuted hover:text-textPrimary'}"
              aria-pressed={splitMenuDir === "col" && splitMenuPos === "after"}
              data-testid="terminal-split-ctx-place-down"
              onclick={() => {
                if (splitContextMenu) splitContextMenu = { ...splitContextMenu, direction: "col", position: "after" };
              }}
            >Down</button>
            <button
              type="button"
              class="flex-1 px-1.5 py-0.5 rounded text-micro transition-colors {splitMenuDir === 'col' && splitMenuPos === 'before' ? 'bg-accent/15 text-accent font-medium' : 'text-textMuted hover:text-textPrimary'}"
              aria-pressed={splitMenuDir === "col" && splitMenuPos === "before"}
              data-testid="terminal-split-ctx-place-up"
              onclick={() => {
                if (splitContextMenu) splitContextMenu = { ...splitContextMenu, direction: "col", position: "before" };
              }}
            >Up</button>
          </div>
        </div>
        <div class="gp-menu-sep my-0.5" role="separator"></div>
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item text-textMuted"
          data-testid="terminal-split-ctx-right"
          disabled={!canSplitHere}
          onclick={() => {
            closeChrome();
            void splitActive("row", "after");
          }}
        >
          <Columns2 size={13} class="shrink-0" />
          <span class="truncate">Split Right · {splitWithLabel}</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item text-textMuted"
          data-testid="terminal-split-ctx-left"
          disabled={!canSplitHere}
          onclick={() => {
            closeChrome();
            void splitActive("row", "before");
          }}
        >
          <Columns2 size={13} class="shrink-0" />
          <span class="truncate">Split Left · {splitWithLabel}</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item text-textMuted"
          data-testid="terminal-split-ctx-down"
          disabled={!canSplitHere}
          onclick={() => {
            closeChrome();
            void splitActive("col", "after");
          }}
        >
          <Rows2 size={13} class="shrink-0" />
          <span class="truncate">Split Down · {splitWithLabel}</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="gp-menu-item text-textMuted"
          data-testid="terminal-split-ctx-up"
          disabled={!canSplitHere}
          onclick={() => {
            closeChrome();
            void splitActive("col", "before");
          }}
        >
          <Rows2 size={13} class="shrink-0" />
          <span class="truncate">Split Up · {splitWithLabel}</span>
        </button>
        <div class="gp-menu-sep my-0.5" role="separator"></div>
        <span class="gp-label block px-2.5 py-0.5">New Split With…</span>
        {#each dockTabTypes as item (item.kind)}
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid={`terminal-split-ctx-new-${item.kind}`}
            disabled={!canSplitCustomHere || !canCreateKind(item.kind)}
            onclick={() => {
              const targetId = activeId;
              const kind = item.kind;
              const dir = splitMenuDir;
              const pos = splitMenuPos;
              closeChrome();
              if (targetId) void splitPaneWithNewKind(targetId, kind, dir, pos);
            }}
          >
            <LauncherIcon kind={item.kind} size={13} class="shrink-0" />
            <span class="truncate">New {item.label}</span>
          </button>
        {/each}
        {#if pairableTabs.length > 0}
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          <span class="gp-label block px-2.5 py-0.5">Pair With Open Tab…</span>
          <div class="max-h-36 overflow-y-auto">
            {#each pairableTabs as candidate (candidate.id)}
              <button
                type="button"
                role="menuitem"
                class="gp-menu-item text-textMuted"
                data-testid="terminal-split-ctx-pair-tab"
                disabled={leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES}
                onclick={() => {
                  const targetId = candidate.id;
                  const dir = splitMenuDir;
                  const pos = splitMenuPos;
                  closeChrome();
                  openTabInSplit(targetId, dir, pos);
                }}
              >
                <LauncherIcon kind={candidate.launcher} size={12} class="shrink-0" />
                <span class="truncate">{tabLabel(candidate)}</span>
              </button>
            {/each}
          </div>
        {/if}
        {#if activeGroup && splitRoot}
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          <span class="gp-label block px-2.5 py-0.5">Active Split Layout</span>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-split-ctx-flip"
            onclick={() => {
              const gId = activeGroup.id;
              const paneTarget = activeId;
              closeChrome();
              flipGroupOrientation(gId, paneTarget);
            }}
          >
            <RotateCw size={13} class="shrink-0" />
            <span>Flip Split Orientation</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-split-ctx-swap"
            onclick={() => {
              const gId = activeGroup.id;
              const paneTarget = activeId;
              closeChrome();
              swapPaneInGroup(gId, paneTarget);
            }}
          >
            <Columns2 size={13} class="shrink-0" />
            <span>Swap Split Panes</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-split-ctx-equalize"
            onclick={() => {
              const gId = activeGroup.id;
              closeChrome();
              equalizeGroupSplits(gId);
            }}
          >
            <Rows2 size={13} class="shrink-0" />
            <span>Equalize Split Sizes</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-split-ctx-zoom"
            onclick={() => {
              closeChrome();
              toggleZoomPane();
            }}
          >
            {#if zoomedPaneId}
              <Minimize2 size={13} class="shrink-0" />
              <span>Restore Split Panes</span>
            {:else}
              <Maximize2 size={13} class="shrink-0" />
              <span>Zoom Active Pane</span>
            {/if}
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-split-ctx-unsplit"
            onclick={() => {
              closeChrome();
              closeSplit();
            }}
          >
            <Minimize2 size={13} class="shrink-0" />
            <span>Ungroup Split (Keep Tabs)</span>
          </button>
          {#if splitGroups.length > 1}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              data-testid="terminal-split-ctx-unsplit-all"
              onclick={() => {
                closeChrome();
                ungroupAllPairs();
              }}
            >
              <X size={13} class="shrink-0" />
              <span>Ungroup All Pairs ({splitGroups.length})</span>
            </button>
          {/if}
        {/if}
      </div>
    {/if}

    {#if groupContextMenu && mode === "shell"}
      {@const targetGroup = splitGroups.find((candidate) => candidate.id === groupContextMenu?.groupId)}
      {#if targetGroup}
        {@const targetIdx = splitGroups.indexOf(targetGroup)}
        {@const targetLabel = splitGroupDisplayLabel(targetGroup, targetIdx, splitGroups.length)}
        {@const groupLeaves = leafPaneIds(targetGroup.tree)}
        {@const allGroupPinned = groupLeaves.length > 0 && groupLeaves.every((leafId) => pinnedTabIds.has(leafId))}
        {@const groupSegIdx = tabStripSegments.findIndex((seg) => seg.kind === "group" && seg.group.id === targetGroup.id)}
        <div
          {@attach focusFirstContextMenuItem}
          use:portal={"body"}
          use:popover={groupContextMenuPopover} out:popoverExit
          data-terminal-group-context-menu
          data-testid="terminal-group-context-menu"
          class="fixed w-56 max-w-[calc(100vw-16px)] gp-menu gp-pop p-1 text-meta text-textPrimary"
          style="z-index: {LAYERS.MENU}"
          role="menu"
          aria-label={`Pair actions for ${targetLabel}`}
          tabindex="-1"
          onkeydown={handleContextMenuKeydown}
        >
          <div class="px-2.5 py-1 flex items-center gap-1.5 text-micro text-textMuted truncate">
            <Columns2 size={11} class="shrink-0 text-accent" />
            <span class="font-medium text-textPrimary truncate">{targetLabel}</span>
            <span class="ml-auto text-textMuted shrink-0">{groupLeaves.length} panes</span>
          </div>
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          {#if activeGroup?.id !== targetGroup.id}
            <button
              type="button"
              role="menuitem"
              class="gp-menu-item text-textMuted"
              onclick={() => {
                const gId = targetGroup.id;
                closeChrome();
                activateSplitGroup(gId);
              }}
            >
              <Eye size={13} class="shrink-0 text-accent" />
              <span class="truncate">Switch to {targetLabel}</span>
            </button>
          {/if}
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-rename"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              startGroupInlineRename(gId);
            }}
          >
            <Pencil size={13} class="shrink-0" />
            <span>Rename Pair…</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-flip"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              flipGroupOrientation(gId);
            }}
          >
            <RotateCw size={13} class="shrink-0" />
            <span>Flip Split Orientation</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-swap"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              swapPaneInGroup(gId);
            }}
          >
            <Columns2 size={13} class="shrink-0" />
            <span>Swap Panes in Pair</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-equalize"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              equalizeGroupSplits(gId);
            }}
          >
            <Rows2 size={13} class="shrink-0" />
            <span>Equalize Split Sizes</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-pin-all"
            onclick={() => {
              const gId = targetGroup.id;
              const nextPinned = !allGroupPinned;
              closeChrome();
              setGroupPinned(gId, nextPinned);
            }}
          >
            {#if allGroupPinned}
              <PinOff size={13} class="shrink-0" />
              <span>Unpin All Panes in Pair</span>
            {:else}
              <Pin size={13} class="shrink-0" />
              <span>Pin All Panes in Pair</span>
            {/if}
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            disabled={groupSegIdx <= 0}
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              moveGroupInStrip(gId, -1);
            }}
          >
            <ChevronLeft size={13} class="shrink-0" />
            <span>Move Pair Left</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            disabled={groupSegIdx < 0 || groupSegIdx >= tabStripSegments.length - 1}
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              moveGroupInStrip(gId, 1);
            }}
          >
            <ChevronRight size={13} class="shrink-0" />
            <span>Move Pair Right</span>
          </button>
          <div class="gp-menu-sep my-0.5" role="separator"></div>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-textMuted"
            data-testid="terminal-group-ctx-ungroup"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              ungroupPair(gId);
            }}
          >
            <Minimize2 size={13} class="shrink-0" />
            <span>Ungroup Pair (Keep Tabs)</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="gp-menu-item text-danger"
            data-testid="terminal-group-ctx-close-all"
            onclick={() => {
              const gId = targetGroup.id;
              closeChrome();
              void closePairTabs(gId);
            }}
          >
            <X size={13} class="shrink-0" />
            <span>Close All Tabs in Pair</span>
          </button>
        </div>
      {/if}
    {/if}

    {#if tabOptions && mode === "shell" && activeId}
      <form class="terminal-popover px-3 py-1.5 flex flex-wrap items-center gap-2 bg-surface" onsubmit={saveTabName}>
        <input aria-label="Terminal tab name" bind:value={renameValue} maxlength="64" placeholder="Use automatic title" class="gp-field min-w-0 w-36" />
        <button type="submit" class="gp-btn" data-size="sm">Rename</button>
        {#if activeTab}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="terminal-tab-pin-toggle"
            aria-pressed={Boolean(activeTab.pinned)}
            title={activeTab.pinned ? "Unpin tab so background tab clicks can replace its split slot" : "Pin tab in split so selecting background tabs will not evict it"}
            onclick={() => {
              if (activeId) tabState = toggleTabPinned(tabState, activeId);
            }}
          >
            {#if activeTab.pinned}<PinOff size={11} /> Unpin{:else}<Pin size={11} /> Pin{/if}
          </button>
        {/if}
        {#if activeGroup}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="terminal-options-ungroup-btn"
            onclick={() => {
              ungroupPair(activeGroup.id);
              tabOptions = false;
            }}
          >
            Ungroup pair
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab left" disabled={orderedStripTabs[0]?.id === activeId} onclick={() => activeId && moveTabInStrip(activeId, -1)}><ChevronLeft size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab right" disabled={orderedStripTabs.at(-1)?.id === activeId} onclick={() => activeId && moveTabInStrip(activeId, 1)}><ChevronRight size={13} /></button>
        {#if !activeIsNonTerminal}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="terminal-tab-opt-copy-on-select"
            aria-pressed={$interfaceStore.terminalCopyOnSelect}
            title="Copy selected terminal text to the clipboard as soon as selection finishes"
            onclick={() => {
              interfaceStore.setTerminalCopyOnSelect(!$interfaceStore.terminalCopyOnSelect);
            }}
          >
            Select-to-Copy: {$interfaceStore.terminalCopyOnSelect ? "On" : "Off"}
          </button>
          <select aria-label="Terminal output actions" onchange={outputAction} class="gp-select">
            <option value="">Output actions</option><option value="selection">Copy selection</option><option value="copy-md">Copy as Markdown</option><option value="last-copy">Copy last command output</option><option value="last-cmd">Copy last command</option><option value="run-last-new-shell">Run in new split shell</option><option value="last-split">Open last output in split</option><option value="copy">Copy retained output</option><option value="export">Export retained output…</option>
          </select>
        {/if}
      </form>
    {/if}

    <div class="flex-1 min-h-0 min-w-0 flex overflow-hidden" class:hidden={mode !== "shell"}>
      {#if explorerSidebarOpen && repoPath}
        <aside
          bind:this={explorerAsideEl}
          data-testid="terminal-explorer-sidebar"
          aria-label="Terminal file explorer sidebar"
          style:width={`${explorerSidebarWidth}px`}
          class="gp-float-card shrink-0 flex flex-col min-h-0 max-w-[45%] overflow-hidden"
          onpointerdowncapture={(event) => {
            lastExplorerMod = { altKey: event.altKey, metaOrCtrlKey: event.metaKey || event.ctrlKey };
          }}
        >
          <!-- The rail below carries its own "Explorer" heading and the dock
               section is called Files, so this row is only where clicks go. -->
          <div class="@container h-8 shrink-0 px-2 bg-surface/80 flex items-center justify-between gap-1 select-none">
            <div class="flex items-center gap-1.5 min-w-0">
              <span class="text-micro text-textMuted shrink-0 @max-[13rem]:hidden" aria-hidden="true">Open in</span>
              <div class="gp-segmented min-w-0" data-size="sm" role="group" aria-label="Open files in">
                <button
                  type="button"
                  data-testid="terminal-explorer-target-tab"
                  aria-pressed={explorerOpenTarget === "tab"}
                  data-active={explorerOpenTarget === "tab" ? "true" : "false"}
                  class="gp-seg-btn inline-flex items-center gap-1"
                  title={`Open clicked files in a Files tab (or update an already-visible file pane) without adding a 3rd split pane · Alt+Click splits · ${codeViewClick} opens in Code view`}
                  onclick={() => setExplorerOpenTarget("tab")}
                >
                  <FileText size={11} class="shrink-0" aria-hidden="true" />
                  <span class="@max-2xs:sr-only">Tab</span>
                </button>
                <button
                  type="button"
                  data-testid="terminal-explorer-target-split"
                  aria-pressed={explorerOpenTarget === "split"}
                  data-active={explorerOpenTarget === "split" ? "true" : "false"}
                  class="gp-seg-btn inline-flex items-center gap-1"
                  title={`Open clicked files side-by-side in split view · ${codeViewClick} opens in Code view`}
                  onclick={() => setExplorerOpenTarget("split")}
                >
                  <Columns2 size={11} class="shrink-0" aria-hidden="true" />
                  <span class="@max-2xs:sr-only">Split</span>
                </button>
                {#if onOpenInCodeView}
                  <button
                    type="button"
                    data-testid="terminal-explorer-target-code"
                    aria-pressed={explorerOpenTarget === "code"}
                    data-active={explorerOpenTarget === "code" ? "true" : "false"}
                    class="gp-seg-btn inline-flex items-center gap-1"
                    title={`Open clicked files in the full Code view · ${codeViewClick} opens a Files tab · Alt+Click splits`}
                    onclick={() => setExplorerOpenTarget("code")}
                  >
                    <ExternalLink size={11} class="shrink-0" aria-hidden="true" />
                    <span class="@max-2xs:sr-only">Code</span>
                  </button>
                {/if}
              </div>
            </div>
            <div class="flex items-center gap-0.5 shrink-0">
              {#if activeExplorerFile}
                <button
                  type="button"
                  class="gp-icon-btn"
                  data-size="sm"
                  data-testid="terminal-explorer-send-file"
                  aria-label={`Send @${activeExplorerFile} to session`}
                  title={`Attach @${activeExplorerFile} to active agent or shell`}
                  onclick={() => activeExplorerFile && sendReferenceToAgent(`@${activeExplorerFile} `)}
                >
                  <Send size={11} />
                </button>
              {/if}
              <button
                type="button"
                class="gp-icon-btn"
                data-size="sm"
                aria-label="Close file explorer sidebar"
                title="Hide docked Explorer sidebar"
                onclick={toggleExplorerSidebar}
              >
                <PanelLeftClose size={12} />
              </button>
            </div>
          </div>
          <div class="flex-1 min-h-0 overflow-hidden">
            <ExplorerRail
              repoPath={activeTab?.checkout ?? repoPath}
              selectedFile={activeExplorerFile}
              onSelectFile={(p) => openFromExplorerSidebar(p, null, null)}
              onPinFile={(p) => openFromExplorerSidebar(p, null, null, true)}
              onSelectMatch={(p, line, col) => openFromExplorerSidebar(p, line, col)}
            />
          </div>
        </aside>
        <!-- Justified: WAI-ARIA 1.2 Window Splitter pattern requires a focusable separator with arrow-key resizing; Svelte's static check classifies role="separator" as non-interactive. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div
          role="separator"
          tabindex="0"
          aria-orientation="vertical"
          aria-label="Resize file explorer sidebar (double-click to reset)"
          aria-valuenow={explorerSidebarWidth}
          aria-valuemin={TERMINAL_EXPLORER_WIDTH_MIN}
          aria-valuemax={TERMINAL_EXPLORER_WIDTH_MAX}
          data-testid="terminal-explorer-sidebar-sash"
          class="w-1.5 shrink-0 flex items-center justify-center cursor-col-resize select-none group"
          onpointerdown={startExplorerResize}
          ondblclick={() => {
            terminalExplorerSidebar.setWidth(TERMINAL_EXPLORER_WIDTH_DEFAULT);
          }}
          onkeydown={handleExplorerSashKeydown}
        >
          <span class="pointer-events-none w-px h-full transition-colors bg-border group-hover:bg-accent/70 group-hover:w-0.5 group-focus-visible:bg-accent"></span>
        </div>
      {/if}
    <div class="terminal-panes flex-1 min-h-0 min-w-0 relative" data-split={splitRoot !== null ? "true" : "false"}>
      <!-- Pane boxes are fractions of this canvas, which is never shorter
           than every pane's minimum: a short dock scrolls its panes instead
           of squeezing a split to grids with no rows. Sash drags measure it. -->
      <div bind:this={panesContainerEl} class="terminal-panes-canvas relative h-full" style:min-height={`${paneCanvasMinHeight}px`}>
      {#if !repoPath}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Open a repository to start a shell.
        </div>
      {:else if tabState.tabs.length === 0}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-textMuted text-xs">
          <span>No sessions open.</span>
          <div class="flex flex-wrap items-center justify-center gap-2 px-4">
            {#each sessionTypes as launcher (launcher.kind)}
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                data-testid={`terminal-empty-launch-${launcher.kind}`}
                data-terminal-new-browser={launcher.kind === "browser" ? "" : undefined}
                disabled={!canCreateKind(launcher.kind)}
                title={capacityTitleFor(launcher.kind)}
                onclick={() => newTab(launcher.kind)}
              >
                <LauncherIcon kind={launcher.kind} size={12} /> {launcher.label}
              </button>
            {/each}
            <button
              type="button"
              class="gp-btn"
              data-size="sm"
              data-testid="terminal-empty-launch-file"
              disabled={!canCreateKind("file")}
              title={capacityTitleFor("file")}
              onclick={() => void openBeside("file")}
            >
              <LauncherIcon kind="file" size={12} /> Explorer
            </button>
          </div>
        </div>
      {:else}
        {#each tabState.tabs as tab (tab.id)}
          <!-- Absolute so hidden siblings keep their box: a session laid out
               at zero height would have its xterm reflow to a 1-row grid and
               tell the shell about it. -->
          {@const isVisiblePane = splitIds ? splitIds.includes(tab.id) : tab.id === tabState.activeId}
          {@const isFocusedPane = tab.id === tabState.activeId}
          {@const paneSlot = splitSlotByTabId[tab.id]}
          {@const paneDisplay = tabDisplayTitle(tab, effectiveAgentStatuses[tab.id], tabBrowserStates[tab.id])}
          {@const paneOscBadge = oscStatusBadge(effectiveAgentStatuses[tab.id])}
          {@const paneSubdirRaw = tab.startDir ? relativeToRepo(tab.startDir, tab.checkout ?? repoPath ?? "") : null}
          {@const paneSubdir = paneSubdirRaw && paneSubdirRaw !== "." ? paneSubdirRaw : null}
          {@const paneCheckout = tab.checkout ? (tab.checkout.split(/[\\/]/).filter(Boolean).pop() ?? tab.checkout) : null}
          {@const paneLocation = paneCheckout ?? paneSubdir}
          {@const isRenamingThisPane = inlineRenameTabId === tab.id && inlineRenameSurface === "pane"}
          <div
            use:paneDropZone={tab.id}
            class="terminal-pane absolute overflow-hidden flex flex-col {splitRoot && !zoomedPaneId ? (isFocusedPane ? 'ring-1 ring-inset ring-accent/65' : 'ring-1 ring-inset ring-border/35') : ''}"
            style={paneBoxStyle(tab.id)}
            data-pane-id={tab.id}
            data-split-active={splitRoot && isFocusedPane ? "true" : "false"}
            class:hidden={!isVisiblePane}
            onfocusin={() => { if (activeId !== tab.id) selectTab(tab.id); }}
            onpointerdown={() => { if (splitRoot && activeId !== tab.id) selectTab(tab.id); }}
            ondragover={(event) => {
              if (draggedTabId) {
                if (draggedTabId === tab.id) return;
                const hint = resolveTabDropZone(event, tab.id);
                if (!hint) return;
                if (hint.mode !== "swap" && !hasPane(splitRoot, draggedTabId) && leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES) return;
                event.preventDefault();
                if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
                tabDropHint = hint;
                return;
              }
              const inApp = terminalDropZones.currentInAppDrag();
              const isFileDrag = Boolean(
                inApp || event.dataTransfer?.types.includes("application/x-gitpulse-filepath"),
              );
              if (!isFileDrag) return;
              event.preventDefault();
              if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
              const el = event.currentTarget;
              if (!(el instanceof HTMLElement)) return;
              const allowEdges = Boolean(inApp?.filesOnly && canEdgeSplitDrop(inApp.paths, tab.id));
              const region = classifyDropRegion(
                { x: event.clientX, y: event.clientY },
                el.getBoundingClientRect(),
                allowEdges,
              );
              fileDropHint = region ? { paneId: tab.id, region } : null;
            }}
            ondragleave={(event) => {
              if (event.currentTarget instanceof HTMLElement && event.currentTarget.contains(event.relatedTarget as Node | null)) {
                return;
              }
              if (tabDropHint?.paneId === tab.id) tabDropHint = null;
              if (fileDropHint?.paneId === tab.id) fileDropHint = null;
            }}
            ondrop={(event) => {
              if (draggedTabId) {
                if (draggedTabId === tab.id) return;
                const hint = resolveTabDropZone(event, tab.id);
                if (!hint || (hint.mode !== "swap" && !hasPane(splitRoot, draggedTabId) && leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES)) {
                  draggedTabId = null;
                  tabDropHint = null;
                  return;
                }
                event.preventDefault();
                const moving = draggedTabId;
                draggedTabId = null;
                tabDropHint = null;
                tabStripDropHint = null;
                zoomedPaneId = null;
                if (hint.mode === "swap") {
                  if (!splitRoot) {
                    selectTab(moving);
                  } else if (hasPane(splitRoot, moving) && hasPane(splitRoot, tab.id)) {
                    const swapped = swapPanes(splitRoot, moving, tab.id);
                    applyGroupsAndCoLocate(
                      upsertSplitGroup(splitGroups, swapped, {
                        groupId: activeGroup?.id,
                        name: activeGroup?.name,
                        activePaneId: moving,
                      }),
                      activateTab(tabState, moving),
                    );
                  } else if (hasPane(splitRoot, tab.id)) {
                    const baseGroups = removePaneFromGroups(splitGroups, moving);
                    const refreshedTarget = findGroupForPane(baseGroups, tab.id);
                    const baseTree = refreshedTarget?.tree ?? splitRoot;
                    let nextTree: SplitNode | null = baseTree;
                    if (pinnedTabIds.has(tab.id)) {
                      const choice = chooseSplitReplacementSlot(baseTree, tab.id, pinnedTabIds);
                      if (choice?.mode === "replace") nextTree = replacePane(baseTree, choice.targetPaneId, moving);
                      else if (choice?.mode === "split") nextTree = splitPane(baseTree, choice.anchorPaneId, moving, "row", "after", 0.5);
                      else {
                        sessionError = "That split pane is pinned and all 8 split slots are full — unpin or remove a pane first.";
                        return;
                      }
                    } else {
                      nextTree = replacePane(baseTree, tab.id, moving);
                    }
                    applyGroupsAndCoLocate(
                      upsertSplitGroup(baseGroups, nextTree, {
                        groupId: refreshedTarget?.id ?? activeGroup?.id,
                        name: refreshedTarget?.name ?? activeGroup?.name,
                        activePaneId: moving,
                      }),
                      activateTab(tabState, moving),
                    );
                  }
                } else {
                  const baseGroups = removePaneFromGroups(splitGroups, moving);
                  const refreshedTarget = findGroupForPane(baseGroups, tab.id);
                  const nextTree = movePaneToSplit(
                    refreshedTarget?.tree ?? null,
                    moving,
                    tab.id,
                    hint.direction,
                    hint.position,
                  );
                  applyGroupsAndCoLocate(
                    upsertSplitGroup(baseGroups, nextTree, {
                      groupId: refreshedTarget?.id,
                      name: refreshedTarget?.name,
                      activePaneId: moving,
                    }),
                    activateTab(tabState, moving),
                  );
                }
                return;
              }
              const inApp = terminalDropZones.currentInAppDrag();
              const transferred = event.dataTransfer?.getData("application/x-gitpulse-filepath")?.trim();
              const paths = inApp?.paths ?? (transferred ? [transferred] : []);
              if (paths.length === 0) return;
              event.preventDefault();
              const el = event.currentTarget;
              const filesOnly = inApp?.filesOnly ?? true;
              const allowEdges = Boolean(filesOnly && canEdgeSplitDrop(paths, tab.id));
              const region =
                el instanceof HTMLElement
                  ? classifyDropRegion(
                      { x: event.clientX, y: event.clientY },
                      el.getBoundingClientRect(),
                      allowEdges,
                    )
                  : "center";
              fileDropHint = null;
              terminalDropZones.endInAppDrag();
              handleFileDropOnPane(tab.id, paths, region ?? "center", filesOnly);
            }}
            role="tabpanel"
            tabindex="-1"
            id={`terminal-pane-${tab.id}`}
            aria-labelledby={`terminal-tab-${tab.id}`}
            aria-label={tabLabel(tab)}
          >
            {#if splitRoot}
              <!-- Justified: a drag handle and context-menu surface for the pane. The
                   focusable controls (rename input, buttons) sit inside it, and dragstart
                   declines when the event comes from one of them. -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="h-7 shrink-0 px-2 flex items-center gap-1.5 min-w-0 select-none text-micro transition-colors {isRenamingThisPane ? '' : 'cursor-grab active:cursor-grabbing'} {isFocusedPane ? 'bg-accent/10 text-textPrimary' : 'bg-surface/80 text-textMuted'}"
                data-testid="terminal-split-pane-header"
                data-pane-focused={isFocusedPane ? "true" : "false"}
                data-pane-pinned={tab.pinned ? "true" : "false"}
                draggable={!isRenamingThisPane}
                ondragstart={(event) => {
                  if (event.target instanceof HTMLElement && event.target.closest("button, input")) {
                    event.preventDefault();
                    return;
                  }
                  draggedGroupId = null;
                  draggedTabId = tab.id;
                  event.dataTransfer?.setData("application/x-gitpulse-terminal-tab", tab.id);
                  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
                }}
                ondragend={() => {
                  draggedTabId = null;
                  draggedGroupId = null;
                  tabDropHint = null;
                }}
                ondblclick={(event) => {
                  if (event.target instanceof HTMLElement && event.target.closest("button, input, [data-testid='terminal-split-pane-title']")) {
                    return;
                  }
                  zoomSplitPane(tab.id);
                }}
                oncontextmenu={(event) => {
                  if (isRenamingThisPane) return;
                  if (event.target instanceof HTMLElement && event.target.closest("input")) return;
                  openTabContextMenu(tab.id, event, event.currentTarget as HTMLElement);
                }}
              >
                <span
                  aria-hidden="true"
                  class="w-1.5 h-1.5 rounded-full shrink-0 {isFocusedPane ? 'bg-accent' : 'bg-border'}"
                ></span>
                {#if paneSlot}
                  <span class="font-mono tabular-nums shrink-0 {isFocusedPane ? 'text-accent font-semibold' : 'text-textMuted'}">
                    #{paneSlot}
                  </span>
                {/if}
                <LauncherIcon kind={tab.launcher} size={11} class="shrink-0" />
                {#if tab.parentSessionId}
                  <span class="text-accent shrink-0" title="Child session spawned by parent agent">↳</span>
                {/if}
                {#if isRenamingThisPane}
                  <input
                    bind:this={inlineRenameInput}
                    bind:value={inlineRenameDraft}
                    data-inline-rename
                    data-testid="terminal-inline-rename-input"
                    aria-label="Rename split pane"
                    maxlength="64"
                    class="gp-field h-5 px-1.5 py-0 min-w-16 w-36 text-micro"
                    onpointerdown={(event) => event.stopPropagation()}
                    onclick={(event) => event.stopPropagation()}
                    onkeydown={(event) => {
                      if (isImeComposition(event)) return;
                      event.stopPropagation();
                      if (event.key === "Enter") {
                        event.preventDefault();
                        commitInlineRename(tab.id, true);
                      } else if (event.key === "Escape") {
                        event.preventDefault();
                        cancelInlineRename(true);
                      }
                    }}
                    onblur={() => commitInlineRename(tab.id)}
                  />
                {:else}
                  <span
                    data-testid="terminal-split-pane-title"
                    class="split-pane-primary truncate min-w-8 {isFocusedPane ? 'font-semibold text-textPrimary' : 'font-medium text-textMuted'}"
                    title={`${launcherLabel(tab.launcher)} — ${paneDisplay.fullTitle} (double-click to rename)`}
                    ondblclick={(event) => {
                      event.stopPropagation();
                      startInlineRename(tab.id, "pane");
                    }}
                  >
                    {paneDisplay.primary}
                  </span>
                {/if}
                {#if paneDisplay.secondary}
                  <span class="split-pane-secondary truncate max-w-36 text-textMuted">· {paneDisplay.secondary}</span>
                {/if}
                {#if paneLocation}
                  <span
                    class="split-pane-location px-1 rounded border border-border/65 bg-surface font-mono text-textMuted truncate max-w-28 shrink-0"
                    title={tab.checkout ?? paneSubdir ?? undefined}
                  >
                    {paneLocation}
                  </span>
                {/if}
                {#if tabStatuses[tab.id] === "exited"}
                  <span class="text-textMuted shrink-0">· Ended</span>
                {:else if tabStatuses[tab.id] === "error"}
                  <span class="text-danger shrink-0">· Error</span>
                {:else if paneOscBadge}
                  <span class="shrink-0 {paneOscBadge.cls}" title={effectiveAgentStatuses[tab.id]?.detail ?? paneOscBadge.label}>
                    · {paneOscBadge.label}
                  </span>
                {/if}
                <span class="flex-1"></span>
                {#if zoomedPaneId === tab.id && hiddenSplitSummary.badgeLabel}
                  <button
                    type="button"
                    class="gp-chip shrink-0 cursor-pointer"
                    data-tone={hiddenSplitSummary.tone ?? "accent"}
                    data-testid="terminal-zoomed-hidden-alert"
                    onpointerdown={(event) => event.stopPropagation()}
                    onclick={(event) => {
                      event.stopPropagation();
                      revealHiddenSplitUrgent();
                    }}
                    title="Hidden split sibling activity — click to restore split panes and show"
                  >
                    {hiddenSplitSummary.badgeLabel}
                  </button>
                {/if}
                {#if isFocusedPane}
                  <span class="split-pane-active-marker gp-chip shrink-0" data-tone="accent" data-testid="terminal-split-active-badge">
                    Active
                  </span>
                {/if}
                <button
                  type="button"
                  class="gp-icon-btn shrink-0 {tab.pinned ? 'text-accent bg-accent/15' : ''}"
                  data-size="sm"
                  data-testid="terminal-split-pin-btn"
                  aria-pressed={Boolean(tab.pinned)}
                  onpointerdown={(event) => event.stopPropagation()}
                  onclick={(event) => {
                    event.stopPropagation();
                    tabState = toggleTabPinned(tabState, tab.id);
                  }}
                  aria-label={tab.pinned ? `Unpin ${tabLabel(tab)} pane` : `Pin ${tabLabel(tab)} pane`}
                  title={tab.pinned ? "Pinned in split — background tab selection will not evict this pane" : "Pin pane so selecting background tabs will not evict it"}
                >
                  {#if tab.pinned}
                    <PinOff size={11} />
                  {:else}
                    <Pin size={11} />
                  {/if}
                </button>
                <button
                  type="button"
                  class="gp-icon-btn shrink-0"
                  data-size="sm"
                  onpointerdown={(event) => event.stopPropagation()}
                  onclick={(event) => {
                    event.stopPropagation();
                    zoomSplitPane(tab.id);
                  }}
                  aria-label={zoomedPaneId === tab.id ? `Restore ${tabLabel(tab)} split size` : `Zoom ${tabLabel(tab)} pane`}
                  title={zoomedPaneId === tab.id ? "Restore split panes (or double-click header)" : "Zoom this pane (or double-click header)"}
                >
                  {#if zoomedPaneId === tab.id}
                    <Minimize2 size={11} />
                  {:else}
                    <Maximize2 size={11} />
                  {/if}
                </button>
                <button
                  type="button"
                  class="gp-icon-btn shrink-0"
                  data-size="sm"
                  onpointerdown={(event) => event.stopPropagation()}
                  onclick={(event) => {
                    event.stopPropagation();
                    removePaneFromSplit(tab.id);
                  }}
                  aria-label={`Remove ${tabLabel(tab)} from split`}
                  title="Keep session running in background tab and remove from split view"
                >
                  <X size={11} />
                </button>
              </div>
            {/if}
            <div class="relative flex-1 min-h-0 min-w-0">
            {#if tab.launcher === "browser"}
            <BrowserSession
              bind:this={browsers[tab.id]}
              tabId={tab.id}
              onscreen={paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              initialUrl={tab.url}
              onUrl={(url) => (tabState = setTabUrl(tabState, tab.id, url))}
              onPageState={(pageState) => {
                const prev = tabBrowserStates[tab.id];
                if (prev?.loading === pageState.loading && prev?.error === pageState.error) return;
                tabBrowserStates = { ...tabBrowserStates, [tab.id]: pageState };
              }}
              onSendToAgent={sendReferenceToAgent}
            />
            {:else if tab.launcher === "file"}
            <TerminalFilePane
              bind:this={filePanes[tab.id]}
              repoPath={tab.checkout ?? repoPath}
              tabId={tab.id}
              filePath={tab.filePath}
              fallbackPath={tab.fallbackPath}
              snippet={tab.snippet ?? null}
              revealLine={tab.revealLine ?? null}
              revealColumn={tab.revealColumn ?? null}
              revealSeq={tab.revealSeq ?? 0}
              defaultExplorerOpen={explorerSidebarOpen ? false : undefined}
              onscreen={paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })}
              onSelectFile={(fp, line, col) => selectInFilePane(tab, fp, line ?? null, col ?? null)}
              onKeepFile={(fp) => keepFileShown(fp, tab.checkout, tab.id)}
              onResolvedPath={(fp) => (tabState = setTabFile(tabState, tab.id, fp, null, null, { preserveReveal: true }))}
              onOpenInCodeView={onOpenInCodeView}
              onDockToSidebar={explorerSidebarOpen ? undefined : () => dockFileTabToSidebar(tab.id)}
              onSendToAgent={sendReferenceToAgent}
            />
            {:else}
            <TerminalSession
              taskRunId={tab.taskRunId}
              resume={tab.resume}
              attachSessionId={tab.attachSessionId}
              bind:this={sessions[tab.id]}
              repoPath={repoPath}
              checkout={tab.checkout}
              onAdmitted={() => admitTab(tab.id)}
              onRefused={() => refuseTab(tab.id)}
              tabId={tab.id}
              launcher={tab.launcher}
              initialPrompt={tab.initialPrompt}
              initialCommand={tab.initialCommand}
              initialAutoWake={tab.autoWake ?? false}
              purpose={tab.purpose}
              startDir={tab.startDir}
              active={visible && tab.id === tabState.activeId && mode === "shell"}
              onscreen={paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              onChord={handleChord}
              onOpenFileSplit={(path, line, column, fallbackPath) => Boolean(openFileSplitFor(path, line, column, { focus: false, anchorId: tab.id, checkout: tab.checkout, fallbackPath }) === undefined)}
              onOpenSnippetSplit={(title, content) => openSnippetSplitFor(title, content) === undefined}
              onSendToAgent={sendReferenceToAgent}
              onRunInNewShell={(cmd, sub) => void runCommandInNewSplitShell(tab.id, cmd, sub)}
              onAgentStatus={(st) => { tabAgentStatuses = { ...tabAgentStatuses, [tab.id]: st }; }}
              revealSelf={() => { showShell(); selectTab(tab.id); void tick().then(() => revealTab(tab.id)); }}
              confirmCloseSelf={() => confirmClose(tab.id)}
              title={effectiveSessionTitle(tab, effectiveAgentStatuses[tab.id])}
              userNamed={Boolean(tab.userNamed)}
              isActive={tab.id === tabState.activeId}
              splitSlot={paneSlot ?? null}
              attentionLabel={oscAttentionLabel(effectiveAgentStatuses[tab.id])}
              onStatus={(status) => { tabStatuses = { ...tabStatuses, [tab.id]: status }; terminalLaunchRequests.update(tab.id, status); }}
              onActivity={() => { if (paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })) return; if (!unread.has(tab.id)) unread = new Set([...unread, tab.id]); }}
            />
            {/if}
            </div>
            {#if tabDropHint?.paneId === tab.id}
              {#if tabDropHint.mode === "swap"}
                <div
                  data-testid="terminal-tab-drop-swap"
                  class="pointer-events-none absolute flex items-center justify-center bg-accent/15 border-2 border-dashed border-accent/65 rounded-lg transition-opacity"
                  style:z-index={LAYERS.SCROLL_CUE}
                  style="left:8px;top:8px;right:8px;bottom:8px;"
                >
                  <span class="px-2.5 py-1 rounded-md bg-surface/95 border border-accent/65 text-meta font-medium text-textPrimary shadow-sm">
                    {!splitRoot
                      ? "Switch to tab"
                      : draggedTabId && hasPane(splitRoot, draggedTabId)
                        ? "Swap split panes"
                        : "Place in split pane"}
                  </span>
                </div>
              {:else}
                <div
                  data-testid="terminal-tab-drop-split"
                  class="pointer-events-none absolute flex items-center justify-center bg-accent/20 border-2 border-accent/65 transition-opacity"
                  style:z-index={LAYERS.SCROLL_CUE}
                  style={tabDropHint.direction === "row"
                    ? (tabDropHint.position === "before" ? "left:0;top:0;width:50%;height:100%;" : "left:50%;top:0;width:50%;height:100%;")
                    : (tabDropHint.position === "before" ? "left:0;top:0;width:100%;height:50%;" : "left:0;top:50%;width:100%;height:50%;")}
                >
                  <span class="px-2.5 py-1 rounded-md bg-surface/95 border border-accent/65 text-meta font-medium text-textPrimary shadow-sm">
                    {tabDropHint.direction === "row"
                      ? (tabDropHint.position === "before" ? "Split left" : "Split right")
                      : (tabDropHint.position === "before" ? "Split top" : "Split bottom")}
                  </span>
                </div>
              {/if}
            {/if}
            {#if fileDropHint?.paneId === tab.id}
              <div
                data-testid={`terminal-file-drop-${fileDropHint.region}`}
                class="pointer-events-none absolute flex items-center justify-center bg-accent/15 border-2 border-dashed border-accent/65 transition-opacity"
                style:z-index={LAYERS.SCROLL_CUE}
                style={fileDropHint.region === "center"
                  ? "left:6px;top:6px;right:6px;bottom:6px;border-radius:8px;"
                  : fileDropHint.region === "left"
                    ? "left:0;top:0;width:50%;height:100%;"
                    : fileDropHint.region === "right"
                       ? "left:50%;top:0;width:50%;height:100%;"
                       : fileDropHint.region === "top"
                         ? "left:0;top:0;width:100%;height:50%;"
                         : "left:0;top:50%;width:100%;height:50%;"}
              >
                <span class="px-2.5 py-1 rounded-md bg-surface/95 border border-accent/65 text-meta font-medium text-textPrimary shadow-sm">
                  {fileDropHint.region !== "center"
                    ? "Split & open file"
                    : isPromptLauncher(tab.launcher)
                      ? `Attach @path to ${launcherLabel(tab.launcher)}`
                      : tab.launcher === "file" && (terminalDropZones.currentInAppDrag()?.filesOnly ?? true)
                        ? "Open file in pane"
                        : "Insert path into shell"}
                </span>
              </div>
            {/if}
          </div>
        {/each}
        {#each splitLayout.sashes as sash (sash.branchId)}
          <!-- Justified: WAI-ARIA 1.2 Window Splitter pattern requires a focusable separator with arrow-key resizing; Svelte's static check classifies role="separator" as non-interactive. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <div
            role="separator"
            tabindex="0"
            aria-orientation={sash.direction === "row" ? "vertical" : "horizontal"}
            aria-label={sash.direction === "row" ? "Resize split columns (double-click to equalize)" : "Resize split rows (double-click to equalize)"}
            aria-valuenow={Math.round(sash.ratio * 100)}
            aria-valuemin={15}
            aria-valuemax={85}
            data-split-sash={sash.branchId}
            class="terminal-split-sash absolute flex items-center justify-center select-none group {sash.direction === 'row' ? '-ml-[3px] w-1.5 cursor-col-resize' : '-mt-[3px] h-1.5 cursor-row-resize'}"
            style:z-index={LAYERS.STICKY}
            style={sash.direction === "row"
              ? `left: ${(sash.box.left * 100).toFixed(4)}%; top: ${(sash.box.top * 100).toFixed(4)}%; height: ${(sash.box.height * 100).toFixed(4)}%;`
              : `left: ${(sash.box.left * 100).toFixed(4)}%; top: ${(sash.box.top * 100).toFixed(4)}%; width: ${(sash.box.width * 100).toFixed(4)}%;`}
            onpointerdown={(event) => startSashDrag(event, sash)}
            ondblclick={() => { if (splitRoot) setActiveSplitTree(equalizeSplits(splitRoot, sash.branchId)); }}
            onkeydown={(event) => handleSashKeydown(event, sash)}
          >
            <span
              class="pointer-events-none transition-colors bg-border group-hover:bg-accent/70 group-focus-visible:bg-accent {sash.direction === 'row' ? 'w-px h-full group-hover:w-0.5' : 'h-px w-full group-hover:h-0.5'}"
            ></span>
          </div>
        {/each}
      {/if}
      </div>
    </div>
    </div>

  {#if section === "agents"}
    <div class="flex-1 min-h-0 min-w-0 overflow-hidden" data-testid="dock-agents-lens">
      <DockAgentsSection
        here={agentRowsHere}
        elsewhere={agentRowsElsewhere}
        launchers={agentLaunchers}
        {followAgent}
        canFollow={Boolean(repoPath)}
        onSelect={selectAgentRow}
        onGoTo={goToAgentSession}
        onLaunch={launchAgent}
        onToggleFollow={() => { if (followAgent) stopFollowing(); else startFollowing(); }}
      />
    </div>
  {/if}

  {#if section === "console"}
  <div class="px-3 h-7 shrink-0 flex items-center gap-1.5 text-micro text-textMuted"><Shield size={11} />Direct & bounded · git commands Manvi-gated</div>
  <!-- Output Area -->
  <div
    bind:this={scrollContainer}
    onscroll={() => { if (scrollContainer) consoleFollowing = followsConsoleOutput(scrollContainer.scrollTop, scrollContainer.clientHeight, scrollContainer.scrollHeight); }}
    class="flex-1 overflow-auto p-4 space-y-4 font-mono text-meta leading-relaxed"
  >
    {#if discardedExecutions > 0}<div role="status" class="text-textMuted">{discardedExecutions} older results removed · retains up to 100 results / 8 MiB.</div>{/if}
    {#if executions.length === 0}
      <div class="flex flex-col items-center justify-center h-full max-w-lg mx-auto text-center space-y-4 text-textMuted font-sans">
        <div class="p-3 rounded-2xl bg-surface border border-border shadow-xs text-accent">
          <Terminal size={28} />
        </div>
          <div>
            <h3 class="font-semibold text-textPrimary text-sm">Direct repository terminal</h3>
            <p class="text-xs text-textMuted mt-1">
              Execute commands directly in your repository — no shell in between, so arguments stay
              literal. Git commands are judged by the Manvi gate before they run; other tools run
              with hard timeouts and capped output.
            </p>
          </div>

        <div class="w-full space-y-1.5 pt-2">
          <div class="gp-label">Quick commands</div>
          <div class="flex flex-wrap gap-1.5 justify-center font-mono text-xs">
            {#each QUICK_COMMANDS as qc}
              <button
                type="button"
                onclick={() => void execute(qc)}
                class="gp-btn font-mono" data-size="sm"
              >
                {qc}
              </button>
            {/each}
          </div>
        </div>
      </div>
    {:else}
      {#each executions as entry (entry.id)}
        <div class="rounded-xl border border-edgeSoft bg-surface/80 shadow-xs overflow-hidden font-mono">
          <!-- Command line header -->
          <div class="px-3 py-1.5 bg-surface flex items-center justify-between gap-2 text-xs">
            <div class="flex items-center gap-2 min-w-0">
              <span class="text-accent font-semibold">$</span>
              <span class="font-semibold text-textPrimary truncate">{entry.command}</span>
            </div>
            <div class="flex items-center gap-2 shrink-0 text-micro">
              {#if entry.running}
                <span class="flex items-center gap-1 text-accent">
                  <LoaderCircle size={12} class="animate-spin" />
                  Running…
                </span>
              {:else if entry.result}
                <span class="text-textMuted flex items-center gap-1">
                  <Clock size={10} />
                  {formatDuration(entry.result.duration_ms)}
                </span>
                {#if entry.result.policy}
                  {@const p = entry.result.policy}
                  <span
                    class="gp-chip"
                    data-tone={p.status === 'allowed' ? 'ok' : p.status === 'blocked' ? 'danger' : 'warn'}
                    title={p.reason || p.detail}
                  >
                    Manvi: {p.status}
                  </span>
                {:else if !entry.result.gated}
                  <span
                    class="gp-chip"
                    data-tone="neutral"
                    title="Non-git commands are not judged by the Manvi gate; they run bounded (timeout, capped output) instead."
                  >
                    not gate-checked
                  </span>
                {/if}
                {#if entry.result.timed_out}
                  <span class="gp-chip" data-tone="warn">
                    timed out
                  </span>
                {:else if entry.result.exit_code === 0}
                  <span class="gp-chip" data-tone="ok">
                    exit 0
                  </span>
                {:else}
                  <span class="gp-chip" data-tone="danger">
                    exit {entry.result.exit_code ?? "?"}
                  </span>
                {/if}
              {:else if entry.error}
                <span class="gp-chip" data-tone="danger">
                  failed
                </span>
              {/if}
              <button
                type="button"
                onclick={() => void copyOutput(entry)}
                class="gp-icon-btn"
                data-size="sm"
                aria-label="Copy command and output"
                title="Copy command and output"
              >
                {#if copiedId === entry.id}
                  <Check size={12} class="text-ok" />
                {:else}
                  <Clipboard size={12} />
                {/if}
              </button>
            </div>
          </div>

          <!-- Output stream contents -->
          <div class="p-3 bg-background/50 space-y-2 overflow-x-auto select-text whitespace-pre-wrap leading-relaxed text-meta">
            {#if entry.running}
              <div class="text-textMuted italic">Executing command…</div>
            {:else if entry.result}
              {#if entry.result.stdout_tail}
                <div class="text-textPrimary">{entry.result.stdout_tail}</div>
              {/if}
              {#if entry.result.stderr_tail}
                <div class="text-danger">{entry.result.stderr_tail}</div>
              {/if}
              {#if !entry.result.stdout_tail && !entry.result.stderr_tail}
                <div class="text-textMuted italic">(No output produced)</div>
              {/if}
              {#if entry.result.truncated}
                <!-- The backend says WHY. It used to say "exceeded cap" for
                     every prefix, including a stream we simply never finished
                     reading — a cause the UI is in no position to assert. -->
                <div class="text-warn text-micro pt-1">
                  [Output is incomplete: {entry.result.truncation_reason ??
                    "reason unavailable"}]
                </div>
              {/if}
            {:else if entry.error}
              <div class="text-danger flex items-start gap-1.5">
                <AlertCircle size={14} class="shrink-0 mt-0.5" />
                <span>{entry.error}</span>
              </div>
            {/if}
          </div>
        </div>
      {/each}
    {/if}
  </div>

  <!-- Input Bar -->
  <div class="p-3 shrink-0 space-y-2">
    {#if validationError}
      <div class="gp-notice mx-0! mt-0! flex items-center gap-1.5" data-tone="danger">
        <AlertCircle size={13} class="shrink-0" />
        <span class="flex-1">{validationError}</span>
        <button
          type="button"
          onclick={() => (validationError = null)}
          class="gp-icon-btn"
          data-size="sm"
          aria-label="Dismiss"
        >
          ✕
        </button>
      </div>
    {/if}

    <div class="flex items-center gap-2">
      <div class="flex-1 flex items-center gap-2 px-3 py-1.5 rounded-xl bg-background border border-border focus-within:border-(--field-focus-edge) transition-colors font-mono">
        <span class="text-accent font-semibold select-none">$</span>
        <!-- A placeholder is not an accessible name: it is announced as a hint
             and vanishes the moment anything is typed, so this field read as
             unlabelled for the whole time it actually held a command. -->
        <input
          bind:this={inputEl}
          bind:value={commandInput}
          onkeydown={handleKeyDown}
          type="text"
          aria-label="Console command"
          placeholder="Enter command (e.g. git status, npm test, cargo update)…"
          disabled={running}
          class="flex-1 bg-transparent text-xs text-textPrimary placeholder:text-faint focus:outline-hidden disabled:opacity-50"
        />
      </div>
      <button
        type="button"
        onclick={() => void execute()}
        disabled={running || !commandInput.trim()}
        class="gp-btn shrink-0 px-4!" data-variant="primary" data-size="md"
      >
        {#if running}
          <LoaderCircle size={14} class="animate-spin" />
        {:else}
          <Play size={13} />
          <span>Run</span>
        {/if}
      </button>
    </div>
  </div>
  {/if}
</div>

<style>
  /* Chrome in the column, never a translucent overlay. On macOS `bg-surface`
     is translucent and xterm paints an opaque grid, so a guessed `inset` over
     the panes made the prompt and these bars share pixels. Find already
     takes a row; shortcuts, sessions, find, and tab options do the same.
     The session-type picker is the exception: a compact `gp-menu` overlay
     (with the float blur) so opening it cannot shove the PTY down. */
  .terminal-popover { flex-shrink: 0; max-height: 40%; overflow: auto; }
  /* A long session list scrolls inside the dock. 18rem is the hard cap so an
     expanded dock cannot grow the list over the window. */
  .terminal-session-list { max-height: min(18rem, 40%); }
  /* Repository, then the checkout path, then the actions. The path fills the
     space a tooltip used to float over. */
  .session-group-head { grid-column: 1 / -1; }
  .session-row {
    display: grid;
    grid-template-columns: minmax(8rem, 16rem) minmax(6rem, 1fr) auto;
    column-gap: 0.75rem;
    align-items: center;
    padding: 0.125rem 0;
  }
  .session-path { min-width: 0; }
  .terminal-strip-row {
    container-type: inline-size;
    container-name: terminal-strip;
  }
  .terminal-tab-scroller {
    overscroll-behavior-x: contain;
    scrollbar-width: thin;
  }
  .terminal-tab-label { max-width: 14rem; }
  [data-crowded="true"] .terminal-tab-label { max-width: 7rem; }
  @container terminal-strip (max-width: 520px) {
    .terminal-launcher-label {
      position: absolute;
      width: 1px;
      height: 1px;
      padding: 0;
      margin: -1px;
      overflow: hidden;
      clip: rect(0, 0, 0, 0);
      white-space: nowrap;
      border: 0;
    }
    .terminal-tab-scroller[data-crowded="true"] .terminal-tab-label {
      max-width: 4rem;
    }
  }
  .terminal-panes { container-type: inline-size; overflow: auto; }
  .terminal-pane { container-type: inline-size; container-name: split-pane; }
  @container split-pane (max-width: 380px) {
    .split-pane-location,
    .split-pane-active-marker {
      display: none;
    }
  }
  @container split-pane (max-width: 280px) {
    .split-pane-secondary {
      display: none;
    }
  }
  /* Each pane keeps at least MIN_PANE_HEIGHT_PX through the canvas's
     min-height (`minimumSplitHeight`), so Find, warnings and the footer leave
     a readable grid and short docks scroll their panes instead. */
</style>
