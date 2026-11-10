/**
 * For a `$derived.by` in a view that stays mounted while hidden (Fleet and
 * Agents sit behind `class:hidden` so the repo pane and its terminals are not
 * torn down). While `active()` is false the returned getter reads nothing but
 * `active`, so the derived has no other dependency and a store publish does not
 * re-run it; it hands back the value it last computed. When `active()` turns
 * true it computes again, so the view shows current data the moment it is
 * shown. The first read always computes, so a view mounted hidden is not empty.
 */
export function recomputeWhileActive<T>(active: () => boolean, compute: () => T): () => T {
  let held: { value: T } | null = null;
  return () => {
    if (held === null || active()) held = { value: compute() };
    return held.value;
  };
}
