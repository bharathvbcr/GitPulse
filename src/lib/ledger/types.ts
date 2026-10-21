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
 */
export interface LedgerStatus {
  /** True only when this process is writing the ledger; see `recording_state`. */
  recording: boolean;
  /**
   * `readable` is a ledger opened read-only (rows readable, writer unknown);
   * `unknown` is no ledger, or one that could not be opened.
   */
  recording_state?: "recording" | "readable" | "unknown";
  path: string;
  /** Appends dropped by this process only; see `dropped_scope`. */
  dropped: number;
  dropped_scope?: "this_process";
  /** `ts_utc` of the newest row the producer could see; null when none. */
  as_of?: string | null;
  last_catch_up?: string | null;
  last_catch_up_basis?: "catch_up_state" | "newest_row" | "none";
  /** No evidence of a recorder within `stale_after_secs`, or none at all. */
  stale?: boolean;
  stale_reason?: string;
  stale_after_secs?: number;
  freshness_ok?: boolean;
  freshness_error?: string;
  /** Bind/unbind writes whose catch-up failed or was cut short; not a dropped append. */
  catch_up_failures?: number;
  error: string;
  error_code: string;
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

