/**
 * The terminal's tab model.
 *
 * Pure on purpose: every rule that decides which shell survives a close, what
 * a tab is called, and when a new one may be opened is a rule worth testing
 * without a PTY, a webview, or an xterm instance behind it.
 */

import { currentSessionLimit, MAX_TERMINAL_SESSIONS } from "./sessionLimit";
import { isImeComposition } from "../keyboard/imeGuard";
import { clean, stripTitleSpinner } from "./sessionActivity";
import type { PermissionMode } from "./agentDefaults";

export { stripTitleSpinner };

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

/**
 * Every tab kind selectable in the terminal dock's Session Typer (`New session type`),
 * including standalone File Explorer tabs alongside process launchers and Browser.
 * Kept distinct from {@link SESSION_TYPES} so process/global settings contracts stay untouched.
 */
export function dockTabTypesFor(
  browserSessions: boolean,
): readonly { kind: TabKind; label: string }[] {
  return [...sessionTypesFor(browserSessions), { kind: "file", label: "File Explorer" }];
}

/** Resolves a requested dock tab kind supported on this host, falling back to `shell`. */
export function offeredDockTabKind(kind: unknown, browserSessions: boolean): TabKind {
  return dockTabTypesFor(browserSessions).find((type) => type.kind === kind)?.kind ?? "shell";
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
  /** Initial label seeded by a task, worktree preset, resume, or child spawn (never a manual user rename). */
  launchName?: string;
  /** True only when `name` was explicitly set by the user via `renameTab`. */
  userNamed?: boolean;
  /** Supplied as a literal agent CLI argument when this tab starts. */
  initialPrompt?: string;
  /** One-shot shell command written once when a fresh shell PTY starts (never persisted). */
  initialCommand?: string;
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
  /** Protects this split pane from being evicted when clicking a background tab in the strip. */
  pinned?: boolean;
  /** Pre-arms quiescent peer auto-wake on unattended kickoff sessions. */
  autoWake?: boolean;
  /** What the session was launched for; sent with every spawn of this tab so a restart keeps its model. */
  purpose?: "commit";
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
  mode: PermissionMode | null;
  /** Durable backend session ledger row id when reopened after quit/crash. */
  ledgerRowId?: string;
  /** True when resuming as a distinct forked Claude session (`--fork-session`). */
  fork?: boolean;
  /**
   * The ended attempt this conversation continues. Linkage only, so the
   * task's Agents pane can list the session under its attempt; a resumed tab
   * is the reader's own and never acts on that attempt's record.
   */
  runId?: string;
  /**
   * A first message for the resumed conversation, sent via `resumeArgs`.
   * The first start only: a restart of the tab resumes without it, or it
   * would be said twice.
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

/**
 * True for canonical UUIDs or the Antigravity `--continue` sentinel produced
 * when an `agy` session ends before reporting a UUID.
 */
export function isResumableSessionRef(value: string, provider?: string): boolean {
  return isSessionId(value) || (provider === "agy" && value === "__agy_continue__");
}

/** The tab already showing this launch, if one is. */
export function tabFor(state: TabState, launch: TaskLaunch): TerminalTab | undefined {
  const { resume, attach } = launch;
  if (attach) return state.tabs.find((tab) => tab.attachSessionId === attach.sessionId);
  if (resume?.ledgerRowId) {
    return state.tabs.find((tab) => tab.resume?.ledgerRowId === resume.ledgerRowId);
  }
  if (resume?.fork) {
    const checkout = launch.checkout ?? null;
    return state.tabs.find(
      (tab) =>
        tab.resume?.fork === true
        && tab.resume.sessionId === resume.sessionId
        && (tab.checkout ?? null) === checkout,
    );
  }
  return resume
    ? state.tabs.find((tab) => !tab.resume?.fork && tab.resume?.sessionId === resume.sessionId)
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

export type TabTitleSource = "custom" | "agent" | "launch" | "file" | "url" | "launcher";

export interface BrowserPageState {
  loading?: boolean;
  error?: string | null;
}

export interface TabDisplayTitle {
  primary: string;
  secondary: string | null;
  fullTitle: string;
  source: TabTitleSource;
}

/** Extracts a concise `host` or `host:port` label from a browser tab URL before `<title>` loads. */
export function browserHostLabel(url: string | null | undefined): string {
  const raw = url?.trim();
  if (!raw) return "";
  try {
    return new URL(raw).host || "";
  } catch {
    try {
      if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw) && /^[\w.[\]:-]+(?:\/|$)/.test(raw)) {
        return new URL(`http://${raw}`).host || "";
      }
    } catch {
      /* ignore malformed address */
    }
    return "";
  }
}

