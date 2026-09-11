<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import type { ViewTab } from "../repos/persist";
  import { activeSectionFor, sectionsFor, VIEW_REGISTRY } from "../views/viewRegistry";
  import { destinationGuide, tipGuideDescId, tipGuideKey } from "../views/viewGuide";
  import { focusTabAt, handleTablistKeydown, tabProps } from "../dom/tablist";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";

  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

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
    class="h-9 shrink-0 px-3 flex items-center gap-3 border-b border-border/60 gp-section-edge bg-surface/40 select-none"
  >
    <div class="sr-only">
      {#each sections as section (section.id)}
        {@const guide = destinationGuide(tipGuideKey(view, section.id))}
        {#if guide}
          <span id={tipGuideDescId(tipGuideKey(view, section.id))}>{guide.summary}</span>
        {/if}
      {/each}
    </div>
    <div
      bind:this={list}
      class="gp-segmented"
      class:gp-liquid-tabs={macos}
      role="tablist"
      aria-label="{groupLabel} sections"
      tabindex="-1"
      onkeydown={onKeydown}
    >
      {#each sections as section (section.id)}
        {@const isActive = active === section.id}
        {@const props = tabProps(view, section.id, isActive)}
        {@const guideKey = tipGuideKey(view, section.id)}
        {@const guide = destinationGuide(guideKey)}
        <button
          type="button"
          role={props.role}
          id={props.id}
          aria-selected={props["aria-selected"]}
          aria-controls={props["aria-controls"]}
          aria-describedby={guide ? tipGuideDescId(guideKey) : undefined}
          tabindex={props.tabindex}
          data-active={isActive ? "true" : "false"}
          data-section={section.id}
          data-tip-guide={guideKey}
          onclick={() => repoStore.setViewSection(view, section.id)}
          class="gp-seg-btn text-[11px]! py-1!"
        >
          {#if macos && isActive}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: `active-section-${view}` }}
              out:sendSelection={{ key: `active-section-${view}` }}
            ></span>
          {/if}
          <span>{section.label}</span>
        </button>
      {/each}
    </div>
    {@render children?.()}
  </div>
{/if}
