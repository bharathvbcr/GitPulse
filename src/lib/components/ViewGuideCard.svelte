<script lang="ts">
  import type { DestinationGuide } from "../views/viewGuide";
  import ViewGuideArt from "./ViewGuideArt.svelte";

  /**
   * The rich destination tooltip: schematic, title, shortcut, and the
   * section map. The global Tooltip mounts this when the anchor carries
   * `data-tip-guide` instead of a one-line `title`.
   */

  let { guide }: { guide: DestinationGuide } = $props();
</script>

<div class="w-72 overflow-hidden text-left">
  <div class="bg-background/80 px-2 pt-2">
    <ViewGuideArt view={guide.view} section={guide.section} />
  </div>
  <div class="px-3 py-2.5">
    <div class="flex items-center justify-between gap-2">
      <div class="text-xs font-semibold leading-tight text-textPrimary">{guide.title}</div>
      {#if guide.shortcut}
        <kbd class="gp-keycap shrink-0 text-[10px]">{guide.shortcut}</kbd>
      {/if}
    </div>
    <p class="mt-1 text-[11px] leading-relaxed text-textMuted">{guide.summary}</p>
    {#if guide.chips.length > 0}
      <div class="mt-2 flex flex-wrap gap-1">
        {#each guide.chips as chip (chip.id)}
          <span
            class="rounded-full px-1.5 py-0.5 text-[9px] font-medium {chip.active
              ? 'bg-accent/15 text-accent'
              : 'bg-background text-textMuted'}"
          >
            {chip.label}
          </span>
        {/each}
      </div>
    {/if}
  </div>
</div>
