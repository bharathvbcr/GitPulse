<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { conflictSessions } from "../diff/conflictSession";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";

  /**
   * Work → Policy: may this merge, and finishing the merges that stopped.
   *
   * Resolve used to be its own section. Agents finish most parked merges, so
   * nearly every visit found an empty state, and it cost a tab for the rare
   * case that needed it. Here the conflict editor leads the pane only while
   * it has something to show, and MANVI follows it.
   *
   * The editor is not gated on the conflict count alone. A parked operation
   * with every file resolved still needs the editor's staged review and its
   * continue/abort banner. An operation probe that failed must be shown, not
   * hidden. Those three take the editor at full height.
   *
   * Drafts retained from an earlier operation are different: they never
   * expire, so one stale draft must not pin a full-height editor over MANVI
   * for good. They get a one-line row that opens the editor on request,
   * which keeps their only door without taking the pane.
   *
   * MANVI is one instance outside every branch. A merge that parks or
   * finishes while this pane is open, often an agent's doing, must not
   * rebuild the panel under the reader and lose its scroll and inputs.
   */
  let {
    loadConflict,
    loadManvi,
  }: {
    loadConflict: ViewLoader;
    loadManvi: ViewLoader;
  } = $props();

  const conflicted = $derived($repoStore.statuses.some((status) => status.is_conflicted));
  const parked = $derived($repoStore.operation.operation !== null);
  const probeFailed = $derived($repoStore.operation.probeFailed);
  const live = $derived(conflicted || parked || probeFailed);
  // Retained drafts are not reactive on their own; reading the store re-runs
  // this whenever the repository's status or operation moves, which is when
  // a draft can appear or stop mattering.
  const retainedDrafts = $derived.by(() => {
    void $repoStore.statuses;
    void $repoStore.operation;
    const path = $repoStore.currentPath;
    return path ? conflictSessions.summaries(path).length : 0;
  });
  let reviewingDrafts = $state(false);
  const showEditor = $derived(live || (reviewingDrafts && retainedDrafts > 0));
</script>

<div
  class="flex-1 min-h-0 flex flex-col {showEditor ? 'overflow-y-auto' : ''}"
  data-policy-layout={showEditor ? "with-conflicts" : "policy"}
>
  {#if showEditor}
    <!-- One scroller for the pane. The editor keeps the full pane height it
         had as its own section, so resolving is unchanged; MANVI follows. -->
    <section aria-label="Conflicts" class="policy-conflicts shrink-0 flex flex-col">
      {#if !live}
        <div class="flex items-center gap-2 px-4 py-1.5 text-[11px] text-textMuted border-b border-border/40">
          <span>Retained conflict drafts</span>
          <button type="button" class="gp-btn ml-auto" onclick={() => (reviewingDrafts = false)}>
            Hide
          </button>
        </div>
      {/if}
      <LazyView load={loadConflict} name="the conflict editor" />
    </section>
  {:else if retainedDrafts > 0}
    <div
      class="shrink-0 flex items-center gap-2 px-4 py-1.5 text-[11px] text-textMuted border-b border-border/40"
      data-testid="policy-retained-drafts"
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
    aria-label="MANVI"
    class="flex flex-col {showEditor ? 'shrink-0 border-t border-border/40' : 'flex-1 min-h-0'}"
  >
    <LazyView load={loadManvi} name="Manvi ops" />
  </section>
</div>

<style>
  /* A definite height: the editor's root is height:100% with overflow
     hidden, so an auto-height parent would collapse it to its heading. */
  .policy-conflicts {
    height: 100%;
    min-height: 28rem;
  }
</style>
