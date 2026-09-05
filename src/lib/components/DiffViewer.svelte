<script lang="ts" module>
  // One cache per app: re-parses only when selectedDiff's string identity
  // changes, so unrelated store publications cost O(1) and the exact parsed
  // row objects survive (keeping memoized word-diff segments attached).
  import { createParseCache } from "../diff/wordDiff";
  import { densityStore } from "../stores/densityStore";
  import { rowHeight } from "../ui/density";

  const parseCache = createParseCache();
</script>

<script lang="ts">
  import type { FileBlob } from "../files/types";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import type { CommitFileChange } from "../stores/graphStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import DiffFileRail from "./DiffFileRail.svelte";
  import {
    buildFileRail,
    railPosition,
    stepFile,
    type RailEntry,
  } from "../diff/fileRail";
  import { buildCommitRail, type CommitEntry } from "../diff/commitRail";
  import { invoke } from "@tauri-apps/api/core";
  import {
    FileCode,
    Check,
    WrapText,
    ChevronUp,
    ChevronDown,
    PanelLeftOpen,
    Copy,
  } from "lucide-svelte";
  import LazyMount from "./LazyMount.svelte";
  // Only an image diff reaches this pane; it does not belong in the chunk
  // every launch parses.
  const loadImageDiffViewer = () => import("./ImageDiffViewer.svelte");
  import EmptyState from "./EmptyState.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import VirtualList from "./VirtualList.svelte";
  import {
    annotateRange,
    computeWordDiff,
    emptyDiffCopy,
    isImagePath,
    replacementBlockBounds,
    type AnnotatedDiffLine,
  } from "../diff/wordDiff";
  import {
    buildFilePatchForHunk,
    buildFilePatchFromLines,
  } from "../diff/patchBuilder";
  import { getImpact } from "../codeintel/client";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";
  import type { CodeintelEdge } from "../codeintel/types";

  // Fixed row geometry keeps the virtualized window math trivial and lets a
  // half-million-line agent diff render exactly like a twenty-line one.
  // Row height follows the Compact/Spacious setting like the branch list
  // and commit table already did; this pane used to ignore it entirely.
  let ROW_HEIGHT = $derived(rowHeight("diff", $densityStore));
  /**
   * How many diff lines may be wrapped at once.
   *
   * Wrapping makes rows variable-height, and variable-height rows cannot be
   * windowed against a fixed `rowHeight` — that is what produced overlapping
   * rows whose first and last wrapped segments were clipped away, hiding code
   * rather than reflowing it. So a wrapped diff renders in full, and the cap
   * is what keeps "renders in full" from meaning a hundred thousand rows.
   *
   * Diffs read closely enough to want wrapping are small; a diff larger than
   * this is being skimmed, and skimming works better unwrapped anyway.
   */
  const WRAP_MAX_LINES = 4_000;
  const OVERSCAN = 20;
  /** Beyond this, even the light parse stops being worth its memory. */
  const MAX_RENDER_LINES = 300_000;

  let viewMode = $state<"unified" | "split">("unified");
  /**
   * The file list travels with the diff.
   *
   * Before this, opening a commit's file switched to this view and left its
   * file list behind in the Graph view that owned it — reading a second file
   * meant going back, finding the commit again, and clicking the next row.
   */
  let railOpen = $state(true);
  let wordWrap = $state(false);
  let oldSrc = $state<string | null>(null);
  let newSrc = $state<string | null>(null);
  let selectedLines = $state<Set<number>>(new Set());
  let dragAnchor = $state<number | null>(null);
  let isDragging = $state(false);

  let impactEdges = $state<CodeintelEdge[]>([]);
  let impactAvailable = $state(false);

  $effect(() => {
    const repoPath = $repoStore.currentPath;
    const filePath = $repoStore.selectedFilePath;
    if (!repoPath || !filePath) {
      impactEdges = [];
      impactAvailable = false;
      return;
    }
    void getImpact(repoPath, filePath, 20).then((res) => {
      if (res.available) {
        impactEdges = res.items;
        impactAvailable = true;
      } else {
        impactEdges = [];
        impactAvailable = false;
      }
    }).catch(() => {
      impactEdges = [];
      impactAvailable = false;
    });
  });

  let allLines = $derived(parseCache.parse($repoStore.selectedDiff));
  // Two independent cuts, one meaning. The backend caps what it reads at its
  // payload budget; this view caps what it renders. Either one makes the rows
  // on screen a prefix, and every consequence — the notice, the staging
  // lockout — follows from that fact rather than from which cut caused it.
  let cutByBackend = $derived($repoStore.selectedDiffTruncated);
  let cutByRenderer = $derived(allLines.length > MAX_RENDER_LINES);
  let truncatedSource = $derived(cutByBackend || cutByRenderer);
  let lines = $derived(cutByRenderer ? allLines.slice(0, MAX_RENDER_LINES) : allLines);
  // Only add/del/ctx rows are diff lines: hdr/meta/binary are chrome, so the
  // "N lines" stat means what a diff reader expects it to mean (and an empty
  // parse — no rows at all — reaches the EmptyState branch).
  let contentLineCount = $derived(
    lines.reduce(
      (count, line) =>
        line.type === "add" || line.type === "del" || line.type === "ctx" ? count + 1 : count,
      0
    )
  );

  interface RailTick {
    key: string;
    topPct: number;
    heightPct: number;
    colorClass: string;
  }

  let railTicks = $derived.by<RailTick[]>(() => {
    if (lines.length === 0) return [];
    const ticks: RailTick[] = [];
    const total = lines.length;
    const step = Math.max(1, Math.floor(total / 80));

    for (let i = 0; i < total; i += step) {
      const slice = lines.slice(i, i + step);
      let adds = 0;
      let dels = 0;
      let hdrs = 0;
      for (const line of slice) {
        if (line.type === "add") adds++;
        else if (line.type === "del") dels++;
        else if (line.type === "hdr") hdrs++;
      }
      if (adds === 0 && dels === 0 && hdrs === 0) continue;

      let colorClass = "bg-accent/80";
      if (adds > dels && adds > 0) colorClass = "bg-emerald-500";
      else if (dels > adds && dels > 0) colorClass = "bg-rose-500";

      const topPct = (i / total) * 100;
      const heightPct = Math.max(1.2, (step / total) * 100);
      ticks.push({
        key: `tick-${i}`,
        topPct,
        heightPct,
        colorClass,
      });
    }
    return ticks;
  });

  function handleRailClick(e: MouseEvent) {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const clickY = e.clientY - rect.top;
    const ratio = Math.max(0, Math.min(1, clickY / rect.height));
    const targetScroll = ratio * (lines.length * ROW_HEIGHT);
    if (viewMode === "unified") {
      unifiedScroll = targetScroll;
    } else {
      splitScroll = targetScroll;
    }
  }

  let isWorkingTreeFile = $derived(
    $repoStore.selectedCommitId === null &&
      $repoStore.statuses.some((s) => s.path === $repoStore.selectedFilePath)
  );
  let isStaged = $derived($repoStore.selectedIsStaged);

  const details = $derived($graphStore.selectedCommitDetails);

  /**
   * The commit's files, fetched when the graph store does not already hold
   * them.
   *
   * The rail cannot depend on the Graph view having run first. A restored
   * session opens straight onto a persisted commit selection, and any future
   * caller that selects a commit file without going through
   * `graphStore.selectCommit` lands here too — in both cases the store is
   * empty and the rail would silently render nothing, which looks exactly
   * like a commit that touched one file.
   */
  let fetchedFiles = $state<{ commitId: string; files: CommitFileChange[] } | null>(null);
  let filesGuard: AsyncGuard | null = null;

  $effect(() => {
    const repo = $repoStore.currentPath;
    const commitId = $repoStore.selectedCommitId;
    // Nothing to fetch when there is no commit, or when the graph store's
    // details already cover this one.
    if (!repo || !commitId || details?.id === commitId) return;
    if (fetchedFiles?.commitId === commitId) return;
    filesGuard?.cancel();
    const guard = createAsyncGuard();
    filesGuard = guard;
    void (async () => {
      try {
        const files = await invoke<CommitFileChange[]>("cmd_get_commit_files", {
          repoPath: repo,
          commitId,
        });
        if (!guard.isLive()) return;
        fetchedFiles = { commitId, files };
      } catch {
        // The rail is an aid, not the content. A failed list leaves the diff
        // itself untouched and simply renders no rail, rather than pushing an
        // error banner over a file the reader can already see.
        if (guard.isLive()) fetchedFiles = { commitId, files: [] };
      }
    })();
  });

  $effect(() => () => filesGuard?.cancel());

  /** This commit's files from whichever source has them. */
  const commitFiles = $derived.by<CommitFileChange[] | null>(() => {
    const commitId = $repoStore.selectedCommitId;
    if (!commitId) return null;
    if (details?.id === commitId) return details.changed_files;
    if (fetchedFiles?.commitId === commitId) return fetchedFiles.files;
    return null;
  });

  const rail = $derived(
    buildFileRail({
      selectionKind: $repoStore.selectedCommitId
        ? "commit"
        : $repoStore.selectedFilePath
          ? "file"
          : "range",
      // Only this commit's own file list may be shown: a stale one from the
      // previously selected commit would offer files that are not in the diff
      // on screen.
      commitFiles,
      commitFilesTruncated: details?.files_list_truncated === true,
      commitFilesTotal: details?.files_total_count ?? 0,
      statuses: $repoStore.statuses,
    }),
  );
  const position = $derived(
    railPosition(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged),
  );
  const prevFile = $derived(
    stepFile(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged, -1),
  );
  const nextFile = $derived(
    stepFile(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged, 1),
  );

  /**
   * Alt+Arrow steps between files.
   *
   * Alt is the modifier because bare arrows scroll the diff and Cmd/Ctrl+Arrow
   * is the OS word/line jump; both are things a reader is doing inside the
   * diff already. Typing targets are excluded so the commit-message box and
   * the search field keep their own arrow behaviour.
   */
  function onWindowKeydown(event: KeyboardEvent): void {
    if (!event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    const target = event.target as HTMLElement | null;
    if (target?.isContentEditable) return;
    const tag = target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    const entry = event.key === "ArrowDown" ? nextFile : prevFile;
    if (!entry) return;
    event.preventDefault();
    openRailEntry(entry);
  }

  /** Recent commits, straight from the rows the graph already drew. */
  const commitRail = $derived(buildCommitRail($graphStore.rows));

  /**
   * Switches the diff to another commit, and opens one of its files.
   *
   * Selecting the commit alone would leave the pane showing the previous
   * commit's file, so the first changed file is opened with it — the reader
   * asked to look at this change, not to be told it is selected.
   */
  async function pickCommit(entry: CommitEntry): Promise<void> {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    let files = fetchedFiles?.commitId === entry.id ? fetchedFiles.files : null;
    if (!files) {
      try {
        files = await invoke<CommitFileChange[]>("cmd_get_commit_files", {
          repoPath: repo,
          commitId: entry.id,
        });
        fetchedFiles = { commitId: entry.id, files };
      } catch {
        files = [];
      }
    }
    const first = files[0];
    if (!first) {
      // A commit that changed nothing (an empty or a merge with no diff) has
      // no file to open; show the whole commit rather than nothing at all.
      void repoStore.selectCommitDiff(entry.id);
      return;
    }
    void repoStore.selectCommitFileDiff(entry.id, first.path);
  }

  /**
   * Returns to uncommitted work, opening its first changed file.
   *
   * A clean tree has nothing to open, so the button does nothing rather than
   * clearing the diff on screen — leaving the reader looking at a blank pane
   * they did not ask for is worse than leaving them where they were. The
   * entry's own "clean" badge already says why.
   */
  function pickWorkingTree(): void {
    const first = $repoStore.statuses[0];
    if (!first) return;
    void repoStore.selectFileDiff(first.path, first.is_staged);
  }

  /** Opens a rail entry through whichever command its source requires. */
  function openRailEntry(entry: RailEntry): void {
    const commitId = $repoStore.selectedCommitId;
    if (rail.source === "commit" && commitId) {
      void repoStore.selectCommitFileDiff(commitId, entry.path);
      return;
    }
    void repoStore.selectFileDiff(entry.path, entry.isStaged);
  }

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
    // A drag that starts on the code itself is a TEXT selection, not a
    // line-range selection. Both gestures live on the same row, so the split
    // is by where the pointer went down: the gutter (checkbox, line number,
    // +/- marker) drags a staging range, the text drags a selection. Without
    // this the row's preventDefault below suppresses native selection and the
    // diff stays uncopyable exactly where people copy from most.
    if ((event.target as Element | null)?.closest?.(".gp-diff-text")) return;
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

  /**
   * Copies the selected lines as plain source, without the +/- markers.
   *
   * The markers are diff notation, not part of the code — pasting them into an
   * editor makes the snippet uncompilable, which is the whole reason someone
   * copies a line out of a diff.
   */
  async function copySelectedLines() {
    if (selectedLines.size === 0) return;
    const text = [...selectedLines]
      .sort((a, b) => a - b)
      .map((index) => {
        const line = lines[index];
        if (!line) return "";
        return line.type === "add" || line.type === "del"
          ? line.content.slice(1)
          : line.content;
      })
      .join("\n");
    const copied = await copyText(text);
    if (copied) toastStore.success(`Copied ${selectedLines.size} line${selectedLines.size === 1 ? "" : "s"}`);
    else toastStore.error("Could not reach the clipboard");
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

  let lastBoundsBlock: { source: AnnotatedDiffLine[]; start: number; end: number } | null = null;

  function unifiedRow(index: number): AnnotatedDiffLine | undefined {
    const line = lines[index];
    if (!line) return undefined;
    const cached =
      lastBoundsBlock &&
      lastBoundsBlock.source === lines &&
      index >= lastBoundsBlock.start &&
      index < lastBoundsBlock.end
        ? lastBoundsBlock
        : null;
    const bounds = cached
      ? [cached.start, cached.end]
      : replacementBlockBounds(lines, index);
    if (!bounds) return line;
    if (!cached) lastBoundsBlock = { source: lines, start: bounds[0], end: bounds[1] };
    annotateRange(lines, bounds[0], bounds[1]);
    return line;
  }

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

  /**
   * Whether wrapping is offered at all for this diff.
   *
   * Counted over whichever list is on screen, because split rows pair lines
   * and are therefore fewer than the unified lines they came from.
   */
  const wrapRowCount = $derived(viewMode === "split" ? splitRows.length : lines.length);
  const wrapAvailable = $derived(wrapRowCount <= WRAP_MAX_LINES);
  /** Wrapping actually in effect — asked for, and permitted. */
  const wrapping = $derived(wordWrap && wrapAvailable);

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

  let viewKey = $state<string | null>(null);

  $effect(() => {
    const key = [
      $repoStore.selectedFilePath,
      $repoStore.selectedCommitId,
      $repoStore.selectedIsStaged,
      $repoStore.selectedIgnoreWhitespace,
    ].join("\u0000");
    if (key === viewKey) return;
    viewKey = key;
    selectedLines = new Set();
    isDragging = false;
    dragAnchor = null;
    unifiedScroll = 0;
    splitScroll = 0;
  });

  let imageBlobKey = $state<string | null>(null);

  $effect(() => {
    const path = $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    const commitId = $repoStore.selectedCommitId;
    const key =
      showingImage && path && repo ? `${repo}\u0000${path}\u0000${commitId ?? ""}` : null;
    if (key === imageBlobKey) return;
    imageBlobKey = key;
    if (!key) {
      oldSrc = null;
      newSrc = null;
      return;
    }
    const requestKey = key;
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
        if (imageBlobKey === requestKey) {
          newSrc = blobUrl(newBlob);
          oldSrc = blobUrl(oldBlob);
        }
      } catch {
        if (imageBlobKey === requestKey) {
          oldSrc = null;
          newSrc = null;
        }
      }
    })();
  });
