<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { normalizeDiffPayload } from "../stores/graphStore";
  import { invoke } from "@tauri-apps/api/core";
  import { FileCode, Check, X } from "lucide-svelte";
  import ImageDiffViewer from "./ImageDiffViewer.svelte";
  import EmptyState from "./EmptyState.svelte";
  import VirtualList from "./VirtualList.svelte";
  import {
    annotateRange,
    computeWordDiff,
    isImagePath,
    parseUnifiedDiff,
    type AnnotatedDiffLine,
  } from "../diff/wordDiff";
  import { decideWhitespaceRefetch } from "../diff/whitespaceToggle";

  interface FileBlob {
    path: string;
    is_binary: boolean;
    is_image: boolean;
    mime: string;
    text?: string | null;
    base64?: string | null;
  }

  // Fixed row geometry keeps the virtualized window math trivial and lets a
  // half-million-line agent diff render exactly like a twenty-line one.
  const ROW_HEIGHT = 20;
  const OVERSCAN = 20;
  /** Beyond this, even the light parse stops being worth its memory. */
  const MAX_RENDER_LINES = 300_000;

  let viewMode = $state<"unified" | "split">("unified");
  let ignoreWhitespace = $state(false);
  let oldSrc = $state<string | null>(null);
  let newSrc = $state<string | null>(null);

  /**
   * The diff may arrive as a legacy bare string or as the new payload object
   * (the backend is mid-transition); the normalizer makes either renderable
   * and carries the truncation metadata a massive commit needs.
   */
  let diffPayload = $derived(normalizeDiffPayload($repoStore.selectedDiff));
  let bannerDismissed = $state(false);
  let commitTruncated = $derived(diffPayload.truncated);
  const MAX_SKIPPED_SHOWN = 5;
  let visibleSkippedFiles = $derived(
    commitTruncated ? diffPayload.skipped_files.slice(0, MAX_SKIPPED_SHOWN) : []
  );
  let moreSkippedCount = $derived(
    Math.max(0, diffPayload.skipped_files.length - visibleSkippedFiles.length)
  );
  let skippedAdditions = $derived(
    diffPayload.skipped_files.reduce((sum, f) => sum + f.additions, 0)
  );
  let skippedDeletions = $derived(
    diffPayload.skipped_files.reduce((sum, f) => sum + f.deletions, 0)
  );
  let shownAdditions = $derived(Math.max(0, diffPayload.total_additions - skippedAdditions));
  let shownDeletions = $derived(Math.max(0, diffPayload.total_deletions - skippedDeletions));

  let allLines = $derived(parseUnifiedDiff(diffPayload.content));
  let truncatedSource = $derived(allLines.length > MAX_RENDER_LINES);
  let lines = $derived(truncatedSource ? allLines.slice(0, MAX_RENDER_LINES) : allLines);

  /**
   * Word-diff runs only over what is on screen. Segments attach to the line
   * objects themselves, so scrolling back is free: each adjacent del/add pair
   * is annotated once per diff, not once per frame. The `segments` guard on
   * annotateRange makes repeated window renders idempotent.
   */
  function unifiedRow(index: number): AnnotatedDiffLine | undefined {
    const line = lines[index];
    if (!line) return undefined;
    const next = lines[index + 1];
    if (next && line.type === "del" && next.type === "add") {
      annotateRange(lines, index, index + 2);
    }
    return line;
  }

  /**
   * Split rows are aligned pairs: an adjacent del/add shares one row, context
   * appears on both sides. Total height is therefore max(left, right) across
   * the whole diff and the scrollbar tells the truth.
   */
  interface SplitRow {
    left: AnnotatedDiffLine | null;
    right: AnnotatedDiffLine | null;
  }

  let splitRows = $derived.by<SplitRow[]>(() => {
    if (viewMode !== "split") return [];
    const rows: SplitRow[] = [];
    let pendingDel: AnnotatedDiffLine | null = null;
    const flushPending = () => {
      if (pendingDel) {
        rows.push({ left: pendingDel, right: null });
        pendingDel = null;
      }
    };
    for (const line of lines) {
      if (line.type === "del") {
        flushPending();
        pendingDel = line;
      } else if (line.type === "add") {
        rows.push({ left: pendingDel, right: line });
        pendingDel = null;
      } else if (line.type === "ctx") {
        flushPending();
        rows.push({ left: line, right: line });
      } else {
        flushPending();
        rows.push({ left: line, right: null });
      }
    }
    flushPending();
    return rows;
  });

  function annotateSplitPair(row: SplitRow): void {
    const left = row.left;
    const right = row.right;
    if (
      !left ||
      !right ||
      left === right ||
      left.segments ||
      right.segments ||
      left.type !== "del" ||
      right.type !== "add"
    ) {
      return;
    }
    const diff = computeWordDiff(left.content.slice(1), right.content.slice(1));
    left.segments = diff.original_segments;
    right.segments = diff.modified_segments;
  }

  function splitRow(index: number): SplitRow | undefined {
    const row = splitRows[index];
    if (!row) return undefined;
    annotateSplitPair(row);
    return row;
  }

  let showingImage = $derived(isImagePath($repoStore.selectedFilePath));

  let unifiedScroll = $state(0);
  let splitScroll = $state(0);

  $effect(() => {
    void $repoStore.selectedFilePath;
    void $repoStore.selectedCommitId;
    unifiedScroll = 0;
    splitScroll = 0;
    bannerDismissed = false;
  });

  let prevIgnore = $state(false);
  $effect(() => {
    if (ignoreWhitespace === prevIgnore) return;
    prevIgnore = ignoreWhitespace;
    const decision = decideWhitespaceRefetch({
      filePath: $repoStore.selectedFilePath,
      commitId: $repoStore.selectedCommitId,
      statuses: $repoStore.statuses,
    });
    if (!decision.refetch || !$repoStore.currentPath) return;
    repoStore.selectFileDiff(
      $repoStore.selectedFilePath as string,
      decision.isStaged,
      ignoreWhitespace
    );
  });

  $effect(() => {
    const path = $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    const commitId = $repoStore.selectedCommitId;
    if (!showingImage || !path || !repo) {
      oldSrc = null;
      newSrc = null;
      return;
    }
    let cancelled = false;
    (async () => {
      const blobUrl = (blob: FileBlob | null): string | null => {
        if (!blob?.base64) return null;
        return `data:${blob.mime || "image/png"};base64,${blob.base64}`;
      };
      try {
        const newBlob = await invoke<FileBlob>("cmd_get_file_blob", {
          repoPath: repo,
          filePath: path,
          commitId: commitId || null,
        });
        let oldBlob: FileBlob | null = null;
        try {
          oldBlob = await invoke<FileBlob>("cmd_get_file_blob", {
            repoPath: repo,
            filePath: path,
            commitId: commitId ? `${commitId}^` : "HEAD",
          });
        } catch {
          oldBlob = null;
        }
        if (!cancelled) {
          newSrc = blobUrl(newBlob);
          oldSrc = blobUrl(oldBlob);
        }
      } catch {
        if (!cancelled) {
          oldSrc = null;
          newSrc = null;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });
</script>

{#if showingImage}
  <ImageDiffViewer filePath={$repoStore.selectedFilePath || "image"} {oldSrc} {newSrc} />
{:else}
<div class="flex-1 flex flex-col bg-background h-full text-xs font-mono select-none overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans">
    <div class="flex items-center gap-2 truncate">
      <FileCode size={16} class="text-accent shrink-0" />
      <span class="font-medium text-textPrimary truncate">{$repoStore.selectedFilePath || $repoStore.selectedCommitId || "Diff View"}</span>
      {#if lines.length > 0}
        <span class="text-[10px] text-textMuted shrink-0">{lines.length.toLocaleString()} lines</span>
      {/if}
    </div>

    <div class="flex items-center gap-3">
      <label class="flex items-center gap-1.5 text-textMuted cursor-pointer hover:text-textPrimary text-xs font-sans">
        <input type="checkbox" bind:checked={ignoreWhitespace} class="rounded bg-surface border-border text-accent focus:ring-0" />
        <span>Ignore Whitespace</span>
      </label>

      <div class="gp-segmented">
        <button
          onclick={() => (viewMode = "unified")}
          data-active={viewMode === "unified" ? "true" : "false"}
          class="gp-seg-btn"
        >
          Unified
        </button>
        <button
          onclick={() => (viewMode = "split")}
          data-active={viewMode === "split" ? "true" : "false"}
          class="gp-seg-btn"
        >
          Split
        </button>
      </div>

      {#if $repoStore.selectedFilePath}
        <button
          onclick={() => $repoStore.selectedFilePath && repoStore.stageFile($repoStore.selectedFilePath)}
          class="gp-btn-primary !py-1"
        >
          <Check size={13} />
          <span>Stage File</span>
        </button>
      {/if}
    </div>
  </div>

  {#if commitTruncated && !bannerDismissed}
    <div class="mx-3 mt-2 px-3 py-2 rounded-xl bg-surfaceHover border border-border/60 text-[11px] font-sans text-textMuted space-y-1">
      <div class="flex items-center justify-between gap-2">
        <span>
          Large commit: showing {diffPayload.included_files.toLocaleString()} of {diffPayload.total_files.toLocaleString()} changed files
          (+{shownAdditions.toLocaleString()}/-{shownDeletions.toLocaleString()} lines shown of +{diffPayload.total_additions.toLocaleString()}/-{diffPayload.total_deletions.toLocaleString()} total)
        </span>
        <button
          onclick={() => (bannerDismissed = true)}
          aria-label="Dismiss truncation notice"
          title="Dismiss truncation notice"
          class="shrink-0 rounded-full p-0.5 text-textMuted hover:text-textPrimary hover:bg-background/70 transition-colors cursor-pointer"
        >
          <X size={13} />
        </button>
      </div>
      {#each visibleSkippedFiles as f (f.path)}
        <div class="flex items-center justify-between gap-3 pl-3 min-w-0">
          <span class="truncate font-mono">{f.path}</span>
          <span class="font-mono shrink-0"><span class="text-green-400">+{f.additions}</span> <span class="text-red-400">-{f.deletions}</span></span>
        </div>
      {/each}
      {#if moreSkippedCount > 0}
        <div class="pl-3">+{moreSkippedCount.toLocaleString()} more files not shown</div>
      {/if}
    </div>
  {/if}

  {#if truncatedSource}
    <div class="mx-3 mt-2 px-3 py-1.5 rounded-xl bg-amber-500/10 border border-amber-500/30 text-[11px] text-amber-300 font-sans flex items-center gap-2">
      <span>⚠</span>
      <span>Diff exceeds {MAX_RENDER_LINES.toLocaleString()} lines — showing the first {MAX_RENDER_LINES.toLocaleString()}. Use the filter bar or open specific files instead of one massive commit.</span>
    </div>
  {/if}

  {#if lines.length === 0}
    <EmptyState
      icon={FileCode}
      title="No diff selected"
      hint="Select a changed file from the sidebar or a commit from the graph to view diffs."
    />
  {:else if viewMode === "unified"}
    <VirtualList items={lines} rowHeight={ROW_HEIGHT} overscan={OVERSCAN} bind:scrollTop={unifiedScroll} class="flex-1 min-h-0">
      {#snippet row(_, index)}
        {@const line = unifiedRow(index)}
        {#if line}
          {#if line.type === "hdr"}
            <div class="px-3 bg-surfaceHover text-textMuted text-[11px] font-medium flex items-center h-5 overflow-hidden whitespace-pre" style="height: {ROW_HEIGHT}px;">
              {line.content}
            </div>
          {:else if line.type === "add"}
            <div class="px-3 bg-green-500/15 text-green-300 flex items-center gap-2 hover:bg-green-500/25 overflow-hidden" style="height: {ROW_HEIGHT}px;">
              <span class="w-10 text-right text-textMuted/50 text-[10px] select-none shrink-0">{line.newNo ?? ""}</span>
              <span class="text-green-400 select-none font-bold shrink-0">+</span>
              <span class="whitespace-pre">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Added" ? "bg-green-500/40 text-green-100" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
            </div>
          {:else if line.type === "del"}
            <div class="px-3 bg-red-500/15 text-red-300 flex items-center gap-2 hover:bg-red-500/25 overflow-hidden" style="height: {ROW_HEIGHT}px;">
              <span class="w-10 text-right text-textMuted/50 text-[10px] select-none shrink-0">{line.oldNo ?? ""}</span>
              <span class="text-red-400 select-none font-bold shrink-0">-</span>
              <span class="whitespace-pre">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Removed" ? "bg-red-500/40 text-red-100" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
            </div>
          {:else}
            <div class="px-3 text-textPrimary/80 flex items-center gap-2 hover:bg-surfaceHover/40 overflow-hidden" style="height: {ROW_HEIGHT}px;">
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.oldNo ?? line.newNo ?? ""}</span>
              <span class="w-2 select-none shrink-0"></span>
              <span class="whitespace-pre">{line.content.startsWith(" ") ? line.content.substring(1) : line.content}</span>
            </div>
          {/if}
        {/if}
      {/snippet}
    </VirtualList>
  {:else}
    <VirtualList items={splitRows} rowHeight={ROW_HEIGHT} overscan={OVERSCAN} bind:scrollTop={splitScroll} class="flex-1 min-h-0 border-r border-border/80">
      {#snippet row(_, index)}
        {@const row = splitRow(index)}
        {#if row}
          {@const left = row.left}
          {@const right = row.right}
          <div class="grid grid-cols-2 divide-x divide-border" style="height: {ROW_HEIGHT}px;">
            <div class="px-3 flex items-center gap-2 overflow-hidden {left ? (left.type === 'del' ? 'bg-red-500/15 text-red-300' : left.type === 'hdr' ? 'bg-surfaceHover text-textMuted' : 'text-textPrimary/80') : ''}">
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{left?.oldNo ?? ""}</span>
              {#if left}
                <span class="whitespace-pre overflow-hidden">{#if left.segments}{#each left.segments as seg}<span class={seg.kind === "Removed" ? "bg-red-500/40" : ""}>{seg.text}</span>{/each}{:else}{left.content.startsWith(" ") || (left.type !== "ctx" && left.type !== "hdr") ? left.content.substring(1) : left.content}{/if}</span>
              {/if}
            </div>
            <div class="px-3 flex items-center gap-2 overflow-hidden {right ? (right.type === 'add' ? 'bg-green-500/15 text-green-300' : right.type === 'hdr' ? '' : 'text-textPrimary/80') : ''}">
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{right?.newNo ?? ""}</span>
              {#if right}
                <span class="whitespace-pre overflow-hidden">{#if right.segments}{#each right.segments as seg}<span class={seg.kind === "Added" ? "bg-green-500/40" : ""}>{seg.text}</span>{/each}{:else}{right.content.startsWith(" ") || (right.type !== "ctx" && right.type !== "hdr") ? right.content.substring(1) : right.content}{/if}</span>
              {/if}
            </div>
          </div>
        {/if}
      {/snippet}
    </VirtualList>
  {/if}
</div>
{/if}
