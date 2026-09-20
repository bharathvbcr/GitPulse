<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { isCaseInsensitiveFs, displayName, isPathAmong } from "../repos/paths";
  import { dropReorderIndex } from "../repos/tabModel";
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
    LayoutGrid,
    ListChecks,
    SquareTerminal,
  } from "@lucide/svelte";
  import { askConfirm, askText } from "../stores/modalStore";
  import {
    computeTabLayout,
    normalizeGroupName,
    type GroupHeaderItem,
  } from "../repos/tabGroups";
  import { terminalSessions, sessionsByRepo } from "../terminal/sessionRegistry";
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
  let recentsOpen = $state(false);
  let recentsTriggerEl: HTMLButtonElement | undefined = $state();
  let recentsEl: HTMLDivElement | undefined = $state();
  let dragFromId = $state<string | null>(null);
  let dragHoverGroup = $state<string | null>(null);
  let dragHoverGroupTimer: ReturnType<typeof setTimeout> | null = null;
  // Where a dragged tab would land. `before` picks the left/right half of the
  // hovered tab; null means "no useful insertion point" and hides the bar.
  let dropTarget = $state<{ index: number; before: boolean } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  let moveAnnouncement = $state("");
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const hideTabStrip = $derived(
    $interfaceStore.autoHideRepoTabs && $repoStore.openTabs.length <= 1,
  );

  const tabLayout = $derived(
    computeTabLayout($repoStore.openTabs, $repoStore.collapsedGroups, terminalCounts),
  );

  const groupHeadersByName = $derived(
    new Map(tabLayout.groups.map((g) => [g.group, g])),
  );

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

  function isFirstInGroup(tab: { group?: string | null }, index: number): boolean {
    const group = normalizeGroupName(tab.group);
    if (!group) return false;
    const firstIndex = $repoStore.openTabs.findIndex(
      (t) => normalizeGroupName(t.group) === group,
    );
    return index === firstIndex;
  }

  function isTabInCollapsedGroup(tab: { group?: string | null }): boolean {
    const group = normalizeGroupName(tab.group);
    return group !== null && collapsedGroupSet.has(group);
  }

  function getGroupInfo(rawGroup: string | null | undefined): GroupHeaderItem | undefined {
    const group = normalizeGroupName(rawGroup);
    return group ? groupHeadersByName.get(group) : undefined;
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

  function closeMenu(options?: { restoreFocus?: boolean }) {
    const opener = menu ? menuOpener : recentsOpen ? recentsTriggerEl : groupMenu ? groupMenuOpener : stripMenu ? stripMenuOpener : null;
    menu = null;
    recentsOpen = false;
    groupMenu = null;
    stripMenu = null;
    menuOpener = null;
    groupMenuOpener = null;
    stripMenuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
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

  function onContext(e: MouseEvent, id: string) {
    e.preventDefault();
    menuOpener = e.currentTarget instanceof HTMLElement
      ? e.currentTarget.querySelector<HTMLElement>('[role="tab"]')
      : null;
    menu = { x: e.clientX, y: e.clientY, id };
    recentsOpen = false;
    groupMenu = null;
    stripMenu = null;
  }

  function onGroupContext(e: MouseEvent, group: string) {
    e.preventDefault();
    groupMenuOpener = e.currentTarget instanceof HTMLElement ? e.currentTarget : null;
    groupMenu = { x: e.clientX, y: e.clientY, group };
    menu = null;
    stripMenu = null;
    recentsOpen = false;
  }

  function onStripContext(e: MouseEvent) {
    if (
      e.target instanceof Element &&
      (e.target.closest("[data-tab-id]") || e.target.closest("[data-group-header]") || e.target.closest("button"))
    ) {
      return;
    }
    e.preventDefault();
    stripMenuOpener = scroller ?? null;
    stripMenu = { x: e.clientX, y: e.clientY };
    menu = null;
    groupMenu = null;
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

  function handlePopupKeydown(e: KeyboardEvent) {
    if (e.key === "Tab") {
      e.preventDefault();
      const popup = e.currentTarget;
      if (popup instanceof HTMLElement) {
        const opener = menu ? menuOpener : recentsOpen ? recentsTriggerEl ?? null : groupMenu ? groupMenuOpener : stripMenu ? stripMenuOpener : null;
        focusAdjacentToMenuOpener(popup, opener, e.shiftKey);
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
    if (e.key === "Escape" && (menu || recentsOpen || groupMenu || stripMenu)) {
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
    dragFromId = null;
    dropTarget = null;
  }

  function onGroupDragOver(e: DragEvent, group: string, isCollapsed: boolean) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    if (dragFromId === null) return;
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
    if (dragFromId !== null) {
      const id = dragFromId;
      repoStore.setTabGroup(id, group);
      repoStore.setGroupCollapsed(group, false);
      announceMove(id);
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

  function announceMove(id: string) {
    const tabs = $repoStore.openTabs;
    const index = tabs.findIndex((tab) => tab.id === id);
    const tab = index >= 0 ? tabs[index] : undefined;
    if (!tab) return;
    moveAnnouncement = `Moved ${tab.label} to position ${index + 1} of ${tabs.length}`;
  }

  function moveFocusedTabTo(id: string, toIndex: number) {
    const before = $repoStore.openTabs.map((tab) => tab.id).join("\0");
    repoStore.moveTab(id, toIndex);
    if (before === $repoStore.openTabs.map((tab) => tab.id).join("\0")) return;
    announceMove(id);
    window.setTimeout(() => focusTabById(id), 0);
  }

  function moveFocusedTabBy(id: string, delta: number) {
    const before = $repoStore.openTabs.map((tab) => tab.id).join("\0");
    repoStore.moveTabBy(id, delta);
    if (before === $repoStore.openTabs.map((tab) => tab.id).join("\0")) return;
    announceMove(id);
    window.setTimeout(() => focusTabById(id), 0);
  }

  function tabIdUnderFocus(): string | null {
    const focused = document.activeElement;
    const shell = focused instanceof Element ? focused.closest("[data-tab-id]") : null;
    if (shell instanceof HTMLElement && shell.dataset.tabId) return shell.dataset.tabId;
    return $repoStore.openTabs.find((tab) => tab.isActive)?.id ?? null;
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
    const tabs = scroller?.querySelectorAll<HTMLElement>("[data-tab-index], [data-group-head]") ?? [];
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
      const tabs = scroller?.querySelectorAll<HTMLElement>("[data-tab-index]") ?? [];
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
   * whatever tab (or gap) is under the pointer, so the indicator can't go
   * stale between child elements. Adjacent-to-self positions are no-op moves
   * and show nothing.
   */
  function onTabDragStart(e: DragEvent, id: string) {
    if (e.target instanceof Element && e.target.closest("[data-tab-close]")) {
      e.preventDefault();
      return;
    }
    dragFromId = id;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", id);
      e.dataTransfer.setData("application/x-gitpulse-repo-tab", id);
    }
  }

  function onScrollerDragOver(e: DragEvent) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    if (dragFromId === null) return;
    const fromIndex = $repoStore.openTabs.findIndex((tab) => tab.id === dragFromId);
    const tabEl = e.target instanceof Element ? e.target.closest("[data-tab-shell-index]") : null;
    if (!(tabEl instanceof HTMLElement) || tabEl.dataset.tabShellIndex === undefined) {
      dropTarget = null;
      return;
    }
    const index = Number(tabEl.dataset.tabShellIndex);
    if (!Number.isInteger(index) || fromIndex < 0) {
      dropTarget = null;
      return;
    }
    const rect = tabEl.getBoundingClientRect();
    const before = e.clientX < rect.left + rect.width / 2;
    if (dropReorderIndex(fromIndex, index, before) === null) {
      dropTarget = null;
      return;
    }
    dropTarget = { index, before };
  }

  function onScrollerDrop(e: DragEvent) {
    e.preventDefault();
    if (dragFromId !== null && dropTarget) {
      const fromIndex = $repoStore.openTabs.findIndex((tab) => tab.id === dragFromId);
      const { index, before } = dropTarget;
      const adjusted = dropReorderIndex(fromIndex, index, before);
      if (adjusted !== null) {
        const id = dragFromId;
        repoStore.moveTab(id, adjusted);
        announceMove(id);
      }
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
        aria-label={`Group ${info.label}, ${info.tabCount} ${info.tabCount === 1 ? 'repository' : 'repositories'}${info.isCollapsed ? ', collapsed' : ''}`}
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
        {#if info.isCollapsed}
          <ChevronRight size={12} class="shrink-0 text-textMuted group-hover:text-textPrimary transition-transform" />
        {:else}
          <ChevronDown size={12} class="shrink-0 text-textMuted group-hover:text-textPrimary transition-transform" />
        {/if}
        <Folder size={12} class="shrink-0 {info.hasActiveTab ? 'text-accent' : 'text-textMuted'}" />
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
        {#each $repoStore.openTabs as tab, index (tab.id)}
          {#if isFirstInGroup(tab, index)}
            {@render groupHead(getGroupInfo(tab.group))}
          {/if}
          {#if !isTabInCollapsedGroup(tab)}
          <div
            role="presentation"
            data-tab-id={tab.id}
            data-tab-shell-index={index}
            title={`${tab.path}\nDrag to reorder · Ctrl+Shift+←/→ to move · P to ${tab.pinned ? "unpin" : "pin"}`}
            draggable="true"
            onauxclick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                void repoStore.closeTab(tab.id);
              }
            }}
            oncontextmenu={(e) => onContext(e, tab.id)}
            ondragstart={(e) => onTabDragStart(e, tab.id)}
            ondragend={endDrag}
            class="group relative min-w-28 pr-1 flex items-center gap-1 rounded-full border shrink-0 cursor-grab active:cursor-grabbing transition-[color,background-color,border-color,box-shadow,opacity] duration-150 {dragFromId === tab.id
              ? 'opacity-60'
              : ''} {dropTarget?.index === index ? 'border-accent/50' : ''} {repoTabChrome(tab)}"
          >
            {#if dropTarget?.index === index}
              <span
                aria-hidden="true"
                class="absolute top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-full bg-accent shadow-glow transition-opacity {dropTarget.before
                  ? 'left-[-3px]'
                  : 'right-[-3px]'}"
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

{#if menu}
  {@const tab = $repoStore.openTabs.find((item) => item.id === menu?.id)}
  {#if tab}
    {@const tabIndex = $repoStore.openTabs.findIndex((item) => item.id === tab.id)}
    {@const lastIndex = $repoStore.openTabs.length - 1}
    {@const canMoveLeft = tabIndex > 0}
    {@const canMoveRight = tabIndex >= 0 && tabIndex < lastIndex}
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
          moveFocusedTabTo(tab.id, 0);
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
          moveFocusedTabTo(tab.id, lastIndex);
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
