<script lang="ts">
  import { repoStore } from "../stores/repoStore";
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

  $effect(() => {
    activeTab;
    const active = scroller?.querySelector("[data-active-view='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

<div bind:this={scroller} class="flex items-center gap-1.5 shrink-0" role="tablist" aria-label="Views">
  <div class="gp-segmented" class:gp-liquid-tabs={macos}>
    {#each navItems as item (item.id)}
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
