<script lang="ts">
  /**
   * The one header of a repository page (Work: Overview, Remote, Tasks;
   * Insights: Pulse, Coverage, Health, Storage) and of the Agents surface.
   *
   * It is the panel header of design language section 7 and 9 (`gp-header`:
   * one height from density, the tonal `surface` step, no rule under it) with
   * the surface title type (`gp-surface-title`, section 6's `text-base`), so
   * moving between pages never changes the header's height, type, padding or
   * alignment. Tool panes (Explorer, Map, Graph, Diff) are named by the view
   * bar's tab and do not carry one.
   *
   * Everything sits on ONE row: the title, what the page wants beside it
   * (`meta`: counts, status chips), the description, then the actions on the
   * right. The description takes only the width left over and truncates with
   * its full text in `title=`, so the bar is the same height whether a page has
   * a description or not. The row is a minimum height, not a fixed one: a page
   * with many actions wraps them onto a second row below a narrow width rather
   * than spilling out of a bar of fixed height.
   *
   * A page with more tools than one row holds (Tasks) passes `toolbar`: a
   * second, full-width row inside the same `<header>`, so the bar stays one
   * element (one fill, one bottom edge for what measures under it) and its
   * tools are laid out on purpose rather than wrapped wherever they ran out.
   * The bar then pads its top by the toolbar's bottom padding, so the title
   * row is not flush with the edge and a corner badge on one of its buttons
   * stays inside the bar. Pages without it render exactly as before.
   */
  import type { Snippet } from "svelte";

  interface Props {
    title: string;
    /** One line beside the title; truncated, its full text on `title=`. */
    description?: string;
    /** Heading element. Level 1 for a full-pane surface, 2 (default) inside a page. */
    level?: 1 | 2;
    /** Before the title, e.g. a navigator toggle. */
    leading?: Snippet;
    /** Right after the title, before the description: a count, status chips. */
    meta?: Snippet;
    /** Right-aligned page-level actions. */
    actions?: Snippet;
    /** A second full-width row of tools under the title row, inside the header. */
    toolbar?: Snippet;
    /** `data-testid` of the description line, for a page whose tests read it. */
    descriptionTestId?: string;
    /** Extra utilities on the bar, e.g. a test hook or a margin. */
    class?: string;
  }

  let { title, description, level = 2, leading, meta, actions, toolbar, descriptionTestId, class: className = "" }: Props = $props();
</script>

<header
  class={["gp-header", className]}
  data-page-header
  style="height: auto; min-height: var(--gp-header-h); flex-wrap: wrap; row-gap: var(--gp-space-1);{toolbar ? ' padding-top: var(--gp-space-2);' : ''}"
>
  {@render leading?.()}
  {#if level === 1}
    <h1 class="gp-surface-title shrink-0">{title}</h1>
  {:else}
    <h2 class="gp-surface-title shrink-0">{title}</h2>
  {/if}
  {@render meta?.()}
  {#if description}
    <p class="m-0 min-w-0 flex-1 truncate text-meta text-textMuted" data-page-header-description data-testid={descriptionTestId} title={description}>{description}</p>
  {/if}
  {#if actions}
    <div class="gp-header-actions" style="flex-shrink: 1; min-width: 0; flex-wrap: wrap; justify-content: flex-end;">{@render actions()}</div>
  {/if}
  {#if toolbar}
    <div data-page-header-toolbar style="flex-basis: 100%; min-width: 0; display: flex; flex-wrap: wrap; align-items: center; gap: var(--gp-space-1) var(--gp-space-3); padding-bottom: var(--gp-space-2);">{@render toolbar()}</div>
  {/if}
</header>
