import { createMenuSync } from "./menuSync";
import {
  observeResize,
  type ObserveResizeOptions,
} from "../dom/observeResize";

export interface StatusResizeHost {
  getBoundingClientRect(): { height: number };
}

export interface StatusResizeOptions extends ObserveResizeOptions {
  send: (height: number) => Promise<void>;
  failed: (cause: unknown) => void;
  isDisposed: () => boolean;
}

/** Clamp the status panel height before asking the native host to resize. */
export function statusPanelHeight(raw: number): number {
  return Math.min(640, Math.max(100, Math.ceil(raw)));
}

/**
 * Wire the status panel host through a coalesced ResizeObserver into the
 * native resize command. The returned dispose cancels any pending frame.
 */
export function mountStatusResize(
  host: StatusResizeHost & Element,
  options: StatusResizeOptions,
): () => void {
  const resize = createMenuSync(options.send, options.failed);
  const stopObserve = observeResize(
    host,
    () => {
      if (options.isDisposed()) return;
      resize.update(statusPanelHeight(host.getBoundingClientRect().height));
    },
    options,
  );
  return () => {
    stopObserve();
    resize.dispose();
  };
}
