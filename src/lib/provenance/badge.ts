/**
 * Turning a `ProvenanceFreshness` into the one thing a row can show.
 *
 * Pure, so the classification is testable without a repository, a component,
 * or a git binary. The rendering lives in `FreshnessBadge.svelte`; every
 * decision about *what* is being claimed lives here.
 */

import type { ProvenanceFreshness } from "./types";

/**
 * What a badge is claiming, in the order the classifier tests for it.
 *
 * The order is the contract, exactly as it is for policy verdicts: each arm
 * below is only reached because every arm above it did not apply. Reordering
 * them changes what the badge means. In particular `unknown` must stay first —
 * every other kind is a statement about the commit, and when the notes could
 * not be read there is no statement to make.
 */
export type FreshnessKind =
  | "unknown"
  | "failed"
  | "unrecognised"
  | "unmeasured"
  | "fresh"
  | "stale"
  | "agent"
  | "unverified";

/** How a kind should read at a glance. Never derived from the kind's name. */
export type FreshnessTone = "good" | "warn" | "bad" | "muted";

export interface FreshnessBadge {
  kind: FreshnessKind;
  tone: FreshnessTone;
  /** Short enough for a table cell. */
  label: string;
  /** The full sentence, for a tooltip or `title`. */
  detail: string;
}

/**
 * Verdict strings this build understands as a pass.
 *
 * A closed set on purpose. The alternative — treating everything that is not a
 * known failure as a pass — means the first writer to invent a new verdict
 * gets a green badge for it. `provenance-verdict-contract` holds this in step
 * with what GitPulse's own writer emits.
 */
export const PASS_VERDICTS: readonly string[] = ["passed", "pass", "ok", "green", "success"];

/** Verdict strings this build understands as a recorded failure. */
export const FAIL_VERDICTS: readonly string[] = [
  "failed",
  "fail",
  "error",
  "red",
  "failure",
  "blocked",
];

/** Confidence as a whole percentage, or null when it was never measured. */
export function confidencePercent(f: ProvenanceFreshness): number | null {
  if (f.confidence === null) return null;
  return Math.round(f.confidence * 100);
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? `1 ${one}` : `${n} ${many}`;
}

function verifiedBy(f: ProvenanceFreshness): string {
  const by = f.verification?.checked_by?.trim();
  return by ? ` by ${by}` : "";
}

/**
 * Classifies one commit's provenance.
 *
 * @param f the freshness record as the backend measured it
 */
export function freshnessBadge(f: ProvenanceFreshness): FreshnessBadge {
  // 1. Nothing could be established. This has to come first: every kind below
  //    is a claim about the commit, and here we have none to make. Letting it
  //    fall through to `unverified` would render a check that could not run
  //    identically to one that ran and found nothing recorded.
  if (!f.notes_readable) {
    return {
      kind: "unknown",
      tone: "muted",
      label: "unknown",
      detail:
        f.unmeasured_reason ||
        "this commit's provenance notes could not be read, so nothing is known about it",
    };
  }

  const verdict = f.verification?.verdict?.trim().toLowerCase() ?? "";

  if (f.verification) {
    // 2. A recorded failure outranks every question about freshness: how stale
    //    a failure is does not make it less of a failure.
    if (FAIL_VERDICTS.includes(verdict)) {
      return {
        kind: "failed",
        tone: "bad",
        label: "failed",
        detail: `verification recorded${verifiedBy(f)} as ${f.verification.verdict}`,
      };
    }

    // 3. A verdict this build has never heard of. Not a pass — a badge that
    //    goes green for an unrecognised word is a badge that goes green for
    //    the next word anyone invents.
    if (!PASS_VERDICTS.includes(verdict)) {
      return {
        kind: "unrecognised",
        tone: "warn",
        label: "unrecognised",
        detail:
          `this build does not recognise the verdict ${JSON.stringify(f.verification.verdict)}, ` +
          "so it is not read as a pass",
      };
    }

    // 4. Verified, but we cannot say how much has moved since. Distinct from
    //    fresh, which is the strongest claim available.
    if (f.distance === null) {
      return {
        kind: "unmeasured",
        tone: "warn",
        label: "verified · age unknown",
        detail:
          `verified${verifiedBy(f)}, but how far the base has moved since could not be ` +
          `measured: ${f.unmeasured_reason || "no reason was given"}`,
      };
    }

    // 5. Verified against exactly this tree.
    if (f.distance === 0) {
      return {
        kind: "fresh",
        tone: "good",
        label: "verified",
        detail: `verified${verifiedBy(f)}, and the base has not moved since`,
      };
    }

    // 6. Verified, decaying.
    const pct = confidencePercent(f);
    return {
      kind: "stale",
      tone: "warn",
      label: `verified · ${f.distance} behind`,
      detail:
        `verified${verifiedBy(f)}, but the base has gained ` +
        `${plural(f.distance, "commit", "commits")} since` +
        (pct === null ? "" : ` — confidence ${pct}%`),
    };
  }

  // 7. An agent wrote it and nobody checked it. Worth saying out loud: it is
  //    not the same as a commit nothing has ever touched.
  if (f.session) {
    return {
      kind: "agent",
      tone: "warn",
      label: "unverified · agent",
      detail:
        `written in agent session ${f.session.session_id}` +
        `${f.session.summary ? ` (${f.session.summary})` : ""}, with no verification recorded`,
    };
  }

  // 8. Nothing recorded, and we know that for a fact.
  return {
    kind: "unverified",
    tone: "muted",
    label: "unverified",
    detail: "no verification has been recorded for this commit",
  };
}

/**
 * True when a badge is worth drawing attention to in a dense list.
 *
 * `unverified` is the overwhelming majority of rows in any repository that has
 * only just started recording, so treating it as noteworthy would mean every
 * row shouts. Everything else is a state someone chose or a state we could not
 * establish, and both are worth seeing.
 */
export function isNoteworthy(badge: FreshnessBadge): boolean {
  return badge.kind !== "unverified";
}
