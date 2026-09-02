/**
 * Wire types for the harness's grant ledger.
 *
 * A grant is how a soft denial becomes an allow: someone argued for a write the
 * plan did not authorise, and the gate recorded that they did. The verdict's
 * `granted` status says one was applied; these say who, why, and until when.
 *
 * Read-only. Revocation mutates state Manvi owns and is deliberately not
 * exposed here — see `src-tauri/src/grants/mod.rs`.
 *
 * Mirrors `src-tauri/src/grants/mod.rs`.
 */

/** Who issued a grant. */
export interface Grantor {
  authority: string;
  name: string;
}

/** What a grant covers. */
export interface GrantScope {
  rule: string;
  target: string;
  task_id: string;
}

/** One recorded override. */
export interface Grant {
  id: string;
  grantor: Grantor;
  reason: string;
  scope: GrantScope;
  issued_at: string;
  expires_at: string;
  /** True once the grant has been spent on a decision. */
  consumed: boolean;
}

/** The grant ledger as GitPulse can see it. */
export interface GrantView {
  /** False when this repository has no grant ledger — the ordinary case. */
  available: boolean;
  path: string;
  grants: Grant[];
  /**
   * Empty when the ledger was read; otherwise why it could not be.
   *
   * Separate from `available` so a ledger that exists and could not be parsed
   * never renders as a repository where nothing was ever granted.
   */
  error: string;
}
