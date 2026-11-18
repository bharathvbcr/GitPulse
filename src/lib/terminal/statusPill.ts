/**
 * What the status-bar terminal pill may say about live sessions.
 *
 * It only repeats fields the dock already published (`terminalSessions`) and,
 * when a caller supplies one, a host context (`cmd_terminal_context`). A
 * missing context contributes no command and is never called idle. "Idle" is
 * only the host's provable-idle answer; a program name is only `process`
 * when the host said the foreground is busy.
 */

import { displayName, isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { describeCheckout, groupSessionsByRepository, type CheckoutTab } from "./checkoutLabel";
import { waitingLabel } from "../health/leases";
import type { LeaseWaiter } from "../stores/hostLoadStore";
import { isProvablyIdle, type TerminalContext } from "./sessionContext";
import {
  cleanTabName,
  isGenericLauncherTitle,
  isHandoffCapableLauncher,
  LAUNCHERS,
  launcherLabel,
  stripTitleSpinner,
  type TabKind,
} from "./tabs";

export interface PillSession {
  key: string;
  repoPath: string;
  label: string;
  /** Canonical launcher/tab kind for agent icon rendering. */
  launcher?: TabKind;
  status: string;
  title?: string;
  /** Checkout the process runs in, when the registry published one. */
  checkout?: string;
  /** Backend id, when the PTY has started. Absent means there is nothing to ask. */
  sessionId?: string;
  /** The panel wired `reveal`, so the session can be brought on screen. */
  canReveal: boolean;
  /** True when this session is currently focused in its repository's terminal dock. */
  isActive?: boolean;
  /** 1-based visible split slot index when shown in a multi-pane split layout, or null. */
  splitSlot?: number | null;
  /** Urgent agent attention phrase (e.g. "Needs input", "Error"), or null. */
  attention?: string | null;
}

export interface PillSessionView extends PillSession {
  name: string;
  /** Distinct task/OSC title when different from the agent/launcher label, or null. */
  displayTitle: string | null;
  /** Canonical agent/launcher label ("Claude", "Antigravity", "Shell", …). */
  agentLabel: string;
  /** Canonical launcher kind for `<LauncherIcon>`. */
  launcherKind: TabKind;
  /** Repository display name resolved across primary checkouts and worktrees. */
  repository: string;
  /** Worktree / checkout slug when running outside the primary checkout, or null. */
  checkoutName: string | null;
  /** Canonical repository path for `<RepoLogo>` lookup (resolves agent worktrees to their open parent repo tab). */
  logoPath: string;
  /** True when the session belongs to the currently open repository (including its worktrees). */
  isCurrentRepo: boolean;
  /** Activity phrase, or null when the host has not said what the foreground is. */
  phrase: string | null;
  /** Host directory or published checkout, or null when neither is known. */
  cwd: string | null;
  /** Short label for `cwd` (preferring repo-relative `repo_dir`), or null when there is no directory to show. */
  where: string | null;
}

export interface TerminalPillView {
  /**
   * Text after the Terminal label. Null when the dock is closed and no
   * session is published — the pill stays a control, not a status fiction.
   */
  summary: string | null;
  /** Launcher kind of the single or highlighted session shown in `summary`, or null. */
  highlightedLauncher: TabKind | null;
  /** Lead line in the hover menu. */
  detail: string;
  alive: number;
  /** Current repository first, then the rest, in registry order. */
  sessions: PillSessionView[];
  /** Set only when exactly one session can be focused. */
  focusKey: string | null;
}

const PHRASE_LIMIT = 36;
const NAME_LIMIT = 28;
const STATUS_LIMIT = 48;

function clip(text: string, max: number): string {
  const glyphs = Array.from(text);
  if (glyphs.length <= max) return text;
  return `${glyphs.slice(0, max - 1).join("")}…`;
}

function cleanSessionTitle(raw: string | undefined, max: number): string {
  if (!raw) return "";
  return stripTitleSpinner(cleanTabName(raw, max));
}

function stripLeadingStatusIcon(title: string): string {
  return title.replace(/^[✳✦★•]+\s+/u, "").trim();
}

const AGENT_PROCESS_BINARIES = new Set([
  "claude",
  "claude-code",
  "codex",
  "manvi",
  "grok",
  "agy",
  "antigravity",
]);

function processBaseName(process: string | null | undefined): string {
  const cleaned = cleanTabName(process ?? "", 64).toLowerCase();
  return cleaned.split(/[\\/]/).pop() ?? cleaned;
}

function inferLauncherKind(session: Pick<PillSession, "launcher" | "label">): TabKind {
  if (session.launcher) return session.launcher;
  const lower = session.label.trim().toLowerCase();
  return LAUNCHERS.find((item) => item.label.toLowerCase() === lower || item.kind === lower)?.kind ?? "shell";
}

/** Session title when meaningful, else the registry label. Never a command. */
export function sessionName(session: Pick<PillSession, "title" | "label" | "launcher">): string {
  const launcherKind = inferLauncherKind(session);
  const title = cleanSessionTitle(session.title, 120);
  if (title && !isGenericLauncherTitle(title, launcherKind)) {
    const cleaned = stripLeadingStatusIcon(title);
    if (cleaned) return clip(cleaned, NAME_LIMIT);
  }
  const label = cleanTabName(session.label, 64);
  return label ? clip(label, NAME_LIMIT) : "Shell";
}

/**
 * Foreground phrase from a host context, or from a non-running lifecycle
 * status. `running` with no context is null: the PTY being up is not a
 * command, and it is not idle.
 *
 * Agent CLIs (`claude`, `codex`, `manvi`, `grok`, `agy`) are the PTY's root
 * process (`busy: false`) and spawn MCP/helper children (`jobs: true`), so
 * Unix shell job-control labels (`"Idle"` / `"Background job"`) never apply
 * to them.
 */
export function activityPhrase(
  status: string,
  context: TerminalContext | null,
  launcherKind?: TabKind,
): string | null {
  const procBase = processBaseName(context?.process);
  const isAgentRoot =
    (launcherKind !== undefined && isHandoffCapableLauncher(launcherKind)) ||
    (context?.busy === false && AGENT_PROCESS_BINARIES.has(procBase));
  if (!isAgentRoot) {
    if (context && isProvablyIdle(context)) return "Idle";
    if (context?.busy === true) {
      const program = cleanTabName(context.process ?? "", 64);
      return program ? clip(program, PHRASE_LIMIT) : "Busy";
    }
    if (context?.jobs === true) return "Background job";
  } else if (context?.busy === true) {
    const program = cleanTabName(context.process ?? "", 64);
    if (program && !AGENT_PROCESS_BINARIES.has(procBase) && procBase !== "node") {
      return clip(program, PHRASE_LIMIT);
    }
  }
  if (status === "starting") return "Starting";
  if (status === "closing") return "Closing";
  const trimmed = cleanTabName(status, 120);
  if (!trimmed || trimmed === "running") return null;
  return clip(trimmed, STATUS_LIMIT);
}

function placeOf(
  path: string | null | undefined,
  repoDir?: string | null,
): { cwd: string | null; where: string | null } {
  const cwd = path?.trim() || null;
  const rel = cleanTabName(repoDir ?? "", 120).replace(/^[\\/]+|[\\/]+$/g, "");
  if (rel && rel !== ".") {
    return { cwd, where: clip(rel, NAME_LIMIT) };
  }
  if (!cwd) return { cwd: null, where: null };
  const where = cleanTabName(displayName(cwd), 64);
  return { cwd, where: where && where !== "repo" ? clip(where, NAME_LIMIT) : null };
}

function line(name: string, phrase: string | null): string {
  return phrase ? `${name} · ${phrase}` : name;
}

export function terminalPillView(
  sessions: readonly PillSession[],
  input: {
    open: boolean;
    repoPath: string | null;
    activity?: Readonly<Record<string, TerminalContext | null | undefined>>;
    /** Builds waiting for a lease slot; a session at its head of one says so. Absent says nothing. */
    buildQueue?: readonly LeaseWaiter[];
    identity?: PathIdentityOptions;
    /** Open dock tab count in the current repository (including Browser/File tabs). */
    openTabs?: number;
    /** Open repository tabs, used to resolve canonical repository & worktree names. */
    checkouts?: readonly CheckoutTab[];
  },
): TerminalPillView {
  const identity = input.identity ?? { caseInsensitive: isCaseInsensitiveFs() };
  const checkouts = input.checkouts ?? [];
  const repoGroups = groupSessionsByRepository(sessions, input.repoPath, checkouts, identity);
  const hereKeys = new Set<string>();
  const groupPrimaryPathBySessionKey = new Map<string, string>();
  const checkoutGroups = groupSessionsByRepository(
    checkouts.map((tab) => ({ repoPath: tab.path })),
    input.repoPath,
    checkouts,
    identity,
  );
  const checkoutPathByGroupKey = new Map<string, string>();
  for (const grp of checkoutGroups) {
    const first = grp.sessions[0]?.repoPath;
    if (first) checkoutPathByGroupKey.set(grp.key, first);
  }
  for (const grp of repoGroups) {
    const parentTabPath =
      checkoutPathByGroupKey.get(grp.key) ?? (grp.here && input.repoPath ? input.repoPath : null);
    for (const session of grp.sessions) {
      if (grp.here) hereKeys.add(session.key);
      if (parentTabPath) groupPrimaryPathBySessionKey.set(session.key, parentTabPath);
    }
  }
  const ranked = [...sessions].sort((left, right) => {
    const leftHere = hereKeys.has(left.key);
    const rightHere = hereKeys.has(right.key);
    if (leftHere === rightHere) return 0;
    return leftHere ? -1 : 1;
  });
  const views = ranked.map((session): PillSessionView => {
    const context = input.activity?.[session.key] ?? null;
    const launcherKind = inferLauncherKind(session);
    const waiting = session.sessionId && input.buildQueue ? waitingLabel(input.buildQueue, session.sessionId) : null;
    const phrase = waiting ?? activityPhrase(session.status, context, launcherKind);
    const place = placeOf(context?.cwd ?? session.checkout, context?.repo_dir);
    const agentLabel = launcherLabel(launcherKind);
    const rawTitle = cleanSessionTitle(session.title, 120);
    const meaningfulTitle =
      rawTitle && !isGenericLauncherTitle(rawTitle, launcherKind)
        ? stripLeadingStatusIcon(rawTitle)
        : "";
    const displayTitle = meaningfulTitle ? clip(meaningfulTitle, 56) : null;
    const checkoutDesc = describeCheckout(session.checkout ?? session.repoPath, checkouts, identity);
    let repository = checkoutDesc.repository || displayName(session.repoPath).trim() || "Repository";
    let rawCheckoutName = checkoutDesc.checkout;
    if (
      !rawCheckoutName &&
      session.checkout &&
      !sameRepo(session.checkout, session.repoPath, identity)
    ) {
      const hostDesc = describeCheckout(session.repoPath, checkouts, identity);
      const hostRepo = hostDesc.repository || displayName(session.repoPath).trim();
      if (hostRepo) repository = hostRepo;
      const leafName = cleanTabName(displayName(session.checkout), 64);
      const prefix = `${repository}.`;
      rawCheckoutName =
        leafName.startsWith(prefix) && leafName.length > prefix.length
          ? leafName.slice(prefix.length)
          : leafName || null;
    }
    const isCurrentRepo =
      hereKeys.has(session.key) ||
      Boolean(input.repoPath && sameRepo(session.repoPath, input.repoPath, identity));
    const hasExactTab = checkouts.some((tab) => sameRepo(tab.path, session.repoPath, identity));
    const logoPath = hasExactTab
      ? session.repoPath
      : (groupPrimaryPathBySessionKey.get(session.key) ?? session.repoPath);
    const cleanedAttention = cleanTabName(session.attention ?? "", 48) || null;
    return {
      ...session,
      attention: cleanedAttention,
      name: sessionName(session),
      displayTitle,
      agentLabel,
      launcherKind,
      repository: clip(cleanTabName(repository, 64) || "Repository", NAME_LIMIT),
      checkoutName: rawCheckoutName ? clip(cleanTabName(rawCheckoutName, 64), NAME_LIMIT) : null,
      logoPath,
      isCurrentRepo,
      phrase,
      ...place,
    };
  });
  const focusKey = views.length === 1 && views[0].canReveal ? views[0].key : null;
  if (views.length === 0) {
    const openCount = Math.max(0, Math.trunc(input.openTabs ?? 0));
    if (openCount > 0) {
      const tabsLabel = openCount === 1 ? "1 tab" : `${openCount} tabs`;
      return {
        summary: input.open ? tabsLabel : null,
        highlightedLauncher: null,
        detail: input.open ? tabsLabel : `${tabsLabel} · dock closed`,
        alive: 0,
        sessions: [],
        focusKey: null,
      };
    }
    return {
      summary: input.open ? "No shell" : null,
      highlightedLauncher: null,
      detail: input.open ? "No shell" : "No shell · dock closed",
      alive: 0,
      sessions: [],
      focusKey: null,
    };
  }
  if (views.length === 1) {
    const only = views[0];
    const summary = line(only.name, only.attention || only.phrase);
    return {
      summary,
      highlightedLauncher: only.launcherKind,
      detail: summary,
      alive: 1,
      sessions: views,
      focusKey,
    };
  }
  const urgent = views.find((view) => Boolean(view.attention));
  const activeHere = input.repoPath
    ? views.find((view) => Boolean(view.isActive) && view.isCurrentRepo)
    : undefined;
  const highlighted = urgent ?? activeHere ?? null;
  const summary = highlighted
    ? line(highlighted.name, highlighted.attention || highlighted.phrase)
    : null;
  const detail = urgent
    ? `${views.length} sessions · ${line(urgent.name, urgent.attention || null)}`
    : `${views.length} sessions`;
  return {
    summary,
    highlightedLauncher: highlighted?.launcherKind ?? null,
    detail,
    alive: views.length,
    sessions: views,
    focusKey: null,
  };
}
