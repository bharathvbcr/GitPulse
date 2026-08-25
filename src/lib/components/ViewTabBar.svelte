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

  function handlePointerDown(event: MouseEvent) {
    if (!openGroup) return;
    const el =
      event.target instanceof Element
        ? event.target
        : event.target instanceof Node
          ? event.target.parentElement
          : null;
    if (el?.closest("[data-view-nav]")) return;
    closeMenu();
  }

  function handleKey(event: KeyboardEvent) {
    if (event.key === "Escape" && openGroup) {
      event.preventDefault();
      closeMenu();
    }
  }

  onMount(() => {
    window.addEventListener("mousedown", handlePointerDown);
    window.addEventListener("keydown", handleKey);
    window.addEventListener("scroll", closeMenu, true);
    window.addEventListener("resize", closeMenu);
    return () => {
      window.removeEventListener("mousedown", handlePointerDown);
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("scroll", closeMenu, true);
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

<div bind:this={scroller} class="flex items-center gap-1.5 shrink-0" data-view-nav role="tablist" aria-label="Views">
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
    use:portal
    data-view-nav
    role="menu"
    class="fixed z-50 min-w-40 gp-menu gp-pop text-xs text-textPrimary"
    style="left: {menuPos.x}px; top: {menuPos.y}px"
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
