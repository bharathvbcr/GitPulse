<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the reflog view renders the
  // last-known table instantly; the fetch then refreshes it in place.
  const reflogCache = createRepoPanelCache<ReflogEntry[]>();
  const opCache = createRepoPanelCache<OpSnapshotSummary[]>();
</script>

<script lang="ts">
  import type { ReflogEntry } from "../branches/types";
  import type {
    OpRefEntry,
    OpSnapshotList,
    OpSnapshotSummary,
    OpUndoOutcome,
    OpUndoPlan,
  } from "../ops/model";
  import type { Guarded } from "../stores/harnessStore";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { filterReflogEntries } from "../branches/reflogFilter";
  import { invoke } from "../ipc/invoke";
  import { AlertCircle, History, RotateCcw } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import Checkbox from "./Checkbox.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { LAYERS } from "../ui/layers";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatDate, plural, shortHash } from "../format";
  import { reportPanelError } from "../diagnostics/report";
  import { suspectsQuery, suspectMarks } from "../codeintel/suspectsQuery";

  type ReflogMode = "reflog" | "ops";
  const MODE_OPTIONS: readonly { value: ReflogMode; label: string; title: string }[] = [
    { value: "reflog", label: "HEAD moves", title: "Git reflog entries for HEAD" },
    { value: "ops", label: "Agent ops", title: "Pre-write snapshots recorded under refs/gitpulse/ops" },
  ];

  const REFLOG_ENTRY_LIMIT = 200;
  const OP_LIST_LIMIT = 50;

  let mode: ReflogMode = $state("reflog");
  let entries: ReflogEntry[] = $state([]);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;
  /** The entry last selected here; several entries can name one commit. */
  let selectedSelector = $state<string | null>(null);

  let opEntries: OpSnapshotSummary[] = $state([]);
  let opTotal = $state(0);
  let opLoading = $state(false);
  let opErrorMsg = $state<string | null>(null);
  let opInflight: AsyncGuard | null = null;
  let reviewingOp: OpSnapshotSummary | null = $state(null);
  let confirmDirty = $state(false);
  let undoing = $state(false);
  let undoError = $state<string | null>(null);
  let undoSuccess = $state<string | null>(null);

  // History's commit filter sits above this list, as it does above the graph.
  // It narrows the entries already loaded (up to REFLOG_ENTRY_LIMIT); the
  // branch chip cannot apply, because this is HEAD's log, not a branch's.
  let shown = $derived(filterReflogEntries(entries, $filterStore.searchQuery));
  let shownOps = $derived.by(() => {
    const q = $filterStore.searchQuery.trim().toLowerCase();
    if (!q) return opEntries;
    return opEntries.filter(
      (snap) =>
        snap.command.toLowerCase().includes(q) ||
        snap.op_ref.toLowerCase().includes(q) ||
        `#${snap.seq}`.includes(q) ||
        (snap.head_ref?.toLowerCase().includes(q) ?? false) ||
        snap.refs.some((r) => r.refname.toLowerCase().includes(q)),
    );
  });
  let selectedCommitId = $derived($graphStore.selectedCommit?.id ?? null);
  // Commits the Suspects list ranked, marked here too: a HEAD move that is
  // also a suspect is the recovery point worth reading first.
  let marks = $derived(suspectMarks($suspectsQuery, $currentRepoPath));

  function diffRefStatus(entry: OpRefEntry): "unchanged" | "deleted" | "moved" {
    if (entry.current_oid === entry.oid) return "unchanged";
    if (entry.current_oid === null) return "deleted";
    return "moved";
  }

  function changedRefCount(snap: OpSnapshotSummary): number {
    return snap.refs.filter((r) => r.current_oid !== r.oid).length;
  }

  async function loadOps() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    opInflight?.cancel();
    const guard = createAsyncGuard();
    opInflight = guard;
    opLoading = true;
    opErrorMsg = null;
    try {
      const list = await invoke<OpSnapshotList>("cmd_list_op_snapshots", {
        repoPath: repo,
        limit: OP_LIST_LIMIT,
      });
      if (!guard.isLive()) return;
      opEntries = list.entries;
      opTotal = list.total;
      opCache.set(repo, list.entries);
      if (reviewingOp) {
        const refreshed = list.entries.find((item) => item.seq === reviewingOp?.seq) ?? null;
        reviewingOp = refreshed;
      }
    } catch (err) {
      if (!guard.isLive()) return;
      const reported = reportPanelError("reflog", err);
      opErrorMsg = `${describeFailure({ verb: "load", object: "agent operations" }, err).headline} ${reported}`;
      opEntries = [];
      opTotal = 0;
    } finally {
      if (guard.isLive()) opLoading = false;
    }
  }

  function selectMode(next: ReflogMode) {
    if (mode === next) return;
    mode = next;
    if (next === "ops") {
      const repo = $repoStore.currentPath;
      if (repo) {
        const cached = opCache.get(repo);
        if (cached) opEntries = cached;
        void loadOps();
      }
    }
  }

  function beginReviewUndo(snap: OpSnapshotSummary) {
    if (reviewingOp?.seq === snap.seq) {
      reviewingOp = null;
      confirmDirty = false;
      undoError = null;
      return;
    }
    reviewingOp = snap;
    confirmDirty = false;
    undoError = null;
    undoSuccess = null;
  }

  function cancelReviewUndo() {
    reviewingOp = null;
    confirmDirty = false;
    undoError = null;
  }

  async function confirmUndo() {
    const repo = $repoStore.currentPath;
    const snap = reviewingOp;
    if (!repo || !snap || undoing) return;
    undoing = true;
    undoError = null;
    undoSuccess = null;
    const plan: OpUndoPlan = {
      seq: snap.seq,
      expected_refs: snap.refs.map((r) => ({
        refname: r.refname,
        expected_oid: r.current_oid,
      })),
      confirm_dirty: confirmDirty,
    };
    try {
      const result = await invoke<Guarded<OpUndoOutcome>>("cmd_undo_op_snapshot", {
        repoPath: repo,
        plan,
      });
      if ($repoStore.currentPath !== repo) return;
      const outcome = result.output;
      undoSuccess = `Restored ${plural(outcome.restored_refs, "reference", "references")} from snapshot #${outcome.seq}.`;
      reviewingOp = null;
      confirmDirty = false;
      await Promise.all([loadOps(), load(), repoStore.refresh(repo)]);
    } catch (err) {
      if ($repoStore.currentPath !== repo) return;
      const reported = reportPanelError("reflog", err);
      undoError = `${describeFailure({ verb: "restore", object: "this snapshot" }, err).headline} ${reported}`;
    } finally {
      undoing = false;
    }
  }

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
      if (prev && prev.repo === repo && prev.head && head && prev.head !== head && mode === "ops") void loadOps();
    });
  });

  $effect(() => {
    return () => {
      inflight?.cancel();
      opInflight?.cancel();
    };
  });

  // Keyed on the repository alone: the ~6s status poll and stats drains
  // re-emit the store object, and a rerun per emission would cancel and
  // restart the reflog IPC each time.
  $effect(() => {
    const repo = $currentRepoPath;
    return untrack(() => {
      if (!repo) {
        inflight?.cancel();
        opInflight?.cancel();
        entries = [];
        errorMsg = null;
        loading = false;
        opEntries = [];
        opTotal = 0;
        opErrorMsg = null;
        opLoading = false;
        reviewingOp = null;
        confirmDirty = false;
        undoError = null;
        undoSuccess = null;
        return undefined;
      }
      // Hydrate last-known entries synchronously so a revisit renders
      // instantly and the refresh below updates the table in place.
      const cached = reflogCache.get(repo);
      if (cached) entries = cached;
      void load();
      const started = inflight;
      if (mode === "ops") {
        const cachedOps = opCache.get(repo);
        if (cachedOps) opEntries = cachedOps;
        void loadOps();
      }
      return () => {
        if (inflight === started) {
          started?.cancel();
        }
        opInflight?.cancel();
      };
    });
  });
