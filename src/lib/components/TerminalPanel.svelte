<script module lang="ts">
  interface TermDims {
    cols: number;
    rows: number;
  }

  /**
   * Every real fit relayouts the grid and fires the PTY resize IPC, while
   * ResizeObserver also callbacks on pure style repaints and zero-size
   * (hidden / mid-layout) states. Refit only when the proposed grid differs
   * from the live one; an unusable proposal skips rather than guesses.
   */
  export function shouldRefit(
    current: TermDims | null,
    proposed?: TermDims | null,
  ): boolean {
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
      return false;
    }
    if (!current || !Number.isFinite(current.cols) || !Number.isFinite(current.rows)) {
      return true;
    }
    return (
      Math.round(proposed.cols) !== Math.round(current.cols) ||
      Math.round(proposed.rows) !== Math.round(current.rows)
    );
  }

  export type AttachAction = "open" | "adopt" | "skip";

  /**
   * xterm's open() is once-only: on an already-opened terminal it
   * early-returns without moving the DOM node, so a swapped container would
   * leave the buffer hanging off a detached parent (empty box, dead keys).
   * All of xterm's listeners live inside its own element subtree, so
   * physically re-parenting that element is safe; only the first attach may
   * use open().
   */
  export function planAttach(
    openedParent: Element | null | undefined,
    container: Element,
  ): AttachAction {
    if (!openedParent) return "open";
    return openedParent === container ? "skip" : "adopt";
  }
</script>

