/**
 * Repository logos: found once, remembered, and shared by every surface.
 *
 * One entry per repository family (every checkout of a repository shows the
 * same logo), keyed by the caller's `key`. Finding a logo costs one file
 * listing and one or a few file reads, so:
 *
 * - Lookups are single-flight per key and run at most `concurrency` at once,
 *   the tab in front first, so restoring thirty tabs never fans out thirty
 *   listings together.
 * - The file that won is remembered across launches, so the next launch reads
 *   that one file and lists nothing. A repository with no logo is remembered
 *   as such for {@link NO_LOGO_TTL_MS}, then looked at again (one may have
 *   been added).
 * - A lookup the backend refused (an untrusted or missing checkout, a listing
 *   over the file cap) is "unavailable", never "no logo": it is not
 *   remembered, and it is asked again after {@link UNAVAILABLE_RETRY_MS}.
 *   A refusal must not read the same as a repository that has no logo.
 *
 * A person's own choice (a particular file, or initials only) is remembered
 * without expiry and wins over the automatic pick. The picker's short list
 * and the browser's full image list share one listing per repository, and
 * the browser's thumbnails are read on their own small queue, so browsing a
 * folder of images never holds up the tabs' own lookups.
 */

import { writable, type Readable } from "svelte/store";
import {
  logoDataUrl,
  logoImagePaths,
  MAX_LOGO_ATTEMPTS,
  MAX_LOGO_CHOICES,
  rankLogoCandidates,
  type LogoBlob,
  type LogoImageList,
} from "./repoLogo";

export const REPO_LOGO_STORAGE_KEY = "gitpulse.repoLogos.v1";
/** Bound on remembered repositories, so the stored map cannot grow without end. */
export const MAX_REMEMBERED_LOGOS = 256;
/** How long "this repository has no logo" is believed before looking again. */
export const NO_LOGO_TTL_MS = 3 * 24 * 60 * 60 * 1000;
/**
 * How long a refused lookup waits before it may be asked again. It doubles
 * with each refusal in a row, up to {@link MAX_UNAVAILABLE_RETRY_MS}: a
 * repository over the listing cap refuses every time, and must not be listed
 * once a minute for as long as its tab is open.
 */
export const UNAVAILABLE_RETRY_MS = 60_000;
export const MAX_UNAVAILABLE_RETRY_MS = 60 * 60_000;
/** Lookups running at once across every repository. */
export const LOGO_CONCURRENCY = 2;
/** How long the picker's image list is reused, so a menu opened again does not list the repository again. */
export const CANDIDATES_TTL_MS = 60_000;
/** Browser thumbnails read at once, apart from the tabs' lookups. */
export const PREVIEW_CONCURRENCY = 3;
/** Thumbnails kept, oldest dropped first. */
export const MAX_PREVIEWS = 300;
/** Characters of thumbnail data URLs kept at once (about 8 MiB of images). */
export const MAX_PREVIEW_CHARS = 12 * 1024 * 1024;

export type LogoStatus = "loading" | "ready" | "none" | "unavailable";
/** Where the shown logo came from: the automatic pick, a chosen file, or a choice of initials. */
export type LogoSource = "auto" | "file" | "initials";

export interface LogoState {
  status: LogoStatus;
  /** A `data:` image URL; set only when `status` is `ready`. */
  url: string | null;
  /** The repository-relative file the URL was read from. */
  path: string | null;
  source: LogoSource;
}

export interface LogoTarget {
  /** One key per repository: its family when known, else its checkout's identity. */
  key: string;
  /** A checkout of the repository to read from. */
  repoPath: string;
  /** The checkout cannot be read right now (untrusted, deleted): ask nothing. */
  blocked?: boolean;
}

export type LogoChoice = { kind: "auto" } | { kind: "file"; path: string } | { kind: "initials" };

export interface LogoCandidate {
  path: string;
  url: string;
}

interface Remembered {
  /** The automatic pick, or the chosen file; null for "none" or initials. */
  path: string | null;
  /** When it was decided, epoch ms. */
  at: number;
  /** Absent for the automatic pick. */
  choice?: "file" | "initials";
}

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface RepoLogoDeps {
  list: (repoPath: string) => Promise<readonly string[]>;
  blob: (repoPath: string, filePath: string) => Promise<LogoBlob>;
  storage: StorageLike | null;
  now?: () => number;
  concurrency?: number;
}

