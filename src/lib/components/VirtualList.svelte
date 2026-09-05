<script lang="ts" generics="T">
  import type { Snippet } from "svelte";
  import {
    clampScrollTop,
    computeWindow,
    ensureNonEmptyWindow,
  } from "../dom/virtualWindow";

  interface Props {
    /** Rows to window over. Omit and pass `itemCount` to render blanks. */
    items?: readonly T[];
    itemCount?: number;
    /** Fixed row height in px; rows must not wrap while virtualizing. */
    rowHeight: number;
    /**
     * Set false to lay every row out in normal flow instead of windowing.
     *
     * Windowing positions row `n` at `n * rowHeight`, so it is only correct
     * while every row really is that tall. A row that wraps is not: it draws
     * over its neighbours and its own overflow is clipped away. Callers that
     * allow wrapping turn this off and accept rendering the whole list, which
     * is why they also have to bound how much they will render.
     */
    virtualize?: boolean;
    overscan?: number;
    /**
     * Size the row container to its widest row instead of to the viewport.
     *
     * Off by default: for a list of truncating rows it would turn every long
     * label into horizontal scroll. The diff turns it on because its rows are
     * code — one shared horizontal scrollbar for the surface, and every row's
     * background running the full width of it, rather than a scrollbar per
     * row and tints that stop where the viewport used to end.
     */
    contentWidth?: boolean;
    /**
     * Two-way bound scroll position. Binding several lists to one variable
     * keeps split panes scrolling in lockstep.
     */
    scrollTop?: number;
    /** Receives the row item (undefined past a shorter column) and its index. */
    row: Snippet<[item: T | undefined, index: number]>;
    class?: string;
  }

  let {
    items,
    itemCount,
    rowHeight,
    virtualize = true,
    overscan = 8,
    contentWidth = false,
    scrollTop = $bindable(0),
    row,
    class: className = "",
  }: Props = $props();

  let scroller: HTMLDivElement | undefined = $state();
  let viewportHeight = $state(0);

  let total = $derived(itemCount ?? items?.length ?? 0);
  // Render from the clamped anchor: when the list shrinks under a deep
  // scroll (whitespace collapse, file switch), the raw bindable sits past
  // the content for a frame until the browser's async clamp round-trips.
  // Window AND translate both derive from the clamped value so the tail
  // paints immediately, and ensureNonEmptyWindow absorbs the residual float
  // edge where the clamped cap rounds past the last row. The spacer below
  // stays at total * rowHeight, so the scrollbar keeps telling the truth and
  // the 0.5px sync guard lets the bindable converge on its own.
  let effectiveScrollTop = $derived(clampScrollTop(scrollTop, total, rowHeight, viewportHeight));
  let win = $derived(
    ensureNonEmptyWindow(
      computeWindow(effectiveScrollTop, viewportHeight, total, rowHeight, overscan),
      total,
      rowHeight,
      viewportHeight
    )
  );

  function handleScroll(event: Event) {
    scrollTop = (event.target as HTMLDivElement).scrollTop;
  }

  $effect(() => {
    const el = scroller;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      // A bogus measurement (NaN / ±Infinity / negative) collapses to 0,
      // exactly how computeWindow treats degenerate viewports — it must not
      // poison the window math.
      const measured = entries[0]?.contentRect.height;
      viewportHeight = Number.isFinite(measured) && measured > 0 ? measured : 0;
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  // Honor an externally written scroll position (split-pane sync). The guard
  // breaks the feedback loop with our own scroll events.
  $effect(() => {
    const el = scroller;
    if (el && Math.abs(el.scrollTop - scrollTop) > 0.5) {
      el.scrollTop = scrollTop;
    }
  });
</script>

<div bind:this={scroller} onscroll={handleScroll} class="overflow-auto gp-scroll relative {className}">
  {#if virtualize}
    <div style="height: {total * rowHeight}px; position: relative;">
      <div
        class="absolute top-0 {contentWidth ? 'left-0 w-max min-w-full' : 'inset-x-0'}"
        style="transform: translate3d(0, {win.start * rowHeight}px, 0);"
      >
        {#each { length: win.end - win.start } as _, i}
          {@render row(items?.[win.start + i], win.start + i)}
        {/each}
      </div>
    </div>
  {:else}
    <!-- Normal flow: each row takes the height its content needs, and the
         browser does the layout. Scroll binding still works, so split panes
         stay in lockstep. -->
    <div class={contentWidth ? "w-max min-w-full" : "contents"}>
      {#each { length: total } as _, i}
        {@render row(items?.[i], i)}
      {/each}
    </div>
  {/if}
</div>
