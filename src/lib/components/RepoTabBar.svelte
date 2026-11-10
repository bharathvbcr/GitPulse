<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { repoStore, type OpenRepoTab } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { isCaseInsensitiveFs, displayName, identityKey, isPathAmong } from "../repos/paths";
  import { portal } from "../dom/portal";
  import { bindHorizontalWheel } from "../dom/overflowHint";
  import { isTauri } from "../platform";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { classifyShortcut, shouldSkipWebviewShortcut } from "../ui/webviewShortcuts";
  import { LAYERS } from "../ui/layers";
  import { popover } from "../ui/popover";
  import { copyText } from "../desktop/clipboard";
  import { focusAdjacentToMenuOpener } from "../ui/focusTrap";
  import {
    ChevronDown,
    ChevronRight,
    Download,
    Pin,
    Plus,
    X,
    Folder,
    FolderGit2,
    FolderOpen,
    Layers,
    Bot,
    LayoutGrid,
    ListChecks,
    SquareTerminal,
    BellRing,
  } from "@lucide/svelte";
  import { askConfirm, askText, isPromptOpen } from "../stores/modalStore";
  import {
    computeTabLayout,
    normalizeGroupName,
    type GroupHeaderItem,
    type StackHeaderItem,
    type TabItem,
  } from "../repos/tabGroups";
  import { activationStops, planDrop, unitForTab, unitIds, type StripUnit } from "../repos/stripNav";
  import { checkoutName } from "../repos/repoFamily";
  import { expandedStacks, lastUsedCheckouts, setStackExpanded } from "../repos/stackState";
  import {
    collapsedInactiveGroups,
    explicitlyExpandedGroups,
    setGroupInactiveCollapsed,
    setGroupExplicitlyExpanded,
  } from "../repos/groupState";
  import {
    lookupGroupColor,
    normalizeTabColor,
    TAB_COLORS,
    TAB_COLOR_INK,
    TAB_COLOR_LABEL,
    type TabColor,
  } from "../repos/tabColors";
  import { terminalSessions, sessionsByRepo } from "../terminal/sessionRegistry";
  import { liveAgentCount, plural } from "../agents/plane";
  import { agentDirectories } from "../agents/cwd";
  import { headerAgentPlane } from "../agents/headerPlane";
  import { repoAgentPill, type RepoAgentPill } from "../agents/repoAttention";
  import { tabCardRows } from "../repos/tabCard";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import WorkspaceActions from "./WorkspaceActions.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import { taskChrome } from "../workbench/taskTabs";

  let {
    onOpen,
    onClone,
  }: {
    onOpen?: () => void;
    onClone?: () => void;
  } = $props();

  /**
   * Live shells per repository, for the tab badge.
   *
   * Now that the dock is per repository tab, a shell can be running somewhere
   * the user is not looking — which is the point, but it means nothing on
   * screen said where. The PTY budget is process-global, so "which of my
   * repositories are holding shells" is also the question to answer when a
   * new one is refused.
   */
  const terminalCounts = $derived(sessionsByRepo($terminalSessions));

  let menu = $state<{ x: number; y: number; id: string } | null>(null);
  // Measured menu box feeds the shared clamp so the tab menu can never open
  // off-screen (the old innerWidth-200 guess overflowed on short windows).
  let menuEl: HTMLDivElement | undefined = $state();
  let menuOpener: HTMLElement | null = null;
  let groupMenu = $state<{ x: number; y: number; group: string } | null>(null);
  let groupMenuEl: HTMLDivElement | undefined = $state();
  let groupMenuOpener: HTMLElement | null = null;
  let stripMenu = $state<{ x: number; y: number } | null>(null);
  let stripMenuEl: HTMLDivElement | undefined = $state();
  let stripMenuOpener: HTMLElement | null = null;
  let stackMenu = $state<{ x: number; y: number; key: string } | null>(null);
  let stackMenuEl: HTMLDivElement | undefined = $state();
  let stackMenuOpener: HTMLElement | null = null;
  let recentsOpen = $state(false);
  let recentsTriggerEl: HTMLButtonElement | undefined = $state();
  let recentsEl: HTMLDivElement | undefined = $state();
  // What is being dragged: a tab, or a folded worktree stack with every
  // checkout in it.
  let dragFrom = $state<StripUnit | null>(null);
  let dragHoverGroup = $state<string | null>(null);
  let dragHoverGroupTimer: ReturnType<typeof setTimeout> | null = null;
  // Where a dragged unit would land. `before` picks the left/right half of the
  // hovered unit; null means "no allowed insertion point" and hides the bar.
  let dropTarget = $state<{ unit: string; before: boolean } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  /** The title bar's one tablist: surfaces and repositories. */
  let tablist: HTMLDivElement | undefined = $state();
  let moveAnnouncement = $state("");
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const identity = (path: string) => identityKey(path, pathOpts);
  const hideTabStrip = $derived(
    $interfaceStore.autoHideRepoTabs && $repoStore.openTabs.length <= 1,
  );

  /**
   * The strip as drawn. Every move, drop and shortcut reads this one value,
   * so what the reader sees and what an action does cannot disagree.
   */
  const tabLayout = $derived(
    computeTabLayout(
      $repoStore.openTabs,
      $repoStore.collapsedGroups,
      terminalCounts,
      $repoStore.groupColors,
      {
        enabled: $interfaceStore.stackWorktreeTabs,
        expanded: $expandedStacks,
        lastUsed: lastUsedCheckouts(),
        identity,
      },
      {
        collapseInactive: $interfaceStore.collapseInactiveGroupTabs,
        collapsedInactiveGroups: $collapsedInactiveGroups,
        expandedGroups: $explicitlyExpandedGroups,
        keepActiveShells: !$interfaceStore.collapseInactiveIncludeShells,
      },
    ),
  );

  const groupHeadersByName = $derived(
    new Map(tabLayout.groups.map((g) => [g.group, g])),
  );

  const stackHeadersByKey = $derived(
    new Map(tabLayout.stacks.map((s) => [s.key, s])),
  );

  function unitKey(unit: StripUnit): string {
    return unit.kind === "tab" ? `tab:${unit.id}` : `stack:${unit.key}`;
  }

  function stackMembers(info: StackHeaderItem): OpenRepoTab[] {
    const byId = new Map($repoStore.openTabs.map((tab) => [tab.id, tab]));
    return info.tabIds.map((id) => byId.get(id)).filter((tab): tab is OpenRepoTab => tab !== undefined);
  }

  /**
   * A parked tab is open but not watched (repos/watchPool.ts): what it shows
   * is as old as its last read, or nothing if it was never read.
   */
  const PARKED_TITLE =
    "Live updates paused: more repositories are open than GitPulse watches at once. Opening this tab reads it and resumes them.";

  function memberOf(info: StackHeaderItem, tab: OpenRepoTab) {
    return checkoutName(tab.path, tab.label, info.root, identity);
  }

  function stackAriaLabel(info: StackHeaderItem): string {
    const member = memberOf(info, info.current);
    const where = member.primary ? "primary checkout" : `worktree ${member.name}`;
    const branch = info.current.currentBranch ? ` on ${info.current.currentBranch}` : "";
    const changes = info.isDirty ? ", uncommitted changes" : "";
    return `${info.label}, ${where}${branch}, ${info.tabCount} checkouts${changes}`;
  }

  const collapsedGroupSet = $derived(
    new Set($repoStore.collapsedGroups),
  );

  const distinctGroupNames = $derived(
    Array.from(
      new Set(
        $repoStore.openTabs
          .map((t) => normalizeGroupName(t.group))
          .filter((g): g is string => g !== null),
      ),
    ),
  );

  const ungroupedTabs = $derived(
    $repoStore.openTabs.filter((t) => !normalizeGroupName(t.group)),
  );

  function isTabInCollapsedGroup(tab: { group?: string | null }): boolean {
    const group = normalizeGroupName(tab.group);
    return group !== null && collapsedGroupSet.has(group);
  }

  function inheritedColorNote(group: string | null | undefined): string | null {
    const color = lookupGroupColor($repoStore.groupColors, normalizeGroupName(group));
    return color ? `Using the ${TAB_COLOR_LABEL[color]} group color` : null;
  }

  function tabColorInfo(tab: OpenRepoTab): { color: TabColor | null; source: "own" | "group" | null } {
    const own = normalizeTabColor(tab.color);
    if (own) return { color: own, source: "own" };
    const inherited = lookupGroupColor($repoStore.groupColors, normalizeGroupName(tab.group));
    return inherited ? { color: inherited, source: "group" } : { color: null, source: null };
  }

  function groupAriaLabel(info: GroupHeaderItem): string {
    const count = `${info.tabCount} ${info.tabCount === 1 ? "repository" : "repositories"}`;
    const collapsed = info.isCollapsed
      ? ", collapsed"
      : info.collapsedInactiveCount > 0
        ? `, ${info.collapsedInactiveCount} inactive collapsed`
        : "";
    const color = info.color ? `, ${TAB_COLOR_LABEL[info.color]}` : "";
    return `Group ${info.label}, ${count}${collapsed}${color}`;
  }

  function groupTitle(info: GroupHeaderItem): string {
    const inactiveNote =
      info.collapsedInactiveCount > 0
        ? ` (${info.collapsedInactiveCount} inactive collapsed)`
        : "";
    const shellsNote =
      info.terminalCount > 0
        ? ` · ${info.terminalCount} active shell${info.terminalCount === 1 ? "" : "s"}`
        : "";
    const action = info.isCollapsed || info.collapsedInactiveCount > 0 ? "expand" : "collapse";
    return `Group: ${info.label} (${info.tabCount} ${info.tabCount === 1 ? 'repository' : 'repositories'}${inactiveNote}${shellsNote})\nClick to ${action} · Right-click for options · F2 to rename · Delete to close`;
  }

  function onGroupHeadClick(info: GroupHeaderItem) {
    if (info.isCollapsed) {
      if (collapsedGroupSet.has(info.group)) {
        repoStore.setGroupCollapsed(info.group, false);
      }
      setGroupExplicitlyExpanded(info.group, true);
    } else if (info.collapsedInactiveCount > 0) {
      setGroupExplicitlyExpanded(info.group, true);
    } else {
      if ($interfaceStore.collapseInactiveGroupTabs || $collapsedInactiveGroups.has(info.group)) {
        setGroupExplicitlyExpanded(info.group, false);
        setGroupInactiveCollapsed(info.group, false);
        repoStore.setGroupCollapsed(info.group, true);
      } else {
        repoStore.toggleGroupCollapsed(info.group);
      }
    }
  }

  function groupChrome(info: { hasActiveTab: boolean }): string {
    if (info.hasActiveTab) {
      return "border-accent/35 bg-accent/10 text-accent font-medium";
    }
    return "border-edge bg-surface/70 text-textSecondary hover:border-accent/35 hover:bg-surfaceHover hover:text-textPrimary";
  }

  let unusedRecents = $derived(
    $repoStore.recentRepos.filter(
      (path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), pathOpts),
    ),
  );
  const tasksOpen = $derived($interfaceStore.globalSurface === "tasks");
  const agentsOpen = $derived($interfaceStore.globalSurface === "agents");
  const fleetOpen = $derived($interfaceStore.globalSurface === "fleet");
  // The Agents plane's own definition, over the directories it last read, so
  // the chip and the plane's "live terminals" are one number.
  const liveAgents = $derived(liveAgentCount($terminalSessions, $agentDirectories));
  const viewingRepo = $derived($interfaceStore.globalSurface === "repository");

  /**
   * What each repository's agents ask of the reader: the same derivation as
   * the view bar's Agents pill (agents/repoAttention.ts), so a repository you
   * are not looking at still reaches you (E1). Unknown stays unknown: a tab
   * draws a mark only for a request, never for "not read".
   */
  const agentStates = $derived(
    new Map(
      $repoStore.openTabs.map((tab) => [
        tab.path,
        repoAgentPill({
          plane: $headerAgentPlane.plane,
          probes: $headerAgentPlane.probes,
          repoPath: tab.path,
          paths: pathOpts,
        }),
      ] as const),
    ),
  );

  function agentRequest(path: string): RepoAgentPill | null {
    const state = agentStates.get(path);
    return state?.state === "attention" ? state : null;
  }

  /** The tab that is the one roving stop: the selected one, never two. */
  const rovingTabId = $derived(viewingRepo ? $repoStore.activeTabId : null);
  const surfaceStop = $derived<"fleet" | "agents" | "tasks" | null>(
    fleetOpen ? "fleet" : agentsOpen ? "agents" : tasksOpen ? "tasks" : null,
  );
  /** With nothing selected (no repository, none active), Fleet is the way in. */
  const fallbackStop = $derived(surfaceStop === null && rovingTabId === null ? "fleet" : null);

  function closeStripMenu(options?: { restoreFocus?: boolean }) {
    const opener = stripMenuOpener;
    stripMenu = null;
    stripMenuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
    }
  }

  function closeGroupMenu(options?: { restoreFocus?: boolean }) {
    const opener = groupMenuOpener;
    groupMenu = null;
    groupMenuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
    }
  }

  /** The control that opened whichever popup is showing, for focus to return to. */
  function currentOpener(): HTMLElement | null {
    if (menu) return menuOpener;
    if (overflowOpen) return overflowTriggerEl ?? null;
    if (recentsOpen) return recentsTriggerEl ?? null;
    if (groupMenu) return groupMenuOpener;
    if (stackMenu) return stackMenuOpener;
    if (stripMenu) return stripMenuOpener;
    return null;
  }

  function closeMenu(options?: { restoreFocus?: boolean }) {
    const opener = currentOpener();
    closeCard();
    menu = null;
    recentsOpen = false;
    overflowOpen = false;
    groupMenu = null;
    stackMenu = null;
    stripMenu = null;
    menuOpener = null;
    groupMenuOpener = null;
    stackMenuOpener = null;
    stripMenuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
    }
  }

  function openStackMenu(e: MouseEvent, info: StackHeaderItem) {
    e.preventDefault();
    e.stopPropagation();
    const trigger = e.currentTarget instanceof HTMLElement ? e.currentTarget : null;
    // A click on the switcher anchors under it; a right-click opens at the pointer.
    const box = e.type === "click" ? trigger?.getBoundingClientRect() : null;
    const wasOpen = stackMenu?.key === info.key;
    closeMenu();
    if (wasOpen && e.type === "click") return;
    stackMenuOpener =
      trigger?.closest<HTMLElement>("[data-stack-shell]")?.querySelector<HTMLElement>("[data-stack-head]") ?? trigger;
    stackMenu = box
      ? { x: box.left, y: box.bottom + 4, key: info.key }
      : { x: e.clientX, y: e.clientY, key: info.key };
  }

  async function confirmCloseCheckouts(info: StackHeaderItem, keepId: string | null) {
    const ids = info.tabIds.filter((id) => id !== keepId);
    closeMenu();
    if (ids.length === 0) return;
    const confirmed = await askConfirm({
      title: keepId ? `Close other checkouts of ${info.label}` : `Close every checkout of ${info.label}`,
      message: `Close ${ids.length} ${info.label} ${ids.length === 1 ? "tab" : "tabs"}? The worktrees stay on disk; only the tabs close.`,
      confirmLabel: keepId ? "Close others" : "Close all",
      destructive: true,
    });
    if (!confirmed) return;
    for (const id of ids) {
      await repoStore.closeTab(id);
    }
  }

  async function promptSetGroup(tabId: string, currentGroup?: string | null) {
    closeMenu();
    const next = await askText({
      title: currentGroup ? "Change group" : "Add to group",
      message: "Enter group name for this repository tab (leave blank to ungroup):",
      placeholder: "e.g. backend, frontend, tools",
      initialValue: currentGroup ?? "",
      confirmLabel: currentGroup ? "Update group" : "Set group",
    });
    if (next !== null) {
      repoStore.setTabGroup(tabId, next);
    }
  }

  async function promptRenameGroup(group: string) {
    closeGroupMenu();
    const next = await askText({
      title: `Rename group "${group}"`,
      message: "Enter a new name for this tab group:",
      placeholder: group,
      initialValue: group,
      confirmLabel: "Rename",
    });
    if (next !== null) {
      repoStore.renameGroup(group, next);
    }
  }

  async function confirmCloseGroup(group: string, count: number) {
    closeGroupMenu();
    const confirmed = await askConfirm({
      title: `Close group "${group}"`,
      message: `Close all ${count} repository ${count === 1 ? "tab" : "tabs"} in "${group}"?`,
      confirmLabel: "Close group tabs",
      destructive: true,
    });
    if (confirmed) {
      repoStore.closeGroup(group);
    }
  }

  async function confirmCloseOtherGroups(keepGroup: string) {
    closeGroupMenu();
    const otherTabs = $repoStore.openTabs.filter(
      (t) => {
        const g = normalizeGroupName(t.group);
        return g !== null && g !== keepGroup;
      },
    );
    if (otherTabs.length === 0) return;
    const confirmed = await askConfirm({
      title: "Close other groups",
      message: `Close all ${otherTabs.length} repository ${otherTabs.length === 1 ? "tab" : "tabs"} in other groups?`,
      confirmLabel: "Close other groups",
      destructive: true,
    });
    if (confirmed) {
      for (const tab of otherTabs) {
        await repoStore.closeTab(tab.id);
      }
    }
  }

  async function confirmCloseAllTabs() {
    closeStripMenu();
    const count = $repoStore.openTabs.length;
    if (count === 0) return;
    const confirmed = await askConfirm({
      title: "Close all repositories",
      message: `Close all ${count} open repository ${count === 1 ? "tab" : "tabs"}?`,
      confirmLabel: "Close all",
      destructive: true,
    });
    if (confirmed) {
      const ids = $repoStore.openTabs.map((t) => t.id);
      for (const id of ids) {
        await repoStore.closeTab(id);
      }
    }
  }

  async function copyPath(path: string) {
    closeMenu();
    if (!(await copyText(path))) {
      repoStore.setError("Could not copy path to clipboard");
    }
  }

  // A tab closed while its menu was open leaves zombie state that only
  // Escape could dismiss; drop the menu the moment its tab vanishes.
  const menuTabOpen = $derived(!menu || $repoStore.openTabs.some((tab) => tab.id === menu?.id));
  $effect(() => {
    if (!menuTabOpen) {
      untrack(() => { menu = null; });
    }
  });

  $effect(() => {
    if (groupMenu && !groupHeadersByName.has(groupMenu.group)) {
      untrack(() => { groupMenu = null; });
    }
  });

  // A stack stops being one when it drops to a single checkout; its switcher
  // must not outlive it pointing at tabs that are now drawn on their own.
  $effect(() => {
    if (stackMenu && !stackHeadersByKey.has(stackMenu.key)) {
      untrack(() => { stackMenu = null; });
    }
  });

  function onContext(e: MouseEvent, id: string) {
    e.preventDefault();
    menuOpener = e.currentTarget instanceof HTMLElement
      ? e.currentTarget.querySelector<HTMLElement>('[role="tab"]')
      : null;
    menu = { x: e.clientX, y: e.clientY, id };
    recentsOpen = false;
    groupMenu = null;
    stackMenu = null;
    stripMenu = null;
  }

  function onGroupContext(e: MouseEvent, group: string) {
    e.preventDefault();
    groupMenuOpener = e.currentTarget instanceof HTMLElement ? e.currentTarget : null;
    groupMenu = { x: e.clientX, y: e.clientY, group };
    menu = null;
    stackMenu = null;
    stripMenu = null;
    recentsOpen = false;
  }

  function onStripContext(e: MouseEvent) {
    if (
      e.target instanceof Element &&
      (e.target.closest("[data-tab-id]") ||
        e.target.closest("[data-group-header]") ||
        e.target.closest("[data-stack-shell]") ||
        e.target.closest("button"))
    ) {
      return;
    }
    e.preventDefault();
    stripMenuOpener = scroller ?? null;
    stripMenu = { x: e.clientX, y: e.clientY };
    menu = null;
    groupMenu = null;
    stackMenu = null;
    recentsOpen = false;
  }

  /**
   * The tab menu, group menu, strip menu, and recents dropdown are popovers with shared
   * dismissal behavior.
   */
  const groupMenuDismissal = $derived({
    anchor: { kind: "point" as const, x: groupMenu?.x ?? 0, y: groupMenu?.y ?? 0 },
    estimate: { width: 176, height: 160 },
    revision: groupMenu?.group,
    dismiss: { inside: "[data-group-menu]", resize: true, escape: "none" as const },
    onDismiss: () => closeGroupMenu(),
  });

  const menuDismissal = $derived({
    anchor: { kind: "point" as const, x: menu?.x ?? 0, y: menu?.y ?? 0 },
    estimate: { width: 176, height: 150 },
    revision: menu?.id,
    dismiss: { inside: "[data-repo-menu]", resize: true, escape: "none" as const },
    onDismiss: () => closeMenu(),
  });

  const stackMenuDismissal = $derived({
    anchor: { kind: "point" as const, x: stackMenu?.x ?? 0, y: stackMenu?.y ?? 0 },
    estimate: { width: 288, height: 240 },
    revision: stackMenu ? `${stackMenu.key}@${stackMenu.x},${stackMenu.y}` : undefined,
    dismiss: { inside: "[data-stack-menu], [data-stack-switch]", resize: true, escape: "none" as const },
    onDismiss: () => { stackMenu = null; stackMenuOpener = null; },
  });

  const stripMenuDismissal = $derived({
    anchor: { kind: "point" as const, x: stripMenu?.x ?? 0, y: stripMenu?.y ?? 0 },
    estimate: { width: 176, height: 160 },
    revision: stripMenu ? `${stripMenu.x},${stripMenu.y}` : undefined,
    dismiss: { inside: "[data-strip-menu]", resize: true, escape: "none" as const },
    onDismiss: () => closeStripMenu(),
  });

  // Anchored to the trigger and clamped to the window: Open now sits beside
  // the tabs rather than at the right edge, so the menu cannot assume room.
  const recentsPlacement = $derived({
    anchor: { kind: "element" as const, element: recentsTriggerEl, gap: 6, align: "start" as const },
    estimate: { width: 320, height: 320 },
    inset: 8,
    revision: $repoStore.recentRepos.length,
    dismiss: { inside: "[data-recents-menu]", resize: true, escape: "none" as const },
    onDismiss: () => { recentsOpen = false; },
  });

  function focusPopup(element: HTMLElement | undefined) {
    window.setTimeout(() => {
      const first = element?.querySelector<HTMLElement>('[role="menuitem"]');
      (first ?? element)?.focus();
      first?.scrollIntoView?.({ block: "nearest" });
    }, 0);
  }

  $effect(() => {
    if (menu && menuEl) focusPopup(menuEl);
  });

  $effect(() => {
    if (recentsOpen && recentsEl) focusPopup(recentsEl);
  });

  $effect(() => {
    if (groupMenu && groupMenuEl) focusPopup(groupMenuEl);
  });

  $effect(() => {
    if (stripMenu && stripMenuEl) focusPopup(stripMenuEl);
  });

  $effect(() => {
    if (stackMenu && stackMenuEl) focusPopup(stackMenuEl);
  });

  function handlePopupKeydown(e: KeyboardEvent) {
    if (e.key === "Tab") {
      e.preventDefault();
      const popup = e.currentTarget;
      if (popup instanceof HTMLElement) {
        focusAdjacentToMenuOpener(popup, currentOpener(), e.shiftKey);
      }
      closeMenu();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      closeMenu({ restoreFocus: true });
      return;
    }
    const popup = e.currentTarget;
    if (!(popup instanceof HTMLElement)) return;
    const items = [...popup.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    if (items.length === 0) return;
    const current = items.findIndex((item) => item === document.activeElement);
    let next: number | null = null;
    if (e.key === "ArrowDown") next = (current + 1 + items.length) % items.length;
    else if (e.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    if (next === null) return;
    e.preventDefault();
    items[next]?.focus();
    items[next]?.scrollIntoView?.({ block: "nearest" });
  }

  function isTypingTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    const tag = target.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable;
  }

  function handleKey(e: KeyboardEvent) {
    if (isImeComposition(e)) return;
    // Escape closes any open tab menu, regardless of where focus sits —
    // same window-listener pattern as ViewTabBar.
    if (e.key === "Escape" && (menu || recentsOpen || overflowOpen || groupMenu || stackMenu || stripMenu)) {
      e.preventDefault();
      closeMenu({ restoreFocus: true });
      return;
    }
    if (shouldSkipWebviewShortcut(e, isTauri())) return;
    // A confirm or name prompt belongs to the tab it was raised on. Switching,
    // closing and opening tabs wait until it is answered (the native menu
    // accelerators are held by the same rule in App's canDispatch).
    if (isPromptOpen()) return;
    switch (classifyShortcut(e)) {
      case "closeActiveTab":
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        void repoStore.closeActiveTab();
        return;
      case "jumpToTab": {
        const digit = e.code.match(/^Digit([1-9])$/);
        if (!digit) return;
        e.preventDefault();
        revealRepository();
        // 9 is the last repository however many are open, as in a browser:
        // there is no ninth to reach for once there are more than nine.
        const wanted = Number(digit[1]);
        void repoStore.activateTabAt(wanted === 9 ? activationStops(tabLayout).length - 1 : wanted - 1);
        return;
      }
      case "cycleTabs": {
        e.preventDefault();
        revealRepository();
        if (e.shiftKey) void repoStore.prevTab();
        else void repoStore.nextTab();
        return;
      }
      case "openRepo":
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        onOpen?.();
        return;
      default:
        return;
    }
  }

  onMount(() => {
    // The app-wide shortcut owner (tab cycling, digit switching, Open) is a
    // window listener for the component's whole life, not a popover's.
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  });

  // The strip hides its track (a classic scrollbar was a blank band under the
  // tabs whenever the system shows scroll bars), so a mouse wheel pans it.
  $effect(() => {
    const el = scroller;
    if (!el) return;
    return bindHorizontalWheel(el);
  });

  function endDrag() {
    if (dragHoverGroupTimer) {
      clearTimeout(dragHoverGroupTimer);
      dragHoverGroupTimer = null;
    }
    dragHoverGroup = null;
    dragFrom = null;
    dropTarget = null;
  }

  function onGroupDragOver(e: DragEvent, group: string, isCollapsed: boolean) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    if (dragFrom === null) return;
    if (isCollapsed) {
      if (dragHoverGroup !== group) {
        if (dragHoverGroupTimer) clearTimeout(dragHoverGroupTimer);
        dragHoverGroup = group;
        dragHoverGroupTimer = setTimeout(() => {
          repoStore.setGroupCollapsed(group, false);
          dragHoverGroup = null;
          dragHoverGroupTimer = null;
        }, 400);
      }
    }
  }

  function onGroupDragLeave(_e: DragEvent, group: string) {
    if (dragHoverGroup === group) {
      if (dragHoverGroupTimer) clearTimeout(dragHoverGroupTimer);
      dragHoverGroup = null;
      dragHoverGroupTimer = null;
    }
  }

  function onGroupDrop(e: DragEvent, group: string) {
    e.preventDefault();
    e.stopPropagation();
    if (dragFrom !== null) {
      // A folded stack carries every checkout in it into the group.
      const ids = unitIds(tabLayout, dragFrom);
      for (const id of ids) repoStore.setTabGroup(id, group);
      repoStore.setGroupCollapsed(group, false);
      if (ids[0]) announceMove(ids[0]);
    }
    endDrag();
  }

  function onGroupKeydown(e: KeyboardEvent, info: GroupHeaderItem) {
    if (e.key === "ArrowRight" && (info.isCollapsed || info.collapsedInactiveCount > 0)) {
      e.preventDefault();
      if (collapsedGroupSet.has(info.group)) repoStore.setGroupCollapsed(info.group, false);
      setGroupExplicitlyExpanded(info.group, true);
      return;
    }
    if (e.key === "ArrowLeft" && !info.isCollapsed && info.collapsedInactiveCount === 0) {
      e.preventDefault();
      repoStore.setGroupCollapsed(info.group, true);
      return;
    }
    if (e.key === "F2") {
      e.preventDefault();
      void promptRenameGroup(info.group);
      return;
    }
    if (e.key === "Delete") {
      e.preventDefault();
      void confirmCloseGroup(info.group, info.tabCount);
      return;
    }
  }

  function focusTabById(id: string) {
    const tabs = scroller?.querySelectorAll<HTMLElement>('[role="tab"][data-tab-id]') ?? [];
    const match = Array.from(tabs).find((el) => el.dataset.tabId === id);
    match?.focus();
  }

  /**
   * Position among the strip's stops — what the reader counts on screen. A
   * folded stack is one stop, so "position 3 of 4" matches the picture.
   */
  function announceMove(id: string) {
    const tab = $repoStore.openTabs.find((item) => item.id === id);
    if (!tab) return;
    const unit = unitForTab(tabLayout, id);
    const stops = tabLayout.visibleItems.filter((item) => item.kind !== "group-header");
    const index = stops.findIndex((item) =>
      unit?.kind === "stack" ? item.kind === "stack-header" && item.key === unit.key : item.id === id,
    );
    const name = unit?.kind === "stack" ? stackHeadersByKey.get(unit.key)?.label ?? tab.label : tab.label;
    moveAnnouncement = index >= 0
      ? `Moved ${name} to position ${index + 1} of ${stops.length}`
      : `Moved ${name}`;
  }

  /** After a move the store applied: say where it landed and keep focus on it. */
  function afterMove(moved: boolean, id: string) {
    if (!moved) return;
    announceMove(id);
    window.setTimeout(() => focusTabById(id), 0);
  }

  function moveFocusedTabTo(id: string, edge: "start" | "end") {
    afterMove(repoStore.moveTabToEdge(id, edge), id);
  }

  function moveFocusedTabBy(id: string, delta: number) {
    afterMove(repoStore.moveTabBy(id, delta), id);
  }

  function tabIdUnderFocus(): string | null {
    const focused = document.activeElement;
    const shell = focused instanceof Element ? focused.closest("[data-tab-id]") : null;
    if (shell instanceof HTMLElement && shell.dataset.tabId) return shell.dataset.tabId;
    return $repoStore.openTabs.find((tab) => tab.isActive)?.id ?? null;
  }

  function setExpanded(info: StackHeaderItem, open: boolean) {
    setStackExpanded(info.key, open);
    const focusId = info.current.id;
    window.setTimeout(() => {
      const head = scroller?.querySelector<HTMLElement>(`[data-stack-head="${CSS.escape(info.key)}"]`);
      (open ? scroller?.querySelector<HTMLElement>(`[role="tab"][data-tab-id="${CSS.escape(focusId)}"]`) : head)?.focus();
    }, 0);
  }

  /**
   * A folded stack's header is a tab: Enter/Space activate the checkout it
   * shows (the button's own click), ArrowDown opens the switcher, and the
   * plus/minus keys unfold or fold it — the group head's ArrowLeft/Right are
   * already the tablist's roving keys here.
   */
  function onStackKeydown(e: KeyboardEvent, info: StackHeaderItem) {
    // Alt+Enter is the one chorded key here. Anything else held down belongs
    // to someone else — Cmd+= / Cmd+- are the zoom accelerators.
    const bare = !e.metaKey && !e.ctrlKey && !e.altKey;
    if ((bare && e.key === "ArrowDown") || (e.key === "Enter" && e.altKey && !e.metaKey && !e.ctrlKey)) {
      e.preventDefault();
      const trigger = (e.currentTarget as HTMLElement | null)
        ?.closest<HTMLElement>("[data-stack-shell]")
        ?.querySelector<HTMLElement>("[data-stack-switch]");
      const box = trigger?.getBoundingClientRect();
      closeMenu();
      stackMenuOpener = e.currentTarget instanceof HTMLElement ? e.currentTarget : null;
      stackMenu = { x: box?.left ?? 0, y: (box?.bottom ?? 0) + 4, key: info.key };
      return;
    }
    if (!bare) return;
    if (e.key === "+" || e.key === "=") {
      e.preventDefault();
      setExpanded(info, true);
      return;
    }
    if (e.key === "-" && info.isExpanded) {
      e.preventDefault();
      setExpanded(info, false);
    }
  }

  /**
   * Arrow-key roving focus across the tablist (ARIA tabs pattern): focus
   * moves and wraps without changing the active repo; Home/End jump to the
   * edges. Ctrl+Shift+←/→ reorders the focused (or active) tab instead.
   */
  function onTablistKeydown(e: KeyboardEvent) {
    if (
      (e.key === "ArrowLeft" || e.key === "ArrowRight") &&
      e.ctrlKey &&
      e.shiftKey &&
      !e.metaKey &&
      !e.altKey
    ) {
      const id = tabIdUnderFocus();
      if (!id) return;
      e.preventDefault();
      moveFocusedTabBy(id, e.key === "ArrowLeft" ? -1 : 1);
      return;
    }
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const key = e.key as RovingKey;
    if (key !== "ArrowLeft" && key !== "ArrowRight" && key !== "Home" && key !== "End") return;
    // One roving set across the surfaces and the repositories: the title bar
    // is one tablist, so Left/Right pass from Tasks into the first repository.
    const tabs =
      tablist?.querySelectorAll<HTMLElement>(
        "[data-surface-tab], [data-tab-index], [data-group-head], [data-stack-head]",
      ) ?? [];
    if (tabs.length === 0) return;
    const current = Array.from(tabs).findIndex((el) => el === document.activeElement);
    const next = nextRovingIndex(current, tabs.length, key);
    if (next === null) return;
    e.preventDefault();
    tabs[next]?.focus();
  }

  async function closeTabFromKeyboard(id: string, index: number) {
    await repoStore.closeTab(id);
    window.setTimeout(() => {
      const tabs = scroller?.querySelectorAll<HTMLElement>("[data-tab-index], [data-stack-head][role='tab']") ?? [];
      (tabs[Math.min(index, tabs.length - 1)] ?? scroller)?.focus();
    }, 0);
  }

  function surfaceChipClass(active: boolean): string {
    return `shrink-0 flex items-center gap-1.5 px-2.5 py-1 rounded-lg border text-xs font-medium transition-colors ${
      active
        ? "border-accent/65 bg-accent/10 text-accent"
        : "border-edge bg-background/50 text-textPrimary hover:border-accent/35 hover:bg-accent/5 hover:text-accent"
    }`;
  }

  // ---- E2: the hover card ---------------------------------------------------
  // Intent before it opens, grace before it closes, as the view switcher's
  // flyout does. It replaces the native `title` tooltip, which could not hold
  // more than a path and could not be placed or styled.
  const CARD_INTENT_MS = 500;
  const CARD_GRACE_MS = 120;
  const NO_AGENTS: RepoAgentPill = { state: "none", live: 0, reason: null, tone: null, phrase: "" };

  let card = $state<{ id: string; anchor: HTMLElement } | null>(null);
  let cardTimer: ReturnType<typeof setTimeout> | undefined;

  function closeCard() {
    clearTimeout(cardTimer);
    cardTimer = undefined;
    card = null;
  }

  function onTabEnter(event: PointerEvent, id: string) {
    if (event.pointerType !== "mouse" && event.pointerType !== "pen") return;
    clearTimeout(cardTimer);
    const anchor = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    if (!anchor || dragFrom !== null || menu || recentsOpen || groupMenu || stackMenu || stripMenu || overflowOpen) return;
    // Already reading cards: the next one follows at once.
    cardTimer = setTimeout(() => {
      card = { id, anchor };
    }, card ? 0 : CARD_INTENT_MS);
  }

  function onTabLeave() {
    clearTimeout(cardTimer);
    cardTimer = setTimeout(closeCard, CARD_GRACE_MS);
  }

  function onCardEnter() {
    clearTimeout(cardTimer);
    cardTimer = undefined;
  }

  const cardTab = $derived(card ? $repoStore.openTabs.find((item) => item.id === card?.id) ?? null : null);
  const cardRows = $derived.by(() => {
    const tab = cardTab;
    if (!tab) return [];
    // Ahead/behind and the last fetch are read for the repository on screen
    // only; any other tab says "Not read" rather than guess.
    const current = tab.isActive;
    const branch = current
      ? $repoStore.branches.find((item) => item.is_current || item.name === $repoStore.currentBranch)
      : undefined;
    const fetchedMs = $repoStore.fetchedAt;
    return tabCardRows({
      tab,
      sync: branch ? { ahead: branch.ahead_count ?? 0, behind: branch.behind_count ?? 0 } : null,
      fetchedAt: current ? (fetchedMs === null ? null : Math.floor(fetchedMs / 1000)) : undefined,
      terminals: terminalCounts.get(tab.path) ?? 0,
      agents: agentStates.get(tab.path) ?? NO_AGENTS,
    });
  });
  const cardPlacement = $derived({
    anchor: { kind: "element" as const, element: card?.anchor ?? null, gap: 6, align: "start" as const },
    estimate: { width: 288, height: 168 },
    inset: 8,
    revision: `${card?.id ?? ""}:${cardRows.length}`,
    dismiss: { inside: "[data-tab-card]", pointer: "pointerdown" as const, scroll: true, resize: true, escape: "bubble" as const },
    onDismiss: () => closeCard(),
  });

  $effect(() => () => clearTimeout(cardTimer));

  // ---- Overflow: the units the strip hides ----------------------------------
  // An IntersectionObserver on the strip's units, so there is no per-frame
  // layout read: it tells us when a unit crosses the scroller's edge. A
  // collapsed group and a folded stack are one unit each (they are one
  // `data-drop-unit` element).
  let offscreen = $state<ReadonlySet<string>>(new Set());
  let overflowOpen = $state(false);
  let overflowTriggerEl: HTMLButtonElement | undefined = $state();
  let overflowEl: HTMLDivElement | undefined = $state();

  function unitKeyOf(element: Element): string | null {
    if (!(element instanceof HTMLElement)) return null;
    if (element.dataset.dropStack) return `stack:${element.dataset.dropStack}`;
    if (element.dataset.tabId) return `tab:${element.dataset.tabId}`;
    return null;
  }

  $effect(() => {
    const root = scroller;
    // The layout is read so the observer is rebuilt when units come and go.
    void tabLayout.visibleItems;
    if (!root || typeof IntersectionObserver !== "function") {
      offscreen = new Set();
      return;
    }
    const hidden = new Map<Element, boolean>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) hidden.set(entry.target, entry.intersectionRatio < 0.98);
        const next = new Set<string>();
        for (const [element, isHidden] of hidden) {
          const key = isHidden && element.isConnected ? unitKeyOf(element) : null;
          if (key) next.add(key);
        }
        offscreen = next;
      },
      { root, threshold: [0, 0.5, 0.98, 1] },
    );
    for (const element of root.querySelectorAll("[data-drop-unit]")) observer.observe(element);
    return () => observer.disconnect();
  });

  interface OverflowUnit {
    key: string;
    id: string;
    label: string;
    branch: string | null;
    dirty: boolean;
    conflicts: number;
    terminals: number;
    ask: RepoAgentPill | null;
  }

  const overflowUnits = $derived.by(() => {
    const units: OverflowUnit[] = [];
    for (const item of tabLayout.visibleItems) {
      if (item.kind === "stack-header") {
        if (!offscreen.has(`stack:${item.key}`)) continue;
        units.push({
          key: `stack:${item.key}`,
          id: item.current.id,
          label: item.label,
          branch: item.current.currentBranch,
          dirty: item.isDirty,
          conflicts: item.conflictedCount,
          terminals: item.terminalCount,
          ask: stackMembers(item).map((member) => agentRequest(member.path)).find((entry) => entry !== null) ?? null,
        });
      } else if (item.kind === "tab") {
        if (!offscreen.has(`tab:${item.tab.id}`)) continue;
        units.push({
          key: `tab:${item.tab.id}`,
          id: item.tab.id,
          label: item.tab.label,
          branch: item.tab.currentBranch,
          dirty: item.tab.isDirty,
          conflicts: item.tab.conflictedCount,
          terminals: terminalCounts.get(item.tab.path) ?? 0,
          ask: agentRequest(item.tab.path),
        });
      }
    }
    return units;
  });

  // The menu's content can empty while open (the window grew): close it.
  $effect(() => {
    if (overflowUnits.length === 0) untrack(() => { overflowOpen = false; });
  });

  const overflowPlacement = $derived({
    anchor: { kind: "element" as const, element: overflowTriggerEl, gap: 6, align: "end" as const },
    estimate: { width: 256, height: 200 },
    inset: 8,
    revision: overflowUnits.length,
    dismiss: { inside: "[data-overflow-menu], [data-overflow-popup]", resize: true, escape: "none" as const },
    onDismiss: () => { overflowOpen = false; },
  });

  $effect(() => {
    if (overflowOpen && overflowEl) focusPopup(overflowEl);
  });

  /**
   * Open-repository pills have to carry their own plate. Ghost text on the
   * glass strip (`text-textMuted` + a transparent border) drops below AA
   * against the macOS hue field, which is why a single open tab used to
   * disappear beside Fleet.
   */
  function repoTabChrome(tab: { isActive: boolean }): string {
    const viewingRepo = $interfaceStore.globalSurface === "repository";
    if (tab.isActive && viewingRepo) {
      return "border-accent/65 bg-accent/10 text-accent shadow-xs";
    }
    if (tab.isActive) {
      return "border-edgeStrong bg-surfaceHover text-textPrimary";
    }
    return "border-edge bg-background/50 text-textPrimary hover:border-accent/35 hover:bg-accent/5 hover:text-accent";
  }

  function revealRepository() {
    interfaceStore.setGlobalSurface("repository");
  }

  function selectRepoTab(id: string) {
    revealRepository();
    void repoStore.activateTab(id);
  }

  /**
   * Container-level dragover: one handler computes the insertion point for
   * whatever unit (or gap) is under the pointer, so the indicator can't go
   * stale between child elements. The bar is drawn only where `planDrop`
   * says the drop would do something allowed — the indicator and the drop
   * ask the same function.
   */
  function onUnitDragStart(e: DragEvent, unit: StripUnit) {
    if (e.target instanceof Element && e.target.closest("[data-tab-close], [data-stack-switch]")) {
      e.preventDefault();
      return;
    }
    dragFrom = unit;
    const id = unit.kind === "tab" ? unit.id : unitIds(tabLayout, unit)[0] ?? "";
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", id);
      e.dataTransfer.setData("application/x-gitpulse-repo-tab", id);
    }
  }

  function unitAt(target: EventTarget | null): { unit: StripUnit; el: HTMLElement } | null {
    const el = target instanceof Element ? target.closest<HTMLElement>("[data-drop-unit]") : null;
    if (!el) return null;
    if (el.dataset.dropStack) return { unit: { kind: "stack", key: el.dataset.dropStack }, el };
    if (el.dataset.tabId) return { unit: { kind: "tab", id: el.dataset.tabId }, el };
    return null;
  }

  function planAt(e: DragEvent) {
    if (dragFrom === null) return null;
    const hit = unitAt(e.target);
    if (!hit) return null;
    const rect = hit.el.getBoundingClientRect();
    const before = e.clientX < rect.left + rect.width / 2;
    const plan = planDrop(tabLayout, dragFrom, hit.unit, before);
    return plan ? { plan, unit: hit.unit, before } : null;
  }

  function onScrollerDragOver(e: DragEvent) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    const found = planAt(e);
    dropTarget = found ? { unit: unitKey(found.unit), before: found.before } : null;
  }

  function onScrollerDrop(e: DragEvent) {
    e.preventDefault();
    const found = planAt(e);
    const source = dragFrom;
    if (found && source) {
      const ids = unitIds(tabLayout, source);
      const regroup = found.plan.group !== undefined ? { ids, group: found.plan.group } : undefined;
      if (repoStore.arrangeTabs(found.plan.order, regroup) && ids[0]) announceMove(ids[0]);
    }
    endDrag();
  }

  const activeRepoTabId = $derived($repoStore.activeTabId);
  $effect(() => {
    // Re-run on activation changes, not just on element binding — otherwise
    // the newly active tab never scrolls into view (mirrors ViewTabBar). Keyed
    // on the id by value, so a status poll does not yank the strip back to
    // the active tab while someone is scrolling it.
    void activeRepoTabId;
    const active = scroller?.querySelector("[data-active-repo='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

{#snippet surfaceTab(spec: {
  id: "fleet" | "agents" | "tasks";
  open: boolean;
  label: string;
  title: string;
  icon: typeof LayoutGrid;
  onselect: () => void;
  onclose: () => void;
  count?: number;
  countTitle?: string;
  countNoun?: string;
  tour?: string;
})}
  {@const Icon = spec.icon}
  {@const surfaceCount = spec.count ?? 0}
  <!-- The tab and its close control are siblings, as a repository tab's are:
       a second target inside a tab is how a close gets mis-hit. -->
  <div class="{surfaceChipClass(spec.open)} pr-1!" data-testid="{spec.id}-tab-chip" data-surface={spec.id}>
    <button
      type="button"
      role="tab"
      class="flex items-center gap-1.5 flex-1 bg-transparent border-0 p-0 text-inherit font-medium"
      aria-selected={spec.open}
      aria-label={surfaceCount > 0 ? `${spec.label}, ${surfaceCount} ${spec.countNoun ?? ""}`.trim() : spec.label}
      tabindex={spec.open || fallbackStop === spec.id ? 0 : -1}
      data-surface-tab={spec.id}
      data-tour={spec.tour}
      onclick={spec.onselect}
      onkeydown={(event) => {
        if (event.key === "Delete" && spec.open) {
          event.preventDefault();
          spec.onclose();
        }
      }}
      title={spec.title}
    >
      <span class="relative inline-flex">
        <Icon size={12} aria-hidden="true" />
        {#if surfaceCount > 0}
          <!-- Below 1100px the label and pill give way to this badge on the
               icon: a count is never a bare number. -->
          <span
            class="gp-attention-badge bg-accent text-onAccent! @min-[1100px]:hidden"
            aria-hidden="true"
            title={spec.countTitle}
          >{surfaceCount}</span>
        {/if}
      </span>
      <span class="hidden @min-[1100px]:inline">{spec.label}</span>
      {#if surfaceCount > 0}
        <span class="hidden @min-[1100px]:inline-flex" aria-hidden="true" title={spec.countTitle}>
          <span class="gp-chip" data-kind="count" data-tone="accent">{surfaceCount}</span>
        </span>
      {/if}
    </button>
    {#if spec.open}
      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        data-tone="danger"
        tabindex="-1"
        data-testid="{spec.id}-tab-close"
        aria-label="Close {spec.label}"
        title="Close {spec.label}"
        onclick={(event) => {
          event.stopPropagation();
          spec.onclose();
        }}
      >
        <X size={11} aria-hidden="true" />
      </button>
    {/if}
  </div>
{/snippet}

{#snippet groupHead(info?: GroupHeaderItem)}
  {#if info}
    <div
      role="presentation"
      class="group relative flex items-center shrink-0"
      data-group-header={info.group}
      ondragover={(e) => onGroupDragOver(e, info.group, info.isCollapsed)}
      ondragleave={(e) => onGroupDragLeave(e, info.group)}
      ondrop={(e) => onGroupDrop(e, info.group)}
    >
      <button
        type="button"
        class="h-7 px-2 flex items-center gap-1.5 rounded-lg border text-xs font-medium cursor-pointer transition-colors {groupChrome(info)} {dragHoverGroup === info.group ? 'ring-2 ring-accent border-accent' : ''}"
        aria-expanded={!info.isCollapsed}
        aria-label={groupAriaLabel(info)}
        title={groupTitle(info)}
        data-group-head={info.group}
        tabindex={info.isCollapsed && info.hasActiveTab ? 0 : -1}
        onclick={() => onGroupHeadClick(info)}
        oncontextmenu={(e) => onGroupContext(e, info.group)}
        onkeydown={(e) => onGroupKeydown(e, info)}
        onauxclick={(e) => {
          if (e.button === 1) {
            e.preventDefault();
            void confirmCloseGroup(info.group, info.tabCount);
          }
        }}
      >
        {#if info.color}
          <span
            aria-hidden="true"
            data-group-color={info.color}
            title="{TAB_COLOR_LABEL[info.color]} group"
            class="w-[3px] self-stretch my-1 rounded-full shrink-0"
            style:background-color={TAB_COLOR_INK[info.color]}
          ></span>
        {/if}
        {#if info.isCollapsed}
          <ChevronRight size={12} class="shrink-0 text-textMuted group-hover:text-textPrimary transition-transform" />
        {:else}
          <ChevronDown size={12} class="shrink-0 text-textMuted group-hover:text-textPrimary transition-transform" />
        {/if}
        <span
          class="inline-flex shrink-0 {info.color ? '' : info.hasActiveTab ? 'text-accent' : 'text-textMuted'}"
          style:color={info.color ? TAB_COLOR_INK[info.color] : undefined}
        >
          <Folder size={12} />
        </span>
        <span class="whitespace-nowrap font-medium text-meta">{info.label}</span>
        <span class="text-micro tabular-nums font-mono opacity-70">({info.tabCount})</span>
        {#if info.isDirty}
          <span class="w-1.5 h-1.5 rounded-full bg-warn shadow-[0_0_6px_rgb(251_191_36/0.8)] shrink-0" title="{info.label} has uncommitted changes"></span>
        {/if}
        {#if info.conflictedCount > 0}
          <span class="text-warn text-micro font-mono shrink-0" title="{info.conflictedCount} conflicted files in {info.label}">{info.conflictedCount}</span>
        {/if}
        {#if info.terminalCount > 0}
          <span
            class="shrink-0 inline-flex items-center gap-0.5 text-accent"
            title={`${info.terminalCount} terminal session${info.terminalCount === 1 ? '' : 's'} running in ${info.label}`}
          >
            <SquareTerminal size={10} aria-hidden="true" />
            {#if info.terminalCount > 1}
              <span class="text-micro font-medium tabular-nums">{info.terminalCount}</span>
            {/if}
            <span class="sr-only">{info.terminalCount} terminal session{info.terminalCount === 1 ? '' : 's'} running</span>
          </span>
        {/if}
      </button>
    </div>
  {/if}
{/snippet}

{#snippet repoTab(item: TabItem)}
  {@const tab = item.tab}
  {@const index = item.index}
  {@const colorInfo = tabColorInfo(tab)}
  {@const dropHere = dropTarget?.unit === `tab:${tab.id}`}
  <div
    role="presentation"
    data-tab-id={tab.id}
    data-tab-shell-index={index}
    data-drop-unit
    data-stack-member={item.stack ?? undefined}
    data-tab-color={colorInfo.color ?? undefined}
    data-tab-color-source={colorInfo.source ?? undefined}
    draggable="true"
    onpointerenter={(e) => onTabEnter(e, tab.id)}
    onpointerleave={onTabLeave}
    onauxclick={(e) => {
      if (e.button === 1) {
        e.preventDefault();
        void repoStore.closeTab(tab.id);
      }
    }}
    oncontextmenu={(e) => onContext(e, tab.id)}
    ondragstart={(e) => {
      onUnitDragStart(e, { kind: "tab", id: tab.id });
      closeCard();
    }}
    ondragend={endDrag}
    class="group relative min-w-28 pr-1 flex items-center gap-1 rounded-full border shrink-0 cursor-grab active:cursor-grabbing transition-[color,background-color,border-color,opacity] {dragFrom?.kind === 'tab' && dragFrom.id === tab.id
      ? 'opacity-60'
      : ''} {dropHere ? 'border-accent/35' : ''} {repoTabChrome(tab)}"
  >
    {#if dropHere && dropTarget}
      <span
        aria-hidden="true"
        class="absolute top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-full bg-accent shadow-glow transition-opacity {dropTarget.before
          ? 'left-[-3px]'
          : 'right-[-3px]'}"
      ></span>
    {/if}
    {#if colorInfo.color}
      <span
        aria-hidden="true"
        class="ml-1.5 w-[3px] self-stretch my-1.5 rounded-full shrink-0"
        style:background-color={TAB_COLOR_INK[colorInfo.color]}
      ></span>
    {/if}
    <button
    type="button"
    role="tab"
      tabindex={tab.isActive && viewingRepo ? 0 : -1}
      aria-selected={tab.isActive && viewingRepo}
      aria-keyshortcuts="Enter p Delete Control+Shift+ArrowLeft Control+Shift+ArrowRight"
      data-active-repo={tab.isActive ? "true" : "false"}
      data-tab-index={index}
      data-tab-id={tab.id}
      data-parked={tab.watch === "parked" ? "true" : undefined}
      onclick={() => selectRepoTab(tab.id)}
      onkeydown={(e) => {
        if (e.key === "p" || e.key === "P") {
          e.preventDefault();
          repoStore.pinTab(tab.id, !tab.pinned);
        } else if (e.key === "Delete") {
          e.preventDefault();
          void closeTabFromKeyboard(tab.id, index);
        }
      }}
      ondblclick={() => repoStore.pinTab(tab.id, !tab.pinned)}
      class="h-full pl-2.5 flex items-center gap-1.5 text-left rounded-l-full focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent/70"
    >
      {#if tab.pinned}
        <Pin size={10} class="text-accent shrink-0" />
      {:else}
        <FolderGit2 size={11} class="shrink-0 {tab.error ? 'text-danger' : 'text-accent'}" />
      {/if}
      <span class="whitespace-nowrap font-medium {tab.watch === 'parked' ? 'opacity-60' : ''}">{tab.label}</span>
      {#if tab.watch === "parked"}
        <span class="sr-only">, live updates paused</span>
      {/if}
      {#if colorInfo.color}
        <span class="sr-only">{colorInfo.source === "own" ? TAB_COLOR_LABEL[colorInfo.color] : `${TAB_COLOR_LABEL[colorInfo.color]} group color`}</span>
      {/if}
      {#if tab.currentBranch}
        <!-- Branch names are the first thing the strip gives up as the window
             narrows (1280px); the hover card and the status bar still say it. -->
        <span class="whitespace-nowrap text-micro font-mono opacity-80 hidden @min-[1280px]:inline">{tab.currentBranch}</span>
      {/if}
      {#if tab.conflictedCount > 0}
        <span class="text-warn shrink-0">{tab.conflictedCount}</span>
      {/if}
      {#if !(tab.isActive && viewingRepo)}
        {@const ask = agentRequest(tab.path)}
        {#if ask}
          <!-- An agent in a repository you are not looking at is waiting. A
               different shape from the dirty dot, and the only place a
               background tab says so. -->
          <span
            class="shrink-0 inline-flex items-center {ask.tone === 'danger' ? 'text-danger' : ask.tone === 'info' ? 'text-info' : 'text-warn'}"
            data-agent-mark={ask.reason}
          >
            <BellRing size={10} aria-hidden="true" />
            <span class="sr-only">, agent: {ask.phrase.toLowerCase()}</span>
          </span>
        {/if}
      {/if}
      {#if terminalCounts.get(tab.path)}
        <!-- Not a button: the tab itself is the way in, and a second
             click target inside a tab is how a close gets mis-hit. -->
        <span
          class="shrink-0 inline-flex items-center gap-0.5 text-accent"
          title={terminalCounts.get(tab.path) === 1
            ? `1 terminal session running in ${tab.label}`
            : `${terminalCounts.get(tab.path)} terminal sessions running in ${tab.label}`}
        >
          <SquareTerminal size={10} aria-hidden="true" />
          {#if (terminalCounts.get(tab.path) ?? 0) > 1}
            <span class="text-micro font-medium tabular-nums">{terminalCounts.get(tab.path)}</span>
          {/if}
          <!-- `title` on a non-focusable span is not reliably
               announced; the tab's accessible name carries it. -->
          <span class="sr-only">{terminalCounts.get(tab.path)} terminal sessions running</span>
        </span>
      {/if}
    </button>
    {#if tab.isDirty}
      <button type="button" class="shrink-0 grid place-items-center w-5 h-5 rounded-full hover:bg-warn/15 focus-visible:ring-1 focus-visible:ring-accent"
        title="Preview uncommitted changes in {tab.label}" aria-label="Preview uncommitted changes in {tab.label}"
        onclick={() => void repoStore.previewUncommitted(tab.path)}>
        <span class="w-1.5 h-1.5 rounded-full bg-warn shadow-[0_0_6px_rgb(251_191_36/0.8)]"></span>
      </button>
    {/if}
    <button
      type="button"
      tabindex="-1"
      data-tab-close
      title="Close"
      aria-label={`Close ${tab.label}`}
      onclick={(e) => {
        e.stopPropagation();
        void repoStore.closeTab(tab.id);
      }}
      class="ml-auto p-0.5 rounded-full opacity-0 group-hover:opacity-100 hover:bg-background hover:text-danger {tab.isActive ? 'opacity-100' : ''}"
    >
      <X size={11} />
    </button>
  </div>
{/snippet}

{#snippet stackHead(info: StackHeaderItem)}
  {@const current = info.current}
  {@const member = memberOf(info, current)}
  {@const colorInfo = tabColorInfo(current)}
  {@const dropHere = dropTarget?.unit === `stack:${info.key}`}
  {@const switcherOpen = stackMenu?.key === info.key}
  <!-- One header for every checkout of a repository. Folded, it is a tab for
       the checkout it shows plus a switcher for the rest; unfolded, it is a
       label in front of the checkouts' own tabs. -->
  <div
    role="presentation"
    data-stack-shell={info.key}
    data-drop-unit
    data-drop-stack={info.key}
    data-stack-expanded={info.isExpanded ? "true" : "false"}
    data-tab-color={!info.isExpanded ? colorInfo.color ?? undefined : undefined}
    title={`${info.label} — ${info.tabCount} checkouts of ${info.root}\nShowing ${member.primary ? "the primary checkout" : member.name}${current.currentBranch ? ` on ${current.currentBranch}` : ""}\nDrag to move them together · ↓ for the switcher · + to show each as a tab`}
    draggable="true"
    ondragstart={(e) => onUnitDragStart(e, { kind: "stack", key: info.key })}
    ondragend={endDrag}
    oncontextmenu={(e) => openStackMenu(e, info)}
    class="group relative flex items-center shrink-0 rounded-full border cursor-grab active:cursor-grabbing transition-[color,background-color,border-color,opacity] {info.isExpanded
      ? groupChrome(info)
      : repoTabChrome({ isActive: info.hasActiveTab })} {dragFrom?.kind === 'stack' && dragFrom.key === info.key
      ? 'opacity-60'
      : ''} {dropHere ? 'border-accent/35' : ''}"
  >
    {#if dropHere && dropTarget}
      <span
        aria-hidden="true"
        class="absolute top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-full bg-accent shadow-glow transition-opacity {dropTarget.before
          ? 'left-[-3px]'
          : 'right-[-3px]'}"
      ></span>
    {/if}
    {#if info.isExpanded}
      <button
        type="button"
        class="h-7 pl-2 pr-2.5 flex items-center gap-1.5 rounded-full text-meta font-medium focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent/70"
        aria-expanded="true"
        aria-label={`${info.label}, ${info.tabCount} checkouts shown as tabs. Fold them into one tab`}
        data-stack-head={info.key}
        tabindex="-1"
        onclick={() => setExpanded(info, false)}
        onkeydown={(e) => onStackKeydown(e, info)}
      >
        <ChevronDown size={12} class="shrink-0 text-textMuted group-hover:text-textPrimary" />
        <Layers size={11} class="shrink-0 {info.hasActiveTab ? 'text-accent' : 'text-textMuted'}" />
        <span class="whitespace-nowrap">{info.label}</span>
        <span class="text-micro tabular-nums font-mono opacity-70">({info.tabCount})</span>
      </button>
    {:else}
      {#if colorInfo.color}
        <span
          aria-hidden="true"
          class="ml-1.5 w-[3px] self-stretch my-1.5 rounded-full shrink-0"
          style:background-color={TAB_COLOR_INK[colorInfo.color]}
        ></span>
      {/if}
      <button
        type="button"
        role="tab"
        tabindex={info.hasActiveTab && viewingRepo ? 0 : -1}
        aria-selected={info.hasActiveTab && viewingRepo}
        aria-label={stackAriaLabel(info)}
        aria-keyshortcuts="Enter ArrowDown + Control+Shift+ArrowLeft Control+Shift+ArrowRight"
        data-stack-head={info.key}
        data-tab-id={current.id}
        data-active-repo={info.hasActiveTab ? "true" : "false"}
        onclick={() => selectRepoTab(current.id)}
        onkeydown={(e) => onStackKeydown(e, info)}
        class="h-7 pl-2.5 pr-1 flex items-center gap-1.5 text-left rounded-l-full focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent/70"
      >
        <Layers size={11} class="shrink-0 {current.error ? 'text-danger' : 'text-accent'}" />
        <span class="whitespace-nowrap font-medium">{info.label}</span>
        {#if member.primary}
          {#if current.currentBranch}
            <span class="whitespace-nowrap text-micro font-mono opacity-80 hidden @min-[1280px]:inline">{current.currentBranch}</span>
          {/if}
        {:else}
          <span class="whitespace-nowrap text-micro font-mono opacity-80 max-w-40 truncate" data-stack-current>{member.name}</span>
        {/if}
        {#if info.conflictedCount > 0}
          <span class="text-warn shrink-0" title="{info.conflictedCount} conflicted files across {info.label}">{info.conflictedCount}</span>
        {/if}
        {#if !(info.hasActiveTab && viewingRepo)}
          {@const ask = stackMembers(info).map((member) => agentRequest(member.path)).find((entry) => entry !== null)}
          {#if ask}
            <span
              class="shrink-0 inline-flex items-center {ask.tone === 'danger' ? 'text-danger' : ask.tone === 'info' ? 'text-info' : 'text-warn'}"
              data-agent-mark={ask.reason}
            >
              <BellRing size={10} aria-hidden="true" />
              <span class="sr-only">, agent: {ask.phrase.toLowerCase()}</span>
            </span>
          {/if}
        {/if}
        {#if info.terminalCount > 0}
          <span
            class="shrink-0 inline-flex items-center gap-0.5 text-accent"
            title={`${info.terminalCount} terminal session${info.terminalCount === 1 ? "" : "s"} running across ${info.label}`}
          >
            <SquareTerminal size={10} aria-hidden="true" />
            {#if info.terminalCount > 1}
              <span class="text-micro font-medium tabular-nums">{info.terminalCount}</span>
            {/if}
          </span>
        {/if}
      </button>
      {#if current.isDirty}
        <button type="button" class="shrink-0 grid place-items-center w-5 h-5 rounded-full hover:bg-warn/15 focus-visible:ring-1 focus-visible:ring-accent"
          tabindex="-1"
          title="Preview uncommitted changes in {member.name}" aria-label="Preview uncommitted changes in {member.name}"
          onclick={() => void repoStore.previewUncommitted(current.path)}>
          <span class="w-1.5 h-1.5 rounded-full bg-warn shadow-[0_0_6px_rgb(251_191_36/0.8)]"></span>
        </button>
      {/if}
      <button
        type="button"
        tabindex="-1"
        data-stack-switch={info.key}
        aria-haspopup="menu"
        aria-expanded={switcherOpen}
        aria-controls={switcherOpen ? "repo-stack-menu" : undefined}
        aria-label={`Switch checkout of ${info.label} (${info.tabCount} open)`}
        title={`${info.tabCount} checkouts of ${info.label} — switch, show as tabs, or close`}
        onclick={(e) => openStackMenu(e, info)}
        class="mr-1 h-5 pl-1.5 pr-1 flex items-center gap-0.5 rounded-full border border-edge bg-background/60 text-micro font-mono tabular-nums hover:border-accent/35 hover:text-accent {switcherOpen ? 'border-accent/65 text-accent' : ''}"
      >
        <span>{info.tabCount}</span>
        {#if info.isDirty && !current.isDirty}
          <!-- Another checkout has changes: say so without naming which until asked. -->
          <span class="w-1 h-1 rounded-full bg-warn" aria-hidden="true"></span>
        {/if}
        <ChevronDown size={10} class="shrink-0" />
      </button>
    {/if}
  </div>
{/snippet}

  <!-- Row 1 of the header (HEADER_SPEC section 2). The window's title bar
       hosts this: the surfaces and the repositories are one tablist with one
       selected tab and one roving stop; Open, the drag zone and the workspace
       actions follow it. -->
  <div class="gp-repo-strip relative flex min-w-0 flex-1 items-center gap-1.5 self-stretch text-xs" data-repo-strip>
    <div
      bind:this={tablist}
      role="tablist"
      tabindex="-1"
      aria-label="Workspace"
      data-workspace-tablist
      class="flex min-w-0 shrink items-center gap-1.5 self-stretch"
      onkeydown={onTablistKeydown}
    >
      <!-- Fleet, Agents and Tasks are peers of a repository tab, not of a
           view: one surface for the whole workspace, not another repository. -->
      {@render surfaceTab({
        id: "fleet",
        open: fleetOpen,
        label: "Fleet",
        title: "Fleet — every open repository at a glance: changes, sync, worktrees, agents and, on demand, size and health.",
        icon: LayoutGrid,
        onselect: () => interfaceStore.toggleFleet(),
        onclose: () => interfaceStore.setFleetOpen(false),
      })}
      {@render surfaceTab({
        id: "agents",
        open: agentsOpen,
        label: "Agents",
        title: "Agents — checkouts on disk, processes this window started, and runs that need you. A checkout is not a running process.",
        icon: Bot,
        onselect: () => interfaceStore.setAgentsOpen(true),
        onclose: () => interfaceStore.setAgentsOpen(false),
        count: liveAgents,
        countTitle: `${plural(liveAgents, "live agent terminal", "live agent terminals")} this window started`,
        countNoun: "running",
      })}
      {@render surfaceTab({
        id: "tasks",
        open: tasksOpen,
        label: "Tasks",
        title: "Tasks — global, workspace and repository Kanban boards",
        icon: ListChecks,
        onselect: () => interfaceStore.setTasksOpen(true),
        onclose: () => interfaceStore.setTasksOpen(false),
        count: $taskChrome.openTabs,
        countTitle: `${$taskChrome.openTabs} open ${$taskChrome.openTabs === 1 ? "task" : "tasks"}`,
        countNoun: "open",
        tour: "tasks",
      })}
    {#if !hideTabStrip}
      <div class="gp-separator m-0! h-3.5! w-0.5! shrink-0" aria-hidden="true"></div>
      <div class="relative min-w-0 max-w-full shrink self-stretch">
      <div
        bind:this={scroller}
        class="gp-header-scroll h-full flex items-center gap-1 min-w-0 py-1"
        data-repo-strip-scroller
        role="presentation"
        ondragover={onScrollerDragOver}
        ondrop={onScrollerDrop}
        oncontextmenu={onStripContext}
      >
        {#each tabLayout.visibleItems as item (`${item.kind}:${item.id}`)}
          {#if item.kind === "group-header"}
            {@render groupHead(item)}
          {:else if item.kind === "stack-header"}
            {@render stackHead(item)}
          {:else}
            {@render repoTab(item)}
          {/if}
        {/each}
      </div>
      <ScrollCue target={scroller} axis="x" diffuse />
      </div>
    {/if}
    </div>

    {#if !hideTabStrip && overflowUnits.length > 0}
      <!-- Only when the strip hides something: a menu of the units that are
           off screen, with their marks, so nothing is reachable only by
           scrolling. Renders nothing when the strip fits. -->
      <div class="relative shrink-0" data-overflow-menu>
        <button
          bind:this={overflowTriggerEl}
          type="button"
          class="gp-btn"
          data-size="sm"
          data-testid="repo-overflow"
          aria-haspopup="menu"
          aria-expanded={overflowOpen}
          aria-controls="repo-overflow-menu"
          aria-label="{overflowUnits.length} more {overflowUnits.length === 1 ? 'repository' : 'repositories'}"
          title="Repositories scrolled out of view"
          onclick={() => {
            menu = null;
            recentsOpen = false;
            overflowOpen = !overflowOpen;
          }}
        >
          <span>{overflowUnits.length} more</span>
          <ChevronDown size={12} aria-hidden="true" />
        </button>
      </div>
    {/if}

    <!-- Open follows the strip like a browser's new-tab control, and the drag
         zone after it keeps a 48px minimum (E3): the tab strip scrolls before
         that space shrinks, so the window can always be grabbed.
         Open and its menu arrow are one split pill: Open's square right edge
         is the divider, the arrow drops its own left border, and `-ml-1.5`
         takes back the strip's gap between them. -->
    <button
      type="button"
      title="Open repository"
      aria-label="Open repository"
      data-testid="open-repo-tab"
      data-tour="repository"
      onclick={() => onOpen?.()}
      class="gp-btn h-[26px] py-0! pl-2.5! pr-2! shrink-0 rounded-r-none! active:transform-none!"
    >
      <Plus size={13} class="text-accent" />
      <span class="hidden @min-[1100px]:inline">Open</span>
    </button>

    <div class="relative shrink-0 -ml-1.5" data-recents-menu>
      <button
        bind:this={recentsTriggerEl}
        type="button"
        title="Clone or open a recent repository"
        aria-label="Clone or open a recent repository"
        aria-haspopup="menu"
        aria-expanded={recentsOpen}
        aria-controls="recent-repositories-menu"
        onclick={() => {
          menu = null;
          recentsOpen = !recentsOpen;
        }}
        data-testid="open-repo-menu"
        class="gp-btn h-[26px] py-0! px-1.5! rounded-l-none! border-l-0! active:transform-none!"
      >
        <ChevronDown size={13} />
      </button>
      {#if recentsOpen}
        <div
          bind:this={recentsEl}
          use:portal={"body"}
          use:popover={recentsPlacement}
          data-recents-menu
          id="recent-repositories-menu"
          role="menu"
          aria-label="Clone or open a recent repository"
          tabindex="-1"
          onkeydown={handlePopupKeydown}
          class="fixed w-80 max-w-[calc(100vw-1rem)] gp-menu gp-pop flex flex-col max-h-[min(28rem,calc(100vh-7.5rem))] shadow-float"
          style="z-index: {LAYERS.MENU}"
        >
          {#if onClone}
            <button
              type="button"
              role="menuitem"
              data-testid="clone-repo-item"
              class="gp-menu-item shrink-0"
              onclick={() => {
                recentsOpen = false;
                onClone?.();
              }}
            >
              <Download size={13} class="text-accent" />
              Clone repository…
            </button>
            <div class="gp-separator shrink-0" aria-hidden="true"></div>
          {/if}
          <div class="px-2 pt-1 pb-1.5 gp-label shrink-0">
            Recent repositories
          </div>
          {#if $repoStore.recentRepos.length === 0}
            <div class="px-3 py-2 text-textMuted">No recent repositories</div>
          {:else}
            <div class="overflow-y-auto min-h-0 flex-1 overscroll-contain space-y-0.5">
              {#each $repoStore.recentRepos as path}
                <div class="flex items-center gap-1 px-0.5">
                  <button
                    type="button"
                    role="menuitem"
                    onclick={() => {
                      recentsOpen = false;
                      void repoStore.openRepo(path);
                    }}
                    class="flex-1 min-w-0 px-2 py-1.5 text-left hover:bg-surfaceHover rounded-lg transition-colors"
                  >
                    <div class="truncate text-textPrimary">{displayName(path)}</div>
                    <div class="truncate text-micro text-textMuted font-mono">{path}</div>
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    aria-label={`Remove ${displayName(path)} from recent repositories`}
                    title="Remove from recents"
                    onclick={() => repoStore.removeRecent(path)}
                    class="p-1 rounded-full text-textMuted hover:text-danger hover:bg-surfaceHover shrink-0"
                  >
                    <X size={11} />
                  </button>
                </div>
              {/each}
            </div>
          {/if}
          {#if unusedRecents.length === 0 && $repoStore.recentRepos.length > 0}
            <div class="px-3 py-1.5 text-micro text-textMuted shrink-0">All recents are already open</div>
          {/if}
        </div>
      {/if}
    </div>

    <!-- The window-drag zone (E3). `min-w-12` is the 48px floor: the free
         space belongs to the strip until it is gone, but never below this. A
         bare drag-region attribute starts a drag only on a direct hit, so
         nothing clickable is affected. -->
    <div class="h-full min-w-12 flex-1 self-stretch" data-tauri-drag-region data-drag-zone></div>

    <!-- Workspace-wide actions live here rather than in a view: they act on
         every open repository, so they belong to the title bar that owns
         them. Hidden with a single tab open, where they say nothing new. -->
    <WorkspaceActions />
    <div class="sr-only" role="status" aria-live="polite" data-move-announcement>{moveAnnouncement}</div>
  </div>

{#snippet colorChoices(
  label: string,
  current: TabColor | null,
  detail: string | null,
  onPick: (color: TabColor | null) => void,
)}
  <div class="px-2 py-1.5" role="group" aria-label={label}>
    <div class="gp-label">{label}</div>
    {#if detail}
      <div class="pt-0.5 text-micro text-textMuted">{detail}</div>
    {/if}
    <div class="flex items-center gap-1 pt-1">
      <button
        type="button"
        role="menuitem"
        aria-label={current === null ? "No color, selected" : "No color"}
        data-color-choice="none"
        title="No color"
        class="relative size-4 rounded-full border border-border bg-surface {current === null ? 'ring-2 ring-textPrimary' : ''}"
        onclick={() => onPick(null)}
      >
        <span class="pointer-events-none absolute left-1/2 top-1/2 h-px w-3 -translate-x-1/2 -translate-y-1/2 rotate-45 bg-textMuted"></span>
      </button>
      {#each TAB_COLORS as color (color)}
        <button
          type="button"
          role="menuitem"
          aria-label={current === color ? `${TAB_COLOR_LABEL[color]}, selected` : TAB_COLOR_LABEL[color]}
          data-color-choice={color}
          title={TAB_COLOR_LABEL[color]}
          class="size-4 rounded-full border border-edgeSoft {current === color ? 'ring-2 ring-textPrimary' : ''}"
          style:background-color={TAB_COLOR_INK[color]}
          onclick={() => onPick(color)}
        ></button>
      {/each}
    </div>
  </div>
{/snippet}

{#if card && cardTab}
  {@const stop = activationStops(tabLayout).indexOf(cardTab.id)}
  <!-- Justified: pointer enter/leave only keep the card open while it is under
       the pointer; it holds no controls. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    use:portal={"body"}
    use:popover={cardPlacement}
    data-tab-card={cardTab.id}
    role="group"
    aria-label="{cardTab.label} details"
    onpointerenter={onCardEnter}
    onpointerleave={onTabLeave}
    class="fixed w-72 max-w-[calc(100vw-1rem)] gp-menu gp-pop p-2.5 text-meta text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <p class="truncate text-xs font-medium">{cardTab.label}</p>
    <dl class="mt-1.5 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
      {#each cardRows as row (row.label)}
        <dt class="text-textMuted">{row.label}</dt>
        <dd class="min-w-0 break-words {row.label === 'Path' ? 'font-mono text-micro' : ''} {row.unknown ? 'text-textMuted' : ''}">{row.value}</dd>
      {/each}
    </dl>
    {#if cardTab.watch === "parked"}
      <p class="mt-1.5 text-textMuted">{PARKED_TITLE}</p>
    {/if}
    <p class="mt-1.5 text-micro text-textMuted">
      Drag to reorder · P to {cardTab.pinned ? "unpin" : "pin"} · {shortcutTextLabel("⌃⇧←/→", $hostPlatform.os)} to move{stop >= 0 && stop < 9 ? ` · ${shortcutTextLabel(`⌃⌥${stop + 1}`, $hostPlatform.os)} to jump here` : ""}
    </p>
  </div>
{/if}

{#if overflowOpen && overflowUnits.length > 0}
  <div
    bind:this={overflowEl}
    use:portal={"body"}
    use:popover={overflowPlacement}
    data-overflow-popup
    id="repo-overflow-menu"
    role="menu"
    aria-label="Repositories scrolled out of view"
    tabindex="-1"
    onkeydown={handlePopupKeydown}
    class="fixed w-64 max-w-[calc(100vw-1rem)] gp-menu gp-pop flex flex-col max-h-[min(24rem,calc(100vh-7.5rem))] shadow-float text-meta text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <div class="overflow-y-auto min-h-0 flex-1 overscroll-contain">
      {#each overflowUnits as unit (unit.key)}
        <button
          type="button"
          role="menuitem"
          data-overflow-unit={unit.key}
          class="gp-menu-item"
          onclick={() => {
            overflowOpen = false;
            selectRepoTab(unit.id);
          }}
        >
          <span class="min-w-0 flex-1 truncate">{unit.label}</span>
          {#if unit.branch}
            <span class="shrink-0 truncate max-w-24 font-mono text-micro text-textMuted">{unit.branch}</span>
          {/if}
          {#if unit.ask}
            <span class="shrink-0 {unit.ask.tone === 'danger' ? 'text-danger' : unit.ask.tone === 'info' ? 'text-info' : 'text-warn'}">
              <BellRing size={10} aria-hidden="true" />
              <span class="sr-only">Agent: {unit.ask.phrase.toLowerCase()}</span>
            </span>
          {/if}
          {#if unit.conflicts > 0}
            <span class="shrink-0 text-warn">{unit.conflicts}<span class="sr-only"> conflicts</span></span>
          {/if}
          {#if unit.terminals > 0}
            <span class="shrink-0 inline-flex items-center gap-0.5 text-accent">
              <SquareTerminal size={10} aria-hidden="true" />{unit.terminals}<span class="sr-only"> terminal sessions</span>
            </span>
          {/if}
          {#if unit.dirty}
            <span class="shrink-0 size-1.5 rounded-full bg-warn" aria-hidden="true"></span>
            <span class="sr-only">Uncommitted changes</span>
          {/if}
        </button>
      {/each}
    </div>
  </div>
{/if}

{#if menu}
  {@const tab = $repoStore.openTabs.find((item) => item.id === menu?.id)}
  {#if tab}
    {@const canMoveLeft = repoStore.canMoveTab(tab.id, -1)}
    {@const canMoveRight = repoStore.canMoveTab(tab.id, 1)}
    <div
      bind:this={menuEl}
      use:portal={"body"}
      use:popover={menuDismissal}
      data-repo-menu
      role="menu"
      aria-label={`Repository actions for ${tab.label}`}
      tabindex="-1"
      onkeydown={handlePopupKeydown}
      class="fixed min-w-44 gp-menu gp-pop text-meta text-textPrimary"
      style="z-index: {LAYERS.MENU}"
    >
      <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.pinTab(tab.id, !tab.pinned); closeMenu(); }}>
        {tab.pinned ? "Unpin" : "Pin"} tab
      </button>
      {@render colorChoices(
        "Tab color",
        normalizeTabColor(tab.color),
        !normalizeTabColor(tab.color) && tab.group
          ? inheritedColorNote(tab.group)
          : null,
        (color) => repoStore.setTabColor(tab.id, color),
      )}
      <button
        role="menuitem"
        class="gp-menu-item {canMoveLeft ? '' : 'opacity-40 pointer-events-none'}"
        aria-disabled={!canMoveLeft}
        onclick={() => {
          if (!canMoveLeft) return;
          moveFocusedTabBy(tab.id, -1);
          closeMenu();
        }}
      >
        Move left
      </button>
      <button
        role="menuitem"
        class="gp-menu-item {canMoveRight ? '' : 'opacity-40 pointer-events-none'}"
        aria-disabled={!canMoveRight}
        onclick={() => {
          if (!canMoveRight) return;
          moveFocusedTabBy(tab.id, 1);
          closeMenu();
        }}
      >
        Move right
      </button>
      <button
        role="menuitem"
        class="gp-menu-item {canMoveLeft ? '' : 'opacity-40 pointer-events-none'}"
        aria-disabled={!canMoveLeft}
        onclick={() => {
          if (!canMoveLeft) return;
          moveFocusedTabTo(tab.id, "start");
          closeMenu();
        }}
      >
        Move to start
      </button>
      <button
        role="menuitem"
        class="gp-menu-item {canMoveRight ? '' : 'opacity-40 pointer-events-none'}"
        aria-disabled={!canMoveRight}
        onclick={() => {
          if (!canMoveRight) return;
          moveFocusedTabTo(tab.id, "end");
          closeMenu();
        }}
      >
        Move to end
      </button>
      <span class="gp-menu-sep" aria-hidden="true"></span>
      <button role="menuitem" class="gp-menu-item" onclick={() => void copyPath(tab.path)}>
        Copy path
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => {
        const id = tab.id;
        closeMenu();
        void repoStore.revokeTrust(id);
      }}>
        Revoke repository trust…
      </button>
      <span class="gp-menu-sep" aria-hidden="true"></span>
      {#if tab.group}
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          const id = tab.id;
          const grp = tab.group;
          closeMenu();
          void promptSetGroup(id, grp);
        }}>
          Change group… ({tab.group})
        </button>
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          repoStore.setTabGroup(tab.id, null);
          closeMenu();
        }}>
          Remove from group
        </button>
        {#if isTabInCollapsedGroup(tab)}
          <button role="menuitem" class="gp-menu-item" onclick={() => {
            repoStore.setGroupCollapsed(tab.group!, false);
            setGroupExplicitlyExpanded(tab.group!, true);
            closeMenu();
          }}>
            Expand group "{tab.group}"
          </button>
        {:else}
          <button role="menuitem" class="gp-menu-item" onclick={() => {
            repoStore.setGroupCollapsed(tab.group!, true);
            closeMenu();
          }}>
            Collapse group "{tab.group}"
          </button>
        {/if}
        {@const tabGrpHeader = groupHeadersByName.get(tab.group)}
        {#if tabGrpHeader && !tabGrpHeader.isCollapsed && tabGrpHeader.collapsedInactiveCount === 0}
          <button role="menuitem" class="gp-menu-item" onclick={() => {
            setGroupInactiveCollapsed(tab.group!, true);
            closeMenu();
          }}>
            Collapse inactive tabs in "{tab.group}"
          </button>
        {:else if tabGrpHeader && tabGrpHeader.collapsedInactiveCount > 0}
          <button role="menuitem" class="gp-menu-item" onclick={() => {
            setGroupExplicitlyExpanded(tab.group!, true);
            closeMenu();
          }}>
            Expand all tabs in "{tab.group}"
          </button>
        {/if}
        {#if tabGrpHeader && tabGrpHeader.terminalCount > 0}
          <button
            role="menuitem"
            class="gp-menu-item"
            onclick={() => {
              interfaceStore.toggleCollapseInactiveIncludeShells();
              closeMenu();
            }}
          >
            {$interfaceStore.collapseInactiveIncludeShells ? "Keep tabs with active shells visible" : "Collapse tabs with active shells"}
          </button>
        {/if}
      {:else}
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          const id = tab.id;
          closeMenu();
          void promptSetGroup(id);
        }}>
          Add to group…
        </button>
      {/if}
      {#if distinctGroupNames.length > 0}
        {#each distinctGroupNames as grp}
          {#if grp !== tab.group}
            <button role="menuitem" class="gp-menu-item" onclick={() => {
              repoStore.setTabGroup(tab.id, grp);
              closeMenu();
            }}>
              Move to group "{grp}"
            </button>
          {/if}
        {/each}
      {/if}
      <button role="menuitem" class="gp-menu-item" onclick={() => {
        repoStore.groupByParentFolder();
        closeMenu();
      }}>
        Group all by parent folder
      </button>
      {#if $repoStore.openTabs.some((t) => t.group)}
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          repoStore.ungroupTabs();
          closeMenu();
        }}>
          Ungroup all repositories
        </button>
      {/if}
      {#if distinctGroupNames.length > 0}
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          repoStore.collapseInactiveGroups();
          closeMenu();
        }}>
          Collapse inactive groups
        </button>
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          interfaceStore.setCollapseInactiveGroupTabs(!$interfaceStore.collapseInactiveGroupTabs);
          closeMenu();
        }}>
          {$interfaceStore.collapseInactiveGroupTabs ? "Show all tabs in groups" : "Collapse inactive tabs in groups"}
        </button>
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            interfaceStore.toggleCollapseInactiveIncludeShells();
            closeMenu();
          }}
        >
          {$interfaceStore.collapseInactiveIncludeShells ? "Keep tabs with active shells visible" : "Collapse tabs with active shells"}
        </button>
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          repoStore.collapseAllGroups();
          closeMenu();
        }}>
          Collapse all groups
        </button>
        <button role="menuitem" class="gp-menu-item" onclick={() => {
          repoStore.expandAllGroups();
          closeMenu();
        }}>
          Expand all groups
        </button>
      {/if}
      {#if tab.group}
        <button role="menuitem" class="gp-menu-item text-danger hover:text-danger" onclick={() => {
          const g = tab.group!;
          const count = groupHeadersByName.get(g)?.tabCount ?? 1;
          closeMenu();
          void confirmCloseGroup(g, count);
        }}>
          Close group "{tab.group}"…
        </button>
      {/if}
      <span class="gp-menu-sep" aria-hidden="true"></span>
      <button role="menuitem" class="gp-menu-item" onclick={() => { void repoStore.closeTab(tab.id); closeMenu(); }}>
        Close
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { void repoStore.closeOtherTabs(tab.id); closeMenu(); }}>
        Close others
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { void repoStore.closeTabsToTheRight(tab.id); closeMenu(); }}>
        Close tabs to the right
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { onOpen?.(); closeMenu(); }}>
        <FolderOpen size={11} /> Open repository…
      </button>
    </div>
  {/if}
{/if}

{#if groupMenu}
  {@const groupHeader = groupHeadersByName.get(groupMenu.group)}
  {#if groupHeader}
    <div
      bind:this={groupMenuEl}
      use:portal={"body"}
      use:popover={groupMenuDismissal}
      data-group-menu
      role="menu"
      aria-label={`Group actions for ${groupHeader.label}`}
      tabindex="-1"
      onkeydown={handlePopupKeydown}
      class="fixed min-w-44 gp-menu gp-pop text-meta text-textPrimary"
      style="z-index: {LAYERS.MENU}"
    >
      <div class="px-2 pt-1 pb-1.5 gp-label shrink-0">
        Group: {groupHeader.label} ({groupHeader.tabCount})
      </div>
      {@render colorChoices(
        "Group color",
        groupHeader.color,
        null,
        (color) => repoStore.setGroupColor(groupHeader.group, color),
      )}
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          if (groupHeader.isCollapsed || groupHeader.collapsedInactiveCount > 0) {
            if (collapsedGroupSet.has(groupHeader.group)) repoStore.setGroupCollapsed(groupHeader.group, false);
            setGroupExplicitlyExpanded(groupHeader.group, true);
          } else {
            repoStore.setGroupCollapsed(groupHeader.group, true);
          }
          closeGroupMenu();
        }}
      >
        {groupHeader.isCollapsed || groupHeader.collapsedInactiveCount > 0 ? "Expand group" : "Collapse group"}
      </button>
      {#if groupHeader.isCollapsed}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            if (collapsedGroupSet.has(groupHeader.group)) repoStore.setGroupCollapsed(groupHeader.group, false);
            setGroupInactiveCollapsed(groupHeader.group, true);
            closeGroupMenu();
          }}
        >
          Collapse inactive tabs
        </button>
      {:else if groupHeader.collapsedInactiveCount === 0}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            setGroupInactiveCollapsed(groupHeader.group, true);
            closeGroupMenu();
          }}
        >
          Collapse inactive tabs
        </button>
      {:else}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            setGroupExplicitlyExpanded(groupHeader.group, true);
            closeGroupMenu();
          }}
        >
          Expand all tabs in group
        </button>
      {/if}
      {#if groupHeader.terminalCount > 0}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            interfaceStore.toggleCollapseInactiveIncludeShells();
            closeGroupMenu();
          }}
        >
          {$interfaceStore.collapseInactiveIncludeShells ? "Keep tabs with active shells visible" : "Collapse tabs with active shells"}
        </button>
      {/if}
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          repoStore.collapseInactiveGroups();
          closeGroupMenu();
        }}
      >
        Collapse inactive groups
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          interfaceStore.setCollapseInactiveGroupTabs(!$interfaceStore.collapseInactiveGroupTabs);
          closeGroupMenu();
        }}
      >
        {$interfaceStore.collapseInactiveGroupTabs ? "Show all tabs in groups" : "Collapse inactive tabs in all groups"}
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          repoStore.expandAllGroups();
          closeGroupMenu();
        }}
      >
        Expand all groups
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          repoStore.collapseAllGroups();
          closeGroupMenu();
        }}
      >
        Collapse all groups
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          const g = groupHeader.group;
          closeGroupMenu();
          void promptRenameGroup(g);
        }}
      >
        Rename group…
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          repoStore.ungroupTabs(groupHeader.group);
          closeGroupMenu();
        }}
      >
        Ungroup repositories
      </button>
      {#if ungroupedTabs.length > 0}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            for (const t of ungroupedTabs) {
              repoStore.setTabGroup(t.id, groupHeader.group);
            }
            closeGroupMenu();
          }}
        >
          Add open ungrouped repositories ({ungroupedTabs.length})
        </button>
      {/if}
      <span class="gp-menu-sep" aria-hidden="true"></span>
      {#if tabLayout.groups.length > 1}
        <button
          role="menuitem"
          class="gp-menu-item text-danger hover:text-danger"
          onclick={() => void confirmCloseOtherGroups(groupHeader.group)}
        >
          Close other groups…
        </button>
      {/if}
      <button
        role="menuitem"
        class="gp-menu-item text-danger hover:text-danger"
        onclick={() => {
          const g = groupHeader.group;
          const count = groupHeader.tabCount;
          closeGroupMenu();
          void confirmCloseGroup(g, count);
        }}
      >
        Close group repositories…
      </button>
    </div>
  {/if}
{/if}

{#if stackMenu}
  {@const info = stackHeadersByKey.get(stackMenu.key)}
  {#if info}
    {@const members = stackMembers(info)}
    <div
      bind:this={stackMenuEl}
      use:portal={"body"}
      use:popover={stackMenuDismissal}
      id="repo-stack-menu"
      data-stack-menu={info.key}
      role="menu"
      aria-label={`Checkouts of ${info.label}`}
      tabindex="-1"
      onkeydown={handlePopupKeydown}
      class="fixed w-72 max-w-[calc(100vw-1rem)] gp-menu gp-pop text-meta text-textPrimary flex flex-col"
      style="z-index: {LAYERS.MENU}"
    >
      <div class="px-2 pt-1 pb-1.5 gp-label shrink-0 truncate" title={info.root}>
        {info.label} · {info.tabCount} checkouts
      </div>
      <div class="overflow-y-auto overscroll-contain min-h-0 max-h-[min(22rem,calc(100vh-10rem))] space-y-0.5">
        {#each members as tab (tab.id)}
          {@const name = memberOf(info, tab)}
          <div class="flex items-center gap-1 px-0.5" data-stack-member-row={tab.id}>
            <button
              type="button"
              role="menuitem"
              aria-current={tab.isActive ? "true" : undefined}
              class="flex-1 min-w-0 px-2 py-1.5 text-left rounded-lg transition-colors hover:bg-surfaceHover {tab.isActive ? 'bg-accent/10 text-accent' : ''}"
              title={tab.path}
              onclick={() => {
                // Read before closing: the menu's values derive from its state.
                const id = tab.id;
                closeMenu();
                selectRepoTab(id);
              }}
            >
              <div class="flex items-center gap-1.5 min-w-0">
                <span class="truncate font-medium">{name.primary ? `${name.name} (primary)` : name.name}</span>
                {#if name.agent}
                  <span class="gp-pill !px-1.5 !py-0 shrink-0 text-micro" title="Worktree created by {name.agent}">{name.agent}</span>
                {/if}
                {#if tab.isDirty}
                  <span class="w-1.5 h-1.5 rounded-full bg-warn shrink-0" title="Uncommitted changes"></span>
                  <span class="sr-only">uncommitted changes</span>
                {/if}
                {#if tab.conflictedCount > 0}
                  <span class="text-warn shrink-0 font-mono" title="{tab.conflictedCount} conflicted files">{tab.conflictedCount}</span>
                {/if}
                {#if terminalCounts.get(tab.path)}
                  <span class="shrink-0 inline-flex items-center gap-0.5 text-accent" title="{terminalCounts.get(tab.path)} terminal sessions running">
                    <SquareTerminal size={10} aria-hidden="true" />
                    <span class="text-micro tabular-nums">{terminalCounts.get(tab.path)}</span>
                  </span>
                {/if}
              </div>
              <div class="truncate text-micro text-textMuted font-mono">
                {tab.currentBranch ?? (tab.error ? "unavailable" : tab.isLoading ? "loading…" : "detached")}
              </div>
            </button>
            <button
              type="button"
              role="menuitem"
              aria-label={`Close ${name.name}`}
              title="Close this tab (the worktree stays on disk)"
              class="p-1 rounded-full text-textMuted hover:text-danger hover:bg-surfaceHover shrink-0"
              onclick={() => {
                const id = tab.id;
                // The focused row goes with the tab; put focus back in the
                // menu so arrow keys keep working (the menu closes itself if
                // the stack dissolves).
                void repoStore.closeTab(id).then(() => {
                  if (stackMenuEl?.isConnected) focusPopup(stackMenuEl);
                });
              }}
            >
              <X size={11} />
            </button>
          </div>
        {/each}
      </div>
      <span class="gp-menu-sep" aria-hidden="true"></span>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          const target = info;
          const open = !info.isExpanded;
          closeMenu();
          setExpanded(target, open);
        }}
      >
        {info.isExpanded ? "Fold into one tab" : "Show each checkout as a tab"}
      </button>
      {#if info.tabCount > 1}
        <button role="menuitem" class="gp-menu-item text-danger hover:text-danger" onclick={() => { const target = info; void confirmCloseCheckouts(target, target.current.id); }}>
          Close other worktrees…
        </button>
      {/if}
      <button role="menuitem" class="gp-menu-item text-danger hover:text-danger" onclick={() => { const target = info; void confirmCloseCheckouts(target, null); }}>
        Close all {info.tabCount} checkouts…
      </button>
      {#if info.group}
        <span class="gp-menu-sep" aria-hidden="true"></span>
        {@const stackGrpHeader = groupHeadersByName.get(info.group)}
        {#if stackGrpHeader && !stackGrpHeader.isCollapsed && stackGrpHeader.collapsedInactiveCount === 0}
          <button
            role="menuitem"
            class="gp-menu-item"
            onclick={() => {
              setGroupInactiveCollapsed(info.group!, true);
              closeMenu();
            }}
          >
            Collapse inactive tabs in "{info.group}"
          </button>
        {:else if stackGrpHeader && stackGrpHeader.collapsedInactiveCount > 0}
          <button
            role="menuitem"
            class="gp-menu-item"
            onclick={() => {
              setGroupExplicitlyExpanded(info.group!, true);
              closeMenu();
            }}
          >
            Expand all tabs in "{info.group}"
          </button>
        {/if}
        {#if stackGrpHeader && stackGrpHeader.terminalCount > 0}
          <button
            role="menuitem"
            class="gp-menu-item"
            onclick={() => {
              interfaceStore.toggleCollapseInactiveIncludeShells();
              closeMenu();
            }}
          >
            {$interfaceStore.collapseInactiveIncludeShells ? "Keep tabs with active shells visible" : "Collapse tabs with active shells"}
          </button>
        {/if}
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            repoStore.collapseInactiveGroups();
            closeMenu();
          }}
        >
          Collapse inactive groups
        </button>
        <button
          role="menuitem"
          class="gp-menu-item"
          onclick={() => {
            interfaceStore.setCollapseInactiveGroupTabs(!$interfaceStore.collapseInactiveGroupTabs);
            closeMenu();
          }}
        >
          {$interfaceStore.collapseInactiveGroupTabs ? "Show all tabs in groups" : "Collapse inactive tabs in groups"}
        </button>
      {/if}
    </div>
  {/if}
{/if}

{#if stripMenu}
  <div
    bind:this={stripMenuEl}
    use:portal={"body"}
    use:popover={stripMenuDismissal}
    data-strip-menu
    role="menu"
    aria-label="Repository tab strip actions"
    tabindex="-1"
    onkeydown={handlePopupKeydown}
    class="fixed min-w-48 gp-menu gp-pop text-meta text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <button role="menuitem" class="gp-menu-item" onclick={() => { onOpen?.(); closeStripMenu(); }}>
      <FolderOpen size={11} /> Open repository…
    </button>
    {#if $repoStore.lastClosed.length > 0}
      <button role="menuitem" class="gp-menu-item" onclick={() => { void repoStore.reopenLastClosed(); closeStripMenu(); }}>
        Reopen closed repository ({displayName($repoStore.lastClosed[0])})
      </button>
    {/if}
    <span class="gp-menu-sep" aria-hidden="true"></span>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={() => { interfaceStore.setStackWorktreeTabs(!$interfaceStore.stackWorktreeTabs); closeStripMenu(); }}
    >
      {$interfaceStore.stackWorktreeTabs ? "Give every worktree its own tab" : "Stack worktrees of one repository"}
    </button>
    <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.groupByParentFolder(); closeStripMenu(); }}>
      Group all by parent folder
    </button>
    {#if distinctGroupNames.length > 0}
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          interfaceStore.setCollapseInactiveGroupTabs(!$interfaceStore.collapseInactiveGroupTabs);
          closeStripMenu();
        }}
      >
        {$interfaceStore.collapseInactiveGroupTabs ? "Show all tabs in groups" : "Collapse inactive tabs in groups"}
      </button>
      <button
        role="menuitem"
        class="gp-menu-item"
        onclick={() => {
          interfaceStore.toggleCollapseInactiveIncludeShells();
          closeStripMenu();
        }}
      >
        {$interfaceStore.collapseInactiveIncludeShells ? "Keep tabs with active shells visible" : "Collapse tabs with active shells"}
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.collapseInactiveGroups(); closeStripMenu(); }}>
        Collapse inactive groups
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.expandAllGroups(); closeStripMenu(); }}>
        Expand all groups
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.collapseAllGroups(); closeStripMenu(); }}>
        Collapse all groups
      </button>
      <button role="menuitem" class="gp-menu-item" onclick={() => { repoStore.ungroupTabs(); closeStripMenu(); }}>
        Ungroup all repositories
      </button>
    {/if}
    {#if $repoStore.openTabs.length > 0}
      <span class="gp-menu-sep" aria-hidden="true"></span>
      <button role="menuitem" class="gp-menu-item text-danger hover:text-danger" onclick={() => void confirmCloseAllTabs()}>
        Close all repositories…
      </button>
    {/if}
  </div>
{/if}
