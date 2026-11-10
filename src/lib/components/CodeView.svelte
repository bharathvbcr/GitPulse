<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewSectionBar from "./ViewSectionBar.svelte";
  import ViewSectionPanel from "./ViewSectionPanel.svelte";

  /** Named once: the tab and its panel must agree on the id. */
  const view = "code" as const;
  import FileViewer from "./FileViewer.svelte";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";

  /**
   * Code: the working tree, under whichever lens the question needs.
   *
   * Explorer is the file: its rail toggles between the tree and content
   * search, and the open file between the editor and its blame — both used
   * to be sections here. Map is the structural navigator over the consumer
   * repo map — subsystems, entry points, area tests — and does not need a
   * file selected.
   *
   * Explorer is eager (the default section). Map stays lazy.
   */

  let {
    /** Map navigator loader, declared at module scope by App so it stays stable. */
    loadMap,
  }: {
    loadMap: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("code", $repoStore.viewSections));
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="code" />
  <ViewSectionPanel {view} {section}>

    <!-- Sections swap by {#if}, never by {#key}: keying would rebuild the pane
         and replay the entrance fade, and FileViewer would lose its open tabs
         and unsaved drafts on every switch. -->
    {#if section === "map"}
      <LazyView load={loadMap} name="Map" />
    {:else}
      <FileViewer />
    {/if}
  </ViewSectionPanel>
</div>
