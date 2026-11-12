<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { ChevronDown, SquareTerminal } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { repoStore } from "../stores/repoStore";
  import type { MutationTarget } from "../repos/storeTypes";
  import { reportFailure } from "../stores/mutationOutcome";
  import { toastStore } from "../stores/toastStore";
  import { worktreeSetupNotices } from "../branches/types";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import {
    TERMINAL_DOCK_MAX_HEIGHT,
    TERMINAL_DOCK_MIN_HEIGHT,
    TERMINAL_DOCK_RESIZE_STEP,
    clampTerminalDockHeight,
    fitTerminalDockHeight,
  } from "../terminal/dockMetrics";
  import { nextHostedTerminals } from "../terminal/repoHosts";
  import { awaitedTabIds, taskTerminalRequests } from "../terminal/taskLaunches";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
  import { requestReveal } from "../files/revealRequests";
  import { observeResize } from "../dom/observeResize";
  import { motionExitMs } from "../motion/tokens";
  import { panelModes } from "../ui/panelMode";
  import { LAYERS } from "../ui/layers";
  import { floatingPanelDismiss } from "../ui/panelDismiss";
  import { popover, type PopoverOptions } from "../ui/popover";

  /**
   * The terminal, docked beneath the active view.
   *
   * Two things this shape gets right that a view could not. A PTY has to
   * outlive a view switch, so the pane was already mounted once and hidden
   * thereafter — never really a page. And a command's output is worth reading
   * *against* something: a Health remediation, a failing test, the diff you
   * are about to commit. A full-screen terminal hid all of it.
   *
   * Mounted on first open and kept mounted after, because unmounting loses
   * the scrollback (and detaches anything running). Closing hides it. Switching repository tabs also hides rather
   * than unmounts: one panel per visited tab, bound to that tab's path, so
   * the scrollback is still there when you come back. Closing the repository
   * tab is what drops the panel; what was still running there is detached,
   * not killed, and stays under Sessions.
   */

  let {
    open,
    onClose,
    load,
    expanded = $bindable(false),
  }: {
    open: boolean;
    onClose: () => void;
    /** Loader for TerminalPanel, declared at module scope by the caller. */
    load: ViewLoader;
    /**
     * The dock fills its column, leaving the view above it no height. Bound
     * by the host, which must then hide that view: a zero-height view still
     * paints whatever it positions over the terminal, and keeps those
     * controls in the tab order.
     */
    expanded?: boolean;
  } = $props();

  // svelte-ignore state_referenced_locally
  // Justified: capturing only the initial value of `open` is the intent. This
  // latch answers "has the dock ever been open" — a one-way question. The
  // effect below handles every later open, and nothing may set it back to
  // false while a shell is running.
  let mounted = $state(open);
  /**
   * Repository tabs a queued task terminal is waiting for. They are hosted
   * with the dock closed, so an agent launched from a task sheet starts
   * without taking the reader to the repository surface.
   */
  const awaited = $derived(
    awaitedTabIds($repoStore.openTabs, $taskTerminalRequests, { caseInsensitive: isCaseInsensitiveFs() }),
  );
  $effect(() => {
    if (open || awaited.size > 0) mounted = true;
  });

  /**
   * Repository tabs whose terminal panel has been created. The derived below
   * also includes the active tab while the dock is open so the first paint
   * has a host without waiting for this latch; the latch is what keeps a
   * panel alive after the user leaves it.
   */
  let hostedIds = $state(new Set<string>());
  // The tab ids as a value: repoStore rebuilds openTabs on every publish.
  const openTabIdsKey = $derived($repoStore.openTabs.map((tab) => tab.id).join("\n"));
  const activeTabId = $derived($repoStore.activeTabId);
  $effect(() => {
    const ids = openTabIdsKey === "" ? [] : openTabIdsKey.split("\n");
    const next = nextHostedTerminals(hostedIds, ids, activeTabId, open, awaited);
    if (next !== hostedIds) untrack(() => { hostedIds = next; });
  });

  const hostedTabs = $derived(
    $repoStore.openTabs.filter(
      (tab) => hostedIds.has(tab.id) || (open && tab.id === $repoStore.activeTabId),
    ),
  );

  /**
   * Bringing a session from another repository on screen.
   *
   * The dock owns this, not the panel: a panel is bound to the single path it
   * was handed and must never consult the live `currentPath`, so it cannot be
   * the thing that switches repository tabs. The dock already spans them.
   */
  function goToSession(session: TerminalSessionRecord) {
    return focusTerminalSession(session);
  }

  /*
   * Docked or overlay (panelMode.ts). Docked, the dock owns a row under the
   * view, settles in, and leaves in the same exit (transform and opacity, the
   * row collapsing once it is gone). Overlay, it floats over the bottom of the view
   * and slides on transform; the view keeps the full column. The dock is
   * never unmounted: hiding is display:none, so an overlay holds the element
   * for its exit and only then hides it.
   */
  let overlay = $derived($panelModes.dock === "overlay");
  let leaving = $state(false);
  let leaveTimer: ReturnType<typeof setTimeout> | null = null;
  // svelte-ignore state_referenced_locally
  // Justified: the previous value of `open` is only ever compared, never
  // rendered, so capturing the initial one is the intent.
  let wasOpen = open;
  // Read when the dock closes, not memoized: the reduced-motion preference is
  // not reactive state, and a stale 70 would hold an exit the reader refused.
  let exitMs = $state(motionExitMs("slow"));
  const phase = $derived(leaving ? "slide-out" : overlay ? "slide-in" : "in");

  $effect(() => {
    const isOpen = open;
    untrack(() => {
      if (isOpen === wasOpen) return;
      wasOpen = isOpen;
      if (leaveTimer) clearTimeout(leaveTimer);
      leaveTimer = null;
      exitMs = motionExitMs("slow");
      if (isOpen || exitMs === 0) {
        leaving = false;
        return;
      }
      leaving = true;
      leaveTimer = setTimeout(() => {
        leaving = false;
        leaveTimer = null;
      }, exitMs);
    });
  });

  /*
   * A floating dock closes on a pointerdown outside it. Only while it floats
   * and is open: docked, it owns a row and a click elsewhere is no reason to
   * hide a running shell. Menus the panel opened are portaled to <body>, and
   * the status bar's terminal pill is the dock's own trigger, so both count as
   * inside (ui/panelDismiss.ts); the popover owner does the judging.
   */
  const dockDismissOptions = $derived<PopoverOptions>({
    dismiss: {
      ...floatingPanelDismiss("[data-terminal-dock]", "[data-terminal-pill], [data-terminal-pill-menu]"),
      pointer: overlay && open && !leaving ? "pointerdown" : "none",
    },
    onDismiss: () => onClose(),
  });

  function openInCodeView(path: string, line: number | null) {
    requestReveal(path, line, null);
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code");
  }

  async function addAgentWorktree(
    target: MutationTarget,
    targetPath: string,
    newBranch: string,
  ): Promise<{ ok: true; path: string } | { ok: false; error: string | null }> {
    const outcome = await repoStore.addWorktree(
      {
        targetPath,
        newBranch,
        startPoint: null,
        detach: false,
        cowCaches: true,
        runHooks: false,
        zeroCopyDevmap: false,
        seedDevmap: true,
        sparsePaths: null,
      },
      target,
      true,
    );
    if (!outcome.ok) {
      if (outcome.refused) {
        reportFailure(outcome, "Could not create isolated worktree");
        return { ok: false, error: null };
      }
      return { ok: false, error: outcome.error ?? "it did not complete." };
    }
    for (const notice of worktreeSetupNotices(outcome.output ?? {}, "create")) {
      toastStore.warning(notice);
    }
    return { ok: true, path: outcome.output?.path ?? targetPath };
  }

  let host: HTMLDivElement | undefined = $state();
  let dragging = $state(false);
  /** Height of the column the dock shares with the view, for the ceiling. */
  let containerHeight = $state(0);
  /**
   * The height a pointer drag is asking for, held locally until the drag ends.
   *
   * Writing the store per pointermove meant a synchronous localStorage write
   * and an interfaceStore publish per frame — and every subscriber of that
   * store (App's zoom, the graph's palette, …) re-ran with it. The drag
   * renders from this value and commits once on pointerup/pointercancel.
   * Null whenever no drag is in flight; the keyboard path never uses it.
   */
  let dragHeight = $state<number | null>(null);

  // The stored height is a request; what renders also respects the room the
  // window actually has, so a dock sized on a large display cannot swallow
  // the view when the same preference is restored on a small one.
  let heightCeiling = $derived(
    fitTerminalDockHeight(TERMINAL_DOCK_MAX_HEIGHT, containerHeight, expanded ? 0 : 160),
  );
  let height = $derived(
    expanded
      ? fitTerminalDockHeight(TERMINAL_DOCK_MAX_HEIGHT, containerHeight, 0)
      : dragHeight !== null
        ? fitTerminalDockHeight(dragHeight, containerHeight)
        : fitTerminalDockHeight($interfaceStore.terminalDockHeight, containerHeight),
  );

  $effect(() => {
    const parent = host?.parentElement;
    if (!parent || typeof ResizeObserver === "undefined") return;
    return observeResize(parent, ([entry]) => {
      containerHeight = entry.contentRect.height;
    });
  });

  function startDrag(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = height;
    // Collapsing out of the expanded state starts the drag from what was on
    // screen; like every other drag height, it reaches the store on release.
    dragHeight = expanded ? clampTerminalDockHeight(startHeight) : null;
    expanded = false;
    dragging = true;

    // Pointer capture, not window listeners: a drag that leaves the window
    // still ends on pointerup, and nothing else on the page sees the moves.
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);

    const move = (moveEvent: PointerEvent) => {
      // Dragging up grows the dock, so the delta is inverted. Clamped exactly
      // as the store would clamp it, so the release commits what was shown.
      dragHeight = clampTerminalDockHeight(startHeight + (startY - moveEvent.clientY));
    };
    const end = () => {
      commitDragHeight();
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

  /** One store write per drag, not one per pointermove. */
  function commitDragHeight() {
    if (dragHeight === null) return;
    interfaceStore.setTerminalDockHeight(dragHeight);
    dragHeight = null;
  }

  function handleSeparatorKey(event: KeyboardEvent) {
    const currentHeight = height;
    if (expanded && (event.key === "ArrowUp" || event.key === "ArrowDown")) expanded = false;
    if (event.key === "ArrowUp") {
      event.preventDefault();
      interfaceStore.setTerminalDockHeight(currentHeight + TERMINAL_DOCK_RESIZE_STEP);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      interfaceStore.setTerminalDockHeight(currentHeight - TERMINAL_DOCK_RESIZE_STEP);
    }
  }

  onDestroy(() => {
    if (leaveTimer) clearTimeout(leaveTimer);
    // A dock torn down mid-drag still keeps the height the reader chose.
    commitDragHeight();
    dragging = false;
  });
</script>

{#if mounted}
  <!-- Hidden, never unmounted: display:none pauses rendering, not the shell. -->
  <div
    bind:this={host}
    use:popover={dockDismissOptions}
    class="gp-dock-motion flex flex-col overflow-hidden rounded-t-lg bg-surfaceHover {overlay ? 'absolute inset-x-0 bottom-0 shadow-float' : 'shrink-0'}"
    class:hidden={!open && !leaving}
    style="height: {height}px; --gp-dock-exit-ms: {exitMs}ms;{overlay ? ` z-index: ${LAYERS.STICKY};` : ''}"
    data-terminal-dock
    data-dock-mode={overlay ? "overlay" : "docked"}
    data-phase={phase}
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
      aria-valuemax={heightCeiling}
      title="Drag to resize · ↑/↓ to nudge"
      tabindex="0"
      onpointerdown={startDrag}
      onkeydown={handleSeparatorKey}
      class="h-1.5 shrink-0 cursor-row-resize hover:bg-accent/40 focus-visible:bg-accent/50 focus-visible:outline-hidden transition-colors {dragging
        ? 'bg-accent/50'
        : ''}"
    ></div>

    {#if hostedTabs.length === 0}
    <div class="h-7 shrink-0 px-2.5 flex items-center gap-2 bg-surface/60 select-none">
      <SquareTerminal size={12} class="text-accent shrink-0" />
      <span class="text-meta font-medium text-textPrimary">Terminal</span>
      <div class="flex-1"></div>
      <button
        type="button"
        onclick={onClose}
        class="gp-icon-btn" data-size="sm"
        title="Hide the terminal ({shortcutTextLabel('⌃`', $hostPlatform.os)}) — the session keeps running"
        aria-label="Hide the terminal dock"
      >
        <ChevronDown size={13} />
      </button>
    </div>
    {/if}

    <div class="flex-1 min-h-0 relative">
      {#each hostedTabs as tab (tab.id)}
        <!-- Absolute so a hidden sibling keeps its box: a panel laid out at
             zero height would reflow its xterm to a 1-row grid. -->
        <div
          class="absolute inset-0 flex flex-col"
          class:hidden={tab.id !== $repoStore.activeTabId}
          data-terminal-host={tab.id}
        >
          <LazyView
            {load}
            name="the terminal"
            props={{
              repoPath: tab.path,
              visible: open && tab.id === $repoStore.activeTabId,
              dockDragging: dragging,
              onClose,
              expanded,
              onToggleExpanded: () => (expanded = !expanded),
              dockMode: overlay ? "overlay" : "docked",
              onToggleDockMode: () => panelModes.toggle("dock"),
              onGoToSession: goToSession,
              onOpenInCodeView: openInCodeView,
              onCreateAgentWorktree: (targetPath: string, newBranch: string) =>
                addAgentWorktree({ tabId: tab.id, path: tab.path }, targetPath, newBranch),
              checkouts: $repoStore.openTabs,
            }}
          />
        </div>
      {/each}
    </div>
  </div>
{/if}
