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
import hiddenCodepoints from "../../../scripts/fixtures/hidden-codepoints.json";
import { MAX_TERMINAL_SESSIONS } from "./sessionLimit";

/** The renderer event the native notifier announces attention on. */
export const ATTENTION_EVENT = "gitpulse-session-attention";

/**
 * The hook events the notifier names (`alerts::bridge::EVENTS`), and what
 * each asks of the reader. The guard is the first test in
 * `sessionActivity.test.ts` ("the attention vocabulary matches the native
 * notifier's"), which reads the Rust table and fails if a name or its kind
 * differs here; `parseAttention` drops an event outside this table.
 */
export const ATTENTION_EVENTS = {
  permission_prompt: { kind: "needs-you", urgency: "blocking", label: "Needs your permission" },
  permission_request: { kind: "needs-you", urgency: "blocking", label: "Needs your permission" },
  idle_prompt: { kind: "finished", urgency: "informational", label: "Waiting for you" },
  agent_needs_input: { kind: "needs-you", urgency: "blocking", label: "Needs your input" },
  elicitation_dialog: { kind: "needs-you", urgency: "blocking", label: "Asking you a question" },
  elicitation_url_dialog: { kind: "needs-you", urgency: "blocking", label: "Asking you to open a link" },
  agent_completed: { kind: "finished", urgency: "informational", label: "Finished its work" },
  turn_finished: { kind: "finished", urgency: "informational", label: "Finished its turn" },
  error: { kind: "error", urgency: "error", label: "Stopped on an error" },
  prompt_submitted: { kind: "clear", urgency: "clear", label: "Working again" },
  tool_finished: { kind: "clear", urgency: "clear", label: "Working again" },
  session_ended: { kind: "clear", urgency: "clear", label: "Session ended" },
} as const satisfies Record<string, { kind: AttentionKind | "clear"; urgency: AttentionUrgency; label: string }>;

/**
 * Semantic urgency of an attention announcement (`alerts::bridge::Urgency`).
 */
export type AttentionUrgency = "blocking" | "error" | "informational" | "clear";

/**
 * Standing attention urgency persisted on `SessionAttention` (never `"clear"`,
 * since `"clear"` resolves standing attention to `null`).
 */
export type SessionAttentionUrgency = Exclude<AttentionUrgency, "clear">;

/**
 * `signalled` is a terminal convention (a bell, an OSC 9 notice) that says
 * only that the program wanted attention, never why.
 */
export type AttentionKind = "needs-you" | "finished" | "error" | "signalled";

/**
 * Whether a bell or OSC notice counts as an ask (the "Count a terminal bell
 * as waiting for you" setting). Off until the reader turns it on. This module
 * owns the flag rather than importing the setting: `interfaceStore` imports
 * `tabs`, which imports this module, so reading the store here would close a
 * cycle that fails at load. The store pushes every change in.
 */
let signalledAsks = false;

export function setSignalledAsksForReader(on: boolean): void {
  signalledAsks = on;
}

const VALID_STANDING_URGENCIES = new Set<string>(["blocking", "error", "informational"]);

/**
 * Classifies a `SessionAttention` object or bare `AttentionKind` into its
 * effective standing urgency (`"blocking" | "error" | "informational" | null`).
 *
 * Explicit `urgency` on `SessionAttention` takes precedence (except opted-in
 * `"signalled"`, which promotes to `"blocking"` when `bellAsks` is true so
 * callers see uniform blocking semantics across all surfaces). Falls back to
 * `ATTENTION_EVENTS[event]` and finally `kind` when `urgency` is omitted by
 * legacy fixtures.
 */
export function classifyAttentionUrgency(
  target: SessionAttention | AttentionKind | null | undefined,
  bellAsks: boolean = signalledAsks,
): SessionAttentionUrgency | null {
  if (!target) return null;
  if (typeof target === "string") {
    if (target === "needs-you") return "blocking";
    if (target === "error") return "error";
    if (target === "finished") return "informational";
    if (target === "signalled") return bellAsks ? "blocking" : "informational";
    return null;
  }
  if (target.kind === "signalled") {
    if (target.urgency === "blocking" || target.urgency === "error") return target.urgency;
    return bellAsks ? "blocking" : "informational";
  }
  if (target.urgency && VALID_STANDING_URGENCIES.has(target.urgency)) {
    return target.urgency;
  }
  if (target.event && Object.hasOwn(ATTENTION_EVENTS, target.event)) {
    const mapped = ATTENTION_EVENTS[target.event as keyof typeof ATTENTION_EVENTS].urgency;
    return mapped === "clear" ? null : mapped;
  }
  return classifyAttentionUrgency(target.kind, bellAsks);
}

