/**
 * Remembering the window's size, position and maximized state.
 *
 * GitPulse persists nearly everything else about a session — open repository
 * tabs, the active view and its section, terminal dock height, Fleet state,
 * theme, recent repositories — and then opened at a fixed 1280×850 in the
 * middle of the primary display every single launch. On a multi-monitor desk
 * that means repositioning the window as the first act of every session.
 *
 * Deliberately hand-rolled rather than taking `tauri-plugin-window-state`: a
 * new dependency is a decision for the project to make, and the whole of what
 * that plugin does for this app is the hundred lines below.
 *
 * The restore is CLAMPED to a monitor that currently exists. A saved rectangle
 * is a statement about the display layout at the time it was saved, and
 * restoring it verbatim after a monitor is unplugged puts the window somewhere
 * the user cannot reach — the classic failure of naive window-state restore.
 */

const STORAGE_KEY = "gitpulse_window_state";

/** Physical pixels, as Tauri reports and accepts them. */
export interface WindowRect {
  x: number;
  y: number;
  width: number;
  height: number;
  maximized: boolean;
}

/** A monitor's usable area, in the same physical-pixel space as `WindowRect`. */
export interface MonitorArea {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Smallest window we will ever restore; mirrors tauri.conf.json's minimums. */
export const MIN_WINDOW_WIDTH = 900;
export const MIN_WINDOW_HEIGHT = 600;

/** How much of the window must land on a monitor for the rect to be usable. */
const MIN_VISIBLE_PX = 80;

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

/**
 * Parses a stored rect, rejecting anything that is not a complete, finite one.
 *
 * A partially-valid rect is not better than none: restoring a window with a
 * NaN width is how an app opens invisible.
 */
export function parseWindowState(raw: string | null): WindowRect | null {
  if (!raw) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    const candidate = parsed as Record<string, unknown>;
    if (
      !isFiniteNumber(candidate.x) ||
      !isFiniteNumber(candidate.y) ||
      !isFiniteNumber(candidate.width) ||
      !isFiniteNumber(candidate.height)
    ) {
      return null;
    }
    if (candidate.width <= 0 || candidate.height <= 0) return null;
    return {
      x: Math.round(candidate.x),
      y: Math.round(candidate.y),
      width: Math.round(candidate.width),
      height: Math.round(candidate.height),
      maximized: candidate.maximized === true,
    };
  } catch {
    return null;
  }
}

/** Whether `rect` overlaps `area` by enough to be grabbable. */
function overlaps(rect: WindowRect, area: MonitorArea): boolean {
  const overlapX =
    Math.min(rect.x + rect.width, area.x + area.width) - Math.max(rect.x, area.x);
  const overlapY =
    Math.min(rect.y + rect.height, area.y + area.height) - Math.max(rect.y, area.y);
  return overlapX >= MIN_VISIBLE_PX && overlapY >= MIN_VISIBLE_PX;
}

/**
 * A rect that is safe to apply given the monitors that exist right now.
 *
 * Returns null when there is nothing sensible to restore, which the caller
 * reads as "use the configured defaults" rather than as an error.
 */
export function clampToMonitors(
  rect: WindowRect | null,
  monitors: readonly MonitorArea[],
): WindowRect | null {
  if (!rect) return null;
  if (monitors.length === 0) return null;

  const width = Math.max(MIN_WINDOW_WIDTH, rect.width);
  const height = Math.max(MIN_WINDOW_HEIGHT, rect.height);
  const sized: WindowRect = { ...rect, width, height };

  if (monitors.some((monitor) => overlaps(sized, monitor))) return sized;

  // The saved monitor is gone. Centre on the first one that exists rather than
  // opening off-screen; the size is still the user's, only the position is not.
  const home = monitors[0];
  const fitWidth = Math.min(width, home.width);
  const fitHeight = Math.min(height, home.height);
  return {
    x: Math.round(home.x + (home.width - fitWidth) / 2),
    y: Math.round(home.y + (home.height - fitHeight) / 2),
    width: fitWidth,
    height: fitHeight,
    maximized: rect.maximized,
  };
}

