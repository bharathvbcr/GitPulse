<script lang="ts">
  import ScrollCue from "../src/lib/components/ScrollCue.svelte";
  import type { OverflowHint } from "../src/lib/dom/overflowHint";

  /**
   * Three horizontal scrollers and one vertical one, each a shape a real
   * host uses: a bare strip of items (the repository tabs), a second strip to
   * retarget onto, and a padded `min-w-max` toolbar row (the code viewer's
   * action bars). The page fills them with items itself; this component
   * only owns which element the cue watches and whether it is mounted.
   */
  type Which = "strip" | "other" | "toolbar" | "none";

  let strip = $state<HTMLDivElement>();
  let other = $state<HTMLDivElement>();
  let toolbar = $state<HTMLDivElement>();
  let column = $state<HTMLDivElement>();
  let which = $state<Which>("strip");
  let mounted = $state(true);
  let hint = $state<OverflowHint | undefined>(undefined);

  const target = $derived(
    which === "strip" ? strip : which === "other" ? other : which === "toolbar" ? toolbar : undefined,
  );

  export function retarget(next: Which) {
    which = next;
  }

  export function setMounted(next: boolean) {
    mounted = next;
  }

  export function setHint(next: OverflowHint | undefined) {
    hint = next;
  }

  export function elements() {
    return { strip, other, toolbar, column };
  }
</script>

<div data-cue-host="x" class="relative bg-surface" style="width: 400px; height: 120px;">
  <div bind:this={strip} data-scroller="strip" class="gp-header-scroll flex items-center gap-1" style="height: 40px;"></div>
  <div bind:this={other} data-scroller="other" class="gp-header-scroll flex items-center gap-1" style="height: 40px;"></div>
  <div bind:this={toolbar} data-scroller="toolbar" class="gp-header-scroll" style="height: 40px;">
    <div data-row class="flex items-center justify-between gap-2 px-3 py-1.5 min-w-max w-full"></div>
  </div>
  {#if mounted}
    <ScrollCue {target} axis="x" {hint} />
  {/if}
</div>

<div data-cue-host="y" class="relative bg-background" style="width: 200px; height: 160px; margin-top: 8px;">
  <div bind:this={column} data-scroller="column" class="h-full overflow-y-auto"></div>
  <ScrollCue target={column} axis="y" />
</div>
