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
