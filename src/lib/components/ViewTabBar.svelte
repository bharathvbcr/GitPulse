<script lang="ts">
  import { onMount } from "svelte";
  import { ChevronDown } from "lucide-svelte";
  import { repoStore } from "../stores/repoStore";
  import type { ViewTab } from "../repos/persist";
  import {
    VIEW_NAV,
    formatViewTabLabel,
    isViewNavGroupActive,
    viewNavTriggerLabel,
    type ViewGroupId,
    type ViewNavGroup,
    type ViewNavItem,
  } from "../views/viewNav";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";

  let { conflictedCount = 0 }: { conflictedCount?: number } = $props();

  let openGroup = $state<ViewGroupId | null>(null);
  let menuPos = $state<{ x: number; y: number } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();

  let activeTab = $derived($repoStore.activeTab);
  let openMenu = $derived(VIEW_NAV.find((group) => group.id === openGroup) ?? null);

  function closeMenu() {
    openGroup = null;
    menuPos = null;
  }

  function toggleMenu(group: ViewNavGroup, event: MouseEvent) {
    if (openGroup === group.id) {
      closeMenu();
      return;
    }
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    openGroup = group.id;
    menuPos = { x: rect.left, y: rect.bottom + 4 };
  }

  function selectTab(tab: ViewTab) {
    repoStore.setActiveTab(tab);
    closeMenu();
  }

  function tabClass(item: ViewNavItem, active: boolean): string {
    const warning = item.id === "conflict";
    if (active && warning) {
      return "!text-amber-400";
    }
    return "";
  }

  function menuClass(active: boolean): string {
    if (active) {
      return "bg-surfaceHover !text-accent shadow-sm font-semibold";
    }
    return "text-textMuted hover:text-textPrimary";
  }

  function handlePointerDown(event: PointerEvent) {
    if (!openGroup) return;
    // The panel and any header menu trigger count as "inside".
    // Marking the whole tablist would swallow Graph/Diff/Resolve clicks
    // and header padding, leaving the menu stuck open.
    if (
      !shouldDismissOverlay(
        event.target,
        "[data-view-nav-menu], [data-view-nav-trigger]",
      )
    ) {
      return;
    }
    closeMenu();
  }

  function handleKey(event: KeyboardEvent) {
    if (event.key === "Escape" && openGroup) {
      event.preventDefault();
      closeMenu();
    }
  }

  onMount(() => {
    // Capture so a stopPropagation on a pane cannot trap the menu open.
    // Do not close on arbitrary nested scroll: focusing the trigger inside
    // `.gp-header-scroll` can fire a scroll event and dismiss on open.
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKey);
    window.addEventListener("resize", closeMenu);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("resize", closeMenu);
    };
  });

  $effect(() => {
    activeTab;
    const active = scroller?.querySelector("[data-active-view='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

<div bind:this={scroller} class="flex items-center gap-1.5 shrink-0" role="tablist" aria-label="Views">
  {#each VIEW_NAV as group (group.id)}
    {#if group.kind === "tabs"}
      <div class="gp-segmented">
        {#each group.items as item (item.id)}
          {@const active = activeTab === item.id}
          <button
            type="button"
            role="tab"
            aria-selected={active}
            data-active-view={active ? "true" : "false"}
            data-active={active ? "true" : "false"}
            onclick={() => selectTab(item.id)}
            class="gp-seg-btn {tabClass(item, active)}"
          >
            {formatViewTabLabel(item, conflictedCount)}
          </button>
        {/each}
      </div>
    {:else}
      {@const active = isViewNavGroupActive(group, activeTab)}
      <div class="relative">
        <button
          type="button"
          aria-haspopup="menu"
          aria-expanded={openGroup === group.id}
          data-view-nav-trigger={group.id}
          data-active-view={active ? "true" : "false"}
          onclick={(event) => toggleMenu(group, event)}
          class="gp-btn !py-1 flex items-center gap-1 {menuClass(active)}"
        >
          <span>{viewNavTriggerLabel(group, activeTab)}</span>
          <ChevronDown size={11} class="pointer-events-none text-textMuted" />
        </button>
      </div>
    {/if}
  {/each}
</div>

{#if openMenu && menuPos}
  <div
    use:portal={"body"}
    data-view-nav-menu
    role="menu"
    class="fixed min-w-40 gp-menu gp-pop text-xs text-textPrimary"
    style="left: {menuPos.x}px; top: {menuPos.y}px; z-index: {LAYERS.MENU}"
  >
    {#each openMenu.items as item (item.id)}
      {@const active = activeTab === item.id}
      <button
        type="button"
        role="menuitem"
        onclick={() => selectTab(item.id)}
        class="gp-menu-item justify-between {active
          ? 'text-accent font-semibold'
          : 'text-textMuted hover:text-textPrimary'}"
      >
        <span>{formatViewTabLabel(item, conflictedCount)}</span>
        {#if active}
          <span>✓</span>
        {/if}
      </button>
    {/each}
  </div>
{/if}
