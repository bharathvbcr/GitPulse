<script lang="ts">
  import type { Snippet } from "svelte";
  import type { Attachment } from "svelte/attachments";
  import { onDestroy } from "svelte";
  import { get } from "svelte/store";
  import { observeResize } from "../../dom/observeResize";
  import {
    FILE_TREE_DEFAULT_WIDTH,
    FILE_TREE_MIN_WIDTH,
    FILE_TREE_RESIZE_STEP,
    clampFileTreeWidth,
    fileTreeColumn,
    fileTreeMaxWidth,
  } from "../../files/fileTreeColumn";

  let {
    children,
    reserve = FILE_TREE_MIN_WIDTH,
  }: {
    children: Snippet;
    /**
     * Pixels the rest of this row must keep. Explorer passes the editor
     * minimum (and the dashboard, when it is open). Blame keeps the default,
     * a floor for the annotation list.
     */
    reserve?: number;
  } = $props();

  let paneWidth = $state(0);
  let dragging = $state(false);
  /** A pointerup after a real drag. A stationary double-click still resets. */
  let lastDragEndAt = 0;
  let dragStartX = 0;
  let dragStartWidth = 0;
  let dragMoved = false;
  let lastPaneWidth = 0;
  let lastReserve = FILE_TREE_MIN_WIDTH;

  let preferred = $derived($fileTreeColumn);
  let displayed = $derived(clampFileTreeWidth(preferred, paneWidth, reserve));
  let maxWidth = $derived(
    paneWidth > 0 ? fileTreeMaxWidth(paneWidth, reserve) : Math.max(displayed, FILE_TREE_MIN_WIDTH),
  );

  // The row's width is layout, not a value we can derive. The attachment
  // measures it and drops the observer when this column unmounts.
  const watchRow: Attachment<HTMLDivElement> = (element) => {
    const pane = element.parentElement;
    if (!pane) return;
    const measure = () => {
      paneWidth = pane.clientWidth;
    };
    measure();
    return observeResize(pane, measure);
  };

  function rememberPane() {
    lastPaneWidth = paneWidth;
    lastReserve = reserve;
  }

  function startResize(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    const target = event.currentTarget;
    if (!(target instanceof HTMLElement)) return;
    target.setPointerCapture(event.pointerId);
    dragging = true;
    dragMoved = false;
    dragStartX = event.clientX;
    dragStartWidth = displayed;
    rememberPane();
  }

  function moveResize(event: PointerEvent) {
    if (!dragging) return;
    rememberPane();
    if (Math.abs(event.clientX - dragStartX) >= 3) dragMoved = true;
    fileTreeColumn.setWidthLive(dragStartWidth + (event.clientX - dragStartX), paneWidth, reserve);
  }

  function endResize(event: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const moved = dragMoved;
    dragMoved = false;
    if (moved) {
      lastDragEndAt = Date.now();
      fileTreeColumn.setWidth(
        dragStartWidth + (event.clientX - dragStartX),
        lastPaneWidth,
        lastReserve,
      );
    }
    const target = event.currentTarget;
    if (target instanceof HTMLElement && target.hasPointerCapture?.(event.pointerId)) {
      target.releasePointerCapture(event.pointerId);
    }
  }

  function resizeKeydown(event: KeyboardEvent) {
    let next: number | null = null;
    if (event.key === "ArrowLeft") next = displayed - FILE_TREE_RESIZE_STEP;
    else if (event.key === "ArrowRight") next = displayed + FILE_TREE_RESIZE_STEP;
    else if (event.key === "Home") next = FILE_TREE_MIN_WIDTH;
    else if (event.key === "End") next = maxWidth;
    if (next === null) return;
    event.preventDefault();
    rememberPane();
    fileTreeColumn.setWidth(next, paneWidth, reserve);
  }

  function resetWidth() {
    // The sidebar's guard: a dblclick that the browser fires at the end of a
    // drag must not snap back to the default. A double-click that never
    // moved still does — that pointerup does not stamp lastDragEndAt.
    if (Date.now() - lastDragEndAt <= 250) return;
    rememberPane();
    fileTreeColumn.setWidth(FILE_TREE_DEFAULT_WIDTH, paneWidth, reserve);
  }

  onDestroy(() => {
    if (!dragMoved) return;
    fileTreeColumn.setWidth(get(fileTreeColumn), lastPaneWidth, lastReserve);
  });
</script>

<!-- Explorer and Blame each mount one of these. The width lives in
     `fileTreeColumn`, so dragging either handle is the same write. -->
<div
  {@attach watchRow}
  data-file-tree-column
  class="relative h-full shrink-0 overflow-hidden"
  style="width: {displayed}px; min-width: {FILE_TREE_MIN_WIDTH}px; max-width: 50%"
>
  {@render children()}

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- Justified: the WAI-ARIA window-splitter pattern, the same one the
       sidebar and the diff file rail use — a focusable `separator` with
       aria-valuenow/min/max. The rule treats every separator as
       non-interactive, which is true of the decorative ones. -->
  <div
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize file tree"
    tabindex="0"
    aria-valuemin={FILE_TREE_MIN_WIDTH}
    aria-valuemax={maxWidth}
    aria-valuenow={displayed}
    title="Drag to resize · double-click to reset · ←/→ to nudge"
    class="absolute inset-y-0 right-0 z-10 w-2 cursor-col-resize touch-none hover:bg-accent/40 focus-visible:bg-accent/50 focus-visible:outline-hidden {dragging
      ? 'bg-accent/50'
      : ''}"
    onpointerdown={startResize}
    onpointermove={moveResize}
    onpointerup={endResize}
    onpointercancel={endResize}
    onkeydown={resizeKeydown}
    ondblclick={resetWidth}
  ></div>
</div>
