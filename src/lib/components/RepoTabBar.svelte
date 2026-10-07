<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { repoStore, type OpenRepoTab } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { isCaseInsensitiveFs, displayName, identityKey, isPathAmong } from "../repos/paths";
  import { portal } from "../dom/portal";
  import { isTauri } from "../platform";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { classifyShortcut, shouldSkipWebviewShortcut } from "../ui/webviewShortcuts";
  import { LAYERS } from "../ui/layers";
  import { popover } from "../ui/popover";
  import { copyText } from "../desktop/clipboard";
  import { enumerateFocusables } from "../ui/focusTrap";
  import {
    ChevronDown,
    ChevronRight,
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
  } from "@lucide/svelte";
  import { askConfirm, askText } from "../stores/modalStore";
  import {
    computeTabLayout,
    normalizeGroupName,
    type GroupHeaderItem,
    type StackHeaderItem,
    type TabItem,
  } from "../repos/tabGroups";
  import { planDrop, unitForTab, unitIds, type StripUnit } from "../repos/stripNav";
  import { checkoutName } from "../repos/repoFamily";
  import { expandedStacks, lastUsedCheckouts, setStackExpanded } from "../repos/stackState";
  import {
    lookupGroupColor,
    normalizeTabColor,
    TAB_COLORS,
    TAB_COLOR_INK,
    TAB_COLOR_LABEL,
    type TabColor,
  } from "../repos/tabColors";
  import { terminalSessions, sessionsByRepo } from "../terminal/sessionRegistry";
  import { liveAgentCount } from "../agents/plane";
  import { agentDirectories } from "../agents/cwd";
  import WorkspaceActions from "./WorkspaceActions.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import { taskChrome } from "../workbench/taskTabs";

  let {
    onOpen,
  }: {
    onOpen?: () => void;
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
    const collapsed = info.isCollapsed ? ", collapsed" : "";
    const color = info.color ? `, ${TAB_COLOR_LABEL[info.color]}` : "";
    return `Group ${info.label}, ${count}${collapsed}${color}`;
  }

  function groupChrome(info: { hasActiveTab: boolean }): string {
    if (info.hasActiveTab) {
      return "border-accent/50 bg-accent/10 text-accent font-medium";
    }
    return "border-border/70 bg-surface/70 text-textSecondary hover:border-accent/30 hover:bg-surfaceHover hover:text-textPrimary";
  }

  let unusedRecents = $derived(
    $repoStore.recentRepos.filter(
      (path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), pathOpts),
    ),
  );
  const tasksOpen = $derived($interfaceStore.globalSurface === "tasks");
  const agentsOpen = $derived($interfaceStore.globalSurface === "agents");
  // The plane's headline counts live terminals with this same call and the
  // same directories, so the chip and the headline say one number.
  const liveAgents = $derived(liveAgentCount($terminalSessions, $agentDirectories));

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
    if (recentsOpen) return recentsTriggerEl ?? null;
    if (groupMenu) return groupMenuOpener;
    if (stackMenu) return stackMenuOpener;
    if (stripMenu) return stripMenuOpener;
    return null;
  }

  function closeMenu(options?: { restoreFocus?: boolean }) {
    const opener = currentOpener();
    menu = null;
    recentsOpen = false;
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
  $effect(() => {
    if (menu && !$repoStore.openTabs.some((tab) => tab.id === menu?.id)) {
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

  const recentsDismissal = {
    dismiss: { inside: "[data-recents-menu]", resize: true, escape: "none" as const },
    onDismiss: () => { recentsOpen = false; },
  };

  function focusPopup(element: HTMLElement | undefined) {
    window.setTimeout(() => {
      const first = element?.querySelector<HTMLElement>('[role="menuitem"]');
      (first ?? element)?.focus();
      first?.scrollIntoView?.({ block: "nearest" });
    }, 0);
  }

  function focusAdjacentToMenuOpener(
    popup: HTMLElement,
    opener: HTMLElement | null,
    backwards: boolean,
  ) {
    const candidates = enumerateFocusables<HTMLElement>(document).filter(
      (candidate) =>
        candidate.tabIndex >= 0 &&
        !popup.contains(candidate) &&
        candidate.getClientRects().length > 0,
    );
    if (candidates.length === 0) return;

    const openerIndex = opener ? candidates.indexOf(opener) : -1;
    let target = openerIndex >= 0
      ? candidates[openerIndex + (backwards ? -1 : 1)]
      : undefined;
    if (!target && opener?.isConnected) {
      const direction = backwards
        ? Node.DOCUMENT_POSITION_PRECEDING
        : Node.DOCUMENT_POSITION_FOLLOWING;
      const ordered = backwards ? [...candidates].reverse() : candidates;
      target = ordered.find((candidate) => opener.compareDocumentPosition(candidate) & direction);
    }
    target ??= candidates[backwards ? candidates.length - 1 : 0];
    window.setTimeout(() => target?.focus(), 0);
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
    if (e.key === "Escape" && (menu || recentsOpen || groupMenu || stackMenu || stripMenu)) {
      e.preventDefault();
      closeMenu({ restoreFocus: true });
      return;
    }
    if (shouldSkipWebviewShortcut(e, isTauri())) return;
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
        void repoStore.activateTabAt(Number(digit[1]) - 1);
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
    if (e.key === "ArrowRight" && info.isCollapsed) {
      e.preventDefault();
      repoStore.setGroupCollapsed(info.group, false);
      return;
    }
    if (e.key === "ArrowLeft" && !info.isCollapsed) {
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
    const tabs = scroller?.querySelectorAll<HTMLElement>("[data-tab-index], [data-group-head], [data-stack-head]") ?? [];
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
        ? "border-accent/60 bg-accent/10 text-accent"
        : "border-border/70 bg-background/50 text-textPrimary hover:border-accent/40 hover:bg-accent/5 hover:text-accent"
    }`;
  }

  /**
   * Open-repository pills have to carry their own plate. Ghost text on the
   * glass strip (`text-textMuted` + a transparent border) drops below AA
   * against the macOS hue field, which is why a single open tab used to
   * disappear beside Fleet.
   */
  function repoTabChrome(tab: { isActive: boolean }): string {
    const viewingRepo = $interfaceStore.globalSurface === "repository";
    if (tab.isActive && viewingRepo) {
      return "border-accent/60 bg-accent/10 text-accent shadow-xs";
    }
    if (tab.isActive) {
      return "border-border/80 bg-surfaceHover text-textPrimary";
    }
    return "border-border/70 bg-background/50 text-textPrimary hover:border-accent/40 hover:bg-accent/5 hover:text-accent";
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

  $effect(() => {
    // Re-run on activation changes, not just on element binding — otherwise
    // the newly active tab never scrolls into view (mirrors ViewTabBar).
    const tabs = $repoStore.openTabs;
    const activeId = tabs.find((tab) => tab.isActive)?.id ?? null;
    void activeId;
    const active = scroller?.querySelector("[data-active-repo='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

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
        class="h-7 px-2 flex items-center gap-1.5 rounded-lg border text-xs font-medium cursor-pointer transition-[color,background-color,border-color,box-shadow] duration-150 {groupChrome(info)} {dragHoverGroup === info.group ? 'ring-2 ring-accent border-accent' : ''}"
        aria-expanded={!info.isCollapsed}
        aria-label={groupAriaLabel(info)}
        title={`Group: ${info.label} (${info.tabCount} ${info.tabCount === 1 ? 'repository' : 'repositories'})\nClick to ${info.isCollapsed ? 'expand' : 'collapse'} · Right-click for options · F2 to rename · Delete to close`}
        data-group-head={info.group}
        tabindex={info.isCollapsed && info.hasActiveTab ? 0 : -1}
        onclick={() => repoStore.toggleGroupCollapsed(info.group)}
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
        <span class="whitespace-nowrap font-medium text-[11px]">{info.label}</span>
        <span class="text-[10px] tabular-nums font-mono opacity-70">({info.tabCount})</span>
        {#if info.isDirty}
          <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shadow-[0_0_6px_rgb(251_191_36/0.8)] shrink-0" title="{info.label} has uncommitted changes"></span>
        {/if}
        {#if info.conflictedCount > 0}
          <span class="text-amber-400 text-[10px] font-mono shrink-0" title="{info.conflictedCount} conflicted files in {info.label}">{info.conflictedCount}</span>
        {/if}
        {#if info.terminalCount > 0}
          <span
            class="shrink-0 inline-flex items-center gap-0.5 text-accent"
            title={`${info.terminalCount} terminal session${info.terminalCount === 1 ? '' : 's'} running in ${info.label}`}
          >
            <SquareTerminal size={10} aria-hidden="true" />
            {#if info.terminalCount > 1}
              <span class="text-[9px] font-medium tabular-nums">{info.terminalCount}</span>
            {/if}
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
    title={`${tab.path}\nDrag to reorder · Ctrl+Shift+←/→ to move · P to ${tab.pinned ? "unpin" : "pin"}${colorInfo.color ? `\n${colorInfo.source === "own" ? TAB_COLOR_LABEL[colorInfo.color] : `${TAB_COLOR_LABEL[colorInfo.color]} group`}` : ""}`}
    draggable="true"
    onauxclick={(e) => {
      if (e.button === 1) {
        e.preventDefault();
        void repoStore.closeTab(tab.id);
      }
    }}
    oncontextmenu={(e) => onContext(e, tab.id)}
    ondragstart={(e) => onUnitDragStart(e, { kind: "tab", id: tab.id })}
    ondragend={endDrag}
    class="group relative min-w-28 pr-1 flex items-center gap-1 rounded-full border shrink-0 cursor-grab active:cursor-grabbing transition-[color,background-color,border-color,box-shadow,opacity] duration-150 {dragFrom?.kind === 'tab' && dragFrom.id === tab.id
      ? 'opacity-60'
      : ''} {dropHere ? 'border-accent/50' : ''} {repoTabChrome(tab)}"
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
      tabindex={tab.isActive ? 0 : -1}
      aria-selected={tab.isActive}
      aria-keyshortcuts="Enter p Delete Control+Shift+ArrowLeft Control+Shift+ArrowRight"
      data-active-repo={tab.isActive ? "true" : "false"}
      data-tab-index={index}
      data-tab-id={tab.id}
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
        <FolderGit2 size={11} class="shrink-0 {tab.error ? 'text-rose-400' : 'text-accent'}" />
      {/if}
      <span class="whitespace-nowrap font-medium">{tab.label}</span>
      {#if colorInfo.color}
        <span class="sr-only">{colorInfo.source === "own" ? TAB_COLOR_LABEL[colorInfo.color] : `${TAB_COLOR_LABEL[colorInfo.color]} group color`}</span>
      {/if}
      {#if tab.currentBranch}
        <span class="whitespace-nowrap text-[10px] font-mono opacity-80 hidden sm:inline">{tab.currentBranch}</span>
      {/if}
      {#if tab.conflictedCount > 0}
        <span class="text-amber-400 shrink-0">{tab.conflictedCount}</span>
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
            <span class="text-[9px] font-medium tabular-nums">{terminalCounts.get(tab.path)}</span>
          {/if}
          <!-- `title` on a non-focusable span is not reliably
               announced; the tab's accessible name carries it. -->
          <span class="sr-only">{terminalCounts.get(tab.path)} terminal sessions running</span>
        </span>
      {/if}
    </button>
    {#if tab.isDirty}
      <button type="button" class="shrink-0 grid place-items-center w-5 h-5 rounded-full hover:bg-amber-500/15 focus-visible:ring-1 focus-visible:ring-accent"
        title="Preview uncommitted changes in {tab.label}" aria-label="Preview uncommitted changes in {tab.label}"
        onclick={() => void repoStore.previewUncommitted(tab.path)}>
        <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shadow-[0_0_6px_rgb(251_191_36/0.8)]"></span>
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
      class="ml-auto p-0.5 rounded-full opacity-0 group-hover:opacity-100 hover:bg-background hover:text-rose-400 {tab.isActive ? 'opacity-100' : ''}"
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
    class="group relative flex items-center shrink-0 rounded-full border cursor-grab active:cursor-grabbing transition-[color,background-color,border-color,box-shadow,opacity] duration-150 {info.isExpanded
      ? groupChrome(info)
      : repoTabChrome({ isActive: info.hasActiveTab })} {dragFrom?.kind === 'stack' && dragFrom.key === info.key
      ? 'opacity-60'
      : ''} {dropHere ? 'border-accent/50' : ''}"
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
        class="h-7 pl-2 pr-2.5 flex items-center gap-1.5 rounded-full text-[11px] font-medium focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent/70"
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
        <span class="text-[10px] tabular-nums font-mono opacity-70">({info.tabCount})</span>
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
        tabindex={info.hasActiveTab ? 0 : -1}
        aria-selected={info.hasActiveTab}
        aria-label={stackAriaLabel(info)}
        aria-keyshortcuts="Enter ArrowDown + Control+Shift+ArrowLeft Control+Shift+ArrowRight"
        data-stack-head={info.key}
        data-tab-id={current.id}
        data-active-repo={info.hasActiveTab ? "true" : "false"}
        onclick={() => selectRepoTab(current.id)}
        onkeydown={(e) => onStackKeydown(e, info)}
        class="h-7 pl-2.5 pr-1 flex items-center gap-1.5 text-left rounded-l-full focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent/70"
      >
        <Layers size={11} class="shrink-0 {current.error ? 'text-rose-400' : 'text-accent'}" />
        <span class="whitespace-nowrap font-medium">{info.label}</span>
        {#if member.primary}
          {#if current.currentBranch}
            <span class="whitespace-nowrap text-[10px] font-mono opacity-80 hidden sm:inline">{current.currentBranch}</span>
          {/if}
        {:else}
          <span class="whitespace-nowrap text-[10px] font-mono opacity-80 max-w-40 truncate" data-stack-current>{member.name}</span>
        {/if}
        {#if info.conflictedCount > 0}
          <span class="text-amber-400 shrink-0" title="{info.conflictedCount} conflicted files across {info.label}">{info.conflictedCount}</span>
        {/if}
        {#if info.terminalCount > 0}
          <span
            class="shrink-0 inline-flex items-center gap-0.5 text-accent"
            title={`${info.terminalCount} terminal session${info.terminalCount === 1 ? "" : "s"} running across ${info.label}`}
          >
            <SquareTerminal size={10} aria-hidden="true" />
            {#if info.terminalCount > 1}
              <span class="text-[9px] font-medium tabular-nums">{info.terminalCount}</span>
            {/if}
          </span>
        {/if}
      </button>
      {#if current.isDirty}
        <button type="button" class="shrink-0 grid place-items-center w-5 h-5 rounded-full hover:bg-amber-500/15 focus-visible:ring-1 focus-visible:ring-accent"
          tabindex="-1"
          title="Preview uncommitted changes in {member.name}" aria-label="Preview uncommitted changes in {member.name}"
          onclick={() => void repoStore.previewUncommitted(current.path)}>
          <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shadow-[0_0_6px_rgb(251_191_36/0.8)]"></span>
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
        class="mr-1 h-5 pl-1.5 pr-1 flex items-center gap-0.5 rounded-full border border-border/60 bg-background/60 text-[10px] font-mono tabular-nums hover:border-accent/50 hover:text-accent {switcherOpen ? 'border-accent/60 text-accent' : ''}"
      >
        <span>{info.tabCount}</span>
        {#if info.isDirty && !current.isDirty}
          <!-- Another checkout has changes: say so without naming which until asked. -->
          <span class="w-1 h-1 rounded-full bg-amber-400" aria-hidden="true"></span>
        {/if}
        <ChevronDown size={10} class="shrink-0" />
      </button>
    {/if}
  </div>
{/snippet}

  <div class="gp-glass gp-repo-tabs relative z-20 h-11 bg-surface border-b border-border/60 gp-section-edge flex items-center select-none shrink-0 text-xs px-2 gap-1.5">
    <!-- Fleet sits left of the tabs because it is above them: one surface for
         the whole workspace, not another repository. -->
    <button
      type="button"
      class={surfaceChipClass($interfaceStore.globalSurface === "fleet")}
      aria-pressed={$interfaceStore.globalSurface === "fleet"}
      data-testid="fleet-tab-chip"
      onclick={() => interfaceStore.toggleFleet()}
      title="Fleet — every open repository at a glance: changes, sync, worktrees, agents and, on demand, size and health."
    >
      <LayoutGrid size={12} />
      <span>Fleet</span>
    </button>
    <div class="{surfaceChipClass(agentsOpen)} pr-1!" data-testid="agents-tab-chip">
      <button
        type="button"
        class="flex items-center gap-1.5 flex-1 bg-transparent border-0 p-0 text-inherit font-medium"
        aria-pressed={agentsOpen}
        onclick={() => interfaceStore.setAgentsOpen(true)}
        title="Agents — checkouts on disk, processes this window started, and attempts that need you. A checkout is not a running process."
      >
        <Bot size={12} />
        <span>Agents</span>
        {#if liveAgents > 0}
          <span class="gp-pill !px-1.5 !py-0 min-w-4 justify-center" title="{liveAgents} live {liveAgents === 1 ? 'agent' : 'agents'} this window started">{liveAgents}</span>
        {/if}
      </button>
      {#if agentsOpen}
        <button
          type="button"
          class="p-0.5 rounded hover:bg-surfaceHover text-textMuted hover:text-rose-400"
          data-testid="agents-tab-close"
          aria-label="Close Agents"
          title="Close Agents"
          onclick={() => interfaceStore.setAgentsOpen(false)}
        >
          <X size={11} />
        </button>
      {/if}
    </div>
    <div class="{surfaceChipClass(tasksOpen)} pr-1!" data-testid="tasks-tab-chip">
      <button
        type="button"
        class="flex items-center gap-1.5 flex-1 bg-transparent border-0 p-0 text-inherit font-medium"
        aria-pressed={tasksOpen}
        data-tour="tasks"
        onclick={() => interfaceStore.setTasksOpen(true)}
        title="Tasks — global, workspace and repository Kanban boards"
      >
        <ListChecks size={12} />
        <span>Tasks</span>
        {#if $taskChrome.openTabs > 0}
          <span class="gp-pill !px-1.5 !py-0 min-w-4 justify-center" title="{$taskChrome.openTabs} open {$taskChrome.openTabs === 1 ? 'task' : 'tasks'}">{$taskChrome.openTabs}</span>
        {/if}
      </button>
      {#if tasksOpen}
        <button
          type="button"
          class="p-0.5 rounded hover:bg-surfaceHover text-textMuted hover:text-rose-400"
          data-testid="tasks-tab-close"
          aria-label="Close Tasks"
          title="Close Tasks"
          onclick={() => interfaceStore.setTasksOpen(false)}
        >
          <X size={11} />
        </button>
      {/if}
    </div>
    {#if !hideTabStrip}
      <div class="h-3.5 w-1 rounded-full bg-border/50 shrink-0" aria-hidden="true"></div>
      <div class="relative min-w-0 max-w-full shrink self-stretch">
      <div
        bind:this={scroller}
        class="h-full flex items-center gap-1 overflow-x-auto min-w-0 py-1"
        role="tablist"
        tabindex="-1"
        aria-label="Open repositories"
        onkeydown={onTablistKeydown}
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
      <ScrollCue target={scroller} axis="x" />
      </div>
    {/if}

    <button
      type="button"
      title="Open repository"
      aria-label="Open repository"
      data-testid="open-repo-tab"
      onclick={() => onOpen?.()}
      class="gp-btn py-1! px-2.5! shrink-0"
    >
      <Plus size={13} class="text-accent" />
      <span>Open</span>
    </button>

    <div class="relative shrink-0" data-recents-menu>
      <button
        bind:this={recentsTriggerEl}
        type="button"
        title="Recent repositories"
        aria-haspopup="menu"
        aria-expanded={recentsOpen}
        aria-controls="recent-repositories-menu"
        onclick={() => {
          menu = null;
          recentsOpen = !recentsOpen;
        }}
        class="gp-icon-btn p-1! h-full"
      >
        <ChevronDown size={13} />
      </button>
      {#if recentsOpen}
        <div
          bind:this={recentsEl}
          use:popover={recentsDismissal}
          id="recent-repositories-menu"
          role="menu"
          aria-label="Recent repositories"
          tabindex="-1"
          onkeydown={handlePopupKeydown}
          class="absolute right-0 top-full mt-1.5 w-80 max-w-[calc(100vw-1rem)] gp-menu gp-pop flex flex-col max-h-[min(28rem,calc(100vh-7.5rem))] shadow-float"
          style="z-index: {LAYERS.MENU}"
        >
          <div class="px-2 pt-1 pb-1.5 text-[10px] uppercase tracking-wider text-textMuted shrink-0">
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
                    <div class="truncate text-[10px] text-textMuted font-mono">{path}</div>
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    aria-label={`Remove ${displayName(path)} from recent repositories`}
                    title="Remove from recents"
                    onclick={() => repoStore.removeRecent(path)}
                    class="p-1 rounded-full text-textMuted hover:text-rose-400 hover:bg-surfaceHover shrink-0"
                  >
                    <X size={11} />
                  </button>
                </div>
              {/each}
            </div>
          {/if}
          {#if unusedRecents.length === 0 && $repoStore.recentRepos.length > 0}
            <div class="px-3 py-1.5 text-[10px] text-textMuted shrink-0 border-t border-border/40">All recents are already open</div>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Workspace-wide actions live here rather than in a view: they act on
         every open repository, so they belong to the tab strip that owns
         them. Hidden with a single tab open, where they say nothing new. -->
    <WorkspaceActions />
    <div class="sr-only" role="status" aria-live="polite">{moveAnnouncement}</div>
  </div>

{#snippet colorChoices(
  label: string,
  current: TabColor | null,
  detail: string | null,
  onPick: (color: TabColor | null) => void,
)}
  <div class="px-2 py-1.5" role="group" aria-label={label}>
    <div class="text-[10px] uppercase tracking-wider text-textMuted font-medium">{label}</div>
    {#if detail}
      <div class="pt-0.5 text-[10px] text-textMuted">{detail}</div>
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
          class="size-4 rounded-full border border-black/15 {current === color ? 'ring-2 ring-textPrimary' : ''}"
          style:background-color={TAB_COLOR_INK[color]}
          onclick={() => onPick(color)}
        ></button>
      {/each}
    </div>
  </div>
{/snippet}

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
      class="fixed min-w-44 gp-menu gp-pop text-[11px] text-textPrimary"
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
        <button role="menuitem" class="gp-menu-item text-rose-400 hover:text-rose-300" onclick={() => {
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
      class="fixed min-w-44 gp-menu gp-pop text-[11px] text-textPrimary"
      style="z-index: {LAYERS.MENU}"
    >
      <div class="px-2 pt-1 pb-1.5 text-[10px] uppercase tracking-wider text-textMuted shrink-0 font-medium">
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
          repoStore.toggleGroupCollapsed(groupHeader.group);
          closeGroupMenu();
        }}
      >
        {groupHeader.isCollapsed ? "Expand group" : "Collapse group"}
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
          class="gp-menu-item text-rose-400 hover:text-rose-300"
          onclick={() => void confirmCloseOtherGroups(groupHeader.group)}
        >
          Close other groups…
        </button>
      {/if}
      <button
        role="menuitem"
        class="gp-menu-item text-rose-400 hover:text-rose-300"
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
      class="fixed w-72 max-w-[calc(100vw-1rem)] gp-menu gp-pop text-[11px] text-textPrimary flex flex-col"
      style="z-index: {LAYERS.MENU}"
    >
      <div class="px-2 pt-1 pb-1.5 text-[10px] uppercase tracking-wider text-textMuted shrink-0 font-medium truncate" title={info.root}>
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
                  <span class="gp-pill !px-1.5 !py-0 shrink-0 text-[9px]" title="Worktree created by {name.agent}">{name.agent}</span>
                {/if}
                {#if tab.isDirty}
                  <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shrink-0" title="Uncommitted changes"></span>
                  <span class="sr-only">uncommitted changes</span>
                {/if}
                {#if tab.conflictedCount > 0}
                  <span class="text-amber-400 shrink-0 font-mono" title="{tab.conflictedCount} conflicted files">{tab.conflictedCount}</span>
                {/if}
                {#if terminalCounts.get(tab.path)}
                  <span class="shrink-0 inline-flex items-center gap-0.5 text-accent" title="{terminalCounts.get(tab.path)} terminal sessions running">
                    <SquareTerminal size={10} aria-hidden="true" />
                    <span class="text-[9px] tabular-nums">{terminalCounts.get(tab.path)}</span>
                  </span>
                {/if}
              </div>
              <div class="truncate text-[10px] text-textMuted font-mono">
                {tab.currentBranch ?? (tab.error ? "unavailable" : tab.isLoading ? "loading…" : "detached")}
              </div>
            </button>
            <button
              type="button"
              role="menuitem"
              aria-label={`Close ${name.name}`}
              title="Close this tab (the worktree stays on disk)"
              class="p-1 rounded-full text-textMuted hover:text-rose-400 hover:bg-surfaceHover shrink-0"
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
        <button role="menuitem" class="gp-menu-item text-rose-400 hover:text-rose-300" onclick={() => { const target = info; void confirmCloseCheckouts(target, target.current.id); }}>
          Close other checkouts…
        </button>
      {/if}
      <button role="menuitem" class="gp-menu-item text-rose-400 hover:text-rose-300" onclick={() => { const target = info; void confirmCloseCheckouts(target, null); }}>
        Close all {info.tabCount} checkouts…
      </button>
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
    class="fixed min-w-48 gp-menu gp-pop text-[11px] text-textPrimary"
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
      <button role="menuitem" class="gp-menu-item text-rose-400 hover:text-rose-300" onclick={() => void confirmCloseAllTabs()}>
        Close all repositories…
      </button>
    {/if}
  </div>
{/if}
