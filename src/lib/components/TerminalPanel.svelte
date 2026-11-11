<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { platformChord, shortcutTextLabel } from "../ui/platformCopy";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { closeWithConfirmation, terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { loadAgentDefaults } from "../stores/agentDefaultsStore";
  import type { FocusOutcome } from "../terminal/sessionFocus";
  import { isPromptLauncher, terminalLaunchRequests } from "../terminal/launchRequests";
  import { taskTerminalRequests, taskTerminalHolds, consumeTaskTerminalRequest, heldElsewhere, holdTaskTerminal, launchFor, releaseTaskTerminal, requestFor, type HostCandidate, type TaskTerminalRequest } from "../terminal/taskLaunches";
  import { isCaseInsensitiveFs, sameRepo } from "../repos/paths";
  import { consoleLaunchRequests, consumeConsoleLaunch } from "../terminal/consoleLaunches";
  import { boundedCommand, retainCommand, retainExecutions, followsConsoleOutput } from "../terminal/consoleHistory";
  import { harnessStore } from "../stores/harnessStore";
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
    ListChecks,
    X,
    Plus,
    Search,
    ChevronDown,
    Maximize2,
    Pin,
    PinOff,
    Minimize2,
    Columns2,
    Settings2,
    ChevronLeft,
    ChevronRight,
    ChevronsUpDown,
    Globe,
  } from "@lucide/svelte";
  import { tokenizeCommand } from "../terminal/tokenize";
  import type { TerminalRunResult } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import TerminalSession from "./TerminalSession.svelte";
  import BrowserSession from "./BrowserSession.svelte";
  import { CONTEXT_UNKNOWN, closeQuestion, startDirFrom } from "../terminal/sessionContext";
  import { askConfirm } from "../stores/modalStore";
  import { openTaskForRun } from "../workbench/taskOpen";
  import { groupSessionsByRepository, sessionRow, type CheckoutTab } from "../terminal/checkoutLabel";
  import { sessionCountLabel, sessionIsLive, sessionListCommand, sessionListNotice, sessionStatusLabel } from "../terminal/sessionList";
  import { forgetEndedTail } from "../terminal/endedBuffer";
  import ScrollCue from "./ScrollCue.svelte";
  import {
    activateTab,
    offeredSessionKind,
    sessionTypesFor,
    canOpenTab,
    MAX_STRIP_TABS,
    closeTab,
    cycleTab,
    initialState,
    launcherLabel,
    openTab,
    tabFor,
    paneOnScreen,
    setTabStartDir,
    setTabTitle,
    setTabUrl,
    browserTabFor,
    openBrowserTab,
    renameTab,
    moveTab,
    tabLabel,
    terminalTabChord,
    terminalTabDestination,
    type SessionKind,
    type TabState,
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
  import { popover, restoreFocusTo } from "../ui/popover";

  /** The shared wire shape; aliased for this panel's existing call sites. */
  type TerminalRunResponse = TerminalRunResult;

  let {
    repoPath = null,
    visible = true,
    onClose,
    expanded = false,
    onToggleExpanded,
    dockMode = "docked",
    onToggleDockMode,
    onGoToSession,
    checkouts = [],
  }: {
    repoPath?: string | null;
    /** False while another repository's panel (or a closed dock) is showing. */
    visible?: boolean;
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

  onMount(() => {
    inputEl?.focus();
    // The session limit is an agent default; read it before the first
    // session so the panel stops where the backend does.
    void loadAgentDefaults().catch(() => {});
    return () => {
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
  let mode = $state<PtyMode>("shell");
  function initialTabs(): TabState {
    const launch = get(terminalLaunchRequests);
    if (launch && repoPath && sameRepo(launch.repoPath, repoPath, pathOpts)) return { tabs: [], activeId: null };
    // A panel mounted for a queued request starts empty: the request opener
    // below opens its tab, through the same admission as every other.
    if (requestFor(get(taskTerminalRequests), repoPath, pathOpts, checkouts)) return { tabs: [], activeId: null };
    return initialState();
  }
  let tabState = $state<TabState>(untrack(initialTabs));
  const activeId = $derived(tabState.activeId);
  const activeTitle = $derived(tabState.tabs.find((tab) => tab.id === activeId)?.title);
  let sessions = $state<Record<string, TerminalSession | undefined>>({});
  /**
   * Browser tabs, apart from `sessions` on purpose: a browser runs no process,
   * so every terminal-only action (find, scrollback, output, context) reaches
   * `sessions[id]`, finds nothing for a browser tab, and does nothing.
   */
  let browsers = $state<Record<string, BrowserSession | undefined>>({});
  /** The session types this host can open; browser sessions are macOS-only. */
  const sessionTypes = $derived(sessionTypesFor($hostPlatform.browser_sessions));
  /** What the reader chose last; shown as `nextLauncher` once this host is known to offer it. */
  let chosenLauncher = $state<SessionKind>(get(interfaceStore).terminalLauncher);
  const nextLauncher = $derived(offeredSessionKind(chosenLauncher, $hostPlatform.browser_sessions));
  const activeIsBrowser = $derived(tabState.tabs.find((tab) => tab.id === activeId)?.launcher === "browser");
  let splitIds = $state<[string, string] | null>(null);
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
  let unread = $state(new Set<string>());
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
  function canCreateKind(kind: SessionKind): boolean {
    if (kind !== "browser") return canCreate;
    return !stripFull && $hostPlatform.browser_sessions;
  }
  function capacityTitleFor(kind: SessionKind): string {
    if (kind !== "browser") return capacityTitle;
    if (!$hostPlatform.browser_sessions) return "Browser sessions are not available on this computer yet";
    return stripFull
      ? `This repository's terminal holds ${MAX_STRIP_TABS} tabs, the most it can — close ended ones to open another`
      : "New browser session — a local dev server's page";
  }
  let focusTabStrip = false;
  const tabExtras = $derived.by(() => {
    const extras: Record<string, { status?: string; unread?: boolean }> = {};
    for (const tab of tabState.tabs) {
      extras[tab.id] = { status: tabStatuses[tab.id], unread: unread.has(tab.id) };
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
  const filteredLaunchers = $derived(filterLaunchers(sessionTypes, launcherQuery));
  /**
   * Compact overlay, not an in-flow chrome row. A full-width `terminal-popover`
   * shoved the PTY down by the height of the type list. The other chrome
   * bars stay in the column because they are short toolbars; this one is a
   * picker, so it uses the shared portaled popover (`ui/popover.ts`).
   */
  const launcherMenuDismissal = $derived({
    anchor: { kind: "element" as const, element: launcherTriggerEl, gap: 6 },
    estimate: { width: 208, height: 220 },
    inset: 8,
    revision: `${launcherQuery}:${filteredLaunchers.length}`,
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

  function closeChrome() {
    tabOptions = false;
    sessionListOpen = false;
    sessionSearchOpen = false;
    launcherMenuOpen = false;
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
    launcherHighlight = Math.max(0, sessionTypes.findIndex((launcher) => launcher.kind === nextLauncher));
    closeChrome();
    launcherMenuOpen = true;
    void tick().then(() => launcherSearchInput?.focus());
  }

  function chooseLauncher(kind: SessionKind) {
    chosenLauncher = kind;
    interfaceStore.setTerminalLauncher(kind);
    launcherMenuOpen = false;
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
          mode = "shell";
          consumeTaskTerminalRequest(request);
          continue;
        }
        if (room <= 0) break;
        const next = openTab(tabState, request.provider, launchFor(request, here, pathOpts));
        const opened = next.tabs.find((candidate) => !tabState.tabs.includes(candidate));
        if (!opened) break;
        if (!holdTaskTerminal(request, holder)) continue;
        tabState = next;
        mode = "shell";
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
      mode = "console";
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
      sessionError = `That session's task could not be opened: ${formatError(cause)}`;
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
        ? "That session could not be shown. It may have just exited; the Sessions list refreshes when it does."
        : "That session is no longer running.";
    }
  }

  function newTab(launcher: SessionKind, initialPrompt?: string): boolean {
    if (!repoPath || !canCreateKind(launcher)) return false;
    focusTabStrip = false;
    tabState = openTab(tabState, launcher, initialPrompt);
    if (splitIds && activeId) splitIds = [splitIds[0], activeId];
    return true;
  }

  /**
   * A new tab from the toolbar, the chord, or a split. A shell opens where the
   * shell beside it is working, as every terminal does — but only inside this
   * repository, which is the only place a session may start. An agent CLI
   * treats its working directory as the project root, so it keeps the root.
   */
  async function openBeside(launcher: SessionKind): Promise<boolean> {
    const fromId = tabState.activeId;
    // An unreadable context offers no directory, so the tab starts at the root.
    const read = launcher === "shell" && fromId ? await sessions[fromId]?.readContext() : undefined;
    const dir = startDirFrom(read ?? CONTEXT_UNKNOWN);
    if (!newTab(launcher)) return false;
    if (dir && tabState.activeId) tabState = setTabStartDir(tabState, tabState.activeId, dir);
    return true;
  }

  /** The Sessions list's close: the panel's question first, then stop the process. */
  async function closeListed(session: TerminalSessionRecord) {
    try {
      await closeWithConfirmation(session);
    } catch (error: unknown) {
      sessionError = formatError(error);
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
        await (sessions[id] ?? browsers[id])?.close();
      } catch (error: unknown) {
        sessionError = formatError(error);
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
      mode = "shell";
      if (claimed.launcher === "browser") {
        claimed.complete(openBrowserFor(claimed.url));
        return;
      }
      const opened = newTab(claimed.launcher, claimed.prompt);
      const id = tabState.activeId;
      // Only a prompt-carrying launch is an AGENT session; a plain shell
      // started from the palette must not appear in the agent list that
      // Coverage's "View agent session" reads.
      const agent = isPromptLauncher(claimed.launcher) ? claimed.launcher : null;
      if (opened && id && agent && claimed.prompt !== undefined) terminalLaunchRequests.remember({
        id, repoPath: claimed.repoPath, launcher: agent, status: "starting",
        reveal() {
          mode = "shell";
          selectTab(id);
          void tick().then(() => revealTab(id));
        },
      });
      claimed.complete(opened ? undefined : capacityTitleFor(claimed.launcher));
    });
  });

  onDestroy(() => {
    for (const tab of tabState.tabs) {
      terminalLaunchRequests.forget(tab.id);
      forgetEndedTail(tab.id);
    }
    releaseTaskTerminal(holder);
  });

  function selectTab(id: string, keepStripFocus = false) {
    focusTabStrip = keepStripFocus;
    if (splitIds && !splitIds.includes(id)) splitIds = [splitIds[0], id];
    tabState = activateTab(tabState, id);

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
    const partner = splitIds?.includes(id) ? splitIds.find((other) => other !== id) ?? null : null;
    if (splitIds?.includes(id)) splitIds = null;
    tabState = closeTab(tabState, id, partner);
    const { [id]: _status, ...statuses } = tabStatuses;
    tabStatuses = statuses;
    unread = new Set([...unread].filter((key) => key !== id));
    const { [id]: _gone, ...rest } = sessions;
    sessions = rest;
    const { [id]: _page, ...pages } = browsers;
    browsers = pages;
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
    const opened = openBrowserTab(tabState, url);
    if (!opened.id) return capacityTitleFor("browser");
    focusTabStrip = false;
    tabState = opened.state;
    if (splitIds && activeId) splitIds = [splitIds[0], activeId];
    return undefined;
  }

  /** Brings a tab's session forward: the terminal's grid, or the browser's page. */
  function revealTab(id: string) {
    (sessions[id] ?? browsers[id])?.reveal();
  }

  function handleChord(event: KeyboardEvent): boolean {
    if (mode !== "shell") return false;
    if (!visible || event.defaultPrevented) return false;
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
    if (event.key === "Escape" && !isImeComposition(event) && (tabOptions || sessionListOpen || sessionSearchOpen || launcherMenuOpen)) {
      const sessionsWereOpen = sessionListOpen;
      closeChrome();
      event.preventDefault(); event.stopPropagation();
      if (sessionsWereOpen) restoreFocusTo(sessionListTrigger);
      else if (activeId) revealTab(activeId);
      return;
    }
    if (handleChord(event)) return;
    if (mode === "shell" && activeId) sessions[activeId]?.handleViewChord(event);
  }

  async function handleTabKey(event: KeyboardEvent) {
    if (isImeComposition(event) || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
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

  // OSC titles can widen a tab after it is selected. Keep its entire control
  // (including Close) visible without refocusing the shell on every title.
  $effect(() => {
    activeTitle;
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

  async function toggleSplit() {
    if (splitIds) { splitIds = null; return; }
    const first = activeId;
    if (!first) return;
    let second = tabState.tabs.find((tab) => tab.id !== first)?.id;
    if (!second) { await openBeside(nextLauncher); second = activeId ?? undefined; }
    if (second && second !== first && tabState.tabs.some((tab) => tab.id === first)) splitIds = [first, second];
  }

  function showTabOptions() {
    renameValue = tabState.tabs.find((tab) => tab.id === activeId)?.name ?? "";
    const next = !tabOptions;
    closeChrome();
    tabOptions = next;
  }

  function saveTabName(event: SubmitEvent) {
    event.preventDefault();
    if (activeId) tabState = renameTab(tabState, activeId, renameValue);
    tabOptions = false;
  }

  function outputAction(event: Event) {
    if (!(event.currentTarget instanceof HTMLSelectElement) || !activeId) return;
    const session = sessions[activeId], action = event.currentTarget.value;
    event.currentTarget.value = "";
    if (action === "selection") void session?.copySelection();
    else if (action === "copy") void session?.copyOutput();
    else if (action === "export") session?.exportOutput();
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
      const errMessage = formatError(err);
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
      if (consoleFollowing && visible && mode === "console") void scrollToBottom();
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
<div class="relative flex-1 flex flex-col bg-background h-full min-w-0 text-xs font-sans overflow-hidden" role="region" aria-label="Terminal" onkeydown={handlePanelKey}>
  <!-- Header Bar -->
  <div class="px-3 py-1.5 bg-surface/60 flex flex-wrap gap-2 items-center shrink-0">
    <div class="flex flex-1 items-center gap-2 min-w-0">
      <Terminal size={14} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Terminal</span>
      <span class="text-textMuted font-mono truncate text-micro" title={repoPath ?? "No repository"}>
        {repoPath?.split(/[\\/]/).pop() ?? "No repository"}
      </span>
    </div>
    <div class="flex items-center gap-1 shrink-0">
      <div class="gp-segmented" role="group" aria-label="Terminal mode">
        <button
          type="button"
          aria-pressed={mode === "shell"}
          data-active={mode === "shell" ? "true" : "false"}
          class="gp-seg-btn text-meta! py-0.5!"
          onclick={() => (mode = "shell")}
          title="A real interactive shell in this repository"
        >
          <SquareTerminal size={11} class="inline mr-1 -mt-0.5" />Shell
        </button>
        <button
          type="button"
          aria-pressed={mode === "console"}
          data-active={mode === "console" ? "true" : "false"}
          class="gp-seg-btn text-meta! py-0.5!"
          onclick={() => (mode = "console")}
          title="Run single commands with capped output and per-run policy verdicts"
        >
          <ListChecks size={11} class="inline mr-1 -mt-0.5" />Console
        </button>
      </div>
      {#if mode === "shell"}
        <button type="button" class="gp-icon-btn" disabled={!activeId || !repoPath || activeIsBrowser} aria-label="Find in terminal" title="Find in terminal ({platformChord('⌘F', 'Ctrl+Shift+F', $hostPlatform.os)})" onclick={() => activeId && sessions[activeId]?.openFind()}><Search size={13} /></button>
        <button type="button" class="gp-icon-btn" disabled={!activeId || !repoPath || activeIsBrowser} aria-label="Clear scrollback" title="Clear scrollback; keep the current prompt and session" onclick={() => activeId && sessions[activeId]?.clearScrollback()}><Trash2 size={13} /></button>
      {/if}
      {#if mode === "console" && executions.length > 0}
        <button
          type="button"
          onclick={clearOutput}
          class="gp-btn" data-size="sm"
          title="Clear terminal output"
        >
          <Trash2 size={12} />
          <span>Clear</span>
        </button>
      {/if}
      <button
        bind:this={sessionListTrigger}
        type="button"
        class="gp-icon-btn px-2! gap-1 text-micro! tabular-nums whitespace-nowrap"
        aria-label="All terminal sessions"
        aria-expanded={sessionListOpen}
        aria-controls="terminal-session-list"
        title="Sessions across repositories"
        data-testid="terminal-session-count"
        onclick={toggleSessionList}
      >{sessionCountLabel($terminalSessions.length, $terminalSessionLimit)}</button>
      {#if onToggleExpanded}
        <button type="button" class="gp-icon-btn" aria-label={expanded ? "Restore terminal size" : "Expand terminal"} title={expanded ? "Restore terminal size" : "Expand terminal"} onclick={onToggleExpanded}>
          {#if expanded}<Minimize2 size={13} />{:else}<Maximize2 size={13} />{/if}
        </button>
      {/if}
      {#if onToggleDockMode}
        <button type="button" class="gp-icon-btn" aria-label="Float terminal over the view" aria-pressed={dockMode === "overlay"} data-terminal-dock-mode-toggle title={dockMode === "overlay" ? "Dock the terminal under the view" : "Float the terminal over the view"} onclick={onToggleDockMode}>
          {#if dockMode === "overlay"}<PinOff size={13} />{:else}<Pin size={13} />{/if}
        </button>
      {/if}
      {#if onClose}
        <button type="button" class="gp-icon-btn" aria-label="Hide the terminal dock" title="Hide the terminal ({shortcutTextLabel('⌃`', $hostPlatform.os)}) — sessions keep running" onclick={onClose}><ChevronDown size={14} /></button>
      {/if}
    </div>
  </div>

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
            {@const row = sessionRow(session, repoPath, checkouts, pathOpts)}
            {@const where = session.checkout ?? session.repoPath}
            {@const index = sessionListFlat.findIndex((candidate) => candidate.key === session.key)}
            {@const focused = session.key === activeId && row.here}
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
                <div class="session-group-head flex items-baseline gap-2 pt-1.5 pb-0.5" data-testid="session-group">
                  <span class="font-medium" data-testid="session-repository">{group.repository}</span>
                  <span class="text-textMuted">{group.sessions.length} {group.sessions.length === 1 ? "session" : "sessions"}</span>
                  {#if group.here}<span class="text-textMuted">This repository</span>{/if}
                </div>
              {/if}
              <span class="min-w-0 flex items-center gap-1.5">
                {#if row.agent}<span class="gp-chip shrink-0" data-tone="accent" data-testid="session-agent" title={`Agent: ${row.agent}`}><span class="sr-only">Agent: </span>{row.agent}</span>{/if}
                <span class="min-w-0 truncate">
                  <span class="font-medium text-textPrimary">{session.title?.trim() || session.label.trim() || "Shell"}</span>
                  {#if row.checkout}<span class="text-textMuted" aria-hidden="true"> · </span><span class="sr-only">, checkout </span><span data-testid="session-checkout">{row.checkout}</span>{/if}
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
                    ? (row.here ? "Show this session" : `Switch to ${session.repoPath} and show this session`)
                    : "This session's panel is not mounted, so it cannot be shown"}
                  onclick={(event) => {
                    event.stopPropagation();
                    void goToSession(session);
                  }}
                >Go to</button>
                {#if row.taskRunId}
                  <button
                    type="button"
                    class="gp-btn" data-size="sm"
                    data-session-aux
                    data-testid="session-open-task"
                    title={`Open the task ${session.title ? `“${session.title}” ` : ""}this session is working on`}
                    onclick={(event) => {
                      event.stopPropagation();
                      void openListedTask(row.taskRunId);
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
  <!-- Tab strip + sessions. Rendered in BOTH modes and merely hidden in
       Console, because unmounting a session kills the shell — the same
       hide-don't-kill rule TerminalDock applies to the whole dock. -->
    <div
      class="shrink-0 flex items-stretch gap-2 px-2 h-9 min-w-0 bg-surface/40"
      class:hidden={mode !== "shell"}
      data-crowded={crowdedTabStrip(tabState.tabs.length) ? "true" : "false"}
    >
      <!-- Only the tabs scroll. The launcher group sat inside the scroller
           behind an `ml-auto`, so past a handful of tabs the way to open one
           more scrolled off the right edge. -->
      <div class="relative flex-1 min-w-0 self-stretch">
      <div
        bind:this={tabScroller}
        data-terminal-tab-scroller
        class="terminal-tab-scroller h-full flex items-stretch gap-1 overflow-x-auto overflow-y-hidden"
        role="tablist"
        aria-label="Terminal sessions"
      >
      {#each tabState.tabs as tab (tab.id)}
        <div
          data-terminal-tab={tab.id}
          class="group flex items-center gap-1 pl-2 pr-1 my-1 rounded-lg border text-meta shrink-0 transition-colors {tab.id ===
          tabState.activeId
            ? 'bg-surface border-edgeStrong text-textPrimary'
            : 'bg-transparent border-transparent text-textMuted hover:bg-surface/70 hover:text-textPrimary'}"
        >
          <button
            type="button"
            role="tab"
            id={`terminal-tab-${tab.id}`}
            aria-controls={`terminal-pane-${tab.id}`}
            aria-selected={tab.id === tabState.activeId}
            tabindex={tab.id === tabState.activeId ? 0 : -1}
            class="terminal-tab-label truncate"
            onclick={() => selectTab(tab.id)}
            onkeydown={handleTabKey}
            title={`${launcherLabel(tab.launcher)} — ${tab.title?.trim().slice(0, 256) || tabLabel(tab)}`}
          >
            {#if unread.has(tab.id)}<span aria-label="Unread output" class="text-accent">● </span>{/if}{tabLabel(tab)}{#if tabStatuses[tab.id] === "exited"}<span class="text-textMuted"> · Ended</span>{:else if tabStatuses[tab.id] === "error"}<span class="text-danger"> · Error</span>{/if}
          </button>
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
      {/each}
      </div>
      <ScrollCue target={tabScroller} axis="x" />
      </div>

      <div class="flex items-center gap-1 shrink-0 pl-2">
        <button type="button" class="gp-icon-btn" aria-label="Find terminal session" aria-expanded={sessionSearchOpen} disabled={!tabState.tabs.length} title="Find a session in this repository" onclick={() => sessionSearchOpen ? closeChrome() : openSessionSearch()}><ChevronsUpDown size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label={splitIds ? "Close split view" : "Split terminal"} aria-pressed={Boolean(splitIds)} disabled={!activeId || (tabState.tabs.length < 2 && !canCreate)} onclick={toggleSplit}><Columns2 size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label="Terminal tab options" aria-expanded={tabOptions} disabled={!activeId} onclick={showTabOptions}><Settings2 size={13} /></button>
        <div class="relative shrink-0" data-terminal-launcher>
          <button
            bind:this={launcherTriggerEl}
            type="button"
            class="gp-btn max-w-28 truncate" data-size="sm"
            aria-label="New session type"
            aria-haspopup="listbox"
            aria-expanded={launcherMenuOpen}
            aria-controls="terminal-launcher-menu"
            title="Choose what the next session runs"
            onclick={() => launcherMenuOpen ? closeChrome() : openLauncherMenu()}
          >
            {launcherLabel(nextLauncher)}
            <ChevronDown size={11} />
          </button>
        </div>
        <button type="button" class="gp-icon-btn" data-terminal-new disabled={!repoPath || !canCreateKind(nextLauncher)} onclick={() => void openBeside(nextLauncher)} aria-label={`New ${launcherLabel(nextLauncher)} session`} title={capacityTitleFor(nextLauncher)}><Plus size={14} /></button>
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
            <button
              type="button"
              role="option"
              aria-selected={index === sessionHighlight}
              class="flex w-full items-center gap-2 py-0.5 px-1 rounded text-left {index === sessionHighlight ? 'bg-accent/15 text-textPrimary' : 'text-textMuted hover:bg-surfaceHover'}"
              onclick={() => chooseSearchedTab(tab.id)}
            >
              <span class="flex-1 min-w-0 truncate">{tabLabel(tab)}</span>
              <span class="shrink-0 text-micro">{launcherLabel(tab.launcher)}{#if tabStatuses[tab.id] === "exited"} · Ended{:else if tabStatuses[tab.id] === "error"} · Error{/if}{#if unread.has(tab.id)} · Unread{/if}</span>
            </button>
          {:else}
            <span class="text-textMuted">No terminals match.</span>
          {/each}
        </div>
      </div>
    {/if}

    {#if launcherMenuOpen && mode === "shell"}
      <div
        id="terminal-launcher-menu"
        use:portal={"body"}
        use:popover={launcherMenuDismissal}
        data-terminal-launcher-popup
        class="fixed w-52 max-w-[calc(100vw-16px)] gp-menu gp-pop p-1.5 text-meta text-textPrimary"
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
              {launcher.label}
            </button>
          {:else}
            <span class="block px-2.5 py-1.5 text-textMuted">No session types match.</span>
          {/each}
        </div>
      </div>
    {/if}

    {#if tabOptions && mode === "shell" && activeId}
      <form class="terminal-popover px-3 py-1.5 flex flex-wrap items-center gap-2 bg-surface" onsubmit={saveTabName}>
        <input aria-label="Terminal tab name" bind:value={renameValue} maxlength="64" placeholder="Use automatic title" class="gp-field min-w-0 w-36" />
        <button type="submit" class="gp-btn" data-size="sm">Rename</button>
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab left" disabled={tabState.tabs[0]?.id === activeId} onclick={() => activeId && (tabState = moveTab(tabState, activeId, -1))}><ChevronLeft size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab right" disabled={tabState.tabs.at(-1)?.id === activeId} onclick={() => activeId && (tabState = moveTab(tabState, activeId, 1))}><ChevronRight size={13} /></button>
        {#if !activeIsBrowser}
          <select aria-label="Terminal output actions" onchange={outputAction} class="gp-select">
            <option value="">Output actions</option><option value="selection">Copy selection</option><option value="copy">Copy retained output</option><option value="export">Export retained output…</option>
          </select>
        {/if}
      </form>
    {/if}
    <div class="terminal-panes flex-1 min-h-0 relative" class:hidden={mode !== "shell"}>
      {#if !repoPath}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Open a repository to start a shell.
        </div>
      {:else if tabState.tabs.length === 0}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-textMuted text-xs">
          <span>No sessions open.</span>
          <div class="flex items-center gap-2">
            <button type="button" class="gp-btn" data-size="sm" onclick={() => newTab("shell")}>
              <SquareTerminal size={12} /> New shell
            </button>
            {#if $hostPlatform.browser_sessions}
              <button type="button" class="gp-btn" data-size="sm" data-terminal-new-browser onclick={() => newTab("browser")} title={capacityTitleFor("browser")}>
                <Globe size={12} /> New browser
              </button>
            {/if}
          </div>
        </div>
      {:else}
        {#each tabState.tabs as tab (tab.id)}
          <!-- Absolute so hidden siblings keep their box: a session laid out
               at zero height would have its xterm reflow to a 1-row grid and
               tell the shell about it. -->
          <div
            class="terminal-pane absolute inset-0"
            data-position={splitIds?.[0] === tab.id ? "left" : splitIds?.[1] === tab.id ? "right" : "full"}
            class:hidden={splitIds ? !splitIds.includes(tab.id) : tab.id !== tabState.activeId}
            onfocusin={() => { if (activeId !== tab.id) selectTab(tab.id); }}
            role="tabpanel"
            id={`terminal-pane-${tab.id}`}
            aria-labelledby={`terminal-tab-${tab.id}`}
            aria-label={tabLabel(tab)}
          >
            {#if tab.launcher === "browser"}
            <BrowserSession
              bind:this={browsers[tab.id]}
              tabId={tab.id}
              onscreen={paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              initialUrl={tab.url}
              onUrl={(url) => (tabState = setTabUrl(tabState, tab.id, url))}
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
              startDir={tab.startDir}
              active={visible && tab.id === tabState.activeId && mode === "shell"}
              onscreen={paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              onChord={handleChord}
              revealSelf={() => { mode = "shell"; selectTab(tab.id); void tick().then(() => revealTab(tab.id)); }}
              confirmCloseSelf={() => confirmClose(tab.id)}
              title={tab.name}
              onStatus={(status) => { tabStatuses = { ...tabStatuses, [tab.id]: status }; terminalLaunchRequests.update(tab.id, status); }}
              onActivity={() => { if (paneOnScreen({ visible, mode, splitIds, activeId: tabState.activeId, tabId: tab.id })) return; if (!unread.has(tab.id)) unread = new Set([...unread, tab.id]); }}
            />
            {/if}
          </div>
        {/each}
      {/if}
    </div>

  {#if mode === "console"}
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
  <div class="p-3 bg-surface shrink-0 space-y-2">
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
      <div class="flex-1 flex items-center gap-2 px-3 py-1.5 rounded-xl bg-background border border-border focus-within:border-accent transition-colors font-mono">
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
          class="flex-1 bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-hidden disabled:opacity-50"
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
  .terminal-tab-scroller {
    overscroll-behavior-x: contain;
    scrollbar-width: thin;
  }
  .terminal-tab-label { max-width: 14rem; }
  [data-crowded="true"] .terminal-tab-label { max-width: 7rem; }
  .terminal-panes { container-type: inline-size; overflow: auto; }
  /* Keep a readable grid after Find, warnings, popovers, and the footer
     take their space. Short docks scroll their panes instead of crushing
     them to zero. */
  .terminal-pane { min-height: 180px; }
  .terminal-pane[data-position="left"] { right: 50%; border-right: 1px solid var(--color-border); }
  .terminal-pane[data-position="right"] { left: 50%; }
  @container (max-width: 620px) {
    .terminal-pane[data-position="left"] { right: 0; bottom: auto; height: max(50%, 180px); border-right: 0; border-bottom: 1px solid var(--color-border); }
    .terminal-pane[data-position="right"] { left: 0; top: max(50%, 180px); bottom: auto; height: max(50%, 180px); }
  }
</style>
