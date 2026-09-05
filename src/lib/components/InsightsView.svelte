<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewSectionBar from "./ViewSectionBar.svelte";
  import ViewSectionPanel from "./ViewSectionPanel.svelte";

  /** Named once: the tab and its panel must agree on the id. */
  const view = "insights" as const;
  import LazyView, { type ViewLoader } from "./LazyView.svelte";

  /**
   * Insights: what this repository is like, as opposed to what is happening
   * in it.
   *
   * Pulse, Coverage, Health and Storage were four top-level entries, and
   * every one of them is empty until a scan has run — four permanent claims
   * on the header that pay off occasionally. They are also the same shape:
   * an on-demand measurement that must say when it was capped rather than
   * presenting a floor as a total. Gathering them here is what lets that
   * contract have one owner instead of four.
   *
   * Every section stays lazily loaded. A user who opens Insights for the
   * activity heatmap should not pay for the coverage parser, and the entry
   * chunk must not regain any of them.
   */

  let {
    loadPulse,
    loadCoverage,
    loadHealth,
    loadStorage,
  }: {
    loadPulse: ViewLoader;
    loadCoverage: ViewLoader;
    loadHealth: ViewLoader;
    loadStorage: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("insights", $repoStore.viewSections));
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="insights" />
  <ViewSectionPanel {view} {section}>

    {#if section === "coverage"}
      <LazyView load={loadCoverage} name="Coverage" />
    {:else if section === "health"}
      <LazyView load={loadHealth} name="Health" />
    {:else if section === "storage"}
      <LazyView load={loadStorage} name="Storage" />
    {:else}
      <LazyView load={loadPulse} name="Pulse" />
    {/if}
  </ViewSectionPanel>
</div>
