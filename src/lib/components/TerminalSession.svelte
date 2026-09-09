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
  /**
   * A colour xterm will accept for a translucent surface.
   *
   * `css.toColor` in @xterm/xterm 6.0.0 parses `#rgb`, `#rgba`, `#rrggbb` and
   * `#rrggbbaa` itself and sends everything else through a canvas probe that
   * THROWS `css.toColor: Unsupported css format` when the sampled alpha is not
   * 255. `getComputedStyle` normalises a translucent token to
   * `rgba(20, 26, 41, 0.5)`, which takes exactly that path — so the alpha has to
   * be re-spelled as hex here, at the boundary the constraint belongs to, rather
   * than left for the terminal to parse and reject.
   *
   * Anything already hex, or any form this cannot read, is passed through
   * untouched: xterm's own parser is a better judge of it than a guess is.
   */
  export function hexColor(value: string): string {
    // `transparent` is a keyword, not a function, and it takes the canvas path
    // too -- where it samples alpha 0 and throws just as `rgba(…, 0.5)` does.
    if (value === "transparent") return "#00000000";
    const channels = value.match(/^rgba?\(([^)]+)\)$/i);
    if (!channels) return value;
    const parts = channels[1].split(/[\s,/]+/).filter(Boolean).map(Number);
    if (parts.length < 3 || parts.slice(0, 3).some((n) => !Number.isFinite(n))) return value;
    const alpha = parts.length > 3 && Number.isFinite(parts[3]) ? parts[3] : 1;
    const byte = (n: number) =>
      Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
    const opacity = alpha >= 1 ? "" : byte(alpha * 255);
    return `#${parts.slice(0, 3).map(byte).join("")}${opacity}`;
  }
</script>

