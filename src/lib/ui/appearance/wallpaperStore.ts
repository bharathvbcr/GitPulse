/**
 * The reader's own wallpaper: an image, or a video and its first frame.
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
 *
 * A video is kept as picked (a browser cannot re-encode one), bounded by
 * `MAX_WALLPAPER_VIDEO_BYTES`, and accepted only once this window has decoded
 * its first frame: the decoder decides, not the MIME type, since WKWebView and
 * Chrome play different codecs. That frame is re-encoded like any image and
 * stored as the still, so a video wallpaper draws through the same image path
 * whenever it is not allowed to move. It plays from an object URL
 * (`media-src 'self' blob:`), muted, only when wallpaperMotion.ts says so.
 */
import { readable, writable, type Readable } from "svelte/store";

/** Largest file accepted before decoding. Big photos are fine; this bounds the decode. */
export const MAX_WALLPAPER_SOURCE_BYTES = 40 * 1024 * 1024;
/** Longest edge after re-encoding: sharp on a 5K display once the veil is over it. */
export const MAX_WALLPAPER_EDGE = 2560;
export const WALLPAPER_QUALITY = 0.86;

/** Raster types the decoder is asked to read. SVG is deliberately absent. */
export const WALLPAPER_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif", "image/avif"] as const;
/** Video types offered to the decoder; the decoder still has the last word. */
export const WALLPAPER_VIDEO_TYPES = ["video/mp4", "video/webm", "video/quicktime"] as const;
/** Largest video kept. It is stored as picked, so this bounds the IndexedDB entry. */
export const MAX_WALLPAPER_VIDEO_BYTES = 150 * 1024 * 1024;
/** Where the still is taken from: a hair past zero, so a decoded frame exists. */
const FIRST_FRAME_SEEK_S = 0.05;
/** How long that seek may take before the frame in hand is used. */
const SEEK_TIMEOUT_MS = 3_000;
/** How long a video may take to show its first frame before it is refused. */
export const VIDEO_FIRST_FRAME_TIMEOUT_MS = 15_000;
/** What the file picker offers: every image and video type above. */
export const WALLPAPER_ACCEPT = [...WALLPAPER_TYPES, ...WALLPAPER_VIDEO_TYPES].join(",");

const DB_NAME = "gitpulse-wallpaper";
const DB_VERSION = 1;
const STORE = "images";
const KEY = "custom";

export interface StoredWallpaper {
  /**
   * The still: the re-encoded image, or a video's first frame. Saved as bytes
   * (an ArrayBuffer with `blobType`), since WebKit refuses Blobs in IndexedDB
   * in a non-persistent store; a Blob here is a record from before that, and
   * still reads.
   */
  readonly blob: Blob | ArrayBuffer;
  /** The still's media type when `blob` is bytes. */
  readonly blobType?: string;
  /** The picked file's name, for the settings row. */
  readonly name: string;
  readonly width: number;
  readonly height: number;
  readonly savedAt: number;
  /**
   * The video's bytes, when the wallpaper is one; `blob` is then its first
   * frame. Bytes, like the still. Absent on every record saved before videos
   * existed, which load as images.
   */
  readonly video?: ArrayBuffer;
  /** The video's media type, so the bytes play back as what they are. */
  readonly videoType?: string;
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
      /** The video's object URL, or null for an image. `url` is the still either way. */
      readonly videoUrl: string | null;
    };

export type WallpaperRefusal = { readonly ok: false; readonly reason: string };

