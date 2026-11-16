/**
 * Decoded avatar images for the canvas graph.
 *
 * The canvas cannot draw a URL, only a loaded image, and a paint must never
 * wait on the network. {@link avatarImage} therefore answers synchronously:
 * the element once it has loaded, otherwise null (the initials fill) while
 * starting the load exactly once. Loads coalesce into one
 * {@link onAvatarImagesLoaded} callback per animation frame so a screenful of
 * avatars arriving together costs one repaint.
 *
 * `crossOrigin = "anonymous"` keeps the canvas untainted (the CDN answers with
 * `access-control-allow-origin: *`); a URL that fails is remembered and never
 * retried, so a dead CDN cannot turn every paint into a request.
 */

/** Cap on remembered URLs (loaded and failed); the oldest is evicted first. */
export const MAX_AVATAR_IMAGES = 512;

type Entry = { state: "loading" | "loaded" | "failed"; image: HTMLImageElement | null };

const entries = new Map<string, Entry>();
const listeners = new Set<() => void>();
let flushScheduled = false;

function scheduleFlush(): void {
  if (flushScheduled) return;
  flushScheduled = true;
  const run = () => {
    flushScheduled = false;
    for (const listener of [...listeners]) listener();
  };
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
  else setTimeout(run, 16);
}

function evictOverflow(): void {
  while (entries.size > MAX_AVATAR_IMAGES) {
    const oldest = entries.keys().next();
    if (oldest.done) return;
    entries.delete(oldest.value);
  }
}

function startLoad(url: string): Entry {
  const image = new Image();
  const entry: Entry = { state: "loading", image };
  image.crossOrigin = "anonymous";
  image.referrerPolicy = "no-referrer";
  image.decoding = "async";
  image.onload = () => {
    // An evicted entry's late load must not resurrect it or repaint.
    if (entries.get(url) !== entry) return;
    entry.state = image.naturalWidth > 0 ? "loaded" : "failed";
    if (entry.state === "failed") entry.image = null;
    scheduleFlush();
  };
  image.onerror = () => {
    if (entries.get(url) !== entry) return;
    entry.state = "failed";
    entry.image = null;
  };
  entries.set(url, entry);
  evictOverflow();
  image.src = url;
  return entry;
}

/** The image when it has loaded and decoded, else null (and a load is started once). */
export function avatarImage(url: string): HTMLImageElement | null {
  const entry = entries.get(url) ?? startLoad(url);
  if (entry.state !== "loaded" || entry.image === null) return null;
  const image = entry.image;
  return image.complete && image.naturalWidth > 0 ? image : null;
}

/** Subscribe to "some avatar images finished loading"; returns the unsubscribe. */
export function onAvatarImagesLoaded(callback: () => void): () => void {
  listeners.add(callback);
  return () => {
    listeners.delete(callback);
  };
}

/** Test seam. */
export function resetAvatarImagesForTests(): void {
  entries.clear();
  listeners.clear();
  flushScheduled = false;
}
