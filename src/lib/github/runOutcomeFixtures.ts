/**
 * Test fixtures' way to give a run the outcome Rust would have given it.
 *
 * The classifier lives in `src-tauri/src/github/outcome.rs`. Its test holds
 * that file's cases to the classifier, and this reads the same cases, so a
 * fixture built from `status` and `conclusion` carries the real answer and
 * not a second opinion written beside it. A pair the cases do not list is
 * `unknown`, which is also what the classifier says of a word it does not know.
 * Nothing in the application imports this.
 */
import cases from "../../../src-tauri/src/github/run_outcome_cases.json";
import { asRunOutcome, type RunOutcome } from "./types";

const OUTCOMES = new Map<string, RunOutcome>(
  cases.map((entry) => [`${entry.status}\u0000${entry.conclusion}`, asRunOutcome(entry.outcome)]),
);

/** The outcome for the two fields, after the trimming and case-folding the classifier does. */
export function outcomeFor(status: string | undefined, conclusion: string | undefined): RunOutcome {
  const key = `${(status ?? "").trim().toLowerCase()}\u0000${(conclusion ?? "").trim().toLowerCase()}`;
  return OUTCOMES.get(key) ?? "unknown";
}