<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore } from "../stores/harnessStore";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
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
  import type {
    TerminalRunResult,
    TerminalSpawned,
    TerminalOutputPayload,
    TerminalExitPayload,
  } from "../terminal/runResult";
  import { themeStore } from "../stores/themeStore";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import { createListenerTracker } from "../dom/listenerTracker";

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
  // Tracker, not a bare array: listen() promises can resolve after cleanup
  // ran (fast tab switch remount), and a late unlisten must fire immediately
  // instead of landing in a drained array and leaking for the webview life.
  const unlisteners = createListenerTracker();
  /** Copy-feedback reset timer; cleared on teardown so it cannot fire post-unmount. */
  let copiedResetTimer: ReturnType<typeof setTimeout> | null = null;
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

  /** Creates the single XTerm instance for this panel's lifetime. It does
   * NOT bind a container: attachment is the attach effect's job, so the
   * buffer survives Shell↔Console toggles that swap ptyContainer nodes. */
  function ensureTerm(): XTerm | null {
    if (term) return term;
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
    term = created;
    return term;
  }

  function refitIfResized() {
    if (!fitAddon || !term) return;
    try {
      const proposed = fitAddon.proposeDimensions();
      if (!shouldRefit({ cols: term.cols, rows: term.rows }, proposed)) return;
      fitAddon.fit();
    } catch {
      /* container collapsed; refit when it has size again */
    }
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
    const epoch = ++spawnEpoch;
    ptySpawning = true;
    ptyError = null;
    ptyExited = false;
    ptySessionId = null;
    earlyOutput = [];
    term.reset();
    try {
      const dims = fitAddon?.proposeDimensions();
      const spawned = await invoke<TerminalSpawned>(
        "cmd_terminal_spawn",
        {
          repoPath,
          rows: Math.max(dims?.rows ?? 24, 2),
          cols: Math.max(dims?.cols ?? 80, 2),
        },
      );
      if (epoch !== spawnEpoch) {
        // Superseded while pending (repo/mode change, retry, or unmount):
        // the backend session exists but its owner is gone — kill it here
        // instead of leaking an orphaned shell.
        void killPty(spawned.id);
        return;
      }
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
      // A stale spawn's failure belongs to no live owner; the current one
      // owns the error surface.
      if (epoch === spawnEpoch) ptyError = formatError(err);
    } finally {
      if (epoch === spawnEpoch) {
        ptySpawning = false;
        term?.focus();
      }
    }
  }

  onMount(() => {
    inputEl?.focus();
    void Promise.all([
      listen<TerminalOutputPayload>("terminal-output", (event) => {
        const bytes = base64ToBytes(event.payload.data_b64);
        if (ptySessionId === event.payload.id) {
          writePty(bytes);
        } else if (ptySessionId === null && ptySpawning) {
          earlyOutput.push({ id: event.payload.id, bytes });
          if (earlyOutput.length > 64) earlyOutput.shift();
        }
      }),
      listen<TerminalExitPayload>(
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
      for (const fn of unlistenFns) unlisteners.track(fn);
    });
    return () => {
      unlisteners.dispose();
      if (copiedResetTimer !== null) {
        clearTimeout(copiedResetTimer);
        copiedResetTimer = null;
      }
      resizeObserver?.disconnect();
      resizeObserver = null;
      spawnEpoch += 1; // a spawn landing after unmount must kill itself
      void killPty(ptySessionId);
      ptySessionId = null;
      term?.dispose();
      term = null;
      fitAddon = null;
    };
  });

  /**
   * Keeps the one XTerm attached to whichever container node the Shell
   * layout currently rendered. Declared above the lifecycle effect so that
   * on first mount open() has run (and proposeDimensions() is meaningful)
   * before spawnPty reads it. Session state is read untracked on purpose:
   * a session id arriving must not re-run attachment.
   */
  $effect(() => {
    const container = ptyContainer;
    if (!container) return;
    const t = ensureTerm();
    if (!t) return;
    const action = planAttach(t.element?.parentElement ?? null, container);
    if (action === "open") {
      t.open(container);
    } else if (action === "adopt" && t.element) {
      container.replaceChildren(t.element);
    }
    if (!resizeObserver) {
      resizeObserver = new ResizeObserver(refitIfResized);
    }
    // Reconnect per container: the previous node may stay detached forever,
    // and observing a dead node would silence every future refit.
    resizeObserver.disconnect();
    resizeObserver.observe(container);
    refitIfResized();
    // Focus is safe after re-parenting — xterm's textarea moves with its
    // element — but only worth stealing when a shell is actually live.
    if (action === "adopt" && untrack(() => ptySessionId !== null && !ptyExited)) {
      t.focus();
    }
  });

  /** Theme flips re-resolve the palette from CSS variables; construction
   * already read them once, this keeps a live buffer in sync afterwards.
   * The store may flip the html class inside a view-transition callback
   * after this effect runs; the next emission re-syncs, matching how
   * CommitTable treats its cached theme. */
  $effect(() => {
    $themeStore;
    if (term) term.options.theme = termTheme();
  });

  /** One live session per repository: switching repos (or leaving the shell
   * tab) kills the old session before a new one spawns. The session id is
   * deliberately kept out of $state here — an effect that read it would
   * re-run on its own spawn and tear down what it just created. */
  let liveCleanupTarget: string | null = null;
  /** Bumped whenever a pending spawn's owner goes away, so the spawn kills
   * its backend session instead of adopting it into a dead lifecycle. */
  let spawnEpoch = 0;
  /**
   * Lifecycle inputs the PTY actually depends on ("shell:<path>" or null).
   * repoStore publishes a fresh object on every status poll (~6s) and stats
   * drain, and any $repoStore read re-runs this effect — killing and
   * respawning the user's live shell per emission would be catastrophic, so
   * teardown fires only when mode or repo path genuinely change.
   */
  let ptyLifecycleKey: string | null = null;
  $effect(() => {
    const path = $repoStore.currentPath;
    const key = mode === "shell" && path ? `shell:${path}` : null;
    if (key === ptyLifecycleKey) return;
    ptyLifecycleKey = key;
    // Genuine lifecycle change: orphan any pending spawn, then drop the
    // session (if any) owned by the previous inputs before spawning anew.
    spawnEpoch += 1;
    void killPty(liveCleanupTarget);
    liveCleanupTarget = null;
    if (key && path) void spawnPty(path);
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
    {#if ptyError || ptyExited || ptySpawning || ptyShell}
      <!-- One fixed-height status row: spawn/error/exited/info content swaps
           inside it, so the terminal's box never resizes (and the
           ResizeObserver never refits) merely because the text rotated. -->
      <div class="shrink-0 border-t border-border/60 bg-surface/60 flex items-center gap-2 px-4 h-8">
        {#if ptySpawning}
          <LoaderCircle size={13} class="animate-spin text-accent shrink-0" />
          <span class="text-textMuted text-[11px]">Starting shell…</span>
        {:else if ptyError}
          <AlertCircle size={13} class="text-rose-400 shrink-0" />
          <span class="text-rose-300 flex-1 truncate text-[11px]">{ptyError}</span>
          <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={() => void restartPty()}>
            <RotateCw size={12} /> Retry
          </button>
        {:else if ptyExited}
          <span class="text-textMuted flex-1 text-[11px]">The shell session ended.</span>
          <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={() => void restartPty()}>
            <RotateCw size={12} /> Restart shell
          </button>
        {:else}
          <span class="text-[10px] text-textMuted font-mono truncate">{ptyShell} · cwd {$repoStore.currentPath ?? ""}</span>
        {/if}
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
