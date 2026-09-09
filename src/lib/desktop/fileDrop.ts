/**
 * Native window drag-drop is for OS file drops (open a repository).
 *
 * Tauri/WKWebView also emits enter/over for in-app HTML5 drags (Kanban cards,
 * tab reorder). Those events have empty `paths`. Treating `over` as "show the
 * overlay" is what painted "Drop a Git repository to open" over the board on
 * every card drag — `over` never carries paths.
 */

export type NativeDragPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
};

export interface FileDropGesture {
  /** True after an enter that carried file paths; survives a spurious leave. */
  sawFiles: boolean;
}

export interface FileDropDecision {
  sawFiles: boolean;
  overlay: boolean;
  dropped: string | null;
}

export function hasFilePaths(paths: string[] | undefined): boolean {
  return Array.isArray(paths) && paths.some((path) => path.length > 0);
}

export function firstDroppedPath(paths: string[] | undefined): string | null {
  if (!Array.isArray(paths)) return null;
  return paths.find((path) => path.length > 0) ?? null;
}

export function reduceFileDrop(
  payload: NativeDragPayload,
  gesture: FileDropGesture,
): FileDropDecision {
  switch (payload.type) {
    case "enter": {
      const sawFiles = hasFilePaths(payload.paths);
      return { sawFiles, overlay: sawFiles, dropped: null };
    }
    case "over":
      // Keep-alive only: cancel the hide grace after a spurious leave during
      // a real file drag. Never *start* the overlay from over — it has no paths.
      return { sawFiles: gesture.sawFiles, overlay: gesture.sawFiles, dropped: null };
    case "leave":
      return { sawFiles: gesture.sawFiles, overlay: false, dropped: null };
    case "drop":
      return { sawFiles: false, overlay: false, dropped: firstDroppedPath(payload.paths) };
  }
}