/**
 * Whether a rect is worth writing.
 *
 * A maximized or minimized window reports its screen-filling or zero geometry,
 * and saving that as the restore size is how "un-maximize" stops meaning
 * anything. Only a normal window's rect is recorded; the maximized FLAG is
 * carried separately.
 */
export function shouldPersist(rect: WindowRect): boolean {
  if (rect.maximized) return false;
  return rect.width >= MIN_WINDOW_WIDTH && rect.height >= MIN_WINDOW_HEIGHT;
}

export interface WindowStateStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function readWindowState(storage: WindowStateStorage | null): WindowRect | null {
  if (!storage) return null;
  try {
    return parseWindowState(storage.getItem(STORAGE_KEY));
  } catch {
    return null;
  }
}

/**
 * Writes `rect`, merging the maximized flag onto the last normal geometry.
 *
 * Maximizing must not erase the size to restore to, so a maximized rect only
 * updates the flag.
 */
export function writeWindowState(
  storage: WindowStateStorage | null,
  rect: WindowRect,
): void {
  if (!storage) return;
  try {
    const next = shouldPersist(rect)
      ? rect
      : { ...(readWindowState(storage) ?? rect), maximized: rect.maximized };
    storage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* a full or unavailable store must never break window handling */
  }
}

// --- Tauri wiring -----------------------------------------------------------

/**
 * Restores the saved rect and starts recording changes.
 *
 * Returns a disposer. Outside Tauri it is a no-op, so callers wire it
 * unconditionally the way they do `syncWindowChrome`.
 *
 * Saves are debounced: a drag emits a move event per frame, and writing
 * localStorage on each one is both wasteful and pointless — only where the
 * drag ENDS matters.
 */
export async function installWindowStatePersistence(
  isTauriHost: boolean,
  storage: WindowStateStorage | null =
    typeof localStorage === "undefined" ? null : localStorage,
  debounceMs = 400,
): Promise<() => void> {
  if (!isTauriHost || !storage) return () => {};

  try {
    const { getCurrentWindow, availableMonitors } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();

    const monitors = (await availableMonitors()).map((monitor) => ({
      x: monitor.position.x,
      y: monitor.position.y,
      width: monitor.size.width,
      height: monitor.size.height,
    }));

    const restored = clampToMonitors(readWindowState(storage), monitors);
    if (restored) {
      const { PhysicalPosition, PhysicalSize } = await import("@tauri-apps/api/dpi");
      await win.setSize(new PhysicalSize(restored.width, restored.height));
      await win.setPosition(new PhysicalPosition(restored.x, restored.y));
      // Maximize last: doing it before the size lands stores the wrong
      // geometry as the un-maximize target.
      if (restored.maximized) await win.maximize();
    }

    let timer: ReturnType<typeof setTimeout> | undefined;
    const capture = async () => {
      try {
        const [size, position, maximized] = await Promise.all([
          win.outerSize(),
          win.outerPosition(),
          win.isMaximized(),
        ]);
        writeWindowState(storage, {
          x: position.x,
          y: position.y,
          width: size.width,
          height: size.height,
          maximized,
        });
      } catch {
        /* the window can go away mid-capture during shutdown */
      }
    };
    const schedule = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = setTimeout(() => void capture(), debounceMs);
    };

    const unlistenResize = await win.onResized(schedule);
    const unlistenMove = await win.onMoved(schedule);

    return () => {
      if (timer !== undefined) clearTimeout(timer);
      unlistenResize();
      unlistenMove();
      // One final synchronous-ish capture so a quit inside the debounce window
      // does not lose the last move.
      void capture();
    };
  } catch {
    // Window geometry needs a live Tauri window and its permissions; without
    // them the app simply opens at its configured default, as before.
    return () => {};
  }
}
