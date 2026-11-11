/**
 * Notices a terminal session start can end with, in one dependency-free module.
 *
 * The launch queue, the agent browser queue and the session lifecycle all say
 * the same thing about a start that is still under way. They import it from
 * here instead of each keeping a copy that drifts, and a module installed at
 * boot (`browseRequests`) can import it without pulling the launch queue into
 * the entry chunk. Tests compare against the constants, never the prose.
 */

/** Refused while another terminal session is still waiting for the panel to claim it. */
export const TERMINAL_SESSION_PENDING = "Another terminal session is already starting.";

/** A task run's terminal whose spawn is still unresolved past its watchdog; Reconnect waits for the same run rather than starting a second one. */
export const TASK_TERMINAL_STARTING = "The task's terminal session is still starting. Reconnect waits for this same run.";