<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { Terminal as XTerm } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { SearchAddon } from "@xterm/addon-search";
  import "@xterm/xterm/css/xterm.css";
  import { AlertCircle, LoaderCircle, RotateCw, Search, ChevronUp, ChevronDown, X, Minus, Plus, ArrowDownToLine } from "@lucide/svelte";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { harnessStore } from "../stores/harnessStore";
  import { themeStore } from "../stores/themeStore";
  import { formatError } from "../ui/formatError";
  import { createSessionLifecycle } from "../terminal/sessionLifecycle";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { copyText } from "../desktop/clipboard";
  import { ptyBus } from "../terminal/ptyBus.tauri";
  import { launcherLabel, type LauncherKind } from "../terminal/tabs";
  import type { TerminalSpawned } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import {
    clampTerminalFontSize, terminalViewChord, terminalSearchSummary,
    TERMINAL_FONT_DEFAULT, TERMINAL_FONT_MIN, TERMINAL_FONT_MAX,
    SEARCH_HIGHLIGHT_LIMIT, SEARCH_QUERY_LIMIT,
  } from "../terminal/viewControls";

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
    tabId,
    launcher,
    taskRunId,
    active,
    onTitle,
    onChord,
    onStatus = () => {},
    onActivity = () => {},
  }: {
    repoPath: string;
    tabId: string;
    launcher: LauncherKind;
    taskRunId?: string;
    active: boolean;
    onTitle: (title: string) => void;
    onStatus?: (status: string) => void;
    onActivity?: () => void;
    /** Returns true when the panel consumed the event; xterm then ignores it. */
    onChord: (event: KeyboardEvent) => boolean;
  } = $props();

  let container = $state<HTMLDivElement | null>(null);
  let warning = $state<string | null>(null);
  let shellPath = $state("");
  let exited = $state(false);
  let error = $state<string | null>(null);
  let spawning = $state(false);
  let findOpen = $state(false);
  let findInput = $state<HTMLInputElement | null>(null);
  let query = $state("");
  let caseSensitive = $state(false);
  let resultIndex = $state(-1);
  let resultCount = $state(0);
  let fontSize = $state(get(interfaceStore).terminalFontSize);
  let scrolledBack = $state(false);

  /** Non-reactive handles: observers and the emulator must not tear down with runes. */
  let term: XTerm | null = null;
  let fitAddon: FitAddon | null = null;
  let searchAddon: SearchAddon | null = null;
  let searchKey: string | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let resizeFrame: number | null = null;
  let themeObserver: MutationObserver | null = null;
  let lifecycle: ReturnType<typeof createSessionLifecycle> | null = null;
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
      background: hexColor(v("--bg-terminal", "#141a29")),
      foreground: v("--text-primary", "#e9edf8"),
      cursor: v("--accent-color", "#809eff"),
      // The glyph drawn INSIDE a block cursor, so it is a foreground and stays
      // opaque even when the surface behind it does not.
      cursorAccent: v("--bg-surface", "#141a29"),
      selectionBackground: "rgb(128 158 255 / 0.32)",
    };
  }

  function ensureTerm(): XTerm | null {
    if (term) return term;
    const created = new XTerm({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
      fontSize,
      lineHeight: 1.15,
      // The official search addon uses xterm's proposed decoration API.
      allowProposedApi: true,
      cursorBlink: true,
      convertEol: false,
      theme: termTheme(),
      // Required before `open()` for a non-opaque background, and not
      // changeable afterwards. Its documented cost is the texture-atlas
      // renderers; this terminal loads fit/search addons, so the DOM renderer
      // draws the background as a plain CSS colour.
      allowTransparency: true,
      scrollback: 5000,
    });
    fitAddon = new FitAddon();
    created.loadAddon(fitAddon);
    searchAddon = new SearchAddon({ highlightLimit: SEARCH_HIGHLIGHT_LIMIT });
    created.loadAddon(searchAddon);
    searchAddon.onDidChangeResults((result) => {
      resultIndex = result.resultIndex;
      resultCount = result.resultCount;
    });
    created.onScroll(() => {
      scrolledBack = created.buffer.active.viewportY < created.buffer.active.baseY;
    });
    created.onData((data) => {
      lifecycle?.write(data);
    });
    created.onBinary((data) => { lifecycle?.write(data, true); });
    created.onResize(({ cols, rows }) => {
      lifecycle?.resize(rows, cols);
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
      if (handleViewChord(event)) return false;
      return !onChord(event);
    });
    term = created;
    return term;
  }

  function refitIfResized() {
    if (!fitAddon || !term) return;
    try {
      const proposal = fitAddon.proposeDimensions();
      const proposed = proposal ? { cols: Math.min(1000, proposal.cols), rows: Math.min(1000, proposal.rows) } : proposal;
      if (!shouldRefit({ cols: term.cols, rows: term.rows }, proposed)) return;
      if (proposal && (proposal.cols > 1000 || proposal.rows > 1000) && proposed) {
        term.resize(proposed.cols, proposed.rows);
      } else fitAddon.fit();
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

  function launcherConfig(kind: LauncherKind): { program?: string; args?: string[] } {
    // A bare name, resolved backend-side against the same PATH repair every
    // other GitPulse spawn uses — a GUI-launched app's own PATH does not
    // contain the directories these CLIs install into.
    return kind === "shell" ? {} : { program: kind, args: [] };
  }

  function createLifecycle() {
    return createSessionLifecycle({
      key: tabId, repoPath, label: launcherLabel(launcher), bus: ptyBus, registry: terminalSessions,
      singleAttempt: !!taskRunId,
      transport: {
        spawn: () => {
          const dims = fitAddon?.proposeDimensions();
          const cfg = launcherConfig(launcher);
          if (taskRunId) return invoke<TerminalSpawned>("cmd_workbench_launch_terminal", {
            input: JSON.stringify({ id: taskRunId, expected_revision: 1, rows: Math.max(dims?.rows ?? 24, 2), cols: Math.max(dims?.cols ?? 80, 2) }),
          });
          return invoke<TerminalSpawned>("cmd_terminal_spawn", {
            repoPath, rows: Math.max(dims?.rows ?? 24, 2), cols: Math.max(dims?.cols ?? 80, 2),
            program: cfg.program, args: cfg.args,
          });
        },
        write: (sessionId, data, binary) => invoke("cmd_terminal_write", { sessionId, data, binary }),
        resize: (sessionId, rows, cols) => invoke("cmd_terminal_resize", { sessionId, rows, cols }),
        kill: (sessionId) => invoke("cmd_terminal_kill", { sessionId }),
      },
      hooks: {
        state(status, message) {
          spawning = status === "starting";
          exited = status === "exited";
          error = status === "error" ? message ?? "Terminal failed" : null;
          onStatus(status);
        },
        started(spawned) {
          shellPath = spawned.shell;
          harnessStore.recordAction({
            repoPath,
            kind: "terminal-session",
            label: `${launcherLabel(launcher)} started in ${spawned.cwd} (${spawned.shell}) — not gate-checked`,
            ok: true,
          });
          if (active) reveal();
        },
        output(b64, sessionId) {
          try {
            const bytes = base64ToBytes(b64);
            term?.write(bytes, () => {
              if (disposed) return;
              void invoke("cmd_terminal_ack", { sessionId, bytes: bytes.length }).catch((err: unknown) => { if (lifecycle?.isCurrent(sessionId)) lifecycle.fail(`Output acknowledgement failed: ${formatError(err)}`); });
            });
            if (!active) onActivity();
          } catch (err) { lifecycle?.fail(`Invalid terminal output: ${formatError(err)}`); }
        },
        exit(event) {
          const why = event.error || event.signal || (event.exit_code === null ? "exited" : `exit ${event.exit_code}`);
          term?.writeln(`\r\n\u001b[2m[session closed — ${why}]\u001b[0m`);
        },
        reset() {
          return new Promise<void>((resolve) => {
            if (!term || disposed) { resolve(); return; }
            term.write("", () => {
              if (!disposed) { term?.reset(); warning = null; searchKey = null; }
              resolve();
            });
          });
        },
        warning(message) { warning = message; },
      },
    });
  }

  async function spawnPty() { await lifecycle?.start(); }

  export function restart() { void lifecycle?.restart(); }

  export async function copySelection() {
    const text = term?.getSelection() ?? "";
    if (!text) warning = "Select terminal text to copy.";
    else warning = await copyText(text) ? null : "Could not copy terminal selection.";
  }

  export function retainedOutput(): string {
    const buffer = term?.buffer.active;
    if (!buffer) return "";
    const lines: string[] = [];
    for (let i = 0; i < buffer.length; i++) {
      const line = buffer.getLine(i);
      const next = buffer.getLine(i + 1);
      lines.push((line?.translateToString(!next?.isWrapped) ?? "") + (next?.isWrapped ? "" : "\n"));
    }
    return lines.join("").trimEnd();
  }

  export async function copyOutput() {
    warning = await copyText(retainedOutput()) ? null : "Could not copy retained terminal output.";
  }

  export async function exportOutput() {
    try { await invoke<boolean>("cmd_terminal_export", { data: retainedOutput() }); }
    catch (err) { if (!disposed) warning = formatError(err); }
  }

  /** Called by the panel when this tab becomes visible again. */
  export function reveal() {
    refitIfResized();
    if (findOpen) findInput?.focus();
    else term?.focus();
  }

  export async function openFind() {
    const selected = term?.getSelection();
    if (selected && !selected.includes("\n")) query = selected.slice(0, SEARCH_QUERY_LIMIT);
    findOpen = true;
    await tick();
    if (!disposed && active) {
      findInput?.focus();
      findInput?.select();
    }
  }

  function closeFind() {
    findOpen = false;
    searchAddon?.clearDecorations();
    if (active) term?.focus();
  }

  function runFind(backwards = false, incremental = false) {
    if (!searchAddon) return;
    const nextKey = JSON.stringify([query, caseSensitive]);
    // addon-search 0.16 assigns its options before comparing them, so a case
    // toggle alone leaves old highlights/counts cached. Invalidate at this seam.
    if (searchKey !== nextKey) searchAddon.clearDecorations();
    searchKey = nextKey;
    if (!query) {
      searchAddon.clearDecorations();
      resultCount = 0;
      resultIndex = -1;
      return;
    }
    const options = {
      caseSensitive, incremental,
      decorations: {
        matchBorder: "#b79538", matchOverviewRuler: "#b79538",
        activeMatchBorder: "#809eff", activeMatchColorOverviewRuler: "#809eff",
      },
    };
    if (backwards) searchAddon.findPrevious(query, options);
    else searchAddon.findNext(query, options);
  }

  function handleFindKey(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeFind();
    } else if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      runFind(event.shiftKey);
    }
  }

  function setFontSize(size: number) {
    fontSize = clampTerminalFontSize(size);
    interfaceStore.setTerminalFontSize(fontSize);
    if (term) term.options.fontSize = fontSize;
    // Font metrics settle at layout; ResizeObserver also handles the changed box.
    void tick().then(() => { if (!disposed && active) refitIfResized(); });
  }

  export function handleViewChord(event: KeyboardEvent): boolean {
    if (!active || event.defaultPrevented) return false;
    const chord = terminalViewChord(event);
    if (!chord) return false;
    event.preventDefault();
    event.stopPropagation();
    if (chord === "find") void openFind();
    else if (chord === "zoom-reset") setFontSize(TERMINAL_FONT_DEFAULT);
    else setFontSize(fontSize + (chord === "zoom-in" ? 1 : -1));
    return true;
  }

  export function clearScrollback() {
    searchAddon?.clearDecorations();
    term?.clear();
    if (findOpen) runFind(false, true);
    else if (active) term?.focus();
  }

  function scrollToLatest() {
    term?.scrollToBottom();
    term?.focus();
  }

  onMount(() => {
    const host = container;
    if (host) {
      const t = ensureTerm();
      t?.open(host);
      resizeObserver = new ResizeObserver(() => {
        if (resizeFrame !== null) return;
        resizeFrame = requestAnimationFrame(() => { resizeFrame = null; if (!disposed) refitIfResized(); });
      });
      resizeObserver.observe(host);
      refitIfResized();
    }
    // themeStore publishes before a View Transition applies its CSS. Observe
    // the actual class/style commit too, including accent and glass changes.
    themeObserver = new MutationObserver(() => {
      if (term) term.options.theme = termTheme();
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style"] });
    lifecycle = createLifecycle();
    void spawnPty();
    return () => {
      disposed = true;
      lifecycle?.dispose();
      lifecycle = null;
      if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
      resizeFrame = null;
      resizeObserver?.disconnect();
      resizeObserver = null;
      themeObserver?.disconnect();
      themeObserver = null;
      term?.dispose();
      term = null;
      fitAddon = null;
      searchAddon = null;
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

  $effect(() => {
    query;
    caseSensitive;
    if (!findOpen || !active) return;
    const timer = setTimeout(() => runFind(false, true), 120);
    return () => clearTimeout(timer);
  });
</script>

<div class="h-full w-full flex flex-col min-h-0 min-w-0">
  {#if warning}
    <div role="status" class="shrink-0 flex gap-2 items-center text-[11px] text-amber-300 px-3 py-1 border-b border-border/60">
      <span class="flex-1 min-w-0 truncate" title={warning}>{warning}</span>
      <button type="button" aria-label="Dismiss terminal message" class="gp-icon-btn" onclick={() => (warning = null)}><X size={12} /></button>
    </div>
  {/if}
  {#if findOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: Enter/Escape bubble from the input and search buttons, retaining their ordinary focus order. -->
    <div class="flex items-center gap-1.5 px-3 h-9 shrink-0 bg-surface border-b border-border/60" role="search" aria-label="Find in terminal" onkeydown={handleFindKey}>
      <Search size={13} class="text-textMuted shrink-0" />
      <input bind:this={findInput} bind:value={query} maxlength={SEARCH_QUERY_LIMIT} aria-label="Find in terminal output" placeholder="Find in terminal…" class="min-w-0 w-48 flex-1 bg-transparent text-textPrimary text-xs outline-none" />
      <span role="status" class="text-[10px] text-textMuted whitespace-nowrap tabular-nums">{query ? terminalSearchSummary(resultIndex, resultCount) : ""}</span>
      <button type="button" class="gp-icon-btn text-[11px]!" class:text-accent={caseSensitive} aria-label="Match case" aria-pressed={caseSensitive} title="Match case" onclick={() => (caseSensitive = !caseSensitive)}>Aa</button>
      <button type="button" class="gp-icon-btn" aria-label="Previous match" title="Previous match (Shift+Enter)" disabled={!query} onclick={() => runFind(true)}><ChevronUp size={13} /></button>
      <button type="button" class="gp-icon-btn" aria-label="Next match" title="Next match (Enter)" disabled={!query} onclick={() => runFind()}><ChevronDown size={13} /></button>
      <button type="button" class="gp-icon-btn" aria-label="Close find" title="Close find (Esc)" onclick={closeFind}><X size={13} /></button>
    </div>
  {/if}
  <div class="flex-1 min-h-0 relative p-2">
    <div
      bind:this={container}
      class="h-full w-full bg-surface overflow-hidden"
      data-terminal-session
    ></div>
    {#if scrolledBack}
      <button type="button" class="gp-btn absolute bottom-3 right-5 text-[11px]! shadow-lg" onclick={scrollToLatest}><ArrowDownToLine size={12} /> Latest output</button>
    {/if}
  </div>
  {#if error || exited || spawning || shellPath}
    <!-- One fixed-height status row: spawn/error/exited/info content swaps
         inside it, so the terminal's box never resizes (and the
         ResizeObserver never refits) merely because the text rotated. -->
    <div class="shrink-0 min-w-0 border-t border-border/60 gp-section-edge bg-surface/60 flex items-center gap-2 px-4 h-8">
      {#if spawning}
        <LoaderCircle size={13} class="animate-spin text-accent shrink-0" />
        <span class="text-textMuted text-[11px]">Starting {launcherLabel(launcher)}…</span>
      {:else if error}
        <AlertCircle size={13} class="text-rose-400 shrink-0" />
        <span class="text-rose-300 flex-1 min-w-0 truncate text-[11px]" title={error}>{error}</span>
        <button type="button" class="gp-btn py-1! text-[11px]!" onclick={restart}>
          <RotateCw size={12} /> {taskRunId ? "Reconnect attempt" : "Retry"}
        </button>
      {:else if exited}
        <span class="text-textMuted flex-1 text-[11px]">This session ended.</span>
        <button type="button" class="gp-btn py-1! text-[11px]!" onclick={restart} disabled={!!taskRunId} title={taskRunId ? "Launch a new attempt from the task details." : "Restart this terminal"}>
          <RotateCw size={12} /> Restart
        </button>
      {:else}
        <span class="w-1.5 h-1.5 rounded-full bg-emerald-400 shrink-0" aria-hidden="true"></span>
        <span class="text-[10px] text-textMuted font-mono truncate" title={`${shellPath} · Started in ${repoPath}`}>{shellPath.split(/[\\/]/).pop()} · {repoPath.split(/[\\/]/).pop()}</span>
      {/if}
      <div class="ml-auto flex items-center gap-1 shrink-0" role="group" aria-label="Terminal text size">

        <button type="button" class="gp-icon-btn p-0.5!" aria-label="Decrease terminal text size" title="Smaller text (⌘− / Ctrl+Shift+−)" disabled={fontSize <= TERMINAL_FONT_MIN} onclick={() => setFontSize(fontSize - 1)}><Minus size={12} /></button>
        <button type="button" class="text-[10px] text-textMuted tabular-nums px-1" aria-label="Reset terminal text size" title="Reset text size (⌘0 / Ctrl+Shift+0)" onclick={() => setFontSize(TERMINAL_FONT_DEFAULT)}>{fontSize}px</button>
        <button type="button" class="gp-icon-btn p-0.5!" aria-label="Increase terminal text size" title="Larger text (⌘+ / Ctrl+Shift++)" disabled={fontSize >= TERMINAL_FONT_MAX} onclick={() => setFontSize(fontSize + 1)}><Plus size={12} /></button>
      </div>
    </div>
  {/if}
</div>
