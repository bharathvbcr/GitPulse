<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { activeSectionFor } from "../views/viewRegistry";
  import ViewSectionBar from "./ViewSectionBar.svelte";
  import ViewSectionPanel from "./ViewSectionPanel.svelte";

  /** Named once: the tab and its panel must agree on the id. */
  const view = "work" as const;
  import WorkView from "./WorkView.svelte";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";

  /**
   * Work: everything in flight, and the surfaces that act on it.
   *
   * The Overview section is `WorkView` unchanged — deliberately. It carries
   * the app's strictest honesty contracts (an unreadable verdict is never
   * counted as allowed; a source that could not be read is named rather than
   * rendered as empty), and those must not move in the same change that moves
   * the layout. This component only decides which pane is on screen.
   *
   * What the merge actually removes is duplication: the GitHub and MANVI
   * views each issued `cmd_github_context` — four `gh` round trips, up to 45s
   * — to draw overlapping lists of the pull requests, issues, runs and
   * releases Work already joins into its rows.
   */

  let {
    loadConflict,
    loadGitHub,
    loadStack,
    loadManvi,
  }: {
    loadConflict: ViewLoader;
    loadGitHub: ViewLoader;
    loadStack: ViewLoader;
    loadManvi: ViewLoader;
  } = $props();

  const section = $derived(activeSectionFor("work", $repoStore.viewSections));
  const conflictedCount = $derived(
    $repoStore.statuses.filter((s) => s.is_conflicted).length,
  );
</script>

<div class="flex-1 flex flex-col min-h-0">
  <ViewSectionBar view="work">
    <!-- Resolve stopped being a tab, so the count that used to ride its
         header label rides here instead. A parked merge must never be
         quieter than it was: Work is always in the header, its rows sort
         blocked worktrees first, and the status bar keeps its own chip. -->
    {#if conflictedCount > 0}
      <button
        type="button"
        onclick={() => repoStore.setViewSection("work", "resolve")}
        class="ml-auto shrink-0 inline-flex items-center gap-1 px-2 py-0.5 rounded-full border border-rose-500/40 bg-rose-500/10 text-[11px] font-semibold text-rose-600 dark:text-rose-400 hover:bg-rose-500/20 transition-colors"
        title="{conflictedCount} file{conflictedCount === 1 ? '' : 's'} still carry conflict markers"
      >
        {conflictedCount} conflict{conflictedCount === 1 ? "" : "s"}
      </button>
    {/if}
  </ViewSectionBar>
  <ViewSectionPanel {view} {section}>

    {#if section === "resolve"}
      <LazyView load={loadConflict} name="the conflict editor" />
    {:else if section === "remote"}
      <LazyView load={loadGitHub} name="GitHub" />
    {:else if section === "stack"}
      <LazyView load={loadStack} name="the code stack" />
    {:else if section === "policy"}
      <LazyView load={loadManvi} name="Manvi ops" />
    {:else}
      <WorkView />
    {/if}
  </ViewSectionPanel>
</div>
