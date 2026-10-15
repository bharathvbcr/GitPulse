import type { MutationOutcome } from "../repos/storeTypes";
import { toastStore } from "./toastStore";

/**
 * Says a mutation's failure when nobody else has.
 *
 * `runMutating` files a failure on the session it ran against, and the app
 * shows the active session's error as a toast. A refusal (the target changed)
 * and a failure in a background tab never reach that toast, so they are named
 * here — once: a failure that was already surfaced is left to the app's own
 * toast rather than doubled.
 *
 * Returns whether the mutation succeeded, so a caller reads
 * `if (!reportFailure(outcome, "Could not …")) return;` before its own success
 * message.
 */
export function reportFailure(outcome: MutationOutcome, what: string): boolean {
  if (outcome.ok) return true;
  if (!outcome.surfaced) toastStore.error(`${what}: ${outcome.error ?? "it did not complete."}`);
  return false;
}
