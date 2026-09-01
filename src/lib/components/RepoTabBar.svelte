<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { isCaseInsensitiveFs, displayName, isPathAmong } from "../repos/paths";
  import { portal } from "../dom/portal";
  import { isTauri } from "../platform";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { classifyShortcut, shouldSkipWebviewShortcut } from "../ui/webviewShortcuts";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { clampMenuPosition } from "../branches/menuPosition";
  import {
    ChevronDown,
    Pin,
    Plus,
    X,
    FolderGit2,
    FolderOpen,
  } from "lucide-svelte";
  import WorkspaceActions from "./WorkspaceActions.svelte";

  let {
    onOpen,
  }: {
    onOpen?: () => void;
  } = $props();

  let menu: { x: number; y: number; id: string } | null = $state(null);
  // Measured menu box feeds the shared clamp so the tab menu can never open
  // off-screen (the old innerWidth-200 guess overflowed on short windows).
  let menuEl: HTMLDivElement | undefined = $state();
  let menuPos = $state({ left: 0, top: 0 });
  let recentsOpen = $state(false);
  let dragFrom = $state<number | null>(null);
  // Where a dragged tab would land. `before` picks the left/right half of the
  // hovered tab; null means "no useful insertion point" and hides the bar.
  let dropTarget = $state<{ index: number; before: boolean } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  let unusedRecents = $derived(
    $repoStore.recentRepos.filter(
      (path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), pathOpts),
    ),
  );

  function closeMenu() {
    menu = null;
    recentsOpen = false;
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
      closeMenu();
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
    window.addEventListener("resize", closeMenu);
    return () => {
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("resize", closeMenu);
    };
  });

  function endDrag() {
    dragFrom = null;
    dropTarget = null;
  }

  /**
   * Arrow-key roving focus across the tablist (ARIA tabs pattern): focus
   * moves and wraps without changing the active repo; Home/End jump to the
   * edges. Focus on the container itself enters the list in travel direction.
   */
  function onTablistKeydown(e: KeyboardEvent) {
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

  /**
   * Container-level dragover: one handler computes the insertion point for
   * whatever tab (or gap) is under the pointer, so the indicator can't go
   * stale between child elements. Adjacent-to-self positions are no-op moves
   * and show nothing.
   */
  function onScrollerDragOver(e: DragEvent) {
    e.preventDefault();
    if (dragFrom === null) return;
    const tabEl = e.target instanceof Element ? e.target.closest("[data-tab-index]") : null;
    if (!(tabEl instanceof HTMLElement) || !tabEl.dataset.tabIndex) {
      dropTarget = null;
      return;
    }
    const index = Number(tabEl.dataset.tabIndex);
    if (!Number.isInteger(index)) {
      dropTarget = null;
      return;
    }
    const rect = tabEl.getBoundingClientRect();
    const before = e.clientX < rect.left + rect.width / 2;
    const insertAt = before ? index : index + 1;
    if (insertAt === dragFrom || insertAt === dragFrom + 1) {
      dropTarget = null;
      return;
    }
    dropTarget = { index, before };
  }

  function onScrollerDrop(e: DragEvent) {
    e.preventDefault();
    if (dragFrom !== null && dropTarget) {
      const { index, before } = dropTarget;
      const insertAt = before ? index : index + 1;
      const adjusted = insertAt > dragFrom ? insertAt - 1 : insertAt;
      if (adjusted !== dragFrom) repoStore.reorderTabs(dragFrom, adjusted);
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
  <div class="h-10 bg-surface/60 border-b border-border/60 flex items-center select-none shrink-0 text-[11px] px-2 gap-1">
    <div
      bind:this={scroller}
      class="flex-1 flex items-center gap-1 overflow-x-auto min-w-0 py-1"
      role="tablist"
      tabindex="0"
      aria-label="Open repositories"
      onkeydown={onTablistKeydown}
      ondragover={onScrollerDragOver}
      ondrop={onScrollerDrop}
    >
      {#each $repoStore.openTabs as tab, index (tab.id)}
        <div
          role="tab"
          tabindex={tab.isActive ? 0 : -1}
          aria-selected={tab.isActive}
          aria-keyshortcuts="Enter p"
          data-active-repo={tab.isActive ? "true" : "false"}
          data-tab-index={index}
          title={`${tab.path}\n←/→ move tabs · P to ${tab.pinned ? "unpin" : "pin"}`}
          draggable="true"
          onclick={() => repoStore.activateTab(tab.id)}
          onkeydown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              repoStore.activateTab(tab.id);
            } else if (e.key === "p" || e.key === "P") {
              // Keyboard twin of the double-click pin affordance.
              e.preventDefault();
              repoStore.pinTab(tab.id, !tab.pinned);
            }
          }}
          onauxclick={(e) => {
            if (e.button === 1) {
              e.preventDefault();
              void repoStore.closeTab(tab.id);
            }
          }}
          ondblclick={() => repoStore.pinTab(tab.id, !tab.pinned)}
          oncontextmenu={(e) => onContext(e, tab.id)}
          ondragstart={() => (dragFrom = index)}
          ondragend={endDrag}
          class="group relative max-w-[14rem] min-w-[7rem] pl-2.5 pr-1 flex items-center gap-1.5 rounded-full border shrink-0 transition-[color,background-color,border-color,box-shadow] duration-150 {dropTarget?.index === index ? 'border-accent/50' : ''} {tab.isActive
            ? 'bg-surfaceHover border-border/80 text-textPrimary shadow-sm'
            : 'border-transparent text-textMuted hover:text-textPrimary hover:bg-surfaceHover/60'}"
        >
          {#if dropTarget?.index === index}
            <span
              aria-hidden="true"
              class="absolute top-1/2 -translate-y-1/2 w-[3px] h-5 rounded-full bg-accent shadow-glow transition-opacity {dropTarget.before
                ? '-left-[3px]'
                : '-right-[3px]'}"
            ></span>
          {/if}
          {#if tab.pinned}
            <Pin size={10} class="text-accent shrink-0" />
          {:else}
            <FolderGit2 size={11} class="shrink-0 {tab.error ? 'text-rose-400' : 'text-accent'}" />
          {/if}
          <span class="truncate font-medium">{tab.label}</span>
          {#if tab.currentBranch}
            <span class="truncate text-[10px] text-textMuted/80 font-mono hidden sm:inline">{tab.currentBranch}</span>
          {/if}
          {#if tab.isDirty}
            <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shrink-0 shadow-[0_0_6px_rgb(251_191_36/0.8)]" title="Uncommitted changes"></span>
          {/if}
          {#if tab.conflictedCount > 0}
            <span class="text-amber-400 shrink-0">{tab.conflictedCount}</span>
          {/if}
          <button
            type="button"
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

    <button
      type="button"
      title="Open repository"
      onclick={() => onOpen?.()}
      class="gp-icon-btn !p-1 shrink-0 hover:text-accent"
    >
      <Plus size={13} />
    </button>

    <div class="relative shrink-0" data-recents-menu>
      <button
        type="button"
        title="Recent repositories"
        onclick={() => (recentsOpen = !recentsOpen)}
        class="gp-icon-btn !p-1 h-full"
      >
        <ChevronDown size={13} />
      </button>
      {#if recentsOpen}
        <div
          class="absolute right-0 top-full mt-1.5 w-80 gp-menu gp-pop"
          style="z-index: {LAYERS.MENU}"
        >
          <div class="px-2 pt-1 pb-1.5 text-[10px] uppercase tracking-wider text-textMuted">
            Recent repositories
          </div>
          {#if $repoStore.recentRepos.length === 0}
            <div class="px-3 py-2 text-textMuted">No recent repositories</div>
          {:else}
            {#each $repoStore.recentRepos as path}
              <div class="flex items-center gap-1 px-0.5">
                <button
                  type="button"
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
                  title="Remove from recents"
                  onclick={() => repoStore.removeRecent(path)}
                  class="p-1 rounded-full text-textMuted hover:text-rose-400 hover:bg-surfaceHover"
                >
                  <X size={11} />
                </button>
              </div>
            {/each}
          {/if}
          {#if unusedRecents.length === 0 && $repoStore.recentRepos.length > 0}
            <div class="px-3 py-1.5 text-[10px] text-textMuted">All recents are already open</div>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Workspace-wide actions live here rather than in a view: they act on
         every open repository, so they belong to the tab strip that owns
         them. Hidden with a single tab open, where they say nothing new. -->
    <WorkspaceActions />
  </div>
{/if}

{#if menu}
  {@const tab = $repoStore.openTabs.find((item) => item.id === menu?.id)}
  {#if tab}
    <div
      bind:this={menuEl}
      use:portal={"body"}
      data-repo-menu
      class="fixed min-w-44 gp-menu gp-pop text-[11px] text-textPrimary"
      style="left: {menuPos.left}px; top: {menuPos.top}px; z-index: {LAYERS.MENU}"
    >
      <button class="gp-menu-item" onclick={() => { repoStore.pinTab(tab.id, !tab.pinned); closeMenu(); }}>
        {tab.pinned ? "Unpin" : "Pin"} tab
      </button>
      <button class="gp-menu-item" onclick={() => { void navigator.clipboard.writeText(tab.path); closeMenu(); }}>
        Copy path
      </button>
      <button class="gp-menu-item" onclick={() => { void repoStore.closeTab(tab.id); closeMenu(); }}>
        Close
      </button>
      <button class="gp-menu-item" onclick={() => { void repoStore.closeOtherTabs(tab.id); closeMenu(); }}>
        Close others
      </button>
      <button class="gp-menu-item" onclick={() => { void repoStore.closeTabsToTheRight(tab.id); closeMenu(); }}>
        Close tabs to the right
      </button>
      <button class="gp-menu-item" onclick={() => { onOpen?.(); closeMenu(); }}>
        <FolderOpen size={11} /> Open repository…
      </button>
    </div>
  {/if}
{/if}