function fileFallbackFor(tab: TerminalTab): string {
  if (tab.launcher !== "file") return "";
  return tab.filePath ? (tab.filePath.split(/[\\/]/).pop() ?? "") : (tab.snippet?.title ?? "");
}

function clipCodepoints(value: string, maxChars: number): string {
  const glyphs = Array.from(value);
  if (glyphs.length <= maxChars) return value;
  return `${glyphs.slice(0, maxChars - 1).join("")}…`;
}

function distinctClean(candidate: string | null | undefined, primaryLower: string, maxChars: number): string | null {
  if (!candidate) return null;
  const cleaned = cleanTabName(candidate, maxChars);
  if (!cleaned || cleaned.toLowerCase() === primaryLower) return null;
  return cleaned;
}

const GENERIC_LAUNCHER_TITLES: Readonly<Record<TabKind, ReadonlySet<string>>> = {
  shell: new Set(["shell"]),
  claude: new Set(["claude", "claude code", "claude-code", "anthropic claude"]),
  manvi: new Set(["manvi", "manvi cli"]),
  codex: new Set(["codex", "codex cli", "openai codex"]),
  grok: new Set(["grok", "grok cli", "xai grok"]),
  agy: new Set(["agy", "antigravity", "antigravity cli", "google antigravity"]),
  browser: new Set(["browser"]),
  file: new Set(["files", "file explorer"]),
};

/**
 * True when an OSC or launch title only repeats the launcher's generic brand
 * name (such as startup `"✳ Claude Code"`, `"Claude Code"`, or `"OpenAI Codex"`),
 * carrying no task or topic information.
 */
export function isGenericLauncherTitle(raw: string | null | undefined, launcher: TabKind): boolean {
  if (!raw) return true;
  const normalized = stripTitleSpinner(cleanTabName(raw, 256))
    .replace(/^[✳✦★•·\-–—:\s]+/u, "")
    .trim()
    .toLowerCase();
  if (!normalized) return true;
  if (normalized === launcherLabel(launcher).toLowerCase()) return true;
  return GENERIC_LAUNCHER_TITLES[launcher]?.has(normalized) ?? false;
}

function resolveTabPrimaryTitle(tab: TerminalTab): { raw: string; source: TabTitleSource } {
  const customName = tab.userNamed ? cleanTabName(tab.name ?? "", 120) : "";
  if (customName) return { raw: customName, source: "custom" };
  const oscTitle = tab.title ? stripTitleSpinner(cleanTabName(tab.title, 256)) : "";
  if (oscTitle && !isGenericLauncherTitle(oscTitle, tab.launcher)) {
    return { raw: oscTitle, source: "agent" };
  }
  const seededName = cleanTabName(tab.launchName ?? tab.name ?? "", 120);
  if (seededName && !isGenericLauncherTitle(seededName, tab.launcher)) {
    return { raw: seededName, source: "launch" };
  }
  const fileFallback = cleanTabName(fileFallbackFor(tab), 120);
  if (fileFallback) return { raw: fileFallback, source: "file" };
  if (tab.launcher === "browser" && tab.url) {
    const host = cleanTabName(browserHostLabel(tab.url), 120);
    if (host) return { raw: host, source: "url" };
  }
  return { raw: launcherLabel(tab.launcher), source: "launcher" };
}

/**
 * The text on a tab: custom rename when user-pinned, else live OSC title, else
 * launch-seeded task/worktree name, clipped so a long title cannot stretch the strip.
 */
export function tabLabel(tab: TerminalTab): string {
  const { raw } = resolveTabPrimaryTitle(tab);
  if (!raw) return launcherLabel(tab.launcher);
  return clipCodepoints(raw, 28);
}

/**
 * Full header / pane title and secondary context for a session tab.
 * Separates user renames, live OSC 0/2 agent/program titles, launch-seeded names,
 * browser URL/loading/error state, file `:revealLine` context, and OSC 7501 status detail.
 */