/** Whether `file` may be decoded at all. Pure, so the rule is tested without a browser. */
export function checkWallpaperFile(file: { readonly type: string; readonly size: number; readonly name: string }):
  | { readonly ok: true; readonly kind: "image" | "video" }
  | WallpaperRefusal {
  if (file.type === "image/svg+xml" || /\.svgz?$/i.test(file.name)) {
    return { ok: false, reason: "SVG files can't be used as a wallpaper. Use a PNG, JPEG, WebP, GIF or AVIF image." };
  }
  if ((WALLPAPER_VIDEO_TYPES as readonly string[]).includes(file.type)) {
    if (file.size === 0) return { ok: false, reason: "The file is empty." };
    if (file.size > MAX_WALLPAPER_VIDEO_BYTES) {
      return {
        ok: false,
        reason: `The video is larger than ${MAX_WALLPAPER_VIDEO_BYTES / (1024 * 1024)} MB. Use a shorter or smaller copy.`,
      };
    }
    return { ok: true, kind: "video" };
  }
  if (!(WALLPAPER_TYPES as readonly string[]).includes(file.type)) {
    return {
      ok: false,
      reason: `“${file.name}” isn't an image or video this window can use. Use a PNG, JPEG, WebP, GIF or AVIF image, or an MP4, WebM or MOV video.`,
    };
  }
  if (file.size > MAX_WALLPAPER_SOURCE_BYTES) {
    return {
      ok: false,
      reason: `The image is larger than ${MAX_WALLPAPER_SOURCE_BYTES / (1024 * 1024)} MB. Use a smaller copy.`,
    };
  }
  if (file.size === 0) return { ok: false, reason: "The file is empty." };
  return { ok: true, kind: "image" };
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

/**
 * Decodes a video far enough to draw its first frame, and re-encodes that frame
 * as the still. Rejects when this window cannot decode the video at all, or
 * cannot within `VIDEO_FIRST_FRAME_TIMEOUT_MS`.
 */
async function firstFrame(file: Blob): Promise<{ blob: Blob; width: number; height: number }> {
  const url = URL.createObjectURL(file);
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.preload = "auto";
  try {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`it showed no picture within ${VIDEO_FIRST_FRAME_TIMEOUT_MS / 1000} seconds`)),
        VIDEO_FIRST_FRAME_TIMEOUT_MS,
      );
      video.addEventListener("loadeddata", () => (clearTimeout(timer), resolve()), { once: true });
      video.addEventListener(
        "error",
        () => {
          clearTimeout(timer);
          const detail = video.error ? ` (${video.error.code}${video.error.message ? `: ${video.error.message}` : ""})` : "";
          reject(new Error(`this window can't decode it${detail}`));
        },
        { once: true },
      );
      video.src = url;
    });
    if (video.videoWidth === 0 || video.videoHeight === 0) throw new Error("it has no picture");
    // WebKit can report data loaded before any frame is decoded, and draws
    // black from it. A small seek and its `seeked` mean a decoded frame; a seek
    // that never lands within the bound draws whatever there is.
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, SEEK_TIMEOUT_MS);
      video.addEventListener("seeked", () => (clearTimeout(timer), resolve()), { once: true });
      video.currentTime = FIRST_FRAME_SEEK_S;
    });
    const size = fitWithin(video.videoWidth, video.videoHeight);
    const canvas = document.createElement("canvas");
    canvas.width = size.width;
    canvas.height = size.height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("this window couldn't draw its first frame");
    context.drawImage(video, 0, 0, size.width, size.height);
    const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", WALLPAPER_QUALITY));
    if (!blob) throw new Error("this window couldn't encode its first frame");
    return { blob, width: size.width, height: size.height };
  } finally {
    // Release the decoder before the URL.
    video.removeAttribute("src");
    video.load();
    URL.revokeObjectURL(url);
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
      // The request's own error names the cause (a quota, a value this store
      // cannot hold); the transaction's is often null by the time it aborts.
      const cause = (fallback: string) => {
        const error = request.error ?? transaction.error;
        return error ? new Error(`${fallback} (${error.name}${error.message ? `: ${error.message}` : ""})`) : new Error(fallback);
      };
      transaction.oncomplete = () => resolve(value);
      transaction.onerror = () => reject(cause("IndexedDB refused the change."));
      transaction.onabort = () => reject(cause("IndexedDB aborted the change."));
    });
  } finally {
    db.close();
  }
}

/** Whether an IndexedDB value is a wallpaper record, old (image only) or new. */
export function isStored(value: unknown): value is StoredWallpaper {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    (record.blob instanceof Blob ||
      (record.blob instanceof ArrayBuffer && typeof record.blobType === "string" && record.blobType.startsWith("image/"))) &&
    typeof record.name === "string" &&
    typeof record.width === "number" &&
    typeof record.height === "number" &&
    (record.video === undefined ||
      (record.video instanceof ArrayBuffer && typeof record.videoType === "string" && record.videoType.startsWith("video/")))
  );
}

// --- the store ---------------------------------------------------------------

const state = writable<WallpaperState>({ status: "loading" });
let currentUrl: string | null = null;
let currentVideoUrl: string | null = null;

function publish(stored: StoredWallpaper | null): void {
  if (currentUrl !== null) URL.revokeObjectURL(currentUrl);
  if (currentVideoUrl !== null) URL.revokeObjectURL(currentVideoUrl);
  currentUrl = null;
  currentVideoUrl = null;
  if (stored === null) {
    state.set({ status: "none" });
    return;
  }
  currentUrl = URL.createObjectURL(
    stored.blob instanceof Blob ? stored.blob : new Blob([stored.blob], { type: stored.blobType }),
  );
  currentVideoUrl = stored.video ? URL.createObjectURL(new Blob([stored.video], { type: stored.videoType })) : null;
  state.set({
    status: "ready",
    url: currentUrl,
    name: stored.name,
    width: stored.width,
    height: stored.height,
    videoUrl: currentVideoUrl,
  });
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
    encoded = allowed.kind === "video" ? await firstFrame(file) : await reencode(file);
  } catch (error) {
    return {
      ok: false,
      reason:
        allowed.kind === "video"
          ? `Couldn't use “${file.name}” as a video wallpaper: ${message(error)}.`
          : `Couldn't read “${file.name}” as an image: ${message(error)}`,
    };
  }
  let still: ArrayBuffer;
  try {
    still = await encoded.blob.arrayBuffer();
  } catch (error) {
    return { ok: false, reason: `Couldn't read the encoded picture: ${message(error)}` };
  }
  let stored: StoredWallpaper = {
    blob: still,
    blobType: encoded.blob.type || "image/jpeg",
    name: file.name.slice(0, 120),
    width: encoded.width,
    height: encoded.height,
    savedAt: Date.now(),
  };
  if (allowed.kind === "video") {
    try {
      stored = { ...stored, video: await file.arrayBuffer(), videoType: file.type };
    } catch (error) {
      return { ok: false, reason: `Couldn't read “${file.name}”: ${message(error)}` };
    }
  }
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
