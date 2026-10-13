/**
 * Everything the repository store remembers about one open tab outside the
 * session record itself, and the one way to forget it.
 *
 * These maps are keyed by tab id or repository path and written by the
 * hydrate, poll, stats and mutation paths. Closing a tab used to remove them
 * by hand in each of three close routes, and the routes drifted: only
 * closeTab dropped `familyUnknown`, and no route dropped the rest, so a long
 * session that opened and closed many repositories grew them without bound.
 * `forget` is what every close route calls.
 */
export function createSessionBook(sameRepo: (a: string, b: string) => boolean) {
  /**
   * Tab ids whose family is unknown because no resolve has succeeded for them
   * yet (restored untrusted, or unreachable at restore). A resolve that
   * answered "no common directory" is an answer and is not listed. `hydrate`
   * asks again for these, once per hydrate: without it a family split at
   * restore stayed split until the app restarted.
   */
  const familyUnknown = new Set<string>();
  /**
   * Ordering token for snapshot fetches. `activateTab` starts a hydrate at
   * generation N; `refresh()` and watcher events start more at the SAME N
   * (refresh never bumps). Generation alone cannot order those, so the older
   * response could resolve last and overwrite fresher data. Only the
   * latest-started fetch may apply.
   */
  const snapshotRuns = new Map<string, number>();
  /** Per tab: the newest status poll. */
  const pollRuns = new Map<string, number>();
  /** Tabs whose branch stats are being read now. */
  const statsInflight = new Set<string>();
  /**
   * Per-repo deadline until which watcher events count as echoes of our own
   * just-landed mutation rather than external changes; see
   * WATCHER_ECHO_SUPPRESS_MS.
   */
  const mutationEchoUntil = new Map<string, number>();

  return {
    familyUnknown,
    snapshotRuns,
    pollRuns,
    statsInflight,
    mutationEchoUntil,
    /** Drop every record of a closed tab. An in-flight read that finds its run gone discards its result. */
    forget(id: string, path: string | null): void {
      familyUnknown.delete(id);
      snapshotRuns.delete(id);
      pollRuns.delete(id);
      statsInflight.delete(id);
      if (path === null) return;
      for (const key of [...mutationEchoUntil.keys()]) if (sameRepo(key, path)) mutationEchoUntil.delete(key);
    },
    /** How many records are held, for the tests and diagnostics that watch for growth. */
    size(): number {
      return familyUnknown.size + snapshotRuns.size + pollRuns.size + statsInflight.size + mutationEchoUntil.size;
    },
  };
}
