/**
 * The terminal's tab model.
 *
 * Pure on purpose: every rule that decides which shell survives a close, what
 * a tab is called, and when a new one may be opened is a rule worth testing
 * without a PTY, a webview, or an xterm instance behind it.
 */

import { currentSessionLimit, MAX_TERMINAL_SESSIONS } from "./sessionLimit";
import { isImeComposition } from "../keyboard/imeGuard";
import { clean } from "./sessionActivity";
import type { PermissionMode } from "./agentDefaults";

/** What a tab runs. `shell` is the user's own login shell; the rest are agent CLIs. */
export type LauncherKind = "shell" | "claude" | "manvi" | "codex" | "grok" | "agy";

export const LAUNCHERS: readonly { kind: LauncherKind; label: string }[] = [
  { kind: "shell", label: "Shell" },
  { kind: "claude", label: "Claude" },
  { kind: "manvi", label: "Manvi" },
  { kind: "codex", label: "Codex" },
  { kind: "grok", label: "Grok" },
  { kind: "agy", label: "Antigravity" },
];

/**
 * What a tab of the terminal dock can be: a process ({@link LauncherKind}) or
 * a browser session, which shows a local dev server's page in a native view
 * (`BrowserSession.svelte`) and runs no process. `LauncherKind` stays the
 * process-only type, since settings, agent defaults and the session registry
 * are all about processes.
 */
export type SessionKind = LauncherKind | "browser";

/**
 * Any pane kind in the terminal dock: a process, a browser view, or an
 * integrated file/code/Markdown split (`TerminalFilePane.svelte`).
 */
export type TabKind = SessionKind | "file";

/** Every session type, in the order the new-session menu lists them. */
export const SESSION_TYPES: readonly { kind: SessionKind; label: string }[] = [
  ...LAUNCHERS,
  { kind: "browser", label: "Browser" },
];

/**
 * The session types this host can open. `browserSessions` is the backend's
 * capability report (`HostPlatform.browser_sessions`), never a guess from the
 * OS name.
 */
export function sessionTypesFor(browserSessions: boolean): readonly { kind: SessionKind; label: string }[] {
  return browserSessions ? SESSION_TYPES : LAUNCHERS;
}

/** A stored or requested kind this host can open, or `shell`. */
export function offeredSessionKind(kind: unknown, browserSessions: boolean): SessionKind {
  return sessionTypesFor(browserSessions).find((type) => type.kind === kind)?.kind ?? "shell";
}

export interface TerminalTab {
  id: string;
  launcher: TabKind;
  /**
   * What the running program last called itself (an OSC 0/2 title), or null
   * while it has said nothing. Null is not "no title" — it is "nothing has
   * been reported yet", and {@link tabLabel} is what turns either into text.
   */
  title: string | null;
  name?: string;
  /** Supplied as a literal agent CLI argument when this tab starts. */
  initialPrompt?: string;
  /**
   * Repository-relative directory this tab's process starts in, copied from
   * the session it was opened beside. Absent means the repository root.
   */
  startDir?: string;
  /**
   * The checkout this tab's process runs in, when that is not the panel's
   * own: an agent hosted in another checkout of the same repository (see
   * `taskLaunches.hostTabFor`). Absent means the panel's repository.
   */
  checkout?: string;
  taskRunId?: string;
  /** Set when this tab picks an ended attempt's Claude Code conversation back up. */
  resume?: ResumeLaunch;
  /**
   * A still-running session a reloaded page left behind, which this tab
   * takes over (`cmd_terminal_attach`) instead of starting a process.
   */
  attachSessionId?: string;
  /**
   * Parent backend PTY session ID when this tab is an Option A child session
   * spawned via `gitpulse_terminal_spawn` / `gitpulse-hook term spawn`.
   */
  parentSessionId?: string;
  /**
   * A browser tab's address: the one it was opened on, then whatever its page
   * last loaded (`setTabUrl`). What an agent's or a link's request for the
   * same address finds (`browserTabFor`) instead of opening another tab.
   */
  url?: string;
  /** Repository-relative path open inside a `file` pane (`TerminalFilePane.svelte`). */
  filePath?: string;
  /** Repo-root fallback path when a subdirectory link could also refer to the root (`W6.5-8`). */
  fallbackPath?: string;
  /** 1-based line requested for a `file` pane reveal. */
  revealLine?: number | null;
  /** 1-based column requested for a `file` pane reveal. */
  revealColumn?: number | null;
  /** Monotonic bump so clicking the same `path:line` twice re-reveals it. */
  revealSeq?: number;
  /** Read-only virtual command-output snippet rendered in `TerminalFilePane.svelte` without dirtying Git. */
  snippet?: { title: string; content: string };
  /** Pre-arms quiescent peer auto-wake on unattended kickoff sessions. */
  autoWake?: boolean;
}

