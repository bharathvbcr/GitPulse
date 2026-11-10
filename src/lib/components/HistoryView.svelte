<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewContextBar from "./ViewContextBar.svelte";
  import ViewSectionPanel from "./ViewSectionPanel.svelte";

  /** Named once: the tab and its panel must agree on the id. */
  const view = "history" as const;
  import FilterBar from "./FilterBar.svelte";
  import CommitTable from "./CommitTable.svelte";
  import GraphNotices from "./GraphNotices.svelte";
  import CommitDetails from "./CommitDetails.svelte";
  import CommitReconciler from "./CommitReconciler.svelte";
  import DiffViewer from "./DiffViewer.svelte";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import type { HistoryList } from "../views/historyList";

  /**
   * The two lists Graph can draw above the commit details. "HEAD moves" is
   * the reflog, which was its own section; the title keeps the word findable.
   */
  const HISTORY_LISTS: readonly { value: HistoryList; label: string; title: string }[] = [
    { value: "commits", label: "Commits", title: "The commit graph: branches, merges and the selected commit" },
    { value: "moves", label: "HEAD moves", title: "Reflog: every movement of HEAD, with recovery points after a reset, checkout or amend" },
  ];

  /**
   * History: everything that answers "what happened to this repository".
   *
   * Graph, Diff and Reflog were three top-level tabs. They were never three
   * destinations — they are three renderings of one subject, and the split
   * was expensive in a way the code admitted: the Diff view had to grow its
   * own commit picker and file rail purely so a user would not have to walk
   * back to Graph for the commit they had just been looking at.
   *
   * The sections share `selectedCommitId`, so switching keeps the subject.
   * Reflog then folded into Graph as its second list (`historyList`): both
   * are lists of commits feeding the same details pane. The commit filter
   * lives in the section bar rather than as a full-width strip, because this
   * is the only view it filters.
   */

  let {
    /** Reflog's loader, declared at module scope by App so it is stable. */
    loadReflog,
    /** Suspects' loader, same contract: stable identity or LazyView remounts. */
    loadSuspects,
  }: {
    loadReflog: ViewLoader;
    loadSuspects: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("history", $repoStore.viewSections));
  const list = $derived($repoStore.historyList);
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewContextBar label="Commit filter">
    {#if section === "graph"}
      <SettingSegment
        ariaLabel="Graph list"
        options={HISTORY_LISTS}
        value={list}
        onselect={(next) => repoStore.setHistoryList(next)}
      />
    {/if}
    <!-- The filter applies to every section here: the graph walk and the
         diff's commit list are drawn from it, and the reflog list narrows
         its loaded entries by the same text (the branch chip cannot apply to
         HEAD's log). -->
    <FilterBar />
  </ViewContextBar>
  <ViewSectionPanel {view} {section}>

    <!-- Sections swap by {#if}, never by {#key}: keying would rebuild the pane
         and replay the entrance fade on every switch, and CommitTable would
         re-hydrate its virtual window from scratch. -->
    {#if section === "diff"}
      <DiffViewer />
    {:else if section === "suspects"}
      <!-- The one section that reads the history backwards: from a symptom to
           the commits that could have caused it. Lazy like the reflog — it
           pulls the code-graph client, which no other History section needs. -->
      <LazyView load={loadSuspects} name="regression suspects" />
    {:else}
      <div class="flex-1 flex flex-col min-h-0">
        <!-- What this graph is NOT drawing, above the graph it is about.
             A repository fact, not a fault: faults go to diagnostics, and
             filing these there too is what turned a crash log into a list of
             true sentences. See `GraphNotes` in src-tauri/src/commands. -->
        {#if list === "moves"}
          <!-- Lazy: the reflog's chunk stays out of the entry bundle until
               someone opens it. -->
          <LazyView load={loadReflog} name="the reflog" />
        {:else}
          <GraphNotices notices={$graphStore.notices} />
          <CommitTable />
        {/if}
        <!-- Under either list: the diff follows the selection whichever one
             is on screen, including when History opens straight onto HEAD's
             moves and the commit table never mounts. -->
        <CommitReconciler />
        <CommitDetails />
      </div>
    {/if}
  </ViewSectionPanel>
</div>
