/**
 * When a Mavi thread should interrupt the person with a toast.
 *
 * Two things interrupt: a thread starts waiting for an answer, or a thread
 * fails. A finished turn does not: the header badge and the rail already say
 * so, and a toast per turn is noise. Anything the person caused (a stop, an
 * interrupt, a move to the terminal) is not a failure (`last_turn` says
 * "interrupted", an exit they asked for is `exit_ok`).
 *
 * Only a transition this run observed can notify. The first status seen for a
 * thread (at launch, on a reload, on a replay) is recorded silently, so a
 * reload never re-announces a request or a failure the person already saw.
 * The same notice for the same thread is not repeated within
 * {@link REPEAT_MS}: approve, work, approve again is one toast, not three.
 */
import type { ChildCard } from "./childTabs.svelte";
import type { ThreadStatus } from "./threads";

export type MaviNoticeKind = "needs_you" | "failed";

export const REPEAT_MS = 60_000;

export interface MaviNotice {
  readonly key: string;
  readonly kind: MaviNoticeKind;
}

/**
 * Compares the statuses now with the ones last seen and returns what to
 * announce. Updates `seen` in place; keys no longer present are forgotten.
 */
export function maviNotices(
  seen: Map<string, ThreadStatus>,
  now: ReadonlyArray<readonly [key: string, status: ThreadStatus]>,
  announced: Map<string, number> = new Map(),
  clock: number = Date.now(),
): MaviNotice[] {
  const out: MaviNotice[] = [];
  const present = new Set<string>();
  for (const [key, status] of now) {
    present.add(key);
    const before = seen.get(key);
    seen.set(key, status);
    if (before === undefined || before === status) continue;
    if (status !== "needs_you" && status !== "failed") continue;
    const id = `${key}:${status}`;
    const last = announced.get(id);
    if (last !== undefined && clock - last < REPEAT_MS) continue;
    announced.set(id, clock);
    out.push({ key, kind: status });
  }
  for (const key of [...seen.keys()]) if (!present.has(key)) seen.delete(key);
  return out;
}

/**
 * The toast text for a terminal tab a thread started changing state. The tab's
 * title leads, then the thread it belongs to, so two tabs from two threads
 * read apart.
 */
export function childTabNotice(card: Pick<ChildCard, "title" | "state">, threadTitle: string): string {
  const what = card.state === "needs_input" ? "needs your approval" : card.state === "done" ? "finished" : "ended";
  return `“${card.title}” (from “${threadTitle}”) ${what}.`;
}
