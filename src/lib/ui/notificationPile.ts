/**
 * How many real cards and decorative lips a notification pile draws.
 *
 * One item is just a card. Two or more stay a pile: one card in front and at
 * most two lips behind it. Expanding draws every loaded card and no lips.
 * A non-finite or empty count draws nothing, so a bad length cannot look
 * like one notice.
 *
 * The caller picks which item is in front. The inbox lists newest first and
 * shows that prefix. Toasts list oldest first and show the suffix.
 */
export function notificationPile(count: number, expanded: boolean): { shown: number; peeks: number } {
  if (!Number.isFinite(count) || count <= 0) return { shown: 0, peeks: 0 };
  const total = Math.trunc(count);
  if (total <= 0) return { shown: 0, peeks: 0 };
  if (expanded || total === 1) return { shown: total, peeks: 0 };
  return { shown: 1, peeks: Math.min(2, total - 1) };
}

/** Unread notices hidden behind the front card. Expanding hides none. */
export function buriedUnread(readAt: readonly (number | null)[], expanded: boolean): number {
  if (expanded || readAt.length < 2) return 0;
  let unread = 0;
  for (let i = 1; i < readAt.length; i += 1) if (readAt[i] === null) unread += 1;
  return unread;
}

/**
 * Label for the control that opens the pile or puts it back.
 *
 * Null means there is nothing to stack, so the inbox draws no control.
 * The count is how many notices are loaded, not how many the store still
 * has — the older-page button is what says the listing continues.
 */
export function pileToggleLabel(count: number, expanded: boolean, unreadBehind: number): string | null {
  if (!Number.isFinite(count) || Math.trunc(count) <= 1) return null;
  const total = Math.trunc(count);
  if (expanded) return "Show stack";
  const unread = Number.isFinite(unreadBehind) ? Math.max(0, Math.trunc(unreadBehind)) : 0;
  const base = `${total} notifications`;
  return unread > 0 ? `${base}, ${unread} unread` : base;
}

/**
 * True when focus moved to a node that is still inside the pile.
 *
 * Toasts open while the pointer or focus is on them. Moving from one button
 * to another inside the pile must not collapse it or restart the countdown.
 */
export function focusStayedInside(
  root: { contains(target: unknown): boolean } | null | undefined,
  next: unknown,
): boolean {
  return next != null && root != null && root.contains(next);
}
