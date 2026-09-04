<script lang="ts">
  import { onDestroy } from "svelte";
  import { ChevronDown, SquareTerminal } from "lucide-svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import {
    TERMINAL_DOCK_MAX_HEIGHT,
    TERMINAL_DOCK_MIN_HEIGHT,
    TERMINAL_DOCK_RESIZE_STEP,
    fitTerminalDockHeight,
  } from "../terminal/dockMetrics";

  /**
   * The terminal, docked beneath the active view.
   *
   * Two things this shape gets right that a view could not. A PTY has to
   * outlive a view switch, so the pane was already mounted once and hidden
   * thereafter — never really a page. And a command's output is worth reading
   * *against* something: a Health remediation, a failing test, the diff you
   * are about to commit. A full-screen terminal hid all of it.
   *
   * Mounted on first open and kept mounted after, because unmounting kills
   * the shell. Closing hides it; only a repository switch (App's `{#key}`)
   * tears the session down.
   */

  let {
    open,
    onClose,
    load,
  }: {
    open: boolean;
    onClose: () => void;
    /** Loader for TerminalPanel, declared at module scope by the caller. */
    load: ViewLoader;
  } = $props();

  // svelte-ignore state_referenced_locally
  // Justified: capturing only the initial value of `open` is the intent. This
  // latch answers "has the dock ever been open" — a one-way question. The
  // effect below handles every later open, and nothing may set it back to
  // false while a shell is running.
  let mounted = $state(open);
  $effect(() => {
    if (open) mounted = true;
  });

  let host: HTMLDivElement | undefined = $state();
  let dragging = $state(false);
  /** Height of the column the dock shares with the view, for the ceiling. */
  let containerHeight = $state(0);

  // The stored height is a request; what renders also respects the room the
  // window actually has, so a dock sized on a large display cannot swallow
  // the view when the same preference is restored on a small one.
  let height = $derived(
    fitTerminalDockHeight($interfaceStore.terminalDockHeight, containerHeight),
  );

  $effect(() => {
    const parent = host?.parentElement;
    if (!parent || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      containerHeight = entry.contentRect.height;
    });
    observer.observe(parent);
    return () => observer.disconnect();
  });

  function startDrag(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = height;
    dragging = true;

    // Pointer capture, not window listeners: a drag that leaves the window
    // still ends on pointerup, and nothing else on the page sees the moves.
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);

    const move = (moveEvent: PointerEvent) => {
      // Dragging up grows the dock, so the delta is inverted.
      interfaceStore.setTerminalDockHeight(startHeight + (startY - moveEvent.clientY));
    };
    const end = () => {
      dragging = false;
      handle.releasePointerCapture(event.pointerId);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  }

  function handleSeparatorKey(event: KeyboardEvent) {
    if (event.key === "ArrowUp") {
      event.preventDefault();
      interfaceStore.setTerminalDockHeight(height + TERMINAL_DOCK_RESIZE_STEP);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      interfaceStore.setTerminalDockHeight(height - TERMINAL_DOCK_RESIZE_STEP);
    }
  }

  onDestroy(() => {
    dragging = false;
  });
</script>

{#if mounted}
  <!-- Hidden, never unmounted: display:none pauses rendering, not the shell. -->
  <div
    bind:this={host}
    class="shrink-0 flex flex-col border-t border-border bg-background"
    class:hidden={!open}
    style="height: {height}px"
    data-terminal-dock
  >
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: the WAI-ARIA window-splitter pattern, the same one the
         sidebar's resize handle uses — a focusable `separator` carrying
         aria-valuenow/min/max, which the spec defines as interactive
         precisely because it is focusable. The rule treats every `separator`
         as non-interactive. -->
    <div
      role="separator"
      aria-orientation="horizontal"
      aria-label="Resize the terminal dock"
      aria-valuenow={height}
      aria-valuemin={TERMINAL_DOCK_MIN_HEIGHT}
      aria-valuemax={TERMINAL_DOCK_MAX_HEIGHT}
      title="Drag to resize · ↑/↓ to nudge"
      tabindex="0"
      onpointerdown={startDrag}
      onkeydown={handleSeparatorKey}
      class="h-1.5 shrink-0 cursor-row-resize hover:bg-accent/40 focus-visible:bg-accent/50 focus-visible:outline-none transition-colors {dragging
        ? 'bg-accent/50'
        : ''}"
    ></div>

    <div class="h-7 shrink-0 px-2.5 flex items-center gap-2 border-b border-border/60 bg-surface/60 select-none">
      <SquareTerminal size={12} class="text-accent shrink-0" />
      <span class="text-[11px] font-medium text-textPrimary">Terminal</span>
      <div class="flex-1"></div>
      <button
        type="button"
        onclick={onClose}
        class="gp-icon-btn !p-0.5"
        title="Hide the terminal (⌃`) — the session keeps running"
        aria-label="Hide the terminal dock"
      >
        <ChevronDown size={13} />
      </button>
    </div>

    <div class="flex-1 min-h-0 flex flex-col">
      <LazyView {load} name="the terminal" />
    </div>
  </div>
{/if}
