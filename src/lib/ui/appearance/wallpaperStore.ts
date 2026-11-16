/**
 * The reader's own wallpaper image.
 *
 * It does NOT live in the interface preferences: that blob is written whole to
 * localStorage on every change and its quota failure is swallowed, so a
 * megabyte data URL there would quietly stop every other preference from
 * saving. The image lives in IndexedDB as a Blob, and the window draws it
 * through an object URL (`img-src blob:` is already in the CSP), so no new
 * native command and no file path is involved.
 *
 * What is stored is never the picked file itself. The file is decoded by the
 * browser's image decoder and re-encoded through a canvas as a bounded JPEG:
 * metadata is gone, the size is capped, and an SVG (which is a document, not a
 * picture) is refused before it is decoded.
 */
import { readable, writable, type Readable } from "svelte/store";

/** Largest file accepted before decoding. Big photos are fine; this bounds the decode. */
export const MAX_WALLPAPER_SOURCE_BYTES = 40 * 1024 * 1024;
/** Longest edge after re-encoding: sharp on a 5K display once the veil is over it. */
export const MAX_WALLPAPER_EDGE = 2560;
export const WALLPAPER_QUALITY = 0.86;

/** Raster types the decoder is asked to read. SVG is deliberately absent. */
export const WALLPAPER_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif", "image/avif"] as const;

const DB_NAME = "gitpulse-wallpaper";
const DB_VERSION = 1;
const STORE = "images";
const KEY = "custom";

export interface StoredWallpaper {
  readonly blob: Blob;
  /** The picked file's name, for the settings row. */
  readonly name: string;
  readonly width: number;
  readonly height: number;
  readonly savedAt: number;
}

export type WallpaperState =
  | { readonly status: "loading" }
  | { readonly status: "none" }
  /** IndexedDB could not be opened or read; drawn as "not available", never as "none". */
  | { readonly status: "unavailable"; readonly reason: string }
  | {
      readonly status: "ready";
      readonly url: string;
      readonly name: string;
      readonly width: number;
      readonly height: number;
    };

export type WallpaperRefusal = { readonly ok: false; readonly reason: string };

/** Whether `file` may be decoded at all. Pure, so the rule is tested without a browser. */
export function checkWallpaperFile(file: { readonly type: string; readonly size: number; readonly name: string }):
  | { readonly ok: true }
  | WallpaperRefusal {
  if (file.type === "image/svg+xml" || /\.svgz?$/i.test(file.name)) {
    return { ok: false, reason: "SVG files can't be used as a wallpaper. Use a PNG, JPEG, WebP, GIF or AVIF image." };
  }
  if (!(WALLPAPER_TYPES as readonly string[]).includes(file.type)) {
    return {
      ok: false,
      reason: `“${file.name}” isn't an image this window can use. Use a PNG, JPEG, WebP, GIF or AVIF image.`,
    };
  }
  if (file.size > MAX_WALLPAPER_SOURCE_BYTES) {
    return {
      ok: false,
      reason: `The image is larger than ${MAX_WALLPAPER_SOURCE_BYTES / (1024 * 1024)} MB. Use a smaller copy.`,
    };
  }
  if (file.size === 0) return { ok: false, reason: "The file is empty." };
  return { ok: true };
}

/** The size the image is re-encoded at: the longest edge held to `MAX_WALLPAPER_EDGE`, never upscaled. */
export function fitWithin(width: number, height: number, edge = MAX_WALLPAPER_EDGE): { width: number; height: number } {
  const longest = Math.max(width, height);
  if (longest <= edge) return { width: Math.max(1, Math.round(width)), height: Math.max(1, Math.round(height)) };
  const scale = edge / longest;
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

async function reencode(file: Blob): Promise<{ blob: Blob; width: number; height: number }> {
  const bitmap = await createImageBitmap(file);
  try {
    const size = fitWithin(bitmap.width, bitmap.height);
    const canvas = document.createElement("canvas");
    canvas.width = size.width;
    canvas.height = size.height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("This window couldn't draw the image.");
    context.drawImage(bitmap, 0, 0, size.width, size.height);
    const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", WALLPAPER_QUALITY));
    if (!blob) throw new Error("This window couldn't encode the image.");
    return { blob, width: size.width, height: size.height };
  } finally {
    bitmap.close();
  }
}

// --- IndexedDB ---------------------------------------------------------------

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("This window has no IndexedDB."));
      return;
    }
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(STORE)) request.result.createObjectStore(STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB refused to open."));
    request.onblocked = () => reject(new Error("IndexedDB is held open by another window."));
  });
}

