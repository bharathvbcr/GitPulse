import type { Grant } from "./types";

export type GrantLifecycle = "active" | "used" | "expired" | "unknown";

/**
 * Mirrors MANVI's `Grant.Active`: consumed grants are never active, and an
 * unconsumed grant is active only before its expiry. An invalid or absent
 * timestamp stays visibly unknown instead of being promoted to active.
 */
export function grantLifecycle(
  grant: Grant,
  now: number = Date.now(),
): GrantLifecycle {
  if (grant.consumed) return "used";
  const expiresAt = Date.parse(grant.expires_at);
  if (!Number.isFinite(expiresAt)) return "unknown";
  return now < expiresAt ? "active" : "expired";
}

/** Active grants in the same newest-first order used by the ledger UI. */
export function activeGrants(
  grants: readonly Grant[],
  now: number = Date.now(),
): Grant[] {
  return grants
    .filter((grant) => grantLifecycle(grant, now) === "active")
    .slice()
    .reverse();
}
