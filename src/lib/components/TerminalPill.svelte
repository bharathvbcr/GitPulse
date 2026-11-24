<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { PanelBottom } from "@lucide/svelte";
  import { onDestroy, untrack } from "svelte";
  import type { LeaseWaiter } from "../stores/hostLoadStore";
  import type { TerminalContext } from "../terminal/sessionContext";
  import { panelTabCounts } from "../terminal/panelTabCounts";
  import { activityAttentionLabel, sessionActivity } from "../terminal/sessionActivity";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { terminalPillView } from "../terminal/statusPill";
  import { isGenericLauncherTitle } from "../terminal/tabs";
  import { diagnostics } from "../diagnostics/diagnostics";
  import LauncherIcon from "./LauncherIcon.svelte";

  /**
   * How long the pointer may spend between the pill and its portaled menu
   * before the menu closes. The menu is not a child of the pill once portaled,
   * so the leave event fires while the pointer is still on its way.
   */
  const MENU_CLOSE_GRACE_MS = 220;

  let menuOpen = $state(false);
  let menuLaunching = $state(false);
  let focusSearchOnOpen = $state(false);
  let pillEl = $state<HTMLButtonElement | null>(null);
  let activity = $state<Record<string, TerminalContext | null>>({});
  /** Builds waiting for a slot, read with the host contexts; empty when it could not be read. */
  let buildQueue = $state<LeaseWaiter[]>([]);
  let closeTimer: ReturnType<typeof setTimeout> | undefined;

  // The hover menu (search, quick-launch, host-context polling) loads as its
  // own chunk on first hover or keyboard open, keeping the entry chunk lean.
  let Menu = $state<(typeof import("./TerminalPillMenu.svelte"))["default"] | null>(null);
  let menuRequested = false;

  function ensureMenuLoaded() {
    if (Menu || menuRequested) return;
    menuRequested = true;
    import("./TerminalPillMenu.svelte").then(
      (module) => {
        Menu = module.default;
      },
      (error: unknown) => {
        menuRequested = false;
        diagnostics.error("terminal-pill:menu", error);
      },
    );
  }

  let pillSessions = $derived(
    $terminalSessions.map((session) => {
      const launcher = session.launcher ?? "shell";
      const liveAct = session.sessionId ? $sessionActivity.get(session.sessionId) : undefined;
      const registryTitle =
        session.title && !isGenericLauncherTitle(session.title, launcher)
          ? session.title
          : undefined;
      const liveTitle =
        liveAct?.title && !isGenericLauncherTitle(liveAct.title, launcher)
          ? liveAct.title
          : undefined;
      return {
        key: session.key,
        repoPath: session.repoPath,
        label: session.label,
        launcher,
        status: session.status,
        title: registryTitle ?? liveTitle,
        checkout: session.checkout,
        sessionId: session.sessionId,
        canReveal: typeof session.reveal === "function",
        isActive: session.isActive,
        splitSlot: session.splitSlot,
        attention: session.attention ?? activityAttentionLabel(liveAct),
      };
    }),
  );
  let openDockTabs = $derived(panelTabCounts.countFor($panelTabCounts, $repoStore.currentPath));
  let terminalPill = $derived(
    terminalPillView(pillSessions, {
      open: $repoStore.terminalOpen,
      repoPath: $repoStore.currentPath,
      activity,
      openTabs: openDockTabs,
      checkouts: $repoStore.openTabs ?? [],
      buildQueue,
    }),
  );
  let pillLabel = $derived.by(() => {
    const count = terminalPill.alive > 1 ? `, ${terminalPill.alive} sessions` : "";
    return terminalPill.summary ? `Dock, ${terminalPill.summary}${count}` : `Dock${count}`;
  });

  /**
   * Keep the hover menu pinned while focus is inside the menu (e.g. typing in
   * the search box or navigating results) or while a launch is in flight, so
   * pointer drift never dismisses an active search mid-keystroke.
   */
  function isMenuPinnedByInteraction(): boolean {
    if (menuLaunching || focusSearchOnOpen) return true;
    const active = typeof document !== "undefined" ? document.activeElement : null;
    return active instanceof Element && Boolean(active.closest("[data-terminal-pill-menu]"));
  }

  function showMenu() {
    clearTimeout(closeTimer);
    closeTimer = undefined;
    menuOpen = true;
    ensureMenuLoaded();
  }

  function scheduleHide() {
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => {
      closeTimer = undefined;
      if (isMenuPinnedByInteraction()) return;
      menuOpen = false;
      focusSearchOnOpen = false;
    }, MENU_CLOSE_GRACE_MS);
  }

  function onPointerLeaveMenu() {
    if (isMenuPinnedByInteraction()) return;
    scheduleHide();
  }

  function closeMenu() {
    clearTimeout(closeTimer);
    closeTimer = undefined;
    menuOpen = false;
    focusSearchOnOpen = false;
  }

  function onPillFocusOut(event: FocusEvent) {
    const next = event.relatedTarget;
    if (
      next instanceof Element &&
      (next.closest("[data-terminal-pill]") || next.closest("[data-terminal-pill-menu]"))
    ) {
      return;
    }
    scheduleHide();
  }

  function onPillKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && menuOpen) {
      event.preventDefault();
      event.stopPropagation();
      closeMenu();
      return;
    }
    if (event.key !== "ArrowDown") return;
    event.preventDefault();
    focusSearchOnOpen = true;
    showMenu();
  }

  function toggleDock() {
    repoStore.toggleTerminal();
  }

  function trackPill(node: HTMLButtonElement) {
    pillEl = node;
    return () => {
      if (pillEl === node) pillEl = null;
    };
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape" || !menuOpen) return;
    event.preventDefault();
    event.stopPropagation();
    closeMenu();
  }

  onDestroy(() => clearTimeout(closeTimer));

