<script lang="ts">
  let {
    variant = "text",
    count = 1,
    width = "",
    height = "",
    class: extraClass = "",
    animated = true,
  }: {
    variant?: "text" | "rect" | "circle" | "card" | "tree-row" | "code-lines";
    count?: number;
    width?: string;
    height?: string;
    class?: string;
    animated?: boolean;
  } = $props();

  const items = $derived(Array.from({ length: Math.max(1, count) }, (_, i) => i));
</script>

<div
  class="flex flex-col gap-2 w-full {extraClass}"
  role="status"
  aria-label="Loading content"
  aria-busy="true"
>
  {#each items as i (i)}
    {#if variant === "circle"}
      <div
        class="rounded-full bg-surfaceHover/80 border border-border/40 shrink-0 {animated ? 'animate-pulse' : ''}"
        style="width: {width || '2rem'}; height: {height || '2rem'};"
      ></div>
    {:else if variant === "rect"}
      <div
        class="rounded-xl bg-surfaceHover/80 border border-border/40 w-full {animated ? 'animate-pulse' : ''}"
        style="width: {width || '100%'}; height: {height || '4rem'};"
      ></div>
    {:else if variant === "card"}
      <div
        class="rounded-2xl bg-surface border border-border/70 p-4 space-y-2.5 w-full shadow-card {animated ? 'animate-pulse' : ''}"
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
    {:else}
      <!-- Default Text Variant -->
      <div
        class="rounded-full bg-surfaceHover/80 border border-border/40 {animated ? 'animate-pulse' : ''}"
        style="width: {width || (i === items.length - 1 && items.length > 1 ? '65%' : '100%')}; height: {height || '0.75rem'};"
      ></div>
    {/if}
  {/each}
</div>
