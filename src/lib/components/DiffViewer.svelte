<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { invoke } from "@tauri-apps/api/core";
  import { FileCode, Check } from "lucide-svelte";
  import ImageDiffViewer from "./ImageDiffViewer.svelte";
  import EmptyState from "./EmptyState.svelte";
  import VirtualList from "./VirtualList.svelte";
  import {
    annotateRange,
    computeWordDiff,
    emptyDiffCopy,
    isImagePath,
    parseUnifiedDiff,
    type AnnotatedDiffLine,
  } from "../diff/wordDiff";
  import {
    buildFilePatchForHunk,
    buildFilePatchFromLines,
  } from "../diff/patchBuilder";
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
  let selectedLines = $state<Set<number>>(new Set());
  let dragAnchor = $state<number | null>(null);
  let isDragging = $state(false);

  let allLines = $derived(parseUnifiedDiff($repoStore.selectedDiff || ""));
  let truncatedSource = $derived(allLines.length > MAX_RENDER_LINES);
  let lines = $derived(truncatedSource ? allLines.slice(0, MAX_RENDER_LINES) : allLines);
  // Metadata and binary notices are chrome, not content: they must not move
  // the "N lines" stat any more than they move the line-number gutters.
  let contentLineCount = $derived(
    lines.reduce((count, line) => (line.type === "meta" || line.type === "binary" ? count : count + 1), 0)
  );

  let isWorkingTreeFile = $derived(
    $repoStore.selectedCommitId === null &&
      $repoStore.statuses.some((s) => s.path === $repoStore.selectedFilePath)
  );
  let isStaged = $derived($repoStore.selectedIsStaged);

  $effect(() => {
    // Reset selection when switching files or diff contents
    $repoStore.selectedFilePath;
    $repoStore.selectedDiff;
    selectedLines = new Set();
    isDragging = false;
    dragAnchor = null;
  });

  function lineSelectable(index: number): boolean {
    const line = lines[index];
    return !!line && (line.type === "add" || line.type === "del");
  }

  function toggleLine(index: number) {
    if (!lineSelectable(index)) return;
    const next = new Set(selectedLines);
    if (next.has(index)) next.delete(index);
    else next.add(index);
    selectedLines = next;
  }

  function selectRange(from: number, to: number) {
    const lo = Math.min(from, to);
    const hi = Math.max(from, to);
    const next = new Set<number>();
    for (let i = lo; i <= hi; i++) {
      if (lineSelectable(i)) next.add(i);
    }
    selectedLines = next;
  }

  function onLinePointerDown(index: number, event: PointerEvent) {
    if (!isWorkingTreeFile || !lineSelectable(index)) return;
    event.preventDefault();
    isDragging = true;
    dragAnchor = index;
    selectedLines = new Set([index]);
    (event.currentTarget as HTMLElement | null)?.setPointerCapture?.(event.pointerId);
  }

  function onLinePointerEnter(index: number) {
    if (!isDragging || dragAnchor === null || !lineSelectable(index)) return;
    selectRange(dragAnchor, index);
  }

  function onLinePointerUp() {
    isDragging = false;
    dragAnchor = null;
  }

  async function stageHunk(hunkIndex: number) {
    if (!$repoStore.selectedFilePath) return;
    const patch = buildFilePatchForHunk(lines, $repoStore.selectedFilePath, hunkIndex);
    if (!patch) return;
    await repoStore.stageSelectivePatch(patch, !isStaged);
    selectedLines = new Set();
  }

  async function stageSelected(isStaging: boolean) {
    if (!$repoStore.selectedFilePath || selectedLines.size === 0) return;
    const patch = buildFilePatchFromLines(lines, $repoStore.selectedFilePath, selectedLines);
    if (!patch) return;
    await repoStore.stageSelectivePatch(patch, isStaging);
    selectedLines = new Set();
  }

  let selectedGraphRow = $derived.by(() => {
    const id = $repoStore.selectedCommitId;
    if (!id) return null;
    return (
      $graphStore.rows.find((row) => row.id === id) ??
      ($graphStore.selectedCommit?.id === id ? $graphStore.selectedCommit : null)
    );
  });
  let emptyCopy = $derived(
    emptyDiffCopy($repoStore.selectedCommitId !== null && selectedGraphRow?.is_merge === true)
  );

  /**
   * Word-diff runs only over what is on screen. Segments attach to the line
   * objects themselves, so scrolling back is free: each adjacent del/add pair
   * is annotated once per diff, not once per frame. The `segments` guard on
   * annotateRange makes repeated window renders idempotent. Multi-line
   * replacement blocks are paired as a unit (min(del, add) lines), not just
   * the first adjacent pair.
   */
  function replacementBlockBounds(index: number): [number, number] | null {
    const line = lines[index];
    if (!line || (line.type !== "del" && line.type !== "add")) return null;
    let start = index;
    while (start > 0 && lines[start - 1].type === "add") start -= 1;
    while (start > 0 && lines[start - 1].type === "del") start -= 1;
    let end = index + 1;
    while (end < lines.length && lines[end].type === "del") end += 1;
    while (end < lines.length && lines[end].type === "add") end += 1;
    return [start, end];
  }

  function unifiedRow(index: number): AnnotatedDiffLine | undefined {
    const line = lines[index];
    if (!line) return undefined;
    const bounds = replacementBlockBounds(index);
    if (bounds) annotateRange(lines, bounds[0], bounds[1]);
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
  });

  let prevIgnore = $state(false);
  let prevSelectedPath = $state<string | null>(null);
  let prevCommitId = $state<string | null>(null);
  let prevStaged = $state(false);

  $effect(() => {
    const currentPath = $repoStore.selectedFilePath;
    const currentCommit = $repoStore.selectedCommitId;
    const currentStaged = $repoStore.selectedIsStaged;
    const pathChanged =
      currentPath !== prevSelectedPath ||
      currentCommit !== prevCommitId ||
      currentStaged !== prevStaged;
    const ignoreChanged = ignoreWhitespace !== prevIgnore;

    if (!pathChanged && !ignoreChanged) return;

    prevIgnore = ignoreWhitespace;
    prevSelectedPath = currentPath;
    prevCommitId = currentCommit;
    prevStaged = currentStaged;

    if (!ignoreWhitespace && !ignoreChanged) return;

    const decision = decideWhitespaceRefetch({
      filePath: currentPath,
      commitId: currentCommit,
      statuses: $repoStore.statuses,
      isStaged: currentStaged,
    });
    if (!decision.refetch || !$repoStore.currentPath) return;
    repoStore.selectFileDiff(
      currentPath as string,
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
      {#if contentLineCount > 0}
        <span class="text-[10px] text-textMuted shrink-0">{contentLineCount.toLocaleString()} lines</span>
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

      {#if isWorkingTreeFile}
        <button
          onclick={() => $repoStore.selectedFilePath && (isStaged ? repoStore.unstageFile($repoStore.selectedFilePath) : repoStore.stageFile($repoStore.selectedFilePath))}
          class="gp-btn-primary !py-1"
        >
          <Check size={13} />
          <span>{isStaged ? "Unstage File" : "Stage File"}</span>
        </button>
      {/if}
    </div>
  </div>

  {#if truncatedSource}
    <div class="mx-3 mt-2 px-3 py-1.5 rounded-xl bg-amber-500/10 border border-amber-500/30 text-[11px] text-amber-300 font-sans flex items-center gap-2">
      <span>⚠</span>
      <span>Diff exceeds {MAX_RENDER_LINES.toLocaleString()} lines — showing the first {MAX_RENDER_LINES.toLocaleString()}. Use the filter bar or open specific files instead of one massive commit.</span>
    </div>
  {/if}

  {#if lines.length === 0}
    <EmptyState
      icon={FileCode}
      title={emptyCopy.title}
      hint={emptyCopy.hint}
    />
  {:else if viewMode === "unified"}
    <VirtualList items={lines} rowHeight={ROW_HEIGHT} overscan={OVERSCAN} bind:scrollTop={unifiedScroll} class="flex-1 min-h-0">
      {#snippet row(_, index)}
        {@const line = unifiedRow(index)}
        {#if line}
          {#if line.type === "hdr"}
            <div class="px-3 bg-surfaceHover text-textMuted text-[11px] font-medium flex items-center justify-between h-5 overflow-x-auto" style="height: {ROW_HEIGHT}px;">
              <span class="truncate">{line.content}</span>
              {#if isWorkingTreeFile && line.content.startsWith("@@")}
                <button
                  onclick={() => stageHunk(index)}
                  class="ml-2 shrink-0 px-2 py-0.5 text-[10px] rounded bg-surface border border-border/80 text-accent hover:bg-accent/15 transition-colors font-sans"
                >
                  {isStaged ? "Unstage Hunk" : "Stage Hunk"}
                </button>
              {/if}
            </div>
          {:else if line.type === "meta"}
            <div class="px-3 bg-surfaceHover/40 text-textMuted/70 text-[10px] italic flex items-center h-5 select-none overflow-x-auto" style="height: {ROW_HEIGHT}px;">
              <span class="whitespace-pre">{line.content}</span>
            </div>
          {:else if line.type === "binary"}
            <div class="px-3 bg-amber-500/10 text-amber-300/90 text-[11px] flex items-center gap-2 h-5 select-none overflow-x-auto" style="height: {ROW_HEIGHT}px;">
              <span class="shrink-0 rounded-sm bg-amber-500/20 px-1 font-sans">binary</span>
              <span class="whitespace-pre">{line.content}</span>
            </div>
          {:else if line.type === "add"}
            <div
              class="px-3 bg-green-500/15 text-green-300 flex items-center gap-2 hover:bg-green-500/25 overflow-x-auto {selectedLines.has(index) ? 'ring-1 ring-inset ring-accent/60 bg-green-500/25' : ''}"
              style="height: {ROW_HEIGHT}px;"
              role="group"
              onpointerdown={(e) => onLinePointerDown(index, e)}
              onpointerenter={() => onLinePointerEnter(index)}
              onpointerup={onLinePointerUp}
            >
              {#if isWorkingTreeFile}
                <button
                  onclick={(e) => { e.stopPropagation(); toggleLine(index); }}
                  onpointerdown={(e) => e.stopPropagation()}
                  class="w-3.5 h-3.5 flex items-center justify-center rounded border {selectedLines.has(index) ? 'bg-accent border-accent text-white' : 'border-border/60 hover:border-accent/80'} select-none shrink-0"
                  title={selectedLines.has(index) ? "Deselect line" : "Select line for patch staging"}
                >
                  {#if selectedLines.has(index)}
                    <Check size={10} />
                  {/if}
                </button>
              {/if}
              <span class="w-10 text-right text-textMuted/50 text-[10px] select-none shrink-0">{line.newNo ?? ""}</span>
              <span class="text-green-400 select-none font-bold shrink-0">+</span>
              <span class="whitespace-pre">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Added" ? "bg-green-500/40 text-green-100" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
            </div>
          {:else if line.type === "del"}
            <div
              class="px-3 bg-red-500/15 text-red-300 flex items-center gap-2 hover:bg-red-500/25 overflow-x-auto {selectedLines.has(index) ? 'ring-1 ring-inset ring-accent/60 bg-red-500/25' : ''}"
              style="height: {ROW_HEIGHT}px;"
              role="group"
              onpointerdown={(e) => onLinePointerDown(index, e)}
              onpointerenter={() => onLinePointerEnter(index)}
              onpointerup={onLinePointerUp}
            >
              {#if isWorkingTreeFile}
                <button
                  onclick={(e) => { e.stopPropagation(); toggleLine(index); }}
                  onpointerdown={(e) => e.stopPropagation()}
                  class="w-3.5 h-3.5 flex items-center justify-center rounded border {selectedLines.has(index) ? 'bg-accent border-accent text-white' : 'border-border/60 hover:border-accent/80'} select-none shrink-0"
                  title={selectedLines.has(index) ? "Deselect line" : "Select line for patch staging"}
                >
                  {#if selectedLines.has(index)}
                    <Check size={10} />
                  {/if}
                </button>
              {/if}
              <span class="w-10 text-right text-textMuted/50 text-[10px] select-none shrink-0">{line.oldNo ?? ""}</span>
              <span class="text-red-400 select-none font-bold shrink-0">-</span>
              <span class="whitespace-pre">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Removed" ? "bg-red-500/40 text-red-100" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
            </div>
          {:else}
            <div class="px-3 text-textPrimary/80 flex items-center gap-2 hover:bg-surfaceHover/40 overflow-x-auto" style="height: {ROW_HEIGHT}px;">
              {#if isWorkingTreeFile}
                <span class="w-3.5 shrink-0"></span>
              {/if}
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.oldNo ?? line.newNo ?? ""}</span>
              <span class="w-2 select-none shrink-0"></span>
              <span class="whitespace-pre">{line.content.startsWith(" ") ? line.content.substring(1) : line.content}</span>
            </div>
          {/if}
        {/if}
      {/snippet}
    </VirtualList>

    {#if isWorkingTreeFile && selectedLines.size > 0}
      <div class="p-2.5 border-t border-border/80 bg-surface flex items-center justify-between font-sans text-xs shrink-0 shadow-lg">
        <span class="text-textMuted font-mono text-[11px]">{selectedLines.size} line(s) selected for staging</span>
        <div class="flex items-center gap-2">
          <button onclick={() => selectedLines = new Set()} class="gp-btn-ghost !py-1 !text-xs">
            Clear
          </button>
          <button onclick={() => stageSelected(false)} class="gp-btn-ghost !py-1 !text-xs">
            Unstage Selected ({selectedLines.size})
          </button>
          <button onclick={() => stageSelected(true)} class="gp-btn-primary !py-1 !text-xs">
            <Check size={12} />
            <span>Stage Selected ({selectedLines.size})</span>
          </button>
        </div>
      </div>
    {/if}
  {:else}
    <VirtualList items={splitRows} rowHeight={ROW_HEIGHT} overscan={OVERSCAN} bind:scrollTop={splitScroll} class="flex-1 min-h-0 border-r border-border/80">
      {#snippet row(_, index)}
        {@const row = splitRow(index)}
        {#if row}
          {@const left = row.left}
          {@const right = row.right}
          <div class="grid grid-cols-2 divide-x divide-border" style="height: {ROW_HEIGHT}px;">
            <div class="px-3 flex items-center gap-2 overflow-x-auto {left ? (left.type === 'del' ? 'bg-red-500/15 text-red-300' : left.type === 'add' ? '' : left.type === 'meta' || left.type === 'binary' || left.type === 'hdr' ? 'bg-surfaceHover text-textMuted' : 'text-textPrimary/80') : ''}">
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{left?.oldNo ?? ""}</span>
              {#if left}
                <span class="whitespace-pre">{#if left.segments}{#each left.segments as seg}<span class={seg.kind === "Removed" ? "bg-red-500/40" : ""}>{seg.text}</span>{/each}{:else}{left.type === "add" || left.type === "del" ? left.content.substring(1) : left.content}{/if}</span>
              {/if}
            </div>
            <div class="px-3 flex items-center gap-2 overflow-x-auto {right ? (right.type === 'add' ? 'bg-green-500/15 text-green-300' : right.type === 'meta' || right.type === 'binary' || right.type === 'hdr' ? 'bg-surfaceHover text-textMuted italic' : 'text-textPrimary/80') : ''}">
              <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{right?.newNo ?? ""}</span>
              {#if right}
                <span class="whitespace-pre">{#if right.segments}{#each right.segments as seg}<span class={seg.kind === "Added" ? "bg-green-500/40" : ""}>{seg.text}</span>{/each}{:else}{right.type === "add" || right.type === "del" ? right.content.substring(1) : right.content}{/if}</span>
              {/if}
            </div>
          </div>
        {/if}
      {/snippet}
    </VirtualList>
  {/if}
</div>
{/if}
