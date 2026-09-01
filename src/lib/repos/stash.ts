/**
 * The stash stack, as the UI sees it.
 *
 * The safety property this model exists to preserve: **a stash action must
 * carry the object id the user was actually looking at.** `git stash drop`
 * only accepts `stash@{n}`, and that index shifts whenever anything else
 * pushes or drops — another worktree, another client, an agent. Sending the
 * index alone means a stale list silently destroys someone else's work.
 *
 * So every action here is addressed by `(index, oid)` and the backend refuses
 * the pair if the index no longer holds that object. The UI's job is simply to
 * never separate the two.
 */

/** Mirrors the Rust `StashEntry`; snake_case like every other wire type. */
export interface StashEntry {
  index: number;
  selector: string;
  oid: string;
  subject: string;
  message: string;
  branch: string | null;
  timestamp: number;
}

/** Mirrors the Rust `StashAction` under `rename_all = "lowercase"`. */
export type StashAction = "apply" | "pop" | "drop";

export const STASH_ACTIONS: readonly StashAction[] = ["apply", "pop", "drop"];

/** Button text. Named so each control stands alone out of context. */
export function stashActionLabel(action: StashAction): string {
  switch (action) {
    case "apply":
      return "Apply";
    case "pop":
      return "Apply & remove";
    case "drop":
      return "Delete";
  }
}

/**
 * What the action does, in consequences rather than mechanism.
 *
 * "Pop" means nothing to someone who has not used git from a terminal, and
 * the difference between apply and pop is exactly the thing a new user gets
 * wrong — so the wording leads with whether the entry survives.
 */
export function stashActionConsequence(action: StashAction): string {
  switch (action) {
    case "apply":
      return "Restores these changes into your working tree and keeps the stash entry.";
    case "pop":
      return "Restores these changes into your working tree and removes the stash entry.";
    case "drop":
      return "Deletes this stash entry without restoring anything. The changes it holds are not recoverable from GitPulse.";
  }
}

/**
 * True for actions that can lose work and therefore need confirming.
 *
 * `drop` destroys the entry outright. `pop` also removes it — and if the
 * restore conflicts, the entry is gone while the changes sit unresolved in the
 * tree, which is the worst of both. `apply` always leaves the entry behind.
 */
export function isDestructiveStashAction(action: StashAction): boolean {
  return action === "drop" || action === "pop";
}

/** One line naming the stash, for a list row. */
export function stashTitle(entry: StashEntry): string {
  const message = entry.message.trim();
  return message.length > 0 ? message : entry.subject.trim() || entry.selector;
}

/**
 * The subtitle: which branch it came from, and the short object id.
 *
 * The object id is shown deliberately — it is the identity the action is
 * addressed by, so a user comparing two similar stashes has the same handle
 * the backend uses.
 */
export function stashSubtitle(entry: StashEntry): string {
  const parts: string[] = [];
  if (entry.branch) parts.push(`on ${entry.branch}`);
  const short = entry.oid.slice(0, 7);
  if (short) parts.push(short);
  return parts.join(" · ");
}

/**
 * Whether a list the UI is holding still matches what the backend reported.
 *
 * Compared by `(index, oid)` pairs, which is exactly the tuple every action is
 * addressed by: if any pair moved, every action the user could take from the
 * old list is now suspect and the list must be redrawn before acting.
 */
export function stackMatches(a: readonly StashEntry[], b: readonly StashEntry[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i].index !== b[i].index || a[i].oid !== b[i].oid) return false;
  }
  return true;
}

/**
 * The arguments an action must send, or null when the entry cannot be acted on.
 *
 * Returning the pair as one object makes it structurally impossible to send an
 * index without its object id — the mistake this whole module guards against.
 */
export function stashActionPayload(
  entry: StashEntry | null | undefined,
): { index: number; expectedOid: string } | null {
  if (!entry) return null;
  if (!Number.isInteger(entry.index) || entry.index < 0) return null;
  if (!entry.oid || !/^[0-9a-fA-F]+$/.test(entry.oid)) return null;
  return { index: entry.index, expectedOid: entry.oid };
}

/**
 * Recognizes the backend's stale-stack refusal, so the UI can respond by
 * refreshing rather than by showing a raw error the user cannot act on.
 */
export function isStaleStackError(message: string): boolean {
  return /Refresh the stash list/i.test(message);
}

/** Empty-state wording that distinguishes "no stashes" from "not loaded". */
export function stashEmptyMessage(loaded: boolean): string {
  return loaded
    ? "No stashed changes."
    : "Stash list not loaded yet.";
}