export function tabDisplayTitle(
  tab: TerminalTab,
  agentStatus?: { state?: string; detail?: string | null } | null,
  browserState?: BrowserPageState | null,
): TabDisplayTitle {
  const { raw, source } = resolveTabPrimaryTitle(tab);
  const fileLine =
    tab.launcher === "file" && tab.filePath && typeof tab.revealLine === "number" && tab.revealLine > 0
      ? Math.floor(tab.revealLine)
      : null;
  const fileCol =
    fileLine !== null && typeof tab.revealColumn === "number" && tab.revealColumn > 0
      ? Math.floor(tab.revealColumn)
      : null;

  const baseFullTitle = cleanTabName(raw, 256) || launcherLabel(tab.launcher);
  const fullTitle =
    source === "file" && tab.filePath && fileLine !== null
      ? `${cleanTabName(tab.filePath, 240)}:${fileLine}${fileCol !== null ? `:${fileCol}` : ""}`
      : baseFullTitle;
  const primary =
    source === "file" && fileLine !== null
      ? `${clipCodepoints(baseFullTitle, 110)}:${fileLine}`
      : clipCodepoints(baseFullTitle, 120);
  const primaryLower = primary.toLowerCase();
  const oscDetail = cleanTabName(agentStatus?.detail ?? "", 96);
  const rawLaunchLabel = cleanTabName(tab.launchName ?? (!tab.userNamed ? (tab.name ?? "") : ""), 96);
  const launchLabel = isGenericLauncherTitle(rawLaunchLabel, tab.launcher) ? "" : rawLaunchLabel;
  const rawLiveOsc = tab.title ? stripTitleSpinner(cleanTabName(tab.title, 96)) : "";
  const liveOsc = isGenericLauncherTitle(rawLiveOsc, tab.launcher) ? "" : rawLiveOsc;
  const browserSecondary = (): string | null => {
    if (tab.launcher !== "browser") return null;
    if (browserState?.error) {
      return distinctClean(browserState.error, primaryLower, 96) ?? "Page load error";
    }
    if (browserState?.loading) {
      return "Loading…";
    }
    return (
      distinctClean(browserHostLabel(tab.url), primaryLower, 96) ??
      distinctClean(tab.url, primaryLower, 96)
    );
  };

  let secondary: string | null = null;
  if (source === "custom") {
    const customFileLoc =
      tab.launcher === "file" && tab.filePath
        ? `${tab.filePath}${fileLine !== null ? `:${fileLine}` : ""}`
        : null;
    secondary =
      distinctClean(liveOsc, primaryLower, 96) ??
      distinctClean(launchLabel, primaryLower, 96) ??
      distinctClean(customFileLoc, primaryLower, 96) ??
      browserSecondary() ??
      distinctClean(oscDetail, primaryLower, 96);
  } else if (source === "agent") {
    secondary =
      distinctClean(launchLabel, primaryLower, 96) ??
      distinctClean(oscDetail, primaryLower, 96) ??
      browserSecondary();
  } else if (source === "file") {
    if (tab.filePath) {
      const lastSlash = tab.filePath.lastIndexOf("/");
      const dir = lastSlash > 0 ? tab.filePath.slice(0, lastSlash) : null;
      const dirWithLine =
        dir && fileLine !== null
          ? `${dir}:${fileLine}`
          : dir ?? (fileLine !== null ? `L${fileLine}` : null);
      secondary = distinctClean(dirWithLine, primaryLower, 96) ?? distinctClean(oscDetail, primaryLower, 96);
    } else if (tab.snippet) {
      secondary = distinctClean("Captured output", primaryLower, 96) ?? distinctClean(oscDetail, primaryLower, 96);
    }
  } else {
    secondary = browserSecondary() ?? distinctClean(oscDetail, primaryLower, 96);
  }
  return { primary, secondary, fullTitle, source };
}

/**
 * Best descriptive title for `TerminalSessionRecord.title` (status pill and
 * cross-repository Sessions list), returning `undefined` when only generic
 * launcher labeling is present.
 */
