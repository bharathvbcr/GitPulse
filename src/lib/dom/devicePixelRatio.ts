/** What a watcher needs of a window: the ratio and the media-query list that reports its changes. */
export interface PixelRatioWindow {
  readonly devicePixelRatio: number;
  matchMedia(query: string): Pick<MediaQueryList, "addEventListener" | "removeEventListener">;
}

/**
 * Calls `onChange` with the new ratio each time `devicePixelRatio` changes.
 *
 * Moving a window between displays, or zooming it, changes the ratio without
 * any resize event, so a box that is the same size in CSS pixels owes its
 * contents a re-measure all the same. A media query on the current resolution
 * is the one signal for it, and it stops matching at the first change, so the
 * query is armed again for the ratio it moved to each time. Returns the stop
 * function; after it, nothing fires.
 */
export function watchDevicePixelRatio(
  onChange: (ratio: number) => void,
  win: PixelRatioWindow | undefined = typeof window === "undefined" ? undefined : window,
): () => void {
  if (!win || typeof win.matchMedia !== "function") return () => {};
  let stopped = false;
  let disarm = () => {};
  const arm = () => {
    const media = win.matchMedia(`(resolution: ${win.devicePixelRatio || 1}dppx)`);
    if (typeof media.addEventListener !== "function") return;
    const listener = () => {
      disarm();
      if (stopped) return;
      arm();
      onChange(win.devicePixelRatio || 1);
    };
    media.addEventListener("change", listener);
    disarm = () => media.removeEventListener("change", listener);
  };
  arm();
  return () => {
    stopped = true;
    disarm();
    disarm = () => {};
  };
}
