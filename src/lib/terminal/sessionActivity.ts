/**
 * What each terminal session has been doing lately, for anything that wants
 * to watch an agent without looking at its terminal — the task's Agents pane.
 *
 * Three facts, each from the one place that observes it:
 *
 *  - **Output**: when the session last printed. From `TerminalSession`'s
 *    output path. Recency is all it means: output is not progress, and quiet
 *    is not "waiting" — an agent can think silently or print while stuck.
 *  - **Title**: what the program last called itself (OSC 0/2). Agent CLIs use
 *    it to say what they are working on; shown verbatim, bounded, never
 *    interpreted.
 *  - **Attention**: what the agent last asked for, announced by the native
 *    notifier (`gitpulse-session-attention`, see `alerts::Attention`) whether
 *    or not it raised a banner. It stands until it is answered: the user types
 *    into that session, the agent's hooks report it working again (a prompt
 *    submitted from anywhere, the approved tool running), or the process ends
 *    while still asking. The notifier decides which of those answers what, and
 *    announces the answer as an event whose kind is `clear`. More output does
 *    not clear it: an agent that finished and then repainted its prompt has
 *    still finished. A page that was not listening when something was
 *    announced reads what stands from the notifier once it is.
 *
 * Keyed by the backend's PTY id (`TerminalSessionRecord.sessionId`), the one
 * id both the renderer and the notifier know. Bounded, and throttled to one
 * publish per session per second, because output arrives per chunk and some
 * programs rewrite their title on every spinner frame.
 */

import { writable, type Readable } from "svelte/store";
import { MAX_TERMINAL_SESSIONS } from "./sessionLimit";

/** The renderer event the native notifier announces attention on. */
export const ATTENTION_EVENT = "gitpulse-session-attention";

/**
 * The hook events the notifier names (`alerts::bridge::EVENTS`), and what
 * each asks of the reader. `sessionActivity.contract.test.ts` reads the Rust
 * table and fails if one is missing here.
 */
export const ATTENTION_EVENTS = {
  permission_prompt: { kind: "needs-you", label: "Needs your permission" },
  permission_request: { kind: "needs-you", label: "Needs your permission" },
  idle_prompt: { kind: "needs-you", label: "Waiting for you" },
  agent_needs_input: { kind: "needs-you", label: "Needs your input" },
  elicitation_dialog: { kind: "needs-you", label: "Asking you a question" },
  elicitation_url_dialog: { kind: "needs-you", label: "Asking you to open a link" },
  agent_completed: { kind: "finished", label: "Finished its work" },
  turn_finished: { kind: "finished", label: "Finished its turn" },
  error: { kind: "error", label: "Stopped on an error" },
  prompt_submitted: { kind: "clear", label: "Working again" },
  tool_finished: { kind: "clear", label: "Working again" },
  session_ended: { kind: "clear", label: "Session ended" },
} as const satisfies Record<string, { kind: AttentionKind | "clear"; label: string }>;

/**
 * `signalled` is a terminal convention (a bell, an OSC 9 notice) that says
 * only that the program wanted attention, never why.
 */
export type AttentionKind = "needs-you" | "finished" | "error" | "signalled";

/**
 * Whether a kind is the agent waiting on the reader rather than reporting
 * what it did. A bell counts: it is how an agent with no hooks says it is
 * blocked. Finished does not: it is worth showing, not worth chasing. The one
 * rule every surface — the task pane, the board chip, the tray — counts by.
 */
export function asksForReader(kind: AttentionKind | null | undefined): boolean {
  return kind === "needs-you" || kind === "error" || kind === "signalled";
}

export interface SessionAttention {
  kind: AttentionKind;
  label: string;
  /** What the agent said, when it said anything. */
  detail: string | null;
  at: number;
}

export interface SessionActivity {
  lastOutputAt: number | null;
  title: string | null;
  attention: SessionAttention | null;
}

/** Publish at most this often per session for output and title changes. */
export const ACTIVITY_PUBLISH_MS = 1000;
/** Sessions tracked; twice the most that can be open, for ones still closing. */
export const MAX_TRACKED_ACTIVITY = MAX_TERMINAL_SESSIONS * 2;
const MAX_TITLE = 120;
const MAX_DETAIL = 240;
const SESSION_ID = /^[\w-]{1,128}$/;