export function effectiveSessionTitle(
  tab: TerminalTab,
  agentStatus?: { state?: string; detail?: string | null } | null,
): string | undefined {
  const customName = tab.userNamed ? cleanTabName(tab.name ?? "", 120) : "";
  if (customName) return customName;
  const oscTitle = tab.title ? stripTitleSpinner(cleanTabName(tab.title, 256)) : "";
  if (oscTitle && !isGenericLauncherTitle(oscTitle, tab.launcher)) return oscTitle;
  const launchLabel = cleanTabName(tab.launchName ?? tab.name ?? "", 120);
  if (launchLabel && !isGenericLauncherTitle(launchLabel, tab.launcher)) return launchLabel;
  const detail = cleanTabName(agentStatus?.detail ?? "", 120);
  return detail || undefined;
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
    const launchLabel = cleanTabName(task.title, 120) || undefined;
    tab.attachSessionId = task.attach.sessionId;
    if (launchLabel) {
      tab.name = launchLabel;
      tab.launchName = launchLabel;
    }
  } else if (task?.resume) {
    // A plain agent tab, not the attempt's: the attempt ended and its
    // record is final, so this session is the reader's own continuation.
    if (task.resume.fork) {
      if (launcher !== "claude" || !isSessionId(task.resume.sessionId)) return state;
      const launchLabel = cleanTabName(`${task.title} (forked)`, 120) || undefined;
      tab.resume = { ...task.resume, ...(task.runId ? { runId: task.runId } : {}) };
      if (launchLabel) {
        tab.name = launchLabel;
        tab.launchName = launchLabel;
      }
    } else {
      if ((launcher !== "claude" && launcher !== "agy") || !isResumableSessionRef(task.resume.sessionId, launcher)) return state;
      const suffix = task.resume.ledgerRowId ? "(reopened)" : "(resumed)";
      const launchLabel = cleanTabName(`${task.title} ${suffix}`, 120) || undefined;
      tab.resume = { ...task.resume, ...(task.runId ? { runId: task.runId } : {}) };
      if (launchLabel) {
        tab.name = launchLabel;
        tab.launchName = launchLabel;
      }
    }
  } else if (task) {
    tab.taskRunId = task.runId;
    const launchLabel = cleanTabName(task.title, 120) || undefined;
    if (launchLabel) {
      tab.name = launchLabel;
      tab.launchName = launchLabel;
    }
  }
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
    const cleaned = cleanTabName(options.title, 120);
    if (cleaned) {
      tab.name = cleaned;
      tab.launchName = cleaned;
    }
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

export type AgentLauncherKind = Exclude<LauncherKind, "shell">;

export function isHandoffCapableLauncher(kind: TabKind): kind is AgentLauncherKind {
  return (
    kind === "claude" ||
    kind === "manvi" ||
    kind === "codex" ||
    kind === "grok" ||
    kind === "agy"
  );
}

export interface PairHandoffTarget {
  readonly id: string;
  readonly launcher: AgentLauncherKind;
  readonly slot: number;
  readonly fromSlot: number;
  readonly label: string;
  readonly buttonLabel: string;
}

/**
 * Resolves live prompt-capable agent peers visible in the current split workspace
 * relative to `activeId`, supporting any agent CLI combination (including same-agent
 * splits like two Claude panes) with `#slot` disambiguation.
 */
export function resolvePairHandoffTargets(
  tabs: readonly TerminalTab[],
  visibleIds: readonly string[],
  activeId: string | null,
  statuses?: Readonly<Record<string, string | undefined>>,
): PairHandoffTarget[] {
  if (!activeId || visibleIds.length < 2) return [];
  const fromIdx = visibleIds.indexOf(activeId);
  if (fromIdx === -1) return [];
  const activeTab = tabs.find((tab) => tab.id === activeId);
  if (!activeTab || !isHandoffCapableLauncher(activeTab.launcher)) return [];
  if (statuses?.[activeTab.id] === "exited") return [];

  const rawTargets: Array<{ tab: TerminalTab; launcher: AgentLauncherKind; slot: number }> = [];
  const launcherCounts = new Map<AgentLauncherKind, number>();
  launcherCounts.set(activeTab.launcher, 1);

  for (let i = 0; i < visibleIds.length; i += 1) {
    const id = visibleIds[i];
    if (id === activeId) continue;
    const candidate = tabs.find((tab) => tab.id === id);
    if (!candidate || !isHandoffCapableLauncher(candidate.launcher)) continue;
    if (statuses?.[candidate.id] === "exited") continue;
    rawTargets.push({ tab: candidate, launcher: candidate.launcher, slot: i + 1 });
    launcherCounts.set(candidate.launcher, (launcherCounts.get(candidate.launcher) ?? 0) + 1);
  }

  return rawTargets.map(({ tab, launcher, slot }) => {
    const base = launcherLabel(launcher);
    const needsSlot =
      (launcherCounts.get(launcher) ?? 0) > 1 || rawTargets.length > 1;
    const targetName = needsSlot ? `${base} #${slot}` : base;
    return {
      id: tab.id,
      launcher,
      slot,
      fromSlot: fromIdx + 1,
      label: targetName,
      buttonLabel: `Handoff -> ${targetName}`,
    };
  });
}

