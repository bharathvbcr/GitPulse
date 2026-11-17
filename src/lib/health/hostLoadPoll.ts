import { createAdaptiveTimer, type AdaptiveHost } from "../runtime/adaptiveTimer";
import { refreshHostLoad } from "../stores/hostLoadStore";

/** How often a visible input row rereads the sampler's status. */
export const HOST_LOAD_POLL_MS = 5000;

/**
 * Keeps a row's live reading fresh while it is on screen: one read now, then
 * one every `HOST_LOAD_POLL_MS`. The cadence is `createAdaptiveTimer`'s, so it
 * stops while the window is in the background and rejoins with one immediate
 * read, and the read itself is single-flight. Returns the stop function.
 */
export function pollHostLoad(
  refresh: () => Promise<unknown> = refreshHostLoad,
  host?: AdaptiveHost | null,
): () => void {
  const read = () => void refresh().catch(() => undefined);
  read();
  return host === undefined ? createAdaptiveTimer(read, HOST_LOAD_POLL_MS) : createAdaptiveTimer(read, HOST_LOAD_POLL_MS, host);
}
