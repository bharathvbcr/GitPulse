<script lang="ts" generics="T">
  import type { Snippet } from "svelte";
  import { computeWindow } from "../dom/virtualWindow";

  interface Props {
    /** Rows to window over. Omit and pass `itemCount` to render blanks. */
    items?: readonly T[];
    itemCount?: number;
    /** Fixed row height in px; rows must not wrap. */
    rowHeight: number;
    overscan?: number;
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
    overscan = 8,
    scrollTop = $bindable(0),
    row,
    class: className = "",
  }: Props = $props();

  let scroller: HTMLDivElement | undefined = $state();
  let viewportHeight = $state(0);

  let total = $derived(itemCount ?? items?.length ?? 0);
  let win = $derived(computeWindow(scrollTop, viewportHeight, total, rowHeight, overscan));

  function handleScroll(event: Event) {
    scrollTop = (event.target as HTMLDivElement).scrollTop;
  }

  $effect(() => {
    const el = scroller;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      viewportHeight = entries[0]?.contentRect.height || 0;
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
  <div style="height: {total * rowHeight}px; position: relative;">
    <div
      class="absolute inset-x-0 top-0"
      style="transform: translate3d(0, {win.start * rowHeight}px, 0);"
    >
      {#each { length: win.end - win.start } as _, i}
        {@render row(items?.[win.start + i], win.start + i)}
      {/each}
    </div>
  </div>
</div>