/**
 * Builds a bounded, ANSI-stripped handoff prompt for 1-click context transfer
 * between visible agent split panes.
 */
export function buildPairHandoffPrompt(options: {
  fromLauncher: TabKind;
  fromSlot?: number | null;
  toLauncher?: TabKind | null;
  toSlot?: number | null;
  checkoutBranch?: string | null;
  openFilePath?: string | null;
  openFileLine?: number | null;
  terminalTail?: string | null;
}): string {
  const senderBase = launcherLabel(options.fromLauncher);
  const senderSlot = typeof options.fromSlot === "number" && options.fromSlot > 0 ? ` #${options.fromSlot}` : "";
  const receiverPart = options.toLauncher
    ? ` -> ${launcherLabel(options.toLauncher)}${typeof options.toSlot === "number" && options.toSlot > 0 ? ` #${options.toSlot}` : ""}`
    : "";
  const branchTag = options.checkoutBranch?.trim() ? ` (${options.checkoutBranch.trim()})` : "";
  const parts: string[] = [`[Handoff from ${senderBase}${senderSlot}${receiverPart}${branchTag}]`];
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
    const cleaned = cleanTabName(options.name, 120);
    if (cleaned) {
      tab.name = cleaned;
      tab.launchName = cleaned;
    }
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

/** Seeds a one-shot shell command consumed once when a fresh shell PTY starts. */
export function setTabInitialCommand(state: TabState, id: string, initialCommand: string | null): TabState {
  const trimmed = initialCommand?.trim() ?? "";
  if (!trimmed || !state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, initialCommand: trimmed } : tab)) };
}

/** Pre-arms quiescent peer auto-wake on an unattended kickoff tab. */
export function setTabAutoWake(state: TabState, id: string, autoWake: boolean): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, autoWake } : tab)) };
}

/** Records what a newly opened tab was launched for (`purpose` of `cmd_terminal_spawn`). */
export function setTabPurpose(state: TabState, id: string, purpose: "commit"): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, purpose } : tab)) };
}

/** Sets typed resume metadata (and optional display name) on a newly opened tab. */
export function setTabResume(state: TabState, id: string, resume: ResumeLaunch, name?: string): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  const cleanedName = name ? cleanTabName(name, 120) : "";
  return {
    ...state,
    tabs: state.tabs.map((tab) =>
      tab.id === id
        ? {
            ...tab,
            resume: { ...resume },
            ...(cleanedName
              ? {
                  launchName: cleanedName,
                  ...(tab.userNamed ? {} : { name: cleanedName }),
                }
              : {}),
          }
        : tab,
    ),
  };
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

/** Finds an existing non-snippet `file` tab in the same checkout (matching `filePath` when supplied, or the first disk `file` tab in that checkout). */
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
      (tab) => tab.launcher === "file" && !tab.snippet && sameCheckout(tab) && tab.filePath === cleanPath,
    );
    if (exact) return exact;
  }
  return state.tabs.find((tab) => tab.launcher === "file" && !tab.snippet && sameCheckout(tab));
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
  const switchedFile = cleanPath !== undefined && cleanPath !== tab.filePath;
  return {
    ...state,
    tabs: state.tabs.map((candidate) =>
      candidate.id === id
        ? {
            ...candidate,
            ...(cleanPath !== undefined ? { filePath: cleanPath, snippet: undefined } : {}),
            ...(switchedFile && candidate.userNamed
              ? { userNamed: false, name: candidate.launchName }
              : {}),
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
                ...(tab.userNamed ? { userNamed: false, name: tab.launchName } : {}),
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
  const existing = state.tabs.find((tab) => tab.id === id);
  if (!existing) return state;
  const cleaned = stripTitleSpinner(cleanTabName(title, 256)) || null;
  if (existing.title === cleaned) return state;
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, title: cleaned } : tab)),
  };
}

