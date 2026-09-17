/**
 * The one vocabulary every delivery source is reduced to.
 *
 * GitHub Actions and Firebase App Hosting each have their own state words —
 * `in_progress`/`completed`+`conclusion` on one side, `PROGRESSING`/`SUCCEEDED`
 * on the other — and those vocabularies stay where they belong, in
 * `github/runLifecycle.ts` and `firebase/rolloutState.ts`. What the polling,
 * transition detection and timeline need is narrower: is this thing still
 * moving, did it end well, did it end badly, or can we not tell?
 *
 * Four cases, not three. `unknown` exists because both sources can hand us a
 * state this build has never heard of, and a state we cannot judge must not be
 * folded into either verdict: counted as success it hides a breakage, counted
 * as failure it invents one, and counted as in-flight it polls forever.
 */
export type MonitorPhase = "in_flight" | "settled_ok" | "settled_bad" | "unknown";

/** Whether this phase is still moving, and so worth polling for. */
export function isInFlight(phase: MonitorPhase): boolean {
  return phase === "in_flight";
}

/**
 * Whether this phase has stopped moving.
 *
 * `unknown` counts as settled, deliberately. It is the phase we cannot judge,
 * and treating it as in-flight would keep the poll timer alive forever on a
 * state that will never change into one we recognise.
 */
export function isSettled(phase: MonitorPhase): boolean {
  return phase !== "in_flight";
}

/**
 * Whether this phase is a verdict — something that can be counted in a rate.
 *
 * Only `settled_ok` and `settled_bad` qualify. Anything else has no place in a
 * numerator or a denominator, which is what keeps a pass rate from being
 * diluted by rows we could not read.
 */
export function isVerdict(phase: MonitorPhase): boolean {
  return phase === "settled_ok" || phase === "settled_bad";
}
