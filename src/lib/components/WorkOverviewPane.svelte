<script lang="ts">
  import { tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { conflictSessions } from "../diff/conflictSession";
  import { WORK_CONFLICTS_ID, conflictsFocusRequest, takeConflictsFocus } from "../ui/workConflicts";
  import { harnessStore } from "../stores/harnessStore";
  import { manviFocusHint } from "../ui/manviFocus";
  import { openManviFocus } from "../ui/openManviFocus";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import WorkView from "./WorkView.svelte";

  /**
   * Work → Overview: the workspace, and finishing the merges that stopped.
   *
   * Resolve used to be its own section, then half of Policy. Agents finish most
   * parked merges, so nearly every visit found an empty state, and it cost a
   * tab for the rare case that needed it. Here the conflict editor leads the
   * pane only while it has something to show, and Overview follows it.
   *
   * The editor is not gated on the conflict count alone. A parked operation
   * with every file resolved still needs the editor's staged review and its
   * continue/abort banner. An operation probe that failed must be shown, not
   * hidden. Those three take the editor at full height.
   *
   * Drafts retained from an earlier operation are different: they never
   * expire, so one stale draft must not pin a full-height editor over Overview
   * for good. They get a one-line row that opens the editor on request,
   * which keeps their only door without taking the pane.
   *
   * Overview is one instance outside every branch. A merge that parks or
   * finishes while this pane is open, often an agent's doing, must not
   * rebuild it under the reader and lose its scroll, filter and inputs.
   *
   * Every read of the repository here goes through a field store
   * (`conflictedCount`, `statuses`, `currentPath`), which notifies only when
   * its own value changes. Reading `$repoStore` would re-run all of this on
   * every status publish.
   */
  let { loadConflict }: { loadConflict: ViewLoader } = $props();

  const { conflictedCount, statuses } = repoStore;
  const conflicted = $derived($conflictedCount > 0);
  const parked = $derived($repoStore.operation.operation !== null);
  const probeFailed = $derived($repoStore.operation.probeFailed);
  const live = $derived(conflicted || parked || probeFailed);
  // Retained drafts are not reactive on their own; reading the statuses and
  // the operation state re-runs this whenever the repository's status or
  // operation moves, which is when a draft can appear or stop mattering.
  const retainedDrafts = $derived.by(() => {
    void $statuses;
    void parked;
    void probeFailed;
    const path = $currentRepoPath;
    return path ? conflictSessions.summaries(path).length : 0;
  });
  let reviewingDrafts = $state(false);
  const showEditor = $derived(live || (reviewingDrafts && retainedDrafts > 0));
  // Conflicts outrank a blocked verdict (viewAttention.ts): while the editor
  // leads the page, the notice would only push it down.
  const blockedVerdict = $derived(!showEditor && $harnessStore.lastVerdict?.status === "blocked");

  // A door to the conflicts (status bar, tray, merge dialog, palette, a row)
  // asks for the editor; it is taken here, on mount or while mounted, and
  // cleared even when nothing is parked any more, so it cannot fire on a
  // later visit. Scroll and focus are instant, like every other deep link.
  $effect(() => {
    if (!$conflictsFocusRequest) return;
    untrack(() => {
      takeConflictsFocus();
      if (!showEditor) return;
      void tick().then(() => {
        const section = document.getElementById(WORK_CONFLICTS_ID);
        section?.scrollIntoView({ block: "start" });
        section?.focus({ preventScroll: true });
      });
    });
  });
</script>

<div
  class="flex-1 min-h-0 flex flex-col {showEditor ? 'overflow-y-auto' : ''}"
  data-overview-layout={showEditor ? "with-conflicts" : "overview"}
>
  {#if showEditor}
    <!-- One scroller for the pane. The editor keeps the full pane height it
         had as its own section, so resolving is unchanged; Overview follows. -->
    <section id={WORK_CONFLICTS_ID} tabindex="-1" aria-label="Conflicts" class="work-conflicts shrink-0 flex flex-col">
      {#if !live}
        <div class="flex items-center gap-2 px-4 py-1.5 bg-surface/40 text-meta text-textMuted">
          <span>Retained conflict drafts</span>
          <button type="button" class="gp-btn ml-auto" onclick={() => (reviewingDrafts = false)}>
            Hide
          </button>
        </div>
      {/if}
      <LazyView load={loadConflict} name="the conflict editor" />
    </section>
  {:else if blockedVerdict}
    <!-- The attention dot on Overview says "Manvi blocked the last action";
         the why is in Policy activity, a collapsed disclosure below the rows.
         One line and one door, only while that verdict is the latest. -->
    <div
      class="shrink-0 flex items-center gap-2 px-4 py-1.5 bg-surface/40 text-meta text-textMuted"
      data-testid="work-blocked-verdict"
      role="status"
    >
      <span>Manvi blocked the last action.</span>
      <button type="button" class="gp-btn ml-auto" title={manviFocusHint("activity")} onclick={() => openManviFocus("activity")}>
        Show activity
      </button>
    </div>
  {/if}
  {#if !showEditor && retainedDrafts > 0}
    <div
      class="shrink-0 flex items-center gap-2 px-4 py-1.5 bg-surface/40 text-meta text-textMuted"
      data-testid="work-retained-drafts"
    >
      <span>
        {retainedDrafts} conflict draft{retainedDrafts === 1 ? "" : "s"} kept from an earlier merge or rebase.
      </span>
      <button type="button" class="gp-btn ml-auto" onclick={() => (reviewingDrafts = true)}>
        Review drafts
      </button>
    </div>
  {/if}
  <section
    aria-label="Overview"
    class="flex flex-col {showEditor ? 'shrink-0 bg-surface/20' : 'flex-1 min-h-0'}"
  >
    <WorkView />
  </section>
</div>

<style>
  /* A definite height: the editor's root is height:100% with overflow
     hidden, so an auto-height parent would collapse it to its heading. */
  .work-conflicts {
    height: 100%;
    min-height: 28rem;
  }
</style>