/**
 * Returns `true` strictly when the attention has `"blocking"` urgency
 * (human action required, or opted-in `"signalled"` when `bellAsks` is true).
 * Returns `false` for `"error"`, `"informational"`, and `null`; use
 * `asksForReader` when checking whether either `"blocking"` or `"error"` needs
 * the reader.
 */
export function isBlockingAttention(
  target: SessionAttention | AttentionKind | null | undefined,
  bellAsks: boolean = signalledAsks,
): boolean {
  return classifyAttentionUrgency(target, bellAsks) === "blocking";
}

/**
 * Returns `true` strictly when the attention is `"informational"` and NOT an
 * opted-in ask (`!asksForReader`).
 */
export function isInformationalAttention(
  target: SessionAttention | AttentionKind | null | undefined,
  bellAsks: boolean = signalledAsks,
): boolean {
  return classifyAttentionUrgency(target, bellAsks) === "informational";
}

/**
 * Whether an attention event or kind is waiting on the reader rather than
 * reporting an informational notice.
 *
 * Returns `true` strictly for `"blocking"` + `"error"` + (`"signalled"` iff
 * `bellAsks` is true), and `false` for `"informational"` (including
 * `idle_prompt`, `turn_finished`, `agent_completed`, and passive terminal
 * signals). Passing `bellAsks` explicitly keeps pure projections (e.g. menu
 * bar state) decoupled from the module-global flag.
 */
export function asksForReader(
  kindOrAttention: SessionAttention | AttentionKind | null | undefined,
  bellAsks: boolean = signalledAsks,
): boolean {
  const urgency = classifyAttentionUrgency(kindOrAttention, bellAsks);
  return urgency === "blocking" || urgency === "error";
}

export interface SessionAttention {
  kind: AttentionKind;
  urgency?: SessionAttentionUrgency;
  event?: string | null;
  channel?: string;
  label: string;
  /** What the agent said, when it said anything. */
  detail: string | null;
  at: number;
}

export interface SessionActivity {
  lastOutputAt: number | null;
  lastInputAt?: number | null;
  lastPromptSubmittedAt?: number | null;
  dirtyPrompt?: boolean;
  fullyIdle?: boolean;
  providerSessionId?: string | null;
  title: string | null;
  attention: SessionAttention | null;
}

/**
 * Maps native notifier hook/bell attention onto the terminal tab/split status
 * model when no explicit OSC 7501 status overrides it.
 */
export function activityToAgentStatus(
  activity: SessionActivity | null | undefined,
): { state: "blocked" | "error" | "done"; detail: string | null } | null {
  const attention = activity?.attention;
  if (!attention) return null;
  const detail = attention.detail ?? (attention.label || null);
  const urgency = classifyAttentionUrgency(attention);
  if (urgency === "blocking") return { state: "blocked", detail };
  if (urgency === "error") return { state: "error", detail };
  if (attention.kind === "signalled") return null;
  if (urgency === "informational") return { state: "done", detail };
  return null;
}

/**
 * Concise cross-surface attention badge phrase ("Needs input" / "Failed") for
 * native hook attention when no OSC 7501 label overrides it.
 */
export function activityAttentionLabel(
  activity: SessionActivity | null | undefined,
): string | null {
  const mapped = activityToAgentStatus(activity);
  if (!mapped) return null;
  if (mapped.state === "blocked") return "Needs input";
  if (mapped.state === "error") return "Failed";
  return null;
}

/** Silence required on both input and output before quiescent auto-wake may fire. */
export const PEER_QUIESCENCE_MS = 3_000;

export interface AutoWakeFacts {
  autoWake: boolean;
  activity: SessionActivity | undefined;
  foregroundProcess: string | null;
  inboxGeneration: number;
  attemptedGeneration: number;
  now: number;
}

/**
 * Verdict 2's 5-invariant quiescent auto-wake check for an Antigravity (`agy`)
 * terminal tab:
 *  1. `autoWake` is armed AND `Stop` reported `fullyIdle === true` (`NO_TOOL_CALL`),
 *  2. Foreground process in the PTY is `"agy"`,
 *  3. `dirtyPrompt === false` (latched `true` on any user keystroke; cleared only on `PreInvocation`),
 *  4. `>= 3_000ms` silence since both `lastInputAt` and `lastOutputAt`,
 *  5. Single-shot per `inboxGeneration` (`attemptedGeneration < inboxGeneration`).
 */
