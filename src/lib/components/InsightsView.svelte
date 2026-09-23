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
   * Pulse, Coverage, Health, Secrets and Storage are on-demand measurements
   * that must say when they were capped rather than presenting a floor as a
   * total. Gathering them here is what lets that contract have one owner.
   *
   * Every section stays lazily loaded. A user who opens Insights for the
   * activity heatmap should not pay for the coverage parser, and the entry
   * chunk must not regain any of them.
   */

  let {
    loadPulse,
    loadCoverage,
    loadHealth,
    loadSecrets,
    loadStorage,
  }: {
    loadPulse: ViewLoader;
    loadCoverage: ViewLoader;
    loadHealth: ViewLoader;
    loadSecrets: ViewLoader;
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
    {:else if section === "secrets"}
      <LazyView load={loadSecrets} name="Secrets" />
    {:else if section === "storage"}
      <LazyView load={loadStorage} name="Storage" />
    {:else}
      <LazyView load={loadPulse} name="Pulse" />
    {/if}
  </ViewSectionPanel>
</div>
