<script lang="ts">
  import { maviStore } from "../mavi/store.svelte";
  import { showMaviThread } from "../mavi/navigate";
  import { STATUS_LABEL, STATUS_PRIORITY } from "../mavi/threads";
  import { repoStore } from "../stores/repoStore";
  import { hostPlatform } from "../stores/platformStore";
  import { describeError } from "../ui/copy/describeFailure";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { LAYERS } from "../ui/layers";
  import { ChevronDown, ChevronUp, MessagesSquare, Plus, Search, SquareTerminal, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { onDestroy, tick } from "svelte";
  import { portal } from "../dom/portal";
  import { invoke } from "../ipc/invoke";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { isCaseInsensitiveFs, sameRepo } from "../repos/paths";
  import { loadLeaseStatus, type LeaseWaiter } from "../stores/hostLoadStore";
  import { isTauri } from "../platform";
  import { terminalLaunchRequests } from "../terminal/launchRequests";
  import { CONTEXT_READ_MS } from "../terminal/inputQueue";
  import { contextReadOf, type TerminalContext } from "../terminal/sessionContext";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import {
    highlightSearchText,
    pillActionId,
    pillFilterCounts,
    pillSearchActions,
    resolvePillActionIndex,
    searchTerminalPill,
    type PillSessionFilter,
  } from "../terminal/pillSearch";
  import type { PillSessionView, TerminalPillView } from "../terminal/statusPill";
  import { stepSearchIndex, TERMINAL_SEARCH_QUERY_LIMIT } from "../terminal/tabSearch";
  import { launcherLabel, offeredSessionKind, sessionTypesFor, type SessionKind } from "../terminal/tabs";
  import { popover, restoreFocusTo, popoverExit } from "../ui/popover";
  import LauncherIcon from "./LauncherIcon.svelte";
  import RepoLogo from "./RepoLogo.svelte";

  /** Host context reads per batch. The rest keep the registry's own status. */
  const ACTIVITY_READ_LIMIT = 8;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  let {
    pillEl,
    terminalPill,
    focusSearchOnMount = false,
    onSearchFocused,
    onPointerEnter,
    onPointerLeave,
    onFocusIn,
    onFocusOut,
    onClose,
    onToggleDock,
    onActivity,
    onBuildQueue,
    onLaunchingChange,
  }: {
    pillEl: HTMLButtonElement | null;
    terminalPill: TerminalPillView;
    focusSearchOnMount?: boolean;
    onSearchFocused: () => void;
    onPointerEnter: () => void;
    onPointerLeave: () => void;
    onFocusIn: () => void;
    onFocusOut: (event: FocusEvent) => void;
    onClose: () => void;
    onToggleDock: () => void;
    onActivity: (patch: Record<string, TerminalContext | null>) => void;
    onBuildQueue: (queue: LeaseWaiter[]) => void;
    onLaunchingChange: (busy: boolean) => void;
  } = $props();

  let launching = $state(false);
  let launchError = $state<string | null>(null);
  let searchQuery = $state("");
  let sessionFilter = $state<PillSessionFilter>("all");
  /** Stable `${kind}:${key}` lock so live session churn never shifts the highlighted target. */
  let selectedActionId = $state<string | null>(null);
  let menuEl = $state<HTMLDivElement | null>(null);
  let searchInputEl = $state<HTMLInputElement | null>(null);
  /** Per-open deduplication so filtering past the first 8 sessions polls newly visible rows without re-reading earlier ones. */
  const polledSessionKeys = new Set<string>();
  let leasesPolled = false;

  let filterCounts = $derived(pillFilterCounts(terminalPill.sessions));
  let effectiveFilter = $derived.by<PillSessionFilter>(() => {
    if (sessionFilter === "agents" && filterCounts.agents === 0) return "all";
    if (sessionFilter === "attention" && filterCounts.attention === 0) return "all";
    if (sessionFilter === "current" && filterCounts.current === 0) return "all";
    return sessionFilter;
  });
  let quickLaunchers = $derived(sessionTypesFor($hostPlatform.browser_sessions));
  let defaultLauncherKind = $derived(
    offeredSessionKind($interfaceStore.terminalLauncher, $hostPlatform.browser_sessions),
  );
  let searchResults = $derived(
    searchTerminalPill({
      sessions: terminalPill.sessions,
      query: searchQuery,
      filter: effectiveFilter,
      openTabs: $repoStore.openTabs ?? [],
      recentRepos: $repoStore.recentRepos ?? [],
      currentRepoPath: $repoStore.currentPath,
      defaultLauncher: $interfaceStore.terminalLauncher,
      browserSessions: $hostPlatform.browser_sessions,
      identity: pathOpts,
    }),
  );
  let isSearching = $derived(searchResults.query.length > 0);
  let searchActions = $derived(isSearching ? pillSearchActions(searchResults) : []);
  let clampedResultIndex = $derived(resolvePillActionIndex(searchActions, selectedActionId));
  let activeOptionId = $derived(
    clampedResultIndex >= 0 ? searchActions[clampedResultIndex]?.optionId : undefined,
  );
  let sessionActionByKey = $derived(
    new Map(
      searchActions.flatMap((action, index) =>
        action.kind === "session" ? [[action.key, { index, optionId: action.optionId, id: pillActionId(action) }] as const] : [],
      ),
    ),
  );
  let launchActionByKey = $derived(
    new Map(
      searchActions.flatMap((action, index) =>
        action.kind === "launch" ? [[action.key, { index, optionId: action.optionId, id: pillActionId(action) }] as const] : [],
      ),
    ),
  );
  let activityKey = $derived(
    JSON.stringify(
      searchResults.sessions
        .flatMap((session) => (session.sessionId ? [[session.key, session.sessionId] as const] : []))
        .slice(0, ACTIVITY_READ_LIMIT),
    ),
  );

  $effect(() => {
    if (!focusSearchOnMount || !searchInputEl) return;
    searchInputEl.focus();
    onSearchFocused();
  });

  function closeMenu() {
    launchError = null;
    searchQuery = "";
    sessionFilter = "all";
    selectedActionId = null;
    onClose();
  }

  function clearSearch() {
    searchQuery = "";
    selectedActionId = null;
    searchInputEl?.focus();
  }

  function setSessionFilter(next: PillSessionFilter) {
    sessionFilter = next;
    selectedActionId = null;
  }

  function revealActiveOption(index: number) {
    const optionId = searchActions[index]?.optionId;
    if (!optionId) return;
    void tick().then(() => {
      menuEl?.querySelector<HTMLElement>(`#${optionId}`)?.scrollIntoView({ block: "nearest" });
    });
  }

  function activateSearchAction(action: (typeof searchActions)[number]) {
    if (action.kind === "session") {
      void focusListed(action.key);
    } else {
      void newTerminal(action.launch.repoPath, action.launch.launcher);
    }
  }

  function onSearchKeydown(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (searchQuery) {
        searchQuery = "";
        selectedActionId = null;
        return;
      }
      if (effectiveFilter !== "all") {
        sessionFilter = "all";
        selectedActionId = null;
        return;
      }
      closeMenu();
      restoreFocusTo(pillEl);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (isSearching) {
        if (searchActions.length === 0) return;
        event.preventDefault();
        event.stopPropagation();
        const next = stepSearchIndex(
          searchActions.length,
          clampedResultIndex,
          event.key === "ArrowDown" ? 1 : -1,
        );
        if (next >= 0 && searchActions[next]) {
          selectedActionId = pillActionId(searchActions[next]);
          revealActiveOption(next);
        }
        return;
      }
      if (event.key === "ArrowDown") {
        const first = menuEl?.querySelector<HTMLButtonElement>(
          "button:not(:disabled):not([data-testid='terminal-pill-search-clear'])",
        );
        if (first) {
          event.preventDefault();
          event.stopPropagation();
          first.focus();
        }
      }
      return;
    }
    if (event.key === "Enter" && isSearching) {
      if (event.shiftKey && searchResults.launches.length > 0) {
        const firstLaunch = searchResults.launches[0];
        event.preventDefault();
        event.stopPropagation();
        void newTerminal(firstLaunch.repoPath, firstLaunch.launcher);
        return;
      }
      const selected = searchActions[clampedResultIndex] ?? searchActions[0];
      if (!selected) return;
      event.preventDefault();
      event.stopPropagation();
      activateSearchAction(selected);
    }
  }

  function onMenuKeydown(event: KeyboardEvent) {
    if (event.target === searchInputEl) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeMenu();
      restoreFocusTo(pillEl);
      return;
    }
    if (
      event.key === "/" &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      searchInputEl
    ) {
      event.preventDefault();
      searchInputEl.focus();
      searchInputEl.select();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const items = [
      ...(event.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>(
        "button:not(:disabled):not([data-testid='terminal-pill-search-clear'])",
      ),
    ];
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "ArrowUp" && at === 0 && searchInputEl) {
      event.preventDefault();
      searchInputEl.focus();
      return;
    }
    const next = event.key === "ArrowDown" ? (at + 1) % items.length : (at - 1 + items.length) % items.length;
    event.preventDefault();
    items[next]?.focus();
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
      launchError = describeError(error, "terminal-pill");
    }
  }

  async function newTerminal(
    targetPath: string | null = $repoStore.currentPath,
    launcher: SessionKind = $interfaceStore.terminalLauncher,
  ) {
    if (!targetPath || launching) return;
    launching = true;
    onLaunchingChange(true);
    launchError = null;
    try {
      let launchPath = targetPath;
      const current = $repoStore.currentPath;
      if (!current || !sameRepo(targetPath, current, pathOpts)) {
        const openTab = ($repoStore.openTabs ?? []).find((tab) => sameRepo(tab.path, targetPath, pathOpts));
        if (openTab) {
          interfaceStore.setGlobalSurface("repository");
          if (openTab.id !== $repoStore.activeTabId) {
            await repoStore.activateTab(openTab.id);
          }
          launchPath = openTab.path;
        } else {
          // A recent or closed repository requires explicit trust verification
          // and canonical path resolution before spawning a terminal inside it.
          if (!(await repoStore.trustRepo(targetPath))) return;
          interfaceStore.setGlobalSurface("repository");
          let readyPath: string | null = null;
          const opened = await repoStore.openRepo(targetPath, {
            activate: true,
            onReady: (canonical) => {
              readyPath = canonical;
            },
          });
          if (!opened) {
            launchError = `Couldn't open ${targetPath}`;
            return;
          }
          launchPath = readyPath ?? targetPath;
        }
      }
      // Request first, then show the dock: the panel claims the request once
      // it is visible, which may be this open. Same order as the palette.
      const launched = terminalLaunchRequests.request(launchPath, launcher);
      repoStore.setTerminalOpen(true);
      await launched;
      closeMenu();
    } catch (error) {
      launchError = describeError(error, "terminal-pill");
    } finally {
      launching = false;
      onLaunchingChange(false);
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
    estimate: { width: 320, height: 280 },
    inset: 8,
    revision: `${terminalPill.detail}:${terminalPill.alive}:${launchError ?? ""}:${effectiveFilter}:${searchResults.query}:${searchResults.sessions.length}:${searchResults.launches.length}:${searchResults.hiddenLaunches}:${terminalPill.sessions.map((s) => `${s.key}:${s.displayTitle ?? ""}:${s.phrase ?? ""}`).join("|")}`,
    dismiss: {
      inside: "[data-terminal-pill], [data-terminal-pill-menu]",
      pointer: "pointerdown" as const,
      scroll: true,
      resize: true,
      escape: "none" as const,
    },
    onDismiss: () => closeMenu(),
  });

  function trackMenu(node: HTMLDivElement) {
    menuEl = node;
    return () => {
      if (menuEl === node) menuEl = null;
    };
  }

  function trackSearchInput(node: HTMLInputElement) {
    searchInputEl = node;
    return () => {
      if (searchInputEl === node) searchInputEl = null;
    };
  }

  /**
   * Reads host context for visible sessions once per open (deduplicated across
   * search refinement so newly matched sessions past the initial 8 still get
   * live context without re-polling already-read sessions).
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
        if (polledSessionKeys.has(sessionKey)) continue;
        targets.push({ key: sessionKey, sessionId });
      }
      if (targets.length === 0 && leasesPolled) return;
      const controller = new AbortController();
      let cancelled = false;
      void (async () => {
        const next: Record<string, TerminalContext | null> = {};
        if (!leasesPolled) {
          leasesPolled = true;
          try {
            const leases = await loadLeaseStatus(true);
            if (!cancelled) onBuildQueue(leases?.queue ?? []);
          } catch {
            // An unread queue says nothing: no session is called queued on a guess.
            if (!cancelled) onBuildQueue([]);
          }
        }
        for (const session of targets) {
          if (cancelled) return;
          polledSessionKeys.add(session.key);
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
        if (cancelled || Object.keys(next).length === 0) return;
        onActivity(next);
      })();
      return () => {
        cancelled = true;
        controller.abort();
      };
    };
  }

  onDestroy(() => {
    onLaunchingChange(false);
  });

  /** Mavi threads with a running agent, most urgent first: the pill lists every agent, not only terminals. */
  let maviLive = $derived(
    maviStore.records
      .filter((record) => {
        const phase = maviStore.summaryOf(record)?.phase;
        return !!phase && phase !== "ended";
      })
      .map((record) => ({ key: record.key, title: record.title, status: maviStore.status(record) }))
      .sort((a, b) => STATUS_PRIORITY.indexOf(a.status) - STATUS_PRIORITY.indexOf(b.status))
      .slice(0, 6),
  );