/** Strips control characters and bounds text that came from a program. */
function clean(value: unknown, max: number): string | null {
  if (typeof value !== "string") return null;
  const text = Array.from(value.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").replace(/\s+/g, " ").trim()).slice(0, max).join("");
  return text || null;
}

/**
 * Replies xterm writes to the PTY on the program's behalf: focus in/out,
 * cursor-position and device-attribute reports, mode reports, OSC colour
 * replies, mouse reports. None of them is the reader answering anything.
 */
const TERMINAL_REPLY = new RegExp(
  [
    "\\x1b\\[[IO]",
    "\\x1b\\[\\d+;\\d+R",
    "\\x1b\\[[?>=]?[\\d;]*c",
    "\\x1b\\[\\??[\\d;]*\\$y",
    "\\x1b\\][\\d;]*[^\\x07\\x1b]*(?:\\x07|\\x1b\\\\)",
    "\\x1b\\[<\\d+;\\d+;\\d+[mM]",
    "\\x1b\\[M[\\s\\S]{3}",
  ].join("|"),
  "g",
);

/** Whether data xterm sent to the PTY came from the reader rather than xterm. */
export function isReaderInput(data: string): boolean {
  if (!data) return false;
  return data.replace(TERMINAL_REPLY, "").length > 0;
}

/**
 * Validates an announcement at the boundary. A malformed one is no
 * announcement: it came over IPC, from a native side that may be newer.
 * `attention: null` is a valid announcement: whatever stood was answered.
 */
export function parseAttention(raw: unknown, now: number): { session: string; attention: SessionAttention | null } | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  if (typeof row.session !== "string" || !SESSION_ID.test(row.session)) return null;
  if (typeof row.channel !== "string" || !row.channel) return null;
  const detail = clean(row.detail, MAX_DETAIL);
  const event = typeof row.event === "string" && Object.hasOwn(ATTENTION_EVENTS, row.event)
    ? ATTENTION_EVENTS[row.event as keyof typeof ATTENTION_EVENTS]
    : null;
  if (event?.kind === "clear") return { session: row.session, attention: null };
  const attention: SessionAttention = event
    ? { kind: event.kind, label: event.label, detail, at: now }
    : { kind: "signalled", label: "Signalled", detail, at: now };
  return { session: row.session, attention };
}

/**
 * What stood while a page was not listening (`alerts::StandingAttention`,
 * held to it by `check:types`): an announcement as `parseAttention` reads it,
 * plus how long ago it was made. Read as `unknown` and validated row by row,
 * because it crosses IPC from a native side that may be newer.
 */
export interface StandingAttention {
  session: string;
  channel: string;
  event: string | null;
  detail: string | null;
  age_ms: number;
}

/** The longest age a replayed attention may claim: a clock that went wrong is not a week-old prompt. */
const MAX_REPLAY_AGE_MS = 7 * 24 * 60 * 60 * 1000;

const EMPTY: SessionActivity = Object.freeze({ lastOutputAt: null, title: null, attention: null });

