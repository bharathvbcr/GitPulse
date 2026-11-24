<script lang="ts">
  import { ArrowLeft, Filter, History, RefreshCw, ShieldAlert } from "@lucide/svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { describeError } from "../ui/copy/describeFailure";
  import { shortHash } from "../format";
  import UnifiedPatchView from "./UnifiedPatchView.svelte";
  import {
    diffTurnCheckpoint,
    filterTurnCheckpoints,
    formatOmissionSummary,
    formatRelativeTurnTime,
    listTurnCheckpoints,
    totalOmittedFiles,
    type TurnCheckpointDiff,
    type TurnCheckpointList,
    type TurnCheckpointSummary,
  } from "../diff/turnCheckpoints";

  interface Props {
    repoPath: string | null;
  }

  let { repoPath }: Props = $props();

  let listData = $state<TurnCheckpointList | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let query = $state("");

  let selected = $state<TurnCheckpointSummary | null>(null);
  let againstPrevious = $state(false);
  let diffData = $state<TurnCheckpointDiff | null>(null);
  let diffLoading = $state(false);
  let diffError = $state<string | null>(null);

  let listGuard: AsyncGuard | null = null;
  let diffGuard: AsyncGuard | null = null;

  const filtered = $derived(filterTurnCheckpoints(listData?.entries ?? [], query));

  function previousIdFor(turnId: string): string | null {
    if (!listData) return null;
    const idx = listData.entries.findIndex((item) => item.id === turnId);
    if (idx === -1 || idx + 1 >= listData.entries.length) return null;
    return listData.entries[idx + 1].id;
  }

  const previousTurnId = $derived(selected ? previousIdFor(selected.id) : null);

  async function refresh() {
    if (!repoPath) {
      listData = null;
      return;
    }
    listGuard?.cancel();
    const guard = createAsyncGuard();
    listGuard = guard;
    loading = true;
    loadError = null;
    try {
      const next = await listTurnCheckpoints(repoPath, 60);
      if (guard.isLive()) {
        listData = next;
        if (selected && !next.entries.some((entry) => entry.id === selected?.id)) {
          selected = null;
          diffData = null;
        }
      }
    } catch (err) {
      if (guard.isLive()) loadError = describeError(err, "history");
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  async function loadDiffForSelected(comparePrev: boolean) {
    if (!repoPath || !selected) return;
    const prevId = previousIdFor(selected.id);
    const against = comparePrev ? prevId : null;
    if (against === null && selected.parent === null) {
      diffLoading = false;
      diffData = null;
      diffError = null;
      return;
    }
    diffGuard?.cancel();
    const guard = createAsyncGuard();
    diffGuard = guard;
    diffLoading = true;
    diffError = null;
    try {
      const next = await diffTurnCheckpoint(repoPath, selected.id, against);
      if (guard.isLive()) diffData = next;
    } catch (err) {
      if (guard.isLive()) diffError = describeError(err, "diff");
    } finally {
      if (guard.isLive()) diffLoading = false;
    }
  }

  function selectTurn(entry: TurnCheckpointSummary) {
    selected = entry;
    const hasPrev = previousIdFor(entry.id) !== null;
    againstPrevious = hasPrev;
    diffData = null;
    diffError = null;
    if (!hasPrev && entry.parent === null) {
      diffLoading = false;
      return;
    }
    void loadDiffForSelected(hasPrev);
  }

  function backToList() {
    diffGuard?.cancel();
    selected = null;
    diffData = null;
    diffError = null;
  }

  function setCompareMode(comparePrev: boolean) {
    againstPrevious = comparePrev;
    void loadDiffForSelected(comparePrev);
  }

  $effect(() => {
    void repoPath;
    backToList();
    listData = null;
    void refresh();
    return () => {
      listGuard?.cancel();
      diffGuard?.cancel();
    };
  });

  $effect(() => {
    if (!selected) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        backToList();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  });
</script>

<div class="flex flex-1 flex-col min-h-0 overflow-hidden text-meta">
  {#if !repoPath}
    <div class="p-3 text-textMuted text-micro">Open a repository to inspect agent turn checkpoints.</div>
  {:else if selected}
    <div class="shrink-0 flex items-center justify-between gap-1.5 px-2 py-1.5">
      <button type="button" class="gp-btn text-micro!" data-size="sm" onclick={backToList}>
        <ArrowLeft size={11} />
        <span>Turns</span>
      </button>
      <span class="font-mono text-micro text-textMuted truncate">
        {#if diffData}
          {shortHash(diffData.base_commit)} → {shortHash(diffData.commit)}
        {:else}
          {shortHash(selected.commit)}
        {/if}
      </span>
    </div>

    <div class="flex-1 min-h-0 overflow-y-auto p-2 space-y-2">
      <div class="space-y-1">
        <div class="font-medium text-text break-words">{selected.label || selected.id}</div>
        <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-micro font-mono text-textMuted">
          <span>{formatRelativeTurnTime(selected.created_at)}</span>
          {#if selected.branch}<span>{selected.branch}</span>{/if}
          {#if selected.event}<span>{selected.event}</span>{/if}
          {#if selected.session_id}
            <span class="gp-chip" data-tone="neutral" title="Session {selected.session_id}">
              {selected.session_id.slice(0, 8)}
            </span>
          {/if}
          <span title={selected.stats_complete ? undefined : "Stats truncated at capture limit"}>
            {#if !selected.stats_complete}~{/if}<span class="text-ok">+{selected.additions}</span>
            <span class="text-danger">−{selected.deletions}</span>
          </span>
        </div>
      </div>

      {#if previousTurnId && selected.parent !== null}
        <div class="gp-segmented p-0.5! inline-flex" role="group" aria-label="Diff base">
          <button
            type="button"
            class="gp-seg-btn px-1.5! text-micro"
            aria-pressed={againstPrevious}
            data-active={againstPrevious ? "true" : "false"}
            onclick={() => setCompareMode(true)}
          >
            Previous turn
          </button>
          <button
            type="button"
            class="gp-seg-btn px-1.5! text-micro"
            aria-pressed={!againstPrevious}
            data-active={!againstPrevious ? "true" : "false"}
            onclick={() => setCompareMode(false)}
          >
            Since HEAD
          </button>
        </div>
      {/if}

      {#if totalOmittedFiles(selected.omissions) > 0 || selected.omissions.wholesale}
        <div class="rounded border border-warn/35 bg-warn/10 px-2 py-1.5 text-micro space-y-0.5">
          <div class="flex items-center gap-1 font-medium text-text">
            <ShieldAlert size={12} class="text-warn shrink-0" />
            <span>Checkpoint omissions</span>
          </div>
          {#each formatOmissionSummary(selected.omissions) as note (note)}
            <div class="text-textMuted">{note}</div>
          {/each}
        </div>
      {/if}

      {#if !previousTurnId && selected.parent === null}
        <div class="rounded border border-border px-2.5 py-2 text-micro text-textMuted">
          Initial checkpoint has no parent commit or earlier turn to diff against.
        </div>
      {:else if diffLoading}
        <div class="py-3 text-micro text-textMuted">Loading turn diff…</div>
      {:else if diffError}
        <div class="gp-notice space-y-1.5" data-tone="danger" role="alert">
          <div>{diffError}</div>
          <button type="button" class="gp-btn" data-size="sm" onclick={() => void loadDiffForSelected(againstPrevious)}>
            Try again
          </button>
        </div>
      {:else if diffData}
        <UnifiedPatchView
          patch={diffData.patch}
          truncated={diffData.truncated}
          redacted={diffData.redacted}
          defaultExpandLimit={4}
          emptyMessage="No textual diff recorded for this turn."
        />
      {/if}
    </div>
  {:else}
    <div class="shrink-0 flex items-center gap-1 px-2 py-1.5">
      <Filter size={11} class="shrink-0 text-textMuted" />
      <input
        bind:value={query}
        type="text"
        placeholder="Filter turns…"
        aria-label="Filter turn checkpoints"
        class="flex-1 min-w-0 bg-transparent text-micro text-text outline-none placeholder:text-textMuted"
      />
      <button
        type="button"
        class="gp-icon-btn"
        aria-label="Refresh turn checkpoints"
        title="Refresh turn checkpoints"
        disabled={loading}
        onclick={() => void refresh()}
      >
        <RefreshCw size={11} class={loading ? "animate-spin" : ""} />
      </button>
    </div>

    <div class="flex-1 min-h-0 overflow-y-auto p-1.5 space-y-1">
      {#if loading && !listData}
        <div class="p-2 text-micro text-textMuted">Loading turn checkpoints…</div>
      {:else if loadError}
        <div class="gp-notice m-1 space-y-1.5" data-tone="danger" role="alert">
          <div>{loadError}</div>
          <button type="button" class="gp-btn" data-size="sm" onclick={() => void refresh()}>Try again</button>
        </div>
      {:else if filtered.length === 0}
        <div class="flex flex-col items-center justify-center gap-1.5 p-4 text-center text-micro text-textMuted">
          <History size={16} />
          <span>{listData?.entries.length ? "No turns match filter." : "No turn checkpoints recorded yet."}</span>
          <span class="opacity-75">Agent turn snapshots appear under <code>refs/gitpulse/turns/</code>.</span>
        </div>
      {:else}
        {#each filtered as entry (entry.id)}
          {@const omitted = totalOmittedFiles(entry.omissions)}
          <button
            type="button"
            class="w-full rounded border border-border/65 bg-background/60 px-2 py-1.5 text-left hover:border-accent/65 transition-colors space-y-1"
            onclick={() => selectTurn(entry)}
          >
            <div class="flex items-start justify-between gap-1.5">
              <span class="text-micro font-medium text-text line-clamp-2">{entry.label || entry.id}</span>
              <span class="shrink-0 font-mono text-micro text-textMuted">{formatRelativeTurnTime(entry.created_at)}</span>
            </div>
            <div class="flex items-center justify-between gap-1.5 text-micro font-mono text-textMuted">
              <span class="truncate">
                {entry.files} {entry.files === 1 ? "file" : "files"}{entry.branch ? ` · ${entry.branch}` : ""}
              </span>
              <span class="shrink-0 flex items-center gap-1.5" title={entry.stats_complete ? undefined : "Stats truncated at capture limit"}>
                {#if omitted > 0}
                  <span class="text-warn" title="{omitted} omitted files">!{omitted}</span>
                {/if}
                {#if !entry.stats_complete}~{/if}<span class="text-ok">+{entry.additions}</span>
                <span class="text-danger">−{entry.deletions}</span>
              </span>
            </div>
          </button>
        {/each}
        {#if listData?.truncated}
          <div class="px-2 py-1 text-micro text-textMuted">
            Showing {listData.entries.length} of {listData.total} checkpoints.
          </div>
        {/if}
      {/if}
    </div>
  {/if}
</div>
