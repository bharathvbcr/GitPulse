<script lang="ts">
  import { Image } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";

  let {
    filePath = "image",
    oldSrc = null,
    newSrc = null,
  }: {
    filePath?: string;
    oldSrc?: string | null;
    newSrc?: string | null;
  } = $props();

  let mode: "2up" | "swipe" | "onion" = $state("swipe");
  let swipePosition = $state(50);
  let opacity = $state(50);
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans select-none overflow-hidden">
  <div class="px-3 py-1.5 border-b border-border/60 bg-surface/60 flex items-center justify-between">
    <div class="flex items-center gap-2 min-w-0">
      <Image size={15} class="text-accent shrink-0" />
      <span class="font-medium text-textPrimary truncate">{filePath}</span>
    </div>
    <div class="gp-segmented">
      <button onclick={() => (mode = "2up")} data-active={mode === "2up" ? "true" : "false"} class="gp-seg-btn !py-0.5 !text-[11px]">2-Up</button>
      <button onclick={() => (mode = "swipe")} data-active={mode === "swipe" ? "true" : "false"} class="gp-seg-btn !py-0.5 !text-[11px]">Swipe</button>
      <button onclick={() => (mode = "onion")} data-active={mode === "onion" ? "true" : "false"} class="gp-seg-btn !py-0.5 !text-[11px]">Onion Skin</button>
    </div>
  </div>

  <div class="flex-1 flex items-center justify-center p-8 bg-background relative overflow-hidden">
    {#if !oldSrc && !newSrc}
      <EmptyState icon={Image} title="No image preview" hint="Nothing to compare for this path." />
    {:else if mode === "2up"}
      <div class="flex gap-8 items-center">
        <div class="flex flex-col items-center gap-2">
          <span class="text-textMuted text-[11px]">Before (Old)</span>
          <div class="w-64 h-64 bg-surface border border-border/70 rounded-2xl shadow-card flex items-center justify-center overflow-hidden">
            {#if oldSrc}
              <img src={oldSrc} alt="old" class="max-w-full max-h-full object-contain" />
            {:else}
              <span class="text-textMuted text-xs">Missing</span>
            {/if}
          </div>
        </div>
        <div class="flex flex-col items-center gap-2">
          <span class="text-textMuted text-[11px]">After (New)</span>
          <div class="w-64 h-64 bg-surface border border-accent/40 rounded-2xl shadow-card flex items-center justify-center overflow-hidden">
            {#if newSrc}
              <img src={newSrc} alt="new" class="max-w-full max-h-full object-contain" />
            {:else}
              <span class="text-textMuted text-xs">Missing</span>
            {/if}
          </div>
        </div>
      </div>
    {:else if mode === "swipe"}
      <div class="flex flex-col items-center gap-4 w-[28rem]">
        <div class="w-80 h-80 bg-surface border border-border/70 rounded-2xl shadow-card relative overflow-hidden">
          {#if newSrc}
            <img src={newSrc} alt="new" class="absolute inset-0 w-full h-full object-contain" />
          {/if}
          <div class="absolute inset-0 overflow-hidden border-r-2 border-accent" style="width: {swipePosition}%;">
            {#if oldSrc}
              <img src={oldSrc} alt="old" class="absolute inset-0 w-80 h-80 object-contain" />
            {/if}
          </div>
        </div>
        <input type="range" min="0" max="100" bind:value={swipePosition} class="w-full accent-accent" />
      </div>
    {:else}
      <div class="flex flex-col items-center gap-4 w-[28rem]">
        <div class="w-80 h-80 bg-surface border border-border/70 rounded-2xl shadow-card relative overflow-hidden">
          {#if oldSrc}
            <img src={oldSrc} alt="old" class="absolute inset-0 w-full h-full object-contain" />
          {/if}
          {#if newSrc}
            <img src={newSrc} alt="new" class="absolute inset-0 w-full h-full object-contain" style="opacity: {opacity / 100};" />
          {/if}
        </div>
        <input type="range" min="0" max="100" bind:value={opacity} class="w-full accent-accent" />
      </div>
    {/if}
  </div>
</div>