/**
 * An ended attempt's Claude Code conversation, picked up again with
 * `claude --resume <sessionId>`.
 *
 * It carries the attempt's own permission mode because a resumed conversation
 * is the same work continuing: under the host-wide default, an attempt that
 * was only allowed to inspect would come back able to edit.
 */
export interface ResumeLaunch {
  sessionId: string;
  mode: PermissionMode;
  /**
   * The ended attempt this conversation continues. Linkage only, so the
   * task's Agents pane can list the session under its attempt; a resumed tab
   * is the reader's own and never acts on that attempt's record.
   */
  runId?: string;
  /**
   * A first message for the resumed conversation, sent as the prompt of
   * `claude --resume <id> -- <prompt>` (`resumeArgs`). The first start only: a
   * restart of the tab resumes without it, or it would be said twice.
   */
  prompt?: string;
}

/**
 * A task attempt's terminal; with `resume`, its conversation resumed; with
 * `attach`, a session a reloaded page left running, shown again.
 */
export interface TaskLaunch {
  runId: string;
  title: string;
  resume?: ResumeLaunch;
  attach?: { sessionId: string };
  /**
   * Checkout-relative directory the work ran in, when that is below the
   * checkout's root. A resumed Claude Code conversation is kept under the
   * directory it ran in, so it has to start there to be found.
   */
  startDir?: string;
  /** See `TerminalTab.checkout`; `startDir` is relative to it when set. */
  checkout?: string;
}

/**
 * A checkout-relative start directory the host will accept, or null for the
 * root: one that climbs or is absolute is refused there, so it is not
 * offered here either.
 */
export function relativeStartDir(dir: string | null | undefined): string | null {
  if (!dir || dir === ".") return null;
  if (/^([\\/]|[A-Za-z]:)/.test(dir) || dir.split(/[\\/]/).includes("..")) return null;
  return dir;
}

const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The only session id shape Claude Code's `--resume` and `--session-id` take. */
export function isSessionId(value: string): boolean { return CANONICAL_UUID.test(value); }

/** The tab already showing this launch, if one is. */
export function tabFor(state: TabState, launch: TaskLaunch): TerminalTab | undefined {
  const { resume, attach } = launch;
  if (attach) return state.tabs.find((tab) => tab.attachSessionId === attach.sessionId);
  return resume
    ? state.tabs.find((tab) => tab.resume?.sessionId === resume.sessionId)
    : state.tabs.find((tab) => tab.taskRunId === launch.runId);
}

export interface TabState {
  tabs: TerminalTab[];
  activeId: string | null;
}

export function launcherLabel(kind: TabKind): string {
  if (kind === "file") return "Files";
  return SESSION_TYPES.find((l) => l.kind === kind)?.label ?? kind;
}

/**
 * The text on a tab: what the program calls itself, else what it was started
 * as. A shell-reported title is trimmed and clipped — a program may emit an
 * arbitrarily long one, and a tab strip is not the place to discover that.
 */
export function tabLabel(tab: TerminalTab): string {
  const fileFallback =
    tab.launcher === "file"
      ? tab.filePath
        ? (tab.filePath.split(/[\\/]/).pop() ?? "")
        : (tab.snippet?.title ?? "")
      : "";
  const reported = tab.name?.trim() || tab.title?.trim() || fileFallback;
  if (!reported) return launcherLabel(tab.launcher);
  return reported.length > 28 ? `${reported.slice(0, 27)}…` : reported;
}

let sequence = 0;

/** Ids are per-webview and only ever compared to each other. */
function nextTabId(): string {
  sequence += 1;
  return `tab-${sequence}`;
}

