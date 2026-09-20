/**
 * Which repository tabs still have a live terminal panel.
 *
 * A PTY dies with its component. App used to `{#key}` the dock on
 * `currentPath`, so switching repository tabs unmounted every session and
 * wiped the scrollback. The dock now keeps one panel per visited tab and
 * hides the rest; this helper is the latch for that set.
 *
 * Closing a repository tab drops it from `openTabIds`, which is what
 * unmounts that panel and kills its shells. Switching away only hides.
 * Opening the dock on a tab that has never hosted one adds it. Visiting a
 * tab while the dock is closed does not spawn a shell in the background.
 *
 * PTYs are process-global (`MAX_PTY_SESSIONS`). Keeping panels alive means
 * a switch no longer frees that budget; a spawn that would have succeeded
 * after a teardown may now hit the ceiling, and the existing spawn error
 * is what says so. That budget is also why `activeDockOpen` has to be the
 * ACTIVE tab's own state and not a workspace-wide flag: as one shared
 * boolean, every repository the user merely visited while a shell ran
 * somewhere else latched a panel here and spent a slot on a shell nobody
 * asked for. The rule below never changed — what it is told did.
 */

export function nextHostedTerminals(
  hosted: ReadonlySet<string>,
  openTabIds: readonly string[],
  /** The repository tab on screen; only it can newly latch a panel. */
  activeTabId: string | null,
  /**
   * Whether the dock is open ON THE ACTIVE TAB — `RepoState.terminalOpen`,
   * which is per repository tab. Never a workspace-wide preference.
   */
  activeDockOpen: boolean,
): Set<string> {
  const open = new Set(openTabIds);
  const next = new Set<string>();
  for (const id of hosted) {
    if (open.has(id)) next.add(id);
  }
  if (activeDockOpen && activeTabId && open.has(activeTabId)) {
    next.add(activeTabId);
  }
  if (hosted instanceof Set && sameSet(hosted, next)) return hosted;
  return next;
}

function sameSet(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  if (a.size !== b.size) return false;
  for (const id of a) {
    if (!b.has(id)) return false;
  }
  return true;
}
