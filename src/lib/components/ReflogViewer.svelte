<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the reflog view renders the
  // last-known table instantly; the fetch then refreshes it in place.
  const reflogCache = createRepoPanelCache<ReflogEntry[]>();
</script>

<script lang="ts">
  import type { ReflogEntry } from "../branches/types";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { filterReflogEntries } from "../branches/reflogFilter";
  import { invoke } from "../ipc/invoke";
  import { History } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatDate, plural, shortHash } from "../format";
  import { reportPanelError } from "../diagnostics/report";
  import { suspectsQuery, suspectMarks } from "../codeintel/suspectsQuery";

  const REFLOG_ENTRY_LIMIT = 200;

  let entries: ReflogEntry[] = $state([]);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;
  /** The entry last selected here; several entries can name one commit. */
  let selectedSelector = $state<string | null>(null);

  // History's commit filter sits above this list, as it does above the graph.
  // It narrows the entries already loaded (up to REFLOG_ENTRY_LIMIT); the
  // branch chip cannot apply, because this is HEAD's log, not a branch's.
  let shown = $derived(filterReflogEntries(entries, $filterStore.searchQuery));
  let selectedCommitId = $derived($graphStore.selectedCommit?.id ?? null);
  // Commits the Suspects list ranked, marked here too: a HEAD move that is
  // also a suspect is the recovery point worth reading first.
  let marks = $derived(suspectMarks($suspectsQuery, $currentRepoPath));

  async function load() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const next = await invoke<ReflogEntry[]>("cmd_get_reflog", {
        repoPath: repo,
        maxEntries: REFLOG_ENTRY_LIMIT,
      });
      if (!guard.isLive()) return;
      entries = next;
      reflogCache.set(repo, next);
    } catch (err) {
      if (!guard.isLive()) return;
      errorMsg = reportPanelError("reflog", err);
      entries = [];
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  /**
   * Shows the entry's commit in the details pane below this list, in place,
   * through the path every Graph list selects by (`selectHistoryCommit`).
   *
   * It used to hand off to `inspectCommitInHistory`, which selected the
   * commit on Graph while this pane, then its own section, stayed on screen:
   * the click changed nothing anyone could see. Selection by id also reaches
   * commits the graph walk does not draw, which is what a reflog is for; the
   * pin keeps it selected when a filter typed above stops the graph drawing it.
   */
  function inspectEntry(entry: ReflogEntry) {
    selectedSelector = entry.selector;
    repoStore.selectHistoryCommit(entry.commit_id, {
      summary: `${entry.action}: ${entry.message}`,
      pin: true,
      // The details pane's branch-here and reset-here name it by this.
      label: entry.selector,
    });
  }

  /**
   * HEAD moved (a reset or checkout from the details pane, a commit made in
   * the terminal): the reflog gained an entry. Keyed on the graph's head for
   * the same repository only — a repository switch is the effect below's.
   */
  let seenHead: { repo: string | null; head: string | null } | null = null;
  $effect(() => {
    const repo = $currentRepoPath;
    const head = $graphStore.headId;
    untrack(() => {
      const prev = seenHead;
      seenHead = { repo, head };
      // A null head is the graph not having loaded yet (a cold mount, a
      // repository it had not cached): its arrival is not a move, and the
      // repository effect below already loads for it.
      if (prev && prev.repo === repo && prev.head && head && prev.head !== head) void load();
    });
  });

  $effect(() => {
    return () => inflight?.cancel();
  });

  // Keyed on the repository alone: the ~6s status poll and stats drains
  // re-emit the store object, and a rerun per emission would cancel and
  // restart the reflog IPC each time.
  $effect(() => {
    const repo = $currentRepoPath;
    return untrack(() => {
      if (!repo) {
        inflight?.cancel();
        entries = [];
        errorMsg = null;
        loading = false;
        return undefined;
      }
      // Hydrate last-known entries synchronously so a revisit renders
      // instantly and the refresh below updates the table in place.
      const cached = reflogCache.get(repo);
      if (cached) entries = cached;
      void load();
      const started = inflight;
      return () => {
        if (inflight === started) {
          started?.cancel();
        }
      };
    });
  });
</script>

<div class="flex-1 flex flex-col bg-background min-h-0 text-xs font-sans overflow-hidden">
  <div class="px-4 py-1.5 border-b border-border/60 gp-section-edge bg-surface/60 flex items-center justify-between shrink-0">
    <span class="text-textMuted" role="status">
      {#if $filterStore.searchQuery.trim() && entries.length > 0}
        {shown.length} of {plural(entries.length, "loaded entry", "loaded entries")} match
      {:else}
        HEAD's reflog · {plural(entries.length, "entry", "entries")}
      {/if}
    </span>
    <button onclick={load} class="gp-btn">Refresh</button>
  </div>

  <div class="flex-1 min-h-0 flex flex-col">
    {#if loading && entries.length === 0}
      <div class="p-4 text-textMuted">Loading reflog…</div>
    {:else if errorMsg}
      <div class="p-4 text-rose-400">{errorMsg}</div>
    {:else if entries.length === 0}
      <EmptyState
        icon={History}
        title="No reflog entries"
        hint="Reference updates (commits, checkouts, resets, merges) will appear here."
      />
    {:else if shown.length === 0}
      <EmptyState
        icon={History}
        title="No reflog entries match"
        hint="The commit filter narrows the entries already loaded. Clear it to see them all."
      />
    {:else}
      {#if entries.length >= REFLOG_ENTRY_LIMIT}
        <div class="shrink-0 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5 text-[11px] text-amber-600 dark:text-amber-300" role="status">
          Showing the 200 most recent reflog entries. Older reflog history may exist.
        </div>
      {/if}
      <div class="flex-1 min-h-0 overflow-auto">
        <table class="w-full text-left">
          <thead class="sticky top-0 bg-surface text-[10px] uppercase text-textMuted">
            <tr>
              <th class="px-3 py-2 font-medium">Selector</th>
              <th class="px-3 py-2 font-medium">SHA</th>
              <th class="px-3 py-2 font-medium">Action</th>
              <th class="px-3 py-2 font-medium">Message</th>
              <th class="px-3 py-2 font-medium">When</th>
            </tr>
          </thead>
          <tbody>
            {#each shown as entry (entry.selector)}
              {@const current = entry.selector === selectedSelector && entry.commit_id === selectedCommitId}
              <tr
                aria-current={current ? "true" : undefined}
                class="group border-t border-border/30 hover:bg-surfaceHover/60 transition-colors focus-within:bg-surfaceHover {current ? 'bg-accent/10' : ''}"
              >
                <td class="px-3 py-1.5 font-mono rounded-l-lg">
                  <button
                    type="button"
                    aria-label={`Inspect ${entry.selector}, commit ${shortHash(entry.commit_id, 8)}, ${entry.action}: ${entry.message}, ${formatDate(entry.timestamp)}`}
                    title="Show this commit below, where it can be branched from or reset to"
                    onclick={() => inspectEntry(entry)}
                    class="rounded px-1 -mx-1 text-accent hover:underline focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-accent"
                  >
                    {entry.selector}
                  </button>
                </td>
                <td class="px-3 py-1.5 font-mono whitespace-nowrap">
                  {shortHash(entry.commit_id, 8)}
                  {#if marks.get(entry.commit_id)}
                    {@const mark = marks.get(entry.commit_id)!}
                    <span class="gp-chip ml-1 font-sans" data-tone="danger" title={mark.title}>suspect #{mark.rank}</span>
                  {/if}
                </td>
                <td class="px-3 py-1.5 text-textPrimary">{entry.action}</td>
                <td class="px-3 py-1.5 text-textMuted truncate max-w-md">{entry.message}</td>
                <td class="px-3 py-1.5 text-textMuted whitespace-nowrap rounded-r-lg">{formatDate(entry.timestamp)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>