export function createTab(launcher: TabKind, initialPrompt?: string): TerminalTab {
  return { id: nextTabId(), launcher, title: null, ...(initialPrompt === undefined ? {} : { initialPrompt }) };
}

export function initialState(launcher: SessionKind = "shell", task?: TaskLaunch): TabState {
  return openTab({ tabs: [], activeId: null }, launcher, task);
}

/**
 * Whether one more tab fits under `limit` tabs. A reactive caller passes
 * `$terminalSessionLimit` so it re-evaluates when the user changes the limit.
 */
export function canOpenTab(state: TabState, limit: number = currentSessionLimit()): boolean {
  return state.tabs.length < limit;
}

/**
 * The most tabs one repository's strip holds, live or ended.
 *
 * Not the user's session limit: that bounds live processes, and the session
 * registry enforces it across every repository. A strip bounded by it too
 * counted ended tabs as live ones, so once a repository's agents had run and
 * finished as many times as the limit, no agent could start there — under a
 * banner saying every session was in use while most slots stood free. Agents
 * in worktrees share their repository's strip (`taskLaunches.hostTabFor`), so
 * that was the common case, not an edge. This bounds the strip's memory only.
 */
export const MAX_STRIP_TABS = MAX_TERMINAL_SESSIONS;

/** What the panel knows about where a pane stands. */
export interface PaneVisibility {
  /** The dock itself is open. */
  visible: boolean;
  /** The panel is showing terminals rather than the console. */
  mode: string;
  /** The two tabs a split is showing, or null/undefined when not split. */
  splitIds?: readonly string[] | null;
  activeId: string | null;
  tabId: string;
}

/**
 * Whether the user can actually see this tab's terminal.
 *
 * Three things used to answer this separately in the panel — the `hidden`
 * class, the unread-dot guard, and nothing at all for notifications — and they
 * did not agree: the unread guard consulted `splitIds` without a fallback, so
 * outside split view it relied on the caller having already checked. That was
 * true and undocumented, which is the shape a bug takes when someone adds a
 * third caller.
 *
 * Being the *selected* tab is not the same as being visible. A selected tab in
 * a collapsed dock is on nobody's screen, and treating it as watched is
 * exactly how a waiting agent goes unannounced.
 */
export function paneOnScreen({ visible, mode, splitIds, activeId, tabId }: PaneVisibility): boolean {
  if (!visible || mode !== "shell") return false;
  return splitIds ? splitIds.includes(tabId) : tabId === activeId;
}

/**
 * Opens a tab and focuses it, or returns the state unchanged at the ceiling.
 *
 * Silent refusal is the caller's cue to have disabled the control already;
 * this returning the same object is what makes "nothing happened" checkable.
 */
export function openTab(state: TabState, launcher: TabKind, launch?: string | TaskLaunch): TabState {
  // Non-process panes run no process, so they have nothing to hand a prompt or
  // a task attempt to.
  if ((launcher === "browser" || launcher === "file") && launch !== undefined) return state;
  const task = typeof launch === "object" ? launch : undefined;
  if (task) {
    const existing = tabFor(state, task);
    if (existing) return { ...state, activeId: existing.id };
  }
  if (!canOpenTab(state, MAX_STRIP_TABS)) return state;
  const tab = createTab(launcher, typeof launch === "string" ? launch : undefined);
  if (task?.attach) {
    if (task.resume || !task.attach.sessionId) return state;
    tab.attachSessionId = task.attach.sessionId;
    tab.name = task.title;
  } else if (task?.resume) {
    // A plain agent tab, not the attempt's: the attempt ended and its
    // record is final, so this session is the reader's own continuation.
    if ((launcher !== "claude" && launcher !== "agy") || !isSessionId(task.resume.sessionId)) return state;
    tab.resume = { ...task.resume, runId: task.runId };
    tab.name = `${task.title} (resumed)`;
  } else if (task) { tab.taskRunId = task.runId; tab.name = task.title; }
  // A taken-over session is already running wherever it runs.
  const startDir = task && !task.attach ? relativeStartDir(task.startDir) : null;
  if (startDir) tab.startDir = startDir;
  if (task && !task.attach && task.checkout) tab.checkout = task.checkout;
  return { tabs: [...state.tabs, tab], activeId: tab.id };
}

