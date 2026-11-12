import { writable, type Readable } from "svelte/store";

/** Tailwind `w-72`: the width both trees used before they could be dragged. */
export const FILE_TREE_DEFAULT_WIDTH = 288;
/** Keeps the filter row usable. Matches the diff file rail's floor. */
export const FILE_TREE_MIN_WIDTH = 180;
/** Same nudge as the sidebar splitter. */
export const FILE_TREE_RESIZE_STEP = 16;
/** The tree never takes more than half the row it sits in. */
export const FILE_TREE_MAX_FRACTION = 0.5;
/**
 * Ceiling for a persisted value when no row has been measured yet. A drag is
 * capped by the row first; this only stops a corrupt blob from painting a
 * multi-thousand-pixel column before that measurement.
 */
export const FILE_TREE_ABSOLUTE_MAX = 4000;

export const FILE_TREE_COLUMN_STORAGE_KEY = "gitpulse_file_tree_column";

/**
 * Widest the column may be for this row.
 *
 * Half the row, and whatever must stay for the rest of it (`reserve`). The
 * floor is still {@link FILE_TREE_MIN_WIDTH} when the row is too narrow to
 * honour both — the filter row wins over a perfect half.
 * An unmeasured row reports the absolute ceiling so a saved width is not
 * collapsed before layout.
 */
export function fileTreeMaxWidth(paneWidth: number, reserve = FILE_TREE_MIN_WIDTH): number {
  if (!Number.isFinite(paneWidth) || paneWidth <= 0) return FILE_TREE_ABSOLUTE_MAX;
  const half = Math.floor(paneWidth * FILE_TREE_MAX_FRACTION);
  const room = Number.isFinite(reserve)
    ? Math.floor(paneWidth - Math.max(0, reserve))
    : half;
  return Math.max(FILE_TREE_MIN_WIDTH, Math.min(half, room, FILE_TREE_ABSOLUTE_MAX));
}

/** Pixel width safe to store and to paint. Non-finite input is the default. */
export function clampFileTreeWidth(
  px: number,
  paneWidth = 0,
  reserve = FILE_TREE_MIN_WIDTH,
): number {
  if (!Number.isFinite(px)) return FILE_TREE_DEFAULT_WIDTH;
  const cap = fileTreeMaxWidth(paneWidth, reserve);
  return Math.min(cap, Math.max(FILE_TREE_MIN_WIDTH, Math.round(px)));
}

/**
 * Pure parse of the persisted column. Anything that is not a width object
 * falls back to the default; a numeric width still passes through the clamp.
 */
export function loadFileTreeWidth(raw: string | null): number {
  if (!raw) return FILE_TREE_DEFAULT_WIDTH;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return FILE_TREE_DEFAULT_WIDTH;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return FILE_TREE_DEFAULT_WIDTH;
  }
  const width = (parsed as Record<string, unknown>).width;
  if (typeof width !== "number") return FILE_TREE_DEFAULT_WIDTH;
  return clampFileTreeWidth(width);
}

function readStorage(): Storage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    return window.localStorage;
  } catch {
    return null;
  }
}

export function saveFileTreeWidth(
  width: number,
  storage: Pick<Storage, "setItem"> | null = readStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(
      FILE_TREE_COLUMN_STORAGE_KEY,
      JSON.stringify({ width: clampFileTreeWidth(width) }),
    );
  } catch {
    /* quota / private mode — the in-memory width still stands */
  }
}

export interface FileTreeColumnStore extends Readable<number> {
  /** User-committed width. Persists once. */
  setWidth(px: number, paneWidth?: number, reserve?: number): void;
  /** Drag stream. Updates subscribers and does not touch storage. */
  setWidthLive(px: number, paneWidth?: number, reserve?: number): void;
}

export function createFileTreeColumnStore(
  storage: Pick<Storage, "getItem" | "setItem"> | null = readStorage(),
): FileTreeColumnStore {
  const initial = loadFileTreeWidth(storage?.getItem(FILE_TREE_COLUMN_STORAGE_KEY) ?? null);
  const { subscribe, set } = writable(initial);

  return {
    subscribe,
    setWidth(px, paneWidth = 0, reserve = FILE_TREE_MIN_WIDTH) {
      const next = clampFileTreeWidth(px, paneWidth, reserve);
      saveFileTreeWidth(next, storage);
      set(next);
    },
    setWidthLive(px, paneWidth = 0, reserve = FILE_TREE_MIN_WIDTH) {
      set(clampFileTreeWidth(px, paneWidth, reserve));
    },
  };
}

/**
 * The file tree's one width. Explorer's column paints the clamp for its row
 * and a drag writes this value; Blame renders beside that column, so it never
 * needs a width of its own.
 */
export const fileTreeColumn = createFileTreeColumnStore();
