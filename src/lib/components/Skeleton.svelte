<script lang="ts">
  import { motionExitMs } from "../motion/tokens";
  import { startSkeletonGate, type SkeletonGate } from "../ui/skeletonGate";

  let {
    variant = "text",
    count = 1,
    width = "",
    height = "",
    class: extraClass = "",
    animated = true,
    hold = false,
  }: {
    variant?: "text" | "rect" | "circle" | "card" | "tree-row" | "code-lines" | "graph-rows" | "diff-hunks" | "terminal-replay" | "pane-shell";
    count?: number;
    width?: string;
    height?: string;
    class?: string;
    animated?: boolean;
    /**
     * Opt in to the 400 ms minimum: a shown skeleton waits out what is left of it, then
     * leaves in 70 ms. Off by default because the hold is an outro, and Svelte keeps an
     * outgoing branch in the DOM while its replacement mounts, so content in the same flow
     * lands a skeleton lower (and a caption in the branch outlives the load) for the whole
     * hold. Turn it on only for a skeleton swapped into the same box without taking flow
     * space the replacement needs. The 300 ms show delay applies either way.
     */
    hold?: boolean;
  } = $props();

  const items = $derived(Array.from({ length: Math.max(1, count) }, (_, i) => i));

  /*
   * A load that finishes inside 300 ms never shows the placeholder, and one
   * that did appear stays 400 ms (DESIGN_LANGUAGE §8.4). The box is always
   * laid out, only transparent until the delay passes, so nothing jumps when
   * it appears. With `hold` the outro waits out whatever is left of the
   * 400 ms, then leaves in the 70 ms exit; without it the skeleton leaves at once.
   */
  let shown = $state(false);
  let gate: SkeletonGate | null = null;

  $effect(() => {
    gate = startSkeletonGate(() => {
      shown = true;
    });
    return () => gate?.cancel();
  });

  function holdThenLeave(_node: Element) {
    // Leaving at once is the default: a skeleton still in flow while its
    // replacement mounts would split the box between them (the content lands
    // a skeleton lower for the hold). Only a caller that opts in with `hold`
    // keeps it for the 400 ms minimum.
    if (!hold) return { duration: 0 };
    return {
      delay: gate?.holdRemainingMs() ?? 0,
      duration: motionExitMs("fast"),
      css: (t: number) => `opacity: ${t}`,
    };
  }
</script>

<div
  class="flex flex-col gap-2 w-full {shown ? '' : 'opacity-0'} {extraClass}"
  role="status"
  aria-label="Loading content"
  aria-busy="true"
  data-skeleton-shown={shown}
  out:holdThenLeave|global
