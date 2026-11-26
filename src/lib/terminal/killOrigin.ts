/**
 * The route a terminal close came by: the optional `origin` argument of
 * `cmd_terminal_kill`, mirrored from `terminal::lifecycle::KillOrigin`.
 *
 * Omitting it is `unspecified` and is treated as a direct close of one tab.
 * A protected session survives `batch_close`, `stop_children`, `agent_close`
 * and `load_governor`; a direct close (`tab_close`, `sessions_list_stop`,
 * `unspecified`) and quitting with "stop everything" still stop it.
 */
export type KillOrigin =
  | "tab_close"
  | "batch_close"
  | "sessions_list_stop"
  | "stop_children"
  | "agent_close"
  | "agent_exec_cleanup"
  | "load_governor"
  | "quit_stop_all"
  | "unspecified";

/**
 * What `cmd_terminal_kill` answers, mirrored from `terminal::lifecycle::KillOutcome`.
 * A protected session a batch route spared is `skipped_protected`: a result,
 * not an error, so nothing reads a message to learn it.
 */
export type KillOutcome = "killed" | "skipped_protected";

/** A close the backend declined because the session is protected from that route. */
export class ProtectedSkip extends Error {
  constructor(readonly sessionId: string) {
    super(`session ${sessionId} is protected and was not closed; close its tab directly to stop it`);
    this.name = "ProtectedSkip";
  }
}

/**
 * Turns the backend's answer into the close's result: returns when the session
 * was closed and throws `ProtectedSkip` when it was spared, so a caller's
 * `catch` tells the two apart by type (`protectedSkipOf`).
 */
export function settleKill(sessionId: string, outcome: KillOutcome): void {
  if (outcome === "skipped_protected") throw new ProtectedSkip(sessionId);
}

/**
 * The session a close was spared for because it is protected, or null when
 * `error` is anything else. A spared session is not a failure to close it.
 */
export function protectedSkipOf(error: unknown): string | null {
  return error instanceof ProtectedSkip ? error.sessionId : null;
}
