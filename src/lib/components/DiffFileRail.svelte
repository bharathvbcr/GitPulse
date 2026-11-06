<script lang="ts">
  /**
   * The file list that stays beside the diff.
   *
   * Presentational on purpose: it takes a built rail and reports which entry
   * was clicked. The Diff view owns which store call that turns into, because
   * a commit file and a working-tree file open through different commands and
   * only the view knows which selection is live.
   *
   * What changed, and why: the old rail printed the basename of every entry
   * into a fixed 224px column with no filter and no grouping. On this
   * repository's own head commit that is `marketplace.json` twice,
   * `plugin.json` three times and `mod.rs` eight times — two hundred rows
   * that cannot be told apart, every one of them a tab stop in the DOM. Rows
   * are labelled by `railRows` now (shortest unique path suffix, or the
   * shared explorer's tree), filtered, and windowed.
   *
   * Two tabs share the column: Files (which file of this change) and History
   * (which change: the commit graph, rendered by the caller's `history`
   * snippet). One tab at a time owns the whole height, so the column never
   * holds two scrollers competing for one wheel event.
   */
  import type { Snippet } from "svelte";
  import {
    ChevronDown,
    ChevronRight,
    FileCode,
    Filter,
    GitBranch,
    GitCommitHorizontal,
    GitCompareArrows,
    ListTree,
    PanelLeftClose,
    Rows3,
    X,
  } from "@lucide/svelte";
  import { churnLabel, truncationNote, type FileRail, type RailEntry } from "../diff/fileRail";
  import {
    activeRowIndex,
    buildRailRows,
    filterNote,
    type RailMode,
    type RailRow,
  } from "../diff/railRows";
  import type { RailTab } from "../diff/historyRail";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import VirtualList from "./VirtualList.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import type { PreviewMarker } from "../codeintel/previewSummary";

  let {
    rail,
    currentPath,
    currentIsStaged,
    changeLabel,
    comparing = null,
    onExitCompare,
    onOpen,
    onCollapse,
    width = 248,
    onResize,
    tab = $bindable<RailTab>("files"),
    history,
    /** A1 markers from the shared preview store — not a second query path. */
    previewMarkers = null,
  }: {
    rail: FileRail;
    currentPath: string | null;
    currentIsStaged: boolean;
    /** What is on screen, in words: a commit's summary, or uncommitted work. */
    changeLabel: string;
    /** `base..target` while a comparison is on screen; null otherwise. */
    comparing?: string | null;
    /** Leaves the comparison for its target commit. */
    onExitCompare?: () => void;
    onOpen: (entry: RailEntry) => void;
    onCollapse: () => void;
    width?: number;
    onResize?: (next: number) => void;
    /**
     * Which tab is showing. Owned by the caller so it survives a file switch
     * and so a shortcut outside the rail can flip it.
     */
    tab?: RailTab;
    /** The History tab's body; the tab is offered only when one is given. */
    history?: Snippet;
    previewMarkers?: Map<string, PreviewMarker> | null;
  } = $props();

  /** Row height for the windowed file list; must match the row's own height. */
  const ROW_HEIGHT = 22;
  /**
   * Below this the list fits on screen and windowing only costs a wrapper.
   * Above it, every row is a DOM node and a tab stop.
   */
  const VIRTUALIZE_ABOVE = 60;

  /** The tab strip wears the header's glass pill on macOS, like every other tablist. */
  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  let query = $state("");
  let mode = $state<RailMode>("list");
  let collapsed = $state<Set<string>>(new Set());
  let listScroll = $state(0);
  let fileListEl: HTMLUListElement | undefined = $state();

  const note = $derived(truncationNote(rail));
  const showingHistory = $derived(tab === "history" && history !== undefined);
  const filesHeading = $derived(
    rail.source === "commit"
      ? "Commit files"
      : rail.source === "compare"
        ? "Compared files"
        : "Changed files",
  );

  const result = $derived(
    buildRailRows({
      entries: rail.entries,
      mode,
      query,
      isCollapsed: (dir) => collapsed.has(dir),
    }),
  );
  const rows = $derived(result.rows);
  const activeIndex = $derived(
    activeRowIndex(rows, currentPath, currentIsStaged, rail.source === "worktree"),
  );
  const searchNote = $derived(filterNote(result, query));
  const virtualize = $derived(rows.length > VIRTUALIZE_ABOVE);

  /**
   * Keeps the open file in view when the selection moves without a click —
   * the Alt+Arrow stepper, or a jump from another pane. Only follows the
   * selection, never the reader's own scrolling.
   */
  /** Last followed row. A plain `let`, never $state: the follow
   *  effect reads and writes it, and a reactive read there would
   *  recreate a self-dependency. */
  let lastFollowed = -1;
  $effect(() => {
    const index = activeIndex;
    if (index < 0 || index === lastFollowed) return;
    lastFollowed = index;
    if (!virtualize) return;
    listScroll = Math.max(0, index * ROW_HEIGHT - ROW_HEIGHT * 4);
  });

  function toggleDir(path: string): void {
    const next = new Set(collapsed);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    collapsed = next;
  }

  /**
   * Status letters get a colour so the list scans without reading every row.
   * Unknown letters fall through to muted rather than to a colour that would
   * assert a meaning this build does not know.
   */
  function statusTone(code: string): string {
    switch (code.charAt(0)) {
      case "A":
        return "text-emerald-600 dark:text-emerald-400";
      case "D":
        return "text-rose-600 dark:text-rose-400";
      case "R":
      case "C":
        return "text-sky-600 dark:text-sky-400";
      case "M":
        return "text-amber-600 dark:text-amber-400";
      case "?":
        return "text-textMuted";
      default:
        return "text-textMuted";
    }
  }

  function isActiveRow(row: RailRow, index: number): boolean {
    return row.kind === "file" && index === activeIndex;
  }

  // --- resize handle -------------------------------------------------------
  const MIN_WIDTH = 180;
  const MAX_WIDTH = 520;
  let dragging = $state(false);

  function startResize(event: PointerEvent): void {
    if (!onResize) return;
    dragging = true;
    const startX = event.clientX;
    const startWidth = width;
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture?.(event.pointerId);
    const move = (e: PointerEvent) => {
      const next = Math.round(startWidth + (e.clientX - startX));
      onResize?.(Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, next)));
    };
    const stop = () => {
      dragging = false;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  }

  /** Keyboard resizing, because a drag handle alone is a pointer-only control. */
  function resizeKey(event: KeyboardEvent): void {
    if (!onResize) return;
    const step = event.shiftKey ? 40 : 12;
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      onResize(Math.max(MIN_WIDTH, width - step));
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      onResize(Math.min(MAX_WIDTH, width + step));
    } else if (event.key === "Home") {
      // Home/End jump to the extremes: the window-splitter pattern's answer to
      // "narrowest" and "widest", which arrow-stepping alone makes a chore.
      event.preventDefault();
      onResize(MIN_WIDTH);
    } else if (event.key === "End") {
      event.preventDefault();
      onResize(MAX_WIDTH);
    }
  }
