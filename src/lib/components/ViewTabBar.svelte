<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { focusTabAt, handleTablistKeydown } from "../dom/tablist";
  import { VIEW_PANE_ID } from "../views/viewRegistry";
  import { viewAccelerator } from "../views/viewShortcuts";
  import type { ViewTab } from "../repos/persist";
  import { formatViewTabLabel, type ViewNavItem } from "../views/viewNav";
  import { visibleViewNav } from "../views/viewVisibility";
  import { interfaceStore } from "../stores/interfaceStore";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";

  const macos = isMacOS();
  // Svelte owns interruption and element cleanup. Only the decorative pill
  // moves; labels and focus targets stay in place, even during rapid changes.
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  /**
   * The header view switcher: four tabs, no menus.
   *
   * This used to carry a portalled dropdown with capture-phase dismissal,
   * because fifteen views could not fit a title bar and two thirds of them
   * had to fold into "Inspect" and "More". Consolidation removed the reason
   * rather than the symptom — a new panel is a section of the view that owns
   * its subject now — so the last menu group emptied and the dropdown branch
   * became unreachable. It is deleted rather than left standing beside the
   * tabs it no longer serves.
   */

  let { conflictedCount = 0 }: { conflictedCount?: number } = $props();

  let scroller: HTMLDivElement | undefined = $state();

  let activeTab = $derived($repoStore.activeTab);
  // The header lists only the views the user kept, plus the ones visibility
  // pins on regardless (the active view, and Work while conflicts stand).
  let navItems = $derived(
    visibleViewNav($interfaceStore.hiddenViews, { activeTab, conflictedCount }),
  );

  function selectTab(tab: ViewTab) {
    repoStore.setActiveTab(tab);
  }

  // Resolve is a section of Work now, so the warning colour follows the
  // conflicts rather than the tab: Work only turns amber while files actually
  // carry markers. Colouring Work amber whenever it is active would make the
  // signal mean "you are on Work", which is not a warning at all.
  function tabClass(item: ViewNavItem, active: boolean): string {
    const warning = item.id === "work" && conflictedCount > 0;
    if (active && warning) {
      return "!text-amber-400";
    }
    return "";
  }

  /**
   * Arrow keys move between views, and only the selected tab is in the tab
   * order. The strip declared `role="tab"` and implemented neither, so a
   * screen-reader user was told these were tabs and then found none of the
   * behaviour that word promises.
   */
  function onKeydown(event: KeyboardEvent) {
    const current = navItems.findIndex((item) => item.id === activeTab);
    const move = handleTablistKeydown(event.key, current, navItems.length);
    if (!move) return;
    event.preventDefault();
    const target = navItems[move.index];
    if (!target) return;
    selectTab(target.id);
    focusTabAt(scroller, move.index);
  }

  $effect(() => {
    activeTab;
    const active = scroller?.querySelector("[data-active-view='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

<div
  bind:this={scroller}
  class="flex items-center gap-1.5 shrink-0"
  role="tablist"
  aria-label="Views"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="gp-segmented" class:gp-liquid-tabs={macos}>
    {#each navItems as item (item.id)}
      {@const active = activeTab === item.id}
      {@const accelerator = viewAccelerator(item.id)}
      <button
        type="button"
        role="tab"
        aria-selected={active}
        aria-controls={VIEW_PANE_ID}
        tabindex={active ? 0 : -1}
        data-active-view={active ? "true" : "false"}
        data-active={active ? "true" : "false"}
        onclick={() => selectTab(item.id)}
        title={accelerator ? `${item.label} (${accelerator})` : item.label}
        class="gp-seg-btn {tabClass(item, active)}"
      >
        {#if macos && active}
          <span
            class="gp-liquid-selection gp-gpu"
            aria-hidden="true"
            in:receiveSelection={{ key: "active-view" }}
            out:sendSelection={{ key: "active-view" }}
          ></span>
        {/if}
        <span>{formatViewTabLabel(item, conflictedCount)}</span>
        <!-- The uncommitted-file count rode the Diff tab. Diff is a section
             of History now, and History is where that count is acted on, so
             the badge follows the content. -->
        {#if item.id === "history" && $repoStore.statuses.length > 0}
          <span class="ml-1 px-1.5 py-0.2 rounded-full text-[9px] font-mono bg-amber-500/20 text-amber-600 dark:text-amber-300 font-semibold">
            {$repoStore.statuses.length}
          </span>
        {/if}
      </button>
    {/each}
  </div>
</div>
