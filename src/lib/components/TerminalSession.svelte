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
  import { onMount, tick, untrack } from "svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { platformChord } from "../ui/platformCopy";
  import { invoke } from "../ipc/invoke";
  import { Terminal as XTerm } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { SearchAddon } from "@xterm/addon-search";
  import "@xterm/xterm/css/xterm.css";
  import { AlertCircle, AlertTriangle, LoaderCircle, RotateCw, Search, ChevronUp, ChevronDown, X, Minus, Plus, ArrowDownToLine, ExternalLink } from "@lucide/svelte";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { harnessStore } from "../stores/harnessStore";
  import { themeStore } from "../stores/themeStore";
  import { formatError } from "../ui/formatError";
  import { createSessionLifecycle } from "../terminal/sessionLifecycle";
  import { terminalDeadline } from "../terminal/inputQueue";
  import { parseTerminalContext, type TerminalContext } from "../terminal/sessionContext";
  import { isEditingElsewhere } from "../keyboard/terminalFocus";
  import { planPaste } from "../terminal/pasteGuard";
  import { askConfirm } from "../stores/modalStore";
  import { createOutputCredit, planTerminalOutput, type PaintToken } from "../terminal/outputCredit";
  import { applyHelperTextareaHardening, createEraseGuard, describeTerminalControl, webkit229ChordEvent, type TerminalInputEvent } from "../terminal/eraseGuard";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { copyText } from "../desktop/clipboard";
  import { ptyBus } from "../terminal/ptyBus.tauri";
  import { launcherLabel, type LauncherKind, type ResumeLaunch } from "../terminal/tabs";
  import { agentPromptArgs } from "../terminal/launchRequests";
  import {
    effectiveMode,
    PERMISSION_LABELS,
    requiresAcknowledgement,
    type PermissionMode,
  } from "../terminal/agentDefaults";
  import { agentDefaults, loadAgentDefaults } from "../stores/agentDefaultsStore";
  import { terminalAttendance } from "../stores/sessionAlertsStore";
  import { sessionActivity } from "../terminal/sessionActivity";
  import type { TerminalSpawned } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { observeResize } from "../dom/observeResize";
  import { openExternal } from "../desktop/openExternal";
  import { repoStore } from "../stores/repoStore";
  import { linksForRow, resolveLinkAction, type LinkBuffer } from "../terminal/links";
  import { requestReveal } from "../files/revealRequests";
  import { openTaskForRun } from "../workbench/taskOpen";
  import { toastStore } from "../stores/toastStore";
  import {
    clampTerminalFontSize, hasRenderedBox, macLineEditing, spawnGridSize, terminalViewChord, terminalSearchSummary,
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
    initialPrompt,
    startDir,
    taskRunId,
    resume,
    attachSessionId,
    active,
    onscreen = false,
    onTitle,
    onChord,
    onStatus = () => {},
    onActivity = () => {},
    revealSelf,
    confirmCloseSelf,
    title,
  }: {
    repoPath: string;
    tabId: string;
    launcher: LauncherKind;
    initialPrompt?: string;
    /** Repository-relative directory to start in; absent starts at the root. */
    startDir?: string;
    taskRunId?: string;
    /** Pick an ended attempt's Claude Code conversation back up, in its own mode. */
    resume?: ResumeLaunch;
    /** Take over this still-running session (left by a reloaded page) instead of starting one. */
    attachSessionId?: string;
    active: boolean;
    /**
     * Whether the user can actually see this session right now — the selected
     * tab of a dock that is itself open, or either half of a split.
     *
     * Distinct from `active`: an active tab inside a collapsed dock is on
     * nobody's screen, and suppressing its notifications would be the one
     * failure this whole feature exists to prevent.
     */
    onscreen?: boolean;
    onTitle: (title: string) => void;
    onStatus?: (status: string) => void;
    onActivity?: () => void;
    /** Returns true when the panel consumed the event; xterm then ignores it. */
    onChord: (event: KeyboardEvent) => boolean;
    /**
     * Panel-owned "select my tab and focus me", published on this session's
     * registry record so the cross-repository Sessions list can jump to it.
     * The panel owns it because focusing an xterm behind a hidden tab shows
     * nothing.
     */
    revealSelf?: () => void;
    /** The panel's question before a close; see `TerminalSessionRecord.confirmClose`. */
    confirmCloseSelf?: () => Promise<boolean>;
    /** What this session is about (a task title), for the Sessions list. */
    title?: string;
  } = $props();

  /**
   * The backend's id for this PTY, once it has one.
   *
   * Empty until `started`, and deliberately not defaulted to the tab id: a
   * wrong id would tell the notifier that some other session is on screen.
   */
  let nativeSessionId = "";
  /**
   * The permission mode this tab launches with, or null for the CLI's own
   * default. Read once before the spawn because it becomes argv, and a change
   * can only mean the next session.
   */
  let permissionMode: PermissionMode | null = null;
  /**
   * Set when the reader has agreed to this tab running without permission
   * checks. Per tab and per app run — never stored, because the stored thing
   * is the preference and this is the agreement to act on it once.
   *
   * A restart reuses it: the agreement was about this tab, and a reader who
   * pressed restart is not asking to be asked again about a session they are
   * already watching.
   */
  let acknowledgedBypass = $state(false);
  /** Shown instead of a spawn while a bypass launch waits to be agreed to. */
  let awaitingAcknowledgement = $state(false);
  let container = $state<HTMLDivElement | null>(null);
  let warning = $state<string | null>(null);
  let shellPath = $state("");
  /** Absolute directory the current process started in, as the backend reported it. */
  let startedIn = $state("");
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
  /** What the pointer is currently over, shown in the footer so the target is
   * readable before the click rather than only after it. */
  let hoveredLink = $state<string | null>(null);

  /** Non-reactive handles: observers and the emulator must not tear down with runes. */
  let term: XTerm | null = null;
  let fitAddon: FitAddon | null = null;
  let searchAddon: SearchAddon | null = null;
  let searchKey: string | null = null;
  let linkProvider: { dispose(): void } | null = null;
  let stopResize: (() => void) | null = null;
  let themeObserver: MutationObserver | null = null;
  let lifecycle: ReturnType<typeof createSessionLifecycle> | null = null;
  /**
   * Set once the component is gone. A spawn IPC in flight at that moment still
   * returns a live backend session, which has to be killed rather than adopted
   * into a dead lifecycle.
   */
  let disposed = false;
  /** Outstanding PTY output credit for this view. Released on paint, or on teardown when paint will not happen. */
  const credit = createOutputCredit();

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

  /**
   * xterm's buffer in the shape `links.ts` reads it.
   *
   * The per-character column map is built by walking cells rather than by
   * indexing the translated string, because the two disagree wherever a
   * double-width glyph sits: `translateToString` emits one character where the
   * grid spent two cells, and a link range computed from string offsets then
   * underlines text to the left of the link. Cells with width 0 are the
   * placeholders that follow a wide glyph and carry no character of their own.
   */
  function linkBuffer(t: XTerm): LinkBuffer {
    const scratch = t.buffer.active.getNullCell();
    return {
      row(index: number) {
        // Re-read the active buffer every call: a program entering the
        // alternate screen swaps it underneath a cached reference.
        const line = t.buffer.active.getLine(index - 1);
        if (!line) return null;
        const chars: string[] = [];
        const columns: number[] = [];
        let column = 1;
        const width = Math.min(line.length, t.cols);
        for (let x = 0; x < width; x++) {
          const cell = line.getCell(x, scratch);
          if (!cell) break;
          const cellWidth = cell.getWidth();
          if (cellWidth === 0) continue;
          // An untouched cell reads as the empty string and occupies a column.
          const text = cell.getChars() || " ";
          for (let i = 0; i < text.length; i++) columns.push(column);
          chars.push(text);
          column += cellWidth;
        }
        return { isWrapped: line.isWrapped, text: chars.join(""), columns };
      },
    };
  }

  /**
   * Acts on a clicked link — the detected spans and OSC 8 hyperlinks both
   * arrive here, so one policy covers both.
   *
   * A refusal is shown rather than swallowed: a click that silently does
   * nothing is indistinguishable from a click that missed the link, and the
   * reason is the only thing that tells the user the output asked for
   * something GitPulse will not do.
   */
  async function activateLink(text: string) {
    const action = resolveLinkAction(text, repoPath);
    if (action.kind === "refused") {
      warning = action.reason;
      return;
    }
    if (action.kind === "url") {
      try {
        await openExternal(action.url);
      } catch (err) {
        warning = `Could not open ${action.url}: ${formatError(err)}`;
      }
      return;
    }
    // `selectFilePath` acts on whichever repository tab is active, so a click
    // in a background repository's terminal would open the path in the wrong
    // checkout. Refuse instead of opening someone else's file.
    const state = get(repoStore);
    const activePath = state.openTabs.find((tab) => tab.id === state.activeTabId)?.path ?? "";
    if (activePath !== repoPath) {
      warning = "Switch to this terminal's repository to open its files.";
      return;
    }
    // Recorded before the selection, so the request is already waiting when
    // the viewer mounts and reads the file. A reference with no line simply
    // opens the file, which is why an unusable request is not an error here.
    requestReveal(action.path, action.line, action.column);
    repoStore.selectFilePath(action.path);
    repoStore.setActiveTab("code");
  }

  /** Hover text for a link, so the target is readable before it is clicked. */
  function linkTitle(text: string): string {
    const action = resolveLinkAction(text, repoPath);
    if (action.kind === "url") return `Open ${action.url} in your browser`;
    if (action.kind === "file") {
      const at = action.line === null ? "" : ` (line ${action.line}${action.column === null ? "" : `, column ${action.column}`})`;
      return `Open ${action.path}${at}`;
    }
    return action.reason;
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
      screenReaderMode: get(interfaceStore).terminalScreenReader,
      /**
       * OSC 8 hyperlinks — the ones a program embeds in its own output.
       * `allowNonHttpProtocols` stays false so xterm drops a non-web target
       * before `activate` ever sees it; `activateLink` then applies the same
       * allowlist again. The duplication is deliberate: xterm's filter lives
       * in its OSC provider only, so it protects this path and not the
       * detected-span path, and only one of the two is a GitPulse decision.
       */
      linkHandler: {
        activate: (_event, text) => { void activateLink(text); },
        allowNonHttpProtocols: false,
      },
    });
    fitAddon = new FitAddon();
    created.loadAddon(fitAddon);
    searchAddon = new SearchAddon({ highlightLimit: SEARCH_HIGHLIGHT_LIMIT });
    created.loadAddon(searchAddon);
    searchAddon.onDidChangeResults((result) => {
      resultIndex = result.resultIndex;
      resultCount = result.resultCount;
    });
    /**
     * URLs and repository file references in ordinary output, which carries
     * no OSC 8 markup at all — compilers, test runners and `git status` print
     * plain text. Scanning happens on hover of a row xterm has not already
     * asked about, so the per-call cost lands on a mousemove and is bounded
     * inside `linksForRow`.
     */
    linkProvider = created.registerLinkProvider({
      provideLinks(bufferLineNumber, callback) {
        if (disposed || !term) { callback(undefined); return; }
        try {
          // A span GitPulse will refuse to open is not decorated as a link at
          // all. Underlining it and then doing nothing on click is the worse
          // option: the affordance would promise something the policy has
          // already decided against, and a user who clicks twice learns to
          // distrust the underline rather than the output.
          const found = linksForRow(linkBuffer(created), bufferLineNumber)
            .filter((link) => resolveLinkAction(link.text, repoPath).kind !== "refused");
          callback(found.length ? found.map((link) => ({
            range: link.range,
            text: link.text,
            activate: (_event, text) => { void activateLink(text); },
            hover: () => { hoveredLink = linkTitle(link.text); },
            leave: () => { hoveredLink = null; },
          })) : undefined);
        } catch {
          // A buffer read that races a reset must not break linkification for
          // the rest of the session; this row simply has no links this time.
          callback(undefined);
        }
      },
    });
    created.onScroll(() => {
      scrolledBack = created.buffer.active.viewportY < created.buffer.active.baseY;
    });
    created.onData((data) => {
      const shape = describeTerminalControl(data);
      if (shape) {
        const line = `pty-input ${shape.detail}`;
        if (shape.printable > 0) console.info("[gitpulse-terminal]", line);
        else console.debug("[gitpulse-terminal]", line);
      }
      // The reader answering whatever the agent last asked; xterm's own
      // replies (focus, cursor reports) are told apart inside.
      if (nativeSessionId) sessionActivity.input(nativeSessionId, data);
      lifecycle?.write(data);
    });
    created.onBinary((data) => { lifecycle?.write(data, true); });
    created.onResize(({ cols, rows }) => {
      lifecycle?.resize(rows, cols);
    });
    // OSC 0/2: what the running program calls itself. A shell configured to
    // report its directory, or an agent CLI reporting its task, then names its
    // own tab — which is the whole reason a tab strip beats a session counter.
    created.onTitleChange((title) => { onTitle(title); if (nativeSessionId) sessionActivity.title(nativeSessionId, title); });
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
    if (!disposed) {
      for (const token of credit.flush()) paintOutput(token);
    }
    return term;
  }

  function acknowledgeOutput(sessionId: string, bytes: number) {
    if (bytes <= 0) return;
    void invoke("cmd_terminal_ack", { sessionId, bytes }).catch((err: unknown) => {
      if (lifecycle?.isCurrent(sessionId)) lifecycle.fail(`Output acknowledgement failed: ${formatError(err)}`, true);
    });
  }

  function paintOutput(token: PaintToken) {
    const termNow = term;
    if (!termNow || disposed) {
      acknowledgeOutput(token.sessionId, token.release());
      return;
    }
    try {
      termNow.write(token.bytes, () => {
        acknowledgeOutput(token.sessionId, token.release());
      });
    } catch (err) {
      acknowledgeOutput(token.sessionId, token.release());
      lifecycle?.fail(`Terminal output was not painted: ${formatError(err)}`, true);
    }
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

  function launcherConfig(kind: LauncherKind): { program?: string; args?: string[] } {
    // A bare name, resolved backend-side against the same PATH repair every
    // other GitPulse spawn uses — a GUI-launched app's own PATH does not
    // contain the directories these CLIs install into.
    //
    // Only the prompt. The notification flags and the permission flags are
    // the backend's (`terminal_command::notify_flags` / `policy`), added in
    // front of these at spawn so they can never land behind Claude Code's
    // `-- <text>` and be read as prompt — and so a task attempt, which never
    // passes through here, gets the same ones.
    const args = resume ? ["--resume", resume.sessionId] : agentPromptArgs(kind, initialPrompt) ?? [];
    return kind === "shell" ? {} : { program: kind, args };
  }

  /**
   * The mode this tab will start in, decided here rather than in the backend
   * because the reader has to be told — and, for bypass, asked — before
   * anything spawns.
   *
   * A task run is excluded: its permission mode was chosen in the handoff
   * form for that run, and a host-wide default must not quietly re-decide it.
   */
  function resolvePermissionMode(): PermissionMode | null {
    if (taskRunId) return null;
    // Nothing starts: the process already runs under the mode it started with.
    if (attachSessionId) return null;
    // A resumed conversation continues the attempt, so it keeps that
    // attempt's mode rather than taking the host-wide default.
    if (resume) return resume.mode;
    const view = agentDefaults();
    return effectiveMode(view.defaults, launcher, view.launchers);
  }

  /** The session this tab is still to take over, until its first start. */
  let attachPending: string | null = untrack(() => attachSessionId ?? null);

  function createLifecycle() {
    return createSessionLifecycle({
      key: tabId, repoPath, label: launcherLabel(launcher), bus: ptyBus, registry: terminalSessions,
      singleAttempt: !!taskRunId,
      reveal: revealSelf,
      confirmClose: confirmCloseSelf,
      title,
      taskRunId,
      continuesRunId: resume?.runId,
      transport: {
        spawn: () => {
          // A tab hidden before its shell starts measures as NaN; NaN becomes
          // `null` on the wire and the backend's u16 refuses the spawn.
          const { rows, cols } = spawnGridSize(fitAddon?.proposeDimensions(), hasRenderedBox(container));
          const cfg = launcherConfig(launcher);
          // Once: a later restart of this tab starts a fresh process the
          // ordinary way, because the session it took over has ended.
          if (attachPending) {
            const sessionId = attachPending;
            attachPending = null;
            return invoke<TerminalSpawned>("cmd_terminal_attach", { sessionId, rows, cols });
          }
          if (taskRunId) return invoke<TerminalSpawned>("cmd_workbench_launch_terminal", {
            input: JSON.stringify({ id: taskRunId, expected_revision: 1, rows, cols }),
          });
          return invoke<TerminalSpawned>("cmd_terminal_spawn", {
            repoPath, rows, cols,
            program: cfg.program, args: cfg.args,
            startDir: startDir ?? null,
            permissionMode,
            // Sent only for the mode that needs it. The backend refuses an
            // acknowledgement attached to any other mode, so a bug that sent
            // this unconditionally would fail on the next ordinary launch
            // rather than wait to matter.
            acknowledged: requiresAcknowledgement(permissionMode) ? acknowledgedBypass : false,
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
          // The backend's own id for this PTY, which is what a notification is
          // keyed by. The tab id is a renderer invention and means nothing to
          // the notifier.
          // A restart is a new process with a new id. The old id is dead, and
          // left behind it pushes the live one out of the bounded report.
          if (nativeSessionId && nativeSessionId !== spawned.id) { terminalAttendance.forget(nativeSessionId); sessionActivity.forget(nativeSessionId); }
          nativeSessionId = spawned.id;
          terminalAttendance.report(nativeSessionId, onscreen);
          startedIn = spawned.cwd;
          // A session taken over after a reload was already recorded as
          // started, by the page that started it.
          if (spawned.id !== attachSessionId) harnessStore.recordAction({
            repoPath,
            kind: "terminal-session",
            label: `${launcherLabel(launcher)} started in ${spawned.cwd} (${spawned.shell}) — not gate-checked`,
            ok: true,
          });
          // Not while the user is typing somewhere else — a rename field, a
          // search box. The shell can wait for focus; their word cannot.
          if (active && !isEditingElsewhere(document.activeElement, container)) reveal();
        },
        output(b64, sessionId, reserved) {
          const decision = planTerminalOutput(credit, b64, sessionId, reserved ?? null, {
            terminal: term !== null && !disposed,
            disposed,
          });
          if (decision.action === "paint") paintOutput(decision.token);
          else if (decision.action === "ack") {
            acknowledgeOutput(sessionId, decision.bytes);
            if (decision.failure) lifecycle?.fail(decision.failure, true);
          } else if (decision.action === "overflow") {
            for (const owed of decision.owed) acknowledgeOutput(owed.sessionId, owed.bytes);
            lifecycle?.fail(decision.failure, true);
          } else if (decision.action === "stop") {
            void lifecycle?.stop(decision.failure);
          }
          if (decision.action !== "stop" && !disposed) {
            sessionActivity.output(sessionId);
            if (!active) onActivity();
          }
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

  /** Agreed to for this tab. The stored preference is untouched. */
  function acknowledgeBypass() {
    acknowledgedBypass = true;
    awaitingAcknowledgement = false;
    void spawnPty();
  }

  /**
   * Declining starts the session at "ask every time" rather than not at all.
   *
   * A reader who opened a terminal wants a terminal; the thing they declined
   * was the authority, not the session. Narrowing is always safe — the
   * backend accepts any mode without an acknowledgement except bypass — and
   * the stored default is left alone, because this is one launch and not a
   * change of mind about every future one.
   */
  function declineBypass() {
    permissionMode = "ask";
    acknowledgedBypass = false;
    awaitingAcknowledgement = false;
    void spawnPty();
  }

  export function restart() { void lifecycle?.restart(); }

  /** Opens the task this attempt belongs to, where a new attempt starts. */
  async function openOwnTask() {
    if (!taskRunId) return;
    try {
      await openTaskForRun(taskRunId);
    } catch (cause) {
      toastStore.error(`This attempt's task could not be opened: ${formatError(cause)}`);
    }
  }

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

  /**
   * What this session is running and where, or null when it has no live
   * process or the answer is malformed. Bounded: a stuck query must not hold
   * up the close or the new tab that asked.
   */
  export async function readContext(): Promise<TerminalContext | null> {
    const sessionId = nativeSessionId;
    if (!sessionId || !lifecycle?.isCurrent(sessionId)) return null;
    try {
      const raw = await terminalDeadline(invoke<unknown>("cmd_terminal_context", { sessionId }), 1500, "Reading terminal context");
      return parseTerminalContext(raw);
    } catch {
      return null;
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
    const chord = terminalViewChord(event, $hostPlatform.os);
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

  const ERASE_CAPTURE_EVENTS = ["keydown", "keyup", "keypress", "beforeinput", "input", "compositionstart", "compositionupdate", "compositionend"] as const;

  function eraseInputFromDom(event: Event): TerminalInputEvent {
    const source = event as KeyboardEvent & InputEvent;
    return {
      type: event.type,
      key: typeof source.key === "string" ? source.key : undefined,
      keyCode: typeof source.keyCode === "number" ? source.keyCode : undefined,
      ctrlKey: source.ctrlKey === true,
      altKey: source.altKey === true,
      metaKey: source.metaKey === true,
      shiftKey: source.shiftKey === true,
      repeat: source.repeat === true,
      isComposing: source.isComposing === true,
      inputType: typeof source.inputType === "string" ? source.inputType : undefined,
      data: typeof source.data === "string" ? source.data : null,
      applicationCursor: term?.modes?.applicationCursorKeysMode === true,
      code: typeof source.code === "string" ? source.code : undefined,
      screenReader: term?.options.screenReaderMode === true,
      timeStamp: Number.isFinite(event.timeStamp) ? event.timeStamp : undefined,
    };
  }

  const hardenedHelpers = new WeakSet<HTMLTextAreaElement>();
  function hardenHelper(textarea: HTMLTextAreaElement) {
    if (hardenedHelpers.has(textarea)) return;
    applyHelperTextareaHardening(textarea);
    hardenedHelpers.add(textarea);
  }

  onMount(() => {
    const host = container;
    const eraseGuard = createEraseGuard();
    /** Pending arm expiries, cleared with the view so none outlives it. */
    const armTimers = new Set<ReturnType<typeof setTimeout>>();
    const expireLater = (gen: number, ms: number) => {
      const timer = setTimeout(() => { armTimers.delete(timer); eraseGuard.expire(gen); }, ms);
      armTimers.add(timer);
    };
    // After xterm's own keydown (a capture listener on the same textarea,
    // registered later, runs after it): what xterm actually did, not a guess.
    const onXtermKeydown = (event: Event) => eraseGuard.observe(event.defaultPrevented);
    // No keyup arrives for a key held while focus leaves.
    const onHelperBlur = () => eraseGuard.reset();
    let observedHelper: HTMLTextAreaElement | null = null;
    /**
     * Every paste goes through `planPaste`: bracketed-paste markers inside the
     * clipboard are removed, and a paste that would run before it is read asks
     * first. Ahead of xterm's own listeners, which never strip the markers.
     */
    const onPasteCapture = (event: Event) => {
      if (!(event instanceof ClipboardEvent) || !host || !(event.target instanceof Node) || !host.contains(event.target)) return;
      const raw = event.clipboardData?.getData("text/plain");
      const current = term;
      if (raw === undefined || !current) return;
      event.preventDefault();
      event.stopPropagation();
      const plan = planPaste(raw, current.modes.bracketedPasteMode);
      if (!plan.question) { current.paste(plan.text); return; }
      void askConfirm({ ...plan.question, confirmLabel: "Paste" }).then((confirmed) => {
        if (disposed || term !== current) return;
        if (confirmed) current.paste(plan.text);
        current.focus();
      });
    };
    const onEraseCapture = (event: Event) => {
      const target = event.target;
      if (!(target instanceof HTMLTextAreaElement) || !target.classList.contains("xterm-helper-textarea")) return;
      if (host && !host.contains(target)) return;
      hardenHelper(target);
      if (event instanceof KeyboardEvent) {
        const asChord = webkit229ChordEvent(event);
        if (asChord && (handleViewChord(asChord) || onChord(asChord))) {
          target.value = "";
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        // Before the erase guard: ⌘⌫ is a Backspace keydown, and the guard
        // would send it as a single DEL.
        const edit = event.type === "keydown"
          ? macLineEditing(event, $hostPlatform.os, term?.buffer.active.type === "alternate")
          : null;
        if (edit !== null) {
          target.value = "";
          event.preventDefault();
          event.stopPropagation();
          eraseGuard.reset();
          if (term) term.input(edit, true);
          else lifecycle?.write(edit);
          return;
        }
      }
      const decision = eraseGuard.decide(eraseInputFromDom(event));
      if (decision.clearTextarea && !(event as KeyboardEvent).isComposing) target.value = "";
      const swallowed = /len=(\d+)/.exec(decision.detail);
      const dump = swallowed !== null && Number(swallowed[1]) > 1;
      if (decision.trace === "suppress" && dump) console.info("[gitpulse-terminal]", decision.detail);
      else if (decision.trace) console.debug("[gitpulse-terminal]", decision.detail);
      if (decision.releaseAfterTail) {
        const gen = decision.generation;
        queueMicrotask(() => expireLater(gen, 0));
      }
      if (decision.armTtlMs !== null) expireLater(decision.generation, decision.armTtlMs);
      // Through xterm, as if typed: it scrolls to the prompt, honours
      // disableStdin, and fires onData, which is the one path to the PTY.
      if (decision.send) {
        if (term) term.input(decision.send, true);
        else lifecycle?.write(decision.send);
      }
      if (decision.cancelDefault) event.preventDefault();
      if (decision.suppress) event.stopPropagation();
    };
    if (host) {
      // Parent capture is registered before open(), so it runs before xterm
      // binds the helper textarea. One Backspace is one erase; the WebKit
      // keypress / insertText that would type the deleted line back never
      // reaches xterm, including the keyCode 229 path that resends the textarea.
      for (const type of ERASE_CAPTURE_EVENTS) host.addEventListener(type, onEraseCapture, true);
      host.addEventListener("paste", onPasteCapture, true);
      const t = ensureTerm();
      t?.open(host);
      host.querySelectorAll("textarea.xterm-helper-textarea").forEach((node) => {
        if (node instanceof HTMLTextAreaElement) hardenHelper(node);
      });
      const helper = t?.textarea ?? null;
      if (helper) {
        observedHelper = helper;
        helper.addEventListener("keydown", onXtermKeydown, true);
        helper.addEventListener("blur", onHelperBlur);
      }
      stopResize = observeResize(host, () => {
        if (!disposed) refitIfResized();
      });
      refitIfResized();
    }
    // themeStore publishes before a View Transition applies its CSS. Observe
    // the actual class/style commit too, including accent and glass changes.
    themeObserver = new MutationObserver(() => {
      if (term) term.options.theme = termTheme();
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style"] });
    lifecycle = createLifecycle();
    // Awaited before the spawn so the launch reflects the saved default mode
    // rather than the one the store starts at.
    void Promise.allSettled([loadAgentDefaults()])
      .then(() => {
        // Read after the load settles, from the store rather than the
        // resolved value, so a failed read falls back to "the CLI's own
        // default" rather than to a stale mode.
        permissionMode = resolvePermissionMode();
      })
      .finally(() => {
        if (disposed) return;
        // A launch that turns permission checks off waits to be agreed to.
        // Nothing spawns until it is; the reader sees why, not a blank tab.
        if (requiresAcknowledgement(permissionMode) && !acknowledgedBypass) {
          awaitingAcknowledgement = true;
          return;
        }
        void spawnPty();
      });
    return () => {
      if (host) {
        for (const type of ERASE_CAPTURE_EVENTS) host.removeEventListener(type, onEraseCapture, true);
        host.removeEventListener("paste", onPasteCapture, true);
      }
      observedHelper?.removeEventListener("keydown", onXtermKeydown, true);
      observedHelper?.removeEventListener("blur", onHelperBlur);
      observedHelper = null;
      for (const timer of armTimers) clearTimeout(timer);
      armTimers.clear();
      disposed = true;
      for (const owed of credit.releaseAll()) acknowledgeOutput(owed.sessionId, owed.bytes);
      if (nativeSessionId) { terminalAttendance.forget(nativeSessionId); sessionActivity.forget(nativeSessionId); }
      lifecycle?.dispose();
      lifecycle = null;
      linkProvider?.dispose();
      linkProvider = null;
      stopResize?.();
      stopResize = null;
      themeObserver?.disconnect();
      themeObserver = null;
      term?.dispose();
      term = null;
      fitAddon = null;
      searchAddon = null;
    };
  });

  /**
   * Toggling screen reader support applies to every live session, not just
   * the next one: someone turning it on has assistive technology running now,
   * and telling them to restart their shells to be read is not an answer.
   */
  $effect(() => {
    const on = $interfaceStore.terminalScreenReader;
    if (term) term.options.screenReaderMode = on;
  });

  /**
   * Text size is one setting, so every open terminal follows it — not only
   * the tab that changed it and the tabs opened afterwards.
   */
  $effect(() => {
    const size = clampTerminalFontSize($interfaceStore.terminalFontSize);
    untrack(() => {
      if (size === fontSize) return;
      fontSize = size;
      if (term) term.options.fontSize = size;
      void tick().then(() => { if (!disposed && active) refitIfResized(); });
    });
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
    // A hover label left over from before the switch names a link the pointer
    // is no longer on, in a tab that may not even be visible.
    else hoveredLink = null;
  });

  $effect(() => {
    query;
    caseSensitive;
    if (!findOpen || !active) return;
    const timer = setTimeout(() => runFind(false, true), 120);
    return () => clearTimeout(timer);
  });

  /**
   * Tells the notifier whether the user can see this session.
   *
   * Reported from here rather than from the panel because only this component
   * knows the backend id, and only after the PTY has started. Before that
   * there is nothing to report and nothing that could notify.
   */
  $effect(() => {
    const visible = onscreen;
    if (!nativeSessionId) return;
    terminalAttendance.report(nativeSessionId, visible);
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
    {#if awaitingAcknowledgement}
      <!-- Covers the grid rather than sitting beside it: nothing has spawned,
           and an empty terminal next to a notice reads as a session that
           started and printed nothing. -->
      <div
        data-terminal-acknowledge
        class="absolute inset-2 bg-surface/95 flex flex-col items-center justify-center gap-2 px-6 text-center"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="gp-bypass-title-{tabId}"
      >
        <AlertTriangle size={18} class="text-amber-500 shrink-0" aria-hidden="true" />
        <span id="gp-bypass-title-{tabId}" class="text-textPrimary text-xs font-medium">
          Start {launcherLabel(launcher)} with no permission checks?
        </span>
        <p class="text-textMuted text-[11px] leading-snug max-w-sm">
          Your saved default for {launcherLabel(launcher)} is
          <span class="text-textPrimary">{PERMISSION_LABELS.bypass.label}</span>. This session will
          run without permission prompts and without a sandbox, in
          <span class="font-mono">{repoPath.split(/[\\/]/).pop()}</span>. GitPulse asks every time
          rather than remembering the answer.
        </p>
        <div class="flex items-center gap-2 mt-1">
          <button
            type="button"
            class="gp-btn py-1! text-[11px]!"
            data-testid="bypass-acknowledge"
            onclick={acknowledgeBypass}
          >
            Start this session
          </button>
          <button
            type="button"
            class="gp-btn py-1! text-[11px]!"
            data-testid="bypass-decline"
            onclick={declineBypass}
          >
            Use {PERMISSION_LABELS.ask.label.toLowerCase()} instead
          </button>
        </div>
      </div>
    {/if}
  </div>
  {#if error || exited || spawning || shellPath || hoveredLink}
    <!-- One fixed-height status row: spawn/error/exited/info content swaps
         inside it, so the terminal's box never resizes (and the
         ResizeObserver never refits) merely because the text rotated. -->
    <div data-terminal-status class="shrink-0 min-w-0 border-t border-border/60 gp-section-edge bg-surface/60 flex items-center gap-2 px-4 h-8">
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
        {#if taskRunId}
          <!-- The task details are where a new attempt starts, so the way
               there is here rather than only named in a tooltip. -->
          <button type="button" class="gp-btn py-1! text-[11px]!" data-testid="terminal-open-task" onclick={() => void openOwnTask()}>
            Open task
          </button>
        {/if}
        <button type="button" class="gp-btn py-1! text-[11px]!" onclick={restart} disabled={!!taskRunId} title={taskRunId ? "Launch a new attempt from the task details." : "Restart this terminal"}>
          <RotateCw size={12} /> Restart
        </button>
      {:else if hoveredLink}
        <!-- Same row, so revealing a link target never resizes the grid. -->
        <ExternalLink size={11} class="text-accent shrink-0" aria-hidden="true" />
        <span class="text-[10px] text-textMuted truncate" title={hoveredLink}>{hoveredLink}</span>
      {:else}
        <span class="w-1.5 h-1.5 rounded-full bg-emerald-400 shrink-0" aria-hidden="true"></span>
        <span class="text-[10px] text-textMuted font-mono truncate" title={`${shellPath} · Started in ${startedIn || repoPath}`}>{shellPath.split(/[\\/]/).pop()} · {repoPath.split(/[\\/]/).pop()}{startDir ? `/${startDir}` : ""}</span>
      {/if}
      <div class="ml-auto flex items-center gap-1 shrink-0" role="group" aria-label="Terminal text size">

        <button type="button" class="gp-icon-btn p-0.5!" aria-label="Decrease terminal text size" title="Smaller text ({platformChord('⌘−', 'Ctrl+Shift+−', $hostPlatform.os)})" disabled={fontSize <= TERMINAL_FONT_MIN} onclick={() => setFontSize(fontSize - 1)}><Minus size={12} /></button>
        <button type="button" class="text-[10px] text-textMuted tabular-nums px-1" aria-label="Reset terminal text size" title="Reset text size ({platformChord('⌘0', 'Ctrl+Shift+0', $hostPlatform.os)})" onclick={() => setFontSize(TERMINAL_FONT_DEFAULT)}>{fontSize}px</button>
        <button type="button" class="gp-icon-btn p-0.5!" aria-label="Increase terminal text size" title="Larger text ({platformChord('⌘+', 'Ctrl+Shift++', $hostPlatform.os)})" disabled={fontSize >= TERMINAL_FONT_MAX} onclick={() => setFontSize(fontSize + 1)}><Plus size={12} /></button>
      </div>
    </div>
  {/if}
</div>
