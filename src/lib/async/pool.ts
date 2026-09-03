/**
 * Bounded fan-out for IPC work.
 *
 * `Promise.all(items.map(call))` starts every call at once. On the JS side
 * that looks free — the promises just queue — but each one lands on Tauri's
 * blocking pool, which admits 512 concurrent tasks, and most of the commands
 * here spawn `git`. A workspace-wide refresh across 64 repositories issues
 * five commands each, so hundreds of `git` processes existed at the same
 * instant, and the process ran out of file descriptors: a GUI launch inherits
 * a soft `RLIMIT_NOFILE` of 256, and past it every spawn fails with
 * "Too many open files (os error 24)".
 *
 * The backend now caps concurrent children too (`engine::git_cli`'s spawn
 * gate), but a cap reached by queueing hundreds of blocked threads is still
 * the wrong shape. Callers bound the fan-out here, at the point that knows how
 * wide the work is.
 */

/** Default width for IPC fan-out. Matches `DEFAULT_BULK_CONCURRENCY`. */
export const DEFAULT_FAN_OUT = 4;

/**
 * Runs `task(index)` for every index in `[0, count)`, at most `concurrency`
 * at a time, and resolves with the results in index order.
 *
 * Rejects with the first rejection, like `Promise.all`; workers already in
 * flight are allowed to settle rather than being abandoned mid-IPC. Callers
 * that must not fail as a group catch inside `task`.
 */
export async function mapWithConcurrency<T>(
  count: number,
  concurrency: number,
  task: (index: number) => Promise<T>,
): Promise<T[]> {
  const total = Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0;
  if (total === 0) return [];
  // A non-finite width must fall back rather than propagate: `Array.from({
  // length: NaN })` is empty, which would spawn zero workers and resolve with
  // a hole-filled array while nothing had actually run.
  const requested = Number.isFinite(concurrency)
    ? Math.floor(concurrency)
    : DEFAULT_FAN_OUT;
  const width = Math.max(1, Math.min(requested, total));

  const results = new Array<T>(total);
  let nextIndex = 0;
  // An array rather than a `let`: workers assign it from inside a closure, and
  // control-flow narrowing would otherwise decide the variable is still null
  // at the throw below.
  const failures: unknown[] = [];

  async function worker(): Promise<void> {
    for (;;) {
      // A failure stops new work from being claimed, but never cancels a call
      // already issued — a half-observed `git fetch` cannot be un-run.
      if (failures.length > 0) return;
      const index = nextIndex;
      if (index >= total) return;
      nextIndex += 1;
      try {
        results[index] = await task(index);
      } catch (err: unknown) {
        failures.push(err);
        return;
      }
    }
  }

  await Promise.all(Array.from({ length: width }, () => worker()));
  if (failures.length > 0) throw failures[0];
  return results;
}

/**
 * `mapWithConcurrency` over an array, for the common case.
 */
export async function mapItems<I, T>(
  items: readonly I[],
  concurrency: number,
  task: (item: I, index: number) => Promise<T>,
): Promise<T[]> {
  return mapWithConcurrency(items.length, concurrency, (index) =>
    task(items[index], index),
  );
}
