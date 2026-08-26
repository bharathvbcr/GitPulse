<script lang="ts">
  import { onMount, tick } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore } from "../stores/harnessStore";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  // The class is named Terminal; aliased because lucide exports an icon of
  // the same name below.
  import { Terminal as XTerm } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
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
    RotateCw,
    ListChecks,
  } from "lucide-svelte";
  import { tokenizeCommand } from "../terminal/tokenize";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import type { PolicyVerdict } from "../stores/harnessStore";

  /** Wire shape of `crate::terminal::TerminalRunResult`. */
  interface TerminalRunResponse {
    command: string;
    gated: boolean;
    policy?: PolicyVerdict | null;
    timed_out: boolean;
    exit_code: number | null;
    stdout_tail: string;
    stderr_tail: string;
    truncated: boolean;
    duration_ms: number;
  }

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

  // ---------------------------------------------------------------------
  // Interactive shell (PTY) — a real shell per repository. It runs OUTSIDE
  // the MANVI gate by nature: a shell can execute anything, so claiming
  // gate coverage here would be a check that cannot run reporting what a
  // check that ran reports. The bounded Console tab is the gated surface.
  // ---------------------------------------------------------------------
  type PtyMode = "shell" | "console";
  let mode = $state<PtyMode>("shell");
  let ptyContainer = $state<HTMLDivElement | null>(null);
  let ptySessionId = $state<string | null>(null);
  let ptyShell = $state<string>("");
  let ptyExited = $state(false);
  let ptyError = $state<string | null>(null);
  let ptySpawning = $state(false);

  /** Non-reactive handles: events and observers must not tear down with runes. */
  let term: XTerm | null = null;
  let fitAddon: FitAddon | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let unlisteners: UnlistenFn[] = [];
  /** Output that arrives between spawn request and id assignment. */
  let earlyOutput: { id: string; bytes: Uint8Array }[] = [];

  function termTheme(): Record<string, string> {
    const css = getComputedStyle(document.documentElement);
    const v = (name: string, fallback: string) =>
      css.getPropertyValue(name).trim() || fallback;
    return {
      background: v("--bg-surface", "#141a29"),
      foreground: v("--text-primary", "#e9edf8"),
      cursor: v("--accent-color", "#809eff"),
      cursorAccent: v("--bg-surface", "#141a29"),
      selectionBackground: "rgb(128 158 255 / 0.32)",
    };
  }

  function ensureTerm(): XTerm | null {
    if (term || !ptyContainer) return term;
    const created = new XTerm({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
      fontSize: 12,
      cursorBlink: true,
      convertEol: false,
      theme: termTheme(),
      scrollback: 5000,
    });
    fitAddon = new FitAddon();
    created.loadAddon(fitAddon);
    created.open(ptyContainer);
    try {
      fitAddon.fit();
    } catch {
      /* zero-size container before layout settles; the observer refits */
    }
    created.onData((data) => {
      if (ptySessionId && !ptyExited) {
        void invoke("cmd_terminal_write", { sessionId: ptySessionId, data }).catch(() => {});
      }
    });
    created.onResize(({ cols, rows }) => {
      if (ptySessionId && !ptyExited) {
        void invoke("cmd_terminal_resize", { sessionId: ptySessionId, rows, cols }).catch(() => {});
      }
    });
    resizeObserver = new ResizeObserver(() => {
      if (!fitAddon || !ptyContainer) return;
      try {
        fitAddon.fit();
      } catch {
        /* container collapsed; refit when it has size again */
      }
    });
    resizeObserver.observe(ptyContainer);
    term = created;
    return term;
  }

  function writePty(bytes: Uint8Array) {
    term?.write(bytes);
  }

  function base64ToBytes(b64: string): Uint8Array {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }

  async function killPty(sessionId: string | null) {
    if (!sessionId) return;
    try {
      await invoke("cmd_terminal_kill", { sessionId });
    } catch {
      /* already gone — the exit event or backend reap handled it */
    }
  }

  async function restartPty() {
    const path = $repoStore.currentPath;
    if (path) await spawnPty(path);
  }

  async function spawnPty(repoPath: string) {
    if (!ensureTerm() || !term) return;
    ptySpawning = true;
    ptyError = null;
    ptyExited = false;
    ptySessionId = null;
    earlyOutput = [];
    term.reset();
    try {
      const dims = fitAddon?.proposeDimensions();
      const spawned = await invoke<{ id: string; shell: string; cwd: string }>(
        "cmd_terminal_spawn",
        {
          repoPath,
          rows: Math.max(dims?.rows ?? 24, 2),
          cols: Math.max(dims?.cols ?? 80, 2),
        },
      );
      ptySessionId = spawned.id;
      liveCleanupTarget = spawned.id;
      ptyShell = spawned.shell;
      for (const chunk of earlyOutput) {
        if (chunk.id === spawned.id) writePty(chunk.bytes);
      }
      earlyOutput = [];
      harnessStore.recordAction({
        kind: "terminal-session",
        label: `Interactive shell started in ${spawned.cwd} (${spawned.shell}) — not gate-checked`,
        ok: true,
      });
    } catch (err) {
      ptyError = formatError(err);
    } finally {
      ptySpawning = false;
      term?.focus();
    }
  }

  onMount(() => {
    inputEl?.focus();
    void Promise.all([
      listen<{ id: string; data_b64: string }>("terminal-output", (event) => {
        const bytes = base64ToBytes(event.payload.data_b64);
        if (ptySessionId === event.payload.id) {
          writePty(bytes);
        } else if (ptySessionId === null && ptySpawning) {
          earlyOutput.push({ id: event.payload.id, bytes });
          if (earlyOutput.length > 64) earlyOutput.shift();
        }
      }),
      listen<{ id: string; exit_code: number | null; signal: string }>(
        "terminal-exit",
        (event) => {
          if (ptySessionId !== event.payload.id) return;
          ptyExited = true;
          ptySessionId = null;
          const why =
            event.payload.signal ||
            (event.payload.exit_code === null ? "exited" : `exit ${event.payload.exit_code}`);
          term?.writeln(`\r\n\u001b[2m[shell closed — ${why}]\u001b[0m`);
        },
      ),
    ]).then((unlistenFns) => {
      unlisteners.push(...unlistenFns);
    });
    return () => {
      for (const fn of unlisteners.splice(0)) fn();
      resizeObserver?.disconnect();
      resizeObserver = null;
      void killPty(ptySessionId);
      ptySessionId = null;
      term?.dispose();
      term = null;
      fitAddon = null;
    };
  });

  /** One live session per repository: switching repos (or leaving the shell
   * tab) kills the old session before a new one spawns. The session id is
   * deliberately kept out of $state here — an effect that read it would
   * re-run on its own spawn and tear down what it just created. */
  let liveCleanupTarget: string | null = null;
  $effect(() => {
    const path = $repoStore.currentPath;
    if (mode !== "shell" || !path) return;
    void spawnPty(path);
    return () => {
      void killPty(liveCleanupTarget);
      liveCleanupTarget = null;
    };
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
      setTimeout(() => {
        if (copiedId === entry.id) copiedId = null;
      }, 1500);
    }
  }

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(2)}s`;
  }

  onMount(() => {
    inputEl?.focus();
  });
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
      <div class="gp-segmented" role="tablist" aria-label="Terminal mode">
        <button
          type="button"
          data-active={mode === "shell" ? "true" : "false"}
          class="gp-seg-btn !text-[11px] !py-0.5"
          onclick={() => (mode = "shell")}
          title="A real interactive shell in this repository"
        >
          <SquareTerminal size={11} class="inline mr-1 -mt-0.5" />Shell
        </button>
        <button
          type="button"
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

  {#if mode === "shell"}
    <!-- Interactive shell: a real PTY streamed over events. -->
    <div class="flex-1 min-h-0 p-3">
      <div
        bind:this={ptyContainer}
        class="h-full w-full rounded-xl border border-border/70 bg-surface overflow-hidden p-1.5"
      ></div>
    </div>
    {#if ptyError || ptyExited || ptySpawning}
      <div class="px-4 py-2 border-t border-border/60 bg-surface/60 flex items-center gap-2 shrink-0 text-[11px]">
        {#if ptySpawning}
          <LoaderCircle size={13} class="animate-spin text-accent" />
          <span class="text-textMuted">Starting shell…</span>
        {:else if ptyError}
          <AlertCircle size={13} class="text-rose-400" />
          <span class="text-rose-300 flex-1">{ptyError}</span>
          <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={() => void restartPty()}>
            <RotateCw size={12} /> Retry
          </button>
        {:else if ptyExited}
          <span class="text-textMuted flex-1">The shell session ended.</span>
          <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={() => void restartPty()}>
            <RotateCw size={12} /> Restart shell
          </button>
        {/if}
      </div>
    {:else if ptyShell}
      <div class="px-4 py-1.5 border-t border-border/60 bg-surface/60 text-[10px] text-textMuted font-mono truncate shrink-0">
        {ptyShell} · cwd {$repoStore.currentPath ?? ""}
      </div>
    {/if}
  {:else}
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