</script>

<svelte:window onkeydown={onWindowKeydown} />

<!-- The terminal left the header when it stopped being a view. The pill
     still toggles the dock; its menu only calls actions the dock already
     has (focus, new session, switch). -->
{#if $repoStore.currentPath}
  <!-- Justified: pointer enter/leave and focus only open the menu. The
       button inside toggles the dock; menu buttons are the other actions. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="relative"
    data-terminal-pill
    onpointerenter={showMenu}
    onpointerleave={onPointerLeaveMenu}
    onfocusin={showMenu}
    onfocusout={onPillFocusOut}
  >
    <button
      type="button"
      {@attach trackPill}
      data-testid="terminal-pill"
      aria-pressed={$repoStore.terminalOpen}
      aria-expanded={menuOpen}
      aria-controls="terminal-pill-menu"
      aria-label={pillLabel}
      onclick={toggleDock}
      onkeydown={onPillKeydown}
      title="Toggle the dock ({shortcutTextLabel('⌃`', $hostPlatform.os)})"
      class="inline-flex h-6 max-w-[16rem] shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-2 text-meta font-medium leading-none transition-colors {$repoStore.terminalOpen
        ? 'border-accent bg-accent/10 text-accent'
        : 'border-edge bg-surfaceHover text-textPrimary hover:border-edgeStrong hover:bg-accent/10'}"
    >
      <PanelBottom size={12} class="shrink-0" />
      <span>Dock</span>
      {#if terminalPill.summary}
        {#if terminalPill.highlightedLauncher}
          <LauncherIcon kind={terminalPill.highlightedLauncher} size={11} class="shrink-0" />
        {/if}
        <span class="min-w-0 max-w-[8rem] truncate font-mono opacity-80" data-testid="terminal-pill-summary">{terminalPill.summary}</span>
      {/if}
      {#if terminalPill.alive > 0}
        <span
          class="inline-flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-accent/25 px-1 font-mono leading-none text-accent"
          data-testid="terminal-pill-count"
        >{terminalPill.alive}</span>
      {/if}
    </button>
  </div>
  {#if menuOpen && Menu}
    <Menu
      {pillEl}
      {terminalPill}
      focusSearchOnMount={focusSearchOnOpen}
      onSearchFocused={() => { focusSearchOnOpen = false; }}
      onPointerEnter={showMenu}
      onPointerLeave={onPointerLeaveMenu}
      onFocusIn={showMenu}
      onFocusOut={onPillFocusOut}
      onClose={closeMenu}
      onToggleDock={toggleDock}
      onActivity={(patch) => { activity = { ...untrack(() => activity), ...patch }; }}
      onBuildQueue={(queue) => { buildQueue = queue; }}
      onLaunchingChange={(busy) => { menuLaunching = busy; }}
    />
  {/if}
{/if}
