<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import type { ViewTab } from "../repos/persist";
  import { activeSectionFor, sectionsFor } from "../views/viewRegistry";

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
</script>

{#if sections.length > 1}
  <div
    class="h-9 shrink-0 px-3 flex items-center gap-3 border-b border-border/60 bg-surface/40 select-none"
  >
    <div class="gp-segmented" role="tablist" aria-label="{view} sections">
      {#each sections as section (section.id)}
        {@const isActive = active === section.id}
        <button
          type="button"
          role="tab"
          aria-selected={isActive}
          data-active={isActive ? "true" : "false"}
          data-section={section.id}
          onclick={() => repoStore.setViewSection(view, section.id)}
          class="gp-seg-btn !text-[11px] !py-1"
        >
          {section.label}
        </button>
      {/each}
    </div>
    {@render children?.()}
  </div>
{/if}
