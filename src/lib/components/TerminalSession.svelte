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
   *
   * With tabs this earns its keep a second time: an inactive tab is
   * `display:none`, so its observer fires with a zero rect on every strip
   * change. Those proposals are unusable and must be skipped, not rounded to
   * a 1x1 grid the shell would then be told about.
   */
  export function shouldRefit(
    current: TermDims | null,
    proposed?: TermDims | null,
  ): boolean {
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
      return false;
    }
    if (proposed.cols < 1 || proposed.rows < 1) return false;
    if (!current || !Number.isFinite(current.cols) || !Number.isFinite(current.rows)) {
      return true;
    }
    return (
      Math.round(proposed.cols) !== Math.round(current.cols) ||
      Math.round(proposed.rows) !== Math.round(current.rows)
    );
  }
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { Terminal as XTerm } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { AlertCircle, LoaderCircle, RotateCw } from "lucide-svelte";
  import { harnessStore } from "../stores/harnessStore";
  import { themeStore } from "../stores/themeStore";
  import { formatError } from "../ui/formatError";
  import { ptyBus } from "../terminal/ptyBus.tauri";
  import { launcherLabel, type LauncherKind } from "../terminal/tabs";
  import type { TerminalSpawned } from "../terminal/runResult";

  /**
   * One interactive PTY: a shell or agent CLI, its xterm, and nothing else.
   *
   * Extracted from TerminalPanel when tabs arrived. The panel used to hold a
   * single session inline, which is why "switch launcher" had to mean "kill
   * the session you were using" — there was only one to switch. One session
   * per component instance makes N of them a rendering question rather than a
   * lifecycle rewrite, and makes the teardown rule trivially right: this
   * component dying IS the session ending.
   *
   * An inactive instance stays mounted and hidden. Unmounting would dispose
   * the xterm and kill the shell, which is exactly what a tab switch must not
   * do — the same hide-don't-kill rule TerminalDock applies to the whole dock.
   */
  let {
    repoPath,
    launcher,
    active,
    onTitle,
    onChord,
  }: {
    repoPath: string;
    launcher: LauncherKind;
    active: boolean;
    onTitle: (title: string) => void;
    /** Returns true when the panel consumed the event; xterm then ignores it. */
    onChord: (event: KeyboardEvent) => boolean;
  } = $props();

  let container = $state<HTMLDivElement | null>(null);
  let sessionId = $state<string | null>(null);
  let shellPath = $state("");
  let exited = $state(false);
  let error = $state<string | null>(null);
  let spawning = $state(false);

  /** Non-reactive handles: observers and the emulator must not tear down with runes. */
  let term: XTerm | null = null;
  let fitAddon: FitAddon | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let unsubscribe: (() => void) | null = null;
  /**
   * Set once the component is gone. A spawn IPC in flight at that moment still
   * returns a live backend session, which has to be killed rather than adopted
   * into a dead lifecycle.
   */
  let disposed = false;

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
      if (sessionId && !exited) {
        void invoke("cmd_terminal_write", { sessionId, data }).catch(() => {});
      }
    });
    created.onResize(({ cols, rows }) => {
      if (sessionId && !exited) {
        void invoke("cmd_terminal_resize", { sessionId, rows, cols }).catch(() => {});
      }
    });
    // OSC 0/2: what the running program calls itself. A shell configured to
    // report its directory, or an agent CLI reporting its task, then names its
    // own tab — which is the whole reason a tab strip beats a session counter.
    created.onTitleChange((title) => onTitle(title));
    // xterm forwards nearly every keystroke to the PTY, so a tab chord typed
    // with the terminal focused — which is where it will always be typed —
    // reaches the shell instead of the strip unless it is intercepted here.
    // Returning false is what stops the write; anything the panel declines
    // falls through to the shell untouched.
    created.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return true;
      return !onChord(event);
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

  function base64ToBytes(b64: string): Uint8Array {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }

  async function killPty(id: string | null) {
    if (!id) return;
    try {
      await invoke("cmd_terminal_kill", { sessionId: id });
    } catch {
      /* already gone — the exit event or backend reap handled it */
    }
  }

  function launcherConfig(kind: LauncherKind): { program?: string; args?: string[] } {
    // A bare name, resolved backend-side against the same PATH repair every
    // other GitPulse spawn uses — a GUI-launched app's own PATH does not
    // contain the directories these CLIs install into.
    return kind === "shell" ? {} : { program: kind, args: [] };
  }

  function adopt(spawned: TerminalSpawned) {
    sessionId = spawned.id;
    shellPath = spawned.shell;
    unsubscribe = ptyBus.subscribe(spawned.id, {
      onOutput: (b64) => term?.write(base64ToBytes(b64)),
      onExit: (event) => {
        exited = true;
        sessionId = null;
        const why =
          event.signal || (event.exit_code === null ? "exited" : `exit ${event.exit_code}`);
        term?.writeln(`\r\n\u001b[2m[session closed — ${why}]\u001b[0m`);
      },
    });
    harnessStore.recordAction({
      repoPath,
      kind: "terminal-session",
      label: `${launcher === "shell" ? "Interactive shell" : `Agent (${launcherLabel(launcher)})`} started in ${spawned.cwd} (${spawned.shell}) — not gate-checked`,
      ok: true,
    });
  }

  async function spawnPty() {
    const t = ensureTerm();
    if (!t) return;
    spawning = true;
    error = null;
    exited = false;
    try {
      const dims = fitAddon?.proposeDimensions();
      const cfg = launcherConfig(launcher);
      const spawned = await invoke<TerminalSpawned>("cmd_terminal_spawn", {
        repoPath,
        rows: Math.max(dims?.rows ?? 24, 2),
        cols: Math.max(dims?.cols ?? 80, 2),
        program: cfg.program,
        args: cfg.args,
      });
      if (disposed) {
        void killPty(spawned.id);
        return;
      }
      adopt(spawned);
    } catch (err) {
      if (!disposed) error = formatError(err);
    } finally {
      if (!disposed) {
        spawning = false;
        if (active) term?.focus();
      }
    }
  }

  export function restart() {
    unsubscribe?.();
    unsubscribe = null;
    void killPty(sessionId);
    sessionId = null;
    term?.reset();
    void spawnPty();
  }

  /** Called by the panel when this tab becomes visible again. */
  export function reveal() {
    refitIfResized();
    term?.focus();
  }

  onMount(() => {
    const host = container;
    if (host) {
      const t = ensureTerm();
      t?.open(host);
      resizeObserver = new ResizeObserver(refitIfResized);
      resizeObserver.observe(host);
      refitIfResized();
    }
    void spawnPty();
    return () => {
      disposed = true;
      unsubscribe?.();
      unsubscribe = null;
      resizeObserver?.disconnect();
      resizeObserver = null;
      void killPty(sessionId);
      sessionId = null;
      term?.dispose();
      term = null;
      fitAddon = null;
    };
  });

  /** Theme flips re-resolve the palette from CSS variables. */
  $effect(() => {
    $themeStore;
    if (term) term.options.theme = termTheme();
  });

  /**
   * A hidden container has no size, so xterm cannot lay out while inactive.
   * Becoming active is therefore the moment to refit — the ResizeObserver
   * fires too, but only once the browser has recomputed layout, and the
   * terminal must not paint a stale grid in between.
   */
  $effect(() => {
    if (active) reveal();
  });
