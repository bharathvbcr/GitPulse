<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { LAYERS } from "../ui/layers";
  import { graphStore } from "../stores/graphStore";
  import CommitCadence from "./CommitCadence.svelte";
  import LanguageSegment from "./LanguageSegment.svelte";
  import {
    GitBranch,
    RefreshCw,
    AlertTriangle,
    FileDiff,
    ArrowUp,
    ArrowDown,
    Keyboard,
    CheckCircle2,
    GitMerge,
    HelpCircle,
    SquareTerminal,
  } from "@lucide/svelte";
  import { tabMarker, tabTooltip } from "../repos/operation";
  import { describeWatch, watchMarker } from "../repos/watchState";
  import { RadioTower } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { resolveStatusBarMode } from "../ui/statusBarMode";
  import { hostLoadPulse, startHostLoad } from "../stores/hostLoadStore";
  import { statusChip } from "../health/hostLoad";
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { Gauge } from "@lucide/svelte";
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
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo } from "../ui/popover";

  let {
    onOpenShortcuts,
  }: {
    onOpenShortcuts?: () => void;
  } = $props();

  let currentBranch = $derived($repoStore.currentBranch || "HEAD");
  let branchInfo = $derived(
    $repoStore.branches.find((b) => b.is_current || b.name === currentBranch)
  );

  let dirtyCount = $derived($repoStore.statuses.length);
  let stagedCount = $derived($repoStore.statuses.filter((s) => s.is_staged).length);
  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);

  let aheadCount = $derived(branchInfo?.ahead_count ?? 0);
  let behindCount = $derived(branchInfo?.behind_count ?? 0);

  /**
   * A parked merge/rebase/cherry-pick is the highest-stakes thing the status
   * bar can say, so it renders first and it is a button: a user who has
   * wandered to Code or the graph mid-merge has no other cue that the repository
   * is mid-operation, and one click takes them to where they can finish it.
   */
  /**
   * Surfaced only when live updates are actually degraded. A repository whose
   * watcher failed keeps refreshing on a timer, so this is a "slower than
   * usual" notice, not an error — but leaving it unsaid is what makes a stale
   * branch name look current.
   */
  let watchState = $derived($repoStore.watch);
  let statusUnavailableSince = $derived($repoStore.statusUnavailableSince ?? null);

  /**
   * Host memory pressure, said only when it matters: pressure up, the
   * compressor thrashing, or the sampler stopped. A quiet machine adds
   * nothing to the bar (`health/hostLoad.ts` owns the rule).
   */
  let loadChip = $derived(statusChip($hostLoadPulse));
  onMount(() => startHostLoad());
  let watchLabel = $derived(watchMarker(watchState));

  let operationState = $derived($repoStore.operation);
  let operationMarker = $derived(tabMarker(operationState));
  let operationTip = $derived(tabTooltip(operationState));

  /**
   * The bar owns its own visibility rather than App.svelte, because only it
   * knows the four signals that override a hidden preference: a parked
   * operation, unresolved conflicts, a degraded watcher, and a status poll
   * that stopped answering.
   */
  let resolved = $derived(
    resolveStatusBarMode($interfaceStore.statusBarMode, {
      operationParked: Boolean(operationMarker),
      conflictedCount,
      watchDegraded: Boolean(watchLabel),
      statusUnavailable: statusUnavailableSince !== null,
    }),
  );
  let detail = $derived(resolved.mode);
  let forcedTip = $derived(
    resolved.forced
      ? "Shown despite the hidden status-bar setting: this repository needs attention."
      : undefined,
  );

  function openShortcuts() {
    if (onOpenShortcuts) {
      onOpenShortcuts();
    } else {
      window.dispatchEvent(new CustomEvent("gitpulse:shortcuts"));
    }
  }

  function openCommandPalette() {
    window.dispatchEvent(new CustomEvent("gitpulse:palette"));
  }

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
      place: "above" as const,
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