export function canAutoWakePeerInbox(facts: AutoWakeFacts): boolean {
  if (!facts.autoWake) return false;
  if (facts.inboxGeneration <= 0 || facts.attemptedGeneration >= facts.inboxGeneration) return false;
  if (facts.foregroundProcess !== "agy") return false;
  const activity = facts.activity;
  if (!activity || !activity.fullyIdle || activity.dirtyPrompt) return false;
  if (activity.lastInputAt != null && facts.now - activity.lastInputAt < PEER_QUIESCENCE_MS) return false;
  if (activity.lastOutputAt != null && facts.now - activity.lastOutputAt < PEER_QUIESCENCE_MS) return false;
  return true;
}

/** Publish output and title changes at most this often, across all sessions. */
export const ACTIVITY_PUBLISH_MS = 1000;
/** Sessions tracked; twice the most that can be open, for ones still closing. */
export const MAX_TRACKED_ACTIVITY = MAX_TERMINAL_SESSIONS * 2;
const MAX_TITLE = 120;
const MAX_DETAIL = 240;
const SESSION_ID = /^[\w-]{1,128}$/;
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * The code points the cleaner drops (format characters `Cf`: bidirectional
 * overrides and isolates, zero-width characters, the byte-order mark, the tag
 * block; and private use `Co`) and the ones it turns into a space (controls and
 * whitespace). Read from the table `scripts/hidden-codepoints.mjs` generates,
 * which Rust's `alerts::text` reads too, rather than from the running engine's
 * `\p{…}` classes, so both sides are pinned to one Unicode version.
 */
function rangeClass(ranges: ReadonlyArray<readonly [number, number]>): string {
  const at = (cp: number) => `\\u{${cp.toString(16)}}`;
  return `[${ranges.map(([lo, hi]) => (lo === hi ? at(lo) : `${at(lo)}-${at(hi)}`)).join("")}]`;
}
const HIDDEN = new RegExp(rangeClass(hiddenCodepoints.drop as Array<[number, number]>), "gu");
const SEPARATORS = new RegExp(`${rangeClass(hiddenCodepoints.space as Array<[number, number]>)}+`, "gu");

/**
 * Cleans and bounds text that came from a program: hidden characters dropped,
 * controls and whitespace collapsed to single spaces. Mirrors Rust's
 * `alerts::text::sanitize` over the same generated table;
 * `scripts/fixtures/sanitize-cases.json` is the readable table both are also
 * tested against, row by row.
 */
export function clean(value: unknown, max: number): string | null {
  if (typeof value !== "string") return null;
  const text = Array.from(value.replace(HIDDEN, "").replace(SEPARATORS, " ").trim()).slice(0, max).join("");
  return text || null;
}

const TRANSIENT_SPINNER_CHAR = /^[\u2800-\u28ff◐◓◑◒◴◷◶◵✢✶✻✽·]$/u;

/**
 * Strips transient 8–12 Hz CLI spinner frames (Braille dots, quarter-circle/clock
 * spinners, and starburst/dot glyphs such as `✢`, `✶`, `✻`, `✽`, `·`) from the
 * start or end of a terminal title when separated by whitespace (or when the title
 * is a bare spinner glyph), while keeping static status icons such as `✳` and
 * mid-title separators (`api · worker`) intact.
 */
