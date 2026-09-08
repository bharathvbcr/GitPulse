/**
 * Stable, collision-free keys for Svelte `{#each}` blocks.
 *
 * Svelte 5 throws `each_key_duplicate` when an explicit key repeats. Lists that
 * look unique in the producer (repo map samples, notices, branch rows) still
 * arrive with duplicates in the wild — a second claimant on a key used to take
 * the whole pane down. Suffix collisions instead of crashing.
 */

/** Returns a function that yields a unique string for each base key. */
export function uniqueKeyAllocator(): (base: string) => string {
  const seen = new Map<string, number>();
  const allocated = new Set<string>();
  return (base) => {
    const key = base.length > 0 ? base : "__empty";
    let count = seen.get(key) ?? 0;
    let candidate = count === 0 ? key : `${key}#${count}`;
    // A literal input can equal a suffix generated for another base. Track
    // emitted keys too; a per-base counter alone cannot guarantee uniqueness.
    while (allocated.has(candidate)) {
      count += 1;
      candidate = `${key}#${count}`;
    }
    seen.set(key, count + 1);
    allocated.add(candidate);
    return candidate;
  };
}

/**
 * Deduplicate while preserving first-seen order.
 *
 * Use at the list owner when duplicates are illegal in the UI (same path twice
 * is noise, not two rows). Templates that still need a key should pass the
 * result through {@link keyedList}.
 */
export function dedupePreserveOrder(items: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of items) {
    if (seen.has(item)) continue;
    seen.add(item);
    out.push(item);
  }
  return out;
}

export interface KeyedItem<T> {
  readonly item: T;
  readonly key: string;
}

/**
 * Pair each item with a unique render key derived from `keyOf`.
 *
 * Prefer this at the template edge when the source may legally repeat an id
 * (or when an empty fallback would collapse many rows onto one key).
 */
export function keyedList<T>(
  items: readonly T[],
  keyOf: (item: T, index: number) => string,
): KeyedItem<T>[] {
  const alloc = uniqueKeyAllocator();
  return items.map((item, index) => ({
    item,
    key: alloc(keyOf(item, index)),
  }));
}

/** True when every `keyOf` result is unique — for contract tests. */
export function eachKeysAreUnique<T>(
  items: readonly T[],
  keyOf: (item: T, index: number) => string,
): boolean {
  const seen = new Set<string>();
  for (let i = 0; i < items.length; i++) {
    const key = keyOf(items[i], i);
    if (seen.has(key)) return false;
    seen.add(key);
  }
  return true;
}
