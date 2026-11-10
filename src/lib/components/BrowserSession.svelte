<script lang="ts">
  /**
   * A browser session: the terminal-dock tab that shows a local dev server's
   * page. The page itself is a native webview laid over this component's
   * content area (`src-tauri/src/browser`), so this component's job is the
   * address bar and keeping that view exactly where the area is, and only
   * while it can be seen:
   *
   * - placed in window points, measured from the area and scaled by the page
   *   zoom actually in effect (`uiScale.pageZoom`);
   * - hidden whenever the tab is off screen (another tab, Console mode, a
   *   hidden dock or repository) and whenever something of GitPulse's own is
   *   drawn over the area, because a native view sits above the whole page
   *   and would cover a menu or dialog. Anything portaled to the body (every
   *   menu, dialog and toast) is found by its box, at any size; anything else
   *   by hit-testing a grid at most 64px apart (a ninth of the area on a
   *   larger one), so an unportaled overlay smaller than that, or one that
   *   ignores the pointer, can be missed. The view hides a frame or two after
   *   the overlay paints;
   * - closed when the tab closes; a main-page reload closes it from the
   *   backend (`desktop::handle_page_load`), and sessions are not restored.
   *
   * While the page has keyboard focus it receives the keys, so the dock's
   * chords do not fire there; click the address bar or the tab strip first.
   */
  import { onDestroy, onMount, tick } from "svelte";
  import { get } from "svelte/store";
  import { AlertCircle, ArrowLeft, ArrowRight, ExternalLink, Globe, LoaderCircle, RotateCw } from "@lucide/svelte";
  import { openExternal } from "../desktop/openExternal";
  import { pageZoom } from "../ui/uiScale";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { formatError } from "../ui/formatError";
  import { CONTEXT_UNKNOWN, type ContextRead } from "../terminal/sessionContext";
  import {
    browserIpc,
    isOccluded,
    lastAddress,
    normalizeAddress,
    onPageEvents,
    overlapsRect,
    paneShown,
    rememberAddress,
    sameBounds,
    samplePoints,
    toWindowPoints,
    type HistoryStep,
    type PageEvent,
    type PaneBounds,
  } from "../browser/browserSession";

  let {
    tabId,
    onscreen = false,
    onTitle,
    initialUrl,
    onUrl,
  }: {
    tabId: string;
    /** This tab's pane is the one (or one of the split two) on screen. */
    onscreen?: boolean;
    /** The page's title, for the tab strip. */
    onTitle?: (title: string) => void;
    /** Opened as soon as the tab mounts: an agent's or a link's request. */
    initialUrl?: string;
    /** The address the page last loaded, for the tab (`setTabUrl`). */
    onUrl?: (url: string) => void;
  } = $props();

  let address = $state(lastAddress());
  let editing = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let opened = $state(false);
  let occluded = $state(false);
  // Raw: always replaced whole, and handed to the backend as a plain object.
  let bounds = $state.raw<PaneBounds | null>(null);
  let host = $state<HTMLDivElement | null>(null);
  let addressInput = $state<HTMLInputElement | null>(null);
  let disposed = false;
  let opening = false;

  const shown = $derived(paneShown({ opened, onscreen, occluded, bounds }));

  /**
   * Every command to this session's pane, in order. They are separate async
   * IPC calls, so without the queue a quick hide-then-show could land shown
   * and then hidden.
   */
  let queue: Promise<void> = Promise.resolve();
  /** Queues `op`; resolves once it has run. A failure is shown, never thrown. */
  function send(op: () => Promise<void>): Promise<void> {
    queue = queue
      .then(() => (disposed ? undefined : op()))
      .catch((cause: unknown) => {
        if (!disposed) error = formatError(cause);
      });
    return queue;
  }

  let sentBounds: PaneBounds | null = null;
  let sentVisible: boolean | null = null;

  function measure() {
    if (!host || disposed) return;
    const rect = host.getBoundingClientRect();
    const next = toWindowPoints(rect, get(pageZoom));
    if (!sameBounds(bounds, next)) bounds = next;
    occluded = onscreen && opened
      ? coveredByPortal(host, rect) || isOccluded(host, samplePoints(rect), (x, y) => document.elementFromPoint(x, y))
      : false;
  }

  /**
   * Menus, dialogs and toasts are portaled to the body (`dom/portal.ts`), so
   * any visible body child that is not the app and overlaps the area covers
   * it, however small — including one that ignores the pointer, which the
   * hit-test cannot see.
   */
  function coveredByPortal(el: HTMLElement, rect: DOMRect): boolean {
    for (const child of document.body.children) {
      if (child.contains(el)) continue;
      const style = getComputedStyle(child);
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") continue;
      if (overlapsRect(rect, child.getBoundingClientRect())) return true;
    }
    return false;
  }

  let frame = 0;
  function schedule() {
    if (frame || disposed || typeof requestAnimationFrame === "undefined") return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      measure();
    });
  }

  // Size, window and zoom changes, for as long as the area exists.
  $effect(() => {
    const el = host;
    if (!el) return;
    measure();
    const resize = new ResizeObserver(schedule);
    resize.observe(el);
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    const unzoom = pageZoom.subscribe(schedule);
    return () => {
      resize.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      unzoom();
    };
  });

  // While the view could be on screen: overlays coming and going anywhere in
  // the document, and moves no resize reports (a neighbour's transition).
  $effect(() => {
    if (!opened || !onscreen) {
      occluded = false;
      return;
    }
    schedule();
    const mutations = new MutationObserver(schedule);
    mutations.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "style", "open", "hidden", "aria-hidden"],
    });
    // Stops while the window is hidden, like every recurring UI timer.
    const stopSettle = createAdaptiveTimer(schedule, 500);
    return () => {
      mutations.disconnect();
      stopSettle();
    };
  });

  $effect(() => {
    const next = bounds;
    if (!opened || !next || sameBounds(sentBounds, next)) return;
    sentBounds = next;
    void send(() => browserIpc.setBounds(tabId, next));
  });

  $effect(() => {
    const visible = shown;
    if (!opened || sentVisible === visible) return;
    sentVisible = visible;
    void send(() => browserIpc.setVisible(tabId, visible));
  });

  function onPage(event: PageEvent) {
    switch (event.kind) {
      case "loading":
        loading = true;
        error = null;
        if (!editing) address = event.url;
        break;
      case "loaded":
        loading = false;
        if (!editing) address = event.url;
        if (event.url !== "about:blank") {
          rememberAddress(event.url);
          onUrl?.(event.url);
        }
        break;
      case "title":
        if (event.title.trim()) onTitle?.(event.title);
        break;
      case "refused":
        loading = false;
        error = `Blocked ${event.url.slice(0, 200)}: browser sessions open local development servers only.`;
        break;
    }
  }

  onMount(() => {
    const stop = onPageEvents(tabId, onPage);
    if (initialUrl) {
      address = initialUrl;
      // Once laid out, so the page opens where the tab is.
      void tick().then(() => submit());
    }
    return stop;
  });

  async function submit(event?: Event) {
    event?.preventDefault();
    const url = normalizeAddress(address);
    if (!url) return;
    error = null;
    editing = false;
    addressInput?.blur();
    if (opened) {
      await send(() => browserIpc.navigate(tabId, url));
      return;
    }
    if (opening) return;
    opening = true;
    measure();
    const at = bounds ?? { x: 0, y: 0, width: 0, height: 0 };
    try {
      await send(async () => {
        await browserIpc.open(tabId, url, at);
        // wry shows a new view at once; hide it again if it should not be.
        opened = true;
        sentBounds = at;
        sentVisible = true;
        rememberAddress(url);
      });
    } finally {
      opening = false;
    }
  }

  function step(to: HistoryStep) {
    if (opened) void send(() => browserIpc.go(tabId, to));
  }

  function onAddressKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      editing = false;
      addressInput?.blur();
    }
  }

  onDestroy(() => {
    disposed = true;
    if (frame) cancelAnimationFrame(frame);
    // Behind anything still queued, so an open in flight is closed after it
    // lands; a page already closed (the tab's ×) is not closed twice.
    void queue.then(() => (opened ? browserIpc.close(tabId) : undefined)).catch(() => {});
  });

  /** Ends the session: the page is closed and the view removed. */
  export async function close(): Promise<void> {
    await queue;
    await browserIpc.close(tabId);
    opened = false;
  }

  /** The tab was brought forward: a session with no page yet wants an address. */
  /**
   * Shows `url` here: the page loads it again if it is already on it (an
   * agent asking again after an edit wants the change), and a session not yet
   * opened opens on it.
   */
  export async function show(url: string): Promise<void> {
    address = url;
    await submit();
  }

  /** The address bar's address in the system browser, for what this session cannot do. */
  async function openOutside() {
    const url = normalizeAddress(address);
    if (!url) return;
    try {
      await openExternal(url);
    } catch (cause) {
      error = `Could not open ${url} in your browser: ${formatError(cause)}`;
    }
  }

  export function reveal() {
    schedule();
    if (!opened) addressInput?.focus();
  }

  /** A browser session runs no process, so there is no job to read. */
  export function readContext(): Promise<ContextRead> {
    return Promise.resolve(CONTEXT_UNKNOWN);
  }

  /** The terminal's view chords (text size, find) have nothing to act on here. */
  export function handleViewChord(_event: KeyboardEvent): boolean {
    return false;
  }
