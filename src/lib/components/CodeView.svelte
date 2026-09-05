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
   * Files and Blame were two header entries reading one file. Both keyed off
   * `selectedFilePath`, and the split showed in the code: Blame carried its
   * own explorer rail and its own path box so a user would not have to walk
   * back to Files for the file they already had open — the same tell the Diff
   * view gave before History absorbed it. As sections the selection survives
   * the switch, so the editor's Blame button changes lens instead of
   * teleporting, and Blame's second picker could go.
   *
   * Explorer is `FileViewer` unchanged and eager: it is the section Code
   * opens on, so deferring it would only add a round trip. Blame stays lazy —
   * it was already its own chunk and nothing about the merge makes it cheaper
   * to parse at startup.
   */

  let {
    /** Blame's loader, declared at module scope by App so it stays stable. */
    loadBlame,
  }: {
    loadBlame: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("code", $repoStore.viewSections));
  const selected = $derived($repoStore.selectedFilePath);
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="code">
    <!-- The subject both sections share, named once. Blame used to print the
         path into its own input; there is one file here, and it is the file
         the Explorer section has open.

         Explorer draws its own breadcrumb, where each folder is a control that
         reveals it in the tree. Repeating the path here put the same string on
         screen twice, three lines apart, one copy inert — so this now speaks
         only for Blame, which has no breadcrumb of its own. -->
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
    {:else}
      <FileViewer />
    {/if}
  </ViewSectionPanel>
</div>
