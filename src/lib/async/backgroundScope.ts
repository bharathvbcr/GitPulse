import { bindForegroundChanges, isBackgroundDocument, type ForegroundDocument } from "../runtime/foreground";
import type { BackgroundScope } from "./pacedQueue";

interface WorkspaceScope {
  currentPath: string | null;
  openTabs: ReadonlyArray<{ path: string }>;
}

/** One lifecycle adapter for all background queues, with no idle timer. */
export function installBackgroundScope(options: {
  subscribe: (listener: (workspace: WorkspaceScope) => void) => () => void;
  target: ForegroundDocument & EventTarget;
  /** Focus and blur land here. Omitted uses `window` only when `target` is `document`. */
  frame?: EventTarget | null;
  apply: (scope: BackgroundScope) => void;
}): () => void {
  let workspace: WorkspaceScope = { currentPath: null, openTabs: [] };
  let previous: BackgroundScope | null = null;
  let disposed = false;
  function update(): void {
    if (disposed) return;
    const next: BackgroundScope = {
      activeKey: workspace.currentPath,
      retainedKeys: workspace.openTabs.map((tab) => tab.path),
      visible: !isBackgroundDocument(options.target),
    };
    if (previous && previous.activeKey === next.activeKey && previous.visible === next.visible &&
      previous.retainedKeys.length === next.retainedKeys.length &&
      previous.retainedKeys.every((key, i) => key === next.retainedKeys[i])) return;
    previous = next;
    options.apply(next);
  }
  const frame = options.frame !== undefined
    ? options.frame
    : options.target === (typeof document === "undefined" ? null : document) && typeof window !== "undefined"
      ? window
      : null;
  const unsubscribe = options.subscribe((next) => { workspace = next; update(); });
  const unbind = bindForegroundChanges(options.target, frame, update);
  update();
  return () => {
    if (disposed) return;
    disposed = true;
    unsubscribe();
    unbind();
    options.apply({ activeKey: null, retainedKeys: [], visible: false });
  };
}