</script>

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
  aria-label="Dock sessions"
  use:portal={"body"}
  use:popover={menuPopover} out:popoverExit
  onpointerenter={onPointerEnter}
  onpointerleave={onPointerLeave}
  onfocusin={onFocusIn}
  onfocusout={onFocusOut}
  onkeydown={onMenuKeydown}
  class="fixed w-80 max-w-[calc(100vw-16px)] gp-menu gp-pop p-1.5 text-meta font-sans text-textPrimary"
  style="z-index: {LAYERS.MENU}"
>
  {#snippet highlightedText(text: string)}
    {#if isSearching}
      {#each highlightSearchText(text, searchResults.query) as chunk}
        {#if chunk.matched}<mark class="rounded-xs bg-accent/20 text-textPrimary">{chunk.text}</mark>{:else}{chunk.text}{/if}
      {/each}
    {:else}
      {text}
    {/if}
  {/snippet}

  {#snippet sessionCard(
    session: PillSessionView,
    optionId: string | undefined,
    highlighted: boolean,
    actionId: string | undefined,
    asOption: boolean,
  )}
    <button
      type="button"
      id={optionId}
      role={asOption ? "option" : undefined}
      aria-selected={asOption ? highlighted : undefined}
      class="gp-menu-item flex-col! items-stretch! gap-1 py-1.5! text-meta {highlighted
        ? 'bg-accent/15 ring-1 ring-inset ring-accent/35'
        : session.isActive && session.isCurrentRepo
          ? 'bg-accent/10'
          : ''}"
      data-testid="terminal-pill-session"
      disabled={!session.canReveal}
      title={`${session.agentLabel}${session.displayTitle ? ` — ${session.displayTitle}` : ""} · ${session.cwd ?? session.checkout ?? session.repoPath}`}
      onpointerenter={() => {
        if (actionId !== undefined) selectedActionId = actionId;
      }}
      onclick={() => void focusListed(session.key)}
    >
      <span class="flex w-full min-w-0 items-center gap-1.5">
        <LauncherIcon kind={session.launcherKind} size={14} />
        <span class="min-w-0 flex-1 truncate font-medium text-textPrimary" data-testid="terminal-pill-session-title">
          <span class="sr-only">Switch to {" "}</span>{@render highlightedText(session.displayTitle ?? session.agentLabel)}
        </span>
        {#if session.splitSlot}
          <span class="gp-chip shrink-0 tabular-nums" data-tone={session.isActive ? "accent" : "neutral"} data-testid="terminal-pill-session-split">#{session.splitSlot}</span>
        {/if}
        {#if session.isActive}
          <span class="gp-chip shrink-0" data-tone="accent" data-testid="terminal-pill-session-active">Active</span>
        {/if}
        {#if session.attention}
          <span class="gp-chip shrink-0" data-tone={session.attention === "Failed" ? "danger" : "warn"} data-testid="terminal-pill-session-attention">{session.attention}</span>
        {/if}
      </span>
      <span class="flex w-full min-w-0 items-center gap-1 text-micro text-textMuted">
        <RepoLogo path={session.logoPath} name={session.repository} size={12} />
        <span class="shrink-0 max-w-[8rem] truncate font-medium text-textSecondary" data-testid="terminal-pill-session-repo">{@render highlightedText(session.repository)}</span>
        {#if session.checkoutName}
          <span class="min-w-0 truncate font-mono text-textMuted" data-testid="terminal-pill-session-checkout">/{@render highlightedText(session.checkoutName)}</span>
        {:else if session.where && (session.where.includes("/") || session.where !== session.repository)}
          <span class="min-w-0 truncate font-mono text-textMuted">/{@render highlightedText(session.where)}</span>
        {/if}
        {#if session.displayTitle}
          <span class="shrink-0 text-textMuted" data-testid="terminal-pill-session-agent">· {@render highlightedText(session.agentLabel)}</span>
        {/if}
        {#if !session.attention && session.phrase}
          <span class="ml-auto shrink-0 max-w-[9rem] truncate text-textMuted" title={session.phrase} data-testid="terminal-pill-session-phrase">{session.phrase}</span>
        {/if}
      </span>
    </button>
  {/snippet}

  <div class="flex items-center justify-between gap-2 px-2 py-1">
    <span class="inline-flex min-w-0 items-center gap-1.5 truncate font-medium text-textSecondary" data-testid="terminal-pill-detail">
      {#if terminalPill.highlightedLauncher}
        <LauncherIcon kind={terminalPill.highlightedLauncher} size={12} class="shrink-0" />
      {:else}
        <SquareTerminal size={12} class="shrink-0 text-accent" />
      {/if}
      <span class="truncate">{terminalPill.detail}</span>
    </span>
    <span class="shrink-0 font-mono text-micro text-textMuted">{shortcutTextLabel('⌃`', $hostPlatform.os)}</span>
  </div>
  <div class="relative px-1 pb-1 pt-0.5">
    <Search size={12} class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-textMuted" aria-hidden="true" />
    <input
      {@attach trackSearchInput}
      type="text"
      role="combobox"
      autocomplete="off"
      spellcheck="false"
      maxlength={TERMINAL_SEARCH_QUERY_LIMIT}
      data-testid="terminal-pill-search"
      aria-label="Search terminals, agents, or repositories"
      aria-autocomplete="list"
      aria-expanded={isSearching}
      aria-controls={isSearching ? "terminal-pill-results" : undefined}
      aria-activedescendant={isSearching ? activeOptionId : undefined}
      placeholder="Search agents, repositories, or new session…"
      bind:value={searchQuery}
      oninput={() => { selectedActionId = null; }}
      onkeydown={onSearchKeydown}
      class="gp-field w-full pl-6! {searchQuery ? 'pr-6!' : ''}"
      data-size="sm"
    />
    {#if searchQuery}
      <button
        type="button"
        class="gp-icon-btn absolute right-1.5 top-1/2 -translate-y-1/2"
        data-size="sm"
        data-testid="terminal-pill-search-clear"
        aria-label="Clear search"
        title="Clear search (Esc)"
        onclick={clearSearch}
      >
        <X size={11} aria-hidden="true" />
      </button>
    {/if}
  </div>
  {#if filterCounts.showBar}
    <div
      class="flex flex-wrap items-center gap-1 px-1 pb-1"
      role="group"
      aria-label="Filter sessions"
      data-testid="terminal-pill-filters"
    >
      <button
        type="button"
        class="gp-seg-btn px-1.5! text-micro!"
        data-active={effectiveFilter === "all"}
        aria-pressed={effectiveFilter === "all"}
        data-testid="terminal-pill-filter-all"
        onclick={() => setSessionFilter("all")}
      >
        <span>All</span>
        <span class="font-mono tabular-nums opacity-75">{filterCounts.total}</span>
      </button>
      {#if filterCounts.agents > 0 && filterCounts.agents < filterCounts.total}
        <button
          type="button"
          class="gp-seg-btn px-1.5! text-micro!"
          data-active={effectiveFilter === "agents"}
          aria-pressed={effectiveFilter === "agents"}
          data-testid="terminal-pill-filter-agents"
          onclick={() => setSessionFilter("agents")}
        >
          <span>Agents</span>
          <span class="font-mono tabular-nums opacity-75">{filterCounts.agents}</span>
        </button>
      {/if}
      {#if filterCounts.attention > 0}
        <button
          type="button"
          class="gp-seg-btn px-1.5! text-micro!"
          data-active={effectiveFilter === "attention"}
          aria-pressed={effectiveFilter === "attention"}
          data-testid="terminal-pill-filter-attention"
          onclick={() => setSessionFilter("attention")}
        >
          <span>Attention</span>
          <span class="font-mono tabular-nums text-warn">{filterCounts.attention}</span>
        </button>
      {/if}
      {#if filterCounts.current > 0 && filterCounts.current < filterCounts.total}
        <button
          type="button"
          class="gp-seg-btn px-1.5! text-micro!"
          data-active={effectiveFilter === "current"}
          aria-pressed={effectiveFilter === "current"}
          data-testid="terminal-pill-filter-current"
          onclick={() => setSessionFilter("current")}
        >
          <span>Current</span>
          <span class="font-mono tabular-nums opacity-75">{filterCounts.current}</span>
        </button>
      {/if}
    </div>
  {/if}
  {#if launchError}
    <p class="px-2.5 py-1 text-danger" role="alert">{launchError}</p>
  {/if}
  {#if isSearching}
    <div class="gp-menu-sep" role="separator"></div>
    <div
      id="terminal-pill-results"
      role="listbox"
      aria-label="Matching sessions and new session targets"
      class="max-h-72 overflow-y-auto flex flex-col gap-0.5 py-0.5"
      data-testid="terminal-pill-session-list"
    >
      {#if searchResults.sessions.length > 0}
        {#if searchResults.launches.length > 0}
          <span class="gp-label px-2 pt-0.5" role="presentation">Sessions ({searchResults.sessions.length})</span>
        {/if}
        {#each searchResults.sessions as session (session.key)}
          {@const action = sessionActionByKey.get(session.key)}
          {@render sessionCard(
            session,
            action?.optionId,
            action !== undefined && action.index === clampedResultIndex,
            action?.id,
            Boolean(action),
          )}
        {/each}
      {/if}
      {#if searchResults.launches.length > 0}
        {#if searchResults.sessions.length > 0}
          <div class="gp-menu-sep my-0.5" role="presentation"></div>
        {/if}
        <div class="flex items-center justify-between gap-2 px-2 pt-0.5" role="presentation">
          <span class="gp-label">New session</span>
          {#if searchResults.sessions.length > 0}
            <span class="font-mono text-micro text-textMuted" title="Press Shift+Enter to start the top new session">⇧↵</span>
          {/if}
        </div>
        {#each searchResults.launches as launch (launch.key)}
          {@const action = launchActionByKey.get(launch.key)}
          {@const highlighted = action !== undefined && action.index === clampedResultIndex}
          <button
            type="button"
            id={action?.optionId}
            role="option"
            aria-selected={highlighted}
            class="gp-menu-item flex-col! items-stretch! gap-0.5 py-1.5! text-meta {highlighted ? 'bg-accent/15 ring-1 ring-inset ring-accent/35' : ''}"
            data-testid="terminal-pill-launch"
            data-launch-source={launch.source}
            disabled={launching}
            title={`Start ${launch.launcherLabel} in ${launch.repoPath}`}
            onpointerenter={() => {
              if (action !== undefined) selectedActionId = action.id;
            }}
            onclick={() => void newTerminal(launch.repoPath, launch.launcher)}
          >
            <span class="flex w-full min-w-0 items-center gap-1.5">
              <LauncherIcon kind={launch.launcher} size={13} />
              <span class="min-w-0 flex-1 truncate font-medium text-textPrimary" data-testid="terminal-pill-launch-label">{@render highlightedText(launch.label)}</span>
              <Plus size={12} class="shrink-0 text-textMuted" aria-hidden="true" />
            </span>
            <span class="flex w-full min-w-0 items-center gap-1 text-micro text-textMuted">
              <RepoLogo path={launch.logoPath} name={launch.repository} size={12} />
              <span class="shrink-0 max-w-[9rem] truncate font-medium text-textSecondary" data-testid="terminal-pill-launch-repo">{@render highlightedText(launch.repository)}</span>
              {#if launch.checkoutName}
                <span class="min-w-0 truncate font-mono text-textMuted">/{@render highlightedText(launch.checkoutName)}</span>
              {/if}
              {#if launch.source === "recent"}
                <span class="ml-auto shrink-0 text-textMuted" data-testid="terminal-pill-launch-recent">Recent</span>
              {:else if launch.isCurrentRepo}
                <span class="ml-auto shrink-0 text-textMuted">This repository</span>
              {:else if launch.group}
                <span class="ml-auto shrink-0 max-w-[6rem] truncate text-textMuted">{@render highlightedText(launch.group)}</span>
              {/if}
            </span>
          </button>
        {/each}
        {#if searchResults.hiddenLaunches > 0}
          <p class="px-2 py-1 text-micro text-textMuted" role="presentation" data-testid="terminal-pill-launch-overflow">
            +{searchResults.hiddenLaunches} more {searchResults.hiddenLaunches === 1 ? "option" : "options"} — type to narrow
          </p>
        {/if}
      {/if}
      {#if searchResults.sessions.length === 0 && searchResults.launches.length === 0}
        <p class="px-2.5 py-2 text-textMuted" role="presentation" data-testid="terminal-pill-empty">
          No matching sessions or session types.
        </p>
      {/if}
    </div>
  {:else}
    {#if terminalPill.sessions.length > 0}
      <div class="gp-menu-sep" role="separator"></div>
      <div class="max-h-60 overflow-y-auto flex flex-col gap-0.5 py-0.5" data-testid="terminal-pill-session-list">
        {#each searchResults.sessions as session (session.key)}
          {@render sessionCard(session, undefined, false, undefined, false)}
        {/each}
      </div>
    {/if}
    <div class="gp-menu-sep" role="separator"></div>
    <div class="px-1 py-1" role="group" aria-label="Quick start session" data-testid="terminal-pill-quick-launchers">
      <div class="flex flex-wrap items-center gap-1">
        {#each quickLaunchers as item (item.kind)}
          <button
            type="button"
            class="gp-btn h-6! gap-1! px-2! text-micro! {item.kind === defaultLauncherKind ? 'border-accent bg-accent/10 text-textPrimary' : ''}"
            data-size="sm"
            data-testid="terminal-pill-quick-launcher"
            data-launcher={item.kind}
            disabled={launching}
            title={`Start a new ${item.label} session in this repository`}
            onclick={() => void newTerminal($repoStore.currentPath, item.kind)}
          >
            <LauncherIcon kind={item.kind} size={11} />
            <span>{item.label}</span>
          </button>
        {/each}
      </div>
    </div>
    {#if maviLive.length > 0}
      <div class="gp-menu-sep" role="separator"></div>
      <div class="flex flex-col gap-0.5 py-0.5" role="group" aria-label="Mavi threads" data-testid="terminal-pill-mavi">
        <span class="gp-label px-2.5 py-0.5">Mavi</span>
        {#each maviLive as thread (thread.key)}
          <button
            type="button"
            class="gp-menu-item text-meta"
            title="Open this conversation in Mavi"
            onclick={() => { closeMenu(); void showMaviThread(thread.key); }}
          >
            <MessagesSquare size={13} class="shrink-0 {thread.status === 'needs_you' ? 'text-warn' : 'text-accent'}" />
            <span class="min-w-0 flex-1 truncate">{thread.title}</span>
            <span class="gp-chip shrink-0" data-tone={thread.status === "needs_you" ? "warn" : thread.status === "failed" ? "danger" : "neutral"}>{STATUS_LABEL[thread.status]}</span>
          </button>
        {/each}
      </div>
    {/if}
    <div class="gp-menu-sep" role="separator"></div>
    <button
      type="button"
      class="gp-menu-item text-meta"
      data-testid="terminal-pill-toggle"
      onclick={() => { onToggleDock(); closeMenu(); }}
    >
      {#if $repoStore.terminalOpen}<ChevronDown size={13} class="shrink-0 text-textMuted" />{:else}<ChevronUp size={13} class="shrink-0 text-textMuted" />{/if}
      <span>{$repoStore.terminalOpen ? "Hide dock" : "Show dock"}</span>
    </button>
    {#if terminalPill.focusKey}
      <button
        type="button"
        class="gp-menu-item text-meta"
        data-testid="terminal-pill-focus"
        onclick={() => { if (terminalPill.focusKey) void focusListed(terminalPill.focusKey); }}
      >
        <SquareTerminal size={13} class="shrink-0 text-accent" />
        <span>Focus session</span>
      </button>
    {/if}
    <button
      type="button"
      class="gp-menu-item text-meta"
      data-testid="terminal-pill-new"
      disabled={launching}
      onclick={() => void newTerminal()}
    >
      <LauncherIcon kind={$interfaceStore.terminalLauncher} size={13} />
      <span class="flex-1 truncate text-left">New {launcherLabel($interfaceStore.terminalLauncher)} session</span>
      <Plus size={12} class="shrink-0 text-textMuted" />
    </button>
  {/if}
</div>
