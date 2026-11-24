import type { ViewTab } from "../repos/persist";
import type { PolicyStatus } from "../stores/harnessStore";

/**
 * What a section needs from the reader right now, for the header switcher.
 *
 * The switcher collapses every view but the active one into a single pill,
 * and a collapsed pill must never hide a problem: a parked merge was never
 * allowed to be quieter than it was when Resolve had its own header tab, and
 * a blocked MANVI verdict is the same kind of fact. Both now belong to Overview,
 * which leads with the conflict editor since Resolve and Policy folded into it. So the facts that already
 * exist are carried onto the section that owns them, and from there onto the
 * view's pill. Nothing here is fetched for the switcher's sake.
 */
export interface AttentionContext {
  /** Files still carrying conflict markers. */
  conflictedCount: number;
  /** Status of the last MANVI verdict, or null when none was recorded. */
  verdictStatus: PolicyStatus | null;
  /** Mavi threads waiting for the person's answer to a permission request. */
  maviNeedsYou?: number;
  /** Mavi threads whose last turn or agent failed and that no one has looked at. */
  maviFailed?: number;
}

export type AttentionTone = "rose" | "amber";

export interface SectionAttention {
  tone: AttentionTone;
  /** A count to print, or null for a dot. */
  count: number | null;
  /** The sentence the badge stands for, for its tooltip and accessible name. */
  reason: string;
}

export function sectionAttention(
  view: ViewTab,
  section: string,
  context: AttentionContext,
): SectionAttention | null {
  if (view !== "work" || section !== "overview") return null;
  // Overview holds the conflict editor (Resolve folded into it), and the
  // editor leads the page while files carry markers, so the count outranks a
  // blocked verdict: it is the thing the reader lands on. A blocked verdict
  // also lives on Overview, on the row of the work it blocked.
  if (context.conflictedCount > 0) {
    const n = context.conflictedCount;
    return {
      tone: "rose",
      count: n,
      reason: n === 1 ? "1 file still carries conflict markers" : `${n} files still carry conflict markers`,
    };
  }
  if (context.verdictStatus === "blocked") {
    return { tone: "rose", count: null, reason: "Manvi blocked the last action" };
  }
  return null;
}

/**
 * Mavi has no sections (its rail, conversation and changes are one pane), so
 * its pill reads the threads directly. A request waiting for an answer
 * outranks a failure: the agent is blocked on the reader.
 */
export function maviAttention(context: AttentionContext): SectionAttention | null {
  const waiting = context.maviNeedsYou ?? 0;
  if (waiting > 0) {
    return {
      tone: "amber",
      count: waiting,
      reason: waiting === 1 ? "1 Mavi thread is waiting for your answer" : `${waiting} Mavi threads are waiting for your answer`,
    };
  }
  const failed = context.maviFailed ?? 0;
  if (failed > 0) {
    return {
      tone: "rose",
      count: null,
      reason: failed === 1 ? "A Mavi thread failed" : `${failed} Mavi threads failed`,
    };
  }
  return null;
}

/** The most urgent attention among a view's sections, for its collapsed pill. */
export function viewAttention(
  view: ViewTab,
  sections: readonly string[],
  context: AttentionContext,
): SectionAttention | null {
  if (view === "mavi") return maviAttention(context);
  for (const section of sections) {
    const found = sectionAttention(view, section, context);
    if (found) return found;
  }
  return null;
}
