<script lang="ts">
  import BlameViewer from "../src/lib/components/BlameViewer.svelte";
  import { densityStore } from "../src/lib/stores/densityStore";
  import { interfaceStore } from "../src/lib/stores/interfaceStore";
  import { themeStore } from "../src/lib/stores/themeStore";

  let { setMode }: { setMode: (mode: string) => void } = $props();

  /** 0 = full width, 1 = a split pane, 2 = a narrow window. */
  let narrow = $state(0);

  /**
   * Blame under a real layout.
   *
   * The pane is given a bounded height on purpose: the timeline strip, the
   * virtual list and the explorer share one column, and a page that lets the
   * list grow forever cannot show that the strip stayed inside its own row.
   */
  const MODES = ["mixed", "fresh", "uncommitted", "skewed", "long", "empty", "error"];
</script>

<main
  class="flex flex-col h-screen gap-2 p-3 bg-background text-textPrimary"
  style:max-width={narrow === 1 ? "780px" : narrow === 2 ? "560px" : "100%"}
>
  <div class="flex flex-wrap gap-2 shrink-0">
    {#each MODES as mode (mode)}
      <button class="gp-btn" onclick={() => setMode(mode)}>Blame: {mode}</button>
    {/each}
    <button class="gp-btn" onclick={() => densityStore.toggle()}>Toggle density</button>
    <button class="gp-btn" onclick={() => (narrow = (narrow + 1) % 3)}>Toggle narrow layout</button>
    <button
      class="gp-btn"
      onclick={() =>
        interfaceStore.setTimestampStyle(
          $interfaceStore.timestampStyle === "relative" ? "absolute" : "relative",
        )}
    >
      Toggle timestamps
    </button>
    <button class="gp-btn" onclick={() => themeStore.setTheme($themeStore === "dark" ? "light" : "dark")}>
      Toggle theme
    </button>
  </div>
  <div class="min-h-0 flex-1 flex flex-col border border-border rounded overflow-hidden">
    <BlameViewer />
  </div>
</main>