/**
 * Adopts an already-spawned child PTY (`terminal-child-spawned`) into the tab
 * strip via `attachSessionId` so `TerminalSession` attaches to the running
 * process instead of starting a second one.
 *
 * Idempotent on `sessionId`, bounded by `MAX_STRIP_TABS`, and leaves active
 * focus on the caller's current tab unless `focus` is true or the strip is empty.
 */
export function openAttachedChildTab(
  state: TabState,
  options: {
    sessionId: string;
    parentSessionId?: string | null;
    launcher?: string | null;
    title?: string | null;
    startDir?: string | null;
    focus?: boolean;
  },
): { state: TabState; id: string | null; opened: boolean } {
  const sessionId = options.sessionId.trim();
  if (!sessionId) return { state, id: null, opened: false };
  const parentId = options.parentSessionId?.trim() || undefined;
  const existing = state.tabs.find((tab) => tab.attachSessionId === sessionId);
  if (existing) {
    const updatedTabs =
      parentId && existing.parentSessionId !== parentId
        ? state.tabs.map((tab) =>
            tab.id === existing.id ? { ...tab, parentSessionId: parentId } : tab,
          )
        : state.tabs;
    const nextBase = updatedTabs === state.tabs ? state : { ...state, tabs: updatedTabs };
    return {
      state: options.focus ? activateTab(nextBase, existing.id) : nextBase,
      id: existing.id,
      opened: false,
    };
  }
  if (!canOpenTab(state, MAX_STRIP_TABS)) {
    return { state, id: null, opened: false };
  }
  const kind: LauncherKind =
    LAUNCHERS.find((candidate) => candidate.kind === options.launcher)?.kind ?? "shell";
  const tab = createTab(kind);
  tab.attachSessionId = sessionId;
  if (parentId) tab.parentSessionId = parentId;
  if (options.title) {
    const cleaned = cleanTabName(options.title);
    if (cleaned) tab.name = cleaned;
  }
  const relDir = relativeStartDir(options.startDir);
  if (relDir) tab.startDir = relDir;
  return {
    state: {
      tabs: [...state.tabs, tab],
      activeId: options.focus || !state.activeId ? tab.id : state.activeId,
    },
    id: tab.id,
    opened: true,
  };
}

/**
 * Returns live child tabs owned by `parentSessionId` (`status !== "exited"`).
 */
export function childTabsForParent(
  state: TabState,
  parentSessionId: string | null | undefined,
  statuses?: Readonly<Record<string, string | undefined>>,
): TerminalTab[] {
  const pid = parentSessionId?.trim();
  if (!pid) return [];
  return state.tabs.filter(
    (tab) => tab.parentSessionId === pid && statuses?.[tab.id] !== "exited",
  );
}

const ANSI_CSI_OR_OSC =
  // eslint-disable-next-line no-control-regex
  /\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])/g;

/**
 * Builds a bounded, ANSI-stripped handoff prompt for 1-click context transfer
 * between paired Claude Code and Antigravity split panes.
 */
export function buildPairHandoffPrompt(options: {
  fromLauncher: TabKind;
  checkoutBranch?: string | null;
  openFilePath?: string | null;
  openFileLine?: number | null;
  terminalTail?: string | null;
}): string {
  const sender = launcherLabel(options.fromLauncher);
  const branchTag = options.checkoutBranch?.trim() ? ` (${options.checkoutBranch.trim()})` : "";
  const parts: string[] = [`[Handoff from ${sender}${branchTag}]`];
  const filePath = options.openFilePath?.trim();
  if (filePath) {
    const lineRef =
      typeof options.openFileLine === "number" && options.openFileLine > 0
        ? `#L${options.openFileLine}`
        : "";
    parts.push(`Context file: @${filePath}${lineRef}`);
  }
  const cleanedTail = (options.terminalTail ?? "")
    .replace(ANSI_CSI_OR_OSC, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.trim().length > 0);
  if (cleanedTail.length > 0) {
    let tailText = cleanedTail.slice(-20).join("\n");
    if (tailText.length > 1600) {
      tailText = `…${tailText.slice(tailText.length - 1600)}`;
    }
    parts.push(`Recent terminal context:\n\`\`\`\n${tailText}\n\`\`\``);
  }
  return `${parts.join("\n")}\n`;
}

