<script lang="ts">
  /**
   * The sidebar's Git | Explorer switch.
   *
   * Real tabs (`dom/tablist.ts`), as the header's section tabs are: each points
   * at the panel Sidebar renders for it, only the active one is in the Tab
   * order, and the arrow keys, Home and End move between them with selection
   * following focus — both panels are mounted or cached, so there is nothing
   * to wait for.
   *
   * It wears the header switcher's track. On the Mac that is the liquid track
   * with one selection pill gliding between the two tabs; elsewhere the
   * segmented control's raised bubble. Either way the active tab also carries
   * the accent and the semibold weight, so it reads in both themes and under
   * Increase Contrast, where the primitive outlines it.
   *
   * Each tab is a 28px row that fills its half of the track. `gp-seg-btn` is
   * not a flex box on its own: before this the icon sat on its own baseline
   * at the left edge and the label floated centred beside it.
   */
  import { FolderTree, GitBranch } from "@lucide/svelte";
  import { crossfade } from "svelte/transition";
  import { focusTabAt, tabProps } from "../dom/tablist";
  import { isMacOS } from "../platform";
  import { sidebarViewForKey, SIDEBAR_VIEWS, type SidebarView } from "../sidebar/layoutStore";
  import { liquidSelection } from "../ui/transitions";

  let {
    view,
    onSelect,
  }: {
    view: SidebarView;
    onSelect: (view: SidebarView) => void;
  } = $props();

  const GROUP = "sidebar-view";
  const TABS = {
    git: { label: "Git", title: "Show Git branches, worktrees, and changes", icon: GitBranch },
    files: { label: "Explorer", title: "Browse and search repository files", icon: FolderTree },
  } as const;

  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  let list: HTMLDivElement | undefined = $state();

  function onKeydown(event: KeyboardEvent) {
    const next = sidebarViewForKey(view, event.key);
    if (!next) return;
    event.preventDefault();
    onSelect(next);
    // Roving tabindex changes which tab is tabbable, not where focus sits.
    focusTabAt(list, SIDEBAR_VIEWS.indexOf(next));
  }
</script>

<div
  bind:this={list}
  class="gp-segmented gp-liquid-tabs flex w-full"
  role="tablist"
  aria-label="Sidebar view"
  tabindex="-1"
  data-testid="sidebar-view-switcher"
  onkeydown={onKeydown}
>
  {#each SIDEBAR_VIEWS as id (id)}
    {@const tab = TABS[id]}
    {@const isActive = view === id}
    {@const props = tabProps(GROUP, id, isActive)}
    {@const Icon = tab.icon}
    <button
      type="button"
      role={props.role}
      id={props.id}
      aria-selected={props["aria-selected"]}
      aria-controls={props["aria-controls"]}
      tabindex={props.tabindex}
      data-testid="sidebar-view-{id}"
      data-active={isActive ? "true" : "false"}
      title={tab.title}
      onclick={() => onSelect(id)}
      class="gp-seg-btn relative inline-flex h-7 flex-1 items-center justify-center gap-1.5"
    >
      {#if macos && isActive}
        <span
          class="gp-liquid-selection gp-gpu"
          aria-hidden="true"
          in:receiveSelection={{ key: GROUP }}
          out:sendSelection={{ key: GROUP }}
        ></span>
      {/if}
      <!-- `relative`: on the Mac the selection pill is absolutely placed and
           would otherwise paint over a static icon. -->
      <Icon size={13} aria-hidden="true" class="relative shrink-0" />
      <span>{tab.label}</span>
    </button>
  {/each}
</div>