async function withStore<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await openDb();
  try {
    return await new Promise<T>((resolve, reject) => {
      const transaction = db.transaction(STORE, mode);
      const request = run(transaction.objectStore(STORE));
      let value: T;
      request.onsuccess = () => {
        value = request.result;
      };
      transaction.oncomplete = () => resolve(value);
      transaction.onerror = () => reject(transaction.error ?? new Error("IndexedDB refused the change."));
      transaction.onabort = () => reject(transaction.error ?? new Error("IndexedDB aborted the change."));
    });
  } finally {
    db.close();
  }
}

function isStored(value: unknown): value is StoredWallpaper {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    record.blob instanceof Blob &&
    typeof record.name === "string" &&
    typeof record.width === "number" &&
    typeof record.height === "number"
  );
}

// --- the store ---------------------------------------------------------------

const state = writable<WallpaperState>({ status: "loading" });
let currentUrl: string | null = null;

function publish(stored: StoredWallpaper | null): void {
  if (currentUrl !== null) URL.revokeObjectURL(currentUrl);
  currentUrl = null;
  if (stored === null) {
    state.set({ status: "none" });
    return;
  }
  currentUrl = URL.createObjectURL(stored.blob);
  state.set({ status: "ready", url: currentUrl, name: stored.name, width: stored.width, height: stored.height });
}

function message(error: unknown): string {
  return error instanceof Error && error.message !== "" ? error.message : String(error);
}

let loaded: Promise<void> | null = null;

/** Reads the stored image once per page. Safe to call again; it returns the same load. */
export function loadWallpaper(): Promise<void> {
  if (loaded) return loaded;
  if (typeof window === "undefined" || typeof indexedDB === "undefined") {
    state.set({ status: "unavailable", reason: "This window has no IndexedDB." });
    loaded = Promise.resolve();
    return loaded;
  }
  loaded = withStore<unknown>("readonly", (store) => store.get(KEY))
    .then((value) => publish(isStored(value) ? value : null))
    .catch((error: unknown) => state.set({ status: "unavailable", reason: message(error) }));
  return loaded;
}

/** The custom wallpaper as it stands. Starts the load on first subscription. */
export const customWallpaper: Readable<WallpaperState> = readable<WallpaperState>({ status: "loading" }, (set) => {
  const stop = state.subscribe(set);
  void loadWallpaper();
  return stop;
});

/**
 * Re-encodes and stores `file`, then publishes it. Resolves with a refusal the
 * settings row can show; it only throws for a defect, never for a bad file.
 */
export async function saveWallpaper(file: File): Promise<{ readonly ok: true } | WallpaperRefusal> {
  const allowed = checkWallpaperFile(file);
  if (!allowed.ok) return allowed;
  let encoded: { blob: Blob; width: number; height: number };
  try {
    encoded = await reencode(file);
  } catch (error) {
    return { ok: false, reason: `Couldn't read “${file.name}” as an image: ${message(error)}` };
  }
  const stored: StoredWallpaper = {
    blob: encoded.blob,
    name: file.name.slice(0, 120),
    width: encoded.width,
    height: encoded.height,
    savedAt: Date.now(),
  };
  try {
    await withStore("readwrite", (store) => store.put(stored, KEY));
  } catch (error) {
    return { ok: false, reason: `Couldn't save the wallpaper: ${message(error)}` };
  }
  publish(stored);
  return { ok: true };
}

/** Forgets the stored image. */
export async function clearWallpaper(): Promise<{ readonly ok: true } | WallpaperRefusal> {
  try {
    await withStore("readwrite", (store) => store.delete(KEY));
  } catch (error) {
    return { ok: false, reason: `Couldn't remove the wallpaper: ${message(error)}` };
  }
  publish(null);
  return { ok: true };
}