/**
 * Normalizes a user-supplied worktree slug into a safe `[a-z0-9._-]` segment,
 * or returns `null` when empty or unsafe (`..`).
 */
export function sanitizeWorktreeSlug(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const cleaned = raw
    .trim()
    .toLowerCase()
    .replace(/[\s/\\]+/g, "-")
    .replace(/[^a-z0-9._-]/g, "")
    .replace(/-{2,}/g, "-")
    .replace(/^[.-]+|[.-]+$/g, "")
    .slice(0, 48)
    .replace(/[.-]+$/g, "");
  if (!cleaned || cleaned.includes("..")) return null;
  return cleaned;
}

/**
 * Opens an interactive process tab bound to an explicit worktree `checkout`
 * without fabricating a `taskRunId`.
 */
export function openCheckoutTab(
  state: TabState,
  launcher: LauncherKind,
  options: {
    checkout: string;
    name?: string | null;
    startDir?: string | null;
    initialPrompt?: string;
  },
): { state: TabState; id: string | null } {
  const checkout = options.checkout.trim();
  if (!checkout || !canOpenTab(state, MAX_STRIP_TABS)) {
    return { state, id: null };
  }
  const tab = createTab(launcher, options.initialPrompt);
  tab.checkout = checkout;
  if (options.name) {
    const cleaned = cleanTabName(options.name);
    if (cleaned) tab.name = cleaned;
  }
  const relDir = relativeStartDir(options.startDir);
  if (relDir) tab.startDir = relDir;
  return {
    state: { tabs: [...state.tabs, tab], activeId: tab.id },
    id: tab.id,
  };
}

/**
 * Closes a tab and picks the next focus.
 *
 * The neighbour rule is the one every tabbed terminal uses: focus moves to the
 * tab on the right, or to the left when the closed tab was last — so closing
 * repeatedly walks a direction instead of jumping to an edge. Closing an
 * inactive tab never moves focus.
 *
 * Closing the last tab empties the strip rather than auto-opening a
 * replacement: the caller owns whether "no tabs" means "closed the dock" or
 * "start a fresh shell", and inventing a session here would spawn a process
 * nobody asked for.
 */
export function closeTab(state: TabState, id: string, prefer?: string | null): TabState {
  const index = state.tabs.findIndex((tab) => tab.id === id);
  if (index === -1) return state;
  const tabs = state.tabs.filter((tab) => tab.id !== id);
  if (state.activeId !== id) return { tabs, activeId: state.activeId };
  // Closing one half of a split leaves the other half on screen; showing a
  // neighbour instead would swap the terminal the user was just looking at.
  const preferred = prefer ? tabs.find((tab) => tab.id === prefer) : undefined;
  const next = preferred ?? tabs[index] ?? tabs[index - 1] ?? null;
  return { tabs, activeId: next?.id ?? null };
}

/** Pins where a tab's process starts. Only before it spawns does this mean anything. */
export function setTabStartDir(state: TabState, id: string, startDir: string | null): TabState {
  if (startDir === null || !state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, startDir } : tab)) };
}

/** Pre-arms quiescent peer auto-wake on an unattended kickoff tab. */
export function setTabAutoWake(state: TabState, id: string, autoWake: boolean): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, autoWake } : tab)) };
}

export function activateTab(state: TabState, id: string): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, activeId: id };
}

/** Records a title the running program reported for one tab. */
/** One address spelled two ways (`localhost:3000` and `localhost:3000/`) is one address. */
function sameAddress(a: string, b: string): boolean {
  try {
    return new URL(a).href === new URL(b).href;
  } catch {
    return a === b;
  }
}

/** The longest address a tab keeps; the backend loads no longer one. */
const MAX_TAB_URL = 2048;

/**
 * Opens a browser tab on `url` — or, when a browser tab is already on that
 * address, brings it forward instead (`opened: false`), so a request repeated
 * in an edit-reload loop reuses its session rather than piling up tabs.
 */
