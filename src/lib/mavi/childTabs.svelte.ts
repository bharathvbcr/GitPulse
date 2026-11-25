/**
 * Terminal tabs a Mavi agent started (`gitpulse_terminal_spawn`), shown in the
 * thread that owns them.
 *
 * GitPulse emits `mavi-child-state` for an orchestrated child whose owner is a
 * Mavi thread. The thread id in the payload is the thread's `ThreadRecord.key`.
 * A child that needs the person (a permission dialog, a question) becomes a
 * card in that thread; it leaves when the prompt clears, and a child that
 * finishes or ends stays as a card until it is dismissed. Without this, a
 * child waiting on the person was visible only inside its own tab while the
 * thread that launched it looked idle.
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";

export const MAVI_CHILD_EVENT = "mavi-child-state";

/** The most cards one thread keeps; the oldest finished ones go first. */
export const MAX_CARDS = 20;

export type ChildState = "needs_input" | "cleared" | "ended" | "done";

/** The payload exactly as the host emits it (snake_case). */
export interface ChildStatePayload {
  thread_id: string;
  child_session_id: string;
  title: string | null;
  state: ChildState;
  detail: string | null;
  exit_code: number | null;
}

export type CardState = "needs_input" | "done" | "ended";

export interface ChildCard {
  /** The terminal session's id (the native PTY id). */
  readonly sessionId: string;
  readonly title: string;
  readonly state: CardState;
  readonly detail: string | null;
  readonly exitCode: number | null;
  readonly at: number;
}

export interface ChildChange {
  readonly threadKey: string;
  readonly card: ChildCard;
  /** The card is new or its state changed: worth a toast. */
  readonly announce: boolean;
}

function validPayload(value: unknown): value is ChildStatePayload {
  if (typeof value !== "object" || value === null) return false;
  const p = value as Record<string, unknown>;
  return (
    typeof p.thread_id === "string" &&
    p.thread_id !== "" &&
    typeof p.child_session_id === "string" &&
    p.child_session_id !== "" &&
    (p.state === "needs_input" || p.state === "cleared" || p.state === "ended" || p.state === "done")
  );
}

/**
 * Applies one payload to a thread's cards (returns the new list). Pure, so the
 * ordering rules are testable: a `cleared` removes a card that is waiting but
 * never one that already finished, and a finished card is never reopened by a
 * late `needs_input`.
 */
export function applyChild(
  cards: readonly ChildCard[],
  payload: ChildStatePayload,
  now: number,
): { cards: ChildCard[]; card: ChildCard | null; announce: boolean } {
  const before = cards.find((c) => c.sessionId === payload.child_session_id);
  const rest = cards.filter((c) => c.sessionId !== payload.child_session_id);
  if (payload.state === "cleared") {
    if (before && before.state === "needs_input") return { cards: rest, card: null, announce: false };
    return { cards: [...cards], card: null, announce: false };
  }
  if (before && before.state !== "needs_input" && payload.state === "needs_input") {
    return { cards: [...cards], card: null, announce: false };
  }
  const card: ChildCard = {
    sessionId: payload.child_session_id,
    title: payload.title?.trim() || before?.title || "Terminal tab",
    state: payload.state,
    detail: payload.detail?.trim() || null,
    exitCode: payload.exit_code,
    at: now,
  };
  const announce = !before || before.state !== card.state;
  const next = [...rest, card];
  // Bounded: drop the oldest finished cards before any that wait on the person.
  while (next.length > MAX_CARDS) {
    const drop = next.findIndex((c) => c.state !== "needs_input");
    next.splice(drop === -1 ? 0 : drop, 1);
  }
  return { cards: next, card, announce };
}

class ChildTabs {
  /** Cards by thread key. */
  byThread = $state<Record<string, ChildCard[]>>({});
  #listeners = new Set<(change: ChildChange) => void>();
  #unlisten: UnlistenFn | null = null;
  #starting: Promise<void> | null = null;
  #now: () => number = () => Date.now();

  forThread(key: string): readonly ChildCard[] {
    return this.byThread[key] ?? [];
  }

  /** How many of a thread's children wait on the person. */
  waiting(key: string): number {
    return this.forThread(key).filter((c) => c.state === "needs_input").length;
  }

  onChange(listener: (change: ChildChange) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Feeds one event payload in. Anything that is not a well-formed payload is ignored. */
  handle(payload: unknown): void {
    if (!validPayload(payload)) return;
    const key = payload.thread_id;
    const out = applyChild(this.byThread[key] ?? [], payload, this.#now());
    if (out.cards.length) this.byThread[key] = out.cards;
    else delete this.byThread[key];
    if (out.card) for (const listener of this.#listeners) listener({ threadKey: key, card: out.card, announce: out.announce });
  }

  dismiss(key: string, sessionId: string): void {
    const rest = this.forThread(key).filter((c) => c.sessionId !== sessionId);
    if (rest.length) this.byThread[key] = rest;
    else delete this.byThread[key];
  }

  /** A forgotten thread keeps no cards. */
  forget(key: string): void {
    delete this.byThread[key];
  }

  /** Starts listening. Idempotent. */
  start(): Promise<void> {
    if (this.#unlisten) return Promise.resolve();
    this.#starting ??= listen<unknown>(MAVI_CHILD_EVENT, (event) => this.handle(event.payload))
      .then((unlisten) => {
        this.#unlisten = unlisten;
      })
      .finally(() => {
        this.#starting = null;
      });
    return this.#starting;
  }

  stop(): void {
    this.#unlisten?.();
    this.#unlisten = null;
  }
}

export const childTabs = new ChildTabs();

/** What closing a forgotten thread's terminal tabs did. */
export interface CloseChildrenResult {
  closed: number;
  skippedProtected: number;
}

/**
 * Closes the terminal tabs a thread's agent started. Called when the thread is
 * forgotten, never on an interrupt or a stop: those leave work running that
 * the person may still want.
 */
export function closeThreadChildren(threadKey: string): Promise<CloseChildrenResult> {
  return invoke<CloseChildrenResult>("cmd_mavi_close_thread_children", { threadKey });
}
