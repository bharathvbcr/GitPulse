import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatError } from "../ui/formatError";
import { isTauri } from "../platform";
import { reduceFileDrop } from "./fileDrop";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
  type NativeEvent,
} from "./nativeActions";

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
  const unlistenAll: Array<() => void> = [];
  try {
    unlistenAll.push(
      await listen<NativeEvent>("gitpulse-menu", (event) => {
        dispatchNativeMenu(event.payload, handlers);
      }),
    );
    unlistenAll.push(
      await listen<NativeEvent>("gitpulse-open-repo", (event) => {
        if (event.payload.path) handlers.openRepo(event.payload.path);
      }),
    );
    unlistenAll.push(
      await listen<string>("gitpulse-open-error", (event) => {
        handlers.openError(event.payload);
      }),
    );

    // Dynamic import mirrors windowChrome.ts: a static import here would pin
    // @tauri-apps/api/window into the main chunk and warn under Vite.
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    // In-app HTML5/pointer drags also emit native enter/over, but without file
    // paths. reduceFileDrop is what keeps the "Drop a Git repository" veil off
    // the Kanban board and the tab strip.
    let sawFiles = false;
    let overlay = false;
    unlistenAll.push(
      await getCurrentWindow().onDragDropEvent(async (event) => {
        const decision = reduceFileDrop(event.payload, { sawFiles });
        sawFiles = decision.sawFiles;
        // `over` during a file drag must re-assert true so App's hide-grace
        // timer from a spurious leave is cancelled. False→false is skipped so
        // Kanban card drags do not start that timer on every pointer move.
        if (decision.overlay || decision.overlay !== overlay) {
          handlers.setDropActive?.(decision.overlay);
        }
        overlay = decision.overlay;
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
    for (const unlisten of unlistenAll) {
      try {
        unlisten();
      } catch {
        /* an already-dead listener must not mask the original failure */
      }
    }
    throw err;
  }

  return () => {
    for (const unlisten of unlistenAll) unlisten();
  };
}