</script>

<div class="h-full flex flex-col min-h-0" data-browser-session={tabId}>
  <form class="shrink-0 flex items-center gap-1 px-2 h-8 border-b border-border/60 bg-surface/40" onsubmit={submit}>
    <button type="button" class="gp-icon-btn" aria-label="Back" title="Back" disabled={!opened} onclick={() => step("back")}><ArrowLeft size={13} /></button>
    <button type="button" class="gp-icon-btn" aria-label="Forward" title="Forward" disabled={!opened} onclick={() => step("forward")}><ArrowRight size={13} /></button>
    <button type="button" class="gp-icon-btn" aria-label="Reload" title="Reload" disabled={!opened} onclick={() => step("reload")}>
      {#if loading}<LoaderCircle size={13} class="animate-spin" />{:else}<RotateCw size={13} />{/if}
    </button>
    <input
      bind:this={addressInput}
      bind:value={address}
      aria-label="Address"
      placeholder="localhost:3000"
      spellcheck="false"
      autocomplete="off"
      maxlength="2048"
      class="flex-1 min-w-0 bg-background border border-border rounded px-2 py-0.5 text-[11px] font-mono"
      onfocus={() => (editing = true)}
      onblur={() => (editing = false)}
      onkeydown={onAddressKey}
    />
    <button type="submit" class="gp-btn py-0! text-[11px]!">{opened ? "Go" : "Open"}</button>
    <button type="button" class="gp-icon-btn" aria-label="Open in your browser" title="Open this address in your system browser" onclick={() => void openOutside()}><ExternalLink size={13} /></button>
  </form>
  {#if error}
    <div class="shrink-0 px-3 py-1 border-b border-rose-500/30 bg-rose-500/10 text-rose-300 text-[11px] flex items-start gap-1.5" role="alert" data-testid="browser-session-error">
      <AlertCircle size={12} class="shrink-0 mt-0.5" />
      <span class="flex-1" style="overflow-wrap:anywhere">{error}</span>
      <button type="button" class="text-xs hover:text-white" aria-label="Dismiss" onclick={() => (error = null)}>✕</button>
    </div>
  {/if}
  <div bind:this={host} class="relative flex-1 min-h-0" data-browser-host>
    {#if !opened}
      <div class="h-full flex flex-col items-center justify-center gap-2 p-4 text-center text-textMuted text-xs">
        <Globe size={18} />
        <span>Open a local development server, like <code>localhost:3000</code> or just <code>5173</code>.</span>
        <span class="text-[10px] max-w-sm">The page runs in its own browser with no access to GitPulse and its own storage. Only localhost addresses open; sessions close when the window reloads.</span>
      </div>
    {:else if onscreen && occluded}
      <div class="h-full flex items-center justify-center text-[11px] text-textMuted" data-testid="browser-session-covered">
        The page is hidden while a menu or dialog is open.
      </div>
    {/if}
  </div>
</div>