export function openBrowserTab(state: TabState, url?: string): { state: TabState; id: string | null; opened: boolean } {
  const address = url?.trim().slice(0, MAX_TAB_URL) || undefined;
  if (address) {
    const existing = browserTabFor(state, address);
    if (existing) return { state: activateTab(state, existing.id), id: existing.id, opened: false };
  }
  const next = openTab(state, "browser");
  const added = next.tabs.find((tab) => !state.tabs.includes(tab));
  if (!added) return { state, id: null, opened: false };
  if (!address) return { state: next, id: added.id, opened: true };
  return { state: setTabUrl(next, added.id, address), id: added.id, opened: true };
}

/** The browser tab on `url`, if one is. */
export function browserTabFor(state: TabState, url: string): TerminalTab | undefined {
  return state.tabs.find((tab) => tab.launcher === "browser" && tab.url !== undefined && sameAddress(tab.url, url));
}

/** Records the address a browser tab's page is on. */
export function setTabUrl(state: TabState, id: string, url: string): TabState {
  const tab = state.tabs.find((candidate) => candidate.id === id);
  if (!tab || tab.launcher !== "browser") return state;
  const address = url.slice(0, MAX_TAB_URL);
  if (tab.url === address) return state;
  return { ...state, tabs: state.tabs.map((candidate) => (candidate.id === id ? { ...candidate, url: address } : candidate)) };
}

const MAX_TAB_FILE_PATH = 4096;

/** Normalizes and validates a repository-relative file path for a `file` pane. */
export function normalizePaneFilePath(raw: string | null | undefined): string | undefined {
  if (!raw) return undefined;
  const trimmed = raw.trim().replace(/\\/g, "/").replace(/^(?:\.\/)+/, "");
  if (!trimmed || trimmed.length > MAX_TAB_FILE_PATH || trimmed.includes("\0")) return undefined;
  if (trimmed.startsWith("/") || /^[A-Za-z]:/.test(trimmed)) return undefined;
  if (trimmed.split("/").some((part) => part === ".." || part === "")) return undefined;
  return trimmed;
}

/** Finds an existing `file` tab in the same checkout (matching `filePath` when supplied, or the first `file` tab in that checkout). */
export function fileTabFor(
  state: TabState,
  filePath?: string,
  checkout?: string,
): TerminalTab | undefined {
  const cleanPath = normalizePaneFilePath(filePath);
  const normalizedCheckout = checkout?.trim() || undefined;
  const sameCheckout = (tab: TerminalTab) => (tab.checkout || undefined) === normalizedCheckout;
  if (cleanPath) {
    const exact = state.tabs.find(
      (tab) => tab.launcher === "file" && sameCheckout(tab) && tab.filePath === cleanPath,
    );
    if (exact) return exact;
  }
  return state.tabs.find((tab) => tab.launcher === "file" && sameCheckout(tab));
}

/** Updates a `file` tab's selected path and optional 1-based reveal line/column. */
export function setTabFile(
  state: TabState,
  id: string,
  filePath?: string,
  line: number | null = null,
  column: number | null = null,
  options?: {
    checkout?: string;
    fallbackPath?: string;
    preserveReveal?: boolean;
  },
): TabState {
  const tab = state.tabs.find((candidate) => candidate.id === id);
  if (!tab || tab.launcher !== "file") return state;
  const cleanPath = normalizePaneFilePath(filePath);
  const cleanFallback = normalizePaneFilePath(options?.fallbackPath);
  const effectiveFallback =
    cleanFallback && cleanFallback !== cleanPath ? cleanFallback : undefined;
  const nextCheckout = options?.checkout !== undefined ? (options.checkout.trim() || undefined) : tab.checkout;
  const nextLine = options?.preserveReveal
    ? (tab.revealLine ?? null)
    : (line && line > 0 ? Math.floor(line) : null);
  const nextColumn = options?.preserveReveal
    ? (tab.revealColumn ?? null)
    : (column && column > 0 ? Math.floor(column) : null);
  const nextSeq = options?.preserveReveal ? (tab.revealSeq ?? 1) : (tab.revealSeq ?? 0) + 1;
  return {
    ...state,
    tabs: state.tabs.map((candidate) =>
      candidate.id === id
        ? {
            ...candidate,
            ...(cleanPath !== undefined ? { filePath: cleanPath, snippet: undefined } : {}),
            fallbackPath: effectiveFallback,
            ...(nextCheckout ? { checkout: nextCheckout } : { checkout: undefined }),
            revealLine: nextLine,
            revealColumn: nextColumn,
            revealSeq: nextSeq,
          }
        : candidate,
    ),
  };
}

