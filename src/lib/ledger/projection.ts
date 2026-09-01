import type { AgentActionEntry } from "../agents/activity";
import type { PolicyVerdict } from "../stores/harnessStore";
import type { LedgerEvent } from "./types";

/**
 * Turns durable ledger rows into the journal entries the UI already renders.
 *
 * The journal used to *be* the record: a 200-entry array in browser memory,
 * erased by a reload and blind to anything that happened while the app was
 * closed. It is now a projection — the ledger on disk is the record, and this
 * is one view of it. Everything here is pure so the mapping can be tested
 * without a webview.
 */

/** Verbs the journal already knows, derived from a dotted ledger action. */
export function kindForAction(action: string): string {
  const [family, verb] = action.split(".", 2);
  if (!verb) return family || "action";
  switch (`${family}.${verb}`) {
    case "git.commit":
      return "commit";
    case "git.push":
      return "push";
    case "git.pull":
      return "pull";
    case "git.fetch":
      return "fetch";
    case "git.merge":
      return "merge";
    case "git.rebase":
      return "rebase";
    case "git.checkout":
    case "git.switch":
      return "checkout";
    case "git.branch":
      return "branch";
    case "git.add":
      return "stage";
    case "git.restore":
      return "discard";
    case "git.stash":
      return "stash";
    case "git.worktree":
      return "worktree";
    case "git.cherry_pick":
      return "cherry-pick";
    case "git.revert":
      return "revert";
    case "git.reset":
      return "reset";
    case "git.tag":
      return "tag";
    default:
      // A verb this build does not map still reaches the journal under its own
      // name. Dropping it would make the record quieter than the truth.
      return family === "file" ? "edit" : verb;
  }
}

/**
 * Parses the verdict a row carries.
 *
 * A row with no verdict returns null, which the journal renders as "no policy
 * decision" — deliberately not as an approval. Unparseable JSON returns null
 * for the same reason: a verdict we cannot read is not a verdict that passed.
 */
export function verdictOf(event: LedgerEvent): PolicyVerdict | null {
  if (!event.verdict_json) return null;
  try {
    return JSON.parse(event.verdict_json) as PolicyVerdict;
  } catch {
    return null;
  }
}

/**
 * True when this row records a gate decision rather than a completed action.
 *
 * A gate row with outcome `ok` means "the gate authorised this", not "this
 * succeeded". Callers that count successful operations must exclude them, and
 * the distinction is carried in the data rather than inferred from the action
 * name.
 */
export function isGateRow(event: LedgerEvent): boolean {
  if (!event.detail_json) return false;
  try {
    const detail = JSON.parse(event.detail_json) as { phase?: string };
    return detail.phase === "gate";
  } catch {
    return false;
  }
}

/** A human-readable target for the journal line. */
export function labelFor(event: LedgerEvent): string {
  return event.object ?? event.action;
}

/**
 * Projects one durable event into a journal entry.
 *
 * `ok` is false for anything the gate blocked or that ran and failed — the two
 * remain distinguishable through the event's own `outcome`, which the detail
 * view reads.
 */
export function eventToAction(event: LedgerEvent): AgentActionEntry {
  return {
    id: event.id,
    ts: Date.parse(event.ts_utc),
    kind: kindForAction(event.action),
    label: labelFor(event),
    ok: event.outcome === "ok",
    verdict: verdictOf(event),
  };
}

/**
 * Merges newly-read events into the journal, newest last, without duplicates.
 *
 * Ledger ids are monotonic, so a row already present is a re-read rather than a
 * new event — which happens whenever a notification and a poll race.
 */
export function mergeEvents(
  existing: AgentActionEntry[],
  events: LedgerEvent[],
  displayCap: number
): AgentActionEntry[] {
  if (events.length === 0) return existing;
  const seen = new Set(existing.map((e) => e.id));
  const merged = existing.slice();
  for (const event of events) {
    if (seen.has(event.id)) continue;
    seen.add(event.id);
    merged.push(eventToAction(event));
  }
  merged.sort((a, b) => a.id - b.id);
  // A *display* cap, not a data cap: the rows beyond it are still on disk and
  // still reachable by paging. That is the whole difference from the ring
  // buffer this replaced.
  return merged.length > displayCap ? merged.slice(merged.length - displayCap) : merged;
}
