<script lang="ts">
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewContextBar from "./ViewContextBar.svelte";
  import ViewSectionPanel from "./ViewSectionPanel.svelte";

  /** Named once: the tab and its panel must agree on the id. */
  const view = "history" as const;
  import { filterStore } from "../stores/filterStore";
  import {
    providePageSearchScope,
    registerPageSearch,
    registerStandingFilter,
  } from "../search/pageSearch.svelte";
  import CommitTable from "./CommitTable.svelte";
  import GraphNotices from "./GraphNotices.svelte";
  import CommitDetails from "./CommitDetails.svelte";
  import CommitReconciler from "./CommitReconciler.svelte";
  import DiffViewer from "./DiffViewer.svelte";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import SuspectsQueryBar from "./SuspectsQueryBar.svelte";
  import { suspectsQuery } from "../codeintel/suspectsQuery";
  // The lists Graph can draw above the commit details. "HEAD moves" is the
  // reflog and "Suspects" the regression ranking, each once its own section;
  // their titles keep those words findable.
  import { HISTORY_LISTS } from "../views/historyList";

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
   * Reflog then folded into Graph as its second list and Suspects as its
   * third (`historyList`): all three are lists of commits feeding the same
   * details pane, selecting through one path (`selectHistoryCommit`). The
   * context bar carries the commit filter for the first two and the suspects
   * question for the third, because a ranking is not a list text narrows.
   */

  let {
    /** Reflog's loader, declared at module scope by App so it is stable. */
    loadReflog,
    /** The Suspects list's loader, same contract: stable identity or LazyView remounts. */
    loadSuspects,
  }: {
    loadReflog: ViewLoader;
    loadSuspects: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("history", $repoStore.viewSections));
  const list = $derived($repoStore.historyList);
  const asksSuspects = $derived(section === "graph" && list === "suspects");

  // The suspects answer belongs to one repository; a switch drops it before
  // any list or chip can draw it under another repository's name.
  $effect(() => {
    suspectsQuery.bindRepo($currentRepoPath);
  });

  // The view bar's search field. On Graph's Commits and HEAD moves lists it is
  // the commit filter; the Diff section's viewer (mounted inside this scope)
  // makes it a find instead. The Suspects list asks its question in the
  // context bar (SuspectsQueryBar), so there the field has nothing of its own
  // and searches the repository; ⌘F goes to the symptom (findChordTarget).
  providePageSearchScope("history:diff");
  $effect(() => {
    if (asksSuspects) return;
    return registerPageSearch("history:graph", {
      kind: "filter",
      label: "Filter commits",
      placeholder: "Filter commits by message, author or path (author:alice, feat:)…",
      query: () => $filterStore.searchQuery,
      setQuery: (next) => filterStore.setSearch(next),
      chip: () =>
        $filterStore.selectedBranch
          ? { label: $filterStore.selectedBranch, title: "Clear branch filter", clear: () => filterStore.selectBranch(null) }
          : null,
    });
  });
  // The filter narrows every section's commits: the graph walk, the diff's
  // commit list and the reflog's loaded entries (the branch cannot apply to
  // HEAD's log). Where the field is a find, the applied filter stays in view.
  // It does not narrow the Suspects ranking, so no chip claims it does there.
  $effect(() =>
    registerStandingFilter("history", () => {
      if (asksSuspects) return null;
      const { searchQuery, selectedBranch } = $filterStore;
      if (!searchQuery && !selectedBranch) return null;
      const parts = [searchQuery && `“${searchQuery}”`, selectedBranch].filter(Boolean).join(" · ");
      return { label: `Commits: ${parts}`, title: "The commit filter applied in Graph. Clear it", clear: () => filterStore.clear() };
    }),
  );
</script>

<div class="flex-1 flex flex-col min-h-0">
  {#if section === "graph"}
    <!-- Graph's three lists. The commit filter that used to sit beside them is
         the view bar's search field now; the Suspects list's question stays
         here, because a ranking is one answer, not a list text narrows. -->
    <ViewContextBar label="Graph list">
      <SettingSegment
        ariaLabel="Graph list"
        options={HISTORY_LISTS}
        value={list}
        onselect={(next) => repoStore.setHistoryList(next)}
      />
      {#if asksSuspects}
        <SuspectsQueryBar />
      {/if}
    </ViewContextBar>
  {/if}
  <ViewSectionPanel {view} {section}>

    <!-- Sections swap by {#if}, never by {#key}: keying would rebuild the pane
         and replay the entrance fade on every switch, and CommitTable would
         re-hydrate its virtual window from scratch. -->
    {#if section === "diff"}
      <DiffViewer />
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
        {:else if list === "suspects"}
          <!-- The list that reads the history backwards: from a symptom to
               the commits that could have caused it. Lazy like the reflog. -->
          <LazyView load={loadSuspects} name="regression suspects" />
        {:else}
          <GraphNotices notices={$graphStore.notices} />
          <CommitTable />
        {/if}
        <!-- Under every list: the diff follows the selection whichever one
             is on screen, including when History opens straight onto HEAD's
             moves or Suspects and the commit table never mounts. -->
        <CommitReconciler />
        <CommitDetails />
      </div>
    {/if}
  </ViewSectionPanel>
</div>
