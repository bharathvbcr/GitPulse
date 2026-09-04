/**
 * Which refs the commit graph walks and labels.
 *
 * The backend derives BOTH answers from one list (`src-tauri/src/graph/
 * ref_scope.rs`) so the graph can never draw a lane it has no name for. This
 * module is the frontend's copy of the union plus the validator persisted
 * preferences pass through — a stored value is user data and a wrong one must
 * fall back to the safe scope, not reach the IPC boundary.
 */
export type RefScope = "named" | "all";

export function isRefScope(value: unknown): value is RefScope {
  return value === "named" || value === "all";
}

/** Longest ref label a row chip draws before it is elided. */
export const REF_LABEL_MAX = 32;

/**
 * A ref path shortened to something a chip can draw.
 *
 * Named refs are short by nature — `main`, `origin/feature`, `v1.2.0` — and
 * pass through untouched. The refs the all-refs scope adds are not: a real
 * one in this repository is
 * `codex/turn-diffs/checkpoints/<64 hex>/<64 hex>/<epoch>/<uuid>`, 209
 * characters, which stretches the commit row until the summary is off-screen.
 *
 * Whole leading path segments are kept, because the NAMESPACE is the part a
 * reader can act on — `codex/turn-diffs/…` answers "what is this lane?" while
 * the trailing hash does not. The full path stays in the chip's title, so
 * nothing is lost, only folded.
 */
export function shortRefLabel(name: string, max: number = REF_LABEL_MAX): string {
  // A budget under four characters cannot hold a segment and an ellipsis;
  // clamp rather than emitting a label that is nothing but punctuation.
  const budget = Math.max(4, Math.floor(max));
  if (name.length <= budget) return name;

  let kept = "";
  for (const segment of name.split("/")) {
    const next = kept ? `${kept}/${segment}` : segment;
    // Two characters reserved for the "/…" that marks the fold.
    if (next.length > budget - 2) break;
    kept = next;
  }
  if (kept) return `${kept}/…`;

  // One enormous segment with no fold point. Slice by code points: cutting
  // between the halves of a surrogate pair produces a replacement character,
  // which is a rendering bug wearing a ref name's clothes.
  return `${[...name].slice(0, budget - 1).join("")}…`;
}
