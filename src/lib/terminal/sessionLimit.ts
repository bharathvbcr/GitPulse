/**
 * How many terminal sessions may be open at once, across every repository.
 *
 * The user's choice (Settings → Agents → Terminal sessions open at once),
 * stored as `max_terminal_sessions` with the other agent defaults and held
 * live by the backend's session registry, which is what actually refuses.
 * This store mirrors it so the UI stops at the same number: a UI that stopped
 * short would make a reachable session unreachable, and one that ran past it
 * would surface the backend's refusal as an unexplained spawn error.
 *
 * `agentDefaultsStore` sets it whenever agent defaults are read or saved;
 * until then it is the default, which is also what the backend starts with
 * when nothing is stored. `tabs.contract.test.ts` reads both Rust constants
 * and fails if these drift from them.
 */
import { get, writable, type Readable } from "svelte/store";

/** Sessions open at once when nothing is stored (`DEFAULT_PTY_SESSIONS`). */
export const DEFAULT_TERMINAL_SESSIONS = 32;
/** The most a user may choose (`MAX_PTY_SESSIONS`). */
export const MAX_TERMINAL_SESSIONS = 128;

/** Whether `value` is a limit the backend accepts: a whole number in 1..=ceiling. */
export function isTerminalSessionLimit(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= MAX_TERMINAL_SESSIONS;
}

const limit = writable(DEFAULT_TERMINAL_SESSIONS);

export const terminalSessionLimit: Readable<number> = { subscribe: limit.subscribe };

/** The limit right now, for code outside a reactive context. */
export function currentSessionLimit(): number {
  return get(limit);
}

/** Adopts the stored limit; anything the backend would not accept means the default. */
export function setTerminalSessionLimit(value: unknown): void {
  limit.set(isTerminalSessionLimit(value) ? value : DEFAULT_TERMINAL_SESSIONS);
}
