/**
 * Fold a composed blast radius into one line a human can read at a glance.
 *
 * The panel used to print the kernel's own prose: a 50-word essay about
 * unresolved attribution sites, beside fully-qualified `path::Type.member`
 * strings clipped on the right so the symbol name — the only part anyone
 * reads — was the part that got cut. That is a correct answer delivered in a
 * form nobody consumes, which in practice is not an answer.
 *
 * What is compressed here is the *explanation*. The *qualification* is not:
 * `headline` always states whether the number is exact, a floor, a ceiling or
 * merely approximate, because a bounded answer wearing an exact answer's
 * clothes is the specific failure this module exists to prevent. The full
 * kernel prose survives verbatim in `detail` for the reader who expands it.
 */

import type { ComposedBlastRadius } from "./blastCompose";
import { firstClause, summarizeWalkIncomplete } from "./walkIncomplete";

/**
 * How the total relates to the truth.
 *
 * `exact` is the only value that promises a number; every other value tells
 * the reader which direction the error runs. `partial` outranks the bound
 * words because a cancelled walk did not merely stop early — it never ran for
 * some seeds, so the shape of the miss is unknown rather than one-sided.
 */
export type BlastConfidence =
  | "unavailable"
  | "partial"
  | "floor"
  | "ceiling"
  | "approximate"
  | "exact";

export interface BlastGlance {
  /** Plain-language sentence. Always carries the qualification. */
  headline: string;
  /** Total as composed, or null when there is no number to report. */
  impacted: number | null;
  /** Deepest hop band that came back. */
  hops: number;
  confidence: BlastConfidence;
  /**
   * Short phrases naming *why* the answer is qualified, derived from the
   * structured fields — never guessed from prose. Empty when nothing
   * qualifies the answer.
   */
  caveats: string[];
  /** Folded kernel prose, for the expanded view. Null when there is none. */
  detail: string | null;
  /** True when the reader must not take the number at face value. */
  qualified: boolean;
}

function plural(n: number, one: string, many = `${one}s`): string {
  return n === 1 ? one : many;
}

export function blastGlance(blast: ComposedBlastRadius | null | undefined): BlastGlance {
  if (!blast) {
    return {
      headline: "No impact answer yet.",
      impacted: null,
      hops: 0,
      confidence: "unavailable",
      caveats: [],
      detail: null,
      qualified: true,
    };
  }

  const hops = blast.layers.length;

  if (!blast.available) {
    const cause = firstClause(blast.reason);
    return {
      headline: "Impact could not be measured — this is not the same as no impact.",
      impacted: null,
      hops,
      confidence: "unavailable",
      caveats: cause ? [cause] : [],
      detail: summarizeWalkIncomplete([blast.reason, blast.walk_incomplete]),
      qualified: true,
    };
  }

  const caveats: string[] = [];

  // Under-counting: every one of these means symbols exist that this answer
  // does not name. Cancellation is tracked apart because it is the only one
  // that leaves the *shape* of the miss unknown.
  const cancelled = blast.cancelled_seeds > 0;
  if (cancelled) {
    caveats.push(
      `${blast.cancelled_seeds} ${plural(blast.cancelled_seeds, "file")} stopped before the walk finished`,
    );
  }

  const refused = blast.unavailable_seeds.length - blast.cancelled_seeds;
  if (refused > 0) {
    caveats.push(`${refused} ${plural(refused, "file")} could not be walked`);
  }

  if (blast.unmatched_targets.length > 0) {
    const n = blast.unmatched_targets.length;
    caveats.push(`${n} ${plural(n, "file")} not in the index`);
  }

  const walkClause = firstClause(blast.walk_incomplete);
  if (blast.walk_incomplete) {
    caveats.push(walkClause ?? "the graph walk reported it did not finish");
  }

  if (blast.layers_truncated) {
    caveats.push("the hop list was cut to fit");
  }

  const underCounts =
    cancelled ||
    refused > 0 ||
    blast.unmatched_targets.length > 0 ||
    Boolean(blast.walk_incomplete) ||
    blast.layers_truncated;

  // Over-counting runs the other way: per-seed walks are summed, so a symbol
  // reachable from two changed files is counted twice. Reporting only "at
  // least" when both hold would assert a floor the number does not have.
  const overCounts = blast.overlap_possible;
  if (overCounts) {
    caveats.push("symbols reached from several files are counted once per file");
  }

  const confidence: BlastConfidence = cancelled
    ? "partial"
    : underCounts && overCounts
      ? "approximate"
      : underCounts
        ? "floor"
        : overCounts
          ? "ceiling"
          : "exact";

  return {
    headline: headlineFor(confidence, blast.total_impacted, hops),
    impacted: blast.total_impacted,
    hops,
    confidence,
    caveats,
    detail: summarizeWalkIncomplete([blast.walk_incomplete, blast.reason]),
    qualified: confidence !== "exact",
  };
}

/**
 * The sentence itself.
 *
 * Every branch but `exact` puts its hedging word *before* the number, so the
 * qualification cannot be lost by clipping the tail of the line.
 */
function headlineFor(confidence: BlastConfidence, total: number, hops: number): string {
  const reach = hops > 0 ? ` within ${hops} ${plural(hops, "hop")}` : "";

  if (total === 0) {
    return confidence === "exact"
      ? "Nothing else in the index references this change."
      : `No references found${reach}, but the search was incomplete.`;
  }

  const n = total.toLocaleString();
  const thing = plural(total, "symbol");
  switch (confidence) {
    case "partial":
      // Hedge first: "Reaches 12 symbols within 2 hops so far" clipped at the
      // dash reads as a finished count, which is the whole failure.
      return `Interrupted after reaching ${n} ${thing}${reach} — more remain.`;
    case "floor":
      return `Reaches at least ${n} ${thing}${reach}.`;
    case "ceiling":
      return `Reaches up to ${n} ${thing}${reach}.`;
    case "approximate":
      return `Reaches roughly ${n} ${thing}${reach}.`;
    case "unavailable":
      return "Impact could not be measured — this is not the same as no impact.";
    case "exact":
      return `Reaches ${n} ${thing}${reach}.`;
  }
}

/**
 * A count with its hedge attached, for a chip too small to hold a sentence.
 *
 * Shares the headline's vocabulary on purpose. The diff header used to print
 * a bare "2636 affected callers" beside a separate amber "walk incomplete",
 * which reads as two unrelated facts; "at least 2,636 callers" is both shorter
 * and harder to misread, and keeping the wording in one module is what stops
 * the chip and the panel from describing the same walk in different words.
 */
export function hedgedCount(count: number, qualified: boolean, noun: string): string {
  const n = count.toLocaleString();
  const thing = plural(count, noun);
  return qualified ? `at least ${n} ${thing}` : `${n} ${thing}`;
}

/** One word for the confidence chip beside the headline. */
export function confidenceLabel(confidence: BlastConfidence): string {
  switch (confidence) {
    case "unavailable":
      return "unavailable";
    case "partial":
      return "interrupted";
    case "floor":
      return "at least";
    case "ceiling":
      return "at most";
    case "approximate":
      return "approximate";
    case "exact":
      return "complete";
  }
}