>
  {#each items as i (i)}
    {#if variant === "circle"}
      <div
        class="rounded-full bg-surfaceHover/80 border border-edgeSoft shrink-0 {animated ? 'animate-pulse' : ''}"
        style="width: {width || '2rem'}; height: {height || '2rem'};"
      ></div>
    {:else if variant === "rect"}
      <div
        class="rounded-xl bg-surfaceHover/80 border border-edgeSoft w-full {animated ? 'animate-pulse' : ''}"
        style="width: {width || '100%'}; height: {height || '4rem'};"
      ></div>
    {:else if variant === "card"}
      <div
        class="rounded-2xl bg-surface border border-edge p-4 space-y-2.5 w-full shadow-card {animated ? 'animate-pulse' : ''}"
      >
        <div class="h-3.5 bg-surfaceHover/90 rounded-full w-1/3"></div>
        <div class="h-2.5 bg-surfaceHover/70 rounded-full w-3/4"></div>
        <div class="h-2 bg-surfaceHover/50 rounded-full w-1/2"></div>
      </div>
    {:else if variant === "tree-row"}
      <div
        class="flex items-center gap-2.5 py-1 px-2 w-full {animated ? 'animate-pulse' : ''}"
      >
        <div class="w-3.5 h-3.5 rounded bg-surfaceHover/80 shrink-0"></div>
        <div
          class="h-3 bg-surfaceHover/80 rounded-full"
          style="width: {i % 3 === 0 ? '60%' : i % 2 === 0 ? '45%' : '75%'};"
        ></div>
      </div>
    {:else if variant === "code-lines"}
      <div
        class="flex items-center gap-3 py-0.5 px-3 w-full font-mono {animated ? 'animate-pulse' : ''}"
      >
        <div class="w-8 h-2.5 bg-surfaceHover/40 rounded shrink-0"></div>
        <div
          class="h-2.5 bg-surfaceHover/70 rounded-full"
          style="width: {i % 4 === 0 ? '70%' : i % 3 === 0 ? '40%' : i % 2 === 0 ? '85%' : '55%'};"
        ></div>
      </div>
    {:else if variant === "graph-rows"}
      <div
        class="flex items-center gap-3 py-1.5 px-3 w-full {animated ? 'animate-pulse' : ''}"
      >
        <div class="w-6 flex items-center justify-center shrink-0">
          <div class="w-2.5 h-2.5 rounded-full bg-surfaceHover/90 border border-edgeSoft"></div>
        </div>
        {#if i % 3 === 0}
          <div class="h-4 w-14 rounded-full bg-surfaceHover/80 border border-edgeSoft shrink-0"></div>
        {/if}
        <div
          class="h-3 rounded-full bg-surfaceHover/80"
          style="width: {i % 4 === 0 ? '48%' : i % 3 === 0 ? '62%' : i % 2 === 0 ? '38%' : '54%'};"
        ></div>
        <div class="ml-auto flex items-center gap-2 shrink-0">
          <div class="h-2.5 w-14 rounded-full bg-surfaceHover/50"></div>
          <div class="h-2.5 w-10 rounded-full bg-surfaceHover/40"></div>
        </div>
      </div>
    {:else if variant === "diff-hunks"}
      <div class="flex flex-col gap-1.5 w-full {animated ? 'animate-pulse' : ''}">
        {#if i === 0}
          <div class="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-surfaceHover/60 border border-edgeSoft">
            <div class="h-2.5 w-28 rounded-full bg-surfaceHover/90"></div>
            <div class="h-2.5 w-12 rounded-full bg-surfaceHover/60 ml-auto"></div>
          </div>
        {/if}
        <div class="flex items-center gap-3 py-0.5 px-3 w-full font-mono">
          <div class="w-7 h-2.5 bg-surfaceHover/40 rounded shrink-0"></div>
          <div class="w-7 h-2.5 bg-surfaceHover/40 rounded shrink-0"></div>
          <div
            class="h-2.5 bg-surfaceHover/75 rounded-full"
            style="width: {i % 4 === 0 ? '72%' : i % 3 === 0 ? '42%' : i % 2 === 0 ? '84%' : '58%'};"
          ></div>
        </div>
      </div>
    {:else if variant === "terminal-replay"}
      {#if i === 0}
        <div class="flex flex-col gap-2 w-full">
          <div class="h-0.5 w-full overflow-hidden rounded-full bg-surfaceHover/50">
            <div class="h-full w-1/3 rounded-full bg-accent/70 {animated ? 'gp-indeterminate' : ''}"></div>
          </div>
          <div class="flex items-center gap-2 px-3 py-1 text-xs text-textMuted">
            <div class="w-2 h-2 rounded-full bg-accent/70 {animated ? 'animate-pulse' : ''}"></div>
            <span>Reattaching terminal…</span>
          </div>
        </div>
      {/if}
    {:else if variant === "pane-shell"}
      {#if i === 0}
        <div class="flex flex-col gap-3 p-4 w-full {animated ? 'animate-pulse' : ''}">
          <div class="flex items-center gap-3">
            <div class="h-4 w-36 rounded-full bg-surfaceHover/90"></div>
            <div class="h-4 w-20 rounded-full bg-surfaceHover/60 ml-auto"></div>
          </div>
          <div class="grid grid-cols-1 gap-2.5">
            <div class="rounded-xl bg-surface border border-edgeSoft p-3.5 space-y-2">
              <div class="h-3 w-1/3 rounded-full bg-surfaceHover/85"></div>
              <div class="h-2.5 w-2/3 rounded-full bg-surfaceHover/60"></div>
            </div>
            <div class="rounded-xl bg-surface border border-edgeSoft p-3.5 space-y-2">
              <div class="h-3 w-2/5 rounded-full bg-surfaceHover/85"></div>
              <div class="h-2.5 w-3/4 rounded-full bg-surfaceHover/60"></div>
            </div>
          </div>
        </div>
      {/if}
    {:else}
      <!-- Default Text Variant -->
      <div
        class="rounded-full bg-surfaceHover/80 border border-edgeSoft {animated ? 'animate-pulse' : ''}"
        style="width: {width || (i === items.length - 1 && items.length > 1 ? '65%' : '100%')}; height: {height || '0.75rem'};"
      ></div>
    {/if}
  {/each}
</div>