export function toggleTabPinned(state: TabState, id: string, pinned?: boolean): TabState {
  const existing = state.tabs.find((tab) => tab.id === id);
  if (!existing) return state;
  const nextPinned = pinned ?? !existing.pinned;
  if (Boolean(existing.pinned) === nextPinned) return state;
  return {
    ...state,
    tabs: state.tabs.map((tab) =>
      tab.id === id ? { ...tab, ...(nextPinned ? { pinned: true } : { pinned: undefined }) } : tab,
    ),
  };
}

/**
 * Leading + trailing coalescer for high-frequency OSC 0/2 title updates (8–12 Hz
 * CLI spinners). Delivers the first distinct normalized title immediately so
 * real task transitions have zero latency, and coalesces rapid bursts within
 * `intervalMs` into one trailing update.
 */
export function createTitleCoalescer(
  onFlush: (title: string) => void,
  intervalMs = 150,
  timers: {
    now?: () => number;
    setTimeout?: typeof globalThis.setTimeout;
    clearTimeout?: typeof globalThis.clearTimeout;
  } = {},
) {
  const nowFn = timers.now ?? Date.now;
  const setTimer = timers.setTimeout ?? globalThis.setTimeout;
  const clearTimer = timers.clearTimeout ?? globalThis.clearTimeout;
  let lastDelivered: string | null = null;
  let lastDeliveredAt = -Infinity;
  let pendingTitle: string | null = null;
  let timer: ReturnType<typeof globalThis.setTimeout> | null = null;
  let disposed = false;

  const deliver = (normalized: string, timestamp: number) => {
    lastDelivered = normalized;
    lastDeliveredAt = timestamp;
    onFlush(normalized);
  };

  return {
    push(rawTitle: string): void {
      if (disposed) return;
      const normalized = stripTitleSpinner(cleanTabName(rawTitle, 256));
      if (timer !== null) {
        if (normalized === lastDelivered && pendingTitle === null) return;
        pendingTitle = normalized;
        return;
      }
      if (normalized === lastDelivered) return;
      const now = nowFn();
      const elapsed = now - lastDeliveredAt;
      if (elapsed >= intervalMs) {
        deliver(normalized, now);
        return;
      }
      pendingTitle = normalized;
      timer = setTimer(() => {
        timer = null;
        if (disposed || pendingTitle === null) return;
        const next = pendingTitle;
        pendingTitle = null;
        if (next !== lastDelivered) {
          deliver(next, nowFn());
        }
      }, Math.max(1, intervalMs - elapsed));
    },
    flush(): void {
      if (timer !== null) {
        clearTimer(timer);
        timer = null;
      }
      if (!disposed && pendingTitle !== null) {
        const next = pendingTitle;
        pendingTitle = null;
        if (next !== lastDelivered) {
          deliver(next, nowFn());
        }
      }
    },
    dispose(): void {
      disposed = true;
      pendingTitle = null;
      if (timer !== null) {
        clearTimer(timer);
        timer = null;
      }
    },
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
  const cleaned = cleanTabName(name);
  return {
    ...state,
    tabs: state.tabs.map((tab) => {
      if (tab.id !== id) return tab;
      if (cleaned) {
        return { ...tab, name: cleaned, userNamed: true };
      }
      return { ...tab, name: tab.launchName, userNamed: false };
    }),
  };
}

export function moveTab(state: TabState, id: string, step: -1 | 1): TabState {
  const from = state.tabs.findIndex((tab) => tab.id === id);
  const to = from + step;
  if (from < 0 || to < 0 || to >= state.tabs.length) return state;
  const tabs = [...state.tabs];
  [tabs[from], tabs[to]] = [tabs[to], tabs[from]];
  return { ...state, tabs };
}
