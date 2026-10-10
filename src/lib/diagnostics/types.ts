/**
 * The backend's durable log, as returned by `cmd_diagnostic_persisted_log`.
 *
 * Mirrors `PersistedLog` in src-tauri/src/logging.rs; the pairing is pinned by
 * `npm run check:types`.
 */
export interface PersistedLog {
  /** The live log file, or "" when this build keeps none. */
  path: string;
  /** Tail of the durable record, oldest line first, spanning both generations. */
  lines: string[];
  /** Why the record is incomplete, when it is; null when nothing is missing. */
  degraded: string | null;
}

/**
 * What a failed read looks like.
 *
 * The section is written even when the log cannot be fetched, because an
 * omitted section and a genuinely quiet backend are indistinguishable to
 * whoever reads the report — and only one of them means nothing went wrong.
 */
export function unreadablePersistedLog(reason: string): PersistedLog {
  return { path: "", lines: [], degraded: `could not be read: ${reason}` };
}

/**
 * A background thread that panicked and is being retried, as
 * `cmd_degraded_subsystems` returns it. Mirrors `Degraded` in
 * src-tauri/src/supervisor.rs; the pairing is pinned by `npm run check:types`.
 */
export interface Degraded {
  subsystem: string;
  /** The panic's message, one bounded printable line. */
  reason: string;
  since_unix: number;
  /** Ticks that have panicked since the record was made. */
  panics: number;
  /** The thread is gone and nothing is retrying it. */
  stopped: boolean;
}

/**
 * The attribution daemon, as `cmd_daemon_status` returns it. Mirrors
 * `DaemonStatus` in src-tauri/src/daemon_lock.rs.
 */
export interface DaemonStatus {
  running: boolean;
  pid: number | null;
  version: string | null;
  app_version: string;
  /** A daemon is running and is not this build. */
  mismatch: boolean;
  /** Set when the lock could not be examined, which is not "not running". */
  error: string | null;
}