/**
 * Opens or reuses a `file` pane tab (`TerminalFilePane.svelte`).
 *
 * When `reuseExisting` is true (default), clicking multiple file links in a
 * terminal updates the already-open file split in place rather than piling up
 * tabs; passing `focus: false` leaves keyboard focus on the terminal beside it.
 */
export function openFileTab(
  state: TabState,
  options: {
    filePath?: string;
    fallbackPath?: string;
    checkout?: string;
    line?: number | null;
    column?: number | null;
    reuseExisting?: boolean;
    focus?: boolean;
  } = {},
): { state: TabState; id: string | null; opened: boolean } {
  const {
    filePath,
    fallbackPath,
    checkout,
    line = null,
    column = null,
    reuseExisting = true,
    focus = true,
  } = options;
  const cleanPath = normalizePaneFilePath(filePath);
  const cleanFallback = normalizePaneFilePath(fallbackPath);
  const effectiveFallback =
    cleanFallback && cleanFallback !== cleanPath ? cleanFallback : undefined;
  const cleanCheckout = checkout?.trim() || undefined;
  if (filePath !== undefined && filePath.trim() !== "" && !cleanPath) {
    return { state, id: null, opened: false };
  }
  if (reuseExisting) {
    const existing = fileTabFor(state, cleanPath, cleanCheckout);
    if (existing) {
      const updated = setTabFile(state, existing.id, cleanPath ?? existing.filePath, line, column, {
        checkout: cleanCheckout,
        fallbackPath: effectiveFallback,
      });
      return {
        state: focus ? activateTab(updated, existing.id) : updated,
        id: existing.id,
        opened: false,
      };
    }
  }
  if (!canOpenTab(state, MAX_STRIP_TABS)) return { state, id: null, opened: false };
  const tab = createTab("file");
  if (cleanPath) tab.filePath = cleanPath;
  if (effectiveFallback) tab.fallbackPath = effectiveFallback;
  if (cleanCheckout) tab.checkout = cleanCheckout;
  tab.revealLine = line && line > 0 ? Math.floor(line) : null;
  tab.revealColumn = column && column > 0 ? Math.floor(column) : null;
  tab.revealSeq = 1;
  return {
    state: {
      tabs: [...state.tabs, tab],
      activeId: focus || !state.activeId ? tab.id : state.activeId,
    },
    id: tab.id,
    opened: true,
  };
}

/**
 * Opens or reuses a read-only virtual `file` pane tab showing captured terminal
 * command output (`snippet`), avoiding an existing `file` tab that is currently
 * viewing a disk file so unsaved editor drafts are never displaced.
 */
export function openSnippetTab(
  state: TabState,
  options: {
    title: string;
    content: string;
    reuseExisting?: boolean;
    focus?: boolean;
  },
): { state: TabState; id: string | null; opened: boolean } {
  const title = cleanTabName(options.title, 64) || "Command output";
  const content = options.content.slice(0, 256 * 1024);
  const { reuseExisting = true, focus = false } = options;
  if (reuseExisting) {
    const existing = state.tabs.find(
      (tab) => tab.launcher === "file" && !tab.filePath,
    );
    if (existing) {
      const updated: TabState = {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.id === existing.id
            ? {
                ...tab,
                filePath: undefined,
                snippet: { title, content },
                revealLine: 1,
                revealColumn: null,
                revealSeq: (tab.revealSeq ?? 0) + 1,
              }
            : tab,
        ),
      };
      return {
        state: focus ? activateTab(updated, existing.id) : updated,
        id: existing.id,
        opened: false,
      };
    }
  }
  if (!canOpenTab(state, MAX_STRIP_TABS)) return { state, id: null, opened: false };
  const tab = createTab("file");
  tab.snippet = { title, content };
  tab.revealSeq = 1;
  return {
    state: {
      tabs: [...state.tabs, tab],
      activeId: focus || !state.activeId ? tab.id : state.activeId,
    },
    id: tab.id,
    opened: true,
  };
}

