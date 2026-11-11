/**
 * Which of an attempt card's buttons may be pressed while an action runs.
 *
 * The Agents pane used to hold one `busy` flag, so a merge that took seconds
 * on one attempt disabled Stop agent, Cancel preparation and Stop managed run
 * everywhere, with nothing saying which row was working. Now each card knows
 * its own in-flight action (`inflight`, run id to the verb it shows), and one
 * serialisation slot (`holder`, a run id) covers the actions that write git or
 * the checkout. Stopping and opening things are never held behind it.
 */

/** Run id to the verb its card shows while that attempt's action is in flight. */
export type InflightActions = Readonly<Record<string, string>>;

/**
 * Whether `runId`'s button may be pressed.
 *
 * - A card with an action in flight is locked, mutating or not.
 * - A mutating action (merge, discard, release, resume, trust) is also locked
 *   while another attempt holds the slot; a non-mutating one (stop, cancel,
 *   show, view) never is.
 */
export function canAct(runId: string, inflight: InflightActions, holder: string | null, mutates: boolean): boolean {
  if (inflight[runId]) return false;
  if (mutates && holder !== null && holder !== runId) return false;
  return true;
}

/** What the card says while another attempt holds the slot, or null when none does. */
export function slotHeldNote(runId: string, inflight: InflightActions, holder: string | null): string | null {
  if (holder === null || holder === runId) return null;
  const verb = inflight[holder];
  return verb ? `Waiting: another run is ${verb.toLowerCase()}.` : "Waiting: another run's action is running.";
}
