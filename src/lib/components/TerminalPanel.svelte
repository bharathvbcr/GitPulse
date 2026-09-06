<script lang="ts">
  import { onMount, tick } from "svelte";
  import { repoStore } from "../stores/repoStore";
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
  } from "lucide-svelte";
  import { tokenizeCommand } from "../terminal/tokenize";
  import type { TerminalRunResult } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import TerminalSession from "./TerminalSession.svelte";
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
    tabLabel,
    terminalTabChord,
    type LauncherKind,
    type TabState,
  } from "../terminal/tabs";

  /** The shared wire shape; aliased for this panel's existing call sites. */
  type TerminalRunResponse = TerminalRunResult;

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
  let validationError = $state<string | null>(null);
  let copiedId = $state<string | null>(null);

  let inputEl = $state<HTMLInputElement | null>(null);
  let scrollContainer = $state<HTMLDivElement | null>(null);
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
  // what each is called. The repository boundary is App's `{#key currentPath}`
  // — a repo switch remounts this panel, and every session dies with its own
  // component rather than through a lifecycle effect here that had to be
  // memoised against repoStore's ~6s republish.
  // ---------------------------------------------------------------------
  type PtyMode = "shell" | "console";
  let mode = $state<PtyMode>("shell");
  let tabState = $state<TabState>(initialState());
  let sessions = $state<Record<string, TerminalSession | undefined>>({});

  const repoPath = $derived($repoStore.currentPath);

  function newTab(launcher: LauncherKind) {
    if (!canOpenTab(tabState)) return;
    tabState = openTab(tabState, launcher);
  }

  function selectTab(id: string) {
    tabState = activateTab(tabState, id);
  }

  /**
   * Closing drops the component, whose teardown kills the shell. Emptying the
   * strip is allowed and leaves the empty state, which offers a new tab — a
   * terminal that silently respawns what you just closed is worse than one
   * that waits to be asked.
   */
  function dropTab(id: string) {
    tabState = closeTab(tabState, id);
    const { [id]: _gone, ...rest } = sessions;
    sessions = rest;
  }

  function handleChord(event: KeyboardEvent): boolean {
    if (mode !== "shell") return false;
    const chord = terminalTabChord(event);
    if (!chord) return false;
    event.preventDefault();
    if (chord === "new") newTab("shell");
    else if (chord === "close" && tabState.activeId) dropTab(tabState.activeId);
    else if (chord === "next") tabState = cycleTab(tabState, 1);
    else if (chord === "prev") tabState = cycleTab(tabState, -1);
    return true;
  }

  /**
   * A hidden xterm cannot lay out, so a tab that becomes active has to be told
   * to refit — its ResizeObserver only fires after the browser recomputes
   * layout, and the grid it would paint until then is the stale one.
   */
  $effect(() => {
    const id = tabState.activeId;
    if (mode !== "shell" || !id) return;
    sessions[id]?.reveal();
  });

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

  async function execute(rawCommand?: string) {
    const textToRun = (rawCommand ?? commandInput).trim();
    if (!textToRun || running) return;

    const repoPath = $repoStore.currentPath;
    if (!repoPath) {
      validationError = "No repository open.";
      return;
    }

    const tokenized = tokenizeCommand(textToRun);
    if (!tokenized.ok) {
      validationError = tokenized.error;
      return;
    }

    validationError = null;

    // Update command history
    if (history.length === 0 || history[history.length - 1] !== textToRun) {
      history.push(textToRun);
    }
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
    running = true;
    void scrollToBottom();

    try {
      const response = await invoke<TerminalRunResponse>("cmd_terminal_run", {
        repoPath,
        args: tokenized.argv,
        // Long enough for a cold install/build; the backend clamps to [1s, 30min].
        timeoutSecs: 600,
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
      void scrollToBottom();
      inputEl?.focus();
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

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  <!-- Header Bar -->
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-2 min-w-0">
      <Terminal size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Terminal</span>
      <span class="text-textMuted font-mono truncate max-w-md">
        {$repoStore.currentPath ?? "No repository"}
      </span>
    </div>
    <div class="flex items-center gap-2">
      <div class="gp-segmented" role="group" aria-label="Terminal mode">
        <button
          type="button"
          aria-pressed={mode === "shell"}
          data-active={mode === "shell" ? "true" : "false"}
          class="gp-seg-btn !text-[11px] !py-0.5"
          onclick={() => (mode = "shell")}
          title="A real interactive shell in this repository"
        >
          <SquareTerminal size={11} class="inline mr-1 -mt-0.5" />Shell
        </button>
        <button
          type="button"
          aria-pressed={mode === "console"}
          data-active={mode === "console" ? "true" : "false"}
          class="gp-seg-btn !text-[11px] !py-0.5"
          onclick={() => (mode = "console")}
          title="Run single commands with capped output and per-run policy verdicts"
        >
          <ListChecks size={11} class="inline mr-1 -mt-0.5" />Console
        </button>
      </div>
      {#if mode === "shell"}
        <div class="flex items-center gap-1.5 px-2 py-0.5 rounded-full bg-surface border border-border/60 text-[10px] text-textMuted">
          <AlertCircle size={11} class="text-amber-400 shrink-0" />
          <span>unguarded: a shell runs outside the MANVI gate</span>
        </div>
      {:else}
        <div class="flex items-center gap-1.5 px-2 py-0.5 rounded-full bg-surface border border-border/60 text-[10px] text-textMuted">
          <Shield size={11} class="text-accent" />
          <span>Direct & bounded · git commands MANVI-gated</span>
        </div>
      {/if}
      {#if mode === "console" && executions.length > 0}
        <button
          type="button"
          onclick={clearOutput}
          class="gp-btn !py-1"
          title="Clear terminal output"
        >
          <Trash2 size={12} />
          <span>Clear</span>
        </button>
      {/if}
    </div>
  </div>

  <!-- Tab strip + sessions. Rendered in BOTH modes and merely hidden in
       Console, because unmounting a session kills the shell — the same
       hide-don't-kill rule TerminalDock applies to the whole dock. -->
    <div
      class="shrink-0 flex items-stretch gap-2 px-2 h-8 border-b border-border/60 bg-surface/40"
      class:hidden={mode !== "shell"}
    >
      <!-- Only the tabs scroll. The launcher group sat inside the scroller
           behind an `ml-auto`, so past a handful of tabs the way to open one
           more scrolled off the right edge. -->
      <div
        class="flex-1 min-w-0 flex items-stretch gap-1 overflow-x-auto"
        role="tablist"
        aria-label="Terminal sessions"
      >
      {#each tabState.tabs as tab (tab.id)}
        <div
          class="group flex items-center gap-1 pl-2 pr-1 my-1 rounded-lg border text-[11px] shrink-0 transition-colors {tab.id ===
          tabState.activeId
            ? 'bg-surface border-accent/50 text-textPrimary'
            : 'bg-transparent border-transparent text-textMuted hover:bg-surface/70 hover:text-textPrimary'}"
        >
          <button
            type="button"
            role="tab"
            aria-selected={tab.id === tabState.activeId}
            class="max-w-[14rem] truncate"
            onclick={() => selectTab(tab.id)}
            title={`${launcherLabel(tab.launcher)} — ${tabLabel(tab)}`}
          >
            {tabLabel(tab)}
          </button>
          <button
            type="button"
            class="p-0.5 rounded opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-surfaceHover text-textMuted hover:text-rose-300"
            onclick={() => dropTab(tab.id)}
            aria-label={`Close ${tabLabel(tab)}`}
            title="Close this session (⌃⇧W) — the process is terminated"
          >
            <X size={11} />
          </button>
        </div>
      {/each}
      </div>

      <div class="flex items-center gap-1 shrink-0 border-l border-border/60 pl-2">
        <span class="text-[10px] text-textMuted uppercase tracking-wider">New</span>
        {#each LAUNCHERS as launcher (launcher.kind)}
          <button
            type="button"
            class="px-2 py-0.5 my-1 rounded-full text-[10px] border border-border/60 text-textMuted hover:text-textPrimary hover:border-accent/60 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={!canOpenTab(tabState)}
            onclick={() => newTab(launcher.kind)}
            title={canOpenTab(tabState)
              ? launcher.kind === "shell"
                ? "Open another interactive shell (⌃⇧T)"
                : `Open a new tab running the ${launcher.label} CLI in this worktree`
              : `All ${MAX_TERMINAL_TABS} terminal sessions are open — close one first`}
          >
            {launcher.label}
          </button>
        {/each}
      </div>
    </div>

    <div class="flex-1 min-h-0 relative" class:hidden={mode !== "shell"}>
      {#if !repoPath}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Open a repository to start a shell.
        </div>
      {:else if tabState.tabs.length === 0}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-textMuted text-xs">
          <span>No sessions open.</span>
          <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={() => newTab("shell")}>
            <SquareTerminal size={12} /> New shell
          </button>
        </div>
      {:else}
        {#each tabState.tabs as tab (tab.id)}
          <!-- Absolute so hidden siblings keep their box: a session laid out
               at zero height would have its xterm reflow to a 1-row grid and
               tell the shell about it. -->
          <div
            class="absolute inset-0"
            class:hidden={tab.id !== tabState.activeId}
            role="tabpanel"
            aria-label={tabLabel(tab)}
          >
            <TerminalSession
              bind:this={sessions[tab.id]}
              repoPath={repoPath}
              launcher={tab.launcher}
              active={tab.id === tabState.activeId && mode === "shell"}
              onTitle={(title) => (tabState = setTabTitle(tabState, tab.id, title))}
              onChord={handleChord}
            />
          </div>
        {/each}
      {/if}
    </div>

  {#if mode === "console"}
  <!-- Output Area -->
  <div
    bind:this={scrollContainer}
    class="flex-1 overflow-auto p-4 space-y-4 font-mono text-[11px] leading-relaxed"
  >
    {#if executions.length === 0}
      <div class="flex flex-col items-center justify-center h-full max-w-lg mx-auto text-center space-y-4 text-textMuted font-sans">
        <div class="p-3 rounded-2xl bg-surface border border-border shadow-sm text-accent">
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
        <div class="rounded-xl border border-border/70 bg-surface/80 shadow-sm overflow-hidden font-mono">
          <!-- Command line header -->
          <div class="px-3 py-1.5 bg-surface border-b border-border/50 flex items-center justify-between gap-2 text-xs">
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
                <div class="text-amber-400 text-[10px] pt-1">
                  [Output exceeded cap; tail shown above]
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
  <div class="p-3 border-t border-border bg-surface shrink-0 space-y-2">
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
          class="flex-1 bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none disabled:opacity-50"
        />
      </div>
      <button
        type="button"
        onclick={() => void execute()}
        disabled={running || !commandInput.trim()}
        class="gp-btn-primary !px-4 !py-2 shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
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
