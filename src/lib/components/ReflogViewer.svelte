<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the reflog view renders the
  // last-known table instantly; the fetch then refreshes it in place.
  const reflogCache = createRepoPanelCache<ReflogEntry[]>();
</script>

<script lang="ts">
  import type { ReflogEntry, ResetPreview } from "../branches/types";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { filterReflogEntries } from "../branches/reflogFilter";
  import { invoke } from "../ipc/invoke";
  import { AlertCircle, GitBranchPlus, History, RotateCcw } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { LAYERS } from "../ui/layers";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatDate, plural, shortHash } from "../format";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import { askConfirm, askText } from "../stores/modalStore";
  import { toastStore } from "../stores/toastStore";
  import { reportFailure } from "../stores/mutationOutcome";
  import { describeResetPreview } from "../branches/resetPreview";

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
      const reported = reportPanelError("reflog", err);
      errorMsg = `${describeFailure({ verb: "load", object: "the reflog" }, err).headline} ${reported}`;
      entries = [];
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  /**
   * Shows the entry's commit in the details pane below this list, in place.
   *
   * It used to hand off to `inspectCommitInHistory`, which selected the
   * commit on Graph while this pane, then its own section, stayed on screen:
   * the click changed nothing anyone could see. Selection by id also reaches
   * commits the graph walk does not draw, which is what a reflog is for.
   */
  function inspectEntry(entry: ReflogEntry) {
    selectedSelector = entry.selector;
    void graphStore.selectCommitById(entry.commit_id, { summary: `${entry.action}: ${entry.message}` });
    void repoStore.selectCommitDiff(entry.commit_id);
  }

  /** The non-destructive way back: a new branch at the entry's commit. */
  async function branchHere(entry: ReflogEntry) {
    // The repository this was asked about, held across the prompt.
    const target = repoStore.captureTarget();
    const name = await askText({
      title: "Create Branch Here",
      message: `New branch at ${shortHash(entry.commit_id, 8)} (${entry.selector}: ${entry.message})`,
      placeholder: "recovered/work",
      confirmLabel: "Create Branch",
    });
    const trimmed = name?.trim();
    if (!trimmed) return;
    const outcome = await repoStore.createBranch(trimmed, entry.commit_id, target);
    if (!reportFailure(outcome, "Could not create the branch")) return;
    toastStore.success(`Created branch "${trimmed}" at ${shortHash(entry.commit_id)}`);
    void load();
  }

  /**
   * Moves the current branch to the entry's commit with `reset --keep`.
   *
   * The preview is read first so the confirmation names every commit the
   * branch would drop — and how many of them nothing else still reaches.
   * `--keep` never destroys uncommitted work: git refuses the reset when a
   * local change would be overwritten, and that refusal is what is shown.
   */
  async function resetHere(entry: ReflogEntry) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const target = repoStore.captureTarget();
    let preview: ResetPreview;
    try {
      preview = await invoke<ResetPreview>("cmd_reset_preview", {
        repoPath: repo,
        target: entry.commit_id,
      });
    } catch (err) {
      toastStore.error(`Could not preview the reset: ${formatError(err)}`);
      return;
    }
    if (repo !== $repoStore.currentPath) return;
    const described = describeResetPreview(preview, entry.selector);
    const confirmed = await askConfirm({
      title: described.title,
      message: described.message,
      confirmLabel: "Reset",
      destructive: preview.leaving_total > 0,
    });
    if (!confirmed) return;
    // The peeled oid, not the name: what was previewed is what moves.
    const outcome = await repoStore.resetTo("keep", preview.target, target);
    if (!reportFailure(outcome, "Could not reset")) return;
    toastStore.success(
      `${preview.branch ?? "HEAD"} reset to ${shortHash(preview.target)}` +
        (preview.leaving_total > 0 ? ` — ${plural(preview.leaving_total, "commit")} left the branch` : ""),
    );
    void load();
  }

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
  <div class="gp-header">
    <span class="min-w-0 flex-1 truncate text-textMuted" role="status">
      {#if $filterStore.searchQuery.trim() && entries.length > 0}
        {shown.length} of {plural(entries.length, "loaded entry", "loaded entries")} match
      {:else}
        HEAD's reflog · {plural(entries.length, "entry", "entries")}
      {/if}
    </span>
    <button type="button" onclick={load} class="gp-btn" data-size="sm">Refresh</button>
  </div>

  <div class="flex-1 min-h-0 flex flex-col">
    {#if loading && entries.length === 0}
      <div class="p-4"><Skeleton variant="tree-row" count={6} /></div>
    {:else if errorMsg}
      <div class="p-4">
        <div class="gp-notice" data-tone="unknown" role="alert">
          <AlertCircle size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
          <span class="min-w-0 flex-1 break-words">{errorMsg}</span>
          <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={load}>Try again</button>
        </div>
      </div>
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
        <div class="shrink-0 px-3 pt-2">
          <div class="gp-notice text-meta" data-tone="warn" role="status">
            Showing the 200 most recent reflog entries. Older reflog history may exist.
          </div>
        </div>
      {/if}
      <div class="flex-1 min-h-0 overflow-auto">
        <table class="w-full text-left">
          <thead class="sticky top-0 bg-surface text-textMuted" style="z-index: {LAYERS.STICKY}">
            <tr>
              <th class="gp-label px-3 py-2">Selector</th>
              <th class="gp-label px-3 py-2">SHA</th>
              <th class="gp-label px-3 py-2">Action</th>
              <th class="gp-label px-3 py-2">Message</th>
              <th class="gp-label px-3 py-2">When</th>
              <th class="gp-label px-3 py-2"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each shown as entry (entry.selector)}
              {@const current = entry.selector === selectedSelector && entry.commit_id === selectedCommitId}
              <tr
                aria-current={current ? "true" : undefined}
                class="group hover:bg-surfaceHover/60 transition-colors focus-within:bg-surfaceHover {current ? 'bg-accent/10' : ''}"
              >
                <td class="px-3 py-1.5 font-mono rounded-l-lg">
                  <button
                    type="button"
                    aria-label={`Inspect ${entry.selector}, commit ${shortHash(entry.commit_id, 8)}, ${entry.action}: ${entry.message}, ${formatDate(entry.timestamp)}`}
                    title="Show this commit below"
                    onclick={() => inspectEntry(entry)}
                    class="gp-btn -mx-2.5 font-mono focus-visible:ring-1 focus-visible:ring-accent"
                    data-variant="ghost"
                    data-size="sm"
                  >
                    {entry.selector}
                  </button>
                </td>
                <td class="px-3 py-1.5 font-mono">{shortHash(entry.commit_id, 8)}</td>
                <td class="px-3 py-1.5 text-textPrimary">{entry.action}</td>
                <td class="px-3 py-1.5 text-textMuted truncate max-w-md">{entry.message}</td>
                <td class="px-3 py-1.5 text-textMuted whitespace-nowrap">{formatDate(entry.timestamp)}</td>
                <td class="px-2 py-1 whitespace-nowrap rounded-r-lg">
                  <div class="flex items-center gap-0.5 opacity-60 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity">
                    <button
                      type="button"
                      aria-label={`Create a branch at ${entry.selector}`}
                      title="Create a branch at this commit"
                      onclick={() => branchHere(entry)}
                      class="gp-icon-btn focus-visible:ring-1 focus-visible:ring-accent"
                      data-size="sm"
                    >
                      <GitBranchPlus size={13} />
                    </button>
                    <button
                      type="button"
                      aria-label={`Reset the current branch to ${entry.selector}`}
                      title="Reset the current branch here (keeps local changes)…"
                      onclick={() => resetHere(entry)}
                      class="gp-icon-btn focus-visible:ring-1 focus-visible:ring-accent"
                      data-size="sm"
                      data-tone="danger"
                    >
                      <RotateCcw size={13} />
                    </button>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>
