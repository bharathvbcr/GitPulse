/**
 * Kept apart from `reloadRestore.ts` on purpose. The session bridge runs this
 * at start-up, in the entry chunk, while `reloadRestore` pulls in the split-tree
 * model (~70 KB of source) that only the terminal dock needs. Importing it from
 * here keeps that model out of the entry chunk, whose byte budget is enforced
 * by `vite build` (see lazyChunks.test.ts).
 */

/** Pauses before the second and third listing tries: 3 tries in all, then the failure stands. */
export const LISTING_RETRY_DELAYS_MS: readonly number[] = [500, 2000];

/**
 * Runs `attempt` (the host's session listing), trying again after each delay
 * when it throws, and throws the last error once the delays are spent. Bounded:
 * a host that never answers is reported, not asked forever.
 */
export async function retryListing<T>(
  attempt: () => Promise<T>,
  options: { delays?: readonly number[]; sleep?: (ms: number) => Promise<void> } = {},
): Promise<T> {
  const delays = options.delays ?? LISTING_RETRY_DELAYS_MS;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  for (let tries = 0; ; tries += 1) {
    try {
      return await attempt();
    } catch (error) {
      if (tries >= delays.length) throw error;
      await sleep(delays[tries]);
    }
  }
}
