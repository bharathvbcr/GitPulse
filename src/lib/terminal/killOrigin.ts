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
 * The session a close was refused for because it is protected, or null when
 * `error` is anything else. The backend answers `cmd_terminal_kill` with a
 * plain string naming the session (`skipped_protected_message`), so this is the
 * one place that reads it; a refusal is the session being spared, not a
 * failure to close it.
 */
export function protectedSkipOf(error: unknown): string | null {
  const message = typeof error === "string" ? error : error instanceof Error ? error.message : null;
  if (!message) return null;
  return /session (\S+) is protected and was not closed by /.exec(message)?.[1] ?? null;
}
