import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatError } from "../ui/formatError";
import { isTauri } from "../platform";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
  type NativeMenuPayload,
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
  try {
    await invoke("cmd_set_recent_menu", { paths });
  } catch {
    /* menu rebuild is best-effort outside a packaged window */
  }
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
      await listen<NativeMenuPayload>("gitpulse-menu", (event) => {
        dispatchNativeMenu(event.payload, handlers);
      }),
    );
    unlistenAll.push(
      await listen<NativeMenuPayload>("gitpulse-open-repo", (event) => {
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
    unlistenAll.push(
      await getCurrentWindow().onDragDropEvent(async (event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          handlers.setDropActive?.(true);
          return;
        }
        if (event.payload.type === "leave") {
          handlers.setDropActive?.(false);
          return;
        }
        handlers.setDropActive?.(false);
        const dropped = event.payload.paths[0];
        if (!dropped) return;
        try {
          const root = await resolveGitRoot(dropped);
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