const MAX_KEY_LENGTH = 4096;
const MAX_PATH_LENGTH = 1024;

function validPath(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= MAX_PATH_LENGTH && !value.includes("\0");
}

/** Reads the remembered map. Anything malformed is dropped entry by entry. */
export function loadRememberedLogos(storage: StorageLike | null): Map<string, Remembered> {
  const out = new Map<string, Remembered>();
  if (!storage) return out;
  let parsed: unknown;
  try {
    const raw = storage.getItem(REPO_LOGO_STORAGE_KEY);
    if (!raw) return out;
    parsed = JSON.parse(raw);
  } catch {
    return out;
  }
  // Stored as pairs, never as an object keyed by repository: a key such as
  // `__proto__` must not be able to assign a prototype.
  if (!Array.isArray(parsed)) return out;
  for (const entry of parsed.slice(-MAX_REMEMBERED_LOGOS)) {
    if (!Array.isArray(entry) || entry.length !== 2) continue;
    const [key, value] = entry as [unknown, unknown];
    if (typeof key !== "string" || key.length === 0 || key.length > MAX_KEY_LENGTH) continue;
    if (!value || typeof value !== "object") continue;
    const record = value as Record<string, unknown>;
    const at = typeof record.at === "number" && Number.isFinite(record.at) ? record.at : null;
    if (at === null) continue;
    const choice = record.choice === "file" || record.choice === "initials" ? record.choice : undefined;
    const path = validPath(record.path) ? record.path : null;
    if (choice === "file" && path === null) continue;
    out.set(key, { path: choice === "initials" ? null : path, at, ...(choice ? { choice } : {}) });
  }
  return out;
}

function saveRememberedLogos(storage: StorageLike | null, remembered: Map<string, Remembered>): void {
  if (!storage) return;
  try {
    storage.setItem(REPO_LOGO_STORAGE_KEY, JSON.stringify([...remembered.entries()].slice(-MAX_REMEMBERED_LOGOS)));
  } catch {
    // Storage full or unavailable: the logo is still shown, only not remembered.
  }
}

const LOADING: LogoState = { status: "loading", url: null, path: null, source: "auto" };