{#if detail !== "hidden"}
<footer
  title={forcedTip}
  class="gp-glass h-6 shrink-0 bg-surface gp-section-edge px-3 flex items-center justify-between text-meta font-sans text-textMuted select-none"
  style:z-index={LAYERS.SCROLL_CUE}
  role="status"
  aria-label="Repository Status Bar"
>
  <!-- Left Segment: Branch, Ahead/Behind, Dirty & Conflicts -->
  <div class="flex items-center gap-3 min-w-0">
    <!-- Branch Pill -->
    <div
      class="inline-flex items-center gap-1.5 font-medium text-textPrimary truncate"
      title="Current Branch: {currentBranch}"
    >
      <GitBranch size={12} class="text-accent shrink-0" />
      <span class="truncate max-w-[140px] font-mono text-meta">{currentBranch}</span>
    </div>

    <!-- Live-update health. Absent when the watcher is working, which is
         the overwhelmingly common case. -->
    {#if watchLabel}
      <span
        class="inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded border border-warn/65 bg-warn/10 px-1.5 py-0.2 font-medium text-warn"
        title={describeWatch(watchState)}
      >
        <RadioTower size={11} class="shrink-0" />
        <span>{watchLabel}</span>
      </span>
    {/if}

    <!-- The status poll has stopped answering, so the file list and counts
         below are the last ones read, not current ones. Absent while it answers. -->
    {#if statusUnavailableSince !== null}
      <span
        class="inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded border border-warn/65 bg-warn/10 px-1.5 py-0.2 font-medium text-warn"
        title="git status has not answered since {statusUnavailableSince
          ? new Date(statusUnavailableSince).toLocaleTimeString()
          : ''}; the file list and counts may be out of date. The Diagnostics log has the error."
        data-testid="status-unavailable"
      >
        <AlertTriangle size={11} class="shrink-0" />
        <span>Status unavailable</span>
      </span>
    {/if}

    <!-- Host memory pressure. Absent while the machine is fine. -->
    {#if loadChip}
      <button
        type="button"
        onclick={() => repoStore.setActiveTab("insights", "health")}
        class="inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded border px-1.5 py-0.2 font-medium transition-colors {loadChip.tone ===
        'danger'
          ? 'border-danger/65 bg-danger/10 text-danger hover:bg-danger/20'
          : loadChip.tone === 'warn'
            ? 'border-warn/65 bg-warn/10 text-warn hover:bg-warn/20'
            : 'border-edge bg-surfaceHover text-textMuted hover:text-textPrimary'}"
        title={loadChip.title}
        data-testid="status-host-load"
      >
        <Gauge size={11} class="shrink-0" />
        <span>{loadChip.text}</span>
      </button>
    {/if}

    <!-- Parked operation: merge / rebase / cherry-pick / revert / bisect -->
    {#if operationMarker}
      <button
        type="button"
        onclick={() => repoStore.setActiveTab("work", "resolve")}
        class="inline-flex items-center gap-1 px-1.5 py-0.2 rounded border font-medium transition-colors {operationState.probeFailed
          ? 'border-warn/65 bg-warn/10 text-warn hover:bg-warn/20'
          : 'border-accent/35 bg-accent/10 text-accent hover:bg-accent/20'}"
        title={operationTip}
      >
        {#if operationState.probeFailed}
          <HelpCircle size={11} class="shrink-0" />
        {:else}
          <GitMerge size={11} class="shrink-0" />
        {/if}
        <span class="font-mono text-micro">{operationMarker}</span>
      </button>
    {/if}

    <!-- Sync status (Ahead / Behind) -->
    {#if aheadCount > 0 || behindCount > 0}
      <div
        class="inline-flex items-center gap-1 font-mono text-micro text-textMuted px-1.5 py-0.2 rounded bg-surfaceHover border border-edge"
        title="{aheadCount} commits ahead, {behindCount} commits behind upstream"
      >
        {#if aheadCount > 0}
          <span class="flex items-center text-ok">
            <ArrowUp size={10} />{aheadCount}
          </span>
        {/if}
        {#if behindCount > 0}
          <span class="flex items-center text-info">
            <ArrowDown size={10} />{behindCount}
          </span>
        {/if}
      </div>
    {/if}

    <!-- Ambient readouts below are what "Compact" drops: they say nothing is
         wrong, which is exactly the noise a decluttered bar should lose. -->
    {#if detail === "full"}
      <span class="inline-block h-2.5 w-0.5 rounded-full bg-border/50 align-middle" aria-hidden="true"></span>
    {/if}

    <!-- Working Tree Changes / Dirty Files -->
    {#if dirtyCount > 0}
      <button
        type="button"
        onclick={() => void repoStore.previewUncommitted()}
        class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors"
        title="View {dirtyCount} changed file{dirtyCount === 1 ? '' : 's'} ({stagedCount} staged)"
      >
        <FileDiff size={12} class="text-warn shrink-0" />
        <span>{dirtyCount} modified</span>
        {#if stagedCount > 0}
          <span class="text-micro text-ok font-mono">
            ({stagedCount} staged)
          </span>
        {/if}
      </button>
    {:else if detail === "full"}
      <span class="inline-flex items-center gap-1 text-textMuted/70">
        <CheckCircle2 size={11} class="text-ok/80" />
        <span>Clean</span>
      </span>
    {/if}

    <!-- Conflicts Indicator -->
    {#if conflictedCount > 0}
      <button
        type="button"
        onclick={() => repoStore.setActiveTab("work", "resolve")}
        class="inline-flex items-center gap-1 font-semibold text-danger hover:brightness-110 transition-colors animate-pulse"
        title="{conflictedCount} unresolved conflict{conflictedCount === 1 ? '' : 's'}"
      >
        <AlertTriangle size={12} class="shrink-0" />
        <span>{conflictedCount} conflict{conflictedCount === 1 ? '' : 's'}</span>
      </button>
    {/if}
  </div>

  <!-- Center Segment: Language mix and background activity -->
  <div class="hidden sm:flex items-center gap-3 text-micro min-w-0">
    <!-- The language breakdown used to be its own 32px strip. It is reference
         material, not a control, so it rides here and expands on demand. -->
    {#if detail === "full" && $interfaceStore.showLanguageBar}
      <LanguageSegment />
    {/if}
    <!-- Cadence reads the commits already loaded for the graph, so it costs
         no additional fetch. Hidden while syncing, when the window would be
         drawn from a partially loaded history. -->
    {#if detail === "full" && !$repoStore.isLoading && $graphStore.commits.length > 0}
      <CommitCadence commits={$graphStore.commits} days={30} />
    {/if}
    {#if $repoStore.isLoading}
      <div class="flex items-center gap-1.5 text-accent font-medium">
        <RefreshCw size={10} class="animate-spin shrink-0" />
        <span>Syncing…</span>
      </div>
    {/if}
  </div>

  <!-- Right Segment: Quick Shortcuts -->
  {#if detail === "full"}
  <div class="flex items-center gap-3">
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
          class="inline-flex h-5 max-w-[16rem] shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-1.5 text-[11px] font-medium leading-none transition-colors {$repoStore.terminalOpen
            ? 'border-accent/50 bg-accent/10 text-accent'
            : 'border-border/80 bg-surfaceHover text-textPrimary hover:border-accent/40 hover:bg-accent/10'}"
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
          class="fixed w-64 max-w-[calc(100vw-16px)] gp-menu gp-pop p-1 text-[11px] font-sans text-textPrimary"
          style="z-index: {LAYERS.MENU}"
        >
          <p class="px-2.5 py-1 text-textMuted" data-testid="terminal-pill-detail">{terminalPill.detail}</p>
          {#if terminalPill.sessions.length === 1 && terminalPill.sessions[0]?.where}
            <p class="px-2.5 pb-1 truncate text-textMuted" title={terminalPill.sessions[0]?.cwd}>{terminalPill.sessions[0]?.where}</p>
          {/if}
          {#if launchError}
            <p class="px-2.5 py-1 text-rose-600 dark:text-rose-400" role="alert">{launchError}</p>
          {/if}
          <button
            type="button"
            class="gp-menu-item text-[11px]"
            data-testid="terminal-pill-toggle"
            onclick={() => { toggleDock(); closeMenu(); }}
          >
            {$repoStore.terminalOpen ? "Hide terminal" : "Show terminal"}
          </button>
          {#if terminalPill.focusKey}
            <button
              type="button"
              class="gp-menu-item text-[11px]"
              data-testid="terminal-pill-focus"
              onclick={() => { if (terminalPill.focusKey) void focusListed(terminalPill.focusKey); }}
            >
              Focus session
            </button>
          {/if}
          <button
            type="button"
            class="gp-menu-item text-[11px]"
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
                class="gp-menu-item text-[11px]"
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

    <button
      type="button"
      onclick={openCommandPalette}
      class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors text-micro"
      title="Open Command Palette"
    >
      <span class="gp-keycap">{shortcutTextLabel("⌘K", $hostPlatform.os)}</span>
      <span class="hidden md:inline">Palette</span>
    </button>

    <button
      type="button"
      onclick={openShortcuts}
      class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors text-micro"
      title="Open Keyboard Shortcuts"
    >
      <Keyboard size={11} class="shrink-0" />
      <span class="gp-keycap">?</span>
      <span class="hidden md:inline">Shortcuts</span>
    </button>
  </div>
  {/if}
</footer>
{/if}
