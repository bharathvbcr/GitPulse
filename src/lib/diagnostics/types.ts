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