export function setTabTitle(state: TabState, id: string, title: string): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, title: cleanTabName(title, 256) } : tab)),
  };
}

/**
 * Focus one step along the strip, wrapping at both ends.
 *
 * Wrapping matters more than it looks: without it the chord dies silently at
 * whichever end you are already on, which reads as a broken shortcut.
 */
export function cycleTab(state: TabState, step: 1 | -1): TabState {
  if (state.tabs.length < 2) return state;
  const index = state.tabs.findIndex((tab) => tab.id === state.activeId);
  if (index === -1) return state;
  const next = (index + step + state.tabs.length) % state.tabs.length;
  return { ...state, activeId: state.tabs[next].id };
}

/** What a terminal keyboard chord asks the strip to do. */
export type TabChord = "new" | "close" | "next" | "prev" | "ignore";

/**
 * The chord this event is, or null when it is ordinary input.
 *
 * Control rather than Command throughout, matching the dock's own ⌃` toggle:
 * on macOS ⌘` is the OS window cycler and ⌘W would close the app window out
 * from under a running shell. Pure so the bindings are checkable without a
 * webview — the handler that consumes this sits inside xterm's key path, where
 * a wrong answer silently eats a keystroke the shell needed.
 */
export function terminalTabChord(event: {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  repeat?: boolean;
}): TabChord | null {
  if (isImeComposition(event)) return null;
  if (!event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.key === "Tab") return event.shiftKey ? "prev" : "next";
  if (!event.shiftKey) return null;
  if (event.repeat && ["t", "w"].includes(event.key.toLowerCase())) return "ignore";
  // Shift makes `key` the uppercase letter; compare on one case only.
  switch (event.key.toLowerCase()) {
    case "t":
      return "new";
    case "w":
      return "close";
    default:
      return null;
  }
}

/** Arrow navigation belongs to the tab strip, never the shell's input. */
export function terminalTabDestination(state: TabState, key: string): string | null {
  if (!state.tabs.length) return null;
  if (key === "Home") return state.tabs[0].id;
  if (key === "End") return state.tabs[state.tabs.length - 1].id;
  if (key === "ArrowRight") return cycleTab(state, 1).activeId;
  if (key === "ArrowLeft") return cycleTab(state, -1).activeId;
  return null;
}

/**
 * The most raw code units of a title that `cleanTabName` reads. A megabyte
 * title costs one slice of this, not a pass over all of it; hidden characters
 * and blanks are dropped from inside it, so a long run of them ahead of the
 * visible text still leaves room for the text.
 */
const MAX_RAW_TITLE = 65_536;

/** A surrogate with no partner: a high one not followed by a low, or a low not preceded by a high. */
const UNPAIRED_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g;

/**
 * Titles are display data, never terminal control sequences or unbounded
 * state. What counts as hidden is `clean()`'s to say, so a program-set title
 * cannot carry what a notification could not. The result is well-formed
 * UTF-16: an unpaired surrogate (one the raw bound cut in two, or one the
 * program sent) is dropped before `clean()`, since `JSON.stringify` escapes it
 * as `\udXXX` and the backend's JSON reader refuses that.
 */
export function cleanTabName(value: string, limit = 64): string {
  return clean(value.slice(0, MAX_RAW_TITLE).replace(UNPAIRED_SURROGATE, ""), limit) ?? "";
}

export function renameTab(state: TabState, id: string, name: string): TabState {
  return { ...state, tabs: state.tabs.map((tab) => tab.id === id ? { ...tab, name: cleanTabName(name) } : tab) };
}

export function moveTab(state: TabState, id: string, step: -1 | 1): TabState {
  const from = state.tabs.findIndex((tab) => tab.id === id);
  const to = from + step;
  if (from < 0 || to < 0 || to >= state.tabs.length) return state;
  const tabs = [...state.tabs];
  [tabs[from], tabs[to]] = [tabs[to], tabs[from]];
  return { ...state, tabs };
}
