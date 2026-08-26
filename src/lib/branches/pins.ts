/**
 * Pure pin-list persistence for BranchList.
 *
 * localStorage content is hostile input: it can hold another repo's shape,
 * hand-edited garbage, or JSON that explodes on parse. Every helper here is
 * total — no input can throw — so the component's load path can apply the
 * parsed result unconditionally. That unconditional application is the fix
 * for the cross-repo pin leak: when a repo has NO stored entry, parsing must
 * yield an empty set that overwrites whatever pins the previous repo left
 * in state, instead of silently keeping them and later persisting them into
 * the new repo's key.
 *
 * Ordering contract: both directions sort with default lexicographic order
 * after deduplication, so `parse(serialize(x))` round-trips to an identical
 * value and callers can cheaply compare serializations for identity-stable
 * state updates.
 */

/** Storage key for a repo's pinned branch names (`gitpulse:pinned:<path>`). */
export function pinnedKey(repoPath: string): string {
  return `gitpulse:pinned:${repoPath}`;
}

/**
 * Parse stored pin JSON into a sorted, deduped list of branch names.
 * null/undefined, non-array JSON (objects, bare strings, numbers), arrays
 * containing non-string entries, and unparseable garbage all fail closed
 * to [] rather than throwing or leaking partial data. Deeply nested arrays
 * can exhaust the stack inside JSON.parse; the try/catch absorbs that too.
 */
export function parsePinned(raw: string | null | undefined): string[] {
  if (typeof raw !== "string" || raw.length === 0) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const names = new Set<string>();
  for (const item of parsed) {
    if (typeof item === "string") names.add(item);
  }
  return [...names].sort();
}

/** Sorted, deduped JSON serialization of a pin list (inverse of parsePinned). */
export function serializePinned(names: Iterable<string>): string {
  const unique = new Set<string>();
  for (const name of names) {
    if (typeof name === "string") unique.add(name);
  }
  return JSON.stringify([...unique].sort());
}
