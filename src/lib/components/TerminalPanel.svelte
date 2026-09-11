<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { terminalLaunchRequests } from "../terminal/launchRequests";
  import { taskTerminalRequests, consumeTaskTerminal } from "../terminal/taskLaunches";
  import { consoleLaunchRequests, consumeConsoleLaunch } from "../terminal/consoleLaunches";
  import { boundedCommand, retainCommand, retainExecutions, followsConsoleOutput } from "../terminal/consoleHistory";
  import { harnessStore } from "../stores/harnessStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Terminal,
    Play,
    LoaderCircle,
    Trash2,
    Clipboard,
    Check,
    AlertCircle,
    Shield,
    Clock,
    SquareTerminal,
    ListChecks,
    X,
    Plus,
    Search,
    ChevronDown,
    Maximize2,
    Minimize2,
    Keyboard,
    Columns2,
    Settings2,
    ChevronLeft,
    ChevronRight,
  } from "@lucide/svelte";
  import { tokenizeCommand } from "../terminal/tokenize";
  import type { TerminalRunResult } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import TerminalSession from "./TerminalSession.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import {
    LAUNCHERS,
    MAX_TERMINAL_TABS,
    activateTab,
    canOpenTab,
    closeTab,
    cycleTab,
    initialState,
    launcherLabel,
    openTab,
    setTabTitle,
    renameTab,
    moveTab,
    tabLabel,
    terminalTabChord,
    terminalTabDestination,
    type LauncherKind,
    type TabState,
  } from "../terminal/tabs";

  /** The shared wire shape; aliased for this panel's existing call sites. */
  type TerminalRunResponse = TerminalRunResult;

  let {
    repoPath = null,
    visible = true,
    onClose,
    expanded = false,
    onToggleExpanded,
  }: {
    repoPath?: string | null;
    /** False while another repository's panel (or a closed dock) is showing. */
    visible?: boolean;
    onClose?: () => void;
    expanded?: boolean;
    onToggleExpanded?: () => void;
  } = $props();

  interface ExecutionEntry {
    id: string;
    command: string;
    timestamp: number;
    running: boolean;
    result?: TerminalRunResponse;
    error?: string;
  }

  let commandInput = $state("");
  let history = $state<string[]>([]);
  let historyIndex = $state(-1);
  let savedDraft = $state("");
  let executions = $state<ExecutionEntry[]>([]);
  let running = $state(false);
  let consoleFollowing = $state(true);
  let discardedExecutions = $state(0);
  let validationError = $state<string | null>(null);
  let copiedId = $state<string | null>(null);

  let inputEl = $state<HTMLInputElement | null>(null);
  let scrollContainer = $state<HTMLDivElement | null>(null);
  let tabScroller: HTMLDivElement | undefined = $state();
  /** Copy-feedback reset timer; cleared on teardown so it cannot fire post-unmount. */
  let copiedResetTimer: ReturnType<typeof setTimeout> | null = null;

  onMount(() => {
    inputEl?.focus();
    return () => {
      if (copiedResetTimer !== null) {
        clearTimeout(copiedResetTimer);
        copiedResetTimer = null;
      }
    };
  });

  // ---------------------------------------------------------------------
  // Interactive shells (PTY), one per tab. They run OUTSIDE the MANVI gate by
  // nature: a shell can execute anything, so claiming gate coverage here would
  // be a check that cannot run reporting what a check that ran reports. The
  // bounded Console tab is the gated surface.
  //
  // Session ownership lives in TerminalSession, one instance per tab. This
  // component owns only the strip: which tabs exist, which is focused, and
  // what each is called. The repository this panel talks to is a prop from
  // the dock — one panel per open repo tab, hidden rather than remounted —
  // so a tab switch cannot kill the shells, and a hidden panel cannot follow
  // `currentPath` into a different worktree.
  // ---------------------------------------------------------------------
  type PtyMode = "shell" | "console";
  let mode = $state<PtyMode>("shell");
  function initialTabs(): TabState {
    if (get(terminalLaunchRequests)?.repoPath === repoPath) return { tabs: [], activeId: null };
    const request = get(taskTerminalRequests).find((request) => request.repoPath === repoPath);
    return request ? initialState(request.provider, { runId: request.runId, title: request.title }) : initialState();
  }
  let tabState = $state<TabState>(untrack(initialTabs));
  const activeId = $derived(tabState.activeId);
  const activeTitle = $derived(tabState.tabs.find((tab) => tab.id === activeId)?.title);
  let sessions = $state<Record<string, TerminalSession | undefined>>({});
  let nextLauncher = $state<LauncherKind>(get(interfaceStore).terminalLauncher);
  let splitIds = $state<[string, string] | null>(null);
  let tabOptions = $state(false);
  let sessionListOpen = $state(false);
  let renameValue = $state("");
  let tabStatuses = $state<Record<string, string>>({});
  let unread = $state(new Set<string>());
  const canCreate = $derived(canOpenTab(tabState) && $terminalSessions.length < MAX_TERMINAL_TABS);
  const capacityTitle = $derived(canCreate ? "New terminal session" : `All ${MAX_TERMINAL_TABS} terminal sessions are open — close one in Sessions`);
  let shortcutsOpen = $state(false);
  let focusTabStrip = false;

  $effect(() => {
    const request = $taskTerminalRequests.find((request) => request.repoPath === repoPath);
    if (!request || (!canCreate && !tabState.tabs.some((tab) => tab.taskRunId === request.runId))) return;
    untrack(() => {
      tabState = openTab(tabState, request.provider, { runId: request.runId, title: request.title });
      mode = "shell";
      consumeTaskTerminal(request.runId);
    });
  });

  $effect(() => {
    const queued = $consoleLaunchRequests[0];
    if (!queued || !visible || !repoPath) return;
    if (running) return;
    untrack(() => {
      const claimed = consumeConsoleLaunch();
      if (!claimed) return;
      mode = "console";
      void execute(claimed.command, claimed.timeoutSecs ?? 1200);
    });
  });

  function newTab(launcher: LauncherKind, initialPrompt?: string): boolean {
    if (!repoPath || !canCreate) return false;
    focusTabStrip = false;
    tabState = openTab(tabState, launcher, initialPrompt);
    if (splitIds && activeId) splitIds = [splitIds[0], activeId];
    return true;
  }

  $effect(() => {
    const request = $terminalLaunchRequests;
    if (!request || !visible || request.repoPath !== repoPath) return;
    untrack(() => {
      const claimed = terminalLaunchRequests.take(request.repoPath);
      if (!claimed) return;
      mode = "shell";
      const opened = newTab(claimed.launcher, claimed.prompt);
      const id = tabState.activeId;
      if (opened && id) terminalLaunchRequests.remember({
        id, repoPath: claimed.repoPath, launcher: claimed.launcher, status: "starting",
        reveal() {
          mode = "shell";
          selectTab(id);
          void tick().then(() => sessions[id]?.reveal());
        },
      });
      claimed.complete(opened ? undefined : capacityTitle);
    });
  });

  onDestroy(() => { for (const tab of tabState.tabs) terminalLaunchRequests.forget(tab.id); });

  function selectTab(id: string, keepStripFocus = false) {
    focusTabStrip = keepStripFocus;
    if (splitIds && !splitIds.includes(id)) splitIds = [splitIds[0], id];
    tabState = activateTab(tabState, id);

  }

  /**
   * Closing drops the component, whose teardown kills the shell. Emptying the
   * strip is allowed and leaves the empty state, which offers a new tab — a
   * terminal that silently respawns what you just closed is worse than one
   * that waits to be asked.
   */
  function dropTab(id: string) {
    terminalLaunchRequests.forget(id);
    focusTabStrip = false;
    if (splitIds?.includes(id)) splitIds = null;
    tabState = closeTab(tabState, id);
    const { [id]: _status, ...statuses } = tabStatuses;
    tabStatuses = statuses;
    unread = new Set([...unread].filter((key) => key !== id));
    const { [id]: _gone, ...rest } = sessions;
    sessions = rest;
  }

  function handleChord(event: KeyboardEvent): boolean {
    if (mode !== "shell") return false;
    if (!visible || event.defaultPrevented) return false;
    const chord = terminalTabChord(event);
    if (!chord) return false;
    event.preventDefault();
    event.stopPropagation();
    focusTabStrip = false;
    if (chord === "new") newTab("shell");
    else if (chord === "close" && tabState.activeId) dropTab(tabState.activeId);
    else if (chord === "next" || chord === "prev") {
      const next = cycleTab(tabState, chord === "next" ? 1 : -1).activeId;
      if (next) selectTab(next);
    }
    return true;
  }

  function handlePanelKey(event: KeyboardEvent) {
    if (!visible || event.defaultPrevented) return;
    if (event.key === "Escape" && !isImeComposition(event) && (tabOptions || sessionListOpen || shortcutsOpen)) {
      tabOptions = false; sessionListOpen = false; shortcutsOpen = false;
      event.preventDefault(); event.stopPropagation();
      if (activeId) sessions[activeId]?.reveal();
      return;
    }
    if (handleChord(event)) return;
    if (mode === "shell" && activeId) sessions[activeId]?.handleViewChord(event);
  }

  async function handleTabKey(event: KeyboardEvent) {
    if (isImeComposition(event) || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    const id = terminalTabDestination(tabState, event.key);
    if (!id) return;
    event.preventDefault();
    event.stopPropagation();
    selectTab(id, true);
    await tick();
    if (visible && mode === "shell" && activeId === id) {
      tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-tab-${id}"]`)?.focus();
    }
  }

  /**
   * A hidden xterm cannot lay out, so a tab that becomes active has to be told
   * to refit — its ResizeObserver only fires after the browser recomputes
   * layout, and the grid it would paint until then is the stale one.
   */
  $effect(() => {
    if (!visible || mode !== "shell") return;
    const id = activeId;
    if (!id) return;
    if (unread.has(id)) untrack(() => { unread = new Set([...unread].filter((key) => key !== id)); });
    const session = sessions[id];
    void tick().then(() => {
      if (!visible || mode !== "shell" || activeId !== id) return;
      session?.reveal();
      if (focusTabStrip) tabScroller?.querySelector<HTMLButtonElement>(`[id="terminal-tab-${id}"]`)?.focus();
    });
  });

  // OSC titles can widen a tab after it is selected. Keep its entire control
  // (including Close) visible without refocusing the shell on every title.
  $effect(() => {
    activeTitle;
    if (!visible || mode !== "shell") return;
    const id = activeId;
    const scroller = tabScroller;
    void tick().then(() => {
      if (!visible || mode !== "shell" || activeId !== id) return;
      scroller?.querySelector(`[data-terminal-tab="${id}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
    });
  });

  function toggleSplit() {
    if (splitIds) { splitIds = null; return; }
    const first = activeId;
    if (!first) return;
    let second = tabState.tabs.find((tab) => tab.id !== first)?.id;
    if (!second) { newTab(nextLauncher); second = activeId ?? undefined; }
    if (second && second !== first) splitIds = [first, second];
  }

  function showTabOptions() {
    renameValue = tabState.tabs.find((tab) => tab.id === activeId)?.name ?? "";
    tabOptions = !tabOptions;
    sessionListOpen = false; shortcutsOpen = false;
  }

  function saveTabName(event: SubmitEvent) {
    event.preventDefault();
    if (activeId) tabState = renameTab(tabState, activeId, renameValue);
    tabOptions = false;
  }

  function outputAction(event: Event) {
    if (!(event.currentTarget instanceof HTMLSelectElement) || !activeId) return;
    const session = sessions[activeId], action = event.currentTarget.value;
    event.currentTarget.value = "";
    if (action === "selection") void session?.copySelection();
    else if (action === "copy") void session?.copyOutput();
    else if (action === "export") session?.exportOutput();
  }

  function trimExecutions() {
    const retained = retainExecutions(executions);
    discardedExecutions += executions.length - retained.length;
    executions = retained;
  }

  const QUICK_COMMANDS = [
    "git status",
    "git log -n 5 --oneline",
    "git diff --stat",
    "npm test",
    "cargo check",
  ];

  async function scrollToBottom() {
    await tick();
    if (scrollContainer) {
      scrollContainer.scrollTop = scrollContainer.scrollHeight;
    }
  }

  async function execute(rawCommand?: string, timeoutSecs = 600) {
    const textToRun = (rawCommand ?? commandInput).trim();
    if (!textToRun || running) return;

    if (!repoPath) {
      validationError = "No repository open.";
      return;
    }

    const tokenized = tokenizeCommand(textToRun);
    if (!tokenized.ok) {
      validationError = tokenized.error;
      return;
    }

    if (!boundedCommand(textToRun)) {
      validationError = "Console commands are limited to 64 KiB.";
      return;
    }
    validationError = null;

    // Update command history
    history = retainCommand(history, textToRun);
    historyIndex = -1;
    savedDraft = "";
    commandInput = "";

    const entryId = `exec-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    const entry: ExecutionEntry = {
      id: entryId,
      command: textToRun,
      timestamp: Date.now(),
      running: true,
    };

    executions = [...executions, entry];
    trimExecutions();
    consoleFollowing = true;
    running = true;
    void scrollToBottom();

    try {
      const response = await invoke<TerminalRunResponse>("cmd_terminal_run", {
        repoPath,
        args: tokenized.argv,
        // Long enough for a cold install/build; the backend clamps to [1s, 30min].
        timeoutSecs,
      });

      executions = executions.map((e) =>
        e.id === entryId ? { ...e, running: false, result: response } : e,
      );

      harnessStore.recordAction({
        repoPath,
        kind: "terminal",
        label: textToRun,
        ok: !response.timed_out && response.exit_code === 0,
        verdict: response.policy ?? null,
      });
    } catch (err) {
      const errMessage = formatError(err);
      executions = executions.map((e) =>
        e.id === entryId ? { ...e, running: false, error: errMessage } : e,
      );

      harnessStore.recordAction({
        repoPath,
        kind: "terminal",
        label: textToRun,
        ok: false,
      });
    } finally {
      running = false;
      trimExecutions();
      if (consoleFollowing && visible && mode === "console") void scrollToBottom();
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Enter/Arrow keys during an IME conversion belong to the composition,
    // not to command execution or history navigation.
    if (isImeComposition(e)) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void execute();
    } else if (e.key === "ArrowUp") {
      if (history.length === 0) return;
      e.preventDefault();
      if (historyIndex === -1) {
        savedDraft = commandInput;
        historyIndex = history.length - 1;
      } else if (historyIndex > 0) {
        historyIndex -= 1;
      }
      commandInput = history[historyIndex] ?? "";
    } else if (e.key === "ArrowDown") {
      if (historyIndex === -1) return;
      e.preventDefault();
      if (historyIndex < history.length - 1) {
        historyIndex += 1;
        commandInput = history[historyIndex] ?? "";
      } else {
        historyIndex = -1;
        commandInput = savedDraft;
      }
    }
  }

  function clearOutput() {
    executions = [];
    discardedExecutions = 0;
    validationError = null;
  }

  async function copyOutput(entry: ExecutionEntry) {
    let text = `$ ${entry.command}\n`;
    if (entry.result) {
      if (entry.result.stdout_tail) text += `${entry.result.stdout_tail}\n`;
      if (entry.result.stderr_tail) text += `${entry.result.stderr_tail}\n`;
    } else if (entry.error) {
      text += `Error: ${entry.error}\n`;
    }
    if (await copyText(text.trim())) {
      copiedId = entry.id;
      if (copiedResetTimer !== null) clearTimeout(copiedResetTimer);
      copiedResetTimer = setTimeout(() => {
        copiedResetTimer = null;
        if (copiedId === entry.id) copiedId = null;
      }, 1500);
    }
  }

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(2)}s`;
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<!-- Justified: keyboard events bubble from the region's interactive controls; the region itself is not a focus stop. -->
<div class="relative flex-1 flex flex-col bg-background h-full min-w-0 text-xs font-sans overflow-hidden" role="region" aria-label="Terminal" onkeydown={handlePanelKey}>
  <!-- Header Bar -->
  <div class="px-3 py-1.5 border-b border-border/60 gp-section-edge bg-surface/60 flex flex-wrap gap-2 items-center shrink-0">
    <div class="flex flex-1 items-center gap-2 min-w-0">
      <Terminal size={14} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Terminal</span>
      <span class="text-textMuted font-mono truncate text-[10px]" title={repoPath ?? "No repository"}>
        {repoPath?.split(/[\\/]/).pop() ?? "No repository"}
      </span>
    </div>
    <div class="flex items-center gap-1 shrink-0">
      <div class="gp-segmented" role="group" aria-label="Terminal mode">
        <button
          type="button"
          aria-pressed={mode === "shell"}
          data-active={mode === "shell" ? "true" : "false"}
          class="gp-seg-btn text-[11px]! py-0.5!"
          onclick={() => (mode = "shell")}
          title="A real interactive shell in this repository"
        >
          <SquareTerminal size={11} class="inline mr-1 -mt-0.5" />Shell
        </button>
        <button
          type="button"
          aria-pressed={mode === "console"}
          data-active={mode === "console" ? "true" : "false"}
          class="gp-seg-btn text-[11px]! py-0.5!"
          onclick={() => (mode = "console")}
          title="Run single commands with capped output and per-run policy verdicts"
        >
          <ListChecks size={11} class="inline mr-1 -mt-0.5" />Console
        </button>
      </div>
      {#if mode === "shell"}
        <button type="button" class="gp-icon-btn" disabled={!activeId || !repoPath} aria-label="Find in terminal" title="Find in terminal (⌘F / Ctrl+Shift+F)" onclick={() => activeId && sessions[activeId]?.openFind()}><Search size={13} /></button>
        <button type="button" class="gp-icon-btn" disabled={!activeId || !repoPath} aria-label="Clear scrollback" title="Clear scrollback; keep the current prompt and session" onclick={() => activeId && sessions[activeId]?.clearScrollback()}><Trash2 size={13} /></button>
      {/if}
      {#if mode === "console" && executions.length > 0}
        <button
          type="button"
          onclick={clearOutput}
          class="gp-btn py-1!"
          title="Clear terminal output"
        >
          <Trash2 size={12} />
          <span>Clear</span>
        </button>
      {/if}
      <button type="button" class="gp-icon-btn text-[10px]!" aria-label="All terminal sessions" aria-expanded={sessionListOpen} title="Sessions across repositories" onclick={() => { sessionListOpen = !sessionListOpen; tabOptions = false; shortcutsOpen = false; }}>{$terminalSessions.length}/{MAX_TERMINAL_TABS}</button>
      <button type="button" class="gp-icon-btn" aria-label="Terminal shortcuts" aria-expanded={shortcutsOpen} title="Terminal shortcuts" onclick={() => { shortcutsOpen = !shortcutsOpen; tabOptions = false; sessionListOpen = false; }}><Keyboard size={13} /></button>
      {#if onToggleExpanded}
        <button type="button" class="gp-icon-btn" aria-label={expanded ? "Restore terminal size" : "Expand terminal"} title={expanded ? "Restore terminal size" : "Expand terminal"} onclick={onToggleExpanded}>
          {#if expanded}<Minimize2 size={13} />{:else}<Maximize2 size={13} />{/if}
        </button>
      {/if}
      {#if onClose}
        <button type="button" class="gp-icon-btn" aria-label="Hide the terminal dock" title="Hide the terminal (⌃`) — sessions keep running" onclick={onClose}><ChevronDown size={14} /></button>
      {/if}
    </div>
  </div>

  {#if sessionListOpen}
    <div class="terminal-popover px-3 py-2 overflow-auto border-b border-border/60 gp-section-edge bg-surface text-[11px]" aria-label="Sessions across repositories">
      {#each $terminalSessions as session (session.key)}
        <div class="flex gap-2 items-center py-0.5">
          <span class="flex-1 min-w-0 truncate" title={session.repoPath}>{session.repoPath.split(/[\\/]/).pop()} · {session.label} · {session.status}</span>
          <button type="button" class="gp-btn py-0!" onclick={() => session.close().catch((error: unknown) => (validationError = formatError(error)))}>Close session</button>
        </div>
      {/each}
      {#if !$terminalSessions.length}<span>No active processes.</span>{/if}
    </div>
  {/if}
  {#if shortcutsOpen}
    <div class="terminal-popover px-3 py-2 flex flex-wrap gap-x-5 gap-y-1 text-[10px] text-textMuted bg-surface border-b border-border/60 gp-section-edge" aria-label="Terminal keyboard shortcuts">
      <span><kbd>Ctrl+Shift+T</kbd> New shell</span>
      <span><kbd>Ctrl+Shift+W</kbd> Close session</span>
      <span><kbd>Ctrl+Tab / Ctrl+Shift+Tab</kbd> Next / previous tab</span>
      <span><kbd>⌘F / Ctrl+Shift+F</kbd> Find</span>
      <span><kbd>Enter / Shift+Enter</kbd> Next / previous match</span>
      <span><kbd>Esc</kbd> Close find</span>
      <span><kbd>⌘ + / − / 0</kbd> Text size (Ctrl+Shift on Windows/Linux)</span>
      <span><kbd>← / → / Home / End</kbd> Navigate focused tabs</span>
      <span>Shell commands run outside the MANVI gate. Console git commands are MANVI-gated.</span>
    </div>
  {/if}

  <!-- Tab strip + sessions. Rendered in BOTH modes and merely hidden in
       Console, because unmounting a session kills the shell — the same
       hide-don't-kill rule TerminalDock applies to the whole dock. -->
    <div
      class="shrink-0 flex items-stretch gap-2 px-2 h-9 border-b border-border/60 gp-section-edge bg-surface/40"
      class:hidden={mode !== "shell"}
    >
      <!-- Only the tabs scroll. The launcher group sat inside the scroller
           behind an `ml-auto`, so past a handful of tabs the way to open one
           more scrolled off the right edge. -->
      <div class="relative flex-1 min-w-0 self-stretch">
      <div
        bind:this={tabScroller}
        class="h-full flex items-stretch gap-1 overflow-x-auto"
        role="tablist"
        aria-label="Terminal sessions"
      >
      {#each tabState.tabs as tab (tab.id)}
        <div
          data-terminal-tab={tab.id}
          class="group flex items-center gap-1 pl-2 pr-1 my-1 rounded-lg border text-[11px] shrink-0 transition-colors {tab.id ===
          tabState.activeId
            ? 'bg-surface border-accent/50 text-textPrimary'
            : 'bg-transparent border-transparent text-textMuted hover:bg-surface/70 hover:text-textPrimary'}"
        >
          <button
            type="button"
            role="tab"
            id={`terminal-tab-${tab.id}`}
            aria-controls={`terminal-pane-${tab.id}`}
            aria-selected={tab.id === tabState.activeId}
            tabindex={tab.id === tabState.activeId ? 0 : -1}
            class="max-w-56 truncate"
            onclick={() => selectTab(tab.id)}
            onkeydown={handleTabKey}
            title={`${launcherLabel(tab.launcher)} — ${tab.title?.trim().slice(0, 256) || tabLabel(tab)}`}
          >
            {#if unread.has(tab.id)}<span aria-label="Unread output" class="text-accent">● </span>{/if}{tabLabel(tab)}{#if tabStatuses[tab.id] === "exited"}<span class="text-textMuted"> · Ended</span>{:else if tabStatuses[tab.id] === "error"}<span class="text-rose-300"> · Error</span>{/if}
          </button>
          <button
            type="button"
            class="p-0.5 rounded opacity-50 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-surfaceHover text-textMuted hover:text-rose-300"
            onclick={() => dropTab(tab.id)}
            aria-label={`Close ${tabLabel(tab)}`}
            title="Close this session (⌃⇧W) — the process is terminated"
          >
            <X size={11} />
          </button>
        </div>
      {/each}
      </div>
      <ScrollCue target={tabScroller} axis="x" />
      </div>

      <div class="flex items-center gap-1 shrink-0 border-l border-border/60 pl-2">
        <button type="button" class="gp-icon-btn" aria-label={splitIds ? "Close split view" : "Split terminal"} aria-pressed={Boolean(splitIds)} disabled={!activeId || (tabState.tabs.length < 2 && !canCreate)} onclick={toggleSplit}><Columns2 size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label="Terminal tab options" aria-expanded={tabOptions} disabled={!activeId} onclick={showTabOptions}><Settings2 size={13} /></button>
        <select bind:value={nextLauncher} onchange={() => interfaceStore.setTerminalLauncher(nextLauncher)} aria-label="New session type" class="max-w-24 bg-surface text-textPrimary text-[11px] rounded px-1 py-0.5 border border-border/60">
          {#each LAUNCHERS as launcher (launcher.kind)}
            <option value={launcher.kind}>{launcher.label}</option>
          {/each}
        </select>
        <button type="button" class="gp-icon-btn" disabled={!repoPath || !canCreate} onclick={() => newTab(nextLauncher)} aria-label={`New ${launcherLabel(nextLauncher)} session`} title={capacityTitle}><Plus size={14} /></button>
      </div>
    </div>

    {#if tabOptions && mode === "shell" && activeId}
      <form class="terminal-popover px-3 py-1.5 flex flex-wrap items-center gap-2 border-b border-border/60 gp-section-edge bg-surface" onsubmit={saveTabName}>
        <input aria-label="Terminal tab name" bind:value={renameValue} maxlength="64" placeholder="Use automatic title" class="min-w-0 w-36 bg-surface rounded px-2 py-1 border border-border text-xs" />
        <button type="submit" class="gp-btn py-1!">Rename</button>
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab left" disabled={tabState.tabs[0]?.id === activeId} onclick={() => activeId && (tabState = moveTab(tabState, activeId, -1))}><ChevronLeft size={13} /></button>
        <button type="button" class="gp-icon-btn" aria-label="Move terminal tab right" disabled={tabState.tabs.at(-1)?.id === activeId} onclick={() => activeId && (tabState = moveTab(tabState, activeId, 1))}><ChevronRight size={13} /></button>
        <select aria-label="Terminal output actions" onchange={outputAction} class="bg-surface border border-border rounded px-1 py-1 text-xs">
          <option value="">Output actions</option><option value="selection">Copy selection</option><option value="copy">Copy retained output</option><option value="export">Export retained output…</option>
        </select>
      </form>
    {/if}
    <div class="terminal-panes flex-1 min-h-0 relative" class:hidden={mode !== "shell"}>
      {#if !repoPath}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Open a repository to start a shell.
        </div>
      {:else if tabState.tabs.length === 0}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-textMuted text-xs">
          <span>No sessions open.</span>
          <button type="button" class="gp-btn py-1! text-[11px]!" onclick={() => newTab("shell")}>
            <SquareTerminal size={12} /> New shell
          </button>
        </div>
      {:else}
        {#each tabState.tabs as tab (tab.id)}
          <!-- Absolute so hidden siblings keep their box: a session laid out
               at zero height would have its xterm reflow to a 1-row grid and
               tell the shell about it. -->
          <div
            class="terminal-pane absolute inset-0"
            data-position={splitIds?.[0] === tab.id ? "left" : splitIds?.[1] === tab.id ? "right" : "full"}
            class:hidden={splitIds ? !splitIds.includes(tab.id) : tab.id !== tabState.activeId}
            onfocusin={() => { if (activeId !== tab.id) selectTab(tab.id); }}
            role="tabpanel"
            id={`terminal-pane-${tab.id}`}
            aria-labelledby={`terminal-tab-${tab.id}`}
            aria-label={tabLabel(tab)}
          >
            <TerminalSession
              taskRunId={tab.taskRunId}
              bind:this={sessions[tab.id]}
              repoPath={repoPath}
              tabId={tab.id}
              launcher={tab.launcher}
              initialPrompt={tab.initialPrompt}
              active={visible && tab.id === tabState.activeId && mode === "shell"}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              onChord={handleChord}
              onStatus={(status) => { tabStatuses = { ...tabStatuses, [tab.id]: status }; terminalLaunchRequests.update(tab.id, status); }}
              onActivity={() => { if (visible && mode === "shell" && splitIds?.includes(tab.id)) return; if (!unread.has(tab.id)) unread = new Set([...unread, tab.id]); }}
            />
          </div>
        {/each}
      {/if}
    </div>

  {#if mode === "console"}
  <div class="px-3 h-7 shrink-0 flex items-center gap-1.5 text-[10px] text-textMuted border-b border-border/60"><Shield size={11} />Direct & bounded · git commands MANVI-gated</div>
  <!-- Output Area -->
  <div
    bind:this={scrollContainer}
    onscroll={() => { if (scrollContainer) consoleFollowing = followsConsoleOutput(scrollContainer.scrollTop, scrollContainer.clientHeight, scrollContainer.scrollHeight); }}
    class="flex-1 overflow-auto p-4 space-y-4 font-mono text-[11px] leading-relaxed"
  >
    {#if discardedExecutions > 0}<div role="status" class="text-textMuted">{discardedExecutions} older results removed · retains up to 100 results / 8 MiB.</div>{/if}
    {#if executions.length === 0}
      <div class="flex flex-col items-center justify-center h-full max-w-lg mx-auto text-center space-y-4 text-textMuted font-sans">
        <div class="p-3 rounded-2xl bg-surface border border-border shadow-xs text-accent">
          <Terminal size={28} />
        </div>
          <div>
            <h3 class="font-semibold text-textPrimary text-sm">Direct Repository Terminal</h3>
            <p class="text-xs text-textMuted mt-1">
              Execute commands directly in your repository — no shell in between, so arguments stay
              literal. Git commands are judged by the MANVI gate before they run; other tools run
              with hard timeouts and capped output.
            </p>
          </div>

        <div class="w-full space-y-1.5 pt-2">
          <div class="text-[10px] uppercase font-bold tracking-wider text-textMuted">Quick Commands</div>
          <div class="flex flex-wrap gap-1.5 justify-center font-mono text-xs">
            {#each QUICK_COMMANDS as qc}
              <button
                type="button"
                onclick={() => void execute(qc)}
                class="px-2.5 py-1 rounded-full bg-surface border border-border/80 hover:border-accent/60 hover:text-textPrimary transition-all text-[11px]"
              >
                {qc}
              </button>
            {/each}
          </div>
        </div>
      </div>
    {:else}
      {#each executions as entry (entry.id)}
        <div class="rounded-xl border border-border/70 bg-surface/80 shadow-xs overflow-hidden font-mono">
          <!-- Command line header -->
          <div class="px-3 py-1.5 bg-surface border-b border-border/50 gp-section-edge flex items-center justify-between gap-2 text-xs">
            <div class="flex items-center gap-2 min-w-0">
              <span class="text-accent font-bold">$</span>
              <span class="font-semibold text-textPrimary truncate">{entry.command}</span>
            </div>
            <div class="flex items-center gap-2 shrink-0 text-[10px]">
              {#if entry.running}
                <span class="flex items-center gap-1 text-accent">
                  <LoaderCircle size={12} class="animate-spin" />
                  Running…
                </span>
              {:else if entry.result}
                <span class="text-textMuted flex items-center gap-1">
                  <Clock size={10} />
                  {formatDuration(entry.result.duration_ms)}
                </span>
                {#if entry.result.policy}
                  {@const p = entry.result.policy}
                  <span
                    class="px-1.5 py-0.5 rounded-full border text-[9px] uppercase font-bold {p.status === 'allowed' ? 'bg-emerald-500/10 text-emerald-300 border-emerald-500/30' : p.status === 'blocked' ? 'bg-rose-500/10 text-rose-300 border-rose-500/30' : 'bg-amber-500/10 text-amber-300 border-amber-500/30'}"
                    title={p.reason || p.detail}
                  >
                    MANVI: {p.status}
                  </span>
                {:else if !entry.result.gated}
                  <span
                    class="px-1.5 py-0.5 rounded-full border border-border bg-surfaceHover text-textMuted text-[9px] uppercase font-bold"
                    title="Non-git commands are not judged by the MANVI gate; they run bounded (timeout, capped output) instead."
                  >
                    not gate-checked
                  </span>
                {/if}
                {#if entry.result.timed_out}
                  <span class="px-1.5 py-0.5 rounded-full bg-amber-500/10 text-amber-300 border border-amber-500/30 font-semibold">
                    timed out
                  </span>
                {:else if entry.result.exit_code === 0}
                  <span class="px-1.5 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30 font-semibold">
                    exit 0
                  </span>
                {:else}
                  <span class="px-1.5 py-0.5 rounded-full bg-rose-500/10 text-rose-300 border border-rose-500/30 font-semibold">
                    exit {entry.result.exit_code ?? "?"}
                  </span>
                {/if}
              {:else if entry.error}
                <span class="px-1.5 py-0.5 rounded-full bg-rose-500/10 text-rose-300 border border-rose-500/30 font-semibold">
                  failed
                </span>
              {/if}
              <button
                type="button"
                onclick={() => void copyOutput(entry)}
                class="p-1 rounded hover:bg-surfaceHover text-textMuted hover:text-textPrimary"
                title="Copy command and output"
              >
                {#if copiedId === entry.id}
                  <Check size={12} class="text-emerald-400" />
                {:else}
                  <Clipboard size={12} />
                {/if}
              </button>
            </div>
          </div>

          <!-- Output stream contents -->
          <div class="p-3 bg-background/50 space-y-2 overflow-x-auto select-text whitespace-pre-wrap leading-relaxed text-[11px]">
            {#if entry.running}
              <div class="text-textMuted italic">Executing command…</div>
            {:else if entry.result}
              {#if entry.result.stdout_tail}
                <div class="text-textPrimary">{entry.result.stdout_tail}</div>
              {/if}
              {#if entry.result.stderr_tail}
                <div class="text-rose-300/90">{entry.result.stderr_tail}</div>
              {/if}
              {#if !entry.result.stdout_tail && !entry.result.stderr_tail}
                <div class="text-textMuted italic">(No output produced)</div>
              {/if}
              {#if entry.result.truncated}
                <!-- The backend says WHY. It used to say "exceeded cap" for
                     every prefix, including a stream we simply never finished
                     reading — a cause the UI is in no position to assert. -->
                <div class="text-amber-400 text-[10px] pt-1">
                  [Output is incomplete: {entry.result.truncation_reason ??
                    "reason unavailable"}]
                </div>
              {/if}
            {:else if entry.error}
              <div class="text-rose-400 flex items-start gap-1.5">
                <AlertCircle size={14} class="shrink-0 mt-0.5" />
                <span>{entry.error}</span>
              </div>
            {/if}
          </div>
        </div>
      {/each}
    {/if}
  </div>

  <!-- Input Bar -->
  <div class="p-3 border-t border-border gp-section-edge bg-surface shrink-0 space-y-2">
    {#if validationError}
      <div class="px-3 py-1.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-300 text-[11px] flex items-center gap-1.5">
        <AlertCircle size={13} class="shrink-0" />
        <span class="flex-1">{validationError}</span>
        <button
          type="button"
          onclick={() => (validationError = null)}
          class="text-xs hover:text-white"
        >
          ✕
        </button>
      </div>
    {/if}

    <div class="flex items-center gap-2">
      <div class="flex-1 flex items-center gap-2 px-3 py-1.5 rounded-xl bg-background border border-border focus-within:border-accent/70 transition-colors font-mono">
        <span class="text-accent font-bold select-none">$</span>
        <input
          bind:this={inputEl}
          bind:value={commandInput}
          onkeydown={handleKeyDown}
          type="text"
          placeholder="Enter command (e.g. git status, npm test, cargo update)..."
          disabled={running}
          class="flex-1 bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-hidden disabled:opacity-50"
        />
      </div>
      <button
        type="button"
        onclick={() => void execute()}
        disabled={running || !commandInput.trim()}
        class="gp-btn-primary px-4! py-2! shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
      >
        {#if running}
          <LoaderCircle size={14} class="animate-spin" />
        {:else}
          <Play size={13} />
          <span>Run</span>
        {/if}
      </button>
    </div>
  </div>
  {/if}
</div>

<style>
  /* Chrome in the column, never an overlay. On macOS `bg-surface` is
     translucent and xterm paints an opaque grid, so a guessed `inset` over
     the panes made the prompt and these bars share pixels. Find already
     takes a row; shortcuts, sessions, and tab options do the same. */
  .terminal-popover { flex-shrink: 0; max-height: 40%; overflow: auto; }
  .terminal-panes { container-type: inline-size; overflow: auto; }
  /* Keep a readable grid after Find, warnings, popovers, and the footer
     take their space. Short docks scroll their panes instead of crushing
     them to zero. */
  .terminal-pane { min-height: 180px; }
  .terminal-pane[data-position="left"] { right: 50%; border-right: 1px solid var(--color-border); }
  .terminal-pane[data-position="right"] { left: 50%; }
  @container (max-width: 620px) {
    .terminal-pane[data-position="left"] { right: 0; bottom: auto; height: max(50%, 180px); border-right: 0; border-bottom: 1px solid var(--color-border); }
    .terminal-pane[data-position="right"] { left: 0; top: max(50%, 180px); bottom: auto; height: max(50%, 180px); }
  }
</style>
