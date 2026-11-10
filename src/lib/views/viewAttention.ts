import type { ViewTab } from "../repos/persist";
import type { PolicyStatus } from "../stores/harnessStore";

/**
 * What a section needs from the reader right now, for the header switcher.
 *
 * The switcher collapses every view but the active one into a single pill,
 * and a collapsed pill must never hide a problem: a parked merge was never
 * allowed to be quieter than it was when Resolve had its own header tab, and
 * a blocked MANVI verdict is the same kind of fact. Both now belong to Policy,
 * which holds the conflict editor since Resolve folded into it. So the facts that already
 * exist are carried onto the section that owns them, and from there onto the
 * view's pill. Nothing here is fetched for the switcher's sake.
 */
export interface AttentionContext {
  /** Files still carrying conflict markers. */
  conflictedCount: number;
  /** Status of the last MANVI verdict, or null when none was recorded. */
  verdictStatus: PolicyStatus | null;
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
  if (view !== "work" || section !== "policy") return null;
  // Policy holds the conflict editor (Resolve folded into it), and the editor
  // leads the pane while files carry markers, so the count outranks a blocked
  // verdict: it is the thing the reader lands on.
  if (context.conflictedCount > 0) {
    const n = context.conflictedCount;
    return {
      tone: "rose",
      count: n,
      reason: n === 1 ? "1 file still carries conflict markers" : `${n} files still carry conflict markers`,
    };
  }
  if (context.verdictStatus === "blocked") {
    return { tone: "rose", count: null, reason: "MANVI blocked the last action" };
  }
  return null;
}

/** The most urgent attention among a view's sections, for its collapsed pill. */
export function viewAttention(
  view: ViewTab,
  sections: readonly string[],
  context: AttentionContext,
): SectionAttention | null {
  for (const section of sections) {
    const found = sectionAttention(view, section, context);
    if (found) return found;
  }
  return null;
}
