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
   * Explorer and Blame share `selectedFilePath`. Map is the structural
   * navigator over the consumer repo map — subsystems, entry points, area
   * tests — and does not need a file selected.
   *
   * Explorer is eager (the default section). Blame and Map stay lazy.
   */

  let {
    /** Blame's loader, declared at module scope by App so it stays stable. */
    loadBlame,
    /** Map navigator loader — same stability rule as Blame. */
    loadMap,
  }: {
    loadBlame: ViewLoader;
    loadMap: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("code", $repoStore.viewSections));
  const selected = $derived($repoStore.selectedFilePath);
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="code">
    <!-- The subject Blame shares with Explorer. Map has its own subject
         (subsystems), so the path chip stays Blame-only. -->
    {#if selected && section === "blame"}
      <span
        class="ml-auto shrink-0 max-w-[40ch] truncate font-mono text-[11px] text-textMuted"
        title={selected}
      >
        {selected}
      </span>
    {/if}
  </ViewSectionBar>
  <ViewSectionPanel {view} {section}>

    <!-- Sections swap by {#if}, never by {#key}: keying would rebuild the pane
         and replay the entrance fade, and FileViewer would lose its open tabs
         and unsaved drafts on every switch. -->
    {#if section === "blame"}
      <LazyView load={loadBlame} name="blame" />
    {:else if section === "map"}
      <LazyView load={loadMap} name="Map" />
    {:else}
      <FileViewer />
    {/if}
  </ViewSectionPanel>
</div>
