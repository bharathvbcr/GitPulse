<script lang="ts">
  import { onMount } from "svelte";
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
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { clampMenuPosition } from "../branches/menuPosition";
  import { copyText } from "../desktop/clipboard";
  import { enumerateFocusables } from "../ui/focusTrap";
  import {
    ChevronDown,
    Pin,
    Plus,
    X,
    FolderGit2,
    FolderOpen,
    LayoutGrid,
  } from "@lucide/svelte";
  import WorkspaceActions from "./WorkspaceActions.svelte";
  import ScrollCue from "./ScrollCue.svelte";

  let {
    onOpen,
  }: {
    onOpen?: () => void;
  } = $props();

  let menu: { x: number; y: number; id: string } | null = $state(null);
  // Measured menu box feeds the shared clamp so the tab menu can never open
  // off-screen (the old innerWidth-200 guess overflowed on short windows).
  let menuEl: HTMLDivElement | undefined = $state();
  let menuOpener: HTMLElement | null = null;
  let menuPos = $state({ left: 0, top: 0 });
  let recentsOpen = $state(false);
  let recentsTriggerEl: HTMLButtonElement | undefined = $state();
  let recentsEl: HTMLDivElement | undefined = $state();
  let dragFromId = $state<string | null>(null);
  // Where a dragged tab would land. `before` picks the left/right half of the
  // hovered tab; null means "no useful insertion point" and hides the bar.
  let dropTarget = $state<{ index: number; before: boolean } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  let moveAnnouncement = $state("");
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  let unusedRecents = $derived(
    $repoStore.recentRepos.filter(
      (path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), pathOpts),
    ),
  );

  function closeMenu(options?: { restoreFocus?: boolean }) {
    const opener = menu ? menuOpener : recentsOpen ? recentsTriggerEl : null;
    menu = null;
    recentsOpen = false;
    menuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
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
      menu = null;
    }
  });

  function onContext(e: MouseEvent, id: string) {
    e.preventDefault();
    menuOpener = e.currentTarget instanceof HTMLElement
      ? e.currentTarget.querySelector<HTMLElement>('[role="tab"]')
      : null;
    menu = { x: e.clientX, y: e.clientY, id };
    // First paint at the raw anchor (clamped by estimate); the effect below
    // repositions from the real measured box once it exists.
    menuPos = clampMenuPosition(e.clientX, e.clientY, 176, 150, window.innerWidth, window.innerHeight);
    recentsOpen = false;
  }

  $effect(() => {
    if (!menu || !menuEl) return;
    menuPos = clampMenuPosition(
      menu.x,
      menu.y,
      menuEl.offsetWidth,
      menuEl.offsetHeight,
      window.innerWidth,
      window.innerHeight,
    );
  });

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

  function handlePopupKeydown(e: KeyboardEvent) {
    if (e.key === "Tab") {
      e.preventDefault();
      const popup = e.currentTarget;
      if (popup instanceof HTMLElement) {
        const opener = menu ? menuOpener : recentsOpen ? recentsTriggerEl ?? null : null;
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
    if (e.key === "Escape" && (menu || recentsOpen)) {
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
        void repoStore.activateTabAt(Number(digit[1]) - 1);
        return;
      }
      case "cycleTabs": {
        e.preventDefault();
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

  function handlePointerDown(e: PointerEvent) {
    if (menu && shouldDismissOverlay(e.target, "[data-repo-menu]")) {
      closeMenu();
    }
    if (recentsOpen && shouldDismissOverlay(e.target, "[data-recents-menu]")) {
      recentsOpen = false;
    }
  }

  onMount(() => {
    window.addEventListener("keydown", handleKey);
    window.addEventListener("pointerdown", handlePointerDown, true);
    // Same contract as BranchList's menu: a resized window can leave the
    // clamped position pointing at nothing useful, so close instead.
    const handleResize = () => closeMenu();
    window.addEventListener("resize", handleResize);
    return () => {
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("resize", handleResize);
    };
  });

  function endDrag() {
    dragFromId = null;
    dropTarget = null;
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
    const tabs = scroller?.querySelectorAll<HTMLElement>("[data-tab-index]") ?? [];
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

{#if $repoStore.openTabs.length > 0}
  <div class="gp-glass gp-repo-tabs relative z-20 h-10 bg-surface/60 border-b border-border/60 gp-section-edge flex items-center select-none shrink-0 text-[11px] px-2 gap-1">
    <!-- Fleet sits left of the tabs because it is above them: one surface for
         the whole workspace, not another repository. -->
    <button
      type="button"
      class="shrink-0 flex items-center gap-1.5 px-2 py-1 rounded-lg border transition-colors {$interfaceStore.fleetOpen
        ? 'border-accent/60 bg-accent/10 text-accent'
        : 'border-transparent text-textMuted hover:bg-surfaceHover hover:text-textPrimary'}"
      aria-pressed={$interfaceStore.fleetOpen}
      data-testid="fleet-tab-chip"
      onclick={() => interfaceStore.toggleFleet()}
      title="Fleet — every open repository at a glance: changes, sync, worktrees, agents and, on demand, size and health."
    >
      <LayoutGrid size={12} />
      <span>Fleet</span>
    </button>
    <div class="h-3.5 w-1 rounded-full bg-border/50 shrink-0" aria-hidden="true"></div>
    <div class="relative min-w-0 flex-1 self-stretch">
    <div
      bind:this={scroller}
      class="h-full flex items-center gap-1 overflow-x-auto min-w-0 py-1"
      role="tablist"
      tabindex="-1"
      aria-label="Open repositories"
      onkeydown={onTablistKeydown}
      ondragover={onScrollerDragOver}
      ondrop={onScrollerDrop}
    >
      {#each $repoStore.openTabs as tab, index (tab.id)}
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
            : ''} {dropTarget?.index === index ? 'border-accent/50' : ''} {tab.isActive
            ? 'bg-surfaceHover border-border/80 text-textPrimary shadow-xs'
            : 'border-transparent text-textMuted hover:text-textPrimary hover:bg-surfaceHover/60'}"
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
            onclick={() => repoStore.activateTab(tab.id)}
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
              <span class="whitespace-nowrap text-[10px] text-textMuted/80 font-mono hidden sm:inline">{tab.currentBranch}</span>
            {/if}
            {#if tab.conflictedCount > 0}
              <span class="text-amber-400 shrink-0">{tab.conflictedCount}</span>
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
      {/each}
    </div>
    <ScrollCue target={scroller} axis="x" />
    </div>

    <button
      type="button"
      title="Open repository"
      onclick={() => onOpen?.()}
      class="gp-icon-btn p-1! shrink-0 hover:text-accent"
    >
      <Plus size={13} />
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
{/if}

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
      data-repo-menu
      role="menu"
      aria-label={`Repository actions for ${tab.label}`}
      tabindex="-1"
      onkeydown={handlePopupKeydown}
      class="fixed min-w-44 gp-menu gp-pop text-[11px] text-textPrimary"
      style="left: {menuPos.left}px; top: {menuPos.top}px; z-index: {LAYERS.MENU}"
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