</script>

<!-- Root-level: `svelte:window` may not sit inside a block, and the
     shortcut must work in the image-diff branch too. -->
<svelte:window onkeydown={onWindowKeydown} />

{#if showingImage}
  <LazyMount
    load={loadImageDiffViewer}
    name="The image comparison view"
    props={{ filePath: $repoStore.selectedFilePath || "image", oldSrc, newSrc }}
  />
{:else}
<div class="flex-1 flex flex-col bg-background h-full text-xs font-mono select-none overflow-hidden">
  <!-- Toolbar -->
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans shrink-0">
    <div class="flex gap-2 {wrapping ? 'items-start' : 'items-center'} truncate">
      {#if $repoStore.selectedFilePath}
        <LanguageLogo filePath={$repoStore.selectedFilePath} size={16} class="shrink-0" />
      {:else}
        <FileCode size={16} class="text-accent shrink-0" />
      {/if}
      <span class="font-medium text-textPrimary truncate">{ $repoStore.selectedFilePath || $repoStore.selectedCommitId || "Diff View" }</span>
      {#if contentLineCount > 0}
        <span class="text-[10px] text-textMuted shrink-0">{contentLineCount.toLocaleString()} lines</span>
      {/if}
      {#if impactAvailable && impactEdges.length > 0}
        <span
          class="text-[10px] px-2 py-0.5 rounded-full bg-accent/15 text-accent border border-accent/30 shrink-0 font-sans"
          title={`${impactEdges.length} downstream callers/dependencies affected by this file in devmap`}
        >
          {impactEdges.length} {impactEdges.length === 1 ? "affected caller" : "affected callers"}
        </span>
      {/if}
    </div>

    <div class="flex items-center gap-3">
      <!-- Step between the files of this commit (or of the working tree)
           without leaving the diff. Disabled rather than wrapping at the
           edges: silently jumping back to the first file reads as a broken
           button, not as the end of a list. -->
      {#if rail.entries.length > 1}
        <div class="flex items-center gap-1">
          <button
            type="button"
            class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
            disabled={!prevFile}
            onclick={() => prevFile && openRailEntry(prevFile)}
            title="Previous file (Alt+↑)"
            aria-label="Previous file"
          >
            <ChevronUp size={13} />
          </button>
          <span class="text-[10px] text-textMuted tabular-nums font-sans">
            {position.index || "–"}/{position.total}
          </span>
          <button
            type="button"
            class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
            disabled={!nextFile}
            onclick={() => nextFile && openRailEntry(nextFile)}
            title="Next file (Alt+↓)"
            aria-label="Next file"
          >
            <ChevronDown size={13} />
          </button>
        </div>
      {/if}
      {#if !railOpen && (rail.entries.length > 0 || commitRail.entries.length > 0)}
        <button
          type="button"
          class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-xs text-textMuted"
          onclick={() => (railOpen = true)}
          title="Show the file list"
        >
          <PanelLeftOpen size={13} />
          Files
        </button>
      {/if}

      <!-- Word Wrap Toggle -->
      <button
        type="button"
        onclick={() => (wordWrap = !wordWrap)}
        aria-pressed={wrapping}
        disabled={!wrapAvailable}
        title={wrapAvailable
          ? "Wrap long lines"
          : `Wrapping is unavailable above ${WRAP_MAX_LINES.toLocaleString()} lines — this diff has ${wrapRowCount.toLocaleString()}. Wrapped rows vary in height and cannot be windowed, so the whole diff would render at once.`}
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-xs disabled:opacity-40 {wrapping
          ? 'border-accent/60 text-accent font-semibold bg-accent/10'
          : 'text-textMuted'}"
        aria-label="Toggle word wrap"
      >
        <WrapText size={13} />
        <span class="hidden sm:inline">Wrap</span>
      </button>

      <!-- Ignore Whitespace -->
      <label class="flex items-center gap-1.5 text-textMuted cursor-pointer hover:text-textPrimary text-xs font-sans">
        <input
          type="checkbox"
          checked={$repoStore.selectedIgnoreWhitespace}
          onchange={(e) => repoStore.setIgnoreWhitespace(e.currentTarget.checked)}
          class="rounded bg-surface border-border text-accent focus:ring-0"
        />
        <span class="hidden sm:inline">Ignore Whitespace</span>
      </label>

      <!-- Unified / Split View Toggle -->
      <div class="gp-segmented" role="group" aria-label="Diff layout">
        <button
          onclick={() => (viewMode = "unified")}
          aria-pressed={viewMode === "unified"}
          data-active={viewMode === "unified" ? "true" : "false"}
          class="gp-seg-btn"
        >
          Unified
        </button>
        <button
          onclick={() => (viewMode = "split")}
          aria-pressed={viewMode === "split"}
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
    <div class="mx-3 mt-2 px-3 py-1.5 rounded-xl bg-amber-500/10 border border-amber-500/30 text-[11px] text-amber-600 dark:text-amber-300 font-sans flex gap-2 {wrapping ? 'items-start' : 'items-center'} shrink-0">
      <span>⚠</span>
      <span>
        {#if cutByBackend}
          This diff is larger than GitPulse reads in one go — showing the first
          {contentLineCount.toLocaleString()} lines. Open individual files from the
          rail to see the rest. Staging is disabled here because a partial diff
          would stage less than these rows show.
        {:else}
          Diff exceeds {MAX_RENDER_LINES.toLocaleString()} lines — showing the first {MAX_RENDER_LINES.toLocaleString()}. Use the filter bar or open specific files instead of one massive commit.
        {/if}
      </span>
    </div>
  {/if}

  {#if lines.length === 0}
    <EmptyState
      icon={FileCode}
      title={emptyCopy.title}
      hint={emptyCopy.hint}
    />
  {:else}
    <div class="flex-1 flex min-h-0 relative overflow-hidden">
      {#if railOpen && (rail.entries.length > 0 || commitRail.entries.length > 0)}
        <DiffFileRail
          {rail}
          commits={commitRail}
          currentPath={$repoStore.selectedFilePath}
          currentIsStaged={$repoStore.selectedIsStaged}
          selectedCommitId={$repoStore.selectedCommitId}
          workingTreeCount={$repoStore.statuses.length}
          onOpen={openRailEntry}
          onPickCommit={pickCommit}
          onPickWorkingTree={pickWorkingTree}
          onCollapse={() => (railOpen = false)}
        />
      {/if}
      <!-- Main Virtualized Diff Surface -->
      {#if viewMode === "unified"}
        <VirtualList items={lines} rowHeight={ROW_HEIGHT} virtualize={!wrapping} overscan={OVERSCAN} bind:scrollTop={unifiedScroll} class="flex-1 min-h-0">
          {#snippet row(_, index)}
            {@const line = unifiedRow(index)}
            {#if line}
              {#if line.type === "hdr"}
                <div class="px-3 bg-surfaceHover text-textMuted text-[11px] font-medium flex items-center justify-between h-5 overflow-x-auto" style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}>
                  <span class="truncate font-mono">{line.content}</span>
                  {#if isWorkingTreeFile && line.content.startsWith("@@")}
                    <button
                      onclick={() => stageHunk(index)}
                      disabled={truncatedSource}
                      title={truncatedSource
                        ? "Partial data — the diff is truncated, so staging would silently stage less than this hunk shows"
                        : undefined}
                      class="ml-2 shrink-0 px-2 py-0.5 text-[10px] rounded bg-surface border border-border/80 text-accent hover:bg-accent/15 transition-colors font-sans"
                    >
                      {isStaged ? "Unstage Hunk" : "Stage Hunk"}
                    </button>
                  {/if}
                </div>
              {:else if line.type === "meta"}
                <div class="px-3 bg-surfaceHover/40 text-textMuted/70 text-[10px] italic flex items-center h-5 select-none overflow-x-auto" style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}>
                  <span class="whitespace-pre">{line.content}</span>
                </div>
              {:else if line.type === "binary"}
                <div class="px-3 bg-amber-500/10 text-amber-700 dark:text-amber-300/90 text-[11px] flex gap-2 {wrapping ? 'items-start' : 'items-center'} h-5 select-none overflow-x-auto" style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}>
                  <span class="shrink-0 rounded-sm bg-amber-500/20 px-1 font-sans">binary</span>
                  <span class="whitespace-pre">{line.content}</span>
                </div>
              {:else if line.type === "add"}
                <div
                  class="px-3 bg-emerald-500/15 text-emerald-800 dark:text-emerald-300 flex gap-2 {wrapping ? 'items-start' : 'items-center'} hover:bg-emerald-500/25 {wrapping ? '' : 'overflow-x-auto'} {selectedLines.has(index) ? 'ring-1 ring-inset ring-accent/60 bg-emerald-500/25' : ''}"
                  style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}
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
                  <span class="text-emerald-600 dark:text-emerald-400 select-none font-bold shrink-0">+</span>
                  <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Added" ? "bg-emerald-500/35 text-emerald-950 dark:text-emerald-100 font-semibold px-0.5 rounded" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
                </div>
              {:else if line.type === "del"}
                <div
                  class="px-3 bg-rose-500/15 text-rose-800 dark:text-rose-300 flex gap-2 {wrapping ? 'items-start' : 'items-center'} hover:bg-rose-500/25 {wrapping ? '' : 'overflow-x-auto'} {selectedLines.has(index) ? 'ring-1 ring-inset ring-accent/60 bg-rose-500/25' : ''}"
                  style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}
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
                  <span class="text-rose-600 dark:text-rose-400 select-none font-bold shrink-0">-</span>
                  <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}">{#if line.segments}{#each line.segments as seg}<span class={seg.kind === "Removed" ? "bg-rose-500/35 text-rose-950 dark:text-rose-100 font-semibold px-0.5 rounded" : ""}>{seg.text}</span>{/each}{:else}{line.content.substring(1)}{/if}</span>
                </div>
              {:else}
                <div class="px-3 text-textPrimary/80 flex gap-2 {wrapping ? 'items-start' : 'items-center'} hover:bg-surfaceHover/40 {wrapping ? '' : 'overflow-x-auto'}" style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}>
                  {#if isWorkingTreeFile}
                    <span class="w-3.5 shrink-0"></span>
                  {/if}
                  <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.oldNo ?? line.newNo ?? ""}</span>
                  <span class="w-2 select-none shrink-0"></span>
                  <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}">{line.content.startsWith(" ") ? line.content.substring(1) : line.content}</span>
                </div>
              {/if}
            {/if}
          {/snippet}
        </VirtualList>
      {:else}
        <VirtualList items={splitRows} rowHeight={ROW_HEIGHT} virtualize={!wrapping} overscan={OVERSCAN} bind:scrollTop={splitScroll} class="flex-1 min-h-0 border-r border-border/80">
          {#snippet row(_, index)}
            {@const row = splitRow(index)}
            {#if row}
              {@const left = row.left}
              {@const right = row.right}
              <div class="grid grid-cols-2 divide-x divide-border" style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}>
                <div class="px-3 flex min-w-0 gap-2 {wrapping ? 'items-start' : 'items-center'} {wrapping ? '' : 'overflow-x-auto'} {left ? (left.type === 'del' ? 'bg-rose-500/15 text-rose-800 dark:text-rose-300' : left.type === 'add' ? '' : left.type === 'meta' || left.type === 'binary' || left.type === 'hdr' ? 'bg-surfaceHover text-textMuted' : 'text-textPrimary/80') : ''}">
                  <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{left?.oldNo ?? ""}</span>
                  {#if left}
                    <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}">{#if left.segments}{#each left.segments as seg}<span class={seg.kind === "Removed" ? "bg-rose-500/35 text-rose-950 dark:text-rose-100 font-semibold px-0.5 rounded" : ""}>{seg.text}</span>{/each}{:else}{left.type === "add" || left.type === "del" ? left.content.substring(1) : left.content}{/if}</span>
                  {/if}
                </div>
                <div class="px-3 flex min-w-0 gap-2 {wrapping ? 'items-start' : 'items-center'} {wrapping ? '' : 'overflow-x-auto'} {right ? (right.type === 'add' ? 'bg-emerald-500/15 text-emerald-800 dark:text-emerald-300' : right.type === 'meta' || right.type === 'binary' || right.type === 'hdr' ? 'bg-surfaceHover text-textMuted italic' : 'text-textPrimary/80') : ''}">
                  <span class="w-10 text-right text-textMuted/40 text-[10px] select-none shrink-0">{right?.newNo ?? ""}</span>
                  {#if right}
                    <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}">{#if right.segments}{#each right.segments as seg}<span class={seg.kind === "Added" ? "bg-emerald-500/35 text-emerald-950 dark:text-emerald-100 font-semibold px-0.5 rounded" : ""}>{seg.text}</span>{/each}{:else}{right.type === "add" || right.type === "del" ? right.content.substring(1) : right.content}{/if}</span>
                  {/if}
                </div>
              </div>
            {/if}
          {/snippet}
        </VirtualList>
      {/if}

      <!-- Hunk Rail / Diff Minimap -->
      {#if railTicks.length > 0}
        <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
        <!-- Accepted, not fixed: the minimap is a pointer-only shortcut for
             scrolling the virtual list beside it, and is marked
             role="presentation" so assistive tech skips it. Keyboard users
             scroll the list itself. Giving the rail its own key bindings
             would add a second way to do one thing; if the list ever stops
             being keyboard-scrollable, this becomes a real gap. -->
        <div
          class="w-3 bg-surface/90 border-l border-border/70 relative h-full shrink-0 cursor-pointer overflow-hidden group/rail hover:w-4 transition-[width] duration-150"
          title="Diff Minimap (click to navigate)"
          onclick={handleRailClick}
          role="presentation"
        >
          {#each railTicks as tick (tick.key)}
            <div
              class="absolute left-0.5 right-0.5 rounded-sm {tick.colorClass} shadow-sm"
              style="top: {tick.topPct}%; height: {tick.heightPct}%;"
            ></div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- Selection Bar -->
    {#if isWorkingTreeFile && selectedLines.size > 0}
      <div class="p-2.5 border-t border-border/80 bg-surface flex items-center justify-between font-sans text-xs shrink-0 shadow-lg">
        <span class="text-textMuted font-mono text-[11px]">{selectedLines.size} line(s) selected for staging</span>
        <div class="flex gap-2 {wrapping ? 'items-start' : 'items-center'}">
          <button onclick={() => selectedLines = new Set()} class="gp-btn !py-1 !text-xs">
            Clear
          </button>
          <button onclick={copySelectedLines} class="gp-btn !py-1 !text-xs" title="Copy the selected lines without their diff markers">
            <Copy size={12} />
            <span>Copy</span>
          </button>
          <button onclick={() => stageSelected(false)} class="gp-btn !py-1 !text-xs">
            Unstage Selected ({selectedLines.size})
          </button>
          <button onclick={() => stageSelected(true)} class="gp-btn-primary !py-1 !text-xs">
            <Check size={12} />
            <span>Stage Selected ({selectedLines.size})</span>
          </button>
        </div>
      </div>
    {/if}
  {/if}
</div>
{/if}