</script>

<div class="flex-1 flex flex-col bg-background min-h-0 text-xs font-sans overflow-hidden">
  <div class="gp-header">
    <SettingSegment
      ariaLabel="Reflog source"
      size="sm"
      options={MODE_OPTIONS}
      value={mode}
      onselect={selectMode}
    />
    <span class="min-w-0 flex-1 truncate text-textMuted" role="status">
      {#if mode === "ops"}
        {#if $filterStore.searchQuery.trim() && opEntries.length > 0}
          {shownOps.length} of {plural(opEntries.length, "snapshot", "snapshots")} match
        {:else}
          Agent ops · {plural(opEntries.length, "snapshot", "snapshots")}{#if opTotal > opEntries.length} (of {opTotal}){/if}
        {/if}
      {:else if $filterStore.searchQuery.trim() && entries.length > 0}
        {shown.length} of {plural(entries.length, "loaded entry", "loaded entries")} match
      {:else}
        HEAD's reflog · {plural(entries.length, "entry", "entries")}
      {/if}
    </span>
    <button type="button" onclick={() => (mode === "ops" ? void loadOps() : void load())} class="gp-btn" data-size="sm">Refresh</button>
  </div>

  <div class="flex-1 min-h-0 flex flex-col">
    {#if mode === "reflog"}
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
                    title="Show this commit below, where it can be branched from or reset to"
                    onclick={() => inspectEntry(entry)}
                    class="gp-btn -mx-2.5 font-mono focus-visible:ring-1 focus-visible:ring-accent"
                    data-variant="ghost"
                    data-size="sm"
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
    {:else}
      {#if undoSuccess}
        <div class="shrink-0 px-3 pt-2">
          <div class="gp-notice text-meta" data-tone="ok" role="status">
            <span class="min-w-0 flex-1 break-words">{undoSuccess}</span>
          </div>
        </div>
      {/if}
      {#if reviewingOp}
        <div class="gp-card mx-3 mt-2 p-3 flex flex-col gap-2 shrink-0" role="region" aria-label={`Review undo for snapshot #${reviewingOp.seq}`}>
          <div class="flex items-start gap-2">
            <div class="min-w-0 flex-1">
              <div class="text-textPrimary font-medium truncate">
                Review undo · Snapshot #{reviewingOp.seq}
                <span class="ml-1 font-mono text-textMuted">{reviewingOp.op_ref}</span>
              </div>
              <div class="text-textMuted font-mono truncate">{reviewingOp.command}</div>
              {#if reviewingOp.stash_oid}
                <div class="text-meta text-textMuted mt-0.5">
                  Pre-op worktree stash preserved at <span class="font-mono">{shortHash(reviewingOp.stash_oid, 8)}</span>
                </div>
              {/if}
            </div>
            <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={cancelReviewUndo} disabled={undoing}>Cancel</button>
            <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={confirmUndo} disabled={undoing}>
              {undoing ? "Restoring…" : "Confirm undo"}
            </button>
          </div>
          {#if undoError}
            <div class="gp-notice" data-tone="unknown" role="alert">
              <AlertCircle size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
              <span class="min-w-0 flex-1 break-words">{undoError}</span>
            </div>
          {/if}
          {#if reviewingOp.refs.length === 0}
            <div class="text-meta text-textMuted">No branch or tag references were recorded in this snapshot.</div>
          {:else}
            <div class="max-h-36 overflow-auto flex flex-col gap-1">
              {#each reviewingOp.refs as refEntry (refEntry.refname)}
                {@const status = diffRefStatus(refEntry)}
                <div class="flex items-center gap-2 text-meta font-mono">
                  <span class="min-w-0 flex-1 truncate text-textPrimary">{refEntry.refname}</span>
                  <span class="shrink-0 text-textMuted">{refEntry.current_oid ? shortHash(refEntry.current_oid, 8) : "deleted"}</span>
                  <span class="shrink-0 text-textMuted">→</span>
                  <span class="shrink-0 text-textPrimary">{shortHash(refEntry.oid, 8)}</span>
                  <span class="gp-chip font-sans shrink-0" data-tone={status === "unchanged" ? "neutral" : "warn"}>{status}</span>
                </div>
              {/each}
            </div>
          {/if}
          <Checkbox
            checked={confirmDirty}
            disabled={undoing}
            onchange={(next) => {
              confirmDirty = next;
            }}
            label="Confirm dirty working tree overwrite if checked-out HEAD moves"
          />
        </div>
      {/if}
      {#if opLoading && opEntries.length === 0}
        <div class="p-4"><Skeleton variant="tree-row" count={6} /></div>
      {:else if opErrorMsg}
        <div class="p-4">
          <div class="gp-notice" data-tone="unknown" role="alert">
            <AlertCircle size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
            <span class="min-w-0 flex-1 break-words">{opErrorMsg}</span>
            <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={loadOps}>Try again</button>
          </div>
        </div>
      {:else if opEntries.length === 0}
        <EmptyState
          icon={RotateCcw}
          title="No agent op snapshots"
          hint="Gated agent Git writes will record restorable ref snapshots under refs/gitpulse/ops."
        />
      {:else if shownOps.length === 0}
        <EmptyState
          icon={RotateCcw}
          title="No agent ops match"
          hint="The filter narrows the snapshots already loaded. Clear it to see them all."
        />
      {:else}
        <div class="flex-1 min-h-0 overflow-auto">
          <table class="w-full text-left" aria-label="Agent operation snapshots">
            <thead class="sticky top-0 bg-surface text-textMuted" style="z-index: {LAYERS.STICKY}">
              <tr>
                <th class="gp-label px-3 py-2">Snapshot</th>
                <th class="gp-label px-3 py-2">Command</th>
                <th class="gp-label px-3 py-2">Refs</th>
                <th class="gp-label px-3 py-2">When</th>
                <th class="gp-label px-3 py-2">Undo</th>
              </tr>
            </thead>
            <tbody>
              {#each shownOps as snap (snap.op_ref)}
                {@const changed = changedRefCount(snap)}
                {@const reviewing = reviewingOp?.seq === snap.seq}
                <tr
                  aria-current={reviewing ? "true" : undefined}
                  class="group hover:bg-surfaceHover/60 transition-colors focus-within:bg-surfaceHover {reviewing ? 'bg-accent/10' : ''}"
                >
                  <td class="px-3 py-1.5 font-mono whitespace-nowrap rounded-l-lg" title={snap.op_ref}>
                    #{snap.seq}
                    {#if snap.stash_oid}
                      <span class="gp-chip ml-1 font-sans" title={`Worktree snapshot ${snap.stash_oid}`}>worktree stash</span>
                    {/if}
                  </td>
                  <td class="px-3 py-1.5 font-mono text-textPrimary truncate max-w-md">{snap.command}</td>
                  <td class="px-3 py-1.5 text-textMuted whitespace-nowrap">
                    {changed > 0 ? `${plural(changed, "changed", "changed")} / ${snap.refs.length}` : `${snap.refs.length} matching`}
                  </td>
                  <td class="px-3 py-1.5 text-textMuted whitespace-nowrap">{formatDate(snap.created_at)}</td>
                  <td class="px-3 py-1.5 whitespace-nowrap rounded-r-lg">
                    <button
                      type="button"
                      class="gp-btn focus-visible:ring-1 focus-visible:ring-accent"
                      data-size="sm"
                      onclick={() => beginReviewUndo(snap)}
                    >
                      {reviewing ? "Close review" : "Review undo"}
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    {/if}
  </div>
</div>
