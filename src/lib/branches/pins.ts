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
 *
 * Bounded footprint: per-repo blobs are additionally listed in a
 * most-recently-used index (PINNED_INDEX_KEY), so stale repositories can be
 * discovered and evicted without knowing their paths up front — the same
 * cap-and-evict discipline storage/history.ts applies to scan snapshots
 * with MAX_REPOS_WITH_HISTORY. The index is MRU-first and deliberately NOT
 * sorted: position encodes recency, unlike the sorted pin blobs themselves,
 * so an index entry must never round-trip through a sorting serializer.
 */

import type { StorageLike } from "../repos/persist";

/** Storage key for a repo's pinned branch names (`gitpulse:pinned:<path>`). */
export function pinnedKey(repoPath: string): string {
  return `gitpulse:pinned:${repoPath}`;
}

/** MRU index of repo paths that have pin blobs; newest first. */
export const PINNED_INDEX_KEY = "gitpulse:pinned-index:v1";

/**
 * Repositories we keep pin blobs for; when the index saturates,
 * prunePinnedIndex deletes the least-recently-used repos' blob keys.
 */
export const MAX_PINNED_REPOS = 64;

/**
 * Parse persisted index JSON into a deduped MRU-ordered list of repo paths.
 * Same fail-closed posture as parsePinned: non-array JSON, non-string or
 * empty entries, duplicates, and unparseable garbage never throw — hostile
 * entries are skipped, and for duplicates only the FIRST occurrence survives
 * because index position encodes recency.
 */
export function parseIndex(raw: string | null | undefined): string[] {
  if (typeof raw !== "string" || raw.length === 0) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of parsed) {
    if (typeof item !== "string" || item.length === 0 || seen.has(item)) continue;
    seen.add(item);
    out.push(item);
  }
  return out;
}

/** Moves `repoPath` to the front of the MRU index without mutating input. */
export function touchIndex(index: readonly string[], repoPath: string): string[] {
  return [repoPath, ...index.filter((path) => path !== repoPath)];
}

/** Reads and validates the persisted MRU index. Missing/corrupt → []. */
export function loadPinnedIndex(storage: StorageLike | null): string[] {
  if (!storage) return [];
  let raw: string | null;
  try {
    raw = storage.getItem(PINNED_INDEX_KEY);
  } catch {
    return [];
  }
  return parseIndex(raw);
}

function savePinnedIndex(storage: StorageLike | null, paths: readonly string[]): void {
  if (!storage) return;
  try {
    storage.setItem(PINNED_INDEX_KEY, JSON.stringify([...paths]));
  } catch {
    /* quota / private mode — best-effort; the next prune re-trims */
  }
}

/**
 * Stores one repo's serialized pin blob (callers build it with
 * serializePinned so serialization stays in one place) and, ONLY when that
 * write lands, bumps the repo to the front of the MRU index. The index is
 * not capped here on purpose: trimming before prunePinnedIndex runs would
 * orphan overflow blobs beyond discovery — exactly the unbounded
 * accumulation this module exists to bound. Returns whether the blob was
 * persisted; quota/private-mode failures fail closed like every writer here.
 */
export function saveRepoPins(
  storage: StorageLike | null,
  repoPath: string,
  serializedPins: string,
): boolean {
  if (!storage || !repoPath) return false;
  try {
    storage.setItem(pinnedKey(repoPath), serializedPins);
  } catch {
    /* quota / private mode — in-memory state still serves this session */
    return false;
  }
  const touched = touchIndex(loadPinnedIndex(storage), repoPath);
  savePinnedIndex(storage, touched);
  return true;
}

/**
 * Bounds the total pin-blob footprint: keeps the MAX_PINNED_REPOS most
 * recently used entries, deletes the evicted repos' `gitpulse:pinned:<path>`
 * keys, drops entries whose blob has already vanished, and persists the
 * trimmed index. A corrupt/unreadable index degrades to a no-op rather than
 * mass-deleting blobs it could not understand. Never throws: an eviction
 * whose removeItem fails stays listed in the index (an entry leaves only
 * once its blob is confirmed gone), so quota failures merely defer to the
 * next prune instead of stranding undiscoverable blobs.
 */
export function prunePinnedIndex(storage: StorageLike | null): void {
  if (!storage) return;
  const index = loadPinnedIndex(storage);
  // Nothing tracked (or corrupt): no trustworthy recency order exists, so
  // never guess at what is safe to delete.
  if (index.length === 0) return;

  const kept = index.slice(0, MAX_PINNED_REPOS);
  const evicted = index.slice(MAX_PINNED_REPOS);

  const alive: string[] = [];
  for (const path of kept) {
    let raw: string | null;
    try {
      raw = storage.getItem(pinnedKey(path));
    } catch {
      // Unreadable ≠ absent: keep the entry rather than destroy pins we
      // failed to verify; the next prune re-checks.
      alive.push(path);
      continue;
    }
    if (raw !== null) alive.push(path);
  }

  const deleteFailed: string[] = [];
  for (const path of evicted) {
    try {
      storage.removeItem(pinnedKey(path));
    } catch {
      deleteFailed.push(path);
    }
  }

  const next = [...alive, ...deleteFailed];
  if (next.length !== index.length) {
    savePinnedIndex(storage, next);
  }
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
