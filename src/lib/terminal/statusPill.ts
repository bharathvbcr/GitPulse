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
import { isProvablyIdle, type TerminalContext } from "./sessionContext";

export interface PillSession {
  key: string;
  repoPath: string;
  label: string;
  status: string;
  title?: string;
  /** Checkout the process runs in, when the registry published one. */
  checkout?: string;
  /** Backend id, when the PTY has started. Absent means there is nothing to ask. */
  sessionId?: string;
  /** The panel wired `reveal`, so the session can be brought on screen. */
  canReveal: boolean;
}

export interface PillSessionView extends PillSession {
  name: string;
  /** Activity phrase, or null when the host has not said what the foreground is. */
  phrase: string | null;
  /** Host directory or published checkout, or null when neither is known. */
  cwd: string | null;
  /** Short label for `cwd`, or null when there is no directory to show. */
  where: string | null;
}

export interface TerminalPillView {
  /**
   * Text after the Terminal label. Null when the dock is closed and no
   * session is published — the pill stays a control, not a status fiction.
   */
  summary: string | null;
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
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

/** Session title, else the registry label. Never a command. */
export function sessionName(session: Pick<PillSession, "title" | "label">): string {
  const title = session.title?.trim();
  if (title) return clip(title, NAME_LIMIT);
  const label = session.label.trim();
  return label ? clip(label, NAME_LIMIT) : "Shell";
}

/**
 * Foreground phrase from a host context, or from a non-running lifecycle
 * status. `running` with no context is null: the PTY being up is not a
 * command, and it is not idle.
 */
export function activityPhrase(status: string, context: TerminalContext | null): string | null {
  if (context && isProvablyIdle(context)) return "Idle";
  if (context?.busy === true) {
    const program = context.process?.trim() ?? "";
    return program ? clip(program, PHRASE_LIMIT) : "Busy";
  }
  if (context?.jobs === true) return "Background job";
  if (status === "starting") return "Starting";
  if (status === "closing") return "Closing";
  const trimmed = status.trim();
  if (!trimmed || trimmed === "running") return null;
  return clip(trimmed, STATUS_LIMIT);
}

function placeOf(path: string | null | undefined): { cwd: string | null; where: string | null } {
  const cwd = path?.trim() || null;
  if (!cwd) return { cwd: null, where: null };
  const where = displayName(cwd).trim();
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
    identity?: PathIdentityOptions;
  },
): TerminalPillView {
  const identity = input.identity ?? { caseInsensitive: isCaseInsensitiveFs() };
  const ranked = [...sessions].sort((left, right) => {
    const leftHere = input.repoPath ? sameRepo(left.repoPath, input.repoPath, identity) : false;
    const rightHere = input.repoPath ? sameRepo(right.repoPath, input.repoPath, identity) : false;
    if (leftHere === rightHere) return 0;
    return leftHere ? -1 : 1;
  });
  const views = ranked.map((session): PillSessionView => {
    const context = input.activity?.[session.key] ?? null;
    const phrase = activityPhrase(session.status, context);
    const place = placeOf(context?.cwd ?? session.checkout);
    return { ...session, name: sessionName(session), phrase, ...place };
  });
  const focusKey = views.length === 1 && views[0].canReveal ? views[0].key : null;
  if (views.length === 0) {
    return {
      summary: input.open ? "No shell" : null,
      detail: input.open ? "No shell" : "No shell · dock closed",
      alive: 0,
      sessions: [],
      focusKey: null,
    };
  }
  if (views.length === 1) {
    const only = views[0];
    const summary = line(only.name, only.phrase);
    return { summary, detail: summary, alive: 1, sessions: views, focusKey };
  }
  return {
    summary: null,
    detail: `${views.length} sessions`,
    alive: views.length,
    sessions: views,
    focusKey: null,
  };
}
