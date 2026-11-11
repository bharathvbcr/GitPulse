/**
 * Bounded visual ANSI scrollback snapshot cache backed by `@xterm/addon-serialize`.
 *
 * Unlike plain-text `retainedOutput()`, `SerializeAddon.serialize()` preserves
 * SGR colors, bold/dim attributes, hyperlinks, and cursor layout. Keeping a
 * bounded snapshot per tab allows visual state restoration (for example across
 * ended-buffer release or split rehydration) without unbounded memory growth.
 */

export const MAX_SCROLLBACK_SNAPSHOT_BYTES = 256 * 1024;
export const MAX_SCROLLBACK_SNAPSHOT_ENTRIES = 32;

const SGR_RESET_PREFIX = "\u001b[0m";
const ANSI_ESCAPE = /\u001b\[[0-9;:?]*[ -/]*[@-~]|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/g;
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: false });

export interface ScrollbackSnapshotsOptions {
  maxBytesPerEntry?: number;
  maxEntries?: number;
}

export interface ScrollbackSnapshots {
  /** Calls `serialize()` safely, bounds the result, and stores it under `key`. */
  capture(key: string, serialize: () => string): boolean;
  /** Reads the stored snapshot without removing it. */
  peek(key: string): string | null;
  /** Reads and removes the stored snapshot. */
  take(key: string): string | null;
  /** Drops any snapshot stored under `key`. */
  forget(key: string): void;
}

function hasVisibleText(serialized: string): boolean {
  return serialized.replace(ANSI_ESCAPE, "").trim().length > 0;
}

function clipSnapshotBytes(serialized: string, maxBytes: number): string {
  const bytes = encoder.encode(serialized);
  if (bytes.byteLength <= maxBytes) return serialized;
  const resetBytes = encoder.encode(SGR_RESET_PREFIX);
  const budget = Math.max(0, maxBytes - resetBytes.byteLength);
  if (budget === 0) return "";
  let slice = bytes.subarray(bytes.byteLength - budget);
  // Skip UTF-8 continuation bytes (10xxxxxx) at the cut point.
  let start = 0;
  while (start < slice.length && (slice[start] & 0xc0) === 0x80) start += 1;
  slice = slice.subarray(start);
  let tail = decoder.decode(slice);
  const newline = tail.indexOf("\n");
  if (newline >= 0 && newline + 1 < tail.length) {
    tail = tail.slice(newline + 1);
  }
  return `${SGR_RESET_PREFIX}${tail}`;
}

export function createScrollbackSnapshots(
  options: ScrollbackSnapshotsOptions = {},
): ScrollbackSnapshots {
  const maxBytes = options.maxBytesPerEntry ?? MAX_SCROLLBACK_SNAPSHOT_BYTES;
  const maxEntries = Math.max(1, options.maxEntries ?? MAX_SCROLLBACK_SNAPSHOT_ENTRIES);
  const entries = new Map<string, string>();

  return {
    capture(key, serialize) {
      if (!key) return false;
      let raw: string;
      try {
        raw = serialize();
      } catch {
        return false;
      }
      if (!raw || !hasVisibleText(raw)) return false;
      const clipped = clipSnapshotBytes(raw, maxBytes);
      if (!clipped) return false;
      entries.delete(key);
      entries.set(key, clipped);
      while (entries.size > maxEntries) {
        const oldest = entries.keys().next().value;
        if (oldest === undefined) break;
        entries.delete(oldest);
      }
      return true;
    },
    peek(key) {
      return entries.get(key) ?? null;
    },
    take(key) {
      const value = entries.get(key) ?? null;
      if (value !== null) entries.delete(key);
      return value;
    },
    forget(key) {
      entries.delete(key);
    },
  };
}

export const scrollbackSnapshots = createScrollbackSnapshots();