export function stripTitleSpinner(value: string): string {
  let slice = value.trim();
  while (slice.length > 0) {
    const glyphs = Array.from(slice);
    const first = glyphs[0];
    if (first && TRANSIENT_SPINNER_CHAR.test(first)) {
      if (glyphs.length === 1) return "";
      const rest = glyphs.slice(1).join("");
      if (/^\s/u.test(rest)) {
        slice = rest.trimStart();
        continue;
      }
    }
    break;
  }
  while (slice.length > 0) {
    const glyphs = Array.from(slice);
    const last = glyphs[glyphs.length - 1];
    if (last && TRANSIENT_SPINNER_CHAR.test(last)) {
      if (glyphs.length === 1) return "";
      const head = glyphs.slice(0, -1).join("");
      if (/\s$/u.test(head)) {
        slice = head.trimEnd();
        continue;
      }
    }
    break;
  }
  return slice.trim();
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

/**
 * Control sequences the reader's navigation keys send (arrows, Home/End,
 * Delete, Shift-Tab, a bracketed-paste marker). Moving through a menu is not
 * answering it: Enter, Escape or a digit is what answers, and those are not
 * CSI sequences. The same arrows and Home/End arrive as SS3 (`ESC O A`) when a
 * program turns on application cursor keys, which menus commonly do. Text
 * inside a paste still counts, because only the sequences are removed.
 */
const CSI_KEY = /\x1b(?:\[[0-?]*[ -/]*[@-~]|O[A-DHF])/g;

/**
 * Whether data xterm sent to the PTY came from the reader and answers what the
 * program asked, rather than being xterm's own reply or a navigation key.
 */
export function isReaderInput(data: string): boolean {
  if (!data) return false;
  return data.replace(TERMINAL_REPLY, "").replace(CSI_KEY, "").length > 0;
}

/** Whether data xterm sent to the PTY is purely an automatic terminal reply. */
function isTerminalReplyOnly(data: string): boolean {
  if (!data) return true;
  return data.replace(TERMINAL_REPLY, "").length === 0;
}

/**
 * Unknown event names already logged: each of the first 16 is said once. Past
 * that the memory is a counter and a timestamp, not a set, so a newer native
 * side that names many events cannot grow it; the counter is what keeps a new
 * name from vanishing, because it is said in the next summary warning (at most
 * one a minute, carrying the total and the latest name).
 */
const loggedUnknown = new Set<string>();
const MAX_LOGGED_UNKNOWN = 16;
const SUMMARY_EVERY_MS = 60_000;
let pastCap = 0;
let lastSummaryAt = -Infinity;

function logUnknownEvent(event: unknown, now: number): void {
  const name = typeof event === "string" ? clean(event, 64) ?? "" : `<${typeof event}>`;
  if (loggedUnknown.has(name)) return;
  if (loggedUnknown.size < MAX_LOGGED_UNKNOWN) {
    loggedUnknown.add(name);
    console.warn(`[gitpulse-alerts] ignored an attention event this window does not know: ${JSON.stringify(name)}`);
    return;
  }
  pastCap += 1;
  if (now - lastSummaryAt < SUMMARY_EVERY_MS) return;
  lastSummaryAt = now;
  console.warn(
    `[gitpulse-alerts] ignored ${pastCap} more attention events past the first ${MAX_LOGGED_UNKNOWN} unknown names; latest: ${JSON.stringify(name)}`,
  );
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
  const channel = clean(row.channel, 64);
  if (!channel) return null;
  const detail = clean(row.detail, MAX_DETAIL);
  const MAX_DATE_MS = 8.64e15;
  const at =
    typeof row.at === "number" && Number.isFinite(row.at) && row.at > 0 && row.at <= MAX_DATE_MS
      ? row.at
      : now;
  // No event is a bell or an OSC notice: it says only that the program wanted
  // attention. An event this table does not name is a native side newer than
  // this window; counting it as a bell would put an unknown meaning on the
  // board as a request, so it is no announcement.
  let rawEvent: string | null = null;
  let event: (typeof ATTENTION_EVENTS)[keyof typeof ATTENTION_EVENTS] | null = null;
  if (row.event !== undefined && row.event !== null) {
    if (typeof row.event !== "string" || !Object.hasOwn(ATTENTION_EVENTS, row.event)) {
      logUnknownEvent(row.event, now);
      return null;
    }
    rawEvent = row.event;
    event = ATTENTION_EVENTS[rawEvent as keyof typeof ATTENTION_EVENTS];
  }
  if (event?.kind === "clear") {
    return { session: row.session, attention: null };
  }
  if (event) {
    const urgency: SessionAttentionUrgency = event.urgency;
    return {
      session: row.session,
      attention: {
        kind: event.kind,
        urgency,
        event: rawEvent,
        channel,
        label: event.label,
        detail,
        at,
      },
    };
  }
  const rawUrgency = typeof row.urgency === "string" ? row.urgency : null;
  if (rawUrgency === "clear") return { session: row.session, attention: null };
  if (rawUrgency === "blocking") {
    return {
      session: row.session,
      attention: {
        kind: "needs-you",
        urgency: "blocking",
        event: null,
        channel,
        label: "Needs your input",
        detail,
        at,
      },
    };
  }
  if (rawUrgency === "error") {
    return {
      session: row.session,
      attention: {
        kind: "error",
        urgency: "error",
        event: null,
        channel,
        label: "Stopped on an error",
        detail,
        at,
      },
    };
  }
  return {
    session: row.session,
    attention: {
      kind: "signalled",
      urgency: "informational",
      event: null,
      channel,
      label: "Signalled",
      detail,
      at,
    },
  };
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
  fully_idle?: boolean;
  provider_session_id?: string;
}

/** The longest age a replayed attention may claim: a clock that went wrong is not a week-old prompt. */
const MAX_REPLAY_AGE_MS = 7 * 24 * 60 * 60 * 1000;

const EMPTY: SessionActivity = Object.freeze({
  lastOutputAt: null,
  lastInputAt: null,
  lastPromptSubmittedAt: null,
  dirtyPrompt: false,
  fullyIdle: false,
  providerSessionId: null,
  title: null,
  attention: null,
});

export function createSessionActivity(clock: () => number = Date.now) {
  const entries = new Map<string, SessionActivity>();
  /** When the last throttled publish went out, whichever session caused it. */
  let lastThrottled = -Infinity;
  /** When each session's attention last changed here, for `replay`. */
  const changed = new Map<string, number>();
  const store = writable<ReadonlyMap<string, SessionActivity>>(new Map());
  let pending: ReturnType<typeof setTimeout> | null = null;

  const publish = () => {
    if (pending) { clearTimeout(pending); pending = null; }
    store.set(new Map(entries));
  };
  /**
   * Publishes now, or once the throttle window has passed. The window is
   * shared by every session: each publish copies the whole map and re-runs
   * every subscriber, so a per-session window let N chattering sessions cost
   * N publishes a second. One trailing publish carries every session's latest.
   */
  const publishThrottled = (now: number) => {
    if (now - lastThrottled >= ACTIVITY_PUBLISH_MS) {
      lastThrottled = now;
      publish();
    } else if (!pending) {
      pending = setTimeout(() => { pending = null; lastThrottled = clock(); store.set(new Map(entries)); }, ACTIVITY_PUBLISH_MS - (now - lastThrottled));
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
        const at = Math.max(value.lastOutputAt ?? 0, value.lastInputAt ?? 0, value.attention?.at ?? 0);
        if (at < oldestAt) { oldest = key; oldestAt = at; }
      }
      if (oldest) { entries.delete(oldest); changed.delete(oldest); }
    }
    entries.set(session, change(current));
    return true;
  };

  return {
    subscribe: store.subscribe as Readable<ReadonlyMap<string, SessionActivity>>["subscribe"],
    /** The session printed something. */
    output(session: string) {
      const now = clock();
      if (update(session, (current) => ({ ...current, lastOutputAt: now }))) publishThrottled(now);
    },
    /** The program renamed itself; a rename is also a sign of life. */
    title(session: string, value: string) {
      const now = clock();
      const rawClean = clean(value, MAX_TITLE);
      const title = rawClean ? (stripTitleSpinner(rawClean) || null) : null;
      const existing = entries.get(session);
      if (existing && existing.title === title && existing.lastOutputAt !== null && now - existing.lastOutputAt < ACTIVITY_PUBLISH_MS) {
        return;
      }
      if (update(session, (current) => ({ ...current, title, lastOutputAt: now }))) publishThrottled(now);
    },
    /**
     * The reader typed into the session: whatever it asked is answered, and
     * `dirtyPrompt` latches true until `PreInvocation` / `prompt_submitted`
     * fires (never cleared on Backspace or Escape).
     * True when something stood, so the caller can tell the notifier — which
     * would otherwise replay the question to the next page and leave its
     * banner up.
     */
    input(session: string, data: string): boolean {
      if (isTerminalReplyOnly(data)) return false;
      const now = clock();
      const answers = isReaderInput(data);
      const hadAttention = answers && Boolean(entries.get(session)?.attention);
      if (
        update(session, (current) => ({
          ...current,
          lastInputAt: now,
          dirtyPrompt: true,
          attention: answers ? null : current.attention,
        }))
      ) {
        if (hadAttention) changed.set(session, now);
        publish();
      }
      return hadAttention;
    },
    /** Clears `dirtyPrompt` and `fullyIdle` when a prompt boundary (`PreInvocation`) fires. */
    markPromptSubmitted(session: string): boolean {
      const now = clock();
      if (
        !update(session, (current) => ({
          ...current,
          dirtyPrompt: false,
          fullyIdle: false,
          lastPromptSubmittedAt: now,
        }))
      ) {
        return false;
      }
      publish();
      return true;
    },
    /** An announcement from the native notifier, as received. */
    announce(raw: unknown) {
      const now = clock();
      const parsed = parseAttention(raw, now);
      if (!parsed) return false;
      const row = raw as Record<string, unknown>;
      const rawEvent = typeof row.event === "string" ? row.event : null;
      const rawFullyIdle = typeof row.fully_idle === "boolean" ? row.fully_idle : undefined;
      const rawProviderSessionId =
        typeof row.provider_session_id === "string" && CANONICAL_UUID.test(row.provider_session_id)
          ? row.provider_session_id.toLowerCase()
          : undefined;
      // The notifier owns what stands (it folds a bell into the request it
      // repeats, and decides what an answer ends), and its announcements
      // arrive in order from one thread. So the pane mirrors it rather than
      // second-guessing it: a second rule here would disagree with it exactly
      // when the notifier has reason to differ.
      const currentEntry = entries.get(parsed.session);
      const current = currentEntry?.attention ?? null;
      const nextDirtyPrompt =
        rawEvent === "prompt_submitted" || rawEvent === "session_ended"
          ? false
          : (currentEntry?.dirtyPrompt ?? false);
      const nextFullyIdle =
        rawEvent === "turn_finished" || rawEvent === "agent_completed"
          ? rawFullyIdle === true
          : false;
      const nextPromptSubmittedAt =
        rawEvent === "prompt_submitted" ? now : (currentEntry?.lastPromptSubmittedAt ?? null);
      const nextProviderSessionId = rawProviderSessionId ?? currentEntry?.providerSessionId ?? null;
      const metaChanged =
        nextDirtyPrompt !== (currentEntry?.dirtyPrompt ?? false) ||
        nextFullyIdle !== (currentEntry?.fullyIdle ?? false) ||
        nextPromptSubmittedAt !== (currentEntry?.lastPromptSubmittedAt ?? null) ||
        nextProviderSessionId !== (currentEntry?.providerSessionId ?? null);
      if (parsed.attention === null && !current && !metaChanged) return true;
      changed.set(parsed.session, now);
      if (
        update(parsed.session, (entry) => ({
          ...entry,
          attention: parsed.attention,
          dirtyPrompt: nextDirtyPrompt,
          fullyIdle: nextFullyIdle,
          lastPromptSubmittedAt: nextPromptSubmittedAt,
          providerSessionId: nextProviderSessionId,
        }))
      ) {
        publish();
      }
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
        const rec = row && typeof row === "object" ? (row as Record<string, unknown>) : undefined;
        const age = rec?.age_ms;
        const at = typeof age === "number" && Number.isFinite(age) && age >= 0
          ? now - Math.min(age, MAX_REPLAY_AGE_MS)
          : now;
        const rawEvent = typeof rec?.event === "string" ? rec.event : null;
        const rawFullyIdle = typeof rec?.fully_idle === "boolean" ? rec.fully_idle : undefined;
        const rawProviderSessionId =
          typeof rec?.provider_session_id === "string" && CANONICAL_UUID.test(rec.provider_session_id)
            ? rec.provider_session_id.toLowerCase()
            : undefined;
        const attention = { ...parsed.attention, at };
        if (
          update(parsed.session, (entry) => ({
            ...entry,
            attention,
            fullyIdle:
              rawEvent === "turn_finished" || rawEvent === "agent_completed"
                ? rawFullyIdle === true
                : entry.fullyIdle,
            providerSessionId: rawProviderSessionId ?? entry.providerSessionId,
          }))
        ) {
          applied += 1;
        }
      }
      if (applied) publish();
      return applied;
    },
    /**
     * Dismisses a standing informational notification in the UI without clearing
     * prompt-tracking or idle flags (`dirtyPrompt`, `fullyIdle`,
     * `lastPromptSubmittedAt`). Refuses blocking or error requests that still
     * require user action (`asksForReader(current)`).
     */
    dismiss(session: string): boolean {
      if (!SESSION_ID.test(session)) return false;
      const current = entries.get(session)?.attention ?? null;
      if (!current || asksForReader(current)) return false;
      const now = clock();
      changed.set(session, now);
      if (
        update(session, (entry) => ({
          ...entry,
          attention: null,
        }))
      ) {
        publish();
        return true;
      }
      return false;
    },
    /** The session ended or its tab closed. */
    forget(session: string) {
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