</script>

<div class="h-full w-full flex flex-col min-h-0">
  <div class="flex-1 min-h-0 p-3">
    <div
      bind:this={container}
      class="h-full w-full rounded-xl border border-border/70 bg-surface overflow-hidden p-1.5"
      data-terminal-session
    ></div>
  </div>
  {#if error || exited || spawning || shellPath}
    <!-- One fixed-height status row: spawn/error/exited/info content swaps
         inside it, so the terminal's box never resizes (and the
         ResizeObserver never refits) merely because the text rotated. -->
    <div class="shrink-0 border-t border-border/60 bg-surface/60 flex items-center gap-2 px-4 h-8">
      {#if spawning}
        <LoaderCircle size={13} class="animate-spin text-accent shrink-0" />
        <span class="text-textMuted text-[11px]">Starting {launcherLabel(launcher)}…</span>
      {:else if error}
        <AlertCircle size={13} class="text-rose-400 shrink-0" />
        <span class="text-rose-300 flex-1 truncate text-[11px]">{error}</span>
        <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={restart}>
          <RotateCw size={12} /> Retry
        </button>
      {:else if exited}
        <span class="text-textMuted flex-1 text-[11px]">This session ended.</span>
        <button type="button" class="gp-btn !py-1 !text-[11px]" onclick={restart}>
          <RotateCw size={12} /> Restart
        </button>
      {:else}
        <span class="text-[10px] text-textMuted font-mono truncate">{shellPath} · cwd {repoPath}</span>
      {/if}
    </div>
  {/if}
</div>