export function createRepoLogoStore(deps: RepoLogoDeps) {
  const now = deps.now ?? (() => Date.now());
  const concurrency = Math.max(1, deps.concurrency ?? LOGO_CONCURRENCY);
  const remembered = loadRememberedLogos(deps.storage);
  const states = writable<Record<string, LogoState>>({});
  let current: Record<string, LogoState> = {};
  const inflight = new Map<string, Promise<void>>();
  /** When a key was last refused, for the retry delay. */
  const refusedAt = new Map<string, number>();
  /** Refusals in a row per key; reset by an answer or a choice. */
  const refusals = new Map<string, number>();

  function markRefused(key: string): void {
    refusedAt.set(key, now());
    refusals.set(key, (refusals.get(key) ?? 0) + 1);
    set(key, { status: "unavailable", url: null, path: null, source: "auto" });
  }

  function retryDelay(key: string): number {
    const streak = Math.max(1, refusals.get(key) ?? 1);
    return Math.min(MAX_UNAVAILABLE_RETRY_MS, UNAVAILABLE_RETRY_MS * 2 ** Math.min(streak - 1, 16));
  }
  /** Ranked candidates not yet tried, for a logo the webview could not decode. */
  const untried = new Map<string, string[]>();
  /** The picker's image lists, by key, reused for {@link CANDIDATES_TTL_MS}. A failed read is not kept. */
  const offered = new Map<string, { at: number; promise: Promise<LogoCandidate[]> }>();
  /** The listing the picker and the browser read, by key, for {@link CANDIDATES_TTL_MS}. A refusal is not kept. */
  const listings = new Map<string, { at: number; promise: Promise<readonly string[]> }>();
  /** Thumbnails by key and path: a URL, or null for a file that is not a drawable logo. */
  const previews = new Map<string, string | null>();
  let previewChars = 0;
  const previewQueue: (() => void)[] = [];
  let previewsRunning = 0;
  /** Bumped by a choice, so a lookup that started before it does not overwrite it. */
  const generation = new Map<string, number>();

  const queue: { front: boolean; run: () => void }[] = [];
  let running = 0;

  function set(key: string, state: LogoState): void {
    if (state.status === "ready" || state.status === "none") refusals.delete(key);
    current = { ...current, [key]: state };
    states.set(current);
  }

  function remember(key: string, value: Remembered): void {
    remembered.delete(key);
    remembered.set(key, value);
    while (remembered.size > MAX_REMEMBERED_LOGOS) {
      const oldest = remembered.keys().next();
      if (oldest.done) break;
      remembered.delete(oldest.value);
    }
    saveRememberedLogos(deps.storage, remembered);
  }

  function forget(key: string): void {
    if (remembered.delete(key)) saveRememberedLogos(deps.storage, remembered);
  }

  function pump(): void {
    while (running < concurrency && queue.length > 0) {
      const next = queue.shift();
      if (!next) break;
      running++;
      next.run();
    }
  }

  function schedule(job: () => Promise<void>, front: boolean): Promise<void> {
    return new Promise<void>((resolve) => {
      const entry = {
        front,
        run: () => {
          job()
            .catch(() => undefined)
            .finally(() => {
              running--;
              resolve();
              pump();
            });
        },
      };
      if (front) queue.unshift(entry);
      else queue.push(entry);
      pump();
    });
  }

  /** Reads one file into a URL. `refused` is a thrown read, as opposed to bytes that are not an image. */
  async function read(target: LogoTarget, path: string): Promise<{ url: string | null; refused: boolean }> {
    try {
      const blob = await deps.blob(target.repoPath, path);
      return { url: logoDataUrl(path, blob), refused: false };
    } catch {
      return { url: null, refused: true };
    }
  }

  /**
   * Tries candidates in order; the first that draws wins. `keep` remembers
   * the outcome; it is false while a person's chosen file stands, so a
   * fallback never overwrites their choice.
   */
  async function tryCandidates(target: LogoTarget, candidates: string[], stamp: number, keep: boolean): Promise<void> {
    let refused = 0;
    while (candidates.length > 0) {
      const path = candidates.shift() as string;
      const { url, refused: wasRefused } = await read(target, path);
      if ((generation.get(target.key) ?? 0) !== stamp) return;
      if (url) {
        untried.set(target.key, candidates);
        set(target.key, { status: "ready", url, path, source: "auto" });
        if (keep) remember(target.key, { path, at: now() });
        return;
      }
      if (wasRefused) refused++;
    }
    untried.delete(target.key);
    if (refused > 0) {
      // Every read that could have answered was refused: not a "no logo".
      markRefused(target.key);
      return;
    }
    set(target.key, { status: "none", url: null, path: null, source: "auto" });
    if (keep) remember(target.key, { path: null, at: now() });
  }

  /** One listing for the picker and the browser, joined while in flight. Anything but an array is a refusal. */
  function sharedListing(target: LogoTarget): Promise<readonly string[]> {
    const held = listings.get(target.key);
    if (held && now() - held.at < CANDIDATES_TTL_MS) return held.promise;
    const promise = deps.list(target.repoPath).then((listing) => {
      if (!Array.isArray(listing)) throw new Error("Repository listing unavailable");
      return listing;
    });
    listings.set(target.key, { at: now(), promise });
    promise.catch(() => {
      if (listings.get(target.key)?.promise === promise) listings.delete(target.key);
    });
    return promise;
  }

  function dropPreview(id: string): void {
    if (!previews.has(id)) return;
    previewChars -= previews.get(id)?.length ?? 0;
    previews.delete(id);
  }

  function keepPreview(id: string, url: string | null): void {
    dropPreview(id);
    previews.set(id, url);
    previewChars += url?.length ?? 0;
    while (previews.size > MAX_PREVIEWS || (previewChars > MAX_PREVIEW_CHARS && previews.size > 1)) {
      const oldest = previews.keys().next();
      if (oldest.done || oldest.value === id) break;
      dropPreview(oldest.value);
    }
  }

  function pumpPreviews(): void {
    while (previewsRunning < PREVIEW_CONCURRENCY && previewQueue.length > 0) {
      const next = previewQueue.shift();
      if (next) next();
    }
  }

  /** Lists the repository and takes the best candidate that draws. */
  async function autoPick(target: LogoTarget, stamp: number, keep: boolean): Promise<void> {
    let listing: readonly string[];
    try {
      listing = await deps.list(target.repoPath);
    } catch {
      if ((generation.get(target.key) ?? 0) !== stamp) return;
      markRefused(target.key);
      return;
    }
    if ((generation.get(target.key) ?? 0) !== stamp) return;
    if (!Array.isArray(listing)) {
      // Not a listing at all: a refusal by another name, not "no logo".
      markRefused(target.key);
      return;
    }
    await tryCandidates(target, rankLogoCandidates(listing, MAX_LOGO_ATTEMPTS), stamp, keep);
  }

  async function resolve(target: LogoTarget): Promise<void> {
    const stamp = generation.get(target.key) ?? 0;
    const stale = () => (generation.get(target.key) ?? 0) !== stamp;
    const known = remembered.get(target.key);
    if (known?.choice === "initials") {
      set(target.key, { status: "none", url: null, path: null, source: "initials" });
      return;
    }
    if (known?.choice === "file" && known.path) {
      const { url } = await read(target, known.path);
      if (stale()) return;
      if (url) {
        set(target.key, { status: "ready", url, path: known.path, source: "file" });
        return;
      }
      // The chosen file is gone or no longer an image: fall back to the
      // automatic pick, keeping the choice in case the file comes back.
    } else if (known && known.path) {
      const { url } = await read(target, known.path);
      if (stale()) return;
      if (url) {
        set(target.key, { status: "ready", url, path: known.path, source: "auto" });
        return;
      }
      forget(target.key);
    } else if (known && known.path === null && !known.choice && now() - known.at < NO_LOGO_TTL_MS) {
      set(target.key, { status: "none", url: null, path: null, source: "auto" });
      return;
    }
    await autoPick(target, stamp, known?.choice !== "file");
  }

  function start(target: LogoTarget, front: boolean): Promise<void> {
    const pending = inflight.get(target.key);
    if (pending) return pending;
    if (!current[target.key] || current[target.key].status !== "ready") set(target.key, LOADING);
    const stamp = generation.get(target.key) ?? 0;
    const job = schedule(
      () =>
        resolve(target).catch(() => {
          // Whatever went wrong, the entry must not sit in "loading" for good.
          if ((generation.get(target.key) ?? 0) !== stamp) return;
          markRefused(target.key);
        }),
      front,
    ).finally(() => {
      if (inflight.get(target.key) === job) inflight.delete(target.key);
    });
    inflight.set(target.key, job);
    return job;
  }

  return {
    subscribe: states.subscribe as Readable<Record<string, LogoState>>["subscribe"],

    /** Starts a lookup for this repository unless one is known, running, or recently refused. */
    request(target: LogoTarget, options: { priority?: boolean } = {}): void {
      if (!target.key || !target.repoPath) return;
      if (target.blocked) return;
      const state = current[target.key];
      if (state && state.status !== "unavailable") return;
      if (state?.status === "unavailable" && now() - (refusedAt.get(target.key) ?? 0) < retryDelay(target.key)) return;
      void start(target, options.priority === true);
    },

    /**
     * The shown logo failed to decode in the webview: try the next ranked
     * candidate, or settle on initials. A chosen file that fails stays chosen
     * and falls back to the automatic pick.
     */
    reportBroken(target: LogoTarget, url: string): void {
      const state = current[target.key];
      if (!state || state.url !== url) return;
      const stamp = generation.get(target.key) ?? 0;
      const rest = untried.get(target.key) ?? [];
      const chosen = remembered.get(target.key)?.choice === "file";
      if (!chosen && state.path && remembered.get(target.key)?.path === state.path) forget(target.key);
      set(target.key, LOADING);
      // A chosen file that does not draw falls back to the automatic pick;
      // an automatic one moves on to the next candidate.
      const job = schedule(
        () => (state.source === "file" ? autoPick(target, stamp, false) : tryCandidates(target, rest, stamp, !chosen)),
        false,
      );
      inflight.set(target.key, job);
      void job.finally(() => {
        if (inflight.get(target.key) === job) inflight.delete(target.key);
      });
    },

    /** Image files a person may choose from, best first, each already readable. */
    candidates(target: LogoTarget): Promise<LogoCandidate[]> {
      const held = offered.get(target.key);
      if (held && now() - held.at < CANDIDATES_TTL_MS) return held.promise;
      const promise = (async () => {
        const listing = await sharedListing(target);
        const paths = rankLogoCandidates(listing, MAX_LOGO_CHOICES, { anyImage: true });
        const out: LogoCandidate[] = [];
        for (const path of paths) {
          const { url, refused } = await read(target, path);
          if (!refused) keepPreview(`${target.key}\0${path}`, url);
          if (url) out.push({ path, url });
        }
        return out;
      })();
      offered.set(target.key, { at: now(), promise });
      promise.catch(() => {
        if (offered.get(target.key)?.promise === promise) offered.delete(target.key);
      });
      return promise;
    },

    /**
     * Every image in the repository, in path order, for a person to browse.
     * Rejects when the repository cannot be listed, so a refusal never reads
     * as "no images".
     */
    async images(target: LogoTarget): Promise<LogoImageList> {
      if (target.blocked) throw new Error("Repository not readable");
      return logoImagePaths(await sharedListing(target));
    },

    /**
     * One image's thumbnail: a URL, or null when the file is not a logo this
     * app can draw (over the size cap, or not the image its name claims).
     * Rejects when the read is refused, or when `signal` aborts before the
     * read starts; neither outcome is kept.
     */
    preview(target: LogoTarget, path: string, signal?: AbortSignal): Promise<string | null> {
      const id = `${target.key}\0${path}`;
      if (previews.has(id)) return Promise.resolve(previews.get(id) ?? null);
      if (!validPath(path)) return Promise.resolve(null);
      return new Promise<string | null>((resolvePreview, reject) => {
        previewQueue.push(() => {
          if (signal?.aborted) {
            reject(new Error("Preview cancelled"));
            return;
          }
          previewsRunning++;
          read(target, path)
            .then(({ url, refused }) => {
              if (refused) {
                reject(new Error(`Couldn't read ${path}`));
                return;
              }
              keepPreview(id, url);
              resolvePreview(url);
            })
            .finally(() => {
              previewsRunning--;
              pumpPreviews();
            });
        });
        pumpPreviews();
      });
    },

    /** Drops one repository's thumbnails, when the browser that read them closes. */
    releasePreviews(target: LogoTarget): void {
      const prefix = `${target.key}\0`;
      for (const id of [...previews.keys()]) if (id.startsWith(prefix)) dropPreview(id);
    },

    /** Characters of thumbnail URLs held, for the bound's tests. */
    previewFootprint(): { count: number; chars: number } {
      return { count: previews.size, chars: previewChars };
    },

    /** Records a person's choice and shows it. */
    choose(target: LogoTarget, choice: LogoChoice): Promise<void> {
      generation.set(target.key, (generation.get(target.key) ?? 0) + 1);
      inflight.delete(target.key);
      untried.delete(target.key);
      refusedAt.delete(target.key);
      refusals.delete(target.key);
      if (choice.kind === "initials") remember(target.key, { path: null, at: now(), choice: "initials" });
      else if (choice.kind === "file" && validPath(choice.path)) remember(target.key, { path: choice.path, at: now(), choice: "file" });
      else forget(target.key);
      const next = { ...current };
      delete next[target.key];
      current = next;
      states.set(current);
      return start(target, true);
    },

    /** The person's standing choice for this repository, for a menu to mark. */
    choiceFor(key: string): LogoChoice {
      const known = remembered.get(key);
      if (known?.choice === "initials") return { kind: "initials" };
      if (known?.choice === "file" && known.path) return { kind: "file", path: known.path };
      return { kind: "auto" };
    },
  };
}

export type RepoLogoStore = ReturnType<typeof createRepoLogoStore>;
