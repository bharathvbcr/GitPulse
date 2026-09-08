import type { BackgroundScope } from "./pacedQueue";

interface WorkspaceScope {
  currentPath: string | null;
  openTabs: ReadonlyArray<{ path: string }>;
}

/** One lifecycle adapter for all background queues, with no idle timer. */
export function installBackgroundScope(options: {
  subscribe: (listener: (workspace: WorkspaceScope) => void) => () => void;
  target: Pick<Document, "visibilityState" | "addEventListener" | "removeEventListener">;
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
      visible: options.target.visibilityState === "visible",
    };
    if (previous && previous.activeKey === next.activeKey && previous.visible === next.visible &&
      previous.retainedKeys.length === next.retainedKeys.length &&
      previous.retainedKeys.every((key, i) => key === next.retainedKeys[i])) return;
    previous = next;
    options.apply(next);
  }
  const unsubscribe = options.subscribe((next) => { workspace = next; update(); });
  options.target.addEventListener("visibilitychange", update);
  update();
  return () => {
    if (disposed) return;
    disposed = true;
    unsubscribe();
    options.target.removeEventListener("visibilitychange", update);
    options.apply({ activeKey: null, retainedKeys: [], visible: false });
  };
}
