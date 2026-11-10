<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { LAYERS } from "../ui/layers";
  import { SquareTerminal } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { onDestroy, tick, untrack } from "svelte";
  import { portal } from "../dom/portal";
  import { invoke } from "../ipc/invoke";
  import { isTauri } from "../platform";
  import { terminalLaunchRequests } from "../terminal/launchRequests";
  import { CONTEXT_READ_MS } from "../terminal/inputQueue";
  import { contextReadOf, type TerminalContext } from "../terminal/sessionContext";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { terminalPillView } from "../terminal/statusPill";
  import { launcherLabel } from "../terminal/tabs";
  import { popover, restoreFocusTo } from "../ui/popover";

  /**
   * How long the pointer may spend between the pill and its portaled menu
   * before the menu closes. The menu is not a child of the pill once portaled,
   * so the leave event fires while the pointer is still on its way.
   */
  const MENU_CLOSE_GRACE_MS = 220;
  /** Host context reads per hover. The rest keep the registry's own status. */
  const ACTIVITY_READ_LIMIT = 8;

  let menuOpen = $state(false);
  let launching = $state(false);
  let launchError = $state<string | null>(null);
  let pillEl = $state<HTMLButtonElement | null>(null);
  let menuEl = $state<HTMLDivElement | null>(null);
  let activity = $state<Record<string, TerminalContext | null>>({});
  let closeTimer: ReturnType<typeof setTimeout> | undefined;

  let pillSessions = $derived(
    $terminalSessions.map((session) => ({
      key: session.key,
      repoPath: session.repoPath,
      label: session.label,
      status: session.status,
      title: session.title,
      checkout: session.checkout,
      sessionId: session.sessionId,
      canReveal: typeof session.reveal === "function",
    })),
  );
  let terminalPill = $derived(
    terminalPillView(pillSessions, {
      open: $repoStore.terminalOpen,
      repoPath: $repoStore.currentPath,
      activity,
    }),
  );
  let pillLabel = $derived.by(() => {
    const count = terminalPill.alive > 1 ? `, ${terminalPill.alive} sessions` : "";
    return terminalPill.summary ? `Terminal, ${terminalPill.summary}${count}` : `Terminal${count}`;
  });
  let activityKey = $derived(
    menuOpen
      ? JSON.stringify(
          terminalPill.sessions
            .flatMap((session) => (session.sessionId ? [[session.key, session.sessionId] as const] : []))
            .slice(0, ACTIVITY_READ_LIMIT),
        )
      : "",
  );

  function showMenu() {
    clearTimeout(closeTimer);
    closeTimer = undefined;
    menuOpen = true;
  }

  function scheduleHide() {
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => {
      closeTimer = undefined;
      menuOpen = false;
      launchError = null;
    }, MENU_CLOSE_GRACE_MS);
  }

  function closeMenu() {
    clearTimeout(closeTimer);
    closeTimer = undefined;
    menuOpen = false;
    launchError = null;
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
    showMenu();
    void tick().then(() => menuEl?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus());
  }

  function onMenuKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeMenu();
      restoreFocusTo(pillEl);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const items = [...(event.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "ArrowDown" ? (at + 1) % items.length : (at - 1 + items.length) % items.length;
    event.preventDefault();
    items[next]?.focus();
  }

  function toggleDock() {
    repoStore.toggleTerminal();
  }

  async function focusListed(key: string) {
    const record = $terminalSessions.find((session) => session.key === key);
    if (!record) {
      launchError = "No shell";
      return;
    }
    try {
      const outcome = await focusTerminalSession(record);
      if (!outcome.ok) {
        launchError = outcome.reason === "no-session" ? "No shell" : "This session cannot be shown";
        return;
      }
      closeMenu();
    } catch (error) {
      launchError = error instanceof Error ? error.message : String(error);
    }
  }

  async function newTerminal() {
    const path = $repoStore.currentPath;
    if (!path || launching) return;
    launching = true;
    launchError = null;
    try {
      // Request first, then show the dock: the panel claims the request once
      // it is visible, which may be this open. Same order as the palette.
      const launched = terminalLaunchRequests.request(path, $interfaceStore.terminalLauncher);
      repoStore.setTerminalOpen(true);
      await launched;
      closeMenu();
    } catch (error) {
      launchError = error instanceof Error ? error.message : String(error);
    } finally {
      launching = false;
    }
  }

  const menuPopover = $derived({
    anchor: {
      kind: "element" as const,
      element: pillEl,
      gap: 6,
      place: "below" as const,
      align: "end" as const,
    },
    estimate: { width: 256, height: 180 },
    inset: 8,
    revision: `${terminalPill.detail}:${terminalPill.alive}:${launchError ?? ""}`,
    dismiss: {
      inside: "[data-terminal-pill], [data-terminal-pill-menu]",
      pointer: "pointerdown" as const,
      scroll: true,
      resize: true,
      escape: "none" as const,
    },
    onDismiss: () => closeMenu(),
  });

  function trackPill(node: HTMLButtonElement) {
    pillEl = node;
    return () => {
      if (pillEl === node) pillEl = null;
    };
  }

  function trackMenu(node: HTMLDivElement) {
    menuEl = node;
    return () => {
      if (menuEl === node) menuEl = null;
    };
  }

  /**
   * One host-context read per open of the menu. `key` is the session list, so
   * writing `activity` does not run this again: a missing answer stays a
   * missing answer, and the browser preview (no Tauri) never calls invoke.
   */
  function watchActivity(key: string) {
    return () => {
      if (!key || !isTauri()) return;
      let rows: unknown;
      try {
        rows = JSON.parse(key);
      } catch {
        return;
      }
      if (!Array.isArray(rows)) return;
      const targets: { key: string; sessionId: string }[] = [];
      for (const row of rows) {
        if (!Array.isArray(row) || row.length !== 2) continue;
        const [sessionKey, sessionId] = row;
        if (typeof sessionKey !== "string" || typeof sessionId !== "string" || !sessionId) continue;
        targets.push({ key: sessionKey, sessionId });
      }
      if (targets.length === 0) return;
      const controller = new AbortController();
      let cancelled = false;
      void (async () => {
        const next: Record<string, TerminalContext | null> = {};
        for (const session of targets) {
          if (cancelled) return;
          try {
            const raw = await invoke<unknown>(
              "cmd_terminal_context",
              { sessionId: session.sessionId },
              { deadlineMs: CONTEXT_READ_MS, signal: controller.signal },
            );
            const read = contextReadOf(raw);
            next[session.key] = read.kind === "known" ? read.context : null;
          } catch {
            if (controller.signal.aborted) return;
            next[session.key] = null;
          }
        }
        if (cancelled) return;
        activity = { ...untrack(() => activity), ...next };
      })();
      return () => {
        cancelled = true;
        controller.abort();
      };
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
    onpointerleave={scheduleHide}
    onfocusin={showMenu}
    onfocusout={onPillFocusOut}
  >
    <button
      type="button"
      {@attach trackPill}
      data-testid="terminal-pill"
      aria-pressed={$repoStore.terminalOpen}
      aria-expanded={menuOpen}
      aria-haspopup="menu"
      aria-controls="terminal-pill-menu"
      aria-label={pillLabel}
      onclick={toggleDock}
      onkeydown={onPillKeydown}
      title="Toggle the terminal dock ({shortcutTextLabel('⌃`', $hostPlatform.os)})"
      class="inline-flex h-6 max-w-[16rem] shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-2 text-meta font-medium leading-none transition-colors {$repoStore.terminalOpen
        ? 'border-accent bg-accent/10 text-accent'
        : 'border-edge bg-surfaceHover text-textPrimary hover:border-edgeStrong hover:bg-accent/10'}"
    >
      <SquareTerminal size={12} class="shrink-0" />
      <span>Terminal</span>
      {#if terminalPill.summary}
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
  {#if menuOpen}
    <!-- Justified: pointer enter/leave keep the portaled menu open across
         the gap from the pill. The buttons inside are the controls. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      {@attach trackMenu}
      {@attach watchActivity(activityKey)}
      id="terminal-pill-menu"
      data-testid="terminal-pill-menu"
      data-terminal-pill-menu
      role="group"
      aria-label="Terminal options"
      use:portal={"body"}
      use:popover={menuPopover}
      onpointerenter={showMenu}
      onpointerleave={scheduleHide}
      onfocusin={showMenu}
      onfocusout={onPillFocusOut}
      onkeydown={onMenuKeydown}
      class="fixed w-64 max-w-[calc(100vw-16px)] gp-menu gp-pop p-1 text-meta font-sans text-textPrimary"
      style="z-index: {LAYERS.MENU}"
    >
      <p class="px-2.5 py-1 text-textMuted" data-testid="terminal-pill-detail">{terminalPill.detail}</p>
      {#if terminalPill.sessions.length === 1 && terminalPill.sessions[0]?.where}
        <p class="px-2.5 pb-1 truncate text-textMuted" title={terminalPill.sessions[0]?.cwd}>{terminalPill.sessions[0]?.where}</p>
      {/if}
      {#if launchError}
        <p class="px-2.5 py-1 text-danger" role="alert">{launchError}</p>
      {/if}
      <button
        type="button"
        class="gp-menu-item text-meta"
        data-testid="terminal-pill-toggle"
        onclick={() => { toggleDock(); closeMenu(); }}
      >
        {$repoStore.terminalOpen ? "Hide terminal" : "Show terminal"}
      </button>
      {#if terminalPill.focusKey}
        <button
          type="button"
          class="gp-menu-item text-meta"
          data-testid="terminal-pill-focus"
          onclick={() => { if (terminalPill.focusKey) void focusListed(terminalPill.focusKey); }}
        >
          Focus session
        </button>
      {/if}
      <button
        type="button"
        class="gp-menu-item text-meta"
        data-testid="terminal-pill-new"
        disabled={launching}
        onclick={() => void newTerminal()}
      >
        New {launcherLabel($interfaceStore.terminalLauncher)} session
      </button>
      {#if terminalPill.sessions.length > 1}
        <div class="gp-menu-sep" role="separator"></div>
        {#each terminalPill.sessions as session (session.key)}
          <button
            type="button"
            class="gp-menu-item text-meta"
            data-testid="terminal-pill-session"
            disabled={!session.canReveal}
            title={session.cwd ?? session.repoPath}
            onclick={() => void focusListed(session.key)}
          >
            <span class="min-w-0 flex-1 truncate">Switch to {session.name}</span>
            {#if session.phrase}
              <span class="shrink-0 text-textMuted">{session.phrase}</span>
            {/if}
          </button>
        {/each}
      {/if}
    </div>
  {/if}
{/if}
