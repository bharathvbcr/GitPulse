import { invoke } from "../ipc/invoke";
import { listen } from "@tauri-apps/api/event";
import { get } from "svelte/store";
import { formatError } from "../ui/formatError";
import { isTauri } from "../platform";
import { pageZoom } from "../ui/uiScale";
import { reduceFileDrop } from "./fileDrop";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
  type NativeEvent,
} from "./nativeActions";
import { createListenerTracker } from "../dom/listenerTracker";

export async function takePendingOpen(): Promise<string | null> {
  if (!isTauri()) return null;
  try {
    return await invoke<string | null>("cmd_take_pending_open");
  } catch {
    return null;
  }
}

export async function syncRecentMenu(paths: string[]): Promise<void> {
  if (!isTauri()) return;
  await invoke("cmd_set_recent_menu", { paths });
}

export async function resolveGitRoot(path: string): Promise<string> {
  return invoke<string>("cmd_resolve_git_root", { path });
}

export async function subscribeNativeShell(handlers: NativeMenuHandlers): Promise<() => void> {
  if (!isTauri()) return () => {};

  // Handles are collected as they resolve so a later failure unwinds the
  // earlier listeners instead of leaking them.
  const listeners = createListenerTracker();
  try {
    listeners.track(
      await listen<NativeEvent>("gitpulse-menu", (event) => {
        dispatchNativeMenu(event.payload, handlers);
      }),
    );
    listeners.track(
      await listen<NativeEvent>("gitpulse-open-repo", (event) => {
        if (event.payload.path) handlers.openRepo(event.payload.path);
      }),
    );
    listeners.track(
      await listen<string>("gitpulse-open-error", (event) => {
        handlers.openError(event.payload);
      }),
    );

    // Dynamic import mirrors windowChrome.ts: a static import here would pin
    // @tauri-apps/api/window and terminal drop-zone helpers into the startup
    // entry chunk under Vite.
    const [{ getCurrentWindow }, { physicalToCssPoint, terminalDropZones }] = await Promise.all([
      import("@tauri-apps/api/window"),
      import("../terminal/dropZoneRegistry"),
    ]);
    const appWindow = getCurrentWindow();
    const readScaleFactor = async (): Promise<number> => {
      if (typeof appWindow.scaleFactor !== "function") return 1;
      try {
        const sf = await appWindow.scaleFactor();
        return Number.isFinite(sf) && sf > 0 ? sf : 1;
      } catch {
        return 1;
      }
    };
    let scaleFactor = await readScaleFactor();
    // In-app HTML5/pointer drags also emit native enter/over, but without file
    // paths. reduceFileDrop is what keeps the "Drop a Git repository" veil off
    // the Kanban board and the tab strip.
    let sawFiles = false;
    let overlay = false;
    let activePaths: string[] = [];
    listeners.track(
      await appWindow.onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          activePaths = Array.isArray(event.payload.paths)
            ? event.payload.paths.filter((p) => p.length > 0)
            : [];
          void readScaleFactor().then((sf) => {
            scaleFactor = sf;
          });
        }
        const decision = reduceFileDrop(event.payload, { sawFiles });
        sawFiles = decision.sawFiles;

        if (event.payload.type === "leave") {
          terminalDropZones.clearHover();
        }

        if (decision.dropped !== null) {
          const droppedPaths =
            "paths" in event.payload && Array.isArray(event.payload.paths)
              ? event.payload.paths.filter((p) => p.length > 0)
              : [decision.dropped];
          activePaths = [];
          if ("position" in event.payload && event.payload.position) {
            const cssPoint = physicalToCssPoint(
              event.payload.position,
              scaleFactor,
              get(pageZoom),
            );
            if (terminalDropZones.dropAtCssPoint(cssPoint, droppedPaths)) {
              if (overlay) {
                handlers.setDropActive?.(false);
                overlay = false;
              }
              return;
            }
          }
          terminalDropZones.clearHover();
        }

        let wantOverlay = decision.overlay;
        if (
          sawFiles &&
          decision.dropped === null &&
          "position" in event.payload &&
          event.payload.position
        ) {
          const cssPoint = physicalToCssPoint(
            event.payload.position,
            scaleFactor,
            get(pageZoom),
          );
          if (terminalDropZones.hoverCssPoint(cssPoint, activePaths) !== null) {
            wantOverlay = false;
          }
        }

        // `over` during a file drag must re-assert true so App's hide-grace
        // timer from a spurious leave is cancelled. False→false is skipped so
        // Kanban card drags do not start that timer on every pointer move.
        if (wantOverlay || wantOverlay !== overlay) {
          handlers.setDropActive?.(wantOverlay);
        }
        overlay = wantOverlay;
        if (decision.dropped === null) return;
        try {
          const root = await resolveGitRoot(decision.dropped);
          handlers.openRepo(root);
        } catch (err) {
          handlers.openError(formatError(err));
        }
      }),
    );
  } catch (err) {
    listeners.dispose();
    throw err;
  }

  return () => listeners.dispose();
}