export function createSessionActivity(clock: () => number = Date.now) {
  const entries = new Map<string, SessionActivity>();
  const published = new Map<string, number>();
  /** When each session's attention last changed here, for `replay`. */
  const changed = new Map<string, number>();
  const store = writable<ReadonlyMap<string, SessionActivity>>(new Map());
  let pending: ReturnType<typeof setTimeout> | null = null;

  const publish = () => {
    if (pending) { clearTimeout(pending); pending = null; }
    store.set(new Map(entries));
  };
  /** Publishes now, or once the session's throttle window has passed. */
  const publishThrottled = (session: string, now: number) => {
    const last = published.get(session) ?? -Infinity;
    if (now - last >= ACTIVITY_PUBLISH_MS) {
      published.set(session, now);
      publish();
    } else if (!pending) {
      pending = setTimeout(() => { pending = null; published.set(session, clock()); store.set(new Map(entries)); }, ACTIVITY_PUBLISH_MS - (now - last));
    }
  };
  const update = (session: string, change: (current: SessionActivity) => SessionActivity): boolean => {
    if (!SESSION_ID.test(session)) return false;
    const current = entries.get(session) ?? EMPTY;
    if (!entries.has(session) && entries.size >= MAX_TRACKED_ACTIVITY) {
      // The longest-quiet session gives way.
      let oldest: string | null = null;
      let oldestAt = Infinity;
      for (const [key, value] of entries) {
        const at = Math.max(value.lastOutputAt ?? 0, value.attention?.at ?? 0);
        if (at < oldestAt) { oldest = key; oldestAt = at; }
      }
      if (oldest) { entries.delete(oldest); published.delete(oldest); changed.delete(oldest); }
    }
    entries.set(session, change(current));
    return true;
  };

  return {
    subscribe: store.subscribe as Readable<ReadonlyMap<string, SessionActivity>>["subscribe"],
    /** The session printed something. */
    output(session: string) {
      const now = clock();
      if (update(session, (current) => ({ ...current, lastOutputAt: now }))) publishThrottled(session, now);
    },
    /** The program renamed itself; a rename is also a sign of life. */
    title(session: string, value: string) {
      const now = clock();
      const title = clean(value, MAX_TITLE);
      if (update(session, (current) => ({ ...current, title, lastOutputAt: now }))) publishThrottled(session, now);
    },
    /**
     * The reader typed into the session: whatever it asked is answered.
     * True when something stood, so the caller can tell the notifier — which
     * would otherwise replay the question to the next page and leave its
     * banner up.
     */
    input(session: string, data: string): boolean {
      if (!isReaderInput(data)) return false;
      const current = entries.get(session);
      if (!current?.attention) return false;
      entries.set(session, { ...current, attention: null });
      changed.set(session, clock());
      publish();
      return true;
    },
    /** An announcement from the native notifier, as received. */
    announce(raw: unknown) {
      const now = clock();
      const parsed = parseAttention(raw, now);
      if (!parsed) return false;
      // The notifier owns what stands (it folds a bell into the request it
      // repeats, and decides what an answer ends), and its announcements
      // arrive in order from one thread. So the pane mirrors it rather than
      // second-guessing it: a second rule here would disagree with it exactly
      // when the notifier has reason to differ.
      const current = entries.get(parsed.session)?.attention ?? null;
      if (parsed.attention === null && !current) return true;
      changed.set(parsed.session, now);
      if (update(parsed.session, (entry) => ({ ...entry, attention: parsed.attention }))) publish();
      return true;
    },
    /**
     * What stood while this page was not listening, read from the notifier
     * once it is. A session announced since `since` is newer than the
     * snapshot and keeps what it was told. Returns how many were applied.
     */
    replay(raw: unknown, since: number): number {
      if (!Array.isArray(raw)) return 0;
      const now = clock();
      let applied = 0;
      for (const row of raw.slice(0, MAX_TRACKED_ACTIVITY)) {
        const parsed = parseAttention(row, now);
        if (!parsed?.attention) continue;
        if ((changed.get(parsed.session) ?? -Infinity) >= since) continue;
        const age = row && typeof row === "object" ? (row as Record<string, unknown>).age_ms : undefined;
        const at = typeof age === "number" && Number.isFinite(age) && age >= 0
          ? now - Math.min(age, MAX_REPLAY_AGE_MS)
          : now;
        const attention = { ...parsed.attention, at };
        if (update(parsed.session, (entry) => ({ ...entry, attention }))) applied += 1;
      }
      if (applied) publish();
      return applied;
    },
    /** The session ended or its tab closed. */
    forget(session: string) {
      published.delete(session);
      changed.delete(session);
      if (entries.delete(session)) publish();
    },
  };
}

export const sessionActivity = createSessionActivity();

/**
 * Feeds the native notifier's announcements into `activity`, then catches up
 * on what stood before this page was listening. One call per page, from the
 * app's session bridge; resolves to the unlisten function, and rejects when
 * the event cannot be listened to — which the caller must say, because then
 * no agent's request for attention will reach the pane. A failed catch-up is
 * reported through `onReplayError` and does not undo the listener.
 */
export async function bindAttention(
  listen: (event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>,
  activity: Pick<ReturnType<typeof createSessionActivity>, "announce" | "replay"> = sessionActivity,
  standing?: () => Promise<unknown>,
  onReplayError?: (error: unknown) => void,
  clock: () => number = Date.now,
): Promise<() => void> {
  const since = clock();
  const unlisten = await listen(ATTENTION_EVENT, (event) => { activity.announce(event.payload); });
  if (standing) {
    try {
      activity.replay(await standing(), since);
    } catch (error) {
      onReplayError?.(error);
    }
  }
  return unlisten;
}
