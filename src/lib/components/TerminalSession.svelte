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
   *
   * `rendered` is whether the terminal's element has a box at all, and it
   * outranks the proposal. Inside `display: none` `getComputedStyle` answers
   * with the declared `100%`, which the fit addon's `parseInt` reads as a
   * 100 px box: a confident, usable-looking 11 x 6 (measured in Chrome and
   * WebKit). A proposal made without a box is therefore not evidence about
   * the grid, whatever it says.
   */
  export function shouldRefit(
    current: TermDims | null,
    proposed?: TermDims | null,
    rendered = true,
  ): boolean {
    if (!rendered) return false;
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

  /**
   * The palette xterm is given, from a reader of CSS tokens.
   *
   * Every value that can be translucent is spelled in a form xterm parses on
   * its own (`hexColor`). The selection colour used to be written as
   * `rgb(128 158 255 / 0.32)`, which is neither hex nor the comma form xterm's
   * parser accepts: it went to the canvas probe, which throws for alpha below
   * 255, the parser swallowed the throw, and every terminal selected in
   * xterm's default translucent white instead. The other colours are opaque
   * by construction (`--c-*` channel triplets), which the probe reads.
   */
  export function buildTermTheme(v: (name: string, fallback: string) => string): Record<string, string> {
    return {
      background: hexColor(v("--bg-terminal", "#141a29")),
      foreground: v("--text-primary", "#e9edf8"),
      cursor: v("--accent-color", "#809eff"),
      // The glyph drawn INSIDE a block cursor, so it is a foreground and stays
      // opaque even when the surface behind it does not.
      cursorAccent: v("--bg-surface", "#141a29"),
      selectionBackground: hexColor("rgb(128 158 255 / 0.32)"),
    };
  }

  /**
   * Whether two palettes are the same colours. Setting a theme on xterm
   * rebuilds its colour set and refreshes the glyph atlas whether or not
   * anything changed, so an unchanged palette is not set again.
   */
  export function sameTheme(a: Record<string, string> | null, b: Record<string, string> | null): boolean {
    if (!a || !b) return false;
    const keys = Object.keys(a);
    return keys.length === Object.keys(b).length && keys.every((key) => a[key] === b[key]);
  }

  /**
   * Whether a hex colour produced by `hexColor` carries sub-opaque alpha, so
   * xterm only enables `allowTransparency` when the surface actually uses it
   * (preserving the opaque WebGL fast-path on solid dark/light themes).
   */
  export function isTranslucentHexColor(value: string | undefined): boolean {
    if (!value) return false;
    if (/^#[0-9a-f]{4}$/i.test(value)) return value[4].toLowerCase() !== "f";
    if (/^#[0-9a-f]{8}$/i.test(value)) return value.slice(7, 9).toLowerCase() !== "ff";
    return false;
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
  import { SerializeAddon } from "@xterm/addon-serialize";
  import { Unicode11Addon } from "@xterm/addon-unicode11";
  import { WebglAddon } from "@xterm/addon-webgl";
  import "@xterm/xterm/css/xterm.css";
  import { AlertCircle, AlertTriangle, LoaderCircle, RotateCw, Search, ChevronUp, ChevronDown, X, Minus, Plus, ArrowDownToLine, ExternalLink, FileText } from "@lucide/svelte";
  import { get } from "svelte/store";
  import { interfaceStore } from "../stores/interfaceStore";
  import { harnessStore } from "../stores/harnessStore";
  import { themeStore } from "../stores/themeStore";
  import { formatError } from "../ui/formatError";
  import { createSessionLifecycle } from "../terminal/sessionLifecycle";
  import { CONTEXT_READ_MS, terminalDeadline } from "../terminal/inputQueue";
  import { isProvablyIdle, parseTerminalContext, readSessionContext, type ContextRead } from "../terminal/sessionContext";
  import { isEditingElsewhere } from "../keyboard/terminalFocus";
  import { planPaste } from "../terminal/pasteGuard";
  import { askConfirm } from "../stores/modalStore";
  import { createAckCoalescer, createOutputCredit, planTerminalOutput, type PaintToken } from "../terminal/outputCredit";
  import { createRafWriteQueue } from "../terminal/rafWriteQueue";
  import { scrollbackSnapshots } from "../terminal/scrollbackSnapshot";
  import { createSyncFrameWriter } from "../terminal/syncFrames";
  import { applyHelperTextareaHardening, createEraseGuard, describeTerminalControl, webkit229ChordEvent, type TerminalInputEvent } from "../terminal/eraseGuard";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { copyText } from "../desktop/clipboard";
  import { ptyBus } from "../terminal/ptyBus.tauri";
  import { launcherLabel, type LauncherKind, type ResumeLaunch } from "../terminal/tabs";
  import { agentPromptArgs, resumeArgs } from "../terminal/launchRequests";
  import { AGY_CONTINUE_SENTINEL, conversationForStart, hostedClaude, resolveTranscriptAnswer, resumers, settleConversation, unindexedExitOffer, type ExitFacts, type ResumeOutcome } from "../terminal/hostedAgent";
  import { agentEvents } from "../terminal/agentEvents";
  import { checkTranscripts } from "../terminal/transcripts";
  import {
    effectiveMode,
    PERMISSION_LABELS,
    requiresAcknowledgement,
    type PermissionMode,
  } from "../terminal/agentDefaults";
  import { agentDefaults, loadAgentDefaults } from "../stores/agentDefaultsStore";
  import { reportAttentionAnswered, terminalAttendance } from "../stores/sessionAlertsStore";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { createGpuRenderer, type GpuRenderer, type GpuState } from "../terminal/gpuRenderer";
  import { createFrameProbe, FRAME_PROBE_OSC, handleFrameProbeCommand } from "../diagnostics/frameProbe";
  import {
    clipEndedTail,
    endedTail,
    forgetEndedTail,
    releaseEndedScrollback,
    rememberEndedTail,
    shouldReleaseEndedBuffer,
  } from "../terminal/endedBuffer";
  import type { AgentArtifactRead, TerminalSpawned } from "../terminal/runResult";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { observeResize } from "../dom/observeResize";
  import { watchDevicePixelRatio } from "../dom/devicePixelRatio";
  import { createFitScheduler } from "../terminal/fitSchedule";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { createScrollFeel, isScrolledBack, scrollFeelOptions, type ScrollFeel } from "../terminal/scrollFeel";
  import { wheelDiagnostic } from "../terminal/wheelDiagnostic";
  import { openExternal } from "../desktop/openExternal";
  import { isLoopbackUrl } from "../browser/browserSession";
  import { terminalLaunchRequests } from "../terminal/launchRequests";
  import { repoStore } from "../stores/repoStore";
  import { linksForRow, resolveLinkAction, type LinkBuffer } from "../terminal/links";
  import { requestReveal } from "../files/revealRequests";
  import { openTaskForRun } from "../workbench/taskOpen";
  import { noteAttempt } from "../terminal/taskLaunches";
  import { toastStore } from "../stores/toastStore";
  import { keepDetached, listingFamily, parseListing } from "../terminal/detachedSessions";
  import { clipboardImageToPngBase64, clipboardMayHoldFiles, extractClipboardImageFile } from "../terminal/clipboardImage";
  import {
    createCommandBlockTracker,
    createCommandDecorationRing,
    extractBlockCommand,
    extractBlockOutput,
    extractLoopbackServerUrl,
    formatCommandDuration,
    formatFailedCommandForAgent,
    parseOsc133,
    parseOsc7501,
    parseOsc7Path,
    resolveOsc7Subdir,
    type AgentOscStatus,
  } from "../terminal/commandBlocks";
  import { formatDroppedPathsForSession } from "../terminal/dropFormatting";
  import {
    agentPromptKeymap, clampTerminalFontSize, commandJumpChord, hasRenderedBox, macLineEditing, macOptionOptions, spawnGridSize, terminalScrollbackFor,
    terminalViewChord, terminalSearchSummary,
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
    checkout,
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
    onAdmitted,
    onRefused,
    revealSelf,
    confirmCloseSelf,
    onOpenFileSplit,
    onOpenSnippetSplit,
    onSendToAgent,
    onAgentStatus,
    title,
  }: {
    /** The repository tab hosting this session: its slot, its Sessions row, its close. */
    repoPath: string;
    /**
     * The checkout the process runs in and whose files its links name, when
     * that is another checkout of the same repository (a hosted agent; see
     * `taskLaunches.hostTabFor`). Absent means `repoPath`.
     */
    checkout?: string;
    tabId: string;
    launcher: LauncherKind;
    initialPrompt?: string;
    /** Directory below the checkout to start in; absent starts at its root. */
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
    /** This tab took a session slot (see `SessionHooks.admitted`). */
    onAdmitted?: () => void;
    /** Every slot was taken; true takes the refusal back (see `SessionHooks.refused`). */
    onRefused?: (message: string) => boolean;
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
    /** Opens a repository-relative file inside a split `TerminalFilePane` beside this terminal. */
    onOpenFileSplit?: (path: string, line: number | null, column: number | null, fallbackPath?: string) => boolean;
    /** Opens a read-only Markdown/text snippet inside a split `TerminalFilePane` beside this terminal. */
    onOpenSnippetSplit?: (title: string, content: string) => boolean;
    /** Sends a reference or formatted failing-command payload to the active/paired agent session. */
    onSendToAgent?: (text: string) => void;
    /** Reports Herdr-compatible OSC 7501 agent status updates for the tab badge. */
    onAgentStatus?: (status: AgentOscStatus | null) => void;
    /** What this session is about (a task title), for the Sessions list. */
    title?: string;
  } = $props();

  /** Where the process runs and whose files its output names. */
  const workPath = $derived(checkout ?? repoPath);

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
  /** The safer answer: initial focus for the bypass prompt. */
  let declineButton = $state<HTMLButtonElement | null>(null);
  let container = $state<HTMLDivElement | null>(null);
  let warning = $state<string | null>(null);
  /** Typing is queued behind a send the host has not answered; retracts itself, separate from `warning`. */
  let slowInput = $state<string | null>(null);
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
  let scrollFeel: ScrollFeel | null = null;
  /** What the pointer is currently over, shown in the footer so the target is
   * readable before the click rather than only after it. */
  let hoveredLink = $state<string | null>(null);
  /** Set once xterm is open in its container; a renderer can only attach after. */
  let opened = $state(false);
  /** How this session draws now; on the container as `data-renderer`. */
  let renderer = $state<GpuState>("dom");
  /** Live repository-relative directory from OSC 7 (`""` for root, `null` when outside repo or unknown). */
  let activeCwdSubdir = $state<string | null>(untrack(() => startDir ?? null));
  /** Latest loopback dev-server URL observed in this session's output. */
  let devServerUrl = $state<string | null>(null);
  let dismissedDevServerUrl = $state<string | null>(null);
  /** Live artifact directory reported by Antigravity `SessionStart`, when present. */
  let agentArtifactDir = $state<string | null>(null);
  let lastExitCode = $state<number | null>(null);
  const commandBlocks = createCommandBlockTracker();
  const commandDecorations = createCommandDecorationRing(64);
  const outputTextDecoder = new TextDecoder("utf-8", { fatal: false });

  export async function openAgentArtifact() {
    if (!nativeSessionId) return;
    try {
      const artifact = await invoke<AgentArtifactRead>("cmd_terminal_read_agent_artifact", {
        sessionId: nativeSessionId,
        hintArtifactDir: agentArtifactDir ?? hostedClaude.byTerminal(nativeSessionId)?.artifactDir ?? null,
      });
      if (artifact.repo_relative_path && onOpenFileSplit?.(artifact.repo_relative_path, 1, null)) {
        return;
      }
      if (onOpenSnippetSplit?.(artifact.title, artifact.content)) {
        return;
      }
      warning = "Could not open agent plan beside this terminal.";
    } catch (err) {
      warning = `Could not open agent plan: ${formatError(err)}`;
    }
  }

  /** Non-reactive handles: observers and the emulator must not tear down with runes. */
  let term: XTerm | null = null;
  let gpu: GpuRenderer | null = null;
  let stopAgentMeta: (() => void) | null = null;
  /** Frame pacing on request from the program in this terminal (OSC 7700). */
  const frameProbe = createFrameProbe();
  let fitAddon: FitAddon | null = null;
  let searchAddon: SearchAddon | null = null;
  let serializeAddon: SerializeAddon | null = null;
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
    return buildTermTheme((name, fallback) => css.getPropertyValue(name).trim() || fallback);
  }

  /** The palette on the terminal now, so an unchanged one is not set again. */
  let appliedTheme: Record<string, string> | null = null;

  /**
   * The one place a palette reaches xterm. Called from the store effect and
   * from the observer on the root's class/style, which also fires for accent,
   * tab-size and UI-scale changes that leave the palette alone; each set
   * would rebuild the colour set and the glyph atlas of every session, the
   * hidden ones included.
   */
  function applyTheme() {
    if (!term) return;
    const next = termTheme();
    if (sameTheme(appliedTheme, next)) return;
    appliedTheme = next;
    term.options.theme = next;
    gpu?.clearAtlas();
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
  function resolveLinkInSession(text: string) {
    if (activeCwdSubdir) {
      const subAction = resolveLinkAction(text, workPath, activeCwdSubdir);
      if (subAction.kind !== "refused") return subAction;
    }
    return resolveLinkAction(text, workPath);
  }

  async function activateLink(text: string, event?: MouseEvent) {
    const action = resolveLinkAction(text, workPath);
    const resolved = activeCwdSubdir && action.kind !== "url" ? resolveLinkInSession(text) : action;
    if (resolved.kind === "refused") {
      warning = resolved.reason;
      return;
    }
    if (resolved.kind === "url") {
      // A local dev server opens in a browser session beside this terminal,
      // where an agent can be pointed at it; anything else, or a session that
      // cannot open, goes to the system browser as before.
      if (opensInBrowserSession(resolved.url)) {
        try {
          await terminalLaunchRequests.requestBrowser(repoPath, resolved.url);
          return;
        } catch (err) {
          warning = `${resolved.url} opened in your browser: a browser session couldn't (${formatError(err)}).`;
        }
      }
      try {
        await openExternal(resolved.url);
      } catch (err) {
        warning = `Couldn't open ${resolved.url}. ${formatError(err)}`;
      }
      return;
    }
    const jumpToCodeView = Boolean(event?.metaKey || event?.ctrlKey);
    if (!jumpToCodeView && onOpenFileSplit?.(resolved.path, resolved.line, resolved.column, resolved.fallbackPath)) {
      return;
    }
    // `selectFilePath` acts on whichever repository tab is active, so a click
    // in a background repository's terminal would open the path in the wrong
    // checkout. Refuse instead of opening someone else's file.
    const state = get(repoStore);
    const activePath = state.openTabs.find((tab) => tab.id === state.activeTabId)?.path ?? "";
    if (checkout && activePath !== checkout) {
      // A hosted agent's files are in its own checkout, which has no tab
      // until someone asks for one. This click is that ask.
      let ready = false;
      const opened = await repoStore.openRepo(checkout, { onReady: () => { ready = true; } });
      if (!opened || !ready) {
        warning = `Couldn't open ${checkout.split(/[\\/]/).pop()} to show ${resolved.path}.`;
        return;
      }
    } else if (!checkout && activePath !== repoPath) {
      warning = "Switch to this terminal's repository to open its files.";
      return;
    }
    // Recorded before the selection, so the request is already waiting when
    // the viewer mounts and reads the file. A reference with no line simply
    // opens the file, which is why an unusable request is not an error here.
    requestReveal(resolved.path, resolved.line, resolved.column);
    repoStore.selectFilePath(resolved.path);
    repoStore.setActiveTab("code");
  }

  /** Whether a clicked link goes to a browser session rather than the system browser. */
  function opensInBrowserSession(url: string): boolean {
    return get(hostPlatform).browser_sessions && isLoopbackUrl(url);
  }

  /** Hover text for a link, so the target is readable before it is clicked. */
  function linkTitle(text: string): string {
    const action = resolveLinkInSession(text);
    if (action.kind === "url") {
      return opensInBrowserSession(action.url)
        ? `Open ${action.url} in a browser session (the address bar's ↗ opens it in your browser)`
        : `Open ${action.url} in your browser`;
    }
    if (action.kind === "file") {
      const at = action.line === null ? "" : ` (line ${action.line}${action.column === null ? "" : `, column ${action.column}`})`;
      return onOpenFileSplit
        ? `Open ${action.path}${at} beside terminal (${platformChord("⌘-click", "Ctrl+click", $hostPlatform.os)} for Code view)`
        : `Open ${action.path}${at}`;
    }
    return action.reason;
  }

  /** Rows of history this session keeps: its kind's setting, see `terminalScrollbackFor`. */
  function sessionScrollback(): number {
    return terminalScrollbackFor({ launcher, taskRunId }, get(interfaceStore));
  }

  function ensureTerm(): XTerm | null {
    if (term) return term;
    const created = new XTerm({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
      fontSize,
      lineHeight: 1.15,
      ...macOptionOptions(get(hostPlatform).os, get(interfaceStore).terminalOptionAsMeta),
      // The official search addon uses xterm's proposed decoration API.
      allowProposedApi: true,
      cursorBlink: true,
      convertEol: false,
      theme: termTheme(),
      // Required before `open()` for a non-opaque background, and not
      // changeable afterwards. Its documented cost falls on the texture-atlas
      // renderer, which an on-screen session now uses (`gpuRenderer.ts`); the
      // DOM renderer a hidden or lost session falls back to draws the
      // background as a plain CSS colour.
      allowTransparency: isTranslucentHexColor(termTheme().background),
      scrollback: sessionScrollback(),
      // Wheel feel: see `terminal/scrollFeel.ts` and docs/PERFORMANCE.md (2026-11-06).
      ...scrollFeelOptions(get(interfaceStore).terminalSmoothWheel),
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
        activate: (event, text) => { void activateLink(text, event); },
        allowNonHttpProtocols: false,
      },
    });
    // xterm's built-in width table is Unicode 6, which gives every emoji one
    // cell while the font draws two. The addon registers the Unicode 11 table
    // and the version is switched before anything is written, so the cursor
    // column, wrapping and the cells links and search read all agree with what
    // a program that measured the glyph as wide has laid out. Here and not
    // later: this is the one place a terminal is built, including a recreated
    // one that is given an ended session's remembered tail.
    created.loadAddon(new Unicode11Addon());
    created.unicode.activeVersion = "11";
    fitAddon = new FitAddon();
    created.loadAddon(fitAddon);
    searchAddon = new SearchAddon({ highlightLimit: SEARCH_HIGHLIGHT_LIMIT });
    created.loadAddon(searchAddon);
    serializeAddon = new SerializeAddon();
    created.loadAddon(serializeAddon);
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
            .filter((link) => resolveLinkInSession(link.text).kind !== "refused");
          callback(found.length ? found.map((link) => ({
            range: link.range,
            text: link.text,
            activate: (event, text) => { void activateLink(text, event); },
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
      scrolledBack = isScrolledBack(created.buffer.active);
    });
    created.onData((data) => {
      const shape = describeTerminalControl(data);
      if (shape) {
        const line = `pty-input ${shape.detail}`;
        if (shape.printable > 0) console.info("[gitpulse-terminal]", line);
        else console.debug("[gitpulse-terminal]", line);
      }
      // The reader answering whatever the agent last asked; xterm's own
      // replies (focus, cursor reports) are told apart inside. When something
      // stood, the notifier is told too, or it would replay the question and
      // leave its banner up.
      if (nativeSessionId && sessionActivity.input(nativeSessionId, data)) reportAttentionAnswered(nativeSessionId);
      lifecycle?.write(data);
    });
    created.onBinary((data) => { lifecycle?.write(data, true); });
    // The renderer benches (scripts/tui-redraw-bench.mjs) ask for frame
    // pacing from inside the session. The answer goes back as the terminal's
    // own reply, not as typing; see diagnostics/frameProbe.ts.
    created.parser.registerOscHandler(FRAME_PROBE_OSC, (payload) => {
      const reply = handleFrameProbeCommand(frameProbe, payload);
      if (reply) created.input(reply, false);
      return true;
    });
    created.parser.registerOscHandler(7, (payload) => {
      const oscPath = parseOsc7Path(payload);
      if (oscPath) {
        activeCwdSubdir = resolveOsc7Subdir(workPath, oscPath);
      }
      return true;
    });
    created.parser.registerOscHandler(133, (payload) => {
      const mark = parseOsc133(payload);
      if (mark) {
        commandBlocks.onMark(mark, () => created.registerMarker(0));
        if (mark.kind === "command_finished") {
          lastExitCode = mark.exitCode;
          const detail = commandBlocks.lastFinishedDetail();
          if (detail && created.buffer.active.type !== "alternate" && detail.promptMarker.line >= 0) {
            const cursorAbs = created.buffer.active.baseY + created.buffer.active.cursorY;
            const decMarker = created.registerMarker(detail.promptMarker.line - cursorAbs);
            if (decMarker) {
              const decoration = created.registerDecoration({
                marker: decMarker,
                anchor: "right",
                width: 1,
              });
              if (!decoration) {
                decMarker.dispose();
              } else {
                commandDecorations.push({ marker: decMarker, decoration });
                decoration.onRender((el) => {
                  if (el.dataset.gpOsc133 === "1") return;
                  el.dataset.gpOsc133 = "1";
                  el.style.width = "auto";
                  el.style.right = "6px";
                  el.style.left = "auto";
                  el.style.zIndex = "6";
                  el.style.pointerEvents = "auto";
                  el.style.display = "inline-flex";
                  el.style.alignItems = "center";
                  el.style.gap = "4px";
                  el.style.padding = "0 5px";
                  el.style.borderRadius = "4px";
                  el.style.fontSize = "10px";
                  el.style.lineHeight = "16px";
                  el.classList.add("font-mono");
                  const failed = detail.exitCode !== null && detail.exitCode !== 0;
                  el.style.background = failed ? "rgba(244, 63, 94, 0.16)" : "rgba(16, 185, 129, 0.14)";
                  el.style.border = failed ? "1px solid rgba(244, 63, 94, 0.35)" : "1px solid rgba(16, 185, 129, 0.3)";
                  el.style.color = failed ? "#fda4af" : "#6ee7b7";

                  const badge = document.createElement("span");
                  const dur = detail.durationMs !== null ? ` ${formatCommandDuration(detail.durationMs)}` : "";
                  badge.textContent = failed
                    ? `✕ exit ${detail.exitCode}${dur}`
                    : `✓${dur || " ok"}`;
                  el.appendChild(badge);

                  if (failed) {
                    const askBtn = document.createElement("button");
                    askBtn.type = "button";
                    askBtn.setAttribute("data-testid", "terminal-osc133-ask-agent");
                    askBtn.textContent = "Ask agent";
                    askBtn.title = "Send failing command and output to active agent terminal";
                    askBtn.style.marginLeft = "2px";
                    askBtn.style.padding = "0 4px";
                    askBtn.style.borderRadius = "3px";
                    askBtn.style.background = "rgba(244, 63, 94, 0.22)";
                    askBtn.style.color = "#ffe4e6";
                    askBtn.style.cursor = "pointer";
                    askBtn.addEventListener("mousedown", (ev) => ev.stopPropagation());
                    askBtn.addEventListener("click", (ev) => {
                      ev.preventDefault();
                      ev.stopPropagation();
                      if (disposed || !term || detail.promptMarker.isDisposed) return;
                      const cmd = extractBlockCommand(term.buffer.active, {
                        promptLine: detail.promptMarker.line,
                        outputStartLine: detail.outputStartMarker && !detail.outputStartMarker.isDisposed
                          ? detail.outputStartMarker.line
                          : null,
                      });
                      const out =
                        detail.outputStartMarker &&
                        !detail.outputStartMarker.isDisposed &&
                        detail.outputEndMarker &&
                        !detail.outputEndMarker.isDisposed
                          ? extractBlockOutput(term.buffer.active, {
                              outputStartLine: detail.outputStartMarker.line,
                              outputEndLine: detail.outputEndMarker.line,
                            })
                          : "";
                      const formatted = formatFailedCommandForAgent({
                        command: cmd,
                        exitCode: detail.exitCode,
                        durationMs: detail.durationMs,
                        cwd: activeCwdSubdir,
                        output: out,
                      });
                      if (onSendToAgent) {
                        onSendToAgent(`${formatted}\n`);
                      } else {
                        void copyText(formatted).then((ok) => {
                          if (ok) toastStore.info("Copied failing command & output to clipboard");
                        });
                      }
                    });
                    el.appendChild(askBtn);
                  }
                });
              }
            }
          }
        } else if (mark.kind === "command_executed") {
          lastExitCode = null;
        }
      }
      return true;
    });
    created.parser.registerOscHandler(7501, (payload) => {
      const parsed = parseOsc7501(payload);
      if (parsed) onAgentStatus?.(parsed.state === "clear" ? null : parsed);
      return true;
    });
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
    appliedTheme = termTheme();
    if (!disposed) {
      for (const token of credit.flush()) paintOutput(token);
    }
    return term;
  }

  /**
   * Released credit is owed here and sent once per frame (or at once past
   * `ACK_FLUSH_THRESHOLD`), not as one IPC call per 4 KiB chunk. Teardown
   * flushes it synchronously.
   */
  const acks = createAckCoalescer((sessionId, bytes) => {
    void invoke("cmd_terminal_ack", { sessionId, bytes }).catch((err: unknown) => {
      if (lifecycle?.isCurrent(sessionId)) lifecycle.fail(`Output acknowledgement failed: ${formatError(err)}`, true);
    });
  });

  /** Cancel (0x18): aborts an escape sequence a gap in the output cut short. */
  const CAN = Uint8Array.of(0x18);

  function acknowledgeOutput(sessionId: string, bytes: number) {
    if (bytes <= 0) return;
    acks.add(sessionId, bytes);
  }

  /**
   * Frame-budgeted write coalescer: merges whole DEC ?2026-safe chunks emitted
   * inside one animation frame into a single `term.write` call (with a 16 ms
   * backstop timer so hidden windows still return PTY output credit).
   */
  const rafWrites = createRafWriteQueue({
    write(bytes, written) {
      if (!term || disposed) throw new Error("the terminal is closed");
      term.write(bytes, written);
    },
    onError(err) {
      lifecycle?.fail(`Terminal output was not painted: ${formatError(err)}`, true);
    },
  });

  /**
   * Every byte the emulator is given as output goes through here, so each
   * write ends outside a DEC 2026 block (see `syncFrames.ts`: xterm drops the
   * paint of a frame whose successor has opened by the time its animation
   * frame fires). Held bytes are acknowledged when they are written.
   */
  const syncFrames = createSyncFrameWriter({
    write(bytes, written) {
      if (!term || disposed) throw new Error("the terminal is closed");
      rafWrites.enqueue(bytes, written);
    },
    onError(err) {
      lifecycle?.fail(`Terminal output was not painted: ${formatError(err)}`, true);
    },
  });

  function paintOutput(token: PaintToken) {
    const termNow = term;
    if (!termNow || disposed) {
      acknowledgeOutput(token.sessionId, token.release());
      return;
    }
    try {
      syncFrames.push(token.bytes, () => {
        acknowledgeOutput(token.sessionId, token.release());
      });
    } catch (err) {
      acknowledgeOutput(token.sessionId, token.release());
      lifecycle?.fail(`Terminal output was not painted: ${formatError(err)}`, true);
    }
  }

  /**
   * Layout moved: fit, at most once per `FIT_MIN_INTERVAL_MS`. The resize
   * observer and the first fit come here; showing a terminal and changing the
   * font go through `fits.settle()`. All of them end in `fitNow`, which is the
   * only place the emulator is resized and so the only place that decides
   * whether it may be.
   */
  function refitIfResized() {
    fits.request();
  }

  /**
   * Measure the box and, if the grid it holds changed, resize to it.
   *
   * A terminal with no box (a hidden tab, a collapsed dock, another
   * repository's dock, a container squeezed to nothing) is left exactly as it
   * is. Measuring it would answer with a small confident grid that is not
   * evidence of anything, and the PTY would be told it, which makes the
   * program inside repaint for a tab nobody is looking at. It is fitted once
   * when it is shown again (`reveal`, and the observer when the box returns).
   */
  function fitNow() {
    if (disposed || !fitAddon || !term) return;
    try {
      const rendered = hasRenderedBox(container);
      const proposal = rendered ? fitAddon.proposeDimensions() : undefined;
      const proposed = proposal ? { cols: Math.min(1000, proposal.cols), rows: Math.min(1000, proposal.rows) } : proposal;
      if (!shouldRefit({ cols: term.cols, rows: term.rows }, proposed, rendered)) return;
      if (proposal && (proposal.cols > 1000 || proposal.rows > 1000) && proposed) {
        term.resize(proposed.cols, proposed.rows);
      } else fitAddon.fit();
    } catch {
      /* container collapsed; refit when it has size again */
    }
  }

  const fits = createFitScheduler(fitNow);

  /** The grid's cell size changed under an unchanged box: a new font size. */
  function refitAfterMetricsChange() {
    fits.settle();
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
    const prompt = takeResumePrompt();
    const args = resume
      ? (kind === "agy"
          ? ["--conversation", resume.sessionId, ...(agentPromptArgs("agy", prompt) ?? [])]
          : resumeArgs(resume.sessionId, prompt))
      : agentPromptArgs(kind, initialPrompt) ?? [];
    return kind === "shell" ? {} : { program: kind, args };
  }
  /**
   * The first message of a resumed conversation, once: a restart of this tab
   * resumes without it, because the conversation already heard it.
   */
  let resumePrompt: string | undefined = untrack(() => resume?.prompt);
  function takeResumePrompt(): string | undefined {
    const prompt = resumePrompt;
    resumePrompt = undefined;
    return prompt;
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

  /**
   * The Claude Code conversation this tab's process runs, when it is one
   * GitPulse can name: a fresh launch carries a `--session-id` GitPulse chose,
   * a resumed one the id it resumes. Null for a shell, a task attempt (whose
   * id the backend assigns) and a `--continue` launch.
   */
  let hostedId: string | null = null;
  /**
   * The process running `hostedId` has started and not been seen to end. A
   * view going away while it runs leaves the record (the process is kept, and
   * the next tab to show it finds the record by its PTY).
   */
  let hostedLive = false;
  /**
   * Why a conversation a signal ended is not offered: its transcript is not
   * there, or could not be checked. Shown in place of Resume.
   */
  let transcriptNote = $state<string | null>(null);
  /** Set to relaunch the conversation a signal ended; consumed by the next spawn. */
  let resumeOverride: string | null = null;
  /** The conversation a signal ended in this tab, which Resume relaunches. */
  let resumeOffer = $state<string | null>(null);
  /** The dead PTY the offer was registered under for an incident's "Resume N". */
  let offerTerminal: string | null = null;

  function withdrawOffer() {
    if (offerTerminal) resumers.unregister(offerTerminal);
    offerTerminal = null;
    resumeOffer = null;
    transcriptNote = null;
  }

  /**
   * What the end of this tab's process does to its conversation's record
   * (`settleConversation`, the one decision every place that sees the end
   * shares). An end from outside keeps the record and, once Claude Code's
   * transcript of it is found, offers Resume here and to an incident's
   * "Resume N". Absent offers nothing; Unknown says it could not check.
   */
  function settleHosted(end: ExitFacts | "closed", terminalId: string | null) {
    const conversation = hostedId;
    hostedLive = false;
    if (!conversation) {
      const fallback = !taskRunId && launcher === "agy"
        ? unindexedExitOffer("agy", null, end)
        : null;
      if (fallback && terminalId) {
        resumeOffer = fallback;
        offerTerminal = terminalId;
        resumers.register(terminalId, resumeEnded);
      }
      return;
    }
    if (!settleConversation(hostedClaude, conversation, end) || !terminalId) return;
    void checkTranscripts([conversation]).then((answers) => {
      // A restart, a new process or this view going away while the check ran:
      // the offer belongs to a session that is no longer this tab's.
      if (disposed || hostedId !== conversation || nativeSessionId !== terminalId || hostedLive) return;
      const answer = answers.get(conversation) ?? { state: "unknown" as const, reason: "no answer" };
      if (answer.state === "absent") hostedClaude.forget(conversation);
      const resolved = resolveTranscriptAnswer(hostedClaude, conversation, launcher === "agy" ? "agy" : "claude", answer);
      if (resolved.offer) {
        resumeOffer = resolved.offer;
        offerTerminal = terminalId;
        resumers.register(terminalId, resumeEnded);
      } else {
        transcriptNote = resolved.note;
      }
    });
  }

  /**
   * Relaunches the conversation a signal ended, in this tab: the same
   * directory and permission mode (this tab's own, read once at mount), by
   * the same `--resume <id>` launch a resumed attempt uses.
   */
  async function resumeEnded(): Promise<ResumeOutcome> {
    const id = resumeOffer;
    if (!id || disposed || !lifecycle) return "failed";
    withdrawOffer();
    resumeOverride = id;
    return lifecycle.restart();
  }

  /** The session this tab is still to take over, until its first start. */
  let attachPending: string | null = untrack(() => attachSessionId ?? null);

  /**
   * Runs a command that starts or takes over a session with that session's
   * own output channel, opened first so nothing it prints before the id comes
   * back is missed, and tells the bus whose channel it is once the id is known.
   */
  async function withOutput(run: (channel: object) => Promise<TerminalSpawned>): Promise<TerminalSpawned> {
    const output = ptyBus.openOutput();
    try {
      const spawned = await run(output.channel);
      output.bind(spawned.id);
      return spawned;
    } catch (error) {
      output.release();
      throw error;
    }
  }

  function createLifecycle() {
    return createSessionLifecycle({
      key: tabId, repoPath, ...(checkout ? { checkout } : {}), label: launcherLabel(launcher), launcher, bus: ptyBus, registry: terminalSessions,
      singleAttempt: !!taskRunId,
      adopting: () => !taskRunId && attachPending !== null,
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
          const attachTo = attachPending;
          attachPending = null;
          const resumeId = resumeOverride;
          resumeOverride = null;
          hostedId = null;
          // A conversation GitPulse can name is one it can resume. A fresh
          // Claude launch is given an id and a resumed one keeps its own; a
          // takeover keeps whatever is on record for its PTY and records
          // nothing (`conversationForStart`). A task attempt's run id is its
          // session id, assigned by the backend.
          if ((launcher === "claude" || launcher === "agy") && !taskRunId) {
            const provider = launcher === "agy" ? "agy" : "claude";
            const decided = conversationForStart(hostedClaude, {
              attach: attachTo, resumeId, args: cfg.args,
              record: { repoPath: workPath, startDir: startDir ?? null, mode: permissionMode, startedAt: Date.now(), provider },
              provider,
            });
            cfg.args = decided.args;
            hostedId = decided.sessionId;
          }
          if (attachTo) {
            return withOutput((onOutput) => invoke<TerminalSpawned>("cmd_terminal_attach", { sessionId: attachTo, rows, cols, onOutput }));
          }
          if (taskRunId) return withOutput((onOutput) => invoke<TerminalSpawned>("cmd_workbench_launch_terminal", {
            input: JSON.stringify({ id: taskRunId, expected_revision: 1, rows, cols }),
            onOutput,
          }));
          return withOutput((onOutput) => invoke<TerminalSpawned>("cmd_terminal_spawn", {
            repoPath: workPath, rows, cols,
            program: cfg.program, args: cfg.args,
            startDir: startDir ?? null,
            permissionMode,
            // Sent only for the mode that needs it. The backend refuses an
            // acknowledgement attached to any other mode, so a bug that sent
            // this unconditionally would fail on the next ordinary launch
            // rather than wait to matter.
            acknowledged: requiresAcknowledgement(permissionMode) ? acknowledgedBypass : false,
            onOutput,
          }));
        },
        write: (sessionId, data, binary) => invoke("cmd_terminal_write", { sessionId, data, binary }),
        resize: (sessionId, rows, cols) => invoke("cmd_terminal_resize", { sessionId, rows, cols }),
        kill: (sessionId) => invoke("cmd_terminal_kill", { sessionId }),
        detach: (sessionId) => invoke<boolean>("cmd_terminal_detach", { sessionId }),
      },
      // Only a plain shell positively at its prompt may end with its view.
      // An agent, a running job, or a context that cannot be read is kept.
      idle: async (sessionId) => {
        if (launcher !== "shell") return false;
        const context = parseTerminalContext(await invoke<unknown>("cmd_terminal_context", { sessionId }));
        // `busy` is only the foreground; a `cmd &` or a `^Z`'d editor is a job too.
        return context !== null && isProvablyIdle(context);
      },
      // Listed in Sessions, where it can be shown again or stopped. Looked up
      // while this tab still holds the slot, which it then hands over.
      keep: async (sessionId) => {
        const raw = await invoke<unknown>("cmd_terminal_sessions");
        const listing = Array.isArray(raw) ? raw.map(parseListing).find((row) => row?.id === sessionId) ?? null : null;
        if (!listing) return null;
        const family = await listingFamily(listing, (repo) => repoStore.familyOf(repo));
        return (slot) => keepDetached({ ...listing, detached: true }, slot, { family });
      },
      hooks: {
        state(status, message) {
          spawning = status === "starting";
          if (spawning) {
            withdrawOffer();
            onAgentStatus?.(null);
            devServerUrl = null;
            dismissedDevServerUrl = null;
            agentArtifactDir = null;
            stopAgentMeta?.();
            stopAgentMeta = null;
          }
          exited = status === "exited";
          if (exited) onAgentStatus?.(null);
          error = status === "error" ? message ?? "Terminal failed" : null;
          onStatus(status);
          if (taskRunId) reportAttemptStart(taskRunId, status, message);
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
          if (hostedId) {
            hostedClaude.bind(hostedId, spawned.id);
            hostedLive = true;
          }
          const rememberedMeta = hostedClaude.byTerminal(spawned.id);
          if (rememberedMeta?.artifactDir) {
            agentArtifactDir = rememberedMeta.artifactDir;
          }
          stopAgentMeta?.();
          stopAgentMeta = agentEvents.subscribeAgentMeta(spawned.id, (meta) => {
            if (disposed || nativeSessionId !== spawned.id) return;
            if (meta.artifact_dir) agentArtifactDir = meta.artifact_dir;
            if ((launcher === "claude" || launcher === "agy") && !taskRunId) {
              const bound = hostedClaude.bindAgentMeta({
                terminalId: spawned.id,
                sessionId: meta.conversation_id,
                provider: meta.agent === "agy" || launcher === "agy" ? "agy" : "claude",
                repoPath: workPath,
                startDir: startDir ?? null,
                mode: permissionMode,
                artifactDir: meta.artifact_dir ?? null,
              });
              if (bound) {
                hostedId = bound.sessionId;
                hostedLive = true;
              }
            }
          });
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
        output(bytes, sessionId) {
          const decision = planTerminalOutput(credit, bytes, sessionId, {
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
          }
          if (!disposed) {
            if ($hostPlatform.browser_sessions && bytes.length > 0 && bytes.length <= 16_384) {
              const decoded = outputTextDecoder.decode(bytes, { stream: true });
              const readyUrl = extractLoopbackServerUrl(decoded);
              if (readyUrl && readyUrl !== devServerUrl) {
                devServerUrl = readyUrl;
              }
            }
            sessionActivity.output(sessionId);
            if (!active) onActivity();
          }
        },
        exit(event) {
          stopAgentMeta?.();
          stopAgentMeta = null;
          agentEvents.forgetAgentMeta(event.id);
          // GitPulse's own Close does reach here when its exit event lands
          // before the close is acknowledged, and it says so
          // (`ended_by_gitpulse`): only a signal from outside leaves a
          // conversation to offer back, and only one Claude Code saved.
          settleHosted(event, event.id);
          const why = event.error || event.signal || (event.exit_code === null ? "exited" : `exit ${event.exit_code}`);
          syncFrames.flush();
          rafWrites.flushSync();
          term?.writeln(`\r\n\u001b[2m[session closed — ${why}]\u001b[0m`);
        },
        // The close was acknowledged; its exit event may not come at all.
        closed() { settleHosted("closed", null); },
        reset() {
          return new Promise<void>((resolve) => {
            if (!term || disposed) { resolve(); return; }
            syncFrames.flush();
            rafWrites.flushSync();
            term.write("", () => {
              if (!disposed) { commandDecorations.clear(); commandBlocks.clear(); term?.reset(); syncFrames.reset(); warning = null; searchKey = null; }
              resolve();
            });
          });
        },
        warning(message) { warning = message; },
        slow(message) { slowInput = message; },
        // CAN aborts any escape, OSC or DCS sequence the gap cut short, and
        // does nothing at all on the ground state. Queued behind what was
        // painted, ahead of what the resumed stream sends.
        skipped() {
          if (!term || disposed) return;
          syncFrames.flush();
          rafWrites.flushSync();
          syncFrames.push(CAN, () => {});
        },
        admitted() { onAdmitted?.(); },
        refused(message) { return onRefused?.(message) ?? false; },
      },
    });
  }

  /**
   * Tells the attempt's row what became of its start. This tab is usually
   * hidden (a launch starts the agent out of sight), so a spawn the host
   * refused — the CLI missing or too old, a bad argument — was said only in
   * here, and the failed start then released its registry slot, leaving the
   * Agents pane to say a quiet "not connected". Only a failure before this
   * tab ever had a process counts as a failed start; a later one is a live
   * session's problem, which the pane already shows from the registry.
   *
   * A failed start is one whose registry slot is gone once the lifecycle
   * has finished with it (it releases the slot right after this hook). A
   * spawn still pending past its watchdog also reports an error, but keeps
   * its slot and may yet start, so it is not one.
   */
  function reportAttemptStart(runId: string, status: "starting" | "running" | "exited" | "error", message?: string) {
    if (status === "running") noteAttempt(runId, "running", `${launcherLabel(launcher)} is running.`);
    else if (status === "starting") noteAttempt(runId, "starting", `Starting ${launcherLabel(launcher)}…`);
    else if (status === "error" && !nativeSessionId) {
      // The lifecycle stringifies the rejection; its "Error: " says nothing.
      const cause = (message ?? "").replace(/^Error:\s*/, "").trim() || "The terminal failed to start.";
      queueMicrotask(() => {
        if (nativeSessionId || get(terminalSessions).some((record) => record.key === tabId)) return;
        noteAttempt(runId, "failed", cause);
      });
    }
  }

  async function spawnPty() { await lifecycle?.start(); }

  /** Agreed to for this tab. The stored preference is untouched. */
  async function acknowledgeBypass() {
    acknowledgedBypass = true;
    awaitingAcknowledgement = false;
    void spawnPty();
    await handBackFocus();
  }

  /**
   * The prompt's button is gone once either answer lands, and the stored
   * restore target would be the xterm textarea — so focus goes to the grid
   * explicitly, and only for the tab being looked at.
   */
  async function handBackFocus() {
    await tick();
    if (active) term?.focus();
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
  async function declineBypass() {
    permissionMode = "ask";
    acknowledgedBypass = false;
    awaitingAcknowledgement = false;
    void spawnPty();
    await handBackFocus();
  }

  export function restart() { void lifecycle?.restart(); }

  /**
   * The user's Close: ends the process. Called before the tab is dropped, so
   * the unmount that follows has nothing left to keep. Rejects when the
   * process could not be stopped; the tab then stays, with its retry.
   */
  export async function close(): Promise<void> { await lifecycle?.close(); }

  /**
   * The attempt this tab belongs to: its own, or the one a resumed
   * conversation continues. Its task lists both, so both link back to it.
   */
  const linkedRunId = $derived(taskRunId ?? resume?.runId ?? null);
  const startingLabel = $derived(`Starting ${launcherLabel(launcher)}…`);
  const endedLabel = $derived(resumeOffer ? "This session was ended by a signal." : "This session ended.");
  /** What the always-mounted live region says: spawning wins, as it does in the status row. */
  const statusAnnouncement = $derived(spawning ? startingLabel : !error && exited ? endedLabel : "");

  /** Opens the task this tab's attempt belongs to, where a new attempt starts. */
  async function openOwnTask() {
    if (!linkedRunId) return;
    try {
      await openTaskForRun(linkedRunId);
    } catch (cause) {
      toastStore.error(`This attempt's task couldn't be opened: ${formatError(cause)}`);
    }
  }

  export async function copySelection() {
    const text = term?.getSelection() ?? "";
    if (!text) warning = "Select terminal text to copy.";
    else warning = await copyText(text) ? null : "Couldn't copy terminal selection.";
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
    warning = await copyText(retainedOutput()) ? null : "Couldn't copy retained terminal output.";
  }

  export function lastCommandOutput(): { output: string; exitCode: number | null } | null {
    const buffer = term?.buffer.normal;
    if (!buffer) return null;
    const block = commandBlocks.lastCompletedBlock();
    if (!block) return null;
    const output = extractBlockOutput(buffer, block);
    return { output, exitCode: block.exitCode };
  }

  export async function copyLastCommandOutput(): Promise<boolean> {
    const last = lastCommandOutput();
    if (!last || !last.output.trim()) {
      warning = "No completed command output recorded yet.";
      return false;
    }
    const ok = await copyText(last.output);
    warning = ok ? null : "Could not copy last command output.";
    return ok;
  }

  export async function exportOutput() {
    try { await invoke<boolean>("cmd_terminal_export", { data: retainedOutput() }); }
    catch (err) { if (!disposed) warning = formatError(err); }
  }

  export function serializeVisualOutput(): string {
    try {
      return serializeAddon?.serialize() ?? retainedOutput();
    } catch {
      return retainedOutput();
    }
  }

  /** Suppresses intermediate SIGWINCH storms while a dock or split sash is actively dragged. */
  export function setDragging(dragging: boolean) {
    fits.setDragging(dragging);
  }

  /** Injects text (file `@path` reference, selection range, or dropped paths) into the live PTY. */
  export function insertText(text: string, options?: { focus?: boolean }): boolean {
    if (!text || disposed || exited) return false;
    if (term) {
      if (launcher === "shell" && term.modes.bracketedPasteMode) {
        term.paste(planPaste(text, true).text);
      } else {
        term.input(text, true);
      }
      if (options?.focus !== false) term.focus();
      return true;
    }
    if (lifecycle) {
      lifecycle.write(text);
      return true;
    }
    return false;
  }

  export function isLive(): boolean {
    return !disposed && !exited;
  }

  export function backendSessionId(): string | null {
    return nativeSessionId || null;
  }

  /** Called by the panel when this tab becomes visible again. */
  export function reveal() {
    // Measure immediately if the pane already has a layout box so the first
    // visible frame paints the real grid rather than a 1-frame stale size;
    // `fits.settle()` then confirms on the next animation frame.
    fitNow();
    // After the layout that shows this pane (next frame, before it paints),
    // once: the box has its real size by then, and the observer's own fit for
    // the same change finds the grid already right.
    fits.settle();
    // The grid has no PTY behind it while the bypass prompt waits, so every
    // way back to this tab (Escape out of a menu, "View agent session", a
    // notification) lands on the prompt's safer answer instead.
    if (awaitingAcknowledgement) declineButton?.focus({ preventScroll: true });
    else if (findOpen) findInput?.focus();
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
   * What this session is running and where. Bounded (CONTEXT_READ_MS, whose
   * reason is in `inputQueue.ts`): a stuck query must not hold up the close or
   * the new tab that asked. The answer keeps "could not tell" apart from
   * "nothing is running" (see `ContextRead`); the decision logic is in
   * `readSessionContext`, which this only feeds the tab's own state.
   */
  export function readContext(): Promise<ContextRead> {
    const sessionId = nativeSessionId;
    return readSessionContext(
      { sessionId, current: Boolean(sessionId && lifecycle?.isCurrent(sessionId)), spawning, exited },
      (id) => terminalDeadline(invoke<unknown>("cmd_terminal_context", { sessionId: id }), CONTEXT_READ_MS, "Reading terminal context"),
    );
  }

  function setFontSize(size: number) {
    fontSize = clampTerminalFontSize(size);
    interfaceStore.setTerminalFontSize(fontSize);
    if (term) term.options.fontSize = fontSize;
    // Font metrics settle at layout; ResizeObserver also handles the changed box.
    refitAfterMetricsChange();
  }

  export function handleViewChord(event: KeyboardEvent): boolean {
    if (!active || event.defaultPrevented) return false;
    const jump = commandJumpChord(
      event,
      $hostPlatform.os,
      term?.buffer.active.type === "alternate",
    );
    if (jump && term) {
      const targetLine = commandBlocks.jumpTarget(term.buffer.active.viewportY, jump);
      if (targetLine !== null) {
        event.preventDefault();
        event.stopPropagation();
        term.scrollToLine(targetLine);
        return true;
      }
    }
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
    commandDecorations.clear();
    commandBlocks.clear();
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
     * The canvas reports a lost context the moment it happens. The addon's own
     * `onContextLoss` waits three seconds for a restore first, with a blank
     * canvas on screen meanwhile; this falls back to the DOM renderer at once.
     * Captured on the host because the event does not bubble.
     */
    const onContextLost = (event: Event) => gpu?.lose(event.target);
    const pastePlainText = (raw: string, current: XTerm) => {
      const plan = planPaste(raw, current.modes.bracketedPasteMode);
      if (!plan.question) { current.paste(plan.text); return; }
      void askConfirm({ ...plan.question, confirmLabel: "Paste" }).then((confirmed) => {
        if (disposed || term !== current) return;
        if (confirmed) current.paste(plan.text);
        current.focus();
      });
    };
    const pasteImageFile = (imageFile: File, current: XTerm) => {
      void clipboardImageToPngBase64(imageFile)
        .then((pngBase64) =>
          invoke<string>("cmd_terminal_save_clipboard_image", {
            tabId,
            pngBase64,
          }),
        )
        .then((savedPath) => {
          if (disposed || term !== current) return;
          const formatted = formatDroppedPathsForSession([savedPath], {
            launcher,
            repoPath: workPath,
          });
          insertText(formatted, { focus: true });
        })
        .catch((err) => {
          if (!disposed) warning = `Could not paste clipboard image: ${formatError(err)}`;
        });
    };
    /**
     * Every paste goes through `planPaste`: bracketed-paste markers inside the
     * clipboard are removed, and a paste that would run before it is read asks
     * first. Ahead of xterm's own listeners, which never strip the markers.
     */
    const onPasteCapture = (event: Event) => {
      if (!(event instanceof ClipboardEvent) || !host || !(event.target instanceof Node) || !host.contains(event.target)) return;
      const current = term;
      if (!current) return;
      const imageFile = extractClipboardImageFile(event.clipboardData);
      const raw = event.clipboardData?.getData("text/plain");
      const mayHoldFiles = clipboardMayHoldFiles(event.clipboardData, raw ?? "");
      if (mayHoldFiles) {
        event.preventDefault();
        event.stopPropagation();
        void invoke<string[]>("cmd_terminal_clipboard_files")
          .catch(() => [] as string[])
          .then((paths) => {
            if (disposed || term !== current) return;
            if (paths.length > 0) {
              const formatted = formatDroppedPathsForSession(paths, {
                launcher,
                repoPath: workPath,
              });
              insertText(formatted, { focus: true });
              return;
            }
            if (imageFile) {
              pasteImageFile(imageFile, current);
              return;
            }
            if (raw) {
              pastePlainText(raw, current);
            }
          });
        return;
      }
      if (imageFile) {
        event.preventDefault();
        event.stopPropagation();
        pasteImageFile(imageFile, current);
        return;
      }
      if (raw === undefined) return;
      event.preventDefault();
      event.stopPropagation();
      pastePlainText(raw, current);
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
        // would send it as a single DEL. Shift+Enter sends ESC CR for
        // multi-line prompts in coding agent TUIs and normal-screen shells.
        const alternateScreen = term?.buffer.active.type === "alternate";
        const edit = event.type === "keydown"
          ? (macLineEditing(event, $hostPlatform.os, alternateScreen) ??
             agentPromptKeymap(event, launcher, alternateScreen))
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
      host.addEventListener("webglcontextlost", onContextLost, true);
      const t = ensureTerm();
      t?.open(host);
      if (t) scrollFeel = createScrollFeel(t, { enabled: () => get(interfaceStore).terminalSmoothWheel, onSample: wheelDiagnostic() });
      if (t) {
        gpu = createGpuRenderer({
          attach: () => {
            const addon = new WebglAddon();
            t.loadAddon(addon);
            // The addon exposes no canvas; its WebGL one is the only canvas
            // on the screen without a `-layer` class (the link layer has one).
            // The renderer loses its context on release (xterm.js #6068).
            const canvas = [...(t.element?.querySelectorAll("canvas") ?? [])].find(
              (node) => ![...node.classList].some((name) => name.endsWith("-layer")),
            );
            return {
              canvas,
              dispose: () => addon.dispose(),
              onContextLoss: (listener) => addon.onContextLoss(listener),
              clearTextureAtlas: () => addon.clearTextureAtlas(),
            };
          },
          refresh: () => t.refresh(0, t.rows - 1),
          onChange: (state) => (renderer = state),
        });
        opened = true;
      }
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
    themeObserver = new MutationObserver(applyTheme);
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
        host.removeEventListener("webglcontextlost", onContextLost, true);
      }
      observedHelper?.removeEventListener("keydown", onXtermKeydown, true);
      observedHelper?.removeEventListener("blur", onHelperBlur);
      observedHelper = null;
      for (const timer of armTimers) clearTimeout(timer);
      armTimers.clear();
      disposed = true;
      // Closing the tab is the reader's own end of it, not an incident. A
      // process still running is kept (`lifecycle.dispose`), and so is its
      // conversation's record: the tab that shows it next finds it there.
      withdrawOffer();
      if (hostedId && !hostedLive) hostedClaude.forget(hostedId);
      syncFrames.discard();
      rafWrites.discard();
      for (const owed of credit.releaseAll()) acknowledgeOutput(owed.sessionId, owed.bytes);
      // Before `lifecycle` goes: nothing owed waits for a frame this view
      // will not see, and a failure is still reported against the session.
      acks.flush();
      if (nativeSessionId) { terminalAttendance.forget(nativeSessionId); sessionActivity.forget(nativeSessionId); }
      stopAgentMeta?.();
      stopAgentMeta = null;
      lifecycle?.dispose();
      lifecycle = null;
      linkProvider?.dispose();
      linkProvider = null;
      stopResize?.();
      stopResize = null;
      themeObserver?.disconnect();
      themeObserver = null;
      scrollFeel?.detach();
      scrollFeel = null;
      // Before the terminal: the addon gives its slot back to the shared pool.
      gpu?.dispose();
      gpu = null;
      frameProbe.stop();
      commandDecorations.clear();
      commandBlocks.clear();
      term?.dispose();
      term = null;
      fitAddon = null;
      searchAddon = null;
      serializeAddon = null;
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

  /** The wheel glide follows its setting on live sessions too. */
  $effect(() => {
    void $interfaceStore.terminalSmoothWheel;
    scrollFeel?.refresh();
  });

  /** Option as Meta follows its setting on live sessions too (macOS only). */
  $effect(() => {
    const options = macOptionOptions(get(hostPlatform).os, $interfaceStore.terminalOptionAsMeta);
    if (!term) return;
    term.options.macOptionIsMeta = options.macOptionIsMeta;
    term.options.macOptionClickForcesSelection = options.macOptionClickForcesSelection;
  });

  /**
   * Scrollback follows its setting on live sessions too. Lowering it trims the
   * oldest rows at once, which is the point of lowering it. A released ended
   * session sits at this same value (`releaseEndedScrollback`), and raising
   * it does not bring trimmed rows back.
   */
  $effect(() => {
    const rows = terminalScrollbackFor({ launcher, taskRunId }, $interfaceStore);
    if (term && term.options.scrollback !== rows) term.options.scrollback = rows;
  });

  /**
   * Silent kernel socket-table radar (`cmd_terminal_listening_ports`): catches
   * loopback dev servers that omit URL stdout lines while onscreen, and stops
   * immediately once `devServerUrl` is populated or the pane hides/exits.
   */
  $effect(() => {
    const canProbe =
      Boolean(onscreen) &&
      !exited &&
      !devServerUrl &&
      Boolean(startedIn) &&
      Boolean(nativeSessionId) &&
      $hostPlatform.browser_sessions;
    if (!canProbe) return;
    const sessionId = nativeSessionId;
    return untrack(() => {
      let cancelled = false;
      let inFlight = false;
      const stopTimer = createAdaptiveTimer(() => {
        if (cancelled || disposed || exited || devServerUrl || inFlight || nativeSessionId !== sessionId) return;
        inFlight = true;
        void invoke<number[]>("cmd_terminal_listening_ports", { sessionId })
          .then((ports) => {
            if (cancelled || disposed || exited || devServerUrl || nativeSessionId !== sessionId) return;
            if (Array.isArray(ports) && ports.length > 0 && ports[0] > 0) {
              devServerUrl = `http://localhost:${ports[0]}`;
            }
          })
          .catch(() => {})
          .finally(() => {
            inFlight = false;
          });
      }, 3_000);
      return () => {
        cancelled = true;
        stopTimer();
      };
    });
  });

  /**
   * An ended session nobody is looking at does not keep its scrollback.
   * The last screen stays in the viewport. A plain-text tail is written back
   * only when that viewport is empty, so it is not appended onto the screen
   * the trim already kept. A live session, hidden or not, is not touched.
   * The release restores the scrollback setting once the trim is done; it is
   * read untracked, so a settings change does not re-run the release.
   */
  let endedReleased = false;
  let endedReplayed = false;
  $effect(() => {
    const release = shouldReleaseEndedBuffer(exited ? "exited" : "running", onscreen);
    const current = term;
    const id = tabId;
    if (!current) return;
    untrack(() => {
      if (release) {
        // Once per release. Each call is a real trim, and one made after the
        // hidden dock was fitted to a sliver would discard the wrapped rows
        // above that sliver, which reflow back onto the last screen.
        if (endedReleased) return;
        if (serializeAddon) scrollbackSnapshots.capture(id, () => serializeAddon!.serialize());
        const tail = clipEndedTail(retainedOutput());
        if (tail) rememberEndedTail(id, tail);
        endedReleased = true;
        endedReplayed = false;
        releaseEndedScrollback(current, terminalScrollbackFor({ launcher, taskRunId }, $interfaceStore));
        return;
      }
      if (!endedReleased || endedReplayed) return;
      endedReplayed = true;
      endedReleased = false;
      // A restart or resume cleared the screen on purpose; the old tail is not
      // this session's output.
      const visual = scrollbackSnapshots.take(id);
      if (!exited) { forgetEndedTail(id); return; }
      if (retainedOutput().trim()) return;
      if (visual) {
        current.write(visual);
        return;
      }
      const tail = endedTail(id);
      // convertEol is off, and retainedOutput joins lines with a bare \n.
      if (tail) current.write(tail.replace(/\r?\n/g, "\r\n"));
    });
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
      // Every session that has a box, on screen or not active: a split's
      // second half keeps its box when the font changes, so only this refits
      // it. A hidden one is skipped by `fitNow` and fitted when it is shown.
      refitAfterMetricsChange();
    });
  });

  /**
   * A window moved to a display with another pixel ratio keeps its CSS box,
   * so the resize observer stays silent while xterm rebuilds its cell metrics
   * and glyph atlas underneath an unchanged grid. Measure again, then clear
   * the atlas (a no-op on the DOM renderer) so no glyph is drawn from the old
   * display's raster.
   */
  $effect(() => {
    // A session with no box has nothing to redraw; the atlas call full-refreshes.
    const stop = watchDevicePixelRatio(() => fits.settle(() => { if (hasRenderedBox(container)) term?.clearTextureAtlas(); }));
    return () => {
      stop();
      fits.cancel();
    };
  });

  /** Theme flips re-resolve the palette from CSS variables. */
  $effect(() => {
    $themeStore;
    applyTheme();
  });

  /**
   * Nothing else moves focus into the bypass prompt, so a keyboard user would
   * keep typing into a grid with no PTY behind it. Only the tab being looked
   * at takes it: a background dock tab can wait on the same prompt. This is
   * the moment the prompt first appears; every later return to the tab goes
   * through `reveal()`, which makes the same choice.
   */
  $effect(() => {
    if (!awaitingAcknowledgement || !active) return;
    void tick().then(() => {
      if (awaitingAcknowledgement && active) declineButton?.focus({ preventScroll: true });
    });
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

  /**
   * WebGL while attended: the same `onscreen` the notifier is told. A hidden
   * tab, a collapsed dock or another repository's dock keeps its warm context
   * only through a bounded hold and gives it up when another session needs the
   * slot; turning the setting off releases it at once. See
   * `terminal/gpuRenderer.ts` for the cap, the hold and the loss budget.
   */
  $effect(() => {
    const enabled = $interfaceStore.terminalGpu;
    const want = opened && onscreen && enabled;
    untrack(() => gpu?.sync(want, enabled));
  });
</script>

<div class="h-full w-full flex flex-col min-h-0 min-w-0">
  <!-- Mounted from the start, so the text that lands in it later is announced;
       a status inserted together with its words is mostly left unread. The
       visible row below is hidden from assistive technology to match. -->
  <span class="sr-only" role="status" aria-live="polite" data-terminal-announcer>{statusAnnouncement}</span>
  {#if warning}
    <div role="status" class="shrink-0 flex gap-2 items-center text-meta text-warn px-3 py-1">
      <span class="flex-1 min-w-0 truncate" title={warning}>{warning}</span>
      <button type="button" aria-label="Dismiss terminal message" class="gp-icon-btn" onclick={() => (warning = null)}><X size={12} /></button>
    </div>
  {/if}
  {#if slowInput}
    <div role="status" class="shrink-0 text-meta text-warn px-3 py-1 truncate" title={slowInput}>{slowInput}</div>
  {/if}
  {#if findOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: Enter/Escape bubble from the input and search buttons, retaining their ordinary focus order. -->
    <div class="flex items-center gap-1.5 px-3 h-9 shrink-0 bg-surface" role="search" aria-label="Find in terminal" onkeydown={handleFindKey}>
      <Search size={13} class="text-textMuted shrink-0" />
      <input bind:this={findInput} bind:value={query} maxlength={SEARCH_QUERY_LIMIT} aria-label="Find in terminal output" placeholder="Find in terminal…" class="min-w-0 w-48 flex-1 bg-transparent text-textPrimary text-xs outline-none" />
      <span role="status" class="text-micro text-textMuted whitespace-nowrap tabular-nums">{query ? terminalSearchSummary(resultIndex, resultCount) : ""}</span>
      <button type="button" class="gp-icon-btn text-meta!" class:text-accent={caseSensitive} aria-label="Match case" aria-pressed={caseSensitive} title="Match case" onclick={() => (caseSensitive = !caseSensitive)}>Aa</button>
      <button type="button" class="gp-icon-btn" aria-label="Previous match" title="Previous match (Shift+Enter)" disabled={!query} onclick={() => runFind(true)}><ChevronUp size={13} /></button>
      <button type="button" class="gp-icon-btn" aria-label="Next match" title="Next match (Enter)" disabled={!query} onclick={() => runFind()}><ChevronDown size={13} /></button>
      <button type="button" class="gp-icon-btn" aria-label="Close find" title="Close find (Esc)" onclick={closeFind}><X size={13} /></button>
    </div>
  {/if}
  <div class="flex-1 min-h-0 relative p-2">
    <!-- `isolate`: xterm stacks its own layers with z-indexes (WebGL's
         full-size link canvas is z-index 2). Contained here, none of them can
         rise above the overlays below; uncontained, that canvas took every
         click meant for "Start this session". -->
    <div
      bind:this={container}
      class="h-full w-full bg-surface overflow-hidden isolate"
      data-terminal-session
      data-renderer={renderer}
    ></div>
    {#if scrolledBack}
      <button type="button" class="gp-btn absolute bottom-3 right-5 text-meta! shadow-lg" data-size="sm" onclick={scrollToLatest}><ArrowDownToLine size={12} /> Latest output</button>
    {/if}
    {#if awaitingAcknowledgement}
      <!-- Covers the grid rather than sitting beside it: nothing has spawned,
           and an empty terminal next to a notice reads as a session that
           started and printed nothing. -->
      <div
        data-terminal-acknowledge
        class="absolute inset-2 bg-surface/95 flex flex-col items-center justify-center gap-2 px-6 text-center"
        role="alertdialog"
        aria-labelledby="gp-bypass-title-{tabId}"
        aria-describedby="gp-bypass-body-{tabId}"
      >
        <AlertTriangle size={18} class="text-warn shrink-0" aria-hidden="true" />
        <span id="gp-bypass-title-{tabId}" class="text-textPrimary text-xs font-medium">
          Start {launcherLabel(launcher)} with no permission checks?
        </span>
        <p id="gp-bypass-body-{tabId}" class="text-textMuted text-meta leading-snug max-w-sm">
          Your saved default for {launcherLabel(launcher)} is
          <span class="text-textPrimary">{PERMISSION_LABELS.bypass.label}</span>. This session will
          run without permission prompts and without a sandbox, in
          <span class="font-mono">{workPath.split(/[\\/]/).pop()}</span>. GitPulse asks every time
          rather than remembering the answer.
        </p>
        <div class="flex items-center gap-2 mt-1">
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="bypass-acknowledge"
            onclick={acknowledgeBypass}
          >
            Start this session
          </button>
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="bypass-decline"
            bind:this={declineButton}
            onclick={declineBypass}
          >
            Use {PERMISSION_LABELS.ask.label.toLowerCase()} instead
          </button>
        </div>
      </div>
    {/if}
  </div>
  {#if error || exited || spawning || shellPath || hoveredLink || renderer === "reset" || renderer === "lost"}
    <!-- One fixed-height status row: spawn/error/exited/info content swaps
         inside it, so the terminal's box never resizes (and the
         ResizeObserver never refits) merely because the text rotated. -->
    <!-- An error is the one row that must show its whole message, so it lets
         the height go (inline style beats h-8); every other row stays fixed. -->
    <div
      data-terminal-status
      class="shrink-0 min-w-0 bg-surface/60 flex items-center gap-2 px-4 h-8"
      style:height={error && !spawning ? "auto" : undefined}
      style:min-height={error && !spawning ? "2rem" : undefined}
      style:padding-block={error && !spawning ? "0.25rem" : undefined}
    >
      {#if spawning}
        <LoaderCircle size={13} class="animate-spin text-accent shrink-0" />
        <span aria-hidden="true" class="text-textMuted text-meta">{startingLabel}</span>
      {:else if error}
        <AlertCircle size={13} class="text-danger shrink-0" />
        <span role="alert" class="text-danger flex-1 min-w-0 break-words whitespace-pre-wrap max-h-24 overflow-y-auto text-meta">{error}</span>
        <button type="button" class="gp-btn" data-size="sm" onclick={restart}>
          <RotateCw size={12} /> {taskRunId ? "Reconnect run" : "Try again"}
        </button>
      {:else if exited}
        <span aria-hidden="true" class="text-textMuted flex-1 text-meta">{endedLabel}</span>
        {#if linkedRunId}
          <!-- The task details are where a new run starts, so the way
               there is here rather than only named in a tooltip. -->
          <button type="button" class="gp-btn" data-size="sm" data-testid="terminal-open-task" onclick={() => void openOwnTask()}>
            Open task
          </button>
        {/if}
        {#if transcriptNote}
          <span class="text-textMuted text-meta" data-testid="terminal-resume-unavailable">{transcriptNote}</span>
        {/if}
        {#if resumeOffer}
          <button type="button" class="gp-btn" data-size="sm" data-testid="terminal-resume-claude" onclick={() => void resumeEnded()} title={launcher === "agy" ? (resumeOffer === AGY_CONTINUE_SENTINEL ? "Resume Antigravity's latest conversation in this workspace with --continue" : "Resume this Antigravity conversation with --conversation, in the same directory and permission mode") : "Resume this Claude Code conversation with --resume, in the same directory and permission mode"}>
            <RotateCw size={12} /> Resume
          </button>
        {/if}
        <button type="button" class="gp-btn" data-size="sm" onclick={restart} disabled={!!taskRunId} title={taskRunId ? "Start a new run from the task details." : "Restart this terminal"}>
          <RotateCw size={12} /> Restart
        </button>
      {:else if hoveredLink}
        <!-- Same row, so revealing a link target never resizes the grid. -->
        <ExternalLink size={11} class="text-accent shrink-0" aria-hidden="true" />
        <span class="text-micro text-textMuted truncate" title={hoveredLink}>{hoveredLink}</span>
      {:else}
        {@const liveSubdir = activeCwdSubdir ?? startDir ?? ""}
        <span class="w-1.5 h-1.5 rounded-full {lastExitCode !== null && lastExitCode !== 0 ? 'bg-danger' : 'bg-ok'} shrink-0" aria-hidden="true"></span>
        <span class="text-micro text-textMuted font-mono truncate" title={`${shellPath} · Working directory ${liveSubdir ? `${workPath}/${liveSubdir}` : (startedIn || workPath)}`}>{shellPath.split(/[\\/]/).pop()} · {workPath.split(/[\\/]/).pop()}{liveSubdir ? `/${liveSubdir}` : ""}</span>
        {#if lastExitCode !== null && lastExitCode !== 0}
          <span class="text-micro font-mono text-danger shrink-0" title="Last command exit status">exit {lastExitCode}</span>
        {/if}
        {#if devServerUrl && devServerUrl !== dismissedDevServerUrl && $hostPlatform.browser_sessions}
          {@const previewPort = (() => { try { return new URL(devServerUrl).port; } catch { return ""; } })()}
          <span class="inline-flex items-center gap-0.5 shrink-0">
            <button
              type="button"
              class="gp-btn py-0! px-1.5! text-micro! text-accent border-accent/35 bg-accent/10 flex items-center gap-1"
              data-testid="terminal-dev-server-preview"
              title={`Open ${devServerUrl} in a split browser session`}
              onclick={() => devServerUrl && void terminalLaunchRequests.requestBrowser(repoPath, devServerUrl)}
            >
              <ExternalLink size={10} />
              <span>Preview {previewPort ? `:${previewPort}` : "app"}</span>
            </button>
            <button
              type="button"
              class="gp-icon-btn p-0.5!"
              aria-label="Dismiss preview suggestion"
              title="Dismiss preview suggestion"
              onclick={() => { dismissedDevServerUrl = devServerUrl; }}
            >
              <X size={10} />
            </button>
          </span>
        {/if}
        {#if agentArtifactDir}
          <button
            type="button"
            class="gp-btn py-0! px-1.5! text-micro! text-accent border-accent/35 bg-accent/10 flex items-center gap-1 shrink-0"
            data-testid="terminal-open-agent-plan"
            title="Open this agent's active Markdown plan or artifact beside the terminal"
            onclick={() => void openAgentArtifact()}
          >
            <FileText size={10} />
            <span>Open plan</span>
          </button>
        {/if}
      {/if}
      {#if renderer === "reset" || renderer === "lost"}
        <!-- A lost WebGL context is not blank (the DOM renderer is already
             drawing the same rows), but it is slower; say why. -->
        <span
          data-terminal-gpu-note
          class="ml-auto shrink-0 text-micro text-warn whitespace-nowrap"
          title={renderer === "reset"
            ? "The graphics context was lost, for example by a GPU reset or waking from sleep. This terminal draws without the GPU and will try again."
            : "This terminal draws without the GPU: its graphics context was lost repeatedly or couldn't be created. Reopening the tab tries again."}
        >{renderer === "reset" ? "Drawing without GPU after a graphics reset" : "Drawing without GPU"}</span>
      {/if}
      <div class="{renderer === 'reset' || renderer === 'lost' ? '' : 'ml-auto '}flex items-center gap-1 shrink-0" role="group" aria-label="Terminal text size">

        <button type="button" class="gp-icon-btn" data-size="sm" aria-label="Decrease terminal text size" title="Smaller text ({platformChord('⌘−', 'Ctrl+Shift+−', $hostPlatform.os)})" disabled={fontSize <= TERMINAL_FONT_MIN} onclick={() => setFontSize(fontSize - 1)}><Minus size={12} /></button>
        <button type="button" class="text-micro text-textMuted tabular-nums px-1" aria-label="Reset terminal text size" title="Reset text size ({platformChord('⌘0', 'Ctrl+Shift+0', $hostPlatform.os)})" onclick={() => setFontSize(TERMINAL_FONT_DEFAULT)}>{fontSize}px</button>
        <button type="button" class="gp-icon-btn" data-size="sm" aria-label="Increase terminal text size" title="Larger text ({platformChord('⌘+', 'Ctrl+Shift++', $hostPlatform.os)})" disabled={fontSize >= TERMINAL_FONT_MAX} onclick={() => setFontSize(fontSize + 1)}><Plus size={12} /></button>
      </div>
    </div>
  {/if}
</div>
