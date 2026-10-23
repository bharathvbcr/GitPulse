/**
 * Wire types for the durable action ledger.
 *
 * These live in their own module rather than beside the component that renders
 * them because `wire-type-locality-contract` forbids snake_case payload shapes
 * inside `.svelte` files: `check:types` cannot reach them there, and every
 * instance previously found had already drifted from its Rust counterpart.
 *
 * Mirrors `src-tauri/src/ledger/mod.rs`; held in lockstep by the `ledger`
 * contract in `scripts/check-coverage-types.mjs`.
 */

/** One durable event, exactly as `ledger::Event` serializes it. */
export interface LedgerEvent {
  id: number;
  ulid: string;
  ts_utc: string;
  schema_version: number;
  repo_path: string;
  worktree_path: string | null;
  actor_kind: string;
  actor_id: string | null;
  session_id: string | null;
  task_id: string | null;
  action: string;
  object: string | null;
  argv_json: string | null;
  outcome: string;
  verdict_json: string | null;
  before_ref: string | null;
  after_ref: string | null;
  duration_ms: number | null;
  detail_json: string | null;
}

/**
 * Whether the ledger is recording, and what is known to be missing if not.
 *
 * The UI must be able to say "this history is incomplete". A repository whose
 * ledger cannot be opened returns no events, and no events is exactly what a
 * repository with nothing in it returns.
 *
 * Extends `Freshness` because Rust flattens it into the same object: the
 * freshness fields sit beside `recording`, not under a key of their own. The
 * app's creating status does not read freshness and carries the "not read"
 * default (`stale: true`, `freshness_ok: false`).
 */
export interface LedgerStatus extends Freshness {
  /** True only when this process is writing the ledger; never from a read-only open. */
  recording: boolean;
  /** `recording`, `readable` (opened read-only; who records is not known) or `unknown`. */
  recording_state: string;
  path: string;
  /** Appends dropped by THIS process; see `dropped_scope`. */
  dropped: number;
  /** What `dropped` counts: `this_process`. */
  dropped_scope: string;
  /** Bind/unbind writes whose catch-up failed or was cut short; not a dropped append. */
  catch_up_failures?: number;
  /** Sibling-worktree bind/unbind rows the ledger keeps as history only. */
  history_only_bindings: number;
  error: string;
  error_code: string;
}

/** Mirrors `ledger::freshness::Freshness`. */
export interface Freshness {
  as_of: string | null;
  last_catch_up: string | null;
  last_catch_up_basis: string;
  stale: boolean;
  stale_reason: string;
  stale_after_secs: number;
  freshness_ok: boolean;
  freshness_error: string;
  catch_up: CatchUpSummary | null;
}

/** Mirrors `ledger::freshness::CatchUpSummary`. */
export interface CatchUpSummary {
  worktrees: number;
  truncated: boolean;
  recorded: number;
  transcripts: number;
  transcripts_listed: number;
  transcripts_unlisted: number;
  skipped_lines: number;
  failed_records: number;
  error: string;
  stale_worktrees: number;
  future_dated_worktrees: number;
}

/** The freshness block of a ledger that was not read: stale, with the reason. */
export const FRESHNESS_NOT_READ: Freshness = {
  as_of: null,
  last_catch_up: null,
  last_catch_up_basis: "none",
  stale: true,
  stale_reason: "freshness was not read for this answer",
  stale_after_secs: 600,
  freshness_ok: false,
  freshness_error: "freshness was not read for this answer",
  catch_up: null,
};

/**
 * A complete `LedgerStatus` from the fields a caller actually knows. The rest
 * are what the app's creating status reports: `recording_state` follows
 * `recording`, drops are this process's, and freshness was not read.
 */
export function ledgerStatus(
  known: Pick<LedgerStatus, "recording" | "path" | "dropped" | "error" | "error_code">,
): LedgerStatus {
  return {
    ...FRESHNESS_NOT_READ,
    ...known,
    recording_state: known.recording ? "recording" : "unknown",
    dropped_scope: "this_process",
    history_only_bindings: 0,
  };
}

/** Payload of the `ledger-appended` event. */
export interface LedgerAppended {
  repo_path: string;
  cursor: number;
}

/**
 * The three outcomes the schema's CHECK constraint permits.
 *
 * Named to mirror the Rust enum exactly, which is what
 * `enum-variant-contract` compares. Module-scoped, so the generic name costs
 * nothing at the call sites that import it.
 */
export type Outcome = "ok" | "failed" | "blocked";

/** The three actor kinds the schema's CHECK constraint permits. */
export type ActorKind = "human" | "agent" | "system";