</script>

<aside
  class="relative flex shrink-0 flex-col border-r border-border/60 bg-surface/40 font-sans"
  style="width: {width}px"
  aria-label="Files in this diff"
>
  <!-- Which tab, and which change is on screen. The change line answers
       "what am I looking at" from either tab, so neither has to be opened to
       find out. -->
  <div class="flex items-center gap-1.5 border-b border-border/60 gp-section-edge px-2 py-1">
    {#if history}
      <div class="gp-segmented p-0.5!" class:gp-liquid-tabs={macos} role="tablist" aria-label="File rail">
        <button
          type="button"
          role="tab"
          class="gp-seg-btn flex items-center gap-1 px-1.5! py-0.5! text-[10px]"
          aria-selected={!showingHistory}
          data-active={!showingHistory ? "true" : "false"}
          title="Files in this change"
          onclick={() => (tab = "files")}
        >
          {#if macos && !showingHistory}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: "rail-tab" }}
              out:sendSelection={{ key: "rail-tab" }}
            ></span>
          {/if}
          <span class="flex items-center gap-1">
            <FileCode size={11} />
            Files
            <span class="tabular-nums opacity-70">{rail.entries.length}</span>
          </span>
        </button>
        <button
          type="button"
          role="tab"
          class="gp-seg-btn flex items-center gap-1 px-1.5! py-0.5! text-[10px]"
          aria-selected={showingHistory}
          data-active={showingHistory ? "true" : "false"}
          title="Commit history — pick or compare commits ({shortcutTextLabel('⌥H', $hostPlatform.os)})"
          onclick={() => (tab = "history")}
          data-history-tab
        >
          {#if macos && showingHistory}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: "rail-tab" }}
              out:sendSelection={{ key: "rail-tab" }}
            ></span>
          {/if}
          <span class="flex items-center gap-1">
            <GitBranch size={11} />
            History
          </span>
        </button>
      </div>
    {:else}
      <FileCode size={12} class="shrink-0 text-accent" />
      <span class="text-[11px] font-semibold text-textPrimary">{filesHeading}</span>
    {/if}
    <span class="flex-1"></span>
    <button
      type="button"
      class="gp-icon-btn p-1!"
      onclick={onCollapse}
      title="Hide the file list"
      aria-label="Hide the file list"
    >
      <PanelLeftClose size={12} />
    </button>
  </div>

  <div
    class="flex items-center gap-1.5 border-b border-border/60 px-2.5 py-1 text-[10px]"
    data-change-line
  >
    {#if comparing}
      <GitCompareArrows size={11} class="shrink-0 text-accent" />
      <span class="min-w-0 flex-1 truncate text-textPrimary" title="Comparing {comparing}">
        Comparing <span class="font-mono">{comparing}</span>
      </span>
      {#if onExitCompare}
        <button
          type="button"
          class="gp-icon-btn p-0.5!"
          title="Stop comparing and show the newer commit alone"
          aria-label="Stop comparing"
          onclick={onExitCompare}
          data-exit-compare
        >
          <X size={11} />
        </button>
      {/if}
    {:else}
      <GitCommitHorizontal size={11} class="shrink-0 text-accent" />
      <span class="min-w-0 flex-1 truncate text-textMuted" title={changeLabel}>{changeLabel}</span>
    {/if}
  </div>

  {#if showingHistory && history}
    {@render history()}
  {:else}
  <div class="flex items-center gap-1.5 border-b border-border/60 gp-section-edge px-2.5 py-1.5">
    <FileCode size={12} class="shrink-0 text-accent" />
    <span class="text-[11px] font-semibold text-textPrimary">
      {filesHeading}
    </span>
    <span class="ml-auto text-[10px] tabular-nums text-textMuted">{rail.entries.length}</span>
    <div class="gp-segmented p-0.5!" role="group" aria-label="File list layout">
      <button
        type="button"
        class="gp-seg-btn px-1.5! py-0.5!"
        aria-pressed={mode === "list"}
        data-active={mode === "list" ? "true" : "false"}
        title="Flat list, in the order git reports"
        onclick={() => (mode = "list")}
      >
        <Rows3 size={11} />
      </button>
      <button
        type="button"
        class="gp-seg-btn px-1.5! py-0.5!"
        aria-pressed={mode === "tree"}
        data-active={mode === "tree" ? "true" : "false"}
        title="Group by directory"
        onclick={() => (mode = "tree")}
      >
        <ListTree size={11} />
      </button>
    </div>
  </div>

  <div class="flex items-center gap-1 border-b border-border/60 gp-section-edge px-2 py-1">
    <Filter size={11} class="shrink-0 text-textMuted" />
    <input
      bind:value={query}
      type="text"
      class="min-w-0 flex-1 bg-transparent py-0.5 text-[11px] text-textPrimary outline-hidden placeholder:text-textMuted/60"
      placeholder="Filter files…"
      aria-label="Filter files in this diff"
      onkeydown={(e) => {
        if (e.key === "Escape" && query) {
          e.stopPropagation();
          query = "";
        }
      }}
    />
    {#if query}
      <button
        type="button"
        class="gp-icon-btn p-0.5!"
        onclick={() => (query = "")}
        title="Clear the filter"
        aria-label="Clear the filter"
      >
        <X size={11} />
      </button>
    {/if}
  </div>

  <!-- A cut-short list must say so. Rendering the first fifty of three hundred
       as though they were all of them tells the reader they have seen the
       whole commit. -->
  {#if note}
    <p class="border-b border-border/60 px-2.5 py-1 text-[10px] text-amber-600 dark:text-amber-400">
      {note}
    </p>
  {/if}
  {#if searchNote}
    <p class="border-b border-border/60 px-2.5 py-1 text-[10px] text-textMuted">{searchNote}</p>
  {/if}

  {#snippet railRow(row: RailRow | undefined, index: number)}
    {#if row?.kind === "dir"}
      <button
        type="button"
        class="flex w-full items-center gap-1 py-0.5 pr-2 text-left text-[11px] text-textMuted hover:bg-surfaceHover"
        style="height: {ROW_HEIGHT}px; padding-left: {8 + row.depth * 10}px"
        aria-expanded={!collapsed.has(row.path)}
        title={row.path}
        onclick={() => toggleDir(row.path)}
      >
        {#if collapsed.has(row.path)}
          <ChevronRight size={11} class="shrink-0" />
        {:else}
          <ChevronDown size={11} class="shrink-0" />
        {/if}
        <span class="min-w-0 flex-1 truncate font-medium">{row.name}</span>
        <span class="shrink-0 font-mono text-[9px] opacity-70">{row.fileCount}</span>
      </button>
    {:else if row?.kind === "file"}
      {@const active = isActiveRow(row, index)}
      {@const churn = churnLabel(row.entry)}
      {@const marker = previewMarkers?.get(row.entry.path) ?? null}
      <button
        type="button"
        class="flex w-full items-center gap-1.5 py-0.5 pr-2 text-left text-[11px] hover:bg-surfaceHover {active
          ? 'bg-accent/10 text-accent'
          : 'text-textMuted'}"
        style="height: {ROW_HEIGHT}px; padding-left: {8 + row.depth * 10}px"
        aria-current={active ? "true" : undefined}
        title={marker ? `${row.title} — ${marker.title}` : row.title}
        onclick={() => onOpen(row.entry)}
      >
        <span class="w-3 shrink-0 font-mono text-[10px] {statusTone(row.entry.statusCode)}">
          {row.entry.statusCode.charAt(0) || "?"}
        </span>
        <span class="flex min-w-0 flex-1 items-baseline gap-0.5 truncate">
          {#if row.dir}
            <span class="min-w-0 shrink truncate text-[10px] opacity-55">{row.dir}/</span>
          {/if}
          <span class="shrink-0 truncate">{row.name}</span>
        </span>
        {#if marker && marker.kind !== "clean"}
          <span
            class="shrink-0 rounded px-0.5 font-mono text-[9px] tabular-nums {marker.kind === 'breaks'
              ? 'bg-rose-500/15 text-rose-600 dark:text-rose-400'
              : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'}"
            title={marker.title}
            data-preview-marker={marker.kind}
          >
            {marker.label}
          </span>
        {/if}
        {#if rail.source === "worktree" && row.entry.isStaged}
          <span class="shrink-0 text-[9px] text-emerald-600 dark:text-emerald-400">staged</span>
        {/if}
        {#if churn}
          <span class="shrink-0 font-mono text-[9px] tabular-nums text-textMuted">{churn}</span>
        {/if}
      </button>
    {/if}
  {/snippet}

  {#if rows.length === 0}
    <p class="px-2.5 py-3 text-[11px] text-textMuted">
      {rail.entries.length === 0 ? "No files in this change." : "No file matches the filter."}
    </p>
  {:else if virtualize}
    <VirtualList
      items={rows}
      rowHeight={ROW_HEIGHT}
      overscan={12}
      bind:scrollTop={listScroll}
      scrollCue
      class="min-h-0 flex-1 py-1"
    >
      {#snippet row(item, index)}
        {@render railRow(item, index)}
      {/snippet}
    </VirtualList>
  {:else}
    <div class="relative min-h-0 flex-1">
    <ul bind:this={fileListEl} class="min-h-0 h-full overflow-y-auto gp-scroll py-1">
      {#each rows as row, index (row.key)}
        <li>{@render railRow(row, index)}</li>
      {/each}
    </ul>
    <ScrollCue target={fileListEl} axis="y" />
    </div>
  {/if}
  {/if}

  {#if onResize}
    <!-- Sits on the border, so a 224px column stops being a fixed cost for
         every repository whose paths are longer than 224px. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: a focusable `separator` IS the ARIA window-splitter
         pattern — it takes focus, reports aria-valuenow/min/max, moves on the
         arrow keys and jumps to its extremes on Home/End. svelte-check
         classifies every separator as non-interactive, which is true only of
         the decorative kind. -->
    <div
      class="absolute inset-y-0 -right-1 z-10 w-2 cursor-col-resize {dragging
        ? 'bg-accent/40'
        : 'hover:bg-accent/25'}"
      role="separator"
      aria-label="Resize the file list"
      aria-orientation="vertical"
      aria-valuenow={width}
      aria-valuemin={MIN_WIDTH}
      aria-valuemax={MAX_WIDTH}
      tabindex="0"
      onpointerdown={startResize}
      onkeydown={resizeKey}
    ></div>
  {/if}
</aside>
