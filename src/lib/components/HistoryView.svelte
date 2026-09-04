<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewSectionBar from "./ViewSectionBar.svelte";
  import FilterBar from "./FilterBar.svelte";
  import CommitTable from "./CommitTable.svelte";
  import CommitDetails from "./CommitDetails.svelte";
  import DiffViewer from "./DiffViewer.svelte";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";

  /**
   * History: everything that answers "what happened to this repository".
   *
   * Graph, Diff and Reflog were three top-level tabs. They were never three
   * destinations — they are three renderings of one subject, and the split
   * was expensive in a way the code admitted: the Diff view had to grow its
   * own commit picker and file rail purely so a user would not have to walk
   * back to Graph for the commit they had just been looking at.
   *
   * All three share `selectedCommitId`, so switching sections keeps the
   * subject. The commit filter lives in the section bar rather than as a
   * full-width strip, because this is the only view it filters.
   */

  let {
    /** Reflog's loader, declared at module scope by App so it is stable. */
    loadReflog,
  }: {
    loadReflog: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("history", $repoStore.viewSections));
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="history">
    <!-- The filter applies to every section here: the graph walk, the diff's
         commit list, and the reflog table are all drawn from it. -->
    <FilterBar />
  </ViewSectionBar>

  <!-- Sections swap by {#if}, never by {#key}: keying would rebuild the pane
       and replay the entrance fade on every switch, and CommitTable would
       re-hydrate its virtual window from scratch. -->
  {#if section === "diff"}
    <DiffViewer />
  {:else if section === "reflog"}
    <LazyView load={loadReflog} name="the reflog" />
  {:else}
    <div class="flex-1 flex flex-col min-h-0">
      <CommitTable />
      <CommitDetails />
    </div>
  {/if}
</div>
