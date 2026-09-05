<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import type { ViewTab } from "../repos/persist";
  import { activeSectionFor, sectionsFor, VIEW_REGISTRY } from "../views/viewRegistry";
  import { focusTabAt, handleTablistKeydown, tabProps } from "../dom/tablist";
  import { sectionAccelerator } from "../views/viewShortcuts";

  /**
   * A view's own lens switcher.
   *
   * The header used to carry one tab per lens, which is why the app had to
   * teleport between them mid-thought. Here the lenses sit inside the view
   * that owns the subject, so switching one keeps the other — the selected
   * commit, the open file — exactly where it was.
   *
   * Every sectioned view renders this, so the control cannot drift per view;
   * it reads the catalog rather than taking a list of its own.
   *
   * The tablist semantics are real ones. This declared `role="tablist"` and
   * `role="tab"` with no `aria-controls`, no panel, no roving tabindex and no
   * arrow keys — announcing a pattern it did not implement. The panel side is
   * `ViewSectionPanel`, which every sectioned view wraps its content in.
   */

  let {
    view,
    /** Extra controls for the current section, rendered to the right. */
    children,
  }: {
    view: ViewTab;
    children?: import("svelte").Snippet;
  } = $props();

  const sections = $derived(sectionsFor(view));
  const active = $derived(activeSectionFor(view, $repoStore.viewSections));
  const activeIndex = $derived(Math.max(0, sections.findIndex((s) => s.id === active)));
  // The registry's display label, not the raw id: the old accessible name
  // announced "work sections".
  const groupLabel = $derived(VIEW_REGISTRY[view].label);

  let list: HTMLDivElement | undefined = $state();

  function onKeydown(event: KeyboardEvent) {
    const move = handleTablistKeydown(event.key, activeIndex, sections.length);
    if (!move) return;
    event.preventDefault();
    const target = sections[move.index];
    if (!target) return;
    repoStore.setViewSection(view, target.id);
    // Selection follows focus, and focus has to be moved explicitly: roving
    // tabindex changes which tab is tabbable, not where focus sits.
    focusTabAt(list, move.index);
  }
</script>

{#if sections.length > 1}
  <div
    class="h-9 shrink-0 px-3 flex items-center gap-3 border-b border-border/60 bg-surface/40 select-none"
  >
    <div
      bind:this={list}
      class="gp-segmented"
      role="tablist"
      aria-label="{groupLabel} sections"
      tabindex="-1"
      onkeydown={onKeydown}
    >
      {#each sections as section, index (section.id)}
        {@const isActive = active === section.id}
        {@const props = tabProps(view, section.id, isActive)}
        <button
          type="button"
          role={props.role}
          id={props.id}
          aria-selected={props["aria-selected"]}
          aria-controls={props["aria-controls"]}
          tabindex={props.tabindex}
          data-active={isActive ? "true" : "false"}
          data-section={section.id}
          onclick={() => repoStore.setViewSection(view, section.id)}
          class="gp-seg-btn !text-[11px] !py-1"
          title="{section.label} ({sectionAccelerator(index)})"
        >
          {section.label}
        </button>
      {/each}
    </div>
    {@render children?.()}
  </div>
{/if}
